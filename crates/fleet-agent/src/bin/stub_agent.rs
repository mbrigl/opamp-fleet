//! A stub Managed Process for tests (ADR-0017): pure Rust so it behaves identically on Linux,
//! macOS, and Windows CI — no shell scripts.
//!
//! Behaviour, driven entirely by arguments:
//! - `--version` — print a version line the way real agents do (free text around a SemVer),
//!   then exit; what the plugins' version probe invokes.
//! - `--touch <path>` — write a marker file, then keep running. The marker holds this run's
//!   process id and every argument, one per line, so a test observes both *that* the stub ran
//!   and *with what*; a restart rewrites it with a fresh pid.
//! - `--exit-code <n> --exit-after-ms <m>` — exit with code `n` after `m` milliseconds, for
//!   crash-path tests. Without them the stub sleeps until it is killed.
//! - `--ignore-hup` (Unix only) — ignore `SIGHUP`, the way a daemon that reloads on it survives
//!   the signal; without the flag the default disposition terminates the stub. What the reload
//!   tests (ADR-0017) use for both sides of `reload-or-restart`.
//! - `--record-hup <path>` (Unix only) — survive `SIGHUP` and write `hups=<count>` to `path`
//!   each time one arrives, so a test sees that the signal reached this process, not only that
//!   the process is still there.
//!
//! A kind that knows its agent states the arguments itself (ADR-0017), so a test driving one
//! cannot pass the stub a flag. It writes them, one per line, into `<program>.args` beside the
//! copy of the stub it installed, and the stub adds them to its own.

use std::time::Duration;

/// How many `SIGHUP`s have arrived, counted by the handler `--record-hup` installs.
#[cfg(unix)]
static HUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
extern "C" fn count_hup(_: libc::c_int) {
    HUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(extra) =
        std::env::current_exe().and_then(|exe| std::fs::read_to_string(exe.with_extension("args")))
    {
        args.extend(extra.lines().map(str::to_string));
    }
    #[cfg(unix)]
    if args.iter().any(|a| a == "--ignore-hup") {
        // SAFETY: installs SIG_IGN for SIGHUP; no memory is touched.
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }
    #[cfg(unix)]
    let record_hup = args
        .iter()
        .position(|a| a == "--record-hup")
        .and_then(|i| args.get(i + 1))
        .cloned();
    #[cfg(unix)]
    if record_hup.is_some() {
        // SAFETY: the handler only increments an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGHUP, count_hup as *const () as libc::sighandler_t);
        }
    }
    if args.iter().any(|a| a == "--version") {
        println!("stub_agent version 9.9.9 (test build)");
        return;
    }
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };

    if let Some(path) = flag("--touch") {
        let mut marker = format!("pid={}\n", std::process::id());
        if let Ok(token) = std::env::var("OPAMP_SUPERVISOR_TOKEN") {
            marker.push_str(&format!("token={token}\n"));
        }
        for arg in &args {
            marker.push_str(arg);
            marker.push('\n');
        }
        // Whole or not at all: a test waits for the file to exist and then reads it, so it is
        // written beside its place and renamed into it.
        let partial = format!("{path}.partial");
        std::fs::write(&partial, marker).expect("write the marker file");
        std::fs::rename(&partial, path).expect("move the marker file into place");
    }

    let exit_code: Option<i32> = flag("--exit-code").and_then(|v| v.parse().ok());
    let exit_after: Option<u64> = flag("--exit-after-ms").and_then(|v| v.parse().ok());
    if let Some(code) = exit_code {
        std::thread::sleep(Duration::from_millis(exit_after.unwrap_or(0)));
        std::process::exit(code);
    }
    #[cfg(unix)]
    if let Some(path) = record_hup {
        let mut recorded = 0;
        loop {
            let hups = HUPS.load(std::sync::atomic::Ordering::SeqCst);
            if hups != recorded {
                std::fs::write(&path, format!("hups={hups}\n")).expect("write the hup record");
                recorded = hups;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
