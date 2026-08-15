//! `opamp-package-sign` — an operator helper for building and signing OpAMP Fleet packages
//! (ADR-0015, ADR-0018).
//!
//! Package signatures are **raw Ed25519** over the artifact bytes, verified by the Client with the
//! `ring` provider (see `client::packages::verify`). This tool produces exactly that format — a
//! `keygen`/`sign` counterpart to the Client's verify. `pack` is the same idea one step earlier:
//! it writes container formats [`client::archive`] can open, with the member named the way the
//! Supervisor will look for it, and its tests open what it wrote with that same module.
//!
//! It is an operator tool, and lives in its own crate for that reason (ADR-0065): nothing the
//! Server or Client runtime does depends on it, and a managed host never runs it.
//!
//! Typical use — build an artifact, sign it, upload it:
//!
//! ```text
//! opamp-package-sign keygen --out fleet-signing.pk8   # prints the public key (hex) to stdout
//! # put that hex in the Client's `[packages] verification_key`
//! sha=$(opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail)
//! sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
//! curl -X PUT "http://<server>:4320/api/v1/packages/promtail/promtail/3.0.0/entries/linux/amd64?signature=$sig" \
//!      --data-binary @promtail-3.0.0.tar.gz
//! ```
//!
//! Script-friendly: the hex output (public key, signature, or SHA-256) goes to stdout alone;
//! status messages go to stderr.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    name = "opamp-package-sign",
    about = "Build, hash, and sign OpAMP Fleet packages"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an Ed25519 signing key, write the private key to a file, and print the public key
    /// (hex) — the value for the Client's `[packages] verification_key`.
    Keygen {
        /// Where to write the PKCS#8 private key. Keep it secret; ideally off the Server host.
        #[arg(long, default_value = "package-signing-key.pk8")]
        out: PathBuf,
    },
    /// Sign an artifact and print the signature (hex) — the value for the upload's `signature`.
    Sign {
        /// The PKCS#8 private key from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// The package artifact to sign (the exact bytes uploaded to the Server).
        artifact: PathBuf,
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
    /// `zip`: the Client reads one (ADR-0064) so that a build published as a zip travels as
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
    /// Print an artifact's SHA-256 (hex) — the `sha256` of `PUT /api/v1/packages/{name}/source`
    /// for an artifact this Server will not hold.
    Sha256 {
        /// The artifact to hash, exactly as the Agents will fetch it.
        artifact: PathBuf,
    },
}

/// The package container formats a Client can open (ADR-0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
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
///
/// Until ADR-0024 this was `#[path = "../archive.rs"] mod archive`, a second compilation of the
/// same file, because a binary in a crate without a library has no other way to reach it.
#[cfg(test)]
use client::archive;

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Keygen { out } => {
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|_| "cannot generate a key".to_string())?;
            let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
                .map_err(|_| "the generated key is unusable".to_string())?;
            write_private_key(&out, pkcs8.as_ref())?;
            eprintln!(
                "wrote the private key to {} (keep it secret)",
                out.display()
            );
            eprintln!("public key (hex) — set this as the Client's [packages] verification_key:");
            println!("{}", hex::encode(keypair.public_key().as_ref()));
            Ok(())
        }
        Command::Sign { key, artifact } => {
            let keypair = load_key(&key)?;
            let bytes = std::fs::read(&artifact)
                .map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
            println!("{}", hex::encode(keypair.sign(&bytes).as_ref()));
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
                Format::SevenZ => pack_7z(&program, &member, &out, archive_key.as_deref())?,
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
    // release nothing — each artifact is packed on a runner of its own platform (ADR-0025), so the
    // Linux and macOS ones carry the mode and `client.exe`, which has no use for it, does not.
    #[cfg(unix)]
    {
        entry.has_windows_attributes = true;
        entry.windows_attributes = client::archive::unix_mode_attributes(0o755);
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
fn sha256_file(path: &Path) -> Result<Vec<u8>, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(hasher.finalize().to_vec())
}

fn load_key(path: &PathBuf) -> Result<Ed25519KeyPair, String> {
    let pkcs8 = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| format!("{} is not a valid PKCS#8 Ed25519 key", path.display()))
}

/// Writes the private key, owner-read/write only on Unix — it is a secret.
fn write_private_key(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot restrict {}: {e}", path.display()))?;
    }
    Ok(())
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
        run(Cli {
            command: Command::Pack {
                program: program.to_path_buf(),
                out: out.to_path_buf(),
                format,
                program_name: program_name.map(str::to_string),
                archive_key: archive_key.map(str::to_string),
            },
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

    /// Encryption is the whole reason ADR-0018 admits `.7z`: the artifact stays unreadable
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

    /// What the release ships is a `.7z` (ADR-0025), and an operator who unpacks one by hand must
    /// get a file that runs. Both containers therefore carry an executable mode of their own — the
    /// tar in its header, the 7z in 7-Zip's Unix-attribute convention — rather than relying on the
    /// Client, which sets the mode itself but only on the path where *it* installs the package.
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

    /// A package delivers exactly one program (ADR-0018), so a directory is refused where the
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
        run(Cli {
            command: Command::Keygen {
                out: keyfile.clone(),
            },
        })
        .expect("keygen");
        let keypair = load_key(&keyfile).expect("load");
        let public = keypair.public_key().as_ref().to_vec();
        let bytes = std::fs::read(&artifact).expect("read");
        let signature = keypair.sign(&bytes).as_ref().to_vec();

        // The exact check from client::packages::verify — raw Ed25519, public key, over the bytes.
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
}
