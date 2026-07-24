    about = "Build, hash, and sign OpAMP Fleet packages"
    /// for an artifact this Server will not hold.
/// The Client's own unpacker, used by this binary's tests rather than restated in them. What `pack`
/// has to get right is not "is this a valid archive" but "does *this* code open it and find the
/// member" — a container the Client cannot open would be discovered on a host, at rollout time, as
/// a failed install on every matched Agent. Test-only: the tool itself never unpacks. (It does
/// reach one item of `archive` outside tests — `unix_mode_attributes`, the 7z convention that
/// module also decodes.)
///
/// Until ADR-0011 this was `#[path = "../archive.rs"] mod archive`, a second compilation of the
/// same file, because a binary in a crate without a library has no other way to reach it.
use client::archive;
                     is more than one file cannot be delivered as one",
    // `mut` is used only by the Unix block below; on Windows there is nothing to set.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut entry = sevenz_rust2::ArchiveEntry::new_file(member);
    // The member is a program, so it is marked executable — `7z x` on Linux or macOS then yields
    // something that runs, without a `chmod +x` nobody documented. The tar path has always done
    // this; a `.7z` says it through 7-Zip's Unix-attribute convention instead of a tar mode field.
    //
    // Only off Windows, which is 7-Zip's own rule: bit 15 means `FILE_ATTRIBUTE_INTEGRITY_STREAM`
    // there, and the Windows build neither writes nor expects the Unix extension. It costs the
    // release nothing — each artifact is packed on a runner of its own platform (ADR-0023), so the
    // Linux and macOS ones carry the mode and `client.exe`, which has no use for it, does not.
    #[cfg(unix)]
    {
        entry.has_windows_attributes = true;
        entry.windows_attributes = client::archive::unix_mode_attributes(0o755);
    }
        .push_archive_entry(entry, Some(source))
    #[test]
    #[cfg(unix)]
    fn a_packed_program_is_executable_when_it_is_unpacked_by_hand() {
        let source = program(dir.path(), "client");

        let seven = dir.path().join("client.7z");
        pack(&source, &seven, Format::SevenZ, None, None).expect("pack");
        let reader = sevenz_rust2::ArchiveReader::open(&seven, Default::default()).expect("open");
        let entry = reader
            .archive()
            .files
            .iter()
            .find(|e| e.name() == "client")
            .expect("the member is there");
        assert!(
            entry.has_windows_attributes,
            "without the attribute there is no mode to restore"
        );
        // Bit 15 says the high half is a Unix mode; the high half says a regular file, rwxr-xr-x.
        assert_eq!(entry.windows_attributes() & 0x8000, 0x8000);
        assert_eq!(entry.windows_attributes() >> 16, 0o100_755);

        let tarball = dir.path().join("client.tar.gz");
        pack(&source, &tarball, Format::TarGz, None, None).expect("pack");
        let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(
            std::fs::File::open(&tarball).expect("open"),
        ));
        let mode = entries
            .entries()
            .expect("entries")
            .next()
            .expect("one member")
            .expect("readable")
            .header()
            .mode()
            .expect("a mode");
        assert_eq!(mode & 0o777, 0o755);
    }

    /// The member the Client itself would reject: the same convention that carries a mode can say
    /// "symbolic link", and what `pack` writes must never be mistaken for one.
    #[test]
    #[cfg(unix)]
    fn the_mode_a_pack_writes_is_not_read_back_as_a_link() {
        let source = program(dir.path(), "client");
        let artifact = dir.path().join("client.7z");

        pack(&source, &artifact, Format::SevenZ, None, None).expect("pack");

        assert_eq!(
            unpacked(&artifact, "client", None),
        // The tree path is the one that validates every member before writing anything, and a
        // member whose mode said `S_IFLNK` would refuse the whole archive there.
        let dest = dir.path().join("tree");
        archive::extract_tree_7z(&artifact, Path::new("client"), &dest, None)
            .expect("no link was seen");
    }

