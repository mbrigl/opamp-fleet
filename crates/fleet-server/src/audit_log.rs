//! The audit record on disk (ADR-0023), the adapter behind [`crate::audit::Audit`]: one JSON line
//! per security decision, each carrying the SHA-256 of the line before it, written by one task in
//! the order the decisions were taken. What is queued together is written and synced together, up
//! to [`WRITE_BATCH`] lines, so how fast the channel empties is not bound by how often the disk
//! syncs.
//!
//! A decision is recorded through [`AuditLog::record`], which never waits on the disk: the entry goes
//! through a bounded channel to the writer task. When the channel is full, or the last write failed,
//! the call answers [`Unavailable`], and the caller refuses what it was about to allow — a security
//! decision is never taken without its record (ADR-0023 clause 5). Refusals are aggregated per
//! event and peer past ten a second, never dropped (ADR-0023 clause 4), and counted before they
//! reach the channel, where they may take no more than [`REFUSAL_SHARE`] of it: a flood of refusals
//! from however many peers cannot leave an admission or an operator act without room for its
//! record. The store behind the writer is a port; the filesystem adapter is
//! [`crate::fs::FsAuditStore`], one chain in files that rotate by size.
//!
//! On PostgreSQL the record is one chain per lease
//! (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance), the adapter
//! [`crate::postgres::audit::PgAuditStore`]. Each entry is marked with the lease it was decided
//! under. The writer writes into the chain of the lease held now: entries decided under a lease
//! that lapsed before they were written go into the next chain, marked `taken_under` the old one,
//! and a write that fails keeps its lines for the next attempt instead of counting them lost. An
//! orderly shutdown closes the chain with `audit.chain_closed` ([`AuditLog::close`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tracing::{error, info};

use crate::audit::{Audit, Entry, Field, Unavailable};
use crate::fleet::Clock;

/// The entries waiting for the writer at most (ADR-0023 clause 5).
pub const CHANNEL_CAPACITY: usize = 10_000;

/// The refusals of one event from one peer written per second before the rest are counted.
pub const REFUSALS_PER_SECOND: u32 = 10;

/// The entries of the channel refusals may take at once; the rest is kept for every other
/// decision.
pub const REFUSAL_SHARE: usize = CHANNEL_CAPACITY / 2;

/// The lines the writer hands the store at most in one append, and so syncs together.
pub const WRITE_BATCH: usize = 1024;

/// The hash a chain starts from.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Where the record left off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tail {
    /// The last non-empty line, whatever it holds: the next entry chains to it.
    pub last_line: String,
    /// The highest `seq` an entry near the end carries; the next entry takes the one after it.
    pub last_seq: u64,
    /// Whether the record ends in a line that is not a whole entry — a crash mid-write.
    pub torn: bool,
}

/// Which chain a store writes the next lines into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chain {
    /// The store's one chain: the filesystem's, which rotates and is never closed.
    One,
    /// The chain of the lease held now, by its identifier: one chain per lease.
    Lease(String),
    /// No lease is held, so there is no chain to write into until one is.
    Unleased,
}

/// The record's port (ADR-0009): where the lines go.
pub trait AuditStore: Send {
    /// The chain the next lines go into, as of now; [`tail`](Self::tail), [`append`](Self::append)
    /// and [`close`](Self::close) act on it until this is asked again. A store of one chain
    /// answers [`Chain::One`].
    fn chain(&mut self) -> Chain {
        Chain::One
    }

    /// Appends `lines` as [`append`](Self::append) does, the last of them the chain's closing
    /// entry, and records the chain as closed on that entry's hash, in one write. Only a store of
    /// one chain per lease closes one.
    ///
    /// # Errors
    /// Returns an error when the lines cannot be written or the chain cannot be closed; then none
    /// of them is in the store.
    fn close(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
        let _ = (seq, lines);
        Err("this record is one chain, which is never closed".to_string())
    }

    /// The lines the chain of the lease `chain` holds from `seq` on, in order, whether or not it is
    /// closed: what a write whose answer was lost left there. A store of one chain holds none to
    /// ask for.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn stored(&mut self, chain: &str, seq: u64) -> Result<Vec<String>, String> {
        let _ = (chain, seq);
        Ok(Vec::new())
    }

    /// Where the record left off, if it holds anything.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn tail(&mut self) -> Result<Option<Tail>, String>;

    /// Appends `lines` in order, each with its newline, to the current file, starting one named
    /// after `seq`, the first line's, when there is none; flushed to disk once for all of them.
    ///
    /// # Errors
    /// Returns an error when the lines cannot be written and flushed; then none of them is in the
    /// store.
    fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String>;

    /// The size of the current file.
    fn current_bytes(&self) -> u64;

    /// Closes the current file so the next append starts one named after its `seq`, and deletes
    /// the oldest files past `keep`; answers each deleted file's name and its last line.
    ///
    /// # Errors
    /// Returns an error when a file cannot be deleted.
    fn rotate(&mut self, keep: usize) -> Result<Vec<(String, String)>, String>;
}

/// How large a file grows and how many are kept (`[audit]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_file_bytes: u64,
    pub keep_files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_file_bytes: 64 * 1024 * 1024,
            keep_files: 16,
        }
    }
}

/// What the writer is sent: an entry, or a request to write out everything before it.
enum Message {
    /// An entry, with the chain of the lease it was decided under, where the store keeps one
    /// chain per lease and one was held.
    Entry(Entry, Option<String>),
    /// Answered once every entry queued before it was taken, its second's aggregates included:
    /// `true` when all of them were written, `false` when the record was failing.
    Drain(std::sync::mpsc::SyncSender<bool>),
    /// [`Drain`](Self::Drain), and then the chain closed, where the store keeps one per lease:
    /// `true` once it is.
    Close(std::sync::mpsc::SyncSender<bool>),
}

/// The handle every decision is recorded through.
pub struct AuditLog {
    tx: std::sync::mpsc::SyncSender<Message>,
    failing: Arc<AtomicBool>,
    /// Decisions refused because they could not be recorded, told in the next entry.
    unrecorded: Arc<AtomicU64>,
    clock: Arc<dyn Clock>,
    refusals: Arc<Mutex<Refusals>>,
    /// Refusals in the channel right now, bounded by [`REFUSAL_SHARE`].
    queued_refusals: Arc<AtomicUsize>,
    /// The lease each decision is taken under, where the store keeps one chain per lease.
    held: Option<crate::instance::Held>,
}

/// Which refusals are aggregated together: the event and the peer, an IPv6 peer by its /64.
type RefusalKey = (String, Option<String>);

/// The refusals counted per event and peer, shared by [`AuditLog::record`], which counts them
/// before they are queued, and the writer, which writes the aggregates.
#[derive(Default)]
struct Refusals {
    /// Each event and peer heard from, at most [`REFUSAL_KEYS`] of them.
    window: HashMap<RefusalKey, Slot>,
}

/// The refusals of one event and peer the writer has not yet accounted for.
#[derive(Clone, Copy)]
struct Slot {
    /// The second `count` belongs to.
    second: u64,
    count: u32,
    /// Refusals past the allowance of earlier seconds, not yet written as an aggregate.
    carried: u32,
}

/// The events and peers counted at once at most. Only the writer's tick lets go of them, so while
/// it is stalled a refusal of yet another event or peer finds no room and goes unrecorded.
pub const REFUSAL_KEYS: usize = CHANNEL_CAPACITY;

/// What becomes of one refusal.
#[derive(Debug, PartialEq, Eq)]
enum Counted {
    /// Among the first [`REFUSALS_PER_SECOND`] of its second: written as it is.
    Queue,
    /// Past them: counted for the aggregate.
    Aggregated,
    /// No room to count it.
    NoRoom,
}

impl Refusals {
    /// Counts one refusal.
    fn admit(&mut self, key: RefusalKey, second: u64) -> Counted {
        if !self.window.contains_key(&key) && self.window.len() >= REFUSAL_KEYS {
            return Counted::NoRoom;
        }
        let slot = self.window.entry(key).or_insert(Slot {
            second,
            count: 0,
            carried: 0,
        });
        if slot.second != second {
            slot.carried = slot
                .carried
                .saturating_add(slot.count.saturating_sub(REFUSALS_PER_SECOND));
            slot.second = second;
            slot.count = 0;
        }
        slot.count = slot.count.saturating_add(1);
        if slot.count <= REFUSALS_PER_SECOND {
            Counted::Queue
        } else {
            Counted::Aggregated
        }
    }

    /// Takes the unwritten counts of every second before `second`, letting go of the events and
    /// peers that have nothing left in the current one.
    fn take_passed(&mut self, second: u64) -> Vec<(RefusalKey, u32)> {
        let mut passed = Vec::new();
        self.window.retain(|key, slot| {
            let current = slot.second >= second;
            let unwritten = if current {
                slot.carried
            } else {
                slot.carried
                    .saturating_add(slot.count.saturating_sub(REFUSALS_PER_SECOND))
            };
            if unwritten > 0 {
                passed.push((key.clone(), unwritten));
            }
            slot.carried = 0;
            current
        });
        passed
    }
}

fn refusal_key(entry: &Entry) -> RefusalKey {
    (
        entry.event.clone(),
        // Counted as the throttle counts it: an IPv6 peer by its /64, so new addresses from one
        // block do not each get ten lines a second.
        match entry.fields.get("peer") {
            Some(Field::Text(peer)) => Some(peer.parse::<std::net::IpAddr>().map_or_else(
                |_| peer.clone(),
                |ip| crate::throttle::peer_key(ip).to_string(),
            )),
            _ => None,
        },
    )
}

impl AuditLog {
    /// Opens the chain where `store` left it and starts the writer task.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    pub fn start(
        store: Box<dyn AuditStore>,
        limits: Limits,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        Self::start_with(store, limits, clock, None)
    }

    /// [`start`](Self::start) for a store of one chain per lease: each decision is marked with
    /// the lease `held` names when it is recorded, and the writer writes into the chain of the
    /// lease held when it writes.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    pub fn start_under(
        store: Box<dyn AuditStore>,
        limits: Limits,
        clock: Arc<dyn Clock>,
        held: crate::instance::Held,
    ) -> Result<Self, String> {
        Self::start_with(store, limits, clock, Some(held))
    }

    fn start_with(
        mut store: Box<dyn AuditStore>,
        limits: Limits,
        clock: Arc<dyn Clock>,
        held: Option<crate::instance::Held>,
    ) -> Result<Self, String> {
        let chain = store.chain();
        let per_lease = chain != Chain::One;
        let (seq, prev, torn) = match store.tail()? {
            None => (0, GENESIS.to_string(), false),
            Some(tail) => (tail.last_seq, line_hash(&tail.last_line), tail.torn),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_CAPACITY);
        let failing = Arc::new(AtomicBool::new(false));
        let unrecorded = Arc::new(AtomicU64::new(0));
        let refusals = Arc::new(Mutex::new(Refusals::default()));
        let queued_refusals = Arc::new(AtomicUsize::new(0));
        let mut writer = Writer {
            store,
            limits,
            clock: clock.clone(),
            seq,
            prev,
            refusals: refusals.clone(),
            queued_refusals: queued_refusals.clone(),
            failing: failing.clone(),
            unrecorded: unrecorded.clone(),
            batch: Vec::new(),
            batch_bytes: 0,
            batch_start_prev: String::new(),
            per_lease,
            chain: match chain {
                Chain::Lease(id) => Some(id),
                Chain::One | Chain::Unleased => None,
            },
        };
        if torn {
            writer.write(Entry::new("audit.chain_broken", "recorded"), None);
            writer.commit();
        }
        // A thread of its own: the writes are blocking file I/O, and must not hold a runtime
        // worker that admission needs.
        std::thread::Builder::new()
            .name("audit".to_string())
            .spawn(move || writer.run(&rx))
            .map_err(|e| format!("cannot start the audit writer: {e}"))?;
        Ok(AuditLog {
            tx,
            failing,
            unrecorded,
            clock,
            refusals,
            queued_refusals,
            held,
        })
    }

    /// Waits until every entry queued so far is written, for at most `timeout`; `false` when it
    /// was not, in time or at all because the record is failing. The process exits after this at shutdown, and the writer thread with it, so what
    /// is still queued would otherwise be lost without a trace (ADR-0023).
    #[must_use]
    pub fn drain(&self, timeout: Duration) -> bool {
        self.ask(timeout, Message::Drain)
    }

    /// [`drain`](Self::drain) at an orderly shutdown, and then, where the store keeps one chain
    /// per lease, the chain closed with `audit.chain_closed` and `reason` `shutdown`
    /// (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clause 3).
    /// `false` when something queued was not written or the chain could not be closed: its lease
    /// is then left to lapse, so the instance that takes it over closes the chain.
    #[must_use]
    pub fn close(&self, timeout: Duration) -> bool {
        self.ask(timeout, Message::Close)
    }

    /// Sends the writer the request `message` makes of an answer channel, and waits for its answer
    /// for at most `timeout`.
    fn ask(
        &self,
        timeout: Duration,
        message: fn(std::sync::mpsc::SyncSender<bool>) -> Message,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let (done, written) = std::sync::mpsc::sync_channel(1);
        let mut message = message(done);
        loop {
            match self.tx.try_send(message) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(back)) => {
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    message = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
            }
        }
        written
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or(false)
    }

    fn unrecorded(&self) -> Unavailable {
        self.unrecorded.fetch_add(1, Ordering::SeqCst);
        Unavailable
    }
}

impl Audit for AuditLog {
    /// Records one decision. Answers [`Unavailable`] while the last write failed or the channel is
    /// full; the decision is then counted and told in the next entry that is written. A refusal
    /// past the per-second allowance of its event and peer is counted for its aggregate and never
    /// queued; one that finds the refusals' share of the channel taken, or no room to be counted
    /// ([`REFUSAL_KEYS`]), is counted as unrecorded.
    fn record(&self, entry: Entry) -> Result<(), Unavailable> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(self.unrecorded());
        }
        let under = self.held.as_ref().and_then(|held| held.borrow().clone());
        if !entry.is_refusal() {
            return self
                .tx
                .try_send(Message::Entry(entry, under))
                .map_err(|_| self.unrecorded());
        }
        let second = self.clock.now_ms() / 1000;
        let counted = self
            .refusals
            .lock()
            .expect("audit refusals lock")
            .admit(refusal_key(&entry), second);
        match counted {
            Counted::Queue => {}
            Counted::Aggregated => return Ok(()),
            Counted::NoRoom => return Err(self.unrecorded()),
        }
        if self.queued_refusals.fetch_add(1, Ordering::SeqCst) >= REFUSAL_SHARE {
            self.queued_refusals.fetch_sub(1, Ordering::SeqCst);
            return Err(self.unrecorded());
        }
        self.tx.try_send(Message::Entry(entry, under)).map_err(|_| {
            self.queued_refusals.fetch_sub(1, Ordering::SeqCst);
            self.unrecorded()
        })
    }
}

/// The SHA-256 of a line as written, hex.
#[must_use]
pub fn line_hash(line: &str) -> String {
    hex::encode(Sha256::digest(line.as_bytes()))
}

/// An entry as the line it is written as: `seq`, `time` (RFC 3339, UTC, from `time_ms`), `event`,
/// `outcome`, its fields, and `prev`, one JSON object.
#[must_use]
pub fn render(seq: u64, time_ms: u64, entry: &Entry, prev: &str) -> String {
    let mut line = Map::new();
    line.insert("seq".into(), seq.into());
    line.insert("time".into(), rfc3339(time_ms).into());
    line.insert("event".into(), entry.event.clone().into());
    line.insert("outcome".into(), entry.outcome.clone().into());
    for (key, value) in &entry.fields {
        let value = match value {
            Field::Text(text) => Value::from(text.clone()),
            Field::Unsigned(n) => Value::from(*n),
            Field::Signed(n) => Value::from(*n),
        };
        line.insert(key.clone(), value);
    }
    line.insert("prev".into(), prev.into());
    Value::Object(line).to_string()
}

/// Emits a line written to the record as the `tracing` event every entry is also (ADR-0023
/// clause 6).
pub(crate) fn emit(seq: u64, event: &str, outcome: &str, text: &str) {
    info!(target: "audit", seq, event = %event, outcome = %outcome, "{}", text);
}

/// The writer task's state.
struct Writer {
    store: Box<dyn AuditStore>,
    limits: Limits,
    clock: Arc<dyn Clock>,
    /// The `seq` and the hash of the last line given its place, the batch's included.
    seq: u64,
    prev: String,
    refusals: Arc<Mutex<Refusals>>,
    queued_refusals: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
    unrecorded: Arc<AtomicU64>,
    /// The lines given their place in the chain and not yet handed to the store.
    batch: Vec<Line>,
    /// Their size in the file, newlines included.
    batch_bytes: u64,
    /// The hash of the line before the batch, which the chain goes back to when the batch fails.
    batch_start_prev: String,
    /// Whether the store keeps one chain per lease: then a batch that fails is kept, not lost, and
    /// written into the chain of the next lease where its own lapsed.
    per_lease: bool,
    /// The chain the batch's lines are given their place in, where the store keeps one per lease.
    chain: Option<String>,
}

/// One line of the batch.
struct Line {
    seq: u64,
    text: String,
    /// The decision, as it was queued.
    entry: Entry,
    /// When the line was first given its place.
    time_ms: u64,
    /// The decisions that went unrecorded before it, which it tells.
    unrecorded: u64,
    /// The chain of the lease the decision was taken under, if a lease was held.
    decided_under: Option<String>,
    /// What goes unrecorded when the line is lost: its decision and the count it carries.
    lost: u64,
}

impl Writer {
    fn run(mut self, rx: &std::sync::mpsc::Receiver<Message>) {
        use std::sync::mpsc::RecvTimeoutError;
        let mut last_tick = std::time::Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(message) => {
                    self.handle(message);
                    // What is queued already joins the batch, and the whole batch is synced once.
                    for _ in 1..WRITE_BATCH {
                        match rx.try_recv() {
                            Ok(message) => self.handle(message),
                            Err(_) => break,
                        }
                    }
                    self.commit();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush_window(u64::MAX);
                    self.commit();
                    return;
                }
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                last_tick = std::time::Instant::now();
                self.tick(rx);
            }
        }
    }

    fn tick(&mut self, rx: &std::sync::mpsc::Receiver<Message>) {
        let second = self.clock.now_ms() / 1000;
        // What was queued before the second ended is written before its aggregates, so they
        // follow the refusals they count.
        for _ in 0..CHANNEL_CAPACITY {
            match rx.try_recv() {
                Ok(message) => self.handle(message),
                Err(_) => break,
            }
        }
        self.flush_window(second);
        self.commit();
        // While writing fails nothing is let through, so the writer itself tries again; the first
        // entry that lands says how much went unrecorded. A store of one chain per lease kept the
        // batch that failed, which is tried again first.
        if self.failing.load(Ordering::SeqCst) {
            if !self.per_lease || self.batch.is_empty() {
                self.append(Entry::new("audit.resumed", "recorded"), None);
            }
            self.commit();
            if self.per_lease
                && !self.failing.load(Ordering::SeqCst)
                && self.unrecorded.load(Ordering::SeqCst) > 0
            {
                self.append(Entry::new("audit.resumed", "recorded"), None);
                self.commit();
            }
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Entry(entry, under) => self.take(entry, under),
            Message::Drain(done) => {
                self.flush_window(u64::MAX);
                self.commit();
                let _ = done.send(!self.failing.load(Ordering::SeqCst));
            }
            Message::Close(done) => {
                self.flush_window(u64::MAX);
                self.commit();
                let written = !self.failing.load(Ordering::SeqCst);
                let _ = done.send(written && (!self.per_lease || self.close_chain()));
            }
        }
    }

    /// Writes `audit.chain_closed` with `reason` `shutdown` into the chain and closes it; `true`
    /// once it is. A closing entry that could not be written is taken back, so the chain stays
    /// open for the instance that takes over the lease to close.
    fn close_chain(&mut self) -> bool {
        // Into the chain of the lease held now, so that a closing entry taken back leaves the
        // writer where that chain ends.
        if self.follow_chain().is_err() {
            return false;
        }
        let (seq, prev) = (self.seq, self.prev.clone());
        self.append(
            Entry::new("audit.chain_closed", "recorded").with("reason", "shutdown"),
            None,
        );
        self.commit_as(true);
        if self.failing.load(Ordering::SeqCst) {
            if self
                .batch
                .last()
                .is_some_and(|line| line.entry.event == "audit.chain_closed")
            {
                self.batch.pop();
                self.seq = seq;
                self.prev = prev;
            }
            return false;
        }
        true
    }

    /// Writes an entry that came through the channel.
    fn take(&mut self, entry: Entry, under: Option<String>) {
        if entry.is_refusal() {
            self.queued_refusals.fetch_sub(1, Ordering::SeqCst);
        }
        self.write(entry, under);
    }

    /// Writes one aggregate per event and peer whose second has passed.
    fn flush_window(&mut self, second: u64) {
        let passed = self
            .refusals
            .lock()
            .expect("audit refusals lock")
            .take_passed(second);
        for (key, count) in passed {
            self.write_aggregate(&key, count);
        }
    }

    fn write_aggregate(&mut self, (event, peer): &RefusalKey, count: u32) {
        self.write(
            Entry::new(&format!("{event}.aggregated"), "refused")
                .with("peer", peer.clone())
                .with("count", count),
            None,
        );
    }

    /// Gives the entry its place in the chain and adds it to the batch; rotates first when the file
    /// is full. A store of one chain per lease does not rotate.
    fn write(&mut self, entry: Entry, under: Option<String>) {
        if self.per_lease {
            self.append(entry, under);
            return;
        }
        if self.store.current_bytes() + self.batch_bytes >= self.limits.max_file_bytes {
            // The batch goes into the file it was counted against; whether that file is full is
            // then up to what landed.
            self.commit();
        }
        if self.store.current_bytes() >= self.limits.max_file_bytes {
            match self.store.rotate(self.limits.keep_files) {
                Ok(deleted) => {
                    for (file, last) in deleted {
                        self.append(
                            Entry::new("audit.rotated", "recorded")
                                .with("deleted", file)
                                .with("deleted_last_hash", line_hash(&last)),
                            None,
                        );
                    }
                }
                Err(e) => error!(error = %e, "cannot rotate the audit record"),
            }
        }
        self.append(entry, under);
    }

    fn append(&mut self, entry: Entry, under: Option<String>) {
        let unrecorded = self.unrecorded.swap(0, Ordering::SeqCst);
        let mut line = Line {
            seq: 0,
            text: String::new(),
            // A retry that fails was no decision; the entries it carried the count of were.
            lost: u64::from(entry.event != "audit.resumed") + unrecorded,
            entry,
            time_ms: self.clock.now_ms(),
            unrecorded,
            decided_under: under,
        };
        if self.batch.is_empty() {
            self.batch_start_prev = self.prev.clone();
        }
        self.place(&mut line);
        self.batch_bytes += line.text.len() as u64 + 1;
        self.batch.push(line);
        // While writing fails, a store of one chain per lease keeps the batch for the writer's
        // own retry rather than trying again with every line.
        if self.batch.len() >= WRITE_BATCH
            && !(self.per_lease && self.failing.load(Ordering::SeqCst))
        {
            self.commit();
        }
    }

    /// Gives `line` the next place in the chain: its `seq`, and its text chained to the line
    /// before. In a chain of a lease the line names the chain, and, when its decision was taken
    /// under another lease, that lease's chain as `taken_under`.
    fn place(&mut self, line: &mut Line) {
        let mut entry = line.entry.clone();
        if line.unrecorded > 0 {
            entry
                .fields
                .insert("unrecorded_before".into(), Field::Unsigned(line.unrecorded));
        }
        if let Some(chain) = &self.chain {
            entry
                .fields
                .insert("chain".into(), Field::Text(chain.clone()));
            if let Some(under) = line.decided_under.as_ref().filter(|under| *under != chain) {
                entry
                    .fields
                    .insert("taken_under".into(), Field::Text(under.clone()));
            }
        }
        line.seq = self.seq + 1;
        line.text = render(line.seq, line.time_ms, &entry, &self.prev);
        self.prev = line_hash(&line.text);
        self.seq = line.seq;
    }

    /// Takes up the chain `chain` where the store says it ends, and gives every line of the batch
    /// its place in it again.
    ///
    /// A write into the old chain whose answer was lost may have landed: the lines of the batch the
    /// old chain holds are written already, and are dropped rather than written twice.
    fn switch_to(&mut self, chain: String) -> Result<(), String> {
        if let (Some(old), Some(first)) = (self.chain.clone(), self.batch.first().map(|l| l.seq)) {
            let stored = self.store.stored(&old, first)?;
            let landed = self
                .batch
                .iter()
                .zip(&stored)
                .take_while(|(line, stored)| line.text == **stored)
                .count();
            for line in self.batch.drain(..landed) {
                emit(line.seq, &line.entry.event, &line.entry.outcome, &line.text);
            }
        }
        let (seq, prev) = match self.store.tail()? {
            None => (0, GENESIS.to_string()),
            Some(tail) => (tail.last_seq, line_hash(&tail.last_line)),
        };
        self.chain = Some(chain);
        (self.seq, self.prev) = (seq, prev);
        self.batch_start_prev = self.prev.clone();
        let mut batch = std::mem::take(&mut self.batch);
        self.batch_bytes = 0;
        for line in &mut batch {
            self.place(line);
            self.batch_bytes += line.text.len() as u64 + 1;
        }
        self.batch = batch;
        Ok(())
    }

    fn commit(&mut self) {
        self.commit_as(false);
    }

    /// Where the store keeps one chain per lease: takes up the chain of the lease held now, if it
    /// is not the one the batch was placed in. An error while no lease is held.
    fn follow_chain(&mut self) -> Result<(), String> {
        if !self.per_lease {
            return Ok(());
        }
        match self.store.chain() {
            Chain::Unleased => Err("no lease is held, so there is no chain to write into".into()),
            Chain::Lease(chain) if self.chain.as_ref() != Some(&chain) => self.switch_to(chain),
            Chain::Lease(_) | Chain::One => Ok(()),
        }
    }

    /// Hands the batch to the store in one append, or, with `closing`, closes the chain on its
    /// last line. A line counts as written only once the store has synced it. When the append
    /// fails, every line of the batch is lost as a single one is, and the chain goes on from the
    /// line before the batch; a store of one chain per lease keeps the batch for the next attempt
    /// instead, in the chain of the lease held then.
    fn commit_as(&mut self, closing: bool) {
        if self.batch.is_empty() {
            return;
        }
        let ready = self.follow_chain();
        if self.batch.is_empty() {
            // Every line had landed in the chain it was written into.
            if ready.is_ok() {
                self.failing.store(false, Ordering::SeqCst);
            }
            return;
        }
        let batch = std::mem::take(&mut self.batch);
        let first = batch.first().map_or(0, |line| line.seq);
        let written = ready.and_then(|()| {
            let lines: Vec<&str> = batch.iter().map(|line| line.text.as_str()).collect();
            if closing {
                self.store.close(first, &lines)
            } else {
                self.store.append(first, &lines)
            }
        });
        match written {
            Ok(()) => {
                self.batch_bytes = 0;
                self.failing.store(false, Ordering::SeqCst);
                for line in &batch {
                    emit(line.seq, &line.entry.event, &line.entry.outcome, &line.text);
                }
            }
            Err(e) => {
                if self.per_lease {
                    self.batch = batch;
                } else {
                    self.batch_bytes = 0;
                    self.seq = first - 1;
                    self.prev = std::mem::take(&mut self.batch_start_prev);
                    let lost: u64 = batch.iter().map(|line| line.lost).sum();
                    self.unrecorded.fetch_add(lost, Ordering::SeqCst);
                }
                if !self.failing.swap(true, Ordering::SeqCst) {
                    error!(error = %e, "cannot write the audit record — refusing what needs one");
                }
            }
        }
    }
}

fn rfc3339(ms: u64) -> String {
    let nanos = i128::from(ms) * 1_000_000;
    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// Walks lines in order and names the first whose `prev` does not match the line before it
/// (ADR-0023 clause 3); `Ok(count)` when the whole chain holds. A line that is not an entry holds
/// when the file store repaired it: the torn piece a crash left, followed by `audit.chain_broken`
/// chained on its hash. A torn line nothing follows does not hold.
///
/// # Errors
/// Returns the position of the first break.
pub fn verify<'a>(lines: impl IntoIterator<Item = (String, &'a str)>) -> Result<u64, String> {
    let mut prev: Option<String> = None;
    // The torn line the next entry must record as broken: its place, its hash and why it is not
    // an entry.
    let mut torn: Option<(String, String, String)> = None;
    let mut count = 0;
    for (place, line) in lines {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(e) => {
                if let Some((place, _, e)) = torn {
                    return Err(format!(
                        "{place}: not an audit entry: {e}, and no audit.chain_broken follows it"
                    ));
                }
                torn = Some((place, line_hash(line), e.to_string()));
                continue;
            }
        };
        let stated = value
            .get("prev")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{place}: no prev"))?;
        if let Some((torn_place, hash, e)) = torn.take() {
            if value.get("event").and_then(Value::as_str) != Some("audit.chain_broken")
                || stated != hash
            {
                return Err(format!(
                    "{torn_place}: not an audit entry: {e}, and the entry after it does not \
                     record it as audit.chain_broken"
                ));
            }
        } else if let Some(prev) = &prev {
            if stated != prev {
                return Err(format!(
                    "{place}: prev {stated} does not match the entry before it ({prev})"
                ));
            }
        }
        prev = Some(line_hash(line));
        count += 1;
    }
    if let Some((place, _, e)) = torn {
        return Err(format!(
            "{place}: not an audit entry: {e} — a torn last line a crash left"
        ));
    }
    Ok(count)
}

/// A chain of a lease as [`ChainWalk`] found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walked {
    /// How many entries it holds.
    pub entries: u64,
    /// Its last entry: `seq`, `event` and `time`.
    pub last_seq: u64,
    pub last_event: String,
    pub last_time: String,
    /// Whether it is closed: it ends with `audit.chain_closed` on the hash its record holds.
    pub closed: bool,
    /// The `seq` and the hash of the archive's last entry, where its `audit.chain_opened` names
    /// them as its predecessor
    /// (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clause 6).
    pub predecessor: Option<(u64, String)>,
}

/// Walks one chain of a lease in `seq` order
/// (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clauses 3 and 5):
/// it starts with `audit.chain_opened` and 64 zeros as `prev`, every entry names the chain, carries
/// the `seq` after the one before and the hash of the one before as its `prev`, and nothing follows
/// an `audit.chain_closed`. Every error names the chain and the first entry that fails.
pub struct ChainWalk {
    chain: String,
    entries: u64,
    last: Option<(u64, String, String, String)>,
    closed_at: Option<u64>,
    predecessor: Option<(u64, String)>,
}

impl ChainWalk {
    #[must_use]
    pub fn new(chain: &str) -> Self {
        ChainWalk {
            chain: chain.to_string(),
            entries: 0,
            last: None,
            closed_at: None,
            predecessor: None,
        }
    }

    /// Takes the entry the store keeps as `seq`, the next in order.
    ///
    /// # Errors
    /// Names the chain and the entry when it does not follow the one before.
    pub fn step(&mut self, seq: u64, line: &str) -> Result<(), String> {
        let chain = self.chain.clone();
        let fail = |what: String| format!("chain {chain}, entry {seq}: {what}");
        let value: Value =
            serde_json::from_str(line).map_err(|e| fail(format!("not an audit entry: {e}")))?;
        let text = |key: &str| value.get(key).and_then(Value::as_str);
        if value.get("seq").and_then(Value::as_u64) != Some(seq) {
            return Err(fail(
                "its seq is not the one it is stored under".to_string(),
            ));
        }
        if text("chain") != Some(chain.as_str()) {
            return Err(fail("it does not name this chain".to_string()));
        }
        if let Some(closed) = self.closed_at {
            return Err(fail(format!(
                "it follows the audit.chain_closed of entry {closed}"
            )));
        }
        let event = text("event").unwrap_or_default().to_string();
        let prev = text("prev").ok_or_else(|| fail("no prev".to_string()))?;
        match &self.last {
            None => {
                if seq != 1 || event != "audit.chain_opened" {
                    return Err(fail(
                        "the chain does not begin with its audit.chain_opened — entries before \
                         this one are missing"
                            .to_string(),
                    ));
                }
                if prev != GENESIS {
                    return Err(fail("its prev is not 64 zeros".to_string()));
                }
                if let (Some(seq), Some(hash)) = (
                    value.get("predecessor_seq").and_then(Value::as_u64),
                    text("predecessor_hash"),
                ) {
                    self.predecessor = Some((seq, hash.to_string()));
                }
            }
            Some((last_seq, last_hash, _, _)) => {
                if seq != last_seq + 1 {
                    return Err(fail(format!(
                        "it follows entry {last_seq} — the entries between are missing"
                    )));
                }
                if prev != last_hash {
                    return Err(fail(format!(
                        "its prev {prev} does not match the entry before it ({last_hash})"
                    )));
                }
            }
        }
        if event == "audit.chain_closed" {
            self.closed_at = Some(seq);
        }
        let time = text("time").unwrap_or_default().to_string();
        self.last = Some((seq, line_hash(line), event, time));
        self.entries += 1;
        Ok(())
    }

    /// The chain, once every entry was taken; `closed_hash` is the hash its record holds as its
    /// closing entry's, if it was closed.
    ///
    /// # Errors
    /// Names the chain when it holds no entry, when entries are missing from the end of a closed
    /// chain or its last entry is not the recorded one, and when an entry closes a chain whose
    /// record is not closed.
    pub fn finish(self, closed_hash: Option<&str>) -> Result<Walked, String> {
        let Some((last_seq, last_hash, last_event, last_time)) = self.last else {
            return Err(format!("chain {}: it holds no entry", self.chain));
        };
        match (closed_hash, self.closed_at) {
            (Some(recorded), _) if recorded != last_hash || last_event != "audit.chain_closed" => {
                return Err(format!(
                    "chain {}, entry {last_seq}: the chain was closed on {recorded}, and its last \
                     entry is not that one — entries are missing from its end",
                    self.chain
                ));
            }
            (None, Some(closing)) => {
                return Err(format!(
                    "chain {}, entry {closing}: it closes the chain, whose record is not closed",
                    self.chain
                ));
            }
            _ => {}
        }
        Ok(Walked {
            entries: self.entries,
            last_seq,
            last_event,
            last_time,
            closed: closed_hash.is_some(),
            predecessor: self.predecessor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Manual(AtomicU64);

    impl Clock for Manual {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Files in memory: each a name and its lines.
    type Files = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    #[derive(Clone, Default)]
    struct Memory(Files, Arc<AtomicBool>);

    impl AuditStore for Memory {
        fn tail(&mut self) -> Result<Option<Tail>, String> {
            let files = self.0.lock().expect("lock");
            Ok(files.iter().rev().find_map(|(_, lines)| {
                let last_line = lines.last()?.clone();
                let seq = |line: &String| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .and_then(|value| value["seq"].as_u64())
                };
                Some(Tail {
                    last_seq: lines.iter().filter_map(seq).max().unwrap_or(0),
                    torn: seq(&last_line).is_none(),
                    last_line,
                })
            }))
        }
        fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
            if self.1.load(Ordering::SeqCst) {
                return Err("disk full".into());
            }
            let mut files = self.0.lock().expect("lock");
            if files.is_empty() || files.last().is_some_and(|(name, _)| name.is_empty()) {
                files.retain(|(name, _)| !name.is_empty());
                files.push((format!("audit-{seq}.jsonl"), Vec::new()));
            }
            let file = &mut files.last_mut().expect("a file").1;
            file.extend(lines.iter().map(|line| (*line).to_string()));
            Ok(())
        }
        fn current_bytes(&self) -> u64 {
            let files = self.0.lock().expect("lock");
            files.last().map_or(0, |(_, lines)| {
                lines.iter().map(|l| l.len() as u64 + 1).sum()
            })
        }
        fn rotate(&mut self, keep: usize) -> Result<Vec<(String, String)>, String> {
            let mut files = self.0.lock().expect("lock");
            let mut deleted = Vec::new();
            while files.len() >= keep {
                let (name, lines) = files.remove(0);
                deleted.push((name, lines.last().cloned().unwrap_or_default()));
            }
            files.push((String::new(), Vec::new()));
            Ok(deleted)
        }
    }

    impl Memory {
        fn lines(&self) -> Vec<String> {
            self.0
                .lock()
                .expect("lock")
                .iter()
                .flat_map(|(_, lines)| lines.clone())
                .collect()
        }
    }

    fn clock() -> Arc<Manual> {
        Arc::new(Manual(AtomicU64::new(1_790_000_000_000)))
    }

    /// Waits until every entry queued so far is written; only a defect takes ten seconds.
    fn written(audit: &AuditLog) {
        assert!(
            audit.drain(Duration::from_secs(10)),
            "the queued entries were not written"
        );
    }

    /// Waits until `done` holds, for ten seconds at most; only a defect takes that long.
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn chain_of(memory: &Memory) -> Result<u64, String> {
        let lines = memory.lines();
        verify(
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| (i.to_string(), l.as_str())),
        )
    }

    /// A store that takes its time over every line, as a disk under load does.
    #[derive(Clone, Default)]
    struct Slow(Memory);

    impl AuditStore for Slow {
        fn tail(&mut self) -> Result<Option<Tail>, String> {
            self.0.tail()
        }
        fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
            std::thread::sleep(Duration::from_millis(2));
            self.0.append(seq, lines)
        }
        fn current_bytes(&self) -> u64 {
            self.0.current_bytes()
        }
        fn rotate(&mut self, keep: usize) -> Result<Vec<(String, String)>, String> {
            self.0.rotate(keep)
        }
    }

    /// Draining returns only once every entry queued before it is written, so the shutdown that
    /// follows it loses none of them.
    /// Verifies: ADR-0023
    #[test]
    fn a_drain_returns_once_everything_queued_is_written() {
        let slow = Slow::default();
        let audit =
            AuditLog::start(Box::new(slow.clone()), Limits::default(), clock()).expect("start");
        for n in 0..50 {
            audit
                .record(Entry::new("admission.admitted", "admitted").with("n", n))
                .expect("record");
        }
        assert!(audit.drain(Duration::from_secs(10)));
        assert_eq!(slow.0.lines().len(), 50, "an entry was still queued");
        assert_eq!(chain_of(&slow.0), Ok(50));
    }

    /// A drain over a failing record says so, so the shutdown that follows does not pass the lost
    /// entries over in silence.
    /// Verifies: ADR-0023
    #[test]
    fn a_drain_over_a_failing_record_answers_false() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        memory.1.store(true, Ordering::SeqCst);
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("queued");
        assert!(!audit.drain(Duration::from_secs(10)));
    }

    /// A store that counts its appends, each one sync to disk.
    #[derive(Clone, Default)]
    struct Counting(Memory, Arc<AtomicUsize>);

    impl AuditStore for Counting {
        fn tail(&mut self) -> Result<Option<Tail>, String> {
            self.0.tail()
        }
        fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.append(seq, lines)
        }
        fn current_bytes(&self) -> u64 {
            self.0.current_bytes()
        }
        fn rotate(&mut self, keep: usize) -> Result<Vec<(String, String)>, String> {
            self.0.rotate(keep)
        }
    }

    /// Entries that queue while the store is busy are synced together, in the order they were
    /// queued, so a flood empties the channel at more than the disk's sync rate.
    /// Verifies: ADR-0023
    #[test]
    fn entries_queued_together_are_synced_together_in_order() {
        let counting = Counting::default();
        let audit =
            AuditLog::start(Box::new(counting.clone()), Limits::default(), clock()).expect("start");
        // The writer waits on the store while the test holds it, and the entries queue.
        let held = counting.0 .0.lock().expect("lock");
        for n in 0..100 {
            audit
                .record(Entry::new("admission.admitted", "admitted").with("n", n))
                .expect("record");
        }
        drop(held);
        assert!(audit.drain(Duration::from_secs(10)));
        // The writer may already be in an append of the first entry when the rest queue; the
        // rest go together. A sync per line would be a hundred.
        assert!(
            counting.1.load(Ordering::SeqCst) <= 2,
            "a hundred queued entries took a sync each"
        );
        let lines = counting.0.lines();
        let order: Vec<(u64, u64)> = lines
            .iter()
            .map(|l| {
                let value = serde_json::from_str::<Value>(l).expect("json");
                (
                    value["seq"].as_u64().expect("seq"),
                    value["n"].as_u64().expect("n"),
                )
            })
            .collect();
        assert_eq!(order, (1..=100).zip(0..100).collect::<Vec<_>>());
        assert_eq!(chain_of(&counting.0), Ok(100));
    }

    /// A batch is counted against the file it goes into: a file full before the batch ends is
    /// closed there, never filled past its limit by more than the line that reached it.
    /// Verifies: ADR-0023, ADR-0029
    #[test]
    fn a_batch_rotates_where_a_single_line_would() {
        const MAX: u64 = 1_000;
        let memory = Memory::default();
        let limits = Limits {
            max_file_bytes: MAX,
            keep_files: 100,
        };
        let audit = AuditLog::start(Box::new(memory.clone()), limits, clock()).expect("start");
        let held = memory.0.lock().expect("lock");
        for n in 0..50 {
            audit
                .record(Entry::new("issuance.signed", "issued").with("n", n))
                .expect("record");
        }
        drop(held);
        assert!(audit.drain(Duration::from_secs(10)));
        let files = memory.0.lock().expect("lock").clone();
        let longest = files
            .iter()
            .flat_map(|(_, lines)| lines.iter().map(|l| l.len() as u64 + 1))
            .max()
            .expect("a line");
        assert!(files.len() > 1, "the record never rotated");
        for (name, lines) in &files {
            let bytes: u64 = lines.iter().map(|l| l.len() as u64 + 1).sum();
            assert!(bytes <= MAX + longest, "{name} holds {bytes} bytes");
        }
        assert_eq!(chain_of(&memory), Ok(50));
    }

    /// An append that fails loses every line of its batch, as it loses a single one: none of them
    /// is kept, each is counted unrecorded, and the chain goes on from the line before them.
    /// Verifies: ADR-0023
    #[test]
    fn a_failed_append_loses_every_line_of_its_batch() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("record");
        assert!(audit.drain(Duration::from_secs(10)));
        let held = memory.0.lock().expect("lock");
        for n in 0..5 {
            audit
                .record(Entry::new("admission.admitted", "admitted").with("n", n))
                .expect("record");
        }
        memory.1.store(true, Ordering::SeqCst);
        drop(held);
        assert!(!audit.drain(Duration::from_secs(10)));
        assert_eq!(
            memory.lines().len(),
            1,
            "a line of the failed batch was kept"
        );
        memory.1.store(false, Ordering::SeqCst);
        until("the writer never retried", || memory.lines().len() >= 2);
        let lines = memory.lines();
        assert!(lines[1].contains("audit.resumed"), "{lines:#?}");
        assert!(lines[1].contains("\"seq\":2"), "{}", lines[1]);
        assert!(lines[1].contains("\"unrecorded_before\":5"), "{}", lines[1]);
        assert_eq!(chain_of(&memory), Ok(2));
    }

    /// Verifies: ADR-0023
    #[test]
    fn each_entry_carries_the_hash_of_the_one_before() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        for n in 0..3 {
            audit
                .record(Entry::new("admission.admitted", "admitted").with("n", n))
                .expect("record");
        }
        written(&audit);
        let lines = memory.lines();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].contains(&format!("\"prev\":\"{GENESIS}\"")),
            "{}",
            lines[0]
        );
        assert_eq!(chain_of(&memory), Ok(3));
        let mut tampered = lines.clone();
        tampered[1] = tampered[1].replace("\"n\":1", "\"n\":9");
        assert!(verify(
            tampered
                .iter()
                .enumerate()
                .map(|(i, l)| (i.to_string(), l.as_str()))
        )
        .is_err());
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_chain_survives_a_restart_and_a_rotation() {
        let memory = Memory::default();
        let limits = Limits {
            max_file_bytes: 300,
            keep_files: 2,
        };
        {
            let audit = AuditLog::start(Box::new(memory.clone()), limits, clock()).expect("start");
            for n in 0..4 {
                audit
                    .record(Entry::new("issuance.signed", "issued").with("n", n))
                    .expect("record");
            }
            written(&audit);
        }
        let audit = AuditLog::start(Box::new(memory.clone()), limits, clock()).expect("restart");
        for n in 4..8 {
            audit
                .record(Entry::new("issuance.signed", "issued").with("n", n))
                .expect("record");
        }
        written(&audit);
        let lines = memory.lines();
        assert!(
            lines.iter().any(|l| l.contains("audit.rotated")),
            "{lines:#?}"
        );
        let seqs: Vec<u64> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<Value>(l).expect("json")["seq"]
                    .as_u64()
                    .expect("seq")
            })
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
        // What is kept chains on from what was deleted.
        assert!(chain_of(&memory).is_ok(), "{lines:#?}");
    }

    /// Verifies: ADR-0023
    #[test]
    fn a_truncated_file_is_recorded_as_a_broken_chain() {
        let memory = Memory::default();
        memory.0.lock().expect("lock").push((
            "audit-1.jsonl".into(),
            vec![
                "{\"seq\":7,\"prev\":\"00\"}".into(),
                "{\"seq\":8,\"prev\":\"00".into(),
            ],
        ));
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("record");
        written(&audit);
        let lines = memory.lines();
        assert!(lines[2].contains("audit.chain_broken"), "{lines:#?}");
        assert!(
            lines[2].contains(&line_hash(&lines[1])),
            "it chains on from the torn line"
        );
        assert!(
            lines[2].contains("\"seq\":8"),
            "it counts on from the last whole entry, never from 1: {}",
            lines[2]
        );
    }

    /// `audit-verify` holds the file store's repair (a torn line, then `audit.chain_broken` chained
    /// on its hash), and refuses a torn line nothing records as broken, in the middle or at the
    /// end.
    /// Verifies: ADR-0023
    #[test]
    fn a_torn_line_holds_only_when_the_next_entry_records_it_as_broken() {
        let place = |lines: &[String]| -> Vec<(String, String)> {
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| (i.to_string(), l.clone()))
                .collect()
        };
        let check = |lines: &[String]| {
            let placed = place(lines);
            verify(placed.iter().map(|(p, l)| (p.clone(), l.as_str())))
        };
        let first = format!("{{\"seq\":1,\"prev\":\"{GENESIS}\",\"event\":\"a\"}}");
        let fragment = "{\"seq\":2,\"pr".to_string();
        let broken = |event: &str, prev: &str| {
            format!("{{\"seq\":2,\"prev\":\"{prev}\",\"event\":\"{event}\"}}")
        };
        let repaired = broken("audit.chain_broken", &line_hash(&fragment));
        let after = format!(
            "{{\"seq\":3,\"prev\":\"{}\",\"event\":\"a\"}}",
            line_hash(&repaired)
        );
        assert_eq!(
            check(&[first.clone(), fragment.clone(), repaired.clone(), after]),
            Ok(3)
        );
        let err = check(&[first.clone(), fragment.clone()]).expect_err("torn at the end");
        assert!(err.starts_with("1:") && err.contains("torn"), "{err}");
        let other = broken("admission.admitted", &line_hash(&fragment));
        let err = check(&[first.clone(), fragment.clone(), other]).expect_err("not broken");
        assert!(err.starts_with("1:"), "{err}");
        let elsewhere = broken("audit.chain_broken", &line_hash(&first));
        let err = check(&[first.clone(), fragment.clone(), elsewhere]).expect_err("other prev");
        assert!(err.starts_with("1:"), "{err}");
        let err = check(&[first, fragment.clone(), fragment, repaired]).expect_err("two torn");
        assert!(err.starts_with("1:"), "{err}");
    }

    /// Verifies: ADR-0023
    #[test]
    fn refusals_beyond_ten_a_second_are_aggregated_with_their_count() {
        let memory = Memory::default();
        let clock = clock();
        let audit = AuditLog::start(Box::new(memory.clone()), Limits::default(), clock.clone())
            .expect("start");
        for _ in 0..25 {
            audit.refusal(
                Entry::new("admission.refused", "refused")
                    .peer(Some("192.0.2.1".parse().expect("ip"))),
            );
        }
        let refused = |memory: &Memory| {
            memory
                .lines()
                .iter()
                .filter(|l| l.contains("\"admission.refused\""))
                .count()
        };
        until("the refusals queued were not written", || {
            refused(&memory) >= 10
        });
        // The aggregate is written once its second has passed, by the writer's own tick.
        clock.0.fetch_add(1_000, Ordering::SeqCst);
        until("no aggregate", || {
            memory
                .lines()
                .iter()
                .any(|l| l.contains("admission.refused.aggregated"))
        });
        let lines = memory.lines();
        let written = lines
            .iter()
            .filter(|l| l.contains("\"admission.refused\""))
            .count();
        assert_eq!(written, 10);
        let aggregate = lines
            .iter()
            .find(|l| l.contains("admission.refused.aggregated"))
            .expect("an aggregate");
        assert!(aggregate.contains("\"count\":15"), "{aggregate}");
    }

    /// A refusal of the rate limit never waits for its entry: one that finds the channel full is
    /// counted, and the next entry written says so in `unrecorded_before`.
    /// Verifies: ADR-0012, ADR-0023
    #[test]
    fn a_throttle_refusal_that_finds_the_channel_full_is_counted_unrecorded() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        // The writer waits on the store while the test holds it, and the channel fills.
        let held = memory.0.lock().expect("lock");
        let mut queued = 0;
        while audit
            .record(Entry::new("admission.admitted", "admitted"))
            .is_ok()
        {
            queued += 1;
            assert!(queued <= CHANNEL_CAPACITY + 1, "the channel never filled");
        }
        audit.refusal(
            Entry::new("agent_rate.throttled", "throttled")
                .peer(Some("192.0.2.1".parse().expect("ip")))
                .with("bucket", "host"),
        );
        drop(held);
        until("the writer fell behind", || memory.lines().len() >= queued);
        let lines = memory.lines();
        assert!(
            !lines.iter().any(|l| l.contains("agent_rate.throttled")),
            "the refusal found no room"
        );
        assert!(
            lines.iter().any(|l| l.contains("\"unrecorded_before\":2")),
            "the admission that found no room and the refusal are counted"
        );
    }

    /// A flood of refusals — from one peer, and from as many peers as the channel holds entries —
    /// never leaves an admission without room for its record, even while the writer is stalled.
    /// Verifies: ADR-0023
    #[test]
    fn a_flood_of_refusals_leaves_room_for_every_other_record() {
        let memory = Memory::default();
        let clock = clock();
        let audit = AuditLog::start(Box::new(memory.clone()), Limits::default(), clock.clone())
            .expect("start");
        // The writer waits on the store while the test holds it, as it would on a slow disk.
        let held = memory.0.lock().expect("lock");
        for _ in 0..CHANNEL_CAPACITY * 2 {
            audit.refusal(
                Entry::new("operator.refused", "refused")
                    .peer(Some("192.0.2.1".parse().expect("ip"))),
            );
        }
        for n in 0..u32::try_from(CHANNEL_CAPACITY * 2).expect("fits") {
            audit.refusal(
                Entry::new("operator.refused", "refused")
                    .peer(Some(std::net::Ipv4Addr::from(0x0a00_0000 + n).into())),
            );
        }
        assert_eq!(
            audit.record(Entry::new("admission.admitted", "admitted")),
            Ok(()),
            "an admission found the channel full of refusals"
        );
        drop(held);
        // Once the second has passed, the one peer's flood is ten lines and an aggregate.
        clock.0.fetch_add(1_000, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let aggregate = loop {
            let lines = memory.lines();
            if let Some(aggregate) = lines
                .iter()
                .find(|l| l.contains("operator.refused.aggregated") && l.contains("192.0.2.1"))
            {
                break aggregate.clone();
            }
            assert!(std::time::Instant::now() < deadline, "no aggregate");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            aggregate.contains(&format!("\"count\":{}", CHANNEL_CAPACITY * 2 - 10)),
            "{aggregate}"
        );
        let written = memory
            .lines()
            .iter()
            .filter(|l| l.contains("\"operator.refused\"") && l.contains("\"192.0.2.1\""))
            .count();
        assert_eq!(written, REFUSALS_PER_SECOND as usize);
    }

    /// While the writer is stalled, the refusals it has yet to account for are counted for a
    /// bounded number of events and peers; a refusal of one more is unrecorded, not remembered.
    /// The bound is on memory, which no interface shows, so the table is looked at directly.
    /// Verifies: ADR-0023
    #[test]
    fn a_stalled_writer_counts_refusals_for_a_bounded_number_of_peers() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        let held = memory.0.lock().expect("lock");
        for n in 0..u32::try_from(REFUSAL_KEYS * 3).expect("fits") {
            audit.refusal(
                Entry::new("operator.refused", "refused")
                    .peer(Some(std::net::Ipv4Addr::from(0x0a00_0000 + n).into())),
            );
        }
        assert!(
            audit.refusals.lock().expect("lock").window.len() <= REFUSAL_KEYS,
            "every peer of a stalled writer is remembered"
        );
        drop(held);
    }

    /// Verifies: ADR-0023
    #[test]
    fn a_write_that_fails_refuses_until_one_succeeds() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        memory.1.store(true, Ordering::SeqCst);
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("queued");
        assert!(
            !audit.drain(Duration::from_secs(10)),
            "the failed write went unnoticed"
        );
        assert_eq!(
            audit.record(Entry::new("admission.admitted", "admitted")),
            Err(Unavailable)
        );
        memory.1.store(false, Ordering::SeqCst);
        // The writer tries again on its own tick, and its first line that lands resumes the record.
        until("the writer never retried", || {
            memory.lines().iter().any(|l| l.contains("audit.resumed"))
        });
        // The line is in the store a moment before the writer clears its failure; a drain is
        // answered after that, on the writer's own thread.
        written(&audit);
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("recording works again once the store does");
        written(&audit);
        let resumed = memory
            .lines()
            .into_iter()
            .find(|l| l.contains("audit.resumed"))
            .expect("a resumption entry");
        assert!(resumed.contains("\"unrecorded_before\":2"), "{resumed}");
    }

    /// A chain of a lease as its writer and a closing instance write it: `audit.chain_opened`,
    /// `entries` decisions and, with `closed`, `audit.chain_closed`.
    fn lease_chain(entries: u64, closed: bool) -> Vec<String> {
        let mut prev = GENESIS.to_string();
        let mut lines = Vec::new();
        let mut push = |entry: Entry| {
            let line = render(
                lines.len() as u64 + 1,
                0,
                &entry.with("chain", "c-1"),
                &prev,
            );
            prev = line_hash(&line);
            lines.push(line);
        };
        push(Entry::new("audit.chain_opened", "recorded"));
        for n in 0..entries {
            push(Entry::new("admission.admitted", "admitted").with("n", n));
        }
        if closed {
            push(Entry::new("audit.chain_closed", "recorded").with("reason", "shutdown"));
        }
        lines
    }

    fn walked(lines: &[String], closed_hash: Option<&str>) -> Result<Walked, String> {
        let mut walk = ChainWalk::new("c-1");
        for line in lines {
            let seq = serde_json::from_str::<Value>(line).expect("json")["seq"]
                .as_u64()
                .expect("seq");
            walk.step(seq, line)?;
        }
        walk.finish(closed_hash)
    }

    /// A chain of a lease holds when it begins with its `audit.chain_opened` and every entry
    /// follows the one before; an altered entry, one removed from the end of a closed chain, an
    /// entry after its `audit.chain_closed`, and a closing the record does not hold each fail,
    /// naming the chain and the entry; an open chain is reported open with its last entry.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_chain_of_a_lease_fails_where_it_was_cut_or_changed() {
        let open = lease_chain(3, false);
        let walked_open = walked(&open, None).expect("an open chain holds");
        assert!(!walked_open.closed);
        assert_eq!((walked_open.entries, walked_open.last_seq), (4, 4));
        assert_eq!(walked_open.last_event, "admission.admitted");

        let closed = lease_chain(3, true);
        let hash = line_hash(closed.last().expect("closing"));
        assert!(walked(&closed, Some(&hash)).expect("holds").closed);

        let mut altered = closed.clone();
        altered[2] = altered[2].replace("\"n\":1", "\"n\":7");
        let err = walked(&altered, Some(&hash)).expect_err("altered");
        assert!(err.starts_with("chain c-1, entry 4:"), "{err}");

        let cut = &closed[..closed.len() - 1];
        let err = walked(cut, Some(&hash)).expect_err("cut at the end");
        assert!(
            err.contains("chain c-1, entry 4:") && err.contains("missing"),
            "{err}"
        );

        let mut after = closed.clone();
        after.push(render(
            6,
            0,
            &Entry::new("admission.admitted", "admitted").with("chain", "c-1"),
            &hash,
        ));
        let err = walked(&after, Some(&hash)).expect_err("after the close");
        assert!(
            err.contains("entry 6:") && err.contains("chain_closed"),
            "{err}"
        );

        let err = walked(&closed, None).expect_err("a closing the record does not hold");
        assert!(err.contains("entry 5:"), "{err}");

        let err = walked(&open[1..], None).expect_err("its beginning removed");
        assert!(
            err.contains("entry 2:") && err.contains("chain_opened"),
            "{err}"
        );

        let mut other = open.clone();
        other[1] = other[1].replace("c-1", "c-2");
        assert!(walked(&other, None)
            .expect_err("another chain's")
            .contains("entry 2:"));
    }
}
