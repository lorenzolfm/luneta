use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::Path;
use std::process;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lune_domain::mux::TerminalKey;
use lune_domain::projects::layout::{self, Direction, Order, Sides, Towards};
use lune_domain::projects::{Argv, Arranged, Listed, Opens, WORKTREES_MAX, quoted};
use lune_domain::projects::{
    Ask, Browser, BrowserTarget, BrowserUrl, Change, Closing, Dir, Id, Line, Name, Placed,
    Subfolder,
};
use lune_domain::projects::{Holds, ProjectName, Worktree};
use lune_domain::projects::{
    LAYOUT_VIEWS_MAX, LINE_BYTES_MAX, PATH_BYTES_MAX, VIEWS_MAX, WorktreeId,
};
use lune_domain::projects::{
    Layout, Made, MoveTo, Project, ProjectId, Projects, Ran, Relaid, Runs,
};
use lune_domain::projects::{Side, Size, Terminal, TerminalViewId, Used, View, ViewId};
use lune_protocol::LIST_RESUME_RECORDS_BYTES_MAX;
use lune_protocol::program_resume::{
    ResumeArgv, ResumeCwdSource, ResumeRecord, ShellKind, escape_for_display,
};
use lune_protocol::program_status::bound_fair;
use lune_protocol::shell::Doing;
use prost::Message as _;
use tokio::sync::{mpsc as answers, oneshot};
use tonic::Status;

use crate::api::{self, show_request, show_response};
use crate::core::{self, Command as Core, Located, Shown};
use crate::mux::own::server::{self, Is, Terminals, TerminalsListed};
use crate::mux::{Failure, Runs as Opened};
use crate::program::{self, Ran as Looked};
use crate::projects::{self, Changed, Failed, Store};
use crate::resume::config::{self as resume_config, ConfigState};
use crate::resume::store::{RecordSource, ResumeAttempt, ResumeStore, StoredResume};
use crate::resume::synthesize::{LiveTerminal, Synthesizer};
use crate::resume::{self, trust};
use crate::shells::Shell;
use crate::shows::{AskNumber, direction_said};
use crate::terminal_socket::{self, PaneInfo, PaneRef, SocketCommand, SplitAction};
use crate::watch::Troubles;
use crate::{Directory, log, shells};

use errands::{Errands, Unrun};
use homes::{Homes, LookTimeout, Spotted};

mod errands;
mod homes;

pub const CLIENTS_MAX: usize = 64;

pub const ANSWERS_MAX: usize = 64;

const COMMANDS_MAX: usize = 64;

const CLOSED_VIEWS_MAX: usize = 64;

const TAG: &str = "lune terminal ";

pub const RESTORE_PAUSE: Duration = Duration::from_secs(10);

const RESTORE_ATTEMPTS_MAX: u32 = 3;

const RESTORES_PER_PASS: usize = 8;

const RESTORE_SETTLE: Duration = Duration::from_secs(30);

const TYPING_WAIT: Duration = Duration::from_secs(3);

const TYPING_WAIT_MAX: Duration = Duration::from_secs(30);

const FOLDER_LOOK: LookTimeout = LookTimeout::of(Duration::from_secs(1));

const UNSHOWN_SIZE: Size = Size {
    columns: Side::new(80).expect("80 is a side"),
    rows: Side::new(24).expect("24 is a side"),
};

const SHELLS: [&str; 9] = [
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh",
];

const _: () = assert!(ANSWERS_MAX > 1);
const _: () = assert!(CLOSED_VIEWS_MAX > 0);
const _: () = assert!(RESTORE_ATTEMPTS_MAX > 0);
const _: () = assert!(RESTORES_PER_PASS > 0);
const _: () = assert!(RESTORES_PER_PASS <= VIEWS_MAX);
const _: () = assert!(RESTORE_SETTLE.as_nanos() > RESTORE_PAUSE.as_nanos());
const _: () = assert!(TYPING_WAIT.as_nanos() < TYPING_WAIT_MAX.as_nanos());

pub type Out = answers::Sender<Result<api::ShowResponse, Status>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(u64);

pub enum Command {
    Change {
        change: Ask,
        answer: oneshot::Sender<Result<Changed, Failed>>,
    },
    Came {
        worktree: WorktreeId,
        size: Size,
        runs_in: Option<TerminalKey>,
        again: bool,
        asked_number: Option<AskNumber>,
        out: Out,
        answer: oneshot::Sender<Result<ClientId, Status>>,
    },
    Asked {
        client: ClientId,
        asked_number: Option<AskNumber>,
        asked: Result<Asked, String>,
    },
    Left(ClientId),
    Listed(TerminalsListed),
    Emptied {
        since: Instant,
    },
    Crashed,
    Unreached {
        since: Instant,
    },
    Worktrees {
        root: Dir,
        listed: Listed,
        seen: BTreeSet<WorktreeId>,
    },
    Rooted {
        root: Dir,
        listed: Listed,
    },
    Root {
        project: ProjectId,
        answer: oneshot::Sender<Result<Root, Unrooted>>,
    },
    ListResume {
        terminal_id: Option<NonZeroU32>,
        answer: oneshot::Sender<api::ListResumeResponse>,
    },
    ReloadResume {
        answer: oneshot::Sender<api::ReloadResumeResponse>,
    },
    Socket(SocketCommand),
    WorktreeFolder {
        worktree: WorktreeId,
        answer: oneshot::Sender<Result<Dir, Unrooted>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Root {
    Clone(Dir),
    Folder { dir: Dir, name: ProjectName },
}

impl Root {
    #[must_use]
    pub fn dir(&self) -> &Dir {
        match self {
            Root::Clone(dir) | Root::Folder { dir, .. } => dir,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unrooted {
    Unkept(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Size(Size),
    Go(WorktreeId),
    Open {
        beside: Beside,
        opens: Opens,
    },
    Welcome,
    Browser(Option<BrowserUrl>),
    Navigate {
        view: ViewId,
        url: Option<BrowserUrl>,
    },
    BrowserTty {
        view: ViewId,
        tty: Option<String>,
    },
    Review,
    Close(ViewId),
    Focus(Focus),
    Resize {
        sides: Sides,
        growth: layout::Growth,
    },
    Move(Step),
    Fullscreen,
    Arrange(Arranged),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    Alone,
    Largest,
    Focused,
    Towards(Towards),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Done {
    Quiet,
    Edged {
        direction: Direction,
        step: NonZeroU64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Toward {
        direction: Direction,
        step: NonZeroU64,
    },
    Order(Order),
    View(ViewId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Direction(Direction),
    Order(Order),
}

impl Asked {
    pub fn of_api(event: show_request::Event) -> Result<Asked, String> {
        use show_request::Event;
        Ok(match event {
            Event::Hello(_) => return Err("a hello comes only first".to_owned()),
            Event::Size(size) => Asked::Size(size_of(Some(size))?),
            Event::Go(worktree) => Asked::Go(id("go", worktree).map(WorktreeId::new)?),
            Event::Open(show_request::Open { beside, opens }) => {
                use show_request::open::Beside as Said;
                let beside = match beside.ok_or("open: it says no side to open beside")? {
                    Said::Largest(()) => Beside::Largest,
                    Said::Focused(()) => Beside::Focused,
                    Said::Alone(()) => Beside::Alone,
                    Said::Towards(towards) => Beside::Towards(towards_of(towards)?),
                };
                let opens = Opens::of_api(opens).map_err(|why| format!("open: {why}"))?;
                Asked::Open { beside, opens }
            }
            Event::Welcome(()) => Asked::Welcome,
            Event::Browser(show_request::Browser { url }) => {
                let url = url
                    .as_deref()
                    .map(BrowserUrl::parse)
                    .transpose()
                    .map_err(|why| format!("browser: {why}"))?;
                Asked::Browser(url)
            }
            Event::Navigate(show_request::Navigate { view, url }) => {
                let view = id("navigate", view).map(ViewId::new)?;
                let url = url
                    .as_deref()
                    .map(BrowserUrl::parse)
                    .transpose()
                    .map_err(|why| format!("navigate: {why}"))?;
                Asked::Navigate { view, url }
            }
            Event::BrowserTty(show_request::BrowserTty { view, tty }) => {
                let view = id("browser_tty", view).map(ViewId::new)?;
                if let Some(tty) = &tty
                    && (tty.is_empty()
                        || tty.len() > PATH_BYTES_MAX
                        || tty.chars().any(char::is_control))
                {
                    return Err("browser_tty: invalid tty path".to_owned());
                }
                Asked::BrowserTty { view, tty }
            }
            Event::Review(()) => Asked::Review,
            Event::Close(view) => Asked::Close(id("close", view).map(ViewId::new)?),
            Event::Focus(show_request::Focus { to, step }) => {
                use show_request::focus::To;
                Asked::Focus(match to.ok_or("focus: it says no terminal to go to")? {
                    To::Direction(direction) => {
                        let direction = direction_of(direction)?;
                        let step = NonZeroU64::new(step).ok_or("focus: a step numbered 0")?;
                        Focus::Toward { direction, step }
                    }
                    To::Order(order) => Focus::Order(order_of(order)?),
                    To::View(view) => Focus::View(id("focus", view).map(ViewId::new)?),
                })
            }
            Event::Resize(show_request::Resize { sides, growth }) => {
                use show_request::resize;
                let growth = growth_of(growth)?;
                let sides = match sides.ok_or("resize: it says no side to move")? {
                    resize::Sides::Direction(direction) => Sides::One(direction_of(direction)?),
                    resize::Sides::Every(()) => Sides::Every,
                };
                Asked::Resize { sides, growth }
            }
            Event::Move(show_request::Move { to }) => {
                use show_request::r#move::To;
                Asked::Move(match to.ok_or("move: it says nowhere to move to")? {
                    To::Direction(direction) => Step::Direction(direction_of(direction)?),
                    To::Order(order) => Step::Order(order_of(order)?),
                })
            }
            Event::Fullscreen(()) => Asked::Fullscreen,
            Event::Arrange(said) => {
                Asked::Arrange(Arranged::said(said).map_err(|it| format!("arrange: {it}"))?)
            }
        })
    }
}

pub fn size_of(size: Option<show_request::Size>) -> Result<Size, String> {
    let size = size.ok_or("it says no size")?;
    Size::of(size.columns, size.rows).map_err(|why| format!("size: {why}"))
}

pub fn id(field: &str, id: u64) -> Result<Id, String> {
    Id::of(id).ok_or_else(|| format!("{field}: no id is 0"))
}

fn towards_of(said: i32) -> Result<Towards, String> {
    use show_request::Towards as Said;
    match Said::try_from(said) {
        Ok(Said::Right) => Ok(Towards::Right),
        Ok(Said::Down) => Ok(Towards::Down),
        Ok(Said::Unspecified) | Err(_) => Err("open: no side it knows to open towards".to_owned()),
    }
}

pub(crate) fn direction_of(said: i32) -> Result<Direction, String> {
    use show_request::Direction as Said;
    match Said::try_from(said) {
        Ok(Said::Left) => Ok(Direction::Left),
        Ok(Said::Right) => Ok(Direction::Right),
        Ok(Said::Up) => Ok(Direction::Up),
        Ok(Said::Down) => Ok(Direction::Down),
        Ok(Said::Unspecified) | Err(_) => Err("no direction it knows".to_owned()),
    }
}

fn growth_of(said: i32) -> Result<layout::Growth, String> {
    use show_request::Growth as Said;
    match Said::try_from(said) {
        Ok(Said::Grow) => Ok(layout::Growth::Grow),
        Ok(Said::Shrink) => Ok(layout::Growth::Shrink),
        Ok(Said::Unspecified) | Err(_) => Err("resize: it says neither grow nor shrink".to_owned()),
    }
}

fn order_of(said: i32) -> Result<Order, String> {
    use show_request::Order as Said;
    match Said::try_from(said) {
        Ok(Said::Next) => Ok(Order::Next),
        Ok(Said::Previous) => Ok(Order::Previous),
        Ok(Said::Unspecified) | Err(_) => Err("no order it knows".to_owned()),
    }
}

pub fn ask<T>(
    layouts: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    layouts.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
}

pub fn start(
    core: SyncSender<Core>,
    store: Store,
    follower: SyncSender<()>,
    sweep: Duration,
) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    let errands = Errands::start();
    let resume_config = resume_config::load();
    match &resume_config {
        ConfigState::Loaded(config) => {
            for warning in &config.warnings {
                log::say(&format!("resume config: {warning}"));
            }
        }
        ConfigState::Incomplete {
            error, warnings, ..
        } => {
            log::say(&format!(
                "resume config failed to load (program resume disabled): {error}"
            ));
            for warning in warnings {
                log::say(&format!("resume config: {warning}"));
            }
        }
    }
    let (resume_store, resume_store_warning) = ResumeStore::open(store.projects().is_some());
    if let Some(why) = resume_store_warning {
        log::say(&format!("resume records: {why}"));
    }
    let mut homes = Homes::start(FOLDER_LOOK)?;
    if let Some(projects) = store.projects() {
        homes.seed(projects);
    }
    let layouts = Layouts {
        store,
        core,
        follower,
        mux: Terminals,
        errands,
        homes,
        clients: BTreeMap::new(),
        arrivals: 0,
        known: BTreeMap::new(),
        listed: BTreeMap::new(),
        looked: false,
        restore_round: BTreeSet::new(),
        restore_last_at: None,
        restore_attempts: Attempts::default(),
        told: None,
        troubles: Troubles::default(),
        shell: shells::your_shell(),
        resume_config,
        resume_store,
        resume_synth: Synthesizer::default(),
        closed_views: VecDeque::with_capacity(CLOSED_VIEWS_MAX),
    };
    let thread = thread::Builder::new().name("layouts".to_owned());
    thread.spawn(move || run(layouts, &commands, sweep))?;
    Ok(send)
}

struct Layouts {
    store: Store,
    core: SyncSender<Core>,
    follower: SyncSender<()>,
    mux: Terminals,
    errands: Errands,
    homes: Homes,
    clients: BTreeMap<ClientId, Client>,
    arrivals: u64,
    known: BTreeMap<TerminalViewId, Known>,
    listed: BTreeMap<TerminalKey, server::Terminal>,
    looked: bool,
    restore_round: BTreeSet<TerminalViewId>,
    restore_last_at: Option<Instant>,
    restore_attempts: Attempts,
    told: Option<BTreeMap<ViewId, Located>>,
    troubles: Troubles,
    shell: Shell,
    resume_config: ConfigState,
    resume_store: ResumeStore,
    resume_synth: Synthesizer,
    closed_views: VecDeque<ClosedView>,
}

#[derive(Clone, Debug)]
struct ClosedView {
    place: ClosedPlace,
    kind: ClosedKind,
}

#[derive(Clone, Debug)]
struct ClosedPlace {
    project: ProjectId,
    worktree: WorktreeId,
    subfolder: Option<Subfolder>,
    worktree_name: Option<Name>,
    arranged: Arranged,
}

#[derive(Clone, Debug)]
struct ClosedTerminal {
    folder: Dir,
    runs: Runs,
    name: Option<Name>,
    resume: Option<StoredResume>,
}

#[derive(Clone, Debug)]
enum ClosedKind {
    Terminal(Box<ClosedTerminal>),
    Browser { url: Option<BrowserUrl> },
    Review,
    Welcome,
}

enum PreparedTarget {
    Existing {
        worktree: WorktreeId,
        size: Size,
        focus: Option<ViewId>,
        was_empty: bool,
        arranged: Arranged,
    },
    ReopenClone {
        project: ProjectId,
        subfolder: Subfolder,
        name: Option<Name>,
        arranged: Arranged,
    },
}

struct PreparedTerminal {
    spawn_folder: Dir,
    changed_folder: bool,
    runs: Runs,
    name: Option<Name>,
    resume: Option<StoredResume>,
}

enum PreparedKind {
    Terminal(Box<PreparedTerminal>),
    Browser { url: Option<BrowserUrl> },
    Review,
    Welcome,
}

struct PreparedRevive {
    target: PreparedTarget,
    kind: PreparedKind,
}

enum TargetResolution {
    Ready(PreparedRevive),
    Gone,
}

enum ReviveFailure {
    Requeue(Failed),
    KeptInLayout(Failed),
}

struct TerminalSnapshot {
    folder: Dir,
    runs: Runs,
    keeps_restored: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientAsks {
    Unnumbered,
    Numbered(AskNumber),
}

impl ClientAsks {
    fn start(asked_number: Option<AskNumber>) -> ClientAsks {
        asked_number.map_or(ClientAsks::Unnumbered, ClientAsks::Numbered)
    }

    fn last(self) -> Option<AskNumber> {
        match self {
            ClientAsks::Unnumbered => None,
            ClientAsks::Numbered(last) => Some(last),
        }
    }

    fn hear(&mut self, asked_number: Option<AskNumber>) -> Result<(), String> {
        match (*self, asked_number) {
            (ClientAsks::Unnumbered | ClientAsks::Numbered(_), None) => Ok(()),
            (ClientAsks::Unnumbered, Some(number)) => {
                Err(format!("asked {} after unnumbered hello", number.get()))
            }
            (ClientAsks::Numbered(last), Some(number)) if number > last => {
                *self = ClientAsks::Numbered(number);
                Ok(())
            }
            (ClientAsks::Numbered(last), Some(number)) => Err(format!(
                "asked {} did not advance past {}",
                number.get(),
                last.get()
            )),
        }
    }
}

enum ClientPlace {
    Showing {
        worktree: WorktreeId,
        focus: Option<ViewId>,
        browser_ttys: BTreeMap<ViewId, String>,
        laid_said: Option<show_response::Laid>,
    },
    Stranded {
        left: WorktreeId,
    },
}

struct Client {
    size: Size,
    runs_in: Option<TerminalKey>,
    out: Out,
    heard: ClientAsks,
    echoed: Option<AskNumber>,
    place: ClientPlace,
}

impl Client {
    fn showing(&self) -> Option<(WorktreeId, Option<ViewId>)> {
        match self.place {
            ClientPlace::Showing {
                worktree, focus, ..
            } => Some((worktree, focus)),
            ClientPlace::Stranded { .. } => None,
        }
    }

    fn showing_worktree(&self, worktree: WorktreeId) -> bool {
        matches!(self.place, ClientPlace::Showing { worktree: shown, .. } if shown == worktree)
    }

    fn browser_ttys(&self, worktree: WorktreeId) -> Option<&BTreeMap<ViewId, String>> {
        match &self.place {
            ClientPlace::Showing {
                worktree: shown,
                browser_ttys,
                ..
            } if *shown == worktree => Some(browser_ttys),
            ClientPlace::Showing { .. } | ClientPlace::Stranded { .. } => None,
        }
    }
}

struct Known {
    key: TerminalKey,
    heard: Heard,
    settled: Option<Size>,
    remade: Option<Remade>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heard {
    Made(Instant),
    Seen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Remade {
    at: Instant,
    restored: Option<Restored>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Restored {
    Typing {
        line: Line,
        due: Instant,
        by: Instant,
    },
    Resuming {
        deadline: Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paste {
    Now,
    Later,
    Never,
}

#[derive(Default)]
struct Attempts {
    failures: BTreeMap<ViewId, u32>,
}

impl Attempts {
    fn wanted(&self, terminal: ViewId) -> bool {
        self.failures
            .get(&terminal)
            .is_none_or(|count| *count < RESTORE_ATTEMPTS_MAX)
    }

    fn failed(&mut self, terminal: ViewId, why: &str) -> String {
        let count = self.charged(terminal);
        if count < RESTORE_ATTEMPTS_MAX {
            let attempt = format!("attempt {count} of {RESTORE_ATTEMPTS_MAX}");
            format!("terminal {terminal} is not restored, on {attempt}: {why}")
        } else {
            let given_up = format!("given up after {RESTORE_ATTEMPTS_MAX} attempts");
            format!("terminal {terminal} is not restored, and is {given_up}: {why}")
        }
    }

    fn crashed(&mut self, terminal: ViewId, after: Duration) -> String {
        let count = self.charged(terminal);
        let secs = after.as_secs();
        let ended = format!("lune-mux ended {secs}s after terminal {terminal} was restored");
        if count < RESTORE_ATTEMPTS_MAX {
            format!("{ended}, which counts as attempt {count} of {RESTORE_ATTEMPTS_MAX}")
        } else {
            format!("{ended}, and it is given up after {RESTORE_ATTEMPTS_MAX} attempts")
        }
    }

    fn settled(&mut self, terminal: ViewId) {
        self.failures.remove(&terminal);
    }

    fn charged(&mut self, terminal: ViewId) -> u32 {
        assert!(
            self.wanted(terminal),
            "terminal {terminal}, given up, is not restored again"
        );
        let count = self.failures.entry(terminal).or_insert(0);
        *count += 1;
        assert!(
            *count <= RESTORE_ATTEMPTS_MAX,
            "no terminal is tried past the attempts"
        );
        *count
    }

    fn retain(&mut self, unsettled: impl Fn(ViewId) -> bool) {
        self.failures.retain(|terminal, _| unsettled(*terminal));
        assert!(
            self.failures.len() <= VIEWS_MAX,
            "no more failures than terminals"
        );
    }
}

fn run(mut layouts: Layouts, commands: &Receiver<Command>, sweep: Duration) {
    let mut swept = Instant::now();
    loop {
        let restoring = !layouts.restore_round.is_empty();
        let wait = if restoring {
            Duration::ZERO
        } else {
            sweep.saturating_sub(swept.elapsed())
        };
        match commands.recv_timeout(wait) {
            Ok(command) => layouts.heard(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                layouts.rehome_spotted();
                layouts.flushed();
                return;
            }
        }
        layouts.rehome_spotted();
        if swept.elapsed() >= sweep {
            layouts.read_runs();
            layouts.rehome_due();
            layouts.flushed();
            swept = Instant::now();
        }
        layouts.unrun();
        layouts.restore();
        layouts.type_due();
        layouts.settle();
        layouts.tell_core();
        layouts.troubles.done();
    }
}

impl Layouts {
    fn heard(&mut self, command: Command) {
        match command {
            Command::Change { change, answer } => {
                let alone = |made| (made, None);
                let changed = match change {
                    Ask::Change(change) => self.changed_opening(change),
                    Ask::CloseWorktree { worktree, seen } => {
                        self.close_worktree(worktree, &seen).map(alone)
                    }
                    Ask::CloseProject {
                        project,
                        worktrees,
                        seen,
                    } => self.close_project(project, &worktrees, &seen).map(alone),
                    Ask::Bring { worktree, from } => {
                        self.bring(worktree, from).map(|made| alone(Some(made)))
                    }
                    Ask::CloseView(terminal) => self.close_view(terminal).map(alone),
                    Ask::OpenBrowser { target, url } => self.open_browser(target, url),
                    Ask::ReviveView => self.revive_view(),
                };
                let kept = self.store.revision();
                let _ = answer.send(changed.map(|(made, opened)| Changed { made, opened, kept }));
            }
            Command::Came {
                worktree,
                size,
                runs_in,
                again,
                asked_number,
                out,
                answer,
            } => {
                let _ = answer.send(self.came(worktree, size, runs_in, again, asked_number, out));
            }
            Command::Asked {
                client,
                asked_number,
                asked,
            } => {
                let Some(held) = self.clients.get_mut(&client) else {
                    return;
                };
                let stranded = matches!(held.place, ClientPlace::Stranded { .. });
                let heard_ok = held.heard.hear(asked_number);
                if stranded {
                    self.asked_stranded(client, asked_number, heard_ok.and(asked));
                    return;
                }
                let event = match heard_ok
                    .and(asked)
                    .and_then(|asked| self.asked(client, asked))
                {
                    Ok(Done::Quiet) => return,
                    Ok(Done::Edged { direction, step }) => {
                        let (worktree, _) = self.clients[&client]
                            .showing()
                            .expect("a showing client edged");
                        let direction = direction_said(direction).into();
                        let step = step.get();
                        let edged = show_response::Edged {
                            worktree: worktree.get(),
                            direction,
                            step,
                        };
                        show_response::Event::Edged(edged)
                    }
                    Err(why) => show_response::Event::Refused(why),
                };
                let held = self.client(client);
                let heard = held.heard.last();
                let word = api::ShowResponse {
                    event: Some(event),
                    heard: AskNumber::wire(heard),
                };
                if told(&held.out, word) {
                    held.echoed = heard;
                } else {
                    self.clients.remove(&client);
                }
            }
            Command::Left(client) => {
                self.clients.remove(&client);
            }
            Command::Listed(listed) => self.heard_listed(listed),
            Command::Emptied { since } => {
                let none = TerminalsListed {
                    terminals: BTreeMap::new(),
                    whole: true,
                    since,
                };
                self.heard_listed(none);
                self.known.clear();
                self.listed.clear();
            }
            Command::Crashed => {
                self.crashed(|_| true);
                self.looked = true;
                self.known.clear();
                self.listed.clear();
            }
            Command::Worktrees { root, listed, seen } => {
                let listed = Change::Listed {
                    root: root.clone(),
                    listed,
                    seen,
                };
                if let Err(why) = self.followed(listed) {
                    let root = root.as_str();
                    self.troubles
                        .say(format!("{root}: its worktrees are not kept: {why}"));
                }
            }
            Command::Root { project, answer } => {
                let _ = answer.send(self.root(project));
            }
            Command::ListResume {
                terminal_id,
                answer,
            } => {
                let _ = answer.send(self.list_resume(terminal_id));
            }
            Command::ReloadResume { answer } => {
                let _ = answer.send(self.reload_resume());
            }
            Command::WorktreeFolder { worktree, answer } => {
                let _ = answer.send(self.worktree_folder(worktree));
            }
            Command::Rooted { root, listed } => {
                let rooted = Change::Rooted {
                    root: root.clone(),
                    listed,
                };
                if let Err(why) = self.followed(rooted) {
                    let root = root.as_str();
                    self.troubles
                        .say(format!("{root}: it is not kept as a clone: {why}"));
                    return;
                }
                let Some(projects) = self.store.projects() else {
                    return;
                };
                let held = projects.all().iter().find(|it| *it.dir() == root);
                let layouts = held.into_iter().flat_map(Project::worktrees);
                let layouts = layouts.map(Worktree::layout);
                let terminals = layouts.flat_map(Layout::terminals).map(Terminal::id);
                for terminal in terminals {
                    self.homes.mark(terminal);
                }
            }
            Command::Unreached { since } => {
                let gone = |known: &Known| match known.heard {
                    Heard::Made(made) => made < since,
                    Heard::Seen => true,
                };
                self.crashed(gone);
                self.looked = true;
                self.known.retain(|_, known| !gone(known));
                self.listed.clear();
            }
            Command::Socket(command) => self.socket_command(command),
        }
    }

    fn socket_command(&mut self, command: SocketCommand) {
        assert!(
            self.clients.len() <= CLIENTS_MAX,
            "layouts tracks at most CLIENTS_MAX clients"
        );
        match command {
            SocketCommand::CurrentPane {
                tty,
                cwd,
                runtime,
                answer,
            } => {
                let _ =
                    answer.send(self.socket_current_pane(tty.as_deref(), cwd.as_deref(), &runtime));
            }
            SocketCommand::ListPanes {
                tty,
                runtime,
                answer,
            } => {
                let _ = answer.send(self.socket_list_panes(tty.as_deref(), &runtime));
            }
            SocketCommand::Neighbor {
                from,
                direction,
                answer,
            } => {
                let _ = answer.send(self.socket_neighbor(from, direction));
            }
            SocketCommand::Split {
                from,
                direction,
                action,
                answer,
            } => {
                let _ = answer.send(self.socket_split(from, direction, action));
            }
            SocketCommand::TerminalKey { pane, answer } => {
                let _ = answer.send(self.socket_terminal_key(pane));
            }
            SocketCommand::FocusPane { pane, answer } => {
                let _ = answer.send(self.socket_focus_pane(pane));
            }
        }
    }

    fn socket_current_pane(
        &self,
        tty: Option<&str>,
        cwd: Option<&str>,
        runtime: &Path,
    ) -> Result<Option<PaneRef>, String> {
        assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
        let projects = self.kept()?;
        assert!(
            self.known.len() <= VIEWS_MAX,
            "known terminals stay within VIEWS_MAX"
        );
        if let Some(tty) = tty {
            if let Some(view) = terminal_socket::view_from_browser_tty(runtime, tty)
                && let Some((worktree, _)) = projects.holding_browser(view)
            {
                return Ok(Some(PaneRef {
                    id: view,
                    tab: worktree.id(),
                }));
            }
            for (terminal, known) in &self.known {
                if self
                    .listed
                    .get(&known.key)
                    .and_then(|listed| listed.tty.as_deref())
                    == Some(tty)
                    && let Some((worktree, _)) = projects.holding_view(terminal.view())
                {
                    return Ok(Some(PaneRef {
                        id: terminal.view(),
                        tab: worktree.id(),
                    }));
                }
            }
        }
        if let Some(cwd) = cwd {
            for (terminal, known) in &self.known {
                if self
                    .listed
                    .get(&known.key)
                    .and_then(|listed| listed.cwd.as_ref())
                    .is_some_and(|listed_cwd| listed_cwd.as_str() == cwd)
                    && let Some((worktree, _)) = projects.holding_view(terminal.view())
                {
                    return Ok(Some(PaneRef {
                        id: terminal.view(),
                        tab: worktree.id(),
                    }));
                }
            }
        }
        Ok(None)
    }

    fn socket_list_panes(
        &self,
        tty: Option<&str>,
        runtime: &Path,
    ) -> Result<Vec<PaneInfo>, String> {
        assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
        let projects = self.kept()?;
        let scoped = self
            .socket_current_pane(tty, None, runtime)?
            .map(|pane| pane.tab);
        let mut panes = Vec::new();
        for (_, worktree) in projects.worktrees() {
            if scoped.is_some_and(|tab| tab != worktree.id()) {
                continue;
            }
            for view in worktree.layout().views() {
                let (id, tty, command) = match view {
                    View::Terminal(terminal) => {
                        let listed = self
                            .known
                            .get(&terminal.id())
                            .and_then(|known| self.listed.get(&known.key));
                        let tty = listed.and_then(|it| it.tty.clone());
                        let command = listed.and_then(|it| match &it.is {
                            Is::Running {
                                front: Some(front), ..
                            } => front.argv.first().cloned(),
                            Is::Running { front: None, .. } | Is::Ended => None,
                        });
                        (terminal.id().view(), tty, command)
                    }
                    View::Browser(browser) => (
                        browser.id(),
                        self.live_browser_tty(worktree.id(), browser.id(), tty, runtime),
                        Some("terminal-browser".to_owned()),
                    ),
                    View::Welcome(id) | View::Review(id) => (*id, None, None),
                };
                panes.push(PaneInfo {
                    id,
                    tab: worktree.id(),
                    tty,
                    command,
                });
            }
        }
        assert!(
            panes.len() <= VIEWS_MAX,
            "listed panes never exceed VIEWS_MAX"
        );
        Ok(panes)
    }

    fn live_browser_tty(
        &self,
        worktree: WorktreeId,
        view: ViewId,
        caller_tty: Option<&str>,
        runtime: &Path,
    ) -> Option<String> {
        let valid = |registered: &str| {
            terminal_socket::view_from_browser_tty(runtime, registered) == Some(view)
        };
        let matching_ttys = caller_tty.and_then(|caller| {
            self.clients
                .values()
                .filter_map(|client| client.browser_ttys(worktree))
                .find(|ttys| ttys.values().any(|it| it == caller))
        });
        if let Some(ttys) = matching_ttys
            && let Some(registered) = ttys.get(&view)
            && valid(registered)
        {
            return Some(registered.clone());
        }
        self.clients
            .values()
            .filter_map(|client| client.browser_ttys(worktree))
            .find_map(|ttys| {
                let registered = ttys.get(&view)?;
                valid(registered).then(|| registered.clone())
            })
    }

    fn socket_neighbor(
        &self,
        from: ViewId,
        direction: Direction,
    ) -> Result<Option<PaneRef>, String> {
        let projects = self.kept()?;
        let Some((worktree, _)) = projects.holding_view(from) else {
            return Ok(None);
        };
        let shown = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree.id()));
        let size = fit(shown, worktree.layout(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let Some(neighbor) = worktree.layout().beside(from, direction, size) else {
            return Ok(None);
        };
        assert_ne!(neighbor, from, "a view's neighbour is another view");
        assert!(
            worktree.layout().holds(neighbor),
            "the neighbour belongs to the same worktree"
        );
        Ok(Some(PaneRef {
            id: neighbor,
            tab: worktree.id(),
        }))
    }

    fn socket_split(
        &mut self,
        from: ViewId,
        direction: Option<Direction>,
        action: SplitAction,
    ) -> Result<(), String> {
        let projects = self.kept()?;
        let Some((worktree_ref, _)) = projects.holding_view(from) else {
            return Err(format!("no layout holds view {from}"));
        };
        let worktree = worktree_ref.id();
        let shown = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree));
        let size =
            fit(shown, worktree_ref.layout(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let preferred = match direction {
            Some(Direction::Left | Direction::Right) => Beside::Towards(Towards::Right),
            Some(Direction::Up | Direction::Down) => Beside::Towards(Towards::Down),
            None => Beside::Focused,
        };
        let argv = match action {
            SplitAction::Browser(url) => {
                let view = self
                    .place_browser(
                        worktree,
                        Some(from),
                        preferred,
                        Some(Beside::Focused),
                        url,
                        size,
                    )
                    .map_err(Failed::said)?;
                for client in self.clients.values_mut() {
                    if let ClientPlace::Showing {
                        worktree: shown,
                        focus,
                        ..
                    } = &mut client.place
                        && *shown == worktree
                    {
                        *focus = Some(view);
                    }
                }
                return Ok(());
            }
            SplitAction::Command(argv) => argv,
        };
        let opens = Opens::Command(argv);
        let terminal = self.store.fresh_terminal().map_err(Failed::said)?;
        let projects = self.kept()?;
        let (project, found) = projects.find(worktree).ok_or("the worktree is gone")?;
        let (placed, inner) =
            placement(found.layout(), Some(from), preferred, terminal.view(), size).or_else(
                |_| {
                    placement(
                        found.layout(),
                        Some(from),
                        Beside::Focused,
                        terminal.view(),
                        size,
                    )
                },
            )?;
        let folder = project.folder(worktree).ok_or("its project holds it not")?;
        let cwd = Directory::from(&folder);
        let made = Terminal::new(terminal, folder, opens.runs());
        self.changed(Change::Open {
            worktree,
            view: View::Terminal(made),
            placed,
            size,
        })?;
        self.spawned(terminal, &cwd, &opens, inner)?;
        for client in self.clients.values_mut() {
            if let ClientPlace::Showing {
                worktree: shown,
                focus,
                ..
            } = &mut client.place
                && *shown == worktree
            {
                *focus = Some(terminal.view());
            }
        }
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(terminal.view())),
            "split terminal opens focused"
        );
        Ok(())
    }

    fn socket_terminal_key(&self, pane: ViewId) -> Result<TerminalKey, String> {
        let projects = self.kept()?;
        let Some((_, terminal)) = projects.holding_terminal(pane) else {
            return Err(format!("view {pane} runs no terminal"));
        };
        let Some(known) = self.known.get(&terminal.id()) else {
            return Err(format!("terminal {pane} is not running"));
        };
        assert_eq!(
            terminal.id().view(),
            pane,
            "resolved terminal matches requested pane"
        );
        Ok(known.key)
    }

    fn socket_focus_pane(&mut self, pane: ViewId) -> Result<(), String> {
        let projects = self.kept()?;
        let Some((worktree_ref, _)) = projects.holding_view(pane) else {
            return Err(format!("no layout holds view {pane}"));
        };
        let worktree = worktree_ref.id();
        self.relaid(Relaid::Focus { view: pane })?;
        for client in self.clients.values_mut() {
            if let ClientPlace::Showing {
                worktree: shown,
                focus,
                ..
            } = &mut client.place
                && *shown == worktree
            {
                *focus = Some(pane);
            }
        }
        assert_eq!(
            self.layout(worktree).map(Layout::last_focus),
            Ok(Some(pane)),
            "focused pane becomes the worktree's last focus"
        );
        Ok(())
    }

    fn came(
        &mut self,
        worktree: WorktreeId,
        size: Size,
        runs_in: Option<TerminalKey>,
        again: bool,
        asked_number: Option<AskNumber>,
        out: Out,
    ) -> Result<ClientId, Status> {
        if self.clients.len() >= CLIENTS_MAX {
            let many = format!("{CLIENTS_MAX} clients show layouts, as many as may");
            return Err(Status::resource_exhausted(many));
        }
        let projects = self.kept().map_err(Status::failed_precondition)?;
        let Some((_, shown)) = projects.find(worktree) else {
            return Err(Status::not_found(format!(
                "no worktree has the id {worktree}"
            )));
        };
        let focus = arrival(shown);
        self.arrivals += 1;
        let id = ClientId(self.arrivals);
        let place = ClientPlace::Showing {
            worktree,
            focus,
            browser_ttys: BTreeMap::new(),
            laid_said: None,
        };
        let client = Client {
            size,
            runs_in,
            out,
            heard: ClientAsks::start(asked_number),
            echoed: None,
            place,
        };
        assert!(
            self.clients.insert(id, client).is_none(),
            "a client comes once"
        );
        if !again {
            self.went(worktree);
        }
        Ok(id)
    }

    fn asked_stranded(
        &mut self,
        client: ClientId,
        asked_number: Option<AskNumber>,
        asked: Result<Asked, String>,
    ) {
        let ClientPlace::Stranded { left } = self.clients[&client].place else {
            unreachable!("a stranded client asked");
        };
        let refused = match asked {
            Ok(Asked::Size(size)) if asked_number.is_none() => {
                self.client(client).size = size;
                return;
            }
            Ok(Asked::Size(_)) => "size cannot be awaited while stranded".to_owned(),
            Ok(Asked::Go(to)) => {
                let shown = self.store.projects().and_then(|it| it.find(to));
                let Some((_, shown)) = shown else {
                    return self.refuse_stranded(client, format!("no worktree {to}"));
                };
                let focus = arrival(shown);
                self.client(client).place = ClientPlace::Showing {
                    worktree: to,
                    focus,
                    browser_ttys: BTreeMap::new(),
                    laid_said: None,
                };
                self.went(to);
                return;
            }
            Ok(_) => format!("worktree {left} is gone: go to another"),
            Err(why) => why,
        };
        self.refuse_stranded(client, refused);
    }

    fn refuse_stranded(&mut self, client: ClientId, refused: String) {
        let held = self.client(client);
        let heard = held.heard.last();
        let word = api::ShowResponse {
            event: Some(show_response::Event::Refused(refused)),
            heard: AskNumber::wire(heard),
        };
        if told(&held.out, word) {
            held.echoed = heard;
        } else {
            self.clients.remove(&client);
        }
    }

    fn changed_opening(
        &mut self,
        change: Change,
    ) -> Result<(Option<Made>, Option<projects::Opened>), Failed> {
        let forgot = match &change {
            Change::ForgetProject { project } => Some(*project),
            _ => None,
        };
        let Some(opens) = change.opens() else {
            let made = self.store.change(change)?;
            if let Some(project) = forgot {
                self.forget_closed_project(project);
            }
            return Ok((made, None));
        };
        let found_agent = match &change {
            Change::Pull { agent, .. } => agent.clone(),
            _ => None,
        };
        let held: BTreeSet<ProjectId> = self
            .store
            .projects()
            .map_or_default(|projects| projects.all().iter().map(Project::id).collect());
        let made = self.store.change(change)?;
        let opened = match (made, found_agent) {
            (Some(Made::Worktree(worktree)), _) => {
                self.first_made(worktree, &opens, &held)
                    .map_err(Failed::Unopened)?;
                None
            }
            (Some(Made::Found(worktree)), Some(agent)) => {
                let opened = self.found_opened(worktree, agent);
                Some(opened.map_or_else(projects::Opened::Unopened, projects::Opened::View))
            }
            (Some(Made::Found(_)), None)
            | (Some(Made::Project(_) | Made::Taken(_) | Made::Untaken) | None, _) => None,
        };
        Ok((made, opened))
    }

    fn found_opened(&mut self, worktree: WorktreeId, agent: Argv) -> Result<ViewId, String> {
        let opens = Opens::Command(agent);
        let terminal = self.store.fresh_terminal().map_err(Failed::said)?;
        let projects = self.kept()?;
        let (project, found) = projects
            .find(worktree)
            .expect("a worktree just found is held");
        let shown = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree));
        let size = fit(shown, found.layout(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let view = terminal.view();
        let (placed, inner) = placement(found.layout(), None, Beside::Largest, view, size)?;
        let folder = project
            .folder(worktree)
            .expect("a worktree held has its folder");
        let cwd = Directory::from(&folder);
        let made = Terminal::new(terminal, folder, opens.runs());
        self.changed(Change::Open {
            worktree,
            view: View::Terminal(made),
            placed,
            size,
        })?;
        let unopened = |why| format!("no terminal opens in worktree {worktree}: {why}");
        self.spawned(terminal, &cwd, &opens, inner)
            .map_err(unopened)?;
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(view)),
            "terminal {terminal} opens focused in its worktree"
        );
        Ok(view)
    }

    fn first_made(
        &mut self,
        worktree: WorktreeId,
        opens: &Opens,
        held: &BTreeSet<ProjectId>,
    ) -> Result<(), String> {
        let (project, terminal, folder, inner) = {
            let projects = self.kept()?;
            let (project, made) = projects
                .find(worktree)
                .expect("a worktree just made is held");
            let mut terminals = made.layout().terminals();
            let first = terminals
                .next()
                .expect("a worktree is made with its first terminal");
            assert!(
                terminals.next().is_none(),
                "a worktree is made with one terminal"
            );
            let inner = placed(made.layout(), first.id().view(), UNSHOWN_SIZE);
            let inner = inner.expect("a layout lays out the terminal it holds");
            (project.id(), first.id(), first.folder().clone(), inner)
        };
        let cwd = Directory::from(&folder);
        let Err(failure) = self.spawned(terminal, &cwd, opens, inner) else {
            return Ok(());
        };
        let unopened = format!("no terminal opens in worktree {worktree}: {failure}");
        let projects = match self.kept() {
            Ok(projects) => projects,
            Err(unkept) => return Err(format!("{unopened}, and {unkept}")),
        };
        if projects.holding_view(terminal.view()).is_some() {
            return Err(unopened);
        }
        let made_with_it = !held.contains(&project);
        if made_with_it && let Err(why) = self.followed(Change::ForgetProject { project }) {
            let stays = format!("project {project}, made with worktree {worktree}, stays");
            self.troubles.say(format!("{stays}: {why}"));
            return Err(unopened);
        }
        let projects = self
            .store
            .projects()
            .expect("projects just changed are kept");
        assert!(
            projects.find(worktree).is_none(),
            "worktree {worktree} goes unopened"
        );
        let stays = projects.project_of(project).is_some();
        assert_eq!(
            stays, !made_with_it,
            "project {project} goes if it was made with the worktree"
        );
        Err(unopened)
    }

    fn spawned(
        &mut self,
        terminal: TerminalViewId,
        cwd: &Directory,
        opens: &Opens,
        inner: Size,
    ) -> Result<(), String> {
        let opened = match opens.argv() {
            None => Opened::Shell(&self.shell),
            Some(argv) => Opened::Command {
                program: argv.program(),
                args: argv.args(),
            },
        };
        match self.mux.made(cwd, opened, tag(terminal.view()), inner) {
            Ok(key) => {
                self.made(terminal, key, Some(inner), None);
                Ok(())
            }
            Err(failure) => {
                if let Err(why) = self.followed(Change::Unopen { terminal }) {
                    self.troubles
                        .say(format!("terminal {terminal} stays lost: {why}"));
                } else {
                    self.homes.forget(terminal);
                }
                Err(failure.to_string())
            }
        }
    }

    fn went(&mut self, worktree: WorktreeId) {
        let went = Change::Went {
            worktree,
            at: Used::at(SystemTime::now()),
        };
        if let Err(why) = self.changed(went) {
            self.troubles
                .say(format!("worktree {worktree}'s use is not kept: {why}"));
        }
    }

    fn close_project(
        &mut self,
        project: ProjectId,
        seen_worktrees: &BTreeSet<WorktreeId>,
        seen: &BTreeSet<ViewId>,
    ) -> Result<Option<Made>, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unknown = || Failed::Refused(format!("no project has the id {project}"));
        let held = projects.project_of(project).ok_or_else(unknown)?;
        let worktrees: Vec<WorktreeId> = held.worktrees().map(Worktree::id).collect();
        let layouts = held.worktrees().map(Worktree::layout);
        let terminals = layouts.flat_map(Layout::terminals).map(Terminal::id);
        let terminals: Vec<TerminalViewId> = terminals.collect();
        assert!(
            worktrees.len() <= WORKTREES_MAX,
            "a project holds at most {WORKTREES_MAX}"
        );
        assert!(
            terminals.len() <= VIEWS_MAX,
            "a project holds at most {VIEWS_MAX} views"
        );
        let held_worktrees: BTreeSet<WorktreeId> = worktrees.iter().copied().collect();
        let held_terminals: BTreeSet<ViewId> = terminals.iter().map(|it| it.view()).collect();
        if held_worktrees != *seen_worktrees || held_terminals != *seen {
            let changed = format!("project {project}'s worktrees or terminals are not those seen");
            return Err(Failed::Stale(format!("{changed}: they changed since")));
        }
        let mux_closed = self.close_terminals(&terminals)?;
        let kept = self.store.closed(Closing::Project(project));
        let what = format!("project {project}'s terminals");
        let kept = if mux_closed {
            kept.map_err(half_closed(what))
        } else {
            kept
        };
        if let Some(why) = kept? {
            self.unwritten(&why);
        }
        let projects = self
            .store
            .projects()
            .expect("projects just changed are kept");
        assert!(
            projects.project_of(project).is_none(),
            "project {project} is closed"
        );
        for worktree in &worktrees {
            assert!(
                projects.find(*worktree).is_none(),
                "worktree {worktree} goes with it"
            );
        }
        self.forget_closed_project(project);
        self.forget_closed(&terminals);
        Ok(None)
    }

    fn close_worktree(
        &mut self,
        worktree: WorktreeId,
        seen: &BTreeSet<ViewId>,
    ) -> Result<Option<Made>, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unknown = || Failed::Refused(format!("no worktree has the id {worktree}"));
        let (_, shown) = projects.find(worktree).ok_or_else(unknown)?;
        let terminals: Vec<TerminalViewId> = shown.layout().terminals().map(Terminal::id).collect();
        if terminals
            .iter()
            .map(|it| it.view())
            .collect::<BTreeSet<_>>()
            != *seen
        {
            let changed = format!("worktree {worktree}'s terminals are not the ones seen");
            return Err(Failed::Stale(format!("{changed}: they changed since")));
        }
        let most = LAYOUT_VIEWS_MAX;
        assert!(
            terminals.len() <= most,
            "a layout holds at most {most} terminals"
        );
        if !terminals.is_empty() {
            self.read_runs();
        }
        let closed_snapshots = {
            let projects = self.kept().map_err(Failed::NotKept)?;
            let (project, shown) = projects.find(worktree).ok_or_else(unknown)?;
            self.snapshot_worktree_views(project, shown)
        };
        let mux_closed = self.close_terminals(&terminals)?;
        let kept = self.store.closed(Closing::Worktree(worktree));
        let what = format!("worktree {worktree}'s terminals");
        let kept = if mux_closed {
            kept.map_err(half_closed(what))
        } else {
            kept
        };
        if let Some(why) = kept? {
            self.unwritten(&why);
        }
        for snapshot in closed_snapshots {
            self.push_closed(snapshot);
        }
        for client in self.clients.values_mut() {
            if let ClientPlace::Showing {
                worktree: shown,
                browser_ttys,
                ..
            } = &mut client.place
                && *shown == worktree
            {
                browser_ttys.clear();
            }
        }
        self.forget_closed(&terminals);
        Ok(None)
    }

    fn close_view(&mut self, key: TerminalKey) -> Result<Option<Made>, Failed> {
        let held = self.known.iter().find(|(_, known)| known.key == key);
        let Some((&terminal, _)) = held else {
            let id = key.id;
            return Err(Failed::Gone(format!(
                "no view holds terminal {id} made then"
            )));
        };
        self.close(terminal.view())?;
        assert!(
            !self.known.contains_key(&terminal),
            "terminal {terminal} is known no more"
        );
        Ok(None)
    }

    fn close_terminals(&mut self, terminals: &[TerminalViewId]) -> Result<bool, Failed> {
        assert!(
            terminals.len() <= VIEWS_MAX,
            "at most {VIEWS_MAX} views are kept"
        );
        let unclosed = |failure: Failure| Failed::Unclosed(failure.to_string());
        let unseen = terminals
            .iter()
            .any(|terminal| !self.known.contains_key(terminal));
        let tags = if unseen {
            self.mux.tags().map_err(unclosed)?
        } else {
            Vec::new()
        };
        let mut done: Vec<TerminalViewId> = Vec::with_capacity(terminals.len());
        let mut mux_closed = false;
        for terminal in terminals {
            let known = self.known.get(terminal).map(|known| known.key);
            let found = || {
                let found = tags
                    .iter()
                    .find(|(tag, _)| tagged(tag) == Some(terminal.view()));
                found.map(|(_, key)| *key)
            };
            if let Some(key) = known.or_else(found) {
                if let Err(failure) = self.mux.closed(key) {
                    for closed in done {
                        if let Err(failed) = self.closed(closed.view()) {
                            let why = failed.said();
                            let stays = format!("terminal {closed} closed, and stays: {why}");
                            self.troubles.say(stays);
                        }
                    }
                    return Err(unclosed(failure));
                }
                mux_closed = true;
            }
            done.push(*terminal);
        }
        assert_eq!(
            done, terminals,
            "each terminal is closed in the mux, in order"
        );
        Ok(mux_closed)
    }

    fn forget_closed(&mut self, terminals: &[TerminalViewId]) {
        let views: Vec<ViewId> = terminals.iter().map(|it| it.view()).collect();
        for terminal in terminals {
            self.known.remove(terminal);
            self.resume_synth.remove(*terminal);
            self.homes.forget(*terminal);
        }
        if let Err(why) = self.resume_store.remove_many(&views) {
            self.troubles
                .say(format!("saved resume records were not removed: {why}"));
        }
        let projects = self
            .store
            .projects()
            .expect("projects just changed are kept");
        for terminal in terminals {
            let held = projects.holding_view(terminal.view());
            assert!(held.is_none(), "terminal {terminal} left its layout");
            assert!(
                !self.known.contains_key(terminal),
                "terminal {terminal} is known no more"
            );
        }
    }

    fn bring(&mut self, worktree: WorktreeId, from: TerminalKey) -> Result<Made, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        if projects.find(worktree).is_none() {
            return Err(Failed::Refused(format!(
                "no worktree has the id {worktree}"
            )));
        }
        let held = self.known.iter().find(|(_, known)| known.key == from);
        let Some((&held, _)) = held else {
            return Ok(Made::Untaken);
        };
        let on = self.clients.iter().filter(|(_, it)| {
            matches!(it.place, ClientPlace::Showing { focus, .. } if focus == Some(held.view()))
        });
        let on = on.map(|(id, _)| *id);
        let on: Vec<ClientId> = on.collect();
        let mut taken = false;
        for client in on {
            let it = &self.clients[&client];
            let (shown, _) = it.showing().expect("a client focusing a view is showing");
            let sent = api::ShowResponse {
                event: Some(show_response::Event::Sent(worktree.get())),
                heard: AskNumber::wire(it.heard.last()),
            };
            if shown == worktree || told(&it.out, sent) {
                taken = true;
            } else {
                self.clients.remove(&client);
            }
        }
        Ok(if taken {
            Made::Taken(worktree)
        } else {
            Made::Untaken
        })
    }

    fn root(&self, project: ProjectId) -> Result<Root, Unrooted> {
        let projects = self.kept().map_err(Unrooted::Unkept)?;
        let held = projects.project_of(project).ok_or(Unrooted::Unknown)?;
        let dir = held.dir().clone();
        Ok(match held.holds() {
            Holds::Clone(_) => Root::Clone(dir),
            Holds::Folder(_) => Root::Folder {
                dir,
                name: held.name().clone(),
            },
        })
    }

    fn worktree_folder(&self, worktree: WorktreeId) -> Result<Dir, Unrooted> {
        let projects = self.kept().map_err(Unrooted::Unkept)?;
        let (project, found) = projects.find(worktree).ok_or(Unrooted::Unknown)?;
        project.folder(found.id()).ok_or(Unrooted::Unknown)
    }

    fn kept(&self) -> Result<&Projects, String> {
        match self.store.projects() {
            Some(projects) => Ok(projects),
            None => Err(self
                .store
                .unkept()
                .unwrap_or("no projects are kept")
                .to_owned()),
        }
    }

    fn changed(&mut self, change: Change) -> Result<(), String> {
        self.store.change(change).map(drop).map_err(Failed::said)
    }

    fn relaid(&mut self, relaid: Relaid) -> Result<(), String> {
        self.store.relaid(relaid).map_err(Failed::said)
    }

    fn followed(&mut self, change: Change) -> Result<(), String> {
        if let Some(why) = self.store.followed(change).map_err(Failed::said)? {
            self.unwritten(&why);
        }
        Ok(())
    }

    fn unwritten(&mut self, why: &str) {
        self.troubles.say(format!(
            "the layouts are kept, and written on the next sweep: {why}"
        ));
    }

    fn flushed(&mut self) {
        if let Err(failed) = self.store.flush() {
            self.troubles
                .say(format!("the layouts are not written: {}", failed.said()));
        }
    }

    fn asked(&mut self, client: ClientId, asked: Asked) -> Result<Done, String> {
        let (worktree, focus) = self.clients[&client]
            .showing()
            .expect("a showing client asked");
        let size = self.size_of(client);
        let focused = || focus.ok_or_else(|| "the layout holds no view".to_owned());
        match asked {
            Asked::Size(size) => self.client(client).size = size,
            Asked::Go(to) => {
                let projects = self.kept()?;
                let (_, shown) = projects
                    .find(to)
                    .ok_or_else(|| format!("no worktree {to}"))?;
                let focus = arrival(shown);
                let ClientPlace::Showing {
                    worktree,
                    focus: focused,
                    browser_ttys,
                    ..
                } = &mut self.client(client).place
                else {
                    unreachable!("a showing client goes to another worktree");
                };
                *worktree = to;
                *focused = focus;
                browser_ttys.clear();
                self.went(to);
            }
            Asked::Open { beside, opens } => self.open(client, beside, &opens)?,
            Asked::Welcome => self.welcome(client)?,
            Asked::Browser(url) => self.browser(client, url)?,
            Asked::Navigate { view, url } => {
                if !self.layout(worktree)?.holds(view) {
                    return Ok(Done::Quiet);
                }
                if self.kept()?.holding_browser(view).is_none() {
                    return Err(format!("view {view} runs no browser"));
                }
                self.changed(Change::Navigate { view, url })?;
            }
            Asked::BrowserTty { view, tty } => {
                if !self.layout(worktree)?.holds(view) {
                    return Ok(Done::Quiet);
                }
                if self.kept()?.holding_browser(view).is_none() {
                    return Err(format!("view {view} runs no browser"));
                }
                let ClientPlace::Showing { browser_ttys, .. } = &mut self.client(client).place
                else {
                    unreachable!("a showing client updates browser tty");
                };
                match tty {
                    Some(tty) => {
                        if !terminal_socket::is_browser_client_tty(&tty, view) {
                            return Err(format!(
                                "browser_tty: {tty:?} is no browser tty for view {view}"
                            ));
                        }
                        browser_ttys.insert(view, tty);
                    }
                    None => {
                        browser_ttys.remove(&view);
                    }
                }
            }
            Asked::Review => self.review(client)?,
            Asked::Close(view) => {
                if !self.layout(worktree)?.holds(view) {
                    return Ok(Done::Quiet);
                }
                self.close(view).map_err(Failed::said)?;
            }
            Asked::Focus(Focus::View(view)) => {
                if !self.layout(worktree)?.holds(view) {
                    return Err(format!("the layout holds no view {view}"));
                }
                self.focus(client, view)?;
            }
            Asked::Focus(Focus::Toward { direction, step }) => {
                let layout = self.layout(worktree)?;
                let beside = focus.and_then(|from| layout.beside(from, direction, size));
                let Some(to) = beside else {
                    return Ok(Done::Edged { direction, step });
                };
                self.focus(client, to)?;
            }
            Asked::Focus(Focus::Order(order)) => {
                let from = focused()?;
                let to = self.stepped(worktree, from, Step::Order(order), size)?;
                self.focus(client, to)?;
            }
            Asked::Resize { sides, growth } => {
                let Some(view) = focus else {
                    return Ok(Done::Quiet);
                };
                if self.layout(worktree)?.resizes(view, sides, growth, size) {
                    self.relaid(Relaid::Resize {
                        view,
                        sides,
                        growth,
                        size,
                    })?;
                }
            }
            Asked::Move(step) => {
                let one = focused()?;
                let other = self.stepped(worktree, one, step, size)?;
                self.changed(Change::Swap { one, other })?;
            }
            Asked::Fullscreen => {
                let full = match self.layout(worktree)?.full() {
                    Some(_) => None,
                    None => Some(focused()?),
                };
                self.relaid(Relaid::Fill { worktree, full })?;
            }
            Asked::Arrange(arranged) => {
                let Some(shown) = self.layout(worktree)?.arranged() else {
                    return Ok(Done::Quiet);
                };
                if shown == arranged {
                    return Ok(Done::Quiet);
                }
                self.changed(Change::Arrange { worktree, arranged })?;
            }
        }
        Ok(Done::Quiet)
    }

    fn client(&mut self, client: ClientId) -> &mut Client {
        self.clients
            .get_mut(&client)
            .expect("a client asked is held")
    }

    fn layout(&self, worktree: WorktreeId) -> Result<&Layout, String> {
        let projects = self.kept()?;
        let found = projects
            .find(worktree)
            .ok_or_else(|| format!("no worktree {worktree}"));
        let (_, found) = found?;
        Ok(found.layout())
    }

    fn stepped(
        &self,
        worktree: WorktreeId,
        from: ViewId,
        step: Step,
        size: Size,
    ) -> Result<ViewId, String> {
        let layout = self.layout(worktree)?;
        let tree = layout.tree().ok_or("the layout holds no view")?;
        let to = match step {
            Step::Direction(direction) => layout.beside(from, direction, size),
            Step::Order(order) => tree.in_order(from, order),
        };
        to.ok_or_else(|| "no other terminal is there".to_owned())
    }

    fn focus(&mut self, client: ClientId, view: ViewId) -> Result<(), String> {
        let (worktree, _) = self.clients[&client]
            .showing()
            .expect("a showing client focuses");
        self.relaid(Relaid::Focus { view })?;
        let ClientPlace::Showing { focus, .. } = &mut self.client(client).place else {
            unreachable!("a showing client focuses");
        };
        *focus = Some(view);
        let layout = self.layout(worktree)?;
        assert_eq!(
            layout.last_focus(),
            Some(view),
            "view {view} is its worktree's last focus"
        );
        let full = layout.full();
        assert!(
            full.is_none_or(|full| full == view),
            "a focus on view {view} tiles the rest"
        );
        Ok(())
    }

    fn size_of(&self, client: ClientId) -> Size {
        let client = &self.clients[&client];
        let (worktree, _) = client.showing().expect("a showing client has a size");
        let size = client.size;
        let Some((_, found)) = self.store.projects().and_then(|it| it.find(worktree)) else {
            return size;
        };
        let shown = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree));
        fit(shown, found.layout(), &self.known).map_or(size, |fit| fit.size)
    }

    fn open(&mut self, client: ClientId, beside: Beside, opens: &Opens) -> Result<(), String> {
        let (worktree, focus) = self.clients[&client]
            .showing()
            .expect("a showing client opens");
        let size = self.size_of(client);
        let terminal = self.store.fresh_terminal().map_err(Failed::said)?;
        let projects = self.kept()?;
        let (project, shown) = projects.find(worktree).ok_or("the worktree is gone")?;
        if beside == Beside::Alone && !shown.layout().is_empty() {
            return Ok(());
        }
        let (placed, inner) = placement(shown.layout(), focus, beside, terminal.view(), size)?;
        let home = project
            .folder(shown.id())
            .ok_or("its project holds it not")?;
        let listed_cwd = focus
            .and_then(|focus| projects.holding_terminal(focus))
            .and_then(|(_, focused)| self.known.get(&focused.id()))
            .and_then(|known| self.listed.get(&known.key))
            .and_then(|listed| listed.cwd.as_ref())
            .and_then(|cwd| Dir::parse(cwd.as_str()).ok())
            .filter(|cwd| home.below(cwd).is_some());
        let folder = listed_cwd.unwrap_or(home);
        let cwd = Directory::from(&folder);
        let made = Terminal::new(terminal, folder, opens.runs());
        self.changed(Change::Open {
            worktree,
            view: View::Terminal(made),
            placed,
            size,
        })?;
        self.spawned(terminal, &cwd, opens, inner)?;
        if let ClientPlace::Showing { focus, .. } = &mut self.client(client).place {
            *focus = Some(terminal.view());
        }
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(terminal.view())),
            "terminal {terminal} opens focused"
        );
        Ok(())
    }

    fn welcome(&mut self, client: ClientId) -> Result<(), String> {
        let (worktree, focus) = self.clients[&client]
            .showing()
            .expect("a showing client welcomes");
        let size = self.size_of(client);
        let view = self.store.fresh().map_err(Failed::said)?;
        let projects = self.kept()?;
        let (_, shown) = projects.find(worktree).ok_or("the worktree is gone")?;
        let beside = if focus.is_some() {
            Beside::Focused
        } else {
            Beside::Largest
        };
        let (placed, _) = placement(shown.layout(), focus, beside, view, size)?;
        self.changed(Change::Open {
            worktree,
            view: View::Welcome(view),
            placed,
            size,
        })?;
        let projects = self.kept()?;
        let held_in = projects.holding_view(view).map(|(it, _)| it.id());
        assert_eq!(
            held_in,
            Some(worktree),
            "welcome view {view} opened in its worktree"
        );
        assert!(
            projects.holding_terminal(view).is_none(),
            "welcome view {view} runs no terminal"
        );
        if let ClientPlace::Showing { focus, .. } = &mut self.client(client).place {
            *focus = Some(view);
        }
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(view)),
            "welcome view {view} opens focused"
        );
        Ok(())
    }

    fn place_browser(
        &mut self,
        worktree: WorktreeId,
        focus: Option<ViewId>,
        preferred: Beside,
        fallback: Option<Beside>,
        url: Option<BrowserUrl>,
        size: Size,
    ) -> Result<ViewId, Failed> {
        let view = self.store.fresh()?;
        let projects = self.kept().map_err(Failed::NotKept)?;
        let (_, shown) = projects
            .find(worktree)
            .ok_or_else(|| Failed::Refused(format!("no worktree has the id {worktree}")))?;
        let placed = match placement(shown.layout(), focus, preferred, view, size) {
            Ok((placed, _)) => placed,
            Err(why) => match fallback {
                Some(fallback) => {
                    placement(shown.layout(), focus, fallback, view, size)
                        .map_err(Failed::Refused)?
                        .0
                }
                None => return Err(Failed::Refused(why)),
            },
        };
        let browser = View::Browser(Browser::new(view, url));
        self.store.change(Change::Open {
            worktree,
            view: browser,
            placed,
            size,
        })?;
        let projects = self.kept().map_err(Failed::NotKept)?;
        let held_in = projects.holding_browser(view).map(|(it, _)| it.id());
        assert_eq!(
            held_in,
            Some(worktree),
            "browser view {view} opened in its worktree"
        );
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(view)),
            "browser view {view} opens focused"
        );
        Ok(view)
    }

    fn browser(&mut self, client: ClientId, url: Option<BrowserUrl>) -> Result<(), String> {
        let (worktree, focus) = self.clients[&client]
            .showing()
            .expect("a showing client browses");
        let size = self.size_of(client);
        let beside = if focus.is_some() {
            Beside::Focused
        } else {
            Beside::Largest
        };
        let view = self
            .place_browser(worktree, focus, beside, None, url, size)
            .map_err(Failed::said)?;
        if let ClientPlace::Showing { focus, .. } = &mut self.client(client).place {
            *focus = Some(view);
        }
        assert_eq!(
            self.clients[&client].showing(),
            Some((worktree, Some(view))),
            "client focuses opened browser"
        );
        Ok(())
    }

    fn open_browser(
        &mut self,
        target: BrowserTarget,
        url: Option<BrowserUrl>,
    ) -> Result<(Option<Made>, Option<projects::Opened>), Failed> {
        let known_view = |key: TerminalKey| {
            self.known
                .iter()
                .find(|(_, known)| known.key == key)
                .map(|(it, _)| it.view())
        };
        let (resolved, from_key) = match target {
            BrowserTarget::Worktree { worktree, from } => {
                let projects = self.kept().map_err(Failed::NotKept)?;
                let in_same = from.and_then(known_view).filter(|view| {
                    projects
                        .holding_view(*view)
                        .is_some_and(|(held, _)| held.id() == worktree)
                });
                (Ok((worktree, in_same)), from)
            }
            BrowserTarget::From { from, fallback } => {
                let projects = self.kept().map_err(Failed::NotKept)?;
                let found = known_view(from).and_then(|view| {
                    projects
                        .holding_view(view)
                        .map(|(held, _)| (held.id(), Some(view)))
                });
                match (found, fallback) {
                    (Some(existing), _) => (Ok(existing), Some(from)),
                    (None, Some(spot)) => (Err(spot), Some(from)),
                    (None, None) => {
                        return Err(Failed::Refused(
                            "no worktree holds that terminal".to_owned(),
                        ));
                    }
                }
            }
            BrowserTarget::Folder(spot) => (Err(spot), None),
        };
        let (worktree, view, created) = match resolved {
            Ok((worktree, beside_view)) => {
                let view = self.open_browser_in(worktree, beside_view, url)?;
                (worktree, view, false)
            }
            Err(spot) => {
                let existing = self
                    .kept()
                    .map_err(Failed::NotKept)?
                    .existing_worktree_for(&spot);
                if let Some(worktree) = existing {
                    let view = self.open_browser_in(worktree, None, url)?;
                    (worktree, view, false)
                } else {
                    let made = self.store.change(Change::StartBrowser { spot, url })?;
                    let Some(Made::Worktree(worktree)) = made else {
                        unreachable!("StartBrowser for an unmade spot returns Made::Worktree");
                    };
                    let projects = self.kept().map_err(Failed::NotKept)?;
                    let (_, shown) = projects
                        .find(worktree)
                        .expect("a worktree just started for a browser is held");
                    let view = shown
                        .layout()
                        .last_focus()
                        .expect("a started browser worktree focuses its new browser view");
                    assert!(
                        projects.holding_browser(view).is_some(),
                        "started browser view {view} is a browser"
                    );
                    (worktree, view, true)
                }
            }
        };
        let made = match from_key {
            Some(from_key) => self.bring(worktree, from_key)?,
            None if created => Made::Worktree(worktree),
            None => Made::Found(worktree),
        };
        for client in self.clients.values_mut() {
            if let ClientPlace::Showing {
                worktree: shown,
                focus,
                ..
            } = &mut client.place
                && *shown == worktree
            {
                *focus = Some(view);
            }
        }
        Ok((Some(made), Some(projects::Opened::View(view))))
    }

    fn open_browser_in(
        &mut self,
        worktree: WorktreeId,
        beside_view: Option<ViewId>,
        url: Option<BrowserUrl>,
    ) -> Result<ViewId, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let (_, shown) = projects
            .find(worktree)
            .ok_or_else(|| Failed::Refused(format!("no worktree has the id {worktree}")))?;
        let shown_clients = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree));
        let size =
            fit(shown_clients, shown.layout(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let focus = beside_view.or_else(|| shown.layout().last_focus());
        let beside = if focus.is_some() {
            Beside::Focused
        } else {
            Beside::Largest
        };
        self.place_browser(worktree, focus, beside, None, url, size)
    }

    fn review(&mut self, client: ClientId) -> Result<(), String> {
        let (worktree, focus) = self.clients[&client]
            .showing()
            .expect("a showing client reviews");
        let size = self.size_of(client);
        let view = self.store.fresh().map_err(Failed::said)?;
        let projects = self.kept()?;
        let (_, shown) = projects.find(worktree).ok_or("the worktree is gone")?;
        let beside = if focus.is_some() {
            Beside::Focused
        } else {
            Beside::Largest
        };
        let (placed, _) = placement(shown.layout(), focus, beside, view, size)?;
        self.changed(Change::Open {
            worktree,
            view: View::Review(view),
            placed,
            size,
        })?;
        let projects = self.kept()?;
        let held_in = projects.holding_view(view).map(|(it, _)| it.id());
        assert_eq!(
            held_in,
            Some(worktree),
            "review view {view} opened in its worktree"
        );
        assert!(
            projects.holding_terminal(view).is_none(),
            "review view {view} runs no terminal"
        );
        if let ClientPlace::Showing { focus, .. } = &mut self.client(client).place {
            *focus = Some(view);
        }
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(view)),
            "review view {view} opens focused"
        );
        Ok(())
    }
    fn made(
        &mut self,
        terminal: TerminalViewId,
        key: TerminalKey,
        sized: Option<Size>,
        remade: Option<Remade>,
    ) {
        let heard_none = !self.known.values().any(|known| known.heard == Heard::Seen);
        let heard = Heard::Made(Instant::now());
        let known = Known {
            key,
            heard,
            settled: sized,
            remade,
        };
        assert!(
            self.known.insert(terminal, known).is_none(),
            "terminal {terminal} is made once"
        );
        if heard_none {
            match self.follower.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => {}
                Err(TrySendError::Disconnected(())) => {
                    self.troubles
                        .say("the mux's terminals are followed no more".to_owned());
                }
            }
        }
    }

    fn crashed(&mut self, gone: impl Fn(&Known) -> bool) {
        let now = Instant::now();
        let Layouts {
            known,
            restore_attempts,
            troubles,
            ..
        } = self;
        for (terminal, known) in known.iter().filter(|(_, known)| gone(known)) {
            let after = known
                .remade
                .as_ref()
                .map(|it| now.saturating_duration_since(it.at));
            match after {
                Some(after) if after < RESTORE_SETTLE => {
                    troubles.say(restore_attempts.crashed(terminal.view(), after));
                }
                Some(_) | None => restore_attempts.settled(terminal.view()),
            }
        }
    }

    fn restore(&mut self) {
        if !self.looked {
            return;
        }
        if self.restore_round.is_empty() {
            self.restore_begin();
        }
        self.restore_some();
    }

    fn restore_begin(&mut self) {
        assert!(
            self.restore_round.is_empty(),
            "a round begins once the last one is done"
        );
        if self
            .restore_last_at
            .is_some_and(|at| at.elapsed() < RESTORE_PAUSE)
        {
            return;
        }
        let Some(projects) = self.store.projects() else {
            return;
        };
        let known = &self.known;
        let held = |terminal| projects.holding_terminal(terminal).is_some();
        let settled = |terminal: ViewId| {
            let terminal = projects.holding_terminal(terminal).map(|(_, it)| it.id());
            terminal
                .and_then(|terminal| known.get(&terminal))
                .is_some_and(|known| {
                    known
                        .remade
                        .as_ref()
                        .is_none_or(|it| it.at.elapsed() >= RESTORE_SETTLE)
                })
        };
        self.restore_attempts
            .retain(|terminal| held(terminal) && !settled(terminal));
        let attempts = &self.restore_attempts;
        let wanted = |terminal: &TerminalViewId| {
            !known.contains_key(terminal) && attempts.wanted(terminal.view())
        };
        let ids = || projects.terminals().map(|(_, terminal)| terminal.id());
        if !ids().any(|terminal| wanted(&terminal)) {
            return;
        }
        let lost: BTreeMap<ViewId, TerminalViewId> = ids()
            .filter(wanted)
            .map(|terminal| (terminal.view(), terminal))
            .collect();
        assert!(!lost.is_empty(), "a round begins with a terminal lost");
        self.restore_last_at = Some(Instant::now());
        let tags = match self.mux.tags() {
            Ok(tags) => tags,
            Err(failure) => {
                for terminal in lost.keys() {
                    let said = self
                        .restore_attempts
                        .failed(*terminal, &failure.to_string());
                    self.troubles.say(said);
                }
                return;
            }
        };
        let mut adopted = BTreeSet::new();
        for (tag, key) in tags {
            let Some(&terminal) = tagged(&tag).and_then(|view| lost.get(&view)) else {
                continue;
            };
            if self.known.values().any(|known| known.key == key) || !adopted.insert(terminal) {
                continue;
            }
            self.made(terminal, key, None, None);
        }
        let lost: BTreeSet<TerminalViewId> = lost.into_values().collect();
        assert!(adopted.is_subset(&lost), "only a lost terminal is adopted");
        let keys: BTreeSet<TerminalKey> = self.known.values().map(|known| known.key).collect();
        assert_eq!(
            keys.len(),
            self.known.len(),
            "no terminal adopted shares a key"
        );
        self.restore_round = lost.difference(&adopted).copied().collect();
    }

    fn restore_some(&mut self) {
        let mut remade = 0;
        while remade < RESTORES_PER_PASS {
            let Some(terminal) = self.restore_round.pop_first() else {
                break;
            };
            if self.known.contains_key(&terminal) {
                continue;
            }
            let held = |it: &Projects| it.holding_terminal(terminal.view()).is_some();
            let held = self.store.projects().is_some_and(held);
            if !held {
                continue;
            }
            remade += 1;
            if let Err(why) = self.remade(terminal) {
                let said = self.restore_attempts.failed(terminal.view(), &why);
                self.troubles.say(said);
            }
        }
        assert!(
            remade <= RESTORES_PER_PASS,
            "a pass restores no more than its share"
        );
    }

    fn remade(&mut self, terminal: TerminalViewId) -> Result<(), String> {
        let projects = self.kept()?;
        let held = projects.holding_terminal(terminal.view());
        let (held_in, held) = held.ok_or("no layout holds it")?;
        let (project, worktree) = projects.find(held_in.id()).expect("a layout's worktree is");
        let worktree_folder = project
            .folder(worktree.id())
            .ok_or("its project holds it not")?;
        let kept_folder = held.folder().clone();
        let runs = held.runs().clone();
        let layout = worktree.layout();
        let shown = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(held_in.id()));
        let size = fit(shown, layout, &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let inner = placed(layout, terminal.view(), size);
        let inner = inner.expect("a layout lays out the terminals it holds");
        let (folder, changed_folder) = if is_folder(&kept_folder)? {
            (kept_folder, false)
        } else if is_folder(&worktree_folder)? {
            (worktree_folder, true)
        } else {
            let (kept, theirs) = (kept_folder.as_str(), worktree_folder.as_str());
            return Err(format!(
                "neither {kept} nor its worktree's {theirs} is a folder"
            ));
        };
        let stored = self.resume_store.get(terminal.view()).cloned();
        self.spawn_restored(
            terminal,
            &folder,
            changed_folder,
            &runs,
            inner,
            stored.as_ref(),
        )
    }

    fn spawn_restored(
        &mut self,
        terminal: TerminalViewId,
        folder: &Dir,
        changed_folder: bool,
        runs: &Runs,
        inner: Size,
        stored: Option<&StoredResume>,
    ) -> Result<(), String> {
        assert!(
            !self.known.contains_key(&terminal),
            "restored terminal {terminal} is not already known"
        );
        let tagged = tag(terminal.view());

        if let Some(stored) = stored {
            return self.remade_saved(
                terminal,
                folder,
                changed_folder,
                runs,
                inner,
                tagged,
                stored,
            );
        }

        let cwd = Directory::from(folder);
        let args: Vec<String>;
        let mut reset_to_shell = false;
        let (opened, restore) = match runs {
            Runs::Shell => (Opened::Shell(&self.shell), None),
            Runs::Command(line) | Runs::Unrun(line) => {
                let restore = if line.as_str().contains('\n') {
                    let lines = format!("terminal {terminal}'s last command spans lines");
                    self.troubles
                        .say(format!("{lines}, and is kept, not typed"));
                    None
                } else {
                    let now = Instant::now();
                    let (due, by) = (now + TYPING_WAIT, now + TYPING_WAIT_MAX);
                    Some(Restored::Typing {
                        line: line.clone(),
                        due,
                        by,
                    })
                };
                (Opened::Shell(&self.shell), restore)
            }
            Runs::Agent { harness, session } => {
                let allowed = self.resume_store.unkept().is_none()
                    && resume::allows_legacy_agent(
                        &self.resume_config,
                        *harness,
                        session.as_str(),
                        folder.as_str(),
                        resume::running_as_root(),
                    );
                if allowed {
                    let plain = Ran {
                        terminal,
                        folder: folder.clone(),
                        runs: Runs::Shell,
                    };
                    if let Err(why) = self.changed(Change::Ran(vec![plain])) {
                        let program = escape_for_display(harness.program());
                        log::say(&format!(
                            "terminal {terminal}: not resuming saved program {program}, starting a shell (reason=store_unwritten: {why})"
                        ));
                        (Opened::Shell(&self.shell), None)
                    } else {
                        args = vec![harness.resumes().to_owned(), session.as_str().to_owned()];
                        let program = harness.program();
                        let restore = Some(Restored::Resuming {
                            deadline: Instant::now() + RESTORE_SETTLE,
                        });
                        (
                            Opened::Command {
                                program,
                                args: &args,
                            },
                            restore,
                        )
                    }
                } else {
                    let program = escape_for_display(harness.program());
                    log::say(&format!(
                        "terminal {terminal}: not resuming saved program {program}, starting a shell (reason=not_trusted)"
                    ));
                    reset_to_shell = true;
                    (Opened::Shell(&self.shell), None)
                }
            }
        };
        let key = self
            .mux
            .made(&cwd, opened, tagged, inner)
            .map_err(|it| it.to_string())?;
        let remade = Remade {
            at: Instant::now(),
            restored: restore,
        };
        self.made(terminal, key, Some(inner), Some(remade));
        assert_eq!(
            self.known.get(&terminal).map(|it| it.key),
            Some(key),
            "a remade one is known"
        );
        if changed_folder {
            self.homes.forget(terminal);
        }
        if reset_to_shell {
            let plain = Ran {
                terminal,
                folder: folder.clone(),
                runs: Runs::Shell,
            };
            if let Err(why) = self.changed(Change::Ran(vec![plain])) {
                self.troubles.say(format!(
                    "terminal {terminal}'s shell state is not kept: {why}"
                ));
            }
        } else if let Runs::Command(line) = runs {
            let unrun = Ran {
                terminal,
                folder: folder.clone(),
                runs: Runs::Unrun(line.clone()),
            };
            if let Err(why) = self.changed(Change::Ran(vec![unrun])) {
                self.troubles.say(format!(
                    "terminal {terminal}'s line is not kept as unrun: {why}"
                ));
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "remade_saved carries the prepared terminal restore fields"
    )]
    fn remade_saved(
        &mut self,
        terminal: TerminalViewId,
        folder: &Dir,
        changed_folder: bool,
        runs: &Runs,
        inner: Size,
        tagged: Vec<u8>,
        stored: &StoredResume,
    ) -> Result<(), String> {
        let decision = decide_saved_resume(stored, &self.resume_config, &self.shell, folder);
        let (spawn_folder, opened, restore, clear_store) = match decision {
            SavedChoice::Resume {
                argv,
                cwd,
                mode,
                rule,
            } => {
                if let Err(why) = self.resume_store.mark_tried(terminal.view()) {
                    let program = escape_for_display(argv.program());
                    log::say(&format!(
                        "terminal {terminal}: not resuming saved program {program}, starting a shell (reason=store_unwritten: {why})"
                    ));
                    (folder.clone(), SavedOpen::Shell, None, false)
                } else {
                    let program = escape_for_display(argv.program());
                    log::say(&format!(
                        "terminal {terminal}: resuming saved program {program} (rule {rule})"
                    ));
                    let open = match mode {
                        SavedResumeMode::Root => SavedOpen::Root(argv),
                        SavedResumeMode::InShell(kind) => SavedOpen::InShell { kind, argv },
                    };
                    let restore = Some(Restored::Resuming {
                        deadline: Instant::now() + RESTORE_SETTLE,
                    });
                    (cwd, open, restore, false)
                }
            }
            SavedChoice::Skip { reason, program } => {
                let program = escape_for_display(&program);
                log::say(&format!(
                    "terminal {terminal}: not resuming saved program {program}, starting a shell (reason={reason})"
                ));
                (folder.clone(), SavedOpen::Shell, None, true)
            }
        };
        let cwd = Directory::from(&spawn_folder);
        let opened = match &opened {
            SavedOpen::Shell => Opened::Shell(&self.shell),
            SavedOpen::Root(argv) => Opened::Command {
                program: argv.program(),
                args: argv.args(),
            },
            SavedOpen::InShell { kind, argv } => Opened::ResumeInShell {
                shell: &self.shell,
                kind: *kind,
                argv,
            },
        };
        let key = self
            .mux
            .made(&cwd, opened, tagged, inner)
            .map_err(|it| it.to_string())?;
        let remade = Remade {
            at: Instant::now(),
            restored: restore,
        };
        self.made(terminal, key, Some(inner), Some(remade));
        assert_eq!(
            self.known.get(&terminal).map(|it| it.key),
            Some(key),
            "a remade one is known"
        );
        if changed_folder || spawn_folder != *folder {
            self.homes.forget(terminal);
        }
        if clear_store && let Err(why) = self.resume_store.remove(terminal.view()) {
            self.troubles
                .say(format!("terminal {terminal}'s resume record: {why}"));
        }
        if *runs != Runs::Shell {
            let plain = Ran {
                terminal,
                folder: spawn_folder,
                runs: Runs::Shell,
            };
            if let Err(why) = self.changed(Change::Ran(vec![plain])) {
                self.troubles.say(format!(
                    "terminal {terminal}'s shell state is not kept: {why}"
                ));
            }
        }
        Ok(())
    }

    fn unrun(&mut self) {
        let Layouts {
            known,
            errands,
            troubles,
            ..
        } = self;
        let mut owners: Option<BTreeMap<TerminalKey, TerminalViewId>> = None;
        for _ in 0..errands::ERRANDS_MAX {
            let Some(unrun) = errands.unrun() else {
                return;
            };
            match unrun {
                Unrun::Sized { key, size, why } => {
                    let owners = owners.get_or_insert_with(|| {
                        known
                            .iter()
                            .map(|(terminal, known)| (known.key, *terminal))
                            .collect()
                    });
                    let Some(known) = owners.get(&key).and_then(|it| known.get_mut(it)) else {
                        continue;
                    };
                    if known.settled == Some(size) {
                        known.settled = None;
                    }
                    troubles.say(format!("terminal {} is not sized: {why}", key.id));
                }
                Unrun::Pasted { terminals, why } => troubles.say(untyped(&terminals, &why)),
            }
        }
    }

    fn type_due(&mut self) {
        let Layouts {
            known,
            listed,
            errands,
            troubles,
            ..
        } = self;
        let now = Instant::now();
        let mut pastes_due: Vec<(TerminalViewId, TerminalKey, Line)> = Vec::new();
        let mut given_up_by: Vec<(TerminalViewId, Instant)> = Vec::new();
        for (terminal, known) in known.iter_mut() {
            let Some(remade) = &mut known.remade else {
                continue;
            };
            let Some(Restored::Typing { line, due, by }) = &remade.restored else {
                continue;
            };
            let is = listed.get(&known.key).map(|listed| &listed.is);
            match pasted(is, *due, *by, now) {
                Paste::Later => continue,
                Paste::Now => {
                    pastes_due.push((*terminal, known.key, line.clone()));
                    given_up_by.push((*terminal, *by));
                    continue;
                }
                Paste::Never => {
                    let untyped = format!("terminal {terminal}'s last command is not typed");
                    troubles.say(format!(
                        "{untyped}, as it ran or runs another program first"
                    ));
                }
            }
            remade.restored = None;
        }
        assert!(
            pastes_due.len() <= known.len(),
            "a paste is due only where one is restored"
        );
        if pastes_due.is_empty() {
            return;
        }
        assert_eq!(
            given_up_by.len(),
            pastes_due.len(),
            "each paste due has its last moment"
        );
        let terminals: Vec<TerminalViewId> = pastes_due.iter().map(|(it, ..)| *it).collect();
        let taken = errands.paste(pastes_due);
        for (terminal, by) in given_up_by {
            if taken || now >= by {
                let remade = known
                    .get_mut(&terminal)
                    .and_then(|known| known.remade.as_mut());
                remade
                    .expect("a paste due is of a terminal remade")
                    .restored = None;
            }
        }
        if !taken {
            troubles.say(untyped(&terminals, errands::UNTAKEN));
        }
    }

    fn close(&mut self, view: ViewId) -> Result<(), Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unheld = || Failed::Refused(format!("no layout holds the view {view}"));
        let (_, held) = projects.holding_view(view).ok_or_else(unheld)?;
        let View::Terminal(terminal) = held else {
            return self.closed(view);
        };
        let terminal_id = terminal.id();
        if self.known.contains_key(&terminal_id) {
            self.read_runs();
        }
        let key = match self.known.get(&terminal_id) {
            Some(known) => Some(known.key),
            None => self.mux.tagged(&tag(view)).map_err(|failure| {
                let unlooked = format!("terminal {view} is not looked for");
                Failed::Unclosed(format!("{unlooked}: {failure}"))
            })?,
        };
        if let Some(key) = key {
            self.mux
                .closed(key)
                .map_err(|failure| Failed::Unclosed(failure.to_string()))?;
            return self
                .closed(view)
                .map_err(half_closed(format!("terminal {view}")));
        }
        self.closed(view)
    }

    fn closed(&mut self, view: ViewId) -> Result<(), Failed> {
        self.closed_with(view, None)
    }

    fn closed_with(
        &mut self,
        view: ViewId,
        last_listed: Option<&server::Terminal>,
    ) -> Result<(), Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let unheld = || Failed::Refused(format!("no layout holds the view {view}"));
        let (held_in, held_view) = projects.holding_view(view).ok_or_else(unheld)?;
        let (project, _) = projects
            .find(held_in.id())
            .expect("a worktree holding a view belongs to a project");
        let tree = held_in
            .layout()
            .tree()
            .expect("a layout that holds a view is laid");
        let heir = tree.heir(view);
        let terminal = held_view.terminal().map(Terminal::id);
        let snapshot = self.snapshot_closed(project, held_in, held_view, last_listed);
        if let Some(why) = self.store.followed(Change::Close { view })? {
            self.unwritten(&why);
        }
        self.push_closed(snapshot);
        if let Some(terminal) = terminal {
            self.known.remove(&terminal);
            self.resume_synth.remove(terminal);
            self.homes.forget(terminal);
            if let Err(why) = self.resume_store.remove(view) {
                self.troubles
                    .say(format!("terminal {terminal}'s resume record: {why}"));
            }
        }
        for client in self.clients.values_mut() {
            if let ClientPlace::Showing {
                focus,
                browser_ttys,
                ..
            } = &mut client.place
            {
                browser_ttys.remove(&view);
                if *focus == Some(view) {
                    *focus = heir;
                }
            }
        }
        let projects = self.kept().map_err(Failed::NotKept)?;
        assert!(
            projects.holding_view(view).is_none(),
            "view {view} left its layout"
        );
        if let Some(heir) = heir {
            assert!(
                projects.holding_view(heir).is_some(),
                "view {heir} took the room it left"
            );
        }
        let focused = self.clients.values().any(|client| {
            matches!(client.place, ClientPlace::Showing { focus, .. } if focus == Some(view))
        });
        assert!(!focused, "no client focuses view {view}, which closed");
        Ok(())
    }

    fn push_closed(&mut self, closed: ClosedView) {
        assert!(
            self.closed_views.len() <= CLOSED_VIEWS_MAX,
            "closed view history stays within CLOSED_VIEWS_MAX before push"
        );
        if self.closed_views.len() == CLOSED_VIEWS_MAX {
            self.closed_views.pop_front();
        }
        self.closed_views.push_back(closed);
        assert!(
            !self.closed_views.is_empty(),
            "pushing a closed view leaves history non-empty"
        );
        assert!(
            self.closed_views.len() <= CLOSED_VIEWS_MAX,
            "closed view history stays within CLOSED_VIEWS_MAX"
        );
    }

    fn forget_closed_project(&mut self, project: ProjectId) {
        assert!(
            self.closed_views.len() <= CLOSED_VIEWS_MAX,
            "closed view history stays within CLOSED_VIEWS_MAX before forget"
        );
        self.closed_views
            .retain(|entry| entry.place.project != project);
        assert!(
            self.closed_views
                .iter()
                .all(|entry| entry.place.project != project),
            "forgotten project {project} keeps no closed views"
        );
    }

    fn snapshot_worktree_views(&self, project: &Project, shown: &Worktree) -> Vec<ClosedView> {
        assert!(
            project.worktrees().any(|it| it.id() == shown.id()),
            "worktree {} belongs to project {}",
            shown.id(),
            project.id()
        );
        let focus = shown.layout().last_focus();
        let mut views: Vec<&View> = shown.layout().views().collect();
        assert!(
            views.len() <= LAYOUT_VIEWS_MAX,
            "a worktree holds at most {LAYOUT_VIEWS_MAX} views"
        );
        if let Some(focused) = focus {
            let focused_index = views
                .iter()
                .position(|it| layout::Leaf::id(*it) == focused)
                .expect("last_focus is a view in the layout");
            let focused_view = views.remove(focused_index);
            views.push(focused_view);
        }
        let snapshots: Vec<ClosedView> = views
            .iter()
            .map(|view| self.snapshot_closed(project, shown, view, None))
            .collect();
        assert_eq!(
            snapshots.len(),
            views.len(),
            "every view in the worktree is snapshotted"
        );
        snapshots
    }

    fn snapshot_closed(
        &self,
        project: &Project,
        worktree: &Worktree,
        held: &View,
        last_listed: Option<&server::Terminal>,
    ) -> ClosedView {
        let view_id = layout::Leaf::id(held);
        assert!(
            worktree.layout().holds(view_id),
            "snapshotted view {view_id} belongs to worktree {}",
            worktree.id()
        );
        let subfolder = project.subfolder(worktree.id()).cloned();
        assert_eq!(
            subfolder.is_some(),
            matches!(project.holds(), Holds::Clone(_)),
            "clone worktrees have a subfolder and plain folders do not"
        );
        let arranged = worktree
            .layout()
            .arranged()
            .expect("a layout holding a view is arranged");
        let place = ClosedPlace {
            project: project.id(),
            worktree: worktree.id(),
            subfolder,
            worktree_name: worktree.renamed().cloned(),
            arranged,
        };
        let kind = match held {
            View::Terminal(terminal) => {
                let known = self.known.get(&terminal.id());
                let listed = last_listed.or_else(|| known.and_then(|it| self.listed.get(&it.key)));
                let restored = known
                    .and_then(|it| it.remade.as_ref())
                    .and_then(|it| it.restored.as_ref());
                let TerminalSnapshot { folder, runs, .. } =
                    terminal_runs_snapshot(terminal, restored, listed, &self.shell);
                let still_resuming = matches!(restored, Some(Restored::Resuming { .. }));
                let at_shell = listed.is_some() && runs == Runs::Shell && !still_resuming;
                let resume = choose_closed_resume(
                    rustix::process::getuid().as_raw(),
                    listed,
                    at_shell,
                    self.resume_synth.get(terminal.id()),
                    self.resume_store.get(terminal.id().view()),
                );
                ClosedKind::Terminal(Box::new(ClosedTerminal {
                    folder,
                    runs,
                    name: terminal.name().cloned(),
                    resume,
                }))
            }
            View::Browser(browser) => ClosedKind::Browser {
                url: browser.url().cloned(),
            },
            View::Review(_) => ClosedKind::Review,
            View::Welcome(_) => ClosedKind::Welcome,
        };
        ClosedView { place, kind }
    }

    fn revive_view(&mut self) -> Result<(Option<Made>, Option<projects::Opened>), Failed> {
        let _ = self.kept().map_err(Failed::NotKept)?;
        assert!(
            self.closed_views.len() <= CLOSED_VIEWS_MAX,
            "closed view history stays within CLOSED_VIEWS_MAX before revive"
        );
        let mut folder_cache = BTreeMap::new();
        for _ in 0..CLOSED_VIEWS_MAX {
            let Some(entry) = self.closed_views.pop_back() else {
                return Err(Failed::Refused("nothing closed to revive".to_owned()));
            };
            let prepared = match self.resolve_revive_target(&entry, &mut folder_cache) {
                Ok(TargetResolution::Ready(prepared)) => prepared,
                Ok(TargetResolution::Gone) => continue,
                Err(failed) => {
                    self.push_closed(entry);
                    return Err(failed);
                }
            };
            match self.revive_entry(prepared) {
                Ok((made, view)) => {
                    let worktree = match made {
                        Made::Worktree(worktree) | Made::Found(worktree) => worktree,
                        other => unreachable!("revive returns a worktree, not {other:?}"),
                    };
                    for client in self.clients.values_mut() {
                        if let ClientPlace::Showing {
                            worktree: shown,
                            focus,
                            ..
                        } = &mut client.place
                            && *shown == worktree
                        {
                            *focus = Some(view);
                        }
                    }
                    self.went(worktree);
                    return Ok((Some(made), Some(projects::Opened::View(view))));
                }
                Err(ReviveFailure::Requeue(failed)) => {
                    self.push_closed(entry);
                    return Err(failed);
                }
                Err(ReviveFailure::KeptInLayout(failed)) => {
                    return Err(failed);
                }
            }
        }
        assert!(
            self.closed_views.is_empty(),
            "reviving checked every closed view"
        );
        Err(Failed::Refused("nothing closed to revive".to_owned()))
    }

    fn resolve_revive_target(
        &self,
        entry: &ClosedView,
        folder_cache: &mut BTreeMap<Dir, bool>,
    ) -> Result<TargetResolution, Failed> {
        let projects = self.kept().map_err(Failed::NotKept)?;
        let place = &entry.place;
        let (worktree_folder, target) =
            if let Some((project, shown)) = projects.find(place.worktree) {
                let folder = project
                    .folder(place.worktree)
                    .expect("a held worktree has a folder");
                (
                    folder,
                    self.prepared_existing(place.worktree, shown, place.arranged),
                )
            } else {
                let Some(project) = projects.project_of(place.project) else {
                    return Ok(TargetResolution::Gone);
                };
                match (project.holds(), &place.subfolder) {
                    (Holds::Folder(own), subfolder) => {
                        assert!(
                            subfolder.is_none(),
                            "a plain folder project has no clone subfolder"
                        );
                        (
                            project.dir().clone(),
                            self.prepared_existing(own.id(), own, place.arranged),
                        )
                    }
                    (Holds::Clone(_), Some(subfolder)) => {
                        if let Some(existing) = project.worktree_at(subfolder) {
                            let (_, shown) = projects
                                .find(existing)
                                .expect("a clone worktree at subfolder is held");
                            let folder = project
                                .folder(existing)
                                .expect("a held worktree has a folder");
                            (
                                folder,
                                self.prepared_existing(existing, shown, place.arranged),
                            )
                        } else {
                            let folder = project.dir().at(subfolder);
                            if !is_folder_cached(&folder, folder_cache).map_err(Failed::Refused)? {
                                return Ok(TargetResolution::Gone);
                            }
                            (
                                folder,
                                PreparedTarget::ReopenClone {
                                    project: project.id(),
                                    subfolder: subfolder.clone(),
                                    name: place.worktree_name.clone(),
                                    arranged: place.arranged,
                                },
                            )
                        }
                    }
                    (Holds::Clone(_), None) => return Ok(TargetResolution::Gone),
                }
            };

        let kind = match &entry.kind {
            ClosedKind::Terminal(closed_terminal) => {
                let ClosedTerminal {
                    folder,
                    runs,
                    name,
                    resume,
                } = closed_terminal.as_ref();
                let (spawn_folder, changed_folder) = if is_folder_cached(folder, folder_cache)
                    .map_err(Failed::Refused)?
                {
                    (folder.clone(), false)
                } else if folder != &worktree_folder
                    && is_folder_cached(&worktree_folder, folder_cache).map_err(Failed::Refused)?
                {
                    (worktree_folder, true)
                } else {
                    return Ok(TargetResolution::Gone);
                };
                PreparedKind::Terminal(Box::new(PreparedTerminal {
                    spawn_folder,
                    changed_folder,
                    runs: runs.clone(),
                    name: name.clone(),
                    resume: resume.clone(),
                }))
            }
            ClosedKind::Browser { url } => PreparedKind::Browser { url: url.clone() },
            ClosedKind::Review => PreparedKind::Review,
            ClosedKind::Welcome => PreparedKind::Welcome,
        };

        Ok(TargetResolution::Ready(PreparedRevive { target, kind }))
    }

    fn prepared_existing(
        &self,
        worktree: WorktreeId,
        shown: &Worktree,
        arranged: Arranged,
    ) -> PreparedTarget {
        assert_eq!(
            shown.id(),
            worktree,
            "prepared existing target matches its worktree"
        );
        let shown_clients = self
            .clients
            .values()
            .filter(|it| it.showing_worktree(worktree));
        let size =
            fit(shown_clients, shown.layout(), &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let focus = shown.layout().last_focus();
        let was_empty = shown.layout().is_empty();
        if was_empty {
            assert!(
                focus.is_none(),
                "an empty worktree layout has no focused view"
            );
        }
        PreparedTarget::Existing {
            worktree,
            size,
            focus,
            was_empty,
            arranged,
        }
    }

    fn revive_entry(&mut self, prepared: PreparedRevive) -> Result<(Made, ViewId), ReviveFailure> {
        let mut terminal_spawn: Option<(TerminalViewId, Dir, bool, Runs, Option<StoredResume>)> =
            None;
        let view = match prepared.kind {
            PreparedKind::Terminal(prepared_terminal) => {
                let PreparedTerminal {
                    spawn_folder,
                    changed_folder,
                    runs,
                    name,
                    resume,
                } = *prepared_terminal;
                let terminal = self
                    .store
                    .fresh_terminal()
                    .map_err(ReviveFailure::Requeue)?;
                let made = Terminal::new(terminal, spawn_folder.clone(), runs.clone()).named(name);
                terminal_spawn = Some((terminal, spawn_folder, changed_folder, runs, resume));
                View::Terminal(made)
            }
            PreparedKind::Browser { url } => {
                let id = self.store.fresh().map_err(ReviveFailure::Requeue)?;
                View::Browser(Browser::new(id, url))
            }
            PreparedKind::Review => {
                View::Review(self.store.fresh().map_err(ReviveFailure::Requeue)?)
            }
            PreparedKind::Welcome => {
                View::Welcome(self.store.fresh().map_err(ReviveFailure::Requeue)?)
            }
        };
        let view_id = layout::Leaf::id(&view);

        let (made, worktree, inner) = match prepared.target {
            PreparedTarget::Existing {
                worktree,
                size,
                focus,
                was_empty,
                arranged,
            } => {
                let projects = self
                    .kept()
                    .map_err(Failed::NotKept)
                    .map_err(ReviveFailure::Requeue)?;
                let (_, shown) = projects.find(worktree).ok_or_else(|| {
                    ReviveFailure::Requeue(Failed::Refused(format!(
                        "no worktree has the id {worktree}"
                    )))
                })?;
                let (placed, inner) = if focus.is_some() {
                    placement(shown.layout(), focus, Beside::Focused, view_id, size).or_else(|_| {
                        placement(shown.layout(), focus, Beside::Largest, view_id, size)
                    })
                } else {
                    placement(shown.layout(), None, Beside::Largest, view_id, size)
                }
                .map_err(Failed::Refused)
                .map_err(ReviveFailure::Requeue)?;
                self.store
                    .change(Change::Open {
                        worktree,
                        view,
                        placed,
                        size,
                    })
                    .map_err(ReviveFailure::Requeue)?;
                if was_empty
                    && arranged != Arranged::Tabs
                    && let Err(why) = self.changed(Change::Arrange { worktree, arranged })
                {
                    self.troubles.say(format!(
                        "worktree {worktree}'s arrangement is not kept: {why}"
                    ));
                }
                (Made::Found(worktree), worktree, inner)
            }
            PreparedTarget::ReopenClone {
                project,
                subfolder,
                name,
                arranged,
            } => {
                let made = self
                    .store
                    .change(Change::ReviveWorktree {
                        project,
                        subfolder,
                        name,
                        arranged,
                        view,
                    })
                    .map_err(ReviveFailure::Requeue)?;
                let Some(Made::Worktree(worktree)) = made else {
                    unreachable!("ReviveWorktree returns Made::Worktree");
                };
                (
                    Made::Worktree(worktree),
                    worktree,
                    layout::lone(UNSHOWN_SIZE),
                )
            }
        };

        if let Some((terminal, spawn_folder, changed_folder, runs, resume)) = terminal_spawn {
            let stored_resume = match resume {
                Some(saved) => {
                    let our_uid = rustix::process::getuid().as_raw();
                    assert_eq!(
                        saved.record.uid(),
                        our_uid,
                        "revived resume record belongs to current user"
                    );
                    match self.resume_store.upsert(
                        terminal.view(),
                        saved.source,
                        saved.record.clone(),
                    ) {
                        Ok(()) => Some(saved),
                        Err(why) => {
                            self.troubles
                                .say(format!("terminal {terminal}'s resume record: {why}"));
                            None
                        }
                    }
                }
                None => None,
            };
            if let Err(why) = self.spawn_restored(
                terminal,
                &spawn_folder,
                changed_folder,
                &runs,
                inner,
                stored_resume.as_ref(),
            ) {
                let unopened = self.followed(Change::Unopen { terminal });
                if let Err(rollback_why) = &unopened {
                    self.troubles
                        .say(format!("terminal {terminal} stays lost: {rollback_why}"));
                } else {
                    self.homes.forget(terminal);
                }
                if stored_resume.is_some()
                    && let Err(remove_why) = self.resume_store.remove(terminal.view())
                {
                    self.troubles
                        .say(format!("terminal {terminal}'s resume record: {remove_why}"));
                }
                let still_held = self
                    .store
                    .projects()
                    .is_some_and(|projects| projects.holding_view(view_id).is_some());
                if unopened.is_ok() {
                    assert!(!still_held, "unopened terminal {terminal} left its layout");
                }
                let failure =
                    Failed::Unopened(format!("no terminal opens in worktree {worktree}: {why}"));
                return Err(if still_held {
                    ReviveFailure::KeptInLayout(failure)
                } else {
                    ReviveFailure::Requeue(failure)
                });
            }
        }

        let projects = self
            .kept()
            .map_err(Failed::NotKept)
            .map_err(ReviveFailure::KeptInLayout)?;
        let held_in = projects.holding_view(view_id).map(|(it, _)| it.id());
        assert_eq!(
            held_in,
            Some(worktree),
            "revived view {view_id} opened in its worktree"
        );
        let last_focus = self.layout(worktree).map(Layout::last_focus);
        assert_eq!(
            last_focus,
            Ok(Some(view_id)),
            "revived view {view_id} opens focused"
        );
        Ok((made, view_id))
    }

    fn heard_listed(&mut self, listed: TerminalsListed) {
        let TerminalsListed {
            terminals,
            whole,
            since,
        } = listed;
        self.looked |= whole;
        for known in self.known.values_mut() {
            if let Heard::Made(made) = known.heard
                && made < since
            {
                known.heard = Heard::Seen;
            }
        }
        let Some(projects) = self.store.projects() else {
            self.listed = terminals;
            return;
        };
        let mut found: BTreeMap<TerminalViewId, (TerminalKey, Option<Size>)> = BTreeMap::new();
        for (&key, listed) in &terminals {
            let Some(view) = tagged(&listed.tag) else {
                continue;
            };
            let Some((_, held)) = projects.holding_terminal(view) else {
                let id = key.id;
                let unheld = format!("terminal {id} is tagged {view}, no terminal of a layout");
                self.troubles.say(unheld);
                continue;
            };
            let terminal = held.id();
            if found.contains_key(&terminal) {
                let why = format!("two terminals are tagged {terminal}; the first is kept");
                self.troubles.say(why);
                continue;
            }
            found.insert(terminal, (key, listed.size));
        }
        let ended: Vec<TerminalViewId> = if whole {
            let gone = |(terminal, known): &(&TerminalViewId, &Known)| {
                !found.contains_key(*terminal) && known.heard == Heard::Seen
            };
            self.known
                .iter()
                .filter(gone)
                .map(|(terminal, _)| *terminal)
                .collect()
        } else {
            let unread = "a listing is not read whole, and so no terminal is ended";
            self.troubles.say(unread.to_owned());
            Vec::new()
        };
        let before_keys: BTreeMap<TerminalViewId, TerminalKey> = self
            .known
            .iter()
            .map(|(terminal, known)| (*terminal, known.key))
            .collect();
        let owners: BTreeMap<TerminalKey, TerminalViewId> = found
            .iter()
            .map(|(terminal, (key, _))| (*key, *terminal))
            .collect();
        self.known
            .retain(|terminal, known| owners.get(&known.key).is_none_or(|owner| owner == terminal));
        for (terminal, (key, size)) in found {
            match self.known.get_mut(&terminal) {
                Some(known) if known.key == key => {
                    known.heard = Heard::Seen;
                    if size.is_some() {
                        known.settled = size;
                    }
                }
                Some(_) | None => {
                    let heard = Heard::Seen;
                    let (settled, remade) = (size, None);
                    let known = Known {
                        key,
                        heard,
                        settled,
                        remade,
                    };
                    self.known.insert(terminal, known);
                }
            }
        }
        let keys: BTreeSet<TerminalKey> = self.known.values().map(|known| known.key).collect();
        assert_eq!(
            keys.len(),
            self.known.len(),
            "no two terminals share a key of lune-mux"
        );
        let before = std::mem::replace(&mut self.listed, terminals);
        for terminal in ended {
            let last = before_keys.get(&terminal).and_then(|key| before.get(key));
            if let Err(failed) = self.closed_with(terminal.view(), last) {
                let stays = format!("terminal {terminal} ended, and stays in its layout");
                self.troubles.say(format!("{stays}: {}", failed.said()));
            }
        }
        if runs_changed(&self.known, &before_keys, &before, &self.listed) {
            self.read_runs();
        }
    }

    fn read_runs(&mut self) {
        if self.listed.is_empty() {
            return;
        }
        let agents = core::ask(&self.core, Core::AllAgents).unwrap_or_default();
        let now = Instant::now();
        let live_terminals: Vec<LiveTerminal> = self
            .known
            .iter()
            .filter_map(|(&terminal, known)| {
                let listed = self.listed.get(&known.key)?;
                let Is::Running {
                    root,
                    front,
                    resumes,
                    ..
                } = &listed.is
                else {
                    return None;
                };
                Some(LiveTerminal {
                    terminal,
                    key: known.key,
                    root_pid: root.get(),
                    front_pid: front.as_ref().map(|f| f.pid.get()),
                    has_osc_7577: resumes.is_some(),
                })
            })
            .collect();
        self.resume_synth
            .refresh(&live_terminals, &agents, &self.resume_config, now);
        let Layouts {
            store,
            known,
            listed,
            shell,
            resume_store,
            resume_synth,
            troubles,
            ..
        } = self;
        let Some(projects) = store.projects() else {
            return;
        };
        let mut ran = Vec::new();
        let mut elsewhere = Vec::new();
        for (terminal, known) in known.iter_mut() {
            let Some(listed) = listed.get(&known.key) else {
                continue;
            };
            let Some((_, held)) = projects.holding_terminal(terminal.view()) else {
                continue;
            };
            let chosen_osc = match &listed.is {
                Is::Running {
                    resumes: Some(resumes),
                    ..
                } => Some(resumes.chosen().clone()),
                Is::Running { resumes: None, .. } | Is::Ended => None,
            };
            let chosen_synth = resume_synth.get(*terminal).cloned();
            let has_live_resume = chosen_osc.is_some() || chosen_synth.is_some();
            if let Some(remade) = &mut known.remade
                && let Some(Restored::Resuming { deadline }) = &remade.restored
                && (has_live_resume || now >= *deadline)
            {
                remade.restored = None;
            }
            let still_resuming = matches!(
                known
                    .remade
                    .as_ref()
                    .and_then(|remade| remade.restored.as_ref()),
                Some(Restored::Resuming { .. })
            );
            let store_result = if let Some(record) = chosen_osc {
                resume_store.upsert(terminal.view(), RecordSource::Osc7577, record)
            } else if let Some(record) = chosen_synth {
                resume_store.upsert(terminal.view(), RecordSource::Synthesized, record)
            } else if !still_resuming {
                resume_store.remove(terminal.view())
            } else {
                Ok(())
            };
            if let Err(why) = store_result {
                troubles.say(format!(
                    "terminal {terminal}'s resume record is not kept: {why}"
                ));
            }
            let restored = known
                .remade
                .as_ref()
                .and_then(|remade| remade.restored.as_ref());
            let TerminalSnapshot {
                folder,
                runs,
                keeps_restored,
            } = terminal_runs_snapshot(held, restored, Some(listed), shell);
            if !keeps_restored && let Some(remade) = &mut known.remade {
                remade.restored = None;
            }
            if folder != *held.folder() {
                elsewhere.push(*terminal);
            }
            if folder != *held.folder() || runs != *held.runs() {
                ran.push(Ran {
                    terminal: *terminal,
                    folder,
                    runs,
                });
            }
        }
        assert!(
            ran.len() <= VIEWS_MAX,
            "a read runs no more terminals than are held"
        );
        if ran.is_empty() {
            return;
        }
        if let Err(why) = self.changed(Change::Ran(ran)) {
            self.troubles
                .say(format!("what the terminals run is not kept: {why}"));
            return;
        }
        for terminal in elsewhere {
            self.homes.mark(terminal);
        }
    }

    fn reload_resume(&mut self) -> api::ReloadResumeResponse {
        let response = resume::reload(&mut self.resume_config);
        if let Some(api::reload_resume_response::Outcome::Failed(failed)) = &response.outcome {
            log::say(&format!("resume config reload failed: {}", failed.error));
        }
        for warning in &response.warnings {
            log::say(&format!("resume config: {warning}"));
        }
        response
    }

    fn list_resume(&self, terminal_id: Option<NonZeroU32>) -> api::ListResumeResponse {
        let (config_path, config_warnings, config) = resume::config_to_api(&self.resume_config);
        let as_root = resume::running_as_root();
        let mut terminals = Vec::new();
        let mut seen_keys = BTreeSet::new();

        if let Some(projects) = self.store.projects() {
            for (_, terminal) in projects.terminals() {
                let known = self.known.get(&terminal.id());
                let mux_key = known.map(|it| it.key);
                if let Some(key) = mux_key {
                    seen_keys.insert(key);
                }
                if let Some(want) = terminal_id
                    && mux_key.is_none_or(|key| key.id != want)
                {
                    continue;
                }
                let listed = mux_key.and_then(|key| self.listed.get(&key));
                let folder = listed
                    .and_then(|l| l.cwd.as_ref())
                    .and_then(|cwd| Dir::parse(cwd.as_str()).ok())
                    .unwrap_or_else(|| terminal.folder().clone());
                let stored = self.resume_store.get(terminal.id().view());
                let osc_resumes = listed.and_then(|listed| match &listed.is {
                    Is::Running { resumes, .. } => resumes.as_deref(),
                    Is::Ended => None,
                });
                let mut records = Vec::new();
                if let Some(resumes) = osc_resumes {
                    records.push(evaluate_resume_record(
                        RecordSource::Osc7577,
                        resumes.chosen(),
                        true,
                        stored,
                        &self.resume_config,
                        as_root,
                        Some(folder.as_str()),
                    ));
                    for record in resumes.others() {
                        records.push(evaluate_resume_record(
                            RecordSource::Osc7577,
                            record,
                            false,
                            stored,
                            &self.resume_config,
                            as_root,
                            Some(folder.as_str()),
                        ));
                    }
                } else if let Some(synth) = self.resume_synth.get(terminal.id()) {
                    records.push(evaluate_resume_record(
                        RecordSource::Synthesized,
                        synth,
                        true,
                        stored,
                        &self.resume_config,
                        as_root,
                        Some(folder.as_str()),
                    ));
                } else if let Some(stored) = stored {
                    records.push(evaluate_resume_record(
                        stored.source,
                        &stored.record,
                        true,
                        Some(stored),
                        &self.resume_config,
                        as_root,
                        Some(folder.as_str()),
                    ));
                }
                if records.is_empty() && terminal_id.is_none() {
                    continue;
                }
                terminals.push(api::TerminalResumeRecords {
                    owner: Some(api::terminal_resume_records::Owner::Layout(
                        api::terminal_resume_records::LayoutTerminal {
                            view_id: terminal.id().view().get(),
                            terminal: mux_key.map(Into::into),
                        },
                    )),
                    folder: folder.as_str().to_owned(),
                    records,
                });
            }
        }

        for (key, listed) in &self.listed {
            if seen_keys.contains(key) {
                continue;
            }
            if let Some(want) = terminal_id
                && key.id != want
            {
                continue;
            }
            let osc_resumes = match &listed.is {
                Is::Running { resumes, .. } => resumes.as_deref(),
                Is::Ended => None,
            };
            if osc_resumes.is_none() && terminal_id.is_none() {
                continue;
            }
            let fallback_cwd = listed.cwd.as_ref().map(Directory::as_str);
            let mut records = Vec::new();
            if let Some(resumes) = osc_resumes {
                records.push(evaluate_resume_record(
                    RecordSource::Osc7577,
                    resumes.chosen(),
                    true,
                    None,
                    &self.resume_config,
                    as_root,
                    fallback_cwd,
                ));
                for record in resumes.others() {
                    records.push(evaluate_resume_record(
                        RecordSource::Osc7577,
                        record,
                        false,
                        None,
                        &self.resume_config,
                        as_root,
                        fallback_cwd,
                    ));
                }
            }
            terminals.push(api::TerminalResumeRecords {
                owner: Some(api::terminal_resume_records::Owner::Untracked(
                    (*key).into(),
                )),
                folder: fallback_cwd.unwrap_or_default().to_owned(),
                records,
            });
        }

        terminals.sort_by_key(|entry| match &entry.owner {
            Some(api::terminal_resume_records::Owner::Layout(layout)) => (
                layout.terminal.as_ref().map_or(u32::MAX, |k| k.id),
                layout.view_id,
            ),
            Some(api::terminal_resume_records::Owner::Untracked(key)) => (key.id, 0),
            None => (u32::MAX, u64::MAX),
        });
        bound_fair(
            &mut terminals,
            |entry| Some(&mut entry.records),
            |record| record.encoded_len().saturating_add(16),
            LIST_RESUME_RECORDS_BYTES_MAX,
        );
        if terminal_id.is_none() {
            terminals.retain(|entry| !entry.records.is_empty());
        }
        api::ListResumeResponse {
            config_path,
            config_warnings,
            config: Some(config),
            terminals,
        }
    }

    fn rehome_due(&mut self) {
        let Some(projects) = self.store.projects() else {
            self.homes.clear();
            return;
        };
        self.homes.dispatch(projects);
    }

    fn rehome_spotted(&mut self) {
        for _ in 0..homes::LOOKS_MAX {
            let Some(spotted) = self.homes.spotted() else {
                return;
            };
            self.rehome(spotted);
        }
        assert!(
            self.homes.is_idle(),
            "draining every slot empties the homes"
        );
    }

    fn rehome(&mut self, spotted: Spotted) {
        let (terminal, folder, spot) = spotted.into_parts();
        let Some(projects) = self.store.projects() else {
            return;
        };
        let Some((from, held)) = projects.holding_terminal(terminal.view()) else {
            return;
        };
        if *held.folder() != folder {
            return;
        }
        let stays = format!("terminal {terminal}, gone to {}, stays", folder.as_str());
        let spot = match spot {
            Ok(spot) => spot,
            Err(why) => {
                return self
                    .troubles
                    .say(format!("{stays}: its worktree is unknown: {why}"));
            }
        };
        let Some(to) = projects.home(terminal, &spot) else {
            return;
        };
        if let MoveTo::Worktree(to) = to {
            assert_ne!(
                to,
                from.id(),
                "terminal {terminal} moves only to another worktree"
            );
        }
        if let Err(why) = self.moved(terminal, to) {
            self.troubles.say(format!("{stays}: {why}"));
        }
    }

    fn moved(&mut self, terminal: TerminalViewId, to: MoveTo) -> Result<(), String> {
        let projects = self.kept()?;
        let view = terminal.view();
        let (held_in, _) = projects
            .holding_terminal(view)
            .ok_or("no layout holds it")?;
        let from = held_in.id();
        let follows = |client: &Client| {
            matches!(
                client.place,
                ClientPlace::Showing { worktree, focus, .. }
                    if worktree == from && focus == Some(view)
            )
        };
        let unmade = Layout::default();
        let (target, bound) = match &to {
            MoveTo::Worktree(to) => {
                let (_, target) = projects
                    .find(*to)
                    .ok_or_else(|| format!("no worktree {to}"))?;
                (target.layout(), Some(*to))
            }
            MoveTo::Made { .. } => (&unmade, None),
        };
        let shown = self
            .clients
            .values()
            .filter(|it| bound.is_some_and(|bound| it.showing_worktree(bound)) || follows(it));
        let size = fit(shown, target, &self.known).map_or(UNSHOWN_SIZE, |fit| fit.size);
        let followed = self.clients.values().any(follows);
        let to = self
            .store
            .moved(terminal, to, size, followed)
            .map_err(Failed::said)?;
        for client in self.clients.values_mut().filter(|client| follows(client)) {
            if let ClientPlace::Showing {
                worktree,
                browser_ttys,
                ..
            } = &mut client.place
            {
                *worktree = to;
                browser_ttys.clear();
            }
        }
        if followed {
            self.went(to);
        }
        let projects = self
            .store
            .projects()
            .expect("projects just changed are kept");
        let (held_in, _) = projects
            .holding_terminal(view)
            .expect("a terminal moved is held");
        assert_eq!(
            held_in.id(),
            to,
            "terminal {terminal} moved to worktree {to}"
        );
        if followed {
            let last_focus = held_in.layout().last_focus();
            assert_eq!(
                last_focus,
                Some(view),
                "terminal {terminal} is focused where it went"
            );
        }
        assert!(
            !self.clients.values().any(follows),
            "no client of {from} focuses {terminal}"
        );
        Ok(())
    }

    fn tell_core(&mut self) {
        let Some(projects) = self.store.projects() else {
            return;
        };
        tell_core_terminals(
            &self.core,
            &mut self.told,
            &mut self.troubles,
            projects,
            &self.known,
            &self.listed,
            self.looked,
        );
    }

    fn settle(&mut self) {
        let Layouts {
            store,
            clients,
            known,
            looked,
            errands,
            troubles,
            ..
        } = self;
        let Some(projects) = store.projects() else {
            let why = store.unkept().unwrap_or("no projects are kept").to_owned();
            for client in std::mem::take(clients).into_values() {
                let _ = client
                    .out
                    .try_send(Err(Status::failed_precondition(why.clone())));
            }
            return;
        };
        let version = store.revision().to_api();
        let mut gone = Vec::new();
        for (id, client) in clients.iter_mut() {
            let ClientPlace::Showing {
                worktree, focus, ..
            } = &mut client.place
            else {
                continue;
            };
            let Some((_, shown)) = projects.find(*worktree) else {
                gone.push((*id, *worktree));
                continue;
            };
            let layout = shown.layout();
            if let Some(full) = layout.full() {
                *focus = Some(full);
            } else if !focus.is_some_and(|focus| layout.holds(focus)) {
                *focus = arrival(shown);
            }
        }
        for (id, left) in gone {
            let client = clients
                .get_mut(&id)
                .expect("a client whose worktree went is held");
            let heard = client.heard.last();
            let word = api::ShowResponse {
                event: Some(show_response::Event::Left(left.get())),
                heard: AskNumber::wire(heard),
            };
            if told(&client.out, word) {
                client.echoed = heard;
                client.place = ClientPlace::Stranded { left };
            } else {
                clients.remove(&id);
            }
        }
        let worktrees: BTreeSet<WorktreeId> = clients
            .values()
            .filter_map(Client::showing)
            .map(|(worktree, _)| worktree)
            .collect();
        let mut fits: BTreeMap<WorktreeId, Fit> = BTreeMap::new();
        for worktree in worktrees {
            let (_, found) = projects
                .find(worktree)
                .expect("a client's worktree is held");
            let shown = clients.values().filter(|it| it.showing_worktree(worktree));
            let fitted = fit(shown, found.layout(), known);
            let fitted = fitted.expect("a worktree shown has a client");
            fits.insert(worktree, fitted);
        }
        let mut sizes_asked: usize = 0;
        for (&worktree, &Fit { size, outside }) in &fits {
            let (_, found) = projects
                .find(worktree)
                .expect("a client's worktree is held");
            let (_, sized) = found.layout().sized(size);
            for (view, wanted) in sized {
                let View::Terminal(terminal) = view else {
                    continue;
                };
                let terminal = terminal.id();
                let Some(known) = known.get_mut(&terminal) else {
                    continue;
                };
                let runs_in =
                    |it: &Client| it.showing_worktree(worktree) && it.runs_in == Some(known.key);
                let pinned = !outside && clients.values().any(runs_in);
                if known.settled == Some(wanted) || pinned {
                    continue;
                }
                sizes_asked += 1;
                if errands.size(known.key, wanted) {
                    known.settled = Some(wanted);
                } else {
                    let id = known.key.id;
                    troubles.say(format!("terminal {id} is not sized: {}", errands::UNTAKEN));
                }
            }
        }
        assert!(
            sizes_asked <= VIEWS_MAX,
            "a settle asks at most one size of each view"
        );
        let mut slow = Vec::new();
        for (id, client) in clients.iter_mut() {
            let ClientPlace::Showing {
                worktree,
                focus,
                laid_said,
                ..
            } = &mut client.place
            else {
                continue;
            };
            let found = projects.find(*worktree);
            let (_, found) = found.expect("a client's worktree is held");
            let size = fits[worktree].size;
            let laid_body = laid(found, size, *focus, known, *looked);
            let heard = client.heard.last();
            if laid_said.as_ref() == Some(&laid_body) && client.echoed >= heard {
                continue;
            }
            let mut wire_laid = laid_body.clone();
            wire_laid.version = Some(version);
            let word = api::ShowResponse {
                event: Some(show_response::Event::Laid(wire_laid)),
                heard: AskNumber::wire(heard),
            };
            if !told(&client.out, word) {
                slow.push(*id);
                continue;
            }
            *laid_said = Some(laid_body);
            client.echoed = heard;
        }
        for id in slow {
            clients.remove(&id);
        }
        assert!(
            clients.len() <= CLIENTS_MAX,
            "no more than {CLIENTS_MAX} clients show layouts"
        );
    }
}

fn runs_changed(
    known: &BTreeMap<TerminalViewId, Known>,
    before_keys: &BTreeMap<TerminalViewId, TerminalKey>,
    before: &BTreeMap<TerminalKey, server::Terminal>,
    after: &BTreeMap<TerminalKey, server::Terminal>,
) -> bool {
    known.iter().any(|(terminal, known)| {
        if before_keys.get(terminal) != Some(&known.key) {
            return true;
        }
        let was = before.get(&known.key).map(|it| (&it.cwd, &it.is));
        let now = after.get(&known.key).map(|it| (&it.cwd, &it.is));
        was != now
    })
}

fn tell_core_terminals(
    core: &SyncSender<Core>,
    told: &mut Option<BTreeMap<ViewId, Located>>,
    troubles: &mut Troubles,
    projects: &Projects,
    known: &BTreeMap<TerminalViewId, Known>,
    listed: &BTreeMap<TerminalKey, server::Terminal>,
    looked: bool,
) {
    let located = |(_, terminal): (&Worktree, &Terminal)| {
        let id = terminal.id();
        let located = match known.get(&id) {
            Some(known) => Located::Shown(match listed.get_key_value(&known.key) {
                Some((&key, listed)) => Shown::listed(key, listed),
                None => Shown::unlisted(known.key),
            }),
            None if looked => Located::Lost,
            None => Located::Looking,
        };
        (id.view(), located)
    };
    let now: BTreeMap<ViewId, Located> = projects.terminals().map(located).collect();
    if told.as_ref() == Some(&now) {
        return;
    }
    if core.send(Core::Terminals(now.clone())).is_err() {
        troubles.say("the core hears the layouts' terminals no more".to_owned());
        return;
    }
    *told = Some(now);
}

fn untyped(terminals: &[TerminalViewId], why: &str) -> String {
    let terminals: Vec<String> = terminals.iter().map(ToString::to_string).collect();
    let kept = format!(
        "the last commands of terminals {} are kept",
        terminals.join(", ")
    );
    format!("{kept}, maybe not typed: {why}")
}

#[derive(Clone, Copy)]
struct Fit {
    size: Size,
    outside: bool,
}

fn fit<'a>(
    shown: impl Iterator<Item = &'a Client>,
    layout: &Layout,
    known: &BTreeMap<TerminalViewId, Known>,
) -> Option<Fit> {
    shown.fold(None, |fitted, client| {
        let outside = !inside(client, layout, known);
        let size = match fitted {
            Some(Fit { outside: true, .. }) if !outside => return fitted,
            Some(fitted) if fitted.outside == outside => fitted.size.least(client.size),
            Some(_) | None => client.size,
        };
        Some(Fit { size, outside })
    })
}

fn inside(client: &Client, layout: &Layout, known: &BTreeMap<TerminalViewId, Known>) -> bool {
    let holds = |runs_in| {
        known
            .iter()
            .any(|(id, it)| it.key == runs_in && layout.holds(id.view()))
    };
    client.runs_in.is_some_and(holds)
}

fn placed(layout: &Layout, terminal: ViewId, size: Size) -> Option<Size> {
    let (_, sized) = layout.sized(size);
    let mut sized = sized.into_iter();
    sized
        .find(|(view, _)| layout::Leaf::id(*view) == terminal)
        .map(|(_, inner)| inner)
}

enum SavedOpen {
    Shell,
    Root(ResumeArgv),
    InShell { kind: ShellKind, argv: ResumeArgv },
}

enum SavedResumeMode {
    Root,
    InShell(ShellKind),
}

enum SavedChoice {
    Resume {
        argv: ResumeArgv,
        cwd: Dir,
        mode: SavedResumeMode,
        rule: String,
    },
    Skip {
        reason: &'static str,
        program: String,
    },
}

fn evaluate_resume_record(
    source: RecordSource,
    record: &ResumeRecord,
    chosen: bool,
    stored: Option<&StoredResume>,
    config: &ConfigState,
    as_root: bool,
    fallback_cwd: Option<&str>,
) -> api::ResumeRecordInfo {
    let persistence = match stored {
        Some(saved)
            if saved.source == source
                && saved.record.pid() == record.pid()
                && saved.record.proc_start() == record.proc_start()
                && saved.record.argv() == record.argv() =>
        {
            match saved.attempt {
                ResumeAttempt::Pending => api::ResumePersistence::Saved,
                ResumeAttempt::Tried => api::ResumePersistence::Tried,
            }
        }
        _ => api::ResumePersistence::Unsaved,
    };
    let verdict = trust::evaluate(record, config, as_root, fallback_cwd, false);
    resume::record_to_api(source, record, chosen, persistence, &verdict)
}

fn decide_saved_resume(
    stored: &StoredResume,
    config_state: &ConfigState,
    shell: &Shell,
    folder: &Dir,
) -> SavedChoice {
    let our_uid = rustix::process::getuid().as_raw();
    assert_eq!(
        stored.record.uid(),
        our_uid,
        "stored resume records belong to current user"
    );
    let program = stored.record.argv().program().to_owned();
    if config_state.is_incomplete() {
        return SavedChoice::Skip {
            reason: "settings_incomplete",
            program,
        };
    }
    let as_root = resume::running_as_root();
    let allow_root = config_state.loaded().is_some_and(|cfg| cfg.allow_root);
    if as_root && !allow_root {
        return SavedChoice::Skip {
            reason: "program_resume_off",
            program,
        };
    }
    if stored.attempt.is_tried() {
        return SavedChoice::Skip {
            reason: "already_tried",
            program,
        };
    }
    let (spawn_dir, eval_record) = match stored.record.cwd() {
        Some(ResumeCwdSource::Explicit(dir)) => {
            let Ok(parsed) = Dir::parse(dir.as_str()) else {
                return SavedChoice::Skip {
                    reason: "dir_gone",
                    program,
                };
            };
            if is_folder(&parsed) != Ok(true) {
                return SavedChoice::Skip {
                    reason: "dir_gone",
                    program,
                };
            }
            (parsed, stored.record.clone())
        }
        Some(ResumeCwdSource::Inferred(dir)) => {
            if let Some(parsed) = Dir::parse(dir.as_str())
                .ok()
                .filter(|d| is_folder(d) == Ok(true))
            {
                (parsed, stored.record.clone())
            } else {
                (folder.clone(), stored.record.clone().with_cwd(None))
            }
        }
        None => (folder.clone(), stored.record.clone()),
    };
    let verdict = trust::evaluate(
        &eval_record,
        config_state,
        as_root,
        Some(spawn_dir.as_str()),
        true,
    );
    let rule = match verdict {
        trust::TrustVerdict::Trusted { rule } => rule,
        trust::TrustVerdict::UntrustedGlobal(trust::GlobalUntrustedReason::ConfigIncomplete) => {
            return SavedChoice::Skip {
                reason: "settings_incomplete",
                program,
            };
        }
        trust::TrustVerdict::UntrustedGlobal(trust::GlobalUntrustedReason::Root) => {
            return SavedChoice::Skip {
                reason: "program_resume_off",
                program,
            };
        }
        trust::TrustVerdict::UntrustedGlobal(trust::GlobalUntrustedReason::NoRule)
        | trust::TrustVerdict::UntrustedRule { .. } => {
            return SavedChoice::Skip {
                reason: "not_trusted",
                program,
            };
        }
    };
    let mode = if stored.record.placement().is_root() {
        SavedResumeMode::Root
    } else if let Some(kind) = shell.resume_kind() {
        SavedResumeMode::InShell(kind)
    } else {
        return SavedChoice::Skip {
            reason: "shell_unsupported",
            program,
        };
    };
    assert!(!stored.attempt.is_tried());
    if as_root {
        assert!(allow_root);
    }
    SavedChoice::Resume {
        argv: stored.record.argv().clone(),
        cwd: spawn_dir,
        mode,
        rule,
    }
}

fn keeps(restored: Option<&Restored>, held: &Runs, is: &Is) -> bool {
    match restored {
        Some(Restored::Typing { .. } | Restored::Resuming { .. }) => true,
        None => matches!(held, Runs::Unrun(_)) && !ran_since(is),
    }
}

fn pasted(is: Option<&Is>, due: Instant, by: Instant, now: Instant) -> Paste {
    assert!(due <= by, "a paste is due before it is given up");
    let Some(is) = is else {
        return if now >= due { Paste::Now } else { Paste::Later };
    };
    if ran(is) {
        return Paste::Never;
    }
    if runs_in_front(is) {
        return if now >= by {
            Paste::Never
        } else {
            Paste::Later
        };
    }
    let prompts = matches!(
        is,
        Is::Running {
            doing: Some(Doing::Ready),
            ..
        }
    );
    if prompts || now >= due {
        Paste::Now
    } else {
        Paste::Later
    }
}

fn ran_since(is: &Is) -> bool {
    ran(is) || runs_in_front(is)
}

fn ran(is: &Is) -> bool {
    match is {
        Is::Running { doing, .. } => {
            matches!(doing, Some(Doing::Ended { .. } | Doing::Stopped { .. }))
        }
        Is::Ended => true,
    }
}

fn runs_in_front(is: &Is) -> bool {
    match is {
        Is::Running {
            root, front, doing, ..
        } => {
            matches!(doing, Some(Doing::Going { .. }))
                || front.as_ref().is_some_and(|it| it.pid != *root)
        }
        Is::Ended => false,
    }
}

fn choose_closed_resume(
    our_uid: u32,
    listed: Option<&server::Terminal>,
    at_shell: bool,
    synthesized: Option<&ResumeRecord>,
    stored: Option<&StoredResume>,
) -> Option<StoredResume> {
    let osc_resume = listed.and_then(|it| match &it.is {
        Is::Running {
            resumes: Some(resumes),
            ..
        } if resumes.chosen().uid() == our_uid => Some(StoredResume {
            source: RecordSource::Osc7577,
            record: resumes.chosen().clone(),
            attempt: ResumeAttempt::Pending,
        }),
        Is::Running { .. } | Is::Ended => None,
    });
    let chosen = if osc_resume.is_some() {
        osc_resume
    } else if at_shell {
        None
    } else if let Some(record) = synthesized.filter(|record| record.uid() == our_uid) {
        Some(StoredResume {
            source: RecordSource::Synthesized,
            record: record.clone(),
            attempt: ResumeAttempt::Pending,
        })
    } else {
        stored
            .filter(|saved| saved.record.uid() == our_uid)
            .map(|saved| StoredResume {
                source: saved.source,
                record: saved.record.clone(),
                attempt: ResumeAttempt::Pending,
            })
    };
    if let Some(saved) = &chosen {
        assert_eq!(
            saved.attempt,
            ResumeAttempt::Pending,
            "closed resume snapshots start pending"
        );
        assert_eq!(
            saved.record.uid(),
            our_uid,
            "closed resume snapshots belong to current user"
        );
    }
    chosen
}

fn terminal_runs_snapshot(
    terminal: &Terminal,
    restored: Option<&Restored>,
    listed: Option<&server::Terminal>,
    shell: &Shell,
) -> TerminalSnapshot {
    let folder = listed
        .and_then(|it| it.cwd.as_ref())
        .and_then(|cwd| Dir::parse(cwd.as_str()).ok())
        .unwrap_or_else(|| terminal.folder().clone());
    let Some(listed) = listed else {
        return TerminalSnapshot {
            folder,
            runs: terminal.runs().clone(),
            keeps_restored: restored.is_some(),
        };
    };
    let keeps_restored = keeps(restored, terminal.runs(), &listed.is);
    let runs = if keeps_restored {
        terminal.runs().clone()
    } else {
        runs_of(listed, terminal.runs(), shell)
    };
    TerminalSnapshot {
        folder,
        runs,
        keeps_restored,
    }
}

fn is_folder_cached(folder: &Dir, cache: &mut BTreeMap<Dir, bool>) -> Result<bool, String> {
    assert!(
        cache.len() <= 2 * CLOSED_VIEWS_MAX,
        "revive folder cache stays bounded by closed view entries"
    );
    if let Some(&present) = cache.get(folder) {
        return Ok(present);
    }
    let present = is_folder(folder)?;
    cache.insert(folder.clone(), present);
    assert!(
        cache.len() <= 2 * CLOSED_VIEWS_MAX,
        "revive folder cache stays bounded by closed view entries"
    );
    Ok(present)
}

fn is_folder(folder: &Dir) -> Result<bool, String> {
    let mut test = process::Command::new("/bin/sh");
    test.args(["-c", r#"test -d "$1""#, "sh", folder.as_str()]);
    let secs = FOLDER_LOOK.get().as_secs();
    match program::run(&mut test, FOLDER_LOOK.get()) {
        Ok(Looked::Ended { status, .. }) => Ok(status.success()),
        Ok(Looked::Stuck) => Err(format!("{} did not answer in {secs}s", folder.as_str())),
        Err(error) => Err(format!("{} is not looked at: {error}", folder.as_str())),
    }
}

fn told(out: &Out, word: api::ShowResponse) -> bool {
    if out.capacity() <= 1 {
        let slow = format!("the client read too slowly, with {ANSWERS_MAX} answers unread");
        let _ = out.try_send(Err(Status::resource_exhausted(slow)));
        return false;
    }
    out.try_send(Ok(word)).is_ok()
}

fn half_closed(closed: String) -> impl FnOnce(Failed) -> Failed {
    move |failed| Failed::Unclosed(format!("{closed} closed, and {}", failed.said()))
}

fn placement(
    layout: &Layout,
    focus: Option<ViewId>,
    beside: Beside,
    new: ViewId,
    size: Size,
) -> Result<(Placed, Size), String> {
    let Some(tree) = layout.tree() else {
        return Ok((Placed::Alone, layout::lone(size)));
    };
    assert!(
        !tree.holds(new),
        "view {new} is placed in a layout that holds it"
    );
    let focused = || focus.ok_or("the layout holds no view to open beside");
    let roomless = |at: ViewId| format!("view {at} has no room beside it");
    if layout.tabbed() {
        let placed = match beside {
            Beside::Towards(towards) => {
                let at = focused()?;
                let room = layout.room_towards(at, towards, new, size);
                room.then_some(Placed::Beside { at, towards })
                    .ok_or_else(|| roomless(at))?
            }
            Beside::Alone | Beside::Focused | Beside::Largest => layout
                .room_for(new, focus, size)
                .ok_or("no view has room for another")?,
        };
        return Ok((placed, layout::lone(size)));
    }
    match beside {
        Beside::Alone => unreachable!("a view opened alone is opened only in an empty layout"),
        Beside::Towards(towards) => {
            let at = focused()?;
            let inner = tree
                .room(at, towards, new, size)
                .ok_or_else(|| roomless(at))?;
            Ok((Placed::Beside { at, towards }, inner))
        }
        Beside::Focused => {
            let at = focused()?;
            let (towards, inner) = tree
                .room_beside(at, new, size)
                .ok_or_else(|| roomless(at))?;
            Ok((Placed::Beside { at, towards }, inner))
        }
        Beside::Largest => {
            let room = tree.room_beside_largest(new, size);
            let (at, towards, inner) = room.ok_or("no view has room for another")?;
            Ok((Placed::Beside { at, towards }, inner))
        }
    }
}

fn arrival(worktree: &Worktree) -> Option<ViewId> {
    let layout = worktree.layout();
    let first = || layout.views().next().map(layout::Leaf::id);
    let arrived = layout.last_focus().or_else(first);
    let held = arrived.is_none_or(|view| layout.holds(view));
    assert!(
        held,
        "a client arrives on a view of worktree {}",
        worktree.id()
    );
    arrived
}

fn tag(terminal: ViewId) -> Vec<u8> {
    format!("{TAG}{terminal}").into_bytes()
}

fn tagged(tag: &[u8]) -> Option<ViewId> {
    std::str::from_utf8(tag)
        .ok()?
        .strip_prefix(TAG)?
        .parse()
        .ok()
}

fn laid(
    worktree: &Worktree,
    size: Size,
    focus: Option<ViewId>,
    known: &BTreeMap<TerminalViewId, Known>,
    looked: bool,
) -> show_response::Laid {
    let layout = worktree.layout();
    if let Some(focus) = focus {
        assert!(
            layout.holds(focus),
            "a client focuses view {focus} of the layout it shows"
        );
    }
    let (fitted, laid_out) = layout.rects(size);
    let views = laid_out.into_iter().map(|(view, rect)| {
        use show_response::view::Found;
        let id = layout::Leaf::id(view);
        let found = match view {
            View::Welcome(_) => Found::Welcome(()),
            View::Browser(browser) => Found::Browser(show_response::view::Browser {
                url: browser.url().map(|url| url.as_str().to_owned()),
            }),
            View::Review(_) => Found::Review(()),
            View::Terminal(terminal) => match known.get(&terminal.id()) {
                Some(known) => Found::Key(api::TerminalKey {
                    id: known.key.id.get(),
                    made_at: Some(known.key.made.into()),
                }),
                None if looked => Found::Lost(()),
                None => Found::Looking(()),
            },
        };
        show_response::View {
            id: id.get(),
            rect: Some(show_response::Rect {
                column: rect.column.into(),
                row: rect.row.into(),
                columns: rect.columns.into(),
                rows: rect.rows.into(),
            }),
            found: Some(found),
        }
    });
    let views: Vec<show_response::View> = views.collect();
    assert_eq!(
        views.len(),
        layout.len(),
        "a layout lays out each of its terminals"
    );
    let (columns, rows) = fitted.get();
    show_response::Laid {
        worktree: worktree.id().get(),
        size: Some(show_request::Size {
            columns: columns.into(),
            rows: rows.into(),
        }),
        views,
        shown: match (layout.full(), focus) {
            (None, Some(focus)) if layout.tabbed() => {
                Some(show_response::laid::Shown::Tabbed(focus.get()))
            }
            (Some(full), _) => {
                assert_eq!(
                    focus,
                    Some(full),
                    "a client of a full layout focuses its terminal"
                );
                Some(show_response::laid::Shown::Full(full.get()))
            }
            (None, Some(focus)) => Some(show_response::laid::Shown::Tiled(focus.get())),
            (None, None) => None,
        },
        version: None,
    }
}

fn runs_of(listed: &server::Terminal, before: &Runs, shell: &Shell) -> Runs {
    let Is::Running {
        root, front, doing, ..
    } = &listed.is
    else {
        return before.clone();
    };
    match (doing, front) {
        (Some(Doing::Ready | Doing::Ended { .. } | Doing::Stopped { .. }), _) => Runs::Shell,
        (
            Some(Doing::Going {
                command: Some(command),
                ..
            }),
            _,
        ) => runs_line(command.as_str()),
        (_, Some(front)) if front.pid != *root => runs_line(&quoted(&front.argv)),
        (_, Some(front)) if is_shell(&front.argv, shell) => Runs::Shell,
        (_, Some(front)) => runs_line(&quoted(&front.argv)),
        (Some(Doing::Going { command: None, .. }) | None, None) => before.clone(),
    }
}

fn runs_line(line: &str) -> Runs {
    let printable = |char: char| {
        let control = char.is_control() && !matches!(char, '\n' | '\t');
        if control {
            char::REPLACEMENT_CHARACTER
        } else {
            char
        }
    };
    let mut kept: String = line.chars().map(printable).collect();
    let end = (0..=kept.len().min(LINE_BYTES_MAX))
        .rev()
        .find(|end| kept.is_char_boundary(*end));
    kept.truncate(end.expect("a string's start is a char's boundary"));
    assert!(
        kept.len() <= LINE_BYTES_MAX,
        "a line kept is cut to {LINE_BYTES_MAX} bytes"
    );
    Line::parse(&kept).map_or(Runs::Shell, Runs::Command)
}

fn is_shell(argv: &[String], shell: &Shell) -> bool {
    let Some((first, rest)) = argv.split_first() else {
        return false;
    };
    let name = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    let Some(named) = name(Path::new(first.trim_start_matches('-'))) else {
        return false;
    };
    let yours = shell.name() == named;
    if !yours && !SHELLS.contains(&named.as_str()) {
        return false;
    }
    rest.iter().all(|word| is_shell_option(word))
}

fn is_shell_option(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return !long.is_empty() && long != "command";
    }
    word.strip_prefix('-')
        .is_some_and(|short| !short.is_empty() && !short.contains('c'))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::SystemTime;

    use lune_domain::projects::{Spot, Subfolder};
    use lune_protocol::program_status::{AppName, ProgramPhase, ProgramStatus, ProgramStatuses};
    use lune_protocol::said::Said;
    use lune_protocol::shell::Front;

    use super::*;
    use crate::mux::own::OwnMade;
    use crate::mux::own::server::Foreground;
    use crate::process::AgentPid;

    fn listed(is: Is) -> server::Terminal {
        server::Terminal {
            tag: Vec::new(),
            size: None,
            cwd: Some(Directory::parse("/w".to_owned()).unwrap()),
            title: String::new(),
            tty: None,
            is,
            statuses: ProgramStatuses::default(),
        }
    }

    fn running(front: Option<(u32, &[&str])>, doing: Option<Doing>) -> server::Terminal {
        let front = front.map(|(pid, argv)| Foreground {
            pid: AgentPid::new(pid).unwrap(),
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
        });
        listed(Is::Running {
            root: AgentPid::new(10).unwrap(),
            front,
            doing,
            resumes: None,
        })
    }

    fn going(command: Option<&str>) -> Doing {
        let command = command.and_then(|command| Said::new(command.to_owned()));
        Doing::Going {
            command,
            since: SystemTime::UNIX_EPOCH,
            front: Front::Working,
        }
    }

    fn command(line: &str) -> Runs {
        Runs::Command(Line::parse(line).unwrap())
    }

    fn ended_ls() -> Doing {
        let command = Said::new("ls".to_owned());
        let at = SystemTime::UNIX_EPOCH;
        Doing::Ended {
            command,
            at,
            ending: None,
            took: Duration::from_millis(5),
        }
    }

    fn own(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_terminal_runs_its_command_or_its_shell() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("xonsh");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let shell = Shell::of(path.into_os_string()).unwrap();
        let before = command("htop");
        let of = |listed: &server::Terminal| runs_of(listed, &before, &shell);
        let claude = running(Some((20, &["claude"])), Some(going(Some("claude"))));
        assert_eq!(of(&claude), command("claude"));
        assert_eq!(
            of(&running(Some((10, &["fish"])), Some(Doing::Ready))),
            Runs::Shell
        );
        let vim = running(Some((20, &["vim", "notes"])), None);
        assert_eq!(of(&vim), command("vim notes"));
        assert_eq!(of(&running(Some((10, &["-bash"])), None)), Runs::Shell);
        assert_eq!(
            of(&running(Some((10, &["/nix/store/x/fish"])), None)),
            Runs::Shell
        );
        assert_eq!(of(&running(Some((10, &["-xonsh"])), None)), Runs::Shell);
        let kanban = running(Some((10, &["lune", "kanban"])), None);
        assert_eq!(of(&kanban), command("lune kanban"));
        assert_eq!(of(&running(None, None)), before);
        assert_eq!(of(&running(None, Some(going(None)))), before);
        assert_eq!(of(&listed(Is::Ended)), before);
        let long = "é".repeat(LINE_BYTES_MAX);
        let too_long = running(Some((20, &[long.as_str()])), None);
        let cut = "é".repeat(LINE_BYTES_MAX / 2 - 1);
        assert_eq!(
            of(&too_long),
            command(&format!("'{cut}")),
            "a line is cut on a char"
        );
        let bell = running(Some((20, &["printf", "\u{7}"])), None);
        assert_eq!(of(&bell), command("printf '\u{fffd}'"));
        for argv in [
            &["ssh", "box"][..],
            &["bash", "-c", "make"],
            &["bash", "-lc", "make"],
        ] {
            assert_eq!(
                of(&running(Some((10, argv)), None)),
                command(&quoted(&own(argv)))
            );
        }
        for argv in [&["-zsh"][..], &["fish", "--login"], &["bash", "-i"]] {
            assert_eq!(
                of(&running(Some((10, argv)), None)),
                Runs::Shell,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn a_restored_terminal_keeps_what_it_ran_until_it_runs_something_else() {
        let shell = Runs::Shell;
        let line = Line::parse("make test").unwrap();
        let unrun = Runs::Unrun(line.clone());
        let now = Instant::now();
        let typing = Restored::Typing {
            line,
            due: now,
            by: now,
        };
        let prompts = running(Some((10, &["fish"])), Some(Doing::Ready));
        let started = running(Some((10, &["fish"])), None);
        let going = running(Some((10, &["fish"])), Some(going(Some("ls"))));
        let vim = running(Some((20, &["vim"])), None);
        let ended = listed(Is::Ended);
        let ran_ls = running(Some((10, &["fish"])), Some(ended_ls()));
        for is in [&prompts, &started, &going, &vim, &ended, &ran_ls] {
            assert!(!keeps(None, &shell, &is.is), "{is:?}");
            assert!(!keeps(None, &command("make test"), &is.is), "{is:?}");
            assert!(keeps(Some(&typing), &unrun, &is.is), "{is:?}");
        }
        for is in [&prompts, &started] {
            assert!(keeps(None, &unrun, &is.is), "{is:?}");
        }
        for is in [&going, &vim, &ended, &ran_ls] {
            assert!(!keeps(None, &unrun, &is.is), "{is:?}");
        }
        let resuming = Some(&Restored::Resuming {
            deadline: now + Duration::from_secs(5),
        });
        assert!(keeps(resuming, &shell, &started.is));
    }

    #[test]
    fn a_terminal_that_fails_to_restore_is_given_up_after_its_attempts() {
        let (failing, other) = (ViewId::of(7).unwrap(), ViewId::of(8).unwrap());
        let mut attempts = Attempts::default();
        for attempt in 1..RESTORE_ATTEMPTS_MAX {
            assert!(attempts.wanted(failing));
            let said = attempts.failed(failing, "no folder");
            assert!(said.contains(&format!("attempt {attempt} of")), "{said}");
        }
        let said = attempts.failed(failing, "no folder");
        assert!(said.contains("given up"), "{said}");
        assert!(!attempts.wanted(failing));
        assert!(attempts.wanted(other));
        attempts.retain(|terminal| terminal != failing);
        assert!(
            attempts.wanted(failing),
            "a terminal found again is tried afresh"
        );
    }

    #[test]
    fn a_crash_soon_after_a_restore_counts_as_a_failed_attempt() {
        let (crashing, other) = (ViewId::of(7).unwrap(), ViewId::of(8).unwrap());
        let mut attempts = Attempts::default();
        let said = attempts.failed(crashing, "no folder");
        assert!(said.contains("attempt 1 of"), "{said}");
        let said = attempts.crashed(crashing, Duration::from_secs(4));
        assert!(
            said.contains(&format!("ended 4s after terminal {crashing} was")),
            "{said}"
        );
        assert!(said.contains("attempt 2 of"), "{said}");
        for _ in 2..RESTORE_ATTEMPTS_MAX {
            assert!(attempts.wanted(crashing));
            attempts.crashed(crashing, Duration::from_secs(4));
        }
        assert!(
            !attempts.wanted(crashing),
            "a terminal lune-mux keeps ending after is given up"
        );
        attempts.failed(other, "no folder");
        attempts.settled(other);
        attempts.settled(crashing);
        assert!(
            attempts.wanted(other) && attempts.wanted(crashing),
            "a settled one starts afresh"
        );
        for terminal in [other, crashing] {
            let said = attempts.failed(terminal, "no folder");
            assert!(said.contains("attempt 1 of"), "{said}");
        }
    }

    #[test]
    fn a_restored_line_is_pasted_only_where_its_shell_waits_for_one() {
        let start = Instant::now();
        let (due, by) = (start + TYPING_WAIT, start + TYPING_WAIT_MAX);
        let (early, late, too_late) = (start, due, by);
        let prompts = running(Some((10, &["fish"])), Some(Doing::Ready));
        let quiet = running(Some((10, &["bash"])), None);
        let vim = running(Some((20, &["vim"])), None);
        let going = running(Some((10, &["fish"])), Some(going(Some("vim"))));
        let ran_ls = running(Some((10, &["fish"])), Some(ended_ls()));
        let ended = listed(Is::Ended);
        let at = |is: Option<&server::Terminal>, now| pasted(is.map(|it| &it.is), due, by, now);
        assert_eq!(
            at(Some(&prompts), early),
            Paste::Now,
            "a prompt is waited for"
        );
        assert_eq!(at(Some(&quiet), early), Paste::Later);
        assert_eq!(
            at(Some(&quiet), late),
            Paste::Now,
            "a shell with no marks is pasted in time"
        );
        assert_eq!(at(None, early), Paste::Later);
        assert_eq!(at(None, late), Paste::Now);
        for busy in [&vim, &going] {
            assert_eq!(at(Some(busy), late), Paste::Later, "{busy:?}");
            assert_eq!(at(Some(busy), too_late), Paste::Never, "{busy:?}");
        }
        for done in [&ran_ls, &ended] {
            assert_eq!(at(Some(done), early), Paste::Never, "{done:?}");
        }
    }

    #[test]
    fn a_folder_is_looked_at_apart_from_the_layouts_thread() {
        let folder = tempfile::tempdir().unwrap();
        let dir = Dir::parse(folder.path().to_str().unwrap()).unwrap();
        assert_eq!(is_folder(&dir), Ok(true));
        let file = folder.path().join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(
            is_folder(&Dir::parse(file.to_str().unwrap()).unwrap()),
            Ok(false)
        );
        let gone = folder.path().join("gone");
        assert_eq!(
            is_folder(&Dir::parse(gone.to_str().unwrap()).unwrap()),
            Ok(false)
        );
        let spaced = folder.path().join("a b'c");
        fs::create_dir(&spaced).unwrap();
        assert_eq!(
            is_folder(&Dir::parse(spaced.to_str().unwrap()).unwrap()),
            Ok(true)
        );
    }

    #[test]
    fn a_command_line_is_quoted_back_as_a_shell_would_read_it() {
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(quoted(&argv(&["vim", "notes.md"])), "vim notes.md");
        assert_eq!(quoted(&argv(&["sh", "-c", "echo hi"])), "sh -c 'echo hi'");
        assert_eq!(
            quoted(&argv(&["printf", "it's", ""])),
            r"printf 'it'\''s' ''"
        );
    }

    #[test]
    fn a_tag_names_the_terminal_of_a_layout() {
        let id = ViewId::of(0x1f).unwrap();
        assert_eq!(tagged(format!("{TAG}{id}").as_bytes()), Some(id));
        for refused in [
            &b"lune terminal 1f"[..],
            b"terminal 000000000000001f",
            b"\xff",
            b"",
        ] {
            assert_eq!(tagged(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn a_show_word_reads_into_what_it_asks() {
        use show_request::{Event, focus, open, resize};
        let terminal = Event::Focus(show_request::Focus {
            to: Some(focus::To::View(3)),
            step: 0,
        });
        let three = ViewId::of(3).unwrap();
        assert_eq!(
            Asked::of_api(terminal),
            Ok(Asked::Focus(Focus::View(three)))
        );
        let towards_left = || Some(focus::To::Direction(show_request::Direction::Left.into()));
        let stepped = Event::Focus(show_request::Focus {
            to: towards_left(),
            step: 7,
        });
        let step = NonZeroU64::new(7).unwrap();
        let asked = Asked::Focus(Focus::Toward {
            direction: Direction::Left,
            step,
        });
        assert_eq!(Asked::of_api(stepped), Ok(asked), "a step keeps its number");
        let right = show_request::Towards::Right.into();
        let shell = || Some(Opens::Shell.to_api());
        let towards = Some(open::Beside::Towards(right));
        let open = show_request::Open {
            beside: towards,
            opens: shell(),
        };
        let towards = Beside::Towards(Towards::Right);
        let asked = Asked::Open {
            beside: towards,
            opens: Opens::Shell,
        };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let largest = || Some(open::Beside::Largest(()));
        let open = show_request::Open {
            beside: largest(),
            opens: shell(),
        };
        let asked = Asked::Open {
            beside: Beside::Largest,
            opens: Opens::Shell,
        };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let open = show_request::Open {
            beside: Some(open::Beside::Focused(())),
            opens: shell(),
        };
        let asked = Asked::Open {
            beside: Beside::Focused,
            opens: Opens::Shell,
        };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let open = show_request::Open {
            beside: Some(open::Beside::Alone(())),
            opens: shell(),
        };
        let asked = Asked::Open {
            beside: Beside::Alone,
            opens: Opens::Shell,
        };
        assert_eq!(Asked::of_api(Event::Open(open)), Ok(asked));
        let (one_side, every) = (resize::Sides::Direction, Some(resize::Sides::Every(())));
        let (grow, shrink) = (show_request::Growth::Grow, show_request::Growth::Shrink);
        let (grow, shrink) = (grow.into(), shrink.into());
        let around = Event::Resize(show_request::Resize {
            sides: every,
            growth: shrink,
        });
        let asked = Asked::Resize {
            sides: Sides::Every,
            growth: layout::Growth::Shrink,
        };
        assert_eq!(Asked::of_api(around), Ok(asked));
        let left = Some(one_side(show_request::Direction::Left.into()));
        let towards = Event::Resize(show_request::Resize {
            sides: left,
            growth: grow,
        });
        let (sides, growth) = (Sides::One(Direction::Left), layout::Growth::Grow);
        assert_eq!(Asked::of_api(towards), Ok(Asked::Resize { sides, growth }));
        let command = |words| {
            let runs = api::opens::Runs::Command(api::Argv { words });
            Some(api::Opens { runs: Some(runs) })
        };
        let refused = [
            Event::Go(0),
            Event::Focus(show_request::Focus { to: None, step: 0 }),
            Event::Focus(show_request::Focus {
                to: towards_left(),
                step: 0,
            }),
            Event::Resize(show_request::Resize {
                sides: None,
                growth: grow,
            }),
            Event::Resize(show_request::Resize {
                sides: Some(one_side(0)),
                growth: grow,
            }),
            Event::Resize(show_request::Resize {
                sides: every,
                growth: 0,
            }),
            Event::Resize(show_request::Resize {
                sides: every,
                growth: 3,
            }),
            Event::Size(show_request::Size {
                columns: 0,
                rows: 24,
            }),
            Event::Open(show_request::Open {
                beside: None,
                opens: Some(Opens::Shell.to_api()),
            }),
            Event::Open(show_request::Open {
                beside: largest(),
                opens: None,
            }),
            Event::Open(show_request::Open {
                beside: largest(),
                opens: command(vec![String::new()]),
            }),
            Event::Open(show_request::Open {
                beside: largest(),
                opens: command(vec!["x".to_owned(); LINE_BYTES_MAX / 2 + 1]),
            }),
            Event::Hello(show_request::Hello {
                worktree: 1,
                ..Default::default()
            }),
        ];
        for event in refused {
            assert!(Asked::of_api(event.clone()).is_err(), "{event:?}");
        }
    }

    #[test]
    fn a_terminal_whose_title_only_cycles_spinner_glyphs_sends_no_core_terminals() {
        let mut next = 0u64;
        let mut fresh = || {
            next += 1;
            Ok(Id::of(next).unwrap())
        };
        let dir = Dir::parse("/w/lune").unwrap();
        let main = Subfolder::parse("main").unwrap();
        let subfolders = Listed::of(BTreeSet::from([main.clone()])).unwrap();
        let (projects, made) = Projects::default()
            .changed(
                Change::AddProject {
                    spot: Spot::in_clone(dir.clone(), subfolders.clone(), None),
                    name: None,
                },
                None,
                &mut fresh,
            )
            .unwrap();
        let Some(Made::Project(project)) = made else {
            panic!("project made")
        };
        let (projects, _) = projects
            .changed(
                Change::AddWorktree {
                    project,
                    spot: Spot::in_clone(dir, subfolders, Some(main)),
                    name: None,
                    opens: Opens::Shell,
                },
                None,
                &mut fresh,
            )
            .unwrap();
        let terminal = projects.terminals().next().unwrap().1.id();
        let key = TerminalKey {
            id: 1.try_into().unwrap(),
            made: OwnMade::from_nanos(5),
        };
        let known = BTreeMap::from([(
            terminal,
            Known {
                key,
                heard: Heard::Seen,
                settled: None,
                remade: None,
            },
        )]);
        let before_keys = BTreeMap::from([(terminal, key)]);

        let make_listed =
            |title: &str, cwd: &str, size: Option<Size>, is: Is, statuses: ProgramStatuses| {
                BTreeMap::from([(
                    key,
                    server::Terminal {
                        tag: tag(terminal.view()),
                        size,
                        cwd: Directory::parse(cwd.to_owned()).ok(),
                        title: title.to_owned(),
                        tty: None,
                        is,
                        statuses,
                    },
                )])
            };
        let running = |pid: u32, argv: &[&str], doing: Option<Doing>| Is::Running {
            root: AgentPid::new(10).unwrap(),
            front: Some(Foreground {
                pid: AgentPid::new(pid).unwrap(),
                argv: own(argv),
            }),
            doing,
            resumes: None,
        };

        let (core, commands) = mpsc::sync_channel(8);
        let mut told = None;
        let mut troubles = Troubles::default();
        let tell = |told: &mut Option<BTreeMap<ViewId, Located>>,
                    troubles: &mut Troubles,
                    listed: &BTreeMap<TerminalKey, server::Terminal>| {
            tell_core_terminals(&core, told, troubles, &projects, &known, listed, true);
            commands.try_recv().ok()
        };

        let first = make_listed(
            "⠋ Fixing the tests",
            "/w/lune/main",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &first),
            Some(Core::Terminals(_))
        ));

        for spinner in [
            "⠙ Fixing the tests",
            "⠹ Fixing the tests",
            "✳ Fixing the tests",
        ] {
            let next = make_listed(
                spinner,
                "/w/lune/main",
                Some(Size::of(80, 24).unwrap()),
                running(20, &["claude"], None),
                ProgramStatuses::default(),
            );
            assert!(tell(&mut told, &mut troubles, &next).is_none(), "{spinner}");
            assert!(
                !runs_changed(&known, &before_keys, &first, &next),
                "{spinner}"
            );
        }

        let resized_and_osc = make_listed(
            "⠸ Fixing the tests",
            "/w/lune/main",
            Some(Size::of(120, 40).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses {
                root: Some(ProgramStatus {
                    phase: ProgramPhase::Working { progress: None },
                    app: Some(AppName::parse("claude-code").unwrap()),
                    title: None,
                    at: None,
                }),
                children: Vec::new(),
            },
        );
        assert!(tell(&mut told, &mut troubles, &resized_and_osc).is_none());
        assert!(!runs_changed(
            &known,
            &before_keys,
            &first,
            &resized_and_osc
        ));

        let other_pid_same_command = make_listed(
            "⠼ Fixing the tests",
            "/w/lune/main",
            Some(Size::of(80, 24).unwrap()),
            running(21, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(tell(&mut told, &mut troubles, &other_pid_same_command).is_none());
        assert!(runs_changed(
            &known,
            &before_keys,
            &first,
            &other_pid_same_command
        ));

        let new_topic = make_listed(
            "✳ Updating the docs",
            "/w/lune/main",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &new_topic),
            Some(Core::Terminals(_))
        ));

        let plain_one = make_listed(
            "vim a.rs",
            "/w/lune/main",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &plain_one),
            Some(Core::Terminals(_))
        ));

        let plain_two = make_listed(
            "vim b.rs",
            "/w/lune/main",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(tell(&mut told, &mut troubles, &plain_two).is_none());

        let moved = make_listed(
            "vim b.rs",
            "/w/lune/main/src",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["claude"], None),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &moved),
            Some(Core::Terminals(_))
        ));
        assert!(runs_changed(&known, &before_keys, &plain_two, &moved));

        let new_command = make_listed(
            "vim b.rs",
            "/w/lune/main/src",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["cargo", "test"], None),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &new_command),
            Some(Core::Terminals(_))
        ));

        let prompted = make_listed(
            "vim b.rs",
            "/w/lune/main/src",
            Some(Size::of(80, 24).unwrap()),
            running(20, &["cargo", "test"], Some(Doing::Ready)),
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &prompted),
            Some(Core::Terminals(_))
        ));

        let ended = make_listed(
            "vim b.rs",
            "/w/lune/main/src",
            Some(Size::of(80, 24).unwrap()),
            Is::Ended,
            ProgramStatuses::default(),
        );
        assert!(matches!(
            tell(&mut told, &mut troubles, &ended),
            Some(Core::Terminals(_))
        ));
    }

    #[test]
    fn choose_closed_resume_rejects_mismatched_uid_and_resets_attempt_to_pending() {
        use lune_protocol::program_resume::{
            ProcStart, ProcessName, ProcessPid, ProcessPlacement, ReportedAt, TerminalResumes,
        };

        let make_record = |uid: u32| {
            ResumeRecord::new(
                ProcessPid::new(42).unwrap(),
                ProcStart::parse("12345".to_owned()).unwrap(),
                uid,
                ProcessName::parse("claude".to_owned()).unwrap(),
                None,
                None,
                ProcessPlacement::ShellHosted,
                ResumeArgv::parse(vec![
                    "claude".to_owned(),
                    "--resume".to_owned(),
                    "abc".to_owned(),
                ])
                .unwrap(),
                None,
                ReportedAt::from_parts(100, 0).unwrap(),
            )
        };

        let our_uid = 1000;
        let other_uid = 0;
        let other_record = make_record(other_uid);
        let own_record = make_record(our_uid);

        let osc_other = server::Terminal {
            tag: Vec::new(),
            size: None,
            cwd: None,
            title: String::new(),
            tty: None,
            is: Is::Running {
                root: AgentPid::new(10).unwrap(),
                front: None,
                doing: None,
                resumes: Some(Box::new(
                    TerminalResumes::new(other_record.clone(), Vec::new()).unwrap(),
                )),
            },
            statuses: ProgramStatuses::default(),
        };
        assert_eq!(
            choose_closed_resume(our_uid, Some(&osc_other), false, None, None),
            None
        );
        assert_eq!(
            choose_closed_resume(our_uid, None, false, Some(&other_record), None),
            None
        );

        let tried_own = StoredResume {
            source: RecordSource::Synthesized,
            record: own_record.clone(),
            attempt: ResumeAttempt::Tried,
        };
        assert_eq!(
            choose_closed_resume(our_uid, Some(&osc_other), false, None, Some(&tried_own)),
            Some(StoredResume {
                source: RecordSource::Synthesized,
                record: own_record,
                attempt: ResumeAttempt::Pending,
            })
        );
    }
}
