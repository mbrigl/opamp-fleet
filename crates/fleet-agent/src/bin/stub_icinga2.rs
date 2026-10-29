//! A stub `icinga2` for the tests of ADR-draft-icinga-2: pure Rust, so it behaves the same on
//! Linux, macOS, and Windows CI — the discipline `stub_agent` set.
//!
//! It answers the subcommands the Supervisor drives, and nothing else:
//!
//! - `--version` — the banner Icinga actually prints, `r`-prefixed and with a packaging revision,
//!   which is what the kind's own version parser exists for.
//! - `pki new-cert --key … --cert …` — writes both files, as the real one does, each new pair
//!   different from the last: a self-signed certificate is what the parent files a request under.
//! - `pki save-cert --trustedcert …` — writes the pinned parent certificate.
//! - `pki request --ca … --cert …` — writes the CA and rewrites the certificate: the signature.
//!   A self-signed certificate presented without `--ticket` is queued the way Icinga queues it:
//!   the CA is written, the certificate is not, the answer says *"is pending"* and the exit status
//!   is 0. The presented certificate is kept in `<cert>.pending`; a test stands in for
//!   `icinga2 ca sign` by copying it to `<cert>.approved`, and the same certificate presented again
//!   is then signed — a different one is queued anew, as a new fingerprint is on the parent.
//! - `pki verify --cert …` — succeeds unless the certificate says `expired`, and prints a
//!   `Valid Until` line the way the real one does: far in the future, or five days out when the
//!   certificate says `expiring`, which is how the renewal window is exercised.
//! - `daemon -C …` — validates: fails when the root configuration contains `INVALID`.
//! - `daemon …` — stays up until it is stopped, like a foreground daemon. It writes the Supervisor
//!   Endpoint token it was handed into `DataDir/endpoint-token`, as `set: <token>`, or `unset`
//!   where its environment holds none. With `NodeName=stubborn` it starts a worker of its own, as
//!   Icinga's umbrella does, both write their pids into `DataDir`, and both ignore `SIGTERM`, so
//!   only the escalation to `SIGKILL` ends them and a test can see whether the stop took the worker
//!   along. It outlives the reload's `SIGHUP`, as Icinga's umbrella does.
//!
//! On Unix it refuses to run at all — exiting as the dynamic linker does — unless `LD_LIBRARY_PATH`
//! names one tree's `lib/` and nothing else: the real binary links the Boost the tree carries, and
//! a host without it runs no subcommand the kind starts without that environment
//! (ADR-draft-icinga-2).
//!
//! The behaviours are steered by the arguments themselves rather than by the environment, so the
//! tests stay parallel-safe: a `--host` of `unreachable.example` is a parent that is down, one of
//! `echoing.example` refuses the ticket quoting it back, a root configuration containing
//! `INVALID` is one Icinga refuses, and a `NodeName` of `stubborn` is a daemon `SIGTERM` does not
//! end.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let has = |name: &str| args.iter().any(|a| a == name);

    #[cfg(unix)]
    // One directory, a tree's `lib/`: what `cargo test` puts there itself is a list, so the stub
    // tells the kind's environment apart from the one it would merely inherit.
    if !std::env::var("LD_LIBRARY_PATH")
        .is_ok_and(|dirs| !dirs.contains(':') && dirs.ends_with("/lib"))
    {
        eprintln!(
            "icinga2: error while loading shared libraries: libboost_coroutine.so.1.74.0: cannot \
             open shared object file: No such file or directory"
        );
        std::process::exit(127);
    }

    // The worker the daemon starts below: it runs until something ends it.
    if args.first().map(String::as_str) == Some(WORKER) {
        be_stubborn();
        if let Some(pid_file) = args.get(1) {
            let _ = std::fs::write(pid_file, std::process::id().to_string());
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    if has("--version") {
        println!("icinga2 - The Icinga 2 network monitoring daemon (version: r2.14.6-1)");
        return;
    }

    // A parent that is down, so the enrolment retry can be observed.
    let parent_down = value("--host").as_deref() == Some("unreachable.example");

    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("pki"), Some("new-cert")) => {
            let fresh = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            write(value("--key"), &format!("stub-key-{fresh}"));
            write(value("--cert"), &format!("stub-self-signed-{fresh}"));
        }
        (Some("pki"), Some("save-cert")) => {
            if parent_down {
                fail("Cannot connect to host");
            }
            write(value("--trustedcert"), "stub-parent-cert");
        }
        (Some("pki"), Some("request")) => {
            if parent_down {
                fail("Cannot connect to host");
            }
            if value("--host").as_deref() == Some("echoing.example") {
                fail(&format!(
                    "Invalid ticket '{}'",
                    value("--ticket").unwrap_or_default()
                ));
            }
            let cert = value("--cert").unwrap_or_default();
            let presented = std::fs::read_to_string(&cert).unwrap_or_default();
            let approved = std::fs::read_to_string(format!("{cert}.approved"))
                .is_ok_and(|approved| approved == presented);
            if presented.starts_with("stub-self-signed-") && !has("--ticket") && !approved {
                write(value("--ca"), "stub-ca");
                write(Some(format!("{cert}.pending")), &presented);
                println!(
                    "information/cli: !!! Certificate request for CN 'stub' is pending. Waiting \
                     for approval from the parent Icinga instance."
                );
                return;
            }
            // A counter beside the certificate, so a test can assert that enrolment ran *once*
            // without the environment carrying state between parallel tests.
            if !cert.is_empty() {
                let counter = format!("{cert}.requests");
                let count = std::fs::read_to_string(&counter)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                let _ = std::fs::write(counter, (count + 1).to_string());
            }
            write(value("--ca"), "stub-ca");
            write(value("--cert"), "stub-signed-cert");
        }
        (Some("pki"), Some("verify")) => {
            let stored = value("--cert")
                .and_then(|path| std::fs::read_to_string(path).ok())
                .unwrap_or_default();
            if stored.contains("expired") {
                fail("Certificate has expired");
            }
            // The real `pki verify` prints this and exits 0 even for a certificate whose validity
            // has run out — which is why the plugin reads the date rather than the exit status.
            let until = if stored.contains("expiring") {
                let soon = std::time::SystemTime::now() + std::time::Duration::from_secs(5 * 86400);
                format_gmt(soon)
            } else {
                "Aug 13 16:20:17 2041 GMT".to_string()
            };
            println!(" Valid Until:         {until}");
            println!("information/cli: OK: Certificate with CN 'stub' is signed by CA.");
        }
        (Some("daemon"), _) => {
            let invalid = value("-c")
                .and_then(|path| std::fs::read_to_string(path).ok())
                .is_some_and(|text| text.contains("INVALID"));
            if has("-C") {
                if invalid {
                    fail("Error: syntax error, unexpected T_IDENTIFIER");
                }
                println!("Finished validating the configuration file(s).");
                return;
            }
            // The foreground daemon: it runs until the Supervisor stops it — a stubborn one beside
            // a worker of its own.
            let defined = |name: &str| {
                args.iter()
                    .find_map(|arg| arg.strip_prefix(&format!("{name}=")))
                    .map(str::to_string)
            };
            if let Some(data) = defined("DataDir") {
                // "unset" and "set to empty" are told apart: the Runner is to set none.
                let token = std::env::var_os("OPAMP_SUPERVISOR_TOKEN").map_or_else(
                    || "unset".to_string(),
                    |token| format!("set: {}", token.to_string_lossy()),
                );
                write(Some(format!("{data}/endpoint-token")), &token);
            }
            if defined("NodeName").as_deref() == Some("stubborn") {
                be_stubborn();
                let Some(data) = defined("DataDir") else {
                    fail("no DataDir");
                };
                let data = std::path::Path::new(&data);
                let _ = std::fs::write(data.join("umbrella.pid"), std::process::id().to_string());
                let worker = std::env::current_exe().and_then(|exe| {
                    std::process::Command::new(exe)
                        .arg(WORKER)
                        .arg(data.join("worker.pid"))
                        .spawn()
                });
                if let Err(e) = worker {
                    fail(&format!("cannot start the worker: {e}"));
                }
            }
            loop {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        _ => fail("unknown command"),
    }
}

/// `Aug 13 16:20:17 2041 GMT`, the shape OpenSSL prints — computed without a date dependency,
/// since a stub may be crude where the thing it stands in for may not.
fn format_gmt(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days from the civil epoch back to a date (Howard Hinnant's algorithm, shifted to March).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);
    let name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(month - 1) as usize];
    format!(
        "{name} {day:>2} {:02}:{:02}:{:02} {year} GMT",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// The first argument the daemon starts its worker with.
const WORKER: &str = "__worker";

/// What `NodeName=stubborn` asks for: a process the polite stop does not end, and the reload's
/// `SIGHUP` does not end either.
fn be_stubborn() {
    #[cfg(unix)]
    // SAFETY: sets the disposition of two signals before any other thread exists.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
}

fn write(path: Option<String>, contents: &str) {
    if let Some(path) = path {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, contents);
    }
}

fn fail(reason: &str) -> ! {
    eprintln!("critical/cli: {reason}");
    std::process::exit(1);
}
