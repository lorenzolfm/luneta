use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::thread;

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt as _;
use tonic::Status;

use super::api::{self, from_viewer, to_viewer, typed};
use super::attach::{colours, Frame};
use super::server::{connect, refused, runtime};
use super::TerminalKey;

pub const SHOWN_MAX: usize = 1024;

const SAID_MAX: usize = 1024;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Frame(Frame),
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
                assert!(keys.len() <= SHOWN_MAX, "a viewer shows at most {SHOWN_MAX} terminals");
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
    said: mpsc::Sender<Saying>,
    giving_up: mpsc::Sender<String>,
}

impl Viewer {
    pub fn show(&self, keys: BTreeSet<TerminalKey>) {
        self.say(Saying::Showing(keys));
    }

    pub fn typed(&self, key: TerminalKey, event: typed::Event) {
        self.say(Saying::Typed(key, event));
    }

    fn say(&self, saying: Saying) {
        match self.said.try_send(saying) {
            Ok(()) | Err(TrySendError::Closed(_)) => {},
            Err(TrySendError::Full(_)) => self.give_up(untaken()),
        }
    }

    pub fn give_up(&self, why: String) {
        let _ = self.giving_up.try_send(why);
    }
}

pub fn view<Told: FnMut(Seen) + Send + 'static>(
    socket: PathBuf,
    greeting: Greeting,
    mut told: Told,
) -> Viewer {
    let (said, saying) = mpsc::channel(SAID_MAX);
    let (giving_up, given_up) = mpsc::channel(1);
    let (handing, handed) = std::sync::mpsc::sync_channel::<Told>(1);
    let viewing = move || {
        let mut told = handed.recv().expect("`view` hands its thread `told` as it starts");
        let last = match runtime() {
            Ok(runtime) => runtime.block_on(stream(&socket, greeting, saying, given_up, &mut told)),
            Err(failure) => failure.to_string(),
        };
        told(Seen::Lost(last));
    };
    match thread::Builder::new().name("luneta-mux viewer".to_owned()).spawn(viewing) {
        Ok(_) => handing.send(told).expect("the thread waits on `told` before all else"),
        Err(error) => told(Seen::Lost(format!("no thread for a viewer of luneta-mux: {error}"))),
    }
    Viewer { said, giving_up }
}

fn untaken() -> String {
    format!("luneta-mux has yet to take the last {SAID_MAX} events sent its viewer")
}

enum Next {
    Said(Result<api::ToViewer, Status>),
    LetGo,
    Saying(Saying),
    GaveUp(String),
}

async fn stream(
    socket: &Path,
    greeting: Greeting,
    saying: mpsc::Receiver<Saying>,
    given_up: mpsc::Receiver<String>,
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
    let hearing = hearing.map(Next::Said).chain(tokio_stream::once(Next::LetGo));
    let saying = ReceiverStream::new(saying).map(Next::Saying);
    let given_up = ReceiverStream::new(given_up).map(Next::GaveUp);
    let mut next = pin!(hearing.merge(saying).merge(given_up));
    loop {
        let said = match next.next().await {
            Some(Next::Said(Ok(said))) => said,
            Some(Next::Said(Err(status))) => return refused(&status).to_string(),
            Some(Next::LetGo) | None => return "luneta-mux let go of luneta's viewer".to_owned(),
            Some(Next::GaveUp(why)) => return why,
            Some(Next::Saying(saying)) => {
                if let Err(TrySendError::Full(_)) = sending.try_send(saying.said()) {
                    return untaken();
                }
                continue;
            },
        };
        match said.event {
            Some(to_viewer::Event::Frame(frame)) => match Frame::heard(frame) {
                Ok(frame) => told(Seen::Frame(frame)),
                Err(why) => return format!("luneta-mux said what luneta cannot read: {why}"),
            },
            Some(to_viewer::Event::Refused(api::Refused { reason })) => told(Seen::Refused(reason)),
            None => return "luneta-mux said nothing to its viewer".to_owned(),
        }
    }
}
