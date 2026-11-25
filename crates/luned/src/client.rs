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
use crate::daemon::ANSWERING_MAX;
use crate::folder::Folder;

const CONNECT: Duration = Duration::from_secs(1);

pub const ANSWER: Answer = Answer::of(Duration::from_secs(5));

pub const BRIDGED: &[u8] = b"luned\n";

pub const REACH: Duration = Duration::from_secs(10);

pub(crate) const ROUND_TRIP: RoundTrip = RoundTrip::of(Duration::from_secs(5));

const QUIET: Duration = Duration::from_secs(3);

const _: () = assert!(QUIET.as_millis() < ANSWER.waits().as_millis());

#[derive(Clone, Copy, Debug)]
pub struct Answer(Duration);

impl Answer {
    #[must_use]
    pub const fn of(answer: Duration) -> Answer {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        assert!(
            answer.as_millis() < ANSWERING_MAX.as_millis(),
            "luned works at a question for longer than any of lune's askers waits for its answer"
        );
        Answer(answer)
    }

    #[must_use]
    pub const fn waits(self) -> Duration {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RoundTrip {
    pong: Duration,
    answer: Answer,
}

impl RoundTrip {
    #[must_use]
    pub const fn of(round_trip: Duration) -> RoundTrip {
        assert!(!round_trip.is_zero(), "a keepalive waits some time for its pong");
        let answer = ANSWER.0.checked_add(round_trip);
        let answer = answer.expect("an answer through a bridge waits a finite time");
        RoundTrip { pong: round_trip, answer: Answer::of(answer) }
    }
}

const SAID_MAX: u64 = 4096;

pub type Daemon = DaemonClient<Within<Channel>>;

#[derive(Clone, Debug)]
pub struct Within<S> {
    inner: S,
    answer: Duration,
    unanswered: fn(Duration) -> String,
}

impl<S> Within<S> {
    pub(crate) fn new(inner: S, answer: Duration, unanswered: fn(Duration) -> String) -> Self {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        Within { inner, answer, unanswered }
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
        let (answer, unanswered) = (self.answer, self.unanswered);
        let asked = self.inner.call(request);
        Box::pin(async move {
            match tokio::time::timeout(answer, asked).await {
                Ok(heard) => heard.map_err(Into::into),
                Err(_) => Err(Status::deadline_exceeded(unanswered(answer)).into()),
            }
        })
    }
}

#[must_use]
pub fn unanswered(within: Duration) -> String {
    format!("luned did not answer in {}s", within.as_secs_f64())
}

pub async fn connect(folder: &Folder, answer: Answer) -> Result<Daemon, Error> {
    let mut socket = Some(folder.socket());
    let channel = Endpoint::from_static("http://luned")
        .connect_timeout(CONNECT)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.take();
            async move {
                let socket = socket.ok_or_else(went)?;
                mine(UnixStream::connect(socket).await?).map(TokioIo::new)
            }
        }))
        .await?;
    Ok(DaemonClient::new(Within::new(channel, answer.0, unanswered))
        .max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX))
}

pub async fn through(
    command: std::process::Command,
    round_trip: RoundTrip,
) -> Result<Daemon, Error> {
    let mut command = Some(command);
    let channel = Endpoint::from_static("http://luned")
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(round_trip.pong)
        .keep_alive_while_idle(true)
        .connect_with_connector(service_fn(move |_: Uri| {
            let command = command.take();
            async move {
                let command = command.ok_or_else(went)?;
                Piped::bridged(command).await.map(TokioIo::new)
            }
        }))
        .await?;
    Ok(DaemonClient::new(Within::new(channel, round_trip.answer.0, unanswered))
        .max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX))
}

fn went() -> io::Error {
    io::Error::new(ErrorKind::NotConnected, "the luned reached went")
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
            Unbridged::Ended(ended) => write!(f, "it ended before it reached luned ({ended})"),
            Unbridged::Said(said) => write!(f, "it said {said:?} before it reached luned"),
            Unbridged::Silent => {
                write!(f, "it neither reached luned nor ended in {}s", REACH.as_secs())
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
        let refusal = "another user's process listens on luned's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::future::pending;
    use std::io::ErrorKind;
    use std::process::Command;
    use std::time::Duration;

    use lune_waits::Began;
    use tonic::body::Body;
    use tonic::codegen::http::{Request, Response};
    use tonic::Code;
    use tower::{service_fn, BoxError};

    use super::{through, unanswered, RoundTrip, Unbridged, Within};
    use crate::api::daemon_client::DaemonClient;

    #[test]
    fn a_call_past_its_answer_says_luned_did_not_answer_in_time() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let silent = service_fn(|_: Request<Body>| pending::<Result<Response<Body>, BoxError>>());
        let mut daemon = DaemonClient::new(Within::new(silent, Duration::from_secs(1), unanswered));
        let late = runtime.block_on(daemon.info(())).unwrap_err();
        let said = (late.code(), late.message());
        assert_eq!(said, (Code::DeadlineExceeded, "luned did not answer in 1s"));
    }

    #[test]
    fn a_command_that_stalls_ends_its_call_on_the_keepalive_past_its_round_trip_and_runs_once() {
        const STALLS: RoundTrip = RoundTrip::of(Duration::from_secs(1));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut stalls = Command::new("sh");
        stalls.args(["-c", "echo luned; exec sleep 60"]);
        runtime.block_on(async {
            let mut stalled = through(stalls, STALLS).await.unwrap();
            let began = Began::now();
            let ended = stalled.info(()).await.expect_err("a stalled command answered");
            began.lasted().at_least(STALLS.pong, "a stalled call");
            assert_eq!(ended.code(), Code::Unavailable, "{ended:?}");
            let keepalive = sources(&ended).any(|error| error.to_string() == "keep-alive timed out");
            assert!(keepalive, "{ended:?}");
            let again = stalled.info(()).await.expect_err("it ran a command again");
            let went = sources(&again).filter_map(|error| error.downcast_ref::<Unbridged>()).any(|why| {
                matches!(why, Unbridged::Io(error) if error.kind() == ErrorKind::NotConnected)
            });
            assert!(went, "{again:?}");
        });
    }

    fn sources<'a>(
        error: &'a (dyn Error + 'static),
    ) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
        std::iter::successors(Some(error), |&error| error.source())
    }

    #[test]
    fn a_bound_in_part_of_a_second_is_said_exactly() {
        assert_eq!(unanswered(Duration::from_millis(1500)), "luned did not answer in 1.5s");
        assert_eq!(unanswered(Duration::from_millis(250)), "luned did not answer in 0.25s");
    }
}
