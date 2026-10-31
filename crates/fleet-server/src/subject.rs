//! The values a certificate's subject carries (ADR-0022 clause 1, ADR-0024 clauses 15 and 18):
//! the attributes, each with its object identifier, written once here, the rules a value meets,
//! and the values a Host carries. Reading them from an X.509 name and writing one is
//! [`name`].

use serde::{Deserialize, Serialize};

pub mod name;

/// The longest value of a subject attribute: RFC 5280's upper bound for the names that have one.
pub const MAX_VALUE_CHARS: usize = 64;

/// The values of a client certificate's subject a Host carries into the effective description of
/// the Agents that arrive over it directly (ADR-0024 clauses 15 and 18). Each holds the first value
/// of its attribute in the subject; empty when the subject has none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectValues {
    /// `C`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub country: String,
    /// `ST`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// `L`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub locality: String,
    /// `O`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organization: String,
    /// `OU`, which the description calls the site.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organizational_unit: String,
    /// `organizationIdentifier` (2.5.4.97).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organization_identifier: String,
}

impl SubjectValues {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attributes().next().is_none()
    }

    /// The `fleet.host.*` attributes these values are (ADR-0024 clause 18), the empty ones left
    /// out.
    pub fn attributes(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("fleet.host.country", self.country.as_str()),
            ("fleet.host.state", self.state.as_str()),
            ("fleet.host.locality", self.locality.as_str()),
            ("fleet.host.organization", self.organization.as_str()),
            ("fleet.host.site", self.organizational_unit.as_str()),
            (
                "fleet.host.organization_id",
                self.organization_identifier.as_str(),
            ),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
    }
}

/// An attribute of a bootstrap certificate's subject, in the order a certificate lists them
/// (ADR-0022 clause 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribute {
    Country,
    State,
    Locality,
    Organization,
    OrganizationalUnit,
    OrganizationIdentifier,
    CommonName,
}

impl Attribute {
    /// The six attributes a host certificate carries over from its bootstrap certificate, in
    /// certificate order (ADR-0022 clauses 1, 11 and 12 and ADR-0015 clause 23).
    pub const CARRIED: [Attribute; 6] = [
        Attribute::Country,
        Attribute::State,
        Attribute::Locality,
        Attribute::Organization,
        Attribute::OrganizationalUnit,
        Attribute::OrganizationIdentifier,
    ];

    /// The name a request, a list and a refusal give it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Attribute::Country => "country",
            Attribute::State => "state",
            Attribute::Locality => "locality",
            Attribute::Organization => "organization",
            Attribute::OrganizationalUnit => "organizational_unit",
            Attribute::OrganizationIdentifier => "organization_identifier",
            Attribute::CommonName => "common_name",
        }
    }

    /// Its object identifier, X.520.
    #[must_use]
    pub fn oid(self) -> &'static str {
        match self {
            Attribute::Country => "2.5.4.6",
            Attribute::State => "2.5.4.8",
            Attribute::Locality => "2.5.4.7",
            Attribute::Organization => "2.5.4.10",
            Attribute::OrganizationalUnit => "2.5.4.11",
            Attribute::OrganizationIdentifier => "2.5.4.97",
            Attribute::CommonName => "2.5.4.3",
        }
    }

    /// Its object identifier as arcs, for a writer that takes them.
    #[must_use]
    pub fn arcs(self) -> Vec<u64> {
        self.oid()
            .split('.')
            .map(|arc| arc.parse().expect("an arc of a fixed OID"))
            .collect()
    }

    /// Whether a certificate carries it as a `PrintableString`; every other attribute is a
    /// `UTF8String`.
    #[must_use]
    pub fn printable(self) -> bool {
        self == Attribute::Country
    }

    /// Checks a value against ADR-0022 clause 1: a country of exactly two upper-case letters,
    /// any other value 1 to [`MAX_VALUE_CHARS`] characters without a control character.
    ///
    /// # Errors
    /// Returns why the value is refused, naming the attribute.
    pub fn check(self, value: &str) -> Result<(), String> {
        let name = self.key();
        if self == Attribute::Country {
            if value.len() == 2 && value.bytes().all(|b| b.is_ascii_uppercase()) {
                return Ok(());
            }
            return Err(format!(
                "{name} must be two upper-case letters, an ISO 3166-1 alpha-2 code"
            ));
        }
        let chars = value.chars().count();
        if chars == 0 || chars > MAX_VALUE_CHARS {
            return Err(format!("{name} must be 1 to {MAX_VALUE_CHARS} characters"));
        }
        if value.chars().any(char::is_control) {
            return Err(format!("{name} must not carry a control character"));
        }
        Ok(())
    }

    /// Its value in `values`; empty when they hold none.
    #[must_use]
    pub fn of(self, values: &SubjectValues) -> &str {
        match self {
            Attribute::Country => &values.country,
            Attribute::State => &values.state,
            Attribute::Locality => &values.locality,
            Attribute::Organization => &values.organization,
            Attribute::OrganizationalUnit => &values.organizational_unit,
            Attribute::OrganizationIdentifier => &values.organization_identifier,
            Attribute::CommonName => "",
        }
    }

    /// Puts `value` into `values` as this attribute's; a common name has no place there.
    pub fn set(self, values: &mut SubjectValues, value: String) {
        let slot = match self {
            Attribute::Country => &mut values.country,
            Attribute::State => &mut values.state,
            Attribute::Locality => &mut values.locality,
            Attribute::Organization => &mut values.organization,
            Attribute::OrganizationalUnit => &mut values.organizational_unit,
            Attribute::OrganizationIdentifier => &mut values.organization_identifier,
            Attribute::CommonName => return,
        };
        *slot = value;
    }
}

/// The attributes `values` hold, in certificate order, ending with `common_name`: the subject of
/// a host certificate (ADR-0022 clause 11).
#[must_use]
pub fn carried_attributes(values: &SubjectValues, common_name: &str) -> Vec<(Attribute, String)> {
    let mut attributes: Vec<(Attribute, String)> = Attribute::CARRIED
        .into_iter()
        .filter(|attribute| !attribute.of(values).is_empty())
        .map(|attribute| (attribute, attribute.of(values).to_string()))
        .collect();
    attributes.push((Attribute::CommonName, common_name.to_string()));
    attributes
}
