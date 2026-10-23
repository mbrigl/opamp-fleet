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
/// to create. `None` for a name that is not ASCII: how the system upper-cases other letters is not
/// known here, and a product's name is ASCII anyway.
///
/// The derivation is not documented on Microsoft Learn; it reproduces the well-known SID of
/// `NT SERVICE\TrustedInstaller`, which the tests hold it to, and on Windows they compare it with
/// what `LookupAccountNameW` answers.
#[must_use]
pub fn virtual_account_sid(service: &str) -> Option<Sid> {
    if !service.is_ascii() || service.is_empty() {
        return None;
    }
    let name: Vec<u8> = service
        .to_ascii_uppercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &name);
    let mut sid = String::from("S-1-5-80");
    for word in digest.as_ref().as_chunks::<4>().0 {
        let word = u32::from_le_bytes(*word);
        sid.push_str(&format!("-{word}"));
    }
    Some(Sid(sid))
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
    if !prefix.eq_ignore_ascii_case(NT_SERVICE) {
        return None;
    }
    virtual_account_sid(account.get(NT_SERVICE.len()..)?)
}

/// What the Client may do with the second service's pipe: `FILE_GENERIC_READ` — read, its
/// attributes and its DACL, which the Client reads the owner from — and `FILE_WRITE_DATA`, and no
/// more. Not `FILE_GENERIC_WRITE`: its `FILE_APPEND_DATA` is `FILE_CREATE_PIPE_INSTANCE`, which
/// would let the Client's account serve the pipe itself (Named Pipe Security and Access Rights).
pub const PIPE_CLIENT_ACCESS: u32 = 0x0012_0089 | 0x0000_0002;

/// The security descriptor of the second service's named pipe, in SDDL (ADR-0059 clause 4): owned
/// by the account the second service runs as, which the Client checks it for, and a protected DACL
/// that grants the Client's account [`PIPE_CLIENT_ACCESS`] — nothing to create an instance — and
/// the second service's own account all access, which it needs to create the pipe's next instance
/// (`FILE_CREATE_PIPE_INSTANCE`, CreateNamedPipeW). Nobody else is granted anything: not
/// `Everyone`, whom the default descriptor grants read, and not `LocalSystem` or the
/// Administrators, who need no access to the pipe to administer the host.
#[must_use]
pub fn pipe_sddl(own: &Sid, client: &Sid) -> String {
    format!("O:{own}D:P(A;;FA;;;{own})(A;;0x{PIPE_CLIENT_ACCESS:x};;;{client})")
}

pub mod acl;
#[cfg(windows)]
pub mod files;

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

    /// SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO.
    const SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO: u32 = 6;

    #[link(name = "advapi32")]
    extern "system" {
        fn ChangeServiceConfig2W(service: Handle, level: u32, info: *const c_void) -> Bool;
        fn QueryServiceConfig2W(
            service: Handle,
            level: u32,
            buffer: *mut u8,
            len: u32,
            needed: *mut u32,
        ) -> Bool;
    }

    /// The privileges `privileges` as a multi-string: each NUL-terminated, then an empty one.
    fn multi_string(privileges: &[&str]) -> Vec<u16> {
        let mut text: Vec<u16> = privileges
            .iter()
            .flat_map(|name| name.encode_utf16().chain(std::iter::once(0)))
            .collect();
        text.push(0);
        text
    }

    /// Sets the privileges the service `service` — opened with `SERVICE_CHANGE_CONFIG` — keeps:
    /// the SCM removes every other from its token when it starts
    /// (SERVICE_REQUIRED_PRIVILEGES_INFO).
    ///
    /// # Errors
    /// Returns why the configuration could not be changed.
    ///
    /// # Safety
    /// `service` is a valid service handle for the length of the call.
    pub unsafe fn set_required_privileges(service: Handle, privileges: &[&str]) -> io::Result<()> {
        let mut text = multi_string(privileges);
        // SERVICE_REQUIRED_PRIVILEGES_INFOW: one pointer to the multi-string.
        let info: *mut u16 = text.as_mut_ptr();
        // SAFETY: a service handle the caller holds, and the structure — one pointer to a
        // multi-string — alive for the call.
        check(unsafe {
            ChangeServiceConfig2W(
                service,
                SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
                std::ptr::from_ref(&info).cast(),
            )
        })
    }

    /// The privileges the service `service` — opened with `SERVICE_QUERY_CONFIG` — keeps.
    ///
    /// # Errors
    /// Returns why the configuration could not be read.
    ///
    /// # Safety
    /// `service` is a valid service handle for the length of the call.
    pub unsafe fn required_privileges(service: Handle) -> io::Result<Vec<String>> {
        let mut needed = 0u32;
        // SAFETY: a size query.
        let sized = unsafe {
            QueryServiceConfig2W(
                service,
                SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
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
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: a buffer of at least `needed` bytes.
        check(unsafe {
            QueryServiceConfig2W(
                service,
                SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        })?;
        // SAFETY: the call wrote the structure, one pointer to a multi-string within the buffer
        // or null.
        let mut text = unsafe { *buffer.as_ptr().cast::<*const u16>() };
        let mut privileges = Vec::new();
        while !text.is_null() {
            let mut len = 0;
            // SAFETY: a NUL-terminated string of the multi-string.
            while unsafe { *text.add(len) } != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            // SAFETY: `len` characters of it.
            let name = unsafe { std::slice::from_raw_parts(text, len) };
            privileges.push(String::from_utf16_lossy(name));
            // SAFETY: the next string begins after this one's NUL.
            text = unsafe { text.add(len + 1) };
        }
        Ok(privileges)
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *mut c_void,
            revision: u32,
            info: u32,
            text: *mut *mut u16,
            len: *mut u32,
        ) -> Bool;
        fn SetKernelObjectSecurity(handle: Handle, info: u32, descriptor: *mut c_void) -> Bool;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn GetKernelObjectSecurity(
            handle: Handle,
            info: u32,
            descriptor: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> Bool;
    }

    /// The parts `info` names of the security descriptor of the object `handle` names, as SDDL —
    /// as the object holds it (GetKernelObjectSecurity). Not through `GetSecurityInfo`, which
    /// presents a file's DACL that does not carry `SE_DACL_AUTO_INHERITED` against its parent's
    /// current one: an ACE the parent passed on before, and passes on no more, it shows as
    /// explicit, and an object nothing changed reads as changed.
    ///
    /// # Errors
    /// Returns why the descriptor could not be read: the handle lacks `READ_CONTROL`.
    pub fn security_text(handle: RawHandle, info: u32) -> io::Result<String> {
        let mut needed = 0u32;
        // SAFETY: a size query.
        let sized = unsafe {
            GetKernelObjectSecurity(handle.cast(), info, std::ptr::null_mut(), 0, &mut needed)
        };
        if sized == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(INSUFFICIENT_BUFFER) {
                return Err(error);
            }
        }
        let mut descriptor = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: a buffer of at least `needed` bytes.
        check(unsafe {
            GetKernelObjectSecurity(
                handle.cast(),
                info,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        })?;
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: the self-relative descriptor the call wrote; `text` receives a LocalAlloc'd
        // string.
        check(unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor.as_mut_ptr().cast(),
                SDDL_REVISION_1,
                info,
                &mut text,
                std::ptr::null_mut(),
            )
        })?;
        let text = Local(text.cast());
        let mut len = 0;
        // SAFETY: a NUL-terminated string the call wrote.
        while unsafe { *text.0.cast::<u16>().add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` characters of it.
        let wide = unsafe { std::slice::from_raw_parts(text.0.cast::<u16>(), len) };
        Ok(String::from_utf16_lossy(wide))
    }

    /// Sets the parts `info` names of the security descriptor of the object `handle` names to
    /// those of `sddl`, on that object alone: nothing is propagated to what a directory holds.
    ///
    /// # Errors
    /// Returns why the descriptor is no descriptor or could not be set.
    pub fn set_security(handle: RawHandle, info: u32, sddl: &str) -> io::Result<()> {
        let descriptor = SecurityDescriptor::from_sddl(sddl)?;
        // SAFETY: a handle the caller holds, and a descriptor alive across the call.
        check(unsafe { SetKernelObjectSecurity(handle.cast(), info, (descriptor.0).0) })
    }

    /// BY_HANDLE_FILE_INFORMATION, its three FILETIMEs as pairs of 32-bit words.
    #[repr(C)]
    #[derive(Default)]
    struct FileInformation {
        attributes: u32,
        times: [u32; 6],
        volume_serial_number: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    /// LUID_AND_ATTRIBUTES.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct LuidAndAttributes {
        low: u32,
        high: i32,
        attributes: u32,
    }

    /// TOKEN_PRIVILEGES for up to three privileges.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct TokenPrivileges {
        count: u32,
        privileges: [LuidAndAttributes; 3],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(file: Handle, info: *mut FileInformation) -> Bool;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut [u32; 2])
            -> Bool;
        fn AdjustTokenPrivileges(
            token: Handle,
            disable_all: Bool,
            new: *const TokenPrivileges,
            len: u32,
            previous: *mut TokenPrivileges,
            returned: *mut u32,
        ) -> Bool;
    }

    /// How many names the file `handle` names has (`nNumberOfLinks`, GetFileInformationByHandle).
    ///
    /// # Errors
    /// Returns why the information could not be read.
    pub fn link_count(handle: RawHandle) -> io::Result<u32> {
        let mut info = FileInformation::default();
        // SAFETY: a handle the caller holds and a structure of the documented layout.
        check(unsafe { GetFileInformationByHandle(handle.cast(), &mut info) })?;
        Ok(info.links)
    }

    /// The backup, restore and take-ownership privileges of this process's token, enabled while
    /// this lives and returned to what they were when it drops: with them, an entry opened with
    /// `FILE_FLAG_BACKUP_SEMANTICS` is opened whatever its DACL says, and its owner may be set to
    /// the Administrators (CreateFileW, AdjustTokenPrivileges). Only an elevated administrator
    /// holds them; where the token lacks one, nothing is enabled for it, and what it would have
    /// opened fails as it would have without.
    pub struct BackupPrivileges {
        token: OwnedHandle,
        previous: TokenPrivileges,
    }

    impl BackupPrivileges {
        /// Enables the privileges.
        ///
        /// # Errors
        /// Returns why the token could not be opened or adjusted.
        pub fn enable() -> io::Result<BackupPrivileges> {
            const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
            const SE_PRIVILEGE_ENABLED: u32 = 0x2;
            let mut token: Handle = std::ptr::null_mut();
            // SAFETY: the pseudo-handle of this process; `token` receives a handle owned below.
            check(unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    &mut token,
                )
            })?;
            let token = owned(token)?;
            let mut new = TokenPrivileges {
                count: 3,
                ..TokenPrivileges::default()
            };
            for (slot, name) in new.privileges.iter_mut().zip([
                "SeBackupPrivilege",
                "SeRestorePrivilege",
                "SeTakeOwnershipPrivilege",
            ]) {
                let name = wide(OsStr::new(name));
                let mut luid = [0u32; 2];
                // SAFETY: a NUL-terminated name and a LUID to write.
                check(unsafe {
                    LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid)
                })?;
                *slot = LuidAndAttributes {
                    low: luid[0],
                    high: i32::from_ne_bytes(luid[1].to_ne_bytes()),
                    attributes: SE_PRIVILEGE_ENABLED,
                };
            }
            let mut previous = TokenPrivileges::default();
            let mut returned = 0u32;
            // SAFETY: a token opened for adjusting, and structures of the sizes passed.
            check(unsafe {
                AdjustTokenPrivileges(
                    token.as_raw_handle().cast(),
                    0,
                    &new,
                    std::mem::size_of::<TokenPrivileges>() as u32,
                    &mut previous,
                    &mut returned,
                )
            })?;
            Ok(BackupPrivileges { token, previous })
        }
    }

    impl Drop for BackupPrivileges {
        fn drop(&mut self) {
            // SAFETY: the token this holds, and the previous state the adjustment returned.
            unsafe {
                AdjustTokenPrivileges(
                    self.token.as_raw_handle().cast(),
                    0,
                    &self.previous,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
        }
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            attributes: *const SecurityAttributes,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
    }

    /// Opens the client end of the pipe `pipe` for [`super::PIPE_CLIENT_ACCESS`], overlapped, at
    /// the identification level: the server may identify the Client and act as nobody
    /// (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, CreateFileW).
    ///
    /// # Errors
    /// Returns why the pipe cannot be opened: no server listening among them.
    pub fn open_pipe(pipe: &std::path::Path) -> io::Result<OwnedHandle> {
        const OPEN_EXISTING: u32 = 3;
        const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
        const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
        const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
        let name = wide(pipe.as_os_str());
        // SAFETY: a NUL-terminated name and no attributes; the handle is owned below.
        owned(unsafe {
            CreateFileW(
                name.as_ptr(),
                super::PIPE_CLIENT_ACCESS,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        })
    }

    /// Creates the file `path`, which must not exist, carrying the security descriptor `sddl` from
    /// its first moment, and opens it for writing shared with nobody (`CREATE_NEW`, a share mode of
    /// 0 and `lpSecurityAttributes`, CreateFileW): no other handle to it is ever open while this
    /// one is.
    ///
    /// # Errors
    /// Returns why it cannot be created: one that exists among them.
    pub fn create_private(path: &std::path::Path, sddl: &str) -> io::Result<std::fs::File> {
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const CREATE_NEW: u32 = 1;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
        let descriptor = SecurityDescriptor::from_sddl(sddl)?;
        let attributes = descriptor.attributes();
        let name = wide(path.as_os_str());
        // SAFETY: a NUL-terminated name and attributes whose descriptor outlives the call; the
        // handle is owned below.
        let handle = owned(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        })?;
        Ok(std::fs::File::from(handle))
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: Bool, process_id: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    }

    /// A handle to the process `pid` that can be waited for (`SYNCHRONIZE`, OpenProcess), or
    /// `None` where no such process runs any more (`ERROR_INVALID_PARAMETER`).
    ///
    /// # Errors
    /// Returns why it cannot be opened otherwise.
    pub fn process_to_wait_for(pid: u32) -> io::Result<Option<OwnedHandle>> {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        /// ERROR_INVALID_PARAMETER.
        const GONE: i32 = 87;
        // SAFETY: no pointers; the handle is owned below.
        match owned(unsafe { OpenProcess(SYNCHRONIZE, 0, pid) }) {
            Ok(handle) => Ok(Some(handle)),
            Err(e) if e.raw_os_error() == Some(GONE) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Waits up to `within` for the process `process` to end (WaitForSingleObject); whether it
    /// did.
    ///
    /// # Errors
    /// Returns why it cannot be waited for.
    pub fn ended_within(process: &OwnedHandle, within: std::time::Duration) -> io::Result<bool> {
        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_TIMEOUT: u32 = 0x102;
        // Below INFINITE, which never times out.
        let milliseconds = u32::try_from(within.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: a handle owned by the caller, open across the call.
        match unsafe { WaitForSingleObject(process.as_raw_handle().cast(), milliseconds) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn GetSecurityDescriptorDacl(
            descriptor: *mut c_void,
            present: *mut Bool,
            dacl: *mut *mut u8,
            defaulted: *mut Bool,
        ) -> Bool;
        fn GetSecurityDescriptorControl(
            descriptor: *mut c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> Bool;
    }

    /// `SE_DACL_PROTECTED`.
    const DACL_PROTECTED: u16 = 0x1000;

    /// The protection of the DACL `sddl` describes and its ACEs, each the bytes the system makes
    /// of it — type, flags, size, rights and SID — in order. The system reads both an alias and a
    /// SID, and a mask's alias and its number, alike, so two DACLs written differently that say the
    /// same come out the same, and nothing else does.
    fn binary_dacl(sddl: &str) -> io::Result<(bool, Vec<Vec<u8>>)> {
        let descriptor = SecurityDescriptor::from_sddl(sddl)?;
        let raw = (descriptor.0).0;
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: a descriptor the system made, alive across the call.
        check(unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) })?;
        let mut present: Bool = 0;
        let mut defaulted: Bool = 0;
        let mut acl: *mut u8 = std::ptr::null_mut();
        // SAFETY: as above; `acl` points into the descriptor.
        check(unsafe { GetSecurityDescriptorDacl(raw, &mut present, &mut acl, &mut defaulted) })?;
        let protected = control & DACL_PROTECTED != 0;
        if present == 0 || acl.is_null() {
            return Err(io::Error::other(format!("{sddl:?} carries no DACL")));
        }
        // SAFETY: an ACL the system made: an eight-byte header — revision, padding, its size, its
        // count of ACEs, padding — and the ACEs, each beginning with its type, flags and size.
        unsafe {
            let size = usize::from(u16::from_le_bytes([*acl.add(2), *acl.add(3)]));
            let count = usize::from(u16::from_le_bytes([*acl.add(4), *acl.add(5)]));
            let bytes = std::slice::from_raw_parts(acl, size);
            let mut aces = Vec::with_capacity(count);
            let mut at = 8;
            for _ in 0..count {
                let ace_size = bytes
                    .get(at + 2..at + 4)
                    .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
                    .ok_or_else(|| io::Error::other("an ACE beyond its ACL"))?;
                let ace = bytes
                    .get(at..at + ace_size)
                    .ok_or_else(|| io::Error::other("an ACE beyond its ACL"))?;
                aces.push(ace.to_vec());
                at += ace_size.max(4);
            }
            Ok((protected, aces))
        }
    }

    /// Whether the DACL `held` — as an object holds it — says what `set` says: the same ACEs in
    /// the same order, byte for byte as the system makes them, and protected exactly where `set`
    /// is. Only the other control bits — `SE_DACL_AUTO_INHERITED`, which a DACL set on a handle
    /// does not carry — are not compared.
    ///
    /// # Errors
    /// Returns why either is no DACL.
    pub fn same_dacl(held: &str, set: &str) -> io::Result<bool> {
        Ok(binary_dacl(held)? == binary_dacl(set)?)
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
            virtual_account_sid("TrustedInstaller")
                .expect("ascii")
                .as_str(),
            TRUSTED_INSTALLER
        );
        assert_eq!(virtual_account_sid("Dienst-ä"), None, "not ASCII");
        assert_eq!(known_account_sid("NT SERVICE\\Dienst-ä"), None);
        assert_eq!(known_account_sid("ä"), None, "no slice panics");
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
            "O:S-1-5-80-1-2-3-4-5D:P(A;;FA;;;S-1-5-80-1-2-3-4-5)(A;;0x12008b;;;S-1-5-80-6-7-8-9-10)"
        );
        assert_eq!(
            PIPE_CLIENT_ACCESS & 0x4,
            0,
            "no FILE_CREATE_PIPE_INSTANCE for the Client"
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
            Some(lookup_account(r"NT SERVICE\TrustedInstaller").expect("lookup")),
            virtual_account_sid("TrustedInstaller")
        );
        assert!(own_sid().expect("own sid").as_str().starts_with("S-1-5-"));
    }

    /// Two DACLs that say the same are the same however they are written — an alias or its SID,
    /// a mask's alias or its number, with or without automatic inheritance — and two that differ
    /// in a SID, a right, a flag, the order of their ACEs or their protection are not.
    /// Verifies: ADR-0059
    #[cfg(windows)]
    #[test]
    fn a_dacl_is_compared_by_what_it_says() {
        let me = own_sid().expect("own sid");
        let same = |held: &str, set: &str| same_dacl(held, set).expect("dacls");
        assert!(same("D:(A;;0x1200a9;;;LS)", "D:(A;;0x1200a9;;;S-1-5-19)"));
        assert!(same(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
            "D:PAI(A;OICI;0x1f01ff;;;S-1-5-18)(A;OICI;0x1f01ff;;;S-1-5-32-544)"
        ));
        assert!(same(
            &format!("D:(A;OICIID;FA;;;{me})"),
            &format!("D:AI(A;CIOIID;FA;;;{me})")
        ));
        assert!(
            !same("D:(A;;0x1200a9;;;LS)", "D:(A;;0x1200a9;;;NS)"),
            "another SID"
        );
        assert!(
            !same("D:(A;;0x1301bf;;;LS)", "D:(A;;0x1200a9;;;LS)"),
            "a wider mask"
        );
        assert!(!same("D:(A;OICI;FA;;;SY)", "D:(A;;FA;;;SY)"), "other flags");
        assert!(
            !same("D:(A;;FA;;;SY)(A;;FA;;;BA)", "D:(A;;FA;;;BA)(A;;FA;;;SY)"),
            "order"
        );
        assert!(!same("D:(A;;FA;;;SY)", "D:P(A;;FA;;;SY)"), "protection");
        assert!(
            !same("D:(A;;FA;;;SY)", "D:(A;;FA;;;SY)(A;;FA;;;BA)"),
            "an ACE more"
        );
        assert!(
            !same("D:(D;;FA;;;SY)", "D:(A;;FA;;;SY)"),
            "deny is no allow"
        );
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
