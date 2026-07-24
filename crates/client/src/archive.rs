use tracing::debug;

/// How a `.7z` says "Unix mode" — 7-Zip's own convention, which this module both reads and writes.
///
/// The format carries *Windows* attributes. p7zip stashed the Unix `st_mode` in the high 16 bits
/// and set bit 15 to say it had done so, and 7-Zip on Linux and macOS still writes exactly that
/// (`Attrib = (a << 16) | FILE_ATTRIBUTE_UNIX_EXTENSION`) while the Windows build does not. Reading
/// it is how [`list_7z`] spots a symbolic link that the Windows bits alone would not reveal; writing
/// it is how the packer marks a program executable, so `7z x` yields something that runs.
mod unix_attributes {
    /// Bit 15: the high half carries a Unix mode. On Windows the same bit means
    /// `FILE_ATTRIBUTE_INTEGRITY_STREAM`, which is why 7-Zip only ever writes it off Windows.
    pub const EXTENSION: u32 = 0x8000;
    /// A symbolic link by another name — a Windows reparse point.
    pub const REPARSE_POINT: u32 = 0x400;
    /// The file-type field of a Unix mode.
    pub const S_IFMT: u32 = 0o170000;
    /// The file-type value saying "symbolic link".
    pub const S_IFLNK: u32 = 0o120000;
    /// The file-type value saying "regular file".
    pub const S_IFREG: u32 = 0o100000;
}

/// The `windows_attributes` value that carries `mode` as a regular file's Unix mode.
///
/// The full `st_mode` goes in, file-type bits and all, because that is what `stat` hands 7-Zip and
/// what the link check on the way back out reads. Used by `opamp-package-sign pack`; kept here
/// beside the code that decodes it, so the convention has one definition rather than two that can
/// drift apart.
#[must_use]
pub const fn unix_mode_attributes(mode: u32) -> u32 {
    ((unix_attributes::S_IFREG | mode) << 16) | unix_attributes::EXTENSION
}

/// Counts one member that is not written, and names it.
///
/// The count alone is what the install line reports, and a count answers "how many files did this
/// archive have that the tree does not" — not "which one is missing", which is the question asked
/// when the program runs and cannot find its plugin. The name goes to `debug!` rather than to the
/// summary: an archive may hold thousands of members outside the prefix, and a level nobody has
/// turned on costs nothing while an operator chasing a missing file can turn it on for one install.
///
/// The reason is not carried because there is exactly one: the member does not sit under the
/// directory the program was found in (ADR-0019). Every other refusal — a traversing path, a
/// member past the size or count ceiling, an encrypted entry — fails the whole archive with an
/// error that reaches the Server, and none of them reaches here.
fn skipped(member: &Path, summary: &mut TreeSummary) {
    summary.skipped += 1;
    debug!(member = %as_member(member), "member outside the program's directory; not unpacked");
}

            skipped(&member, &mut summary);
                skipped(&member, &mut summary);
    // Attribute bits a member may not carry. A reparse point is a symbolic link by another name;
    // the unix-extension bit puts a mode in the high half, and a mode may say link too.
    use unix_attributes::{EXTENSION, REPARSE_POINT, S_IFLNK, S_IFMT};
            let is_link = attributes & REPARSE_POINT != 0
                || (attributes & EXTENSION != 0 && (attributes >> 16) & S_IFMT == S_IFLNK);
            skipped(&member, &mut summary);
