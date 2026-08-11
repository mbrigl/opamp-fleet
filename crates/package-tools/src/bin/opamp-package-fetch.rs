//! `opamp-package-fetch` — an operator helper that turns an agent release into package artifacts
//! this fleet can install, and optionally uploads them (ADR-0028, ADR-0018).
//!
//! Getting a real agent into the fleet is a research task before it is a command: which repository
//! publishes the binaries, what the assets are called this month, which checksum file goes with
//! them, and whether the container is one a Client can open at all. This tool holds that knowledge
//! for the agent types the manual documents — `otelcol`, `otelcol-contrib`, `glpi-agent`,
//! `telegraf`, `icinga2`, and `supervisor`, this fleet's own Client (ADR-0021) — and asks the
//! operator only what it cannot know: which one, which version, which platforms, and where to send
//! the result.
//!
//! Two rules shape what it produces:
//!
//! - **As published wherever possible.** An artifact that reaches the fleet unaltered is one whose
//!   SHA-256 an operator can compare against the release page, and ADR-0028's line from author to
//!   host stays unbroken. Every artifact is verified against the checksum *upstream* published
//!   before anything else happens to it.
//! - **Repacked only where it must be.** The GLPI Agent has no self-contained Linux archive — its
//!   `.tar.gz` is source — so its AppImage is extracted and repacked deterministically (ADR-0018).
//!   That is the one place the bytes change, and the tool says so.
//!
//! Interactive by default; every prompt has a flag, so the same tool serves a pipeline:
//!   opamp-package-fetch                                   # asks for everything
//!   opamp-package-fetch --agent glpi-agent --version 1.19 --platform linux/amd64 --no-upload

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dialoguer::{Confirm, Input, MultiSelect, Select};
use sha2::{Digest, Sha256};

/// How many release series the operator is offered. Enough to reach the release before last when
/// one turns out badly, few enough to read without scrolling.
const SERIES_SHOWN: usize = 3;

/// The member cap a tree package is held to (ADR-0028). Checked while packing rather than
/// discovered on three hundred hosts at rollout time.
const MAX_TREE_MEMBERS: usize = 10_000;

#[derive(Parser)]
#[command(
    name = "opamp-package-fetch",
    about = "Fetch an agent release and turn it into fleet package artifacts"
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
    /// This fleet's own Client — the package a Client updates *itself* from (ADR-0020, ADR-0021).
    #[value(name = "supervisor")]
    Supervisor,
}

/// Everything that differs between one upstream project and the next, in one place.
struct Source {
    /// The Agent type a Supervisor reports and a package Set is built for (ADR-0028) — also this
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
            // This repository's own releases. The Client is an Agent like any other (ADR-0020), so
            // the artifact that updates it is fetched the way every other agent's is — and since
            // ADR-0021 it is named and packed like one too, which is what makes that possible.
            AgentKind::Supervisor => Source {
                service_name: "supervisor",
                repo: "mbrigl/opamp-fleet",
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
                let rest = tag.strip_prefix('v')?;
                let parts: Vec<&str> = rest.split('.').collect();
                (parts.len() == 3 && parts.iter().all(|p| numeric(p))).then(|| rest.to_string())
            }
            // `version/1.2.3` — this project tags its releases with the prefix the release
            // workflow creates them under (ADR-0011), and nothing else in the repository looks
            // like that.
            AgentKind::Supervisor => {
                let rest = tag.strip_prefix("version/")?;
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
                format!("v{version}")
            }
            AgentKind::Supervisor => format!("version/{version}"),
            AgentKind::GlpiAgent => version.to_string(),
        }
    }

            // Named rather than left to the release, unlike the Collectors': this is *this*
            // project's own build matrix (`.github/workflows/release.yml`), so it moves only when
            // this repository decides it should — and the column's question, "does it run on the
            // hosts I have", deserves the concrete answer where one can be given.
            AgentKind::Supervisor => group_platforms(SUPERVISOR_PLATFORMS.iter().copied()),
    /// Whether the release's assets have to be listed to plan the fetch. Telegraf's GitHub
    /// releases carry no assets at all — its binaries are on a CDN, at a URL built from the
    /// version — so listing them would be a wasted request against a rate-limited API.
    fn needs_assets(self) -> bool {
    /// The line that introduces the per-platform hints printed when the fetch is done.
    ///
    /// Every other agent is installed by a Supervisor, which needs a block; this fleet's own Client
    /// is installed *by itself* over itself, and what admits that is a consent rather than a block
    /// (ADR-0020). One sentence, so the hints below it read as the answer to it.
    fn install_hint(self) -> &'static str {
        match self {
            AgentKind::Supervisor => "What a Client needs to take these:",
            _ => "What a Supervisor needs to install these:",
        }
    }

    /// they agree with the `[[supervisor]]` blocks in `config/supervisor.toml` down to the file names.
                service_name: "glpi-agent",
            // None. A Client is configured by `supervisor.toml` on its host — the one file the fleet
            // deliberately does not own (ADR-0017 admits only `[[supervisor]]` blocks), and none
            // of it has a default this tool could put on a Server.
            AgentKind::Supervisor => &[],
    }
}

    service_name: &'static str,
    checksum: ChecksumSource,
/// One platform's artifact, planned before anything is downloaded.
struct Plan {
    /// The platform as *this fleet* names it (ADR-0028), which is what the upload path carries.
    os: String,
    arch: String,
    /// What has to happen to the downloaded bytes before they are a package artifact.
    action: Action,
    /// The artifact's file name in `--out-dir`.
    out_name: String,
    /// What a `[[supervisor]]` block has to say to install it — or, for this fleet's own Client,
    /// what `[self_update]` has to say, since nothing supervises that one. Printed at the end,
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
}

/// What happens between the download and the artifact.
enum Action {
    /// Nothing. The artifact is uploaded exactly as upstream published it, so the hash the fleet
    /// verifies is the hash on the release page (ADR-0028).
    AsPublished,
    /// Extract the AppImage and repack the tree deterministically (ADR-0018) — the one case where
    /// upstream publishes no archive a Client can install.
    RepackAppImage { wrapper: String },
}

/// One thread: this tool is a sequence of downloads with a question between them, and the prompts
/// block by nature — there is nothing for a multi-threaded runtime to overlap.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // reqwest is built with `rustls-no-provider`, which refuses to construct a TLS client until a
    // process-wide provider exists (ADR-0012).
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
    if available.is_empty() {
        // For this project's own releases the likeliest reason is a known one, and saying it saves
        // the operator a trip to the release page: everything published before ADR-0021 carries
        // the old name and container, and a Set built from those fits no Client reporting the type
        // `supervisor` anyway (ADR-0021).
        let why = match agent {
            AgentKind::Supervisor => {
                " — a release from before the rename publishes \
                 `opamp-fleet-client_<version>_<os>_<arch>.7z`, which is a package no Client of \
                 this type installs (ADR-0021)"
            }
            _ => "",
        };
        return Err(format!(
            "{} {version} publishes nothing this tool can install{why}",
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
             curl -X PUT -H 'Content-Type: application/json' -d '{{}}' \\\n       \
             curl -X PUT --data-binary @<artifact> \\\n       \
            name = source.service_name,
            version = version
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

/// The newest version of each of the last [`SERIES_SHOWN`] release series, newest first.
///
/// A *series* is a `major.minor` line, and only its newest patch is offered: nobody installing
/// 2.16 wants 2.16.3 when 2.16.5 exists, and an agent that patches often — Icinga 2 published five
/// 2.16 patches — would otherwise fill the whole list with one line and hide every version an
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
    agent: AgentKind,
    version: &str,
    assets: &[(String, String)],
) -> Result<Vec<Plan>, String> {
    match agent {
        AgentKind::Otelcol | AgentKind::OtelcolContrib => {
            Ok(collector_plans(agent, version, assets))
        }
        AgentKind::GlpiAgent => Ok(glpi_plans(version, assets)),
        AgentKind::Supervisor => Ok(supervisor_plans(version, assets)),
        AgentKind::Telegraf => Ok(telegraf_plans(version)),
    }
/// The packages of a `Depends` union that provide the libraries `ldd` could not resolve — the
/// remedy for *this* refusal rather than the vendor's whole dependency list.
///
/// A `Depends` union names everything the vendor packages need, most of which a build host already
/// has and some of which is not a library at all: run verbatim it also installs the distribution's
/// own `icinga2-common` — a *different* version of the very package being repacked, with its
/// configuration and its `nagios` account — for no library gained. So the union is filtered down to
/// the entries that answer the sonames the error above names.
///
/// The match is by shape, because nothing in a `Packages` index maps a soname to a package: both
/// sides lose everything but letters and digits, and a soname additionally loses its `.so` infix,
/// which turns `libboost_program_options.so.1.74.0` into the prefix `libboostprogramoptions1.74.0`
/// of `libboost-program-options1.74.0`. A prefix rather than an equality, because a package may say
/// more than its soname does — `libboost-regex1.74.0-icu72` names the ICU it was built against.
///
/// **When any soname finds no package at all, the whole union comes back.** A remedy that is
/// incomplete is worse than one that is broad: it would be run, fail again on the library it left
/// out, and read as the tool having lied. Breadth costs a few packages on a build host; a gap costs
/// the trust in the line.
fn packages_providing(missing: &[&str], dependencies: &[String]) -> Vec<String> {
    fn shape(name: &str) -> String {
        name.chars().filter(char::is_ascii_alphanumeric).collect()
    }
    let mut providing: Vec<String> = Vec::new();
    for soname in missing {
        let wanted = shape(&soname.replace(".so", ""));
        let Some(package) = dependencies
            .iter()
            .find(|package| shape(package).starts_with(&wanted))
        else {
            return dependencies.to_vec();
        };
        if !providing.contains(package) {
            providing.push(package.clone());
        }
    }
    providing
}

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
            action: Action::AsPublished,
            out_name: name.clone(),
            block_hint: format!("binary = {program:?}  (type = \"collector\")"),
        });
    }
    plans.sort_by(|a, b| (&a.os, &a.arch).cmp(&(&b.os, &b.arch)));
    plans
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
/// `supervisor_<version>_<os>_<arch>` (ADR-0021), holding the binary under the name it is installed
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
        plans.push(Plan {
            os,
            arch,
                checksum: ChecksumSource::Sums { urls: sums.clone() },
            action: Action::AsPublished,
            out_name: name.clone(),
            // Not a `[[supervisor]]` block: a Client installs this one over itself, and what
            // admits that is the consent in its own `supervisor.toml` (ADR-0020) — which
            // names this very package by default, so most hosts need no line at all.
            block_hint: format!("[self_update] package = {package:?}  (the default)"),
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
/// published, and an AppImage for Linux, which is repacked (ADR-0018). Everything else in the
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
                checksum: ChecksumSource::Sums {
            action: Action::AsPublished,
            out_name: name.clone(),
        });
    }

    if let Some((_, url)) = assets
        .iter()
        .find(|(name, _)| *name == format!("glpi-agent-{version}-x86_64.AppImage"))
    {
        plans.push(Plan {
            os: "linux".to_string(),
            arch: "amd64".to_string(),
                checksum: ChecksumSource::Sums {
            action: Action::RepackAppImage {
                wrapper: format!("glpi-agent-{version}"),
            },
            out_name: format!("glpi-agent_{version}_linux_amd64.tar.gz"),
        });
    }
    plans
}

/// Telegraf's platforms, which cannot be read from anywhere: its GitHub releases carry no assets,
/// and the CDN that holds the binaries serves no directory listing. The list is therefore this
/// tool's own — kept to the platforms this fleet has a name for — and a URL that has gone away is
/// reported as the 404 it is rather than hidden.
fn telegraf_plans(version: &str) -> Vec<Plan> {
        .iter()
        .map(|(os, arch, up_os, up_arch)| {
            let ext = if *os == "windows" { "zip" } else { "tar.gz" };
            let name = format!("telegraf-{version}_{up_os}_{up_arch}.{ext}");
            let url = format!("https://dl.influxdata.com/telegraf/releases/{name}");
            // The archive wraps everything in a version-named directory, and the program sits at
            // `usr/bin/telegraf` on Unix but at the root on Windows. Neither matters for a
            // single-file package: the Client finds the member by its *file name*, so the upstream
            // archive installs as it is.
            Plan {
                os: (*os).to_string(),
                arch: (*arch).to_string(),
                action: Action::AsPublished,
                out_name: name,
            }
        })
        .collect()
}

/// Upstream's platform words in this fleet's vocabulary (ADR-0028), or `None` for one this fleet
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
    }

    match &plan.action {
        Action::AsPublished => {
            eprintln!("  as published — the fleet verifies upstream's own hash");
            Ok(downloaded)
        }
            let artifact = out_dir.join(&plan.out_name);
            let _ = std::fs::remove_file(&downloaded);
            Ok(artifact)
        }
            let artifact = out_dir.join(&plan.out_name);
            Ok(artifact)
        }
        Action::RepackAppImage { wrapper } => {
            let artifact = out_dir.join(&plan.out_name);
            repack_appimage(&downloaded, wrapper, &artifact)?;
            // The AppImage was a means, not a result; leaving it beside the artifact invites
            // uploading the wrong file.
            let _ = std::fs::remove_file(&downloaded);
            Ok(artifact)
        }
    }
}

        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
/// The SHA-256 upstream published for this artifact, from whichever form the source uses.
async fn published_sha256(source: &ChecksumSource, artifact: &Path) -> Result<String, String> {
    let urls = match source {
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

/// Extracts the AppImage and packs the tree it holds (ADR-0018).
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
    // (ADR-0028), and neither is missed.
    let _ = std::fs::remove_file(tree.join(".DirIcon"));
    if !tree.join("AppRun").exists() {
        return Err("the extracted tree has no AppRun".to_string());
    }
    eprintln!("  packing the tree …");
    pack_tree(&tree, wrapper, out)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    pack_tree(&tree, wrapper, out)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

    }
    let staging = out.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
        // would be missing from every host this artifact reaches. The line names the packages that
        // answer *these* sonames, not the vendor's whole dependency list — see `packages_providing`.
                packages_providing(&missing, dependencies).join(" ")
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
/// Packs a directory as a `.tar.gz` a Client can install as a tree — deterministically, and with
/// every link resolved.
///
/// Determinism is not tidiness: the fleet decides whether to distribute by content hash, so an
/// artifact that differed by when it was packed would be a rollout nobody asked for. Times, owners
/// and order are therefore fixed, exactly as `opamp-package-sign pack` fixes them for one file.
///
/// Links are resolved rather than carried because a tree package refuses them (ADR-0028): what a
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
/// (ADR-0028). Nothing is distributed by either call: a Set reaches an Agent only through a
/// rollout act (ADR-0027), which stays the operator's.
async fn upload(
    server: &str,
    service_name: &str,
    version: &str,
    plan: &Plan,
    artifact: &Path,
) -> Result<(), String> {
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
    Ok(())
}

    let response = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", because(&e)))?;
            let body = response.text().await.unwrap_or_default();
/// Why an upload never came back with a status.
///
/// The Server refuses some uploads *before* it reads a byte of the body — an identity nobody
/// created, a Set already assigned and therefore immutable (ADR-0027), a store at its ceiling
/// (ADR-0028). With hundreds of megabytes already in flight and no `Expect: 100-continue` to hold
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
    /// The `Depends` union of the four vendor packages, sorted and deduplicated the way
    /// `icinga2_plans` hands it to the packer — real bookworm data for Icinga 2 2.16.5.
    fn bookworm_dependencies() -> Vec<String> {
        [
            "adduser",
            "icinga2-common",
            "iputils-ping",
            "libboost-coroutine1.74.0",
            "libboost-filesystem1.74.0",
            "libboost-iostreams1.74.0",
            "libboost-program-options1.74.0",
            "libboost-regex1.74.0-icu72",
            "libboost-thread1.74.0",
            "libc6",
            "libedit2",
            "libgcc-s1",
            "libprotobuf-lite32",
            "libssl3",
            "libstdc++6",
            "libsystemd0",
            "lsb-base",
            "lsb-release",
            "monitoring-plugins-common",
            "procps",
            "ucf",
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// The refusal names sonames; the line under it has to name packages, and only the ones that
    /// answer those sonames. Run as the whole `Depends` union it also installs the distribution's
    /// own `icinga2-common` — another version of the package being repacked — plus `procps`, `ucf`
    /// and the rest, none of which is a library that was missing. The two names that do not match
    /// by spelling are the point of the test: `libboost_regex.so.1.74.0` is answered by a package
    /// that names its ICU too, and `libprotobuf-lite.so.32` moves the version across a hyphen.
    #[test]
    fn the_remedy_names_only_the_packages_that_provide_the_missing_libraries() {
        let missing = [
            "libboost_coroutine.so.1.74.0",
            "libboost_filesystem.so.1.74.0",
            "libboost_iostreams.so.1.74.0",
            "libboost_thread.so.1.74.0",
            "libboost_program_options.so.1.74.0",
            "libboost_regex.so.1.74.0",
            "libprotobuf-lite.so.32",
        ];
        assert_eq!(
            super::packages_providing(&missing, &bookworm_dependencies()),
            [
                "libboost-coroutine1.74.0",
                "libboost-filesystem1.74.0",
                "libboost-iostreams1.74.0",
                "libboost-thread1.74.0",
                "libboost-program-options1.74.0",
                "libboost-regex1.74.0-icu72",
                "libprotobuf-lite32",
            ]
        );

        // One missing library asks for one package, not for the twenty-one around it.
        assert_eq!(
            super::packages_providing(&["libedit.so.2"], &bookworm_dependencies()),
            ["libedit2"]
        );
    }

    /// Nothing in a `Packages` index maps a soname to a package, so the match is by shape and can
    /// come up empty — `libicuuc.so.72` is carried by `libicu72`, which the shape does not reach.
    /// Then the whole union comes back: a line that leaves out a library would be run, fail on
    /// exactly that library, and read as the tool having lied.
    #[test]
    fn a_library_no_dependency_accounts_for_falls_back_to_the_whole_list() {
        let missing = ["libboost_thread.so.1.74.0", "libicuuc.so.72"];
        assert_eq!(
            super::packages_providing(&missing, &bookworm_dependencies()),
            bookworm_dependencies()
        );

        // The same rule with nothing to fall back to: an empty union stays empty, and the caller
        // prints the wording that names no command at all.
        assert!(super::packages_providing(&missing, &[]).is_empty());
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

    /// The list offers release *series*, not tags. Icinga 2 is the agent that makes the difference
    /// visible: five 2.16 patches would have been the entire list, so the one question an operator
    /// asks it — which versions can I still go back to — had no answer in it. Only the newest patch
    /// of a series is offered, because an older one of the same line is not a choice anybody makes.
    #[test]
    fn the_version_list_offers_the_newest_patch_of_each_recent_series() {
        let icinga: Vec<String> = [
            "2.16.5", "2.16.4", "2.16.3", "2.16.2", "2.16.1", "2.16.0", "2.15.2", "2.15.1",
            "2.14.6", "2.13.10",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            super::newest_per_series(&icinga),
            ["2.16.5", "2.15.2", "2.14.6"]
        );

        // An agent that versions in two parts has no patch to collapse, so every version is its own
        // series and the list is simply the last three (GLPI tags `1.15`, `1.14`, …).
        let glpi: Vec<String> = ["1.15", "1.14", "1.13", "1.12"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(super::newest_per_series(&glpi), ["1.15", "1.14", "1.13"]);

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

    /// This project's own release is fetched like any other agent's (ADR-0021): the `.tar.gz` per
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
                plan.block_hint.contains("[self_update]"),
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

    /// An upload the Server refuses before it reads the body — an assigned Set (ADR-0027), an
    /// unknown identity, a full store — leaves the operator with a transport error and no status:
    /// the refusal races a body already in flight and loses. What the operator must still be told
    /// is *why the Server said no*, never "cannot reach" about a Server that answered.
    ///
    /// The stand-in Server here drops the upload connection without answering at all, which is the
    /// same thing seen from the client and the deterministic form of it: a race that sometimes
    /// delivers the response would be a test that sometimes passes.
    #[tokio::test]
    async fn a_refusal_lost_with_the_upload_is_asked_for_again() {

        // What `main` does before anything builds a client (ADR-0012).
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
    /// Every *supervised* agent this tool can fetch carries a default Configuration, and each one
    /// is storable: the name follows the ADR-0021 grammar the Server enforces (which admits no dot,
    /// so no file extension), the body is not empty (the Server refuses an empty one), and it is
    /// aimed at something — by Selector or by Agent type — rather than at the whole fleet.
    /// the only way an agent could reach a Server with a package and nothing to run. The Client's
    /// own package is the one exemption, and it is stated below rather than skipped.
            if matches!(agent, AgentKind::Supervisor) {
                // The one package nothing supervises. A Client reads `supervisor.toml` on its own
                // host, and the fleet owns only the `[[supervisor]]` half of it (ADR-0017), so
                // there is no body this tool could put on a Server — and an offer needs none: the
                // consent that admits this package is already in that file (ADR-0020).
                assert!(
                    defaults.is_empty(),
                    "{service_name} is the Client itself; a Configuration for it would be a \
                     decision this test has not been told about"
                );
                continue;
            }
        client::tls::install_ring_provider();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = format!("http://{}", listener.local_addr().expect("addr"));
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
}
