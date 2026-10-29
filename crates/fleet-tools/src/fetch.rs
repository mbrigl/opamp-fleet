//! `opamp-fleetctl package fetch` — turns an agent release into package artifacts this fleet can
//! install, and optionally uploads them (ADR-0027, ADR-0032).
//!
//! Getting a real agent into the fleet is a research task before it is a command: which repository
//! publishes the binaries, what the assets are called this month, which checksum file goes with
//! them, and whether the container is one a Client can open at all. This tool holds that knowledge
//! for the agent types the manual documents — `otelcol`, `otelcol-contrib`, `icinga2`,
//! and `supervisor`, this fleet's own Client (ADR-0019) — and asks the
//! operator only what it cannot know: which one, which version, which platforms, and where to send
//! the result.
//!
//! Two rules shape what it produces:
//!
//! - **As published wherever possible.** An artifact that reaches the fleet unaltered is one whose
//!   SHA-256 an operator can compare against the release page, and ADR-0027's line from author to
//!   host stays unbroken. Every artifact is verified against the checksum *upstream* published
//!   before anything else happens to it.
//! - **Repacked only where it must be.** An agent whose upstream publishes no archive a Client can
//!   install is repacked deterministically, from its own tree alone (ADR-0032). That is the one
//!   place the bytes change, and the tool says so.
//!
//! Interactive by default; every prompt has a flag, so the same tool serves a pipeline:
//!   opamp-fleetctl package fetch                          # asks for everything
//!   opamp-fleetctl package fetch --agent otelcol-contrib --version 0.158.0 --platform linux/amd64 --no-upload

use crate::package::sha256_file;
use clap::ValueEnum;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use fleet_core::tree::KeptLink;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// How many release series the operator is offered. Enough to reach the release before last when
/// one turns out badly, few enough to read without scrolling.
const SERIES_SHOWN: usize = 3;

/// The member cap a tree package is held to (ADR-0027). Checked while packing rather than
/// discovered on three hundred hosts at rollout time.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
const MAX_TREE_MEMBERS: usize = 10_000;

/// What `package fetch` is asked for; every prompt has its flag.
#[derive(clap::Args)]
pub struct Args {
    /// Which agent to fetch. Omitted, it is asked for.
    #[arg(long, value_enum)]
    agent: Option<AgentKind>,
    /// Which version, as upstream numbers it (`0.158.0`, `1.19`). Omitted, the latest few are
    /// offered.
    #[arg(long)]
    version: Option<String>,
    /// A platform to fetch, as `os/arch` (`linux/amd64`). Repeatable. Omitted, the platforms this
    /// release publishes are offered.
    #[arg(long = "platform", value_name = "OS/ARCH")]
    platforms: Vec<String>,
    /// Where to write the artifacts.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
    /// Upload to this Server when the artifacts are ready, e.g. `https://127.0.0.1:4321`. The
    /// Operator plane serves TLS 1.3 only (ADR-0012).
    #[arg(long, value_name = "URL")]
    server: Option<String>,
    /// The CA certificate (PEM) the Server's certificate chains to, such as the `server-ca.pem`
    /// `pki init` writes. It replaces the system's roots for the calls to the Server alone; the
    /// downloads from upstream keep them. Omitted, the Server is checked against the system's roots.
    #[arg(long, value_name = "FILE", conflicts_with = "no_upload")]
    cacert: Option<PathBuf>,
    /// Write the artifacts and stop — no upload, and no question about one.
    #[arg(long, conflicts_with = "server")]
    no_upload: bool,
    /// Which repack of this version it is, for an agent whose artifact this tool repacks: 1, the
    /// default, for the first, 2 and on for a later one of the same version. The second repack of
    /// 2.14.3 is uploaded as Package `2.14.4-0.r.2`, above the first and below upstream's next
    /// release (ADR-draft-the-operator-program-opamp-fleetctl clause 28).
    #[arg(long, value_name = "N", default_value_t = 1)]
    repack: u32,
    /// Which Debian build to repack, by codename (`bullseye`, `bookworm`, `trixie`). Icinga 2 only.
    /// Every file of the tree comes from that suite's signed archives, so any host builds any of
    /// them; omitted, it is this host's own. It is the artifact's reach: repack the oldest suite
    /// you serve, since glibc cannot travel and is backward compatible (ADR-draft-icinga-2).
    #[arg(long, value_name = "CODENAME")]
    distro: Option<String>,
}

/// The agents this tool knows how to fetch. Adding one is this enum, its [`Source`], and nothing
/// else — the rest of the tool works from what the source says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AgentKind {
    /// The OpenTelemetry Collector, core distribution.
    #[value(name = "otelcol")]
    Otelcol,
    /// The OpenTelemetry Collector, Contrib distribution — the one carrying `opampextension`.
    #[value(name = "otelcol-contrib")]
    OtelcolContrib,
    /// Icinga 2, repacked from the vendor's distribution packages (ADR-draft-icinga-2).
    #[value(name = "icinga2")]
    Icinga2,
    /// This fleet's own Client — the package a Client updates *itself* from (ADR-0018, ADR-0019).
    #[value(name = "supervisor")]
    Supervisor,
}

/// Everything that differs between one upstream project and the next, in one place.
struct Source {
    /// The Agent type a Supervisor reports and a package Set is built for (ADR-0027) — also this
    /// tool's default package name.
    service_name: &'static str,
    /// The GitHub repository whose tags name the versions.
    repo: &'static str,
}

impl AgentKind {
    fn source(self) -> Source {
        match self {
            AgentKind::Otelcol => Source {
                service_name: "otelcol",
                repo: "open-telemetry/opentelemetry-collector-releases",
            },
            AgentKind::OtelcolContrib => Source {
                service_name: "otelcol-contrib",
                repo: "open-telemetry/opentelemetry-collector-releases",
            },
            AgentKind::Icinga2 => Source {
                service_name: "icinga2",
                repo: "Icinga/icinga2",
            },
            // This repository's own releases. The Client is an Agent like any other (ADR-0018), so
            // the artifact that updates it is fetched the way every other agent's is — and by
            // ADR-0034 it is named and packed like one too, which is what makes that possible.
            AgentKind::Supervisor => Source {
                service_name: "supervisor",
                repo: "mbrigl/opamp-fleet",
            },
        }
    }

    /// The version a tag names, or `None` when the tag is not a release of *this* agent.
    ///
    /// Each project spells its tags its own way, and one of them carries tags that are not releases
    /// at all: the Collector repository tags its builder and supervisor alongside
    /// (`cmd/builder/v0.158.0`). A pattern
    /// per source is what keeps those out of the list an operator picks from.
    fn version_of_tag(self, tag: &str) -> Option<String> {
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        let rest = tag.strip_prefix(self.tag_prefix())?;
        let parts = rest.split('.').count();
        // `v0.158.0`, `version/1.2.3` — three numeric parts behind the prefix, and nothing else.
        let shaped = parts == 3;
        (shaped && rest.split('.').all(numeric)).then(|| rest.to_string())
    }

    /// What each project puts in front of the version in its tags: `v`, the `version/` this
    /// project's release workflow creates them under (ADR-0033).
    fn tag_prefix(self) -> &'static str {
        match self {
            AgentKind::Otelcol | AgentKind::OtelcolContrib | AgentKind::Icinga2 => "v",
            AgentKind::Supervisor => "version/",
        }
    }

    /// The tag a version came from — the reverse of [`version_of_tag`](Self::version_of_tag), for
    /// the `--version` flag, which takes the version an operator reads on the release page.
    fn tag_of_version(self, version: &str) -> String {
        format!("{}{version}", self.tag_prefix())
    }

    /// What one upstream version is called on each side: the tag it is fetched under, in
    /// upstream's own spelling, and the Package version it is uploaded as, written as SemVer
    /// (ADR-0028 clause 6) — `1.19` is fetched as `v1.19` and becomes Package `1.19.0`.
    ///
    /// # Errors
    /// Returns the message to print when upstream's number cannot be written as SemVer.
    fn release_names(self, upstream: &str) -> Result<(String, String), String> {
        Ok((self.tag_of_version(upstream), package_version(upstream)?))
    }

    /// Whether this tool repacks the agent's artifact, rather than uploading it as upstream
    /// publishes it: only then can one version have a later repack
    /// (ADR-draft-the-operator-program-opamp-fleetctl clause 28).
    fn repacked(self) -> bool {
        match self {
            // The Linux artifact is a tree built from Icinga's and Debian's packages
            // (ADR-draft-icinga-2).
            AgentKind::Icinga2 => true,
            AgentKind::Otelcol | AgentKind::OtelcolContrib | AgentKind::Supervisor => false,
        }
    }

    /// The systems this agent can be fetched for, as the agent menu states them.
    ///
    /// A hint, not the plan: what is actually offered comes from the release itself a step later,
    /// and for Icinga 2 also from the suite it is repacked from, by default the host's own. It is
    /// here because the menu asks for an agent before any of that is known, and picking one only to
    /// be told that this host builds nothing for it is a round trip the operator should not have
    /// to make.
    ///
    /// The Collectors publish per platform and add new ones between releases, so naming their
    /// architectures here would be a table going stale — the operating systems are the honest
    /// answer, and the platform question that follows shows the release's own list.
    ///
    /// `host` is this machine's distribution codename where it has one, because Icinga 2's answer
    /// depends on it and no other agent's does.
    fn platforms(self, host: Option<&str>) -> String {
        match self {
            AgentKind::Otelcol | AgentKind::OtelcolContrib => {
                "linux, darwin, windows — as the release publishes".to_string()
            }
            // This column answers one question for every agent alike — does it run on the hosts I
            // have — so Icinga 2 states its *reach*: that of the Debian suite this run repacks
            // unless `--distro` names another, which is this host's own (ADR-draft-icinga-2).
            AgentKind::Icinga2 => icinga2_reach(host),
            // Named rather than left to the release, unlike the Collectors': this is *this*
            // project's own build matrix (`.github/workflows/release.yml`), so it moves only when
            // this repository decides it should — and the column's question, "does it run on the
            // hosts I have", deserves the concrete answer where one can be given.
            AgentKind::Supervisor => group_platforms(SUPERVISOR_PLATFORMS.iter().copied()),
        }
    }

    /// Whether the release's assets have to be listed to plan the fetch. An agent whose artifacts
    /// are not assets of its GitHub release says no, sparing a request against a rate-limited
    /// API.
    fn needs_assets(self) -> bool {
        match self {
            AgentKind::Otelcol | AgentKind::OtelcolContrib | AgentKind::Supervisor => true,
            // Icinga 2's GitHub releases carry none of what is repacked — its packages come from
            // the vendor's own repositories (ADR-draft-icinga-2).
            AgentKind::Icinga2 => false,
        }
    }

    /// The line that introduces the per-platform hints printed when the fetch is done.
    ///
    /// Every other agent is installed by a Supervisor, which needs a block; this fleet's own Client
    /// is installed *by itself* over itself, and what admits that is a consent rather than a block
    /// (ADR-0018). One sentence, so the hints below it read as the answer to it.
    fn install_hint(self) -> &'static str {
        match self {
            AgentKind::Supervisor => "What a Client needs to take these:",
            _ => "What a Supervisor needs to install these:",
        }
    }

    /// The Configurations this agent needs before it can do anything, typed the way
    /// `scripts/seed_test_configs.sh` types them — which is where these bodies come from, and why
    /// they agree with the `[[supervisor]]` blocks in `config/supervisor.toml` down to the file names.
    ///
    /// Icinga 2 gets two, because it reads one root file and includes the rest by name
    /// (ADR-draft-icinga-2). Its ticket and its parent's certificate are *not* here: both are per
    /// host, one is a secret, and neither has a sensible default — see `docs/manual/icinga2.md`.
    fn default_configurations(self) -> &'static [DefaultConfiguration] {
        // Each is stated for the `service.name` its agent reports, so a Deployment holding it
        // fits it to that Agent type alone (ADR-0026); the Deployment's Selector does the aiming.
        match self {
            AgentKind::Otelcol => &[DefaultConfiguration {
                name: "otelcol-conf",
                service_name: "otelcol",
                body: include_str!("../../../config/examples/otelcol-conf.yaml"),
            }],
            AgentKind::OtelcolContrib => &[DefaultConfiguration {
                name: "otelcol-contrib-conf",
                service_name: "otelcol-contrib",
                body: include_str!("../../../config/examples/otelcol-contrib-conf.yaml"),
            }],
            // None. A Client is configured by `supervisor.toml` on its host — the one file the fleet
            // deliberately does not own (ADR-0017 admits only `[[supervisor]]` blocks), and none
            // of it has a default this tool could put on a Server.
            AgentKind::Supervisor => &[],
            AgentKind::Icinga2 => &[
                DefaultConfiguration {
                    name: "icinga2-conf",
                    service_name: "icinga2",
                    body: include_str!("../../../config/examples/icinga2-conf.conf"),
                },
                DefaultConfiguration {
                    name: "icinga2-zones",
                    service_name: "icinga2",
                    body: include_str!("../../../config/examples/icinga2-zones.conf"),
                },
            ],
        }
    }
}

/// An upstream version written as a Package version (ADR-0028 clause 6): a number with fewer than
/// three numeric components is completed with `.0` (`1.19` becomes `1.19.0`), and a leading zero
/// is dropped from a numeric component (`24.04.1` becomes `24.4.1`). Both keep upstream's order.
///
/// # Errors
/// Names the number when it cannot be written as SemVer this way.
fn package_version(upstream: &str) -> Result<String, String> {
    // The rule lives in the internal crate, so a kind that reports its program's version by it
    // agrees with the Package this tool names.
    fleet_core::version::written_as_semver(upstream).ok_or_else(|| {
        format!(
            "upstream's version {upstream:?} cannot be written as SemVer, which a Package \
             version is — fetch it by hand and upload it under a SemVer version you choose \
             in the same order"
        )
    })
}

/// The Package version of the `repack`-th repack of one upstream version, whose Package version
/// is `version` (ADR-draft-the-operator-program-opamp-fleetctl clause 28). The first is the version
/// itself. A later one is `<major>.<minor>.<patch+1>-0.r.<n>`: a pre-release of the next patch, so
/// it orders above the first repack and below upstream's next release, and its repack number is a
/// numeric identifier, compared as a number. Build metadata would not do, since two versions that
/// differ only in it compare equal (ADR-0028 clause 2), and the program keeps reporting the
/// version it is, which the Server offers a Package only above.
///
/// # Errors
/// Refuses a repack number below 1, a later repack of an agent this tool does not repack, and a
/// version that is not three numbers.
fn repack_version(agent: AgentKind, version: &str, repack: u32) -> Result<String, String> {
    if repack > 1 && !agent.repacked() {
        return Err(format!(
            "--repack is for an agent whose artifact this tool repacks; {} is uploaded as its \
             release publishes it, so a version of it has one artifact only",
            agent.source().service_name
        ));
    }
    numbered_repack(version, repack)
}

/// The numbering of [`repack_version`], whichever agent it is for.
fn numbered_repack(version: &str, repack: u32) -> Result<String, String> {
    match repack {
        0 => return Err("--repack counts from 1, the first repack of a version".to_string()),
        1 => return Ok(version.to_string()),
        _ => {}
    }
    let numbers: Vec<u64> = version
        .split('.')
        .map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect::<Option<_>>()
        .filter(|numbers: &Vec<u64>| numbers.len() == 3)
        .ok_or_else(|| {
            format!("a later repack of {version:?} cannot be numbered: it is not three numbers")
        })?;
    let next = numbers[2].checked_add(1).ok_or_else(|| {
        format!("a later repack of {version:?} cannot be numbered: no next patch")
    })?;
    Ok(format!("{}.{}.{next}-0.r.{repack}", numbers[0], numbers[1]))
}

/// A Configuration this tool puts on the Server beside the package, when the Server has none of
/// that name yet.
///
/// The body is compiled in rather than read from the repository: this tool is released as a
/// binary an operator runs anywhere (ADR-0008), so a file path beside it would be a default that
/// only works in a checkout.
struct DefaultConfiguration {
    /// The Configuration's name — also the file name its entry gets in the Supervisor's config
    /// directory, which is what the `[[supervisor]]` block has to name.
    name: &'static str,
    /// The Agent type this is for (ADR-0026 clause 4); empty is every type.
    service_name: &'static str,
    body: &'static str,
}

/// One file to fetch, and where the SHA-256 it must match comes from.
struct Download {
    url: String,
    checksum: ChecksumSource,
}

/// One platform's artifact, planned before anything is downloaded.
struct Plan {
    /// The platform as *this fleet* names it (ADR-0027), which is what the upload path carries.
    os: String,
    arch: String,
    /// What has to be fetched. Usually one file; a repacked Icinga 2 tree needs the vendor's binary
    /// package *and* the one holding the ITL, so this is a list (ADR-draft-icinga-2). Every one of
    /// them is verified before any of them is used.
    sources: Vec<Download>,
    /// What has to happen to the downloaded bytes before they are a package artifact.
    action: Action,
    /// The artifact's file name in `--out-dir`.
    out_name: String,
    /// What a `[[supervisor]]` block has to say to install it — or, for this fleet's own Client,
    /// what `[packages] self_update` has to say, since nothing supervises that one. Printed at the end,
    /// because it is the next thing an operator needs.
    ///
    /// For a **wrapped** agent it is the `type` alone (ADR-0017): the Client's kind knows the
    /// program's name, where it sits in the tree, and how it is invoked, so this tool dictates no
    /// line for a host to transcribe. Those facts live in that agent's artifact document under
    /// `docs/artifacts/`, which the kind's own tests pin — the two ends of one artifact, kept in
    /// step by a red test rather than by a hint printed on a terminal.
    block_hint: String,
}

/// Where an artifact's published SHA-256 is found. Every source publishes one; no two publish it
/// the same way, and the Collector changed its mind at 0.158.0.
enum ChecksumSource {
    /// Files holding `<hash>  <name>` lines, `sha256sum` style — the artifact's line is looked up
    /// by name. The Collector's pre-0.158 files.
    ///
    /// Several, because a release may split them: 0.157.0 keeps its Windows assets in a file of
    /// their own. Which file holds a given artifact is answered by looking, not by a rule about
    /// platforms that would need revisiting the next time upstream reorganises.
    Sums { urls: Vec<String> },
    /// A file holding one bare hex digest and nothing else — the Collector's per-asset sidecar
    /// from 0.158.0 on.
    BareDigest { url: String },
    /// The digest is already known — read out of a repository index rather than a sidecar. Icinga
    /// signs its repositories with GPG instead of publishing per-file checksums, so the `SHA256:`
    /// field of the `Packages` index is where its hash comes from — an index read only after its
    /// signed `InRelease` verified and vouched for it (ADR-draft-icinga-2).
    Known { sha256: String },
}

/// What happens between the download and the artifact.
enum Action {
    /// Nothing. The artifact is uploaded exactly as upstream published it, so the hash the fleet
    /// verifies is the hash on the release page (ADR-0027).
    AsPublished,
    /// Unpack the vendor's Debian packages into one normalised, link-free tree and pack that
    /// (ADR-draft-icinga-2): Icinga 2 publishes distribution packages and an MSI, and no portable
    /// tree.
    ///
    /// The first `tree_packages` sources make the tree; the rest are the library packages of
    /// Debian's archive the tree's shared libraries are taken from (ADR-draft-icinga-2 clause 18).
    /// Nothing the tree carries comes from the host that repacks it.
    RepackDebs {
        wrapper: String,
        tree_packages: usize,
    },
}

impl Action {
    /// Whether the artifact is this tool's repack rather than what upstream published: every
    /// action but [`AsPublished`](Self::AsPublished) is a kind's repack (ADR-0032).
    fn repacks(&self) -> bool {
        !matches!(self, Action::AsPublished)
    }
}

/// What a kind's plans are made from: the release as upstream and this tool name it, its assets,
/// and the command line, from which a kind reads what only it asks for.
struct Release<'a> {
    /// The version as upstream numbers it (`1.19`).
    version: &'a str,
    /// Upstream's tag of that version (`v0.158.0`).
    #[allow(
        dead_code,
        reason = "a kind that builds its URLs from the tag reads it"
    )]
    tag: &'a str,
    /// The Package version the artifacts are uploaded as, a later repack's included — what a
    /// repacked artifact is named by (ADR-0028 clause 6).
    #[allow(dead_code, reason = "a kind that repacks names its artifact by it")]
    package_version: &'a str,
    /// The release's assets as `(name, url)`, empty for an agent that does not need them.
    assets: &'a [(String, String)],
    /// The command line, for a flag only one kind takes.
    #[allow(dead_code, reason = "a kind with a flag of its own reads it")]
    args: &'a Args,
}

/// `package fetch`: which agent, which version, which platforms, and whether to upload — each
/// asked for when its flag is missing.
///
/// # Errors
/// Returns the message to print when a download, a check or an upload fails.
pub async fn run(args: Args) -> Result<(), String> {
    // Built before anything is downloaded: a CA path with a typo is cheaper to hear about now than
    // after hundreds of megabytes.
    let server_client = server_http(args.cacert.as_deref())?;
    let agent = match args.agent {
        Some(agent) => agent,
        None => choose_agent()?,
    };
    let source = agent.source();

    let version = match args.version.clone() {
        Some(version) => version,
        None => choose_version(agent, &source).await?,
    };
    // The release is fetched under upstream's spelling, and named as a Package in SemVer — before
    // anything is downloaded, so a number that cannot be written so costs nothing.
    let (tag, package_version) = agent.release_names(&version)?;
    let package_version = repack_version(agent, &package_version, args.repack)?;

    let assets = if agent.needs_assets() {
        release_assets(&source, &tag).await?
    } else {
        Vec::new()
    };
    let release = Release {
        version: &version,
        tag: &tag,
        package_version: &package_version,
        assets: &assets,
        args: &args,
    };
    let available = plans(agent, &release).await?;
    if available.is_empty() {
        // For this project's own releases the likeliest reason is a known one, and saying it saves
        // the operator a trip to the release page: older releases carry another name and container,
        // and a Set built from those fits no Client reporting the type `supervisor` anyway
        // (ADR-0019).
        let why = match agent {
            AgentKind::Supervisor => {
                " — a release from before the rename publishes \
                 `opamp-fleet-client_<version>_<os>_<arch>.7z`, which is a package no Client of \
                 this type installs"
            }
            _ => "",
        };
        return Err(format!(
            "{} {version} publishes nothing this tool can install{why}",
            source.service_name
        ));
    }
    let chosen = select_platforms(&available, &args.platforms)?;

    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", args.out_dir.display()))?;
    let mut produced: Vec<(&Plan, PathBuf)> = Vec::new();
    for plan in &chosen {
        let artifact = produce(plan, &args.out_dir).await?;
        produced.push((plan, artifact));
    }

    let server = upload_target(&args)?;
    if let Some(server) = &server {
        for (plan, artifact) in &produced {
            upload(
                &server_client,
                server,
                source.service_name,
                &package_version,
                plan,
                artifact,
            )
            .await?;
        }
        upload_default_configurations(&server_client, server, agent).await?;
    }

    eprintln!("\nDone. {}", agent.install_hint());
    for (plan, artifact) in &produced {
        eprintln!(
            "  {}/{}  {}\n      {}",
            plan.os,
            plan.arch,
            artifact.display(),
            plan.block_hint
        );
    }
    if server.is_none() {
        eprintln!(
            "\nNothing was uploaded. To do it later, create the package, PUT each artifact, sign \
             each entry, and put it in the deployment that should carry it:\n  \
             curl -X PUT -H 'Content-Type: application/json' -d '{{}}' \\\n       \
             <server>/api/v1/packages/{name}/{version}\n  \
             curl -X PUT --data-binary @<artifact> \\\n       \
             \"<server>/api/v1/packages/{name}/{version}/entries/<os>/<arch>\"\n  \
             curl -X PUT -H 'Content-Type: application/json' -d '{{\"signature\": \"<hex>\"}}' \\\n       \
             \"<server>/api/v1/packages/{name}/{version}/entries/<os>/<arch>/signature\"\n  \
             curl -X PUT <server>/api/v1/deployments/<ring>/packages/{name}/{version}\n  \
             curl -X POST <server>/api/v1/deployments/<ring>/rollout",
            name = source.service_name,
            version = package_version
        );
        // Silent for an agent that has none — the Client's own configuration is its host's
        // (ADR-0017), so there is nothing here to offer and nothing to apologise for.
        if !agent.default_configurations().is_empty() {
            eprintln!(
                "  The default configuration ({}) is not uploaded either; an upload puts it there \
                 when the Server has none of that name. Or seed it from config/examples/ with \
                 scripts/seed_test_configs.sh.",
                agent
                    .default_configurations()
                    .iter()
                    .map(|c| c.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

// ── The questions ───────────────────────────────────────────────────────────

/// What a Linux artifact repacked from one Debian suite reaches, by that suite's codename.
///
/// Each row is the highest `libc6 (>= …)` floor the packages of that build state — the vendor's
/// and every library's the tree carries — read back as the systems it covers: glibc is backward
/// compatible, so a floor is the whole of it and the distribution family is none of it
/// (ADR-draft-icinga-2). The floors are what this tool prints for real once the repository indexes
/// have been read (`icinga2_plans`); the ones below were read for Icinga 2 2.16.5.
///
/// The table is small because the source of truth is not it: it translates a floor into the names
/// an operator recognises, one row per distribution Icinga publishes for. `--distro` accepts the
/// same three, and the test below holds the two lists together.
const ICINGA2_REACH: &[(&str, &str)] = &[
    // libc6 >= 2.30 — Debian 11 is 2.31, Ubuntu 20.04 is 2.31, RHEL 9 is 2.34. RHEL 8 (2.28) is
    // the one this misses, and no build of Icinga's reaches it.
    ("bullseye", "Debian 11+/Ubuntu 20.04+/RHEL 9+"),
    // libc6 >= 2.36 — `icinga2-bin` states 2.34, but the `libstdc++6` it links states 2.36.
    // Ubuntu 22.04 is 2.35 and RHEL 9 is 2.34, so both drop out here; Ubuntu 24.04 and RHEL 10 are
    // 2.39.
    ("bookworm", "Debian 12+/Ubuntu 24.04+/RHEL 10+"),
    // libc6 >= 2.39 — Ubuntu 24.04 is 2.39, RHEL 10 is 2.39.
    ("trixie", "Debian 13+/Ubuntu 24.04+/RHEL 10+"),
];

/// Icinga 2's line in the agent menu, for the host this tool is running on.
///
/// The build defaults to the host's own suite (`chosen_distro`), so the line states that suite's
/// reach. A host that is none of the three builds a Linux artifact only when `--distro` names one,
/// so the line says so rather than naming a reach this run would not deliver unasked. That is the
/// one case where a codename belongs in this column: it is then the flag the operator has to give.
fn icinga2_reach(host: Option<&str>) -> String {
    match host.and_then(|codename| {
        ICINGA2_REACH
            .iter()
            .find(|(name, _)| *name == codename)
            .map(|(_, reach)| *reach)
    }) {
        Some(reach) => format!("linux/amd64 ({reach})"),
        None => "linux/amd64 with --distro bullseye/bookworm/trixie".to_string(),
    }
}

/// Platform pairs as one menu line: `linux/amd64+arm64 windows/amd64`.
///
/// Grouped by operating system, and tightly, because the menu is one line per agent and a line
/// that wraps at eighty columns is one `dialoguer` redraws wrongly when the selection moves.
fn group_platforms<'p>(pairs: impl Iterator<Item = (&'p str, &'p str)>) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for (os, arch) in pairs {
        match groups.iter_mut().find(|(name, _)| *name == os) {
            Some((_, arches)) => arches.push(arch),
            None => groups.push((os, vec![arch])),
        }
    }
    groups
        .iter()
        .map(|(os, arches)| format!("{os}/{}", arches.join("+")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The agent menu's lines, name and systems in two columns.
///
/// Its own function so the width rule below is testable against the very lines the menu shows,
/// rather than against a copy of them that drifts.
fn agent_menu_labels(host: Option<&str>) -> Vec<String> {
    // Asked of clap rather than written out again. A second list of the same agents is how an
    // agent goes missing from this prompt while `--agent` accepts it: adding a variant is one
    // place, and the menu follows it whether or not anyone remembers this function.
    let agents = AgentKind::value_variants();
    // The systems beside each name, in a column: the choice that follows this one is a platform,
    // and an agent that publishes nothing for the systems an operator runs is better seen here
    // than after two more questions.
    let width = agents
        .iter()
        .map(|a| a.source().service_name.len())
        .max()
        .unwrap_or(0);
    agents
        .iter()
        .map(|a| {
            format!(
                "{name:<width$}  {platforms}",
                name = a.source().service_name,
                platforms = a.platforms(host)
            )
        })
        .collect()
}

fn choose_agent() -> Result<AgentKind, String> {
    let agents = AgentKind::value_variants();
    // Not an error when it fails: a host without a `VERSION_CODENAME` still fetches every other
    // agent, and Icinga 2's line then says so instead of the menu refusing to appear.
    let host = host_codename().ok();
    let labels = agent_menu_labels(host.as_deref());
    let picked = Select::new()
        .with_prompt("Which agent")
        .items(&labels)
        .default(0)
        .interact()
        .map_err(|e| format!("cannot read the choice: {e}"))?;
    Ok(agents[picked])
}

async fn choose_version(agent: AgentKind, source: &Source) -> Result<String, String> {
    eprintln!("Reading the last releases of {} …", source.repo);
    let versions = recent_versions(agent, source).await?;
    if versions.is_empty() {
        return Err(format!("{} publishes no release tags", source.repo));
    }
    let picked = Select::new()
        .with_prompt("Which version")
        .items(&versions)
        .default(0)
        .interact()
        .map_err(|e| format!("cannot read the choice: {e}"))?;
    Ok(versions[picked].clone())
}

/// Picks the platforms to fetch: from `--platform` when given, by multi-select otherwise.
fn select_platforms<'p>(available: &'p [Plan], wanted: &[String]) -> Result<Vec<&'p Plan>, String> {
    if !wanted.is_empty() {
        return wanted
            .iter()
            .map(|w| {
                let (os, arch) = w
                    .split_once('/')
                    .ok_or_else(|| format!("{w:?} is not an os/arch pair"))?;
                available
                    .iter()
                    .find(|p| p.os == os && p.arch == arch)
                    .ok_or_else(|| {
                        format!(
                            "this release publishes nothing for {w} (it has: {})",
                            available
                                .iter()
                                .map(|p| format!("{}/{}", p.os, p.arch))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            })
            .collect();
    }
    let labels: Vec<String> = available
        .iter()
        .map(|p| {
            if p.action.repacks() {
                format!("{}/{} (repacked)", p.os, p.arch)
            } else {
                format!("{}/{}", p.os, p.arch)
            }
        })
        .collect();
    let picked = MultiSelect::new()
        .with_prompt("Which platforms (space to select, enter to confirm)")
        .items(&labels)
        .interact()
        .map_err(|e| format!("cannot read the choice: {e}"))?;
    if picked.is_empty() {
        return Err("no platform selected".to_string());
    }
    Ok(picked.into_iter().map(|i| &available[i]).collect())
}

/// Where to upload, if anywhere. `--no-upload` answers it, `--server` answers it, and otherwise it
/// is a question — with the artifacts already written, so a "no" here has still produced them.
fn upload_target(args: &Args) -> Result<Option<String>, String> {
    if args.no_upload {
        return Ok(None);
    }
    if let Some(server) = &args.server {
        return Ok(Some(server.trim_end_matches('/').to_string()));
    }
    let wants = Confirm::new()
        .with_prompt("Upload these to a fleet Server")
        .default(false)
        .interact()
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    if !wants {
        return Ok(None);
    }
    let url: String = Input::new()
        .with_prompt("Server base URL")
        .default("https://127.0.0.1:4321".to_string())
        .interact_text()
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    Ok(Some(url.trim_end_matches('/').to_string()))
}

// ── Upstream ────────────────────────────────────────────────────────────────

/// The newest version of each of the last [`SERIES_SHOWN`] release series, newest first.
///
/// A *series* is a `major.minor` line, and only its newest patch is offered: nobody installing
/// 2.16 wants 2.16.3 when 2.16.5 exists, and an agent that patches often — five patches of one
/// series — would otherwise fill the whole list with one line and hide every version an
/// operator might actually be looking for. Where an agent's versions carry no patch number at all,
/// every version is its own series and this is simply the last three.
///
/// Tags rather than releases: a Collector release object carries hundreds of assets, so listing
/// twenty of them to read their names would move megabytes to answer a question tags answer in
/// kilobytes. The order is this tool's own — sorted by the numbers, not by whatever order the API
/// returned — so the newest is the newest whatever the repository's tagging history looks like.
async fn recent_versions(agent: AgentKind, source: &Source) -> Result<Vec<String>, String> {
    let body = get_json(&format!(
        "https://api.github.com/repos/{}/tags?per_page=100",
        source.repo
    ))
    .await?;
    let mut versions: Vec<String> = body
        .as_array()
        .ok_or("the tag listing is not a list")?
        .iter()
        .filter_map(|tag| tag.get("name")?.as_str())
        .filter_map(|name| agent.version_of_tag(name))
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    versions.dedup();
    Ok(newest_per_series(&versions))
}

/// The newest version of each of the last [`SERIES_SHOWN`] `major.minor` series, given a list
/// already sorted newest first — so the first version of a series encountered *is* its newest.
fn newest_per_series(versions: &[String]) -> Vec<String> {
    let mut newest = Vec::new();
    let mut series_seen: Vec<Vec<u64>> = Vec::new();
    for version in versions {
        // A version with no minor part is its own series, which is what makes an agent that
        // versions in two parts behave as "the last three versions" without a special case.
        let series: Vec<u64> = version_key(version).into_iter().take(2).collect();
        if series_seen.contains(&series) {
            continue;
        }
        series_seen.push(series);
        newest.push(version.clone());
        if newest.len() == SERIES_SHOWN {
            break;
        }
    }
    newest
}

/// A version's parts as numbers, so `0.9.0` sorts below `0.10.0` — which it does not as text.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

/// The assets of one release: `(name, download URL)`.
async fn release_assets(source: &Source, tag: &str) -> Result<Vec<(String, String)>, String> {
    let body = get_json(&format!(
        "https://api.github.com/repos/{}/releases/tags/{tag}",
        source.repo
    ))
    .await?;
    let assets = body
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or_else(|| format!("{} has no release {tag}", source.repo))?;
    Ok(assets
        .iter()
        .filter_map(|asset| {
            Some((
                asset.get("name")?.as_str()?.to_string(),
                asset.get("browser_download_url")?.as_str()?.to_string(),
            ))
        })
        .collect())
}

/// What this release offers, one entry per platform this fleet can name.
async fn plans(agent: AgentKind, release: &Release<'_>) -> Result<Vec<Plan>, String> {
    match agent {
        AgentKind::Otelcol | AgentKind::OtelcolContrib => {
            Ok(collector_plans(agent, release.version, release.assets))
        }
        AgentKind::Supervisor => Ok(supervisor_plans(release.version, release.assets)),
        AgentKind::Icinga2 => {
            let distro = chosen_distro(release.args.distro.as_deref(), host_codename())?;
            icinga2_plans(release.version, &distro).await
        }
    }
}

/// Which Debian suite's build is repacked: the one `--distro` names, else this host's own when it
/// is one Icinga publishes for. A host that is none of them is refused, naming the flag: the
/// Linux artifact is the only one this tool builds for Icinga 2 (ADR-draft-icinga-2 clause 23).
///
/// The suite is the artifact's reach (ADR-draft-icinga-2 clause 19), and nothing else about the
/// host is: every file of the tree comes from the vendor's and Debian's signed archives, so any
/// host builds any of the three. The host's codename is only the default, stated as it is taken.
fn chosen_distro(flag: Option<&str>, host: Result<String, String>) -> Result<String, String> {
    let known = |codename: &str| ICINGA2_REACH.iter().any(|(name, _)| *name == codename);
    let names = || {
        ICINGA2_REACH
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(distro) = flag {
        if !known(distro) {
            return Err(format!(
                "Icinga publishes no Debian build for {distro:?}; --distro takes one of {}",
                names()
            ));
        }
        return Ok(distro.to_string());
    }
    match host {
        Ok(here) if known(&here) => {
            eprintln!("  building the {here} artifact, this host's own; --distro names another");
            Ok(here)
        }
        Ok(here) => Err(format!(
            "this host is {here:?}, which Icinga publishes no Debian build for, so name the one \
             to repack with --distro ({})",
            names()
        )),
        Err(e) => Err(format!(
            "{e}, so name the Debian build to repack with --distro ({})",
            names()
        )),
    }
}

/// What distribution this host is, by the codename its own `/etc/os-release` states.
fn host_codename() -> Result<String, String> {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .map_err(|e| format!("cannot read /etc/os-release: {e}"))?;
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("VERSION_CODENAME="))
        .map(|name| name.trim_matches('"').to_string())
        .filter(|codename| !codename.is_empty())
        .ok_or_else(|| {
            "this host's /etc/os-release names no VERSION_CODENAME, so which vendor build to \
             repack cannot be derived — name it with `--distro <codename>`"
                .to_string()
        })
}

/// Where Icinga publishes what this tool repacks.
const ICINGA_REPO: &str = "https://packages.icinga.com/debian";

/// The key Icinga signs its Debian repositories with, carried in the tool (ADR-draft-icinga-2
/// clause 18): `Icinga GmbH (Build server) <info@icinga.com>`, fingerprint
/// `DD3A F619 8ED0 00B4 C0B7 3956 CC11 6F55 AA7F 2382`. A binary keyring, because that is what
/// `gpgv` reads. A repository index signed by any other key is refused.
const ICINGA_ARCHIVE_KEY: &[u8] = include_bytes!("../trust/icinga-archive.gpg");

/// The primary key a signature over Icinga's index must be made by, or by a subkey of.
const ICINGA_PRIMARY_KEYS: &[&str] = &["DD3AF6198ED000B4C0B73956CC116F55AA7F2382"];

/// The keys Debian signs its archives with, carried in the tool like Icinga's (ADR-draft-icinga-2
/// clause 18) rather than read from the host, so what the tree carries depends on no file of the
/// host that repacks it. A binary keyring of the six automatic signing keys of Debian 11, 12 and
/// 13, as `ftp-master.debian.org/keys.html` lists them and the `debian-archive-keyring` package
/// carries them:
///
/// - `1F89 983E 0081 FDE0 18F3  CC96 73A4 F27B 8DD4 7936` archive, 11/bullseye
/// - `AC53 0D52 0F2F 3269 F5E9  8313 A484 4904 4AAD 5C5D` security archive, 11/bullseye
/// - `B8B8 0B5B 623E AB6A D877  5C45 B7C5 D7D6 3509 47F8` archive, 12/bookworm
/// - `05AB 9034 0C0C 5E79 7F44  A8C8 254C F3B5 AEC0 A8F0` security archive, 12/bookworm
/// - `04B5 4C3C DCA7 9751 B16B  C6B5 2256 29DF 75B1 88BD` archive, 13/trixie
/// - `5E04 A1E3 223A 19A2 0706  E20F 9904 613D 4CCE 68C6` security archive, 13/trixie
///
/// Every suite this tool reads is signed by at least one of them; the per-release stable keys sign
/// beside them and are not needed.
const DEBIAN_ARCHIVE_KEY: &[u8] = include_bytes!("../trust/debian-archive.gpg");

/// The archive primaries of [`DEBIAN_ARCHIVE_KEY`], one of which, or a subkey of it, must have
/// made a signature over the index of a Debian suite or its `-updates`. Debian signs with subkeys,
/// so the key is told by the primary fingerprint `gpgv` states last on its `VALIDSIG` line.
const DEBIAN_ARCHIVE_PRIMARY_KEYS: &[&str] = &[
    "1F89983E0081FDE018F3CC9673A4F27B8DD47936",
    "B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8",
    "04B54C3CDCA79751B16BC6B5225629DF75B188BD",
];

/// The security archive primaries of [`DEBIAN_ARCHIVE_KEY`], one of which, or a subkey of it,
/// must have made a signature over the index of a Debian `-security` suite.
const DEBIAN_SECURITY_PRIMARY_KEYS: &[&str] = &[
    "AC530D520F2F3269F5E98313A48449044AAD5C5D",
    "05AB90340C0C5E797F44A8C8254CF3B5AEC0A8F0",
    "5E04A1E3223A19A20706E20F9904613D4CCE68C6",
];

/// The largest uncompressed `Packages` index this tool reads. Debian's own is about 50 MB; the
/// signed index states the exact size, and this bounds what that statement may ask for.
const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;

/// Where the *check plugins* come from. They are not Icinga's to publish — `monitoring-plugins` is
/// its own project, packaged by the distribution — but an Icinga Agent without `check_disk` and its
/// siblings can barely check anything, so the artifact carries them
/// (ADR-draft-icinga-2's "(+ plugins)").
const DEBIAN_REPO: &str = "https://deb.debian.org/debian";

/// Where Debian publishes its security updates, a suite `<codename>-security` of its own. The
/// shared libraries the tree carries — OpenSSL above all — are taken at the newest version any of
/// the suite, its `-updates` and its `-security` states, as an `apt` with Debian's default sources
/// would install them.
const DEBIAN_SECURITY_REPO: &str = "https://deb.debian.org/debian-security";

/// What Icinga 2 offers for one distribution build, read out of that repository's own index.
///
/// Two packages make one tree: `icinga2-bin` carries the daemon, `icinga2-common` the ITL. Their
/// SHA-256 comes from the `Packages` index — Icinga signs repositories with GPG rather than
/// publishing per-file checksums, and the index is where the digests live, read only once the
/// repository's signature over it verified ([`signed_index`], ADR-draft-icinga-2). The check
/// plugins and every shared library the tree needs come from Debian's archive the same way.
///
/// Only `linux/amd64` for now, and deliberately: an artifact's reach is decided by the suite it
/// was built from, so each build is its own act rather than a loop over architectures. No Windows
/// artifact is offered until a repacked tree has run on a Windows host (ADR-draft-icinga-2 clause
/// 23).
async fn icinga2_plans(version: &str, distro: &str) -> Result<Vec<Plan>, String> {
    let packages = signed_index(ICINGA_REPO, &format!("icinga-{distro}"), &ICINGA_ARCHIVE).await?;
    let mut tree: Vec<&DebEntry> = Vec::new();
    // The highest packaging revision of this release, and the other package at exactly that one:
    // a rebuild of the same Icinga version replaces the earlier one in the vendor's own order.
    let revision = newest(packages.iter().filter(|entry| {
        entry.package == "icinga2-bin" && entry.version.starts_with(&format!("{version}-"))
    }))
    .map(|entry| entry.version.clone())
    .ok_or_else(|| {
        format!(
            "{distro} publishes no icinga2-bin {version} — check the version and the distribution"
        )
    })?;
    for package in ["icinga2-bin", "icinga2-common"] {
        let entry = packages
            .iter()
            .find(|entry| entry.package == package && entry.version == revision)
            .ok_or_else(|| format!("{distro} publishes no {package} {revision}"))?;
        tree.push(entry);
    }

    // The check plugins, from the distribution rather than from Icinga: `-basic` is check_disk,
    // check_load, check_procs and the rest, `-common` the helpers several of them source at run
    // time. Whatever version that distribution ships — they are not versioned with Icinga.
    let debian = debian_index(distro).await?;
    for package in ["monitoring-plugins-basic", "monitoring-plugins-common"] {
        let entry = newest(debian.iter().filter(|entry| entry.package == package))
            .ok_or_else(|| format!("{distro} publishes no {package}"))?;
        tree.push(entry);
    }
    // The shared libraries, as Debian's packages of the same suite: what the four packages declare
    // they need, followed through the library packages, glibc left out (ADR-draft-icinga-2
    // clause 18). Which of their files the tree carries is decided after unpacking, by what the
    // programs link against.
    let libraries = library_packages(&tree, &debian);
    eprintln!(
        "  {} library packages from Debian {distro}: {}",
        libraries.len(),
        libraries
            .iter()
            .map(|entry| entry.package.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    // How old a libc may be, as the packages themselves state it. It is the artifact's reach, and
    // it belongs in front of an operator before the rollout rather than after it
    // (ADR-draft-icinga-2).
    if let Some(floor) = glibc_floor(tree.iter().chain(&libraries).copied()) {
        eprintln!("  this build needs glibc >= {floor} on every host it is rolled out to");
    }
    let tree_packages = tree.len();
    let sources = tree
        .into_iter()
        .chain(libraries)
        .map(|entry| Download {
            url: format!("{}/{}", entry.repo, entry.filename),
            checksum: ChecksumSource::Known {
                sha256: entry.sha256.clone(),
            },
        })
        .collect();
    Ok(vec![linux_plan(version, sources, tree_packages)])
}

/// Icinga 2's Linux artifact for `version`: the `sources`, of which the first `tree_packages` make
/// the tree, repacked under the wrapper `icinga2-<version>` (ADR-draft-icinga-2 clause 17).
fn linux_plan(version: &str, sources: Vec<Download>, tree_packages: usize) -> Plan {
    Plan {
        os: "linux".to_string(),
        arch: "amd64".to_string(),
        sources,
        action: Action::RepackDebs {
            wrapper: format!("icinga2-{version}"),
            tree_packages,
        },
        out_name: format!("icinga2_{version}_linux_amd64.tar.gz"),
        block_hint: "type = \"icinga2\"  (docs/artifacts/icinga2.md)".to_string(),
    }
}

/// Debian's packages for `distro`: the suite, its `-updates` and its `-security`, each verified
/// against the carried keys, in one list — [`newest`] then picks across them.
async fn debian_index(distro: &str) -> Result<Vec<DebEntry>, String> {
    let mut entries = signed_index(DEBIAN_REPO, distro, &DEBIAN_ARCHIVE).await?;
    entries.extend(signed_index(DEBIAN_REPO, &format!("{distro}-updates"), &DEBIAN_ARCHIVE).await?);
    entries.extend(
        signed_index(
            DEBIAN_SECURITY_REPO,
            &format!("{distro}-security"),
            &DEBIAN_SECURITY_ARCHIVE,
        )
        .await?,
    );
    Ok(entries)
}

/// One stanza of a Debian `Packages` index — the fields this tool needs and no others.
#[derive(Debug)]
struct DebEntry {
    /// The repository the index was read from, which `filename` is relative to.
    repo: String,
    package: String,
    version: String,
    filename: String,
    sha256: String,
    depends: String,
    pre_depends: String,
    provides: String,
    section: String,
}

/// Whose signature an index needs: the keyring the tool carries for that archive, and the primary
/// keys in it a signature must be made by.
struct Archive {
    key: &'static [u8],
    primaries: &'static [&'static str],
}

/// Icinga's repositories.
const ICINGA_ARCHIVE: Archive = Archive {
    key: ICINGA_ARCHIVE_KEY,
    primaries: ICINGA_PRIMARY_KEYS,
};

/// Debian's archive, for a suite and its `-updates`.
const DEBIAN_ARCHIVE: Archive = Archive {
    key: DEBIAN_ARCHIVE_KEY,
    primaries: DEBIAN_ARCHIVE_PRIMARY_KEYS,
};

/// Debian's security archive, for a `-security` suite.
const DEBIAN_SECURITY_ARCHIVE: Archive = Archive {
    key: DEBIAN_ARCHIVE_KEY,
    primaries: DEBIAN_SECURITY_PRIMARY_KEYS,
};

/// Reads a suite's `Packages` index the way `apt` trusts it (ADR-draft-icinga-2 clause 18): the
/// suite's `InRelease` is verified with `gpgv` against the archive's carried key, is refused
/// unless it names the suite it was fetched for and once the `Valid-Until` it states has passed,
/// and the index is checked against the SHA-256 and the size that signed text states for it,
/// compressed and uncompressed, before a line of it is read. TLS alone would put the digests —
/// and so every package — in the hands of whoever serves or mirrors the repository.
async fn signed_index(repo: &str, suite: &str, archive: &Archive) -> Result<Vec<DebEntry>, String> {
    let release_url = format!("{repo}/dists/{suite}/InRelease");
    eprintln!("  reading {release_url} …");
    let inrelease = get_bytes(&release_url).await?;
    let release = crate::gpgv::verify_clearsigned(&inrelease, archive.key, archive.primaries)
        .and_then(|release| release_names(&release, suite).map(|()| release))
        .and_then(|release| {
            release_current(&release, time::OffsetDateTime::now_utc()).map(|()| release)
        });
    let release = release.map_err(|e| format!("{release_url}: {e}"))?;
    eprintln!("  verified the repository's signature");
    let compression = index_compression(&release).ok_or_else(|| {
        format!("{release_url}: the signed index names no {INDEX_PATH} this tool reads")
    })?;
    let index_url = format!("{repo}/dists/{suite}/{INDEX_PATH}.{compression}");
    eprintln!("  reading {index_url} …");
    let compressed = get_bytes(&index_url).await?;
    let text = verified_index(&release, compression, &compressed)
        .map_err(|e| format!("{index_url}: {e}"))?;
    let mut entries = parse_deb_index(&text);
    for entry in &mut entries {
        entry.repo = repo.to_string();
    }
    Ok(entries)
}

/// The compressed forms of the `Packages` index this tool reads, the preferred first: Debian's
/// `-updates` and `-security` suites publish only `.xz`, Icinga's repositories only `.gz`.
const INDEX_COMPRESSIONS: [&str; 2] = ["xz", "gz"];

/// The first form of [`INDEX_COMPRESSIONS`] the signed `Release` text states a digest for.
fn index_compression(release: &str) -> Option<&'static str> {
    INDEX_COMPRESSIONS
        .into_iter()
        .find(|ext| release_digest(release, &format!("{INDEX_PATH}.{ext}")).is_some())
}

/// The index this tool reads, as the signed `InRelease` names it.
const INDEX_PATH: &str = "main/binary-amd64/Packages";

/// Refuses a signed `Release` text that names neither as its `Codename` nor as its `Suite` the
/// suite it was fetched for, as `apt` refuses a conflicting distribution. A signature covers the
/// text and not the path it was served under, so without it whoever serves the repository could
/// answer for one suite with another's genuine index: an archived `-security` index, which states
/// no `Valid-Until`, in place of the live one, or another distribution's build of Icinga.
fn release_names(release: &str, suite: &str) -> Result<(), String> {
    let field = |name: &str| {
        release
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
    };
    let (codename, stated) = (field("Codename"), field("Suite"));
    if codename == Some(suite) || stated == Some(suite) {
        return Ok(());
    }
    Err(format!(
        "is signed for {}, not for {suite:?} it was fetched as — refusing it",
        codename
            .or(stated)
            .map_or_else(|| "no suite".to_string(), |named| format!("{named:?}"))
    ))
}

/// Refuses a signed `Release` text whose `Valid-Until` lies before `now`, as `apt` does: a
/// signature never expires, so without it an old index — with the libraries it named before a
/// security fix — could be served again and would still verify. A text that states no
/// `Valid-Until`, as Icinga's and Debian's main suites do, is not bounded by one.
fn release_current(release: &str, now: time::OffsetDateTime) -> Result<(), String> {
    let format = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] UTC"
    );
    let Some(stated) = release
        .lines()
        .find_map(|line| line.strip_prefix("Valid-Until:"))
        .map(str::trim)
    else {
        return Ok(());
    };
    let until = time::PrimitiveDateTime::parse(stated, format)
        .map_err(|_| format!("states a Valid-Until this tool cannot read: {stated:?}"))?
        .assume_utc();
    if until < now {
        return Err(format!(
            "was valid until {stated}, so it may be an old index served again — refusing it"
        ));
    }
    Ok(())
}

/// The SHA-256 and size the signed `Release` text states for `path`.
fn release_digest<'r>(release: &'r str, path: &str) -> Option<(&'r str, u64)> {
    release
        .lines()
        .skip_while(|line| line.trim_end() != "SHA256:")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let (hash, size, name) = (fields.next()?, fields.next()?, fields.next()?);
            (name == path).then_some(())?;
            Some((hash, size.parse().ok()?))
        })
}

/// The `Packages` index out of its compressed bytes — `compression` one of
/// [`INDEX_COMPRESSIONS`] — accepted only as the signed `Release` text states it: the compressed
/// bytes match their digest and size, and the decompressed text matches its own — read no further
/// than the size stated, so a stream that inflates past it is refused rather than followed.
fn verified_index(release: &str, compression: &str, compressed: &[u8]) -> Result<String, String> {
    use sha2::Digest as _;
    let (packed_hash, packed_size) =
        release_digest(release, &format!("{INDEX_PATH}.{compression}"))
            .ok_or_else(|| format!("the signed index names no {INDEX_PATH}.{compression}"))?;
    if compressed.len() as u64 != packed_size
        || hex::encode(sha2::Sha256::digest(compressed)) != packed_hash
    {
        return Err(
            "does not match the SHA-256 and size the signed InRelease states for it".to_string(),
        );
    }
    let (hash, size) = release_digest(release, INDEX_PATH)
        .ok_or_else(|| format!("the signed index names no {INDEX_PATH}"))?;
    if size > MAX_INDEX_BYTES {
        return Err(format!(
            "the signed index states {size} bytes for {INDEX_PATH}, more than this tool reads"
        ));
    }
    let text = match compression {
        "gz" => {
            let mut text = Vec::new();
            flate2::read::GzDecoder::new(compressed)
                .take(size + 1)
                .read_to_end(&mut text)
                .map_err(|e| format!("cannot decompress: {e}"))?;
            text
        }
        "xz" => unxz(compressed, size + 1)?,
        other => return Err(format!("this tool reads no .{other} index")),
    };
    if text.len() as u64 != size || hex::encode(sha2::Sha256::digest(&text)) != hash {
        return Err(format!(
            "decompresses to something other than the {INDEX_PATH} the signed InRelease states"
        ));
    }
    String::from_utf8(text).map_err(|_| format!("{INDEX_PATH} is not text"))
}

/// At most `limit` bytes of what `xz` decompresses `compressed` to. Shelled out to like `gpgv`
/// and `dpkg-deb` (ADR-draft-icinga-2 clause 21): the bytes it is handed already matched the
/// digest the signed `Release` states, and what it answers is checked against that text's own.
fn unxz(compressed: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("xz")
        .args(["--decompress", "--stdout", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run xz, which decompresses Debian's signed index (install xz-utils): {e}"
            )
        })?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let mut text = Vec::new();
    let read = std::thread::scope(|scope| {
        // Written from a thread of its own, since `xz` writes while it reads; a write cut short
        // because the read below stopped at the limit is no error of its own.
        scope.spawn(move || stdin.take().map(|mut input| input.write_all(compressed)));
        let read = stdout
            .map(|out| out.take(limit).read_to_end(&mut text))
            .transpose();
        if text.len() as u64 >= limit {
            let _ = child.kill();
        }
        read
    });
    let status = child.wait().map_err(|e| format!("cannot run xz: {e}"))?;
    read.map_err(|e| format!("cannot decompress: {e}"))?;
    if !status.success() && (text.len() as u64) < limit {
        return Err("xz could not decompress it".to_string());
    }
    Ok(text)
}

/// The entry with the highest version among `entries`, in Debian's own order.
fn newest<'e>(entries: impl Iterator<Item = &'e DebEntry>) -> Option<&'e DebEntry> {
    entries.max_by(|a, b| deb_version_cmp(&a.version, &b.version))
}

/// Debian's version order (`dpkg --compare-versions`): the epoch numerically, then the upstream
/// version, then the revision, each by `verrevcmp` — `~` before everything, the end, letters,
/// then other characters; digit runs by their value.
fn deb_version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(version: &str) -> (u64, &str, &str) {
        let (epoch, rest) = match version.split_once(':') {
            Some((epoch, rest)) if epoch.bytes().all(|b| b.is_ascii_digit()) => {
                (epoch.parse().unwrap_or(0), rest)
            }
            _ => (0, version),
        };
        let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, ""));
        (epoch, upstream, revision)
    }
    let (a, b) = (split(a), split(b));
    a.0.cmp(&b.0)
        .then_with(|| verrevcmp(a.1, b.1))
        .then_with(|| verrevcmp(a.2, b.2))
}

/// dpkg's `verrevcmp`, over bytes.
fn verrevcmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn order(c: Option<u8>) -> i32 {
        match c {
            None => 0,
            Some(b'~') => -1,
            Some(c) if c.is_ascii_digit() => 0,
            Some(c) if c.is_ascii_alphabetic() => i32::from(c),
            Some(c) => i32::from(c) + 256,
        }
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let digit = |s: &[u8], i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !digit(a, i)) || (j < b.len() && !digit(b, j)) {
            let (x, y) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first_diff = Ordering::Equal;
        while digit(a, i) && digit(b, j) {
            if first_diff == Ordering::Equal {
                first_diff = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if digit(a, i) {
            return Ordering::Greater;
        }
        if digit(b, j) {
            return Ordering::Less;
        }
        if first_diff != Ordering::Equal {
            return first_diff;
        }
    }
    Ordering::Equal
}

/// The parse of a `Packages` index [`signed_index`] verified, separated from the fetch so it can be
/// tested against a stanza rather than against the network. RFC 822-ish stanzas separated by
/// blank lines; only the single-line fields matter here, so continuation lines are simply not
/// fields.
fn parse_deb_index(text: &str) -> Vec<DebEntry> {
    let mut entries = Vec::new();
    for stanza in text.split("\n\n") {
        let field = |name: &str| {
            stanza
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}: ")))
                .unwrap_or_default()
                .to_string()
        };
        let package = field("Package");
        if package.is_empty() {
            continue;
        }
        entries.push(DebEntry {
            repo: String::new(),
            package,
            version: field("Version"),
            filename: field("Filename"),
            sha256: field("SHA256"),
            depends: field("Depends"),
            pre_depends: field("Pre-Depends"),
            provides: field("Provides"),
            section: field("Section"),
        });
    }
    entries
}

/// The package names of a relationship field such as `Depends`, one list of alternatives per
/// entry, without version constraints or architecture qualifiers.
fn relationships(field: &str) -> Vec<Vec<&str>> {
    field
        .split(',')
        .map(|entry| {
            entry
                .split('|')
                .filter_map(|alternative| alternative.split_whitespace().next())
                .map(|name| name.split(':').next().unwrap_or(name))
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|alternatives| !alternatives.is_empty())
        .collect()
}

/// The packages the host provides and the tree never carries: glibc and its loader
/// (ADR-draft-icinga-2 clause 18), which are one package in Debian.
const GLIBC_PACKAGES: [&str; 1] = ["libc6"];

/// The package an index answers a relationship's name with: the real package of that name at its
/// newest version, else — a virtual name such as `libboost-regex1.74.0-icu72` — the first package
/// by name that `Provides` it, at its newest. Both orders are the index's own, so the same index
/// gives the same answer.
fn resolve_package<'e>(name: &str, index: &'e [DebEntry]) -> Option<&'e DebEntry> {
    newest(index.iter().filter(|entry| entry.package == name)).or_else(|| {
        let provides = |entry: &&DebEntry| {
            relationships(&entry.provides)
                .iter()
                .flatten()
                .any(|provided| *provided == name)
        };
        let first = index
            .iter()
            .filter(provides)
            .map(|entry| entry.package.as_str())
            .min()?;
        newest(
            index
                .iter()
                .filter(provides)
                .filter(|entry| entry.package == first),
        )
    })
}

/// The library packages the tree's own packages need, out of a verified index: their `Depends` and
/// `Pre-Depends`, followed through every package of Debian's `libs` section, each relationship
/// answered by the first alternative the index holds ([`resolve_package`]). glibc is left out,
/// since the host provides it, and so is everything outside `libs` — `adduser`, `procps`, `ucf` are
/// what an installation runs, not what a program links. Sorted by name, so the plan is the same
/// whichever order the index came in.
///
/// The closure only decides what is downloaded. Which of its files the tree carries is decided by
/// what the programs link against once unpacked ([`bundle_libraries`]), and a library none of
/// these packages holds fails the repack there.
fn library_packages<'e>(roots: &[&DebEntry], index: &'e [DebEntry]) -> Vec<&'e DebEntry> {
    let is_library = |entry: &DebEntry| entry.section == "libs" || entry.section.ends_with("/libs");
    let needs = |entry: &DebEntry| {
        let mut names: Vec<String> = Vec::new();
        for field in [&entry.pre_depends, &entry.depends] {
            for alternatives in relationships(field) {
                if let Some(found) = alternatives
                    .iter()
                    .find_map(|name| resolve_package(name, index))
                {
                    names.push(found.package.clone());
                }
            }
        }
        names
    };
    let mut seen: std::collections::BTreeMap<String, &'e DebEntry> = Default::default();
    let mut pending: Vec<String> = roots.iter().flat_map(|entry| needs(entry)).collect();
    while let Some(name) = pending.pop() {
        if seen.contains_key(&name) || GLIBC_PACKAGES.contains(&name.as_str()) {
            continue;
        }
        let Some(entry) = resolve_package(&name, index).filter(|entry| is_library(entry)) else {
            continue;
        };
        seen.insert(name, entry);
        pending.extend(needs(entry));
    }
    seen.into_values().collect()
}

/// The oldest glibc every one of `entries` runs on: the highest `libc6 (>= …)` their `Depends` and
/// `Pre-Depends` state. A bundled library may need a newer glibc than the program that links it —
/// bookworm's `libstdc++6` states 2.36 where `icinga2-bin` states 2.34 — so the tree's floor is
/// the highest of all of them, not the vendor package's alone.
fn glibc_floor<'e>(entries: impl Iterator<Item = &'e DebEntry>) -> Option<String> {
    entries
        .flat_map(|entry| [libc_floor(&entry.pre_depends), libc_floor(&entry.depends)])
        .flatten()
        .max_by(|a, b| deb_version_cmp(a, b))
}

/// The `libc6 (>= 2.34)` out of a `Depends` field — the oldest glibc that package runs on.
fn libc_floor(depends: &str) -> Option<String> {
    depends.split(',').find_map(|dep| {
        let dep = dep.trim();
        let rest = dep.strip_prefix("libc6 (>= ")?;
        rest.strip_suffix(')').map(str::to_string)
    })
}

/// The Collector publishes `<dist>_<version>_<os>_<arch>.tar.gz` for every platform it supports,
/// so the platform list *is* the asset list — no table here goes stale when upstream adds one.
fn collector_plans(agent: AgentKind, version: &str, assets: &[(String, String)]) -> Vec<Plan> {
    let dist = agent.source().service_name;
    let prefix = format!("{dist}_{version}_");
    let mut plans = Vec::new();
    for (name, url) in assets {
        // `otelcol-contrib_…` also starts with `otelcol_`-like text under a naive check, so the
        // remainder is split off the full prefix and must be exactly `<os>_<arch>.tar.gz`.
        let Some((os, arch)) = platform_of(name, &prefix) else {
            continue;
        };
        let program = if os == "windows" {
            format!("{dist}.exe")
        } else {
            dist.to_string()
        };
        plans.push(Plan {
            os: os.clone(),
            arch,
            sources: vec![Download {
                url: url.clone(),
                checksum: collector_checksum(name, dist, assets),
            }],
            action: Action::AsPublished,
            out_name: name.clone(),
            block_hint: format!("binary = {program:?}  (type = \"collector\")"),
        });
    }
    plans.sort_by(|a, b| (&a.os, &a.arch).cmp(&(&b.os, &b.arch)));
    plans
}

/// The platform an asset named `<prefix><os>_<arch>.tar.gz` is for, in the words an Agent reports
/// — or `None` when the name has another shape or a platform this fleet does not know.
fn platform_of(name: &str, prefix: &str) -> Option<(String, String)> {
    let (os, arch) = name
        .strip_prefix(prefix)?
        .strip_suffix(".tar.gz")?
        .split_once('_')?;
    Some((normalize_os(os)?, normalize_arch(arch)?))
}

/// The platforms this project's own release publishes, in the order its build matrix states them
/// (`.github/workflows/release.yml`). The agent menu names them
/// ([`AgentKind::platforms`]); what is actually fetched comes from the release's assets, so this
/// list understating a new target costs a menu line and never a download.
const SUPERVISOR_PLATFORMS: [(&str, &str); 5] = [
    ("linux", "amd64"),
    ("linux", "arm64"),
    ("darwin", "arm64"),
    ("darwin", "amd64"),
    ("windows", "amd64"),
];

/// This fleet's own Client, as its release publishes it: one `.tar.gz` per platform named
/// `supervisor_<version>_<os>_<arch>` (ADR-0034), holding the binary under the name it is installed
/// as. Nothing is repacked — the artifact *is* the package, which is the point of packing the
/// release with this project's own packer.
///
/// The `.deb`, `.rpm` and `.msi` of the same release are installers for a machine to run, not
/// artifacts a fleet distributes, so they are passed over exactly as the Collector's MSI is.
fn supervisor_plans(version: &str, assets: &[(String, String)]) -> Vec<Plan> {
    let package = AgentKind::Supervisor.source().service_name;
    let prefix = format!("{package}_{version}_");
    // One file for the whole release, written by the publish job from every artifact's own
    // checksum — `<hash>  <name>` lines, which is the shape `ChecksumSource::Sums` looks up by.
    let sums: Vec<String> = assets
        .iter()
        .filter(|(name, _)| name == "SHA256SUMS")
        .map(|(_, url)| url.clone())
        .collect();
    let mut plans = Vec::new();
    for (name, url) in assets {
        let Some((os, arch)) = platform_of(name, &prefix) else {
            continue;
        };
        plans.push(Plan {
            os,
            arch,
            sources: vec![Download {
                url: url.clone(),
                checksum: ChecksumSource::Sums { urls: sums.clone() },
            }],
            action: Action::AsPublished,
            out_name: name.clone(),
            // Not a `[[supervisor]]` block: a Client installs this one over itself, and what
            // admits that is the consent in its own `supervisor.toml` (ADR-0018), which takes
            // only the package named after its Agent type and stands by default, so most hosts
            // need no line at all.
            block_hint: "[packages] self_update = true  (the default)".to_string(),
        });
    }
    plans.sort_by(|a, b| (&a.os, &a.arch).cmp(&(&b.os, &b.arch)));
    plans
}

/// The Collector's checksum, whichever way this release publishes it.
///
/// Up to 0.157.0 it was one file per distribution holding every asset's line; from 0.158.0 each
/// asset has a sidecar holding a bare digest. Both are release assets, so which one exists is
/// visible in the list rather than something to infer from the version number — a rule by version
/// would be wrong for exactly one release and no one would find out until it broke.
fn collector_checksum(asset: &str, dist: &str, assets: &[(String, String)]) -> ChecksumSource {
    let sidecar = format!("{asset}.sha256");
    if let Some((_, url)) = assets.iter().find(|(name, _)| *name == sidecar) {
        return ChecksumSource::BareDigest { url: url.clone() };
    }
    // Every combined file this distribution has, most specific first: 0.157.0 and earlier put the
    // Windows assets in `…_<dist>_windows_checksums.txt` and the rest in `…_<dist>_checksums.txt`.
    let prefix = format!("opentelemetry-collector-releases_{dist}_");
    let mut urls: Vec<String> = assets
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix) && name.ends_with("checksums.txt"))
        .map(|(_, url)| url.clone())
        .collect();
    urls.sort_by_key(|url| std::cmp::Reverse(url.len()));
    ChecksumSource::Sums { urls }
}

/// Upstream's platform words in this fleet's vocabulary (ADR-0027), or `None` for one this fleet
/// has no name for — an Agent reports `os.type` and `host.arch`, and a package entry no Agent can
/// match is one nobody would ever be offered.
fn normalize_os(os: &str) -> Option<String> {
    match os {
        "linux" | "windows" | "darwin" => Some(os.to_string()),
        "macos" => Some("darwin".to_string()),
        _ => None,
    }
}

fn normalize_arch(arch: &str) -> Option<String> {
    match arch {
        "amd64" | "arm64" | "386" => Some(arch.to_string()),
        "x86_64" => Some("amd64".to_string()),
        "aarch64" => Some("arm64".to_string()),
        "i386" => Some("386".to_string()),
        _ => None,
    }
}

// ── Producing an artifact ───────────────────────────────────────────────────

/// Downloads one platform's artifact, verifies it against what upstream published, and applies
/// whatever [`Action`] the plan carries. Returns the file that is now ready to upload.
async fn produce(plan: &Plan, out_dir: &Path) -> Result<PathBuf, String> {
    eprintln!("\n{}/{}", plan.os, plan.arch);
    let mut fetched = Vec::new();
    for source in &plan.sources {
        let downloaded = out_dir.join(
            Path::new(&source.url)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| plan.out_name.clone()),
        );
        eprintln!("  downloading {} …", source.url);
        download(&source.url, &downloaded).await?;

        let published = published_sha256(&source.checksum, &downloaded).await?;
        let actual = hex::encode(sha256_file(&downloaded)?);
        if actual != published {
            // The file stays for inspection: a mismatch is either a truncated download or
            // something that deserves a look, and deleting the evidence serves neither.
            return Err(format!(
                "{} does not match the SHA-256 upstream published\n  upstream: {published}\n  \
                 downloaded: {actual}",
                downloaded.display()
            ));
        }
        eprintln!("  verified against upstream's SHA-256");
        fetched.push(downloaded);
    }
    // Every plan but Icinga's has exactly one; the ones that act on "the download" mean this.
    let downloaded = fetched
        .first()
        .cloned()
        .ok_or_else(|| "a plan with nothing to fetch".to_string())?;

    // A kind that repacks answers its step here; the rest of a repack is the same for every kind.
    let artifact = out_dir.join(&plan.out_name);
    let repacked: Option<Result<(), String>> = match &plan.action {
        Action::AsPublished => None,
        Action::RepackDebs {
            wrapper,
            tree_packages,
        } => Some(repack_debs(&fetched, *tree_packages, wrapper, &artifact)),
    };
    let Some(repacked) = repacked else {
        eprintln!("  as published — the fleet verifies upstream's own hash");
        return Ok(downloaded);
    };
    repacked?;
    // What was downloaded was a means, not a result; leaving it beside the artifact invites
    // uploading the wrong file.
    for file in &fetched {
        let _ = std::fs::remove_file(file);
    }
    eprintln!(
        "  repacked  sha256 {}",
        hex::encode(sha256_file(&artifact)?)
    );
    Ok(artifact)
}

/// The SHA-256 upstream published for this artifact, from whichever form the source uses.
async fn published_sha256(source: &ChecksumSource, artifact: &Path) -> Result<String, String> {
    let urls = match source {
        // Already read out of a repository index (ADR-draft-icinga-2) — there is nothing to fetch.
        ChecksumSource::Known { sha256 } => return Ok(sha256.clone()),
        ChecksumSource::BareDigest { url } => {
            let text = String::from_utf8(get_bytes(url).await?)
                .map_err(|_| format!("{url} is not a checksum file"))?;
            return Ok(text
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string());
        }
        ChecksumSource::Sums { urls } => urls,
    };
    if urls.is_empty() {
        return Err("upstream published no checksum for this artifact".to_string());
    }
    for url in urls {
        let text = String::from_utf8(get_bytes(url).await?)
            .map_err(|_| format!("{url} is not a checksum file"))?;
        if let Some(hash) = parse_sums(&text, artifact) {
            return Ok(hash);
        }
    }
    Err(format!(
        "none of the checksum files upstream published has a line for {} ({})",
        artifact.file_name().unwrap_or_default().to_string_lossy(),
        urls.join(", ")
    ))
}

/// The artifact's digest out of a `sha256sum`-style file: `<hash>  <name>` lines, where the name
/// may carry a `*` for binary mode or a path in front of it. Matched on the file name alone, which
/// is the one thing every one of these files spells the same way.
fn parse_sums(text: &str, artifact: &Path) -> Option<String> {
    let wanted = artifact.file_name()?.to_string_lossy().into_owned();
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| {
            Path::new(name.trim().trim_start_matches('*'))
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == wanted)
        })
        .map(|(hash, _)| hash.trim().to_string())
}

/// Every regular file of that name below `root`, sorted, found without following a link.
fn files_below(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(parent) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.file_name().is_some_and(|n| n == name) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Turns the vendor's Debian packages into one normalised, link-free tree (ADR-draft-icinga-2).
///
/// The layout is normalised rather than kept, so that **one** `program_path` serves every
/// distribution: Debian puts the binary under `/usr/lib/<triplet>/icinga2/sbin` and RHEL under
/// `/usr/lib64/icinga2/sbin`, and a block should not have to know which. What comes along is the
/// daemon, the ITL, the check plugin the package ships, the copyright files — repacking is
/// redistribution — and the shared libraries the daemon and the plugins link against, out of the
/// library packages that follow the first `tree_packages` of `debs`.
///
/// Left out on purpose: `/etc/icinga2` (the fleet delivers configuration), the systemd unit and
/// init script, and the `prepare-dirs`/`safe-reload` helpers, which need a `nagios` account that a
/// fleet-managed host does not have — the Supervisor does that work instead (ADR-draft-icinga-2).
fn repack_debs(
    debs: &[PathBuf],
    tree_packages: usize,
    wrapper: &str,
    out: &Path,
) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("repacking Debian packages needs dpkg-deb, so it runs on Linux".to_string());
    }
    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    let extracted = staging.join("extracted");
    // The library packages apart from the tree's own: only the shared objects the programs link
    // against are taken out of them, and nothing of theirs is looked for by name. Each group is
    // unpacked into one directory, and a link is resolved inside its group's directory
    // (ADR-draft-icinga-2 clause 18).
    let libraries = staging.join("libraries");
    let tree = staging.join("tree");
    for dir in [&extracted, &libraries] {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }

    for (n, deb) in debs.iter().enumerate() {
        let into = if n < tree_packages {
            &extracted
        } else {
            &libraries
        };
        // Unpacked, not installed: `dpkg-deb -x` writes the payload and runs none of the
        // package's scripts.
        let status = std::process::Command::new("dpkg-deb")
            .arg("-x")
            .arg(deb)
            .arg(into)
            .status()
            .map_err(|e| format!("cannot run dpkg-deb (is it installed?): {e}"))?;
        if !status.success() {
            return Err(format!("dpkg-deb could not unpack {}", deb.display()));
        }
        if n < tree_packages {
            apply_alternatives(deb, &staging, &extracted)?;
        }
    }

    for dir in ["sbin", "lib", "share/icinga2", "plugins", "doc"] {
        std::fs::create_dir_all(tree.join(dir))
            .map_err(|e| format!("cannot create {}: {e}", tree.join(dir).display()))?;
    }
    // The real binary, not `/usr/sbin/icinga2` — that is a shell wrapper, and what the Supervisor
    // spawns has to be the process it then watches.
    let binary = find_file(&extracted.join("usr/lib"), "icinga2")?;
    copy_from_tree(&extracted, &binary, &tree.join("sbin/icinga2"))?;
    copy_from_tree(
        &extracted,
        &extracted.join("usr/share/icinga2/include"),
        &tree.join("share/icinga2/include"),
    )?;
    let plugins = extracted.join("usr/lib/nagios/plugins");
    if plugins.is_dir() {
        copy_from_tree(&extracted, &plugins, &tree.join("plugins"))?;
    }
    // Some plugins source these at run time (`utils.sh`, `utils.pm`); without them a check fails
    // with a message about a missing file rather than about what it was checking.
    for helper in ["usr/share/monitoring-plugins", "usr/lib/monitoring-plugins"] {
        let dir = extracted.join(helper);
        if dir.is_dir() {
            copy_from_tree(&extracted, &dir, &tree.join("plugins"))?;
        }
    }
    // Repacking is redistribution, so the copyright file of every package downloaded travels:
    // which library packages the tree carries files of is known only once the libraries are
    // gathered, and one too many errs toward attribution. The changelogs stay behind, weight
    // nobody unpacks on a monitored host (ADR-draft-icinga-2 clause 17). A pool file is
    // `<package>_<version>_<arch>.deb`.
    for (n, deb) in debs.iter().enumerate() {
        let root = if n < tree_packages {
            &extracted
        } else {
            &libraries
        };
        let Some(package) = deb
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split('_').next())
        else {
            continue;
        };
        let copyright = root.join("usr/share/doc").join(package).join("copyright");
        copy_from_tree(
            root,
            &copyright,
            &tree.join("doc").join(format!("{package}.copyright")),
        )?;
    }
    // The daemon *and* every plugin: a plugin is a program of its own, and one whose libraries the
    // host does not have fails as a check that never runs rather than as a Supervisor that does not
    // start — the harder failure to trace of the two.
    let mut programs = vec![tree.join("sbin/icinga2")];
    if let Ok(entries) = std::fs::read_dir(tree.join("plugins")) {
        programs.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file()),
        );
    }
    programs.sort();
    bundle_libraries(&programs, &tree.join("lib"), &[&libraries, &extracted])?;

    pack_tree(&tree, wrapper, &[], out)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// Materialises the alternatives a package's own installation would have created.
///
/// A `.deb` payload is not what an installed package looks like: `dh_installalternatives` puts
/// `update-alternatives --install …` into the maintainer script, and the link it creates exists
/// only after installation. For the check plugins that gap is load-bearing — Debian ships
/// `check_http.deprecated` and `check_curl` and registers **both** under the name `check_http`, so
/// a payload-only repack has no `check_http` at all, and the ITL's `http` CheckCommand calls
/// exactly that name.
///
/// So the rule the package states is applied here: among the alternatives registered for one name,
/// the highest priority wins — which is what `update-alternatives` does in automatic mode — and the
/// winner is copied to the link's path. A package with no alternatives is untouched.
fn apply_alternatives(deb: &Path, staging: &Path, extracted: &Path) -> Result<(), String> {
    let control = staging.join("control");
    let _ = std::fs::remove_dir_all(&control);
    std::fs::create_dir_all(&control)
        .map_err(|e| format!("cannot create {}: {e}", control.display()))?;
    let status = std::process::Command::new("dpkg-deb")
        .arg("-e")
        .arg(deb)
        .arg(&control)
        .status()
        .map_err(|e| format!("cannot run dpkg-deb: {e}"))?;
    if !status.success() {
        return Ok(()); // no control scripts is not a failure, it is a package without any
    }
    let script = match std::fs::read_to_string(control.join("postinst")) {
        Ok(script) => script,
        Err(_) => return Ok(()),
    };
    for (link, target) in winning_alternatives(&script) {
        // The paths are the installed system's; here they name places inside the payload, and
        // only places inside it: a script naming `..` would otherwise write beside the staging.
        let (from, to) = (
            inside_payload(extracted, &target)?,
            inside_payload(extracted, &link)?,
        );
        if from.is_file() && unoccupied(extracted, &to)? {
            copy_from_tree(extracted, &from, &to)?;
            eprintln!(
                "  provided {} as the package's own installation would",
                link
            );
        }
    }
    Ok(())
}

/// Whether `to`, a path inside the unpacked payload `extracted`, is free for an alternative:
/// nothing is there, not even a link. A directory on the way that is a link fails the repack naming
/// it, since a copy to `to` would write wherever that link leads, the host's file system included
/// (ADR-0032).
fn unoccupied(extracted: &Path, to: &Path) -> Result<bool, String> {
    let inside = to
        .strip_prefix(extracted)
        .map_err(|_| format!("{} is not inside {}", to.display(), extracted.display()))?;
    let mut at = extracted.to_path_buf();
    for component in inside.components() {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Err(_) => return Ok(true),
            Ok(_) if at == to => return Ok(false),
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "an alternative names {}, below the link {}, and a repack follows no link to \
                     write (ADR-0032)",
                    to.display(),
                    at.display()
                ));
            }
            Ok(_) => {}
        }
    }
    Ok(false)
}

/// Where an absolute path of the installed system lies inside an unpacked payload, refused when it
/// is not absolute or names anything but plain components — `..` above all (ADR-0032).
fn inside_payload(extracted: &Path, installed: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let path = Path::new(installed);
    let mut components = path.components();
    let plain = components.next() == Some(Component::RootDir)
        && components.clone().next().is_some()
        && components.all(|c| matches!(c, Component::Normal(_)));
    if !plain {
        return Err(format!(
            "a maintainer script names the alternative {installed:?}, which is not a plain path \
             of the installed system — refusing to follow it out of the package"
        ));
    }
    Ok(extracted.join(path.components().skip(1).collect::<PathBuf>()))
}

/// The `(link, target)` pairs a maintainer script's `update-alternatives --install` lines describe,
/// one per name, keeping the highest priority — automatic mode's own rule.
fn winning_alternatives(script: &str) -> Vec<(String, String)> {
    let mut best: std::collections::BTreeMap<String, (i64, String, String)> = Default::default();
    for line in script.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("update-alternatives") || words.next() != Some("--install") {
            continue;
        }
        let (Some(link), Some(name), Some(target), Some(priority)) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            continue;
        };
        let Ok(priority) = priority.parse::<i64>() else {
            continue;
        };
        // The first of equal priorities stays; only a strictly higher one replaces it.
        if best.get(name).is_none_or(|(held, ..)| priority > *held) {
            best.insert(
                name.to_string(),
                (priority, link.to_string(), target.to_string()),
            );
        }
    }
    best.into_values()
        .map(|(_, link, target)| (link, target))
        .collect()
}

/// The sonames glibc provides on the host, together with its loader: what the tree never carries
/// (ADR-draft-icinga-2 clause 18). They are the shared objects Debian's `libc6` ships for amd64,
/// and glibc has kept every one of these names since long before the oldest build this tool
/// repacks.
const GLIBC_SONAMES: [&str; 18] = [
    "ld-linux-x86-64.so.2",
    "libBrokenLocale.so.1",
    "libanl.so.1",
    "libc.so.6",
    "libc_malloc_debug.so.0",
    "libdl.so.2",
    "libm.so.6",
    "libmvec.so.1",
    "libnsl.so.1",
    "libnss_compat.so.2",
    "libnss_dns.so.2",
    "libnss_files.so.2",
    "libnss_hesiod.so.2",
    "libpthread.so.0",
    "libresolv.so.2",
    "librt.so.1",
    "libthread_db.so.1",
    "libutil.so.1",
];

/// Where a Debian package keeps the shared objects a program finds by soname, searched in this
/// order inside each unpacked package tree.
const LIBRARY_DIRS: [&str; 4] = [
    "usr/lib/x86_64-linux-gnu",
    "lib/x86_64-linux-gnu",
    "usr/lib",
    "lib",
];

/// Gathers the shared libraries the programs link against into `lib_dir` — **except glibc and its
/// loader** — out of the unpacked packages under `roots`, and nothing from anywhere else.
///
/// Those two cannot travel: a libc without its matching loader does not work, and with it the
/// program would have to *be* the loader, which a Supervisor's program path cannot express. What
/// follows is the artifact's reach — it runs where the glibc is at least the floor its suite
/// states (ADR-draft-icinga-2) — and the Supervisor points `LD_LIBRARY_PATH` at what lands here,
/// which wins over the binary's own RUNPATH.
///
/// What a program needs is read out of its `DT_NEEDED` entries ([`elf_needed`]), then out of each
/// library gathered for it, until nothing new is needed. Nothing is executed: `ldd` would run the
/// host's dynamic loader on the program and answer with the host's files. A soname the packages do
/// not hold fails the repack naming it and who needs it, since a tree missing one would ship and
/// die on its first start; one is copied as the file its link resolves to inside the directory
/// its package was unpacked into, with the packages unpacked beside it (ADR-0032).
fn bundle_libraries(programs: &[PathBuf], lib_dir: &Path, roots: &[&Path]) -> Result<(), String> {
    std::fs::create_dir_all(lib_dir)
        .map_err(|e| format!("cannot create {}: {e}", lib_dir.display()))?;
    let mut pending: Vec<PathBuf> = programs.iter().rev().cloned().collect();
    let mut missing: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
    let mut bundled = 0usize;
    while let Some(file) = pending.pop() {
        let bytes =
            std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        // A shell or Perl script among the plugins is not an ELF file: a plugin that needs no
        // libraries of its own.
        let Some(needed) = elf_needed(&bytes).map_err(|e| format!("{}: {e}", file.display()))?
        else {
            continue;
        };
        let needer = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for soname in needed {
            if GLIBC_SONAMES.contains(&soname.as_str()) {
                continue;
            }
            if soname.is_empty() || soname.contains('/') || soname.starts_with('.') {
                return Err(format!(
                    "{needer} needs {soname:?}, which is not a library name a repack can carry"
                ));
            }
            let target = lib_dir.join(&soname);
            if target.exists() {
                continue;
            }
            let found = roots.iter().find_map(|root| {
                LIBRARY_DIRS
                    .iter()
                    .map(|dir| root.join(dir).join(&soname))
                    .find(|candidate| std::fs::symlink_metadata(candidate).is_ok())
                    .map(|candidate| (root, candidate))
            });
            if let Some((root, candidate)) = found {
                copy_from_tree(root, &candidate, &target)?;
            }
            if target.is_file() {
                bundled += 1;
                pending.push(target);
            } else {
                missing.entry(soname).or_default().insert(needer.clone());
            }
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "the packages the tree is made of hold no {} the programs link against: {}\n  the \
             tree would fail on its first start, so nothing is packed; the library packages are \
             what the vendor packages declare in Debian's index (ADR-draft-icinga-2)",
            if missing.len() == 1 {
                "library"
            } else {
                "libraries"
            },
            missing
                .iter()
                .map(|(soname, needers)| format!(
                    "{soname} (needed by {})",
                    needers.iter().cloned().collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    eprintln!("  bundled {bundled} shared libraries");
    Ok(())
}

/// The `DT_NEEDED` names of an ELF file — the sonames its dynamic loader would look for — read
/// from its bytes alone. `None` for a file that is not ELF at all, such as a script. Only the
/// 64-bit little-endian form is read, the one of the amd64 build this tool makes; another, or a
/// file whose headers point outside it, is an error rather than a guess.
fn elf_needed(bytes: &[u8]) -> Result<Option<Vec<String>>, String> {
    const PT_LOAD: u32 = 1;
    const PT_DYNAMIC: u32 = 2;
    const DT_NEEDED: u64 = 1;
    const DT_STRTAB: u64 = 5;
    const DT_STRSZ: u64 = 10;
    if bytes.get(..4) != Some(b"\x7fELF".as_slice()) {
        return Ok(None);
    }
    if bytes.get(4..6) != Some([2u8, 1].as_slice()) {
        return Err("is not a 64-bit little-endian ELF file".to_string());
    }
    let malformed = || "is a malformed ELF file".to_string();
    let u16_at = |at: usize| {
        bytes
            .get(at..at.checked_add(2)?)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at.checked_add(4)?)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let u64_at = |at: usize| {
        bytes
            .get(at..at.checked_add(8)?)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    };
    let offset = |value: u64| usize::try_from(value).ok();

    let phoff = u64_at(0x20).and_then(offset).ok_or_else(malformed)?;
    let phentsize = usize::from(u16_at(0x36).ok_or_else(malformed)?);
    let phnum = usize::from(u16_at(0x38).ok_or_else(malformed)?);
    // (virtual address, file offset, size in the file) of each loaded segment, and the dynamic one.
    let mut loads: Vec<(u64, u64, u64)> = Vec::new();
    let mut dynamic = None;
    for n in 0..phnum {
        let at = n
            .checked_mul(phentsize)
            .and_then(|o| o.checked_add(phoff))
            .ok_or_else(malformed)?;
        let kind = u32_at(at).ok_or_else(malformed)?;
        let file_offset = u64_at(at + 8).ok_or_else(malformed)?;
        let vaddr = u64_at(at + 16).ok_or_else(malformed)?;
        let filesz = u64_at(at + 32).ok_or_else(malformed)?;
        match kind {
            PT_LOAD => loads.push((vaddr, file_offset, filesz)),
            PT_DYNAMIC => dynamic = Some((file_offset, filesz)),
            _ => {}
        }
    }
    // Statically linked: nothing to load.
    let Some((dyn_offset, dyn_size)) = dynamic else {
        return Ok(Some(Vec::new()));
    };
    let mut needed = Vec::new();
    let (mut strtab, mut strsz) = (None, None);
    let entries = dyn_size / 16;
    for n in 0..entries {
        let at = n
            .checked_mul(16)
            .and_then(|o| o.checked_add(dyn_offset))
            .and_then(offset)
            .ok_or_else(malformed)?;
        let tag = u64_at(at).ok_or_else(malformed)?;
        let value = u64_at(at + 8).ok_or_else(malformed)?;
        match tag {
            0 => break,
            DT_NEEDED => needed.push(value),
            DT_STRTAB => strtab = Some(value),
            DT_STRSZ => strsz = Some(value),
            _ => {}
        }
    }
    if needed.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let (strtab, strsz) = (strtab.ok_or_else(malformed)?, strsz.ok_or_else(malformed)?);
    // The string table is named by its address once loaded; where that lies in the file is the
    // loaded segment's to say.
    let table = loads
        .iter()
        .find(|(vaddr, _, filesz)| strtab >= *vaddr && strtab - *vaddr < *filesz)
        .and_then(|(vaddr, file_offset, _)| file_offset.checked_add(strtab - vaddr))
        .and_then(offset)
        .ok_or_else(malformed)?;
    let table = bytes
        .get(
            table
                ..table
                    .checked_add(offset(strsz).ok_or_else(malformed)?)
                    .ok_or_else(malformed)?,
        )
        .ok_or_else(malformed)?;
    needed
        .into_iter()
        .map(|at| {
            let rest = offset(at)
                .and_then(|at| table.get(at..))
                .ok_or_else(malformed)?;
            let end = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
            String::from_utf8(rest[..end].to_vec()).map_err(|_| malformed())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// The one file of that name below `root`, wherever the distribution decided to put it.
/// The one regular file of that name below `root`: none, or more than one, fails the repack
/// naming what it found, since which of two the tree carried would depend on the order a
/// directory is read in.
fn find_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    let mut found = files_below(root, name);
    match found.len() {
        0 => Err(format!(
            "the vendor packages hold no {name} below {}",
            root.display()
        )),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "the vendor packages hold more than one {name} below {}: {}",
            root.display(),
            found
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// Copies `from`, a file or directory of the tree `root` was unpacked into, to `to`, resolving
/// every link inside that tree alone (ADR-0032): a link that leads out of it fails the repack
/// naming it, one that names nothing inside it is left out, and the copy carries no link
/// (ADR-0027).
fn copy_from_tree(root: &Path, from: &Path, to: &Path) -> Result<(), String> {
    let inside = from
        .strip_prefix(root)
        .map_err(|_| format!("{} is not inside {}", from.display(), root.display()))?;
    match resolve_inside(root, Path::new(""), inside)? {
        Resolution::Inside {
            real,
            is_dir: false,
        } => copy_file(&root.join(real), to),
        Resolution::Inside { real, is_dir: true } => {
            let dir = root.join(real);
            std::fs::create_dir_all(to)
                .map_err(|e| format!("cannot create {}: {e}", to.display()))?;
            let mut names: Vec<_> = std::fs::read_dir(&dir)
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .flatten()
                .map(|entry| entry.file_name())
                .collect();
            names.sort();
            for name in names {
                copy_from_tree(root, &dir.join(&name), &to.join(&name))?;
            }
            Ok(())
        }
        Resolution::Nothing => Ok(()),
        Resolution::LeadsOut => Err(format!(
            "{} leads out of the tree it was unpacked into, and a repack follows no link out of \
             its tree (ADR-0032)",
            from.display()
        )),
        Resolution::Special => Err(format!(
            "{} is neither a file nor a directory",
            from.display()
        )),
    }
}

/// Packs a directory as a `.tar.gz` a Client can install as a tree — deterministically, with every
/// link resolved, and with nothing from outside the tree (ADR-0032).
///
/// Determinism is not tidiness: the fleet decides whether to distribute by content hash, so an
/// artifact that differed by when it was packed would be a rollout nobody asked for. Times, owners
/// and order are therefore fixed, exactly as `package pack` fixes them for one file.
///
/// Links are resolved rather than carried because a tree package refuses them (ADR-0027): what a
/// link names is not where it sits, which is the one thing a path check cannot judge. A link is
/// resolved inside the tree alone, and one that leads out of it fails the pack before anything is
/// written: following it would put the operator's own files into an artifact the operator then
/// signs, and make its bytes depend on the host that packed it. So does anything that is neither a
/// file nor a directory, which a pack could only hang on or read from outside the tree. A link
/// pointing at nothing inside the tree is dropped.
///
/// The one exception is `kept`, the absolute links the kind names by their exact path in the tree
/// and their exact target (ADR-draft-the-operator-program-opamp-fleetctl clause 27): each is packed
/// as that link, read from the tree and never followed, and the Client creates exactly those. A
/// link at a named path with another target leads out like any other.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
fn pack_tree(root: &Path, wrapper: &str, kept: &[KeptLink], out: &Path) -> Result<(), String> {
    let mut members = Vec::new();
    let mut refused = std::collections::BTreeMap::new();
    // The root is the first ancestor: a link back up to it is a cycle like any other.
    collect_members(
        root,
        Path::new(""),
        Path::new(""),
        kept,
        &mut members,
        &mut vec![PathBuf::new()],
        &mut refused,
    )?;
    if !refused.is_empty() {
        // Quoted and escaped: the names come from upstream and reach the operator's terminal.
        let entries: Vec<&str> = refused.values().map(String::as_str).collect();
        return Err(format!(
            "{root:?} cannot be repacked, since a repack carries only the files and directories \
             of its own tree: {}",
            entries.join(", ")
        ));
    }
    members.sort();

    let file =
        std::fs::File::create(out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let written = write_members(root, wrapper, &members, file, out);
    if written.is_err() {
        // Half an artifact is one somebody might sign.
        let _ = std::fs::remove_file(out);
    }
    written
}

/// Writes the members, in the order given, into `file` as a gzipped tar.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
fn write_members(
    root: &Path,
    wrapper: &str,
    members: &[(PathBuf, Member)],
    file: std::fs::File,
    out: &Path,
) -> Result<(), String> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    for (name, member) in members {
        let inside = Path::new(wrapper).join(name);
        let mut header = tar::Header::new_gnu();
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let real = match member {
            Member::Dir(_) => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, &inside, std::io::empty())
                    .map_err(|e| format!("cannot pack {}: {e}", inside.display()))?;
                continue;
            }
            // A kept link is stored as the link it is, with the mode every link has.
            Member::Link(target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                builder
                    .append_link(&mut header, &inside, target)
                    .map_err(|e| format!("cannot pack {}: {e}", inside.display()))?;
                continue;
            }
            Member::File(real) => real,
        };
        // `real` holds no link, so what is opened is the member the walk judged.
        let full = root.join(real);
        let source = std::fs::File::open(&full)
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let meta = source
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(meta.len());
        header.set_mode(mode_of(&meta));
        header.set_cksum();
        builder
            .append_data(&mut header, &inside, source)
            .map_err(|e| format!("cannot pack {}: {e}", inside.display()))?;
    }
    builder
        .into_inner()
        .map_err(|e| format!("cannot finish {}: {e}", out.display()))?
        .finish()
        .map_err(|e| format!("cannot finish {}: {e}", out.display()))?;
    Ok(())
}

/// What one member of a repack is: a directory or a file, by the path inside the tree it is read
/// from, which holds no link, or a link the kind keeps, by its target.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Member {
    Dir(PathBuf),
    File(PathBuf),
    Link(PathBuf),
}

/// Every member under the directory packed as `name` and read from `real` — directories included,
/// because a tree keeps its shape. A member is its name in the artifact and what it is; the name
/// and the path it is read from differ for everything reached through a linked directory. A link
/// `kept` names is a member at its own path only, never reached through a linked directory. A link
/// that leads out of the tree, and anything that is neither a file nor a directory, is no member
/// but goes into `refused`, keyed by its own path in the tree and saying what it is. The member cap is checked as each member is found, so a
/// tree whose links multiply its paths stops at the cap rather than after the whole walk.
///
/// Links are followed, and that is the whole difficulty. A link to a *directory* has to be
/// descended into and packed **at the name the link carries**, because that name is one the agent
/// uses: a tree may reach its Perl library through `usr/share/perl/5.26`, a link to
/// `5.26.1`, and a tree where that path is an empty directory is one whose agent cannot find a
/// single module. So the contents are packed under both names, exactly as `tar -h` writes them.
///
/// Following links makes a cycle possible, and the guard has to be the *ancestors* of the walk
/// rather than everywhere it has already been: a link pointing back up is a cycle and is not
/// followed — it is packed as an empty directory under its name — while a second path to the same
/// directory is legitimate duplication and must be packed again. Deduplicating by real path
/// instead would leave one of the two names empty — and which one, on directory-read order, which
/// is nobody's guarantee.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
fn collect_members(
    root: &Path,
    name: &Path,
    real: &Path,
    kept: &[KeptLink],
    into: &mut Vec<(PathBuf, Member)>,
    ancestors: &mut Vec<PathBuf>,
    refused: &mut std::collections::BTreeMap<PathBuf, String>,
) -> Result<(), String> {
    let dir = root.join(real);
    for entry in
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let member = name.join(entry.file_name());
        let own = real.join(entry.file_name());
        if member == own && entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            let target = std::fs::read_link(entry.path())
                .map_err(|e| format!("cannot read {}: {e}", own.display()))?;
            if fleet_core::tree::is_kept(kept, &own, &target) {
                into.push((member, Member::Link(target)));
                if into.len() > MAX_TREE_MEMBERS {
                    return Err(format!(
                        "the tree holds more than the {MAX_TREE_MEMBERS} members a package may \
                         carry"
                    ));
                }
                continue;
            }
        }
        // Resolving rather than reading the entry itself: what matters is what the member *is*
        // once the link is followed, because that is what gets packed.
        let (resolved, is_dir) = match resolve_inside(root, real, Path::new(&entry.file_name()))? {
            Resolution::Inside { real, is_dir } => (real, is_dir),
            // A dangling link, which is where the AppImage's Debian leftovers go.
            Resolution::Nothing => continue,
            Resolution::LeadsOut => {
                let target = std::fs::read_link(entry.path())
                    .map_err(|e| format!("cannot read {}: {e}", own.display()))?;
                let says = format!("{own:?} -> {target:?} leads out of the tree");
                refused.insert(own, says);
                continue;
            }
            // A pipe would hang the pack and a device would be read from outside the tree.
            Resolution::Special => {
                let says = format!("{own:?} is neither a file nor a directory");
                refused.insert(own, says);
                continue;
            }
        };
        let what = if is_dir {
            Member::Dir(resolved.clone())
        } else {
            Member::File(resolved.clone())
        };
        into.push((member.clone(), what));
        if into.len() > MAX_TREE_MEMBERS {
            return Err(format!(
                "the tree holds more than the {MAX_TREE_MEMBERS} members a package may carry"
            ));
        }
        if !is_dir || ancestors.contains(&resolved) {
            continue;
        }
        ancestors.push(resolved.clone());
        collect_members(root, &member, &resolved, kept, into, ancestors, refused)?;
        ancestors.pop();
    }
    Ok(())
}

/// What a path of the tree is once every link on it is followed inside the tree.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
enum Resolution {
    /// A file or directory inside the tree, by its path relative to the root, which holds no link.
    Inside { real: PathBuf, is_dir: bool },
    /// Nothing inside the tree: a dangling link, a path through a file, or a loop of links.
    Nothing,
    /// The path climbs above the root, or meets a link with an absolute target.
    LeadsOut,
    /// Something inside the tree that is neither a file nor a directory: a pipe, a socket, a
    /// device.
    Special,
}

/// How many links one resolution follows before it counts as a loop — the bound Linux puts on one
/// path lookup.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
const MAX_LINK_HOPS: usize = 40;

/// Resolves `path`, taken relative to `base` — a directory of the tree, by a path that holds no
/// link — by following every link on it inside the tree alone. The file system is only asked what
/// each component is, never to follow a link itself, so nothing outside the tree is ever read.
///
/// A `..` above the root or an absolute target leads out, and so does a path that climbs above the
/// root past a component that does not exist: whether a link leads out is the tree's to say, not
/// the host's.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
fn resolve_inside(root: &Path, base: &Path, path: &Path) -> Result<Resolution, String> {
    use std::path::Component;
    // Components still to resolve, the next one last; `None` if the path is absolute.
    fn components(path: &Path) -> Option<Vec<std::ffi::OsString>> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_os_string()),
                Component::ParentDir => parts.push("..".into()),
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        parts.reverse();
        Some(parts)
    }

    let mut here: Vec<std::ffi::OsString> = base
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    let Some(mut pending) = components(path) else {
        return Ok(Resolution::LeadsOut);
    };
    let mut hops = 0;
    // Whether `here` is a directory; once a component names nothing, the rest of the path is read
    // as text, only to learn whether it climbs out.
    let mut is_dir = true;
    let mut special = false;
    let mut nothing = false;
    while let Some(part) = pending.pop() {
        if part == ".." {
            nothing |= !is_dir;
            if here.pop().is_none() {
                return Ok(Resolution::LeadsOut);
            }
            is_dir = true;
            special = false;
            continue;
        }
        nothing |= !is_dir;
        here.push(part);
        if nothing {
            continue;
        }
        let full = root.join(here.iter().collect::<PathBuf>());
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                nothing = true;
                continue;
            }
            Err(e) => return Err(format!("cannot read {}: {e}", full.display())),
        };
        if !meta.file_type().is_symlink() {
            is_dir = meta.is_dir();
            special = !is_dir && !meta.is_file();
            continue;
        }
        hops += 1;
        if hops > MAX_LINK_HOPS {
            return Ok(Resolution::Nothing);
        }
        let target = std::fs::read_link(&full)
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        // The target is taken relative to the directory holding the link.
        here.pop();
        let Some(mut further) = components(&target) else {
            return Ok(Resolution::LeadsOut);
        };
        pending.append(&mut further);
    }
    if nothing {
        return Ok(Resolution::Nothing);
    }
    if special {
        return Ok(Resolution::Special);
    }
    Ok(Resolution::Inside {
        real: here.iter().collect(),
        is_dir,
    })
}

/// The mode to store, normalized (ADR-draft-the-operator-program-opamp-fleetctl clause 27):
/// `0o755` for a file any execute bit is set on, `0o644` for every other, so no write bit beyond
/// the owner's and no setuid, setgid or sticky bit upstream set reaches the signed artifact. Off
/// Unix it is a decision rather than a reading: a tar written on Windows has no mode to carry, and
/// a tree whose programs arrive unexecutable is a tree that will not run.
#[allow(
    dead_code,
    reason = "a kind whose upstream publishes no installable archive repacks with it, and the core carries none"
)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            0o644
        } else {
            0o755
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o755
    }
}

// ── The Server ──────────────────────────────────────────────────────────────

/// Creates the Set if it is not there and stores this platform's artifact as its entry
/// (ADR-0027). Nothing is distributed by either call: a Set reaches an Agent only through a
/// rollout act (ADR-0026), which stays the operator's.
async fn upload(
    client: &reqwest::Client,
    server: &str,
    service_name: &str,
    version: &str,
    plan: &Plan,
    artifact: &Path,
) -> Result<(), String> {
    let set = format!("{server}/api/v1/packages/{service_name}/{version}");
    eprintln!("  creating the Set at {set} …");
    put_json(client, &set, "{}".to_string()).await?;

    let entry = format!("{set}/entries/{}/{}", plan.os, plan.arch);
    eprintln!("  uploading {} …", artifact.display());
    let bytes =
        std::fs::read(artifact).map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
    let response = match client.put(&entry).body(bytes).send().await {
        Ok(response) => response,
        Err(e) => return Err(refusal_behind(client, &entry, &e).await),
    };
    expect_ok(response, &entry).await?;
    eprintln!(
        "  stored as the {}/{} entry — and it reaches nobody yet: it is unsigned, and a package \
         aims at nothing by itself. Sign the entry on the Packages tab (or PUT its …/signature), \
         then put it in a deployment when it should reach hosts:\n    \
         curl -X PUT <server>/api/v1/deployments/<ring>/packages/{}/{}",
        plan.os, plan.arch, service_name, version
    );
    Ok(())
}

/// Puts this agent's default Configurations beside the package — **only the ones the Server does
/// not already have**.
///
/// A package alone leaves an Agent with nothing to run: the Supervisor holds at "awaiting
/// configuration" until a Configuration of the name its block reads arrives. Uploading the default
/// with the package closes that gap in the same act.
///
/// Two rules keep it from being a surprise. **An existing Configuration is never touched** — the
/// name is asked for first, and one that answers is left exactly as the operator left it, edits and
/// all. And **nothing is distributed**: saving only saves (ADR-0026), and a Configuration reaches an
/// Agent only through a Deployment that holds it (ADR-0026), so the default reaches no Agent until
/// an operator reads it, puts it into a Deployment and presses that Deployment's rollout. That
/// matters here, because these bodies are starting points with example values in them — a parent
/// host to correct, a plugin path to confirm.
async fn upload_default_configurations(
    client: &reqwest::Client,
    server: &str,
    agent: AgentKind,
) -> Result<(), String> {
    for config in agent.default_configurations() {
        let url = format!("{server}/api/v1/configurations/{}", config.name);
        if configuration_exists(client, &url).await? {
            eprintln!("  {} is already on the Server — left as it is", config.name);
            continue;
        }
        let spec = serde_json::json!({
            "body": config.body,
            "service_name": config.service_name,
        });
        eprintln!("  storing the default configuration {} …", config.name);
        put_json(client, &url, spec.to_string()).await?;
        eprintln!(
            "  saved — read it over, put it into the deployment that should carry it and press \
             that deployment's rollout when it should reach hosts, since it carries example values"
        );
    }
    Ok(())
}

/// Whether the Server already holds a Configuration of this name. `404` is the answer that means
/// "no"; anything else that is not a success is an answer this tool must not read past — a Server
/// that refuses the question would otherwise have a default written over the top of what it holds.
async fn configuration_exists(client: &reqwest::Client, url: &str) -> Result<bool, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    expect_ok(response, url).await.map(|()| true)
}

async fn put_json(client: &reqwest::Client, url: &str, body: String) -> Result<(), String> {
    let response = client
        .put(url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
    expect_ok(response, url).await
}

/// Why an upload never came back with a status.
///
/// The Server refuses some uploads *before* it reads a byte of the body — an identity nobody
/// created, a Set already assigned and therefore immutable (ADR-0026), a store at its ceiling
/// (ADR-0027). With hundreds of megabytes already in flight and no `Expect: 100-continue` to hold
/// them back — HTTP's own remedy for exactly this, which this client does not speak — that
/// response races the upload, the connection resets, and what arrives here is a transport error
/// carrying no status at all. Reporting it as "cannot reach" would name the one thing that is not
/// the problem: the Server answered, and said why.
///
/// So ask a second time with an empty artifact. The pre-body checks are the same ones and they run
/// again, but the request is small enough to survive the trip. An empty artifact is itself refused
/// (`400`), so the probe can never store anything — and a `400` is the answer that carries no news:
/// it means the checks passed and the Server got as far as the body, leaving the transport error as
/// the real story.
async fn refusal_behind(client: &reqwest::Client, entry: &str, error: &reqwest::Error) -> String {
    if let Ok(response) = client.put(entry).body(Vec::new()).send().await {
        let status = response.status();
        if status != reqwest::StatusCode::BAD_REQUEST {
            let body = response.text().await.unwrap_or_default();
            return format!("{entry} answered {status}: {}", body.trim());
        }
    }
    format!("cannot reach {entry}: {}", because(error))
}

/// A transport error with the cause that explains it. `reqwest::Error` displays only its own layer
/// — "error sending request for url (…)" — while the sentence that says *what went wrong* sits one
/// or two sources below it.
fn because(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
}

async fn expect_ok(response: reqwest::Response, url: &str) -> Result<(), String> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(format!("{url} answered {status}: {}", body.trim()))
}

// ── HTTP ────────────────────────────────────────────────────────────────────

fn http() -> Result<reqwest::Client, String> {
    client_builder()
        .build()
        .map_err(|e| format!("cannot build an HTTP client: {e}"))
}

/// The client for the Server's Operator plane: the one every download uses, trusting `--cacert`
/// in place of the system's roots when it is given — the Client's own trust, so the two read a CA
/// file alike.
fn server_http(cacert: Option<&Path>) -> Result<reqwest::Client, String> {
    let ca_pem = cacert
        .map(|path| std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display())))
        .transpose()?;
    opamp::client::connection::ClientTls {
        ca_pem,
        identity: None,
    }
    .trust(client_builder())
    .map_err(|e| format!("--cacert: {e}"))?
    .build()
    .map_err(|e| format!("cannot build an HTTP client: {e}"))
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        // TLS 1.3 alone, as every connection of this project (ADR-0012).
        .tls_version_min(reqwest::tls::Version::TLS_1_3)
        // Release hosts redirect to their CDN, so redirects are followed — but never to http.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("a redirect to anything but https is refused")
            } else if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        // An artifact is hundreds of megabytes over someone else's CDN; a whole-request timeout
        // would kill a working download on principle, so only the connect phase is bounded.
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent("opamp-fleetctl")
}

async fn get(url: &str) -> Result<reqwest::Response, String> {
    get_moved(url, DEBIAN_MOVES).await
}

/// Where Debian keeps what `deb.debian.org` no longer serves: an ended release's suites and pool
/// files move to `archive.debian.org`, under the same paths (ADR-draft-icinga-2 clause 18).
const DEBIAN_MOVES: &[(&str, &str)] = &[
    (DEBIAN_REPO, "https://archive.debian.org/debian"),
    (
        DEBIAN_SECURITY_REPO,
        "https://archive.debian.org/debian-security",
    ),
];

/// `url` answered, or — where it answers `404` and lies below the first of a pair of `moves` —
/// the same path below the second. Only a `404` moves: any other failure is the answer, so a
/// network fault never sends a fetch elsewhere. What the archive answers is verified exactly as
/// the original would have been, since the digests that decide come from the signed index.
async fn get_moved(url: &str, moves: &[(&str, &str)]) -> Result<reqwest::Response, String> {
    let response = send(url).await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        if let Some(moved) = moved_url(url, moves) {
            eprintln!("  {url} answered 404 — reading it from Debian's archive instead");
            eprintln!("  reading {moved} …");
            let response = send(&moved).await?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!(
                    "{url} answered 404 Not Found, and {moved} answered {status}"
                ));
            }
            return Ok(response);
        }
    }
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    Ok(response)
}

/// `url` below the second of the first pair of `moves` whose first it lies below, by whole path
/// segments.
fn moved_url(url: &str, moves: &[(&str, &str)]) -> Option<String> {
    moves.iter().find_map(|(from, to)| {
        url.strip_prefix(from)
            .filter(|rest| rest.starts_with('/'))
            .map(|rest| format!("{to}{rest}"))
    })
}

async fn send(url: &str) -> Result<reqwest::Response, String> {
    http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))
}

/// Streams an artifact to disk. An agent release is hundreds of megabytes, and a tool that read
/// one into memory to write it straight back out would be choosing the one shape that cannot work
/// on a small host.
async fn download(url: &str, dest: &Path) -> Result<(), String> {
    use std::io::Write;

    let mut response = get(url).await?;
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("cannot download {url}: {}", because(&e)))?
    {
        file.write_all(&chunk)
            .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    }
    file.flush()
        .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(())
}

/// For the small answers — a checksum file, a JSON listing — where holding the whole body is the
/// simplest thing that works.
async fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    Ok(get(url)
        .await?
        .bytes()
        .await
        .map_err(|e| format!("cannot read {url}: {e}"))?
        .to_vec())
}

async fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let bytes = get_bytes(url).await.map_err(|e| {
        // The one failure an operator hits without doing anything wrong, and it says nothing about
        // itself: GitHub answers 403 for sixty-one unauthenticated requests in an hour.
        if e.contains("403") {
            format!("{e} — GitHub rate-limits unauthenticated requests to 60 per hour")
        } else {
            e
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{url} did not answer JSON: {e}"))
}

#[cfg(test)]
mod tests {
    /// The agent menu names the systems each agent can be fetched for, and the one that needs it
    /// most is Icinga 2: its Linux artifact reaches what the suite it is repacked from reaches,
    /// which is not something to discover three questions later. Every variant answers —
    /// adding one without a line here is the omission this catches.
    #[test]
    fn the_agent_menu_states_the_systems_each_agent_is_published_for() {
        for agent in super::AgentKind::value_variants() {
            let platforms = agent.platforms(Some("bookworm"));
            assert!(
                platforms.contains('/') || platforms.contains("linux"),
                "{agent:?} names no system: {platforms:?}"
            );
        }
    }

    /// Icinga 2's line is the reach of the artifact this host builds by default, its own suite's,
    /// so it is asserted per host rather than once. Three properties hold for every row: the
    /// systems are named with versions — "Debian" alone still leaves a RHEL 8 or Ubuntu 18.04
    /// operator guessing — Linux is the one platform offered (ADR-draft-icinga-2 clause 23), and no
    /// build-host codename appears. That last one is the defect this test exists for: a line that
    /// named the container was read as the reach, and left every Red Hat host looking unserved
    /// (ADR-draft-icinga-2).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn icinga_2s_line_is_the_reach_of_the_host_it_is_read_on() {
        for (codename, _) in super::ICINGA2_REACH {
            let line = super::icinga2_reach(Some(codename));
            assert!(
                line.contains("linux/amd64") && !line.contains("windows"),
                "{codename} offers another platform than Linux: {line:?}"
            );
            for family in ["Debian", "Ubuntu", "RHEL"] {
                assert!(
                    line.contains(family),
                    "{codename} names no {family} version: {line:?}"
                );
            }
            for other in super::ICINGA2_REACH {
                assert!(
                    !line.contains(other.0),
                    "{:?} is a build host, not a reach: {line:?}",
                    other.0
                );
            }
        }

        // A host Icinga publishes no packages for builds nothing unless `--distro` names a suite,
        // and the line says which — the one place a codename is the honest answer, because there
        // it *is* the flag the operator has to give.
        for stranger in [None, Some("noble"), Some("sid")] {
            let line = super::icinga2_reach(stranger);
            assert!(
                line.contains("--distro bullseye/bookworm/trixie") && !line.contains("Debian"),
                "{stranger:?} names a reach it does not build by default: {line:?}"
            );
            assert!(!line.contains("windows"), "{stranger:?}: {line:?}");
        }
    }

    /// The suite repacked is the one `--distro` names, any of the three on any host; omitted, the
    /// host's own when it is one of them. A host that is none of them, and a codename Icinga
    /// publishes no build for, are refused naming the three (ADR-draft-icinga-2 clauses 19 and 23):
    /// with no Windows artifact, there is nothing else to build.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn any_host_repacks_any_suite_and_defaults_to_its_own() {
        let host = |name: &str| Ok::<_, String>(name.to_string());
        assert_eq!(
            super::chosen_distro(Some("bullseye"), host("trixie")),
            Ok("bullseye".to_string())
        );
        assert_eq!(
            super::chosen_distro(Some("trixie"), Err("no os-release".to_string())),
            Ok("trixie".to_string())
        );
        assert_eq!(
            super::chosen_distro(None, host("bookworm")),
            Ok("bookworm".to_string())
        );
        for refused in [
            super::chosen_distro(Some("noble"), host("bookworm")),
            super::chosen_distro(None, host("noble")),
            super::chosen_distro(None, Err("no os-release".to_string())),
        ] {
            let refused = refused.expect_err("nothing to build");
            assert!(refused.contains("bullseye, bookworm, trixie"), "{refused}");
        }
    }

    /// The reach table and the `--distro` flag describe the same three builds, so a codename added
    /// to one and not the other is a menu that promises what the build refuses, or a build nobody
    /// is told about. `docs/manual/tools.md` and `docs/manual/icinga2.md` name them too.
    #[test]
    fn every_distro_the_tool_builds_for_has_a_stated_reach() {
        let documented: Vec<&str> = super::ICINGA2_REACH.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            documented,
            vec!["bullseye", "bookworm", "trixie"],
            "the reach table and the distributions Icinga publishes for have drifted apart"
        );
    }

    /// `group_platforms` states that a menu line wrapping at eighty columns is one `dialoguer`
    /// redraws wrongly as the selection moves — but nothing held anyone to it, and a line written
    /// by hand rather than by that function promptly ran to ninety-five. The rule is measured here
    /// on the lines the menu really shows, with the two columns `dialoguer` prefixes them with.
    ///
    /// Columns, not bytes: the Collectors' line carries an em dash.
    #[test]
    fn no_agent_menu_line_wraps() {
        const SELECTION_PREFIX: usize = 2;
        let hosts = super::ICINGA2_REACH
            .iter()
            .map(|(codename, _)| Some(*codename))
            .chain([None]);
        for label in hosts.flat_map(super::agent_menu_labels) {
            let columns = SELECTION_PREFIX + label.chars().count();
            assert!(
                columns <= 80,
                "this line renders {columns} columns wide and will wrap: {label:?}"
            );
        }
    }

    /// Every agent the `--agent` flag accepts is also offered when the flag is omitted.
    ///
    /// The two come apart as soon as the prompt keeps its own list — an agent reachable by flag
    /// and absent from the prompt — so what is asserted here is that the menu is built from the
    /// same variants clap validates against, and that the newest one is among them.
    #[test]
    fn the_prompt_offers_every_agent_the_flag_accepts() {
        use clap::ValueEnum as _;
        let offered: Vec<&str> = super::AgentKind::value_variants()
            .iter()
            .map(|agent| agent.source().service_name)
            .collect();
        for expected in ["otelcol", "otelcol-contrib", "icinga2"] {
            assert!(offered.contains(&expected), "{expected} is not offered");
        }
    }

    /// The rule `update-alternatives` follows in automatic mode, applied to a payload that has not
    /// been installed: highest priority wins. Taken verbatim from Debian's own
    /// `monitoring-plugins-basic`, which registers two implementations under one name — and whose
    /// lower-priority one is the modern `check_curl`, so guessing "the newest" would pick the
    /// opposite of what installing the package produces.
    #[test]
    fn the_highest_priority_alternative_is_the_one_a_package_would_install() {
        let postinst = "\
set -e
if [ \"$1\" = \"configure\" ]; then
        update-alternatives --install /usr/lib/nagios/plugins/check_http check_http /usr/lib/nagios/plugins/check_http.deprecated 50
fi
if [ \"$1\" = \"configure\" ]; then
        update-alternatives --install /usr/lib/nagios/plugins/check_http check_http /usr/lib/nagios/plugins/check_curl -100
fi
        update-alternatives --install /usr/lib/nagios/plugins/check_ping check_ping /usr/lib/nagios/plugins/check_icmp 10
echo done
";
        let mut won = super::winning_alternatives(postinst);
        won.sort();
        assert_eq!(
            won,
            vec![
                (
                    "/usr/lib/nagios/plugins/check_http".to_string(),
                    "/usr/lib/nagios/plugins/check_http.deprecated".to_string()
                ),
                (
                    "/usr/lib/nagios/plugins/check_ping".to_string(),
                    "/usr/lib/nagios/plugins/check_icmp".to_string()
                ),
            ]
        );
        assert!(super::winning_alternatives("echo nothing to do").is_empty());
    }

    /// What a maintainer script names is a path of the installed system, and inside the payload it
    /// stays: a `..` or a relative path is refused rather than joined (ADR-0032).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn an_alternative_names_only_a_place_inside_the_payload() {
        let root = Path::new("/staging/extracted");
        assert_eq!(
            super::inside_payload(root, "/usr/lib/nagios/plugins/check_http"),
            Ok(root.join("usr/lib/nagios/plugins/check_http"))
        );
        assert_eq!(
            super::inside_payload(root, "/usr/./lib/x"),
            Ok(root.join("usr/lib/x")),
            "a `.` names no other place, and is dropped"
        );
        for escaping in [
            "/usr/lib/nagios/plugins/../../../../etc/cron.d/x",
            "usr/lib/x",
            "/",
            "",
        ] {
            let refused = super::inside_payload(root, escaping).expect_err(escaping);
            assert!(refused.contains("not a plain path"), "{refused}");
        }
    }

    /// An alternative is written only where nothing is, and never through a link: a name the
    /// payload already holds — as a file or as a link, dangling or not — is left as it is, and a
    /// directory link on the way fails the repack naming it rather than being written through
    /// (ADR-0032).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(unix)]
    #[test]
    fn an_alternative_is_written_only_where_nothing_is_and_through_no_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = dir.path().join("extracted");
        let outside = dir.path().join("outside");
        let plugins = payload.join("usr/lib/nagios/plugins");
        std::fs::create_dir_all(&plugins).expect("plugins");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(plugins.join("check_curl"), b"curl").expect("write");
        symlink(outside.join("check_x"), plugins.join("check_dangling")).expect("link");
        symlink(&outside, payload.join("usr/lib/escape")).expect("link");

        let free = |path: &str| super::unoccupied(&payload, &payload.join(path));
        assert_eq!(free("usr/lib/nagios/plugins/check_http"), Ok(true));
        assert_eq!(free("usr/lib/new/dir/check_http"), Ok(true));
        assert_eq!(free("usr/lib/nagios/plugins/check_curl"), Ok(false));
        assert_eq!(
            free("usr/lib/nagios/plugins/check_dangling"),
            Ok(false),
            "a dangling link is not written through"
        );
        let refused = free("usr/lib/escape/check_http").expect_err("through a link");
        assert!(refused.contains("usr/lib/escape"), "{refused}");
        assert!(
            std::fs::read_dir(&outside)
                .expect("outside")
                .next()
                .is_none(),
            "nothing was written outside the payload"
        );
    }

    /// ADR-draft-icinga-2: the digests come out of the repository's own index, because Icinga signs
    /// repositories with GPG instead of publishing per-file checksums. The parse has to find both
    /// packages of one version among the many versions a pool index carries.
    #[test]
    fn the_repository_index_yields_a_packages_filename_digest_and_libc_floor() {
        let index = "\
Package: icinga2-bin
Architecture: amd64
Version: 2.16.4-1+debian13
Depends: libc6 (>= 2.38), libssl3t64 (>= 3.0.0)
Filename: pool/main/i/icinga2/icinga2-bin_2.16.4-1+debian13_amd64.deb
SHA256: aaaa

Package: icinga2-bin
Architecture: amd64
Version: 2.16.3-1+debian13
Filename: pool/main/i/icinga2/icinga2-bin_2.16.3-1+debian13_amd64.deb
SHA256: bbbb

Package: icinga2-common
Architecture: all
Version: 2.16.4-1+debian13
Filename: pool/main/i/icinga2/icinga2-common_2.16.4-1+debian13_all.deb
SHA256: cccc
";
        let entries = super::parse_deb_index(index);
        let find = |package: &str, version: &str| {
            entries
                .iter()
                .find(|e| e.package == package && e.version.starts_with(version))
                .unwrap_or_else(|| panic!("{package} {version}"))
        };
        assert_eq!(find("icinga2-bin", "2.16.4-").sha256, "aaaa");
        assert_eq!(find("icinga2-common", "2.16.4-").sha256, "cccc");
        assert!(
            find("icinga2-bin", "2.16.4-")
                .filename
                .ends_with("icinga2-bin_2.16.4-1+debian13_amd64.deb"),
            "the index names where the file is, so no URL has to be guessed"
        );
        // The older version is still in the index and must not be picked by accident.
        assert_eq!(find("icinga2-bin", "2.16.3-").sha256, "bbbb");

        // The vendor's own statement of how old a libc may be: the artifact's reach
        // (ADR-draft-icinga-2).
        assert_eq!(
            super::libc_floor(&find("icinga2-bin", "2.16.4-").depends).as_deref(),
            Some("2.38")
        );
        assert_eq!(super::libc_floor("libssl3 (>= 3.0.0)"), None);
    }

    /// Icinga's own `InRelease`, as the repository served it, verifies against the key the tool
    /// carries — and nothing else does: an altered byte inside the signed text, or a keyring
    /// without that key, is refused before any digest in it is believed
    /// (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(target_os = "linux")]
    #[test]
    fn the_carried_key_verifies_icingas_own_signed_index() {
        let inrelease = include_str!("../testdata/icinga-bookworm-InRelease");
        let verify = |text: &str, key: &[u8]| {
            crate::gpgv::verify_clearsigned(text.as_bytes(), key, super::ICINGA_PRIMARY_KEYS)
        };

        let release = verify(inrelease, super::ICINGA_ARCHIVE_KEY).expect("signed by Icinga");
        assert!(release.contains("Codename: icinga-bookworm"), "{release}");
        assert!(
            !release.contains("BEGIN PGP"),
            "only the signed text is returned"
        );
        assert_eq!(
            super::release_digest(&release, "main/binary-amd64/Packages.gz"),
            Some((
                "f93c784e3e78c39da3feedaa033203c6bfee13b49809372b6b694285b48d708c",
                127_321
            ))
        );

        let altered = inrelease.replacen(
            "f93c784e3e78c39da3feedaa033203c6bfee13b49809372b6b694285b48d708c",
            "093c784e3e78c39da3feedaa033203c6bfee13b49809372b6b694285b48d708c",
            1,
        );
        let refused = verify(&altered, super::ICINGA_ARCHIVE_KEY).expect_err("altered");
        assert!(refused.contains("does not match"), "{refused}");

        let refused = verify(inrelease, super::DEBIAN_ARCHIVE_KEY).expect_err("not Debian's");
        assert!(refused.contains("nobody vouched for"), "{refused}");
    }

    /// An ended Debian release lives on at `archive.debian.org` under the same paths: a URL below
    /// `deb.debian.org`'s two repositories moves to the same path below the archive's, by whole
    /// path segments, and no other URL moves — Icinga's repository among them
    /// (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn only_debians_own_repositories_move_to_its_archive() {
        let moved = |url: &str| super::moved_url(url, super::DEBIAN_MOVES);
        assert_eq!(
            moved("https://deb.debian.org/debian/dists/bullseye/InRelease").as_deref(),
            Some("https://archive.debian.org/debian/dists/bullseye/InRelease")
        );
        assert_eq!(
            moved(
                "https://deb.debian.org/debian-security/pool/updates/main/i/icu/\
                 libicu67_67.1-7+deb11u1_amd64.deb"
            )
            .as_deref(),
            Some(
                "https://archive.debian.org/debian-security/pool/updates/main/i/icu/\
                 libicu67_67.1-7+deb11u1_amd64.deb"
            )
        );
        for stays in [
            "https://packages.icinga.com/debian/dists/icinga-bullseye/InRelease",
            "https://deb.debian.org/debian-ports/dists/sid/InRelease",
            "https://deb.debian.org/debianx/dists/bullseye/InRelease",
            "https://archive.debian.org/debian/dists/bullseye/InRelease",
        ] {
            assert_eq!(moved(stays), None, "{stays}");
        }
    }

    /// A file that answers `404` is read from where it moved, and only a `404` moves: another
    /// failure is the answer, a URL with no move keeps its `404`, and a moved file the archive
    /// does not hold either fails naming both places. A stand-in host serves both trees
    /// (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[tokio::test]
    async fn a_file_debian_no_longer_serves_is_read_from_its_archive_and_only_then() {
        use std::io::Write;

        opamp::tls::install_ring_provider();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let host = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let answer = if path == "/archive/pool/moved.deb" {
                    "HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\narchived"
                } else if path == "/current/pool/failing.deb" {
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        let current = format!("{host}/current");
        let archive = format!("{host}/archive");
        let moves = [(current.as_str(), archive.as_str())];

        let moved = super::get_moved(&format!("{current}/pool/moved.deb"), &moves)
            .await
            .expect("read from the archive");
        assert_eq!(moved.bytes().await.expect("body").as_ref(), b"archived");

        let refused = super::get_moved(&format!("{current}/pool/failing.deb"), &moves)
            .await
            .expect_err("a 500 is the answer");
        assert!(refused.contains("500"), "{refused}");
        let refused = super::get_moved(&format!("{current}/pool/gone.deb"), &moves)
            .await
            .expect_err("in neither place");
        assert!(
            refused.contains("/current/pool/gone.deb")
                && refused.contains("/archive/pool/gone.deb"),
            "{refused}"
        );
        let refused = super::get_moved(&format!("{host}/elsewhere/moved.deb"), &moves)
            .await
            .expect_err("no move");
        assert!(refused.contains("404"), "{refused}");
    }

    /// An archived suite verifies as a served one does: bullseye-security's last `InRelease`, as
    /// `archive.debian.org` serves it, verifies against the carried keys by a pinned primary, and
    /// states no `Valid-Until`, so it is not refused as expired — the one security index that
    /// states none is the last one (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(target_os = "linux")]
    #[test]
    fn an_archived_suite_verifies_by_the_same_keys_and_states_no_valid_until() {
        let inrelease = include_str!("../testdata/debian-bullseye-security-InRelease");
        let release = crate::gpgv::verify_clearsigned(
            inrelease.as_bytes(),
            super::DEBIAN_ARCHIVE_KEY,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        )
        .expect("signed by Debian");
        super::release_names(&release, "bullseye-security").expect("fetched as itself");
        assert!(!release.contains("Valid-Until:"), "{release}");
        super::release_current(&release, time::OffsetDateTime::now_utc())
            .expect("no Valid-Until bounds it");
        assert_eq!(super::index_compression(&release), Some("xz"));
        crate::gpgv::verify_clearsigned(
            inrelease.as_bytes(),
            super::ICINGA_ARCHIVE_KEY,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        )
        .expect_err("not Icinga's");
    }

    /// A signed index stands only for the suite it names: bullseye-security's last `InRelease`,
    /// genuinely signed by Debian's security keys and stating no `Valid-Until`, is refused when it
    /// is served as `bookworm-security`, and Icinga's bookworm index as another distribution's —
    /// while each is read as itself, by its `Codename` or, as `apt` allows, its `Suite`
    /// (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(target_os = "linux")]
    #[test]
    fn a_signed_index_served_for_another_suite_is_refused() {
        let verified = |text: &str, key: &[u8], primaries: &[&str]| {
            crate::gpgv::verify_clearsigned(text.as_bytes(), key, primaries).expect("signed")
        };
        let archived = verified(
            include_str!("../testdata/debian-bullseye-security-InRelease"),
            super::DEBIAN_ARCHIVE_KEY,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        );
        let refused =
            super::release_names(&archived, "bookworm-security").expect_err("another suite's");
        assert!(
            refused.contains("\"bullseye-security\"") && refused.contains("bookworm-security"),
            "{refused}"
        );
        super::release_names(&archived, "bullseye-security").expect("its Codename");
        super::release_names(&archived, "oldoldstable-security").expect("its Suite");

        let icinga = verified(
            include_str!("../testdata/icinga-bookworm-InRelease"),
            super::ICINGA_ARCHIVE_KEY,
            super::ICINGA_PRIMARY_KEYS,
        );
        super::release_names(&icinga, "icinga-bookworm").expect("its own");
        for other in ["icinga-bullseye", "icinga-trixie", "bookworm"] {
            let refused = super::release_names(&icinga, other).expect_err(other);
            assert!(refused.contains("not for"), "{refused}");
        }
        let refused = super::release_names("Origin: Debian\n", "bookworm").expect_err("unnamed");
        assert!(refused.contains("no suite"), "{refused}");
    }

    /// A signed text past its `Valid-Until` is refused, so an old index served again — one that
    /// predates a security fix — is not read; a text that states none is not bounded by one
    /// (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn an_index_past_its_valid_until_is_refused() {
        let at = |text: &str| {
            time::PrimitiveDateTime::parse(
                text,
                time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]"),
            )
            .expect("instant")
            .assume_utc()
        };
        let release = "Codename: bookworm-security\nDate: Wed, 28 Oct 2026 06:47:35 UTC\n\
                       Valid-Until: Wed, 04 Nov 2026 06:47:35 UTC\n";
        super::release_current(release, at("2026-10-28 12:00")).expect("within");
        let refused = super::release_current(release, at("2026-11-04 06:48")).expect_err("past it");
        assert!(refused.contains("old index served again"), "{refused}");
        super::release_current("Codename: icinga-bookworm\n", at("2099-01-01 00:00"))
            .expect("no Valid-Until");
        super::release_current("Valid-Until: next week\n", at("2026-10-28 12:00"))
            .expect_err("unreadable");
    }

    /// The index is read only as the signed text states it: the compressed bytes match their
    /// digest and size, the decompressed text matches its own, and a stream that inflates past
    /// the stated size is refused rather than followed (ADR-draft-icinga-2 clause 18) — in both
    /// forms it is read in, Icinga's `.gz` and Debian's `.xz`.
    /// Verifies: ADR-draft-icinga-2
    #[cfg(target_os = "linux")]
    #[test]
    fn an_index_is_read_only_as_its_signed_release_states_it() {
        use sha2::Digest as _;
        use std::io::Write as _;
        let gzip = |text: &[u8]| {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(text).expect("gzip");
            encoder.finish().expect("gzip")
        };
        let xz = |text: &[u8]| {
            let mut child = std::process::Command::new("xz")
                .args(["--compress", "--stdout"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("xz");
            let mut stdin = child.stdin.take().expect("stdin");
            let text = text.to_vec();
            let writer = std::thread::spawn(move || stdin.write_all(&text));
            let out = child.wait_with_output().expect("xz");
            writer.join().expect("writer").expect("written");
            out.stdout
        };
        let release_for = |ext: &str, packed: &[u8], text: &[u8]| {
            format!(
                "Origin: test\nSHA256:\n {} {} main/binary-amd64/Packages\n {} {} \
                 main/binary-amd64/Packages.{ext}\nSHA512:\n",
                hex::encode(sha2::Sha256::digest(text)),
                text.len(),
                hex::encode(sha2::Sha256::digest(packed)),
                packed.len()
            )
        };
        let text = b"Package: icinga2-bin\nVersion: 2.14.6-1+debian12\n";
        for (ext, pack) in [("gz", &gzip as &dyn Fn(&[u8]) -> Vec<u8>), ("xz", &xz)] {
            let packed = pack(text);
            let release = release_for(ext, &packed, text);
            assert_eq!(super::index_compression(&release), Some(ext));
            assert_eq!(
                super::verified_index(&release, ext, &packed)
                    .expect("as stated")
                    .as_bytes(),
                text,
                "{ext}"
            );

            let mut altered = packed.clone();
            let last = altered.len() - 1;
            altered[last] ^= 1;
            let refused = super::verified_index(&release, ext, &altered).expect_err("altered");
            assert!(refused.contains("signed InRelease"), "{ext}: {refused}");

            // Signed for less text than the stream holds: the read stops at the stated size.
            let bomb = pack(&vec![b'x'; 1 << 20]);
            let release = release_for(ext, &bomb, text);
            let refused =
                super::verified_index(&release, ext, &bomb).expect_err("inflates past it");
            assert!(
                refused.contains("decompresses to something other"),
                "{ext}: {refused}"
            );

            let refused = super::verified_index("Origin: test\nSHA256:\n", ext, &packed)
                .expect_err("unnamed");
            assert!(refused.contains("names no"), "{ext}: {refused}");
        }
        // Debian states both forms for its main suites; the `.xz` is the one read.
        let both =
            "SHA256:\n 00 1 main/binary-amd64/Packages.gz\n 00 1 main/binary-amd64/Packages.xz\n";
        assert_eq!(super::index_compression(both), Some("xz"));
        assert_eq!(super::index_compression("Origin: test\nSHA256:\n"), None);
    }

    /// The newest packaging revision of a release is the one repacked, in Debian's own order —
    /// `-10` after `-9`, a `~` pre-release before the release, an epoch above everything.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_newest_revision_is_taken_in_debians_order() {
        use std::cmp::Ordering::{Greater, Less};
        for (lower, higher) in [
            ("2.14.6-1+debian12", "2.14.6-2+debian12"),
            ("2.14.6-9", "2.14.6-10"),
            ("1.0~rc1-1", "1.0-1"),
            ("2.14.6-1", "2.14.6-1+b1"),
            ("9.9-1", "1:1.0-1"),
            ("2.36-9", "2.36-9+deb12u1"),
        ] {
            assert_eq!(
                super::deb_version_cmp(lower, higher),
                Less,
                "{lower} < {higher}"
            );
            assert_eq!(
                super::deb_version_cmp(higher, lower),
                Greater,
                "{higher} > {lower}"
            );
        }
        let entries = super::parse_deb_index(
            "Package: icinga2-bin\nVersion: 2.14.6-9+debian12\n\n\
             Package: icinga2-bin\nVersion: 2.14.6-10+debian12\n\n\
             Package: icinga2-bin\nVersion: 2.14.6-2+debian12\n",
        );
        assert_eq!(
            super::newest(entries.iter()).map(|entry| entry.version.as_str()),
            Some("2.14.6-10+debian12")
        );
    }

    /// Debian's archive keys are carried, not read from the host (ADR-draft-icinga-2 clause 18):
    /// the keyring holds exactly the six automatic signing keys `ftp-master.debian.org/keys.html`
    /// lists for Debian 11, 12 and 13, by the fingerprints of their primary keys — computed here
    /// from the keyring's own packets, so a key added, dropped or swapped is a red test.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_carried_debian_keys_are_the_archives_own() {
        assert_eq!(
            primary_fingerprints(super::DEBIAN_ARCHIVE_KEY),
            [
                "1F89983E0081FDE018F3CC9673A4F27B8DD47936", // archive, 11/bullseye
                "AC530D520F2F3269F5E98313A48449044AAD5C5D", // security, 11/bullseye
                "B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8", // archive, 12/bookworm
                "05AB90340C0C5E797F44A8C8254CF3B5AEC0A8F0", // security, 12/bookworm
                "04B54C3CDCA79751B16BC6B5225629DF75B188BD", // archive, 13/trixie
                "5E04A1E3223A19A20706E20F9904613D4CCE68C6", // security, 13/trixie
            ]
        );
        assert_eq!(
            primary_fingerprints(super::ICINGA_ARCHIVE_KEY),
            ["DD3AF6198ED000B4C0B73956CC116F55AA7F2382"]
        );
        // The pins a signature is held to are the keyrings' own primaries, all of them: Debian's
        // archive keys for a suite and its `-updates`, its security keys for `-security`.
        let mut pinned = [
            super::DEBIAN_ARCHIVE_PRIMARY_KEYS,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        ]
        .concat();
        pinned.sort_unstable();
        let mut carried = primary_fingerprints(super::DEBIAN_ARCHIVE_KEY);
        carried.sort_unstable();
        assert_eq!(carried, pinned);
        assert_eq!(
            super::DEBIAN_ARCHIVE_PRIMARY_KEYS,
            [
                "1F89983E0081FDE018F3CC9673A4F27B8DD47936",
                "B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8",
                "04B54C3CDCA79751B16BC6B5225629DF75B188BD",
            ],
            "the archive keys, as keys.html names them"
        );
        assert_eq!(
            primary_fingerprints(super::ICINGA_ARCHIVE_KEY),
            super::ICINGA_PRIMARY_KEYS
        );
    }

    /// The v4 fingerprints of the primary keys of a binary OpenPGP keyring: the SHA-1 of `0x99`,
    /// the two-byte length and the body of each public-key packet (RFC 4880 §12.2).
    fn primary_fingerprints(keyring: &[u8]) -> Vec<String> {
        let mut fingerprints = Vec::new();
        let mut at = 0;
        while at < keyring.len() {
            let header = keyring[at];
            assert!(header & 0x80 != 0, "not an OpenPGP packet at {at}");
            let (tag, body, len) = if header & 0x40 != 0 {
                let first = usize::from(keyring[at + 1]);
                let (len, skip) = match first {
                    0..=191 => (first, 2),
                    192..=223 => (((first - 192) << 8) + usize::from(keyring[at + 2]) + 192, 3),
                    255 => (
                        u32::from_be_bytes(keyring[at + 2..at + 6].try_into().expect("len"))
                            as usize,
                        6,
                    ),
                    _ => panic!("partial length in a keyring"),
                };
                (header & 0x3f, at + skip, len)
            } else {
                let (len, skip) = match header & 3 {
                    0 => (usize::from(keyring[at + 1]), 2),
                    1 => (
                        usize::from(u16::from_be_bytes([keyring[at + 1], keyring[at + 2]])),
                        3,
                    ),
                    2 => (
                        u32::from_be_bytes(keyring[at + 1..at + 5].try_into().expect("len"))
                            as usize,
                        5,
                    ),
                    _ => panic!("indeterminate length in a keyring"),
                };
                ((header >> 2) & 0x0f, at + skip, len)
            };
            if tag == 6 {
                let packet = &keyring[body..body + len];
                let mut hashed = vec![0x99];
                hashed.extend_from_slice(&u16::try_from(len).expect("short").to_be_bytes());
                hashed.extend_from_slice(packet);
                let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &hashed);
                fingerprints.push(hex::encode_upper(digest.as_ref()));
            }
            at = body + len;
        }
        fingerprints
    }

    /// Debian's own `InRelease` of `bookworm-security`, as the archive served it, verifies against
    /// the carried keys — Debian signs with several keys at once, and one of them is enough — by
    /// its security keys, and not by the archive keys a suite and its `-updates` are signed with.
    /// Verifies: ADR-draft-icinga-2
    #[cfg(target_os = "linux")]
    #[test]
    fn the_carried_keys_verify_debians_own_signed_index() {
        let inrelease = include_str!("../testdata/debian-bookworm-security-InRelease");
        let verify = |key: &[u8], primaries: &[&str]| {
            crate::gpgv::verify_clearsigned(inrelease.as_bytes(), key, primaries)
        };
        let release = verify(
            super::DEBIAN_ARCHIVE_KEY,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        )
        .expect("signed by Debian");
        assert!(release.contains("Codename: bookworm-security"), "{release}");
        assert_eq!(super::index_compression(&release), Some("xz"));

        // A security index does not stand in for a suite or its `-updates`.
        let refused = verify(
            super::DEBIAN_ARCHIVE_KEY,
            super::DEBIAN_ARCHIVE_PRIMARY_KEYS,
        )
        .expect_err("not an archive key's");
        assert!(refused.contains("nobody vouched for"), "{refused}");
        // Icinga's key alone vouches for nothing of Debian's.
        let refused = verify(
            super::ICINGA_ARCHIVE_KEY,
            super::DEBIAN_SECURITY_PRIMARY_KEYS,
        )
        .expect_err("not Icinga's");
        assert!(refused.contains("nobody vouched for"), "{refused}");
        // Nor does Debian's keyring, with Debian's keys pinned for another archive.
        let refused =
            verify(super::DEBIAN_ARCHIVE_KEY, super::ICINGA_PRIMARY_KEYS).expect_err("not pinned");
        assert!(refused.contains("nobody vouched for"), "{refused}");
    }

    /// A slice of bookworm's real index, with the relationships that make the closure hard: a
    /// virtual name only a `Provides` answers, a library at a newer version in `-security` than in
    /// the suite, `Pre-Depends`, an alternative whose first name is not in the index, and the
    /// packages an installation runs rather than a program links.
    const BOOKWORM_SLICE: &str = "\
Package: icinga2-bin
Version: 2.16.5-1+debian12
Depends: icinga2-common (= 2.16.5-1+debian12), libboost-regex1.74.0-icu72, libc6 (>= 2.34), libssl3 (>= 3.0.0), libstdc++6 (>= 12), adduser
Section: admin

Package: monitoring-plugins-basic
Version: 2.3.3-5+deb12u2
Depends: iputils-ping, monitoring-plugins-common, procps, ucf, libc6 (>= 2.34), libssl3 (>= 3.0.0)
Section: net

Package: libboost-regex1.74.0
Version: 1.74.0+ds1-21
Provides: libboost-regex1.74.0-icu72
Depends: libc6 (>= 2.14), libgcc-s1 (>= 3.0), libicu72 (>= 72.1~rc-1~), libstdc++6 (>= 11)
Section: libs

Package: libicu72
Version: 72.1-3+deb12u1
Depends: libc6 (>= 2.34), libgcc-s1 (>= 3.0), libstdc++6 (>= 12)
Section: libs

Package: libssl3
Version: 3.0.17-1~deb12u2
Depends: libc6 (>= 2.34)
Section: libs

Package: libssl3
Version: 3.0.22-1~deb12u2
Depends: libc6 (>= 2.34)
Section: libs

Package: libstdc++6
Version: 12.2.0-14+deb12u1
Depends: gcc-12-base (= 12.2.0-14+deb12u1), libc6 (>= 2.36), libgcc-s1 (>= 4.3)
Section: libs

Package: libgcc-s1
Version: 12.2.0-14+deb12u1
Depends: gcc-12-base (= 12.2.0-14+deb12u1), libc6 (>= 2.35)
Section: libs

Package: gcc-12-base
Version: 12.2.0-14+deb12u1
Section: libs

Package: libsystemd0
Version: 252.39-1~deb12u2
Pre-Depends: libcap2 (>= 1:2.10), libzstd1 (>= 1.5.4)
Section: libs

Package: libcap2
Version: 1:2.66-4+deb12u3+b1
Depends: libc6 (>= 2.34)
Section: libs

Package: libzstd1
Version: 1.5.4+dfsg2-5
Depends: libc6 (>= 2.34)
Section: libs

Package: libedit2
Version: 3.1-20221030-2
Depends: libbsd0 (>= 0.1.3), libc6 (>= 2.34), libtinfo6 (>= 6)
Section: libs

Package: libbsd0
Version: 0.11.7-2
Depends: libc6 (>= 2.34), libmd0 (>= 1.0.3-2)
Section: libs

Package: libmd0
Version: 1.0.4-2
Depends: libc6 (>= 2.33)
Section: libs

Package: libtinfo6
Version: 6.4-4
Depends: libc6 (>= 2.34)
Section: libs

Package: libc6
Version: 2.36-9+deb12u13
Depends: libgcc-s1
Section: libs

Package: adduser
Version: 3.134
Depends: passwd
Section: admin

Package: passwd
Version: 1:4.13+dfsg1-1+b1
Depends: libpam0g, libc6
Section: admin

Package: libpam0g
Version: 1.5.2-6+deb12u1
Section: libs

Package: procps
Version: 2:4.0.2-3
Depends: libproc2-0
Section: admin

Package: libproc2-0
Version: 2:4.0.2-3
Section: libs
";

    /// The library packages are the `Depends` and `Pre-Depends` closure of the tree's own packages
    /// in the verified index, through Debian's `libs` section alone and without glibc — newest
    /// version each, a virtual name answered by its provider, and the same answer whatever order
    /// the index came in (ADR-draft-icinga-2 clause 18).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn the_library_packages_are_the_depends_closure_in_the_signed_index() {
        let closure = |text: &str, extra: &str| {
            let index = super::parse_deb_index(text);
            let mut roots: Vec<&super::DebEntry> = index
                .iter()
                .filter(|e| {
                    ["icinga2-bin", "monitoring-plugins-basic"].contains(&e.package.as_str())
                })
                .collect();
            let wanted = super::parse_deb_index(extra);
            roots.extend(wanted.iter());
            super::library_packages(&roots, &index)
                .iter()
                .map(|e| format!("{} {}", e.package, e.version))
                .collect::<Vec<_>>()
        };
        let expected = [
            "gcc-12-base 12.2.0-14+deb12u1",
            "libboost-regex1.74.0 1.74.0+ds1-21",
            "libbsd0 0.11.7-2",
            "libcap2 1:2.66-4+deb12u3+b1",
            "libedit2 3.1-20221030-2",
            "libgcc-s1 12.2.0-14+deb12u1",
            "libicu72 72.1-3+deb12u1",
            "libmd0 1.0.4-2",
            "libssl3 3.0.22-1~deb12u2",
            "libstdc++6 12.2.0-14+deb12u1",
            "libsystemd0 252.39-1~deb12u2",
            "libtinfo6 6.4-4",
            "libzstd1 1.5.4+dfsg2-5",
        ];
        // A root that reaches `libsystemd0` by an alternative whose first name the index lacks,
        // and `libedit2` directly.
        let extra = "Package: extra\nDepends: libsystemd-absent | libsystemd0, libedit2\n";
        assert_eq!(closure(BOOKWORM_SLICE, extra), expected);

        // The floor is the highest any of them states: `libstdc++6`'s 2.36, not the vendor's 2.34.
        let index = super::parse_deb_index(BOOKWORM_SLICE);
        let bin = index
            .iter()
            .find(|e| e.package == "icinga2-bin")
            .expect("bin");
        let libraries = super::library_packages(&[bin], &index);
        assert_eq!(
            super::glibc_floor([bin].into_iter()).as_deref(),
            Some("2.34")
        );
        assert_eq!(
            super::glibc_floor(std::iter::once(bin).chain(libraries)).as_deref(),
            Some("2.36")
        );

        let mut stanzas: Vec<&str> = BOOKWORM_SLICE.split("\n\n").collect();
        stanzas.reverse();
        assert_eq!(
            closure(&stanzas.join("\n\n"), extra),
            expected,
            "the order of the index decides nothing"
        );
    }

    /// The `DT_NEEDED` of an ELF file are read out of its bytes: this very test binary links
    /// glibc, a script is no ELF file and needs nothing, and a file whose headers point outside
    /// it is refused rather than guessed at.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn the_libraries_a_program_needs_are_read_without_running_it() {
        let own = std::fs::read("/proc/self/exe").expect("own binary");
        let needed = super::elf_needed(&own).expect("ELF").expect("is ELF");
        assert!(needed.iter().any(|n| n == "libc.so.6"), "{needed:?}");

        assert_eq!(super::elf_needed(b"#!/bin/sh\necho ok\n"), Ok(None));
        let made = tiny_elf(&["libfoo.so.1", "libc.so.6"]);
        assert_eq!(
            super::elf_needed(&made),
            Ok(Some(vec![
                "libfoo.so.1".to_string(),
                "libc.so.6".to_string()
            ]))
        );
        super::elf_needed(&made[..made.len() - 20]).expect_err("cut short");
        super::elf_needed(&own[..64]).expect_err("headers only");
        let mut big_endian = made.clone();
        big_endian[5] = 2;
        super::elf_needed(&big_endian).expect_err("big-endian");
    }

    /// A minimal 64-bit little-endian ELF file whose dynamic section names `needed`: the header,
    /// one loaded segment covering the whole file at address 0, one dynamic segment, and the
    /// string table.
    #[cfg(unix)]
    fn tiny_elf(needed: &[&str]) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut offsets = Vec::new();
        for name in needed {
            offsets.push(strtab.len() as u64);
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
        }
        let (phoff, phnum) = (64u64, 2u64);
        let dyn_off = phoff + 56 * phnum;
        let dyn_entries = needed.len() as u64 + 3;
        let str_off = dyn_off + 16 * dyn_entries;
        let total = str_off + strtab.len() as u64;
        let mut out = vec![0u8; 64];
        out[..6].copy_from_slice(b"\x7fELF\x02\x01");
        out[0x20..0x28].copy_from_slice(&phoff.to_le_bytes());
        out[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        out[0x38..0x3a].copy_from_slice(&(phnum as u16).to_le_bytes());
        let mut header = |kind: u32, offset: u64, size: u64| {
            let mut ph = vec![0u8; 56];
            ph[..4].copy_from_slice(&kind.to_le_bytes());
            ph[8..16].copy_from_slice(&offset.to_le_bytes());
            ph[16..24].copy_from_slice(&offset.to_le_bytes());
            ph[32..40].copy_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&ph);
        };
        header(1, 0, total);
        header(2, dyn_off, 16 * dyn_entries);
        let mut entry = |tag: u64, value: u64| {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        };
        for offset in offsets {
            entry(1, offset);
        }
        entry(5, str_off);
        entry(10, strtab.len() as u64);
        entry(0, 0);
        out.extend_from_slice(&strtab);
        out
    }

    /// The tree's libraries come out of the unpacked archive packages and from nowhere else: what
    /// the programs need is followed through the libraries gathered for them, a link resolves to
    /// its file inside its package's tree, glibc stays behind — and a library the packages do not
    /// hold fails the repack naming it and who needs it, even where the host has it, as does a
    /// link that would reach into the host for it (ADR-draft-icinga-2 clause 18, ADR-0032).
    /// Verifies: ADR-draft-icinga-2
    #[cfg(unix)]
    #[test]
    fn the_tree_takes_its_libraries_from_the_archives_packages_and_nothing_from_the_host() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let packages = dir.path().join("libraries");
        let usr_lib = packages.join("usr/lib/x86_64-linux-gnu");
        let lib = packages.join("lib/x86_64-linux-gnu");
        std::fs::create_dir_all(&usr_lib).expect("dirs");
        std::fs::create_dir_all(&lib).expect("dirs");
        let foo = tiny_elf(&["libbar.so.0", "libc.so.6"]);
        let bar = tiny_elf(&["libm.so.6"]);
        std::fs::write(usr_lib.join("libfoo.so.1.2.3"), &foo).expect("foo");
        symlink("libfoo.so.1.2.3", usr_lib.join("libfoo.so.1")).expect("link");
        std::fs::write(lib.join("libbar.so.0"), &bar).expect("bar");
        std::fs::write(usr_lib.join("libunused.so.9"), tiny_elf(&[])).expect("unused");

        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).expect("programs");
        let daemon = programs.join("icinga2");
        std::fs::write(
            &daemon,
            tiny_elf(&["libfoo.so.1", "libc.so.6", "ld-linux-x86-64.so.2"]),
        )
        .expect("daemon");
        let script = programs.join("check_script");
        std::fs::write(&script, b"#!/bin/sh\n").expect("script");

        let lib_dir = dir.path().join("tree/lib");
        super::bundle_libraries(&[daemon.clone(), script], &lib_dir, &[&packages])
            .expect("bundled");
        let mut carried: Vec<String> = std::fs::read_dir(&lib_dir)
            .expect("lib")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        carried.sort();
        assert_eq!(carried, ["libbar.so.0", "libfoo.so.1"]);
        assert_eq!(
            std::fs::read(lib_dir.join("libfoo.so.1")).expect("foo"),
            foo
        );
        assert!(!std::fs::symlink_metadata(lib_dir.join("libfoo.so.1"))
            .expect("meta")
            .file_type()
            .is_symlink());

        // `libgcc_s.so.1` is on every host this test runs on — the test binary links it — and it
        // is still not taken from there.
        let wants_more = programs.join("check_more");
        std::fs::write(&wants_more, tiny_elf(&["libgcc_s.so.1"])).expect("plugin");
        let lib_dir = dir.path().join("again/lib");
        let refused =
            super::bundle_libraries(std::slice::from_ref(&wants_more), &lib_dir, &[&packages])
                .expect_err("not in the packages");
        assert!(
            refused.contains("libgcc_s.so.1 (needed by check_more)"),
            "{refused}"
        );
        assert!(!lib_dir.join("libgcc_s.so.1").exists());

        // A package link into the host's own library directory is followed no further.
        symlink(
            "/lib/x86_64-linux-gnu/libgcc_s.so.1",
            lib.join("libgcc_s.so.1"),
        )
        .expect("link");
        let refused =
            super::bundle_libraries(&[wants_more], &dir.path().join("third/lib"), &[&packages])
                .expect_err("a link into the host");
        assert!(refused.contains("ADR-0032"), "{refused}");
    }

    use super::*;

    /// What the Icinga 2 repack copies out of an unpacked vendor package is resolved inside that
    /// tree alone (ADR-0032): a link to a file of the tree is copied as that file, a link into the
    /// host's file system fails the repack naming it, and a link to nothing is left out.
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_copy_out_of_an_unpacked_package_follows_no_link_out_of_it() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let unpacked = dir.path().join("unpacked");
        let plugins = unpacked.join("usr/lib/nagios/plugins");
        std::fs::create_dir_all(&plugins).expect("plugins");
        std::fs::write(plugins.join("check_ping"), b"ping").expect("write");
        symlink("check_ping", plugins.join("check_alias")).expect("link");
        symlink("missing", plugins.join("check_gone")).expect("link");

        let copied = dir.path().join("copied");
        copy_from_tree(&unpacked, &plugins, &copied).expect("copied");
        assert_eq!(
            std::fs::read(copied.join("check_alias")).expect("read"),
            b"ping"
        );
        assert!(!std::fs::symlink_metadata(copied.join("check_alias"))
            .expect("meta")
            .file_type()
            .is_symlink());
        assert!(
            !copied.join("check_gone").exists(),
            "a link to nothing is left out"
        );

        let host = dir.path().join("host-secret");
        std::fs::write(&host, b"secret").expect("write");
        symlink(&host, plugins.join("check_host")).expect("link");
        let err = copy_from_tree(&unpacked, &plugins, &dir.path().join("again"))
            .expect_err("a link into the host");
        assert!(
            err.contains("check_host") && err.contains("ADR-0032"),
            "{err}"
        );
        std::fs::remove_file(plugins.join("check_host")).expect("remove");
        symlink("../../../../../host-secret", plugins.join("check_climb")).expect("link");
        let err = copy_from_tree(&unpacked, &plugins, &dir.path().join("third"))
            .expect_err("a link that climbs out");
        assert!(err.contains("check_climb"), "{err}");
    }

    /// A later repack of one version is numbered as a pre-release of the next patch, so it orders
    /// above the first repack and below upstream's next release, its repack number compared as a
    /// number; the first keeps the version
    /// (ADR-draft-the-operator-program-opamp-fleetctl clause 28).
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn a_later_repack_orders_between_the_version_and_the_next_release() {
        use std::cmp::Ordering;
        let repack = |n| numbered_repack("2.14.3", n);
        assert_eq!(repack(1).as_deref(), Ok("2.14.3"));
        assert_eq!(repack(2).as_deref(), Ok("2.14.4-0.r.2"));
        assert_eq!(repack(10).as_deref(), Ok("2.14.4-0.r.10"));
        let order = [
            "2.14.3".to_string(),
            repack(2).expect("r.2"),
            repack(3).expect("r.3"),
            repack(9).expect("r.9"),
            repack(10).expect("r.10"),
            repack(11).expect("r.11"),
            repack(100).expect("r.100"),
            // Upstream's own pre-release of the next patch still lies above every repack.
            "2.14.4-rc.1".to_string(),
            "2.14.4".to_string(),
        ];
        for pair in order.windows(2) {
            assert_eq!(
                fleet_core::version::precedence(&pair[0], &pair[1]),
                Some(Ordering::Less),
                "{pair:?}"
            );
        }
        // Build metadata would compare equal to the first repack, so it is not the form.
        assert_eq!(
            fleet_core::version::precedence("2.14.3", "2.14.3+r2"),
            Some(Ordering::Equal)
        );
        repack(0).expect_err("counts from 1");
        numbered_repack("2.14.3-rc.1", 2).expect_err("not three numbers");
        numbered_repack("2.14", 2).expect_err("not three numbers");
        numbered_repack(&format!("1.0.{}", u64::MAX), 2).expect_err("no next patch");
        // An agent this tool uploads as upstream publishes it has one artifact per version.
        for agent in [
            AgentKind::Otelcol,
            AgentKind::OtelcolContrib,
            AgentKind::Supervisor,
        ] {
            assert!(!agent.repacked(), "{agent:?}");
            repack_version(agent, "0.158.0", 2).expect_err("not repacked");
            assert_eq!(
                repack_version(agent, "0.158.0", 1).as_deref(),
                Ok("0.158.0")
            );
        }
    }

    /// The tag patterns are what keep a version list from offering things that are not releases of
    /// the agent being fetched — the Collector repository tags its builder alongside.
    #[test]
    fn a_tag_is_a_version_only_when_it_is_this_agents_release() {
        assert_eq!(
            AgentKind::Otelcol.version_of_tag("v0.158.0"),
            Some("0.158.0".to_string())
        );
        for other in [
            "cmd/builder/v0.158.0",
            "cmd/opampsupervisor/v0.158.0",
            "0.158.0",
        ] {
            assert_eq!(AgentKind::Otelcol.version_of_tag(other), None, "{other}");
        }

        // This project's own tags carry the prefix its release workflow creates them under, and
        // the round trip is what `--version` relies on: an operator types what the release page
        // shows, and the tag is derived from it.
        assert_eq!(
            AgentKind::Supervisor.version_of_tag("version/1.2.3"),
            Some("1.2.3".to_string())
        );
        for other in ["v1.2.3", "1.2.3", "version/1.2", "version/1.2.3-dev"] {
            assert_eq!(AgentKind::Supervisor.version_of_tag(other), None, "{other}");
        }
        assert_eq!(
            AgentKind::Supervisor.tag_of_version("1.2.3"),
            "version/1.2.3"
        );
    }

    /// A later repack of one Icinga version is numbered as the core numbers any repack: the second
    /// of 2.14.3 is `2.14.4-0.r.2`, the first keeps the version (ADR-draft-icinga-2 clause 27).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn a_later_icinga_repack_is_numbered_as_the_core_numbers_one() {
        assert!(AgentKind::Icinga2.repacked());
        let icinga = |n| repack_version(AgentKind::Icinga2, "2.14.3", n);
        assert_eq!(icinga(1).as_deref(), Ok("2.14.3"));
        assert_eq!(icinga(2).as_deref(), Ok("2.14.4-0.r.2"));
        assert_eq!(icinga(10).as_deref(), Ok("2.14.4-0.r.10"));
    }

    /// Upstream `1.19` is fetched under its own tag and named Package `1.19.0`, and a number that
    /// cannot be written as SemVer is refused, naming it (ADR-0028 clause 6). How a number is
    /// written is tested with `fleet_core::version::written_as_semver`.
    /// Verifies: ADR-0028
    #[test]
    fn fetch_names_the_package_in_semver_and_fetches_upstreams_tag() {
        assert_eq!(
            AgentKind::Otelcol.release_names("1.19"),
            Ok(("v1.19".to_string(), "1.19.0".to_string()))
        );
        assert_eq!(
            AgentKind::Otelcol.release_names("0.158.0"),
            Ok(("v0.158.0".to_string(), "0.158.0".to_string()))
        );
        for unwritable in ["nightly", "1.2.3.4", "1.x", "", "1.0.0-01"] {
            let refusal = AgentKind::Otelcol
                .release_names(unwritable)
                .expect_err(unwritable);
            assert!(
                refusal.contains(&format!("{unwritable:?}")) && refusal.contains("SemVer"),
                "{refusal}"
            );
        }
    }

    /// Icinga 2's release numbers are SemVer already, so its Package is named by the release number
    /// itself — the number the kind reads out of the daemon's banner, which is what a rollout's
    /// version test compares — and the tag is upstream's own (ADR-draft-icinga-2 clause 17).
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn icinga_2s_package_is_named_by_its_release_number() {
        assert_eq!(
            AgentKind::Icinga2.release_names("2.16.5"),
            Ok(("v2.16.5".to_string(), "2.16.5".to_string()))
        );
        assert_eq!(
            AgentKind::Icinga2.version_of_tag("v2.14.6").as_deref(),
            Some("2.14.6")
        );
        assert!(fleet_core::version::is_semver("2.14.6"));
    }

    /// Versions sort by their numbers: `0.9.0` is older than `0.10.0`, which as text it is not.
    #[test]
    fn versions_sort_by_number_rather_than_by_text() {
        let mut versions = vec![
            "0.9.0".to_string(),
            "0.10.0".to_string(),
            "0.158.0".to_string(),
        ];
        versions.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
        assert_eq!(versions, vec!["0.158.0", "0.10.0", "0.9.0"]);
    }

    /// The list offers release *series*, not tags. An agent that patches often makes the
    /// difference visible: five 2.16 patches would have been the entire list, so the one question an operator
    /// asks it — which versions can I still go back to — would have no answer in it. Only the
    /// newest patch of a series is offered, because an older one of the same line is not a choice
    /// anybody makes.
    #[test]
    fn the_version_list_offers_the_newest_patch_of_each_recent_series() {
        let often_patched: Vec<String> = [
            "2.16.5", "2.16.4", "2.16.3", "2.16.2", "2.16.1", "2.16.0", "2.15.2", "2.15.1",
            "2.14.6", "2.13.10",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            super::newest_per_series(&often_patched),
            ["2.16.5", "2.15.2", "2.14.6"]
        );

        // An agent that versions in two parts has no patch to collapse, so every version is its own
        // series and the list is simply the last three (tags `1.15`, `1.14`, …).
        let two_part: Vec<String> = ["1.15", "1.14", "1.13", "1.12"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(
            super::newest_per_series(&two_part),
            ["1.15", "1.14", "1.13"]
        );

        // Fewer series than asked for is the whole list, not an error — a young repository has
        // nothing else to offer.
        let young: Vec<String> = ["0.2.1", "0.2.0"].map(str::to_string).to_vec();
        assert_eq!(super::newest_per_series(&young), ["0.2.1"]);
        assert!(super::newest_per_series(&[]).is_empty());
    }

    fn asset(name: &str) -> (String, String) {
        (name.to_string(), format!("https://example.invalid/{name}"))
    }

    /// The Collector's platform list is read from the assets, and the two distributions must not
    /// be confused: `otelcol-contrib_…` starts with text a careless `otelcol_` check would accept.
    #[test]
    fn collector_plans_come_from_the_assets_and_keep_the_distributions_apart() {
        let assets = vec![
            asset("otelcol_0.158.0_linux_amd64.tar.gz"),
            asset("otelcol_0.158.0_windows_amd64.tar.gz"),
            asset("otelcol_0.158.0_linux_s390x.tar.gz"),
            asset("otelcol_0.158.0_windows_x64.msi"),
            asset("otelcol-contrib_0.158.0_linux_amd64.tar.gz"),
        ];

        let core = collector_plans(AgentKind::Otelcol, "0.158.0", &assets);
        let platforms: Vec<String> = core
            .iter()
            .map(|p| format!("{}/{}", p.os, p.arch))
            .collect();
        assert_eq!(
            platforms,
            vec!["linux/amd64", "windows/amd64"],
            "an MSI is not an artifact a Supervisor installs, and s390x is a platform this fleet \
             cannot name"
        );
        assert!(
            core[1].block_hint.contains("otelcol.exe"),
            "{:?}",
            core[1].block_hint
        );

        let contrib = collector_plans(AgentKind::OtelcolContrib, "0.158.0", &assets);
        assert_eq!(contrib.len(), 1);
        assert_eq!(
            contrib[0].out_name,
            "otelcol-contrib_0.158.0_linux_amd64.tar.gz"
        );
    }

    /// The release also carries the operator tool's archives, named like the Client's
    /// (ADR-0032 clause 3); they are selected by stem, never by version and platform alone, so none
    /// of them becomes the `supervisor` Package's entry for its platform (ADR-0034 clause 5).
    // Verifies: ADR-0032
    #[test]
    fn the_supervisor_passes_over_the_operator_tools_archives() {
        let assets = vec![
            asset("opamp-fleetctl_1.2.3_linux_amd64.tar.gz"),
            asset("opamp-fleetctl_1.2.3_darwin_arm64.tar.gz"),
            asset("supervisor_1.2.3_linux_amd64.tar.gz"),
            asset("SHA256SUMS"),
        ];
        let plans = supervisor_plans("1.2.3", &assets);
        let names: Vec<&str> = plans.iter().map(|p| p.out_name.as_str()).collect();
        assert_eq!(names, ["supervisor_1.2.3_linux_amd64.tar.gz"]);
    }

    /// This project's own release is fetched like any other agent's (ADR-0034): the `.tar.gz` per
    /// platform is the package, the installers beside it are not, and the one `SHA256SUMS` the
    /// publish job writes is what every artifact is verified against.
    #[test]
    fn the_supervisor_takes_the_archives_of_its_own_release_and_not_the_installers() {
        let assets = vec![
            asset("supervisor_1.2.3_linux_amd64.tar.gz"),
            asset("supervisor_1.2.3_linux_amd64.deb"),
            asset("supervisor_1.2.3_linux_amd64.rpm"),
            asset("supervisor_1.2.3_linux_arm64.tar.gz"),
            asset("supervisor_1.2.3_darwin_arm64.tar.gz"),
            asset("supervisor_1.2.3_darwin_amd64.tar.gz"),
            asset("supervisor_1.2.3_windows_amd64.tar.gz"),
            asset("supervisor_1.2.3_windows_amd64.msi"),
            asset("supervisor_1.1.0_linux_amd64.tar.gz"),
            asset("SHA256SUMS"),
        ];

        let plans = supervisor_plans("1.2.3", &assets);
        let platforms: Vec<String> = plans
            .iter()
            .map(|p| format!("{}/{}", p.os, p.arch))
            .collect();
        assert_eq!(
            platforms,
            vec![
                "darwin/amd64",
                "darwin/arm64",
                "linux/amd64",
                "linux/arm64",
                "windows/amd64",
            ],
            "an installer is not an artifact a fleet distributes, and another release's archive              is not this version"
        );
        // The menu promises exactly these, so a target added to the workflow and not to the list
        // is caught here rather than by an operator who cannot find their platform.
        let promised: Vec<String> = SUPERVISOR_PLATFORMS
            .iter()
            .map(|(os, arch)| format!("{os}/{arch}"))
            .collect();
        let mut promised_sorted = promised.clone();
        promised_sorted.sort();
        assert_eq!(platforms, promised_sorted);

        for plan in &plans {
            assert!(
                matches!(plan.action, Action::AsPublished),
                "the release is packed by this project's own packer; repacking it would only                  break the published hash"
            );
            match &plan.sources[0].checksum {
                ChecksumSource::Sums { urls } => {
                    assert_eq!(urls.len(), 1, "one file holds every artifact's line");
                    assert!(urls[0].ends_with("SHA256SUMS"), "{urls:?}");
                }
                other => panic!(
                    "the release publishes sums, not {:?}",
                    std::mem::discriminant(other)
                ),
            }
            assert!(
                plan.block_hint.contains("[packages] self_update"),
                "a Client takes this one over itself, so the hint is its consent and not a                  [[supervisor]] block: {:?}",
                plan.block_hint
            );
        }

        // Nothing to seed beside it: the Client's configuration is its host's (ADR-0017).
        assert!(AgentKind::Supervisor.default_configurations().is_empty());
    }

    /// The checksum layout changed at 0.158.0, so which one a release uses is read from its assets
    /// rather than derived from its version.
    #[test]
    fn the_collector_checksum_follows_whichever_form_the_release_publishes() {
        let modern = vec![
            asset("otelcol_0.158.0_linux_amd64.tar.gz"),
            asset("otelcol_0.158.0_linux_amd64.tar.gz.sha256"),
        ];
        assert!(matches!(
            collector_checksum("otelcol_0.158.0_linux_amd64.tar.gz", "otelcol", &modern),
            ChecksumSource::BareDigest { .. }
        ));

        let older = vec![
            asset("otelcol_0.157.0_linux_amd64.tar.gz"),
            asset("opentelemetry-collector-releases_otelcol_checksums.txt"),
        ];
        assert!(matches!(
            collector_checksum("otelcol_0.157.0_linux_amd64.tar.gz", "otelcol", &older),
            ChecksumSource::Sums { .. }
        ));
    }

    /// Icinga 2's Linux artifact is the Debian packages repacked under the wrapper
    /// `icinga2-<version>`, named by the Package version, and the block is the kind alone
    /// (ADR-draft-icinga-2 clauses 17 and 24). The packing half of `docs/artifacts/icinga2.md`;
    /// its client half is in `crates/fleet-agent/src/supervisor/icinga2.rs`.
    /// Verifies: ADR-draft-icinga-2
    #[test]
    fn icinga_2s_linux_artifact_is_the_debian_tree_under_its_wrapper() {
        let source = |name: &str| Download {
            url: format!("https://deb.debian.org/debian/pool/main/{name}.deb"),
            checksum: ChecksumSource::Known {
                sha256: "00".repeat(32),
            },
        };
        let plan = linux_plan("2.16.5", vec![source("icinga2-bin"), source("libssl3")], 1);
        assert_eq!((plan.os.as_str(), plan.arch.as_str()), ("linux", "amd64"));
        assert_eq!(plan.sources.len(), 2);
        match &plan.action {
            Action::RepackDebs {
                wrapper,
                tree_packages,
            } => {
                assert_eq!(wrapper, "icinga2-2.16.5");
                assert_eq!(*tree_packages, 1);
            }
            Action::AsPublished => panic!("the Debian packages are repacked into one tree"),
        }
        assert_eq!(plan.out_name, "icinga2_2.16.5_linux_amd64.tar.gz");
        assert_eq!(
            plan.block_hint,
            "type = \"icinga2\"  (docs/artifacts/icinga2.md)"
        );
    }

    /// A directory reached through a link is packed **under the linked name too**, with its
    /// contents — the regression this exists for. A tree may reach its Perl library
    /// through `usr/share/perl/5.26`, a link to `5.26.1`; packing that name as an empty directory
    /// produced a tree whose agent could not find a single module, and packing only one of the two
    /// names left which one to directory-read order. A cycle is still not followed, and a link
    /// pointing at nothing is dropped.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_linked_directory_is_packed_under_both_names_and_a_cycle_does_not_hang() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("lib/5.26.1")).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        std::fs::write(tree.join("lib/5.26.1/Universal.pm"), b"a-module").expect("write");
        std::os::unix::fs::symlink("5.26.1", tree.join("lib/5.26")).expect("link the version");
        std::os::unix::fs::symlink("../lib", tree.join("lib/loop")).expect("link a cycle");
        std::os::unix::fs::symlink("nowhere", tree.join("lib/dangling")).expect("link to nothing");

        let artifact = dir.path().join("packed.tar.gz");
        pack_tree(&tree, "wrapper", &[], &artifact).expect("pack");

        let dest = dir.path().join("unpacked");
        std::fs::create_dir_all(&dest).expect("create");
        let summary =
            fleet_agent::archive::extract_tree_tar_gz(&artifact, Path::new("AppRun"), &[], &dest)
                .expect("the packed tree is one a Client installs");

        assert_eq!(
            std::fs::read(dest.join("lib/5.26.1/Universal.pm")).expect("the real path"),
            b"a-module"
        );
        assert_eq!(
            std::fs::read(dest.join("lib/5.26/Universal.pm")).expect("the linked path"),
            b"a-module",
            "the name the agent actually uses carries the contents"
        );
        assert!(
            !dest.join("lib/dangling").exists(),
            "a link pointing at nothing is not a member"
        );
        // The cycle is a directory that stops rather than a walk that never ends; it is only its
        // presence that matters here, since arriving at all means the pack returned.
        assert!(summary.files >= 3, "{summary:?}");
    }

    /// A link leading back up the tree is not followed when what it leads to is the root itself:
    /// the root is the first ancestor of the walk, so a link right under it to `..` or to the root
    /// packs no second copy of the tree; each such link is packed as an empty directory.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_link_to_the_root_is_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("lib")).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        std::fs::write(tree.join("lib/Universal.pm"), b"a-module").expect("write");
        std::os::unix::fs::symlink("..", tree.join("lib/up")).expect("link the root from below");
        std::os::unix::fs::symlink(".", tree.join("self")).expect("link the root from itself");
        std::os::unix::fs::symlink("../lib/..", tree.join("lib/around"))
            .expect("link the root the long way");

        let artifact = dir.path().join("packed.tar.gz");
        pack_tree(&tree, "wrapper", &[], &artifact).expect("pack");

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(
            std::fs::File::open(&artifact).expect("open"),
        ));
        let entries: Vec<(String, tar::EntryType)> = archive
            .entries()
            .expect("entries")
            .map(|entry| {
                let entry = entry.expect("entry");
                let path = entry.path().expect("path").into_owned();
                let path = path.to_string_lossy();
                (
                    path.trim_end_matches('/').to_string(),
                    entry.header().entry_type(),
                )
            })
            .collect();
        for link in ["wrapper/lib/up", "wrapper/self", "wrapper/lib/around"] {
            assert!(
                entries.contains(&(link.to_string(), tar::EntryType::Directory)),
                "{link} is packed as a directory: {entries:?}"
            );
            assert!(
                !entries
                    .iter()
                    .any(|(path, _)| path.starts_with(&format!("{link}/"))),
                "{link} is packed empty: {entries:?}"
            );
        }
        let files: Vec<&str> = entries
            .iter()
            .map(|(path, _)| path.as_str())
            .filter(|path| path.ends_with("Universal.pm") || path.ends_with("AppRun"))
            .collect();
        assert_eq!(
            files,
            vec!["wrapper/AppRun", "wrapper/lib/Universal.pm"],
            "the tree is packed once, not again under a link to its root"
        );
    }

    /// A link whose target is absolute, or whose resolution climbs above the root, fails the repack
    /// and nothing is written — whether or not the path it would name exists on this host, so the
    /// outcome is the tree's and not the host's; an absolute link to the tree's own root is no
    /// exception. The error names each such link by its path in the tree and its target, quoted,
    /// in the order of their paths, and only those.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_tree_fails_the_repack_naming_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("operator-secret");
        std::fs::write(&outside, b"the-operators-file").expect("write");
        let absolute_existing = outside.to_string_lossy().into_owned();
        let own_root = dir.path().join("tree-6").to_string_lossy().into_owned();
        let cases: [&[(&str, &str)]; 7] = [
            &[("lib/certs", absolute_existing.as_str())],
            &[("lib/up", "../../operator-secret")],
            &[("lib/certs", "/nonexistent-opamp-fleet/secret")],
            &[("lib/up", "../../nonexistent-opamp-fleet")],
            &[("lib/gone", "nowhere/../../../operator-secret")],
            &[
                ("lib/up", "../../operator-secret"),
                ("lib/certs", absolute_existing.as_str()),
            ],
            &[("lib/root", own_root.as_str())],
        ];
        for (n, links) in cases.iter().enumerate() {
            let tree = dir.path().join(format!("tree-{n}"));
            std::fs::create_dir_all(tree.join("lib")).expect("create");
            std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
            std::os::unix::fs::symlink("AppRun", tree.join("inside")).expect("link inside");
            for (link, target) in *links {
                std::os::unix::fs::symlink(target, tree.join(link)).expect("link out");
            }

            let artifact = dir.path().join(format!("packed-{n}.tar.gz"));
            let error = pack_tree(&tree, "wrapper", &[], &artifact).expect_err(&format!(
                "a link out of the tree fails the repack: {links:?}"
            ));

            let mut named: Vec<(usize, &str)> = Vec::new();
            for (link, target) in *links {
                let at = error
                    .find(&format!("{link:?} -> {target:?}"))
                    .unwrap_or_else(|| panic!("the error names {link} and its target: {error}"));
                named.push((at, link));
            }
            named.sort();
            let order: Vec<&str> = named.iter().map(|(_, link)| *link).collect();
            let mut sorted = order.clone();
            sorted.sort();
            assert_eq!(order, sorted, "the links are named in path order: {error}");
            assert!(
                !error.contains("inside"),
                "a link inside the tree is not named: {error}"
            );
            assert!(
                !artifact.exists(),
                "no artifact is left at the output path: {links:?}"
            );
        }
    }

    /// A name from upstream reaches the operator's terminal quoted and escaped, so a control
    /// character in it cannot rewrite what the operator reads.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_tree_is_named_escaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).expect("create");
        std::os::unix::fs::symlink("/etc/\x1b[2Jcerts", tree.join("in\x1b[1Avisible"))
            .expect("link out");

        let error = pack_tree(&tree, "wrapper", &[], &dir.path().join("packed.tar.gz"))
            .expect_err("a link out of the tree fails the repack");

        assert!(
            !error.contains('\x1b'),
            "no raw escape reaches the terminal: {error:?}"
        );
        assert!(
            error.contains(r#""in\u{1b}[1Avisible" -> "/etc/\u{1b}[2Jcerts""#),
            "{error:?}"
        );
    }

    /// Only files and directories are packed: a pipe would hang the repack and a device would be
    /// read from outside the tree, so either, and a link to one, fails it, named, before anything
    /// is written.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_member_that_is_neither_file_nor_directory_fails_the_repack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("run")).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        let made = std::process::Command::new("mkfifo")
            .arg(tree.join("run/pipe"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo made the pipe");
        std::os::unix::fs::symlink("run/pipe", tree.join("to-pipe")).expect("link the pipe");

        let artifact = dir.path().join("packed.tar.gz");
        let error = pack_tree(&tree, "wrapper", &[], &artifact)
            .expect_err("a pipe fails the repack instead of hanging it");

        for named in ["\"run/pipe\"", "\"to-pipe\""] {
            assert!(
                error.contains(&format!("{named} is neither a file nor a directory")),
                "the error names {named}: {error}"
            );
        }
        assert!(!artifact.exists(), "no artifact is left at the output path");
    }

    /// The member cap bounds the walk itself: a tree whose links double its paths at every level
    /// names far more than 10 000 members, and the repack stops at the cap instead of walking
    /// them all — without the bound, this walk would not end in any time a test waits.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_tree_whose_links_multiply_it_stops_at_the_member_cap() {
        const LEVELS: usize = 40;
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        for level in 0..LEVELS {
            let here = tree.join(format!("d{level}"));
            std::fs::create_dir_all(&here).expect("create");
            let next = format!("../d{}", level + 1);
            std::os::unix::fs::symlink(&next, here.join("x")).expect("link");
            std::os::unix::fs::symlink(&next, here.join("y")).expect("link");
        }
        std::fs::create_dir_all(tree.join(format!("d{LEVELS}"))).expect("create");
        std::fs::write(tree.join(format!("d{LEVELS}/leaf")), b"a-leaf").expect("write");

        let artifact = dir.path().join("packed.tar.gz");
        let error = pack_tree(&tree, "wrapper", &[], &artifact).expect_err("past the cap");

        assert!(error.contains("10000 members"), "{error}");
        assert!(!artifact.exists(), "no artifact is left at the output path");
    }

    /// A repack that fails while it writes takes back what it wrote, since half an artifact is one
    /// somebody might sign. A member the walk saw but that cannot be read is the failure here.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_repack_that_fails_while_writing_leaves_no_artifact() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        let locked = tree.join("locked");
        std::fs::write(&locked, b"unreadable").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        if std::fs::File::open(&locked).is_ok() {
            // An account that permissions do not bind (root) can read it, so there is no failure
            // to provoke this way.
            return;
        }

        let artifact = dir.path().join("packed.tar.gz");
        let error = pack_tree(&tree, "wrapper", &[], &artifact).expect_err("an unreadable member");

        assert!(error.contains("locked"), "{error}");
        assert!(!artifact.exists(), "the half-written artifact is removed");
    }

    /// A link that stays inside the tree is still dereferenced, to a file as to a directory and
    /// through other links on the way, and one that names nothing inside the tree is dropped —
    /// however it climbs, as long as it does not climb out.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_tree_is_packed_under_its_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("lib/5.26.1")).expect("create");
        std::fs::create_dir_all(tree.join("bin")).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        std::fs::write(tree.join("lib/5.26.1/Universal.pm"), b"a-module").expect("write");
        std::os::unix::fs::symlink("5.26.1", tree.join("lib/5.26")).expect("link a directory");
        std::os::unix::fs::symlink("../AppRun", tree.join("bin/agent")).expect("link a file");
        std::os::unix::fs::symlink("../lib/5.26/Universal.pm", tree.join("bin/module"))
            .expect("link through a link");
        std::os::unix::fs::symlink("../lib/missing", tree.join("bin/dangling"))
            .expect("link to nothing");

        let artifact = dir.path().join("packed.tar.gz");
        pack_tree(&tree, "wrapper", &[], &artifact).expect("pack");

        let dest = dir.path().join("unpacked");
        std::fs::create_dir_all(&dest).expect("create");
        fleet_agent::archive::extract_tree_tar_gz(&artifact, Path::new("AppRun"), &[], &dest)
            .expect("the packed tree is one a Client installs");
        assert_eq!(
            std::fs::read(dest.join("bin/agent")).expect("the linked file"),
            b"the-entry-point"
        );
        assert_eq!(
            std::fs::read(dest.join("bin/module")).expect("the file behind two links"),
            b"a-module"
        );
        assert_eq!(
            std::fs::read(dest.join("lib/5.26/Universal.pm")).expect("the linked directory"),
            b"a-module"
        );
        assert!(
            !dest.join("bin/dangling").exists(),
            "a link that names nothing inside the tree is not a member"
        );
    }

    #[cfg(unix)]
    const LOADER: KeptLink = KeptLink {
        path: "runtime/lib64/ld-linux-x86-64.so.2",
        target: "/lib64/ld-linux-x86-64.so.2",
    };

    /// A tree with the entry point and the loader link the kind names.
    #[cfg(unix)]
    fn tree_with_a_loader_link(dir: &Path, target: &str) -> PathBuf {
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("runtime/lib64")).expect("create");
        std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
        std::os::unix::fs::symlink(target, tree.join("runtime/lib64/ld-linux-x86-64.so.2"))
            .expect("the loader link");
        tree
    }

    /// A link the kind names by its exact path and target is packed as that link, never followed,
    /// and a Client installing the tree with the same kind creates it.
    ///
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[cfg(unix)]
    #[test]
    fn a_link_the_kind_keeps_is_packed_as_that_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = tree_with_a_loader_link(dir.path(), "/lib64/ld-linux-x86-64.so.2");
        let artifact = dir.path().join("packed.tar.gz");
        pack_tree(&tree, "wrapper", &[LOADER], &artifact).expect("pack");

        let file = std::fs::File::open(&artifact).expect("open");
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let links: Vec<(String, String, u32, u64)> = archive
            .entries()
            .expect("entries")
            .map(|entry| entry.expect("entry"))
            .filter(|entry| entry.header().entry_type().is_symlink())
            .map(|entry| {
                (
                    entry.path().expect("path").to_string_lossy().into_owned(),
                    entry
                        .link_name()
                        .expect("target")
                        .expect("a link has a target")
                        .to_string_lossy()
                        .into_owned(),
                    entry.header().mode().expect("mode"),
                    entry.header().mtime().expect("mtime"),
                )
            })
            .collect();
        assert_eq!(
            links,
            [(
                "wrapper/runtime/lib64/ld-linux-x86-64.so.2".to_string(),
                "/lib64/ld-linux-x86-64.so.2".to_string(),
                0o777,
                0
            )]
        );

        let dest = dir.path().join("unpacked");
        std::fs::create_dir_all(&dest).expect("create");
        fleet_agent::archive::extract_tree_tar_gz(&artifact, Path::new("AppRun"), &[LOADER], &dest)
            .expect("the Client installs the tree with the kind's link");
        assert_eq!(
            std::fs::read_link(dest.join("runtime/lib64/ld-linux-x86-64.so.2")).expect("link"),
            Path::new("/lib64/ld-linux-x86-64.so.2")
        );
    }

    /// Every other link leading out still fails the repack, naming it: the named link without the
    /// kind naming it, the named path with another target, an unnamed absolute link beside a kept
    /// one, and the kept link reached through a linked directory under another name.
    ///
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[cfg(unix)]
    #[test]
    fn a_link_the_kind_does_not_keep_still_fails_the_repack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let refuse = |tree: &Path, kept: &[KeptLink], named: &str| {
            let artifact = tree.with_extension("tar.gz");
            let error = pack_tree(tree, "wrapper", kept, &artifact).expect_err(named);
            assert!(error.contains(named), "{named}: {error}");
            assert!(error.contains("leads out of the tree"), "{error}");
            assert!(!artifact.exists(), "no artifact for {named}");
        };

        let unnamed =
            tree_with_a_loader_link(&dir.path().join("unnamed"), "/lib64/ld-linux-x86-64.so.2");
        refuse(
            &unnamed,
            &[],
            r#""runtime/lib64/ld-linux-x86-64.so.2" -> "/lib64/ld-linux-x86-64.so.2""#,
        );

        let other = tree_with_a_loader_link(&dir.path().join("other"), "/etc/passwd");
        refuse(
            &other,
            &[LOADER],
            r#""runtime/lib64/ld-linux-x86-64.so.2" -> "/etc/passwd""#,
        );

        let beside =
            tree_with_a_loader_link(&dir.path().join("beside"), "/lib64/ld-linux-x86-64.so.2");
        std::os::unix::fs::symlink("/etc/passwd", beside.join("runtime/passwd")).expect("link");
        refuse(&beside, &[LOADER], r#""runtime/passwd" -> "/etc/passwd""#);

        let aliased =
            tree_with_a_loader_link(&dir.path().join("aliased"), "/lib64/ld-linux-x86-64.so.2");
        std::os::unix::fs::symlink("runtime", aliased.join("rt")).expect("link a directory");
        let artifact = dir.path().join("aliased.tar.gz");
        let error = pack_tree(&aliased, "wrapper", &[LOADER], &artifact)
            .expect_err("the kept link under another name");
        assert!(error.contains("leads out of the tree"), "{error}");
        assert!(!artifact.exists());
    }

    /// A repack stores normalized modes: a file with any execute bit `0o755`, every other file
    /// `0o644`, a directory `0o755` — whatever write, setuid, setgid or sticky bits upstream set.
    ///
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[cfg(unix)]
    #[test]
    fn a_repack_stores_normalized_modes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("bin")).expect("create");
        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod")
        };
        for (name, from) in [
            ("AppRun", 0o4777),
            ("bin/tool", 0o710),
            ("bin/data", 0o666),
            ("bin/private", 0o600),
        ] {
            std::fs::write(tree.join(name), name).expect("write");
            mode(&tree.join(name), from);
        }
        mode(&tree.join("bin"), 0o1777);

        let artifact = dir.path().join("packed.tar.gz");
        pack_tree(&tree, "wrapper", &[], &artifact).expect("pack");

        let file = std::fs::File::open(&artifact).expect("open");
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let modes: std::collections::BTreeMap<String, u32> = archive
            .entries()
            .expect("entries")
            .map(|entry| {
                let entry = entry.expect("entry");
                let path = entry.path().expect("path").to_string_lossy().into_owned();
                (path, entry.header().mode().expect("mode"))
            })
            .collect();
        for (path, want) in [
            ("wrapper/AppRun", 0o755),
            ("wrapper/bin/tool", 0o755),
            ("wrapper/bin/data", 0o644),
            ("wrapper/bin/private", 0o644),
        ] {
            assert_eq!(modes.get(path), Some(&want), "{path} in {modes:?}");
        }
        let bin = modes
            .iter()
            .find(|(path, _)| path.trim_end_matches('/') == "wrapper/bin")
            .map(|(_, mode)| *mode);
        assert_eq!(bin, Some(0o755), "{modes:?}");
    }

    /// The bytes of a repack are the tree's alone: the same tree packed twice, and a copy of it
    /// packed from elsewhere on the host, give the same artifact.
    ///
    /// Verifies: ADR-0032
    #[cfg(unix)]
    #[test]
    fn the_same_tree_repacks_to_the_same_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let build = |tree: &Path| {
            std::fs::create_dir_all(tree.join("lib/5.26.1")).expect("create");
            std::fs::write(tree.join("AppRun"), b"the-entry-point").expect("write");
            std::fs::write(tree.join("lib/5.26.1/Universal.pm"), b"a-module").expect("write");
            std::os::unix::fs::symlink("5.26.1", tree.join("lib/5.26")).expect("link");
            std::os::unix::fs::symlink("..", tree.join("lib/up")).expect("link the root");
        };
        let tree = dir.path().join("tree");
        let copy = dir.path().join("elsewhere/copy");
        build(&tree);
        build(&copy);

        let packed = |from: &Path, name: &str| {
            let artifact = dir.path().join(name);
            pack_tree(from, "wrapper", &[], &artifact).expect("pack");
            std::fs::read(&artifact).expect("read")
        };
        let first = packed(&tree, "first.tar.gz");
        assert_eq!(first, packed(&tree, "second.tar.gz"), "packed twice");
        assert_eq!(first, packed(&copy, "copy.tar.gz"), "packed from elsewhere");
    }

    /// Both checksum shapes, read the way each source publishes them — including the `*` a
    /// `sha256sum` binary-mode line carries.
    #[test]
    fn a_published_checksum_is_read_from_either_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir
            .path()
            .join("otelcol-contrib_0.115.0_linux_amd64.tar.gz");
        std::fs::write(&artifact, b"x").expect("write");

        let sums = dir.path().join("sums");
        std::fs::write(
            &sums,
            "aaaa  other.tar.gz\nbbbb *otelcol-contrib_0.115.0_linux_amd64.tar.gz\n",
        )
        .expect("write");
        // Read through the same code the network path uses, with a `file:` URL standing in for the
        // download — the parsing is the part worth testing.
        assert_eq!(
            parse_sums(&std::fs::read_to_string(&sums).expect("read"), &artifact),
            Some("bbbb".to_string())
        );
        assert_eq!(
            parse_sums("aaaa  something-else\n", &artifact),
            None,
            "a file with no line for this artifact is not a checksum for it"
        );
    }

    /// An upload the Server refuses before it reads the body — an assigned Set (ADR-0026), an
    /// unknown identity, a full store — leaves the operator with a transport error and no status:
    /// the refusal races a body already in flight and loses. What the operator must still be told
    /// is *why the Server said no*, never "cannot reach" about a Server that answered.
    ///
    /// The stand-in Server here drops the upload connection without answering at all, which is the
    /// same thing seen from the client and the deterministic form of it: a race that sometimes
    /// delivers the response would be a test that sometimes passes.
    #[tokio::test]
    async fn a_refusal_lost_with_the_upload_is_asked_for_again() {
        use std::io::Write;

        // What `main` does before anything builds a client (ADR-0012).
        opamp::tls::install_ring_provider();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            let mut uploads = 0;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                // Byte at a time, stopping at the blank line: reading the head must never spill
                // into a body this Server has decided not to read.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                if !head.contains("/entries/") {
                    // The Set call is answered as the real Server answers it — body read first,
                    // so the response is not what a reset takes away.
                    let length: usize = head
                        .to_ascii_lowercase()
                        .split("content-length:")
                        .nth(1)
                        .and_then(|rest| rest.split("\r\n").next())
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    let _ = stream.read_exact(&mut body);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                    );
                    continue;
                }
                uploads += 1;
                if uploads == 1 {
                    // The artifact is still being written; this connection ends with nothing on it.
                    continue;
                }
                let refusal = b"{\"error\":\"set otelcol@0.0.1@otelcol is assigned to an Agent\"}";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 409 Conflict\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        refusal.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(refusal);
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("otelcol_0.0.1_linux_amd64.tar.gz");
        // Comfortably past any socket buffer, so the write is still in progress when the
        // connection goes — the shape a real artifact has and a two-byte body would not.
        std::fs::write(&artifact, vec![0u8; 8 * 1024 * 1024]).expect("write");
        let plan = Plan {
            os: "linux".into(),
            arch: "amd64".into(),
            sources: vec![Download {
                url: String::new(),
                checksum: ChecksumSource::BareDigest { url: String::new() },
            }],
            action: Action::AsPublished,
            out_name: "otelcol_0.0.1_linux_amd64.tar.gz".into(),
            block_hint: String::new(),
        };

        let client = http().expect("client");
        let error = upload(&client, &server, "otelcol", "0.0.1", &plan, &artifact)
            .await
            .expect_err("an upload nothing accepted is not a success");
        assert!(
            error.contains("409 Conflict") && error.contains("assigned to an Agent"),
            "the operator is told what the Server refused, not that it could not be reached: {error}"
        );
    }

    /// Every *supervised* agent this tool can fetch carries a default Configuration, and each one
    /// is storable: the name follows the ADR-0019 grammar the Server enforces (which admits no dot,
    /// so no file extension), the body is not empty (the Server refuses an empty one), and it is
    /// stated for its Agent type, so a Deployment holding it fits it to that type alone (ADR-0026).
    ///
    /// This is the test that fails when a variant is added and its default is forgotten, which is
    /// the only way an agent could reach a Server with a package and nothing to run. The Client's
    /// own package is the one exemption, and it is stated below rather than skipped.
    #[test]
    fn every_agent_carries_a_storable_default_configuration() {
        for agent in AgentKind::value_variants() {
            let defaults = agent.default_configurations();
            let service_name = agent.source().service_name;
            if matches!(agent, AgentKind::Supervisor) {
                // The one package nothing supervises. A Client reads `supervisor.toml` on its own
                // host, and the fleet owns only the `[[supervisor]]` half of it (ADR-0017), so
                // there is no body this tool could put on a Server — and an offer needs none: the
                // consent that admits this package is already in that file (ADR-0018).
                assert!(
                    defaults.is_empty(),
                    "{service_name} is the Client itself; a Configuration for it would be a \
                     decision this test has not been told about"
                );
                continue;
            }
            assert!(
                !defaults.is_empty(),
                "{service_name} has no default configuration"
            );
            for config in defaults {
                assert!(
                    !config.name.is_empty()
                        && config
                            .name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{service_name}: {:?} is not a name the Server would accept",
                    config.name
                );
                assert!(
                    !config.body.trim().is_empty(),
                    "{service_name}: {} has an empty body",
                    config.name
                );
                assert_eq!(
                    config.service_name, service_name,
                    "{service_name}: {} would fit every Agent type of its Deployment",
                    config.name
                );
            }
        }
        let icinga = AgentKind::Icinga2.default_configurations();
        let names: Vec<&str> = icinga.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec!["icinga2-conf", "icinga2-zones"],
            "Icinga 2 reads a root file that includes the other by name (ADR-draft-icinga-2), so both travel"
        );
    }

    /// The upload asks before it writes: a Configuration the Server already holds is left exactly
    /// as it is — operator edits included — and only the missing one is stored.
    ///
    /// The stand-in Server answers `200` for `otelcol-conf` and `icinga2-conf` and `404` for
    /// `otelcol-contrib-conf` and `icinga2-zones`, and records every request, so what the test
    /// asserts is the *absence* of a PUT over the top of an existing one.
    #[tokio::test]
    async fn a_default_configuration_the_server_already_holds_is_not_written_over() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        opamp::tls::install_ring_provider();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = format!("http://{}", listener.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let Some(line) = head.lines().next() else {
                    continue;
                };
                recorded.lock().expect("lock").push(line.to_string());
                // A body must be read before the answer, or the response races a reset.
                let length: usize = head
                    .to_ascii_lowercase()
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| rest.split("\r\n").next())
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = stream.read_exact(&mut body);
                let held = ["/otelcol-conf ", "/icinga2-conf "];
                let answer = if line.starts_with("GET") && held.iter().any(|n| line.contains(n)) {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                } else if line.starts_with("GET") {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });

        for agent in [
            AgentKind::Otelcol,
            AgentKind::OtelcolContrib,
            AgentKind::Icinga2,
        ] {
            upload_default_configurations(&http().expect("client"), &server, agent)
                .await
                .expect("the Server accepted the one it was missing");
        }

        let seen = seen.lock().expect("lock").clone();
        let put = |name: &str| {
            let path = format!("/api/v1/configurations/{name} ");
            seen.iter()
                .any(|r| r.starts_with("PUT") && r.contains(&path))
        };
        for missing in ["otelcol-contrib-conf", "icinga2-zones"] {
            assert!(put(missing), "{missing}, missing, is stored: {seen:?}");
        }
        for held in ["otelcol-conf", "icinga2-conf"] {
            assert!(!put(held), "{held}, already held, is left alone: {seen:?}");
        }
    }

    /// `--cacert` is what lets the tool upload to a fleet whose Server certificate a self-made CA
    /// signed: with it the Server is trusted, and without it the same Server is refused for the
    /// reason the operator would otherwise meet — a certificate no system root vouches for.
    ///
    /// Verifies: ADR-0012
    #[tokio::test]
    async fn a_server_signed_by_the_named_ca_is_trusted_and_only_then() {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        use std::io::Write;
        use std::sync::Arc;

        opamp::tls::install_ring_provider();

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("ca");
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().expect("key");
        let leaf = CertificateParams::new(vec!["127.0.0.1".to_string()])
            .expect("params")
            .signed_by(&key, &issuer)
            .expect("leaf");

        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf.der().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("server config"),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = format!("https://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let Ok(connection) = rustls::ServerConnection::new(Arc::clone(&config)) else {
                    continue;
                };
                let mut tls = rustls::StreamOwned::new(connection, stream);
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match tls.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let length: usize = String::from_utf8_lossy(&head)
                    .to_ascii_lowercase()
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| rest.split("\r\n").next())
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = tls.read_exact(&mut body);
                let _ = tls.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                );
                let _ = tls.flush();
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let cacert = dir.path().join("ca.pem");
        std::fs::write(&cacert, ca.pem()).expect("write");
        let url = format!("{server}/api/v1/packages/otelcol/0.0.1");

        let trusting = server_http(Some(&cacert)).expect("the CA reads");
        put_json(&trusting, &url, "{}".to_string())
            .await
            .expect("a Server the named CA signed is reached");

        let error = put_json(&server_http(None).expect("client"), &url, "{}".to_string())
            .await
            .expect_err("without --cacert the same Server is not trusted");
        assert!(error.contains("UnknownIssuer"), "{error}");
    }

    /// A `--cacert` that names no certificate is refused when it is read, before any download,
    /// rather than surfacing as an untrusted Server at the very end.
    #[test]
    fn a_cacert_without_a_certificate_is_refused_up_front() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "").expect("write");
        let error = server_http(Some(&empty)).expect_err("an empty file is no CA");
        assert!(error.contains("no certificates"), "{error}");

        let missing = dir.path().join("missing.pem");
        let error = server_http(Some(&missing)).expect_err("a missing file is no CA");
        assert!(error.contains("cannot read"), "{error}");
    }
}
