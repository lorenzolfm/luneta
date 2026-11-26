use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::pin::{pin, Pin};
use std::sync::Arc;
use std::task::{Context, Poll};

use lune_domain::mux::TerminalKey;
use lune_domain::projects::LAYOUT_VIEWS_MAX;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tonic::{Status, Streaming};

use super::api::{self, from_viewer, to_viewer, typed};
use super::frames::{colours, Frame};
use super::server::{reached_at, refused, socket, unreached_at, Reached, Room, Roomed};
use crate::api::{frames_request, On, FramesRequest};
use crate::folder::Folder;
use crate::remote::Destination;
use crate::streams::{self, Dropped, Inbox, Next, Outbox};
use crate::tasks;

pub const SHOWN_MAX: usize = 1024;

pub const FRAMES_MAX: usize = 64;

pub const SAID_MAX: usize = 1024;

const _: () = assert!(LAYOUT_VIEWS_MAX <= SHOWN_MAX);

#[derive(Clone, Copy, Debug)]
pub struct Greeting {
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
}

impl Greeting {
    fn greeted(self, runs_in: Option<TerminalKey>) -> api::Greeting {
        api::Greeting {
            colours: Some(colours(self.foreground, self.background)),
            runs_in: runs_in.map(TerminalKey::said),
        }
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

    #[must_use]
    pub fn into_frame(self) -> Frame {
        self.frame
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
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.frames.close();
    }
}

#[derive(Clone, Debug)]
pub enum Reach {
    Mux { socket: PathBuf, runs_in: Option<TerminalKey> },
    On { luned: Folder, destination: Destination },
}

pub fn start<Told: FnMut(Seen) + Send + 'static>(
    reach: Reach,
    greeting: Greeting,
    mut told: Told,
) -> Viewer {
    let (outbox, inbox) = streams::boxes(SAID_MAX);
    let frames = Arc::new(Semaphore::new(FRAMES_MAX));
    let budget = Arc::clone(&frames);
    drop(tasks::shared().spawn(async move {
        if let Ok(last) = stream(&reach, greeting, inbox, &budget, &mut told).await {
            told(Seen::Lost(last));
        }
    }));
    Viewer { outbox, frames }
}

pub async fn relayed(
    said: impl Stream<Item = api::FromViewer> + Send + 'static,
) -> Result<impl Stream<Item = Result<api::ToViewer, Status>> + Send + 'static, Status> {
    let room = Room::relayed()?;
    let socket = socket().map_err(|failure| Status::failed_precondition(failure.to_string()))?;
    let unreached =
        |error: io::Error| Status::unavailable(unreached_at(&socket, &error));
    let streams = match reached_at(&socket).await.map_err(unreached)? {
        Reached::Matched(matched) => matched.streams(room),
        Reached::Unreached => return Err(Status::unavailable("lune-mux does not run")),
        Reached::Unmatched(unmatched) => return Err(Status::unavailable(unmatched.to_string())),
    };
    let heard = streams.show(said).await.map_err(|status| relayed_from_mux(&status))?;
    Ok(heard.map(|heard| heard.map_err(|status| relayed_from_mux(&status))))
}

fn relayed_from_mux(status: &Status) -> Status {
    Status::new(status.code(), refused(status).to_string())
}

fn said_by_luned(status: &Status) -> String {
    match status.message() {
        "" => format!("luned: {}", status.code().description()),
        said => said.to_owned(),
    }
}

fn heard_from(reach: &Reach, status: &Status) -> String {
    match reach {
        Reach::Mux { .. } => refused(status).to_string(),
        Reach::On { .. } => said_by_luned(status),
    }
}

fn untaken() -> String {
    format!("lune-mux has yet to take the last {SAID_MAX} events sent its viewer")
}

enum Hearing {
    Mux(Roomed<Streaming<api::ToViewer>>),
    On(crate::Held<Streaming<api::ToViewer>>),
}

impl Stream for Hearing {
    type Item = Result<api::ToViewer, Status>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            Hearing::Mux(heard) => Pin::new(heard).poll_next(context),
            Hearing::On(heard) => Pin::new(heard).poll_next(context),
        }
    }
}

async fn hearing(
    reach: &Reach,
    greeting: Greeting,
    sent: mpsc::Receiver<api::FromViewer>,
) -> Result<Hearing, String> {
    let sent = ReceiverStream::new(sent);
    match reach {
        Reach::Mux { socket, runs_in } => {
            let room = Room::relayed().map_err(|status| refused(&status).to_string())?;
            let streams = match reached_at(socket).await {
                Ok(Reached::Matched(matched)) => matched.streams(room),
                Ok(Reached::Unreached) => return Err("lune-mux does not run".to_owned()),
                Ok(Reached::Unmatched(unmatched)) => return Err(unmatched.to_string()),
                Err(error) => return Err(unreached_at(socket, &error)),
            };
            let hello = from_viewer::Event::Hello(greeting.greeted(*runs_in));
            let first = api::FromViewer { event: Some(hello) };
            let heard = streams.show(tokio_stream::once(first).chain(sent)).await;
            heard.map(Hearing::Mux).map_err(|status| refused(&status).to_string())
        },
        Reach::On { luned, destination } => {
            let daemon = match crate::reach(luned).await {
                Ok(crate::Reached::Matched(daemon)) => daemon,
                Ok(crate::Reached::Unmatched(unmatched)) => return Err(unmatched.to_string()),
                Err(error) => return Err(format!("luned is not reached: {error}")),
            };
            let on = Some(On { destination: destination.to_string() });
            let hello = frames_request::Hello { on, greeting: Some(greeting.greeted(None)) };
            let first = FramesRequest { event: Some(frames_request::Event::Hello(hello)) };
            let said = |said| FramesRequest { event: Some(frames_request::Event::Said(said)) };
            let words = tokio_stream::once(first).chain(sent.map(said));
            let heard = daemon.frames(crate::ANSWER, words).await.map_err(|unasked| match unasked {
                crate::Unasked::Unsent(unsent) => unsent.to_string(),
                crate::Unasked::Asked(status) => said_by_luned(&status),
            })?;
            Ok(Hearing::On(heard))
        },
    }
}

async fn stream(
    reach: &Reach,
    greeting: Greeting,
    inbox: Inbox<Saying>,
    frames: &Arc<Semaphore>,
    told: &mut impl FnMut(Seen),
) -> Result<String, Dropped> {
    let (sending, sent) = mpsc::channel(SAID_MAX);
    let hearing = match hearing(reach, greeting, sent).await {
        Ok(hearing) => hearing,
        Err(why) => return Ok(why),
    };
    let mut next = pin!(inbox.merged(hearing));
    loop {
        let said = match next.next().await {
            Some(Next::Heard(Ok(said))) => said,
            Some(Next::Heard(Err(status))) => return Ok(heard_from(reach, &status)),
            Some(Next::LetGo) | None => return Ok("lune-mux let go of it".to_owned()),
            Some(Next::Untaken) => return Ok(untaken()),
            Some(Next::Dropped) => return Err(Dropped),
            Some(Next::Said(saying)) => {
                if let Err(TrySendError::Full(_)) = sending.try_send(saying.said()) {
                    return Ok(untaken());
                }
                continue;
            },
        };
        match said.event {
            Some(to_viewer::Event::Frame(frame)) => match Frame::heard(frame) {
                Ok(frame) => match Arc::clone(frames).acquire_owned().await {
                    Ok(taken) => told(Seen::Frame(Framed { frame, _taken: taken })),
                    Err(_closed) => return Err(Dropped),
                },
                Err(why) => return Ok(format!("lune-mux said what lune cannot read: {why}")),
            },
            Some(to_viewer::Event::Refused(api::Refused { reason })) => told(Seen::Refused(reason)),
            Some(to_viewer::Event::Closed(_)) => {},
            None => return Ok("lune-mux said nothing to its viewer".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::relayed;
    use crate::mux::own::server::{RELAYED, RELAYED_MAX};

    #[test]
    fn luned_refuses_to_relay_a_viewer_past_the_most_it_relays_at_once() {
        let most = u32::try_from(RELAYED_MAX).unwrap();
        let held = RELAYED.try_acquire_many(most).unwrap();
        let tasks = crate::tasks::shared();
        let Err(refused) = tasks.block_on(relayed(tokio_stream::empty())) else {
            panic!("a viewer past the most is relayed");
        };
        assert_eq!(refused.code(), Code::ResourceExhausted, "{refused:?}");
        let said = format!("at most {RELAYED_MAX} viewers");
        assert!(refused.message().contains(&said), "{}", refused.message());
        drop(held);
    }
}
