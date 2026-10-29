//! luneta's own terminal: the sidebar on the left, zellij on the right, and
//! which of the two has the keys. `Ctrl b` hands them from one to the other,
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
//! sessions, `Ctrl a` on the agents and `Ctrl d` on the directories. What the
//! mouse drags on zellij stays with zellij under a box too. A click on an
//! entry of the menu, a key a line of keys names, a skill the menu's search
//! finds, or a row of the picker, does what its key does, or `Enter` on it,
//! as each said where it drew them; and the one under the pointer is lit.

use std::collections::VecDeque;
use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::CursorShape;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::SetTitle;
use lunetad::api::Host;
use lunetad::log::{self, Process};
use ratatui::layout::{Position, Rect};
use ratatui::DefaultTerminal;

use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange, Theme};
use crate::ask::{Ask, Heard, Keyed};
use crate::brief::{self, Briefing, Written};
use crate::close::Act;
use crate::folds::{self, Folds};
use crate::hosted::{Hosted, Said, Size};
use crate::input;
use crate::keys;
use crate::menu::{self, Choice};
use crate::news::{self, News};
use crate::order::{self, Order};
use crate::own::{Before, Look, Looked};
use crate::picker::{self, List, Picked, Picker};
use crate::pull::{self, Ready};
use crate::sidebar::{self, Asked, Keys, Place, Sidebar};
use crate::skills::{self, Skill};
use crate::spots::{Burst, Drawn, Hit};
use crate::width::Width;
use crate::zellij::{self, After, Arrival, Closes, Refuge, SessionName, Tab};

/// What luneta's loop acts on, from the threads that wait on each source.
pub enum Event {
    /// A key, the mouse, a paste or a resize of luneta's terminal.
    Input(Input),
    /// An answer of luneta's terminal, or what it says of itself unasked,
    /// which comes in among the input whenever it comes.
    Answered(Answer),
    /// luneta's terminal cannot be read any more.
    InputEnded(io::Error),
    Hosted(Said),
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
    /// What came of the brief numbered `number`.
    Briefed { number: u64, written: Written },
    /// What the turn of the ask box numbered `number` heard its agent do.
    Heard { number: u64, heard: Heard },
    /// What zellij answered the picker numbered `number`.
    Picker { number: u64, heard: picker::Heard },
    /// A fold was made or taken away where the folds are kept: by another
    /// luneta, or by this one.
    Folds,
    /// Something was kept of what you saw or marked unread: by another
    /// luneta, or by this one.
    News,
    /// The order of the sidebar's rows was kept: by another luneta, or by
    /// this one.
    Order,
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

/// The key that hands the keys from zellij to the sidebar, and back.
const SWAP: (KeyCode, KeyModifiers) = (KeyCode::Char('b'), KeyModifiers::CONTROL);

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
const AGENTS: (KeyCode, KeyModifiers) = (KeyCode::Char('a'), KeyModifiers::CONTROL);

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

/// luneta's client on its way to a session zellij makes for it, from the one
/// it is in, until zellij titles luneta's terminal there. Dropped before, as
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
    session: String,
    until: Instant,
}

impl Nudged {
    /// The session the action is asked of, which zellij's client is in.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// When the action can go.
    pub fn until(&self) -> Instant {
        self.until
    }
}

impl Client {
    /// Starts `program`, zellij's client, in a terminal of `size`, in `dir`,
    /// with the input held for it, and the look for its `arrival` if any,
    /// until zellij can take it.
    fn start(
        program: String,
        args: Vec<String>,
        dir: &Path,
        size: Size,
        arrival: Option<Arrival>,
        events: Sender<Event>,
    ) -> io::Result<Self> {
        let hosted = Hosted::start(program, args, dir, size, events)?;
        let until = Instant::now() + WAITING_MAX;
        let coming = Some(Coming::Held(Waiting { input: Vec::new(), arrival, until }));
        Ok(Client { hosted, exited: None, coming, making: None, typed: None })
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
    /// zellij first.
    fn nudge(&mut self, here: String) -> Nudged {
        let before = self.typed_at(&here);
        self.nudge_in(here.clone());
        // Where it typed nothing there, as zellij reads no nudge, no wait
        // makes zellij move it.
        let typed = before.or_else(|| self.typed_at(&here));
        let until = typed.map_or_else(Instant::now, |typed| typed + zellij::NUDGED);
        Nudged { session: here, until }
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
    /// pressed a spot, that come fast after it, onto what that drew there.
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
    /// luneta waits for as it ends: it ends with its only session, which
    /// zellij lets go of only once the session's server has ended.
    deleting: Vec<JoinHandle<()>>,
}

/// Runs zellij, attached to `session`, beside the sidebar, until zellij ends:
/// its client detaches or fails, or its session ends and no other runs. Where
/// another does, a client of the newest takes the place of the one that
/// ended. The sidebar is of what the daemon in `folder` says, which is
/// started if none runs.
pub fn run(session: Option<SessionName>, folder: Result<PathBuf, String>) -> ExitCode {
    let dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("luneta: no directory to start in: {error}");
            return ExitCode::FAILURE;
        },
    };
    let session = session.or_else(|| zellij::session_for(&dir));
    if let Some(refusal) = session.as_ref().and_then(zellij::refusal) {
        eprintln!("luneta: {refusal}");
        return ExitCode::FAILURE;
    }
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("luneta: no terminal: {error}");
            return ExitCode::FAILURE;
        },
    };
    // Before the hook, so that no panic of the sidebar's goes by the log. Why
    // there is none is for the sidebar to say, once it has its notes.
    let unkept = log::start(Process::Luneta).err();
    // ratatui's hook puts the terminal back as it found it before a panic is
    // printed, all but the modes luneta turns on itself. The panic goes in
    // the log first, which no terminal can hold up.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::panicked(panic);
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange,
            DisableThemeChange
        );
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
        if answers.kitty {
            let disambiguate = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
            execute!(io::stdout(), PushKeyboardEnhancementFlags(disambiguate))?;
        }
        run_in(
            &mut terminal,
            session.as_ref(),
            folder,
            unkept,
            &dir,
            answers.colours,
            events,
            &mut inbox,
        )
    });
    // Each mode is put back however the rest went, and the first error is the
    // one said.
    let popped =
        if answers.kitty { execute!(io::stdout(), PopKeyboardEnhancementFlags) } else { Ok(()) };
    let restored = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableThemeChange,
        SetCursorStyle::DefaultUserShape
    );
    let left = ratatui::try_restore();
    match ran.and_then(|failed| popped.and(restored).and(left).map(|()| failed)) {
        Ok(None) => ExitCode::SUCCESS,
        // What zellij said as it failed went with luneta's screen.
        Ok(Some(failed)) => {
            eprintln!("{failed}");
            ExitCode::FAILURE
        },
        Err(error) => {
            eprintln!("luneta: {error}");
            ExitCode::FAILURE
        },
    }
}

/// Runs zellij beside the sidebar until it ends, and says what it said as it
/// failed, if it did. zellij's terminal answers it with `colours`, until
/// luneta's terminal says others. `unkept` is why what goes wrong is kept in
/// no log, where it is not.
fn run_in(
    terminal: &mut DefaultTerminal,
    session: Option<&SessionName>,
    folder: Result<PathBuf, String>,
    unkept: Option<String>,
    dir: &Path,
    colours: Colours,
    events: Sender<Event>,
    inbox: &mut Inbox,
) -> io::Result<Option<String>> {
    let (width, width_unread) = Width::kept();
    let areas = areas(Rect::from((Position::ORIGIN, terminal.size()?)), width.columns());
    let folds = Folds::shared();
    // Kept beside the daemon, which lists the sessions it is kept of.
    let news = match &folder {
        Ok(runtime) => News::within(runtime),
        Err(why) => News::unshared(why),
    };
    let order = Order::shared();
    let folds_watched = folds.dir().map(|dir| folds::watch(dir, events.clone())).transpose();
    let news_watched = news::watch(&news, events.clone());
    let order_watched = order.file().map(|file| order::watch(file, events.clone())).transpose();
    let mut sidebar = Sidebar::new(folds, news, order);
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
    let arrival = session.and_then(|session| {
        let before = zellij::before(session.as_str());
        let look = sidebar.sending(session)?;
        Some(Arrival::new(look, before))
    });
    let (program, args) = zellij::client(session);
    let client = Client::start(program, args, dir, window(areas.1), arrival, events.clone())?;
    let daemon = match folder {
        Ok(folder) => Some(folder),
        Err(why) => {
            sidebar.lost(why);
            None
        },
    };
    let mut app = App {
        client,
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
        claude_folder: None,
        daemon,
        events,
        _watching: watching,
        dir: dir.to_owned(),
        ended: false,
        deleting: Vec::new(),
    };
    let ran = run_loop(terminal, &mut app, inbox);
    for deleting in app.deleting.drain(..) {
        // What a thread that panicked left undone stays so.
        let _ = deleting.join();
    }
    ran?;
    Ok(app.failure())
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
        } else if app.sidebar.turning() {
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
            Event::Daemon(_) | Event::Looked(_) | Event::Stayed(_) | Event::Hosted(Said::Titled(_))
        );
        match event {
            Event::Input(input) => match &mut self.client.coming {
                Some(Coming::Held(waiting)) => waiting.input.push(input),
                _ => self.input(input),
            },
            Event::Answered(answer) => self.answered(answer, terminal)?,
            Event::InputEnded(error) => return Err(error),
            Event::Hosted(said) => self.hosted_said(said, terminal)?,
            Event::Reached { claude_folder } => self.claude_folder = claude_folder,
            Event::Daemon(Ok(host)) => {
                let look = self.sidebar.heard(host);
                self.arrived(look);
            },
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Looked(looked) => self.sidebar.looked(looked),
            Event::Stayed(look) => {
                let making = self.client.making.as_ref();
                if making.is_some_and(|making| making.arrival.look.number == look) {
                    self.client.making = None;
                }
                let look = self.sidebar.stayed(look);
                // The client never left: what it typed there counts still.
                self.look(look);
            },
            Event::Notice(notice) => self.sidebar.notify(notice),
            Event::Folds => self.sidebar.refold(),
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

    /// When the picker asks zellij for what its preview shows, while it is
    /// open and is to ask.
    fn due(&self) -> Option<Instant> {
        match &self.focus.over {
            Some(Over::Picker(picker)) => picker.due(),
            _ => None,
        }
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
        if !(self.focused && self.focus.zellij())
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
            // A pull request's or an issue's link pasted with the sidebar's
            // keys, or in the menu over zellij, opens the menu for it. zellij
            // keeps every paste it has the keys for, links too, for the
            // agents in it.
            Input::Paste(text) => match &mut self.focus.over {
                Some(Over::Ask(ask)) => ask.paste(&text),
                Some(Over::Picker(picker)) => picker.paste(&text),
                Some(Over::Menu) => self.sidebar.paste(&text),
                Some(Over::Brief(_) | Over::Note(_)) => {},
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
        if key.kind == KeyEventKind::Release {
            return;
        }
        let chord = (key.code, key.modifiers);
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
            // again.
            Some(Over::Note(_)) => {
                self.give(Focus::on(self.focus.side));
                if chord != MENU {
                    return;
                }
            },
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
            let picker = Box::new(Picker::open(self.pickers, list, self.sidebar.listing()));
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
            if let Some(bytes) = keys::key(key, self.client.hosted.mode()) {
                self.client.hosted.send(bytes);
                self.typed();
            }
        } else {
            let asked = self.sidebar.key(key, SystemTime::now());
            self.follow(asked);
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
        // A click of a burst on one cell is no click on what its first drew
        // under it: on the row a click on the sidebar collapsed opened there,
        // it would fold a session, or take zellij to a pane; on the entry
        // that took the place of a group a click opened, it would press that
        // entry's key, as a delete's, and the next click the key of the entry
        // that took its place in turn; and on zellij, where a click closed
        // the box over it, it would click there.
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
        // would end the talk.
        match &self.focus.over {
            Some(Over::Ask(_) | Over::Picker(_)) => return,
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
    /// still: presses its key, the legend's, the menu's or the picker's, or
    /// picks the skill the menu's search finds, or the picker's row, as
    /// `Enter` does on it. A spot of what closed since it drew, or gave way
    /// to another, does nothing, as what zellij answers a menu closed does:
    /// what was drawn last is what the click was on, but what came since,
    /// with it as luneta drew the next frame, can have changed what it does.
    fn hit(&mut self, hit: Hit) {
        match hit {
            Hit::Legend(key) if self.focus.over.is_none() => {
                let asked = self.sidebar.legend_key(&key, SystemTime::now());
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
            Hit::Legend(_) | Hit::Menu(..) => {},
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
            Asked::Go(place) => {
                self.go(&place, None);
                self.leave();
            },
            Asked::Closes { menu, session, pane } => {
                zellij::closes(menu, &session, pane, &self.events);
            },
            Asked::Delete(act) => match act {
                Act::Pane { session, pane } => zellij::close_pane(&session, pane, &self.events),
                Act::Tab { session, tab, .. } => zellij::close_tab(&session, tab, &self.events),
                Act::Kill { session, started } => {
                    self.deleting.retain(|deleting| !deleting.is_finished());
                    self.deleting.push(zellij::kill(&session, started, &self.events));
                },
                // The plan found zellij's client in `session`, and on its
                // way nowhere, as the key was pressed: where it goes from.
                Act::Leave { session, started, to } => {
                    self.go(&to, Some(After::Kill { session, started }));
                },
            },
            Asked::Open { menu, open } => zellij::open(menu, open, &self.events),
            Asked::OpenThere { menu, place, open } => {
                if !self.go(&place, Some(After::Open(menu, open))) {
                    self.sidebar.close_menu();
                }
            },
            Asked::Start { session, cwd } => {
                self.make(&session, &cwd, None);
                self.leave();
            },
            // The menu waits until zellij has opened it, and takes the keys
            // there with it.
            Asked::Make { menu, session, cwd, tab_cwd, name, command } => {
                let tab = Tab { cwd: tab_cwd, name: Some(name), command };
                if !self.make(&session, &cwd, Some((menu, tab))) {
                    self.sidebar.close_menu();
                }
            },
            Asked::Pull { menu, link, worked } => pull::prepare(menu, link, worked, &self.events),
            Asked::Find { menu, cwd } => skills::find(menu, &cwd, &self.events),
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
        }
    }

    /// Does what the picker asked. Where it takes zellij somewhere, it
    /// closes, and zellij has the keys; a delete leaves it open, but for one
    /// of the session luneta's zellij is in, which takes zellij to another.
    fn picked(&mut self, picked: Picked) {
        match picked {
            Picked::Nothing => {},
            Picked::Close => self.give(Focus::on(self.focus.side)),
            Picked::Go(place) => {
                self.go(&place, None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Start(session) => {
                self.start(&session, None, None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Make { session, cwd } => {
                self.make(&session, &cwd, None);
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Plan { session, deletes } => {
                let plan = self.sidebar.plan(&session, deletes);
                if let Some(Over::Picker(picker)) = &mut self.focus.over {
                    let did = picker.planned(plan);
                    self.picked(did);
                }
            },
            Picked::Delete(Act::Leave { session, started, to }) => {
                self.go(&to, Some(After::Kill { session, started }));
                self.give(Focus::on(self.focus.side.zellij()));
            },
            Picked::Delete(act) => self.follow(Asked::Delete(act)),
        }
    }

    /// Hands what you typed in the ask box to its agent, for you in the pane
    /// luneta's zellij is on. luneta's zellij types there first, as the last
    /// client to type in its session, which zellij moves where the agent has
    /// it take you somewhere.
    fn converse(&mut self) {
        let here = self.sidebar.here_pane();
        // Whether it typed there before or not: another client may have
        // since, and the agent acts well after the nudge.
        if here.is_some()
            && let Ok(seat) = self.seat()
        {
            self.client.nudge_in(seat);
        }
        if let Some(Over::Ask(ask)) = &mut self.focus.over {
            ask.send(here.as_ref(), &self.events);
        }
    }

    /// Takes zellij's client to the pane the sidebar lists after the one it
    /// is on. The sidebar gives the keys up first, which closes a search: the
    /// pane after is the one after in every row, not in the rows a search
    /// leaves, which pass over panes and open folded sessions.
    fn next(&mut self) {
        self.leave();
        if let Some(place) = self.sidebar.next_pane() {
            self.go(&place, None);
        }
    }

    /// Takes zellij's client to `place`, and does `after` once it is there,
    /// in another session. Whether it went.
    fn go(&mut self, place: &Place, after: Option<After>) -> bool {
        let Some(nudged) = self.nudge() else {
            return false;
        };
        let then = match &after {
            // The session it leaves ends once it has left it, which the
            // sidebar says until then.
            Some(After::Kill { .. }) => self.sidebar.leaving(place),
            Some(After::Open(..)) | None => self.sidebar.went(place),
        };
        zellij::go(&nudged, place.session(), place.pane(), then, after, &self.events);
        true
    }

    /// Takes zellij's client to a session it makes, `session`, that starts in
    /// `cwd`, or where the client started, or brings back, where zellij kept
    /// one that ended, and opens `tab` there once it is, for the menu
    /// numbered so. Where zellij makes it, its server cannot bear being asked
    /// anything before it takes the client: luneta asks for the client's
    /// arrival there once zellij titles its terminal in it. Whether it went.
    fn start(&mut self, session: &SessionName, cwd: Option<&str>, tab: Option<(u64, Tab)>) -> bool {
        let Some(nudged) = self.nudge() else {
            return false;
        };
        let then = self.sidebar.sending(session);
        let made = zellij::start(&nudged, session, cwd, then, tab, &self.events);
        let until = Instant::now() + zellij::START;
        self.client.making = made.map(|arrival| Making { arrival, until });
        true
    }

    /// Takes zellij's client to a session zellij makes, `session`, that
    /// starts in `cwd`, and opens `tab` there once it is, as `start` does; or
    /// says why zellij would make none, but take it to one that has the name,
    /// before the sidebar takes the client for one on its way there. Whether
    /// it went.
    fn make(&mut self, session: &SessionName, cwd: &str, tab: Option<(u64, Tab)>) -> bool {
        match zellij::makes(session) {
            Ok(()) => self.start(session, Some(cwd), tab),
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
    fn seat(&self) -> Result<String, String> {
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
        Ok(here.to_owned())
    }

    /// Types `zellij::NUDGE` into zellij's client, for an action asked of
    /// zellij in the session it is in to move it, or says why it is in none.
    fn nudge(&mut self) -> Option<Nudged> {
        match self.seat() {
            Ok(here) => Some(self.client.nudge(here)),
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
            && self.client.typed_at(&here).is_none()
        {
            self.client.nudge_in(here);
        }
    }

    /// zellij's client typed a key for you just now.
    fn typed(&mut self) {
        if let Ok(here) = self.seat() {
            self.client.typed_in(here);
        }
    }

    /// Asks zellij for the clients of a session, if the sidebar wants to know.
    fn look(&self, look: Option<Look>) {
        if let Some(look) = look {
            zellij::look(look, &self.events);
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
        if focus.side == Side::Collapsed || focus.over.is_some() {
            // The strip has no edge to drag nor rows to hold, and a box takes
            // the mouse: what the mouse holds of either is let go, as a
            // release would let it go. A press held on the rows would keep the
            // mouse from zellij, and the keys from the box, and the sidebar
            // never hears the release.
            self.let_go();
            self.sidebar.let_go();
        }
        let (had, has) = (self.focus.zellij(), focus.zellij());
        self.sidebar.focus(match (&focus.over, focus.side) {
            (None, Side::Sidebar) => Keys::Sidebar,
            (Some(Over::Menu), side) => Keys::Menu { lent: side == Side::Sidebar },
            // A brief, the ask box or a note leaves the sidebar as it stands,
            // highlight and all, for the keys to go back to as it closes.
            (
                Some(Over::Brief(_) | Over::Ask(_) | Over::Note(_) | Over::Picker(_)),
                Side::Sidebar,
            ) => Keys::Lent,
            (
                None | Some(Over::Brief(_) | Over::Ask(_) | Over::Note(_) | Over::Picker(_)),
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
            Some(Over::Brief(_) | Over::Ask(_) | Over::Note(_) | Over::Picker(_)) => false,
        };
        if leaves {
            self.give(Focus::on(self.focus.side.zellij()));
        }
    }

    fn focus_window(&mut self, focused: bool) {
        self.focused = focused;
        // The pointer has gone elsewhere, as far as luneta knows, and lights
        // nothing until it moves over luneta again.
        if !focused {
            self.pointer = None;
        }
        if self.focus.zellij()
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
    /// luneta goes on. luneta ends where none does, and where the client
    /// detached, or failed, as where zellij cannot attach.
    fn client_ended(&mut self) -> io::Result<()> {
        let seats = self.sidebar.seats();
        let ended = self.client.exited.filter(ExitStatus::success).filter(|_| !seats.is_empty());
        let Some(Refuge { session: to, before }) = ended.and_then(|_| zellij::refuge(&seats)) else {
            self.ended = true;
            return Ok(());
        };
        let look = self.sidebar.rejoining(&to);
        let arrival = Some(Arrival::new(look, Before::Listed(before)));
        let (program, args) = zellij::rejoin(to.as_str());
        let size = window(self.areas.1);
        self.client = Client::start(program, args, &self.dir, size, arrival, self.events.clone())?;
        // The client that takes its place heard no button go down over it.
        if self.held == Held::Zellij {
            self.held = Held::Nothing;
        }
        Ok(())
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
    /// gone: the text on its terminal, and how it ended. None for a zellij that
    /// detached or quit, or that ended in a way its terminal could not tell.
    fn failure(&self) -> Option<String> {
        let status = self.client.exited.filter(|status| !status.success())?;
        let ended = format!("luneta: zellij ended ({status})");
        let said = self.client.hosted.text();
        Some(if said.is_empty() { ended } else { format!("{said}\n{ended}") })
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
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
                    briefing.draw(buffer, zellij);
                    Drawn::Nothing
                },
                Some(Over::Ask(ask)) => {
                    ask.draw(buffer, zellij);
                    Drawn::Nothing
                },
                Some(Over::Picker(picker)) => picker.draw(buffer, zellij, now),
                Some(Over::Note(note)) => {
                    menu::draw_note(buffer, zellij, note);
                    Drawn::Nothing
                },
                Some(Over::Menu) => {
                    let floating = self.sidebar.floating();
                    floating.map(|menu| menu.draw(buffer, zellij)).unwrap_or_default()
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
