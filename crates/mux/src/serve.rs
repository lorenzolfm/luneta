//! The server: the protocol's service over the model, on the socket in
//! luneta's runtime folder, until its last session ends.

use std::fs;
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream, WatchStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

use crate::api::{self, mux_server};
use crate::asked::{self, Dumped, Name, Opens, Place, Said, Who};
use crate::folder::Claim;
use crate::mux::Mux;
use crate::pane::CLIENT_MESSAGES;

/// How long a server with no session waits for one, as it starts.
pub const FIRST_SESSION: Duration = Duration::from_secs(30);

/// How long a server that is ending waits for its clients to go.
const DRAIN: Duration = Duration::from_secs(5);

/// The most streams a connection has at once.
const STREAMS_MAX: u32 = 256;

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
        let name = Name::parse("name", request.name)?;
        let opens = Opens::parse("opens", request.opens)?;
        let (session, tab, pane) = self.mux.new_session(name, &opens)?;
        Ok(Response::new(api::NewSessionResponse { session, tab, pane }))
    }

    async fn new_tab(&self, request: Request<api::NewTabRequest>) -> Answer<api::NewTabResponse> {
        let request = request.into_inner();
        let name = request.name.map(|name| Name::parse("name", name)).transpose()?;
        let opens = Opens::parse("opens", request.opens)?;
        let (tab, pane) = self.mux.new_tab(request.session, name, &opens)?;
        Ok(Response::new(api::NewTabResponse { tab, pane }))
    }

    async fn close_tab(
        &self,
        request: Request<api::CloseTabRequest>,
    ) -> Answer<api::CloseTabResponse> {
        self.mux.close_tab(request.into_inner().tab)?;
        Ok(Response::new(api::CloseTabResponse {}))
    }

    async fn kill(&self, request: Request<api::KillRequest>) -> Answer<api::KillResponse> {
        self.mux.kill(request.into_inner().session)?;
        Ok(Response::new(api::KillResponse {}))
    }

    async fn rename(&self, request: Request<api::RenameRequest>) -> Answer<api::RenameResponse> {
        let request = request.into_inner();
        self.mux.rename(request.session, Name::parse("name", request.name)?)?;
        Ok(Response::new(api::RenameResponse {}))
    }

    async fn go(&self, request: Request<api::GoRequest>) -> Answer<api::GoResponse> {
        let request = request.into_inner();
        let who = Who::parse("who", request.who)?;
        let to = Place::parse("to", request.to)?;
        Ok(Response::new(api::GoResponse { client: self.mux.go(who, to)? }))
    }

    async fn dump(&self, request: Request<api::DumpRequest>) -> Answer<api::DumpResponse> {
        let request = request.into_inner();
        let dumped = Dumped::parse("dumped", request.dumped)?;
        let answered = self.mux.dump(request.pane, dumped)?;
        let gone = || Status::not_found(format!("pane {} ended", request.pane));
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
        let mut said = request.into_inner();
        let first =
            said.message().await?.ok_or_else(|| Status::invalid_argument("no hello came"))?;
        let hello = asked::hello(first)?;
        let (out, going) = mpsc::channel(CLIENT_MESSAGES);
        let client = self.mux.attach(hello, out.clone())?;
        let mux = Arc::clone(&self.mux);
        tokio::spawn(async move {
            let refused = loop {
                let Ok(Some(message)) = said.message().await else {
                    break None;
                };
                let taken = match asked::said(message) {
                    Ok(Said::Input(input)) => {
                        mux.input(client, input);
                        Ok(())
                    },
                    Ok(Said::Resize(size)) => {
                        mux.resize(client, size);
                        Ok(())
                    },
                    Ok(Said::Go(to)) => {
                        let _ = mux.go(Who::Client(client), to);
                        Ok(())
                    },
                    Err(refusal) => Err(refusal),
                };
                if let Err(refusal) = taken {
                    break Some(refusal);
                }
            };
            mux.detach(client);
            if let Some(refusal) = refused {
                let _ = out.send(Err(refusal)).await;
            }
        });
        Ok(Response::new(UntilEnded { going: ReceiverStream::new(going), ended: false }))
    }
}

/// What goes to a client, up to the word that its session ended, or a
/// refusal of what it said, after which the stream ends.
pub struct UntilEnded {
    going: ReceiverStream<Result<api::ToClient, Status>>,
    ended: bool,
}

impl Stream for UntilEnded {
    type Item = Result<api::ToClient, Status>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        let next = Pin::new(&mut self.going).poll_next(context);
        if let Poll::Ready(Some(said)) = &next {
            let ended =
                matches!(said, Ok(api::ToClient { event: Some(api::to_client::Event::Ended(_)) }));
            self.ended = ended || said.is_err();
        }
        next
    }
}

/// Serves on the claim's socket until the last session ends, or until it has
/// had none for `FIRST_SESSION` as it starts. It says on stderr that it
/// listens once it does.
pub async fn serve(claim: Claim) -> io::Result<()> {
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
            .add_service(mux_server::MuxServer::new(Service { mux: Arc::clone(&mux) }))
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
