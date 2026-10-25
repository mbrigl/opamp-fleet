//! The channel on Windows (ADR-0059 clause 4): a named pipe whose security descriptor grants the
//! Client's account and nobody else ([`crate::win32::pipe_sddl`]), a client identified by its
//! token and a server by the pipe's owner and its process, held open while the Client checks it,
//! a job object for every program the second service starts, and the DACLs that close the second
//! service's process and threads to the process account (ADR-0062).
//!
//! Windows has no signals: a stop ends a program's job, and a signal is refused. A kind that would
//! reload its agent by a signal reloads it by a restart there, as it does without a process account
//! ([`crate::supervisor::process::sighup`]).

use std::future::Future;
use std::io;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use tokio::io::AsyncWriteExt as _;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use tokio::process::{Child, Command};
use tracing::{info, warn};

use super::{encode, Refusals, Reply, Served};
use crate::supervisor::process::Launch;
use crate::win32::{self, Job, SecurityDescriptor, Sid};

/// An account as the channel knows it on Windows: its SID.
pub type Account = Sid;

/// The second service's end of one connection.
pub(super) type ServerStream = NamedPipeServer;

/// The pipe the second service listens on: always one instance waiting for the next client, made
/// with the pipe's security descriptor.
pub struct Listener {
    pipe: PathBuf,
    descriptor: SecurityDescriptor,
    next: NamedPipeServer,
}

/// `CREATE_SUSPENDED`: a program starts nothing before it is in its job.
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// The exit code a program ended through its job reports.
const ENDED: u32 = 1;

/// Creates the pipe `pipe`, owned by the account this service runs as and granting the Client's
/// account `client` alone (ADR-0059 clause 4). Its first instance must be the first of that
/// name: a pipe someone else made under the name first is not taken over, and the bind fails.
/// Remote clients are refused by the pipe itself.
///
/// # Errors
/// Returns an error when this service's account cannot be read, or the pipe cannot be made.
pub fn bind(pipe: &Path, client: &Sid) -> io::Result<Listener> {
    let own = win32::own_sid()?;
    let descriptor = SecurityDescriptor::from_sddl(&win32::pipe_sddl(&own, client))?;
    let next = create(pipe, &descriptor, true)?;
    Ok(Listener {
        pipe: pipe.to_path_buf(),
        descriptor,
        next,
    })
}

fn create(
    pipe: &Path,
    descriptor: &SecurityDescriptor,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: valid SECURITY_ATTRIBUTES whose descriptor outlives the call.
    unsafe {
        options
            .create_with_security_attributes_raw(pipe, std::ptr::from_mut(&mut attributes).cast())
    }
}

impl Listener {
    /// The next client: the instance it connected to, with a new one waiting in its place.
    /// Cancel-safe: a connection that arrives while nobody waits is taken by the next call.
    pub(super) async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        let connected = self.next.connect().await;
        match (connected, create(&self.pipe, &self.descriptor, false)) {
            (Ok(()), Ok(fresh)) => Ok(std::mem::replace(&mut self.next, fresh)),
            (Err(e), Ok(fresh)) => {
                self.next = fresh;
                Err(e)
            }
            // No instance to wait with but this one: it is let go of its client and waits again.
            (_, Err(e)) => {
                let _ = self.next.disconnect();
                Err(e)
            }
        }
    }
}

/// Whether the client of `stream` is the Client's account `client` (ADR-0059 clause 4), read from
/// its token once it has written.
///
/// # Errors
/// Returns why the client is refused.
pub(super) fn admit(stream: &NamedPipeServer, client: &Sid) -> Result<(), String> {
    let peer = win32::pipe_client(stream.as_raw_handle())
        .map_err(|e| format!("cannot identify the pipe's client: {e}"))?;
    admit_sid(&peer, client)
}

/// Whether `peer` is the Client's account `client`.
///
/// # Errors
/// Returns why the peer is refused.
pub fn admit_sid(peer: &Sid, client: &Sid) -> Result<(), String> {
    if peer == client {
        Ok(())
    } else {
        Err(format!("{peer} is not the Client's account ({client})"))
    }
}

/// Whether the service, running as `own`, may serve `client` as the Client's account (ADR-0059
/// clause 1): the service never runs as `LocalSystem`, the Client's account never is
/// `LocalSystem`, and never the account the service runs as.
///
/// # Errors
/// Returns why the service does not start.
pub fn check_client(client: &Sid, own: &Sid) -> Result<(), String> {
    if own.as_str() == win32::LOCAL_SYSTEM {
        return Err(
            "the second service runs as LocalSystem: it runs under the process account, never as \
             LocalSystem"
                .to_string(),
        );
    }
    if client.as_str() == win32::LOCAL_SYSTEM {
        return Err("--client-sid S-1-5-18: the Client's account is never LocalSystem".to_string());
    }
    if client == own {
        return Err(format!(
            "--client-sid {client} is the account this service runs as: the process account is \
             never the Client's"
        ));
    }
    Ok(())
}

/// Tells a client why it is not served, and lets it go: written within a second, so no refused
/// client holds a task of the service's for longer.
pub(super) fn refuse(mut stream: NamedPipeServer, reason: String, refusals: &mut Refusals) {
    if let Some(unlogged) = refusals.note(std::time::Instant::now()) {
        warn!(reason = %reason, unlogged, "refused a connection");
    }
    let Ok(frame) = encode(&Reply::Refused {
        reason,
        version: None,
    }) else {
        return;
    };
    tokio::spawn(async move {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.write_all(&frame)).await;
    });
}

/// Connects to the second service's pipe `pipe` for no more than the pipe grants the Client
/// ([`win32::PIPE_CLIENT_ACCESS`]: `tokio`'s client asks `GENERIC_WRITE`, which it does not
/// grant), at the identification level: the service may identify the Client and act as nobody.
///
/// # Errors
/// Returns why the pipe cannot be opened: no service listening among them.
pub(super) fn connect(pipe: &Path) -> io::Result<NamedPipeClient> {
    use std::os::windows::io::IntoRawHandle as _;
    let handle = win32::open_pipe(pipe)?;
    // SAFETY: a pipe handle opened for overlapped I/O, owned from here by the client.
    unsafe { NamedPipeClient::from_raw_handle(handle.into_raw_handle()) }
}

/// The account that made the pipe `stream` is connected to: its owner, which only that account
/// — or an administrator — can have set (ADR-0059 clause 4).
///
/// # Errors
/// Returns why the owner could not be read.
pub(super) fn server_of(stream: &NamedPipeClient) -> io::Result<Sid> {
    win32::pipe_owner(stream.as_raw_handle())
}

/// The process that serves the pipe instance `stream` is connected to (ADR-0059 clause 4): the
/// second service's, or that of whoever else of the process account made an instance of the pipe.
/// It is held open ([`peer_process`]) until the Client has decided.
pub(super) fn server_process(stream: &NamedPipeClient) -> io::Result<super::PeerProcess> {
    peer_process(win32::pipe_server_process(stream.as_raw_handle())?)
}

/// The process `pid`, opened to ask about it and wait for it before the SCM is asked which process
/// is the second service (ADR-0062): while the handle is open the id cannot pass to another
/// process, and whether it has ended is read from it once the SCM has answered. A process that
/// cannot be opened is one that cannot be read.
///
/// # Errors
/// Returns why it could not be opened, naming it.
pub(super) fn peer_process(pid: u32) -> io::Result<super::PeerProcess> {
    let handle = win32::process_to_check(pid)
        .map_err(|error| io::Error::new(error.kind(), Unopened { pid, error }))?;
    Ok(super::PeerProcess { pid, handle })
}

/// The process the pipe names as its server, which could not be opened: by its id, so that the
/// refusal names it in the log as it names a process that differs.
#[derive(Debug)]
pub(super) struct Unopened {
    pub(super) pid: u32,
    error: io::Error,
}

impl std::fmt::Display for Unopened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "process {} cannot be opened: {}", self.pid, self.error)
    }
}

impl std::error::Error for Unopened {}

/// A program's job: every process it starts is in it too, and ends with it.
pub(super) struct Tree(Job);

impl Tree {
    /// Whether a process of the tree still runs, so that the connection holds on to it after its
    /// program ended.
    pub(super) fn holds_processes(&self) -> bool {
        self.0.active() > 0
    }

    /// Ends every process left in the tree.
    pub(super) fn end(&self) {
        let _ = self.0.terminate(ENDED);
    }
}

/// Starts a program in a job of its own, suspended until it is in the job, so it starts nothing
/// outside it. Its environment is the second service's own — the process account's, as the SCM
/// starts the service under it — with the request's environment laid over it: the block's `env`,
/// then the Supervisor Endpoint's token (ADR-0059 clause 7).
pub(super) fn start(launch: &Launch) -> io::Result<(Child, Tree)> {
    let job = Job::new()?;
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(CREATE_SUSPENDED);
    launch.set_env(&mut command);
    if let Some(dir) = &launch.working_dir {
        command.current_dir(dir);
    }
    let mut child = command.spawn()?;
    let held = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("the program ended before it was put in its job"))
        .and_then(|process| job.assign(process))
        .and_then(|()| {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("the program has no process id"))?;
            win32::resume(pid)
        });
    if let Err(e) = held {
        let _ = child.start_kill();
        return Err(e);
    }
    Ok((child, Tree(job)))
}

/// Windows has no signals: refused, and the Client restarts what it would have signalled.
pub(super) fn signal_program(_child: &Child, _signal: i32, _group: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows has no signals; a program is stopped through its job",
    ))
}

/// Ends the program and everything in its job.
pub(super) fn stop_program(tree: &Tree, child: &mut Child, _own_process_group: bool) {
    let _ = tree.0.terminate(ENDED);
    let _ = child.start_kill();
}

/// The stop at the connection's end: the job, and so the program with all it started, ends at
/// once — Windows has nothing a program could end on gracefully.
pub(super) async fn close(child: &mut Child, tree: &Tree) -> io::Result<ExitStatus> {
    let _ = tree.0.terminate(ENDED);
    let _ = child.start_kill();
    child.wait().await
}

/// An exit as the protocol carries it: the exit code's bits.
pub(super) fn raw_status(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// An exit the protocol carried, as the Client's supervision reads it.
pub(super) fn exit_status(raw: i32) -> ExitStatus {
    ExitStatus::from_raw(u32::from_ne_bytes(raw.to_ne_bytes()))
}

/// `supervisor run --agents <pipe> --client-sid <SID>` under the SCM: the second service
/// (ADR-0059), until `shutdown` resolves or a self-update leaves it behind. It listens only once
/// its process and every thread it has read back closed to the process account (ADR-0062), and
/// closes every [`SWEEP`] what threads the system starts for it after that.
///
/// # Errors
/// Returns an error when the service runs as `LocalSystem`, `client` is `LocalSystem` or the
/// service's own account, its process or one of its threads is not closed, or the pipe cannot be
/// made.
pub async fn run(
    pipe: &Path,
    client: Sid,
    shutdown: impl Future<Output = ()>,
) -> Result<Served, String> {
    let own = win32::own_sid().map_err(|e| format!("cannot read this service's account: {e}"))?;
    check_client(&client, &own)?;
    let thread = CLOSED
        .get()
        .cloned()
        .unwrap_or_else(|| Err("its program did not close it at its start".to_string()))
        .map_err(|e| format!("the second service's process is not closed: {e}"))?;
    // A thread the system started for the process since the program's start is closed too; one
    // that cannot be is found by the read-back.
    close_threads(&thread)?;
    check_closed(
        &win32::process_sddl(&own, &client),
        &win32::thread_sddl(&own),
    )?;
    let listener =
        bind(pipe, &client).map_err(|e| format!("cannot listen on {}: {e}", pipe.display()))?;
    info!(pipe = %pipe.display(), client = %client, "second service listening");
    let sweeping = tokio::spawn(sweep(thread));
    let served = super::serve(listener, client, super::superseded, shutdown).await;
    sweeping.abort();
    if served == Served::Superseded {
        info!(
            "the Client is another build and current names another version; exiting so the \
             service manager starts that one"
        );
    }
    Ok(served)
}

/// The thread DACL this program closed its process and its threads with at its start
/// ([`close_this_process`]), or why it could not: set once, before anything else runs.
static CLOSED: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();

/// How often the second service looks for a thread the system started for it since and closes it:
/// the thread pool the runtime waits for its programs' processes on is one of them.
const SWEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// Closes the second service's process and its threads to the process account (ADR-0062): the
/// process's DACL ([`win32::process_sddl`]) grants the process account and the Client's account
/// `client`, as `--client-sid` names it, only to ask about it and wait for it, and each thread's
/// ([`win32::thread_sddl`]) the process account the same. The first thing its program does, before
/// it parses its command line or sets up its log: it sets the process's DACL, its own thread's and,
/// from a snapshot, those of the threads that exist by then. Nothing can be logged yet, so what
/// fails is kept, and [`run`] does not listen with it. The token's default DACL is left as it is,
/// so the programs the service starts carry the security of their own account.
pub fn close_this_process(client: Option<&str>) {
    let _ = CLOSED.set(closing(client));
}

fn closing(client: Option<&str>) -> Result<String, String> {
    let client = Sid::parse(client.ok_or("its command line names no --client-sid")?)?;
    let own = win32::own_sid().map_err(|e| format!("cannot read this service's account: {e}"))?;
    let thread = win32::thread_sddl(&own);
    win32::set_this_process_dacl(&win32::process_sddl(&own, &client))
        .map_err(|e| format!("cannot set the process's DACL: {e}"))?;
    win32::set_this_thread_dacl(&thread)
        .map_err(|e| format!("cannot set the main thread's DACL: {e}"))?;
    close_threads(&thread)?;
    Ok(thread)
}

/// Closes the calling thread as [`close_this_process`] closed the process: the first act of the
/// thread the SCM starts for the service and of every thread of its runtime. Nothing where the
/// process was not closed, which [`run`] refuses to listen with anyway.
pub fn close_this_thread() {
    if let Some(Ok(thread)) = CLOSED.get() {
        if let Err(e) = win32::set_this_thread_dacl(thread) {
            tracing::error!(error = %e, "cannot close a thread of the second service");
        }
    }
}

/// Gives every thread of this process that carries a DACL other than `sddl` that one, and answers
/// how many it gave it. A thread it cannot close is left to the read-back ([`check_closed`]) and
/// named in a warning.
///
/// # Errors
/// Returns why the threads could not be listed.
fn close_threads(sddl: &str) -> Result<usize, String> {
    let threads = win32::threads_of_this_process()
        .map_err(|e| format!("cannot list the process's threads: {e}"))?;
    let mut closed = 0;
    for thread in threads {
        match win32::thread_dacl(thread) {
            // Ended since the snapshot.
            Ok(None) => continue,
            Ok(Some(held)) if win32::same_dacl(&held, sddl).unwrap_or(false) => continue,
            _ => {}
        }
        match win32::set_thread_dacl(thread, sddl) {
            Ok(true) => closed += 1,
            Ok(false) => {}
            Err(e) => warn!(thread, error = %e, "cannot close a thread of the second service"),
        }
    }
    Ok(closed)
}

/// Whether this process carries the DACL `process` and every one of its threads the DACL
/// `thread`, read back from each and compared by what they say ([`win32::same_dacl`], as ADR-0059
/// clause 6 compares an ACL): the second service does not listen while one differs (ADR-0062).
///
/// # Errors
/// Returns which differs, or cannot be read.
pub(super) fn check_closed(process: &str, thread: &str) -> Result<(), String> {
    let differs = |what: String, held: &str, set: &str| match win32::same_dacl(held, set) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("{what} carries {held}, not {set}")),
        Err(e) => Err(format!(
            "{what} carries {held}, which cannot be compared: {e}"
        )),
    };
    let held = win32::this_process_dacl()
        .map_err(|e| format!("cannot read the DACL of the second service's process: {e}"))?;
    differs("the second service's process".to_string(), &held, process)?;
    let threads = win32::threads_of_this_process()
        .map_err(|e| format!("cannot list the process's threads: {e}"))?;
    for id in threads {
        match win32::thread_dacl(id) {
            Ok(None) => {}
            Ok(Some(held)) => differs(format!("thread {id} of the second service"), &held, thread)?,
            Err(e) => return Err(format!("cannot read the DACL of thread {id}: {e}")),
        }
    }
    Ok(())
}

/// Closes, every [`SWEEP`], the threads the system has started for the process since: the
/// runtime's own close themselves as their first act, but those of the system's thread pool, on
/// which the runtime waits for a program's process, do not.
async fn sweep(thread: String) {
    loop {
        tokio::time::sleep(SWEEP).await;
        match close_threads(&thread) {
            Ok(0) => {}
            Ok(closed) => tracing::debug!(closed, "closed threads the system started"),
            Err(e) => warn!(error = %e, "cannot close the threads the system started"),
        }
    }
}

/// `FILE_FLAG_BACKUP_SEMANTICS`, which opens a directory, and `FILE_FLAG_OPEN_REPARSE_POINT`, which
/// opens a link as itself.
const OPEN_AS_ITSELF: u32 = 0x0200_0000 | 0x0020_0000;

/// `READ_CONTROL`: enough to read the owner.
const READ_CONTROL: u32 = 0x0002_0000;

/// `FILE_ATTRIBUTE_REPARSE_POINT`: a symbolic link or a junction.
const REPARSE_POINT: u32 = 0x400;

/// Removes what the process account wrote under `dir`, the tombstone of the Supervisor named
/// `supervisor` (ADR-0059 clause 5), as [`super::purge`] does on Unix: every entry this service's
/// account owns in each of the directories `shared` names relative to `dir`, and in the
/// directories beneath them, with everything beneath it. `dir` is absolute and the tombstone of
/// that Supervisor, each of `shared` is a path beneath it of plain names alone, and no link or
/// junction is followed on the way or beneath. The service acts with its own rights, which the
/// Client's directories grant it only where its Managed Processes write (ADR-0059 clause 6).
///
/// # Errors
/// Returns why the request is refused, or the first entry that cannot be reached or removed.
pub(super) fn purge(supervisor: &str, dir: &Path, shared: &[PathBuf]) -> Result<(), String> {
    check_tombstone(supervisor, dir)?;
    let me = win32::own_sid().map_err(|e| format!("cannot read this service's account: {e}"))?;
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
        Ok(meta) if !is_directory(&meta) => {
            return Err(format!("{} is no directory", dir.display()))
        }
        Ok(_) => {}
    }
    let mut first = None;
    for relative in shared {
        let result = plain_beneath(dir, relative).and_then(|path| match path {
            Some(path) => remove_own(&path, &me),
            None => Ok(()),
        });
        if let Err(e) = result {
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// The directories directly beneath `dir`, by their names, links and junctions left out: where the
/// second service looks for what the process account wrote under a tombstone. Unix names the
/// Client's directories its group writes; on Windows what the process account may write is
/// granted by ACL ([`crate::storage`]), which the service's own walk meets, so every directory is
/// named and the walk passes over what holds nothing of its own.
///
/// # Errors
/// Returns an error when `dir` cannot be read: `NotFound` where it is gone already.
pub(super) fn directories_beneath(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if std::fs::symlink_metadata(entry.path()).is_ok_and(|meta| is_directory(&meta)) {
            found.push(PathBuf::from(entry.file_name()));
        }
    }
    found.sort();
    Ok(found)
}

/// Whether `dir` is absolute and the tombstone of the Supervisor `supervisor`.
///
/// # Errors
/// Returns the refusal.
pub fn check_tombstone(supervisor: &str, dir: &Path) -> Result<(), String> {
    let named = dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| crate::config::tombstone_of(name) == Some(supervisor));
    if dir.is_absolute() && dir.parent().is_some() && named {
        Ok(())
    } else {
        Err(format!(
            "{} is not the tombstone of the Supervisor {supervisor}",
            dir.display()
        ))
    }
}

/// `dir` joined with `relative`, a path of plain names, reached through directories alone, never
/// through a link; `None` where a part of it does not exist.
fn plain_beneath(dir: &Path, relative: &Path) -> Result<Option<PathBuf>, String> {
    let plain = !relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)));
    if !plain {
        return Err(format!(
            "{} is not a path beneath the Supervisor's directory",
            relative.display()
        ));
    }
    let mut path = dir.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if is_directory(&meta) => {}
            Ok(_) => return Err(format!("{} is no directory", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(Some(path))
}

/// A directory, and not a link or junction to one.
fn is_directory(meta: &std::fs::Metadata) -> bool {
    meta.is_dir() && meta.file_attributes() & REPARSE_POINT == 0
}

/// The owner of `path`, opened as itself.
fn owner(path: &Path) -> io::Result<Sid> {
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(OPEN_AS_ITSELF)
        .open(path)?;
    win32::file_owner(file.as_raw_handle())
}

/// Removes every entry `me` owns in `dir`, with everything beneath it — a link as a link — and
/// looks the same way into each directory there it can read.
fn remove_own(dir: &Path, me: &Sid) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut first = None;
    for entry in entries {
        let result = entry
            .and_then(|entry| {
                let path = entry.path();
                // Read from the directory, not through the entry: a link is seen as one.
                let meta = std::fs::symlink_metadata(&path)?;
                Ok((path, meta))
            })
            .map_err(|e| format!("{}: {e}", dir.display()))
            .and_then(|(path, meta)| {
                let gone = |result: io::Result<()>| match result {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => {
                        Err(format!("{}: {e}", path.display()))
                    }
                    _ => Ok(()),
                };
                match owner(&path) {
                    Ok(owner) if &owner == me => {
                        if is_directory(&meta) {
                            gone(std::fs::remove_dir_all(&path))
                        } else if meta.is_dir() {
                            gone(std::fs::remove_dir(&path))
                        } else {
                            gone(std::fs::remove_file(&path))
                        }
                    }
                    // A directory of the Client's it may not read holds nothing of its own.
                    Ok(_) if is_directory(&meta) => remove_own(&path, me).or(Ok(())),
                    Ok(_) => Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(format!("{}: {e}", path.display())),
                }
            });
        if let Err(e) = result {
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::super::{read_frame, write_frame, Channel, Request, ServiceProcess};
    use super::*;

    fn sid(text: &str) -> Sid {
        Sid::parse(text).expect("sid")
    }

    /// The process this test runs as, named as the second service's: it serves the in-process
    /// services (ADR-0059 clause 4).
    fn this_process() -> ServiceProcess {
        ServiceProcess::named(|| Ok(Some(std::process::id())))
    }

    /// Only the Client's account is served, and the service runs as neither `LocalSystem` nor the
    /// Client's account, nor serves `LocalSystem` (ADR-0059 clauses 1 and 4).
    /// Verifies: ADR-0059
    #[test]
    fn only_the_clients_account_is_admitted_and_never_local_system() {
        let client = sid("S-1-5-80-1-2-3-4-5");
        let own = sid("S-1-5-80-6-7-8-9-10");
        assert!(admit_sid(&client, &client).is_ok());
        let refused = admit_sid(&own, &client).expect_err("another account");
        assert!(refused.contains("S-1-5-80-6-7-8-9-10"), "{refused}");
        assert!(check_client(&client, &own).is_ok());
        assert!(check_client(&client, &sid(win32::LOCAL_SYSTEM)).is_err());
        assert!(check_client(&sid(win32::LOCAL_SYSTEM), &own).is_err());
        assert!(check_client(&own, &own).is_err());
    }

    /// A purge takes only the tombstone of the Supervisor it names, by an absolute path.
    /// Verifies: ADR-0059
    #[test]
    fn a_purge_takes_only_the_tombstone_of_the_supervisor_it_names() {
        let root = tempfile::tempdir().expect("tempdir");
        let tombstone = root.path().join(crate::reconfigure::tombstone_name("otel"));
        assert!(check_tombstone("otel", &tombstone).is_ok());
        assert!(check_tombstone("other", &tombstone).is_err());
        assert!(check_tombstone("otel", &root.path().join("otel")).is_err());
        assert!(check_tombstone("otel", Path::new(".purge-otel-7")).is_err());
        assert!(
            purge("otel", &tombstone, &[PathBuf::from("..")]).is_ok(),
            "gone already"
        );
        std::fs::create_dir(&tombstone).expect("tombstone");
        assert!(purge("otel", &tombstone, &[PathBuf::from("..")]).is_err());
    }

    /// What this account owns beneath a shared directory of the tombstone is removed, with
    /// everything beneath it, and nothing else. The second service runs under its own account,
    /// which owns what it creates; a test run elevated creates what the Administrators own, so the
    /// test makes this account the owner of what stands for the service's own.
    /// Verifies: ADR-0059
    #[test]
    fn the_second_service_removes_what_it_wrote_under_a_tombstone() {
        use crate::win32::files::{Access, Entry};
        let me = win32::own_sid().expect("own sid");
        let root = tempfile::tempdir().expect("tempdir");
        let tombstone = root.path().join(crate::reconfigure::tombstone_name("otel"));
        let data = tombstone.join("data");
        std::fs::create_dir_all(data.join("deep")).expect("data");
        std::fs::write(data.join("deep").join("state"), b"x").expect("state");
        std::fs::write(data.join("log"), b"x").expect("log");
        std::fs::write(data.join("theirs"), b"x").expect("theirs");
        for own in ["deep", "log"] {
            Entry::open(&data.join(own), Access::Owner, false)
                .and_then(|entry| entry.set_owner(me.as_str()))
                .expect("owned by this account");
        }
        let theirs = Entry::open(&data.join("theirs"), Access::Dacl, false)
            .and_then(|entry| entry.owner())
            .expect("owner");
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"x").expect("outside");
        purge("otel", &tombstone, &[PathBuf::from("data")]).expect("purge");
        assert!(!data.join("deep").exists() && !data.join("log").exists());
        assert_eq!(
            data.join("theirs").exists(),
            theirs != me,
            "what another account owns stays"
        );
        assert!(outside.exists());
    }

    fn unique_pipe() -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\opamp-fleet-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    /// The second service in process, serving this test's account on a pipe of its own, and the
    /// Client's channel to it.
    async fn served() -> (Channel, tokio::sync::oneshot::Sender<()>, PathBuf) {
        let pipe = unique_pipe();
        let me = win32::own_sid().expect("own sid");
        let listener = bind(&pipe, &me).expect("bind");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(super::super::serve(listener, me.clone(), || false, async {
            let _ = stopped.await;
        }));
        let channel = Channel::connect(&pipe, me, this_process())
            .await
            .expect("connect");
        (channel, stop, pipe)
    }

    fn cmd(script: &str) -> Launch {
        Launch {
            program: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            args: vec!["/c".to_string(), script.to_string()],
            env: Vec::new(),
            endpoint_token: None,
            working_dir: None,
            own_process_group: true,
        }
    }

    /// A program the second service starts on the Client's request runs, reports its output and
    /// its exit code, through the pipe.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_started_program_reports_its_output_and_exit() {
        let (channel, _stop, _pipe) = served().await;
        let output = channel
            .output("test", cmd("echo hello& exit 3"))
            .await
            .expect("output");
        assert_eq!(output.status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("hello"),
            "{output:?}"
        );
    }

    /// A signal is refused on Windows, which has none.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_signal_is_refused() {
        let (channel, _stop, _pipe) = served().await;
        let mut child = channel
            .start("test", cmd("ping -n 30 127.0.0.1 >nul"))
            .await
            .expect("start");
        assert!(child.signal(1, true).await.is_err());
        child.kill().await.expect("kill");
    }

    /// The pipe's owner is the account that made it, and the Client takes it for no other.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn the_client_talks_only_to_the_process_account() {
        let (channel, _stop, pipe) = served().await;
        drop(channel);
        let me = win32::own_sid().expect("own sid");
        let other = sid("S-1-5-80-1-2-3-4-5");
        let refused = Channel::connect(&pipe, other, this_process())
            .await
            .err()
            .expect("another account serves it");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
        assert!(refused.to_string().contains(me.as_str()), "{refused}");
        // Until the service has a new instance waiting, every one is busy: the Client's channel
        // tries again, as this does.
        let stream = connect_when_free(&pipe).await;
        assert_eq!(server_of(&stream).expect("owner"), me);
    }

    /// The pipe's server is the process the Client reads, and the Client takes no instance served
    /// by a process other than the one the SCM names as the second service's (ADR-0059 clause 4).
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn the_client_talks_only_to_the_process_the_scm_names() {
        let (channel, _stop, pipe) = served().await;
        drop(channel);
        let me = win32::own_sid().expect("own sid");
        let own = std::process::id();
        let other = own.wrapping_add(1);
        let refused = Channel::connect(&pipe, me, ServiceProcess::named(move || Ok(Some(other))))
            .await
            .err()
            .expect("another process serves it");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
        assert!(
            refused
                .to_string()
                .contains(&format!("served by process {own}")),
            "{refused}"
        );
        let stream = connect_when_free(&pipe).await;
        assert_eq!(
            server_process(&stream).expect("the server's process").pid,
            own
        );
    }

    /// Connects to `pipe` once an instance is free: `ERROR_PIPE_BUSY` while the service has none
    /// waiting yet.
    async fn connect_when_free(pipe: &Path) -> NamedPipeClient {
        const PIPE_BUSY: i32 = 231;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match connect(pipe) {
                Err(e)
                    if e.raw_os_error() == Some(PIPE_BUSY) && std::time::Instant::now() < until =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                connected => return connected.expect("connect"),
            }
        }
    }

    /// A pipe of that name made by someone first is not taken over.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_pipe_made_first_by_another_is_not_taken_over() {
        let pipe = unique_pipe();
        let me = win32::own_sid().expect("own sid");
        let _first = bind(&pipe, &me).expect("bind");
        assert!(bind(&pipe, &me).is_err());
    }

    /// A client of another account is refused once it has written, and the service goes on
    /// serving the Client's.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_peer_of_another_account_is_refused() {
        let pipe = unique_pipe();
        let me = win32::own_sid().expect("own sid");
        let listener = bind(&pipe, &me).expect("bind");
        let other = sid("S-1-5-80-1-2-3-4-5");
        let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(super::super::serve(listener, other, || false, async {
            let _ = stopped.await;
        }));
        let mut stream = connect_when_free(&pipe).await;
        write_frame(
            &mut stream,
            &Request::Hello {
                version: fleet_core::version::current().to_string(),
            },
        )
        .await
        .expect("hello");
        let reply: Option<Reply> = read_frame(&mut stream).await.expect("reply");
        assert!(
            matches!(&reply, Some(Reply::Refused { reason, .. }) if reason.contains("is not the Client's account")),
            "{reply:?}"
        );
    }

    /// Closing the Client's connection ends every program it started, and what that program
    /// started and left running: the job holds them all (ADR-0059 clause 4).
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn closing_the_connection_ends_the_whole_job() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("alive");
        let (channel, _stop, _pipe) = served().await;
        // The program starts a process of its own and ends; that process writes on. Its loop is
        // a script of its own: quotes inside an argument do not reach `cmd` as `cmd` reads them.
        let looping = dir.path().join("loop.cmd");
        std::fs::write(
            &looping,
            format!(
                "@echo off\r\n:again\r\necho x>>\"{}\"\r\nping -n 2 127.0.0.1 >nul\r\ngoto again\r\n",
                marker.display()
            ),
        )
        .expect("loop.cmd");
        let mut launch = cmd("start");
        launch.args = vec![
            "/c".to_string(),
            "start".to_string(),
            "/b".to_string(),
            r"C:\Windows\System32\cmd.exe".to_string(),
            "/c".to_string(),
            looping.display().to_string(),
        ];
        let mut child = channel.start("test", launch).await.expect("start");
        let _ = child.wait().await;
        let size = || std::fs::metadata(&marker).map(|m| m.len()).unwrap_or(0);
        let wait = || tokio::time::sleep(std::time::Duration::from_secs(3));
        let before = size();
        wait().await;
        assert!(size() > before, "what the program left behind runs");
        drop(child);
        drop(channel);
        wait().await;
        let after = size();
        wait().await;
        assert_eq!(size(), after, "and ends with the connection");
    }

    /// The Supervisor Endpoint's token is laid over the program's environment last (ADR-0059
    /// clause 7).
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_program_has_the_token_last() {
        let (channel, _stop, _pipe) = served().await;
        let mut launch = cmd("echo %OPAMP_SUPERVISOR_TOKEN%");
        launch.env = vec![(
            "OPAMP_SUPERVISOR_TOKEN".to_string(),
            "from the block".to_string(),
        )];
        launch.endpoint_token = Some(zeroize::Zeroizing::new("the token".to_string()));
        let output = channel.output("test", launch).await.expect("output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("the token") && !stdout.contains("from the block"),
            "{output:?}"
        );
    }

    /// What [`a_second_service_listens_only_once_closed_to_the_process_account`] runs its helper
    /// with: the pipe it may listen on.
    const CLOSING_PIPE: &str = "FLEET_CLOSING_PIPE";
    const CLOSING_HELPER: &str =
        "supervisor::channel::windows::tests::close_and_listen_as_the_second_service_would";
    /// The Client's account the helper closes its process to: one that is not its own.
    const CLIENT: &str = "S-1-5-80-1-2-3-4-5";

    /// Runs `body` on the runtime the second service runs on, whose threads close themselves.
    fn on_the_agents_runtime<T>(body: impl Future<Output = T>) -> T {
        crate::service::runtime::agents_runtime()
            .expect("the runtime")
            .block_on(body)
    }

    /// A thread that runs until it is told to end, and is joined then.
    fn parked_thread() -> (
        u32,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let (end, ended) = std::sync::mpsc::channel::<()>();
        let joined = std::thread::spawn(move || {
            id_tx.send(win32::this_thread_id()).expect("send");
            let _ = ended.recv();
        });
        (id_rx.recv().expect("its id"), end, joined)
    }

    /// Not a test of its own: what [`a_second_service_listens_only_once_closed_to_the_process_account`]
    /// runs in a process of its own, as the second service's program would, since closing a
    /// process cannot be undone for the rest of it.
    #[test]
    #[ignore = "run in a process of its own by a_second_service_listens_only_once_closed_to_the_process_account"]
    fn close_and_listen_as_the_second_service_would() {
        let Ok(pipe) = std::env::var(CLOSING_PIPE) else {
            return;
        };
        let pipe = PathBuf::from(pipe);
        let me = win32::own_sid().expect("own sid");
        let client = sid(CLIENT);
        let process = win32::process_sddl(&me, &client);
        let thread = win32::thread_sddl(&me);
        let gone =
            |pipe: &Path| matches!(connect(pipe), Err(e) if e.kind() == io::ErrorKind::NotFound);

        // Not yet closed: it does not listen.
        let refused = on_the_agents_runtime(run(&pipe, client.clone(), async {}))
            .expect_err("it listened before it closed its process");
        assert!(refused.contains("not closed"), "{refused}");
        assert!(gone(&pipe));

        // Closed as the program does first: the process and every thread read back as set.
        close_this_process(Some(CLIENT));
        assert_eq!(CLOSED.get(), Some(&Ok(thread.clone())));
        check_closed(&process, &thread).expect("closed");
        assert!(
            win32::same_dacl(&win32::this_process_dacl().expect("dacl"), &process)
                .expect("compared")
        );

        // The runtime's threads close themselves, the blocking ones among them.
        on_the_agents_runtime(async {
            tokio::task::spawn_blocking(|| ()).await.expect("blocking");
            // A worker closes itself once it has started running: that is waited for, briefly.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while let Err(e) = check_closed(&process, &thread) {
                assert!(
                    std::time::Instant::now() < until,
                    "the runtime's threads did not close themselves: {e}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });

        // A thread that did not close itself differs, and is closed before the service listens.
        let (open, end, joined) = parked_thread();
        let differs = check_closed(&process, &thread).expect_err("an open thread passed");
        assert!(differs.contains(&format!("thread {open}")), "{differs}");
        let served = on_the_agents_runtime(run(&pipe, client.clone(), async {}))
            .expect("closed, it listens");
        assert_eq!(served, Served::Stopped);
        let held = win32::thread_dacl(open).expect("read").expect("runs");
        assert!(
            win32::same_dacl(&held, &thread).expect("compared"),
            "{held}"
        );
        end.send(()).expect("end");
        joined.join().expect("joined");

        // A thread that carries another DACL, which this account may not change: no listening.
        let (other, end, joined) = parked_thread();
        assert!(win32::set_thread_dacl(other, "D:P(A;;RC;;;OW)").expect("set"));
        let refused = on_the_agents_runtime(run(&pipe, client.clone(), async {}))
            .expect_err("it listened with a thread of another DACL");
        assert!(refused.contains(&format!("thread {other}")), "{refused}");
        assert!(gone(&pipe));
        end.send(()).expect("end");
        joined.join().expect("joined");

        // A process that carries another DACL: no listening.
        win32::set_this_process_dacl(&win32::process_sddl(&me, &sid("S-1-5-80-6-7-8-9-10")))
            .expect("another process DACL");
        let refused = on_the_agents_runtime(run(&pipe, client, async {}))
            .expect_err("it listened with a process of another DACL");
        assert!(
            refused.contains("the second service's process carries"),
            "{refused}"
        );
        assert!(gone(&pipe));
        println!("closed and read back");
    }

    /// The second service's process and every one of its threads carry the DACLs of ADR-0062 and
    /// read back as such; one whose process or a thread carries another does not listen, a thread
    /// of its runtime closes itself, and one the system started is closed before it listens. In a
    /// process of its own, the test binary's helper above.
    /// Verifies: ADR-0062
    #[test]
    fn a_second_service_listens_only_once_closed_to_the_process_account() {
        let exe = std::env::current_exe().expect("the test binary");
        let out = std::process::Command::new(exe)
            .args([CLOSING_HELPER, "--exact", "--ignored", "--nocapture"])
            .env(CLOSING_PIPE, unique_pipe())
            .output()
            .expect("run the helper");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{said}");
        assert!(said.contains("closed and read back"), "{said}");
    }

    /// A program that runs until it is ended.
    fn a_process() -> std::process::Child {
        std::process::Command::new(r"C:\Windows\System32\cmd.exe")
            .args(["/c", "ping -n 60 127.0.0.1 >nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn")
    }

    /// A peer whose process the Client cannot open is refused as one that cannot be read, before
    /// the SCM's answer counts (ADR-0062). The process is one that does not exist: a DACL closed
    /// to everyone does not stop a caller holding `SeDebugPrivilege`, as CI's elevated runner may.
    /// Verifies: ADR-0062
    #[tokio::test]
    async fn the_client_refuses_a_peer_whose_process_it_cannot_open() {
        // Process ids are multiples of four; this one is never handed out.
        let pid = u32::MAX - 3;
        let peer = peer_process(pid);
        let refused = super::super::check_process(
            peer,
            &ServiceProcess::named(move || Ok(Some(pid))),
            Path::new(r"\\.\pipe\opamp-fleet-test"),
        )
        .await
        .expect_err("a process it cannot open passed");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
        assert_eq!(
            super::super::other_process(&refused),
            Some(Some(pid)),
            "logged by its id: {refused}"
        );
        assert!(refused.to_string().contains("cannot be read"), "{refused}");
        assert!(
            refused
                .to_string()
                .contains(&format!("process {pid} cannot be opened")),
            "{refused}"
        );
    }

    /// The Client holds the peer's process while the SCM is asked: one that has ended once the SCM
    /// has answered is refused although the SCM names its id, and one that still runs is accepted
    /// (ADR-0062).
    /// Verifies: ADR-0062
    #[tokio::test]
    async fn the_client_refuses_a_peer_whose_process_ended_once_the_scm_answered() {
        let socket = Path::new(r"\\.\pipe\opamp-fleet-test");
        let child = std::sync::Arc::new(std::sync::Mutex::new(a_process()));
        let pid = child.lock().expect("child").id();
        super::super::check_process(
            peer_process(pid),
            &ServiceProcess::named(move || Ok(Some(pid))),
            socket,
        )
        .await
        .expect("a peer that still runs is accepted");

        let ending = {
            let child = std::sync::Arc::clone(&child);
            ServiceProcess::named(move || {
                let mut child = child.lock().expect("child");
                let _ = child.kill();
                let _ = child.wait();
                Ok(Some(pid))
            })
        };
        let refused = super::super::check_process(peer_process(pid), &ending, socket)
            .await
            .expect_err("a process that ended passed");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
        assert!(refused.to_string().contains("has ended"), "{refused}");
    }
}
