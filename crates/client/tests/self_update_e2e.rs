use server::packages::{PackageStore, Platform};

/// The Platform this test's Client will report about itself (ADR-0019) — the Server offers only
/// the artifact that fits the machine, so a self-update test has to store one for this one.
fn this_host() -> Platform {
    Platform::new(std::env::consts::OS, std::env::consts::ARCH).expect("this host has a platform")
}
/// The version directory laid out before the update. Joined one component at a time, never as
/// `versions/<name>`: this test builds the Windows pointer with `mklink`, a `cmd` builtin that
/// reads an embedded `/` as the start of a switch.

    let app = server::agent_app(state.clone(), server::transport::Admission::open());
        // A clone that did not finish leaves a directory git will not clone into again ("already
        // exists and is not an empty directory") while `Cargo.toml` is still missing, so the two
        // branches below cannot be chosen by the presence of a file the reuse path needs. Ask
        // instead whether there is a repository to fetch into, and clear anything else out of the
        // way -- `target/` survives between CI runs, which is what makes a half-written clone here
        // outlive the run that abandoned it.
        let reusable = checkout.join(".git").exists() && checkout.join("Cargo.toml").exists();
        if !reusable && checkout.exists() {
            std::fs::remove_dir_all(&checkout).expect("clear an unusable checkout");
        }
        if reusable {
    let version_dir = root.join("versions").join(PREVIOUS_VERSION_DIR);
    // Offered the way an operator uploads a release: the number on the archive, without the commit
    // the build carries (ADR-0017). The staged binary reports the full string and must still be
    // recognised as this release — the failure that ADR exists for.
        .expect("put entry");
        std::fs::canonicalize(root.join("versions").join(PREVIOUS_VERSION_DIR))
        root.join("versions")
            .join(PREVIOUS_VERSION_DIR)
    // every time it is (re)started. Placed in the Supervisor's own `program/` directory and named
    // by a bare file name, which since ADR-0032 is the only shape a block may carry: a Managed
    // Process is always one this Client installed.
    let stub = {
        let program_dir = state_dir.join("supervisors/managed/program");
        std::fs::create_dir_all(&program_dir).expect("create the supervisor's program directory");
        let target = program_dir.join("stub-agent");
        std::fs::copy(env!("CARGO_BIN_EXE_stub_agent"), &target).expect("place the stub");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("make the stub executable");
        "stub-agent"
    };
    let full = version_of(&client);
        .unwrap_or_else(|| panic!("{full:?} is not a version"))
        .to_string();
        .expect("put entry");
