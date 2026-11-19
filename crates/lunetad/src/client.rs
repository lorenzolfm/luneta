use std::fmt;
use std::future::Future;
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
use tonic::Status;
use tower::{service_fn, BoxError, Service};

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

pub type Daemon = DaemonClient<Within<Channel>>;

#[derive(Clone, Debug)]
pub struct Within<S> {
    inner: S,
    answer: Duration,
}

impl<S> Within<S> {
    pub(crate) fn new(inner: S, answer: Duration) -> Within<S> {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        assert_eq!(answer.subsec_nanos(), 0, "unanswered says the bound in whole seconds");
        Within { inner, answer }
    }
}

impl<S: Service<R>, R> Service<R> for Within<S>
where
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(context).map_err(Into::into)
    }

    fn call(&mut self, request: R) -> Self::Future {
        let (answer, asked) = (self.answer, self.inner.call(request));
        Box::pin(async move {
            match tokio::time::timeout(answer, asked).await {
                Ok(answered) => answered.map_err(Into::into),
                Err(_) => Err(Status::cancelled(unanswered(answer)).into()),
            }
        })
    }
}

#[must_use]
pub fn unanswered(within: Duration) -> String {
    assert_eq!(within.subsec_nanos(), 0, "only whole seconds are said exactly");
    format!("lunetad did not answer in {}s", within.as_secs())
}

pub async fn connect(folder: &Folder) -> Result<Daemon, Error> {
    let mut socket = Some(folder.socket());
    let channel = Endpoint::from_static("http://lunetad")
        .connect_timeout(CONNECT)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.take();
            async move {
                let socket = socket.ok_or_else(went)?;
                mine(UnixStream::connect(socket).await?).map(TokioIo::new)
            }
        }))
        .await?;
    Ok(DaemonClient::new(Within::new(channel, ANSWER)))
}

pub async fn through(command: std::process::Command) -> Result<Daemon, Error> {
    let mut command = Some(command);
    let channel = Endpoint::from_static("http://lunetad")
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
    Ok(DaemonClient::new(Within::new(channel, ANSWER_THROUGH)))
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

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::time::Duration;

    use tonic::body::Body;
    use tonic::codegen::http::{Request, Response};
    use tonic::Code;
    use tower::{service_fn, BoxError};

    use super::Within;
    use crate::api::daemon_client::DaemonClient;

    #[test]
    fn a_call_past_its_answer_says_lunetad_did_not_answer_in_time() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let silent = service_fn(|_: Request<Body>| pending::<Result<Response<Body>, BoxError>>());
        let mut daemon = DaemonClient::new(Within::new(silent, Duration::from_secs(1)));
        let late = runtime.block_on(daemon.info(())).unwrap_err();
        let said = (late.code(), late.message());
        assert_eq!(said, (Code::Cancelled, "lunetad did not answer in 1s"));
    }
}
