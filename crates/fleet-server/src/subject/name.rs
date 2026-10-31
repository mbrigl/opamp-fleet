//! A subject read from an X.509 name and written into one, by the attributes of
//! [`subject`](super): the text an operator reads, the values a Host carries, the values a
//! bootstrap certificate passes on under the rules it was issued by, and the name a certificate is
//! signed with.

use super::{Attribute, SubjectValues};

/// A name as text, for the operator to read: `C=AT, O=…, organizationIdentifier=…, CN=…`. The
/// parser's own registry knows no `organizationIdentifier` and would print its OID.
pub fn name_text(name: &x509_parser::x509::X509Name<'_>) -> String {
    use x509_parser::oid_registry::{OidEntry, OidRegistry};
    static REGISTRY: std::sync::LazyLock<OidRegistry<'static>> = std::sync::LazyLock::new(|| {
        let mut registry = OidRegistry::default().with_all_crypto().with_x509();
        let identifier =
            x509_parser::oid_registry::Oid::from(&Attribute::OrganizationIdentifier.arcs())
                .expect("a fixed OID");
        registry.insert(
            identifier,
            OidEntry::new("organizationIdentifier", "X.520 organizationIdentifier"),
        );
        registry
    });
    name.to_string_with_registry(&REGISTRY)
        .unwrap_or_else(|_| name.to_string())
}

/// The values of a subject a Host carries (ADR-0024 clause 15), each the first of its attribute.
/// A value that is not a string is left out.
pub fn subject_values(name: &x509_parser::x509::X509Name<'_>) -> SubjectValues {
    let mut values = SubjectValues::default();
    for attribute in Attribute::CARRIED {
        let first = name
            .iter_attributes()
            .filter(|entry| entry.attr_type().to_id_string() == attribute.oid())
            .find_map(|entry| entry.as_str().ok())
            .unwrap_or_default();
        attribute.set(&mut values, first.to_string());
    }
    values
}

/// The six values a bootstrap certificate's subject carries into a host certificate, read under
/// the rules they were issued by (ADR-0022 clause 1 and ADR-0015 clause 23): each attribute at most once, a
/// country of two upper-case letters as a `PrintableString`, every other value of 1 to 64
/// characters without a control character as a `UTF8String` or a `PrintableString`, and an
/// organization and an organizational unit present. Every other attribute, the common name
/// included, is passed over. The certificate may come from a foreign PKI, so nothing in it is
/// taken on trust.
///
/// # Errors
/// Returns why the subject cannot be carried, naming the attribute.
pub fn carried_subject(name: &x509_parser::x509::X509Name<'_>) -> Result<SubjectValues, String> {
    use x509_parser::der_parser::asn1_rs::Tag;
    let mut values = SubjectValues::default();
    let mut seen = Vec::new();
    for entry in name.iter_attributes() {
        let oid = entry.attr_type().to_id_string();
        let Some(attribute) = Attribute::CARRIED
            .into_iter()
            .find(|attribute| attribute.oid() == oid)
        else {
            continue;
        };
        let key = attribute.key();
        if seen.contains(&attribute) {
            return Err(format!("{key} appears more than once"));
        }
        seen.push(attribute);
        // The country is a PrintableString. Every other value may be a UTF8String or the
        // PrintableString older PKIs write, a subset of it; it is carried as a UTF8String
        // (ADR-0015 clause 23).
        let tag = entry.attr_value().tag();
        let (fits, encoding) = if attribute.printable() {
            (tag == Tag::PrintableString, "PrintableString")
        } else {
            (
                tag == Tag::Utf8String || tag == Tag::PrintableString,
                "UTF8String or PrintableString",
            )
        };
        if !fits {
            return Err(format!("{key} is not encoded as a {encoding}"));
        }
        let value = entry
            .as_str()
            .map_err(|_| format!("{key} is not a {encoding}"))?;
        attribute.check(value)?;
        attribute.set(&mut values, value.to_string());
    }
    // Every Host names a customer and a site its Selectors can rely on (ADR-0022 clause 1).
    for attribute in [Attribute::Organization, Attribute::OrganizationalUnit] {
        if attribute.of(&values).is_empty() {
            return Err(format!("{} is missing", attribute.key()));
        }
    }
    Ok(values)
}

/// A subject of `attributes` in their order, each in the encoding ADR-0022 clause 1 gives it.
///
/// # Errors
/// Returns an error for a country that a `PrintableString` cannot hold.
pub fn distinguished_name(
    attributes: &[(Attribute, String)],
) -> Result<rcgen::DistinguishedName, String> {
    let mut name = rcgen::DistinguishedName::new();
    for (attribute, value) in attributes {
        let kind = match attribute {
            Attribute::Country => rcgen::DnType::CountryName,
            Attribute::State => rcgen::DnType::StateOrProvinceName,
            Attribute::Locality => rcgen::DnType::LocalityName,
            Attribute::Organization => rcgen::DnType::OrganizationName,
            Attribute::OrganizationalUnit => rcgen::DnType::OrganizationalUnitName,
            Attribute::OrganizationIdentifier => rcgen::DnType::CustomDnType(attribute.arcs()),
            Attribute::CommonName => rcgen::DnType::CommonName,
        };
        let value = if attribute.printable() {
            rcgen::DnValue::PrintableString(
                rcgen::string::PrintableString::try_from(value.as_str())
                    .map_err(|_| format!("{} {value:?} is no PrintableString", attribute.key()))?,
            )
        } else {
            rcgen::DnValue::Utf8String(value.clone())
        };
        name.push(kind, value);
    }
    Ok(name)
}
