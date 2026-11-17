//! The test helpers the contexts of `fleet` share: an Agent put into the fleet, a report, the
//! view of one Agent, labels, the list read a page at a time, and a clock the test moves.

use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

pub(super) fn insert(state: &AppState, record: AgentRecord) -> InstanceUid {
    let uid = InstanceUid::default();
    state.fleet.lock().expect("fleet lock").insert(uid, record);
    uid
}

/// A full report for the persistence tests: description, capabilities, and a sequence number.
pub(super) fn report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![opamp::attributes::string_attr("service.name", "otelcol")],
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) fn organization(name: &str) -> crate::revocation::SubjectValues {
    crate::revocation::SubjectValues {
        organization: name.to_string(),
        ..Default::default()
    }
}

pub(super) fn view(state: &AppState, uid: &InstanceUid) -> AgentView {
    state
        .snapshot()
        .into_iter()
        .find(|view| view.instance_uid == uid.to_string())
        .expect("in the view")
}

pub(super) fn label(state: &AppState, uid: &InstanceUid, pairs: &[(&str, &str)]) {
    let set = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    if state.set_labels(uid, set).is_err() {
        panic!("labels refused");
    }
}

/// A clock the test moves.
pub(super) struct Manual(pub(super) Arc<AtomicU64>);

impl Clock for Manual {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A full report from `uid` over a certificate naming `host`, at sequence number `seq`.
#[allow(clippy::too_many_arguments)]
pub(super) fn report_listed(
    state: &AppState,
    host: &str,
    subject: crate::revocation::SubjectValues,
    uid: InstanceUid,
    seq: u64,
    capabilities: u64,
    identifying: &[(&str, &str)],
    non_identifying: &[(&str, &str)],
) {
    let pairs = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| opamp::attributes::string_attr(key, value))
            .collect()
    };
    let message = AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: seq,
        capabilities,
        agent_description: Some(AgentDescription {
            identifying_attributes: pairs(identifying),
            non_identifying_attributes: pairs(non_identifying),
        }),
        ..Default::default()
    };
    state.process_presented(
        message,
        Transport::Http,
        None,
        Some(&Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: subject,
            not_after_ms: u64::MAX,
        }),
    );
}

/// The Agents `filter` admits in the order of `sort`, read a page of two at a time, and the
/// count every page names.
pub(super) fn listed(
    state: &AppState,
    filter: crate::listing::AgentFilter,
    sort: crate::listing::AgentSort,
    descending: bool,
) -> Vec<InstanceUid> {
    let mut query = AgentQuery {
        filter,
        sort,
        descending,
        after: None,
        limit: 2,
    };
    let mut seen = Vec::new();
    let mut totals = BTreeSet::new();
    loop {
        let page = state.list_agents(&query).expect("a page");
        assert!(page.agents.len() <= 2);
        totals.insert(page.total);
        seen.extend(
            page.agents
                .iter()
                .map(|view| InstanceUid::parse(&view.instance_uid).expect("uid")),
        );
        match page.next {
            Some(next) => query.after = Some(next),
            None => break,
        }
    }
    assert_eq!(
        totals,
        BTreeSet::from([seen.len()]),
        "every page counts all"
    );
    seen
}

/// A fleet delivering `otelcol@1.0.0`, uploaded and signed for linux/amd64 and held by the
/// `stable` channel that claims every `otelcol`, with a host register.
pub(super) fn delivering_fleet(dir: &std::path::Path) -> (AppState, Arc<Revocations>, PackageId) {
    let store = PackageStore::open(dir.join("packages")).expect("store");
    let id = PackageId::new("otelcol", "1.0.0").expect("id");
    store.create(&id).expect("create");
    store
        .put_entry(&id, &linux(), b"v1".to_vec())
        .expect("entry");
    let revocations = Arc::new(
        Revocations::open(
            Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations"),
    );
    let state = AppState::new(dir.join("configs"))
        .expect("state")
        .with_packages(Some(
            PackageOffering::new(&dir.join("packages"), store).expect("offering"),
        ))
        .with_revocations(Some(revocations.clone()));
    state
        .deployment_store()
        .expect("deployments")
        .put(
            "stable",
            BTreeMap::from([("service.name".to_string(), "otelcol".to_string())]),
        )
        .expect("deployment");
    put_signed(&state, &id);
    (state, revocations, id)
}

pub(super) fn linux() -> Platform {
    Platform::new("linux", "amd64").expect("platform")
}

/// Signs `id` for linux/amd64 and puts it into the `stable` channel — saved, released to nobody.
pub(super) fn put_signed(state: &AppState, id: &PackageId) {
    state
        .put_package_entry_signature(id, &linux(), vec![1; 64])
        .expect("signature");
    state
        .put_deployment_package("stable", id, true)
        .expect("package");
}

/// A top-level revision of `text` for the Agent type `report` describes.
pub(super) fn body(text: &str) -> Revision {
    Revision {
        body: text.to_string(),
        role: String::new(),
        service_name: "otelcol".to_string(),
    }
}

/// A fleet whose host register is kept in `dir`, so a reopen restores both.
pub(super) fn hosted_fleet(dir: &std::path::Path) -> AppState {
    let revocations = Arc::new(
        Revocations::open(
            Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations"),
    );
    AppState::new(dir.join("configs"))
        .expect("state")
        .with_revocations(Some(revocations))
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn record_with(capabilities: u64) -> AgentRecord {
    AgentRecord {
        sequence_num: 1,
        capabilities,
        description: None,
        health: None,
        effective_config: None,
        remote_config_status: None,
        transport: Transport::WebSocket,
        connected: false,
        last_seen_ms: now_ms(),
        restart_pending: false,
        available_components: None,
        components_demanded: None,
        connection_settings_status: None,
        package_statuses: None,
        owner: None,
        carried_ms: 0,
        heard: false,
        labels: BTreeMap::new(),
        host: None,
        placement: Placement::default(),
        holder: None,
        assignment: None,
    }
}

/// Whether the record `stored` holds of `uid` has a restart queued; `None` without a record.
pub(super) fn queued(stored: &dyn AgentStore, uid: &InstanceUid) -> Option<bool> {
    stored
        .get(uid)
        .expect("get")
        .map(|record| record.restart_pending)
}

/// A full report from `uid` describing itself with `pairs`, over a certificate naming `host`
/// whose subject carries `subject`.
pub(super) fn report_over(
    state: &AppState,
    host: &str,
    subject: crate::revocation::SubjectValues,
    uid: InstanceUid,
    pairs: &[(&str, &str)],
) -> Processed {
    let mut message = report(&uid, 1);
    message
        .agent_description
        .as_mut()
        .expect("described")
        .non_identifying_attributes = pairs
        .iter()
        .map(|(key, value)| opamp::attributes::string_attr(key, value))
        .collect();
    state.process_presented(
        message,
        Transport::Http,
        None,
        Some(&Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: subject,
            not_after_ms: u64::MAX,
        }),
    )
}

pub(super) fn restartable(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
    let mut msg = report(uid, sequence_num);
    msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
    msg
}

/// A store passing every call to `inner`, whose next reads of one record can be made to fail,
/// and which can run an act once right after a read of one record: what lets a test place
/// another instance's write between the read and what is done with it. It counts the reads of
/// one record and the reads of several, and its re-keys can be made to fail.
pub(super) struct Hooked {
    inner: Box<dyn AgentStore>,
    pub(super) failing_gets: Arc<AtomicU64>,
    pub(super) after_get: Arc<Mutex<Option<Act>>>,
    /// What runs once after a batched read of records, before it is answered.
    pub(super) after_get_many: Arc<Mutex<Option<Act>>>,
    pub(super) loads: Arc<AtomicU64>,
    pub(super) whole_queries: Arc<AtomicU64>,
    pub(super) failing_rekeys: Arc<std::sync::atomic::AtomicBool>,
    /// Re-keys written whose answer is then lost, as a connection dropped after the commit.
    pub(super) losing_rekeys: Arc<std::sync::atomic::AtomicBool>,
    /// Read backs of a re-key that fail, as a database that does not answer.
    pub(super) failing_moved: Arc<std::sync::atomic::AtomicBool>,
    /// The read backs of a re-key asked for.
    pub(super) moved_reads: Arc<AtomicU64>,
    pub(super) after_holders: Arc<Mutex<Option<Act>>>,
    /// Takes written whose answer is then lost, as a connection dropped after the commit.
    pub(super) losing_takes: Arc<std::sync::atomic::AtomicBool>,
    /// Takes refused before anything is written, as a database that does not answer.
    pub(super) failing_takes: Arc<std::sync::atomic::AtomicBool>,
    /// What runs once after a presence batch committed, before its answers are applied.
    pub(super) after_presence: Arc<Mutex<Option<Act>>>,
    /// The reads of holders and counts, and what runs once before and after the next of them.
    pub(super) takes_reads: Arc<AtomicU64>,
    pub(super) before_takes: Arc<Mutex<Option<Act>>>,
    pub(super) after_takes: Arc<Mutex<Option<Act>>>,
    /// Restarts set queued that fail, as a database that does not answer, and how many were
    /// asked for.
    pub(super) failing_restarts: Arc<std::sync::atomic::AtomicBool>,
    pub(super) restarts_set: Arc<AtomicU64>,
    /// What runs before every call to the store, given the call and the Agent it names.
    pub(super) probe: Arc<Mutex<Option<Probe>>>,
    /// What runs once after a take committed, before it is answered.
    pub(super) after_take: Arc<Mutex<Option<Act>>>,
    /// What runs once after a re-key committed, before it is answered.
    pub(super) after_rekey: Arc<Mutex<Option<Act>>>,
    /// What runs once after a queued restart was cleared, before the clear is answered.
    pub(super) after_clear: Arc<Mutex<Option<Act>>>,
}

/// What [`Hooked`] runs once after a read.
pub(super) type Act = Box<dyn FnOnce() + Send>;

/// What [`Hooked`] runs before every call to the store.
pub(super) type Probe = Arc<dyn Fn(&'static str, Option<&InstanceUid>) + Send + Sync>;

/// Runs the probe set in `probe`, if one is, for `call` naming `uid`.
fn probed(probe: &Mutex<Option<Probe>>, call: &'static str, uid: Option<&InstanceUid>) {
    let probe = probe.lock().expect("hook lock").clone();
    if let Some(probe) = probe {
        probe(call, uid);
    }
}

impl Hooked {
    pub(super) fn new(inner: Box<dyn AgentStore>) -> Self {
        Hooked {
            inner,
            failing_gets: Arc::new(AtomicU64::new(0)),
            after_get: Arc::new(Mutex::new(None)),
            after_get_many: Arc::new(Mutex::new(None)),
            loads: Arc::new(AtomicU64::new(0)),
            whole_queries: Arc::new(AtomicU64::new(0)),
            failing_rekeys: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            losing_rekeys: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            failing_moved: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            moved_reads: Arc::new(AtomicU64::new(0)),
            after_holders: Arc::new(Mutex::new(None)),
            losing_takes: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            failing_takes: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            after_presence: Arc::new(Mutex::new(None)),
            takes_reads: Arc::new(AtomicU64::new(0)),
            before_takes: Arc::new(Mutex::new(None)),
            after_takes: Arc::new(Mutex::new(None)),
            failing_restarts: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            restarts_set: Arc::new(AtomicU64::new(0)),
            probe: Arc::new(Mutex::new(None)),
            after_take: Arc::new(Mutex::new(None)),
            after_rekey: Arc::new(Mutex::new(None)),
            after_clear: Arc::new(Mutex::new(None)),
        }
    }
}

impl AgentStore for Hooked {
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.inner.load()
    }
    fn get(&self, uid: &InstanceUid) -> Result<Option<PersistedAgent>, String> {
        probed(&self.probe, "get", Some(uid));
        let failing = self
            .failing_gets
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Err("the read is held back".to_string());
        }
        let got = self.inner.get(uid);
        let after = self.after_get.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        got
    }
    fn get_many(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        probed(&self.probe, "get_many", None);
        let got = self.inner.get_many(uids);
        let after = self.after_get_many.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        got
    }
    fn query(
        &self,
        only: Option<&[InstanceUid]>,
    ) -> Result<Vec<(InstanceUid, crate::agent_store::StoredAgent)>, String> {
        if only.is_none() {
            self.whole_queries.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.query(only)
    }
    fn query_page(
        &self,
        query: &crate::agent_store::PageQuery<'_>,
    ) -> Result<crate::agent_store::AgentPage, String> {
        probed(&self.probe, "query_page", None);
        self.inner.query_page(query)
    }
    fn reported_values(&self, limit: usize) -> Result<crate::agent_store::ReportedValues, String> {
        self.inner.reported_values(limit)
    }
    fn is_shared(&self) -> bool {
        self.inner.is_shared()
    }
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
        self.inner.put(uid, record)
    }
    fn write_report(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<bool, String> {
        probed(&self.probe, "write_report", Some(uid));
        self.inner.write_report(uid, record)
    }
    fn write_report_unless_held_elsewhere(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        holder: Option<&str>,
        take: Option<u64>,
    ) -> Result<bool, String> {
        self.inner
            .write_report_unless_held_elsewhere(uid, record, holder, take)
    }
    fn create_within(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        max_records: usize,
    ) -> Result<Creation, String> {
        probed(&self.probe, "create_within", Some(uid));
        self.inner.create_within(uid, record, max_records)
    }
    fn set_labels(
        &self,
        uid: &InstanceUid,
        labels: &BTreeMap<String, String>,
    ) -> Result<bool, String> {
        self.inner.set_labels(uid, labels)
    }
    fn set_restart_pending(
        &self,
        uid: &InstanceUid,
        pending: bool,
    ) -> Result<Option<bool>, String> {
        probed(&self.probe, "set_restart_pending", Some(uid));
        if pending {
            self.restarts_set.fetch_add(1, Ordering::SeqCst);
            if self.failing_restarts.load(Ordering::SeqCst) {
                return Err("the restart is held back".to_string());
            }
        }
        let set = self.inner.set_restart_pending(uid, pending);
        if !pending {
            let after = self.after_clear.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
        }
        set
    }
    fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
        self.inner.remove(uid)
    }
    fn write_presence(
        &self,
        holder: &str,
        presence: &[(InstanceUid, Presence, Option<u64>)],
    ) -> Result<HashMap<InstanceUid, u64>, String> {
        let written = self.inner.write_presence(holder, presence);
        let after = self.after_presence.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        written
    }
    fn takes(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, (Option<String>, u64)>, String> {
        self.takes_reads.fetch_add(1, Ordering::SeqCst);
        let before = self.before_takes.lock().expect("hook lock").take();
        if let Some(before) = before {
            before();
        }
        let takes = self.inner.takes(uids);
        let after = self.after_takes.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        takes
    }
    fn remove_if_silent(
        &self,
        uid: &InstanceUid,
        silent_before_ms: u64,
    ) -> Result<Option<crate::agent_store::Silenced>, String> {
        self.inner.remove_if_silent(uid, silent_before_ms)
    }
    fn take(
        &self,
        uid: &InstanceUid,
        holder: &str,
        last_seen_ms: u64,
    ) -> Result<Option<crate::agent_store::Taken>, String> {
        probed(&self.probe, "take", Some(uid));
        if self.failing_takes.load(Ordering::SeqCst) {
            return Err("the take is not written".to_string());
        }
        let taken = self.inner.take(uid, holder, last_seen_ms);
        let after = self.after_take.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        if self.losing_takes.load(Ordering::SeqCst) {
            return Err("the answer of the take is lost".to_string());
        }
        taken
    }
    fn holders(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, Option<String>>, String> {
        probed(&self.probe, "holders", None);
        let holders = self.inner.holders(uids);
        let after = self.after_holders.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        holders
    }
    fn rekey(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), String> {
        probed(&self.probe, "rekey", Some(old));
        if self.failing_rekeys.load(Ordering::SeqCst) {
            return Err("the move is held back".to_string());
        }
        self.inner.rekey(old, new, record)?;
        let after = self.after_rekey.lock().expect("hook lock").take();
        if let Some(after) = after {
            after();
        }
        if self.losing_rekeys.load(Ordering::SeqCst) {
            return Err("the move's answer is lost".to_string());
        }
        Ok(())
    }
    fn moved(&self, old: &InstanceUid, new: &InstanceUid) -> Result<bool, String> {
        probed(&self.probe, "moved", Some(old));
        self.moved_reads.fetch_add(1, Ordering::SeqCst);
        if self.failing_moved.load(Ordering::SeqCst) {
            return Err("the read back is held back".to_string());
        }
        self.inner.moved(old, new)
    }
    fn release(
        &self,
        revisions: &[crate::configs::Pinned],
        agents: &[(InstanceUid, PersistedAgent)],
    ) -> Result<Vec<InstanceUid>, String> {
        self.inner.release(revisions, agents)
    }
    fn pinning(&self, pin: Pin<'_>) -> usize {
        self.inner.pinning(pin)
    }
    fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
        self.inner.referenced_revisions(name)
    }
}

/// An instance on `database`, its Agent records in `agents`, and the host register on the
/// database read at once: what another instance binds after this is not known to it.
pub(super) fn on_postgres(
    database: &Arc<crate::postgres::Database>,
    agents: Box<dyn AgentStore>,
) -> AppState {
    let revocations = Revocations::open(
        Box::new(crate::postgres::ledger::PgLedgerStore::new(
            database.clone(),
        )),
        Arc::new(crate::clock::SystemClock),
        Vec::new(),
    )
    .expect("revocations");
    AppState::with_stores(
        agents,
        Box::new(crate::postgres::configs::PgConfigBackend::new(
            database.clone(),
        )),
        Box::new(crate::clock::SystemClock),
    )
    .expect("state")
    .with_revocations(Some(Arc::new(revocations)))
}

/// An instance on PostgreSQL with package delivery, its blobs under `dir`.
pub(super) fn delivering_on(
    database: &Arc<crate::postgres::Database>,
    agents: Box<dyn AgentStore>,
    dir: &std::path::Path,
) -> AppState {
    let records = crate::Records::Postgres(database.clone());
    let (packages, _) = PackageStore::open_on(
        &records,
        Some(dir),
        &toml::from_str::<crate::config::BlobsConfig>("").expect("blobs"),
        crate::clock::Elapsed::monotonic(),
    )
    .expect("packages");
    on_postgres(database, agents).with_packages(Some(
        PackageOffering::on(&records, packages).expect("deployments"),
    ))
}

/// A Deployment `early`, selecting `ring=early`, that holds a Configuration for `otelcol`.
pub(super) fn aim_at_the_early_ring(state: &AppState) {
    state
        .save_configuration(
            "collector",
            Revision {
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: "otelcol".to_string(),
            },
        )
        .expect("save");
    state
        .deployment_store()
        .expect("deployments")
        .put(
            "early",
            BTreeMap::from([("ring".to_string(), "early".to_string())]),
        )
        .expect("deployment");
    state
        .put_deployment_configuration("early", "collector")
        .expect("hold it");
}

/// A certificate of `host`.
pub(super) fn of_host(host: &str) -> Presented {
    Presented {
        id: CertId::new(b"CA", host),
        host: Some(host.to_string()),
        subject_values: crate::revocation::SubjectValues::default(),
        not_after_ms: u64::MAX,
    }
}

/// A state on files with a host register whose writes of the Hosts run `probe` first, and the
/// state `probe` may reach once it is made.
pub(super) fn with_probed_register(
    dir: &std::path::Path,
    probe: Probe,
    of: &Arc<OnceLock<std::sync::Weak<AppState>>>,
) -> Arc<AppState> {
    let revocations = Arc::new(
        Revocations::open(
            Box::new(ProbedLedger {
                inner: Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                probe,
            }),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations"),
    );
    let state = Arc::new(
        AppState::new(dir.join("configs"))
            .expect("state")
            .with_revocations(Some(revocations)),
    );
    of.set(Arc::downgrade(&state)).expect("set once");
    state
}

pub(super) const LEASE_A: &str = "00000000-0000-7000-8000-00000000000a";

pub(super) const LEASE_B: &str = "00000000-0000-7000-8000-00000000000b";

/// Waits until `done`, and fails naming `what` after 30 seconds without.
pub(super) fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A ledger store that runs a probe before each write a report makes of the host register:
/// binding an `instance_uid`, writing a Host and moving a binding.
pub(super) struct ProbedLedger {
    pub(super) inner: Box<dyn crate::revocation::LedgerStore>,
    pub(super) probe: Probe,
}

impl crate::revocation::LedgerStore for ProbedLedger {
    fn load(&self) -> Result<crate::revocation::Ledger, String> {
        self.inner.load()
    }
    fn put_issued(&self, issued: &crate::revocation::Issued) -> Result<(), String> {
        self.inner.put_issued(issued)
    }
    fn remove_issued(&self, id: &CertId, now_ms: u64) -> Result<bool, String> {
        self.inner.remove_issued(id, now_ms)
    }
    fn register_issued(
        &self,
        issued: &crate::revocation::Issued,
        bounds: &crate::revocation::RegisterBounds,
    ) -> Result<crate::revocation::Registered, String> {
        self.inner.register_issued(issued, bounds)
    }
    fn register_minting(
        &self,
        issued: &crate::revocation::Issued,
        bounds: &crate::revocation::RegisterBounds,
        mint: Option<(&str, &crate::revocation::Host)>,
        bootstrap: Option<&CertId>,
    ) -> Result<crate::revocation::Registered, String> {
        self.inner.register_minting(issued, bounds, mint, bootstrap)
    }
    fn add_revocation(
        &self,
        entry: &crate::revocation::Revocation,
        bound: usize,
    ) -> Result<crate::revocation::Added, String> {
        self.inner.add_revocation(entry, bound)
    }
    fn list_version(&self) -> Option<u64> {
        self.inner.list_version()
    }
    fn remove_revocation(&self, id: &str) -> Result<bool, String> {
        self.inner.remove_revocation(id)
    }
    fn bind(&self, instance_uid: &str, host: &str) -> Result<crate::revocation::Bound, String> {
        (self.probe)("bind", None);
        self.inner.bind(instance_uid, host)
    }
    fn bind_seen(
        &self,
        instance_uid: &str,
        host: &str,
        row: &crate::revocation::Host,
    ) -> Result<crate::revocation::Bound, String> {
        (self.probe)("bind_seen", None);
        self.inner.bind_seen(instance_uid, host, row)
    }
    fn unbind(&self, instance_uid: &str) -> Result<Option<String>, String> {
        self.inner.unbind(instance_uid)
    }
    fn rebind(
        &self,
        old: &str,
        new: &str,
    ) -> Result<Option<(String, crate::revocation::Bound)>, String> {
        (self.probe)("rebind", None);
        self.inner.rebind(old, new)
    }
    fn unbind_host(&self, host: &str) -> Result<(), String> {
        self.inner.unbind_host(host)
    }
    fn writes_hosts_by_act(&self) -> bool {
        self.inner.writes_hosts_by_act()
    }
    fn write_host_act(&self, act: &crate::revocation::HostAct) -> Result<bool, String> {
        (self.probe)("write_host_act", None);
        self.inner.write_host_act(act)
    }
    fn save_hosts(&self, hosts: &BTreeMap<String, crate::revocation::Host>) -> Result<(), String> {
        (self.probe)("save_hosts", None);
        self.inner.save_hosts(hosts)
    }
}

/// The Deployment the stored assignment of `uid` was released through.
pub(super) fn released_through(
    database: &Arc<crate::postgres::Database>,
    uid: &InstanceUid,
) -> Option<String> {
    crate::postgres::agents::PgAgentStore::new(database.clone())
        .get(uid)
        .expect("read")
        .and_then(|stored| stored.assignment)
        .map(|assignment| assignment.deployment)
}
