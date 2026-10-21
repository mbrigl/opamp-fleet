//! The channel between the Client and the second service (ADR-0059): the private protocol both ends
//! speak, the second service that starts, signals and stops programs on the Client's request, and
//! the Client's end, through which [`Spawner::Channel`](super::process::Spawner) starts a Managed
//! Process.
//!
//! The protocol is private to the build (ADR-0059 clause 5): both ends run from the same `current`,
//! so a message is a length-prefixed JSON document — a four-byte big-endian length, then the body —
//! and the first message each way is a handshake that names the build. A peer that names another
//! build is refused.
//!
//! The second service admits one peer, the Client's account, checked by the socket's peer
//! credentials, and serves one connection at a time. When that connection closes it stops every
//! program it started, by the program's process group, so a Client that restarts finds no process
//! it did not start (ADR-0059 clause 4).
//!
//! Unix only: on Linux and macOS the channel is a Unix socket.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::supervisor::process::Launch;

/// The largest message either end accepts. A request carries one program's command line and
/// environment, a reply one line of output; neither comes near it.
const MAX_FRAME: u32 = 1 << 20;

/// The longest line of a program's output relayed as one message; a longer one arrives in pieces.
const MAX_LINE: usize = 16 * 1024;

/// How long the handshake may take before the connection is dropped.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// The largest handshake either end reads: a build name, or a refusal's reason. Until a peer is
/// served it gets no more of the service's memory than this.
const MAX_HANDSHAKE: u32 = 4 * 1024;

/// How often a refused connection is logged at most; the refusals in between are counted into the
/// next warning. A process that can reach the socket cannot fill the log with them.
const REFUSAL_LOG_EVERY: Duration = Duration::from_secs(60);

/// How long a program has to end on `SIGTERM` when the Client's connection closes, before its
/// group is killed. The service manager's stop timeout for the second service is longer, so a stop
/// of the service ends its programs itself rather than having the manager kill it first.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// How long the Client waits for the service to answer a signal. The service sends one at once;
/// it answers late only while it stops its programs, on shutting down, and a stop or a reload that
/// waited for that would wait out [`CLOSE_GRACE`] before its own bound began.
pub(crate) const SIGNAL_ANSWER: Duration = Duration::from_secs(2);

/// How long the Client waits for the end of a program it asked the service to kill: an answer's
/// time and the [`OUTPUT_DRAIN`] the exit report waits for the program's last output.
pub(crate) const KILL_ANSWER: Duration = SIGNAL_ANSWER.saturating_add(OUTPUT_DRAIN);

/// How many lines of output may wait to be written to the Client. A program that writes faster
/// than they go out waits on its pipe, as it would on a slow terminal; nothing else waits on it.
const OUTPUT_QUEUE: usize = 256;

/// How long the exit report waits for the rest of a program's output. A process the program left
/// behind may hold its pipes open; its output is still relayed, after the exit, until the
/// connection ends.
pub(crate) const OUTPUT_DRAIN: Duration = Duration::from_secs(1);

/// How long a start waits for the second service to come back when it finds the connection gone:
/// twice the delay after which the service manager restarts a service that failed, so a start
/// outlasts one restart of the second service.
const RECONNECT: Duration =
    Duration::from_secs(2 * crate::service::manager::RESTART_DELAY_SECS as u64);

/// How often a start tries to reach the second service again while it waits for it.
const RECONNECT_EVERY: Duration = Duration::from_millis(200);

/// What the Client asks of the second service.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Request {
    /// The first message: the Client's build.
    Hello { version: String },
    /// Start a program. `id` is the Client's name for it in every later message.
    Start { id: u64, launch: Launch },
    /// Send `signal` to the program, or, with `group`, to its process group where the program was
    /// asked to lead one ([`Launch::own_process_group`]). Answered, in order, by
    /// [`Reply::Signalled`] or [`Reply::SignalFailed`].
    Signal { id: u64, signal: i32, group: bool },
    /// Kill the program, and its process group where it was asked to lead one.
    Stop { id: u64 },
}

/// What the second service tells the Client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Reply {
    /// The answer to [`Request::Hello`]: the service's build, the same as the Client's.
    Hello { version: String },
    /// The connection is not served, and why. The service closes it after this.
    Refused { reason: String },
    /// The program runs, as `pid`.
    Started { id: u64, pid: u32 },
    /// The program did not start. `os_error` keeps the kind of failure — a missing file, a program
    /// momentarily busy — that the Client's supervision tells apart.
    StartFailed {
        id: u64,
        error: String,
        os_error: Option<i32>,
    },
    /// The signal was sent — or the program has ended, and there was nothing to signal.
    Signalled { id: u64 },
    /// The signal could not be sent, and why.
    SignalFailed { id: u64, error: String },
    /// One line the program wrote, without its line end.
    Output {
        id: u64,
        stream: Stream,
        line: String,
    },
    /// The program ended: its raw wait status, or why waiting for it failed.
    Exited {
        id: u64,
        status: Result<i32, String>,
    },
}

/// Which of a program's output streams a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn name(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// The build both ends must share (ADR-0059 clause 5).
fn this_build() -> &'static str {
    fleet_core::version::current()
}

/// One message as it goes on the wire: its length, then its body.
///
/// # Errors
/// Returns `InvalidInput` for a message larger than [`MAX_FRAME`], which no peer would read.
pub(crate) fn encode<T: Serialize>(message: &T) -> io::Result<Zeroizing<Vec<u8>>> {
    // A start carries the Supervisor Endpoint's token, so every frame is wiped (ADR-0057). The
    // frame is sized before it is written: a buffer that grew would leave copies behind unwiped.
    let mut count = Count(0);
    serde_json::to_writer(&mut count, message).map_err(io::Error::other)?;
    let len = u32::try_from(count.0)
        .ok()
        .filter(|len| *len <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
    let mut frame = Zeroizing::new(Vec::with_capacity(4 + count.0));
    frame.extend_from_slice(&len.to_be_bytes());
    serde_json::to_writer(&mut *frame, message).map_err(io::Error::other)?;
    debug_assert_eq!(frame.capacity(), 4 + count.0, "the frame grew");
    Ok(frame)
}

/// Counts the bytes written to it and keeps none.
struct Count(usize);

impl io::Write for Count {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) async fn write_frame<W, T>(writer: &mut W, message: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let frame = encode(message)?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Reads one message; `None` when the peer closed the connection between two messages.
///
/// Not cancel-safe: each end reads in a task of its own and hands the messages on.
pub(crate) async fn read_frame<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_frame_within(reader, MAX_FRAME).await
}

/// [`read_frame`] for a message of at most `max` bytes: a longer one is refused before its body
/// is read.
async fn read_frame_within<R, T>(reader: &mut R, max: u32) -> io::Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len);
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = Zeroizing::new(vec![0; len as usize]);
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------------------------
// The second service
// ---------------------------------------------------------------------------------------------

/// Whether a connecting peer is the Client's account (ADR-0059 clause 4): only its uid is served.
///
/// # Errors
/// Returns why the peer is refused.
pub fn admit(peer_uid: u32, client_uid: u32) -> Result<(), String> {
    if peer_uid == client_uid {
        Ok(())
    } else {
        Err(format!(
            "uid {peer_uid} is not the Client's account (uid {client_uid})"
        ))
    }
}

/// Whether the service, running as `own_uid`, may serve `client_uid` as the Client's account
/// (ADR-0059 clause 1): the service itself never runs as root, the Client's account is never root,
/// and never the process account the service runs as — a service that served its own account
/// would separate nothing.
///
/// # Errors
/// Returns why the service does not start.
pub fn check_client_uid(client_uid: u32, own_uid: u32) -> Result<(), String> {
    if own_uid == 0 {
        return Err(
            "the second service runs as root: it runs under the process account, never as root"
                .to_string(),
        );
    }
    if client_uid == 0 {
        return Err("--client-uid 0: the Client's account is never root".to_string());
    }
    if client_uid == own_uid {
        return Err(format!(
            "--client-uid {client_uid} is the account this service runs as: the process account \
             is never the Client's"
        ));
    }
    Ok(())
}

/// Binds the second service's socket at `path`.
///
/// A socket left behind by an earlier run is removed first; anything else at that path is not, and
/// the bind fails on it. The socket is made `0660`: the Client's account reaches it through the
/// process account's group (ADR-0059 clause 6), and the peer check decides who is served.
///
/// # Errors
/// Returns an error when the path holds something other than a socket, or the bind fails.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// How the second service ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// `SIGTERM` or `SIGINT`: a clean exit, and the service manager leaves it stopped.
    Stopped,
    /// The Client is another build, and `current` no longer names the version this service runs:
    /// the self-update switched it. The process exits non-zero so the service manager starts the
    /// version `current` names, as it does for the Client (ADR-0059 clause 2).
    Superseded,
}

/// `supervisor run --agents <socket> --client-uid <uid>`: the second service (ADR-0059), until
/// `SIGTERM` or `SIGINT`, or until a self-update leaves it behind. `SIGHUP` is ignored, as the
/// Client ignores it (ADR-0019 clause 12): its default disposition would end the service and leave
/// its programs running.
///
/// # Errors
/// Returns an error when the service runs as root, `client_uid` is root's or the service's own,
/// the runtime cannot be built, or the socket cannot be bound.
pub fn run_service(socket: &Path, client_uid: u32) -> Result<Served, String> {
    // SAFETY: geteuid(2) cannot fail and touches no memory.
    check_client_uid(client_uid, unsafe { libc::geteuid() })?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    runtime.block_on(async {
        // Before the socket is bound: whoever sees it may already signal the service.
        crate::service::runtime::ignore_sighup();
        let listener =
            bind(socket).map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;
        info!(socket = %socket.display(), client_uid, "second service listening");
        let served = serve(listener, client_uid, superseded, terminated()).await;
        let _ = std::fs::remove_file(socket);
        if served == Served::Superseded {
            info!(
                "the Client is another build and current names another version; exiting so the \
                 service manager starts that one"
            );
        }
        Ok(served)
    })
}

/// Whether `current` names a version other than the one this program runs from: a self-update
/// switched it. `false` outside a layout, where there is nothing to switch to.
fn superseded() -> bool {
    use crate::service::layout::{running_exe, Layout};
    running_exe()
        .ok()
        .and_then(|exe| Layout::enclosing(&exe))
        .is_some_and(|(layout, running_dir)| !layout.names(&running_dir))
}

/// `SIGTERM` (what a service manager stops with) or `SIGINT`.
async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

/// Serves the Client on `listener` until `shutdown` resolves, one connection at a time. When a
/// connection closes, or at the shutdown, every program it started is stopped.
///
/// A Client of another build is refused; when `superseded` then says that this service is the one
/// a self-update left behind, it ends with [`Served::Superseded`], so the version the Client runs
/// takes its place. A Client left behind is only refused, until it is restarted itself.
pub async fn serve(
    listener: UnixListener,
    client_uid: u32,
    superseded: impl Fn() -> bool,
    shutdown: impl Future<Output = ()>,
) -> Served {
    let mut shutdown = std::pin::pin!(shutdown);
    let mut refusals = Refusals::default();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    warn!(error = %e, "cannot accept a connection");
                    // Out of descriptors, typically: do not spin on it.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = shutdown.as_mut() => return Served::Stopped,
        };
        let peer = stream
            .peer_cred()
            .map_err(|e| format!("cannot read the peer's credentials: {e}"))
            .and_then(|cred| admit(cred.uid(), client_uid));
        if let Err(reason) = peer {
            refuse(stream, reason, &mut refusals);
            continue;
        }
        match serve_connection(stream, &listener, shutdown.as_mut(), &mut refusals).await {
            Ended::Shutdown => return Served::Stopped,
            Ended::OtherBuild if superseded() => return Served::Superseded,
            Ended::Closed | Ended::OtherBuild => {}
        }
    }
}

/// Tells a peer why it is not served, and closes the connection — at once, reading nothing from
/// it: the refusal goes into the socket's empty buffer without waiting, so no refused peer holds a
/// task, a descriptor or a read of the service's.
fn refuse(stream: UnixStream, reason: String, refusals: &mut Refusals) {
    if let Some(unlogged) = refusals.note(std::time::Instant::now()) {
        warn!(reason = %reason, unlogged, "refused a connection");
    }
    // Written straight to the socket: a stream just accepted has no readiness yet, so tokio's own
    // `try_write` would only ever answer that it would block.
    if let (Ok(frame), Ok(stream)) = (encode(&Reply::Refused { reason }), stream.into_std()) {
        use std::io::Write as _;
        let _ = (&stream).write(&frame);
    }
}

/// The refusals logged so far: one warning per [`REFUSAL_LOG_EVERY`] at most.
#[derive(Default)]
struct Refusals {
    logged: Option<std::time::Instant>,
    unlogged: u64,
}

impl Refusals {
    /// Counts one refusal at `now`; `Some` with the refusals not logged since the last warning
    /// when this one is to be logged.
    fn note(&mut self, now: std::time::Instant) -> Option<u64> {
        if self
            .logged
            .is_some_and(|logged| now.duration_since(logged) < REFUSAL_LOG_EVERY)
        {
            self.unlogged += 1;
            return None;
        }
        self.logged = Some(now);
        Some(std::mem::take(&mut self.unlogged))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Closed,
    /// The Client is another build, and was refused.
    OtherBuild,
    Shutdown,
}

/// What the connection asks of one running program.
enum Control {
    Signal {
        signal: i32,
        group: bool,
    },
    Stop,
    /// The connection closed: stop the program's whole group, gracefully first.
    Close,
}

async fn serve_connection<F: Future<Output = ()>>(
    stream: UnixStream,
    listener: &UnixListener,
    mut shutdown: Pin<&mut F>,
    refusals: &mut Refusals,
) -> Ended {
    let (mut read, mut write) = stream.into_split();
    let hello = tokio::time::timeout(
        HANDSHAKE,
        read_frame_within::<_, Request>(&mut read, MAX_HANDSHAKE),
    )
    .await;
    let refusal = match hello {
        Ok(Ok(Some(Request::Hello { version }))) if version == this_build() => None,
        Ok(Ok(Some(Request::Hello { version }))) => Some((
            format!(
                "the Client is build {version}, this service is build {}",
                this_build()
            ),
            Ended::OtherBuild,
        )),
        Ok(Ok(Some(_))) => Some((
            "the first message must be the handshake".to_string(),
            Ended::Closed,
        )),
        Ok(Ok(None)) => return Ended::Closed,
        Ok(Err(e)) => Some((format!("unreadable handshake: {e}"), Ended::Closed)),
        Err(_) => Some(("no handshake in time".to_string(), Ended::Closed)),
    };
    if let Some((reason, ended)) = refusal {
        warn!(reason = %reason, "refused a connection");
        let _ = write_frame(&mut write, &Reply::Refused { reason }).await;
        return ended;
    }
    let welcome = Reply::Hello {
        version: this_build().to_string(),
    };
    if write_frame(&mut write, &welcome).await.is_err() {
        return Ended::Closed;
    }
    info!("the Client connected");

    let (replies, mut reply_rx) = Replies::new();
    let writer = tokio::spawn(async move {
        while let Some(Queued { reply, permit }) = reply_rx.recv().await {
            // A reply too large to send is lost alone; only a broken connection ends the writer.
            let frame = match encode(&reply) {
                Ok(frame) => frame,
                Err(e) => {
                    warn!(error = %e, "cannot send a reply to the Client");
                    continue;
                }
            };
            if write.write_all(&frame).await.is_err() {
                break;
            }
            drop(permit);
        }
    });
    let (request_tx, mut requests) = mpsc::channel::<Request>(64);
    let reader = tokio::spawn(async move {
        while let Ok(Some(request)) = read_frame::<_, Request>(&mut read).await {
            if request_tx.send(request).await.is_err() {
                break;
            }
        }
    });

    let mut programs: HashMap<u64, mpsc::UnboundedSender<Control>> = HashMap::new();
    let mut running: JoinSet<Supervised> = JoinSet::new();
    // The output of processes the programs left behind, still relayed after their exit until the
    // connection ends.
    let mut left_behind: Vec<JoinHandle<()>> = Vec::new();
    let ended = loop {
        tokio::select! {
            request = requests.recv() => match request {
                // An id names one program until it has ended: another start under it would take
                // over the control of the one that runs.
                Some(Request::Start { id, .. }) if programs.contains_key(&id) => {
                    warn!(id, "a start under the id of a running program; refused");
                    replies.send(Reply::StartFailed {
                        id,
                        error: format!("id {id} names a running program"),
                        os_error: None,
                    });
                }
                Some(Request::Start { id, launch }) => match start(&launch) {
                    Ok(child) => {
                        let pid = child.id().unwrap_or_default();
                        info!(id, pid, program = %launch.program.display(), "program started");
                        replies.send(Reply::Started { id, pid });
                        let (control_tx, control_rx) = mpsc::unbounded_channel();
                        programs.insert(id, control_tx);
                        running.spawn(supervise(
                            id,
                            child,
                            launch.own_process_group,
                            control_rx,
                            replies.clone(),
                        ));
                    }
                    Err(e) => {
                        warn!(id, program = %launch.program.display(), error = %e, "cannot start a program");
                        replies.send(Reply::StartFailed {
                            id,
                            error: e.to_string(),
                            os_error: e.raw_os_error(),
                        });
                    }
                },
                Some(Request::Signal { id, signal, group }) => match programs.get(&id) {
                    Some(control) => {
                        let _ = control.send(Control::Signal { signal, group });
                    }
                    // Ended, as far as this connection knows: nothing to signal, as for a program
                    // the Client has waited for itself.
                    None => replies.send(Reply::Signalled { id }),
                },
                Some(Request::Stop { id }) => {
                    if let Some(control) = programs.get(&id) {
                        let _ = control.send(Control::Stop);
                    }
                }
                Some(Request::Hello { .. }) => warn!("a second handshake on one connection; ignored"),
                None => break Ended::Closed,
            },
            Some(done) = running.join_next() => {
                if let Ok(Supervised { id, mut unanswered, relays }) = done {
                    programs.remove(&id);
                    left_behind.retain(|relay| !relay.is_finished());
                    left_behind.extend(relays);
                    // Signals that came after the program ended still get their answer, here, in
                    // the order they were asked.
                    unanswered.close();
                    while let Ok(control) = unanswered.try_recv() {
                        if let Control::Signal { .. } = control {
                            replies.send(Reply::Signalled { id });
                        }
                    }
                }
            }
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    refuse(stream, "another connection is being served".to_string(), refusals);
                }
            }
            () = shutdown.as_mut() => break Ended::Shutdown,
        }
    };
    // Whatever ended the connection, nothing it started outlives it (ADR-0059 clause 4).
    for control in programs.values() {
        let _ = control.send(Control::Close);
    }
    while let Some(done) = running.join_next().await {
        if let Ok(Supervised { relays, .. }) = done {
            left_behind.extend(relays);
        }
    }
    // A process that left its program's group — through `setsid` — outlives the stop, and its pipe
    // would hold a relay and a descriptor of the service's for as long as it keeps the pipe open.
    // Its output ends with the connection it was relayed to.
    for relay in &left_behind {
        relay.abort();
    }
    reader.abort();
    // The exits go out before the connection does — bounded, since output a left-behind process
    // still relays holds a sender too.
    drop(replies);
    let writing = writer.abort_handle();
    if tokio::time::timeout(OUTPUT_DRAIN, writer).await.is_err() {
        writing.abort();
    }
    info!("the Client's connection ended; its programs are stopped");
    ended
}

/// The replies to the Client, written in the order they are queued. Queuing never waits, so the
/// connection's loop and a program's supervision never wait on the Client; only output, which a
/// program can make without end, is held to [`OUTPUT_QUEUE`] lines, and the program's relay waits
/// for room.
#[derive(Clone)]
struct Replies {
    queue: mpsc::UnboundedSender<Queued>,
    output: Arc<Semaphore>,
}

/// A reply on its way to the Client; an output line holds its place in the output queue until it
/// is written.
struct Queued {
    reply: Reply,
    permit: Option<OwnedSemaphorePermit>,
}

impl Replies {
    fn new() -> (Replies, mpsc::UnboundedReceiver<Queued>) {
        let (queue, queued) = mpsc::unbounded_channel();
        let replies = Replies {
            queue,
            output: Arc::new(Semaphore::new(OUTPUT_QUEUE)),
        };
        (replies, queued)
    }

    /// Queues a reply that is not output: one per request or per program, never waiting.
    fn send(&self, reply: Reply) {
        let _ = self.queue.send(Queued {
            reply,
            permit: None,
        });
    }

    /// Queues a line of output once there is room for it; `false` once the connection is gone.
    async fn output(&self, reply: Reply) -> bool {
        let Ok(permit) = Arc::clone(&self.output).acquire_owned().await else {
            return false;
        };
        self.queue
            .send(Queued {
                reply,
                permit: Some(permit),
            })
            .is_ok()
    }
}

/// Starts a program as the leader of a process group of its own, always: whatever its own
/// [`Launch::own_process_group`] says, the stop at the connection's end takes its whole tree.
fn start(launch: &Launch) -> io::Result<Child> {
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    launch.set_env(&mut command);
    if let Some(dir) = &launch.working_dir {
        command.current_dir(dir);
    }
    command.spawn()
}

/// A program that has ended, as its supervision hands it back to the connection.
struct Supervised {
    id: u64,
    /// What was asked of the program that it did not get to.
    unanswered: mpsc::UnboundedReceiver<Control>,
    /// The relays of its output that did not finish within [`OUTPUT_DRAIN`]: a process it left
    /// behind holds the pipe.
    relays: Vec<JoinHandle<()>>,
}

/// Watches one program: relays its output, carries out what the connection asks, and reports its
/// exit. Returns once the program has ended.
async fn supervise(
    id: u64,
    mut child: Child,
    own_process_group: bool,
    mut control: mpsc::UnboundedReceiver<Control>,
    replies: Replies,
) -> Supervised {
    let group = child.id();
    let stdout = child
        .stdout
        .take()
        .map(|pipe| tokio::spawn(relay(id, Stream::Stdout, pipe, replies.clone())));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| tokio::spawn(relay(id, Stream::Stderr, pipe, replies.clone())));
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            Some(request) = control.recv() => match request {
                Control::Signal { signal, group } => {
                    let sent = match child.id() {
                        Some(pid) => send_signal(pid, signal, group && own_process_group),
                        None => Ok(()),
                    };
                    replies.send(match sent {
                        Ok(()) => Reply::Signalled { id },
                        Err(e) => Reply::SignalFailed {
                            id,
                            error: e.to_string(),
                        },
                    });
                }
                Control::Stop => {
                    if let Some(pid) = child.id() {
                        let _ = send_signal(pid, libc::SIGKILL, own_process_group);
                    }
                    let _ = child.start_kill();
                }
                Control::Close => break close(&mut child, group).await,
            },
        }
    };
    let mut relays = Vec::new();
    let drained = tokio::time::Instant::now() + OUTPUT_DRAIN;
    for mut relay in [stdout, stderr].into_iter().flatten() {
        if tokio::time::timeout_at(drained, &mut relay).await.is_err() {
            relays.push(relay);
        }
    }
    let status = status.map(ExitStatus::into_raw).map_err(|e| e.to_string());
    replies.send(Reply::Exited { id, status });
    Supervised {
        id,
        unanswered: control,
        relays,
    }
}

/// The stop at the connection's end: `SIGTERM` to the program's group, a bounded wait, then
/// `SIGKILL` to the group — sent even when the program ended in time, for anything it left behind.
async fn close(child: &mut Child, group: Option<u32>) -> io::Result<std::process::ExitStatus> {
    if let Some(pgid) = group {
        let _ = send_signal(pgid, libc::SIGTERM, true);
    }
    let status = match tokio::time::timeout(CLOSE_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    if let Some(pgid) = group {
        let _ = send_signal(pgid, libc::SIGKILL, true);
    }
    status
}

fn send_signal(pid: u32, signal: i32, group: bool) -> io::Result<()> {
    let pid = pid as libc::pid_t;
    // SAFETY: plain kill(2); no memory is touched.
    match unsafe { libc::kill(if group { -pid } else { pid }, signal) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Relays a program's output, line by line, until the pipe closes or the connection is gone.
async fn relay(id: u64, stream: Stream, pipe: impl AsyncRead + Unpin, replies: Replies) {
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line_bounded(&mut reader, &mut line, MAX_LINE).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let line = String::from_utf8_lossy(&line).into_owned();
        if !replies.output(Reply::Output { id, stream, line }).await {
            break;
        }
    }
}

/// Reads up to and including the next line end, or `max` bytes, whichever comes first.
async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<usize> {
    let mut total = 0;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        let room = max - buf.len();
        let (take, done) = match available.iter().take(room).position(|b| *b == b'\n') {
            Some(end) => (end + 1, true),
            None => {
                let take = available.len().min(room);
                (take, take == room)
            }
        };
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        total += take;
        if done {
            return Ok(total);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The Client's end
// ---------------------------------------------------------------------------------------------

const CLOSED: &str = "the second service closed the channel";

/// The Client's channel to the second service, one connection at a time. Cloning shares it. A
/// start that finds the connection gone — the service restarted — connects again, waiting up to
/// [`RECONNECT`] for the service to come back; the programs started through the old connection
/// have ended with it.
///
/// The connection closes, and the service stops whatever still runs, when the service goes or
/// when the last clone and the last program started through it are dropped.
#[derive(Clone)]
pub struct Channel {
    shared: Arc<Shared>,
}

struct Shared {
    socket: PathBuf,
    /// The uid the socket must be served by: the process account's (ADR-0059 clause 4).
    service_uid: u32,
    state: tokio::sync::Mutex<State>,
}

#[derive(Default)]
struct State {
    connection: Option<Arc<Connection>>,
    /// When the last wait for the service ended without reaching it: a start in the same outage
    /// tries once rather than waiting out [`RECONNECT`] again behind every Supervisor before it.
    gave_up: Option<Instant>,
}

/// One connection to the second service.
struct Connection {
    /// Requests, encoded: one too large for a message is refused before it is queued.
    requests: mpsc::UnboundedSender<Zeroizing<Vec<u8>>>,
    /// The task that writes them; ended once the service is gone, which closes the connection on
    /// the Client's side too.
    writer: AbortHandle,
    next_id: AtomicU64,
    routes: Mutex<Routes>,
}

#[derive(Default)]
struct Routes {
    /// Set once the service is gone: no route is added after that.
    closed: bool,
    by_id: HashMap<u64, Route>,
}

/// Where the replies about one program go, for as long as its [`RemoteChild`] lives.
struct Route {
    /// The Supervisor the program runs for, which its output is logged under.
    supervisor: String,
    started: Option<oneshot::Sender<io::Result<u32>>>,
    exited: Option<oneshot::Sender<Result<i32, String>>>,
    /// The signals sent and not yet answered, in the order they were sent.
    signals: VecDeque<oneshot::Sender<Result<(), String>>>,
}

impl Channel {
    /// The channel to the second service at `socket`, served by `service_uid`. Nothing is
    /// connected until the first start.
    #[must_use]
    pub fn new(socket: &Path, service_uid: u32) -> Channel {
        Channel {
            shared: Arc::new(Shared {
                socket: socket.to_path_buf(),
                service_uid,
                state: tokio::sync::Mutex::new(State::default()),
            }),
        }
    }

    /// [`Channel::new`], connected now, once.
    ///
    /// # Errors
    /// Returns an error when the socket cannot be reached, is not served by `service_uid`, the
    /// service refuses the connection, or it is another build.
    pub async fn connect(socket: &Path, service_uid: u32) -> io::Result<Channel> {
        let channel = Channel::new(socket, service_uid);
        let connection = Connection::open(socket, service_uid).await?;
        channel.shared.state.lock().await.connection = Some(connection);
        Ok(channel)
    }

    /// Asks the service to start `launch` for the Supervisor named `supervisor`, and waits until it
    /// runs or did not start.
    ///
    /// Cancel-safe: a start dropped before the answer asks the service to stop the program it may
    /// have started, as dropping its [`RemoteChild`] would.
    ///
    /// # Errors
    /// Returns the service's reason the program did not start, keeping its OS error, or why the
    /// service could not be reached.
    pub(crate) async fn start(&self, supervisor: &str, launch: Launch) -> io::Result<RemoteChild> {
        let connection = self.connection().await?;
        match Arc::clone(&connection)
            .start(supervisor, launch.clone())
            .await
        {
            // The service went while the start was on its way, and what it started went with it:
            // the start goes to the service that comes back.
            Err(e) if connection.is_closed() => {
                debug!(error = %e, "the second service went during a start; connecting again");
                self.connection().await?.start(supervisor, launch).await
            }
            started => started,
        }
    }

    /// The open connection, or a new one once the last is gone.
    async fn connection(&self) -> io::Result<Arc<Connection>> {
        let mut state = self.shared.state.lock().await;
        if let Some(connection) = state.connection.as_ref().filter(|c| !c.is_closed()) {
            return Ok(Arc::clone(connection));
        }
        state.connection = None;
        let waited_in_vain = state.gave_up.is_some_and(|at| at.elapsed() < RECONNECT);
        let deadline = Instant::now()
            + if waited_in_vain {
                Duration::ZERO
            } else {
                RECONNECT
            };
        loop {
            match Connection::open(&self.shared.socket, self.shared.service_uid).await {
                Ok(connection) => {
                    state.gave_up = None;
                    state.connection = Some(Arc::clone(&connection));
                    return Ok(connection);
                }
                Err(e) if Instant::now() < deadline => {
                    debug!(socket = %self.shared.socket.display(), error = %e, "the second service cannot be reached yet");
                    tokio::time::sleep(RECONNECT_EVERY).await;
                }
                Err(e) => {
                    state.gave_up = Some(Instant::now());
                    return Err(e);
                }
            }
        }
    }

    /// The connection open now, for a test to look into.
    #[cfg(test)]
    async fn open_connection(&self) -> Option<Arc<Connection>> {
        self.shared.state.lock().await.connection.clone()
    }
}

impl Connection {
    /// Connects to the second service at `socket` and exchanges the handshake.
    ///
    /// The socket's peer is checked before anything is written: whoever serves it learns every
    /// program's command line and environment, the Supervisor Endpoint's token among them, so only
    /// the process account is told (ADR-0059 clause 4).
    async fn open(socket: &Path, service_uid: u32) -> io::Result<Arc<Connection>> {
        let stream = UnixStream::connect(socket).await?;
        check_service(stream.peer_cred()?.uid(), service_uid, socket)?;
        let (mut read, mut write) = stream.into_split();
        let hello = Request::Hello {
            version: this_build().to_string(),
        };
        // A refused Client may find the connection closed before its handshake is written; the
        // refusal is read all the same, and says why.
        let sent = write_frame(&mut write, &hello).await;
        let answer = tokio::time::timeout(
            HANDSHAKE,
            read_frame_within::<_, Reply>(&mut read, MAX_HANDSHAKE),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no handshake in time"));
        let answer = match (answer, sent) {
            (Ok(Ok(Some(refused @ Reply::Refused { .. }))), _) => Some(refused),
            (_, Err(e)) => return Err(e),
            (answer, Ok(())) => answer??,
        };
        match answer {
            Some(Reply::Hello { version }) if version == this_build() => {}
            Some(Reply::Hello { version }) => {
                return Err(io::Error::other(format!(
                    "the second service is build {version}, this Client is build {}",
                    this_build()
                )))
            }
            Some(Reply::Refused { reason }) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
            }
            Some(_) => return Err(io::Error::other("the second service skipped the handshake")),
            None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, CLOSED)),
        }

        let (requests, mut outgoing) = mpsc::unbounded_channel::<Zeroizing<Vec<u8>>>();
        // Ends when every sender is gone — the last `Channel` and `RemoteChild` dropped — or when
        // the service is; dropping the write half then tells the service the Client is gone.
        let writer = tokio::spawn(async move {
            while let Some(frame) = outgoing.recv().await {
                if write.write_all(&frame).await.is_err() {
                    break;
                }
            }
        })
        .abort_handle();
        let connection = Arc::new(Connection {
            requests,
            writer,
            next_id: AtomicU64::new(1),
            routes: Mutex::new(Routes::default()),
        });
        // Holds the connection weakly, so it does not keep the senders above alive.
        let weak = Arc::downgrade(&connection);
        tokio::spawn(async move {
            while let Ok(Some(reply)) = read_frame::<_, Reply>(&mut read).await {
                let Some(connection) = weak.upgrade() else {
                    return;
                };
                connection.dispatch(reply);
            }
            if let Some(connection) = Weak::upgrade(&weak) {
                connection.fail_all();
            }
        });
        Ok(connection)
    }

    /// Whether the service is gone: its replies have ended, or a request could not be written.
    fn is_closed(&self) -> bool {
        self.routes().closed || self.requests.is_closed()
    }

    async fn start(self: Arc<Self>, supervisor: &str, launch: Launch) -> io::Result<RemoteChild> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (started_tx, started) = oneshot::channel();
        let (exited_tx, exited) = oneshot::channel();
        {
            let mut routes = self.routes();
            if routes.closed {
                return Err(closed());
            }
            routes.by_id.insert(
                id,
                Route {
                    supervisor: supervisor.to_string(),
                    started: Some(started_tx),
                    exited: Some(exited_tx),
                    signals: VecDeque::new(),
                },
            );
        }
        if let Err(e) = self.send(&Request::Start { id, launch }) {
            self.routes().by_id.remove(&id);
            return Err(e);
        }
        let pending = Pending {
            connection: &self,
            id,
        };
        let answer = started.await;
        std::mem::forget(pending);
        let pid = answer.map_err(|_| closed())??;
        Ok(RemoteChild {
            connection: self,
            id,
            pid,
            exited: Some(exited),
            outcome: None,
        })
    }

    /// Queues `request`. A request too large for a message fails here, with `InvalidInput`, and
    /// the connection carries on.
    fn send(&self, request: &Request) -> io::Result<()> {
        let frame = encode(request)?;
        self.requests.send(frame).map_err(|_| closed())
    }

    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn dispatch(&self, reply: Reply) {
        match reply {
            Reply::Started { id, pid } => {
                if let Some(started) = self
                    .routes()
                    .by_id
                    .get_mut(&id)
                    .and_then(|route| route.started.take())
                {
                    let _ = started.send(Ok(pid));
                }
            }
            Reply::StartFailed {
                id,
                error,
                os_error,
            } => {
                // Only a start still waiting is answered: under the id of a running program the
                // route is that program's, and stays.
                let started = {
                    let mut routes = self.routes();
                    let started = routes
                        .by_id
                        .get_mut(&id)
                        .and_then(|route| route.started.take());
                    if started.is_some() {
                        routes.by_id.remove(&id);
                    }
                    started
                };
                if let Some(started) = started {
                    let error = match os_error {
                        Some(code) => io::Error::from_raw_os_error(code),
                        None => io::Error::other(error),
                    };
                    let _ = started.send(Err(error));
                }
            }
            // Logged, not written to the Client's own streams: under the Supervisor it belongs to,
            // and escaped, so a program cannot pass a line of its own off as one of the Client's.
            Reply::Output { id, stream, line } => {
                let supervisor = self
                    .routes()
                    .by_id
                    .get(&id)
                    .map(|route| route.supervisor.clone());
                match supervisor {
                    Some(supervisor) => {
                        info!(supervisor = %supervisor, stream = %stream.name(), line = ?line, "output");
                    }
                    // A process the program left behind, still writing after the program's end.
                    None => {
                        info!(program = id, stream = %stream.name(), line = ?line, "output after the program ended");
                    }
                }
            }
            Reply::Signalled { id } => self.answer_signal(id, Ok(())),
            Reply::SignalFailed { id, error } => self.answer_signal(id, Err(error)),
            Reply::Exited { id, status } => {
                if let Some(exited) = self
                    .routes()
                    .by_id
                    .get_mut(&id)
                    .and_then(|route| route.exited.take())
                {
                    let _ = exited.send(status);
                }
            }
            Reply::Hello { .. } | Reply::Refused { .. } => {
                warn!("the second service repeated the handshake; ignored");
            }
        }
    }

    fn answer_signal(&self, id: u64, answer: Result<(), String>) {
        if let Some(signalled) = self
            .routes()
            .by_id
            .get_mut(&id)
            .and_then(|route| route.signals.pop_front())
        {
            let _ = signalled.send(answer);
        }
    }

    /// The service is gone: every program started through it is reported ended, and the
    /// connection is closed on this side too, so the service — should it still read — stops what
    /// it started for it.
    fn fail_all(&self) {
        let mut routes = self.routes();
        routes.closed = true;
        routes.by_id.clear();
        self.writer.abort();
    }
}

/// Whether the socket is served by the process account (ADR-0059 clause 4). Its directory belongs
/// to that account, so any process the account runs could put a socket of its own in its place;
/// only one of another account is told apart here — the Managed Processes share the account, and
/// read one another's environment already (ADR-0059, Consequences).
///
/// # Errors
/// Returns `PermissionDenied` naming both uids.
fn check_service(peer_uid: u32, service_uid: u32, socket: &Path) -> io::Result<()> {
    if peer_uid == service_uid {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} is served by uid {peer_uid}, not by the process account (uid {service_uid})",
            socket.display()
        ),
    ))
}

/// A start sent and not yet answered. Dropped — the start abandoned — it stops the program the
/// service may already run and forgets its route; an answered start forgets the guard instead.
struct Pending<'a> {
    connection: &'a Connection,
    id: u64,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.connection.routes().by_id.remove(&self.id);
        let _ = self.connection.send(&Request::Stop { id: self.id });
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, CLOSED)
}

/// A program the second service started for the Client: what [`tokio::process::Child`] is to a
/// program the Client started itself.
pub struct RemoteChild {
    /// The connection it was started on, which its replies come back on.
    connection: Arc<Connection>,
    id: u64,
    pid: u32,
    exited: Option<oneshot::Receiver<Result<i32, String>>>,
    outcome: Option<Result<ExitStatus, String>>,
}

impl RemoteChild {
    /// The program's pid, until its exit has been seen.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.outcome.is_none().then_some(self.pid)
    }

    /// Waits for the program to end. Cancel-safe; once it has ended, answers at once.
    ///
    /// # Errors
    /// Returns an error when the service could not wait for it, or the channel closed first.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.outcome.is_none() {
            let reported = match self.exited.as_mut() {
                Some(exited) => exited.await.unwrap_or_else(|_| Err(CLOSED.to_string())),
                None => Err(CLOSED.to_string()),
            };
            self.exited = None;
            self.outcome = Some(reported.map(ExitStatus::from_raw));
        }
        match &self.outcome {
            Some(Ok(status)) => Ok(*status),
            Some(Err(e)) => Err(io::Error::other(e.clone())),
            None => Err(closed()),
        }
    }

    /// Sends `signal` to the program, or with `group` to its process group where it leads one, and
    /// waits for the service's answer, up to [`SIGNAL_ANSWER`]. Once the program has ended there is
    /// nothing to signal.
    ///
    /// # Errors
    /// Returns why the service could not send the signal, that it did not answer in time, or that
    /// the channel is closed.
    pub async fn signal(&self, signal: i32, group: bool) -> io::Result<()> {
        if self.outcome.is_some() {
            return Ok(());
        }
        let (signalled, answer) = oneshot::channel();
        {
            let mut routes = self.connection.routes();
            let Some(route) = routes.by_id.get_mut(&self.id) else {
                return Err(closed());
            };
            self.connection.send(&Request::Signal {
                id: self.id,
                signal,
                group,
            })?;
            route.signals.push_back(signalled);
        }
        // A late answer still finds its place in the queue, and goes nowhere; the order holds.
        match tokio::time::timeout(SIGNAL_ANSWER, answer).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(io::Error::other(e)),
            Ok(Err(_)) => Err(closed()),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the second service did not answer the signal in time",
            )),
        }
    }

    /// Kills the program and waits for its end to be reported, up to [`KILL_ANSWER`]: the
    /// service kills at once, except while it stops its programs on shutting down, and then it
    /// ends this one itself.
    ///
    /// # Errors
    /// Returns an error when the channel closed before the exit was reported, or the report did
    /// not come in time.
    pub async fn kill(&mut self) -> io::Result<()> {
        if self.outcome.is_none() {
            self.connection.send(&Request::Stop { id: self.id })?;
        }
        match tokio::time::timeout(KILL_ANSWER, self.wait()).await {
            Ok(waited) => waited.map(|_| ()),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the second service did not report the program's end in time",
            )),
        }
    }
}

impl Drop for RemoteChild {
    /// Takes the program along, as `kill_on_drop` does for a program the Client started itself.
    fn drop(&mut self) {
        self.connection.routes().by_id.remove(&self.id);
        if self.outcome.is_none() {
            let _ = self.connection.send(&Request::Stop { id: self.id });
        }
    }
}

/// A stand-in for the second service, for the cases its real behaviour cannot be steered into: it
/// shakes hands, then answers each request with what `answer` makes of it.
#[cfg(test)]
pub(crate) mod peer {
    use super::*;

    /// A connection to the stand-in, and the directory its socket lives in.
    pub(crate) struct Peer {
        pub(crate) channel: Channel,
        _dir: tempfile::TempDir,
    }

    /// Serves one connection with `answer` and connects the Client's end to it.
    pub(crate) async fn connect(
        mut answer: impl FnMut(Request) -> Vec<Reply> + Send + 'static,
    ) -> Peer {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            while let Ok(Some(request)) = read_frame::<_, Request>(&mut stream).await {
                let replies = match request {
                    Request::Hello { .. } => vec![Reply::Hello {
                        version: this_build().to_string(),
                    }],
                    request => answer(request),
                };
                for reply in replies {
                    if write_frame(&mut stream, &reply).await.is_err() {
                        return;
                    }
                }
            }
        });
        let channel = Channel::connect(&socket, own_uid()).await.expect("connect");
        Peer { channel, _dir: dir }
    }

    /// The uid this test runs as, which serves its stand-ins and its in-process services.
    pub(crate) fn own_uid() -> u32 {
        // SAFETY: getuid(2) cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }

    /// Starts every program as a pid nobody has, kills it on a stop, and never answers a signal —
    /// as the second service does not while it stops its programs on shutting down.
    pub(crate) fn mute(request: Request) -> Vec<Reply> {
        match request {
            Request::Start { id, .. } => vec![Reply::Started { id, pid: u32::MAX }],
            Request::Stop { id } => vec![Reply::Exited {
                id,
                status: Ok(libc::SIGKILL),
            }],
            _ => Vec::new(),
        }
    }

    /// Starts every program as a pid nobody has, and answers nothing else: not a signal and not a
    /// stop.
    pub(crate) fn deaf(request: Request) -> Vec<Reply> {
        match request {
            Request::Start { id, .. } => vec![Reply::Started { id, pid: u32::MAX }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::peer::own_uid;
    use super::*;

    fn sh(script: &str) -> Launch {
        Launch {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            env: Vec::new(),
            endpoint_token: None,
            working_dir: None,
            own_process_group: false,
        }
    }

    struct Service {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<Served>,
    }

    fn service(client_uid: u32) -> Service {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let (stop, task) = listen(&socket, client_uid, || false);
        Service {
            _dir: dir,
            socket,
            stop: Some(stop),
            task,
        }
    }

    /// The service on `socket`, until the sender is used or dropped.
    fn listen(
        socket: &Path,
        client_uid: u32,
        superseded: fn() -> bool,
    ) -> (oneshot::Sender<()>, JoinHandle<Served>) {
        let listener = bind(socket).expect("bind");
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(listener, client_uid, superseded, async {
            let _ = stopped.await;
        }));
        (stop, task)
    }

    async fn connect_raw(socket: &Path, version: &str) -> UnixStream {
        let mut stream = UnixStream::connect(socket).await.expect("connect");
        write_frame(
            &mut stream,
            &Request::Hello {
                version: version.to_string(),
            },
        )
        .await
        .expect("hello");
        stream
    }

    async fn next(stream: &mut UnixStream) -> Option<Reply> {
        tokio::time::timeout(Duration::from_secs(10), read_frame(stream))
            .await
            .expect("a reply in time")
            .expect("a readable reply")
    }

    /// The admission rule itself: the Client's uid, and nobody else's — root included.
    /// Verifies: ADR-0059
    #[test]
    fn only_the_clients_uid_is_admitted() {
        assert!(admit(1000, 1000).is_ok());
        assert!(admit(1001, 1000).is_err());
        assert!(admit(0, 1000).is_err(), "not even root");
    }

    /// The service serves neither root nor its own account as the Client's.
    /// Verifies: ADR-0059
    #[test]
    fn the_client_uid_is_neither_root_nor_the_services_own() {
        assert!(check_client_uid(998, 997).is_ok());
        assert!(check_client_uid(0, 997).is_err());
        assert!(check_client_uid(997, 997).is_err());
    }

    /// The second service never runs as root, whichever account it is told to serve.
    /// Verifies: ADR-0059
    #[test]
    fn the_service_refuses_to_run_as_root() {
        let refused = check_client_uid(998, 0).expect_err("it ran as root");
        assert!(refused.contains("root"), "{refused}");
    }

    /// The Client tells a Managed Process's environment and the Supervisor Endpoint's token to the
    /// process account alone: a socket served by another uid is refused before anything is written
    /// to it. Run without a second account by naming another uid as the process account's.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn the_client_talks_only_to_the_process_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let heard = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut heard = Vec::new();
            let _ = stream.read_to_end(&mut heard).await;
            heard
        });
        let refused = Channel::connect(&socket, own_uid().wrapping_add(1))
            .await
            .err()
            .expect("refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(refused.to_string().contains("process account"), "{refused}");
        let heard = tokio::time::timeout(Duration::from_secs(5), heard)
            .await
            .expect("the Client closed the connection")
            .expect("the listener");
        assert!(heard.is_empty(), "the Client wrote {} bytes", heard.len());
        assert!(check_service(7, 7, &socket).is_ok());
    }

    /// The second service restarted — its programs ended with it — and the next start reaches the
    /// service that came back instead of failing on the channel that went.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_start_reaches_the_service_after_it_restarted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let (stop, task) = listen(&socket, own_uid(), || false);
        let channel = Channel::new(&socket, own_uid());
        let mut first = channel.start("test", sh("sleep 300")).await.expect("start");
        let _ = stop.send(());
        assert_eq!(task.await.expect("the service ended"), Served::Stopped);
        let _ = tokio::time::timeout(Duration::from_secs(15), first.wait())
            .await
            .expect("the program ended with the service");

        // The service manager brings it back a moment later.
        let again = socket.clone();
        let back = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            listen(&again, own_uid(), || false)
        });
        let mut second = channel
            .start("test", sh("exit 7"))
            .await
            .expect("the start reached the restarted service");
        let status = tokio::time::timeout(Duration::from_secs(10), second.wait())
            .await
            .expect("in time")
            .expect("a status");
        assert_eq!(status.code(), Some(7));
        drop(back.await.expect("the service came back"));
    }

    /// A kill the service does not report — it is shutting down, or gone quiet — gives up after
    /// [`KILL_ANSWER`] instead of holding up the stop that sent it for ever.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_kill_the_service_does_not_report_gives_up_in_time() {
        let peer = peer::connect(peer::deaf).await;
        let mut child = peer.channel.start("test", sh("true")).await.expect("start");
        let killed = tokio::time::timeout(KILL_ANSWER * 2, child.kill())
            .await
            .expect("the kill gives up in time");
        assert_eq!(
            killed.err().map(|e| e.kind()),
            Some(io::ErrorKind::TimedOut)
        );
    }

    /// A Client of another build ends a service that a self-update left behind, so the version
    /// `current` names takes its place; a service that is current itself only refuses it.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn another_build_ends_only_a_superseded_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let (_stop, task) = listen(&socket, own_uid(), || true);
        let mut stream = connect_raw(&socket, "0.0.0-another-build").await;
        assert!(matches!(
            next(&mut stream).await,
            Some(Reply::Refused { .. })
        ));
        let served = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the service ended")
            .expect("cleanly");
        assert_eq!(served, Served::Superseded);
    }

    /// A process that left its program's group — through `setsid` — and holds its pipe open
    /// keeps no relay and no descriptor of the service's once the connection has ended.
    /// Verifies: ADR-0059
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_output_of_a_process_left_behind_ends_with_the_connection() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, this_build()).await;
        assert!(matches!(next(&mut stream).await, Some(Reply::Hello { .. })));
        write_frame(
            &mut stream,
            &Request::Start {
                id: 1,
                launch: sh("setsid sleep 300 & echo $!"),
            },
        )
        .await
        .expect("start");
        let left = loop {
            match next(&mut stream).await.expect("open") {
                Reply::Output { line, .. } => break line.trim().parse::<u32>().expect("a pid"),
                Reply::Started { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        };
        // Whatever happens below, the process left behind does not outlive the test.
        struct Reap(u32);
        impl Drop for Reap {
            fn drop(&mut self) {
                // SAFETY: plain kill(2); no memory is touched.
                unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGKILL) };
            }
        }
        let _reap = Reap(left);
        let pipe = std::fs::read_link(format!("/proc/{left}/fd/1")).expect("its stdout");
        let held = || {
            std::fs::read_dir("/proc/self/fd").is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|to| to == pipe))
            })
        };
        // The program's exit is reported while the process it left still holds the pipe.
        loop {
            match next(&mut stream).await.expect("open") {
                Reply::Exited { .. } => break,
                Reply::Output { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(held(), "the relay is gone before the connection ended");

        drop(stream);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while held() {
            assert!(
                std::time::Instant::now() < deadline,
                "the service still holds the pipe of a process left behind"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            alive(left),
            "the process left behind is not the service's to stop"
        );
    }

    /// A peer whose uid is not the Client's is refused at the socket, before any request. Run
    /// without a second account by naming another uid as the Client's: this process is then the
    /// stranger.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_peer_of_another_uid_is_refused() {
        let service = service(own_uid().wrapping_add(1));
        let mut stream = connect_raw(&service.socket, this_build()).await;
        match next(&mut stream).await {
            Some(Reply::Refused { reason }) => {
                assert!(reason.contains("not the Client"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        let refused = Channel::connect(&service.socket, own_uid()).await;
        assert_eq!(
            refused.err().map(|e| e.kind()),
            Some(io::ErrorKind::PermissionDenied)
        );
    }

    /// Nothing is read from a refused peer and nothing waits on it: a stranger that says nothing
    /// is told and dropped at once, however many of them connect, rather than each holding a
    /// descriptor for the length of a handshake.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_refused_peer_is_dropped_at_once() {
        let service = service(own_uid().wrapping_add(1));
        let mut strangers = Vec::new();
        for _ in 0..20 {
            strangers.push(UnixStream::connect(&service.socket).await.expect("connect"));
        }
        for stranger in &mut strangers {
            let reply =
                tokio::time::timeout(Duration::from_secs(1), read_frame::<_, Reply>(stranger))
                    .await
                    .expect("refused without waiting for a handshake");
            assert!(
                matches!(reply, Ok(Some(Reply::Refused { .. }))),
                "{reply:?}"
            );
            let mut rest = Vec::new();
            let closed =
                tokio::time::timeout(Duration::from_secs(1), stranger.read_to_end(&mut rest))
                    .await
                    .expect("closed at once");
            assert!(closed.map_or(true, |n| n == 0), "more after the refusal");
        }
    }

    /// The handshake is a few bytes: a first message that claims more is refused without waiting
    /// for it.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_handshake_larger_than_a_handshake_is_refused_at_once() {
        let service = service(own_uid());
        let mut stream = UnixStream::connect(&service.socket).await.expect("connect");
        stream
            .write_all(&(64 * 1024u32).to_be_bytes())
            .await
            .expect("a length");
        let reply =
            tokio::time::timeout(Duration::from_secs(1), read_frame::<_, Reply>(&mut stream))
                .await
                .expect("refused without waiting for the body");
        assert!(
            matches!(reply, Ok(Some(Reply::Refused { .. }))),
            "{reply:?}"
        );
    }

    /// Repeated refusals are logged once in a while, with how many went unlogged since.
    #[test]
    fn refusals_are_logged_sparingly() {
        let start = std::time::Instant::now();
        let mut refusals = Refusals::default();
        assert_eq!(refusals.note(start), Some(0));
        assert_eq!(refusals.note(start + Duration::from_secs(1)), None);
        assert_eq!(refusals.note(start + Duration::from_secs(2)), None);
        assert_eq!(refusals.note(start + REFUSAL_LOG_EVERY), Some(2));
        assert_eq!(refusals.note(start + REFUSAL_LOG_EVERY), None);
    }

    /// Both ends are one build: a peer that names another is refused.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_peer_of_another_build_is_refused() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, "0.0.0-another-build").await;
        match next(&mut stream).await {
            Some(Reply::Refused { reason }) => assert!(reason.contains("build"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // And the service still serves the right build afterwards.
        Channel::connect(&service.socket, own_uid())
            .await
            .expect("the Client's own build");
    }

    /// One connection is served at a time; a second is refused while the first is open.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_second_connection_is_refused_while_one_is_served() {
        let service = service(own_uid());
        let _first = Channel::connect(&service.socket, own_uid())
            .await
            .expect("first");
        let second = Channel::connect(&service.socket, own_uid()).await;
        assert!(second.is_err(), "the second connection was served");
    }

    /// A program started through the channel runs, its output comes back line by line from both
    /// streams, and its exit status arrives last.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_started_program_reports_its_output_and_exit() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, this_build()).await;
        assert!(matches!(next(&mut stream).await, Some(Reply::Hello { .. })));
        let mut launch = sh("echo \"out $GREETING\"; echo err >&2; exit 3");
        launch.env = vec![("GREETING".to_string(), "hello".to_string())];
        write_frame(&mut stream, &Request::Start { id: 7, launch })
            .await
            .expect("start");

        let mut output = Vec::new();
        let status = loop {
            match next(&mut stream).await.expect("the connection stays open") {
                Reply::Started { id, pid } => assert!(id == 7 && pid > 0),
                Reply::Output { id, stream, line } => {
                    assert_eq!(id, 7);
                    output.push((stream, line));
                }
                Reply::Exited { id, status } => {
                    assert_eq!(id, 7);
                    break ExitStatus::from_raw(status.expect("a status"));
                }
                other => panic!("unexpected {other:?}"),
            }
        };
        output.sort_by_key(|(stream, _)| *stream == Stream::Stderr);
        assert_eq!(
            output,
            vec![
                (Stream::Stdout, "out hello".to_string()),
                (Stream::Stderr, "err".to_string())
            ]
        );
        assert_eq!(status.code(), Some(3));
    }

    /// An id names one program: a start that names a running one fails, and the running program
    /// is still the one its id stops.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_start_that_reuses_a_running_id_fails() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, this_build()).await;
        assert!(matches!(next(&mut stream).await, Some(Reply::Hello { .. })));
        for script in ["sleep 300", "exit 5"] {
            write_frame(
                &mut stream,
                &Request::Start {
                    id: 1,
                    launch: sh(script),
                },
            )
            .await
            .expect("start");
        }
        assert!(matches!(
            next(&mut stream).await,
            Some(Reply::Started { id: 1, .. })
        ));
        match next(&mut stream).await {
            Some(Reply::StartFailed { id: 1, error, .. }) => {
                assert!(error.contains("id"), "{error}")
            }
            other => panic!("expected the second start to fail, got {other:?}"),
        }
        write_frame(&mut stream, &Request::Stop { id: 1 })
            .await
            .expect("stop");
        match next(&mut stream).await {
            Some(Reply::Exited { id: 1, status }) => assert_eq!(
                ExitStatus::from_raw(status.expect("a status")).signal(),
                Some(libc::SIGKILL),
                "the stop reached another program"
            ),
            other => panic!("expected the exit, got {other:?}"),
        }
    }

    /// A program's output reaches the Client's log as events of the Supervisor it runs for, one
    /// per line and escaped, so no line a program writes reads as one of the Client's.
    /// Verifies: ADR-0059
    #[test]
    fn output_is_logged_under_its_supervisor_and_escaped() {
        // A current-thread runtime: every task of the test runs on this thread, whose log
        // `capture_logs` keeps.
        let ((), log) = crate::test_support::capture_logs(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async {
                    let service = service(own_uid());
                    let channel = Channel::connect(&service.socket, own_uid())
                        .await
                        .expect("connect");
                    let forged =
                        r#"printf 'plain\n'; printf 'ERROR fleet_agent: forged\033[0m\r\n' >&2"#;
                    let mut child = channel
                        .start("telegraf-1", sh(forged))
                        .await
                        .expect("start");
                    child.wait().await.expect("ended");
                });
        });

        let output: Vec<&str> = log.lines().filter(|l| l.contains(" output ")).collect();
        assert_eq!(output.len(), 2, "{log}");
        assert!(
            output.iter().any(|l| l.contains("supervisor=telegraf-1")
                && l.contains("stream=stdout")
                && l.contains(r#"line="plain""#)),
            "{log}"
        );
        assert!(
            output.iter().any(|l| l.contains("stream=stderr")
                && l.contains(r#"line="ERROR fleet_agent: forged\u{1b}[0m\r""#)),
            "{log}"
        );
        assert!(!log.contains('\u{1b}'), "an escape reached the log raw");
    }

    /// A signal the service could not send is an error at the Client, as it is when the Client
    /// signals a program itself, so a reload that cannot be signalled falls back to the restart.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_signal_that_cannot_be_sent_is_an_error() {
        let service = service(own_uid());
        let channel = Channel::connect(&service.socket, own_uid())
            .await
            .expect("connect");
        let mut child = channel.start("test", sh("sleep 300")).await.expect("start");
        child.signal(0, false).await.expect("signal 0 is sent");
        // No such signal: kill(2) fails with EINVAL.
        assert!(child.signal(999, false).await.is_err());
        child.kill().await.expect("kill");
        // Once the program has been waited for there is nothing to signal, and nothing fails.
        child
            .signal(libc::SIGTERM, false)
            .await
            .expect("nothing to do");
    }

    /// A signal the service does not answer — it is stopping its programs on shutting down — fails
    /// after [`SIGNAL_ANSWER`] instead of holding up the stop or the reload that sent it.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_signal_the_service_does_not_answer_fails_in_time() {
        let peer = peer::connect(peer::mute).await;
        let child = peer.channel.start("test", sh("true")).await.expect("start");
        let sent = tokio::time::timeout(SIGNAL_ANSWER * 3, child.signal(libc::SIGTERM, true))
            .await
            .expect("the signal gives up in time");
        assert!(sent.is_err(), "an unanswered signal is not taken as sent");
    }

    /// A start that failed answers only a start still waiting: one that names a running program
    /// takes nothing from it, and its exit still reaches it.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_failed_start_under_a_running_id_leaves_its_program_alone() {
        let peer = peer::connect(|request| match request {
            Request::Start { id, .. } => vec![
                Reply::Started { id, pid: u32::MAX },
                Reply::StartFailed {
                    id,
                    error: format!("id {id} names a running program"),
                    os_error: None,
                },
            ],
            request => peer::mute(request),
        })
        .await;
        let mut child = peer.channel.start("test", sh("true")).await.expect("start");
        child
            .kill()
            .await
            .expect("the exit reaches the running program");
    }

    /// A program that cannot start is reported with its OS error, so the Client still tells a
    /// missing program from any other failure.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_program_that_cannot_start_keeps_its_os_error() {
        let service = service(own_uid());
        let channel = Channel::connect(&service.socket, own_uid())
            .await
            .expect("connect");
        let mut launch = sh("");
        launch.program = PathBuf::from("/nonexistent/program");
        let error = channel
            .start("test", launch)
            .await
            .err()
            .expect("a start failure");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// A start too large for one message fails alone, with `InvalidInput`: the connection, and every
    /// program it runs, carries on.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_start_too_large_for_a_message_fails_alone() {
        let service = service(own_uid());
        let channel = Channel::connect(&service.socket, own_uid())
            .await
            .expect("connect");
        let mut running = channel.start("test", sh("sleep 300")).await.expect("start");

        let mut huge = sh("true");
        huge.env = vec![("BULK".to_string(), "x".repeat(2 << 20))];
        let error = channel.start("test", huge).await.err().expect("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let mut next = channel
            .start("test", sh("exit 4"))
            .await
            .expect("a later start still runs");
        let status = tokio::time::timeout(Duration::from_secs(10), next.wait())
            .await
            .expect("in time")
            .expect("a status");
        assert_eq!(status.code(), Some(4));
        assert!(
            running.id().is_some(),
            "the running program was reported ended"
        );
        running.kill().await.expect("kill");
    }

    /// A frame is written into a buffer of its final size, so no outgrown copy of a start's token
    /// is left unwiped.
    /// Verifies: ADR-0057
    #[test]
    fn a_frame_never_outgrows_its_buffer() {
        for size in [0, 1, 100, 4_000, 70_000] {
            let mut launch = sh("true");
            launch.env = vec![("BULK".to_string(), "x".repeat(size))];
            let frame = encode(&launch).expect("encode");
            assert_eq!(frame.capacity(), frame.len(), "size {size}");
        }
    }

    /// A start abandoned before the service answered leaves nothing behind: the program it may
    /// have started is stopped, and its route is gone.
    /// Verifies: ADR-0059
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_abandoned_start_stops_its_program() {
        let service = service(own_uid());
        let channel = Channel::connect(&service.socket, own_uid())
            .await
            .expect("connect");
        // A duration no other process on the host sleeps for, to find this one by.
        let marker = unique_marker();
        let launch = Launch {
            program: PathBuf::from("sleep"),
            args: vec![marker.clone()],
            ..sh("")
        };
        {
            let mut start = std::pin::pin!(channel.start("test", launch));
            // One poll sends the request; the answer cannot be there yet.
            std::future::poll_fn(|cx| {
                assert!(start.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        assert!(
            channel
                .open_connection()
                .await
                .expect("connected")
                .routes()
                .by_id
                .is_empty(),
            "the abandoned start kept its route"
        );
        // The service answers in order: once a later start is answered, the abandoned one has run
        // and been asked to stop.
        let mut later = channel
            .start("test", sh("exit 0"))
            .await
            .expect("a later start");
        later.wait().await.expect("the later program ended");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while running_with_arg(&marker) {
            assert!(
                std::time::Instant::now() < deadline,
                "the abandoned start's program kept running"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A Client slow to read a program's output holds up that program alone: the service still
    /// starts and stops programs while the output waits — here it stops the flood itself, with
    /// nothing read from the connection at all.
    /// Verifies: ADR-0059
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn output_the_client_has_not_read_holds_up_no_request() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, this_build()).await;
        assert!(matches!(next(&mut stream).await, Some(Reply::Hello { .. })));
        let marker = unique_marker();
        let flood = Launch {
            program: PathBuf::from("yes"),
            args: vec![marker.clone()],
            ..sh("")
        };
        write_frame(
            &mut stream,
            &Request::Start {
                id: 1,
                launch: flood,
            },
        )
        .await
        .expect("start");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !running_with_arg(&marker) {
            assert!(std::time::Instant::now() < deadline, "the flood never ran");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Long enough for the flood to fill the socket and every queue in front of it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        for request in [
            Request::Start {
                id: 2,
                launch: sh("true"),
            },
            Request::Stop { id: 1 },
        ] {
            write_frame(&mut stream, &request).await.expect("a request");
        }
        while running_with_arg(&marker) {
            assert!(
                std::time::Instant::now() < deadline,
                "the stop waited for the Client to read the output"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// An argument no other process on the host carries, to find a test's program by.
    #[cfg(target_os = "linux")]
    fn unique_marker() -> String {
        format!(
            "299.{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .subsec_nanos()
        )
    }

    /// Whether a live process has `arg` among its arguments.
    #[cfg(target_os = "linux")]
    fn running_with_arg(arg: &str) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().any(|entry| {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                return false;
            };
            std::fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| {
                cmdline
                    .split(|b| *b == 0)
                    .any(|word| word == arg.as_bytes())
            }) && alive(pid)
        })
    }

    /// Closing the Client's connection stops every program the service started, with everything
    /// in its process group — here a shell and the `sleep` it started.
    /// Verifies: ADR-0059
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn closing_the_connection_stops_the_whole_process_group() {
        let service = service(own_uid());
        let mut stream = connect_raw(&service.socket, this_build()).await;
        assert!(matches!(next(&mut stream).await, Some(Reply::Hello { .. })));
        write_frame(
            &mut stream,
            &Request::Start {
                id: 1,
                launch: sh("sleep 300 & echo $!; wait"),
            },
        )
        .await
        .expect("start");
        let (mut shell, mut worker) = (None, None);
        while shell.is_none() || worker.is_none() {
            match next(&mut stream).await.expect("open") {
                Reply::Started { pid, .. } => shell = Some(pid),
                Reply::Output { line, .. } => worker = line.trim().parse::<u32>().ok(),
                other => panic!("unexpected {other:?}"),
            }
        }
        let (shell, worker) = (shell.expect("shell"), worker.expect("worker"));
        assert!(alive(shell) && alive(worker));

        drop(stream);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while alive(shell) || alive(worker) {
            assert!(
                std::time::Instant::now() < deadline,
                "the program's group outlived the connection"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Whether `pid` is a live process: present and not a zombie, which a container's init may
    /// leave unreaped.
    #[cfg(target_os = "linux")]
    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z" && state != "X")
        })
    }

    /// The shutdown of the service is a close too: what it started does not outlive it.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn the_service_stops_its_programs_when_it_shuts_down() {
        let mut service = service(own_uid());
        let channel = Channel::connect(&service.socket, own_uid())
            .await
            .expect("connect");
        let mut child = channel.start("test", sh("sleep 300")).await.expect("start");
        let _ = service.stop.take().expect("stop").send(());
        let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .expect("the program ended in time")
            .expect("a status");
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        tokio::time::timeout(Duration::from_secs(5), &mut service.task)
            .await
            .expect("the service ended")
            .expect("cleanly");
    }

    /// A line longer than a message holds arrives in pieces, none lost.
    #[tokio::test]
    async fn a_long_line_is_split_not_lost() {
        let data = vec![b'x'; MAX_LINE * 2 + 5];
        let mut reader = tokio::io::BufReader::new(data.as_slice());
        let mut lengths = Vec::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = read_line_bounded(&mut reader, &mut buf, MAX_LINE)
                .await
                .expect("read");
            if n == 0 {
                break;
            }
            lengths.push(n);
        }
        assert_eq!(lengths, vec![MAX_LINE, MAX_LINE, 5]);
    }
}
