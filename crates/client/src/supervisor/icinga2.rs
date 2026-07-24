
/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0025.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fn defaults(&self) -> crate::supervisor::ports::KindDefaults {
    }

        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
        Ok(commands)
    }
        // The rule, not this host's answer: the default is the resolved FQDN where the resolver
        // has a qualified name to give, and the Supervisor's name where it has none. Asserting
        // the second alone passed on a machine without a domain and failed on every CI runner.
            layout.node_name,
            resolved_fqdn().map_or_else(|| "icinga2".to_string(), str::to_string),
            "the host's FQDN, or the Supervisor's name where none resolves"
