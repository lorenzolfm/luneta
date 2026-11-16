use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;

use super::api::{self, from_viewer, to_viewer, typed};
use super::attach::{colours, Frame};
use super::server::{connect, refused, runtime};
use super::TerminalKey;
use crate::projects::PANES_MAX;
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
    fn said(self) -> api::FromViewer {
        let hello = api::Greeting {
            colours: Some(colours(self.foreground, self.background)),
            runs_in: self.runs_in.map(TerminalKey::said),
        };
        api::FromViewer { event: Some(from_viewer::Event::Hello(hello)) }
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

pub fn view<Told: FnMut(Seen) + Send + 'static>(
    socket: PathBuf,
    greeting: Greeting,
    told: Told,
) -> Viewer {
    let (outbox, inbox) = streams::boxes(SAID_MAX, untaken);
    let frames = Arc::new(Semaphore::new(FRAMES_MAX));
    let budget = Arc::clone(&frames);
    let viewing = move |mut told: Told| {
        let last = match runtime() {
            Ok(runtime) => runtime.block_on(stream(&socket, greeting, inbox, &budget, &mut told)),
            Err(failure) => failure.to_string(),
        };
        told(Seen::Lost(last));
    };
    if let Err((mut told, error)) = streams::on_thread("luneta-mux viewer", told, viewing) {
        told(Seen::Lost(format!("no thread for a viewer of luneta-mux: {error}")));
    }
    Viewer { outbox, frames }
}

fn untaken() -> String {
    format!("luneta-mux has yet to take the last {SAID_MAX} events sent its viewer")
}

async fn stream(
    socket: &Path,
    greeting: Greeting,
    inbox: Inbox<Saying>,
    frames: &Arc<Semaphore>,
    told: &mut impl FnMut(Seen),
) -> String {
    let mut client = match connect(socket, None).await {
        Ok(Some(client)) => client,
        Ok(None) => return "luneta-mux does not run".to_owned(),
        Err(error) => return format!("{}: {error}", socket.display()),
    };
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let said = tokio_stream::once(greeting.said()).chain(ReceiverStream::new(sent));
    let hearing = match client.show(said).await {
        Ok(hearing) => hearing.into_inner(),
        Err(status) => return refused(&status).to_string(),
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
