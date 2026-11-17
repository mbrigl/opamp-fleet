//! The test helpers the contexts of `fleet` share: an Agent put into the fleet, a report, the
//! view of one Agent, labels, the list read a page at a time, and a clock the test moves.

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
