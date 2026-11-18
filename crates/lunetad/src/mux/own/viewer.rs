use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tonic::{Status, Streaming};

use super::api::{self, from_viewer, to_viewer, typed};
use super::frames::{colours, Frame};
use super::server::{connect, refused, runtime, socket};
use super::TerminalKey;
use crate::api::{view_request, On, ViewRequest};
use crate::folder::Folder;
use crate::projects::PANES_MAX;
use crate::remote::Destination;
use crate::streams::{self, Inbox, Next, Outbox};

pub const SHOWN_MAX: usize = 1024;

pub const FRAMES_MAX: usize = 64;

const SAID_MAX: usize = 1024;

const _: () = assert!(PANES_MAX <= SHOWN_MAX);

#[derive(Clone, Copy, Debug)]
pub struct Greeting {
    pub runs_in: Option<TerminalKey>,
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
}

impl Greeting {
    fn greeted(self) -> api::Greeting {
        api::Greeting {
            colours: Some(colours(self.foreground, self.background)),
            runs_in: self.runs_in.map(TerminalKey::said),
        }
    }

    fn said(self) -> api::FromViewer {
        api::FromViewer { event: Some(from_viewer::Event::Hello(self.greeted())) }
    }
}

#[derive(Debug)]
pub struct Framed {
    frame: Frame,
    _taken: OwnedSemaphorePermit,
}

impl Framed {
    #[must_use]
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
}

#[derive(Debug)]
pub enum Seen {
    Frame(Framed),
    Refused(String),
    Lost(String),
}

enum Saying {
    Showing(BTreeSet<TerminalKey>),
    Typed(TerminalKey, typed::Event),
}

impl Saying {
    fn said(self) -> api::FromViewer {
        let event = match self {
            Saying::Showing(keys) => {
                let terminals = keys.into_iter().map(TerminalKey::said).collect();
                from_viewer::Event::Showing(api::Showing { terminals })
            },
            Saying::Typed(key, event) => {
                let typed = api::Typed { terminal: Some(key.said()), event: Some(event) };
                from_viewer::Event::Typed(typed)
            },
        };
        api::FromViewer { event: Some(event) }
    }
}

pub struct Viewer {
    outbox: Outbox<Saying>,
    frames: Arc<Semaphore>,
}

impl Viewer {
    pub fn show(&self, keys: BTreeSet<TerminalKey>) {
        assert!(keys.len() <= SHOWN_MAX, "a viewer shows at most {SHOWN_MAX} terminals");
        self.outbox.say(Saying::Showing(keys));
    }

    pub fn typed(&self, key: TerminalKey, event: typed::Event) {
        self.outbox.say(Saying::Typed(key, event));
    }

    pub fn give_up(&self, why: String) {
        self.outbox.give_up(why);
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.frames.close();
    }
}

#[derive(Clone, Debug)]
pub enum Reach {
    Mux(PathBuf),
    On { lunetad: Folder, destination: Destination },
}

pub fn view<Told: FnMut(Seen) + Send + 'static>(
    reach: Reach,
    greeting: Greeting,
    told: Told,
) -> Viewer {
    let (outbox, inbox) = streams::boxes(SAID_MAX, untaken);
    let frames = Arc::new(Semaphore::new(FRAMES_MAX));
    let budget = Arc::clone(&frames);
    let viewing = move |mut told: Told| {
        let last = match runtime() {
            Ok(runtime) => runtime.block_on(stream(&reach, greeting, inbox, &budget, &mut told)),
            Err(failure) => failure.to_string(),
        };
        told(Seen::Lost(last));
    };
    if let Err((mut told, error)) = streams::on_thread("luneta-mux viewer", told, viewing) {
        told(Seen::Lost(format!("no thread for a viewer of luneta-mux: {error}")));
    }
    Viewer { outbox, frames }
}

pub async fn relayed(
    said: impl Stream<Item = api::FromViewer> + Send + 'static,
) -> Result<Streaming<api::ToViewer>, Status> {
    let socket = socket().map_err(|failure| Status::failed_precondition(failure.to_string()))?;
    let unreached =
        |error: io::Error| Status::unavailable(format!("{}: {error}", socket.display()));
    let client = connect(&socket, None).await.map_err(unreached)?;
    let mut client = client.ok_or_else(|| Status::unavailable("luneta-mux does not run"))?;
    Ok(client.show(said).await?.into_inner())
}

fn said_by_lunetad(status: &Status) -> String {
    match status.message() {
        "" => format!("lunetad: {}", status.code().description()),
        said => said.to_owned(),
    }
}

fn untaken() -> String {
    format!("luneta-mux has yet to take the last {SAID_MAX} events sent its viewer")
}

async fn hearing(
    reach: &Reach,
    greeting: Greeting,
    sent: mpsc::Receiver<api::FromViewer>,
) -> Result<Streaming<api::ToViewer>, String> {
    let sent = ReceiverStream::new(sent);
    let heard = match reach {
        Reach::Mux(socket) => {
            let mut client = match connect(socket, None).await {
                Ok(Some(client)) => client,
                Ok(None) => return Err("luneta-mux does not run".to_owned()),
                Err(error) => return Err(format!("{}: {error}", socket.display())),
            };
            let heard = client.show(tokio_stream::once(greeting.said()).chain(sent)).await;
            heard.map_err(|status| refused(&status).to_string())
        },
        Reach::On { lunetad, destination } => {
            let reached = crate::connect(lunetad).await;
            let mut daemon = reached.map_err(|error| format!("lunetad is not reached: {error}"))?;
            let on = Some(On { destination: destination.to_string() });
            let hello = view_request::Hello { on, greeting: Some(greeting.greeted()) };
            let first = ViewRequest { event: Some(view_request::Event::Hello(hello)) };
            let said = |said| ViewRequest { event: Some(view_request::Event::Said(said)) };
            let heard = daemon.view(tokio_stream::once(first).chain(sent.map(said))).await;
            heard.map_err(|status| said_by_lunetad(&status))
        },
    };
    heard.map(tonic::Response::into_inner)
}

async fn stream(
    reach: &Reach,
    greeting: Greeting,
    inbox: Inbox<Saying>,
    frames: &Arc<Semaphore>,
    told: &mut impl FnMut(Seen),
) -> String {
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let hearing = match hearing(reach, greeting, sent).await {
        Ok(hearing) => hearing,
        Err(why) => return why,
    };
    let mut next = pin!(inbox.merged(hearing));
    loop {
        let said = match next.next().await {
            Some(Next::Heard(Ok(said))) => said,
            Some(Next::Heard(Err(status))) => return refused(&status).to_string(),
            Some(Next::LetGo) | None => return "luneta-mux let go of luneta's viewer".to_owned(),
            Some(Next::GaveUp(why)) => return why,
            Some(Next::Said(saying)) => {
                if let Err(TrySendError::Full(_)) = sending.try_send(saying.said()) {
                    return untaken();
                }
                continue;
            },
        };
        match said.event {
            Some(to_viewer::Event::Frame(frame)) => match Frame::heard(frame) {
                Ok(frame) => match Arc::clone(frames).acquire_owned().await {
                    Ok(taken) => told(Seen::Frame(Framed { frame, _taken: taken })),
                    Err(_closed) => return "luneta let go of its viewer".to_owned(),
                },
                Err(why) => return format!("luneta-mux said what luneta cannot read: {why}"),
            },
            Some(to_viewer::Event::Refused(api::Refused { reason })) => told(Seen::Refused(reason)),
            None => return "luneta-mux said nothing to its viewer".to_owned(),
        }
    }
}
