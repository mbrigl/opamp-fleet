//! `opamp-package-fetch` — an operator helper that turns an upstream agent release into package
//! artifacts this fleet can install, and optionally uploads them (ADR-0018, ADR-0052, ADR-0064).
//!
//! Getting a real agent into the fleet is a research task before it is a command: which repository
//! publishes the binaries, what the assets are called this month, which checksum file goes with
//! them, and whether the container is one a Client can open at all. This tool holds that knowledge
//! for the four agent types the manual documents — `otelcol`, `otelcol-contrib`, `glpi-agent`, and
//! `telegraf` — and asks the operator only what it cannot know: which one, which version, which
//! platforms, and where to send the result.
//!
//! Two rules shape what it produces:
//!
//! - **As published wherever possible.** An artifact that reaches the fleet unaltered is one whose
//!   SHA-256 an operator can compare against the release page, and ADR-0018's line from author to
//!   host stays unbroken. Every artifact is verified against the checksum *upstream* published
//!   before anything else happens to it.
//! - **Repacked only where it must be.** The GLPI Agent has no self-contained Linux archive — its
//!   `.tar.gz` is source — so its AppImage is extracted and repacked deterministically (ADR-0064).
//!   That is the one place the bytes change, and the tool says so.
//!
//! Interactive by default; every prompt has a flag, so the same tool serves a pipeline:
//!   opamp-package-fetch                                   # asks for everything
//!   opamp-package-fetch --agent glpi-agent --version 1.19 --platform linux/amd64 --no-upload

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dialoguer::{Confirm, Input, MultiSelect, Select};
use sha2::{Digest, Sha256};

/// How many versions the operator is offered. Enough to reach the release before last when one
/// turns out badly, few enough to read without scrolling.
const VERSIONS_SHOWN: usize = 5;

/// The member cap a tree package is held to (ADR-0023). Checked while packing rather than
/// discovered on three hundred hosts at rollout time.
const MAX_TREE_MEMBERS: usize = 10_000;

#[derive(Parser)]
#[command(
    name = "opamp-package-fetch",
    about = "Fetch an upstream agent release and turn it into fleet package artifacts"
)]
struct Cli {
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
    /// Upload to this Server when the artifacts are ready, e.g. `http://127.0.0.1:4321`.
    #[arg(long, value_name = "URL")]
    server: Option<String>,
    /// Write the artifacts and stop — no upload, and no question about one.
    #[arg(long, conflicts_with = "server")]
    no_upload: bool,
    /// Which vendor build to repack, by distribution codename (`bookworm`, `bullseye`). Icinga 2
    /// only. Omitted, it is this host's own — the only one it can build for, since the tree
    /// bundles the libraries found here; naming another is how you get told that this is the wrong
    /// host for it. It is also the artifact's reach: build on the oldest distribution you serve,
    /// since glibc cannot travel and is backward compatible (ADR-0070, ADR-0071).
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
    /// The GLPI inventory agent.
    #[value(name = "glpi-agent")]
    GlpiAgent,
    /// Telegraf.
    #[value(name = "telegraf")]
    Telegraf,
    /// Icinga 2, repacked from the vendor's distribution packages (ADR-0070).
    #[value(name = "icinga2")]
    Icinga2,
}

/// Everything that differs between one upstream project and the next, in one place.
struct Source {
    /// The Agent type a Supervisor reports and a package Set is built for (ADR-0034) — also this
    /// tool's default package name.
    service_name: &'static str,
    /// The GitHub repository whose tags name the versions. Telegraf's binaries do not live there,
    /// but its versions do.
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
            AgentKind::GlpiAgent => Source {
                service_name: "glpi-agent",
                repo: "glpi-project/glpi-agent",
            },
            AgentKind::Telegraf => Source {
                service_name: "telegraf",
                repo: "influxdata/telegraf",
            },
            AgentKind::Icinga2 => Source {
                service_name: "icinga2",
                repo: "Icinga/icinga2",
            },
        }
    }

    /// The version a tag names, or `None` when the tag is not a release of *this* agent.
    ///
    /// Each project spells its tags its own way, and two of them carry tags that are not releases
    /// at all: the Collector repository tags its builder and supervisor alongside
    /// (`cmd/builder/v0.158.0`), and Telegraf keeps release candidates (`v1.21.0-rc1`). A pattern
    /// per source is what keeps those out of the list an operator picks from.
    fn version_of_tag(self, tag: &str) -> Option<String> {
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        match self {
            // `v0.158.0` — three numeric parts behind a `v`, and nothing else.
            AgentKind::Otelcol
            | AgentKind::OtelcolContrib
            | AgentKind::Telegraf
            | AgentKind::Icinga2 => {
                let rest = tag.strip_prefix('v')?;
                let parts: Vec<&str> = rest.split('.').collect();
                (parts.len() == 3 && parts.iter().all(|p| numeric(p))).then(|| rest.to_string())
            }
            // `1.19`, `1.7.1` — no `v`, two or three parts. `1.0-beta1` fails on the last part.
            AgentKind::GlpiAgent => {
                let parts: Vec<&str> = tag.split('.').collect();
                ((2..=3).contains(&parts.len()) && parts.iter().all(|p| numeric(p)))
                    .then(|| tag.to_string())
            }
        }
    }

    /// The tag a version came from — the reverse of [`version_of_tag`](Self::version_of_tag), for
    /// the `--version` flag, which takes the version an operator reads on the release page.
    fn tag_of_version(self, version: &str) -> String {
        match self {
            AgentKind::Otelcol
            | AgentKind::OtelcolContrib
            | AgentKind::Telegraf
            | AgentKind::Icinga2 => {
                format!("v{version}")
            }
            AgentKind::GlpiAgent => version.to_string(),
        }
    }

    /// The systems this agent can be fetched for, as the agent menu states them.
    ///
    /// A hint, not the plan: what is actually offered comes from the release itself a step later,
    /// and for Icinga 2 also from the host this tool runs on. It is here because the menu asks for
    /// an agent before any of that is known, and picking one only to be told that this host builds
    /// nothing for it is a round trip the operator should not have to make.
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
            AgentKind::GlpiAgent => {
                group_platforms([("linux", "amd64"), ("windows", "amd64")].into_iter())
            }
            AgentKind::Telegraf => group_platforms(
                TELEGRAF_PLATFORMS
                    .iter()
                    .map(|(os, arch, _, _)| (*os, *arch)),
            ),
            // This column answers one question for every agent alike — does it run on the hosts I
            // have — so Icinga 2 states its *reach*, and the reach is this host's (ADR-0070): the
            // tree bundles the libraries found here, so the artifact this run can produce is the
            // one for the distribution this run is on, and no other. Asking the host is therefore
            // not a convenience, it is the only way the line can be true of the build that follows.
            AgentKind::Icinga2 => icinga2_reach(host),
        }
    }

    /// Whether the release's assets have to be listed to plan the fetch. Telegraf's GitHub
    /// releases carry no assets at all — its binaries are on a CDN, at a URL built from the
    /// version — so listing them would be a wasted request against a rate-limited API.
    fn needs_assets(self) -> bool {
        !matches!(self, AgentKind::Telegraf | AgentKind::Icinga2)
    }

    /// The Configurations this agent needs before it can do anything, aimed the way
    /// `scripts/seed_test_configs.sh` aims them — which is where these bodies come from, and why
    /// they agree with the `[[supervisor]]` blocks in `config/client.toml` down to the file names.
    ///
    /// Icinga 2 gets two, because it reads one root file and includes the rest by name (ADR-0068).
    /// Its ticket and its parent's certificate are *not* here: both are per host, one is a secret,
    /// and neither has a sensible default — see `docs/manual/icinga2.md`.
    fn default_configurations(self) -> &'static [DefaultConfiguration] {
        // Aiming differs by how an Agent's type becomes known. The Collectors report a
        // `service.name` of their own, so a Selector on that attribute is what reaches them; the
        // rest are matched by Agent type (ADR-0054).
        match self {
            AgentKind::Otelcol => &[DefaultConfiguration {
                name: "otelcol-conf",
                selector: &[("service.name", "otelcol")],
                service_name: "",
                body: include_str!("../../../../config/examples/otelcol-conf.yaml"),
            }],
            AgentKind::OtelcolContrib => &[DefaultConfiguration {
                name: "otelcol-contrib-conf",
                selector: &[("service.name", "otelcol-contrib")],
                service_name: "",
                body: include_str!("../../../../config/examples/otelcol-contrib-conf.yaml"),
            }],
            AgentKind::GlpiAgent => &[DefaultConfiguration {
                name: "glpi-agent-conf",
                selector: &[],
                service_name: "glpi-agent",
                body: include_str!("../../../../config/examples/glpi-agent-conf.cfg"),
            }],
            AgentKind::Telegraf => &[DefaultConfiguration {
                name: "telegraf-conf",
                selector: &[],
                service_name: "telegraf",
                body: include_str!("../../../../config/examples/telegraf-conf.toml"),
            }],
            AgentKind::Icinga2 => &[
                DefaultConfiguration {
                    name: "icinga2-conf",
                    selector: &[],
                    service_name: "icinga2",
                    body: include_str!("../../../../config/examples/icinga2-conf.conf"),
                },
                DefaultConfiguration {
                    name: "icinga2-zones",
                    selector: &[],
                    service_name: "icinga2",
                    body: include_str!("../../../../config/examples/icinga2-zones.conf"),
                },
            ],
        }
    }
}

/// A Configuration this tool puts on the Server beside the package, when the Server has none of
/// that name yet.
///
/// The body is compiled in rather than read from the repository: this tool is released as a
/// binary an operator runs anywhere (ADR-0065), so a file path beside it would be a default that
/// only works in a checkout.
struct DefaultConfiguration {
    /// The Configuration's name — also the file name its entry gets in the Supervisor's config
    /// directory, which is what the `[[supervisor]]` block has to name.
    name: &'static str,
    /// Equality pairs against the Agent's attributes; empty aims at every Agent of the type below.
    selector: &'static [(&'static str, &'static str)],
    /// The Agent type this is for (ADR-0054); empty is every type.
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
    /// The platform as *this fleet* names it (ADR-0031), which is what the upload path carries.
    os: String,
    arch: String,
    /// What has to be fetched. Usually one file; a repacked Icinga 2 tree needs the vendor's
    /// binary package *and* the one holding the ITL, so this is a list (ADR-0070). Every one of
    /// them is verified before any of them is used.
    sources: Vec<Download>,
    /// What has to happen to the downloaded bytes before they are a package artifact.
    action: Action,
    /// The artifact's file name in `--out-dir`.
    out_name: String,
    /// What a `[[supervisor]]` block has to say to install it — printed at the end, because the
    /// answer differs per agent and platform and is the next thing an operator needs.
    block_hint: String,
}

/// Where an artifact's published SHA-256 is found. Every source publishes one; no two publish it
/// the same way, and the Collector changed its mind at 0.158.0.
enum ChecksumSource {
    /// Files holding `<hash>  <name>` lines, `sha256sum` style — the artifact's line is looked up
    /// by name. GLPI's combined file, Telegraf's `.DIGESTS`, the Collector's pre-0.158 files.
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
    /// field of the `Packages` index is where its hash comes from (ADR-0070).
    Known { sha256: String },
    /// No digest is published at all, and the file signs itself: an Authenticode-signed Windows
    /// artifact, verified against its own contents and **pinned to the publisher** named here
    /// (ADR-0072). Stronger than a digest from the same server, which an attacker holding that
    /// server could rewrite along with the file.
    Publisher { expected: &'static str },
}

/// What happens between the download and the artifact.
enum Action {
    /// Nothing. The artifact is uploaded exactly as upstream published it, so the hash the fleet
    /// verifies is the hash on the release page (ADR-0018).
    AsPublished,
    /// Extract the AppImage and repack the tree deterministically (ADR-0064) — the one case where
    /// upstream publishes no archive a Client can install.
    RepackAppImage { wrapper: String },
    /// Unpack the Windows MSI's payload into the same normalised shape and pack that (ADR-0070).
    /// No libraries are gathered: the payload already carries its DLLs beside the executable,
    /// which is where Windows looks first.
    RepackMsi { wrapper: String },
    /// Unpack the vendor's Debian packages into one normalised, link-free tree and pack that
    /// (ADR-0070): Icinga 2 publishes distribution packages and an MSI, and no portable tree.
    ///
    /// `dependencies` is what the vendor package itself declares it needs. The tree bundles what
    /// `ldd` resolves on the build host, so a host missing one of them cannot produce a complete
    /// artifact — and the refusal can then name the command that fixes it rather than the problem
    /// alone.
    RepackDebs {
        wrapper: String,
        dependencies: Vec<String>,
    },
}

/// One thread: this tool is a sequence of downloads with a question between them, and the prompts
/// block by nature — there is nothing for a multi-threaded runtime to overlap.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // reqwest is built with `rustls-no-provider`, which refuses to construct a TLS client until a
    // process-wide provider exists (ADR-0007).
    client::tls::install_ring_provider();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let agent = match cli.agent {
        Some(agent) => agent,
        None => choose_agent()?,
    };
    let source = agent.source();

    let version = match cli.version.clone() {
        Some(version) => version,
        None => choose_version(agent, &source).await?,
    };
    let tag = agent.tag_of_version(&version);

    let assets = if agent.needs_assets() {
        release_assets(&source, &tag).await?
    } else {
        Vec::new()
    };
    let available = plans(agent, &version, &assets, cli.distro.as_deref()).await?;
    if available.is_empty() {
        return Err(format!(
            "{} {version} publishes nothing this tool can install",
            source.service_name
        ));
    }
    let chosen = select_platforms(&available, &cli.platforms)?;

    std::fs::create_dir_all(&cli.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", cli.out_dir.display()))?;
    let mut produced: Vec<(&Plan, PathBuf)> = Vec::new();
    for plan in &chosen {
        let artifact = produce(plan, &cli.out_dir).await?;
        produced.push((plan, artifact));
    }

    let server = upload_target(&cli)?;
    if let Some(server) = &server {
        for (plan, artifact) in &produced {
            upload(server, source.service_name, &version, plan, artifact).await?;
        }
        upload_default_configurations(server, agent).await?;
    }

    eprintln!("\nDone. What a Supervisor needs to install these:");
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
            "\nNothing was uploaded. To do it later, create the Set and PUT each artifact:\n  \
             curl -X PUT -H 'Content-Type: application/json' -d '{{}}' \\\n       \
             <server>/api/v1/packages/{name}/{name}/{version}\n  \
             curl -X PUT --data-binary @<artifact> \\\n       \
             \"<server>/api/v1/packages/{name}/{name}/{version}/entries/<os>/<arch>\"",
            name = source.service_name,
            version = version
        );
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
    Ok(())
}

// ── The questions ───────────────────────────────────────────────────────────

/// What a Linux artifact built on one Debian host reaches, by that host's codename.
///
/// Each row is the vendor's own `libc6 (>= …)` floor for that build, read back as the systems it
/// covers — glibc is backward compatible, so a floor is the whole of it and the distribution
/// family is none of it (ADR-0071). The floors are Icinga's, from the `Depends` of the
/// `icinga2-bin` each build publishes, and they are what this tool prints for real once the
/// repository index has been read (`icinga2_plans`).
///
/// The table is small because the source of truth is not it: it translates a floor into the names
/// an operator recognises, one row per distribution Icinga publishes for. `--distro` accepts the
/// same three, and the test below holds the two lists together.
const ICINGA2_REACH: &[(&str, &str)] = &[
    // libc6 >= 2.30 — Debian 11 is 2.31, Ubuntu 20.04 is 2.31, RHEL 9 is 2.34. RHEL 8 (2.28) is
    // the one this misses, and no build of Icinga's reaches it.
    ("bullseye", "Debian 11+/Ubuntu 20.04+/RHEL 9+"),
    // libc6 >= 2.34 — Ubuntu 22.04 is 2.35, RHEL 9 is 2.34 exactly.
    ("bookworm", "Debian 12+/Ubuntu 22.04+/RHEL 9+"),
    // libc6 >= 2.38 — Ubuntu 24.04 is 2.39, RHEL 10 is 2.39. RHEL 9 drops out here.
    ("trixie", "Debian 13+/Ubuntu 24.04+/RHEL 10+"),
];

/// Icinga 2's line in the agent menu, for the host this tool is running on.
///
/// A host that is none of the three builds no Linux artifact — the repack needs vendor packages
/// for *this* distribution — so the line says which hosts do rather than naming a reach that this
/// run cannot deliver. That is the one case where a codename belongs in this column: it is then a
/// statement about the build host, which is then the operator's actual problem.
fn icinga2_reach(host: Option<&str>) -> String {
    match host.and_then(|codename| {
        ICINGA2_REACH
            .iter()
            .find(|(name, _)| *name == codename)
            .map(|(_, reach)| *reach)
    }) {
        Some(reach) => format!("linux/amd64 ({reach}) windows/amd64"),
        None => "windows/amd64 — linux needs a bullseye/bookworm/trixie host".to_string(),
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
    // Asked of clap rather than written out again. A second list of the same agents is exactly how
    // `icinga2` came to be missing from this prompt while `--agent icinga2` worked: adding a
    // variant is one place, and the menu now follows it whether or not anyone remembers this
    // function.
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
        .map(|p| match p.action {
            Action::AsPublished => format!("{}/{}", p.os, p.arch),
            Action::RepackAppImage { .. }
            | Action::RepackDebs { .. }
            | Action::RepackMsi { .. } => {
                format!("{}/{} (repacked)", p.os, p.arch)
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
fn upload_target(cli: &Cli) -> Result<Option<String>, String> {
    if cli.no_upload {
        return Ok(None);
    }
    if let Some(server) = &cli.server {
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
        .default("http://127.0.0.1:4321".to_string())
        .interact_text()
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    Ok(Some(url.trim_end_matches('/').to_string()))
}

// ── Upstream ────────────────────────────────────────────────────────────────

/// The most recent [`VERSIONS_SHOWN`] release versions, newest first.
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
    versions.truncate(VERSIONS_SHOWN);
    Ok(versions)
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
async fn plans(
    agent: AgentKind,
    version: &str,
    assets: &[(String, String)],
    distro: Option<&str>,
) -> Result<Vec<Plan>, String> {
    match agent {
        AgentKind::Otelcol | AgentKind::OtelcolContrib => {
            Ok(collector_plans(agent, version, assets))
        }
        AgentKind::GlpiAgent => Ok(glpi_plans(version, assets)),
        AgentKind::Telegraf => Ok(telegraf_plans(version)),
        AgentKind::Icinga2 => {
            // Omitted, it is *this host* — the only distribution it can build for, since the tree
            // carries the libraries found here (ADR-0070). Asking a question with one possible
            // answer would be theatre; stating which build is being made is not.
            let distro = match distro {
                Some(distro) => {
                    same_distro(distro)?;
                    Some(distro.to_string())
                }
                None => match host_codename() {
                    Ok(here) => {
                        eprintln!(
                            "  building the {here} artifact, because that is what this host is"
                        );
                        Some(here)
                    }
                    // The Windows artifact needs no Debian host, so a host that is not one is a
                    // reason to offer less rather than to refuse everything.
                    Err(e) => {
                        eprintln!("  no Linux artifact from this host: {e}");
                        None
                    }
                },
            };
            icinga2_plans(version, distro.as_deref()).await
        }
    }
}

/// Refuses to build for a distribution this host is not.
///
/// The tree carries the libraries `ldd` finds *here*, so building bookworm's artifact on trixie
/// would bundle trixie's — an artifact that runs on neither reliably. The check is the host's own
/// `/etc/os-release`, and the answer is a container of the right distribution (ADR-0070).
fn same_distro(distro: &str) -> Result<(), String> {
    let codename = host_codename()?;
    if codename == distro {
        return Ok(());
    }
    Err(format!(
        "this host is {codename:?}, so it cannot build the {distro:?} artifact: the tree carries \
         the libraries found here, and they would be {codename}'s. Run this in a {distro} container."
    ))
}

/// What distribution this host is, by the codename its own `/etc/os-release` states.
fn host_codename() -> Result<String, String> {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .map_err(|e| format!("cannot read /etc/os-release, so the build host is unknown: {e}"))?;
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

/// Where the *check plugins* come from. They are not Icinga's to publish — `monitoring-plugins` is
/// its own project, packaged by the distribution — but an Icinga Agent without `check_disk` and its
/// siblings can barely check anything, so the artifact carries them (ADR-0070's "(+ plugins)").
const DEBIAN_REPO: &str = "https://deb.debian.org/debian";

/// What Icinga 2 offers for one distribution build, read out of that repository's own index.
///
/// Two packages make one tree: `icinga2-bin` carries the daemon, `icinga2-common` the ITL. Their
/// SHA-256 comes from the `Packages` index — Icinga signs repositories with GPG rather than
/// publishing per-file checksums, and the index is where the digests live (ADR-0070).
///
/// Only `linux/amd64` for now, and deliberately: an artifact's reach is decided by the glibc of
/// the host it was built on, so each build is its own act rather than a loop over architectures
/// this host cannot produce.
async fn icinga2_plans(version: &str, distro: Option<&str>) -> Result<Vec<Plan>, String> {
    let mut plans = vec![windows_plan(version)];
    let Some(distro) = distro else {
        return Ok(plans);
    };
    let index = format!("{ICINGA_REPO}/dists/icinga-{distro}/main/binary-amd64/Packages.gz");
    eprintln!("  reading {index} …");
    let packages = deb_index(&index).await?;
    let mut sources = Vec::new();
    // Every package that makes the tree states what *it* needs, and the tree bundles for all of
    // them — the daemon and each plugin alike. A hint covering only the daemon's would send an
    // operator round the loop again for the first plugin that links something else.
    let mut dependencies: Vec<String> = Vec::new();
    for package in ["icinga2-bin", "icinga2-common"] {
        let entry = packages
            .iter()
            .find(|entry| {
                entry.package == package && entry.version.starts_with(&format!("{version}-"))
            })
            .ok_or_else(|| {
                format!("{distro} publishes no {package} {version} — check the version and the distribution")
            })?;
        dependencies.extend(package_names(&entry.depends));
        sources.push(Download {
            url: format!("{ICINGA_REPO}/{}", entry.filename),
            checksum: ChecksumSource::Known {
                sha256: entry.sha256.clone(),
            },
        });
    }

    // The check plugins, from the distribution rather than from Icinga: `-basic` is check_disk,
    // check_load, check_procs and the rest, `-common` the helpers several of them source at run
    // time. Whatever version that distribution ships — they are not versioned with Icinga.
    let distro_index = format!("{DEBIAN_REPO}/dists/{distro}/main/binary-amd64/Packages.gz");
    eprintln!("  reading {distro_index} …");
    let distro_packages = deb_index(&distro_index).await?;
    for package in ["monitoring-plugins-basic", "monitoring-plugins-common"] {
        let entry = distro_packages
            .iter()
            .find(|entry| entry.package == package)
            .ok_or_else(|| format!("{distro} publishes no {package}"))?;
        dependencies.extend(package_names(&entry.depends));
        sources.push(Download {
            url: format!("{DEBIAN_REPO}/{}", entry.filename),
            checksum: ChecksumSource::Known {
                sha256: entry.sha256.clone(),
            },
        });
    }
    dependencies.sort();
    dependencies.dedup();
    // The vendor's own statement about how old a libc may be. It is the artifact's reach, and it
    // belongs in front of an operator before the rollout rather than after it (ADR-0070).
    if let Some(floor) = packages
        .iter()
        .find(|e| e.package == "icinga2-bin" && e.version.starts_with(&format!("{version}-")))
        .and_then(|e| libc_floor(&e.depends))
    {
        eprintln!("  this build needs glibc >= {floor} on every host it is rolled out to");
    }
    plans.push(Plan {
        os: "linux".to_string(),
        arch: "amd64".to_string(),
        sources,
        action: Action::RepackDebs {
            wrapper: format!("icinga2-{version}"),
            dependencies,
        },
        out_name: format!("icinga2_{version}_linux_amd64.tar.gz"),
        block_hint: "type = \"icinga2\", binary = \"icinga2\", program_path = \"sbin/icinga2\""
            .to_string(),
    });
    Ok(plans)
}

/// Where Icinga publishes the Windows installer, and who signs it (ADR-0072).
const ICINGA_WINDOWS: &str = "https://packages.icinga.com/windows";
const ICINGA_PUBLISHER: &str = "O=Icinga GmbH";

/// The Windows artifact: the MSI's payload, repacked into the same shape as the Linux tree.
///
/// It needs no repository index and no library gathering — the payload carries its own DLLs beside
/// the executable — but it publishes no digest either, so it is verified by its signature instead.
fn windows_plan(version: &str) -> Plan {
    Plan {
        os: "windows".to_string(),
        arch: "amd64".to_string(),
        sources: vec![Download {
            url: format!("{ICINGA_WINDOWS}/Icinga2-v{version}-x86_64.msi"),
            checksum: ChecksumSource::Publisher {
                expected: ICINGA_PUBLISHER,
            },
        }],
        action: Action::RepackMsi {
            wrapper: format!("icinga2-{version}"),
        },
        out_name: format!("icinga2_{version}_windows_amd64.tar.gz"),
        // The check plugins stay beside the daemon rather than moving to `plugins/`: on Windows a
        // program finds its DLLs in its *own* directory first, and separating the check
        // executables from the runtime they share with the daemon would break them.
        block_hint:
            "type = \"icinga2\", binary = \"icinga2.exe\", program_path = \"sbin/icinga2.exe\", \
                     plugin_dir = \"${supervisor_dir}/program/tree/sbin\""
                .to_string(),
    }
}

/// One stanza of a Debian `Packages` index — the fields this tool needs and no others.
struct DebEntry {
    package: String,
    version: String,
    filename: String,
    sha256: String,
    depends: String,
}

/// Reads and parses a gzipped `Packages` index. RFC 822-ish stanzas separated by blank lines; only
/// the single-line fields matter here, so continuation lines are simply not fields.
async fn deb_index(url: &str) -> Result<Vec<DebEntry>, String> {
    let compressed = get_bytes(url).await?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read {url}: {e}"))?;
    Ok(parse_deb_index(&text))
}

/// The parse of [`deb_index`], separated from the fetch so it can be tested against a stanza
/// rather than against the network.
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
            package,
            version: field("Version"),
            filename: field("Filename"),
            sha256: field("SHA256"),
            depends: field("Depends"),
        });
    }
    entries
}

/// The package names of a `Depends` field, without version constraints and taking the first of
/// any alternatives — what an operator would hand to `apt-get install`.
fn package_names(depends: &str) -> Vec<String> {
    depends
        .split(',')
        .filter_map(|dep| {
            let first = dep.split('|').next()?.trim();
            let name = first.split_whitespace().next()?;
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// The `libc6 (>= 2.34)` out of a `Depends` field — the oldest glibc this build runs on.
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
        let Some(rest) = name
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".tar.gz"))
        else {
            continue;
        };
        let Some((os, arch)) = rest.split_once('_') else {
            continue;
        };
        let (Some(os), Some(arch)) = (normalize_os(os), normalize_arch(arch)) else {
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

/// GLPI publishes two self-contained builds: a portable zip for Windows, which travels as
/// published, and an AppImage for Linux, which is repacked (ADR-0064). Everything else in the
/// release is an installer for a machine to run, not an artifact a fleet installs.
fn glpi_plans(version: &str, assets: &[(String, String)]) -> Vec<Plan> {
    let sums: Option<String> = assets
        .iter()
        .find(|(name, _)| *name == format!("glpi-agent-{version}.sha256"))
        .map(|(_, url)| url.clone());
    let mut plans = Vec::new();

    // The zip's name changed case at 1.9 (`glpi-agent-1.8-x64.zip` → `GLPI-Agent-1.9-x64.zip`),
    // so it is matched case-insensitively rather than by a name that is right for some releases.
    if let Some((name, url)) = assets.iter().find(|(name, _)| {
        let lower = name.to_ascii_lowercase();
        lower == format!("glpi-agent-{version}-x64.zip")
    }) {
        plans.push(Plan {
            os: "windows".to_string(),
            arch: "amd64".to_string(),
            sources: vec![Download {
                url: url.clone(),
                checksum: ChecksumSource::Sums {
                    urls: sums.clone().into_iter().collect(),
                },
            }],
            action: Action::AsPublished,
            out_name: name.clone(),
            block_hint: "command = \"glpi-agent.exe\", program_path = \"perl/bin/glpi-agent.exe\""
                .to_string(),
        });
    }

    if let Some((_, url)) = assets
        .iter()
        .find(|(name, _)| *name == format!("glpi-agent-{version}-x86_64.AppImage"))
    {
        plans.push(Plan {
            os: "linux".to_string(),
            arch: "amd64".to_string(),
            sources: vec![Download {
                url: url.clone(),
                checksum: ChecksumSource::Sums {
                    urls: sums.into_iter().collect(),
                },
            }],
            action: Action::RepackAppImage {
                wrapper: format!("glpi-agent-{version}"),
            },
            out_name: format!("glpi-agent_{version}_linux_amd64.tar.gz"),
            block_hint: "command = \"AppRun\", program_path = \"AppRun\", args = [\"--script=glpi-agent\", …]"
                .to_string(),
        });
    }
    plans
}

/// Telegraf's platforms, which cannot be read from anywhere: its GitHub releases carry no assets,
/// and the CDN that holds the binaries serves no directory listing. The list is therefore this
/// tool's own — kept to the platforms this fleet has a name for — and a URL that has gone away is
/// reported as the 404 it is rather than hidden.
///
/// Each entry is (fleet os, fleet arch, upstream os, upstream arch). It sits at file scope because
/// the agent menu names these same platforms ([`AgentKind::platforms`]), and a second copy of the
/// list is a second thing to keep current.
const TELEGRAF_PLATFORMS: [(&str, &str, &str, &str); 7] = [
    ("linux", "amd64", "linux", "amd64"),
    ("linux", "arm64", "linux", "arm64"),
    ("linux", "386", "linux", "i386"),
    ("darwin", "amd64", "darwin", "amd64"),
    ("darwin", "arm64", "darwin", "arm64"),
    ("windows", "amd64", "windows", "amd64"),
    ("windows", "arm64", "windows", "arm64"),
];

/// What Telegraf offers, built from that list — the release itself is never asked.
fn telegraf_plans(version: &str) -> Vec<Plan> {
    TELEGRAF_PLATFORMS
        .iter()
        .map(|(os, arch, up_os, up_arch)| {
            let ext = if *os == "windows" { "zip" } else { "tar.gz" };
            let name = format!("telegraf-{version}_{up_os}_{up_arch}.{ext}");
            let url = format!("https://dl.influxdata.com/telegraf/releases/{name}");
            // The archive wraps everything in a version-named directory, and the program sits at
            // `usr/bin/telegraf` on Unix but at the root on Windows. Neither matters for a
            // single-file package: the Client finds the member by its *file name*, so the upstream
            // archive installs as it is.
            let program = if *os == "windows" {
                "telegraf.exe"
            } else {
                "telegraf"
            };
            Plan {
                os: (*os).to_string(),
                arch: (*arch).to_string(),
                sources: vec![Download {
                    checksum: ChecksumSource::Sums {
                        urls: vec![format!("{url}.DIGESTS")],
                    },
                    url,
                }],
                action: Action::AsPublished,
                out_name: name,
                block_hint: format!("command = {program:?}"),
            }
        })
        .collect()
}

/// Upstream's platform words in this fleet's vocabulary (ADR-0031), or `None` for one this fleet
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

        match &source.checksum {
            ChecksumSource::Publisher { expected } => {
                verify_publisher(&downloaded, expected)?;
            }
            checksum => {
                let published = published_sha256(checksum, &downloaded).await?;
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
            }
        }
        fetched.push(downloaded);
    }
    // Every plan but Icinga's has exactly one; the ones that act on "the download" mean this.
    let downloaded = fetched
        .first()
        .cloned()
        .ok_or_else(|| "a plan with nothing to fetch".to_string())?;

    match &plan.action {
        Action::AsPublished => {
            eprintln!("  as published — the fleet verifies upstream's own hash");
            Ok(downloaded)
        }
        Action::RepackMsi { wrapper } => {
            let artifact = out_dir.join(&plan.out_name);
            repack_msi(&downloaded, wrapper, &artifact)?;
            let _ = std::fs::remove_file(&downloaded);
            eprintln!(
                "  repacked  sha256 {}",
                hex::encode(sha256_file(&artifact)?)
            );
            Ok(artifact)
        }
        Action::RepackDebs {
            wrapper,
            dependencies,
        } => {
            let artifact = out_dir.join(&plan.out_name);
            repack_debs(&fetched, wrapper, dependencies, &artifact)?;
            // The vendor packages were a means, not a result.
            for deb in &fetched {
                let _ = std::fs::remove_file(deb);
            }
            eprintln!(
                "  repacked  sha256 {}",
                hex::encode(sha256_file(&artifact)?)
            );
            Ok(artifact)
        }
        Action::RepackAppImage { wrapper } => {
            let artifact = out_dir.join(&plan.out_name);
            repack_appimage(&downloaded, wrapper, &artifact)?;
            // The AppImage was a means, not a result; leaving it beside the artifact invites
            // uploading the wrong file.
            let _ = std::fs::remove_file(&downloaded);
            eprintln!(
                "  repacked  sha256 {}",
                hex::encode(sha256_file(&artifact)?)
            );
            Ok(artifact)
        }
    }
}

/// Verifies an Authenticode-signed file and that it was signed by `expected` (ADR-0072).
///
/// Two conditions, both required: the signature verifies against the file's own contents, and the
/// signer's subject names the publisher this agent expects. What is deliberately *not* required is
/// a chain to a trusted root — a Linux CA bundle carries web PKI roots, not the code-signing roots
/// Windows trusts, so demanding it would fail on the build host rather than on the artifact. The
/// answer names the signer, so what was proved is visible rather than implied.
fn verify_publisher(artifact: &Path, expected: &str) -> Result<String, String> {
    let output = std::process::Command::new("osslsigncode")
        .arg("verify")
        .arg(artifact)
        .output()
        .map_err(|e| {
            format!("cannot run osslsigncode, which verifies the Windows artifact's signature: {e}")
        })?;
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let signer = publisher_verdict(&report, expected)
        .map_err(|reason| format!("{}: {reason}", artifact.display()))?;
    eprintln!("  verified the signature of {signer}");
    // Said rather than glossed: the signature binds the bytes to a key, and this host cannot say
    // which certificate authority vouches for that key (ADR-0072).
    eprintln!("  (the issuing chain is not validated on this host)");
    Ok(signer)
}

/// The verdict on what `osslsigncode verify` reported — the part worth testing, kept away from the
/// process it came from.
///
/// Two conditions, both required: at least one signature verified against the file's contents, and
/// the signer's subject names the expected publisher. Note what is *not* read: the tool's own
/// overall "Succeeded/Failed", which says Failed on a host without Authenticode roots even when the
/// signature itself is sound (ADR-0072).
fn publisher_verdict(report: &str, expected: &str) -> Result<String, String> {
    let verified = report
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Number of verified signatures:"))
        .filter_map(|count| count.trim().parse::<u32>().ok())
        .any(|count| count >= 1);
    if !verified {
        return Err(
            "carries no signature that verifies against its contents — refusing to repack \
                    bytes nobody vouched for"
                .to_string(),
        );
    }
    let signer = report
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Subject: "))
        .find(|subject| !subject.is_empty())
        .unwrap_or_default()
        .to_string();
    if !signer.contains(expected) {
        return Err(format!(
            "is signed by {signer:?}, not by {expected:?} — refusing to repack it"
        ));
    }
    Ok(signer)
}

/// The SHA-256 upstream published for this artifact, from whichever form the source uses.
async fn published_sha256(source: &ChecksumSource, artifact: &Path) -> Result<String, String> {
    let urls = match source {
        // Already read out of a repository index (ADR-0070) — there is nothing to fetch.
        ChecksumSource::Known { sha256 } => return Ok(sha256.clone()),
        // Verified by its own signature instead, before this is ever reached (ADR-0072).
        ChecksumSource::Publisher { .. } => {
            return Err("this source is verified by its signature, not by a digest".to_string())
        }
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

/// Extracts the AppImage and packs the tree it holds (ADR-0064).
///
/// The AppImage is upstream's only self-contained Linux build, and extracting it here is what
/// spares every fleet host the FUSE dependency — or the re-extraction on every start that avoiding
/// FUSE otherwise costs. Extraction runs the AppImage's own runtime, so this step is Linux x86_64
/// only; every other step of this tool runs anywhere.
fn repack_appimage(appimage: &Path, wrapper: &str, out: &Path) -> Result<(), String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(
            "extracting an AppImage runs it, so this repack needs Linux x86_64 — fetch the \
             Windows artifact here and the Linux one on a Linux host"
                .to_string(),
        );
    }
    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(appimage, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", appimage.display()))?;
    }
    eprintln!("  extracting the AppImage …");
    let status = std::process::Command::new(
        appimage
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", appimage.display()))?,
    )
    .arg("--appimage-extract")
    .current_dir(&staging)
    .stdout(std::process::Stdio::null())
    .status()
    .map_err(|e| format!("cannot run the AppImage: {e}"))?;
    if !status.success() {
        return Err("--appimage-extract failed".to_string());
    }

    let tree = staging.join("squashfs-root");
    // The desktop icon is a link to itself by another name, and the Debian packaging the AppImage
    // is built from leaves links pointing at files no tree carries. Neither survives a package
    // (ADR-0023), and neither is missed.
    let _ = std::fs::remove_file(tree.join(".DirIcon"));
    if !tree.join("AppRun").exists() {
        return Err("the extracted tree has no AppRun".to_string());
    }
    eprintln!("  packing the tree …");
    pack_tree(&tree, wrapper, out)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// Turns the Windows MSI's payload into the same normalised tree the Linux artifact has (ADR-0070).
///
/// Simpler than its Debian counterpart in two ways, and both are properties of the payload rather
/// than decisions taken here: the DLLs already sit beside `icinga2.exe`, which is the first place
/// Windows looks, so nothing is gathered; and the payload carries no links, so nothing has to be
/// dereferenced. What is left is the shape — `sbin/`, `share/icinga2/include/` — so that one block
/// reads the same on both platforms apart from the file extension.
///
/// The check plugins the MSI ships stay in `sbin/` beside the daemon. Moving them to `plugins/`
/// would separate them from the runtime DLLs they share with it, and a plugin that cannot start is
/// a check that never runs.
fn repack_msi(msi: &Path, wrapper: &str, out: &Path) -> Result<(), String> {
    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    let extracted = staging.join("extracted");
    std::fs::create_dir_all(&extracted)
        .map_err(|e| format!("cannot create {}: {e}", extracted.display()))?;

    // Quietly: msiextract prints every member it writes, and 134 file names between "downloading"
    // and "repacked" bury the two lines an operator is actually reading.
    let output = std::process::Command::new("msiextract")
        .arg("-C")
        .arg(&extracted)
        .arg(msi)
        .output()
        .map_err(|e| format!("cannot run msiextract (is msitools installed?): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "msiextract could not unpack {}: {}",
            msi.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // The payload puts everything under one directory named after the product.
    let root = find_dir(&extracted, "ICINGA2")?;
    let tree = staging.join("tree");
    copy_dir(&root.join("sbin"), &tree.join("sbin"))?;
    copy_dir(
        &root.join("share/icinga2/include"),
        &tree.join("share/icinga2/include"),
    )?;
    for name in ["LICENSE", "COPYING", "VERSION"] {
        let file = root.join(name);
        if file.is_file() {
            copy_file(&file, &tree.join("doc").join(name))?;
        }
    }

    pack_tree(&tree, wrapper, out)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// The directory of that name below `root`, wherever the payload put it.
fn find_dir(root: &Path, name: &str) -> Result<PathBuf, String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == name) {
                    return Ok(path);
                }
                stack.push(path);
            }
        }
    }
    Err(format!(
        "the MSI payload holds no {name} directory below {}",
        root.display()
    ))
}

/// Turns the vendor's Debian packages into one normalised, link-free tree (ADR-0070).
///
/// The layout is normalised rather than kept, so that **one** `program_path` serves every
/// distribution: Debian puts the binary under `/usr/lib/<triplet>/icinga2/sbin` and RHEL under
/// `/usr/lib64/icinga2/sbin`, and a block should not have to know which. What comes along is the
/// daemon, the ITL, the check plugin the package ships, the copyright files — repacking is
/// redistribution — and the shared libraries the daemon needs.
///
/// Left out on purpose: `/etc/icinga2` (the fleet delivers configuration), the systemd unit and
/// init script, and the `prepare-dirs`/`safe-reload` helpers, which need a `nagios` account that a
/// fleet-managed host does not have — the Supervisor does that work instead (ADR-0068).
fn repack_debs(
    debs: &[PathBuf],
    wrapper: &str,
    dependencies: &[String],
    out: &Path,
) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("repacking Debian packages needs dpkg-deb, so it runs on Linux".to_string());
    }
    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    let extracted = staging.join("extracted");
    let tree = staging.join("tree");
    std::fs::create_dir_all(&extracted)
        .map_err(|e| format!("cannot create {}: {e}", extracted.display()))?;

    for deb in debs {
        let status = std::process::Command::new("dpkg-deb")
            .arg("-x")
            .arg(deb)
            .arg(&extracted)
            .status()
            .map_err(|e| format!("cannot run dpkg-deb (is it installed?): {e}"))?;
        if !status.success() {
            return Err(format!("dpkg-deb could not unpack {}", deb.display()));
        }
        apply_alternatives(deb, &staging, &extracted)?;
    }

    for dir in ["sbin", "lib", "share/icinga2", "plugins", "doc"] {
        std::fs::create_dir_all(tree.join(dir))
            .map_err(|e| format!("cannot create {}: {e}", tree.join(dir).display()))?;
    }
    // The real binary, not `/usr/sbin/icinga2` — that is a shell wrapper, and what the Supervisor
    // spawns has to be the process it then watches (ADR-0063's lesson).
    let binary = find_file(&extracted.join("usr/lib"), "icinga2")?;
    copy_file(&binary, &tree.join("sbin/icinga2"))?;
    copy_dir(
        &extracted.join("usr/share/icinga2/include"),
        &tree.join("share/icinga2/include"),
    )?;
    let plugins = extracted.join("usr/lib/nagios/plugins");
    if plugins.is_dir() {
        copy_dir(&plugins, &tree.join("plugins"))?;
    }
    // Some plugins source these at run time (`utils.sh`, `utils.pm`); without them a check fails
    // with a message about a missing file rather than about what it was checking.
    for helper in ["usr/share/monitoring-plugins", "usr/lib/monitoring-plugins"] {
        let dir = extracted.join(helper);
        if dir.is_dir() {
            copy_dir(&dir, &tree.join("plugins"))?;
        }
    }
    // Repacking is redistribution, so the vendor's copyright files travel — and only those; the
    // changelogs are weight nobody unpacks on a monitored host (ADR-0070).
    for package in ["icinga2-bin", "icinga2-common"] {
        let copyright = extracted
            .join("usr/share/doc")
            .join(package)
            .join("copyright");
        if copyright.is_file() {
            copy_file(
                &copyright,
                &tree.join("doc").join(format!("{package}.copyright")),
            )?;
        }
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
    bundle_libraries(&programs, &tree.join("lib"), dependencies)?;

    pack_tree(&tree, wrapper, out)?;
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
        // The paths are the installed system's; here they name places inside the payload.
        let (from, to) = (
            extracted.join(target.trim_start_matches('/')),
            extracted.join(link.trim_start_matches('/')),
        );
        if from.is_file() && !to.exists() {
            copy_file(&from, &to)?;
            eprintln!(
                "  provided {} as the package's own installation would",
                link
            );
        }
    }
    Ok(())
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
        let entry = best.entry(name.to_string());
        match entry {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert((priority, link.to_string(), target.to_string()));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if priority > slot.get().0 {
                    slot.insert((priority, link.to_string(), target.to_string()));
                }
            }
        }
    }
    best.into_values()
        .map(|(_, link, target)| (link, target))
        .collect()
}

/// Copies the shared libraries the program needs beside it — **except glibc and its loader**.
///
/// Those two cannot travel: a libc without its matching loader does not work, and with it the
/// program would have to *be* the loader, which a Supervisor's program path cannot express. What
/// follows is the artifact's reach — it runs where the glibc is at least as new as this host's
/// (ADR-0070) — and the Supervisor points `LD_LIBRARY_PATH` at what lands here, which wins over
/// the binary's own RUNPATH.
fn bundle_libraries(
    programs: &[PathBuf],
    lib_dir: &Path,
    dependencies: &[String],
) -> Result<(), String> {
    let mut bundled = 0usize;
    for program in programs {
        bundled += bundle_one(program, lib_dir, dependencies)?;
    }
    eprintln!("  bundled {bundled} shared libraries");
    Ok(())
}

/// The closure of one program, minus what cannot travel. Answers how many libraries it added.
fn bundle_one(program: &Path, lib_dir: &Path, dependencies: &[String]) -> Result<usize, String> {
    let output = std::process::Command::new("ldd")
        .arg(program)
        .output()
        .map_err(|e| format!("cannot run ldd: {e}"))?;
    if !output.status.success() {
        // A shell script among the plugins is not an ELF file, and `ldd` says so — that is not an
        // error, it is a plugin that needs no libraries of its own.
        return Ok(0);
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    // A library the build host does not have is the one failure that would otherwise ship: `ldd`
    // reports it as "not found", the tree is packed without it, and the artifact dies on its first
    // start with a linker error. The vendor package's dependencies have to be installed *here*
    // before its tree can be built.
    let missing: Vec<&str> = listing
        .lines()
        .filter(|line| line.contains("not found"))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    if !missing.is_empty() {
        // The vendor states what it needs, so the refusal can name the command rather than the
        // problem alone. The tree carries what `ldd` resolves here, and what it cannot resolve
        // would be missing from every host this artifact reaches.
        let remedy = if dependencies.is_empty() {
            "install the vendor package's own dependencies on this host first".to_string()
        } else {
            format!(
                "install the vendor package's own dependencies first:\n      sudo apt-get install -y --no-install-recommends {}",
                dependencies.join(" ")
            )
        };
        return Err(format!(
            "the build host is missing {} the package needs: {}\n  {remedy}",
            if missing.len() == 1 {
                "a library"
            } else {
                "libraries"
            },
            missing.join(", ")
        ));
    }
    let mut bundled = 0usize;
    for line in listing.lines() {
        let Some(path) = line.split_whitespace().find(|word| word.starts_with('/')) else {
            continue;
        };
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let is_glibc = [
            "libc.so",
            "libm.so",
            "libdl.so",
            "libpthread.so",
            "librt.so",
        ]
        .iter()
        .any(|core| name.starts_with(core))
            || name.starts_with("ld-linux");
        if is_glibc {
            continue;
        }
        // Resolved, so a `.so.1` symlink becomes the file it points at: the Client refuses an
        // archive that carries a link at all (ADR-0023).
        // Already carried by an earlier program's closure: the daemon and the plugins share most
        // of theirs, and copying a file over itself is work nobody asked for.
        let target = lib_dir.join(&name);
        if target.exists() {
            continue;
        }
        copy_file(Path::new(path), &target)?;
        bundled += 1;
    }
    Ok(bundled)
}

/// The one file of that name below `root`, wherever the distribution decided to put it.
fn find_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return Ok(path);
            }
        }
    }
    Err(format!(
        "the vendor packages hold no {name} below {}",
        root.display()
    ))
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

/// Copies a directory, resolving links into the files they name — the tree must carry none
/// (ADR-0023).
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from)
        .map_err(|e| format!("cannot read {}: {e}", from.display()))?
        .flatten();
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for entry in entries {
        let path = entry.path();
        let target = to.join(entry.file_name());
        // `metadata` follows links, which is exactly what dereferencing means here.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => copy_dir(&path, &target)?,
            Ok(_) => copy_file(&path, &target)?,
            Err(_) => continue, // a dangling link names nothing to carry
        }
    }
    Ok(())
}

/// Packs a directory as a `.tar.gz` a Client can install as a tree — deterministically, and with
/// every link resolved.
///
/// Determinism is not tidiness: the fleet decides whether to distribute by content hash, so an
/// artifact that differed by when it was packed would be a rollout nobody asked for. Times, owners
/// and order are therefore fixed, exactly as `opamp-package-sign pack` fixes them for one file.
///
/// Links are resolved rather than carried because a tree package refuses them (ADR-0023): what a
/// link names is not where it sits, which is the one thing a path check cannot judge. A link
/// pointing at nothing is dropped.
fn pack_tree(root: &Path, wrapper: &str, out: &Path) -> Result<(), String> {
    let mut members = Vec::new();
    collect_members(root, Path::new(""), &mut members, &mut Vec::new())?;
    members.sort();
    if members.len() > MAX_TREE_MEMBERS {
        return Err(format!(
            "the tree holds {} members — past the {MAX_TREE_MEMBERS} a package may carry",
            members.len()
        ));
    }

    let file =
        std::fs::File::create(out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));
    for relative in &members {
        let full = root.join(relative);
        // Following the link is the point: `metadata` resolves, and an error means it resolves to
        // nothing, which is a member to drop rather than a failure to pack.
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        let inside = Path::new(wrapper).join(relative);
        let mut header = tar::Header::new_gnu();
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        if meta.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, &inside, std::io::empty())
                .map_err(|e| format!("cannot pack {}: {e}", inside.display()))?;
            continue;
        }
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(meta.len());
        header.set_mode(mode_of(&meta));
        header.set_cksum();
        let source = std::fs::File::open(&full)
            .map_err(|e| format!("cannot read {}: {e}", full.display()))?;
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

/// Every path under `root`, relative to it — directories included, because a tree keeps its shape.
///
/// Links are followed, and that is the whole difficulty. A link to a *directory* has to be
/// descended into and packed **at the name the link carries**, because that name is one the agent
/// uses: the GLPI AppImage reaches its Perl library through `usr/share/perl/5.26`, a link to
/// `5.26.1`, and a tree where that path is an empty directory is one whose agent cannot find a
/// single module. So the contents are packed under both names, exactly as `tar -h` writes them.
///
/// Following links makes a cycle possible, and the guard has to be the *ancestors* of the walk
/// rather than everywhere it has already been: a link pointing back up is a cycle and is dropped,
/// while a second path to the same directory is legitimate duplication and must be packed again.
/// Deduplicating by real path instead would leave one of the two names empty — and which one, on
/// directory-read order, which is nobody's guarantee.
fn collect_members(
    root: &Path,
    prefix: &Path,
    into: &mut Vec<PathBuf>,
    ancestors: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let dir = root.join(prefix);
    for entry in
        std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let relative = prefix.join(entry.file_name());
        let full = root.join(&relative);
        // Resolving rather than reading the entry itself: what matters is what the member *is*
        // once the link is followed, because that is what gets packed. An error means it resolves
        // to nothing — a dangling link, which is where the AppImage's Debian leftovers go.
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        into.push(relative.clone());
        if !meta.is_dir() {
            continue;
        }
        let real = full.canonicalize().unwrap_or_else(|_| full.clone());
        if ancestors.contains(&real) {
            continue;
        }
        ancestors.push(real);
        collect_members(root, &relative, into, ancestors)?;
        ancestors.pop();
    }
    Ok(())
}

/// The mode to store, which off Unix is a decision rather than a reading: a tar written on Windows
/// has no mode to carry, and a tree whose programs arrive unexecutable is a tree that will not run.
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o755
    }
}

// ── The Server ──────────────────────────────────────────────────────────────

/// Creates the Set if it is not there and stores this platform's artifact as its entry
/// (ADR-0052). Nothing is distributed by either call: a Set reaches an Agent only through a
/// rollout act (ADR-0061), which stays the operator's.
async fn upload(
    server: &str,
    service_name: &str,
    version: &str,
    plan: &Plan,
    artifact: &Path,
) -> Result<(), String> {
    let set = format!("{server}/api/v1/packages/{service_name}/{service_name}/{version}");
    eprintln!("  creating the Set at {set} …");
    let response = http()?
        .put(&set)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("cannot reach {set}: {}", because(&e)))?;
    expect_ok(response, &set).await?;

    let entry = format!("{set}/entries/{}/{}", plan.os, plan.arch);
    eprintln!("  uploading {} …", artifact.display());
    let bytes =
        std::fs::read(artifact).map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
    let response = match http()?.put(&entry).body(bytes).send().await {
        Ok(response) => response,
        Err(e) => return Err(refusal_behind(&entry, &e).await),
    };
    expect_ok(response, &entry).await?;
    eprintln!("  stored as the {}/{} entry — not distributed: press the rollout when it should reach hosts", plan.os, plan.arch);
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
/// all. And **nothing is distributed**: saving only saves (ADR-0061), so the default reaches no
/// Agent until an operator reads it and presses the rollout. That matters here, because these
/// bodies are starting points with example values in them — a parent host to correct, a plugin
/// path to confirm.
async fn upload_default_configurations(server: &str, agent: AgentKind) -> Result<(), String> {
    for config in agent.default_configurations() {
        let url = format!("{server}/api/v1/configurations/{}", config.name);
        if configuration_exists(&url).await? {
            eprintln!("  {} is already on the Server — left as it is", config.name);
            continue;
        }
        let selector: serde_json::Map<String, serde_json::Value> = config
            .selector
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    serde_json::Value::String((*v).to_string()),
                )
            })
            .collect();
        let spec = serde_json::json!({
            "selector": selector,
            "body": config.body,
            "service_name": config.service_name,
        });
        eprintln!("  storing the default configuration {} …", config.name);
        let response = http()?
            .put(&url)
            .header("Content-Type", "application/json")
            .body(spec.to_string())
            .send()
            .await
            .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
        expect_ok(response, &url).await?;
        eprintln!(
            "  saved — read it over and press its rollout when it should reach hosts, since it \
             carries example values"
        );
    }
    Ok(())
}

/// Whether the Server already holds a Configuration of this name. `404` is the answer that means
/// "no"; anything else that is not a success is an answer this tool must not read past — a Server
/// that refuses the question would otherwise have a default written over the top of what it holds.
async fn configuration_exists(url: &str) -> Result<bool, String> {
    let response = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
    match response.status() {
        status if status.is_success() => Ok(true),
        reqwest::StatusCode::NOT_FOUND => Ok(false),
        status => {
            let body = response.text().await.unwrap_or_default();
            Err(format!("{url} answered {status}: {}", body.trim()))
        }
    }
}

/// Why an upload never came back with a status.
///
/// The Server refuses some uploads *before* it reads a byte of the body — an identity nobody
/// created, a Set already assigned and therefore immutable (ADR-0061), a store at its ceiling
/// (ADR-0015). With hundreds of megabytes already in flight and no `Expect: 100-continue` to hold
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
async fn refusal_behind(entry: &str, error: &reqwest::Error) -> String {
    let sent = match http() {
        Ok(client) => client.put(entry).body(Vec::new()).send().await,
        Err(e) => return e,
    };
    if let Ok(response) = sent {
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
    reqwest::Client::builder()
        // An artifact is hundreds of megabytes over someone else's CDN; a whole-request timeout
        // would kill a working download on principle, so only the connect phase is bounded.
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent("opamp-package-fetch")
        .build()
        .map_err(|e| format!("cannot build an HTTP client: {e}"))
}

async fn get(url: &str) -> Result<reqwest::Response, String> {
    let response = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    Ok(response)
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

fn sha256_file(path: &Path) -> Result<Vec<u8>, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(hasher.finalize().to_vec())
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    /// The agent menu names the systems each agent can be fetched for, and the one that needs it
    /// most is Icinga 2: its Linux artifact exists only on a host of the distribution it is built
    /// for, which is not something to discover three questions later. Every variant answers —
    /// adding one without a line here is the omission this catches — and Telegraf's answer is the
    /// table `telegraf_plans` builds from, so the menu cannot claim a platform the tool then fails
    /// to fetch.
    #[test]
    fn the_agent_menu_states_the_systems_each_agent_is_published_for() {
        for agent in super::AgentKind::value_variants() {
            let platforms = agent.platforms(Some("bookworm"));
            assert!(
                platforms.contains('/') || platforms.contains("linux"),
                "{agent:?} names no system: {platforms:?}"
            );
        }

        // Read back what the grouping wrote: every platform `telegraf_plans` fetches is named,
        // and none that it does not.
        let telegraf = super::AgentKind::Telegraf.platforms(None);
        let named: Vec<(String, String)> = telegraf
            .split(' ')
            .flat_map(|group| {
                let (os, arches) = group.split_once('/').expect("os/arch group");
                arches
                    .split('+')
                    .map(move |arch| (os.to_string(), arch.to_string()))
            })
            .collect();
        let fetched: Vec<(String, String)> = super::TELEGRAF_PLATFORMS
            .iter()
            .map(|(os, arch, _, _)| (os.to_string(), arch.to_string()))
            .collect();
        assert_eq!(
            named, fetched,
            "the menu and the fetch disagree: {telegraf:?}"
        );
    }

    /// Icinga 2's line is the reach of the artifact *this host* can build, so it is asserted per
    /// host rather than once. Three properties hold for every row: the systems are named with
    /// versions — "Debian" alone still leaves a RHEL 8 or Ubuntu 18.04 operator guessing — both
    /// platforms are offered, and no build-host codename appears. That last one is the defect this
    /// test exists for: a line that named the container was read as the reach, and left every Red
    /// Hat host looking unserved (ADR-0071).
    #[test]
    fn icinga_2s_line_is_the_reach_of_the_host_it_is_read_on() {
        for (codename, _) in super::ICINGA2_REACH {
            let line = super::icinga2_reach(Some(codename));
            assert!(
                line.contains("linux/amd64") && line.contains("windows/amd64"),
                "{codename} offers less than both platforms: {line:?}"
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

        // A host Icinga publishes no packages for builds the Windows artifact and nothing else,
        // and the line says which hosts would — the one place a codename is the honest answer,
        // because there it *is* the operator's problem.
        for stranger in [None, Some("noble"), Some("sid")] {
            let line = super::icinga2_reach(stranger);
            assert!(
                !line.contains("linux/"),
                "{stranger:?} cannot build a Linux artifact, yet is offered one: {line:?}"
            );
            assert!(
                line.contains("windows/amd64"),
                "{stranger:?} still builds the Windows artifact: {line:?}"
            );
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

    /// ADR-0072: the Windows artifact publishes no digest, so what stands in for one is its own
    /// signature — and the verdict is read from the two things that matter, not from the tool's
    /// overall result, which says `Failed` on a Linux host for want of Authenticode roots even when
    /// the signature is sound. The fixtures below are that real output, abbreviated.
    #[test]
    fn a_windows_artifact_is_accepted_only_when_its_publisher_signed_it() {
        let icinga = "\
Signer's certificate:
\tSubject: /C=DE/ST=Bayern/L=Nuernberg/O=Icinga GmbH/CN=Icinga GmbH
\tIssuer : /C=BE/O=GlobalSign nv-sa/CN=GlobalSign GCC R45 CodeSigning CA 2020
Error: unable to get local issuer certificate
Number of verified signatures: 1
Failed
";
        assert_eq!(
            super::publisher_verdict(icinga, "O=Icinga GmbH").as_deref(),
            Ok("/C=DE/ST=Bayern/L=Nuernberg/O=Icinga GmbH/CN=Icinga GmbH"),
            "a sound signature is accepted although the chain could not be built here"
        );

        // Somebody else's signature, valid in itself: refused by name.
        let stranger = icinga.replace("O=Icinga GmbH", "O=Someone Else");
        let refused = super::publisher_verdict(&stranger, "O=Icinga GmbH").expect_err("refused");
        assert!(refused.contains("Someone Else"), "{refused}");

        // Unsigned, or a signature that does not match the file.
        for unsigned in [
            "No signature found.\n",
            "Number of verified signatures: 0\nFailed\n",
        ] {
            let refused = super::publisher_verdict(unsigned, "O=Icinga GmbH").expect_err("refused");
            assert!(refused.contains("nobody vouched for"), "{refused}");
        }
    }

    /// Every agent the `--agent` flag accepts is also offered when the flag is omitted.
    ///
    /// The two came apart once — `icinga2` was reachable by flag and absent from the prompt for as
    /// long as the prompt kept its own list — so what is asserted here is that the menu is built
    /// from the same variants clap validates against, and that the newest one is among them.
    #[test]
    fn the_prompt_offers_every_agent_the_flag_accepts() {
        use clap::ValueEnum as _;
        let offered: Vec<&str> = super::AgentKind::value_variants()
            .iter()
            .map(|agent| agent.source().service_name)
            .collect();
        assert!(offered.contains(&"icinga2"), "offered: {offered:?}");
        for expected in [
            "otelcol",
            "otelcol-contrib",
            "glpi-agent",
            "telegraf",
            "icinga2",
        ] {
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

    /// ADR-0070: the digests come out of the repository's own index, because Icinga signs
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

        // The vendor's own statement of how old a libc may be: the artifact's reach (ADR-0070).
        assert_eq!(
            super::libc_floor(&find("icinga2-bin", "2.16.4-").depends).as_deref(),
            Some("2.38")
        );
        assert_eq!(super::libc_floor("libssl3 (>= 3.0.0)"), None);
    }

    use super::*;

    /// The tag patterns are what keep a version list from offering things that are not releases of
    /// the agent being fetched — the Collector repository tags its builder alongside, and Telegraf
    /// keeps its release candidates.
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
        assert_eq!(
            AgentKind::Telegraf.version_of_tag("v1.39.3"),
            Some("1.39.3".to_string())
        );
        assert_eq!(AgentKind::Telegraf.version_of_tag("v1.21.0-rc1"), None);

        // GLPI numbers releases with two parts and sometimes three, and never a leading `v`.
        assert_eq!(
            AgentKind::GlpiAgent.version_of_tag("1.19"),
            Some("1.19".to_string())
        );
        assert_eq!(
            AgentKind::GlpiAgent.version_of_tag("1.7.1"),
            Some("1.7.1".to_string())
        );
        assert_eq!(AgentKind::GlpiAgent.version_of_tag("1.0-beta1"), None);
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

    /// GLPI renamed its zip's case at 1.9, so both spellings have to be found — and the Linux
    /// artifact is the one that gets repacked.
    #[test]
    fn glpi_finds_both_zip_spellings_and_repacks_only_linux() {
        for zip in ["GLPI-Agent-1.19-x64.zip", "glpi-agent-1.19-x64.zip"] {
            let assets = vec![
                asset(zip),
                asset("glpi-agent-1.19-x86_64.AppImage"),
                asset("glpi-agent-1.19.sha256"),
            ];
            let plans = glpi_plans("1.19", &assets);
            assert_eq!(plans.len(), 2, "{zip}");
            assert!(matches!(plans[0].action, Action::AsPublished), "{zip}");
            assert_eq!(plans[0].out_name, zip, "the zip travels under its own name");
            assert!(matches!(plans[1].action, Action::RepackAppImage { .. }));
            assert_eq!(plans[1].out_name, "glpi-agent_1.19_linux_amd64.tar.gz");
        }
    }

    /// Telegraf's Windows archive is a zip and its Unix ones are tarballs, and upstream spells
    /// 32-bit `i386` where this fleet says `386`.
    #[test]
    fn telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names() {
        let plans = telegraf_plans("1.39.3");
        let find = |os: &str, arch: &str| {
            plans
                .iter()
                .find(|p| p.os == os && p.arch == arch)
                .unwrap_or_else(|| panic!("no {os}/{arch}"))
        };
        let url = |os: &str, arch: &str| find(os, arch).sources[0].url.clone();
        assert!(url("linux", "amd64").ends_with("telegraf-1.39.3_linux_amd64.tar.gz"));
        assert!(url("windows", "amd64").ends_with("_windows_amd64.zip"));
        assert!(url("linux", "386").ends_with("_linux_i386.tar.gz"));
        assert!(find("windows", "amd64").block_hint.contains("telegraf.exe"));
    }

    /// A directory reached through a link is packed **under the linked name too**, with its
    /// contents — the regression this exists for. The GLPI AppImage reaches its Perl library
    /// through `usr/share/perl/5.26`, a link to `5.26.1`; packing that name as an empty directory
    /// produced a tree whose agent could not find a single module, and packing only one of the two
    /// names left which one to directory-read order. A cycle is still refused, and a link pointing
    /// at nothing is dropped.
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
        pack_tree(&tree, "wrapper", &artifact).expect("pack");

        let dest = dir.path().join("unpacked");
        std::fs::create_dir_all(&dest).expect("create");
        let summary = client::archive::extract_tree_tar_gz(&artifact, Path::new("AppRun"), &dest)
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

    /// Both checksum shapes, read the way each source publishes them — including the `*` a
    /// `sha256sum` binary-mode line carries.
    #[test]
    fn a_published_checksum_is_read_from_either_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("telegraf-1.39.3_linux_amd64.tar.gz");
        std::fs::write(&artifact, b"x").expect("write");

        let sums = dir.path().join("sums");
        std::fs::write(
            &sums,
            "aaaa  other.tar.gz\nbbbb *telegraf-1.39.3_linux_amd64.tar.gz\n",
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

    /// An upload the Server refuses before it reads the body — an assigned Set (ADR-0061), an
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

        // What `main` does before anything builds a client (ADR-0007).
        client::tls::install_ring_provider();

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

        let error = upload(&server, "otelcol", "0.0.1", &plan, &artifact)
            .await
            .expect_err("an upload nothing accepted is not a success");
        assert!(
            error.contains("409 Conflict") && error.contains("assigned to an Agent"),
            "the operator is told what the Server refused, not that it could not be reached: {error}"
        );
    }

    /// Every agent this tool can fetch carries a default Configuration, and each one is storable:
    /// the name follows the ADR-0010 grammar the Server enforces (which admits no dot, so no file
    /// extension), the body is not empty (the Server refuses an empty one), and it is aimed at
    /// something — by Selector or by Agent type — rather than at the whole fleet.
    ///
    /// This is the test that fails when a variant is added and its default is forgotten, which is
    /// the only way an agent could reach a Server with a package and nothing to run.
    #[test]
    fn every_agent_carries_a_storable_default_configuration() {
        for agent in AgentKind::value_variants() {
            let defaults = agent.default_configurations();
            let service_name = agent.source().service_name;
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
                assert!(
                    !config.selector.is_empty() || !config.service_name.is_empty(),
                    "{service_name}: {} would be aimed at every Agent in the fleet",
                    config.name
                );
            }
        }
        let icinga = AgentKind::Icinga2.default_configurations();
        let names: Vec<&str> = icinga.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec!["icinga2-conf", "icinga2-zones"],
            "Icinga 2 reads a root file that includes the other by name (ADR-0068), so both travel"
        );
    }

    /// The upload asks before it writes: a Configuration the Server already holds is left exactly
    /// as it is — operator edits included — and only the missing one is stored.
    ///
    /// The stand-in Server answers `200` for `icinga2-conf` and `404` for `icinga2-zones`, and
    /// records every request, so what the test asserts is the *absence* of a PUT over the top of
    /// the existing one.
    #[tokio::test]
    async fn a_default_configuration_the_server_already_holds_is_not_written_over() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        client::tls::install_ring_provider();

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
                let answer = if line.starts_with("GET") && line.contains("icinga2-conf") {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                } else if line.starts_with("GET") {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });

        upload_default_configurations(&server, AgentKind::Icinga2)
            .await
            .expect("the Server accepted the one it was missing");

        let seen = seen.lock().expect("lock").clone();
        assert!(
            seen.iter()
                .any(|r| r.starts_with("PUT") && r.contains("/api/v1/configurations/icinga2-zones")),
            "the missing configuration is stored: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|r| r.starts_with("PUT") && r.contains("/api/v1/configurations/icinga2-conf")),
            "the one the Server already holds is left alone: {seen:?}"
        );
    }
}
