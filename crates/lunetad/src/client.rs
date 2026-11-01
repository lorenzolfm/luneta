//! How a client reaches the daemon: on its socket, or through a command that
//! reaches it, as `ssh <host> luneta bridge` reaches the daemon of another
//! host.

use std::fmt;
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, BufReader, ReadBuf};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::{timeout_at, Instant};
use tonic::transport::{Channel, Endpoint, Error, Uri};
use tower::service_fn;

use crate::api::daemon_client::DaemonClient;
use crate::folder::Folder;

/// How long a client waits for the socket to take it. A daemon on the same
/// host takes it at once, or not at all.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a client waits for an answer: a daemon that has none in this time
/// is stuck. The daemon gives up on a question as soon, too.
pub const ANSWER: Duration = Duration::from_secs(5);

/// What `luneta bridge` says on its stdout once it has reached the daemon,
/// before it hands on a byte of the daemon's: what comes before it, or an end
/// before it, is why a client through it reached none.
pub const BRIDGED: &[u8] = b"lunetad\n";

/// How long a command has to say `BRIDGED`: ssh's handshake, and a daemon
/// that `luneta bridge` starts on the other host, which it waits for.
pub const REACH: Duration = Duration::from_secs(10);

/// The way to the daemon and back through a command, as ssh's: where a socket
/// takes none, a phone's link can take a second, or more.
const ROUND_TRIP: Duration = Duration::from_secs(5);

/// How long a client through a command waits for an answer: as long as the
/// daemon takes to give up on a question, and the way there and back. `Watch`
/// answers within `ANSWER`, and so within this, by a way that takes less than
/// `ROUND_TRIP`.
const ANSWER_THROUGH: Duration = ANSWER.saturating_add(ROUND_TRIP);

/// How long a client through a command hears nothing from the daemon before
/// it pings it, to tell a way that stalled, as ssh's does over a link that
/// dropped without a word: a ping unanswered for `ROUND_TRIP` closes the
/// connection, where a question that times out leaves it open, for the next
/// question to time out on too. Short enough that the connection closes
/// before a question on it would time out.
const QUIET: Duration = Duration::from_secs(3);

const _: () = assert!(QUIET.as_millis() + ROUND_TRIP.as_millis() < ANSWER_THROUGH.as_millis());

/// The most of what a command says in the place of `BRIDGED` that a client
/// keeps, to say so.
const SAID_MAX: u64 = 4096;

/// A client of the daemon that listens in `folder`.
pub async fn connect(folder: &Folder) -> Result<DaemonClient<Channel>, Error> {
    let socket = folder.socket();
    // The URI goes into each request's `:authority`, and nowhere else: the
    // connector is what reaches the socket, so that a path is a path and never
    // has to survive being spelled as a URI.
    let channel = Endpoint::from_static("http://lunetad")
        .connect_timeout(CONNECT)
        .timeout(ANSWER)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { mine(UnixStream::connect(socket).await?).map(TokioIo::new) }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

/// A client of the daemon that the command `command` makes reaches, over the
/// command's stdin and stdout, as `ssh <host> luneta bridge` reaches the
/// daemon of `<host>`. Each connection runs a command of its own, which is
/// killed as the connection goes: one that ends is made afresh for the next
/// question, and so is one that stalls, once the daemon has been `QUIET` on
/// it and answers no ping. A connection is made once the command says
/// `BRIDGED`, within `REACH`, and why one is not is an `Unbridged`, among the
/// error's sources. Its stderr is the caller's.
pub async fn through<F>(mut command: F) -> Result<DaemonClient<Channel>, Error>
where
    F: FnMut() -> std::process::Command + Send + 'static,
{
    // No `connect_timeout`: a command gets `REACH` from `Piped::bridged`,
    // which says why it reached none by then, where tonic's would not. And
    // pinged between questions too, so that a stalled way is not found by the
    // next question, which would wait on it.
    let channel = Endpoint::from_static("http://lunetad")
        .timeout(ANSWER_THROUGH)
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(ROUND_TRIP)
        .keep_alive_while_idle(true)
        .connect_with_connector(service_fn(move |_: Uri| {
            let command = command();
            async move { Piped::bridged(command).await.map(TokioIo::new) }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

/// Why a command reached no daemon, for a client through it.
#[derive(Debug)]
pub enum Unbridged {
    /// It did not start, or its stdout could not be read.
    Io(io::Error),
    /// It ended before it said anything: as ssh does that reaches no host
    /// (255), a shell with no luneta (127), and a luneta older than `bridge`,
    /// which takes the word for a session's name and says it has no terminal
    /// (1). Why is on the caller's stderr.
    Ended(ExitStatus),
    /// It said something else first, as a login shell that prints does.
    Said(String),
    /// It said nothing within `REACH`, and ran on, as ssh does that waits on
    /// a host that does not answer.
    Silent,
}

impl fmt::Display for Unbridged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unbridged::Io(error) => write!(f, "{error}"),
            Unbridged::Ended(ended) => write!(f, "it ended before it reached lunetad ({ended})"),
            Unbridged::Said(said) => write!(f, "it said {said:?} before it reached lunetad"),
            Unbridged::Silent => {
                write!(f, "it neither reached lunetad nor ended in {}s", REACH.as_secs())
            },
        }
    }
}

impl std::error::Error for Unbridged {}

impl From<io::Error> for Unbridged {
    fn from(error: io::Error) -> Self {
        Unbridged::Io(error)
    }
}

/// A connection to the daemon that listens in `folder`, as `connect` makes
/// one, for what comes on it to be handed on as it comes, as `luneta bridge`
/// hands it on.
pub async fn stream(folder: &Folder) -> io::Result<UnixStream> {
    mine(UnixStream::connect(folder.socket()).await?)
}

/// A command's stdout and stdin, as the two ways of one connection.
struct Piped {
    // What came after `BRIDGED` with it is the daemon's, and kept for hyper.
    stdout: BufReader<ChildStdout>,
    stdin: ChildStdin,
    // Kept for its kill on drop: the command goes with the connection.
    _child: Child,
}

impl Piped {
    /// `command`, spawned, once it has said `BRIDGED`, within `REACH`. Its
    /// stderr is the caller's, whatever `command` said of it: a pipe of it
    /// that none read would hold the command up once it was full.
    async fn bridged(command: std::process::Command) -> Result<Self, Unbridged> {
        let deadline = Instant::now() + REACH;
        let mut command = tokio::process::Command::from(command);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = command.kill_on_drop(true).spawn()?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(io::Error::other("a command spawned piped has no pipes").into());
        };
        let mut stdout = BufReader::new(stdout);
        let mut said = Vec::new();
        let mut line = (&mut stdout).take(SAID_MAX);
        let heard = line.read_until(b'\n', &mut said);
        // What it said by then is what it said, with no newline to end it
        // too, as a prompt that waits for an answer says it.
        if let Ok(heard) = timeout_at(deadline, heard).await {
            heard?;
        }
        match said.as_slice() {
            BRIDGED => Ok(Piped { stdout, stdin, _child: child }),
            // It ended its stdout, and itself, or else runs on without it.
            [] => match timeout_at(deadline, child.wait()).await {
                Ok(ended) => Err(Unbridged::Ended(ended?)),
                Err(_) => Err(Unbridged::Silent),
            },
            said => Err(Unbridged::Said(String::from_utf8_lossy(said).into_owned())),
        }
    }
}

impl AsyncRead for Piped {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(context, buffer)
    }
}

impl AsyncWrite for Piped {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(context)
    }
}

/// The pid of the process that listens in `folder`, as the kernel says: in
/// the namespace of pids that this process is in, and so none where that
/// process is in one this one cannot see. A daemon's own word for its pid is
/// of the namespace it is in.
pub async fn listening(folder: &Folder) -> io::Result<Option<u32>> {
    let stream = mine(UnixStream::connect(folder.socket()).await?)?;
    let pid = stream.peer_cred()?.pid().and_then(|pid| u32::try_from(pid).ok());
    // Linux says 0 for a process it cannot name in this namespace.
    Ok(pid.filter(|pid| *pid != 0))
}

/// `stream`, if this user's process is at its other end, as the kernel says:
/// whoever made the socket answers on it, whatever folder it turned up in,
/// and the daemon a client asks is told what it answers.
fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on lunetad's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}
