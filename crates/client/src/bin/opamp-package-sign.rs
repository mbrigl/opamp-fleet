//! `opamp-package-sign` — an operator helper for signing OpAMP Fleet packages (ADR-0015).
//!
//! Package signatures are **raw Ed25519** over the artifact bytes, verified by the Client with the
//! `ring` provider (see `crate::packages::verify`). This tool produces exactly that format, so it
//! lives alongside the code that defines it — a `keygen`/`sign` counterpart to the Client's verify.
//! It is a standalone operator convenience: nothing the Server or Client runtime does depends on it.
//!
//! Typical use:
//!   opamp-package-sign keygen --out fleet-signing.pk8   # prints the public key (hex) to stdout
//!   # put that hex in the Client's `[packages] verification_key`
//!   sig=$(opamp-package-sign sign --key fleet-signing.pk8 otelcol-1.2.4)
//!   curl -X PUT "http://<server>:4320/api/v1/packages/otelcol?version=1.2.4&signature=$sig" \
//!        --data-binary @otelcol-1.2.4
//!
//! Script-friendly: the hex output (public key or signature) goes to stdout alone; status messages
//! go to stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ring::signature::{Ed25519KeyPair, KeyPair};

#[derive(Parser)]
#[command(
    name = "opamp-package-sign",
    about = "Generate an Ed25519 key and sign OpAMP Fleet packages (ADR-0015)"
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
}

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
    }
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

        // The exact check from crate::packages::verify — raw Ed25519, public key, over the bytes.
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
