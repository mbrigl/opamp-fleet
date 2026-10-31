//! What a certificate signing request asks for, read with `x509-parser`: the adapter the proofs a
//! CSR carries (ADR-0015 clauses 18, 20 and 21) are found through, on the Server and on a Gateway
//! alike.
//!
//! A request arrives as PEM and is decoded once, strictly, by [`der`]: every proof, every claim and
//! the key that is signed are then read from those same bytes. Two PEM readers can pick different
//! blocks out of one input, and a proof read through one would then cover a key the other hands
//! on to be signed.

/// The one PEM label a certificate signing request is taken under.
const LABEL: &str = "CERTIFICATE REQUEST";

/// The DER of a PEM certificate signing request: exactly one `CERTIFICATE REQUEST` block with
/// nothing but whitespace around it and nothing on its boundaries' lines, whose content parses as
/// a request with nothing after it.
///
/// # Errors
/// Returns the `BadRequest` text for anything else — text before or after the block or on a
/// boundary's line, a second block, another label, or content that does not parse.
pub fn der(csr_pem: &[u8]) -> Result<Vec<u8>, String> {
    use x509_parser::prelude::FromDer as _;
    let begin = format!("-----BEGIN {LABEL}-----");
    let end = format!("-----END {LABEL}-----");
    let not_one = || {
        format!(
            "the certificate signing request is not exactly one PEM block labelled {LABEL} with \
             nothing but whitespace around it"
        )
    };
    let text = csr_pem.trim_ascii();
    let body = text
        .strip_prefix(begin.as_bytes())
        .and_then(|rest| rest.strip_suffix(end.as_bytes()))
        .ok_or_else(not_one)?;
    // A line break straight after the BEGIN boundary and straight before the END boundary: no
    // text on either boundary's line.
    let line_break = |b: &u8| matches!(b, b'\n' | b'\r');
    if !body.first().is_some_and(line_break) || !body.last().is_some_and(line_break) {
        return Err(not_one());
    }
    // Base64 and line breaks only: no second boundary, no header, no other text.
    if !body.iter().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=') || b.is_ascii_whitespace()
    }) {
        return Err(not_one());
    }
    let unparsed = || "the certificate signing request does not parse".to_string();
    let (rest, block) = x509_parser::pem::parse_x509_pem(text).map_err(|_| unparsed())?;
    if block.label != LABEL || !rest.trim_ascii().is_empty() {
        return Err(not_one());
    }
    match x509_parser::certification_request::X509CertificationRequest::from_der(&block.contents) {
        Ok(([], _)) => Ok(block.contents),
        _ => Err(unparsed()),
    }
}

/// The text after `prefix` of the first SAN URI a certificate signing request (DER, from [`der`])
/// asks for that starts with it, or `None` when it asks for none. Nothing is verified: this reads
/// what a request carries.
///
/// # Errors
/// Returns an error when the request does not parse.
pub fn requested_uri(csr_der: &[u8], prefix: &str) -> Result<Option<String>, String> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::FromDer as _;
    let (_, request) =
        x509_parser::certification_request::X509CertificationRequest::from_der(csr_der)
            .map_err(|_| "the certificate signing request does not parse".to_string())?;
    Ok(request.requested_extensions().and_then(|mut extensions| {
        extensions.find_map(|extension| match extension {
            ParsedExtension::SubjectAlternativeName(san) => {
                san.general_names.iter().find_map(|name| match name {
                    GeneralName::URI(uri) => uri.strip_prefix(prefix).map(str::to_string),
                    _ => None,
                })
            }
            _ => None,
        })
    }))
}

/// The two proofs a request may carry, each a requested SAN URI under a prefix of its own
/// (ADR-0015 clauses 18 and 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    Bootstrap,
    Renewal,
}

impl ProofKind {
    fn prefix(self) -> &'static str {
        match self {
            ProofKind::Bootstrap => crate::bootstrap_proof::URI_PREFIX,
            ProofKind::Renewal => crate::renewal::URI_PREFIX,
        }
    }

    fn other(self) -> Self {
        match self {
            ProofKind::Bootstrap => ProofKind::Renewal,
            ProofKind::Renewal => ProofKind::Bootstrap,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ProofKind::Bootstrap => "bootstrap",
            ProofKind::Renewal => "renewal",
        }
    }
}

/// What a proof carries: the certificate it rests on (DER), and the signature that certificate's
/// key made over the request's new key. Nothing is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub certificate: Vec<u8>,
    pub signature: Vec<u8>,
}

/// Why a proof could not be read from a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofError {
    /// The request does not parse.
    Request,
    /// The request carries both proofs (ADR-0015 clause 21).
    Both,
    /// The proof of this kind is not base64url.
    NotBase64(ProofKind),
    /// The proof of this kind does not parse.
    Torn(ProofKind),
}

/// What a request carrying both proofs is answered (ADR-0015 clause 21).
pub const BOTH_PROOFS: &str =
    "the certificate signing request carries both a renewal proof and a bootstrap proof";

impl std::fmt::Display for ProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProofError::Request => f.write_str("the certificate signing request does not parse"),
            ProofError::Both => f.write_str(BOTH_PROOFS),
            ProofError::NotBase64(kind) => write!(
                f,
                "the {} proof does not hold: it is not base64url",
                kind.name()
            ),
            ProofError::Torn(kind) => {
                write!(
                    f,
                    "the {} proof does not hold: it does not parse",
                    kind.name()
                )
            }
        }
    }
}

/// The proof of `kind` a certificate signing request (DER, from [`der`]) carries, or `None` when
/// it carries none: the one way the Server and a Gateway read either proof. A request that carries
/// the other proof beside it is refused, so that neither proof stands beside the other.
///
/// # Errors
/// Returns why the proof could not be read: the request does not parse, it carries both proofs,
/// or the proof is not base64url or does not parse.
pub fn proof(csr_der: &[u8], kind: ProofKind) -> Result<Option<Proof>, ProofError> {
    use base64::Engine as _;
    let Some(encoded) = requested_uri(csr_der, kind.prefix()).map_err(|_| ProofError::Request)?
    else {
        return Ok(None);
    };
    if requested_uri(csr_der, kind.other().prefix())
        .map_err(|_| ProofError::Request)?
        .is_some()
    {
        return Err(ProofError::Both);
    }
    let content = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ProofError::NotBase64(kind))?;
    let (certificate, signature) =
        crate::renewal::decode(&content).ok_or(ProofError::Torn(kind))?;
    Ok(Some(Proof {
        certificate: certificate.to_vec(),
        signature: signature.to_vec(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap_proof::URI_PREFIX;

    fn csr(uri: &str) -> String {
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(vec!["edge".to_string()]).expect("params");
        params
            .subject_alt_names
            .push(rcgen::SanType::URI(uri.try_into().expect("uri")));
        params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("pem")
    }

    /// A request's SAN URI is found by its prefix, and a request without one has none.
    /// Verifies: ADR-0015
    #[test]
    fn a_requested_uri_is_found_by_its_prefix() {
        let csr = der(csr(&format!("{URI_PREFIX}abc")).as_bytes()).expect("one block");
        assert_eq!(requested_uri(&csr, URI_PREFIX), Ok(Some("abc".to_string())));
        assert_eq!(requested_uri(&csr, crate::renewal::URI_PREFIX), Ok(None));
        assert!(requested_uri(b"not der", URI_PREFIX).is_err());
    }

    /// A proof is read by its kind, the other kind beside it is refused in either order, and a
    /// proof that is not base64url or does not parse is refused naming its kind.
    /// Verifies: ADR-0015
    #[test]
    fn a_proof_is_read_by_its_kind_and_never_beside_the_other() {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(crate::renewal::encode(b"cert", b"sig"));
        let request = |uris: &[String]| {
            let key = rcgen::KeyPair::generate().expect("key");
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
            for uri in uris {
                params
                    .subject_alt_names
                    .push(rcgen::SanType::URI(uri.as_str().try_into().expect("uri")));
            }
            let pem = params
                .serialize_request(&key)
                .expect("csr")
                .pem()
                .expect("pem");
            der(pem.as_bytes()).expect("one block")
        };
        let bootstrap = format!("{URI_PREFIX}{encoded}");
        let renewal = format!("{}{encoded}", crate::renewal::URI_PREFIX);
        let carried = Proof {
            certificate: b"cert".to_vec(),
            signature: b"sig".to_vec(),
        };

        let one = request(std::slice::from_ref(&bootstrap));
        assert_eq!(proof(&one, ProofKind::Bootstrap), Ok(Some(carried.clone())));
        assert_eq!(proof(&one, ProofKind::Renewal), Ok(None));
        let one = request(std::slice::from_ref(&renewal));
        assert_eq!(proof(&one, ProofKind::Renewal), Ok(Some(carried)));
        assert_eq!(proof(&one, ProofKind::Bootstrap), Ok(None));

        for both in [
            request(&[bootstrap.clone(), renewal.clone()]),
            request(&[renewal, bootstrap]),
        ] {
            for kind in [ProofKind::Bootstrap, ProofKind::Renewal] {
                assert_eq!(proof(&both, kind), Err(ProofError::Both));
            }
        }

        let torn = request(&[format!("{URI_PREFIX}AAAA")]);
        assert_eq!(
            proof(&torn, ProofKind::Bootstrap),
            Err(ProofError::Torn(ProofKind::Bootstrap))
        );
        let not_base64 = request(&[format!("{}a+b", crate::renewal::URI_PREFIX)]);
        let refused = proof(&not_base64, ProofKind::Renewal).expect_err("not base64url");
        assert_eq!(
            refused.to_string(),
            "the renewal proof does not hold: it is not base64url"
        );
        assert_eq!(
            proof(b"not der", ProofKind::Bootstrap),
            Err(ProofError::Request)
        );
    }

    /// One block with whitespace around it is read; text before or after it or on a boundary's
    /// line, a second block, another label, a header, and content that is no request are refused — what makes every
    /// reader of a request read the same one (ADR-0015 clauses 18 and 21).
    /// Verifies: ADR-0021
    /// Verifies: ADR-0015
    #[test]
    fn a_request_is_exactly_one_pem_block() {
        let one = csr("urn:example:one");
        let other = csr("urn:example:other");
        assert!(der(one.as_bytes()).is_ok());
        assert!(der(format!("\n  {one}\r\n\n").as_bytes()).is_ok());
        assert!(der(one.replace('\n', "\r\n").as_bytes()).is_ok());
        let begin = "-----BEGIN CERTIFICATE REQUEST-----";
        let end = "-----END CERTIFICATE REQUEST-----";
        let certificate = one.replace("CERTIFICATE REQUEST", "CERTIFICATE");
        let header = one.replacen("-----\n", "-----\nProc-Type: 4,ENCRYPTED\n\n", 1);
        for refused in [
            format!("x{one}"),
            format!("x{one}{other}"),
            format!("{one}{other}"),
            format!("{one}\n{other}"),
            format!("{one}trailing"),
            certificate,
            header,
            one.replacen(begin, &format!("{begin} junk"), 1),
            one.replacen(&format!("{begin}\n"), &format!("{begin}AAAA\n"), 1),
            one.replacen(&format!("\n{end}"), end, 1),
            one.replacen(&format!("\n{end}"), &format!("\n{end} junk"), 1),
            "-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----"
                .to_string(),
            "not pem".to_string(),
        ] {
            assert!(der(refused.as_bytes()).is_err(), "{refused}");
        }
    }
}
