//! What a certificate signing request asks for, read with `x509-parser`: the adapter the proofs a
//! CSR carries (ADR-0021 clause 27, ADR-0039) are found through, on the Server and on a Gateway
//! alike.

/// The text after `prefix` of the first SAN URI a PEM certificate signing request asks for that
/// starts with it, or `None` when it asks for none. Nothing is verified: this reads what a request
/// carries.
///
/// # Errors
/// Returns an error when the request is not PEM or does not parse.
pub fn requested_uri(csr_pem: &[u8], prefix: &str) -> Result<Option<String>, String> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::prelude::FromDer as _;
    let (_, block) = x509_parser::pem::parse_x509_pem(csr_pem)
        .map_err(|_| "the certificate signing request is not PEM".to_string())?;
    let (_, request) =
        x509_parser::certification_request::X509CertificationRequest::from_der(&block.contents)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap_proof::URI_PREFIX;

    /// A request's SAN URI is found by its prefix, and a request without one has none.
    /// Verifies: ADR-0039
    #[test]
    fn a_requested_uri_is_found_by_its_prefix() {
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(vec!["edge".to_string()]).expect("params");
        params.subject_alt_names.push(rcgen::SanType::URI(
            format!("{URI_PREFIX}abc").try_into().expect("uri"),
        ));
        let csr = params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("pem");
        assert_eq!(
            requested_uri(csr.as_bytes(), URI_PREFIX),
            Ok(Some("abc".to_string()))
        );
        assert_eq!(
            requested_uri(csr.as_bytes(), crate::renewal::URI_PREFIX),
            Ok(None)
        );
        assert!(requested_uri(b"not pem", URI_PREFIX).is_err());
    }
}
