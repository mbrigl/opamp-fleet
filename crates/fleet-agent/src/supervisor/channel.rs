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

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tracing::{info, warn};
use zeroize::Zeroizing;

use crate::supervisor::process::Launch;

/// The largest message either end accepts. A request carries one program's command line and
/// environment, a reply one line of output; neither comes near it.
const MAX_FRAME: u32 = 1 << 20;

/// The longest line of a program's output relayed as one message; a longer one arrives in pieces.
const MAX_LINE: usize = 16 * 1024;

/// How long the handshake may take before the connection is dropped.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// How long a program has to end on `SIGTERM` when the Client's connection closes, before its
/// group is killed.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// How long the exit report waits for the rest of a program's output. A process the program left
/// behind may hold its pipes open; its output is still relayed, after the exit.
const OUTPUT_DRAIN: Duration = Duration::from_secs(1);

/// What the Client asks of the second service.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Request {
    /// The first message: the Client's build.
    Hello { version: String },
    /// Start a program. `id` is the Client's name for it in every later message.
    Start { id: u64, launch: Launch },
    /// Send `signal` to the program, or, with `group`, to its process group where the program was
    /// asked to lead one ([`Launch::own_process_group`]).
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

/// The build both ends must share (ADR-0059 clause 5).
fn this_build() -> &'static str {
    fleet_core::version::current()
}

pub(crate) async fn write_frame<W, T>(writer: &mut W, message: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    // A start carries the Supervisor Endpoint's token, so every frame is wiped (ADR-0057).
    let body = Zeroizing::new(serde_json::to_vec(message).map_err(io::Error::other)?);
    let len = u32::try_from(body.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "message too large"))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
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
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
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

/// `supervisor run --agents <socket> --client-uid <uid>`: the second service (ADR-0059), until
/// `SIGTERM` or `SIGINT`. `SIGHUP` is ignored, as the Client ignores it (ADR-0019 clause 12): its
/// default disposition would end the service and leave its programs running.
///
/// # Errors
/// Returns an error when the runtime cannot be built or the socket cannot be bound.
pub fn run_service(socket: &Path, client_uid: u32) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    runtime.block_on(async {
        // Before the socket is bound: whoever sees it may already signal the service.
        crate::service::runtime::ignore_sighup();
        let listener =
            bind(socket).map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;
        info!(socket = %socket.display(), client_uid, "agents service listening");
        serve(listener, client_uid, terminated()).await;
        let _ = std::fs::remove_file(socket);
        Ok(())
    })
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
pub async fn serve(listener: UnixListener, client_uid: u32, shutdown: impl Future<Output = ()>) {
    let mut shutdown = std::pin::pin!(shutdown);
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
            () = shutdown.as_mut() => return,
        };
        let peer = stream
            .peer_cred()
            .map_err(|e| format!("cannot read the peer's credentials: {e}"))
            .and_then(|cred| admit(cred.uid(), client_uid));
        if let Err(reason) = peer {
            warn!(reason = %reason, "refused a connection");
            tokio::spawn(refuse(stream, reason));
            continue;
        }
        if serve_connection(stream, &listener, shutdown.as_mut()).await == Ended::Shutdown {
            return;
        }
    }
}

/// Tells a peer why it is not served, and closes the connection. Its handshake is read first, so
/// the refusal never races the peer's first write into a closed socket.
async fn refuse(mut stream: UnixStream, reason: String) {
    let _ = tokio::time::timeout(HANDSHAKE, async {
        let _ = read_frame::<_, Request>(&mut stream).await;
        write_frame(&mut stream, &Reply::Refused { reason }).await
    })
    .await;
}

#[derive(Debug, PartialEq, Eq)]
enum Ended {
    Closed,
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
) -> Ended {
    let (mut read, mut write) = stream.into_split();
    let hello = tokio::time::timeout(HANDSHAKE, read_frame::<_, Request>(&mut read)).await;
    let refusal = match hello {
        Ok(Ok(Some(Request::Hello { version }))) if version == this_build() => None,
        Ok(Ok(Some(Request::Hello { version }))) => Some(format!(
            "the Client is build {version}, this service is build {}",
            this_build()
        )),
        Ok(Ok(Some(_))) => Some("the first message must be the handshake".to_string()),
        Ok(Ok(None)) => return Ended::Closed,
        Ok(Err(e)) => Some(format!("unreadable handshake: {e}")),
        Err(_) => Some("no handshake in time".to_string()),
    };
    if let Some(reason) = refusal {
        warn!(reason = %reason, "refused a connection");
        let _ = write_frame(&mut write, &Reply::Refused { reason }).await;
        return Ended::Closed;
    }
    let welcome = Reply::Hello {
        version: this_build().to_string(),
    };
    if write_frame(&mut write, &welcome).await.is_err() {
        return Ended::Closed;
    }
    info!("the Client connected");

    let (reply_tx, mut reply_rx) = mpsc::channel::<Reply>(256);
    let writer = tokio::spawn(async move {
        while let Some(reply) = reply_rx.recv().await {
            if write_frame(&mut write, &reply).await.is_err() {
                break;
            }
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
    let mut running: JoinSet<u64> = JoinSet::new();
    let ended = loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Start { id, launch }) => match start(&launch) {
                    Ok(child) => {
                        let pid = child.id().unwrap_or_default();
                        info!(id, pid, program = %launch.program.display(), "program started");
                        let _ = reply_tx.send(Reply::Started { id, pid }).await;
                        let (control_tx, control_rx) = mpsc::unbounded_channel();
                        programs.insert(id, control_tx);
                        running.spawn(supervise(
                            id,
                            child,
                            launch.own_process_group,
                            control_rx,
                            reply_tx.clone(),
                        ));
                    }
                    Err(e) => {
                        warn!(id, program = %launch.program.display(), error = %e, "cannot start a program");
                        let _ = reply_tx
                            .send(Reply::StartFailed {
                                id,
                                error: e.to_string(),
                                os_error: e.raw_os_error(),
                            })
                            .await;
                    }
                },
                Some(Request::Signal { id, signal, group }) => {
                    if let Some(control) = programs.get(&id) {
                        let _ = control.send(Control::Signal { signal, group });
                    }
                }
                Some(Request::Stop { id }) => {
                    if let Some(control) = programs.get(&id) {
                        let _ = control.send(Control::Stop);
                    }
                }
                Some(Request::Hello { .. }) => warn!("a second handshake on one connection; ignored"),
                None => break Ended::Closed,
            },
            Some(done) = running.join_next() => {
                if let Ok(id) = done {
                    programs.remove(&id);
                }
            }
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(refuse(stream, "another connection is being served".to_string()));
                }
            }
            () = shutdown.as_mut() => break Ended::Shutdown,
        }
    };
    // Whatever ended the connection, nothing it started outlives it (ADR-0059 clause 4).
    for control in programs.values() {
        let _ = control.send(Control::Close);
    }
    while running.join_next().await.is_some() {}
    reader.abort();
    // The exits go out before the connection does — bounded, since output a left-behind process
    // still relays holds a sender too.
    drop(reply_tx);
    let writing = writer.abort_handle();
    if tokio::time::timeout(OUTPUT_DRAIN, writer).await.is_err() {
        writing.abort();
    }
    info!("the Client's connection ended; its programs are stopped");
    ended
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

/// Watches one program: relays its output, carries out what the connection asks, and reports its
/// exit. Returns the program's id once it has ended.
async fn supervise(
    id: u64,
    mut child: Child,
    own_process_group: bool,
    mut control: mpsc::UnboundedReceiver<Control>,
    replies: mpsc::Sender<Reply>,
) -> u64 {
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
                    if let Some(pid) = child.id() {
                        send_signal(pid, signal, group && own_process_group);
                    }
                }
                Control::Stop => {
                    if let Some(pid) = child.id() {
                        send_signal(pid, libc::SIGKILL, own_process_group);
                    }
                    let _ = child.start_kill();
                }
                Control::Close => break close(&mut child, group).await,
            },
        }
    };
    for relay in [stdout, stderr].into_iter().flatten() {
        let _ = tokio::time::timeout(OUTPUT_DRAIN, relay).await;
    }
    let status = status.map(ExitStatus::into_raw).map_err(|e| e.to_string());
    let _ = replies.send(Reply::Exited { id, status }).await;
    id
}

/// The stop at the connection's end: `SIGTERM` to the program's group, a bounded wait, then
/// `SIGKILL` to the group — sent even when the program ended in time, for anything it left behind.
async fn close(child: &mut Child, group: Option<u32>) -> io::Result<std::process::ExitStatus> {
    if let Some(pgid) = group {
        send_signal(pgid, libc::SIGTERM, true);
    }
    let status = match tokio::time::timeout(CLOSE_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    if let Some(pgid) = group {
        send_signal(pgid, libc::SIGKILL, true);
    }
    status
}

fn send_signal(pid: u32, signal: i32, group: bool) {
    let pid = pid as libc::pid_t;
    // SAFETY: plain kill(2); no memory is touched.
    unsafe {
        libc::kill(if group { -pid } else { pid }, signal);
    }
}

/// Relays a program's output, line by line, until the pipe closes or the connection is gone.
async fn relay(
    id: u64,
    stream: Stream,
    pipe: impl AsyncRead + Unpin,
    replies: mpsc::Sender<Reply>,
) {
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
        if replies
            .send(Reply::Output { id, stream, line })
            .await
            .is_err()
        {
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

const CLOSED: &str = "the agents service closed the channel";

/// The Client's connection to the second service. Cloning shares the one connection; when the last
/// clone and the last program started through it are dropped, the connection closes and the
/// service stops whatever still runs.
#[derive(Clone)]
pub struct Channel {
    inner: Arc<Inner>,
}

struct Inner {
    requests: mpsc::UnboundedSender<Request>,
    next_id: AtomicU64,
    routes: Mutex<Routes>,
}

#[derive(Default)]
struct Routes {
    /// Set once the service is gone: no route is added after that.
    closed: bool,
    by_id: HashMap<u64, Route>,
}

/// Where the replies about one program go.
struct Route {
    started: Option<oneshot::Sender<io::Result<u32>>>,
    exited: Option<oneshot::Sender<Result<i32, String>>>,
}

impl Channel {
    /// Connects to the second service at `socket` and exchanges the handshake.
    ///
    /// # Errors
    /// Returns an error when the socket cannot be reached, the service refuses the connection, or
    /// it is another build.
    pub async fn connect(socket: &Path) -> io::Result<Channel> {
        let stream = UnixStream::connect(socket).await?;
        let (mut read, mut write) = stream.into_split();
        let hello = Request::Hello {
            version: this_build().to_string(),
        };
        write_frame(&mut write, &hello).await?;
        let answer = tokio::time::timeout(HANDSHAKE, read_frame::<_, Reply>(&mut read))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no handshake in time"))??;
        match answer {
            Some(Reply::Hello { version }) if version == this_build() => {}
            Some(Reply::Hello { version }) => {
                return Err(io::Error::other(format!(
                    "the agents service is build {version}, this Client is build {}",
                    this_build()
                )))
            }
            Some(Reply::Refused { reason }) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
            }
            Some(_) => return Err(io::Error::other("the agents service skipped the handshake")),
            None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, CLOSED)),
        }

        let (requests, mut outgoing) = mpsc::unbounded_channel::<Request>();
        let inner = Arc::new(Inner {
            requests,
            next_id: AtomicU64::new(1),
            routes: Mutex::new(Routes::default()),
        });
        // Ends when every sender is gone — the last `Channel` and `RemoteChild` dropped — and
        // dropping the write half then tells the service the Client is gone.
        tokio::spawn(async move {
            while let Some(request) = outgoing.recv().await {
                if write_frame(&mut write, &request).await.is_err() {
                    break;
                }
            }
        });
        // Holds the connection weakly, so it does not keep the senders above alive.
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Ok(Some(reply)) = read_frame::<_, Reply>(&mut read).await {
                let Some(inner) = weak.upgrade() else { return };
                inner.dispatch(reply);
            }
            if let Some(inner) = Weak::upgrade(&weak) {
                inner.fail_all();
            }
        });
        Ok(Channel { inner })
    }

    /// Asks the service to start `launch`, and waits until it runs or did not start.
    ///
    /// # Errors
    /// Returns the service's reason the program did not start, keeping its OS error, or that the
    /// channel is closed.
    pub(crate) async fn start(&self, launch: Launch) -> io::Result<RemoteChild> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (started_tx, started) = oneshot::channel();
        let (exited_tx, exited) = oneshot::channel();
        {
            let mut routes = self.inner.routes();
            if routes.closed {
                return Err(closed());
            }
            routes.by_id.insert(
                id,
                Route {
                    started: Some(started_tx),
                    exited: Some(exited_tx),
                },
            );
        }
        let sent = self.inner.requests.send(Request::Start { id, launch });
        if sent.is_err() {
            self.inner.routes().by_id.remove(&id);
            return Err(closed());
        }
        let pid = started.await.map_err(|_| closed())??;
        Ok(RemoteChild {
            channel: self.clone(),
            id,
            pid,
            exited: Some(exited),
            outcome: None,
        })
    }

    fn send(&self, request: Request) -> io::Result<()> {
        self.inner.requests.send(request).map_err(|_| closed())
    }
}

impl Inner {
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
                let route = self.routes().by_id.remove(&id);
                if let Some(started) = route.and_then(|route| route.started) {
                    let error = match os_error {
                        Some(code) => io::Error::from_raw_os_error(code),
                        None => io::Error::other(error),
                    };
                    let _ = started.send(Err(error));
                }
            }
            // Where the program's output went when the Client started it itself: the Client's own
            // streams, which the service manager collects.
            Reply::Output { stream, line, .. } => {
                use std::io::Write as _;
                let _ = match stream {
                    Stream::Stdout => writeln!(std::io::stdout().lock(), "{line}"),
                    Stream::Stderr => writeln!(std::io::stderr().lock(), "{line}"),
                };
            }
            Reply::Exited { id, status } => {
                let route = self.routes().by_id.remove(&id);
                if let Some(exited) = route.and_then(|route| route.exited) {
                    let _ = exited.send(status);
                }
            }
            Reply::Hello { .. } | Reply::Refused { .. } => {
                warn!("the agents service repeated the handshake; ignored");
            }
        }
    }

    /// The service is gone: every program started through it is reported ended.
    fn fail_all(&self) {
        let mut routes = self.routes();
        routes.closed = true;
        routes.by_id.clear();
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, CLOSED)
}

/// A program the second service started for the Client: what [`tokio::process::Child`] is to a
/// program the Client started itself.
pub struct RemoteChild {
    channel: Channel,
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

    /// Sends `signal` to the program, or with `group` to its process group where it leads one.
    ///
    /// # Errors
    /// Returns an error when the channel is closed.
    pub fn signal(&self, signal: i32, group: bool) -> io::Result<()> {
        if self.outcome.is_some() {
            return Ok(());
        }
        self.channel.send(Request::Signal {
            id: self.id,
            signal,
            group,
        })
    }

    /// Kills the program and waits for it to end.
    ///
    /// # Errors
    /// Returns an error when the channel closed before the exit was reported.
    pub async fn kill(&mut self) -> io::Result<()> {
        if self.outcome.is_none() {
            self.channel.send(Request::Stop { id: self.id })?;
        }
        self.wait().await.map(|_| ())
    }
}

impl Drop for RemoteChild {
    /// Takes the program along, as `kill_on_drop` does for a program the Client started itself.
    fn drop(&mut self) {
        if self.outcome.is_none() {
            let _ = self.channel.send(Request::Stop { id: self.id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn own_uid() -> u32 {
        // SAFETY: getuid(2) cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }

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
        task: tokio::task::JoinHandle<()>,
    }

    fn service(client_uid: u32) -> Service {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agents.sock");
        let listener = bind(&socket).expect("bind");
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(listener, client_uid, async {
            let _ = stopped.await;
        }));
        Service {
            _dir: dir,
            socket,
            stop: Some(stop),
            task,
        }
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
        let refused = Channel::connect(&service.socket).await;
        assert_eq!(
            refused.err().map(|e| e.kind()),
            Some(io::ErrorKind::PermissionDenied)
        );
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
        Channel::connect(&service.socket)
            .await
            .expect("the Client's own build");
    }

    /// One connection is served at a time; a second is refused while the first is open.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_second_connection_is_refused_while_one_is_served() {
        let service = service(own_uid());
        let _first = Channel::connect(&service.socket).await.expect("first");
        let second = Channel::connect(&service.socket).await;
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

    /// A program that cannot start is reported with its OS error, so the Client still tells a
    /// missing program from any other failure.
    /// Verifies: ADR-0059
    #[tokio::test]
    async fn a_program_that_cannot_start_keeps_its_os_error() {
        let service = service(own_uid());
        let channel = Channel::connect(&service.socket).await.expect("connect");
        let mut launch = sh("");
        launch.program = PathBuf::from("/nonexistent/program");
        let error = channel.start(launch).await.err().expect("a start failure");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
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
        let channel = Channel::connect(&service.socket).await.expect("connect");
        let mut child = channel.start(sh("sleep 300")).await.expect("start");
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
