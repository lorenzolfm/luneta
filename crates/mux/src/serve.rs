//! The server: the protocol's service over the model, on the socket in
//! luneta's runtime folder, until its last session ends.

use std::fs;
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{oneshot, watch};
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream, WatchStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

use crate::api::{self, mux_server};
use crate::asked::{self, Dumped, Heard, Name, Opens, Place, SessionName, Who};
use crate::folder::Claim;
use crate::id::{ClientId, PaneId, SessionId, TabId};
use crate::mux::Mux;
use crate::pane::{CLIENT_MESSAGES, Out, ToPane, UNREAD_BYTES_MAX};
use crate::pty;

/// How long a server with no session waits for one, as it starts.
pub const FIRST_SESSION: Duration = Duration::from_secs(30);

/// How long a server that is ending waits for its clients to go.
const DRAIN: Duration = Duration::from_secs(5);

/// The most streams a connection has at once.
const STREAMS_MAX: u32 = 256;

/// How long a client that attaches has to say its `Hello`.
const HELLO_WAIT: Duration = Duration::from_secs(5);

/// The most bytes a message to the server has: as many as a pane holds for
/// its program to read.
const MESSAGE_BYTES_MAX: usize = UNREAD_BYTES_MAX;

struct Service {
    mux: Arc<Mux>,
}

type Answer<T> = Result<Response<T>, Status>;

#[tonic::async_trait]
impl mux_server::Mux for Service {
    async fn new_session(
        &self,
        request: Request<api::NewSessionRequest>,
    ) -> Answer<api::NewSessionResponse> {
        let request = request.into_inner();
        let name = SessionName::parse("name", request.name)?;
        let opens = Opens::parse("opens", request.opens)?;
        let (session, made, tab, pane) = self.mux.new_session(name, &opens)?;
        Ok(Response::new(api::NewSessionResponse {
            session: session.said(),
            tab: tab.said(),
            pane: pane.said(),
            made_at: Some(made.into()),
        }))
    }

    async fn new_tab(&self, request: Request<api::NewTabRequest>) -> Answer<api::NewTabResponse> {
        let request = request.into_inner();
        let session = SessionId::parse("session", request.session)?;
        let name = request.name.map(|name| Name::parse("name", name)).transpose()?;
        let opens = Opens::parse("opens", request.opens)?;
        let (tab, pane) = self.mux.new_tab(session, name, &opens)?;
        Ok(Response::new(api::NewTabResponse { tab: tab.said(), pane: pane.said() }))
    }

    async fn close_tab(
        &self,
        request: Request<api::CloseTabRequest>,
    ) -> Answer<api::CloseTabResponse> {
        self.mux.close_tab(TabId::parse("tab", request.into_inner().tab)?)?;
        Ok(Response::new(api::CloseTabResponse {}))
    }

    async fn kill(&self, request: Request<api::KillRequest>) -> Answer<api::KillResponse> {
        self.mux.kill(SessionId::parse("session", request.into_inner().session)?)?;
        Ok(Response::new(api::KillResponse {}))
    }

    async fn rename(&self, request: Request<api::RenameRequest>) -> Answer<api::RenameResponse> {
        let request = request.into_inner();
        let session = SessionId::parse("session", request.session)?;
        self.mux.rename(session, SessionName::parse("name", request.name)?)?;
        Ok(Response::new(api::RenameResponse {}))
    }

    async fn go(&self, request: Request<api::GoRequest>) -> Answer<api::GoResponse> {
        let request = request.into_inner();
        let who = Who::parse("who", request.who)?;
        let to = Place::parse("to", request.to)?;
        Ok(Response::new(api::GoResponse { client: self.mux.go(who, to)?.said() }))
    }

    async fn dump(&self, request: Request<api::DumpRequest>) -> Answer<api::DumpResponse> {
        let request = request.into_inner();
        let pane = PaneId::parse("pane", request.pane)?;
        let dumped = Dumped::parse("dumped", request.dumped)?;
        let handle = self.mux.pane(pane)?;
        let (answer, answered) = oneshot::channel();
        handle.send(ToPane::Dump(dumped, answer)).await;
        let gone = || Status::not_found(format!("pane {pane} ended"));
        let text = answered.await.map_err(|_| gone())??;
        Ok(Response::new(api::DumpResponse { text }))
    }

    async fn list(&self, _: Request<api::ListRequest>) -> Answer<api::Listing> {
        Ok(Response::new(self.mux.listing()))
    }

    type WatchStream = Pin<Box<dyn Stream<Item = Result<api::Listing, Status>> + Send>>;

    async fn watch(&self, _: Request<api::WatchRequest>) -> Answer<Self::WatchStream> {
        let listings = WatchStream::new(self.mux.watched()).map_while(|listing| listing.map(Ok));
        Ok(Response::new(Box::pin(listings)))
    }

    type AttachStream = UntilEnded;

    async fn attach(
        &self,
        request: Request<Streaming<api::ToServer>>,
    ) -> Answer<Self::AttachStream> {
        let mut heard = request.into_inner();
        let first = tokio::time::timeout(HELLO_WAIT, heard.message()).await;
        let first = first.map_err(|_| Status::deadline_exceeded("no hello came in time"))??;
        let hello = asked::hello(first.ok_or_else(|| Status::invalid_argument("no hello came"))?)?;
        let (out, going) = mpsc::channel(CLIENT_MESSAGES);
        let (gone, let_go) = oneshot::channel();
        let client = self.mux.attach(hello, out.clone(), gone)?;
        tokio::spawn(listen(Arc::clone(&self.mux), client, heard, out));
        let going = ReceiverStream::new(going);
        Ok(Response::new(UntilEnded { going, let_go, ended: false }))
    }
}

/// Takes what client `client` says after its `Hello`, until it goes or says
/// what is refused, and then lets go of it.
async fn listen(mux: Arc<Mux>, client: ClientId, mut heard: Streaming<api::ToServer>, out: Out) {
    let refused = loop {
        let message = match heard.message().await {
            Ok(Some(message)) => message,
            Ok(None) => break None,
            Err(refusal) => break Some(refusal),
        };
        match asked::heard(message) {
            Ok(said) => take(&mux, client, said, &out).await,
            Err(refusal) => break Some(refusal),
        }
    };
    if let Some(refusal) = refused
        && let Err(TrySendError::Full(_)) = out.try_send(Err(refusal))
    {
        eprintln!("luneta-mux: client {client} had no room for its refusal");
    }
    mux.detach(client);
}

/// Has `client` do what it said: its input and its size go to the pane it is
/// on, as soon as the pane has room for them, and a `Go` that is refused is
/// told to it.
async fn take(mux: &Mux, client: ClientId, said: Heard, out: &Out) {
    match said {
        Heard::Input(input) => {
            if let Some(handle) = mux.input(client, input.types()) {
                handle.send(ToPane::Input(client, input)).await;
            }
        },
        Heard::Resize(size) => {
            if let Some(handle) = mux.resize(client, size) {
                handle.send(ToPane::Resize(client, size)).await;
            }
        },
        Heard::Go(to) => {
            if let Err(refusal) = mux.go(Who::Client(client), to) {
                let reason = refusal.message().to_owned();
                let stayed = api::to_client::Event::Stayed(api::Stayed { reason });
                if out.send(Ok(api::ToClient { event: Some(stayed) })).await.is_err() {
                    eprintln!("luneta-mux: client {client} went before it heard it stayed");
                }
            }
        },
    }
}

/// What goes to a client, up to the word that its session ended, or a
/// refusal of what it said, after which the stream ends. It ends too once the
/// client is let go of, after what was sent to it before.
pub struct UntilEnded {
    going: ReceiverStream<Result<api::ToClient, Status>>,
    let_go: oneshot::Receiver<()>,
    ended: bool,
}

impl Stream for UntilEnded {
    type Item = Result<api::ToClient, Status>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        let next = Pin::new(&mut self.going).poll_next(context);
        match &next {
            Poll::Ready(Some(said)) => {
                let ended = matches!(
                    said,
                    Ok(api::ToClient { event: Some(api::to_client::Event::Ended(_)) })
                );
                self.ended = ended || said.is_err();
                next
            },
            Poll::Ready(None) => next,
            Poll::Pending => {
                if Pin::new(&mut self.let_go).poll(context).is_ready() {
                    self.ended = true;
                    return Poll::Ready(None);
                }
                Poll::Pending
            },
        }
    }
}

/// Serves on the claim's socket until the last session ends, or until it has
/// had none for `FIRST_SESSION` as it starts. It says on stderr that it
/// listens once it does.
pub async fn serve(claim: Claim) -> io::Result<()> {
    if let Err(error) = pty::raise_files_limit() {
        eprintln!("luneta-mux: the limit of open files stays as it was: {error}");
    }
    let listener = UnixListener::bind(claim.socket())?;
    let mux = Mux::new();
    let mut watched = mux.watched();
    let socket = claim.socket().to_owned();
    eprintln!("luneta-mux: listening on {}", socket.display());
    let (begun, mut beginning) = watch::channel(false);
    let unused = Arc::clone(&mux);
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_SESSION).await;
        unused.end_if_unused();
    });
    let ended = async move {
        while watched.borrow_and_update().is_some() {
            if watched.changed().await.is_err() {
                break;
            }
        }
        match fs::remove_file(&socket) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => eprintln!("luneta-mux: {}: {error}", socket.display()),
        }
        let _ = begun.send(true);
    };
    let served = tokio::spawn(
        Server::builder()
            .max_concurrent_streams(STREAMS_MAX)
            .add_service(
                mux_server::MuxServer::new(Service { mux: Arc::clone(&mux) })
                    .max_decoding_message_size(MESSAGE_BYTES_MAX),
            )
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), ended),
    );
    let _ = beginning.wait_for(|begun| *begun).await;
    drop(claim);
    match tokio::time::timeout(DRAIN, served).await {
        Ok(Ok(served)) => served.map_err(io::Error::other),
        Ok(Err(error)) => Err(io::Error::other(error)),
        Err(_) => {
            eprintln!("luneta-mux: clients still connected after {}s are left", DRAIN.as_secs());
            Ok(())
        },
    }
}
