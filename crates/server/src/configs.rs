use opamp::attributes;
use opamp::proto::AgentDescription;
    /// The Baseline's `AgentConfigObject.role` (ADR-0025), travelling unchanged to the Agent.
    /// The Baseline's `AgentConfigObject.role` (ADR-0025); empty is top-level configuration.
        attributes::string_value(&description.identifying_attributes, key)
            .or_else(|| attributes::string_value(&description.non_identifying_attributes, key))
                .map(|(k, v)| attributes::string_attr(k, v))
