//! The `glpi` kind, whose whole block is `type` and `name` (ADR-draft-glpi-agent clause 1), driven
//! through `Plugin::start` the way the core drives it: the derived
//! program path is where the install puts the program, the version probe reaches it, a delivered
//! Configuration lands where the kind points its agent, and the directories the agent writes into
//! exist by the time it runs.
#![cfg(feature = "glpi")]

mod wrapped;

use fleet_agent::supervisor::ports::Plugin as _;

/// The GLPI Agent's whole block, likewise — and on both platforms, with the program found at the
/// place inside the tree that this platform's constant names.
///
/// The stub plays the program it is installed as: the bundled Perl on Windows, the AppImage's
/// entry point on Linux. Both answer `--version` with a banner of their own unless they are made
/// the agent as the daemon is, so the version reported here is the agent's only if the probe
/// starts it that way — and the daemon itself would exit at once if it were started otherwise.
/// Verifies: G-7, ADR-draft-glpi-agent
#[tokio::test]
async fn a_two_line_glpi_block_runs_reports_and_applies() {
    let plugin = fleet_agent::supervisor::glpi::GlpiPlugin::default();
    assert!(
        plugin.defaults().program_path.is_some(),
        "the GLPI Agent is a tree package on both platforms"
    );
    let mut harness = wrapped::start(&plugin, "glpi", &[]);
    assert_eq!(
        wrapped::wait_for_running_version(&mut harness).await.0,
        "1.19.0"
    );

    // What the agent writes into, which it does not create itself and exits without: the block
    // says nothing about it, and the host was not prepared for it.
    for (dir, what) in [
        ("agent-state", "the agent's own state directory"),
        ("agent-log", "the agent's log directory"),
    ] {
        let dir = harness.dir.path().join(dir);
        wrapped::wait_for(&dir, what).await;
        assert!(dir.is_dir(), "{what}");
    }

    std::fs::write(
        harness.config_dir().join("glpi-agent-conf"),
        "tag = fleet\n",
    )
    .expect("the entry");
    assert_eq!(wrapped::apply_config(&mut harness).await.0, Ok(()));
    wrapped::stop(harness).await;
}
