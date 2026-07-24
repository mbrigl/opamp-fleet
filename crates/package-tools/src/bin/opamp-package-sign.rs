//!
//! Package signatures are **raw Ed25519** over the artifact bytes, verified by the Client with the
//!
//!

use std::process::ExitCode;

use ring::signature::{Ed25519KeyPair, KeyPair};

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
    /// for an artifact this Server will not hold.
}

/// The Client's own unpacker, used by this binary's tests rather than restated in them. What `pack`
/// has to get right is not "is this a valid archive" but "does *this* code open it and find the
/// member" — a container the Client cannot open would be discovered on a host, at rollout time, as
/// a failed install on every matched Agent. Test-only: the tool itself never unpacks. (It does
/// reach one item of `archive` outside tests — `unix_mode_attributes`, the 7z convention that
/// module also decodes.)
///
/// Until ADR-0025 this was `#[path = "../archive.rs"] mod archive`, a second compilation of the
/// same file, because a binary in a crate without a library has no other way to reach it.
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
                     is more than one file cannot be delivered as one",
    }
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
    // release nothing — each artifact is packed on a runner of its own platform (ADR-0029), so the
    // Linux and macOS ones carry the mode and `client.exe`, which has no use for it, does not.
    #[cfg(unix)]
    {
        entry.has_windows_attributes = true;
        entry.windows_attributes = client::archive::unix_mode_attributes(0o755);
    }
        .push_archive_entry(entry, Some(source))
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

        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
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
        // The tree path is the one that validates every member before writing anything, and a
        // member whose mode said `S_IFLNK` would refuse the whole archive there.
        let dest = dir.path().join("tree");
        archive::extract_tree_7z(&artifact, Path::new("client"), &dest, None)
            .expect("no link was seen");
    }

        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = tempfile::tempdir().expect("tempdir");
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
