use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use luneta_protocol::release;
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{oneshot, watch};
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream, WatchStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

use crate::api::{self, mux_server};
use crate::asked::{self, Dumped, Opens, Size, Tag, TerminalKey, ViewerHeard};
use crate::folder::{Bound, Claim, Found};
use crate::id::ClientId;
use crate::mux::{Ending, Listed, Mux};
use crate::terminal::{CLIENT_MESSAGES, ToTerminal, UNREAD_BYTES_MAX, ViewerOut};
use crate::pty;

pub const FIRST_TERMINAL: Duration = Duration::from_secs(30);

const DRAIN: Duration = Duration::from_secs(5);

const SOCKET_LOOK: Duration = Duration::from_secs(1);

const _: () = assert!(!SOCKET_LOOK.is_zero(), "a look at the socket waits between looks");

const STREAMS_MAX: u32 = 256;

const HELLO_WAIT: Duration = Duration::from_secs(5);

const MESSAGE_BYTES_MAX: usize = UNREAD_BYTES_MAX;

struct Service {
    mux: Arc<Mux>,
}

type Answer<T> = Result<Response<T>, Status>;

#[tonic::async_trait]
impl mux_server::Mux for Service {
    async fn dump(&self, request: Request<api::DumpRequest>) -> Answer<api::DumpResponse> {
        let request = request.into_inner();
        let dumped = Dumped::parse("dumped", request.dumped)?;
        let key = TerminalKey::parse("terminal", request.terminal)?;
        let handle = self.mux.terminal(key)?;
        let (answer, answered) = oneshot::channel();
        handle.send(ToTerminal::Dump(dumped, answer)).await;
        let gone = || Status::not_found(format!("terminal {} ended", key.id));
        let text = answered.await.map_err(|_| gone())??;
        Ok(Response::new(api::DumpResponse { text }))
    }

    async fn list(&self, _: Request<api::ListRequest>) -> Answer<api::Listing> {
        Ok(Response::new(self.mux.listing()))
    }

    type WatchStream = Pin<Box<dyn Stream<Item = Result<api::Listing, Status>> + Send>>;

    async fn watch(&self, _: Request<api::WatchRequest>) -> Answer<Self::WatchStream> {
        let listings = WatchStream::new(self.mux.watched()).map_while(|listed| match listed {
            Listed::Holds(listing) => Some(Ok(listing)),
            Listed::Ended(Ending::Emptied) => None,
            Listed::Ended(Ending::Unreached(why)) => Some(Err(Status::unavailable(why))),
        });
        Ok(Response::new(Box::pin(listings)))
    }

    async fn new_terminal(
        &self,
        request: Request<api::NewTerminalRequest>,
    ) -> Answer<api::NewTerminalResponse> {
        let request = request.into_inner();
        let opens = Opens::parse("opens", request.opens)?;
        let tag = Tag::parse("tag", request.tag)?;
        let size = Size::parse("size", request.size)?;
        let key = self.mux.new_terminal(&opens, tag, size)?;
        Ok(Response::new(api::NewTerminalResponse { terminal: Some(key.said()) }))
    }

    async fn close_terminal(
        &self,
        request: Request<api::CloseTerminalRequest>,
    ) -> Answer<api::CloseTerminalResponse> {
        let key = TerminalKey::parse("terminal", request.into_inner().terminal)?;
        self.mux.close_terminal(key)?;
        Ok(Response::new(api::CloseTerminalResponse {}))
    }

    async fn size_terminal(
        &self,
        request: Request<api::SizeTerminalRequest>,
    ) -> Answer<api::SizeTerminalResponse> {
        let request = request.into_inner();
        let key = TerminalKey::parse("terminal", request.terminal)?;
        self.mux.size_terminal(key, Size::parse("size", request.size)?)?;
        Ok(Response::new(api::SizeTerminalResponse {}))
    }

    async fn tag_terminal(
        &self,
        request: Request<api::TagTerminalRequest>,
    ) -> Answer<api::TagTerminalResponse> {
        let request = request.into_inner();
        let key = TerminalKey::parse("terminal", request.terminal)?;
        self.mux.tag_terminal(key, Tag::parse("tag", request.tag)?)?;
        Ok(Response::new(api::TagTerminalResponse {}))
    }

    type ShowStream = Viewed;

    async fn show(&self, request: Request<Streaming<api::FromViewer>>) -> Answer<Self::ShowStream> {
        let mut heard = request.into_inner();
        let first = tokio::time::timeout(HELLO_WAIT, heard.message()).await;
        let first = first.map_err(|_| Status::deadline_exceeded("no hello came in time"))??;
        let first = first.ok_or_else(|| Status::invalid_argument("no hello came"))?;
        let greeting = asked::greeting(first)?;
        let (out, going) = mpsc::channel(CLIENT_MESSAGES);
        let (gone, let_go) = oneshot::channel();
        let client = self.mux.view(greeting, out.clone(), gone)?;
        tokio::spawn(listen_to_viewer(Arc::clone(&self.mux), client, heard, out));
        Ok(Response::new(Viewed { going: ReceiverStream::new(going), let_go, ended: false }))
    }

    async fn info(&self, _: Request<api::InfoRequest>) -> Answer<api::InfoResponse> {
        let (version, built) = (release::VERSION.to_owned(), release::BUILT);
        Ok(Response::new(api::InfoResponse { version, built }))
    }

}

async fn listen_to_viewer(
    mux: Arc<Mux>,
    client: ClientId,
    mut heard: Streaming<api::FromViewer>,
    out: ViewerOut,
) {
    let ending = loop {
        let message = match heard.message().await {
            Ok(Some(message)) => message,
            Ok(None) => break None,
            Err(refusal) => break Some(refusal),
        };
        let said = match asked::viewer_heard(message) {
            Ok(said) => said,
            Err(refusal) => break Some(refusal),
        };
        let refusals = match said {
            ViewerHeard::Showing(keys) => mux.show(client, &keys).unwrap_or_else(|all| vec![all]),
            ViewerHeard::Typed(terminal, input) => match mux.typed(client, terminal) {
                Ok(handle) => {
                    handle.send(ToTerminal::Input(client, input)).await;
                    Vec::new()
                },
                Err(refusal) => vec![refusal],
            },
        };
        for refusal in refusals {
            let reason = refusal.message().to_owned();
            let refused = api::to_viewer::Event::Refused(api::Refused { reason });
            if out.send(Ok(api::ToViewer { event: Some(refused) })).await.is_err() {
                eprintln!("luneta-mux: viewer {client} went before it heard it was refused");
            }
        }
    };
    if let Some(refusal) = ending
        && let Err(TrySendError::Full(_)) = out.try_send(Err(refusal))
    {
        eprintln!("luneta-mux: viewer {client} had no room for its ending refusal");
    }
    mux.unview(client);
}

pub struct Viewed {
    going: ReceiverStream<Result<api::ToViewer, Status>>,
    let_go: oneshot::Receiver<()>,
    ended: bool,
}

impl Stream for Viewed {
    type Item = Result<api::ToViewer, Status>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.going).poll_next(context) {
            Poll::Ready(Some(said)) => {
                self.ended = said.is_err();
                Poll::Ready(Some(said))
            },
            Poll::Ready(None) => Poll::Ready(None),
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

async fn end_once_unreached(mux: Arc<Mux>, bound: Bound) {
    let watched = mux.watched();
    let mut unsure = false;
    loop {
        tokio::time::sleep(SOCKET_LOOK).await;
        if matches!(*watched.borrow(), Listed::Ended(_)) {
            return;
        }
        let socket = bound.socket().display();
        let why = match bound.found() {
            Ok(Found::Ours) => {
                unsure = false;
                continue;
            },
            Ok(Found::Gone) => format!("{socket} is gone"),
            Ok(Found::Replaced) => format!("{socket} is another file now"),
            Err(error) => {
                if !unsure {
                    eprintln!("luneta-mux: {socket} is not looked at, so it runs on: {error}");
                }
                unsure = true;
                continue;
            },
        };
        let why = format!("{why}, so nothing reaches luneta-mux again");
        if let Some(closed) = mux.end_unreached(why.clone()) {
            let terminals = if closed == 1 { "terminal" } else { "terminals" };
            eprintln!("luneta-mux: {why}: it closed its {closed} {terminals} and ends");
        }
        assert!(matches!(*watched.borrow(), Listed::Ended(_)), "the listing ends with the server");
        return;
    }
}

pub async fn serve(claim: Claim) -> io::Result<()> {
    if let Err(error) = pty::raise_files_limit() {
        eprintln!("luneta-mux: the limit of open files stays as it was: {error}");
    }
    let listener = UnixListener::bind(claim.socket())?;
    let bound = claim.bound()?;
    let mux = Mux::new();
    let mut watched = mux.watched();
    eprintln!("luneta-mux: listening on {}", bound.socket().display());
    let (begun, mut beginning) = watch::channel(false);
    let unused = Arc::clone(&mux);
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_TERMINAL).await;
        unused.end_if_unused();
    });
    tokio::spawn(end_once_unreached(Arc::clone(&mux), bound.clone()));
    let ended = async move {
        while matches!(*watched.borrow_and_update(), Listed::Holds(_)) {
            if watched.changed().await.is_err() {
                break;
            }
        }
        if let Err(error) = bound.unbind() {
            eprintln!("luneta-mux: {}: {error}", bound.socket().display());
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
