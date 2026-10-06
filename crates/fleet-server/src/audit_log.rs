//! The audit record on disk (ADR-0030), the adapter behind [`crate::audit::Audit`]: one JSON line
//! per security decision, each carrying the SHA-256 of the line before it, written by one task in
//! the order the decisions were taken.
//!
//! A decision is recorded through [`AuditLog::record`], which never waits on the disk: the entry goes
//! through a bounded channel to the writer task. When the channel is full, or the last write failed,
//! the call answers [`Unavailable`], and the caller refuses what it was about to allow — a security
//! decision is never taken without its record (clause 6). Refusals are aggregated per event and
//! peer past ten a second, never dropped (clause 5). The store behind the writer is a port; the
//! filesystem adapter is [`crate::fs::FsAuditStore`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tracing::{error, info};

use crate::audit::{Audit, Entry, Field, Unavailable};
use crate::fleet::Clock;

/// The entries waiting for the writer at most (clause 6).
pub const CHANNEL_CAPACITY: usize = 10_000;

/// The refusals of one event from one peer written per second before the rest are counted.
pub const REFUSALS_PER_SECOND: u32 = 10;

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

/// The record's port (ADR-0006): where the lines go.
pub trait AuditStore: Send {
    /// Where the record left off, if it holds anything.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn tail(&mut self) -> Result<Option<Tail>, String>;

    /// Appends one line, newline added, to the current file, starting one named after `seq` when
    /// there is none.
    ///
    /// # Errors
    /// Returns an error when the line cannot be written and flushed.
    fn append(&mut self, seq: u64, line: &str) -> Result<(), String>;

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

/// The handle every decision is recorded through.
pub struct AuditLog {
    tx: std::sync::mpsc::SyncSender<Entry>,
    failing: Arc<AtomicBool>,
    /// Decisions refused because they could not be recorded, told in the next entry.
    unrecorded: Arc<AtomicU64>,
}

impl AuditLog {
    /// Opens the chain where `store` left it and starts the writer task.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    pub fn start(
        mut store: Box<dyn AuditStore>,
        limits: Limits,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        let (seq, prev, torn) = match store.tail()? {
            None => (0, GENESIS.to_string(), false),
            Some(tail) => (tail.last_seq, line_hash(&tail.last_line), tail.torn),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_CAPACITY);
        let failing = Arc::new(AtomicBool::new(false));
        let unrecorded = Arc::new(AtomicU64::new(0));
        let mut writer = Writer {
            store,
            limits,
            clock,
            seq,
            prev,
            window: HashMap::new(),
            failing: failing.clone(),
            unrecorded: unrecorded.clone(),
        };
        if torn {
            writer.write(Entry::new("audit.chain_broken", "recorded"));
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
        })
    }
}

impl Audit for AuditLog {
    /// Records one decision. Answers [`Unavailable`] while the last write failed or the channel is
    /// full; the decision is then counted and told in the next entry that is written.
    fn record(&self, entry: Entry) -> Result<(), Unavailable> {
        if self.failing.load(Ordering::SeqCst) {
            self.unrecorded.fetch_add(1, Ordering::SeqCst);
            return Err(Unavailable);
        }
        self.tx.try_send(entry).map_err(|_| {
            self.unrecorded.fetch_add(1, Ordering::SeqCst);
            Unavailable
        })
    }
}

/// The SHA-256 of a line as written, hex.
#[must_use]
pub fn line_hash(line: &str) -> String {
    hex::encode(Sha256::digest(line.as_bytes()))
}

/// The writer task's state.
struct Writer {
    store: Box<dyn AuditStore>,
    limits: Limits,
    clock: Arc<dyn Clock>,
    seq: u64,
    prev: String,
    /// Refusals per event and peer in the current second: the second, and how many came.
    window: HashMap<(String, Option<String>), (u64, u32)>,
    failing: Arc<AtomicBool>,
    unrecorded: Arc<AtomicU64>,
}

impl Writer {
    fn run(mut self, rx: &std::sync::mpsc::Receiver<Entry>) {
        use std::sync::mpsc::RecvTimeoutError;
        let mut last_tick = std::time::Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(entry) => self.take(entry),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush_window(u64::MAX);
                    return;
                }
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                last_tick = std::time::Instant::now();
                self.tick();
            }
        }
    }

    fn tick(&mut self) {
        let second = self.clock.now_ms() / 1000;
        self.flush_window(second);
        // While writing fails nothing is let through, so the writer itself tries again; the first
        // entry that lands says how much went unrecorded.
        if self.failing.load(Ordering::SeqCst) {
            self.append(Entry::new("audit.resumed", "recorded"));
        }
    }

    /// Writes an entry, or counts it when it is a refusal past the per-second allowance.
    fn take(&mut self, entry: Entry) {
        if entry.is_refusal() {
            let second = self.clock.now_ms() / 1000;
            let key = (
                entry.event.clone(),
                // Counted as the throttle counts it: an IPv6 peer by its /64, so new addresses from
                // one block do not each get ten lines a second.
                match entry.fields.get("peer") {
                    Some(Field::Text(peer)) => Some(peer.parse::<std::net::IpAddr>().map_or_else(
                        |_| peer.clone(),
                        |ip| crate::throttle::peer_key(ip).to_string(),
                    )),
                    _ => None,
                },
            );
            let slot = self.window.entry(key.clone()).or_insert((second, 0));
            if slot.0 != second {
                let (_, count) = *slot;
                *slot = (second, 0);
                if count > REFUSALS_PER_SECOND {
                    self.write_aggregate(&key, count - REFUSALS_PER_SECOND);
                }
            }
            let slot = self.window.get_mut(&key).expect("just inserted");
            slot.1 += 1;
            if slot.1 > REFUSALS_PER_SECOND {
                return;
            }
        }
        self.write(entry);
    }

    /// Writes one aggregate per event and peer whose second has passed.
    fn flush_window(&mut self, second: u64) {
        let passed: Vec<_> = self
            .window
            .iter()
            .filter(|(_, (at, _))| *at < second)
            .map(|(key, (_, count))| (key.clone(), *count))
            .collect();
        for (key, count) in passed {
            self.window.remove(&key);
            if count > REFUSALS_PER_SECOND {
                self.write_aggregate(&key, count - REFUSALS_PER_SECOND);
            }
        }
    }

    fn write_aggregate(&mut self, (event, peer): &(String, Option<String>), count: u32) {
        self.write(
            Entry::new(&format!("{event}.aggregated"), "refused")
                .with("peer", peer.clone())
                .with("count", count),
        );
    }

    /// Gives the entry its place in the chain and appends it; rotates first when the file is full.
    fn write(&mut self, entry: Entry) {
        if self.store.current_bytes() >= self.limits.max_file_bytes {
            match self.store.rotate(self.limits.keep_files) {
                Ok(deleted) => {
                    for (file, last) in deleted {
                        self.append(
                            Entry::new("audit.rotated", "recorded")
                                .with("deleted", file)
                                .with("deleted_last_hash", line_hash(&last)),
                        );
                    }
                }
                Err(e) => error!(error = %e, "cannot rotate the audit record"),
            }
        }
        self.append(entry);
    }

    fn append(&mut self, entry: Entry) {
        let seq = self.seq + 1;
        let mut line = Map::new();
        line.insert("seq".into(), seq.into());
        line.insert("time".into(), rfc3339(self.clock.now_ms()).into());
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
        let unrecorded = self.unrecorded.swap(0, Ordering::SeqCst);
        if unrecorded > 0 {
            line.insert("unrecorded_before".into(), unrecorded.into());
        }
        line.insert("prev".into(), self.prev.clone().into());
        let text = Value::Object(line).to_string();
        match self.store.append(seq, &text) {
            Ok(()) => {
                self.seq = seq;
                self.prev = line_hash(&text);
                self.failing.store(false, Ordering::SeqCst);
                info!(target: "audit", seq, event = %entry.event, outcome = %entry.outcome, "{text}");
            }
            Err(e) => {
                // A retry that fails was no decision; the entries it carried the count of were.
                let lost = u64::from(entry.event != "audit.resumed");
                self.unrecorded
                    .fetch_add(lost + unrecorded, Ordering::SeqCst);
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
/// (clause 3); `Ok(count)` when the whole chain holds.
///
/// # Errors
/// Returns the position of the first break.
pub fn verify<'a>(lines: impl IntoIterator<Item = (String, &'a str)>) -> Result<u64, String> {
    let mut prev: Option<String> = None;
    let mut count = 0;
    for (place, line) in lines {
        let value: Value =
            serde_json::from_str(line).map_err(|e| format!("{place}: not an audit entry: {e}"))?;
        let stated = value
            .get("prev")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{place}: no prev"))?;
        if let Some(prev) = &prev {
            if stated != prev {
                return Err(format!(
                    "{place}: prev {stated} does not match the entry before it ({prev})"
                ));
            }
        }
        prev = Some(line_hash(line));
        count += 1;
    }
    Ok(count)
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
        fn append(&mut self, seq: u64, line: &str) -> Result<(), String> {
            if self.1.load(Ordering::SeqCst) {
                return Err("disk full".into());
            }
            let mut files = self.0.lock().expect("lock");
            if files.is_empty() || files.last().is_some_and(|(name, _)| name.is_empty()) {
                files.retain(|(name, _)| !name.is_empty());
                files.push((format!("audit-{seq}.jsonl"), Vec::new()));
            }
            files.last_mut().expect("a file").1.push(line.to_string());
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

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
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

    /// Verifies: ADR-0030
    #[tokio::test]
    async fn each_entry_carries_the_hash_of_the_one_before() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        for n in 0..3 {
            audit
                .record(Entry::new("admission.admitted", "admitted").with("n", n))
                .expect("record");
        }
        settle().await;
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

    /// Verifies: ADR-0030
    #[tokio::test]
    async fn the_chain_survives_a_restart_and_a_rotation() {
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
            settle().await;
        }
        let audit = AuditLog::start(Box::new(memory.clone()), limits, clock()).expect("restart");
        for n in 4..8 {
            audit
                .record(Entry::new("issuance.signed", "issued").with("n", n))
                .expect("record");
        }
        settle().await;
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

    /// Verifies: ADR-0030
    #[tokio::test]
    async fn a_truncated_file_is_recorded_as_a_broken_chain() {
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
        settle().await;
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

    /// Verifies: ADR-0030
    #[tokio::test]
    async fn refusals_beyond_ten_a_second_are_aggregated_with_their_count() {
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
        settle().await;
        clock.0.fetch_add(1_000, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
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
    /// Verifies: ADR-0023, ADR-0030
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
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while memory.lines().len() < queued {
            assert!(
                std::time::Instant::now() < deadline,
                "the writer fell behind"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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

    /// Verifies: ADR-0030
    #[tokio::test]
    async fn a_write_that_fails_refuses_until_one_succeeds() {
        let memory = Memory::default();
        let audit =
            AuditLog::start(Box::new(memory.clone()), Limits::default(), clock()).expect("start");
        memory.1.store(true, Ordering::SeqCst);
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("queued");
        settle().await;
        assert_eq!(
            audit.record(Entry::new("admission.admitted", "admitted")),
            Err(Unavailable)
        );
        memory.1.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        audit
            .record(Entry::new("admission.admitted", "admitted"))
            .expect("recording works again once the store does");
        settle().await;
        let resumed = memory
            .lines()
            .into_iter()
            .find(|l| l.contains("audit.resumed"))
            .expect("a resumption entry");
        assert!(resumed.contains("\"unrecorded_before\":2"), "{resumed}");
    }
}
