//! What `package fetch` reads out of the bytes Debian's archives send for the Icinga 2 repack
//! (CLT-icinga2-repacked-kind §18): the signed `Release` text, the `Packages` index it vouches
//! for, and, out of each package, its maintainer script and the ELF files it carries.
//!
//! Every function here reads downloaded bytes, so each has a fuzz target that runs in CI
//! (specification Q-2): `fuzz/fuzz_targets/deb_release.rs`, `fuzz/fuzz_targets/deb_index.rs`,
//! `fuzz/fuzz_targets/deb_alternatives.rs` and `fuzz/fuzz_targets/elf_needed.rs`. Their size
//! limit is enforced before this module sees the bytes, by the bounded reads of the module above
//! and by the sizes the signed text states.
//!
//! `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the fuzz crate compiles this
//! file into its targets by its path, as it compiles `upstream.rs` (docs/CONVENTIONS.md). That is
//! why this module names nothing of `crate::` and uses only `std` and `time`, which the fuzz crate
//! depends on too.

/// One stanza of a Debian `Packages` index — the fields this tool needs and no others.
#[derive(Debug)]
pub struct DebEntry {
    /// The repository the index was read from, which `filename` is relative to. Empty as parsed;
    /// the reader of the index fills it in.
    pub repo: String,
    pub package: String,
    pub version: String,
    pub filename: String,
    pub sha256: String,
    /// The size of the package file in bytes, as the index states it: the cap its download is
    /// read within (specification Q-2). `None` where the index states none or an unreadable one.
    pub size: Option<u64>,
    pub depends: String,
    pub pre_depends: String,
    pub provides: String,
    pub section: String,
}

/// The parse of a `Packages` index the signed `Release` text vouched for, separated from the fetch so it can be
/// tested against a stanza rather than against the network. RFC 822-ish stanzas separated by
/// blank lines; only the single-line fields matter here, so continuation lines are simply not
/// fields.
pub fn parse_deb_index(text: &str) -> Vec<DebEntry> {
    let mut entries = Vec::new();
    for stanza in text.split("\n\n") {
        let field = |name: &str| {
            stanza
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}: ")))
                .unwrap_or_default()
                .to_string()
        };
        let package = field("Package");
        if package.is_empty() {
            continue;
        }
        entries.push(DebEntry {
            repo: String::new(),
            package,
            version: field("Version"),
            filename: field("Filename"),
            sha256: field("SHA256"),
            size: field("Size").parse().ok(),
            depends: field("Depends"),
            pre_depends: field("Pre-Depends"),
            provides: field("Provides"),
            section: field("Section"),
        });
    }
    entries
}

/// The package names of a relationship field such as `Depends`, one list of alternatives per
/// entry, without version constraints or architecture qualifiers.
pub fn relationships(field: &str) -> Vec<Vec<&str>> {
    field
        .split(',')
        .map(|entry| {
            entry
                .split('|')
                .filter_map(|alternative| alternative.split_whitespace().next())
                .map(|name| name.split(':').next().unwrap_or(name))
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|alternatives| !alternatives.is_empty())
        .collect()
}

/// Debian's version order (`dpkg --compare-versions`): the epoch numerically, then the upstream
/// version, then the revision, each by `verrevcmp` — `~` before everything, the end, letters,
/// then other characters; digit runs by their value.
pub fn deb_version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(version: &str) -> (u64, &str, &str) {
        let (epoch, rest) = match version.split_once(':') {
            Some((epoch, rest)) if epoch.bytes().all(|b| b.is_ascii_digit()) => {
                (epoch.parse().unwrap_or(0), rest)
            }
            _ => (0, version),
        };
        let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, ""));
        (epoch, upstream, revision)
    }
    let (a, b) = (split(a), split(b));
    a.0.cmp(&b.0)
        .then_with(|| verrevcmp(a.1, b.1))
        .then_with(|| verrevcmp(a.2, b.2))
}

/// dpkg's `verrevcmp`, over bytes.
fn verrevcmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn order(c: Option<u8>) -> i32 {
        match c {
            None => 0,
            Some(b'~') => -1,
            Some(c) if c.is_ascii_digit() => 0,
            Some(c) if c.is_ascii_alphabetic() => i32::from(c),
            Some(c) => i32::from(c) + 256,
        }
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let digit = |s: &[u8], i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !digit(a, i)) || (j < b.len() && !digit(b, j)) {
            let (x, y) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first_diff = Ordering::Equal;
        while digit(a, i) && digit(b, j) {
            if first_diff == Ordering::Equal {
                first_diff = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if digit(a, i) {
            return Ordering::Greater;
        }
        if digit(b, j) {
            return Ordering::Less;
        }
        if first_diff != Ordering::Equal {
            return first_diff;
        }
    }
    Ordering::Equal
}

/// The `libc6 (>= 2.34)` out of a `Depends` field — the oldest glibc that package runs on.
pub fn libc_floor(depends: &str) -> Option<String> {
    depends.split(',').find_map(|dep| {
        let dep = dep.trim();
        let rest = dep.strip_prefix("libc6 (>= ")?;
        rest.strip_suffix(')').map(str::to_string)
    })
}

/// Refuses a signed `Release` text that names neither as its `Codename` nor as its `Suite` the
/// suite it was fetched for, as `apt` refuses a conflicting distribution. A signature covers the
/// text and not the path it was served under, so without it whoever serves the repository could
/// answer for one suite with another's genuine index: an archived `-security` index, which states
/// no `Valid-Until`, in place of the live one, or another distribution's build of Icinga.
pub fn release_names(release: &str, suite: &str) -> Result<(), String> {
    let field = |name: &str| {
        release
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
    };
    let (codename, stated) = (field("Codename"), field("Suite"));
    if codename == Some(suite) || stated == Some(suite) {
        return Ok(());
    }
    Err(format!(
        "is signed for {}, not for {suite:?} it was fetched as — refusing it",
        codename
            .or(stated)
            .map_or_else(|| "no suite".to_string(), |named| format!("{named:?}"))
    ))
}

/// Refuses a signed `Release` text whose `Valid-Until` lies before `now`, as `apt` does: a
/// signature never expires, so without it an old index — with the libraries it named before a
/// security fix — could be served again and would still verify. A text that states no
/// `Valid-Until`, as Icinga's and Debian's main suites do, is not bounded by one.
pub fn release_current(release: &str, now: time::OffsetDateTime) -> Result<(), String> {
    let format = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] UTC"
    );
    let Some(stated) = release
        .lines()
        .find_map(|line| line.strip_prefix("Valid-Until:"))
        .map(str::trim)
    else {
        return Ok(());
    };
    let until = time::PrimitiveDateTime::parse(stated, format)
        .map_err(|_| format!("states a Valid-Until this tool cannot read: {stated:?}"))?
        .assume_utc();
    if until < now {
        return Err(format!(
            "was valid until {stated}, so it may be an old index served again — refusing it"
        ));
    }
    Ok(())
}

/// The SHA-256 and size the signed `Release` text states for `path`.
pub fn release_digest<'r>(release: &'r str, path: &str) -> Option<(&'r str, u64)> {
    release
        .lines()
        .skip_while(|line| line.trim_end() != "SHA256:")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let (hash, size, name) = (fields.next()?, fields.next()?, fields.next()?);
            (name == path).then_some(())?;
            Some((hash, size.parse().ok()?))
        })
}

/// The `(link, target)` pairs a maintainer script's `update-alternatives --install` lines describe,
/// one per name, keeping the highest priority — automatic mode's own rule.
pub fn winning_alternatives(script: &str) -> Vec<(String, String)> {
    let mut best: std::collections::BTreeMap<String, (i64, String, String)> = Default::default();
    for line in script.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("update-alternatives") || words.next() != Some("--install") {
            continue;
        }
        let (Some(link), Some(name), Some(target), Some(priority)) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            continue;
        };
        let Ok(priority) = priority.parse::<i64>() else {
            continue;
        };
        // The first of equal priorities stays; only a strictly higher one replaces it.
        if best.get(name).is_none_or(|(held, ..)| priority > *held) {
            best.insert(
                name.to_string(),
                (priority, link.to_string(), target.to_string()),
            );
        }
    }
    best.into_values()
        .map(|(_, link, target)| (link, target))
        .collect()
}

/// The `DT_NEEDED` names of an ELF file — the sonames its dynamic loader would look for — read
/// from its bytes alone. `None` for a file that is not ELF at all, such as a script. Only the
/// 64-bit little-endian form is read, the one of the amd64 build this tool makes; another, or a
/// file whose headers point outside it, is an error rather than a guess.
pub fn elf_needed(bytes: &[u8]) -> Result<Option<Vec<String>>, String> {
    const PT_LOAD: u32 = 1;
    const PT_DYNAMIC: u32 = 2;
    const DT_NEEDED: u64 = 1;
    const DT_STRTAB: u64 = 5;
    const DT_STRSZ: u64 = 10;
    if bytes.get(..4) != Some(b"\x7fELF".as_slice()) {
        return Ok(None);
    }
    if bytes.get(4..6) != Some([2u8, 1].as_slice()) {
        return Err("is not a 64-bit little-endian ELF file".to_string());
    }
    let malformed = || "is a malformed ELF file".to_string();
    let u16_at = |at: usize| {
        bytes
            .get(at..at.checked_add(2)?)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at.checked_add(4)?)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let u64_at = |at: usize| {
        bytes
            .get(at..at.checked_add(8)?)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    };
    let offset = |value: u64| usize::try_from(value).ok();

    let phoff = u64_at(0x20).and_then(offset).ok_or_else(malformed)?;
    let phentsize = usize::from(u16_at(0x36).ok_or_else(malformed)?);
    let phnum = usize::from(u16_at(0x38).ok_or_else(malformed)?);
    // (virtual address, file offset, size in the file) of each loaded segment, and the dynamic one.
    let mut loads: Vec<(u64, u64, u64)> = Vec::new();
    let mut dynamic = None;
    for n in 0..phnum {
        let at = n
            .checked_mul(phentsize)
            .and_then(|o| o.checked_add(phoff))
            .ok_or_else(malformed)?;
        let kind = u32_at(at).ok_or_else(malformed)?;
        let file_offset = u64_at(at + 8).ok_or_else(malformed)?;
        let vaddr = u64_at(at + 16).ok_or_else(malformed)?;
        let filesz = u64_at(at + 32).ok_or_else(malformed)?;
        match kind {
            PT_LOAD => loads.push((vaddr, file_offset, filesz)),
            PT_DYNAMIC => dynamic = Some((file_offset, filesz)),
            _ => {}
        }
    }
    // Statically linked: nothing to load.
    let Some((dyn_offset, dyn_size)) = dynamic else {
        return Ok(Some(Vec::new()));
    };
    let mut needed = Vec::new();
    let (mut strtab, mut strsz) = (None, None);
    let entries = dyn_size / 16;
    for n in 0..entries {
        let at = n
            .checked_mul(16)
            .and_then(|o| o.checked_add(dyn_offset))
            .and_then(offset)
            .ok_or_else(malformed)?;
        let tag = u64_at(at).ok_or_else(malformed)?;
        let value = u64_at(at + 8).ok_or_else(malformed)?;
        match tag {
            0 => break,
            DT_NEEDED => needed.push(value),
            DT_STRTAB => strtab = Some(value),
            DT_STRSZ => strsz = Some(value),
            _ => {}
        }
    }
    if needed.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let (strtab, strsz) = (strtab.ok_or_else(malformed)?, strsz.ok_or_else(malformed)?);
    // The string table is named by its address once loaded; where that lies in the file is the
    // loaded segment's to say.
    let table = loads
        .iter()
        .find(|(vaddr, _, filesz)| strtab >= *vaddr && strtab - *vaddr < *filesz)
        .and_then(|(vaddr, file_offset, _)| file_offset.checked_add(strtab - vaddr))
        .and_then(offset)
        .ok_or_else(malformed)?;
    let table = bytes
        .get(
            table
                ..table
                    .checked_add(offset(strsz).ok_or_else(malformed)?)
                    .ok_or_else(malformed)?,
        )
        .ok_or_else(malformed)?;
    needed
        .into_iter()
        .map(|at| {
            let rest = offset(at)
                .and_then(|at| table.get(at..))
                .ok_or_else(malformed)?;
            let end = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
            String::from_utf8(rest[..end].to_vec()).map_err(|_| malformed())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
