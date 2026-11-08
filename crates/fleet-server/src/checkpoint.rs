//! Signed checkpoints of the heads of the audit chains
//! (ADR-0041).
//!
//! About once a minute, and at an orderly shutdown, the process that signs writes the entry
//! `audit.checkpoint` into its own chain. It names the head of every chain and is signed with the
//! audit key on the PKCS#11 token. Whoever can rewrite the store but cannot use the token cannot
//! cut, remove or rewrite a chain before the newest checkpoint unseen: `server audit-verify
//! --audit-key` checks every chain against the checkpoints.
//!
//! This module holds what both sides share: the entry's fields, the signer port the writer asks
//! through, the port that tells the writer which heads a checkpoint names, and the check
//! `audit-verify` runs over the checkpoints a record holds.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit::{Entry, Field};
use crate::audit_log::{line_hash, GENESIS};

/// How often the signer signs a checkpoint.
pub const INTERVAL: Duration = Duration::from_secs(60);

/// How long the writer waits for the token's signature before it takes the checkpoint back.
pub const SIGN_WAIT: Duration = Duration::from_secs(2);

/// The entry a checkpoint is.
pub const CHECKPOINT: &str = "audit.checkpoint";

/// The entry a checkpoint that could not be signed or written leaves instead.
pub const FAILED: &str = "audit.checkpoint_failed";

/// The name the filesystem store's one chain has in a checkpoint.
pub const FS_CHAIN: &str = "fs";

/// How many intervals past the newest checkpoint an entry may be written before it counts as an
/// unsigned tail.
pub const TAIL_INTERVALS: u32 = 3;

/// The signer port for checkpoints
/// (ADR-0043
/// clause 5): the audit key, by its label, with its public half.
pub trait CheckpointSigner: Send + Sync {
    /// The key's label.
    fn label(&self) -> &str;

    /// The public half as DER `SubjectPublicKeyInfo`.
    fn public_key_der(&self) -> Vec<u8>;

    /// Signs the SHA-256 `digest` with ECDSA P-256: one `C_Sign`, answered as a DER
    /// `ECDSA-Sig-Value`.
    ///
    /// # Errors
    /// Returns the token's error when it does not sign.
    fn sign_digest(&self, digest: &[u8]) -> Result<Vec<u8>, String>;
}

/// The SHA-256, hex, of a public key as DER `SubjectPublicKeyInfo`: what a checkpoint's `key`
/// names.
#[must_use]
pub fn key_sha256(spki: &[u8]) -> String {
    hex::encode(Sha256::digest(spki))
}

/// A public key as DER `SubjectPublicKeyInfo`, as PEM.
#[must_use]
pub fn public_pem(spki: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(spki);
    let mut pem = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(chunk));
        pem.push('\n');
    }
    pem.push_str("-----END PUBLIC KEY-----\n");
    pem
}

/// The head of one chain, as a checkpoint names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Head {
    pub chain: String,
    /// The `seq` of its last entry, 0 for a chain that holds none.
    pub seq: u64,
    /// That entry's hash, 64 zeros for a chain that holds none.
    pub hash: String,
    pub closed: bool,
}

impl Head {
    fn to_json(&self) -> Value {
        json!({"chain": self.chain, "seq": self.seq, "hash": self.hash, "closed": self.closed})
    }

    fn from_json(value: &Value) -> Option<Head> {
        Some(Head {
            chain: value.get("chain")?.as_str()?.to_string(),
            seq: value.get("seq")?.as_u64()?,
            hash: value.get("hash")?.as_str()?.to_string(),
            closed: value.get("closed")?.as_bool()?,
        })
    }
}

/// The newest checkpoint, as the signer goes on from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub checkpoint_seq: u64,
    /// The hash of its line as written.
    pub hash: String,
    /// The line, where the store still holds it: a pruned or rotated one is known by its
    /// `checkpoint_seq` and hash alone.
    pub line: Option<String>,
}

impl Tip {
    /// The heads the line names; none when the line is gone.
    #[must_use]
    pub fn heads(&self) -> Option<Vec<Head>> {
        self.line
            .as_deref()
            .and_then(|line| parse(line).ok())
            .map(|parsed| parsed.heads)
    }
}

/// What the signer found before a checkpoint: the newest one, and the heads of the chains the
/// store holds besides the signer's own, which the writer adds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    pub tip: Option<Tip>,
    pub heads: Vec<Head>,
}

/// Where the signer reads what a checkpoint names, and whether this process signs now.
pub trait CheckpointSource: Send {
    /// The newest checkpoint and the heads of every chain to name, or `None` when this process
    /// does not sign now.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read.
    fn prepare(&mut self) -> Result<Option<Prepared>, String>;

    /// Told once a checkpoint is written, as `tip`.
    fn written(&mut self, tip: Tip) {
        let _ = tip;
    }
}

/// The source of the filesystem store: the Server signs, its own chain is the only one, and the
/// newest checkpoint is the one it wrote last or found at startup.
#[derive(Debug)]
pub struct OneChain {
    tip: Option<Tip>,
}

impl OneChain {
    /// The source that goes on from `tip`, the newest checkpoint the record holds.
    #[must_use]
    pub fn new(tip: Option<Tip>) -> Self {
        OneChain { tip }
    }
}

impl CheckpointSource for OneChain {
    fn prepare(&mut self) -> Result<Option<Prepared>, String> {
        Ok(Some(Prepared {
            tip: self.tip.clone(),
            heads: Vec::new(),
        }))
    }

    fn written(&mut self, tip: Tip) {
        self.tip = Some(tip);
    }
}

/// The checkpoint entry before it is signed: it gets its `seq`, `time`, `prev` and, on
/// PostgreSQL, `chain` as every entry does.
#[must_use]
pub fn unsigned_entry(
    checkpoint_seq: u64,
    checkpoint_prev: &str,
    heads: &[Head],
    label: &str,
    key_sha256: &str,
) -> Entry {
    let mut entry = Entry::new(CHECKPOINT, "signed")
        .with("checkpoint_seq", checkpoint_seq)
        .with("checkpoint_prev", checkpoint_prev.to_string());
    entry.fields.insert(
        "heads".into(),
        Field::Json(Value::Array(heads.iter().map(Head::to_json).collect()).to_string()),
    );
    entry.fields.insert(
        "key".into(),
        Field::Json(json!({"label": label, "sha256": key_sha256}).to_string()),
    );
    entry
}

/// The entry a checkpoint that was not written leaves, naming why.
#[must_use]
pub fn failed_entry(error: &str) -> Entry {
    Entry::new(FAILED, "failed").with("error", error.to_string())
}

/// The SHA-256 a checkpoint's `sig` signs: of its line as written, without `sig`.
///
/// # Errors
/// Returns an error when the line is no JSON object.
pub fn signed_digest(line: &str) -> Result<[u8; 32], String> {
    let mut value: Value =
        serde_json::from_str(line).map_err(|e| format!("not an audit entry: {e}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "not an audit entry: no JSON object".to_string())?;
    object.remove("sig");
    Ok(Sha256::digest(value.to_string().as_bytes()).into())
}

/// A checkpoint as its line holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub checkpoint_seq: u64,
    pub checkpoint_prev: String,
    pub heads: Vec<Head>,
    pub key_label: String,
    pub key_sha256: String,
    pub sig: Vec<u8>,
    /// When it was written, in milliseconds since the epoch.
    pub time_ms: u64,
}

/// Reads a checkpoint's line.
///
/// # Errors
/// Names the first field that is missing or malformed.
pub fn parse(line: &str) -> Result<Parsed, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| format!("not an audit entry: {e}"))?;
    if value.get("event").and_then(Value::as_str) != Some(CHECKPOINT) {
        return Err(format!("not an {CHECKPOINT}"));
    }
    let missing = |key: &str| format!("the checkpoint has no valid {key}");
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let heads = value
        .get("heads")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("heads"))?
        .iter()
        .map(|head| Head::from_json(head).ok_or_else(|| missing("head")))
        .collect::<Result<Vec<_>, _>>()?;
    let key = value.get("key").ok_or_else(|| missing("key"))?;
    Ok(Parsed {
        checkpoint_seq: value
            .get("checkpoint_seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| missing("checkpoint_seq"))?,
        checkpoint_prev: text("checkpoint_prev")
            .ok_or_else(|| missing("checkpoint_prev"))?
            .to_string(),
        heads,
        key_label: key
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| missing("key label"))?
            .to_string(),
        key_sha256: key
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| missing("key sha256"))?
            .to_string(),
        sig: text("sig")
            .and_then(|sig| base64::engine::general_purpose::STANDARD.decode(sig).ok())
            .ok_or_else(|| missing("sig"))?,
        time_ms: time_ms(&value).ok_or_else(|| missing("time"))?,
    })
}

/// The `time` of an entry, in milliseconds since the epoch.
fn time_ms(value: &Value) -> Option<u64> {
    let time = value.get("time").and_then(Value::as_str)?;
    let parsed =
        time::OffsetDateTime::parse(time, &time::format_description::well_known::Rfc3339).ok()?;
    u64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

/// The `checkpoint_seq` and `checkpoint_hash` an `audit.rotated` or `audit.pruned` names as the
/// last checkpoint it removed.
#[must_use]
pub fn removed_checkpoint(value: &Value) -> Option<(u64, String)> {
    Some((
        value.get("checkpoint_seq")?.as_u64()?,
        value.get("checkpoint_hash")?.as_str()?.to_string(),
    ))
}

/// The audit key a verifier checks against: its public half as PEM.
#[derive(Clone, Debug)]
pub struct AuditKey {
    spki: Vec<u8>,
    /// The uncompressed P-256 point inside it.
    point: Vec<u8>,
    sha256: String,
}

impl AuditKey {
    /// Reads the public key PEM `pem`.
    ///
    /// # Errors
    /// Returns an error when it is no PEM `PUBLIC KEY` holding a P-256 key.
    pub fn from_pem(pem: &[u8]) -> Result<Self, String> {
        // One block, read here: the text between its armour lines is base64 of the DER.
        let text = std::str::from_utf8(pem).map_err(|_| "the audit key is no PEM".to_string())?;
        let (_, rest) = text
            .split_once("-----BEGIN PUBLIC KEY-----")
            .ok_or_else(|| "the audit key's PEM holds no PUBLIC KEY".to_string())?;
        let (body, _) = rest
            .split_once("-----END PUBLIC KEY-----")
            .ok_or_else(|| "the audit key's PEM does not end its PUBLIC KEY".to_string())?;
        let body: String = body.split_whitespace().collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| format!("the audit key's PEM is no base64: {e}"))?;
        Self::from_der(&der)
    }

    /// Reads the public key from DER `SubjectPublicKeyInfo`.
    ///
    /// # Errors
    /// Returns an error when it is no P-256 key.
    pub fn from_der(spki: &[u8]) -> Result<Self, String> {
        use x509_parser::prelude::FromDer as _;
        let (rest, info) = x509_parser::x509::SubjectPublicKeyInfo::from_der(spki)
            .map_err(|e| format!("the audit key is no public key: {e}"))?;
        if !rest.is_empty() {
            return Err("the audit key has bytes after its public key".to_string());
        }
        let curve = info
            .algorithm
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.as_oid().ok());
        if info.algorithm.algorithm != x509_parser::oid_registry::OID_KEY_TYPE_EC_PUBLIC_KEY
            || curve != Some(x509_parser::oid_registry::OID_EC_P256)
        {
            return Err(
                "the audit key is not an ECDSA key on the curve P-256, as the audit key is"
                    .to_string(),
            );
        }
        let point = info.subject_public_key.data.to_vec();
        if point.len() != 65 || point[0] != 4 {
            return Err("the audit key is not an uncompressed P-256 key".to_string());
        }
        Ok(AuditKey {
            spki: spki.to_vec(),
            point,
            sha256: key_sha256(spki),
        })
    }

    /// The SHA-256 of the key a checkpoint names.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Whether `sig` on `line` verifies against this key.
    fn verifies(&self, line: &str, sig: &[u8]) -> bool {
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        if let Some(object) = value.as_object_mut() {
            object.remove("sig");
        }
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            &self.point,
        )
        .verify(value.to_string().as_bytes(), sig)
        .is_ok()
    }

    /// The key as DER `SubjectPublicKeyInfo`.
    #[must_use]
    pub fn der(&self) -> &[u8] {
        &self.spki
    }
}

/// One checkpoint the record holds, where it is held, and what it names.
#[derive(Clone, Debug)]
struct Held {
    chain: String,
    seq: u64,
    hash: String,
    line: String,
    parsed: Parsed,
}

/// What `server audit-verify --audit-key` found over the checkpoints: what to print, whether it
/// failed, and the newest checkpoint's line for the operator to keep.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub failed: bool,
    pub newest: Option<String>,
}

/// An `audit.rotated` or `audit.pruned` entry, where it stands and what it names as removed.
#[derive(Clone, Debug)]
struct Removal {
    chain: String,
    seq: u64,
    event: String,
    /// An `audit.pruned`'s chain, and the `seq` and hash of the last entry removed from it.
    entries: Option<(String, u64, String)>,
    /// An `audit.rotated`'s hash of the last line of the file it deleted.
    rotated: Option<String>,
    /// The last checkpoint it removed.
    checkpoint: Option<(u64, String)>,
}

/// The check of a record against its checkpoints. It is fed in two passes: first every checkpoint,
/// every `audit.checkpoint_failed` and every `audit.rotated` and `audit.pruned`, then, after
/// [`plan`](Self::plan), every entry of every chain.
///
/// Only a signed removal counts: an `audit.rotated` or `audit.pruned` is honoured only where a
/// checkpoint names a head of its chain at or after it and the store still holds that head with
/// the hash the checkpoint names; one in the unsigned tail or inside a cut run is reported and
/// never honoured
/// (ADR-0041 clause 4).
#[derive(Debug)]
pub struct Check {
    key: AuditKey,
    since: Option<String>,
    held: Vec<Held>,
    failed_ms: Vec<u64>,
    removals: Vec<Removal>,
    /// After the plan: the last checkpoint a signed removal names.
    removed: Vec<(u64, String)>,
    /// After the plan: what each chain's entries were removed up to, by a signed `audit.pruned`.
    removed_entries: HashMap<String, Vec<(u64, String)>>,
    /// After the plan: the hash of the last line of each file a signed `audit.rotated` deleted.
    rotated: Vec<String>,
    report: Report,
    /// After the plan: the newest checkpoint's time, and the heads to find.
    newest_ms: Option<u64>,
    targets: BTreeMap<(String, u64), (String, u64)>,
    found: HashMap<(String, u64), String>,
    last_seq: HashMap<String, u64>,
    /// The first entry each chain holds: its `seq` and its `prev`.
    first: HashMap<String, (u64, String)>,
    unsigned: Option<String>,
}

impl Check {
    /// A check against `key`, from the saved checkpoint line `since` where one is given.
    #[must_use]
    pub fn new(key: AuditKey, since: Option<String>) -> Self {
        Check {
            key,
            since: since.map(|line| line.trim().to_string()),
            held: Vec::new(),
            failed_ms: Vec::new(),
            removals: Vec::new(),
            removed: Vec::new(),
            removed_entries: HashMap::new(),
            rotated: Vec::new(),
            report: Report::default(),
            newest_ms: None,
            targets: BTreeMap::new(),
            found: HashMap::new(),
            last_seq: HashMap::new(),
            first: HashMap::new(),
            unsigned: None,
        }
    }

    fn fail(&mut self, line: String) {
        self.report.failed = true;
        self.report.lines.push(line);
    }

    /// Takes the entry `line`, stored as `seq` of `chain`, in the first pass: a checkpoint, a
    /// failed one, or a rotation or prune. Every other entry is passed over.
    pub fn collect(&mut self, chain: &str, seq: u64, line: &str) {
        // Most entries are none of these; the cheap test first.
        if !line.contains("\"event\":\"audit.") {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match value.get("event").and_then(Value::as_str) {
            Some(CHECKPOINT) => match parse(line) {
                Ok(parsed) => self.held.push(Held {
                    chain: chain.to_string(),
                    seq,
                    hash: line_hash(line),
                    line: line.to_string(),
                    parsed,
                }),
                Err(e) => self.fail(format!("chain {chain}, entry {seq}: {e}")),
            },
            Some(FAILED) => self.failed_ms.extend(time_ms(&value)),
            Some(event @ ("audit.rotated" | "audit.pruned")) => {
                let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
                self.removals.push(Removal {
                    chain: chain.to_string(),
                    seq,
                    event: event.to_string(),
                    entries: (|| {
                        Some((
                            text("pruned_chain")?,
                            value.get("last_seq")?.as_u64()?,
                            text("last_hash")?,
                        ))
                    })(),
                    rotated: text("deleted_last_hash"),
                    checkpoint: removed_checkpoint(&value),
                });
            }
            _ => {}
        }
    }

    /// Honours the removals a checkpoint that verifies covers, and reports the others.
    ///
    /// A head covers a removal only where the store still holds it with the hash its checkpoint
    /// names: the chain's links from the removal to that head then prove the removal was there
    /// when the head was signed. A removal inside a run that was cut is covered by no head.
    fn sign_removals(&mut self) {
        let mut covered: HashMap<String, u64> = HashMap::new();
        for checkpoint in self.held.iter().filter(|checkpoint| {
            checkpoint.parsed.key_sha256 == self.key.sha256
                && self.key.verifies(&checkpoint.line, &checkpoint.parsed.sig)
        }) {
            for head in &checkpoint.parsed.heads {
                if self.found.get(&(head.chain.clone(), head.seq)) != Some(&head.hash) {
                    continue;
                }
                let to = covered.entry(head.chain.clone()).or_insert(0);
                *to = (*to).max(head.seq);
            }
        }
        for removal in std::mem::take(&mut self.removals) {
            if covered
                .get(&removal.chain)
                .is_none_or(|to| *to < removal.seq)
            {
                self.report.lines.push(format!(
                    "chain {}, entry {}: {} lies after every signed head the record still holds \
                     of its chain, so it is not honoured — a removal counts once a checkpoint \
                     covers it",
                    removal.chain, removal.seq, removal.event
                ));
                continue;
            }
            self.removed.extend(removal.checkpoint);
            if let Some((chain, last_seq, last_hash)) = removal.entries {
                self.removed_entries
                    .entry(chain)
                    .or_default()
                    .push((last_seq, last_hash));
            }
            self.rotated.extend(removal.rotated);
        }
    }

    /// Checks the checkpoints among themselves: each signature, the key each names, their order
    /// and the saved one; and settles which heads the second pass looks for.
    pub fn plan(&mut self) {
        let mut held = std::mem::take(&mut self.held);
        held.sort_by_key(|held| held.parsed.checkpoint_seq);
        for checkpoint in &held {
            let at = format!(
                "checkpoint {} (chain {}, entry {})",
                checkpoint.parsed.checkpoint_seq, checkpoint.chain, checkpoint.seq
            );
            if checkpoint.parsed.key_sha256 != self.key.sha256 {
                self.fail(format!(
                    "{at}: it names the key {:?} with SHA-256 {}, not the key given",
                    checkpoint.parsed.key_label, checkpoint.parsed.key_sha256
                ));
            } else if !self.key.verifies(&checkpoint.line, &checkpoint.parsed.sig) {
                self.fail(format!(
                    "{at}: its signature does not verify against the key given"
                ));
            }
        }
        for pair in held.windows(2) {
            let (before, after) = (&pair[0], &pair[1]);
            let at = format!(
                "checkpoint {} (chain {}, entry {})",
                after.parsed.checkpoint_seq, after.chain, after.seq
            );
            if after.parsed.checkpoint_seq == before.parsed.checkpoint_seq {
                self.fail(format!(
                    "{at}: the record holds two checkpoints {}",
                    after.parsed.checkpoint_seq
                ));
            } else if after.parsed.checkpoint_seq != before.parsed.checkpoint_seq + 1 {
                self.fail(format!(
                    "{at}: it follows checkpoint {} — the checkpoints between are missing",
                    before.parsed.checkpoint_seq
                ));
            } else if after.parsed.checkpoint_prev != before.hash {
                self.fail(format!(
                    "{at}: its checkpoint_prev does not match checkpoint {}",
                    before.parsed.checkpoint_seq
                ));
            }
        }
        // Every head any checkpoint names, the newest one's and every earlier one's.
        if let Some(newest) = held.last() {
            self.newest_ms = Some(newest.parsed.time_ms);
            self.report.newest = Some(newest.line.clone());
        }
        for checkpoint in held.iter().rev() {
            for head in checkpoint.parsed.heads.iter().filter(|head| head.seq > 0) {
                self.targets
                    .entry((head.chain.clone(), head.seq))
                    .or_insert_with(|| (head.hash.clone(), checkpoint.parsed.checkpoint_seq));
            }
        }
        self.failed_ms.sort_unstable();
        if let Some(newest) = held.last() {
            self.report.lines.push(format!(
                "{} checkpoints, the newest {} of {} signed by the key {:?}",
                held.len(),
                newest.parsed.checkpoint_seq,
                rfc3339(newest.parsed.time_ms),
                newest.parsed.key_label
            ));
        }
        self.held = held;
    }

    /// Once every entry was observed: the removals the record proves, the first checkpoint
    /// held against the last one they removed, and the saved one.
    fn signed_removals(&mut self) {
        self.sign_removals();
        let held = std::mem::take(&mut self.held);
        let last_removed = self.removed.iter().max().cloned();
        match held.first() {
            None => self.fail(
                "the record holds no checkpoint — with an audit key, every record holds them; \
                 the checkpoints are missing"
                    .to_string(),
            ),
            Some(first) => {
                let parsed = &first.parsed;
                let follows = match &last_removed {
                    Some((seq, hash)) => {
                        parsed.checkpoint_seq == seq + 1 && parsed.checkpoint_prev == *hash
                    }
                    None => parsed.checkpoint_seq == 1 && parsed.checkpoint_prev == GENESIS,
                };
                if !follows {
                    let before = last_removed.as_ref().map_or_else(
                        || "the first ever, checkpoint 1 after 64 zeros".to_string(),
                        |(seq, _)| {
                            format!("checkpoint {seq}, the last one a rotation or a prune removed")
                        },
                    );
                    self.fail(format!(
                        "checkpoint {} (chain {}, entry {}): the first checkpoint the record holds \
                         does not follow {before} — checkpoints before it are missing",
                        parsed.checkpoint_seq, first.chain, first.seq
                    ));
                }
            }
        }
        self.since(&held, last_removed.as_ref());
        self.held = held;
    }

    /// Checks the saved checkpoint `--since` names against those held.
    fn since(&mut self, held: &[Held], last_removed: Option<&(u64, String)>) {
        let Some(saved) = self.since.clone() else {
            return;
        };
        let parsed = match parse(&saved) {
            Ok(parsed) => parsed,
            Err(e) => {
                self.fail(format!("the saved checkpoint is not one: {e}"));
                return;
            }
        };
        if parsed.key_sha256 != self.key.sha256 || !self.key.verifies(&saved, &parsed.sig) {
            self.fail(format!(
                "the saved checkpoint {} does not verify against the key given",
                parsed.checkpoint_seq
            ));
            return;
        }
        let seq = parsed.checkpoint_seq;
        if let Some(found) = held.iter().find(|held| held.parsed.checkpoint_seq == seq) {
            if found.line != saved {
                self.fail(format!(
                    "checkpoint {seq} (chain {}, entry {}) is not the saved one — the record was \
                     rewritten from it on",
                    found.chain, found.seq
                ));
            } else {
                self.report.lines.push(format!(
                    "the record holds the saved checkpoint {seq} and goes on from it"
                ));
            }
            return;
        }
        let newest = held.last().map_or(0, |held| held.parsed.checkpoint_seq);
        match last_removed {
            _ if seq > newest => self.fail(format!(
                "the record holds checkpoints up to {newest}, and not the saved checkpoint {seq} — \
                 the newest checkpoints were cut"
            )),
            Some((removed, hash)) if *removed == seq => {
                if *hash == line_hash(&saved) {
                    self.report.lines.push(format!(
                        "the saved checkpoint {seq} is the last one a rotation or a prune removed, \
                         as it names it"
                    ));
                } else {
                    self.fail(format!(
                        "the saved checkpoint {seq} is not the one the rotation or prune that \
                         removed it names"
                    ));
                }
            }
            Some((removed, _)) if seq < *removed => self.report.lines.push(format!(
                "the saved checkpoint {seq} is no longer held — a rotation or a prune removed it; \
                 checked from the oldest checkpoint the record holds"
            )),
            _ => self.fail(format!(
                "the record does not hold the saved checkpoint {seq}, and no rotation or prune \
                 removed it"
            )),
        }
    }

    /// Takes one entry of `chain`, stored as `seq`, in the second pass, in each chain's order.
    pub fn observe(&mut self, chain: &str, seq: u64, line: &str) {
        let last = self.last_seq.entry(chain.to_string()).or_insert(0);
        *last = (*last).max(seq);
        if self.first.get(chain).is_none_or(|(first, _)| seq < *first) {
            let prev = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value.get("prev")?.as_str().map(str::to_string))
                .unwrap_or_default();
            self.first.insert(chain.to_string(), (seq, prev));
        }
        if self.targets.contains_key(&(chain.to_string(), seq)) {
            self.found.insert((chain.to_string(), seq), line_hash(line));
        }
        if self.unsigned.is_some() {
            return;
        }
        let Some(newest) = self.newest_ms else {
            return;
        };
        let bound =
            newest + u64::try_from((INTERVAL * TAIL_INTERVALS).as_millis()).unwrap_or(u64::MAX);
        let Some(time) = serde_json::from_str::<Value>(line)
            .ok()
            .as_ref()
            .and_then(time_ms)
        else {
            return;
        };
        let excused = self
            .failed_ms
            .iter()
            .any(|failed| *failed > newest && *failed <= time);
        if time > bound && !excused {
            self.unsigned = Some(format!(
                "chain {chain}, entry {seq}: written at {}, more than {TAIL_INTERVALS} intervals \
                 after the newest checkpoint of {}, and no {FAILED} lies between — an unsigned \
                 tail",
                rfc3339(time),
                rfc3339(newest)
            ));
        }
    }

    /// The report, once every chain was observed. `present` answers whether the store knows a
    /// chain, even one that holds no entry.
    #[must_use]
    pub fn finish(mut self, present: impl Fn(&str) -> bool) -> Report {
        self.signed_removals();
        // A chain whose first entries are gone begins right after what a signed removal names.
        let mut first: Vec<(String, (u64, String))> = std::mem::take(&mut self.first)
            .into_iter()
            .filter(|(_, (seq, _))| *seq > 1)
            .collect();
        first.sort();
        let mut begins: HashMap<String, (u64, String)> = HashMap::new();
        for (chain, (seq, prev)) in first {
            let pruned = self
                .removed_entries
                .get(&chain)
                .is_some_and(|removed| removed.contains(&(seq - 1, prev.clone())));
            let rotated = chain == FS_CHAIN && self.rotated.contains(&prev);
            if pruned || rotated {
                begins.insert(chain, (seq, prev));
            } else {
                self.fail(format!(
                    "chain {chain}, entry {seq}: the entries before it are gone, and no signed \
                     removal names entry {} with hash {prev} as the last it removed",
                    seq - 1
                ));
            }
        }
        for ((chain, seq), (hash, checkpoint)) in std::mem::take(&mut self.targets) {
            if let Some(found) = self.found.get(&(chain.clone(), seq)) {
                if *found != hash {
                    self.fail(format!(
                        "chain {chain}, entry {seq}: its hash is not the one checkpoint \
                         {checkpoint} names — the chain was rewritten"
                    ));
                }
                continue;
            }
            // Before where the chain begins, after a signed removal.
            if let Some((begin, prev)) = begins.get(&chain) {
                if seq < *begin {
                    if seq + 1 == *begin && *prev != hash {
                        self.fail(format!(
                            "chain {chain}, entry {seq}: the removal that took it names another \
                             hash than checkpoint {checkpoint}"
                        ));
                    }
                    continue;
                }
            }
            let removed = self
                .removed_entries
                .get(&chain)
                .and_then(|removed| removed.iter().filter(|(last, _)| *last >= seq).min());
            match removed {
                Some((last, last_hash)) if *last == seq && *last_hash != hash => {
                    self.fail(format!(
                        "chain {chain}, entry {seq}: the rotation or prune that removed it names \
                         another hash than checkpoint {checkpoint}"
                    ));
                }
                Some(_) => {}
                None => match self.last_seq.get(&chain) {
                    Some(last) if *last < seq => self.fail(format!(
                        "chain {chain}, entry {seq}: the chain ends at entry {last}, before the \
                         head checkpoint {checkpoint} names — its tail was cut"
                    )),
                    Some(_) => self.fail(format!(
                        "chain {chain}, entry {seq}: the chain does not hold the head checkpoint \
                         {checkpoint} names, and no signed removal took it"
                    )),
                    None if present(&chain) => self.fail(format!(
                        "chain {chain}, entry {seq}: the chain holds no entry, and checkpoint \
                         {checkpoint} names this head — it was removed"
                    )),
                    None => self.fail(format!(
                        "chain {chain}: checkpoint {checkpoint} names it, and the record does not \
                         hold it — the chain was removed"
                    )),
                },
            }
        }
        if let Some(unsigned) = self.unsigned.take() {
            self.fail(unsigned);
        }
        self.report
    }
}

fn rfc3339(ms: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_log::render;

    const MINUTE: u64 = 60_000;

    /// A P-256 key in software, signing as the token does: the line without `sig`.
    #[derive(Clone)]
    struct Soft {
        pkcs8: Vec<u8>,
        key: AuditKey,
    }

    impl Soft {
        fn new() -> Self {
            let generated =
                rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
            let spki = rcgen::PublicKeyData::subject_public_key_info(&generated);
            Soft {
                pkcs8: generated.serialize_der(),
                key: AuditKey::from_der(&spki).expect("spki"),
            }
        }

        fn pair(&self) -> ring::signature::EcdsaKeyPair {
            ring::signature::EcdsaKeyPair::from_pkcs8(
                &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
                &self.pkcs8,
                &ring::rand::SystemRandom::new(),
            )
            .expect("pair")
        }

        fn sign(&self, line: &str) -> String {
            let mut value: Value = serde_json::from_str(line).expect("json");
            let sig = self
                .pair()
                .sign(
                    &ring::rand::SystemRandom::new(),
                    value.to_string().as_bytes(),
                )
                .expect("sign");
            value["sig"] = Value::from(base64::engine::general_purpose::STANDARD.encode(sig));
            value.to_string()
        }
    }

    /// A record of chains of a lease, written as the writer writes them, its checkpoints signed by
    /// `soft`.
    struct Record {
        soft: Soft,
        lines: Vec<(String, u64, String)>,
        heads: BTreeMap<String, Head>,
        tip: Option<(u64, String)>,
    }

    impl Record {
        fn new() -> Self {
            Record {
                soft: Soft::new(),
                lines: Vec::new(),
                heads: BTreeMap::new(),
                tip: None,
            }
        }

        fn place(&mut self, chain: &str, time_ms: u64, entry: &Entry) -> String {
            let head = self.heads.entry(chain.to_string()).or_insert(Head {
                chain: chain.to_string(),
                seq: 0,
                hash: GENESIS.to_string(),
                closed: false,
            });
            let entry = entry.clone().with("chain", chain.to_string());
            render(head.seq + 1, time_ms, &entry, &head.hash)
        }

        fn push(&mut self, chain: &str, line: String) {
            let head = self.heads.get_mut(chain).expect("placed");
            head.seq += 1;
            head.hash = line_hash(&line);
            if line.contains("\"event\":\"audit.chain_closed\"") {
                head.closed = true;
            }
            self.lines.push((chain.to_string(), head.seq, line));
        }

        fn entry(&mut self, chain: &str, time_ms: u64, event: &str) {
            let line = self.place(chain, time_ms, &Entry::new(event, "recorded"));
            self.push(chain, line);
        }

        /// A checkpoint in `chain` naming every chain's head, the closed ones only until a
        /// checkpoint named them closed.
        fn checkpoint(&mut self, chain: &str, time_ms: u64) -> String {
            self.heads.entry(chain.to_string()).or_insert(Head {
                chain: chain.to_string(),
                seq: 0,
                hash: GENESIS.to_string(),
                closed: false,
            });
            let named: Vec<Head> = self
                .tip_heads()
                .into_iter()
                .filter(|head| head.closed)
                .collect();
            let heads: Vec<Head> = self
                .heads
                .values()
                .filter(|head| !named.contains(head))
                .cloned()
                .collect();
            let (seq, prev) = self
                .tip
                .clone()
                .map_or((1, GENESIS.to_string()), |(seq, hash)| (seq + 1, hash));
            let entry = unsigned_entry(seq, &prev, &heads, "audit", self.soft.key.sha256());
            let unsigned = self.place(chain, time_ms, &entry);
            let line = self.soft.sign(&unsigned);
            self.tip = Some((seq, line_hash(&line)));
            self.push(chain, line.clone());
            line
        }

        fn clone_lines(&self) -> Record {
            Record {
                soft: self.soft.clone(),
                lines: self.lines.clone(),
                heads: self.heads.clone(),
                tip: self.tip.clone(),
            }
        }

        fn tip_heads(&self) -> Vec<Head> {
            self.lines
                .iter()
                .rev()
                .find_map(|(_, _, line)| parse(line).ok())
                .map(|parsed| parsed.heads)
                .unwrap_or_default()
        }

        fn check(&self, since: Option<String>) -> Report {
            self.check_lines(&self.lines, since)
        }

        fn check_lines(&self, lines: &[(String, u64, String)], since: Option<String>) -> Report {
            let mut check = Check::new(self.soft.key.clone(), since);
            for (chain, seq, line) in lines {
                check.collect(chain, *seq, line);
            }
            check.plan();
            for (chain, seq, line) in lines {
                check.observe(chain, *seq, line);
            }
            let present: Vec<String> = lines.iter().map(|(chain, _, _)| chain.clone()).collect();
            check.finish(|chain| present.iter().any(|known| known == chain))
        }
    }

    fn failed(report: &Report, needle: &str) {
        assert!(report.failed, "{:?}", report.lines);
        assert!(
            report.lines.iter().any(|line| line.contains(needle)),
            "{needle:?} not in {:?}",
            report.lines
        );
    }

    /// Two chains with a checkpoint a minute: what most tests start from.
    fn two_chains() -> Record {
        let mut record = Record::new();
        record.entry("a", 0, "audit.chain_opened");
        record.entry("b", 0, "audit.chain_opened");
        record.checkpoint("a", MINUTE);
        record.entry("b", MINUTE + 1, "host.admitted");
        record.entry("a", MINUTE + 2, "host.admitted");
        record.checkpoint("a", 2 * MINUTE);
        record
    }

    /// An untouched record holds against its checkpoints, and the newest one is answered for the
    /// operator to keep.
    /// Verifies: ADR-0041
    #[test]
    fn an_untouched_record_holds_against_its_checkpoints() {
        let record = two_chains();
        let report = record.check(None);
        assert!(!report.failed, "{:?}", report.lines);
        assert_eq!(
            report.newest.as_deref(),
            Some(record.lines.last().expect("line").2.as_str())
        );
        let parsed = parse(&record.lines.last().expect("line").2).expect("checkpoint");
        assert_eq!(parsed.checkpoint_seq, 2);
        assert_eq!(
            parsed.heads.len(),
            2,
            "each chain's head: {:?}",
            parsed.heads
        );
    }

    /// A chain rewritten from a head on, its tail cut, a chain removed whole, or a checkpoint whose
    /// signature does not verify each fail naming the chain and the checkpoint.
    /// Verifies: ADR-0041
    #[test]
    fn a_cut_a_rewrite_a_removed_chain_and_a_bad_signature_fail_naming_them() {
        let record = two_chains();
        // Chain b's head, rewritten with a fresh hash.
        let mut rewritten = record.lines.clone();
        let at = rewritten
            .iter()
            .rposition(|(chain, _, _)| chain == "b")
            .expect("b");
        rewritten[at].2 = rewritten[at].2.replace("host.admitted", "host.refused");
        failed(
            &record.check_lines(&rewritten, None),
            "chain b, entry 2: its hash is not the one checkpoint 2 names",
        );
        // Chain b's tail cut.
        let mut cut = record.lines.clone();
        cut.remove(at);
        failed(
            &record.check_lines(&cut, None),
            "chain b, entry 2: the chain ends at entry 1",
        );
        // Chain b removed with its record.
        let removed: Vec<_> = record
            .lines
            .iter()
            .filter(|(chain, _, _)| chain != "b")
            .cloned()
            .collect();
        failed(
            &record.check_lines(&removed, None),
            "chain b: checkpoint 2 names it, and the record does not hold it",
        );
        // A signature by another key.
        let other = Soft::new();
        let mut forged = record.lines.clone();
        let last = forged.len() - 1;
        forged[last].2 = other.sign(&forged[last].2);
        failed(
            &record.check_lines(&forged, None),
            "checkpoint 2 (chain a, entry 4): its signature does not verify",
        );
    }

    /// A chain closed before the previous checkpoint is not named again, and removing it is still
    /// found from the checkpoint that named it closed.
    /// Verifies: ADR-0041
    #[test]
    fn a_closed_chain_is_carried_forward_and_its_removal_still_fails() {
        let mut record = Record::new();
        record.entry("a", 0, "audit.chain_opened");
        record.entry("b", 0, "audit.chain_opened");
        record.entry("b", 1, "audit.chain_closed");
        record.checkpoint("a", MINUTE);
        record.entry("a", MINUTE + 1, "host.admitted");
        let newest = record.checkpoint("a", 2 * MINUTE);
        let heads = parse(&newest).expect("checkpoint").heads;
        assert!(
            heads.iter().all(|head| head.chain != "b"),
            "the closed chain is not named again: {heads:?}"
        );
        assert!(!record.check(None).failed);
        let removed: Vec<_> = record
            .lines
            .iter()
            .filter(|(chain, _, _)| chain != "b")
            .cloned()
            .collect();
        failed(
            &record.check_lines(&removed, None),
            "chain b: checkpoint 1 names it",
        );
    }

    /// An entry written more than three intervals after the newest checkpoint fails as an
    /// unsigned tail, unless an `audit.checkpoint_failed` lies between.
    /// Verifies: ADR-0041
    #[test]
    fn an_entry_long_after_the_newest_checkpoint_is_an_unsigned_tail() {
        let mut record = two_chains();
        record.entry("b", 2 * MINUTE + 3 * MINUTE, "host.admitted");
        assert!(!record.check(None).failed, "three intervals are allowed");
        record.entry("b", 2 * MINUTE + 3 * MINUTE + 1, "host.admitted");
        failed(&record.check(None), "chain b, entry 4: written at");
        let mut excused = two_chains();
        excused.entry("a", 2 * MINUTE + MINUTE, FAILED);
        excused.entry("b", 2 * MINUTE + 10 * MINUTE, "host.admitted");
        assert!(
            !excused.check(None).failed,
            "{:?}",
            excused.check(None).lines
        );
    }

    /// A record without any checkpoint fails when a key is given: removed whole, they are missed.
    /// Verifies: ADR-0041
    #[test]
    fn a_record_without_checkpoints_fails_naming_them_missing() {
        let mut record = Record::new();
        record.entry("a", 0, "audit.chain_opened");
        failed(&record.check(None), "the record holds no checkpoint");
    }

    /// The checkpoints follow each other: one removed from the middle, or two with one number,
    /// fail; after a removal the first one left follows the last one the removal names.
    /// Verifies: ADR-0041
    #[test]
    fn the_checkpoints_follow_each_other_and_the_first_left_follows_the_last_removed() {
        let mut record = two_chains();
        record.entry("a", 3 * MINUTE - 1, "host.admitted");
        record.checkpoint("a", 3 * MINUTE);
        // The first checkpoint, with everything of chain a up to it, pruned: the prune names its
        // last entry and the checkpoint.
        let first_cp = record
            .lines
            .iter()
            .position(|(_, _, l)| l.contains("\"checkpoint_seq\":1"))
            .expect("cp 1");
        let (_, cp_seq, cp_line) = record.lines[first_cp].clone();
        let pruned = Entry::new("audit.pruned", "recorded")
            .with("pruned_chain", "a")
            .with("last_seq", cp_seq)
            .with("last_hash", line_hash(&cp_line))
            .with("checkpoint_seq", 1u64)
            .with("checkpoint_hash", line_hash(&cp_line));
        let line = record.place("b", 3 * MINUTE + 1, &pruned);
        record.push("b", line);
        // A removal counts once a checkpoint covers it.
        record.checkpoint("a", 3 * MINUTE + 2);
        let kept: Vec<_> = record
            .lines
            .iter()
            .filter(|(chain, seq, _)| !(chain == "a" && *seq <= cp_seq))
            .cloned()
            .collect();
        let report = record.check_lines(&kept, None);
        assert!(!report.failed, "{:?}", report.lines);
        // Without the link to the removed checkpoint, the first one left follows nothing.
        let unlinked: Vec<_> = kept
            .iter()
            .filter(|(_, _, line)| !line.contains("audit.pruned"))
            .cloned()
            .collect();
        failed(
            &record.check_lines(&unlinked, None),
            "does not follow the first ever",
        );
        // A checkpoint removed from the middle.
        let middle: Vec<_> = record
            .lines
            .iter()
            .filter(|(_, _, line)| !line.contains("\"checkpoint_seq\":2"))
            .cloned()
            .collect();
        failed(
            &record.check_lines(&middle, None),
            "it follows checkpoint 1 — the checkpoints between are missing",
        );
    }

    /// A saved checkpoint the record holds unchanged holds; one changed, one past the newest, or
    /// one neither held nor removed fails; one a removal names holds by its hash.
    /// Verifies: ADR-0041
    #[test]
    fn a_saved_checkpoint_is_held_unchanged_or_named_by_its_removal() {
        let record = two_chains();
        let saved = record
            .lines
            .iter()
            .find(|(_, _, line)| line.contains("\"checkpoint_seq\":1"))
            .expect("checkpoint 1")
            .2
            .clone();
        assert!(!record.check(Some(saved.clone())).failed);
        // Rewritten from it on: the record's checkpoint 1 is another.
        let mut rewritten = Record::new();
        rewritten.soft = record.soft.clone();
        rewritten.entry("a", 5, "audit.chain_opened");
        rewritten.entry("b", 0, "audit.chain_opened");
        rewritten.checkpoint("a", MINUTE);
        failed(
            &rewritten.check(Some(saved.clone())),
            "checkpoint 1 (chain a, entry 2) is not the saved one",
        );
        // The newest checkpoints cut: the saved one is past what the record holds.
        let newest = record.lines.last().expect("line").2.clone();
        let cut: Vec<_> = record.lines[..record.lines.len() - 1].to_vec();
        failed(
            &record.check_lines(&cut, Some(newest)),
            "the newest checkpoints were cut",
        );
        // Removed without a removal that names it.
        let removed: Vec<_> = record
            .lines
            .iter()
            .filter(|(_, _, line)| *line != saved)
            .cloned()
            .collect();
        failed(
            &record.check_lines(&removed, Some(saved.clone())),
            "the record does not hold the saved checkpoint 1, and no rotation or prune removed it",
        );
        // Removed by a prune that names it: matched by its hash.
        let mut pruned = record.clone_lines();
        let named = Entry::new("audit.pruned", "recorded")
            .with("checkpoint_seq", 1u64)
            .with("checkpoint_hash", line_hash(&saved));
        let line = pruned.place("b", 2 * MINUTE + 1, &named);
        pruned.push("b", line);
        pruned.checkpoint("a", 2 * MINUTE + 2);
        let kept: Vec<_> = pruned
            .lines
            .iter()
            .filter(|(_, _, line)| *line != saved)
            .cloned()
            .collect();
        let report = pruned.check_lines(&kept, Some(saved.clone()));
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("is the last one a rotation or a prune removed")),
            "{:?}",
            report.lines
        );
        let mut other: Value = serde_json::from_str(&saved).expect("json");
        other.as_object_mut().expect("object").remove("sig");
        other["time"] = Value::from("1970-01-01T00:00:59Z");
        failed(
            &pruned.check_lines(&kept, Some(record.soft.sign(&other.to_string()))),
            "is not the one the rotation or prune that removed it names",
        );
    }

    /// A removal added after the newest checkpoint is reported and not honoured: a chain removed
    /// whole, or cut at its start, with an `audit.pruned` forged to explain it, still fails, and so
    /// does a files record whose oldest entries went with a forged `audit.rotated`.
    /// Verifies: ADR-0041
    #[test]
    fn a_removal_no_checkpoint_covers_is_not_honoured() {
        let mut record = Record::new();
        record.entry("a", 0, "audit.chain_opened");
        record.entry("b", 0, "audit.chain_opened");
        record.entry("b", 1, "host.admitted");
        record.checkpoint("a", MINUTE);
        record.entry("b", MINUTE + 1, "host.admitted");
        record.entry("b", MINUTE + 2, "host.admitted");
        record.checkpoint("a", 2 * MINUTE);
        let head = |chain: &str, seq: u64| {
            record
                .lines
                .iter()
                .find(|(c, s, _)| c == chain && *s == seq)
                .map(|(_, _, line)| line_hash(line))
                .expect("entry")
        };
        let forge = |record: &mut Record, chain: &str, seq: u64, hash: String| {
            let entry = Entry::new("audit.pruned", "recorded")
                .with("pruned_chain", chain.to_string())
                .with("last_seq", seq)
                .with("last_hash", hash);
            let line = record.place("a", 2 * MINUTE + 1, &entry);
            record.push("a", line);
        };
        // Chain b removed whole, an audit.pruned naming its head added.
        let mut whole = record.clone_lines();
        forge(&mut whole, "b", 4, head("b", 4));
        let lines: Vec<_> = whole
            .lines
            .iter()
            .filter(|(chain, _, _)| chain != "b")
            .cloned()
            .collect();
        let report = whole.check_lines(&lines, None);
        failed(&report, "chain b: checkpoint 2 names it");
        failed(
            &report,
            "audit.pruned lies after every signed head the record still holds",
        );
        // Chain b's young entries 1 to 3 cut, an audit.pruned naming entry 3 added.
        let mut young = record.clone_lines();
        forge(&mut young, "b", 3, head("b", 3));
        let lines: Vec<_> = young
            .lines
            .iter()
            .filter(|(chain, seq, _)| !(chain == "b" && *seq <= 3))
            .cloned()
            .collect();
        failed(
            &young.check_lines(&lines, None),
            "chain b, entry 4: the entries before it are gone, and no signed removal",
        );
        // The same prune, covered by a checkpoint after it, counts.
        young.checkpoint("a", 2 * MINUTE + 2);
        let lines: Vec<_> = young
            .lines
            .iter()
            .filter(|(chain, seq, _)| !(chain == "b" && *seq <= 3))
            .cloned()
            .collect();
        let report = young.check_lines(&lines, None);
        assert!(!report.failed, "{:?}", report.lines);

        // Files: the oldest entries gone, an audit.rotated naming the first one left's prev added.
        let mut files = Record::new();
        for n in 0..3 {
            files.entry(FS_CHAIN, n * 1000, "host.admitted");
        }
        files.checkpoint(FS_CHAIN, MINUTE);
        files.entry(FS_CHAIN, MINUTE + 1, "host.admitted");
        files.checkpoint(FS_CHAIN, 2 * MINUTE);
        let cut = 4;
        let prev = files
            .lines
            .iter()
            .find(|(_, seq, _)| *seq == cut)
            .map(|(_, _, line)| line_hash(line))
            .expect("entry 4");
        let rotated = Entry::new("audit.rotated", "recorded")
            .with("deleted", "audit-1.jsonl")
            .with("deleted_last_hash", prev);
        let line = files.place(FS_CHAIN, 2 * MINUTE + 1, &rotated);
        files.push(FS_CHAIN, line);
        let lines: Vec<_> = files
            .lines
            .iter()
            .filter(|(_, seq, _)| *seq > cut)
            .cloned()
            .collect();
        let report = files.check_lines(&lines, None);
        failed(&report, "chain fs, entry 5: the entries before it are gone");
        failed(
            &report,
            "audit.rotated lies after every signed head the record still holds",
        );
    }

    /// The audit key is read only as a P-256 public key: another curve, another algorithm, or
    /// bytes after the key are refused, naming it.
    /// Verifies: ADR-0041
    #[test]
    fn an_audit_key_not_on_p256_is_refused() {
        let p256 = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
        let pem = p256.public_key_pem();
        assert!(AuditKey::from_pem(pem.as_bytes()).is_ok());
        for other in [&rcgen::PKCS_ECDSA_P384_SHA384, &rcgen::PKCS_ED25519] {
            let key = rcgen::KeyPair::generate_for(other).expect("key");
            let err = AuditKey::from_pem(key.public_key_pem().as_bytes()).expect_err("refused");
            assert!(err.contains("P-256"), "{err}");
        }
        let mut trailing = rcgen::PublicKeyData::subject_public_key_info(&p256);
        trailing.push(0);
        assert!(AuditKey::from_der(&trailing).is_err());
    }

    /// A removal counts only where a head that covers it is still there with its signed hash: a
    /// young run cut with a prune placed inside it, naming the chain's own newest head, fails, and
    /// so do two chains cut with prunes that excuse each other.
    /// Verifies: ADR-0041
    #[test]
    fn a_removal_inside_a_cut_run_covers_nothing() {
        let mut record = Record::new();
        for chain in ["a", "b", "c"] {
            record.entry(chain, 0, "audit.chain_opened");
        }
        for minute in 1..=4 {
            for chain in ["b", "c"] {
                record.entry(chain, minute * MINUTE - 2, "host.admitted");
                record.entry(chain, minute * MINUTE - 1, "host.admitted");
            }
            record.checkpoint("a", minute * MINUTE);
        }
        assert!(!record.check(None).failed);
        let line_at = |lines: &[(String, u64, String)], chain: &str, seq: u64| {
            lines
                .iter()
                .find(|(c, s, _)| c == chain && *s == seq)
                .map(|(_, _, line)| line.clone())
                .expect("entry")
        };
        // A prune at `seq` of `into`, chained to the entry before it, naming `last` of `chain`.
        let forged =
            |lines: &[(String, u64, String)], into: &str, seq: u64, chain: &str, last: u64| {
                let entry = Entry::new("audit.pruned", "recorded")
                    .with("pruned_chain", chain.to_string())
                    .with("last_seq", last)
                    .with("last_hash", line_hash(&line_at(lines, chain, last)))
                    .with("chain", into.to_string());
                let prev = line_hash(&line_at(lines, into, seq - 1));
                (
                    into.to_string(),
                    seq,
                    render(seq, 5 * MINUTE, &entry, &prev),
                )
            };
        // Chain b holds 1 to 9; its newest named head is 9. Cut 6 on, a prune at 6 naming 9.
        let mut own = record
            .lines
            .iter()
            .filter(|(chain, seq, _)| !(chain == "b" && *seq >= 6))
            .cloned()
            .collect::<Vec<_>>();
        own.push(forged(&record.lines, "b", 6, "b", 9));
        let report = record.check_lines(&own, None);
        failed(
            &report,
            "chain b, entry 6: audit.pruned lies after every signed head",
        );
        failed(&report, "chain b, entry 9");
        // Chains b and c both cut from 6, each with a prune naming the other's newest head.
        let mut cycle = record
            .lines
            .iter()
            .filter(|(chain, seq, _)| !((chain == "b" || chain == "c") && *seq >= 6))
            .cloned()
            .collect::<Vec<_>>();
        cycle.push(forged(&record.lines, "b", 6, "c", 9));
        cycle.push(forged(&record.lines, "c", 6, "b", 9));
        let report = record.check_lines(&cycle, None);
        failed(
            &report,
            "chain b, entry 6: audit.pruned lies after every signed head",
        );
        failed(
            &report,
            "chain c, entry 6: audit.pruned lies after every signed head",
        );
        failed(&report, "chain b, entry 9");
        failed(&report, "chain c, entry 9");
    }
}
