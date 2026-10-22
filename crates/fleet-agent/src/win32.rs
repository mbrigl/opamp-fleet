//! What the second service and its registration need of Windows that neither the standard library,
//! `tokio` nor `windows-service` offers (ADR-0059): security identifiers, the security descriptor
//! of the named pipe, the identity of a pipe's client and of its server, and job objects.
//!
//! The SID text and the pipe's SDDL are plain string logic, compiled on every platform so that
//! their tests run everywhere. The calls into the system are declared here by hand, each against
//! its Microsoft Learn page: the `windows-sys` features they live in are not enabled anywhere in the
//! build, and enabling them is a dependency decision (ADR-0010) this module does not take.

/// The SID of `LocalSystem`, which never runs the second service and is never the Client's account
/// where a process account is registered (ADR-0059 clause 1).
pub const LOCAL_SYSTEM: &str = "S-1-5-18";

/// `NT AUTHORITY\LocalService`.
pub const LOCAL_SERVICE: &str = "S-1-5-19";

/// `NT AUTHORITY\NetworkService`.
pub const NETWORK_SERVICE: &str = "S-1-5-20";

/// A security identifier in its canonical text: `S-1-` followed by the identifier authority and at
/// least one subauthority, each a decimal number without leading zeros — what
/// `ConvertSidToStringSidW` writes. Two SIDs are the same account when their texts are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sid(String);

impl Sid {
    /// Reads `text` as a SID and writes it canonically, so that a SID typed with leading zeros or a
    /// lower-case `s` compares equal to the one the system reports.
    ///
    /// # Errors
    /// Returns why `text` is no SID.
    pub fn parse(text: &str) -> Result<Sid, String> {
        let refused = || format!("{text:?} is not a security identifier (S-1-5-…)");
        let rest = text
            .strip_prefix("S-")
            .or_else(|| text.strip_prefix("s-"))
            .ok_or_else(refused)?;
        let mut parts = rest.split('-');
        let revision = parts.next().ok_or_else(refused)?;
        if revision != "1" {
            return Err(refused());
        }
        let authority: u64 = number(parts.next()).ok_or_else(refused)?;
        // The identifier authority is six bytes.
        if authority >= 1 << 48 {
            return Err(refused());
        }
        let subauthorities = parts
            .map(|part| number::<u32>(Some(part)))
            .collect::<Option<Vec<u32>>>()
            .ok_or_else(refused)?;
        // SID_MAX_SUB_AUTHORITIES.
        if subauthorities.is_empty() || subauthorities.len() > 15 {
            return Err(refused());
        }
        let mut canonical = format!("S-1-{authority}");
        for sub in subauthorities {
            canonical.push_str(&format!("-{sub}"));
        }
        Ok(Sid(canonical))
    }

    /// The SID as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A decimal number of plain ASCII digits; `None` for anything else, a sign or an empty part among
/// them.
fn number<T: std::str::FromStr>(part: Option<&str>) -> Option<T> {
    let part = part?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// The SID of the virtual account `NT SERVICE\<service>`: `S-1-5-80-` followed by the five
/// little-endian 32-bit words of the SHA-1 of the service name, upper-cased, in UTF-16LE. It needs
/// no service of that name to exist, so a registration names the account of a service it is about
/// to create.
///
/// The derivation is not documented on Microsoft Learn; it reproduces the well-known SID of
/// `NT SERVICE\TrustedInstaller`, which the tests hold it to, and on Windows they compare it with
/// what `LookupAccountNameW` answers.
#[must_use]
pub fn virtual_account_sid(service: &str) -> Sid {
    let name: Vec<u8> = service
        .to_uppercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &name);
    let mut sid = String::from("S-1-5-80");
    for word in digest.as_ref().as_chunks::<4>().0 {
        let word = u32::from_le_bytes(*word);
        sid.push_str(&format!("-{word}"));
    }
    Sid(sid)
}

/// The SID of an account named in one of the passwordless forms a service may run under
/// (ADR-0042 clause 2, ADR-0059 clause 1), where it can be known without asking the system: a
/// virtual account `NT SERVICE\<name>`, `LocalSystem`, and the two built-in service accounts. A
/// group-managed service account (`name$`) is the domain's and is looked up instead; `None` for it
/// and for every other name.
#[must_use]
pub fn known_account_sid(account: &str) -> Option<Sid> {
    const NT_SERVICE: &str = r"NT SERVICE\";
    let known = [
        ("LocalSystem", LOCAL_SYSTEM),
        (r"NT AUTHORITY\SYSTEM", LOCAL_SYSTEM),
        (r"NT AUTHORITY\LocalService", LOCAL_SERVICE),
        (r"NT AUTHORITY\NetworkService", NETWORK_SERVICE),
    ];
    if let Some((_, sid)) = known
        .iter()
        .find(|(name, _)| account.eq_ignore_ascii_case(name))
    {
        return Some(Sid((*sid).to_string()));
    }
    let prefix = account.get(..NT_SERVICE.len())?;
    prefix
        .eq_ignore_ascii_case(NT_SERVICE)
        .then(|| virtual_account_sid(&account[NT_SERVICE.len()..]))
}

/// The security descriptor of the second service's named pipe, in SDDL (ADR-0059 clause 4): owned
/// by the account the second service runs as, which the Client checks it for, and a protected DACL
/// that grants the Client's account read and write — nothing to create an instance — and the
/// second service's own account all access, which it needs to create the pipe's next instance
/// (`FILE_CREATE_PIPE_INSTANCE`, CreateNamedPipeW). Nobody else is granted anything: not
/// `Everyone`, whom the default descriptor grants read, and not `LocalSystem` or the
/// Administrators, who need no access to the pipe to administer the host.
#[must_use]
pub fn pipe_sddl(own: &Sid, client: &Sid) -> String {
    format!("O:{own}D:P(A;;FA;;;{own})(A;;FRFW;;;{client})")
}

#[cfg(windows)]
pub use system::*;

/// The calls into the system.
#[cfg(windows)]
mod system {
    use std::ffi::{c_void, OsStr};
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};

    use super::Sid;

    type Handle = *mut c_void;
    type Bool = i32;
    type Psid = *mut c_void;

    /// SECURITY_ATTRIBUTES.
    #[repr(C)]
    pub struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: Bool,
    }

    /// SID_AND_ATTRIBUTES, the whole of TOKEN_USER.
    #[repr(C)]
    struct TokenUser {
        sid: Psid,
        attributes: u32,
    }

    /// JOBOBJECT_BASIC_LIMIT_INFORMATION.
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimit {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    /// JOBOBJECT_EXTENDED_LIMIT_INFORMATION, its IO_COUNTERS as six 64-bit counters.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    /// JOBOBJECT_BASIC_ACCOUNTING_INFORMATION.
    #[repr(C)]
    #[derive(Default)]
    struct Accounting {
        total_user_time: i64,
        total_kernel_time: i64,
        this_period_total_user_time: i64,
        this_period_total_kernel_time: i64,
        total_page_fault_count: u32,
        total_processes: u32,
        active_processes: u32,
        total_terminated_processes: u32,
    }

    /// THREADENTRY32.
    #[repr(C)]
    #[derive(Default)]
    struct ThreadEntry {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    const TOKEN_QUERY: u32 = 0x0008;
    /// TOKEN_INFORMATION_CLASS::TokenUser.
    const TOKEN_USER_CLASS: i32 = 1;
    const SDDL_REVISION_1: u32 = 1;
    /// SE_OBJECT_TYPE::SE_FILE_OBJECT.
    const SE_FILE_OBJECT: i32 = 1;
    /// SE_OBJECT_TYPE::SE_KERNEL_OBJECT.
    const SE_KERNEL_OBJECT: i32 = 6;
    const OWNER_SECURITY_INFORMATION: u32 = 0x1;
    /// JOBOBJECTINFOCLASS::JobObjectBasicAccountingInformation.
    const JOB_ACCOUNTING: i32 = 1;
    /// JOBOBJECTINFOCLASS::JobObjectExtendedLimitInformation.
    const JOB_EXTENDED_LIMIT: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const TH32CS_SNAPTHREAD: u32 = 0x4;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    /// ERROR_INSUFFICIENT_BUFFER.
    const INSUFFICIENT_BUFFER: i32 = 122;

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn GetCurrentProcess() -> Handle;
        fn GetCurrentThread() -> Handle;
        fn CreateJobObjectW(attributes: *const SecurityAttributes, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, len: u32) -> Bool;
        fn QueryInformationJobObject(
            job: Handle,
            class: i32,
            info: *mut c_void,
            len: u32,
            returned: *mut u32,
        ) -> Bool;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> Bool;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> Bool;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> Bool;
        fn OpenThread(access: u32, inherit: Bool, thread_id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ImpersonateNamedPipeClient(pipe: Handle) -> Bool;
        fn RevertToSelf() -> Bool;
        fn OpenThreadToken(thread: Handle, access: u32, as_self: Bool, token: *mut Handle) -> Bool;
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> Bool;
        fn GetTokenInformation(
            token: Handle,
            class: i32,
            info: *mut c_void,
            len: u32,
            returned: *mut u32,
        ) -> Bool;
        fn ConvertSidToStringSidW(sid: Psid, text: *mut *mut u16) -> Bool;
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            len: *mut u32,
        ) -> Bool;
        fn GetSecurityInfo(
            handle: Handle,
            object_type: i32,
            info: u32,
            owner: *mut Psid,
            group: *mut Psid,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn LookupAccountNameW(
            system: *const u16,
            account: *const u16,
            sid: Psid,
            sid_len: *mut u32,
            domain: *mut u16,
            domain_len: *mut u32,
            use_: *mut i32,
        ) -> Bool;
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn check(ok: Bool) -> io::Result<()> {
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// A handle the system returned, owned; `NULL` is its failure.
    fn owned(handle: Handle) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a valid handle the caller owns and nothing else closes.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    /// Memory the system allocated with `LocalAlloc`, freed when dropped.
    struct Local(*mut c_void);

    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by the system for the caller to free with LocalFree.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// The text of the SID at `sid`.
    ///
    /// # Safety
    /// `sid` points at a valid SID for the length of the call.
    unsafe fn sid_text(sid: Psid) -> io::Result<Sid> {
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` is valid (the caller's promise); `text` receives a LocalAlloc'd string.
        check(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
        let text = Local(text.cast());
        let mut len = 0;
        // SAFETY: a NUL-terminated string the call wrote.
        while unsafe { *text.0.cast::<u16>().add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` characters of that string.
        let wide = unsafe { std::slice::from_raw_parts(text.0.cast::<u16>(), len) };
        Sid::parse(&String::from_utf16_lossy(wide)).map_err(io::Error::other)
    }

    /// The user SID of the token `token`.
    fn token_user(token: &OwnedHandle) -> io::Result<Sid> {
        let mut needed = 0u32;
        // SAFETY: a size query: no buffer, `needed` receives the size.
        let sized = unsafe {
            GetTokenInformation(
                token.as_raw_handle().cast(),
                TOKEN_USER_CLASS,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if sized == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(INSUFFICIENT_BUFFER) {
                return Err(error);
            }
        }
        // Aligned for the pointer TOKEN_USER begins with.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: a buffer of at least `needed` bytes.
        check(unsafe {
            GetTokenInformation(
                token.as_raw_handle().cast(),
                TOKEN_USER_CLASS,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        })?;
        // SAFETY: the call wrote a TOKEN_USER at the start of the buffer, whose SID lies within it.
        unsafe { sid_text((*buffer.as_ptr().cast::<TokenUser>()).sid) }
    }

    /// The account this process runs as.
    ///
    /// # Errors
    /// Returns why the process token could not be read.
    pub fn own_sid() -> io::Result<Sid> {
        let mut token: Handle = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of this process; `token` receives a handle this owns.
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        token_user(&owned(token)?)
    }

    /// The account of the client connected to the pipe instance `pipe`, read from its token through
    /// `ImpersonateNamedPipeClient`, which takes the security context of the last message read from
    /// the pipe: so it is asked once something has been read. The client opened the pipe at the
    /// identification level, the default of `tokio`'s client, which the second service may
    /// impersonate without `SeImpersonatePrivilege` and which lets it identify the client and act
    /// as nobody (ImpersonateNamedPipeClient). The thread token is opened against the service's own
    /// context (`OpenAsSelf`), since nothing can be opened at the identification level
    /// (OpenThreadToken).
    ///
    /// # Errors
    /// Returns why the client could not be identified: an anonymous client among them.
    pub fn pipe_client(pipe: RawHandle) -> io::Result<Sid> {
        // SAFETY: a pipe handle the caller holds for the length of the call.
        check(unsafe { ImpersonateNamedPipeClient(pipe.cast()) })?;
        let mut token: Handle = std::ptr::null_mut();
        // SAFETY: this thread, impersonating; `token` receives a handle this owns.
        let opened =
            check(unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) });
        // SAFETY: ends this thread's impersonation; no argument.
        if unsafe { RevertToSelf() } == 0 {
            // A thread left in the client's context would run the runtime's next task in it.
            tracing::error!(
                error = %io::Error::last_os_error(),
                "cannot end the impersonation of the pipe's client; exiting"
            );
            std::process::abort();
        }
        opened?;
        token_user(&owned(token)?)
    }

    /// The owner of the named pipe `handle` names: the account of the server that created it
    /// (ADR-0059 clause 4). The handle must carry `READ_CONTROL` (GetSecurityInfo), which a client
    /// that opened the pipe for reading holds.
    ///
    /// # Errors
    /// Returns why the owner could not be read.
    pub fn pipe_owner(handle: RawHandle) -> io::Result<Sid> {
        owner_of(handle, SE_KERNEL_OBJECT)
    }

    /// The owner of the file or directory `handle` names, opened with `READ_CONTROL`.
    ///
    /// # Errors
    /// Returns why the owner could not be read.
    pub fn file_owner(handle: RawHandle) -> io::Result<Sid> {
        owner_of(handle, SE_FILE_OBJECT)
    }

    fn owner_of(handle: RawHandle, object_type: i32) -> io::Result<Sid> {
        let mut owner: Psid = std::ptr::null_mut();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: a handle the caller holds; `owner` points into `descriptor`, freed below.
        let error = unsafe {
            GetSecurityInfo(
                handle.cast(),
                object_type,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        let descriptor = Local(descriptor);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        // SAFETY: the owner SID within the descriptor, alive until `descriptor` drops.
        let sid = unsafe { sid_text(owner) };
        drop(descriptor);
        sid
    }

    /// The SID `LookupAccountNameW` answers for `account`.
    ///
    /// # Errors
    /// Returns why the account could not be looked up: one that does not exist among them.
    pub fn lookup_account(account: &str) -> io::Result<Sid> {
        let name = wide(OsStr::new(account));
        let mut sid = vec![0u64; 9];
        let mut sid_len = (sid.len() * 8) as u32;
        let mut domain = vec![0u16; 256];
        let mut domain_len = domain.len() as u32;
        let mut use_ = 0i32;
        // SAFETY: buffers of the lengths passed; a SID is at most 68 bytes, and a domain name
        // longer than the buffer fails the call rather than overrunning it.
        check(unsafe {
            LookupAccountNameW(
                std::ptr::null(),
                name.as_ptr(),
                sid.as_mut_ptr().cast(),
                &mut sid_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut use_,
            )
        })?;
        // SAFETY: the call wrote a SID at the start of `sid`.
        unsafe { sid_text(sid.as_mut_ptr().cast()) }
    }

    /// A security descriptor made from SDDL, freed when dropped.
    pub struct SecurityDescriptor(Local);

    // SAFETY: memory of this process's own, read by the system only during the calls it is passed
    // to.
    unsafe impl Send for SecurityDescriptor {}
    // SAFETY: as above; nothing writes it after it is made.
    unsafe impl Sync for SecurityDescriptor {}

    impl SecurityDescriptor {
        /// The descriptor `sddl` describes.
        ///
        /// # Errors
        /// Returns why the text is no security descriptor.
        pub fn from_sddl(sddl: &str) -> io::Result<SecurityDescriptor> {
            let text = wide(OsStr::new(sddl));
            let mut descriptor: *mut c_void = std::ptr::null_mut();
            // SAFETY: a NUL-terminated string; `descriptor` receives LocalAlloc'd memory.
            check(unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            })?;
            Ok(SecurityDescriptor(Local(descriptor)))
        }

        /// SECURITY_ATTRIBUTES that carry this descriptor and no inheritable handle; valid while
        /// the descriptor lives.
        #[must_use]
        pub fn attributes(&self) -> SecurityAttributes {
            SecurityAttributes {
                length: std::mem::size_of::<SecurityAttributes>() as u32,
                descriptor: (self.0).0,
                inherit_handle: 0,
            }
        }
    }

    /// A job object whose processes all end when its last handle closes
    /// (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`): one per program the second service starts (ADR-0059
    /// clause 4). Every process the program starts belongs to it too, unless it breaks away, which
    /// the job does not allow.
    pub struct Job(OwnedHandle);

    impl Job {
        /// A new, unnamed job.
        ///
        /// # Errors
        /// Returns why the job could not be made.
        pub fn new() -> io::Result<Job> {
            // SAFETY: no attributes and no name.
            let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
            let mut limit = ExtendedLimit::default();
            limit.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: a valid job handle and a structure of the size passed.
            check(unsafe {
                SetInformationJobObject(
                    job.as_raw_handle().cast(),
                    JOB_EXTENDED_LIMIT,
                    std::ptr::from_ref(&limit).cast(),
                    std::mem::size_of::<ExtendedLimit>() as u32,
                )
            })?;
            Ok(Job(job))
        }

        /// Puts the process `process` in the job.
        ///
        /// # Errors
        /// Returns why it could not be assigned.
        pub fn assign(&self, process: RawHandle) -> io::Result<()> {
            // SAFETY: a valid job handle and a process handle the caller holds.
            check(unsafe {
                AssignProcessToJobObject(self.0.as_raw_handle().cast(), process.cast())
            })
        }

        /// Ends every process in the job, with `exit_code`.
        ///
        /// # Errors
        /// Returns why the job could not be terminated.
        pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
            // SAFETY: a valid job handle.
            check(unsafe { TerminateJobObject(self.0.as_raw_handle().cast(), exit_code) })
        }

        /// How many processes of the job still run; an error reads as some.
        #[must_use]
        pub fn active(&self) -> u32 {
            let mut accounting = Accounting::default();
            // SAFETY: a valid job handle and a structure of the size passed.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0.as_raw_handle().cast(),
                    JOB_ACCOUNTING,
                    std::ptr::from_mut(&mut accounting).cast(),
                    std::mem::size_of::<Accounting>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return 1;
            }
            accounting.active_processes
        }
    }

    /// Resumes the threads of the process `pid`, created suspended (`CREATE_SUSPENDED`) so that it
    /// starts nothing before it is in its job: its one thread, found in a snapshot of the system's
    /// threads.
    ///
    /// # Errors
    /// Returns why no thread of the process could be resumed.
    pub fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: a snapshot of every thread; the handle is owned below.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        let mut entry = ThreadEntry {
            size: std::mem::size_of::<ThreadEntry>() as u32,
            ..ThreadEntry::default()
        };
        let mut resumed = false;
        // SAFETY: a valid snapshot and an entry whose size is set.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) };
        while more != 0 {
            if entry.owner_process_id == pid {
                // SAFETY: opens a thread by id; the handle is owned below.
                let thread =
                    owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.thread_id) })?;
                // SAFETY: a valid thread handle.
                if unsafe { ResumeThread(thread.as_raw_handle().cast()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) };
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no thread of process {pid} to resume"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SID is read as the system writes it, and anything else is refused.
    /// Verifies: ADR-0059
    #[test]
    fn a_sid_is_read_canonically() {
        assert_eq!(Sid::parse("S-1-5-18").expect("sid").as_str(), "S-1-5-18");
        assert_eq!(
            Sid::parse("s-1-5-080-01").expect("sid").as_str(),
            "S-1-5-80-1",
            "written as the system writes it"
        );
        for refused in [
            "",
            "S-1",
            "S-1-5",
            "S-2-5-18",
            "S-1--5-18",
            "S-1-5-18-",
            "S-1-5-+18",
            "S-1-5-4294967296",
            "NT SERVICE\\x",
            "S-1-5-18 ",
        ] {
            assert!(Sid::parse(refused).is_err(), "{refused:?}");
        }
    }

    /// The virtual account of a service has the SID the system gives it, which no service of that
    /// name needs to exist for: the well-known SID of `NT SERVICE\TrustedInstaller`, whatever case
    /// the name is written in.
    /// Verifies: ADR-0059
    #[test]
    fn a_virtual_accounts_sid_is_derived_from_its_services_name() {
        const TRUSTED_INSTALLER: &str =
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
        assert_eq!(
            virtual_account_sid("TrustedInstaller").as_str(),
            TRUSTED_INSTALLER
        );
        assert_eq!(
            known_account_sid(r"nt service\trustedinstaller").map(|sid| sid.0),
            Some(TRUSTED_INSTALLER.to_string())
        );
        assert_eq!(
            known_account_sid(r"NT AUTHORITY\LocalService").map(|sid| sid.0),
            Some(LOCAL_SERVICE.to_string())
        );
        assert_eq!(
            known_account_sid("LocalSystem").map(|sid| sid.0),
            Some(LOCAL_SYSTEM.to_string())
        );
        assert_eq!(
            known_account_sid(r"CORP\fleet-agents$"),
            None,
            "the domain's"
        );
        assert_eq!(known_account_sid("bob"), None);
    }

    /// The pipe is owned by the second service's account and grants the Client's account reading
    /// and writing, the service's own everything, and nobody else anything (ADR-0059 clause 4).
    /// Verifies: ADR-0059
    #[test]
    fn the_pipe_admits_the_clients_account_alone() {
        let own = Sid::parse("S-1-5-80-1-2-3-4-5").expect("sid");
        let client = Sid::parse("S-1-5-80-6-7-8-9-10").expect("sid");
        let sddl = pipe_sddl(&own, &client);
        assert_eq!(
            sddl,
            "O:S-1-5-80-1-2-3-4-5D:P(A;;FA;;;S-1-5-80-1-2-3-4-5)(A;;FRFW;;;S-1-5-80-6-7-8-9-10)"
        );
        assert!(sddl.contains("D:P"), "inherits nothing");
        for nobody in ["WD", "AN", "SY", "BA", "BU", "AU"] {
            assert!(!sddl.contains(&format!(";;;{nobody})")), "{nobody}: {sddl}");
        }
    }

    /// On Windows the derivation agrees with the system's own answer for an account that exists.
    /// Verifies: ADR-0059
    #[cfg(windows)]
    #[test]
    fn the_derived_sid_is_the_one_the_system_looks_up() {
        assert_eq!(
            lookup_account(r"NT SERVICE\TrustedInstaller").expect("lookup"),
            virtual_account_sid("TrustedInstaller")
        );
        assert!(own_sid().expect("own sid").as_str().starts_with("S-1-5-"));
    }

    /// A job ends what it holds; the SDDL of the pipe is one the system reads.
    /// Verifies: ADR-0059
    #[cfg(windows)]
    #[test]
    fn a_job_ends_the_process_it_holds() {
        use std::os::windows::io::AsRawHandle as _;
        use std::os::windows::process::CommandExt as _;
        const CREATE_SUSPENDED: u32 = 0x4;
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 60 127.0.0.1 >nul"])
            .creation_flags(CREATE_SUSPENDED)
            .spawn()
            .expect("spawn");
        let job = Job::new().expect("job");
        job.assign(child.as_raw_handle()).expect("assign");
        resume(child.id()).expect("resume");
        assert_eq!(child.try_wait().expect("try_wait"), None, "runs");
        assert!(job.active() >= 1);
        job.terminate(1).expect("terminate");
        let status = child.wait().expect("wait");
        assert_eq!(status.code(), Some(1));

        let own = own_sid().expect("own");
        SecurityDescriptor::from_sddl(&pipe_sddl(&own, &own)).expect("the system reads it");
    }
}
