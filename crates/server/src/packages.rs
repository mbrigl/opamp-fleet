            os: token(os, "os", opamp::attributes::canonical_os)?,
            arch: token(arch, "arch", opamp::attributes::canonical_arch)?,
        // Non-identifying first: that is where an Agent reports its platform, and an identifying
        // copy is the fallback rather than the answer.
            opamp::attributes::string_value(&description.non_identifying_attributes, key).or_else(
                || opamp::attributes::string_value(&description.identifying_attributes, key),
            )
        Platform::new(
            attribute(opamp::attributes::OS_TYPE)?,
            attribute(opamp::attributes::HOST_ARCH)?,
        )
        .ok()
fn token(raw: &str, what: &str, canonicalise: fn(&str) -> &str) -> Result<String, String> {
    // The spelling table is the Client's too (ADR-0011): what an Agent reports and what an artifact
    // is stored under have to fold onto the same token, or the offer misses.
    let canonical = canonicalise(&lowered).to_string();
    opamp::attributes::string_value(
        &description?.identifying_attributes,
    opamp::attributes::string_value(
        &description?.identifying_attributes,
        opamp::attributes::SERVICE_NAME,
    )
