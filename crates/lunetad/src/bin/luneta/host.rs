//! luneta's own terminal: the sidebar on the left, zellij on the right, and
//! which of the two has the keys. `Ctrl a` hands them from one to the other,
//! and a click hands them to the one clicked. `Ctrl q` collapses the sidebar
//! to what it counts, zellij taking the rest of its columns, and opens it
//! again, as a click on it does, and `Alt Tab` takes zellij to the next pane
//! the sidebar lists, whichever has the keys. The sidebar's edge, its last
//! column, drags it wider or narrower, zellij taking the columns it leaves.
//! What the mouse drags on the sidebar's rows stays with the sidebar until it
//! lets go, wherever the mouse goes, as what it drags on zellij stays with
//! zellij. `Alt a` opens the menu in a box over zellij, for the pane zellij
//! is on, and a brief or the ask box the menu asks for shows in a box there
//! too, as does what went wrong with what the menu did there: each has the
//! keys and the mouse until it closes, when the side that had them before has
//! them again. `Ctrl j` opens the picker in a box there too, on the
//! sessions, `Ctrl b` on the agents and `Ctrl d` on the directories. `Alt r`
//! dictates, held or tapped, into the pane zellij is on or the ask box, in a
//! box along the bottom of zellij that has the keys and the mouse until what
//! you said is written. What the mouse drags on zellij stays with zellij
//! under a box too.
//! A click on an entry of the menu, a key a line of keys names, a skill the
//! menu's search finds, or a row of the picker, does what its key does, or
//! `Enter` on it, as each said where it drew them; and the one under the
//! pointer is lit.

use std::collections::VecDeque;
use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::{ClipboardType, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate, SetTitle};
use crossterm::{execute, queue};
use lunetad::api::Host;
use lunetad::asks::User;
use lunetad::log::{self, Process};
use lunetad::Destination;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::{DefaultTerminal, Terminal};

use crate::alerts::{self, Alerts};
use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange, Theme};
use crate::ask::{Ask, Heard, Keyed};
use crate::away;
use crate::brief::{self, Asking, Briefing, Written};
use crate::close::Act;
use crate::desktop::{self, Desktop};
use crate::dictation::{self, Dictated, Dictation};
use crate::due::Due;
use crate::fails::{Back, Failed, Fails};
use crate::folds::{self, Folds};
use crate::hosted::{Hosted, Said, Size};
use crate::images::{self, Went};
use crate::input;
use crate::keys;
use crate::machines::{self, Added, Machine};
use crate::menu::{self, Choice};
use crate::named;
use crate::news::{self, News};
use crate::order::{self, Order};
use crate::own::{Before, Look, Looked};
use crate::peers;
use crate::picker::{self, List, Picked, Picker};
use crate::printed::{self, Printed};
use crate::review;
use crate::pull::{self, Ready};
use crate::sidebar::{self, Asked, HereAgent, Keys, Place, Sidebar};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Fetched, Speech, Spoken};
use crate::spots::{Burst, Drawn, Hit};
use crate::starts::{self, First, Sought, Start};
use crate::width::Width;
use crate::zellij::{
    self, After, Alone, Arrival, Close, Closes, Ended, Refuge, SessionName, Tab, Unused,
};

/// What luneta's loop acts on, from the threads that wait on each source.
pub enum Event {
    /// A key, the mouse, a paste or a resize of luneta's terminal.
    Input(Input),
    /// An answer of luneta's terminal, or what it says of itself unasked,
    /// which comes in among the input whenever it comes.
    Answered(Answer),
    /// luneta's terminal cannot be read any more.
    InputEnded(io::Error),
    /// What the program luneta runs beside the sidebar did: the one of this
    /// number, of those luneta started, which is zellij's client while it is
    /// the last.
    Hosted(u64, Said),
    /// luneta reached a daemon, which reads Claude Code's sessions in
    /// `claude_folder`, where the transcripts of the agents it lists are.
    Reached { claude_folder: Option<PathBuf> },
    /// The host as the daemon says it stands, or why the daemon does not say.
    Daemon(Result<Host, String>),
    /// What zellij answered a look at the clients of a session.
    Looked(Looked),
    /// zellij did not take its client to the session luneta sent it to, on
    /// the go whose look, by its number, was to find it there.
    Stayed(u64),
    /// Something went wrong that the sidebar says.
    Notice(String),
    /// What came of the image `Ctrl v` was to paste in an agent's pane on
    /// another machine.
    Image(Went),
    /// The image `Ctrl v` is sending to this machine is taking a while.
    ImageSlow(Destination),
    /// What zellij opened for the menu numbered `menu`: the terminal, or why
    /// none.
    Opened { menu: u64, opened: Result<u32, String> },
    /// What closing the pane of the menu numbered `menu` closes, as zellij
    /// lists its session, or why it does not say.
    Closes { menu: u64, closes: Result<Closes, String> },
    /// The skills the directory of the menu numbered `menu` can run, or why
    /// the finder found none.
    Skills { menu: u64, skills: Result<Vec<Skill>, String> },
    /// Where the key of the menu numbered `menu`, for a pull request or an
    /// issue, goes, with the worktree made, or why luneta found nothing.
    Pulled { menu: u64, ready: Result<Ready, String> },
    /// What the pane of the menu numbered `menu` printed after its last
    /// command's line, as luneta kept it, or why it kept nothing.
    Printed { menu: u64, printed: Result<Printed, String> },
    /// What came of the brief numbered `number`.
    Briefed { number: u64, written: Written },
    /// What the turn of the ask box numbered `number` heard its agent do.
    Heard { number: u64, heard: Heard },
    /// What zellij answered the picker numbered `number`.
    Picker { number: u64, heard: picker::Heard },
    /// What the speech model said of the dictation numbered `number`.
    Spoken { number: u64, spoken: Spoken },
    /// The skills that the dictation numbered `number` can name, or why the
    /// finder found none.
    Nameable { number: u64, skills: Result<Vec<Skill>, String> },
    /// How the fetch of the speech model goes.
    Fetched(Fetched),
    /// A fold was made or taken away where the folds are kept: by another
    /// luneta, or by this one.
    Folds,
    /// Something was kept of what you saw or marked unread: by another
    /// luneta, or by this one.
    News,
    /// The order of the sidebar's rows was kept: by another luneta, or by
    /// this one.
    Order,
    /// What a notification says, read for it.
    Alerted(alerts::Read),
    /// You clicked the notification of this agent.
    Clicked(alerts::Key),
    /// What came of the lock that the luneta of this machine that puts the
    /// notifications up holds, or of another luneta of the machine.
    Peers(peers::Heard),
}

/// How often the sidebar is drawn again with nothing else to draw it: every
/// second for the ages, and at each turn of the spinner while one turns.
const TICK: Duration = Duration::from_secs(1);

/// The most events that go into one frame. Those that come faster wait for the
/// next, so that luneta draws at least once each this many events, however
/// fast they come.
const FRAME_EVENTS_MAX: usize = 256;

/// How long luneta holds the input for zellij at the most, for a zellij that
/// shows nothing luneta can tell from the blank screen it starts on, as a
/// pane with nothing in it and no frame or bar about it.
const WAITING_MAX: Duration = Duration::from_secs(2);

/// How often luneta looks at zellij's screen while it holds the input for
/// zellij: what zellij draws as luneta draws brings no `Said::Drew` of its
/// own.
const WAITING_LOOK: Duration = Duration::from_millis(50);

/// How long a luneta with no session to join here waits to hear what runs on
/// the other machines, from when it starts to. A machine it has not heard of
/// by then counts as having none: lunetad gives ssh `lunetad::REACH`, and a
/// machine that is off would hold up each start for all of that, with nothing
/// but this to see. One that is on takes a second or two.
const HEARING: Duration = Duration::from_secs(5);

/// What ssh ends with where it failed itself, and not the command it ran on
/// the machine: as where it reached none, or lost its connection.
const SSH_FAILED: i32 = 255;

/// The key that hands the keys from zellij to the sidebar, and back.
const SWAP: (KeyCode, KeyModifiers) = (KeyCode::Char('a'), KeyModifiers::CONTROL);

/// The key that collapses the sidebar, and opens it again.
const COLLAPSE: (KeyCode, KeyModifiers) = (KeyCode::Char('q'), KeyModifiers::CONTROL);

/// The key that takes zellij to the pane the sidebar lists after the one it
/// is on.
const NEXT: (KeyCode, KeyModifiers) = (KeyCode::Tab, KeyModifiers::ALT);

/// The key that opens the menu over zellij, for the pane zellij is on, and
/// closes it.
const MENU: (KeyCode, KeyModifiers) = (KeyCode::Char('a'), KeyModifiers::ALT);

/// The key that opens the picker over zellij, on the sessions.
const SESSIONS: (KeyCode, KeyModifiers) = (KeyCode::Char('j'), KeyModifiers::CONTROL);

/// The key that opens the picker over zellij, on the agents.
const AGENTS: (KeyCode, KeyModifiers) = (KeyCode::Char('b'), KeyModifiers::CONTROL);

/// The key that opens the picker over zellij, on the directories.
const DIRS: (KeyCode, KeyModifiers) = (KeyCode::Char('d'), KeyModifiers::CONTROL);

/// Where the keys are: with a side, or with a box over zellij, which has them
/// and the mouse until it closes, when that side has them again.
#[derive(Debug)]
struct Focus {
    /// The side with the keys, or with them again once the box closes, and
    /// whether the sidebar is open.
    side: Side,
    over: Option<Over>,
}

impl Focus {
    /// `side` with the keys, under no box.
    fn on(side: Side) -> Focus {
        Focus { side, over: None }
    }

    /// Whether zellij has the keys.
    fn zellij(&self) -> bool {
        match (self.side, &self.over) {
            (Side::Zellij | Side::Collapsed, None) => true,
            (Side::Sidebar, None) | (_, Some(_)) => false,
        }
    }

    /// Whether zellij is told it has the keys: while it has them, and while
    /// the dictation box over it has them, for what you say to its pane.
    fn zellij_told(&self) -> bool {
        match (self.side, &self.over) {
            (Side::Zellij | Side::Collapsed, None | Some(Over::Dictation(_, Where::Zellij(_)))) => {
                true
            },
            (Side::Sidebar, _) | (_, Some(_)) => false,
        }
    }
}

/// A side of luneta's terminal that takes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// zellij, beside the sidebar.
    Zellij,
    Sidebar,
    /// zellij, beside the sidebar collapsed, with the rest of its columns:
    /// the sidebar takes the keys only once it opens again.
    Collapsed,
}

impl Side {
    /// zellij, beside the sidebar as it is: collapsed, or open.
    fn zellij(self) -> Side {
        match self {
            Side::Collapsed => Side::Collapsed,
            Side::Zellij | Side::Sidebar => Side::Zellij,
        }
    }
}

/// A box over zellij.
#[derive(Debug)]
enum Over {
    /// The sidebar's menu, for the pane zellij is on. What it opens takes
    /// zellij there, with the keys.
    Menu,
    /// What went wrong with what the menu did, said where it floated, or why
    /// it did not open: any key or click closes it, and does nothing else.
    /// `MENU` opens the menu again, as it says to try again.
    Note(String),
    /// A brief, from when the menu asks for it. One closed while it is asked
    /// is given up, and the model with it.
    Brief(Briefing),
    /// The ask box, from when the menu asks for it until it closes, which
    /// gives up the turn its agent is at.
    Ask(Ask),
    /// The picker, from `SESSIONS`, `AGENTS` or `DIRS` until it closes, or
    /// takes zellij somewhere.
    Picker(Box<Picker>),
    /// The dictation box, from `Alt r` until what you said is written, over
    /// what it writes into, which has the keys again as it closes.
    Dictation(Dictation, Where),
}

/// The fetch of the speech model, and how many bytes of it are in, which a
/// dictation box opened while it goes says.
struct Fetch {
    /// Dropped, as luneta ends, it gives the fetch up.
    _asking: Asking,
    got: u64,
}

/// Where what you dictate is written: in the pane zellij is on, which had this
/// agent in it as the box opened, if it had one, or on the line of the ask box,
/// which waits under the dictation's.
#[derive(Debug)]
enum Where {
    Zellij(Option<HereAgent>),
    Ask(Ask),
}

/// What a button held down holds, wherever the mouse goes, until it is let
/// go: zellij, the sidebar's edge, its last column, or the press luneta took
/// for itself, and never two of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// Nothing: no button is down, or one went down on the sidebar's rows,
    /// which the sidebar holds itself.
    Nothing,
    /// zellij, as a button went down over it: what it drags goes to zellij.
    Zellij,
    /// The sidebar's edge, as the left button went down on it, and whether
    /// it was dragged since. One let go undragged keeps no width, so that
    /// what another luneta dragged meanwhile stays.
    Edge { dragged: bool },
    /// A press luneta took for itself: one that closed a box over zellij, as
    /// the menu or a brief, one on a spot, or one of a burst of clicks that
    /// is none. What it drags, and its release, go nowhere, as zellij heard
    /// no press of it.
    Taken,
}

/// What luneta holds for zellij until zellij can take it: the input, and the
/// look for its client's arrival.
struct Waiting {
    input: Vec<Input>,
    /// Asked for only once zellij is in, where its server may have yet to
    /// take its first client, as `Before::Made` says.
    arrival: Option<Arrival>,
    /// When zellij is handed the input all the same.
    until: Instant,
}

impl Waiting {
    /// Whether zellij's server may have yet to take its first client as
    /// luneta's comes: in a session zellij makes, one of a name it makes up,
    /// as of a client started with none, or one it did not list the clients
    /// of.
    fn fresh(&self) -> bool {
        self.arrival.as_ref().is_none_or(Arrival::fresh)
    }
}

/// How far zellij's client is into its session, until it is in, which zellij
/// says as it first draws for it, as it draws only for one on its screen:
/// once it titles its terminal, or once anything shows there. zellij titles a
/// tab once for all its clients, so that a client that comes to a session
/// already titled gets none.
enum Coming {
    /// zellij has yet to draw, and luneta holds the input for it, as keys
    /// typed as it starts: zellij 0.45 drops keys that reach it before it
    /// puts its client on its screen.
    Held(Waiting),
    /// zellij drew nothing in the time luneta holds the input, and has it,
    /// but luneta asks it for its client's arrival, and starts the daemon,
    /// only once it draws: its server may have yet to take its first client.
    Overdue(Option<Arrival>),
}

impl Coming {
    /// What waits on the client's arrival, if anything.
    fn arrival(self) -> Option<Arrival> {
        match self {
            Coming::Held(Waiting { arrival, .. }) | Coming::Overdue(arrival) => arrival,
        }
    }
}

/// luneta's client on its way to a session zellij makes for it, from the one
/// it is in, or as a client luneta starts to make it, until zellij titles
/// luneta's terminal there. Dropped before, as
/// where zellij did not take it there, or luneta sent it elsewhere since, or
/// it ended, what waits on its arrival hears that it did not come.
struct Making {
    arrival: Arrival,
    /// When luneta takes the client for one zellij did not take there.
    until: Instant,
}

/// zellij's client, which luneta runs beside the sidebar, and what luneta
/// knows of it. One that takes the place of a client that ended knows only
/// what the first did as it started.
struct Client {
    hosted: Hosted,
    /// Where luneta's zellij goes where it fails, as where ssh does not reach
    /// its machine, or zellij cannot attach.
    fails: Fails,
    /// The pane it goes on to once it is in that pane's session, as luneta
    /// started it to go to it.
    then_pane: Option<Place>,
    /// How it ended, once its terminal says.
    exited: Option<ExitStatus>,
    /// None once zellij is in its session.
    coming: Option<Coming>,
    /// Where luneta sent the client to a session zellij makes for it, while
    /// zellij has yet to title luneta's terminal there.
    making: Option<Making>,
    /// The last key it typed, and where, as far as luneta knows. It makes the
    /// client the one zellij moves only in a session it is in still.
    typed: Option<Typed>,
}

/// How a client luneta starts comes to its session.
#[derive(Clone, Copy)]
enum Joins<'a> {
    /// It attaches to this one, which runs, or which ended, which zellij
    /// brings back as it does; and fails where zellij has none by its name.
    Attach(&'a SessionName),
    /// It makes the one luneta found unused just before, in that directory,
    /// or where luneta started, and fails where one runs by its name by then,
    /// or zellij keeps one by it that ended, as `zellij::make` says. luneta
    /// finds it there once zellij titles its terminal in it, as `restart`
    /// says.
    Make(&'a Unused, Option<&'a str>),
}

impl Joins<'_> {
    fn session(&self) -> &SessionName {
        match self {
            Joins::Attach(session) => session,
            Joins::Make(unused, _) => unused.name(),
        }
    }
}

/// Where luneta's client goes on once a client luneta starts in its place is
/// in.
enum Next {
    /// The pane it goes on to.
    Pane(Place),
    /// What it does there, which opens what it goes to, as `After` says.
    After(After),
}

/// A key zellij's client typed in `session`, at `at`: one of yours, or
/// `zellij::NUDGE`. zellij moves the client that last typed in a session on
/// an action asked of it there, unless that client left it since. luneta
/// hears it left only from zellij's title or from a go of its own: one that
/// zellij's keys took out and back, with no title, leaves the key standing,
/// and an action there right after goes on its nudge alone. The look after
/// it says when zellij then took the client nowhere.
struct Typed {
    session: String,
    at: Instant,
}

/// What an action asked of zellij in `session` waits for, to move zellij's
/// client: zellij to take in, by `until`, the last key the client typed
/// there, as the nudge that made this, or one before. Only the client makes
/// one, as it types that nudge.
pub struct Nudged {
    /// The machine whose zellij the session is of, where the nudge went.
    on: Machine,
    session: SessionName,
    until: Instant,
}

impl Nudged {
    /// The machine whose zellij the action is asked of.
    pub fn on(&self) -> &Machine {
        &self.on
    }

    /// The session the action is asked of, of `on`'s, which zellij's client
    /// is in.
    pub fn session(&self) -> &SessionName {
        &self.session
    }

    /// When the action can go.
    pub fn until(&self) -> Instant {
        self.until
    }
}

impl Client {
    /// zellij's client, `hosted`, with the input held for it, and the look
    /// for its `arrival` if any, until zellij can take it; where it goes
    /// where it `fails`; and the pane it goes on to once it is in, if any.
    fn new(
        hosted: Hosted,
        fails: Fails,
        arrival: Option<Arrival>,
        then_pane: Option<Place>,
    ) -> Self {
        let until = Instant::now() + WAITING_MAX;
        let coming = Some(Coming::Held(Waiting { input: Vec::new(), arrival, until }));
        let (exited, making, typed) = (None, None, None);
        Client { hosted, fails, then_pane, exited, coming, making, typed }
    }

    /// Whether it may be on its way into a server zellij makes, which has yet
    /// to take it, its first client: as it starts, or goes to a session
    /// zellij makes. A zellij call then kills that server, as every one asks
    /// every session's server how it is.
    fn arriving(&self) -> bool {
        self.coming.is_some() || self.making.is_some()
    }

    /// It typed a key just now, in `session`, the one it is in.
    fn typed_in(&mut self, session: String) {
        self.typed = Some(Typed { session, at: Instant::now() });
    }

    /// When it last typed in `session`, as far as luneta knows.
    fn typed_at(&self, session: &str) -> Option<Instant> {
        self.typed.as_ref().filter(|typed| typed.session == session).map(|typed| typed.at)
    }

    /// Types `zellij::NUDGE` in `session`, the one it is in, where zellij
    /// reads it as a key.
    fn nudge_in(&mut self, session: String) {
        if let Some(nudge) = zellij::nudge(self.hosted.mode()) {
            self.hosted.send(nudge);
            self.typed_in(session);
        }
    }

    /// Types `zellij::NUDGE` in `here`, the session it is in, for an action
    /// asked of zellij there to move it, which can go once zellij has taken
    /// in the key it typed there last before, or this one, where it typed
    /// none there since it came. The nudge takes the client's turn back from
    /// another that typed there since, in time unless the action reaches
    /// zellij first. It runs on `on`, whose zellij `here` is of.
    fn nudge(&mut self, on: Machine, here: SessionName) -> Nudged {
        let before = self.typed_at(here.as_str());
        self.nudge_in(here.to_string());
        // Where it typed nothing there, as zellij reads no nudge, no wait
        // makes zellij move it.
        let typed = before.or_else(|| self.typed_at(here.as_str()));
        let until = typed.map_or_else(Instant::now, |typed| typed + zellij::NUDGED);
        Nudged { on, session: here, until }
    }
}

/// luneta's queue of events, with what came on it as luneta waited for its
/// terminal's answers put aside, to come first.
pub struct Inbox {
    events: Receiver<Event>,
    aside: VecDeque<Event>,
}

impl Inbox {
    pub fn new(events: Receiver<Event>) -> Self {
        Inbox { events, aside: VecDeque::new() }
    }

    /// The next answer of luneta's terminal by `deadline`, with what comes
    /// before it put aside. None once `deadline` passes, or once the terminal
    /// cannot be read.
    pub fn answer(&mut self, deadline: Instant) -> Option<Answer> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left).ok()? {
                Event::Answered(answer) => return Some(answer),
                ended @ Event::InputEnded(_) => {
                    self.aside.push_back(ended);
                    return None;
                },
                event => self.aside.push_back(event),
            }
        }
    }

    /// The next event, those put aside first, waiting `wait` at the most.
    fn next(&mut self, wait: Duration) -> Result<Event, RecvTimeoutError> {
        match self.aside.pop_front() {
            Some(event) => Ok(event),
            None => self.events.recv_timeout(wait),
        }
    }

    /// The next event, where one has come.
    pub fn try_next(&mut self) -> Option<Event> {
        self.aside.pop_front().or_else(|| self.events.try_recv().ok())
    }
}

struct App {
    client: Client,
    /// How many programs luneta started beside the sidebar, each zellij's
    /// client in its turn.
    started: u64,
    sidebar: Sidebar,
    /// Where the sidebar and zellij were last drawn, which the mouse goes by.
    areas: (Rect, Rect),
    /// How wide the sidebar opens.
    width: Width,
    held: Held,
    focus: Focus,
    /// Whether the window luneta's terminal is in has the focus. The sidebar
    /// sees you in a pane only while it does.
    focused: bool,
    /// The clicks on the cell of one that opened the sidebar collapsed, or
    /// pressed a spot, or next to it, that come fast after it, onto what
    /// that drew there.
    burst: Option<Burst>,
    /// What has the mouse drew last: the box over zellij, where one is, or
    /// else the sidebar, with the spots on its menu or legend, which a click
    /// goes to while it has the mouse still.
    drawn: Drawn,
    /// Where the mouse was last, for the spot under it to be lit.
    pointer: Option<Position>,
    /// The cursor as luneta's terminal was last told to draw it.
    cursor: Option<(CursorShape, bool)>,
    /// luneta's terminal's colours as it said them last, which zellij's
    /// terminal answers with.
    colours: Colours,
    /// What luneta's terminal turned to, until the colours it turned to are
    /// in, when zellij is told.
    turned: Option<Theme>,
    /// How many briefs luneta has asked for. A brief takes the next number
    /// as it is asked, which what comes of it carries.
    briefs: u64,
    /// How many ask boxes luneta has opened, which are numbered as briefs
    /// are.
    asks: u64,
    /// How many pickers luneta has opened, which are numbered as briefs are.
    pickers: u64,
    /// How many dictations luneta has opened, which are numbered as briefs
    /// are.
    dictations: u64,
    /// The image `images::KEY` is sending, if any.
    image: images::Going,
    /// When the key that dictates last went down, by which a press of it
    /// that is the key held is its repeat.
    repeats: dictation::Repeats,
    /// The thread that runs the speech model, from the first dictation on.
    speech: Option<Speech>,
    /// The fetch of the speech model, while it is on its way.
    fetching: Option<Fetch>,
    /// Whether luneta's terminal says when a key is let go, as the kitty
    /// keyboard protocol does, by which a dictation tells a hold from a tap.
    releases: bool,
    /// The folder the daemon reads Claude Code's sessions in, where a brief
    /// reads the transcripts of the agents it lists: none until luneta has
    /// reached one.
    claude_folder: Option<PathBuf>,
    /// The folder of the daemon luneta watches once zellij is in its session,
    /// until then: a daemon that luneta starts asks zellij about every
    /// session at once, which a server that has yet to take its first client
    /// cannot bear, as `Before::Made` says.
    daemon: Option<PathBuf>,
    events: Sender<Event>,
    /// What watches where the folds, the news and the order are kept, for as
    /// long as luneta runs.
    _watching: Vec<notify::RecommendedWatcher>,
    /// The directory luneta started in, which zellij's client runs in.
    dir: PathBuf,
    ended: bool,
    /// What kills and deletes the sessions luneta's keys delete, which
    /// luneta waits for as it ends: it ends with the last session of every
    /// machine, which zellij lets go of only once the session's server has
    /// ended.
    deleting: Vec<JoinHandle<()>>,
    /// The agents that want you, of every machine, and their notifications.
    alerts: Alerts,
    /// What puts the notifications up, which takes them down as luneta
    /// ends.
    desktop: Desktop,
}

/// Runs zellij beside the sidebar, attached to `session`, or to the session
/// `starts` finds where none is named, until zellij ends: its client detaches
/// or fails, or its session ends and no other runs, on any machine lunetad
/// reaches. Where another does, a client of the newest takes the place of the
/// one that ended, of its machine's first. The sidebar is of what the daemon
/// in `folder` says, which is started if none runs.
pub fn run(session: Option<SessionName>, folder: Result<PathBuf, String>) -> ExitCode {
    let dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("luneta: no directory to start in: {error}");
            return ExitCode::FAILURE;
        },
    };
    // Kept beside the daemon, which lists the sessions it is kept of.
    let news = match &folder {
        Ok(runtime) => News::within(runtime),
        Err(why) => News::unshared(why),
    };
    let start = match session {
        Some(session) => starts::named(session),
        None => starts::bare(zellij::session_for(&dir), &news),
    };
    let start = match start {
        Ok(start) => start,
        Err(refusal) => {
            eprintln!("luneta: {refusal}");
            return ExitCode::FAILURE;
        },
    };
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("luneta: no terminal: {error}");
            return ExitCode::FAILURE;
        },
    };
    // The cursor shows where a frame puts it, and nowhere before the first,
    // as luneta waits for its terminal's answers. ratatui's own, for it to
    // know the cursor is hidden: the first frame shows it at zellij's, and
    // ratatui shows it again as luneta ends.
    let hidden = terminal.hide_cursor();
    // Before the hook, so that no panic of the sidebar's goes by the log. Why
    // there is none is for the sidebar to say, once it has its notes.
    let unkept = log::start(Process::Luneta).err();
    // ratatui's hook puts the terminal back as it found it before a panic is
    // printed, all but the modes luneta turns on itself. The panic goes in
    // the log first, which no terminal can hold up.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::panicked(panic);
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = put_back(&mut io::stdout());
        restore(panic);
    }));
    // Not a `sync_channel` of a bound: alacritty hands on what the program
    // does with the screen locked, which the loop locks to draw, so that a
    // full queue would hold each up for the other for good. What fills it are
    // the keys and the mouse, the daemon's answers, zellij's actions and what
    // zellij does with its terminal, of which `hosted` keeps one draw at most.
    let (events, inbox) = mpsc::channel();
    let mut inbox = Inbox::new(inbox);
    input::read(events.clone());
    // The kitty keyboard protocol, where luneta's terminal speaks it, tells
    // apart keys that zellij's client can then tell apart too, as `Shift
    // Enter` from `Enter`. Its colours are what zellij asks its own terminal
    // for, as a program in a pane asks zellij.
    let answers = answers::ask(&mut inbox);
    let ran = execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableThemeChange
    )
    .and_then(|()| {
        // And when a key goes up, for a dictation held as you talk. luneta
        // hands zellij no key going up.
        if answers.kitty {
            let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
            execute!(io::stdout(), PushKeyboardEnhancementFlags(flags))?;
        }
        run_in(
            &mut terminal,
            start,
            news,
            folder,
            unkept,
            &dir,
            answers.colours,
            answers.kitty,
            events,
            &mut inbox,
        )
    });
    // Each mode is put back however the rest went, and the first error is the
    // one said.
    let popped =
        if answers.kitty { execute!(io::stdout(), PopKeyboardEnhancementFlags) } else { Ok(()) };
    let restored = put_back(&mut io::stdout());
    let left = ratatui::try_restore();
    // A terminal that hung up, as one whose window closed, is told nothing:
    // `eprintln!` would panic on it.
    let mut said = io::stderr();
    match hidden.and(ran).and_then(|failed| popped.and(restored).and(left).map(|()| failed)) {
        Ok(None) => ExitCode::SUCCESS,
        // What zellij said as it failed went with luneta's screen.
        Ok(Some(failed)) => {
            let _ = writeln!(said, "{failed}");
            ExitCode::FAILURE
        },
        Err(error) => {
            let _ = writeln!(said, "luneta: {error}");
            ExitCode::FAILURE
        },
    }
}

/// Puts luneta's `terminal` back as luneta found it, all but the kitty
/// keyboard flags, the screen and raw mode, however luneta ends: out of the
/// update of a frame, where a panic ended luneta in one, with the modes
/// luneta turned on off, and the cursor in your own style. The panic hook and
/// the end of `run` both put back what this does, for them to put back the
/// same.
fn put_back(terminal: &mut impl io::Write) -> io::Result<()> {
    execute!(
        terminal,
        EndSynchronizedUpdate,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableThemeChange,
        SetCursorStyle::DefaultUserShape
    )
}

/// Runs zellij beside the sidebar until it ends, from where `start` says, and
/// says what it said as it failed, if it did, or why luneta started no client.
/// zellij's terminal answers it with `colours`, until luneta's terminal says
/// others, which says when a key is let go where `releases` says. `unkept` is
/// why what goes wrong is kept in no log, where it is not.
fn run_in(
    terminal: &mut DefaultTerminal,
    start: Start,
    news: News,
    folder: Result<PathBuf, String>,
    unkept: Option<String>,
    dir: &Path,
    colours: Colours,
    releases: bool,
    events: Sender<Event>,
    inbox: &mut Inbox,
) -> io::Result<Option<String>> {
    let (width, width_unread) = Width::kept();
    let folds = Folds::shared();
    let order = Order::shared();
    let folds_dirs = folds.dirs();
    let folds_watched =
        (!folds_dirs.is_empty()).then(|| folds::watch(&folds_dirs, events.clone())).transpose();
    let news_watched = news::watch(&news, events.clone());
    let order_watched = order.file().map(|file| order::watch(file, events.clone())).transpose();
    let mut sidebar = Sidebar::new(folds, news, order, machines::local_name());
    if let Some(why) = unkept {
        sidebar.notify(format!("what goes wrong is kept in no log: {why}"));
    }
    let mut watching = Vec::new();
    match folds_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => sidebar
            .notify(format!("folds made elsewhere show from luneta's next start: {error}")),
    }
    match news_watched {
        Ok(watcher) => watching.push(watcher),
        Err(error) => sidebar
            .notify(format!("dots of other lunetas show from luneta's next start: {error}")),
    }
    match order_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => sidebar
            .notify(format!("rows moved elsewhere show from luneta's next start: {error}")),
    }
    if let Some(why) = width_unread {
        sidebar.notify(format!("the sidebar opens {} wide: {why}", sidebar::COLUMNS));
    }
    let mut daemon = match folder {
        Ok(folder) => Some(folder),
        Err(why) => {
            sidebar.lost(why);
            None
        },
    };
    let first = match start {
        Start::Now(first) => first,
        Start::Hear { machines, named } => {
            // No session runs here for a daemon that starts to trouble, as
            // `App::ask` says.
            if let Some(folder) = daemon.take() {
                crate::watch::start(folder, events.clone());
                hear(terminal, &mut sidebar, width.columns(), machines, inbox)?;
            }
            match starts::heard(named, &sidebar) {
                Ok(first) => first,
                Err(refusal) => return Ok(Some(format!("luneta: {refusal}"))),
            }
        },
    };
    let ((program, args), fails, arrival) = first_client(first, &mut sidebar);
    let areas = areas(Rect::from((Position::ORIGIN, terminal.size()?)), width.columns());
    let started = 1;
    let hosted = Hosted::start(program, args, dir, window(areas.1), started, events.clone())?;
    let client = Client::new(hosted, fails, arrival, None);
    let desktop = Desktop::start(&events);
    let mut app = App {
        client,
        started,
        sidebar,
        areas,
        width,
        held: Held::Nothing,
        focus: Focus::on(Side::Zellij),
        // Until the terminal says otherwise. One that never tells a program
        // of its window's focus has you in it all along.
        focused: true,
        burst: None,
        drawn: Drawn::Nothing,
        pointer: None,
        cursor: None,
        colours,
        turned: None,
        briefs: 0,
        asks: 0,
        pickers: 0,
        dictations: 0,
        image: images::Going::default(),
        repeats: dictation::Repeats::default(),
        speech: None,
        fetching: None,
        releases,
        claude_folder: None,
        daemon,
        events,
        _watching: watching,
        dir: dir.to_owned(),
        ended: false,
        deleting: Vec::new(),
        alerts: Alerts::default(),
        desktop,
    };
    let ran = run_loop(terminal, &mut app, inbox);
    for deleting in app.deleting.drain(..) {
        // What a thread that panicked left undone stays so.
        let _ = deleting.join();
    }
    ran?;
    Ok(app.failure())
}

/// Draws the sidebar, and what luneta waits for where zellij will show, until
/// lunetad has said what runs on each of `machines`, or `HEARING` has gone by:
/// a daemon that does not answer is looked for again meanwhile, as one can end
/// as it is reached, or make way for a newer. What else comes meanwhile, the
/// keys among it, waits in `inbox` for the loop, with what the daemon said, in
/// the order it came, so that the loop hears of it all as it hears of it from
/// then on.
fn hear(
    terminal: &mut DefaultTerminal,
    sidebar: &mut Sidebar,
    columns: u16,
    machines: Vec<Destination>,
    inbox: &mut Inbox,
) -> io::Result<()> {
    let within = zellij::within();
    let until = Instant::now() + HEARING;
    let mut unheard = machines;
    while !unheard.is_empty() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        whole(terminal, |terminal| {
            terminal.draw(|frame| {
                let now = SystemTime::now();
                let (beside, zellij) = areas(frame.area(), columns);
                let buffer = frame.buffer_mut();
                sidebar.draw(buffer, beside, now);
                draw_hearing(buffer, zellij, &unheard, now);
            })?;
            Ok(())
        })?;
        match inbox.events.recv_timeout(left.min(sidebar::SPIN)) {
            Ok(Event::InputEnded(error)) => return Err(error),
            Ok(event) => {
                match &event {
                    // luneta's zellij has come to no session for it to look for.
                    Event::Daemon(Ok(host)) => {
                        unheard = sidebar::unheard(host, within);
                        let _ = sidebar.heard(host.clone());
                    },
                    Event::Daemon(Err(why)) => sidebar.lost(why.clone()),
                    _ => {},
                }
                inbox.aside.push_back(event);
            },
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the caller holds a sender, for zellij's client")
            },
        }
    }
    Ok(())
}

/// Says in `area`, where zellij will show, that luneta waits to hear what runs
/// on `machines`, with the spinner at `now`.
fn draw_hearing(buffer: &mut Buffer, area: Rect, machines: &[Destination], now: SystemTime) {
    let names: Vec<String> = machines.iter().map(ToString::to_string).collect();
    let doing = format!("Waiting to hear what runs on {}…", names.join(", "));
    let dim = Style::new().add_modifier(Modifier::DIM);
    let lines = menu::waiting(&doing, usize::from(area.width), dim, sidebar::frame(now));
    for (line, y) in lines.iter().zip(area.y + 1..area.bottom()) {
        buffer.set_line(area.x, y, line, area.width);
    }
}

/// The program of luneta's first client, and its arguments, where luneta's
/// zellij goes where it fails, and the look for its arrival, if luneta looks
/// for it: as `first` says, of which the sidebar hears.
fn first_client(
    first: First,
    sidebar: &mut Sidebar,
) -> ((String, Vec<String>), Fails, Option<Arrival>) {
    match first {
        First::Make(session) => {
            let arrival = session.as_ref().and_then(|session| {
                let before = zellij::before(session.as_str());
                let look = sidebar.sending(session)?;
                Some(Arrival::new(Machine::Local, look, before, None))
            });
            (zellij::client(session.as_ref()), Fails::End, arrival)
        },
        First::Here(session) => {
            // Just before the client starts, as `own` goes by: luneta may
            // have waited on its terminal since it chose the session.
            let before = zellij::before(session.as_str());
            let look = sidebar.sending(&session);
            let arrival = look.map(|look| Arrival::new(Machine::Local, look, before, None));
            (zellij::rejoin(session.as_str()), Fails::End, arrival)
        },
        First::Away(destination, Refuge { session, before }) => {
            let program = zellij::attach_away(&destination, session.as_str());
            let machine = Machine::Remote(destination);
            let look = sidebar.swapping(&machine, &session);
            let arrival = Arrival::new(machine, look, Before::Listed(before), None);
            (program, Fails::Onward(Vec::new()), Some(arrival))
        },
    }
}

/// Draws a frame for each batch of events, until zellij ends with no client
/// to take the place of its own, or luneta's terminal can no longer be read.
fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    inbox: &mut Inbox,
) -> io::Result<()> {
    loop {
        let wait = if matches!(app.client.coming, Some(Coming::Held(_))) {
            WAITING_LOOK
        } else if app.sidebar.scrolling() {
            sidebar::SCROLL
        } else if app.turning() {
            sidebar::SPIN
        } else {
            TICK
        };
        // Or until the picker's preview asks zellij for a screen.
        let due = app.due().map(|due| due.saturating_duration_since(Instant::now()));
        let wait = due.map_or(wait, |due| wait.min(due));
        match inbox.next(wait) {
            Ok(event) => app.handle(event, terminal)?,
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the app holds a sender, for zellij's actions")
            },
        }
        // What came while luneta drew goes into one frame, with the event it
        // waited for: `FRAME_EVENTS_MAX` in all.
        for _ in 1..FRAME_EVENTS_MAX {
            let Some(event) = inbox.try_next() else {
                break;
            };
            app.handle(event, terminal)?;
        }
        if app.ended {
            return Ok(());
        }
        app.wait_on(Instant::now());
        app.ticked(Instant::now());
        app.alerting(Instant::now());
        app.sidebar.ticked(SystemTime::now(), Instant::now(), app.focused);
        app.draw(terminal)?;
    }
}

/// The columns the sidebar takes of the terminal, and those left for zellij:
/// `own`, the sidebar's, open or collapsed, or half the terminal's, where that
/// is narrower.
fn areas(frame: Rect, own: u16) -> (Rect, Rect) {
    let sidebar = own.min(frame.width / 2);
    let left = Rect::new(frame.x, frame.y, sidebar, frame.height);
    let right = Rect::new(frame.x + sidebar, frame.y, frame.width - sidebar, frame.height);
    (left, right)
}

/// The size of zellij's terminal, for an `area` of luneta's: in cells, one
/// each way for an area with none, and in pixels where luneta's terminal says
/// how big its cells are.
fn window(area: Rect) -> Size {
    let cell =
        crossterm::terminal::window_size().ok().filter(|size| size.columns > 0 && size.rows > 0);
    let cells = |count: u16| NonZeroU16::new(count).unwrap_or(NonZeroU16::MIN);
    Size {
        columns: cells(area.width),
        lines: cells(area.height),
        cell_width: cell.as_ref().map_or(0, |size| size.width / size.columns),
        cell_height: cell.map_or(0, |size| size.height / size.rows),
    }
}

impl App {
    fn handle(&mut self, event: Event, terminal: &mut DefaultTerminal) -> io::Result<()> {
        // What changes where zellij is, or what the daemon says, changes what
        // the picker lists.
        let relists = matches!(
            event,
            Event::Daemon(_)
                | Event::Looked(_)
                | Event::Stayed(_)
                | Event::Hosted(_, Said::Titled(_))
        );
        match event {
            Event::Input(input) => match &mut self.client.coming {
                Some(Coming::Held(waiting)) => waiting.input.push(input),
                _ => self.input(input),
            },
            Event::Answered(answer) => self.answered(answer, terminal)?,
            Event::InputEnded(error) => return Err(error),
            // What a client that luneta put another in the place of says is
            // of a terminal no longer shown.
            Event::Hosted(number, said) => {
                if number == self.client.hosted.number() {
                    self.hosted_said(said, terminal)?;
                }
            },
            Event::Reached { claude_folder } => self.claude_folder = claude_folder,
            Event::Daemon(Ok(host)) => {
                let look = self.sidebar.heard(host);
                self.arrived(look);
                if let Some(machines) = self.sidebar.reaches() {
                    let acts = self.alerts.heard(machines, Instant::now());
                    self.alert(acts);
                }
            },
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Looked(looked) => {
                if self.sidebar.looked(looked) {
                    self.client.fails.came_in();
                }
                self.on_to_pane();
            },
            Event::Stayed(look) => {
                let stayed = |making: &Making| making.arrival.look.number == look;
                if let Some(making) = self.client.making.take_if(|making| stayed(making)) {
                    making.arrival.stayed(&self.events);
                }
                let look = self.sidebar.stayed(look);
                // The client never left: what it typed there counts still.
                self.look(look);
            },
            Event::Notice(notice) => self.sidebar.notify(notice),
            Event::Image(went) => self.image_went(went),
            Event::ImageSlow(on) => self.sidebar.image_going(Some(on)),
            Event::Folds => self.sidebar.refold(),
            Event::Alerted(read) => {
                if self.alerts.read(&read) {
                    self.desktop.post(read.alert);
                }
            },
            Event::Clicked(key) => self.clicked(&key),
            Event::Peers(heard) => {
                if self.desktop.heard(heard, self.focused) {
                    let acts = self.alerts.again();
                    self.alert(acts);
                }
            },
            Event::Opened { menu, opened } => {
                let asked = self.sidebar.opened(menu, opened);
                self.follow(asked);
            },
            Event::Closes { menu, closes } => {
                let asked = self.sidebar.closes(menu, closes);
                self.follow(asked);
            },
            Event::Skills { menu, skills } => self.sidebar.found(menu, skills),
            Event::Pulled { menu, ready } => {
                let asked = self.sidebar.pulled(menu, ready);
                self.follow(asked);
            },
            Event::Printed { menu, printed } => {
                let asked = self.sidebar.printed(menu, printed);
                self.follow(asked);
            },
            Event::Briefed { number, written } => {
                if let Some(Over::Brief(briefing)) = &mut self.focus.over
                    && briefing.number() == number
                {
                    briefing.written(written);
                }
            },
            Event::Heard { number, heard } => {
                if let Some(Over::Ask(ask)) = &mut self.focus.over
                    && ask.number() == number
                {
                    ask.heard(heard);
                }
            },
            Event::Picker { number, heard } => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.heard(heard);
                    self.picked(did);
                }
            },
            Event::Spoken { number, spoken } => {
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                    && let Some(dictated) = dictation.spoken(spoken, Instant::now())
                {
                    self.undictate(Some(dictated));
                }
            },
            Event::Nameable { number, skills } => {
                if let Err(why) = &skills {
                    self.sidebar.notify(why.clone());
                }
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                    && let Some(dictated) = dictation.found(skills)
                {
                    self.undictate(Some(dictated));
                }
            },
            Event::Fetched(fetched) => self.fetched(&fetched),
            Event::News => self.sidebar.reread(),
            Event::Order => self.sidebar.reorder(),
        }
        if relists {
            self.relist();
        }
        self.settle();
        Ok(())
    }

    /// Hands the picker, while it is open, what the sidebar knows now.
    fn relist(&mut self) {
        if !matches!(self.focus.over, Some(Over::Picker(_))) {
            return;
        }
        let listing = self.sidebar.listing();
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.listed(listing);
        }
    }

    /// Whether a spinner turns: on the sidebar, or in the box over zellij
    /// while what it waits on is at work.
    fn turning(&self) -> bool {
        let over = match &self.focus.over {
            Some(Over::Brief(briefing)) => briefing.turning(),
            Some(Over::Ask(ask)) => ask.turning(),
            Some(Over::Dictation(dictation, _)) => dictation.turning(),
            Some(Over::Menu | Over::Picker(_) | Over::Note(_)) | None => false,
        };
        over || self.sidebar.turning()
    }

    /// When the picker asks zellij for what its preview shows, while it is
    /// open and is to ask, or an agent's notification is to go up, whichever
    /// is first.
    fn due(&self) -> Option<Instant> {
        let picker = match &self.focus.over {
            Some(Over::Picker(picker)) => picker.due(),
            _ => None,
        };
        picker.into_iter().chain(self.alerts.due()).min()
    }

    /// Takes down the notification of an agent in a pane you look at, in
    /// luneta's window, which has the focus, or in another luneta's of this
    /// machine, and has those of the agents that have waited on you long
    /// enough by `now` read for.
    fn alerting(&mut self, now: Instant) {
        let here = self.sidebar.whereabouts();
        let mine = if self.focused { self.alerts.at(here.as_ref()) } else { Vec::new() };
        self.desktop.looking(&mine);
        let mut looked = self.desktop.looks();
        looked.extend(mine);
        let mut acts = self.alerts.looked(&looked);
        acts.extend(self.alerts.ticked(now));
        self.alert(acts);
    }

    /// Does what the notifications are to.
    fn alert(&mut self, acts: Vec<alerts::Act>) {
        for act in acts {
            match act {
                alerts::Act::Read(due) => {
                    alerts::read(due, self.claude_folder.clone(), &self.events);
                },
                alerts::Act::Close(key) => self.desktop.close(key),
            }
        }
    }

    /// Takes zellij's client to the pane of the agent whose notification you
    /// clicked, as `Enter` on its row does, and brings luneta's window
    /// forward: this luneta's, or the one of this machine whose window had
    /// the focus last, which goes there.
    fn clicked(&mut self, key: &alerts::Key) {
        let Some(place) = self.alerts.clicked(key) else {
            return;
        };
        self.desktop.close(key.clone());
        if self.desktop.hand(key) {
            return;
        }
        desktop::raise(self.sidebar.terminal_title());
        self.go(&key.on, &Place::Pane(place), None);
        self.leave();
    }

    /// Lets the picker ask zellij for what its preview shows, once it is
    /// due.
    fn ticked(&mut self, now: Instant) {
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.ticked(now);
        }
        self.ask_zellij();
    }

    /// Asks zellij what the picker has yet to ask, while it is open, once
    /// zellij's client is in: until then, what it asks waits.
    fn ask_zellij(&mut self) {
        if self.client.arriving() {
            return;
        }
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            let number = picker.number();
            for ask in picker.asks() {
                picker::ask(number, ask, &self.events);
            }
        }
    }

    /// Asks zellij what the picker has yet to ask, and gives the keys back
    /// to the side the menu over zellij took them from, once it closed: as a
    /// key or a click closed it, or as zellij said why what a key asked for
    /// went wrong, which a box says in its place.
    fn settle(&mut self) {
        self.ask_zellij();
        if matches!(self.focus.over, Some(Over::Menu)) && self.sidebar.floating().is_none() {
            let over = self.sidebar.said().map(Over::Note);
            self.give(Focus { side: self.focus.side, over });
        }
    }

    /// Hands zellij what luneta holds for it once anything shows on its
    /// screen, and the input once it waited long enough. A client on its way
    /// to a session zellij makes for it is taken for one that stayed, once
    /// luneta waited long enough for zellij to title its terminal there.
    fn wait_on(&mut self, now: Instant) {
        match self.client.coming.take() {
            coming @ Some(_) if !self.client.hosted.blank() => self.came_in(coming),
            Some(Coming::Held(waiting)) if now >= waiting.until => self.overdue(waiting),
            coming => self.client.coming = coming,
        }
        if let Some(making) = self.client.making.take_if(|making| now >= making.until) {
            self.sidebar.notify("zellij did not take its client there".to_owned());
            let look = self.sidebar.stayed(making.arrival.look.number);
            making.arrival.stayed(&self.events);
            self.look(look);
        }
    }

    /// zellij is in its session, as it drew or titled for its client,
    /// `coming` there until then: it is handed the input held for it, if any
    /// is, and asked what waited on that.
    fn came_in(&mut self, coming: Option<Coming>) {
        match coming {
            Some(Coming::Held(Waiting { input, arrival, .. })) => {
                self.ask(arrival);
                self.hand(input);
            },
            Some(Coming::Overdue(arrival)) => self.ask(arrival),
            None => self.ask(None),
        }
    }

    /// zellij drew nothing in the time luneta holds the input for it: it has
    /// the input all the same, and is asked what waited on it at once where
    /// its server took its first client long before, or else once it draws.
    fn overdue(&mut self, waiting: Waiting) {
        let fresh = waiting.fresh();
        self.hand(waiting.input);
        if fresh {
            self.client.coming = Some(Coming::Overdue(waiting.arrival));
        } else {
            self.ask(waiting.arrival);
        }
    }

    /// Asks zellij for its client's `arrival` in its session, if luneta looks
    /// for it, and watches the daemon, if luneta does not yet: a daemon that
    /// luneta starts asks zellij about every session at once.
    fn ask(&mut self, arrival: Option<Arrival>) {
        if let Some(arrival) = arrival {
            zellij::arrive(arrival, &self.events);
        }
        if let Some(folder) = self.daemon.take() {
            crate::watch::start(folder, self.events.clone());
        }
    }

    /// Hands zellij the `input` held for it. A client that took the place of
    /// one that ended heard nothing of where the keys are, and has them as far
    /// as it knows: it hears first where it does not, as the sidebar has them,
    /// or another window.
    fn hand(&mut self, input: Vec<Input>) {
        if !(self.focused && self.focus.zellij_told())
            && let Some(bytes) = keys::focus(false, self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
        for input in input {
            self.input(input);
        }
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Key(key) => self.key(&key),
            Input::Mouse(mouse) => self.mouse(mouse),
            // A pull request's or an issue's link, or a `.md` file's path,
            // pasted with the sidebar's keys, or in the menu over zellij,
            // opens the menu for it, and what is pasted in the menu's box for
            // a machine's destination goes in it. zellij keeps every paste it
            // has the keys for, links too, for the agents in it. The dictation
            // box does nothing with a paste, as with any key but its own.
            Input::Paste(text) => match &mut self.focus.over {
                Some(Over::Ask(ask)) => ask.paste(&text),
                Some(Over::Picker(picker)) => picker.paste(&text),
                Some(Over::Menu) => self.sidebar.paste(&text),
                Some(Over::Brief(_) | Over::Note(_) | Over::Dictation(..)) => {},
                None if self.focus.side == Side::Sidebar => self.sidebar.paste(&text),
                None => self.client.hosted.send(keys::paste(&text, self.client.hosted.mode())),
            },
            Input::FocusGained => self.focus_window(true),
            Input::FocusLost => self.focus_window(false),
            // The next frame is of the new size, and zellij's terminal with it.
            Input::Resize(..) => {},
        }
        // Before the next of the input held for zellij, which goes where the
        // keys are once the menu closed.
        self.settle();
    }

    fn key(&mut self, key: &KeyEvent) {
        let now = Instant::now();
        let key = &self.repeats.read(*key, now);
        // The dictation box takes every key, going up too, until it closes.
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            match dictation.key(key, now) {
                dictation::Keyed::Nothing => {},
                dictation::Keyed::Close => self.undictate(None),
                dictation::Keyed::Dictate => {
                    self.undictate(None);
                    self.dictate();
                },
            }
            return;
        }
        let chord = (key.code, key.modifiers);
        // No key going up reaches what is under the box, nor the key that
        // dictates, held down past the box it opened, as it repeats: it
        // would type itself there, or open the box again.
        if key.kind == KeyEventKind::Release
            || (chord == dictation::KEY && key.kind == KeyEventKind::Repeat)
        {
            return;
        }
        if chord == dictation::KEY && self.dictate() {
            return;
        }
        match &mut self.focus.over {
            // The ask box takes every key, for its agent.
            Some(Over::Ask(ask)) => {
                match ask.key(key) {
                    Keyed::Nothing => {},
                    Keyed::Send => self.converse(),
                    Keyed::Close => self.give(Focus::on(self.focus.side)),
                }
                return;
            },
            // So does the picker.
            Some(Over::Picker(picker)) => {
                let did = picker.key(key);
                self.picked(did);
                return;
            },
            // So does a brief, which does nothing else with them.
            Some(Over::Brief(briefing)) => {
                if briefing.closes(key) {
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            // So does the menu over zellij, but for `MENU`, which closes it as
            // it opened it.
            Some(Over::Menu) => {
                if chord == MENU {
                    self.sidebar.close_menu();
                } else {
                    let asked = self.sidebar.key(key, SystemTime::now());
                    self.follow(asked);
                }
                return;
            },
            // And so does a note, but for `MENU`, which then tries the menu
            // again. A key held down as it came is not the next key, and would
            // take it away unread.
            Some(Over::Note(_)) => {
                if key.kind == KeyEventKind::Repeat {
                    return;
                }
                self.give(Focus::on(self.focus.side));
                if chord != MENU {
                    return;
                }
            },
            // The dictation box took the key, above, as it takes those that go
            // up.
            Some(Over::Dictation(..)) => return,
            None => {},
        }
        if chord == MENU {
            let over = match self.sidebar.float_menu() {
                Ok(()) => Over::Menu,
                Err(why) => Over::Note(why.to_owned()),
            };
            self.give(Focus { side: self.focus.side, over: Some(over) });
            return;
        }
        let list = match chord {
            SESSIONS => Some(List::Sessions),
            AGENTS => Some(List::Agents),
            DIRS => Some(List::Dirs),
            _ => None,
        };
        if let Some(list) = list {
            self.pickers += 1;
            // `SESSIONS` or `DIRS` on a machine's header aims at it: the
            // machines to make the session typed on start on it, and the
            // highlight goes to its directories.
            let aim = match (list, self.focus.side) {
                (List::Sessions | List::Dirs, Side::Sidebar) => self.sidebar.on_header(),
                _ => None,
            };
            let aim = aim.unwrap_or_default();
            let listing = self.sidebar.listing();
            let picker = Box::new(Picker::open(self.pickers, list, listing, aim));
            self.give(Focus { side: self.focus.side, over: Some(Over::Picker(picker)) });
            return;
        }
        if chord == SWAP {
            // A collapsed sidebar opens to take the keys.
            let other = if self.focus.zellij() { Side::Sidebar } else { Side::Zellij };
            self.give(Focus::on(other));
            return;
        }
        if chord == COLLAPSE {
            let collapsed = self.focus.side == Side::Collapsed;
            self.give(Focus::on(if collapsed { Side::Zellij } else { Side::Collapsed }));
            return;
        }
        if chord == NEXT {
            self.next();
            return;
        }
        if self.focus.zellij() {
            if chord == images::KEY && self.key_for_image(key) {
                return;
            }
            self.key_to_zellij(key);
        } else {
            let asked = self.sidebar.key(key, SystemTime::now());
            self.follow(asked);
        }
    }

    /// Hands `key` to zellij's client, as what you typed.
    fn key_to_zellij(&mut self, key: &KeyEvent) {
        if let Some(bytes) = keys::key(key, self.client.hosted.mode()) {
            self.client.hosted.send(bytes);
            self.typed();
        }
    }

    /// Whether `key`, `images::KEY`, goes for an image: on the pane of an
    /// agent on another machine, where luneta runs on the machine you sit
    /// at. It reads the clipboard there, and comes back in `image_went`,
    /// where that holds none. The key held down, or pressed again, while an
    /// image goes is none of the pane's. A new try leaves behind what the last
    /// said in the notes.
    fn key_for_image(&mut self, key: &KeyEvent) -> bool {
        let Some(on) = self.sidebar.far_agent() else {
            return false;
        };
        if desktop::elsewhere() {
            return false;
        }
        if self.image.send(on.clone(), *key, &self.events) {
            self.sidebar.clear_notice();
        }
        true
    }

    /// Takes in what came of the image `images::KEY` was to paste. With no
    /// image on the clipboard the key goes on to zellij, as long as zellij
    /// has the keys and is still on an agent of the machine it was pressed
    /// for: one held up by a clipboard's tool is no key for where you went
    /// since. The path of one kept is pasted where the cursor is now, as long
    /// as that is on the machine that holds the file.
    fn image_went(&mut self, went: Went) {
        self.image.heard();
        self.sidebar.image_going(None);
        match went {
            Went::NoImage { key, on } => {
                if self.focus.zellij() && self.sidebar.far_agent() == Some(&on) {
                    self.key_to_zellij(&key);
                }
            },
            Went::Kept { on, path } => {
                if matches!(self.sidebar.on(), Machine::Remote(there) if *there == on) {
                    self.client.hosted.send(keys::paste(&path, self.client.hosted.mode()));
                    self.typed();
                } else {
                    self.image_not_pasted(&format!("zellij left {on}, where it is at {path}"));
                }
            },
            Went::Failed(why) => self.image_not_pasted(&why),
        }
    }

    /// Says why an image was not pasted, and in the log: in the notes, or in
    /// a box over zellij, where you look, with the sidebar collapsed, which
    /// has no room for them. Under a box that has the keys already, the notes
    /// keep it for the sidebar to open on.
    fn image_not_pasted(&mut self, why: &str) {
        let note = format!("image not pasted: {why}");
        if self.focus.side == Side::Collapsed && self.focus.over.is_none() {
            log::say(&note);
            self.give(Focus { side: Side::Collapsed, over: Some(Over::Note(note)) });
        } else {
            self.sidebar.notify(note);
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        self.pointer = Some(at);
        // A button that went down on zellij before a box opened over it
        // drags there, and lets go there, as it would with no box.
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        if self.focus.over.is_some() && self.held == Held::Zellij && !down {
            if let MouseEventKind::Up(_) = mouse.kind {
                self.held = Held::Nothing;
            }
            self.zellij_mouse(mouse);
            return;
        }
        // A click of a burst on one cell, or next to it, is no click on what
        // its first drew under it: on the row a click on the sidebar
        // collapsed opened there, it would fold a session, or take zellij to
        // a pane; on the entry that took the place of a group a click opened,
        // it would press that entry's key, as a delete's, and the next click
        // the key of the entry that took its place in turn; and on zellij,
        // where a click closed the box over it, it would click there.
        if down {
            if self.burst.as_mut().is_some_and(|burst| burst.takes(at, Instant::now())) {
                self.held = Held::Taken;
                return;
            }
            self.burst = None;
        }
        // A click on a spot of what has the mouse, and the keys, does what the
        // spot says, and nothing else.
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(hit) = self.drawn.at(at)
        {
            self.let_go();
            self.sidebar.let_go();
            self.held = Held::Taken;
            self.burst = Some(Burst::new(at, Instant::now()));
            self.hit(hit);
            return;
        }
        let wheel = match mouse.kind {
            MouseEventKind::ScrollDown => Some(1),
            MouseEventKind::ScrollUp => Some(-1),
            _ => None,
        };
        // The wheel over the skills the menu's search finds moves its cursor.
        if let Some(by) = wheel
            && let Some(Hit::Menu(menu, Choice::Skill(_))) = self.drawn.at(at)
        {
            self.sidebar.scroll_menu(menu, by);
            return;
        }
        // The picker takes the mouse: the wheel moves its highlight, and a
        // click off its box closes it, and does nothing else.
        let off = !self.drawn.boxed().is_some_and(|area| area.contains(at));
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            match (mouse.kind, wheel) {
                (_, Some(by)) => picker.scroll(by),
                (MouseEventKind::Down(_), _) if off => {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                },
                _ => {},
            }
            return;
        }
        // A brief takes the mouse too, and a click, wherever it is, closes
        // one written, and does nothing else; and so do a note, which any
        // click closes, and the menu over zellij, which a click off its box
        // closes. The ask box takes it and does nothing with it: a click
        // would end the talk. So does the dictation box: a click would move
        // what it writes into, or open another box under it.
        match &self.focus.over {
            Some(Over::Ask(_) | Over::Picker(_) | Over::Dictation(..)) => return,
            Some(Over::Brief(briefing)) => {
                if down && briefing.clicked() {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            Some(Over::Menu) => {
                if down && off {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_menu();
                }
                return;
            },
            Some(Over::Note(_)) => {
                if down {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            None => {},
        }
        // Anything but a drag lets the closing button go: its release, or
        // whatever comes after a release luneta never heard, which then does
        // what it does besides.
        if self.held == Held::Taken {
            match mouse.kind {
                MouseEventKind::Drag(_) => return,
                MouseEventKind::Up(_) => {
                    self.held = Held::Nothing;
                    return;
                },
                _ => self.held = Held::Nothing,
            }
        }
        let (sidebar, zellij) = self.areas;
        if matches!(self.held, Held::Edge { .. }) && self.resize(mouse) {
            return;
        }
        let over_zellij = mouse.column >= zellij.x;
        if self.held == Held::Zellij || (over_zellij && !self.sidebar.pressed()) {
            match mouse.kind {
                MouseEventKind::Down(_) => {
                    self.held = Held::Zellij;
                    self.leave();
                },
                MouseEventKind::Up(_) => self.held = Held::Nothing,
                _ => {},
            }
            self.zellij_mouse(mouse);
            return;
        }
        // A click on the sidebar collapsed opens it, as `COLLAPSE` does.
        if self.focus.side == Side::Collapsed {
            if down {
                self.give(Focus::on(Side::Zellij));
                self.burst = Some(Burst::new(at, Instant::now()));
            }
            return;
        }
        // A press on its edge drags it wider or narrower, and leaves the keys
        // where they are.
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && sidebar::edge(sidebar) == Some(mouse.column)
        {
            self.held = Held::Edge { dragged: false };
            return;
        }
        // A press on the sidebar may end in a click that takes zellij's
        // client to a pane.
        if down {
            self.nudge_ahead();
        }
        let asked = self.sidebar.mouse(mouse, sidebar);
        self.follow(asked);
    }

    /// Does what a click on a spot does, where what drew it has the mouse
    /// still: presses its key, the legend's, the search's, the menu's or the
    /// picker's, or picks the skill the menu's search finds, or the picker's
    /// row, as `Enter` does on it. A spot of what closed since it drew, or
    /// gave way to another, does nothing, as what zellij answers a menu closed
    /// does: what was drawn last is what the click was on, but what came
    /// since, with it as luneta drew the next frame, can have changed what it
    /// does.
    fn hit(&mut self, hit: Hit) {
        match hit {
            // The sidebar does nothing with a key of what left the bottom
            // since, or while a box over zellij has the keys.
            Hit::Keys(keyed, key) => {
                let asked = self.sidebar.keys_clicked(keyed, &key, SystemTime::now());
                self.follow(asked);
            },
            Hit::Menu(menu, choice) if matches!(self.focus.over, None | Some(Over::Menu)) => {
                let asked = self.sidebar.chose(menu, choice, SystemTime::now());
                self.follow(asked);
            },
            Hit::Picker(number, pick) => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.click(pick);
                    self.picked(did);
                }
            },
            // A box opened over what drew it since.
            Hit::Menu(..) => {},
        }
    }

    /// Hands zellij what the mouse does, at the cell of zellij's nearest to
    /// it.
    fn zellij_mouse(&mut self, mouse: MouseEvent) {
        let (_, zellij) = self.areas;
        let column = mouse.column.saturating_sub(zellij.x).min(zellij.width.saturating_sub(1));
        let row = mouse.row.saturating_sub(zellij.y);
        if let Some(bytes) = keys::mouse(mouse, column, row, self.client.hosted.mode()) {
            self.client.hosted.send(bytes);
        }
    }

    /// Takes what the mouse does while the sidebar's edge is held, and says
    /// whether that is all it does. A drag puts the edge where the mouse is.
    /// Anything else lets the edge go: a release, or whatever comes after a
    /// release luneta never heard, as of a button let go outside its window,
    /// which then does what it does besides.
    fn resize(&mut self, mouse: MouseEvent) -> bool {
        let (sidebar, zellij) = self.areas;
        if let MouseEventKind::Drag(_) = mouse.kind {
            let terminal = sidebar.width + zellij.width;
            self.width.drag(mouse.column.saturating_sub(sidebar.x), terminal);
            self.held = Held::Edge { dragged: true };
            return true;
        }
        self.let_go();
        matches!(mouse.kind, MouseEventKind::Up(_))
    }

    /// Lets the sidebar's edge go, where it is held, and keeps the width for
    /// the next luneta where it was dragged.
    fn let_go(&mut self) {
        let Held::Edge { dragged } = self.held else {
            return;
        };
        self.held = Held::Nothing;
        if dragged && let Err(why) = self.width.keep() {
            self.sidebar.notify(format!("the sidebar's width stays in this luneta: {why}"));
        }
    }

    /// Does what the sidebar asked. What the menu opens is where you go, with
    /// the keys; a delete leaves you in the sidebar, to go on along its rows,
    /// or, from the menu over zellij, where you were.
    fn follow(&mut self, asked: Asked) {
        match asked {
            Asked::Nothing => {},
            Asked::Take => self.give(Focus::on(Side::Sidebar)),
            Asked::Leave => self.leave(),
            Asked::Go { on, place } => {
                self.go(&on, &place, None);
                self.leave();
            },
            Asked::Closes { menu, on, session, pane } => {
                zellij::closes(menu, &on, session, pane, &self.events);
            },
            Asked::Delete { on, act } => match act {
                Act::Pane { session, pane } => {
                    zellij::close(&on, session, Close::Pane(pane), &self.events);
                },
                Act::Tab { session, tab, panes } => {
                    zellij::close(&on, session, Close::Tab { id: tab, panes }, &self.events);
                },
                Act::Kill { session, started } => {
                    self.deleting.retain(|deleting| !deleting.is_finished());
                    self.deleting.push(zellij::kill(&on, session, started, &self.events));
                },
                // The plan found zellij's client in `session`, on `on`, and on
                // its way nowhere, as the key was pressed: where it goes from.
                Act::Leave { session, started, to } => {
                    self.go(&on, &to, Some(After::Kill { session, started }));
                },
            },
            Asked::Open { menu, on, open } => zellij::open(menu, &on, open, &self.events),
            Asked::OpenThere { menu, on, place, open } => {
                let owed = zellij::owed(menu, &self.events);
                if !self.go(&on, &place, Some(After::Open(owed, open))) {
                    self.sidebar.close_menu();
                }
            },
            Asked::Start { on, cwd } => {
                let unused = match &on {
                    Machine::Local => zellij::unused_for(&cwd),
                    Machine::Remote(destination) => zellij::name_away(destination, &cwd),
                };
                self.make(unused.map_err(|why| format!("menu: {why}")), Some(cwd.as_str()), None);
                self.leave();
            },
            // The menu waits until zellij has opened it, and takes the keys
            // there with it.
            Asked::Make { menu, on, session, cwd, tab_cwd, name, command } => {
                let tab = Tab { cwd: tab_cwd, name: Some(name), command };
                let unused = match &on {
                    Machine::Local => zellij::makes(session),
                    Machine::Remote(destination) => {
                        zellij::makes_away(destination, &session, &cwd)
                    },
                };
                let owed = zellij::owed(menu, &self.events);
                if !self.make(unused, Some(cwd.as_str()), Some((owed, tab))) {
                    self.sidebar.close_menu();
                }
            },
            Asked::Pull { menu, on, work, worked } => {
                pull::prepare(menu, on, work, worked, &self.events);
            },
            Asked::Find { menu, on, cwd } => {
                skills::find(&on, &cwd, &self.events, move |skills| Event::Skills { menu, skills });
            },
            Asked::Printed { menu, on, session, pane, ended } => {
                printed::read(menu, &on, &session, pane, ended, &self.events);
            },
            Asked::Brief(brief) => {
                self.briefs += 1;
                let folder = self.claude_folder.clone();
                let briefing = brief::ask(self.briefs, brief, folder, &self.events);
                self.give(Focus { side: self.focus.side, over: Some(Over::Brief(briefing)) });
            },
            Asked::Ask(target) => {
                self.asks += 1;
                let ask = Ask::new(self.asks, &target, &self.dir);
                self.give(Focus { side: self.focus.side, over: Some(Over::Ask(ask)) });
            },
            // lunetad reads the file again as it changes, and its next answer
            // has the machine, or has it no more.
            Asked::Ssh(destination) => match machines::add(&destination) {
                Ok(Added::Named) => {},
                Ok(Added::Already) => {
                    let named = format!("ssh: luneta's hosts file names {destination} already");
                    self.sidebar.notify(named);
                },
                Err(why) => self.sidebar.notify(format!("ssh: {why}")),
            },
            Asked::Forget(destination) => {
                if let Err(why) = machines::forget(&destination) {
                    self.sidebar.notify(format!("forget {destination}: {why}"));
                }
            },
            Asked::Review { on, pid, does } => review::moved(on, pid, does, &self.events),
        }
    }

    /// Does what the picker asked. Where it takes zellij somewhere, it
    /// closes, and zellij has the keys; a delete leaves it open, but for one
    /// of the session luneta's zellij is in, which takes zellij to another.
    fn picked(&mut self, picked: Picked) {
        match picked {
            Picked::Nothing => {},
            Picked::Close => self.give(Focus::on(self.focus.side)),
            Picked::Go { on, place } => {
                self.go(&on, &place, None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Start { on, session } => {
                // From another machine, a client that makes it refuses a name
                // zellij keeps, which `makes` says first, as the daemon of
                // the machine it makes it on says there.
                let unused = match &on {
                    Machine::Local => zellij::makes(session),
                    Machine::Remote(destination) => zellij::makes_there(destination, session),
                };
                self.make(unused, None, None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::BringBack { on, session } => {
                self.bring_back(&on, &session);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Make { on, session, cwd } => {
                let unused = match &on {
                    Machine::Local => zellij::makes(session),
                    Machine::Remote(destination) => {
                        zellij::makes_away(destination, &session, cwd.as_str())
                    },
                };
                self.make(unused, Some(cwd.as_str()), None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Plan { on, session, deletes } => {
                let plan = self.sidebar.plan(&on, &session, deletes);
                if let Some(Over::Picker(picker)) = &mut self.focus.over {
                    let did = picker.planned(plan);
                    self.picked(did);
                }
            },
            Picked::Delete { on, act: Act::Leave { session, started, to } } => {
                self.go(&on, &to, Some(After::Kill { session, started }));
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Delete { on, act } => self.follow(Asked::Delete { on, act }),
        }
    }

    /// Hands what you typed in the ask box to its agent, for you where you
    /// are: in the pane luneta's zellij is on, where that is on the agent's
    /// machine, or on another. luneta's zellij types in its pane first, as
    /// the last client to type in its session, which zellij moves where the
    /// agent has it take you somewhere.
    fn converse(&mut self) {
        let Some(Over::Ask(ask)) = &self.focus.over else {
            return;
        };
        let user = self.sidebar.user(&ask.machine());
        // Whether it typed there before or not: another client may have
        // since, and the agent acts well after the nudge.
        if matches!(user, User::Pane { .. })
            && let Ok(seat) = self.seat()
        {
            self.client.nudge_in(seat.to_string());
        }
        if let Some(Over::Ask(ask)) = &mut self.focus.over {
            ask.send(user, &self.events);
        }
    }

    /// Opens the dictation box, for what you say to be written where the keys
    /// are, if that is somewhere it can be: in the pane zellij is on, while
    /// zellij has them under no box, or in the ask box, but while its agent
    /// is at a turn. The first time, the box says how the fetch of the model
    /// goes instead, which it starts. Whether it opened. The box takes the
    /// place of what it is over, which waits in it until it closes.
    fn dictate(&mut self) -> bool {
        let (zellij, had) = (self.focus.zellij(), self.focus.zellij_told());
        let under = match self.focus.over.take() {
            None if zellij => Where::Zellij(self.sidebar.here_agent()),
            Some(Over::Ask(ask)) if !ask.turning() => Where::Ask(ask),
            other => {
                self.focus.over = other;
                return false;
            },
        };
        self.dictations += 1;
        let number = self.dictations;
        let dictation = if speech::fetched() {
            let skills = self.nameable(number, &under);
            let speech = self.speech.get_or_insert_with(Speech::start);
            let listening = speech.listen(number, &self.events);
            Dictation::hearing(number, listening, skills, self.releases, Instant::now())
        } else {
            let events = &self.events;
            let fetching = || Fetch { _asking: speech::fetch(events), got: 0 };
            Dictation::fetching(number, self.fetching.get_or_insert_with(fetching).got)
        };
        let over = Some(Over::Dictation(dictation, under));
        self.give_from(had, Focus { side: self.focus.side, over });
        true
    }

    /// The skills that the dictation numbered `number`, into `under`, can name,
    /// which the finder is asked for: those of the directory of the Claude
    /// Code agent in the pane zellij is on. None elsewhere: in a shell, "slash"
    /// is more likely part of a path, and the ask box's agent runs in safe
    /// mode, where none of your skills are.
    fn nameable(&self, number: u64, under: &Where) -> Skills {
        let Where::Zellij(Some(agent)) = under else {
            return Skills::Found(Vec::new());
        };
        let said = move |skills| Event::Nameable { number, skills };
        skills::find(&agent.on, &agent.dir, &self.events, said);
        Skills::Finding
    }

    /// Closes the dictation box, which gives up the dictation it is at, if it
    /// is still at one, and gives the keys back to what it was over. What you
    /// `dictated`, if anything, is written there as it closes: as [`pastes`]
    /// in the pane zellij is on, or on the ask box's line. Then it is sent, as
    /// `Enter` would send it, where you ended the dictation with `Enter`, which
    /// zellij keeps for itself outside its normal mode. No command is written
    /// where the agent it was named to has gone from the pane, as far as luneta
    /// has followed its client: zellij's pane takes any bytes, so no write can
    /// be surer than that.
    fn undictate(&mut self, dictated: Option<Dictated>) {
        let had = self.focus.zellij_told();
        let open = self.focus.over.take_if(|over| matches!(over, Over::Dictation(..)));
        let Some(Over::Dictation(_, under)) = open else {
            return;
        };
        let side = self.focus.side;
        match under {
            Where::Zellij(agent) => {
                self.give_from(had, Focus::on(side));
                let Some(dictated) = dictated else {
                    return;
                };
                let here = self.sidebar.here_agent() == agent;
                let dictated = if here { dictated } else { dictated.moved() };
                let Dictated { written, sends, .. } = dictated;
                let mode = self.client.hosted.mode();
                for paste in pastes(&written, mode) {
                    self.client.hosted.send(paste);
                }
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                if sends && let Some(bytes) = keys::key(&enter, mode) {
                    self.client.hosted.send(bytes);
                }
                self.typed();
            },
            Where::Ask(mut ask) => {
                if let Some(dictated) = &dictated {
                    ask.dictated(&dictated.written.to_string());
                }
                self.give_from(had, Focus { side, over: Some(Over::Ask(ask)) });
                if dictated.is_some_and(|dictated| dictated.sends) {
                    self.converse();
                }
            },
        }
    }

    /// Takes in how the fetch of the speech model goes, for the dictation
    /// box, if it is open: once it ended, the next `Alt r` dictates, or
    /// fetches it again. What went wrong with a fetch the box no longer
    /// shows, the sidebar says.
    fn fetched(&mut self, fetched: &Fetched) {
        match fetched {
            Fetched::Got(got) => {
                if let Some(fetch) = &mut self.fetching {
                    fetch.got = *got;
                }
            },
            Fetched::Ended(ended) => {
                self.fetching = None;
                let shown = matches!(self.focus.over, Some(Over::Dictation(..)));
                if let (Err(why), false) = (ended, shown) {
                    self.sidebar.notify(format!("the speech model did not come: {why}"));
                }
            },
        }
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            dictation.fetched(fetched);
        }
    }

    /// Takes zellij's client to the pane the sidebar lists after the one it
    /// is on, of the machine it runs on. The sidebar gives the keys up first,
    /// which closes a search: the pane after is the one after in every row,
    /// not in the rows a search leaves, which pass over panes and open folded
    /// sessions.
    fn next(&mut self) {
        self.leave();
        if let Some(place) = self.sidebar.next_pane() {
            let machine = self.sidebar.on().clone();
            self.go(&machine, &place, None);
        }
    }

    /// Takes zellij's client to `place` on `machine`, and does `after` once it
    /// is there, in another session. On another machine than the one the
    /// client runs on, a client there takes its place, as `swap` says, and
    /// opens what `after` opens once it is in. Whether it went.
    fn go(&mut self, machine: &Machine, place: &Place, after: Option<After>) -> bool {
        if machine != self.sidebar.on() {
            // The session a kill is of is the one the client leaves, of the
            // machine it leaves, which the sidebar asks for only as the
            // client goes on in a session of that machine.
            if let Some(after @ After::Kill { .. }) = after {
                after.untold(&self.events);
                return false;
            }
            return self.swap(machine, place, after);
        }
        let Some(nudged) = self.nudge() else {
            return false;
        };
        let then = match &after {
            // The session it leaves ends once it has left it, which the
            // sidebar says until then.
            Some(After::Kill { .. }) => self.sidebar.leaving(place),
            Some(After::Open(..)) | None => self.sidebar.went(place),
        };
        zellij::go(&nudged, place.listed().name(), place.pane(), then, after, &self.events);
        true
    }

    /// Takes zellij's client to `place` on `machine`, another than the one it
    /// runs on: a client of that session there takes its place, through ssh
    /// on another machine, or zellij's own here, as `client_ended` has one
    /// take the place of a client that ended, and goes on to the pane once it
    /// is in, where `place` is one, or does `after` there, which opens what
    /// it goes to. Where the client fails, as where ssh does not reach the
    /// machine, or zellij cannot attach, luneta's zellij goes back to the
    /// session it leaves. Whether it went.
    fn swap(&mut self, machine: &Machine, place: &Place, after: Option<After>) -> bool {
        let session = place.listed().name().clone();
        let fails = self.back();
        let before = match machine {
            Machine::Local => zellij::before(session.as_str()),
            Machine::Remote(destination) => zellij::before_away(destination, session.as_str()),
        };
        // The client goes on to what `after` opens, not to the pane it opens
        // it from.
        let pane = || place.pane().map(|_| Next::Pane(place.clone()));
        let next = after.map(Next::After).or_else(pane);
        let joins = Joins::Attach(&session);
        match self.restart(machine, joins, before, fails, next) {
            Ok(()) => true,
            Err(error) => {
                self.sidebar.notify(format!("no client of {session} started: {error}"));
                false
            },
        }
    }

    /// Takes zellij's client to `session` on `machine`, which ended, as zellij
    /// brings it back: a client of it there takes its place, as `swap` says,
    /// on the machine the client runs on as well, which `attach` brings it
    /// back for. `swap_making`'s would refuse a name zellij keeps, and
    /// `switch-session` would make one zellij let go of since, anew, empty:
    /// `attach` fails then, which takes luneta's zellij back. Its server is
    /// new, as `Before::Made` says. Whether it went.
    fn bring_back(&mut self, machine: &Machine, session: &SessionName) -> bool {
        let fails = self.back();
        match self.restart(machine, Joins::Attach(session), Before::Made, fails, None) {
            Ok(()) => true,
            Err(error) => {
                self.sidebar.notify(format!("no client of {session} started: {error}"));
                false
            },
        }
    }

    /// Takes zellij's client to a session it makes, `unused`, on its machine,
    /// another than this one or than the one it runs on, as `make` has it,
    /// that starts in `cwd`, or where luneta started: a client that makes it
    /// there takes its place, as `swap` says, and fails where the name is
    /// taken by then, as `Joins::Make` says, which takes luneta's zellij
    /// back. Once it is in, the tab of `alone` opens there, if any, as
    /// `Alone` says. Whether it went.
    fn swap_making(&mut self, unused: &Unused, cwd: Option<&str>, alone: Option<Alone>) -> bool {
        let fails = self.back();
        let joins = Joins::Make(unused, cwd);
        let next = alone.map(|alone| Next::After(alone.after()));
        match self.restart(unused.on(), joins, Before::Made, fails, next) {
            Ok(()) => true,
            Err(error) => {
                let session = unused.name();
                self.sidebar.notify(format!("no client of {session} started: {error}"));
                false
            },
        }
    }

    /// Where luneta's zellij goes where a client that takes its place on
    /// another machine fails: back to the session it is in, of the machine it
    /// is on now; or nowhere, where luneta does not know which that is.
    fn back(&self) -> Fails {
        let here = self.sidebar.here().and_then(|here| SessionName::running(here).ok());
        let back = here.map(|session| Back { machine: self.sidebar.on().clone(), session });
        back.map_or(Fails::End, Fails::Back)
    }

    /// Starts a client on `machine` that `joins` its session, in the place
    /// of zellij's client, which ends as it goes, if it has not: one that
    /// ended, or that luneta takes elsewhere. `before` is what luneta knows of
    /// the session's clients before it comes, `fails` where luneta's zellij
    /// goes where it fails, and `next` where it goes on to once it is in. The
    /// sidebar has luneta's zellij on `machine` from now on. One that
    /// makes its session is in it once zellij titles luneta's terminal there,
    /// not once it shows anything: one that fails, as where the name is
    /// taken by then, writes why, and a client of the session that took the
    /// name is anyone's but luneta's.
    fn restart(
        &mut self,
        machine: &Machine,
        joins: Joins<'_>,
        before: Before,
        fails: Fails,
        next: Option<Next>,
    ) -> io::Result<()> {
        let (then_pane, after) = match next {
            Some(Next::Pane(place)) => (Some(place), None),
            Some(Next::After(after)) => (None, Some(after)),
            None => (None, None),
        };
        let hosted = self.start_of(machine, joins)?;
        let look = self.sidebar.swapping(machine, joins.session());
        let arrival = Arrival::new(machine.clone(), look, before, after);
        match joins {
            Joins::Attach(_) => self.put(hosted, fails, Some(arrival), then_pane),
            Joins::Make(..) => {
                self.put(hosted, fails, None, then_pane);
                let until = Instant::now() + zellij::START;
                self.client.making = Some(Making { arrival, until });
            },
        }
        Ok(())
    }

    /// Starts the program of a client on `machine` that `joins` its
    /// session, the next that luneta starts: zellij's client here, or there
    /// through ssh.
    fn start_of(&mut self, machine: &Machine, joins: Joins<'_>) -> io::Result<Hosted> {
        let ((program, args), dir) = match (machine, joins) {
            (Machine::Local, Joins::Attach(session)) => (zellij::rejoin(session.as_str()), None),
            // zellij's client makes a session in the directory it starts in.
            (Machine::Local, Joins::Make(unused, cwd)) => (zellij::make(unused.name()), cwd),
            (Machine::Remote(destination), Joins::Attach(session)) => {
                (zellij::attach_away(destination, session.as_str()), None)
            },
            (Machine::Remote(destination), Joins::Make(unused, cwd)) => {
                (zellij::make_away(destination, unused.name().as_str(), cwd), None)
            },
        };
        let dir = dir.map_or(self.dir.clone(), PathBuf::from);
        let (number, size) = (self.started + 1, window(self.areas.1));
        let hosted = Hosted::start(program, args, &dir, size, number, self.events.clone())?;
        self.started = number;
        Ok(hosted)
    }

    /// Has `hosted` take the place of zellij's client, on its `arrival`, if
    /// luneta looks for it as zellij first shows anything, going where it
    /// `fails` where it does, and on to `then_pane` once it is in.
    fn put(
        &mut self,
        hosted: Hosted,
        fails: Fails,
        arrival: Option<Arrival>,
        then_pane: Option<Place>,
    ) {
        self.client = Client::new(hosted, fails, arrival, then_pane);
        // The client that takes its place heard no button go down over it.
        if self.held == Held::Zellij {
            self.held = Held::Nothing;
        }
    }

    /// Takes zellij's client on to the pane luneta started it to go to, once
    /// it is in that pane's session, as far as luneta knows.
    fn on_to_pane(&mut self) {
        let Some(place) = &self.client.then_pane else {
            return;
        };
        let seat = self.seat().ok();
        if seat.as_ref().map(SessionName::as_str) != Some(place.session())
            || self.sidebar.going().is_some()
        {
            return;
        }
        if let Some(place) = self.client.then_pane.take() {
            let machine = self.sidebar.on().clone();
            self.go(&machine, &place, None);
        }
    }

    /// Takes zellij's client to a session it makes on this machine, which it
    /// runs on, as `make` has it, `session`, that starts in `cwd`, or where
    /// the client started, or brings back, where zellij kept one that ended
    /// since luneta found the name unused, and opens the tab of `alone` there
    /// once it is, if any, as `Alone` says. Where zellij makes it, its server
    /// cannot bear being asked anything before it takes the client: luneta
    /// asks for the client's arrival there once zellij titles its terminal in
    /// it. Whether it went.
    fn start(&mut self, session: &SessionName, cwd: Option<&str>, alone: Option<Alone>) -> bool {
        let Some(nudged) = self.nudge() else {
            return false;
        };
        let then = self.sidebar.sending(session);
        let made = zellij::start(&nudged, session, cwd, then, alone, &self.events);
        let until = Instant::now() + zellij::START;
        self.client.making = made.map(|arrival| Making { arrival, until });
        true
    }

    /// Takes zellij's client to a session zellij makes on the machine of
    /// `unused`, by its name, that starts in `cwd`, or where the client
    /// started, and opens `tab` there once it is, if any, for the menu owed
    /// what it opens, in place of the tabs zellij makes it with, as `Alone`
    /// says, as `start` does where the client runs on this machine and the
    /// session is of it, or `swap_making` does anywhere else; or says why
    /// zellij would make none, but take it to one that has the name, before
    /// the sidebar takes the client for one on its way there. Whether it
    /// went.
    fn make(
        &mut self,
        unused: Result<Unused, String>,
        cwd: Option<&str>,
        tab: Option<(Due<u32>, Tab)>,
    ) -> bool {
        match unused {
            Ok(unused) => {
                let alone = tab.map(|(owed, tab)| Alone::new(owed, tab, &unused));
                // `switch-session` takes the client to whatever has the name
                // by the time zellij takes it, and luneta looks just before
                // only here, as `zellij::start` says: elsewhere, a client
                // that makes the session is refused a name taken since.
                match (unused.on(), self.sidebar.on()) {
                    (Machine::Local, Machine::Local) => self.start(unused.name(), cwd, alone),
                    _ => self.swap_making(&unused, cwd, alone),
                }
            },
            Err(why) => {
                self.sidebar.notify(why);
                false
            },
        }
    }

    /// The session zellij's client is in, where a key it types counts and an
    /// action can take it from; or why it is in none, as far as luneta knows:
    /// zellij has yet to have it in one, or to say which, or it is on its way
    /// to another, which the key or the action may reach first.
    fn seat(&self) -> Result<SessionName, String> {
        if self.client.coming.is_some() {
            return Err("zellij has yet to come to its session".to_owned());
        }
        let Some(here) = self.sidebar.here() else {
            return Err("zellij has yet to say which session it is in".to_owned());
        };
        // An action would move the client that last typed in a session that
        // luneta's has yet to come to.
        if let Some(going) = self.sidebar.going() {
            return Err(format!("zellij has yet to take its client to {going}"));
        }
        SessionName::running(here).map_err(|why| format!("zellij is in \"{here}\": {why}"))
    }

    /// Types `zellij::NUDGE` into zellij's client, for an action asked of
    /// zellij in the session it is in to move it, or says why it is in none.
    fn nudge(&mut self) -> Option<Nudged> {
        match self.seat() {
            Ok(here) => Some(self.client.nudge(self.sidebar.on().clone(), here)),
            Err(why) => {
                self.sidebar.notify(why);
                None
            },
        }
    }

    /// Types `zellij::NUDGE` into zellij's client as the keys or a press of
    /// the mouse come to luneta, ahead of the action they may ask for, which
    /// then waits for zellij to take it in for less, if at all. A client that
    /// typed in the session since it came gets none: zellij moves it already,
    /// but where another typed there since, which the nudge before the action
    /// takes the turn back from, and it may be renaming a tab or a pane,
    /// whose name zellij would write the nudge into.
    fn nudge_ahead(&mut self) {
        if let Ok(here) = self.seat()
            && self.client.typed_at(here.as_str()).is_none()
        {
            self.client.nudge_in(here.to_string());
        }
    }

    /// zellij's client typed a key for you just now.
    fn typed(&mut self) {
        if let Ok(here) = self.seat() {
            self.client.typed_in(here.to_string());
        }
    }

    /// Asks zellij for the clients of a session, if the sidebar wants to know.
    fn look(&self, look: Option<Look>) {
        if let Some(look) = look {
            zellij::look(self.sidebar.on(), look, &self.events);
        }
    }

    /// Asks zellij for the clients of the session its client came to, as
    /// `look` is for, if it came to one. What the client typed before, it
    /// typed elsewhere: zellij forgets the keys of a client that leaves, of
    /// one that comes back too.
    fn arrived(&mut self, look: Option<Look>) {
        if look.is_some() {
            self.client.typed = None;
        }
        self.look(look);
    }

    /// Hands the keys to `focus`, and tells zellij when it takes them or
    /// loses them, as a terminal tells a program in it of the window's focus,
    /// and the sidebar where they are.
    fn give(&mut self, focus: Focus) {
        let had = self.focus.zellij_told();
        self.give_from(had, focus);
    }

    /// Hands the keys to `focus`, as `give` does, from where zellij was told
    /// it `had` them, or not: for a box that took the place of what it is
    /// over, or gives it back, as the dictation box does, which leaves where
    /// they were no longer there to tell.
    fn give_from(&mut self, had: bool, focus: Focus) {
        if focus.side == Side::Collapsed || focus.over.is_some() {
            // The strip has no edge to drag nor rows to hold, and a box takes
            // the mouse: what the mouse holds of either is let go, as a
            // release would let it go. A press held on the rows would keep the
            // mouse from zellij, and the keys from the box, and the sidebar
            // never hears the release.
            self.let_go();
            self.sidebar.let_go();
        }
        let has = focus.zellij_told();
        self.sidebar.focus(match (&focus.over, focus.side) {
            (None, Side::Sidebar) => Keys::Sidebar,
            (Some(Over::Menu), side) => Keys::Menu { lent: side == Side::Sidebar },
            // A brief, the ask box, a note, the picker or the dictation box
            // leaves the sidebar as it stands, highlight and all, for the keys
            // to go back to as it closes.
            (
                Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Note(_)
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                Side::Sidebar,
            ) => Keys::Lent,
            (
                None
                | Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Note(_)
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                Side::Zellij | Side::Collapsed,
            ) => Keys::Zellij,
        });
        self.focus = focus;
        if self.focused
            && has != had
            && let Some(bytes) = keys::focus(has, self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
        // What has the keys now may take zellij's client to a pane.
        if had && !has {
            self.nudge_ahead();
        }
    }

    /// Hands zellij the keys, where the sidebar or the menu over zellij has
    /// them, beside the sidebar as it is.
    fn leave(&mut self) {
        let leaves = match &self.focus.over {
            None => self.focus.side == Side::Sidebar,
            Some(Over::Menu) => true,
            Some(
                Over::Brief(_)
                | Over::Ask(_)
                | Over::Note(_)
                | Over::Picker(_)
                | Over::Dictation(..),
            ) => false,
        };
        if leaves {
            self.give(Focus::on(self.focus.side.zellij()));
        }
    }

    fn focus_window(&mut self, focused: bool) {
        self.focused = focused;
        self.desktop.focus(focused);
        if !focused && let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            dictation.unfocused();
        }
        // The pointer has gone elsewhere, as far as luneta knows, and lights
        // nothing until it moves over luneta again.
        if !focused {
            self.pointer = None;
        }
        if self.focus.zellij_told()
            && let Some(bytes) = keys::focus(focused, self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
    }

    fn hosted_said(&mut self, said: Said, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match said {
            // The frame this event goes into draws it.
            Said::Drew => {},
            Said::Titled(title) => {
                execute!(terminal.backend_mut(), SetTitle(&title))?;
                if !title.is_empty() {
                    let there = |making: &mut Making| {
                        sidebar::in_session(&title, &making.arrival.look.session)
                    };
                    if let Some(making) = self.client.making.take_if(there) {
                        zellij::arrive(making.arrival, &self.events);
                    }
                    let look = self.sidebar.titled(title);
                    self.arrived(look);
                    let coming = self.client.coming.take();
                    self.came_in(coming);
                }
            },
            Said::Copied(clipboard, text) => {
                let which = match clipboard {
                    ClipboardType::Clipboard => 'c',
                    ClipboardType::Selection => 'p',
                };
                let text = base64::engine::general_purpose::STANDARD.encode(text);
                let backend = terminal.backend_mut();
                write!(backend, "\x1b]52;{which};{text}\x07")?;
                backend.flush()?;
            },
            Said::Answer(answer) => self.client.hosted.answer(answer.into_bytes()),
            Said::AskedSize(answer) => {
                self.client.hosted.answer(answer(self.client.hosted.size().into()).into_bytes());
            },
            // A colour luneta's terminal did not say, or one of the rest, as
            // the sixteen and the cursor's, goes without an answer.
            Said::AskedColour(index, spell) => {
                if let Some(answer) = self.colours.answer(index, &*spell) {
                    self.client.hosted.answer(answer.into_bytes());
                }
            },
            Said::Rang => {
                let backend = terminal.backend_mut();
                backend.write_all(b"\x07")?;
                backend.flush()?;
            },
            Said::Exited(status) => self.client.exited = Some(status),
            Said::Ended => self.client_ended()?,
        }
        Ok(())
    }

    /// zellij's client ended. Where it ended well, and its session with it, as
    /// zellij quits, the session's last pane exits, or the session is killed,
    /// a client of the newest session that answers takes its place, and
    /// luneta goes on: of another machine's, as `newest_away` finds it, where
    /// none of this one's does. luneta ends where none does, and where the
    /// client detached, or failed, as where zellij cannot attach; but one
    /// that took the place of a client on another machine goes back there
    /// where it failed, and the notes say what it said. A client on another
    /// machine ends as `ended_away` says.
    fn client_ended(&mut self) -> io::Result<()> {
        if let Machine::Remote(destination) = self.sidebar.on() {
            let destination = destination.clone();
            return self.ended_away(&destination);
        }
        if self.client.exited.is_none_or(|status| !status.success()) {
            // A client here goes back where it fails, if anywhere: one of a
            // session luneta went on in here ends luneta, as `Fails::Onward`
            // is of another machine's.
            return match self.client.fails.clone().failed(&Machine::Local) {
                Failed::Back(back) => {
                    self.failed(&Machine::Local);
                    self.go_back(&Machine::Local, back)
                },
                Failed::End | Failed::Onward(_) => {
                    self.ended = true;
                    Ok(())
                },
            };
        }
        // With none, it is in no session that could have gone with it.
        let seats = self.sidebar.seats();
        let refuge = match (!seats.is_empty()).then(|| zellij::ended(&seats)) {
            Some(Ended::Went(refuge)) => refuge,
            Some(Ended::Kept) | None => {
                self.ended = true;
                return Ok(());
            },
        };
        let Some(Refuge { session: to, before }) = refuge else {
            return self.newest_away(&[]);
        };
        let hosted = self.start_of(&Machine::Local, Joins::Attach(&to))?;
        let look = self.sidebar.rejoining(&to);
        let arrival = Arrival::new(Machine::Local, look, Before::Listed(before), None);
        self.put(hosted, Fails::End, Some(arrival), None);
        Ok(())
    }

    /// zellij's client on `destination` ended, or the ssh it ran through did.
    /// Where it ended well, as its session there ended, a client of the
    /// newest session there that answers takes its place, or else of this
    /// machine's newest, where none there does, or its daemon does not say,
    /// or of another machine's, as `newest_here` says.
    /// Where it failed, as where ssh lost the machine, or zellij there could
    /// not attach, luneta's zellij goes where the client `fails`, and the
    /// notes say what was said: back to the session it was in before, or on
    /// to the newest session of a machine it has yet to try. luneta ends
    /// where the client detached, and where it failed with nowhere to go.
    fn ended_away(&mut self, destination: &Destination) -> io::Result<()> {
        let mut fails = self.client.fails.clone();
        let on = Machine::Remote(destination.clone());
        if self.client.exited.is_none_or(|status| !status.success()) {
            return match fails.failed(&on) {
                Failed::End => {
                    self.ended = true;
                    Ok(())
                },
                Failed::Back(back) => {
                    self.failed(&on);
                    self.go_back(&on, back)
                },
                Failed::Onward(tried) => {
                    self.failed(&on);
                    self.newest_here(&tried)
                },
            };
        }
        // With none, it is in no session that could have gone with it, as on
        // this machine.
        let seats = self.sidebar.seats();
        if seats.is_empty() {
            self.ended = true;
            return Ok(());
        }
        let refuge = match zellij::ended_away(destination, &seats) {
            Ok(Ended::Kept) => {
                self.ended = true;
                return Ok(());
            },
            Ok(Ended::Went(refuge)) => refuge,
            Err(why) => {
                self.sidebar.notify(why);
                None
            },
        };
        // It was in the session that ended, whether or not zellij listed it
        // there yet.
        fails.came_in();
        match refuge {
            Some(Refuge { session, before }) => {
                let joins = Joins::Attach(&session);
                self.restart(&on, joins, Before::Listed(before), fails, None)
            },
            None => self.newest_here(std::slice::from_ref(destination)),
        }
    }

    /// zellij's client on `on`, which took the place of one on another
    /// machine, or of one whose session ended, failed: the notes say what it
    /// said, and how it ended, and so does what waits on its arrival.
    fn failed(&mut self, on: &Machine) {
        let exited = self.client.exited;
        let status = exited.map_or_else(|| "its end unread".to_owned(), |it| it.to_string());
        let said = failed_saying(&self.client.hosted, exited, on);
        let note = match on {
            Machine::Local => format!("{said} ({status})"),
            Machine::Remote(destination) => format!("{destination}: {said} ({status})"),
        };
        // What waits on its arrival, as a menu waits on a session made for
        // it, hears why it did not come, not that it ended with no answer.
        let coming = self.client.coming.take().and_then(Coming::arrival);
        let making = self.client.making.take().map(|making| making.arrival);
        for arrival in coming.into_iter().chain(making) {
            arrival.failed(note.clone(), &self.events);
        }
        self.sidebar.notify(note);
    }

    /// Has a client of `back` take the place of zellij's client, which failed
    /// on `on`, another machine: of this machine's newest session where it
    /// was of this machine's and is gone, or of another machine's but `on`,
    /// as `newest_here` says.
    fn go_back(&mut self, on: &Machine, back: Back) -> io::Result<()> {
        let Back { machine, session } = back;
        let before = match &machine {
            Machine::Local => match zellij::before(session.as_str()) {
                Before::Made => {
                    let tried = match on {
                        Machine::Remote(destination) => vec![destination.clone()],
                        Machine::Local => Vec::new(),
                    };
                    return self.newest_here(&tried);
                },
                before => before,
            },
            Machine::Remote(destination) => zellij::before_away(destination, session.as_str()),
        };
        self.restart(&machine, Joins::Attach(&session), before, Fails::End, None)
    }

    /// Has a client of this machine's newest session that answers take the
    /// place of zellij's client, as one of another machine's that ended: or
    /// of another machine's but those `tried`, as `newest_away` says, where
    /// none does.
    fn newest_here(&mut self, tried: &[Destination]) -> io::Result<()> {
        let Some(Refuge { session, before }) = zellij::newest() else {
            return self.newest_away(tried);
        };
        let joins = Joins::Attach(&session);
        self.restart(&Machine::Local, joins, Before::Listed(before), Fails::End, None)
    }

    /// Has a client of the newest session that answers of another machine's
    /// take the place of zellij's client, as the machine it was on has no
    /// session left: of the first machine lunetad reaches, by the sidebar's
    /// headers, but those `tried`, as `Fails::Onward` says, and never one
    /// that may hold luneta's own terminal, as `Sidebar::refuges_away` says.
    /// Where that client fails, luneta's zellij goes on to the next. luneta
    /// ends where no machine has one, and where it does not reach lunetad to
    /// ask; the log says why it passed each machine over.
    fn newest_away(&mut self, tried: &[Destination]) -> io::Result<()> {
        let Sought::Found(destination, Refuge { session, before }) =
            starts::away(&self.sidebar, tried, away::QUICK)
        else {
            self.ended = true;
            return Ok(());
        };
        let (machine, joins) = (Machine::Remote(destination), Joins::Attach(&session));
        let fails = Fails::Onward(tried.to_vec());
        self.restart(&machine, joins, Before::Listed(before), fails, None)
    }

    /// Takes in what luneta's terminal answered, or said of itself unasked.
    fn answered(&mut self, answer: Answer, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match answer {
            Answer::Foreground(_) | Answer::Background(_) => self.colours.hear(answer),
            // It turned dark or light: its colours are asked for again, and
            // zellij is told once they are in, so that a program zellij tells
            // that asks for them hears the new ones.
            Answer::Turned(theme) => {
                self.turned = Some(theme);
                let backend = terminal.backend_mut();
                backend.write_all(answers::TURNED)?;
                backend.flush()?;
            },
            // What it is, the last of what it was asked: the colours it turned
            // to, if it turned, are in.
            Answer::Device => {
                if let Some(theme) = self.turned.take() {
                    self.client.hosted.send(theme.report());
                }
            },
            // Too late to count, as luneta started.
            Answer::Kitty => {},
        }
        Ok(())
    }

    /// What zellij said as it failed, to be said again once luneta's screen is
    /// gone: the text on its terminal, and how it ended, and where luneta took
    /// it to a session by itself, with none left to go on in, how to start in
    /// one all the same: a bare luneta would look for the same again. None for
    /// a zellij that detached or quit, or that ended in a way its terminal
    /// could not tell.
    fn failure(&self) -> Option<String> {
        let status = self.client.exited.filter(|status| !status.success())?;
        let ended = match self.sidebar.on() {
            Machine::Local => format!("luneta: zellij ended ({status})"),
            Machine::Remote(destination) => {
                format!("luneta: zellij on {destination} ended ({status})")
            },
        };
        let ended = match self.client.fails {
            Fails::Onward(_) => format!(
                "{ended}\nluneta: name a session to start there, made if none runs: luneta <name>"
            ),
            Fails::End | Fails::Back(_) => ended,
        };
        let said = self.client.hosted.text();
        Some(if said.is_empty() { ended } else { format!("{said}\n{ended}") })
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        whole(terminal, |terminal| self.frame(terminal))
    }

    /// Draws the sidebar, open or collapsed, zellij's screen beside it, and
    /// the box over zellij, where one is, into a frame of luneta's terminal,
    /// with zellij's cursor while zellij has the keys.
    fn frame(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let now = SystemTime::now();
        terminal.draw(|frame| {
            let collapsed = self.focus.side == Side::Collapsed;
            let own = if collapsed { sidebar::COLLAPSED } else { self.width.columns() };
            let (sidebar, zellij) = areas(frame.area(), own);
            self.areas = (sidebar, zellij);
            let size = self.client.hosted.size();
            if (zellij.width, zellij.height) != (size.columns.get(), size.lines.get()) {
                self.client.hosted.resize(window(zellij));
            }
            let buffer = frame.buffer_mut();
            let beside = if collapsed {
                self.sidebar.draw_collapsed(buffer, sidebar, now);
                Drawn::Nothing
            } else {
                self.sidebar.draw(buffer, sidebar, now)
            };
            let cursor = self.client.hosted.draw(buffer, zellij);
            // What has the mouse is the box over zellij, where one is, or else
            // the sidebar.
            let drawn = match &self.focus.over {
                Some(Over::Brief(briefing)) => {
                    briefing.draw(buffer, zellij, now);
                    Drawn::Nothing
                },
                Some(Over::Ask(ask)) => {
                    ask.draw(buffer, zellij, now);
                    Drawn::Nothing
                },
                Some(Over::Picker(picker)) => picker.draw(buffer, zellij, now),
                Some(Over::Dictation(dictation, under)) => {
                    if let Where::Ask(ask) = under {
                        ask.draw(buffer, zellij, now);
                    }
                    dictation.draw(buffer, zellij, (now, Instant::now()));
                    Drawn::Nothing
                },
                Some(Over::Note(note)) => {
                    menu::draw_note(buffer, zellij, note);
                    Drawn::Nothing
                },
                Some(Over::Menu) => {
                    let floating = self.sidebar.floating();
                    floating.map(|menu| menu.draw(buffer, zellij, now)).unwrap_or_default()
                },
                None => beside,
            };
            if let Some(pointer) = self.pointer {
                drawn.light(buffer, pointer);
            }
            // zellij's cursor shows while zellij has the keys, under no box.
            if self.focus.zellij()
                && let Some(cursor) = cursor
            {
                frame.set_cursor_position(cursor);
            }
            self.drawn = drawn;
        })?;
        let cursor = self.client.hosted.cursor_style();
        if self.cursor != Some(cursor) {
            self.cursor = Some(cursor);
            execute!(terminal.backend_mut(), cursor_style(cursor))?;
        }
        Ok(())
    }
}

/// Writes a frame to `terminal`, as `draw` does, for the terminal to show
/// whole: one that can holds what it shows from the frame's first write to
/// its last. A frame goes out in more than one write, and its cursor moves to
/// where it shows only in the last: a terminal that shows what it has in
/// between shows the cursor where luneta wrote last, as after a spinner that
/// turned on a row of the sidebar.
fn whole<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    draw: impl FnOnce(&mut Terminal<CrosstermBackend<W>>) -> io::Result<()>,
) -> io::Result<()> {
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let drawn = draw(terminal);
    // Ended where it failed too, for the terminal to show what it has.
    let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    drawn.and(ended)
}

/// The bytes that write what was dictated in the pane zellij is on, paste by
/// paste: a slash command, then what it is given, each with a space after it
/// for what you say next. Neither is typed, as zellij takes what is typed for
/// keys, and outside its normal mode acts on them: an `x` in its tab mode closes
/// the tab. Nor are they one paste, as Claude Code folds a long one to
/// `[Pasted text #1]`, and takes a prompt that is only that for words, whatever
/// the paste starts with.
fn pastes(written: &named::Written, mode: TermMode) -> Vec<Vec<u8>> {
    let command = written.command.iter().map(|command| format!("/{command} "));
    let text = (!written.text.is_empty()).then(|| format!("{} ", written.text));
    command.chain(text).map(|paste| keys::paste(&paste, mode)).collect()
}

/// What a client of zellij's on `on` said as it failed, ending `exited`: the
/// last line on its screen, as an ssh that reached no machine, or a zellij
/// that could not attach, or make a session of a name taken, writes it as it
/// ends. One that ended with zellij's screen up still, which zellij takes
/// down as it ends, ended under zellij, as an ssh that lost its connection
/// does: the screen shows what zellij drew, not what went wrong.
fn failed_saying(hosted: &Hosted, exited: Option<ExitStatus>, on: &Machine) -> String {
    if hosted.mode().contains(TermMode::ALT_SCREEN) {
        return match (on, exited.and_then(|status| status.code())) {
            (Machine::Remote(_), Some(SSH_FAILED)) => "ssh lost the connection".to_owned(),
            (Machine::Remote(_), _) => "zellij's client there ended".to_owned(),
            (Machine::Local, _) => "zellij's client ended".to_owned(),
        };
    }
    let text = hosted.text();
    let said = text.lines().rfind(|line| !line.trim().is_empty());
    let ended = match on {
        Machine::Remote(_) => "ssh ended",
        Machine::Local => "zellij's client ended",
    };
    said.map_or(ended, str::trim).to_owned()
}

/// How luneta's terminal is told to draw a cursor of `shape` that blinks or
/// not: as it would draw one of its own, for the shape a program never asked
/// for, which `hosted` gives as hollow.
fn cursor_style((shape, blinking): (CursorShape, bool)) -> SetCursorStyle {
    match (shape, blinking) {
        (CursorShape::HollowBlock, _) => SetCursorStyle::DefaultUserShape,
        (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
        (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
        (CursorShape::Beam, true) => SetCursorStyle::BlinkingBar,
        (CursorShape::Beam, false) => SetCursorStyle::SteadyBar,
        (_, true) => SetCursorStyle::BlinkingBlock,
        (_, false) => SetCursorStyle::SteadyBlock,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use ratatui::{TerminalOptions, Viewport};

    use super::*;

    /// What luneta writes to its terminal, write by write, into `written`:
    /// each flush is one, as it is to stdout.
    #[derive(Default)]
    struct Writes {
        written: Rc<RefCell<Vec<Vec<u8>>>>,
        writing: Vec<u8>,
    }

    impl io::Write for Writes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writing.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if !self.writing.is_empty() {
                self.written.borrow_mut().push(std::mem::take(&mut self.writing));
            }
            Ok(())
        }
    }

    /// A frame with `glyph` at the second column of the second line, and the
    /// cursor at `cursor`, written whole to `terminal`.
    fn frame(terminal: &mut Terminal<CrosstermBackend<Writes>>, glyph: &str, cursor: Position) {
        whole(terminal, |terminal| {
            terminal.draw(|frame| {
                frame.buffer_mut()[(1, 1)].set_symbol(glyph);
                frame.set_cursor_position(cursor);
            })?;
            Ok(())
        })
        .unwrap();
    }

    /// Pasted, what you dictate reaches the pane in any mode of zellij's, which
    /// acts on keys: typed, `/handoff` would open a pane in its pane mode.
    #[test]
    fn what_is_dictated_is_pasted_and_a_command_on_its_own() {
        let pasted = |command: Option<&str>, text: &str| {
            let command = command.map(|name| skills::Name::new(name.to_owned()).expect("a name"));
            let written = named::Written { command, text: text.to_owned(), only_first: Vec::new() };
            pastes(&written, TermMode::BRACKETED_PASTE)
        };
        assert_eq!(
            pasted(Some("handoff"), "the next session fixes CI"),
            [
                b"\x1b[200~/handoff \x1b[201~".to_vec(),
                b"\x1b[200~the next session fixes CI \x1b[201~".to_vec(),
            ],
        );
        assert_eq!(pasted(Some("handoff"), ""), [b"\x1b[200~/handoff \x1b[201~".to_vec()]);
        assert_eq!(pasted(None, "hand this off"), [b"\x1b[200~hand this off \x1b[201~".to_vec()]);
    }

    /// Where `screen` shows its cursor, if it shows one.
    fn shown(screen: &Term<VoidListener>) -> Option<(i32, usize)> {
        let cursor = screen.renderable_content().cursor;
        let point = (cursor.point.line.0, cursor.point.column.0);
        (cursor.shape != CursorShape::Hidden).then_some(point)
    }

    /// A terminal that shows what it has after each write luneta makes shows
    /// the cursor only where a frame puts it, as zellij's, and never where
    /// luneta wrote last, as on a spinner that turned on a row of the
    /// sidebar: a frame is held until it is all written.
    #[test]
    fn the_cursor_shows_only_where_a_frame_puts_it() {
        let viewport = Viewport::Fixed(Rect::new(0, 0, 20, 4));
        let writes = Writes::default();
        let written = Rc::clone(&writes.written);
        let backend = CrosstermBackend::new(writes);
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        let mut screen = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let zellij = Position::new(15, 3);
        frame(&mut terminal, "⠋", zellij);
        for write in written.take() {
            parser.advance(&mut screen, &write);
        }
        assert_eq!(shown(&screen), Some((3, 15)));
        frame(&mut terminal, "⠙", zellij);
        let writes = written.take();
        assert!(writes.len() > 1, "a frame goes out in more than one write");
        for write in writes {
            parser.advance(&mut screen, &write);
            assert_eq!(shown(&screen), Some((3, 15)));
        }
        assert_eq!(screen.grid()[Point::new(Line(1), Column(1))].c, '⠙');
    }

    /// A frame luneta ended in, as a panic in one ends it, shows once luneta
    /// puts its terminal back, which a terminal holds till then, and your
    /// cursor is in your own style again.
    #[test]
    fn putting_back_shows_a_frame_luneta_ended_in() {
        let mut screen = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let mut ended = Vec::new();
        execute!(ended, SetCursorStyle::SteadyBar, BeginSynchronizedUpdate).unwrap();
        ended.extend_from_slice("⠙".as_bytes());
        parser.advance(&mut screen, &ended);
        let spinner = Point::new(Line(0), Column(0));
        assert_eq!(screen.grid()[spinner].c, ' ', "the frame is held");
        assert_eq!(screen.cursor_style().shape, CursorShape::Beam);
        let mut put = Vec::new();
        put_back(&mut put).unwrap();
        parser.advance(&mut screen, &put);
        assert_eq!(screen.grid()[spinner].c, '⠙');
        assert_eq!(screen.cursor_style(), Config::default().default_cursor_style);
    }
}
