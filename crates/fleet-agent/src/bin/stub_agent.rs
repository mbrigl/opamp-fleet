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
//!
//! Installed under the name the `glpi` kind runs (ADR-draft-glpi-agent clause 2), the stub answers
//! as that program does, so a test can tell the agent from what merely shares its file:
//! - as `AppRun`, the AppImage's entry point, it is the GLPI Agent only after
//!   `--script=glpi-agent`; without it, it is the AppImage's installer, whose `--version` names
//!   the installer and which runs nothing else unless it is root.
//! - as `glpi-agent.exe`, the bundled Perl, it reads Perl's switches up to the first argument that
//!   is none, which is the script. Without a script, `--version` prints Perl's own banner. With
//!   one, it is the GLPI Agent only if the script is the tree's `perl/bin/glpi-agent`, the tree's
//!   `perl/agent` is among the `-I` roots, and it was started in the tree's root, as upstream's
//!   portable launcher starts it; otherwise it fails as Perl does on a module it cannot find.

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
    if let Some(banner) = glpi::persona(&args) {
        // Answered: a version line, or the failure the program would have met.
        println!("{banner}");
        return;
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

/// The GLPI Agent's two programs, as the stub plays them (see the module's header).
mod glpi {
    use std::path::{Path, PathBuf};

    /// The version line the GLPI Agent prints for `--version`: the 1.19 AppImage's, with the
    /// package revision of upstream's build, played on both platforms.
    const AGENT: &str = "GLPI Agent (1.19-1)";

    /// What the stub prints and exits on, when it is installed as one of the GLPI programs and
    /// asked for a version — or when it is started in a way that program would not run. `None`
    /// lets the stub run on as a daemon.
    pub fn persona(args: &[String]) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let version = args.iter().any(|a| a == "--version");
        match exe.file_stem()?.to_str()? {
            "AppRun" => {
                if args.iter().any(|a| a == "--script=glpi-agent") {
                    return version.then(|| AGENT.to_string());
                }
                if version {
                    return Some("GLPI Agent AppImage installer v1.19".to_string());
                }
                fail("GLPI Agent AppImage v1.19 can only be run as root when installing")
            }
            "glpi-agent" => {
                let Some(script) = args.iter().position(|a| !a.starts_with('-')) else {
                    if version {
                        return Some(
                            "This is perl 5, version 38, subversion 2 (v5.38.2) built for \
                             MSWin32-x64-multi-thread"
                                .to_string(),
                        );
                    }
                    fail("perl: no script")
                };
                // `perl/bin/glpi-agent.exe`, three levels below the tree's root.
                let tree = exe.parent()?.parent()?.parent()?;
                let started_in_the_root = std::env::current_dir().is_ok_and(|dir| same(&dir, tree));
                let agent_on_inc = args[..script].iter().any(|a| {
                    a.strip_prefix("-I")
                        .is_some_and(|dir| below(Path::new(dir), &["perl", "agent"], tree))
                });
                let the_agent = below(
                    Path::new(&args[script]),
                    &["perl", "bin", "glpi-agent"],
                    tree,
                );
                if !(started_in_the_root && agent_on_inc && the_agent) {
                    fail("Can't locate setup.pm in @INC")
                }
                version.then(|| AGENT.to_string())
            }
            _ => None,
        }
    }

    /// Whether `path` is `tree` followed by `names`, whatever separators and spelling of the root
    /// it was written with.
    fn below(path: &Path, names: &[&str], tree: &Path) -> bool {
        let mut base: PathBuf = path.to_path_buf();
        for name in names.iter().rev() {
            if base.file_name().and_then(|n| n.to_str()) != Some(*name) {
                return false;
            }
            base.pop();
        }
        same(&base, tree)
    }

    fn same(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    fn fail(said: &str) -> ! {
        eprintln!("{said}");
        std::process::exit(2);
    }
}
