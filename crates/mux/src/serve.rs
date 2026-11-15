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
use crate::asked::{self, At, Dumped, Heard, Name, Opens, Place, SessionName, TurnTo, Who};
use crate::folder::Claim;
use crate::id::{ClientId, PaneId, SessionId, TabId};
use crate::mux::{LaidOut, Layouts, Mux};
use crate::pane::{CLIENT_MESSAGES, Out, ToPane, UNREAD_BYTES_MAX};
use crate::pty;

pub const FIRST_SESSION: Duration = Duration::from_secs(30);

const DRAIN: Duration = Duration::from_secs(5);

const STREAMS_MAX: u32 = 256;

const HELLO_WAIT: Duration = Duration::from_secs(5);

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

    async fn new_pane(
        &self,
        request: Request<api::NewPaneRequest>,
    ) -> Answer<api::NewPaneResponse> {
        let request = request.into_inner();
        let at = At::parse("at", request.at)?;
        let opens = Opens::parse("opens", request.opens)?;
        let pane = self.mux.new_pane(at, &opens)?;
        Ok(Response::new(api::NewPaneResponse { pane: pane.said() }))
    }

    async fn close_pane(
        &self,
        request: Request<api::ClosePaneRequest>,
    ) -> Answer<api::ClosePaneResponse> {
        self.mux.close_pane(PaneId::parse("pane", request.into_inner().pane)?)?;
        Ok(Response::new(api::ClosePaneResponse {}))
    }

    async fn turn(&self, request: Request<api::TurnRequest>) -> Answer<api::TurnResponse> {
        let request = request.into_inner();
        let pane = PaneId::parse("pane", request.pane)?;
        let to = TurnTo::parse("into", request.into)?;
        Ok(Response::new(api::TurnResponse { tab: self.mux.turn(pane, to)?.said() }))
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
        let (client, layouts) = self.mux.attach(hello, out.clone(), gone)?;
        tokio::spawn(listen(Arc::clone(&self.mux), client, heard, out));
        Ok(Response::new(UntilEnded::new(ReceiverStream::new(going), laid(layouts), let_go)))
    }
}

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

fn laid(layouts: Layouts) -> Laid {
    let laid = WatchStream::new(layouts).filter_map(|laid| {
        laid.map(|LaidOut { layout, wents }| {
            let said = Ok(api::ToClient { event: Some(api::to_client::Event::Layout(layout)) });
            (said, wents)
        })
    });
    Box::pin(laid.fuse())
}

async fn take(mux: &Arc<Mux>, client: ClientId, said: Heard, out: &Out) {
    match said {
        Heard::Input(input) => {
            if let Some(handle) = mux.input(client, input.types()) {
                handle.send(ToPane::Input(client, input)).await;
            }
        },
        Heard::Resize(size) => mux.resize(client, size),
        Heard::Go(to) => {
            if let Err(refusal) = mux.go(Who::Client(client), to) {
                let reason = refusal.message().to_owned();
                let stayed = api::to_client::Event::Stayed(api::Stayed { reason });
                if out.send(Ok(api::ToClient { event: Some(stayed) })).await.is_err() {
                    eprintln!("luneta-mux: client {client} went before it heard it stayed");
                }
            }
        },
        Heard::Act(act) => {
            if let Err(refusal) = mux.act(client, act) {
                let reason = refusal.message().to_owned();
                let refused = api::to_client::Event::Refused(api::Refused { reason });
                if out.send(Ok(api::ToClient { event: Some(refused) })).await.is_err() {
                    eprintln!("luneta-mux: client {client} went before it heard it was refused");
                }
            }
        },
    }
}

type Said = Result<api::ToClient, Status>;

type Laid = Pin<Box<dyn Stream<Item = (Said, u64)> + Send>>;

pub struct UntilEnded {
    going: ReceiverStream<Said>,
    layouts: Laid,
    let_go: oneshot::Receiver<()>,
    waiting: Option<(Said, u64)>,
    wents: u64,
    spoke: bool,
    ended: bool,
}

impl UntilEnded {
    fn new(going: ReceiverStream<Said>, layouts: Laid, let_go: oneshot::Receiver<()>) -> Self {
        UntilEnded { going, layouts, let_go, waiting: None, wents: 0, spoke: false, ended: false }
    }
}

impl Stream for UntilEnded {
    type Item = Result<api::ToClient, Status>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        if self.spoke {
            if self.waiting.is_none()
                && let Poll::Ready(Some(laid)) = self.layouts.as_mut().poll_next(context)
            {
                self.waiting = Some(laid);
            }
            if self.waiting.as_ref().is_some_and(|(_, wents)| *wents <= self.wents) {
                let (laid, _) = self.waiting.take().expect("a layout waits");
                return Poll::Ready(Some(laid));
            }
        }
        let next = Pin::new(&mut self.going).poll_next(context);
        match &next {
            Poll::Ready(Some(said)) => {
                let event = said.as_ref().ok().and_then(|said| said.event.as_ref());
                let ended = matches!(event, Some(api::to_client::Event::Ended(_)));
                self.ended = ended || said.is_err();
                self.spoke = true;
                if let Some(api::to_client::Event::Went(_)) = event {
                    self.wents += 1;
                }
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

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;

    fn said(event: api::to_client::Event) -> api::ToClient {
        api::ToClient { event: Some(event) }
    }

    fn next(until: &mut UntilEnded) -> api::to_client::Event {
        let mut context = Context::from_waker(Waker::noop());
        match Pin::new(until).poll_next(&mut context) {
            Poll::Ready(Some(Ok(api::ToClient { event: Some(event) }))) => event,
            other => panic!("the client heard {other:?}"),
        }
    }

    #[test]
    fn a_layout_goes_out_after_the_wents_said_before_it_and_ahead_of_the_frames_queued_with_it() {
        let (out, going) = mpsc::channel(8);
        let (layout, layouts) = watch::channel(None);
        let (_gone, let_go) = oneshot::channel();
        let frame = |pane| api::to_client::Event::Frame(api::Frame { pane, ..Default::default() });
        let went = api::to_client::Event::Went(api::Went { session: 1, tab: 1, pane: 1 });
        let attached = api::to_client::Event::Attached(api::Attached { client: 1 });
        for said_now in [attached, frame(1), went.clone(), frame(2), frame(3)] {
            out.try_send(Ok(said(said_now))).unwrap();
        }
        let laid_out = api::Layout { tab: 1, ..Default::default() };
        layout.send(Some(LaidOut { layout: laid_out.clone(), wents: 1 })).unwrap();
        let mut until = UntilEnded::new(ReceiverStream::new(going), laid(layouts), let_go);
        assert!(matches!(next(&mut until), api::to_client::Event::Attached(_)));
        assert_eq!(next(&mut until), frame(1));
        assert_eq!(next(&mut until), went);
        assert_eq!(next(&mut until), api::to_client::Event::Layout(laid_out));
        assert_eq!(next(&mut until), frame(2));
        assert_eq!(next(&mut until), frame(3));
    }
}
