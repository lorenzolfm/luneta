//! How a client reaches the daemon: on its socket, or through a command that
//! reaches it, as `ssh <host> luneta bridge` reaches the daemon of another
//! host.

use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, ChildStdout};
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
/// question. Its stderr is the caller's.
pub async fn through<F>(mut command: F) -> Result<DaemonClient<Channel>, Error>
where
    F: FnMut() -> std::process::Command + Send + 'static,
{
    // A command that starts has connected: there is no connecting to wait
    // for, and the first answer waits for what the command reaches.
    let channel = Endpoint::from_static("http://lunetad")
        .timeout(ANSWER)
        .connect_with_connector(service_fn(move |_: Uri| {
            let command = command();
            async move { Piped::spawn(command).map(TokioIo::new) }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

/// A connection to the daemon that listens in `folder`, as `connect` makes
/// one, for what comes on it to be handed on as it comes, as `luneta bridge`
/// hands it on.
pub async fn stream(folder: &Folder) -> io::Result<UnixStream> {
    mine(UnixStream::connect(folder.socket()).await?)
}

/// A command's stdout and stdin, as the two ways of one connection.
struct Piped {
    stdout: ChildStdout,
    stdin: ChildStdin,
    // Kept for its kill on drop: the command goes with the connection.
    _child: Child,
}

impl Piped {
    fn spawn(command: std::process::Command) -> io::Result<Self> {
        let mut command = tokio::process::Command::from(command);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn()?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(io::Error::other("a command spawned piped has no pipes"));
        };
        Ok(Piped { stdout, stdin, _child: child })
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
