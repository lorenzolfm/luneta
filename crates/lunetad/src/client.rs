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

const CONNECT: Duration = Duration::from_secs(1);

pub const ANSWER: Duration = Duration::from_secs(5);

pub const BRIDGED: &[u8] = b"lunetad\n";

pub const REACH: Duration = Duration::from_secs(10);

const ROUND_TRIP: Duration = Duration::from_secs(5);

const ANSWER_THROUGH: Duration = ANSWER.saturating_add(ROUND_TRIP);

const QUIET: Duration = Duration::from_secs(3);

const _: () = assert!(QUIET.as_millis() + ROUND_TRIP.as_millis() < ANSWER_THROUGH.as_millis());

const SAID_MAX: u64 = 4096;

pub async fn connect(folder: &Folder) -> Result<DaemonClient<Channel>, Error> {
    let mut socket = Some(folder.socket());
    let channel = Endpoint::from_static("http://lunetad")
        .connect_timeout(CONNECT)
        .timeout(ANSWER)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.take();
            async move {
                let socket = socket.ok_or_else(went)?;
                mine(UnixStream::connect(socket).await?).map(TokioIo::new)
            }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

pub async fn through(command: std::process::Command) -> Result<DaemonClient<Channel>, Error> {
    let mut command = Some(command);
    let channel = Endpoint::from_static("http://lunetad")
        .timeout(ANSWER_THROUGH)
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(ROUND_TRIP)
        .keep_alive_while_idle(true)
        .connect_with_connector(service_fn(move |_: Uri| {
            let command = command.take();
            async move {
                let command = command.ok_or_else(went)?;
                Piped::bridged(command).await.map(TokioIo::new)
            }
        }))
        .await?;
    Ok(DaemonClient::new(channel))
}

fn went() -> io::Error {
    io::Error::new(ErrorKind::NotConnected, "the lunetad reached went")
}

#[derive(Debug)]
pub enum Unbridged {
    Io(io::Error),
    Ended(ExitStatus),
    Said(String),
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

pub async fn stream(folder: &Folder) -> io::Result<UnixStream> {
    mine(UnixStream::connect(folder.socket()).await?)
}

struct Piped {
    stdout: BufReader<ChildStdout>,
    stdin: ChildStdin,
    _child: Child,
}

impl Piped {
    async fn bridged(command: std::process::Command) -> Result<Self, Unbridged> {
        let deadline = Instant::now() + REACH;
        let mut command = tokio::process::Command::from(command);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command.kill_on_drop(true).spawn()?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(io::Error::other("a command spawned piped has no pipes").into());
        };
        let mut stdout = BufReader::new(stdout);
        let mut said = Vec::new();
        let mut line = (&mut stdout).take(SAID_MAX);
        let heard = line.read_until(b'\n', &mut said);
        if let Ok(heard) = timeout_at(deadline, heard).await {
            heard?;
        }
        match said.as_slice() {
            BRIDGED => Ok(Piped { stdout, stdin, _child: child }),
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

pub async fn listening(folder: &Folder) -> io::Result<Option<u32>> {
    let stream = mine(UnixStream::connect(folder.socket()).await?)?;
    let pid = stream.peer_cred()?.pid().and_then(|pid| u32::try_from(pid).ok());
    Ok(pid.filter(|pid| *pid != 0))
}

fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on lunetad's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}
