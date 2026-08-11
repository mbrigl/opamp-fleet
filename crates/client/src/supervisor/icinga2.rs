
/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0009.
///
/// `binary` is not among them: the core takes it out and resolves it (ADR-0017), and what arrives
/// here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
            ProcessCommand::ApplyConfig { config, span } => match layout.validate().await {
                Ok(()) => ProcessCommand::ApplyConfig { config, span },
                    // The apply ends here rather than at the Runner, so this is where its trace
                    // learns why (ADR-0016).
                    crate::telemetry::failed(&span, &e);
    fn defaults(&self) -> crate::supervisor::ports::KindDefaults {
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        // The rule, not this host's answer: the default is the resolved FQDN where the resolver
        // has a qualified name to give, and the Supervisor's name where it has none. Asserting
        // the second alone passed on a machine without a domain and failed on every CI runner.
            layout.node_name,
            resolved_fqdn().map_or_else(|| "icinga2".to_string(), str::to_string),
            "the host's FQDN, or the Supervisor's name where none resolves"
