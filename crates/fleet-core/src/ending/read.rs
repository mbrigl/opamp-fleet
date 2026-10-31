//! A certificate's end read with `x509-parser` and `time` (ADR-0032 clause 6): what the Server's
//! and the operator tool's `pki status` list, and what the running Server watches, read and put
//! in words once. Each program reads the file and takes its first certificate through
//! `opamp::tls`; this module reads the end of that certificate.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use x509_parser::prelude::{FromDer as _, X509Certificate};

use super::Standing;

/// When one certificate ends, as `pki status` lists it and the running Server watches it.
#[derive(Debug, Clone)]
pub struct Ending {
    pub file: PathBuf,
    pub subject: String,
    pub serial: String,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
}

impl Ending {
    /// The end of the certificate `der`, read from `file`, which the error names.
    ///
    /// # Errors
    /// Returns the message to print when the certificate does not parse.
    pub fn read(file: &Path, der: &[u8]) -> Result<Self, String> {
        let (_, cert) = X509Certificate::from_der(der)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        Ok(Ending {
            file: file.to_path_buf(),
            subject: cert.subject().to_string(),
            serial: hex::encode(cert.raw_serial()),
            not_before: cert.validity().not_before.to_datetime(),
            not_after: cert.validity().not_after.to_datetime(),
        })
    }

    /// Where this end stands at `now`, by the rule of [`standing`](super::standing).
    #[must_use]
    pub fn standing(&self, now: OffsetDateTime) -> Standing {
        super::standing(
            self.not_before.unix_timestamp(),
            self.not_after.unix_timestamp(),
            now.unix_timestamp(),
        )
    }

    /// Whole days left at `now`, negative once ended.
    #[must_use]
    pub fn days_left(&self, now: OffsetDateTime) -> i64 {
        (self.not_after - now).whole_days()
    }

    /// The line `pki status` prints for it: standing, days left, end, serial, subject and file.
    #[must_use]
    pub fn line(&self, now: OffsetDateTime) -> String {
        let standing = match self.standing(now) {
            Standing::Fine => "ok",
            Standing::Ending => "ENDS SOON",
            Standing::Ended => "ENDED",
        };
        format!(
            "{standing:9} {} days  {}  serial {}  {}  ({})",
            self.days_left(now),
            self.not_after.date(),
            self.serial,
            self.subject,
            self.file.display()
        )
    }
}

/// The exit code of a `pki status` command over `endings` at `now`, by [`exit_code`](super::exit_code).
#[must_use]
pub fn exit_code(endings: &[Ending], now: OffsetDateTime) -> i32 {
    super::exit_code(endings.iter().map(|ending| ending.standing(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, DnType, KeyPair};
    use time::Duration;

    fn certificate(lived: i64, left: i64) -> Vec<u8> {
        let now = OffsetDateTime::now_utc();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, "fleet");
        params.not_before = now - Duration::days(lived);
        params.not_after = now + Duration::days(left);
        let key = KeyPair::generate().expect("key");
        params.self_signed(&key).expect("sign").der().to_vec()
    }

    /// One reading of a certificate's end, and one line for it: what both `pki status` commands
    /// print over the same certificate.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn a_certificate_s_end_is_read_and_put_in_one_line() {
        let now = OffsetDateTime::now_utc();
        let file = Path::new("/etc/fleet/ca.pem");
        let fine = Ending::read(file, &certificate(1, 400)).expect("read");
        assert_eq!(fine.subject, "CN=fleet");
        assert_eq!(fine.days_left(now), 399);
        let line = fine.line(now);
        assert!(line.starts_with("ok        399 days  "), "{line}");
        assert!(
            line.ends_with(&format!("  CN=fleet  ({})", file.display())),
            "{line}"
        );
        assert!(line.contains(&format!("serial {}", fine.serial)), "{line}");

        let ending = Ending::read(file, &certificate(380, 20)).expect("read");
        assert!(
            ending.line(now).starts_with("ENDS SOON 19 days  "),
            "{}",
            ending.line(now)
        );
        let ended = Ending::read(file, &certificate(30, -2)).expect("read");
        assert!(
            ended.line(now).starts_with("ENDED     -2 days  "),
            "{}",
            ended.line(now)
        );
        assert_eq!(exit_code(&[fine.clone(), ending], now), 1);
        assert_eq!(exit_code(&[fine, ended], now), 2);

        let error = Ending::read(file, b"not a certificate").expect_err("refused");
        assert!(
            error.starts_with("cannot read /etc/fleet/ca.pem: "),
            "{error}"
        );
    }
}
