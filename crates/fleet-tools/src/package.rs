//! `opamp-fleetctl package` — fetching, building, hashing and signing OpAMP Fleet packages
//! (ADR-0027, ADR-0032).
//!
//! Package signatures are **raw Ed25519** over a statement of what the artifact is — its Agent
//! type, its version and its SHA-256 (`fleet_core::package::statement`, ADR-0027) — verified by
//! the Client with the `ring` provider. A signature therefore holds only for the type and version
//! it was made for. This tool produces exactly that format — a
//! `keygen`/`sign` counterpart to the Client's verify. `pack` is the same idea one step earlier:
//! it writes container formats [`fleet_agent::archive`] can open, with the member named the way the
//! Supervisor will look for it, and its tests open what it wrote with that same module.
//!
//! It is part of the operator tool, which lives in its own crate for that reason (ADR-0008,
//! ADR-0032): nothing the Server or Client runtime does depends on it, and a managed host never
//! runs it. `fetch` is [`crate::fetch`]; the rest is here.
//!
//! Typical use — build an artifact, upload it, sign its entry (ADR-0027):
//!
//! ```text
//! opamp-fleetctl package keygen --out fleet-signing.pk8   # prints the public key (hex) to stdout
//! # put that hex in the Client's `[packages] verification_key` (and, optionally, the Server's)
//! sha=$(opamp-fleetctl package pack --out promtail-3.0.0.tar.gz ./promtail)
//! sig=$(opamp-fleetctl package sign --key fleet-signing.pk8 --agent-type promtail --version 3.0.0 \
//!       promtail-3.0.0.tar.gz)
//! curl -X PUT "https://<server>:4321/api/v1/packages/promtail/3.0.0/entries/linux/amd64" \
//!      --data-binary @promtail-3.0.0.tar.gz
//! curl -X PUT "https://<server>:4321/api/v1/packages/promtail/3.0.0/entries/linux/amd64/signature" \
//!      -H 'content-type: application/json' --data "{\"signature\": \"$sig\"}"
//! ```
//!
//! Keeping every build below a version off the fleet — a signed floor (ADR-0028):
//!
//! ```text
//! sig=$(opamp-fleetctl package floor --key fleet-signing.pk8 --agent-type promtail --version 3.0.0)
//! curl -X PUT "https://<server>:4321/api/v1/package-floors/promtail" \
//!      -H 'content-type: application/json' --data "{\"version\": \"3.0.0\", \"signature\": \"$sig\"}"
//! ```
//!
//! Script-friendly: the hex output (public key, signature, or SHA-256) goes to stdout alone;
//! status messages go to stderr.

use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The commands of `opamp-fleetctl package` (ADR-0028 clause 22).
#[derive(Subcommand)]
pub enum Command {
    /// Fetch a release of a known agent, verify it against upstream's checksum, and optionally
    /// upload it to a Server with its default configuration.
    Fetch(crate::fetch::Args),
    /// Generate an Ed25519 signing key, write the private key to a file, and print the public key
    /// (hex) — the value for the Client's `[packages] verification_key`.
    Keygen {
        /// Where to write the PKCS#8 private key. Keep it secret; ideally off the Server host.
        #[arg(long, default_value = "package-signing-key.pk8")]
        out: PathBuf,
    },
    /// Sign an artifact and print the signature (hex) — the value of that Package entry's signature
    /// (ADR-0027). Or, with --signing-info, sign every entry of a Package's signing file from the
    /// UI and write the signatures into it (ADR-0027).
    Sign {
        /// The PKCS#8 private key from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// The Agent type the artifact is for — the Package's type on the Server. The signature
        /// holds for this type alone.
        #[arg(long, required_unless_present = "signing_info")]
        agent_type: Option<String>,
        /// The Package's version, a SemVer 2.0.0 version (`1.19.0`, not `1.19`). The signature
        /// holds for this version alone.
        #[arg(long, required_unless_present = "signing_info")]
        version: Option<String>,
        /// The package artifact to sign (the exact bytes uploaded to the Server).
        #[arg(required_unless_present = "signing_info")]
        artifact: Option<PathBuf>,
        /// A Package's signing file, as the UI's "Signing info" downloads it. Every entry is signed
        /// from the hash the file states — **compare those hashes with upstream's checksums or
        /// with `package sha256` over the files first**: the tool checks them against nothing.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["agent_type", "version", "artifact"])]
        signing_info: Option<PathBuf>,
        /// Where to write the signed file. Default: over the --signing-info file.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["agent_type", "version", "artifact"])]
        out: Option<PathBuf>,
    },
    /// Sign a floor and print the signature (hex): below `--version`, no Client takes a package of
    /// `--agent-type` any more, its own update included (ADR-0028). Store it on the Server with
    /// `PUT /api/v1/package-floors/{agent_type}`.
    Floor {
        /// The PKCS#8 private key from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// The Agent type the floor holds for. The signature holds for this type alone.
        #[arg(long)]
        agent_type: String,
        /// The lowest version still taken, a SemVer 2.0.0 version. The signature holds for this
        /// version alone.
        #[arg(long)]
        version: String,
    },
    /// Print the public key (hex) of an existing private key.
    PublicKey {
        /// The PKCS#8 private key from `keygen`.
        #[arg(long)]
        key: PathBuf,
    },
    /// Pack a single-file program into a package artifact and print its SHA-256 (hex).
    ///
    /// The archive holds exactly one member, named so the Supervisor finds it: a Client looks for
    /// the file name of the program its block configures (`binary`/`command`), wherever the
    /// archive keeps it. Pack `./build/promtail` for a Supervisor whose `command = "promtail"` and
    /// the names already agree; `--program-name` is for when they do not.
    ///
    /// Two of the three containers the Client can open are produced. There is deliberately no
    /// `zip`: the Client reads one (ADR-0027 clause 62) so that a build published as a zip travels as
    /// published, which is the opposite of a reason to *write* one here — a zip carries no Unix
    /// modes, and packing an artifact into it would be choosing the one container that cannot say
    /// the program is executable.
    Pack {
        /// The program to pack — one file, since exactly one member is ever installed.
        program: PathBuf,
        /// Where to write the artifact.
        #[arg(long)]
        out: PathBuf,
        /// The container to write. `tar.gz` is what upstream releases ship and needs no key;
        /// `7z` exists for `--archive-key`, which keeps the artifact unreadable wherever it is
        /// stored — including on the Server, which never learns the key.
        #[arg(long, value_enum, default_value_t = Format::TarGz)]
        format: Format,
        /// The member name inside the archive. Defaults to the program's own file name, which is
        /// what a Supervisor configured with a bare file name looks for.
        #[arg(long)]
        program_name: Option<String>,
        /// Encrypt the archive with this key (`7z` only) — the Client's `[packages] archive_key`.
        #[arg(long)]
        archive_key: Option<String>,
    },
    /// Print an artifact's SHA-256 (hex) — the `content_hash` the Server shows for its entry, and
    /// what to compare with upstream's checksums before signing.
    Sha256 {
        /// The artifact to hash, exactly as the Agents will fetch it.
        artifact: PathBuf,
    },
}

/// The package container formats a Client can open (ADR-0027).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// A gzip-compressed tar — what `opentelemetry-collector-releases` and most projects publish.
    #[value(name = "tar.gz")]
    TarGz,
    /// A 7z container, optionally AES-256 encrypted with `--archive-key`.
    #[value(name = "7z")]
    SevenZ,
}

/// The Client's own unpacker, used by this binary's tests rather than restated in them. What `pack`
/// has to get right is not "is this a valid archive" but "does *this* code open it and find the
/// member" — a container the Client cannot open would be discovered on a host, at rollout time, as
/// a failed install on every matched Agent. Test-only: the tool itself never unpacks. (It does
/// reach one item of `archive` outside tests — `unix_mode_attributes`, the 7z convention that
/// module also decodes.)
#[cfg(test)]
use fleet_agent::archive;

/// Runs one `package` command.
///
/// # Errors
/// Returns the message to print when the command fails.
pub fn run(command: Command) -> Result<(), String> {
    match command {
        // One thread: fetching is a sequence of downloads with a question between them, and the
        // prompts block by nature — there is nothing for a multi-threaded runtime to overlap.
        Command::Fetch(args) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the runtime: {e}"))?
            .block_on(crate::fetch::run(args)),
        Command::Keygen { out } => {
            let (pkcs8, public_key) = signing_key()?;
            crate::pki::write_file(&out, &pkcs8, true)?;
            eprintln!(
                "wrote the private key to {} (keep it secret)",
                out.display()
            );
            eprintln!("public key (hex) — set this as the Client's [packages] verification_key:");
            println!("{public_key}");
            Ok(())
        }
        Command::Sign {
            key,
            signing_info: Some(path),
            out,
            ..
        } => {
            // Checked whole before the key is even loaded: a file with one bad entry signs nothing.
            let mut file = crate::signing::read(&path)?;
            let keypair = load_key(&key)?;
            file.sign(&keypair);
            let out = out.unwrap_or(path);
            file.write(&out)?;
            eprintln!(
                "wrote the signatures to {} — import it on the Packages tab. These hashes were \
                 signed as the file states them; they are only as right as your comparison with \
                 upstream's checksums or the files.",
                out.display()
            );
            Ok(())
        }
        Command::Sign {
            key,
            agent_type,
            version,
            artifact,
            ..
        } => {
            // clap makes the three required whenever --signing-info is absent.
            let (Some(agent_type), Some(version), Some(artifact)) = (agent_type, version, artifact)
            else {
                return Err("sign needs --agent-type, --version and the artifact".to_string());
            };
            // Before the key is read: it never signs a version a Client would refuse
            // (ADR-0028 clause 4).
            require_semver(&version)?;
            let keypair = load_key(&key)?;
            let bytes = std::fs::read(&artifact)
                .map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
            let statement =
                fleet_core::package::statement(&agent_type, &version, &Sha256::digest(&bytes));
            println!("{}", hex::encode(keypair.sign(&statement).as_ref()));
            Ok(())
        }
        Command::Floor {
            key,
            agent_type,
            version,
        } => {
            println!("{}", floor_signature(&key, &agent_type, &version)?);
            Ok(())
        }
        Command::PublicKey { key } => {
            let keypair = load_key(&key)?;
            println!("{}", hex::encode(keypair.public_key().as_ref()));
            Ok(())
        }
        Command::Pack {
            program,
            out,
            format,
            program_name,
            archive_key,
        } => {
            // Moved into a buffer wiped when it is dropped; what `clap` keeps of the command line
            // is its own (ADR-0014).
            let archive_key = archive_key.map(Zeroizing::new);
            if !program.is_file() {
                return Err(format!(
                    "{} is not a file — a package delivers exactly one program, and an agent that \
                     is more than one file cannot be delivered as one",
                    program.display()
                ));
            }
            let member = match program_name {
                Some(name) => name,
                None => program
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("{} has no file name", program.display()))?,
            };
            match format {
                Format::TarGz => {
                    if archive_key.is_some() {
                        return Err("--archive-key encrypts a 7z; a .tar.gz has no encryption \
                                    (use --format 7z)"
                            .to_string());
                    }
                    pack_tar_gz(&program, &member, &out)?;
                }
                Format::SevenZ => pack_7z(
                    &program,
                    &member,
                    &out,
                    archive_key.as_deref().map(String::as_str),
                )?,
            }
            eprintln!(
                "wrote {} holding {member:?} — a Supervisor whose program is named {member:?} \
                 installs it",
                out.display()
            );
            if archive_key.is_some() {
                eprintln!(
                    "encrypted: set the same value as [packages] archive_key on every Client that \
                     receives this package"
                );
            }
            eprintln!("sha256 (hex):");
            println!("{}", hex::encode(sha256_file(&out)?));
            Ok(())
        }
        Command::Sha256 { artifact } => {
            println!("{}", hex::encode(sha256_file(&artifact)?));
            Ok(())
        }
    }
}

/// Writes a `.tar.gz` holding `program` under the single entry `member`.
///
/// Every field that would otherwise carry the packing host's state — modification time, owner,
/// group — is zeroed, so packing the same program twice produces the same bytes and therefore the
/// same SHA-256. In a system where a hash decides whether anything is distributed at all, an
/// artifact that differs only by when it was built is a rollout nobody asked for.
fn pack_tar_gz(program: &Path, member: &str, out: &Path) -> Result<(), String> {
    let mut source = std::fs::File::open(program)
        .map_err(|e| format!("cannot read {}: {e}", program.display()))?;
    let size = source
        .metadata()
        .map_err(|e| format!("cannot stat {}: {e}", program.display()))?
        .len();
    let file =
        std::fs::File::create(out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file,
        flate2::Compression::default(),
    ));

    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    // The Client sets the mode itself when it installs (0o755), but an artifact is also something
    // an operator unpacks by hand to check — so it carries an executable mode of its own.
    header.set_mode(0o755);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, member, &mut source)
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    builder
        .into_inner()
        .map_err(|e| format!("cannot finish {}: {e}", out.display()))?
        .finish()
        .map_err(|e| format!("cannot finish {}: {e}", out.display()))?;
    Ok(())
}

/// Writes a `.7z` holding `program` under the single entry `member`, AES-256 encrypted when a key
/// is given.
///
/// Unlike the `.tar.gz` above this is **not** reproducible: encryption draws a fresh salt every
/// time, so two runs over the same program yield different bytes. Take the SHA-256 from the
/// artifact actually uploaded — which is what this command prints.
fn pack_7z(
    program: &Path,
    member: &str,
    out: &Path,
    archive_key: Option<&str>,
) -> Result<(), String> {
    let source = std::fs::File::open(program)
        .map_err(|e| format!("cannot read {}: {e}", program.display()))?;
    let mut writer = sevenz_rust2::ArchiveWriter::create(out)
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    if let Some(key) = archive_key {
        // AES first, then the compressor — the order the crate's own encryption tests use.
        writer.set_content_methods(vec![
            sevenz_rust2::encoder_options::AesEncoderOptions::new(sevenz_rust2::Password::from(
                key,
            ))
            .into(),
            sevenz_rust2::encoder_options::Lzma2Options::default().into(),
        ]);
    }
    // `mut` is used only by the Unix block below; on Windows there is nothing to set.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut entry = sevenz_rust2::ArchiveEntry::new_file(member);
    // The member is a program, so it is marked executable — `7z x` on Linux or macOS then yields
    // something that runs, without a `chmod +x` nobody documented. The tar path has always done
    // this; a `.7z` says it through 7-Zip's Unix-attribute convention instead of a tar mode field.
    //
    // Only off Windows, which is 7-Zip's own rule: bit 15 means `FILE_ATTRIBUTE_INTEGRITY_STREAM`
    // there, and the Windows build neither writes nor expects the Unix extension. It costs the
    // release nothing — each artifact is packed on a runner of its own platform (ADR-0034), so the
    // Linux and macOS ones carry the mode and `client.exe`, which has no use for it, does not.
    #[cfg(unix)]
    {
        entry.has_windows_attributes = true;
        entry.windows_attributes = fleet_agent::archive::unix_mode_attributes(0o755);
    }
    writer
        .push_archive_entry(entry, Some(source))
        .map_err(|e| format!("cannot pack {}: {e}", program.display()))?;
    writer
        .finish()
        .map_err(|e| format!("cannot finish {}: {e}", out.display()))?;
    Ok(())
}

/// Streams a file through SHA-256 — an artifact is a program, too big to read into memory whole.
pub(crate) fn sha256_file(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;

    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// A new package signing key (ADR-0027): the private key as PKCS#8 DER, and its public half in
/// hex, the form `[packages] verification_key` takes. `keygen` and `pki init` make it alike. The
/// private key is wiped when it is dropped; the `ring` document it is copied from is not
/// (ADR-0014).
///
/// # Errors
/// Returns the message to print when no key can be made.
pub(crate) fn signing_key() -> Result<(Zeroizing<Vec<u8>>, String), String> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "cannot generate a key".to_string())?;
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| "the generated key is unusable".to_string())?;
    Ok((
        Zeroizing::new(pkcs8.as_ref().to_vec()),
        hex::encode(keypair.public_key().as_ref()),
    ))
}

/// Refuses a Package version that is not SemVer (ADR-0028 clauses 2 and 4), naming it — the
/// predicate the Server and the Client ask too, so what is signed here is what they take.
///
/// # Errors
/// Returns the message to print when `version` is not SemVer.
pub(crate) fn require_semver(version: &str) -> Result<(), String> {
    if fleet_core::version::is_semver(version) {
        Ok(())
    } else {
        Err(format!(
            "the version {version:?} is not SemVer — a Package version is a SemVer 2.0.0 \
             version (for example 1.19.0 for upstream's 1.19), and a Client refuses any other; \
             nothing was signed"
        ))
    }
}

/// The signature, hex, over the floor statement of `agent_type` at `version`
/// (`fleet_core::package::floor_statement`, ADR-0028 clause 14). A version that is not SemVer is
/// refused before the key is read, as `sign` refuses it (ADR-0028 clause 4).
fn floor_signature(key: &PathBuf, agent_type: &str, version: &str) -> Result<String, String> {
    if !fleet_core::version::is_semver(version) {
        return Err(format!(
            "the version {version:?} is not SemVer — a floor is a SemVer 2.0.0 version (for \
             example 1.19.0 for upstream's 1.19), and a Client refuses any other; nothing was \
             signed"
        ));
    }
    let keypair = load_key(key)?;
    let statement = fleet_core::package::floor_statement(agent_type, version);
    Ok(hex::encode(keypair.sign(&statement).as_ref()))
}

fn load_key(path: &PathBuf) -> Result<Ed25519KeyPair, String> {
    // Wiped once `ring` holds the key (ADR-0014).
    let pkcs8 = std::fs::read(path)
        .map(Zeroizing::new)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| format!("{} is not a valid PKCS#8 Ed25519 key", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::archive;

    fn pack(
        program: &Path,
        out: &Path,
        format: Format,
        program_name: Option<&str>,
        archive_key: Option<&str>,
    ) -> Result<(), String> {
        run(Command::Pack {
            program: program.to_path_buf(),
            out: out.to_path_buf(),
            format,
            program_name: program_name.map(str::to_string),
            archive_key: archive_key.map(str::to_string),
        })
    }

    /// The program bytes, written where a `--touch`-style agent binary would be.
    fn program(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\necho a-foreign-agent\n").expect("write the program");
        path
    }

    fn unpacked(archive: &Path, member: &str, key: Option<&str>) -> Vec<u8> {
        let dir = archive.parent().expect("a parent");
        let out_path = dir.join(format!("unpacked-{member}"));
        let mut out = std::fs::File::create(&out_path).expect("create");
        match archive::detect(archive).expect("detect") {
            archive::Kind::TarGz => {
                archive::extract_tar_gz(archive, member, &mut out).expect("extract tar.gz")
            }
            archive::Kind::SevenZ => {
                archive::extract_7z(archive, member, &mut out, key).expect("extract 7z")
            }
            archive::Kind::Raw | archive::Kind::Zip => {
                panic!("the packer wrote something it never produces (a bare program or a zip)")
            }
        };
        drop(out);
        std::fs::read(&out_path).expect("read the unpacked member")
    }

    /// `sign --version 1.19` and a signing file at `1.19` each fail naming the version, before
    /// the key is read: the key named here does not exist, so reading it would fail with another
    /// message (ADR-0028 clause 4).
    /// Verifies: ADR-0028
    #[test]
    fn sign_refuses_a_version_that_is_not_semver_before_reading_the_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = dir.path().join("no-such.key");
        let artifact = dir.path().join("artifact");
        std::fs::write(&artifact, b"bytes").expect("write");

        let flags = run(Command::Sign {
            key: key.clone(),
            signing_info: None,
            out: None,
            agent_type: Some("otelcol".to_string()),
            version: Some("1.19".to_string()),
            artifact: Some(artifact),
        })
        .expect_err("1.19 is not SemVer");
        assert!(
            flags.contains("\"1.19\"") && flags.contains("SemVer"),
            "{flags}"
        );

        let file = dir.path().join("signing.json");
        std::fs::write(
            &file,
            serde_json::json!({
                "format": crate::signing::FORMAT,
                "agent_type": "otelcol",
                "version": "1.19",
                "entries": [{
                    "os": "linux",
                    "arch": "amd64",
                    "sha256": "127f9a4866448977b174bc2718e4d53646c7ff571782756837ae9bf1412cfb9e"
                }]
            })
            .to_string(),
        )
        .expect("write");
        let from_file = run(Command::Sign {
            key,
            signing_info: Some(file),
            out: None,
            agent_type: None,
            version: None,
            artifact: None,
        })
        .expect_err("1.19 is not SemVer");
        assert!(
            from_file.contains("\"1.19\"") && from_file.contains("SemVer"),
            "{from_file}"
        );
        assert!(!from_file.contains("no-such.key"), "{from_file}");
    }

    /// The round trip that matters: what `pack` writes, the Client opens, finding the member under
    /// the name its `[[supervisor]]` block configures.
    #[test]
    fn a_packed_tar_gz_is_opened_by_the_client_and_holds_the_program() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "promtail");
        let artifact = dir.path().join("promtail-3.0.0.tar.gz");

        pack(&source, &artifact, Format::TarGz, None, None).expect("pack");

        assert_eq!(
            archive::detect(&artifact).expect("detect"),
            archive::Kind::TarGz,
            "the Client decides by leading bytes, so the container must be gzip"
        );
        assert_eq!(
            unpacked(&artifact, "promtail", None),
            std::fs::read(&source).expect("read the program")
        );
    }

    /// Encryption is the whole reason ADR-0027 admits `.7z`: the artifact stays unreadable
    /// wherever it is stored, and the key never reaches the Server.
    #[test]
    fn a_packed_7z_opens_with_the_archive_key_and_not_without_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "promtail");
        let artifact = dir.path().join("promtail-3.0.0.7z");

        pack(&source, &artifact, Format::SevenZ, None, Some("s3cret")).expect("pack");

        assert_eq!(
            archive::detect(&artifact).expect("detect"),
            archive::Kind::SevenZ
        );
        assert_eq!(
            unpacked(&artifact, "promtail", Some("s3cret")),
            std::fs::read(&source).expect("read the program")
        );

        // The Client refuses the wrong key rather than installing something it could not read.
        let mut out = std::fs::File::create(dir.path().join("nope")).expect("create");
        assert!(archive::extract_7z(&artifact, "promtail", &mut out, Some("wrong")).is_err());
        assert!(archive::extract_7z(&artifact, "promtail", &mut out, None).is_err());
    }

    /// An operator who unpacks a published artifact by hand must get a file that runs — the release
    /// ships `.tar.gz` (ADR-0034), and `.7z` stays the container an artifact may be encrypted in
    /// (ADR-0027). Both therefore carry an executable mode of their own — the tar in its header,
    /// the 7z in 7-Zip's Unix-attribute convention — rather than relying on the Client, which sets
    /// the mode itself but only on the path where *it* installs the package.
    #[test]
    #[cfg(unix)]
    fn a_packed_program_is_executable_when_it_is_unpacked_by_hand() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "client");

        let seven = dir.path().join("client.7z");
        pack(&source, &seven, Format::SevenZ, None, None).expect("pack");
        let reader = sevenz_rust2::ArchiveReader::open(&seven, Default::default()).expect("open");
        let entry = reader
            .archive()
            .files
            .iter()
            .find(|e| e.name() == "client")
            .expect("the member is there");
        assert!(
            entry.has_windows_attributes,
            "without the attribute there is no mode to restore"
        );
        // Bit 15 says the high half is a Unix mode; the high half says a regular file, rwxr-xr-x.
        assert_eq!(entry.windows_attributes() & 0x8000, 0x8000);
        assert_eq!(entry.windows_attributes() >> 16, 0o100_755);

        let tarball = dir.path().join("client.tar.gz");
        pack(&source, &tarball, Format::TarGz, None, None).expect("pack");
        let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(
            std::fs::File::open(&tarball).expect("open"),
        ));
        let mode = entries
            .entries()
            .expect("entries")
            .next()
            .expect("one member")
            .expect("readable")
            .header()
            .mode()
            .expect("a mode");
        assert_eq!(mode & 0o777, 0o755);
    }

    /// The member the Client itself would reject: the same convention that carries a mode can say
    /// "symbolic link", and what `pack` writes must never be mistaken for one.
    #[test]
    #[cfg(unix)]
    fn the_mode_a_pack_writes_is_not_read_back_as_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "client");
        let artifact = dir.path().join("client.7z");

        pack(&source, &artifact, Format::SevenZ, None, None).expect("pack");

        assert_eq!(
            unpacked(&artifact, "client", None),
            std::fs::read(&source).expect("read the program")
        );
        // The tree path is the one that validates every member before writing anything, and a
        // member whose mode said `S_IFLNK` would refuse the whole archive there.
        let dest = dir.path().join("tree");
        archive::extract_tree_7z(&artifact, Path::new("client"), &dest, None)
            .expect("no link was seen");
    }

    /// A release is named after its version and the Supervisor is not — `--program-name` is what
    /// bridges that, and getting it wrong is the failure the Client reports as a missing member.
    #[test]
    fn program_name_decides_what_the_supervisor_will_find() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "promtail-3.0.0-linux-amd64");
        let artifact = dir.path().join("release.tar.gz");

        pack(&source, &artifact, Format::TarGz, Some("promtail"), None).expect("pack");

        assert_eq!(
            unpacked(&artifact, "promtail", None),
            std::fs::read(&source).expect("read the program")
        );
        // And the file name it was built from is *not* what the archive holds.
        let mut out = std::fs::File::create(dir.path().join("nope")).expect("create");
        assert!(
            archive::extract_tar_gz(&artifact, "promtail-3.0.0-linux-amd64", &mut out).is_err(),
            "only the configured member name is in the archive"
        );
    }

    /// Packing the same program twice must give the same bytes. A hash decides whether a package
    /// is distributed at all, so an artifact that differs only by when it was built would be a
    /// rollout nobody asked for.
    #[test]
    fn packing_a_tar_gz_twice_yields_the_same_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "promtail");
        let first = dir.path().join("first.tar.gz");
        let second = dir.path().join("second.tar.gz");

        pack(&source, &first, Format::TarGz, None, None).expect("pack");
        pack(&source, &second, Format::TarGz, None, None).expect("pack");

        assert_eq!(
            sha256_file(&first).expect("hash"),
            sha256_file(&second).expect("hash")
        );
    }

    /// A `.tar.gz` has no encryption, so a key given with it is a misunderstanding worth stopping
    /// at — the alternative is an artifact the operator believes is encrypted and is not.
    #[test]
    fn an_archive_key_on_a_tar_gz_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = program(dir.path(), "promtail");
        let artifact = dir.path().join("promtail.tar.gz");

        assert!(pack(&source, &artifact, Format::TarGz, None, Some("s3cret")).is_err());
    }

    /// A package delivers exactly one program (ADR-0027), so a directory is refused where the
    /// operator can still see why — not on every host at rollout time.
    #[test]
    fn packing_something_that_is_not_a_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("out.tar.gz");

        assert!(pack(dir.path(), &artifact, Format::TarGz, None, None).is_err());
    }

    /// The tool's signature must be accepted by exactly the verification the Client performs
    /// (`ring` raw Ed25519 over the artifact bytes) — otherwise a signed package would be refused.
    #[test]
    fn keygen_then_sign_produces_a_signature_the_client_verifier_accepts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keyfile = dir.path().join("k.pk8");
        let artifact = dir.path().join("art.bin");
        std::fs::write(&artifact, b"a-managed-process-binary").expect("write artifact");

        // keygen writes the key and yields the public key; sign yields the signature.
        run(Command::Keygen {
            out: keyfile.clone(),
        })
        .expect("keygen");
        let keypair = load_key(&keyfile).expect("load");
        let public = keypair.public_key().as_ref().to_vec();
        let bytes = std::fs::read(&artifact).expect("read");
        let signature = keypair.sign(&bytes).as_ref().to_vec();

        // The exact check from fleet_agent::packages::verify — raw Ed25519, public key, over the
        // bytes.
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public)
            .verify(&bytes, &signature)
            .expect("the client verifier accepts the tool's signature");

        // A tampered artifact is rejected — the signature is over the content, not just present.
        assert!(
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public)
                .verify(b"tampered", &signature)
                .is_err()
        );
    }

    /// `keygen` creates the key file owner-only from the moment it exists, through the writer `pki
    /// init` uses, and refuses to replace a file that exists, leaving it as it was.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn keygen_writes_the_key_owner_only_and_never_replaces_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keyfile = dir.path().join("signing.pk8");
        run(Command::Keygen {
            out: keyfile.clone(),
        })
        .expect("keygen");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&keyfile)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
        let written = std::fs::read(&keyfile).expect("read");
        let err = run(Command::Keygen {
            out: keyfile.clone(),
        })
        .expect_err("an existing key is not replaced");
        assert!(err.contains("never replaced"), "{err}");
        assert!(err.contains(&keyfile.display().to_string()), "{err}");
        assert_eq!(std::fs::read(&keyfile).expect("read"), written);
    }

    /// `package floor` signs the floor statement of the type and the version, which verifies
    /// against the public key `keygen` printed; a version that is not SemVer is refused before the
    /// key is read.
    /// Verifies: ADR-0028
    #[test]
    fn floor_signs_the_floor_statement_and_refuses_a_version_that_is_not_semver() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keyfile = dir.path().join("k.pk8");
        run(Command::Keygen {
            out: keyfile.clone(),
        })
        .expect("keygen");
        let public = load_key(&keyfile)
            .expect("load")
            .public_key()
            .as_ref()
            .to_vec();
        let signature = hex::decode(floor_signature(&keyfile, "otelcol", "2.0.0").expect("signed"))
            .expect("hex");
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public);
        key.verify(
            &fleet_core::package::floor_statement("otelcol", "2.0.0"),
            &signature,
        )
        .expect("the floor verifies");
        assert!(key
            .verify(
                &fleet_core::package::floor_statement("otelcol", "2.0.1"),
                &signature
            )
            .is_err());

        let refused = floor_signature(&dir.path().join("no-such.key"), "otelcol", "2.0")
            .expect_err("2.0 is not SemVer");
        assert!(
            refused.contains("\"2.0\"") && refused.contains("SemVer"),
            "{refused}"
        );
        assert!(!refused.contains("no-such.key"), "{refused}");
    }
}
