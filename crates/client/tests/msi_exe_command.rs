//! Regression test, and since ADR-0021 clause 8 also a guard on how the hazard was retired.
//!
//! `[INSTALLFOLDER]` always resolves with a trailing backslash, and the C runtime that builds a
//! process's argv treats a backslash before a quote as an escaped, literal quote — so
//! `--root &quot;[INSTALLFOLDER]&quot;` did not end the argument at the closing quote. The root
//! swallowed the rest of the command line, `service install` staged into an impossible path, and
//! every MSI install died with error 1722 ("A program run as part of the setup did not finish as
//! expected").
//!
//! The MSI no longer passes a root at all: `Program Files` holds the payload and the layout goes
//! under `%ProgramData%`, so no directory property reaches a command line. That is the stronger
//! fix — a doubled backslash is one edit away from being undoubled, while an argument that is not
//! there cannot be mis-split. Both facts are asserted below, because the day someone reintroduces
//! `--root` here, they reintroduce error 1722 with it.
//!
//! This test formats each ExeCommand the way msiexec does, splits it under the CRT's documented
//! rules, and feeds the result to the real CLI parser — pure string handling, so the Windows
//! failure is caught on every platform.
use std::path::Path;
/// A realistic resolution of `ProgramFiles64Folder\opamp-fleet`: a space in the path, and — the
/// whole point — the trailing backslash every Windows Installer directory property carries. Still
/// substituted below even though no command line names it any more, so that a reintroduced
/// `--root` would be parsed under the conditions that broke it.
const INSTALLFOLDER: &str = r"C:\Program Files\opamp-fleet\";
const PROGRAM: &str = r"C:\Program Files\opamp-fleet\supervisor.exe";
/// ADR-0021 clause 8: the MSI names neither root, so the install takes the platform defaults —
/// `%ProgramData%\opamp-fleet` for both halves — and `Program Files` holds only the payload.
///
/// This is also what keeps error 1722 retired. A directory property resolves with a trailing
/// backslash, and there is now no command line for one to reach.
#[test]
fn the_msi_names_no_root_so_no_directory_property_reaches_a_command_line() {
    for (id, command) in exe_commands() {
        assert!(
            !command.contains("[INSTALLFOLDER]"),
            "{id} passes a directory property again — the trailing backslash that resolves into \
             it is what produced error 1722 (ADR-0021 clause 8)"
        );
        assert!(
            !command.contains("--root") && !command.contains("--data-root"),
            "{id} names a root; the MSI takes the platform defaults so an archive install and a \
             packaged one put the same things in the same places"
        );
    }
    let args = install_args(parse(&exe_commands()["RegisterService"]));
    assert_eq!(args.root, None);
    assert_eq!(args.data_root, None);
}

        assert_eq!(standing.root, None);
        assert_eq!(withdrawn.root, None);
