use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::num::NonZeroU64;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use luneta_protocol::said::Said;
use luneta_protocol::tab;
use luneta_protocol::v1::{self, agent::Status as Reported, NameSource};

use crate::cursor::Cursor;
use crate::dirs::short_cwd;
use crate::elapsed::{Held, Moment};
use crate::fetch::Fetch;
use crate::places::Places;
use crate::sessions::{Selection, Session};
use crate::tree::{Branch, Leaf};

pub const CLOSE_VALUE: &str = "close";
pub const LABEL_KEY: &str = "luneta_label";

/// The process an agent runs as, which the daemon lists once: what tells one
/// agent from another, while the pane it sits in and the session it names
/// can be another's too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Pid(u32);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Seat {
    Here(String),
    There(String),
}

impl Seat {
    pub fn session(&self) -> &str {
        match self {
            Seat::Here(session) | Seat::There(session) => session,
        }
    }
}

pub struct Live<'a> {
    current: Option<&'a str>,
    places: &'a Places,
    /// The live sessions in the order the session list shows them: the one
    /// you are in first, then the others, newest first.
    order: Vec<&'a str>,
    /// Every live session's panes, the one you are in too, as zellij last
    /// listed them.
    branches: &'a [Branch],
}

impl<'a> Live<'a> {
    /// `live` is the session list's live sessions, which leave out the one
    /// you are in.
    pub fn new(
        current: Option<&'a str>,
        live: &'a [Session],
        places: &'a Places,
        branches: &'a [Branch],
    ) -> Self {
        let mut others: Vec<&Session> = live.iter().collect();
        others.sort_by(|a, b| a.age.cmp(&b.age).then_with(|| a.name.cmp(&b.name)));
        let order = current.into_iter().chain(others.iter().map(|s| s.name.as_str())).collect();
        Live { current, places, order, branches }
    }

    fn leaf(&self, session: &str, pane: u32) -> Option<&'a Leaf> {
        let branch = self.branches.iter().find(|branch| branch.name == session)?;
        branch.panes.iter().find(|leaf| leaf.id == pane)
    }

    /// Where `session` stands in the session list, for its agents to stand
    /// with it. A session the list does not know goes last.
    fn rank(&self, session: &str) -> usize {
        self.order.iter().position(|name| *name == session).unwrap_or(self.order.len())
    }

    fn knows(&self, session: &str) -> bool {
        self.order.contains(&session)
    }

    fn seat(&self, session: &str) -> Seat {
        if self.current == Some(session) {
            Seat::Here(session.to_owned())
        } else {
            Seat::There(session.to_owned())
        }
    }

    /// The pane an agent sits in: the one the daemon followed it to, or else
    /// one found by its id and where it works, which is a guess where the
    /// name the agent holds is another running session's.
    fn place<'b>(&self, agent: &'b Agent) -> Option<Seated<'b>> {
        let (session, guessed) = match &agent.housed {
            Housed::Followed(session) => (session.as_str(), false),
            Housed::Named(named) => {
                let session = self.places.find(agent.pane, agent.cwd.as_deref()?, named)?;
                (session, named != session && self.knows(named))
            },
        };
        Some(Seated { agent, seat: self.seat(session), guessed })
    }
}

/// An agent in the pane `Live` found for it.
struct Seated<'a> {
    agent: &'a Agent,
    seat: Seat,
    /// As `AgentRow::guessed`.
    guessed: bool,
}

/// The session the daemon says an agent's pane is in.
#[derive(Debug, PartialEq, Eq)]
enum Housed {
    /// The one it runs in now, which the daemon followed it to through any
    /// rename: its pane is there, whatever the session has said of its panes.
    Followed(String),
    /// What its environment names, which the daemon cannot follow: a rename
    /// leaves it behind, and another session can take it up. Its pane is
    /// found by its id and where it works.
    Named(String),
}

impl Housed {
    /// The session the daemon says: the one it followed the agent to, or
    /// else the one its environment names.
    fn session(&self) -> &str {
        match self {
            Housed::Followed(session) | Housed::Named(session) => session,
        }
    }
}

/// When an agent came to its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Came {
    /// As Claude Code dates it, to the millisecond.
    Dated(Moment),
    /// When luneta first heard it at its status, which Claude Code dates not,
    /// or heard it again once the daemon had not answered a while.
    Heard(Moment),
}

impl Came {
    pub fn moment(self) -> Moment {
        match self {
            Came::Dated(at) | Came::Heard(at) => at,
        }
    }
}

pub struct Agent {
    housed: Housed,
    pid: Pid,
    name: Option<String>,
    pane: u32,
    status: Status,
    came: Came,
    /// Where the agent works, when Claude Code says.
    cwd: Option<String>,
    /// When its session started, in seconds since the epoch, when Claude Code
    /// says.
    started: Option<NonZeroU64>,
    /// Whether Claude Code says it was started from a prompt, as `cli`: the
    /// agent a person talks to in its pane. A `claude -p` says `sdk-cli`, and
    /// a Claude Code from before it said how it started says neither.
    cli: bool,
}

pub struct AgentRow {
    pid: Pid,
    pub seat: Seat,
    name: Name,
    pub pane: u32,
    pub status: Status,
    /// How long it has held its status.
    pub age: Held,
    pub cwd: Option<String>,
    shared: Shared,
    /// The agent names a session that is still running, but its pane turned
    /// up in another, matched by its id and directory alone: where it sits is
    /// a guess. A session that is gone is one renamed, and no guess.
    pub guessed: bool,
    rank: usize,
    /// How it matches the search term, when there is one.
    found: Option<Found>,
}

impl AgentRow {
    /// `agent` in `seat` at `now`, going by what its pane is named.
    fn new(Seated { agent, seat, guessed }: Seated, live: &Live, now: Moment) -> Self {
        let mut row = AgentRow {
            pid: agent.pid,
            rank: live.rank(seat.session()),
            guessed,
            seat,
            name: agent.name.clone().map_or(Name::Session, Name::Chosen),
            pane: agent.pane,
            status: agent.status.clone(),
            age: now.since(agent.came.moment()),
            cwd: agent.cwd.clone(),
            shared: Shared::Not,
            found: None,
        };
        if matches!(row.name, Name::Session) {
            if let Some(leaf) = live.leaf(row.seat.session(), row.pane) {
                row.name = Name::Pane(label(leaf, Some(&row)));
            }
        }
        row
    }

    /// What the agent's row goes by.
    pub fn name(&self) -> &str {
        match &self.name {
            Name::Chosen(name) | Name::Pane(name) => name,
            Name::Session => self.seat.session(),
        }
    }

    /// The name a person gave the agent.
    pub fn chosen(&self) -> Option<&str> {
        match &self.name {
            Name::Chosen(name) => Some(name),
            Name::Pane(_) | Name::Session => None,
        }
    }

    pub fn label(&self) -> String {
        match self.shared {
            Shared::Not => self.name().to_owned(),
            Shared::InSession => format!("{}:{}", self.name(), self.pane),
            Shared::AcrossSessions => {
                format!("{} ({}:{})", self.name(), self.seat.session(), self.pane)
            },
        }
    }

    /// The characters of its label the search term lands on.
    pub fn hits(&self) -> &[usize] {
        self.found.as_ref().map_or(&[], |found| &found.hits)
    }

    /// The term against its label, which has the characters it lands on, and
    /// against its session's name, which has a score only.
    fn find(&self, matcher: &SkimMatcherV2, term: &str) -> Option<Found> {
        let label = self.label();
        let lit = matcher.fuzzy_indices(&label, term);
        let session = matcher.fuzzy_match(self.seat.session(), term);
        let score = lit.iter().map(|(score, _)| *score).chain(session).max()?;
        Some(Found {
            exact: [label.as_str(), self.name(), self.seat.session()].contains(&term),
            score,
            hits: lit.map(|(_, hits)| hits).unwrap_or_default(),
        })
    }
}

/// What a row goes by.
enum Name {
    /// The name a person gave the agent, with `/rename`.
    Chosen(String),
    /// What its pane is named, as `label` names it.
    Pane(String),
    /// Its session's, before zellij has listed its pane.
    Session,
}

/// How a row matches a search term.
struct Found {
    /// The term is its label, its name or its session's.
    exact: bool,
    score: i64,
    /// The characters of its label the term lands on.
    hits: Vec<usize>,
}

/// Whether another agent goes by a row's name, and so what tells them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shared {
    Not,
    /// Only agents of its session do, and its pane tells it from them.
    InSession,
    /// An agent of another session does, whose panes count from 0 as well:
    /// its session and its pane.
    AcrossSessions,
}

/// The word Claude Code wrote for a status luneta does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusWord(Said);

impl StatusWord {
    fn new(word: &str) -> Option<Self> {
        Said::new(word.to_owned()).map(StatusWord)
    }

    pub fn initial(&self) -> char {
        self.0.as_str().chars().next().expect("what is said is never empty")
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Idle,
    Busy,
    Shell,
    /// A status luneta does not know: a newer Claude Code's, by the word it
    /// wrote, or a newer daemon's, which has no word luneta can read.
    Unknown(Option<StatusWord>),
    /// Claude Code says none.
    Unreported,
}

impl Status {
    /// A status as the daemon says it: by its name where it knows the word,
    /// as the word Claude Code wrote where it does not, and none where Claude
    /// Code says none.
    fn of(reported: Option<&Reported>) -> Self {
        match reported {
            Some(Reported::KnownStatus(known)) => match v1::Status::try_from(*known) {
                Ok(v1::Status::Busy) => Status::Busy,
                Ok(v1::Status::Idle) => Status::Idle,
                Ok(v1::Status::Waiting) => Status::Waiting,
                Ok(v1::Status::Shell) => Status::Shell,
                // A daemon newer than this plugin's.
                Ok(v1::Status::Unspecified) | Err(_) => Status::Unknown(None),
            },
            Some(Reported::OtherStatus(word)) => Status::Unknown(StatusWord::new(word)),
            None => Status::Unreported,
        }
    }

    pub fn word(&self) -> &str {
        match self {
            Status::Waiting => "waiting",
            Status::Idle => "idle",
            Status::Busy => "busy",
            Status::Shell => "shell",
            Status::Unknown(Some(word)) => word.as_str(),
            Status::Unknown(None) => "unknown",
            Status::Unreported => "unreported",
        }
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self, Status::Waiting)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Status::Busy)
    }
}

/// An agent the daemon gives, where a delete key counts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counted {
    /// The session it sits in, or else the one its environment names.
    pub session: String,
    /// The pane it sits in. None where luneta found it in no pane, or
    /// guessed it into one its session's own agent holds: it may be in any
    /// pane of the session.
    pub pane: Option<u32>,
    pub status: Status,
}

impl Counted {
    fn seated(seated: &Seated) -> Self {
        Counted {
            session: seated.seat.session().to_owned(),
            pane: Some(seated.agent.pane),
            status: seated.agent.status.clone(),
        }
    }

    fn unplaced(agent: &Agent) -> Self {
        Counted {
            session: agent.housed.session().to_owned(),
            pane: None,
            status: agent.status.clone(),
        }
    }
}

#[derive(Default)]
pub struct AgentSet {
    pub status: Fetch<Vec<Agent>>,
    pub rows: Cursor<AgentRow>,
    /// The agent in the pane the picker was opened from, kept out of the rows
    /// as the session you are in is kept out of the session list.
    pub current: Option<AgentRow>,
    pub unplaced: usize,
    /// The agents left out for sitting in a pane that another agent holds.
    pub doubled: usize,
    /// Every agent the daemon gives, as a delete key counts them: those the
    /// rows leave out too, for sitting nowhere luneta can find, in a pane
    /// another agent holds, in one closing, in the pane the picker was opened
    /// from, or out of the term.
    counted: Vec<Counted>,
    /// The panes closing since the daemon last said what the agents are
    /// doing, whose agents stay out of the rows until it says again.
    closing: Vec<(String, u32)>,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
    /// What the daemon says of the agents, as of `now`, in the place of what
    /// it said before.
    pub fn heard(&mut self, agents: &[v1::Agent], now: Moment) {
        let before = match &self.status {
            Fetch::Ready(before) => before.as_slice(),
            Fetch::Waiting | Fetch::Failed(_) => &[],
        };
        let agents = agents.iter().filter_map(|agent| Agent::of(agent, now, before)).collect();
        self.status = Fetch::Ready(agents);
        self.closing.clear();
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.status = Fetch::Failed(reason.into());
    }

    /// The rows keep their order, whatever the agents do: their sessions' in
    /// the session list, and the order their panes were opened in within each.
    /// A pane holds one agent: of those the daemon puts in it, the one
    /// `holds_before` picks. The others are counted, and a guess that the
    /// pane's own agent refutes counts among the agents that sit nowhere
    /// luneta can find. A term puts the best match first, as it does in the
    /// session list. An agent is found by its label or by its session's name,
    /// and only its label is lit up. Each label is told apart from the others
    /// whatever the term leaves out, so that it stays what was typed. The
    /// cursor holds to its agent, wherever the agent turns up. Every agent,
    /// those the rows leave out too, is counted where a delete key ends it.
    /// Each row has held its status until `now`.
    pub fn rebuild(
        &mut self,
        term: &str,
        live: &Live,
        origin: Option<u32>,
        now: Moment,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_row().map(|row| row.pid),
        };
        let mut rows: Vec<AgentRow> = Vec::new();
        let mut current = None;
        let (mut unplaced, mut doubled) = (0, 0);
        let mut counted = Vec::new();

        if let Fetch::Ready(all) = &self.status {
            let mut seated: HashMap<(String, u32), Seated> = HashMap::with_capacity(all.len());
            counted.reserve(all.len());
            for agent in all {
                let Some(placed) = live.place(agent) else {
                    unplaced += 1;
                    counted.push(Counted::unplaced(agent));
                    continue;
                };
                let pane = (placed.seat.session().to_owned(), agent.pane);
                if self.closing.contains(&pane) {
                    counted.push(Counted::seated(&placed));
                    continue;
                }
                match seated.entry(pane) {
                    Entry::Vacant(pane) => {
                        pane.insert(placed);
                    },
                    Entry::Occupied(mut pane) => {
                        let out = if holds_before(&placed, pane.get()) {
                            pane.insert(placed)
                        } else {
                            placed
                        };
                        // A pane its session names refutes the guess that put
                        // another agent in it.
                        if out.guessed == pane.get().guessed {
                            doubled += 1;
                            counted.push(Counted::seated(&out));
                        } else {
                            unplaced += 1;
                            counted.push(Counted::unplaced(out.agent));
                        }
                    },
                }
            }
            counted.extend(seated.values().map(Counted::seated));
            let row = |placed| AgentRow::new(placed, live, now);
            let here = live.current.zip(origin).map(|(session, pane)| (session.to_owned(), pane));
            current = here.and_then(|here| seated.remove(&here)).map(row);
            rows = seated.into_values().map(row).collect();
        }

        mark_shared(&mut rows.iter_mut().chain(current.as_mut()).collect::<Vec<_>>());
        if !term.is_empty() {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
            rows.retain_mut(|row| {
                row.found = row.find(matcher, term);
                row.found.is_some()
            });
        }
        rows.sort_by(|a, b| {
            let fit = |row: &AgentRow| row.found.as_ref().map(|found| (found.exact, found.score));
            fit(b)
                .cmp(&fit(a))
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| a.seat.session().cmp(b.seat.session()))
                .then_with(|| a.pane.cmp(&b.pane))
        });

        self.current = current;
        self.unplaced = unplaced;
        self.doubled = doubled;
        self.counted = counted;

        self.rows.replace(rows, |row| Some(row.pid) == held);
    }

    /// Every agent the daemon gave, where a delete key counts it, as of the
    /// last `rebuild`.
    pub fn counted(&self) -> &[Counted] {
        &self.counted
    }

    pub fn selected_row(&self) -> Option<&AgentRow> {
        self.rows.selected_row()
    }

    /// Leaves the agents of the highlighted agent's pane out until the daemon
    /// next says what the agents are doing, for a pane that is closing: an
    /// agent in it can outlive what the daemon says now, and come back. A `claude -p` it runs goes with it, and an
    /// agent in another pane stays, whatever session it names. The rows are
    /// only redrawn by the next `rebuild`.
    pub fn forget_selected(&mut self) {
        let Some(row) = self.rows.selected_row() else {
            return;
        };
        self.closing.push((row.seat.session().to_owned(), row.pane));
    }

    /// Brings back the agents `forget_selected` left out, for a pane that did
    /// not close after all.
    pub fn stop_closing(&mut self) {
        self.closing.clear();
    }

    pub fn any_busy(&self) -> bool {
        self.rows.iter().any(|row| row.status.is_busy())
    }
}

fn mark_shared(rows: &mut [&mut AgentRow]) {
    let shared: Vec<Shared> = {
        let mut seen: HashMap<&str, Vec<&str>> = HashMap::with_capacity(rows.len());
        for row in rows.iter() {
            seen.entry(row.name()).or_default().push(row.seat.session());
        }
        rows.iter()
            .map(|row| match seen[row.name()].as_slice() {
                [_] => Shared::Not,
                alike if alike.iter().all(|session| *session == row.seat.session()) => {
                    Shared::InSession
                },
                _ => Shared::AcrossSessions,
            })
            .collect()
    };
    for (row, shared) in rows.iter_mut().zip(shared) {
        row.shared = shared;
    }
}

/// Whether `agent` is the one a pane holds rather than `held`, of two that
/// the daemon puts in it: one the pane's session names over one guessed into
/// it, then one started from a prompt over a `claude -p`, which has its
/// pane's ids too, and of two alike, the one that started first.
fn holds_before(agent: &Seated, held: &Seated) -> bool {
    let rank = |seated: &Seated| (!seated.guessed, seated.agent.cli);
    match rank(agent).cmp(&rank(held)) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => started_first(agent.agent, held.agent),
    }
}

/// Whether `agent` started before `held`, as far as Claude Code says: a start
/// it says comes before one it does not. A `claude -p` an agent runs starts
/// after it, which is all that tells them apart where Claude Code does not
/// say how each started.
fn started_first(agent: &Agent, held: &Agent) -> bool {
    match (agent.started, held.started) {
        (Some(agent), Some(held)) => agent < held,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn glyph(status: &Status, frame: u64) -> &'static str {
    match status {
        Status::Waiting => "🙋",
        Status::Idle => "☕",
        Status::Busy => SPINNER[(frame as usize) % SPINNER.len()],
        Status::Shell => "🐚",
        Status::Unknown(_) => "🛸",
        Status::Unreported => "❔",
    }
}

pub fn full_tag(status: &Status, frame: u64) -> String {
    format!("{} {}", glyph(status, frame), status.word().to_uppercase())
}

pub fn abbr_tag(status: &Status, frame: u64) -> String {
    match status {
        Status::Unknown(Some(word)) => format!("[{}]", word.initial().to_uppercase()),
        _ => glyph(status, frame).to_owned(),
    }
}

/// The name of an agent's session, if someone chose it: one Claude Code
/// derives from the directory is no name a row goes by. A name from
/// before Claude Code said who chose it is taken for chosen.
fn chosen_name(name: Option<&str>, chosen: bool) -> Option<String> {
    let name = name.map(str::trim).filter(|name| !name.is_empty())?;
    chosen.then(|| name.to_owned())
}

impl Agent {
    /// An agent as the daemon gives it, as of `now`, in the session its pane
    /// is in now: the daemon follows a session through a rename. Where it
    /// cannot tell, the name the agent's environment holds is all there is to
    /// go by. `before` is what the answer before said of the agents. None for
    /// one that runs outside zellij.
    fn of(agent: &v1::Agent, now: Moment, before: &[Agent]) -> Option<Self> {
        let zellij = agent.zellij.as_ref()?;
        let housed = match &zellij.session {
            Some(session) => Housed::Followed(session.clone()),
            None => Housed::Named(zellij.named.clone()),
        };
        if housed.session().is_empty() {
            return None;
        }
        let pid = Pid(agent.pid);
        let status = Status::of(agent.status.as_ref());
        // A status Claude Code does not date came when it was first heard: as
        // the answer before heard it, while it holds, or now.
        let dated = agent.status_since.and_then(|at| moment(at.seconds, at.nanos));
        let came = dated.map_or_else(
            || {
                let held = before.iter().find(|was| was.pid == pid && was.status == status);
                Came::Heard(held.map_or(now, |was| was.came.moment()))
            },
            Came::Dated,
        );
        let name = agent.name.as_ref().and_then(|name| {
            let source = name.source.map(NameSource::try_from);
            let chosen = matches!(source, None | Some(Ok(NameSource::User | NameSource::Peer)));
            chosen_name(Some(&name.text), chosen)
        });
        let started = agent.started_at.as_ref().and_then(|at| u64::try_from(at.seconds).ok());
        let entrypoint = agent.entrypoint.as_deref();
        Some(Agent {
            housed,
            pid,
            name,
            pane: zellij.pane,
            status,
            came,
            cwd: Some(agent.cwd.clone()).filter(|cwd| !cwd.is_empty()),
            started: started.and_then(NonZeroU64::new),
            cli: entrypoint.is_some_and(|entry| entry.eq_ignore_ascii_case("cli")),
        })
    }
}

/// A name a person gave the agent, or else what the pane says it is doing.
/// Claude Code titles a pane `✳ Claude Code` until it has a topic, and some
/// never get one. A tab that someone renamed says more than that, and else the
/// directory the agent works in, which tells two such agents apart. A shell at
/// its prompt titles its pane with its cwd cut down to initials, and is shown
/// its cwd's last two parts instead, as the agent list shows a cwd:
/// `luneta/main`, where `~/P/m/l/main` hides the repository behind an `l`.
fn label(leaf: &Leaf, agent: Option<&AgentRow>) -> String {
    if let Some(name) = agent.and_then(AgentRow::chosen) {
        return name.to_owned();
    }
    let title = bare_title(&leaf.title);
    if let Some(cwd) = leaf.cwd.as_deref().filter(|cwd| abbreviates(shell_title(title), cwd)) {
        return short_cwd(cwd);
    }
    if !title.is_empty() && title != GENERIC_TITLE {
        return title.to_owned();
    }
    if let Some(tab) = tab::named(&leaf.tab) {
        return tab.to_owned();
    }
    if let Some(cwd) = agent.and_then(|agent| agent.cwd.as_deref()) {
        return short_cwd(cwd);
    }
    if title.is_empty() {
        return format!("pane {}", leaf.id);
    }
    title.to_owned()
}

const GENERIC_TITLE: &str = "Claude Code";

/// Whether a title is the cwd, as a shell writes it: whole, or with `~` for
/// home and any part but the last cut short, as fish's `~/P/m/l/main` is
/// `/home/you/Projects/misc/luneta/main`. Each part of the title begins the
/// part of the cwd in its place, counting from the end. A cwd luneta has not
/// caught up with since a `cd` is not the one the title writes, and the title
/// stays until it has.
fn abbreviates(title: &str, cwd: &str) -> bool {
    let (written, rooted) = if title.starts_with('/') {
        (title, true)
    } else if let Some(rest) =
        title.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        (rest, false)
    } else {
        return false;
    };
    let written: Vec<&str> = written.split('/').filter(|part| !part.is_empty()).collect();
    let parts: Vec<&str> = cwd.split('/').filter(|part| !part.is_empty()).collect();
    // `~` stands for a part or more.
    let fits = if rooted { written.len() == parts.len() } else { written.len() < parts.len() };
    let begins = |(written, part): (&&str, &&str)| part.starts_with(written);
    fits && written.last().is_some_and(|last| parts.last() == Some(last))
        && written.iter().rev().zip(parts.iter().rev()).all(begins)
}

/// A shell's title without the host fish puts before it over SSH, as in
/// `[nixos] ~/P/m/l/main`: ten characters of its name at most, in brackets.
fn shell_title(title: &str) -> &str {
    let host = title.strip_prefix('[').and_then(|rest| rest.split_once("] "));
    let host = host.filter(|(host, _)| {
        !host.is_empty() && host.chars().count() <= 10 && !host.contains(char::is_whitespace)
    });
    host.map_or(title, |(_, rest)| rest)
}

/// The title without the glyph an agent leads it with: Claude Code's `✳` at
/// rest, and a turning frame while it works.
fn bare_title(title: &str) -> &str {
    let title = title.trim();
    let mut chars = title.chars();
    match chars.next() {
        Some(glyph) if is_agent_glyph(glyph) => chars.as_str().trim_start(),
        _ => title,
    }
}

fn is_agent_glyph(glyph: char) -> bool {
    matches!(glyph, '✳' | '◐' | '◑' | '✢' | '✶' | '✻' | '✽' | '·')
        || ('\u{2800}'..='\u{28ff}').contains(&glyph)
}

/// The moment a timestamp of the protocol's names, by its seconds since the
/// epoch and the nanoseconds after them, if it is after the epoch.
fn moment(seconds: i64, nanos: i32) -> Option<Moment> {
    let seconds = u64::try_from(seconds).ok()?;
    let millis = u64::try_from(nanos).ok()? / 1_000_000;
    Some(Moment::from_millis(seconds.checked_mul(1000)?.checked_add(millis)?))
}

/// Agents as the daemon gives them, for the tests of whatever shows them.
#[cfg(test)]
pub mod given {
    use std::time::{Duration, SystemTime};

    use luneta_protocol::v1::{self, agent::Status as Reported};

    use super::Status;
    use crate::elapsed::Moment;

    /// When the tests hear the daemon: long enough after the epoch for each
    /// status they date to have come after it.
    pub const NOW: Moment = Moment::from_millis(1_000_000_000);

    /// An agent as the daemon gives it: process `pid`, working in `cwd`, in
    /// `pane` of `session`, which its environment names and the daemon
    /// follows it to, and come to `status` 4 seconds before `NOW`.
    pub fn agent(pid: u32, session: &str, pane: u32, cwd: &str, status: &str) -> v1::Agent {
        v1::Agent {
            pid,
            cwd: cwd.to_owned(),
            status: Some(reported(status)),
            status_since: Some(ago(4)),
            zellij: Some(v1::Zellij {
                named: session.to_owned(),
                session: Some(session.to_owned()),
                pane,
            }),
            ..v1::Agent::default()
        }
    }

    /// `agent`, in a session the daemon cannot follow it to from the name its
    /// environment holds: one renamed before the daemon started, or gone.
    pub fn unfollowed(mut agent: v1::Agent) -> v1::Agent {
        if let Some(zellij) = &mut agent.zellij {
            zellij.session = None;
        }
        agent
    }

    /// `status` as the daemon says it: by its name, where it knows the word.
    pub fn reported(status: &str) -> Reported {
        let known = match status {
            "busy" => v1::Status::Busy,
            "idle" => v1::Status::Idle,
            "waiting" => v1::Status::Waiting,
            "shell" => v1::Status::Shell,
            word => return Reported::OtherStatus(word.to_owned()),
        };
        Reported::KnownStatus(known.into())
    }

    /// `status` as an agent the daemon gives has it.
    pub fn status(status: &str) -> Status {
        Status::of(Some(&reported(status)))
    }

    /// The moment `secs` before `NOW`, as the protocol spells a moment.
    pub fn ago<T: From<SystemTime>>(secs: u64) -> T {
        at(NOW.millis() - secs * 1000)
    }

    /// The moment `millis` after the epoch, as the protocol spells a moment.
    pub fn at<T: From<SystemTime>>(millis: u64) -> T {
        T::from(SystemTime::UNIX_EPOCH + Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::given::{self, agent, unfollowed, NOW};
    use crate::elapsed::Age;
    use super::*;
    use crate::close::Closes;

    /// The rows of the agents the daemon gives at `NOW`, in the session you
    /// are in, `current`.
    fn rows(agents: &[v1::Agent], current: Option<&str>, places: &Places) -> AgentSet {
        let mut set = AgentSet::default();
        set.heard(agents, NOW);
        let live = Live::new(current, &[], places, &[]);
        set.rebuild("", &live, None, NOW, Selection::SnapToTop);
        set
    }

    /// The agent the daemon gives as `given`, as of `NOW`.
    fn read(given: &v1::Agent) -> Agent {
        Agent::of(given, NOW, &[]).expect("an agent in zellij")
    }

    /// The session an agent's environment names is the one its pane opened
    /// in, which a rename leaves behind and another session can take up. The
    /// daemon follows a session through a rename, and where two sessions hold
    /// a pane like the agent's, the agent sits in the one the daemon says.
    /// Where the daemon cannot tell, the name the environment holds stands.
    #[test]
    fn an_agent_sits_in_the_session_the_daemon_follows_its_pane_to() {
        let places = Places::of(&[
            ("old", &[(0, "/w/repo"), (1, "/w/repo")]),
            ("renamed", &[(0, "/w/repo"), (1, "/w/repo")]),
        ]);
        let renamed = v1::Zellij {
            named: "old".to_owned(),
            session: Some("renamed".to_owned()),
            pane: 0,
        };
        let agents = [
            v1::Agent { zellij: Some(renamed), ..agent(100, "old", 0, "/w/repo", "idle") },
            unfollowed(agent(200, "old", 1, "/w/repo", "idle")),
        ];
        let agents = rows(&agents, None, &places);
        let seats: Vec<(u32, &Seat, bool)> =
            agents.rows.iter().map(|row| (row.pane, &row.seat, row.guessed)).collect();
        assert_eq!(
            seats,
            [
                (1, &Seat::There("old".to_owned()), false),
                (0, &Seat::There("renamed".to_owned()), false),
            ]
        );
    }

    /// An agent reads as the daemon gives it: a status the daemon knows by
    /// its name, a word it does not as the word, a number of a newer daemon's
    /// as unknown, and none as unreported; held since the daemon says it came
    /// to it; the name its session goes by only where someone chose it, or
    /// Claude Code does not say who did; and where it works.
    #[test]
    fn an_agent_reads_as_the_daemon_gives_it() {
        let known = |status: v1::Status| Some(Reported::KnownStatus(status.into()));
        let statuses = [
            (known(v1::Status::Busy), Status::Busy),
            (known(v1::Status::Idle), Status::Idle),
            (known(v1::Status::Waiting), Status::Waiting),
            (known(v1::Status::Shell), Status::Shell),
            (Some(given::reported("Compacting")), Status::Unknown(StatusWord::new("Compacting"))),
            (Some(Reported::OtherStatus(String::new())), Status::Unknown(None)),
            (known(v1::Status::Unspecified), Status::Unknown(None)),
            (Some(Reported::KnownStatus(99)), Status::Unknown(None)),
            (None, Status::Unreported),
        ];
        for (status, read_as) in statuses {
            let given = v1::Agent { status: status.clone(), ..agent(100, "s", 0, "/w/repo", "idle") };
            assert_eq!(read(&given).status, read_as, "{status:?}");
        }

        let sources = [
            (Some(NameSource::User), Some("fixes")),
            (Some(NameSource::Peer), Some("fixes")),
            (Some(NameSource::Derived), None),
            (Some(NameSource::Collision), None),
            (Some(NameSource::Auto), None),
            (Some(NameSource::Hook), None),
            (Some(NameSource::Other), None),
            (None, Some("fixes")),
        ];
        for (source, name) in sources {
            let named = v1::Name { text: "fixes".to_owned(), source: source.map(Into::into) };
            let given = v1::Agent { name: Some(named), ..agent(100, "s", 0, "/w/repo", "idle") };
            assert_eq!(read(&given).name.as_deref(), name, "{source:?}");
        }

        let read = read(&agent(100, "s", 0, "/w/repo", "idle"));
        assert_eq!(read.came, Came::Dated(NOW.back(Held::from_secs(4))));
        assert_eq!(read.cwd.as_deref(), Some("/w/repo"));
    }

    #[test]
    fn a_failure_leaves_no_agents_behind() {
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let mut agents = rows(&[agent(100, "s", 0, "/w/s", "idle")], None, &places);
        assert_eq!(agents.rows.len(), 1);

        agents.fail("lunetad does not answer");
        let live = Live::new(None, &[], &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);
        assert!(agents.rows.is_empty());
        assert!(agents.rows.selected().is_none());
    }

    /// An agent in pane 0 of `luneta`, which names `misc-luneta`, what the
    /// session was called before a rename the daemon did not see, and one in
    /// its pane 6.
    fn two_in_one_session() -> [v1::Agent; 2] {
        [
            unfollowed(agent(101, "misc-luneta", 0, "/w/luneta", "idle")),
            agent(102, "luneta", 6, "/w/luneta", "idle"),
        ]
    }

    fn luneta_holds_both() -> Places {
        Places::of(&[("luneta", &[(0, "/w/luneta"), (6, "/w/luneta")])])
    }

    #[test]
    fn a_renamed_session_takes_its_agents_with_it() {
        let agents = rows(&two_in_one_session(), Some("luneta"), &luneta_holds_both());

        let seats: Vec<&Seat> = agents.rows.iter().map(|row| &row.seat).collect();
        assert_eq!(seats, [&Seat::Here("luneta".to_owned()); 2]);
        let labels: Vec<String> = agents.rows.iter().map(super::AgentRow::label).collect();
        assert_eq!(labels, ["luneta:0", "luneta:6"]);
        assert_eq!(agents.unplaced, 0);
    }

    /// A pane of `/w/luneta`, as zellij lists it.
    fn leaf(id: u32, title: &str, tab: &str) -> Leaf {
        Leaf {
            id,
            title: title.to_owned(),
            tab: tab.to_owned(),
            tab_id: None,
            cwd: Some("/w/luneta".to_owned()),
            closes: Closes::Pane,
        }
    }

    #[test]
    fn a_title_loses_the_glyph_its_agent_leads_it_with() {
        assert_eq!(bare_title("✳ Floresta issue 1326"), "Floresta issue 1326");
        assert_eq!(bare_title("⠐ Floresta issue 1326"), "Floresta issue 1326");
        assert_eq!(bare_title("◑ Floresta issue 1326"), "Floresta issue 1326");
        assert_eq!(bare_title("✻Thinking"), "Thinking");
        assert_eq!(bare_title("  ~/P/m/floresta "), "~/P/m/floresta");
        assert_eq!(bare_title("nvim ✳"), "nvim ✳");
        assert_eq!(bare_title("✳"), "");
    }

    #[test]
    fn a_renamed_tab_names_an_agent_that_has_no_topic_yet() {
        let leaf = |title: &str, tab: &str| Leaf {
            id: 7,
            title: title.to_owned(),
            tab: tab.to_owned(),
            tab_id: None,
            cwd: None,
            closes: Closes::Pane,
        };
        assert_eq!(label(&leaf("✳ Claude Code", "liquid dual"), None), "liquid dual");
        assert_eq!(label(&leaf("✳ Claude Code", "Tab #13"), None), "Claude Code");
        assert_eq!(label(&leaf("✳ pix outflow", "liquid dual"), None), "pix outflow");
        assert_eq!(label(&leaf("", "Tab #2"), None), "pane 7");
        assert_eq!(label(&leaf("", "notes"), None), "notes");
    }

    #[test]
    fn a_shell_at_its_prompt_is_named_for_its_cwd() {
        let leaf = |title: &str, cwd: Option<&str>| Leaf {
            id: 7,
            title: title.to_owned(),
            tab: String::new(),
            tab_id: None,
            cwd: cwd.map(str::to_owned),
            closes: Closes::Pane,
        };
        let main = Some("/home/you/Projects/misc/luneta/main");
        assert_eq!(label(&leaf("~/P/m/l/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("[nixos] ~/P/m/l/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("~/Projects/misc/luneta/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("/home/you/Projects/misc/luneta/main", main), None), "luneta/main");
        // Before luneta is told the cwd, and after a `cd` it has not yet caught
        // up with.
        assert_eq!(label(&leaf("~/P/m/l/main", None), None), "~/P/m/l/main");
        let before = Some("/home/you/Projects/misc/luneta");
        assert_eq!(label(&leaf("~/P/m/l/main", before), None), "~/P/m/l/main");
        assert_eq!(label(&leaf("nvim ~/P/m/l/main", main), None), "nvim ~/P/m/l/main");
        assert_eq!(label(&leaf("~", Some("/home/you")), None), "~");
    }

    #[test]
    fn a_title_abbreviates_a_cwd_only_part_for_part() {
        let main = "/home/you/Projects/misc/luneta/main";
        assert!(abbreviates("~/P/m/l/main", main));
        assert!(abbreviates("~/l/main", main));
        assert!(abbreviates("~/main", main));
        assert!(abbreviates("~/.c/fish", "/home/you/.config/fish"));
        assert!(abbreviates("/etc/nixos", "/etc/nixos"));
        assert!(abbreviates("/h/y/P/m/l/main/", main));
        // A part that begins another, a last part cut short, and too many or
        // too few parts for what `~` and `/` stand for.
        assert!(!abbreviates("~/P/m/x/main", main));
        assert!(!abbreviates("~/P/m/l/ma", main));
        assert!(!abbreviates("/P/m/l/main", main));
        assert!(!abbreviates("~/home/you/Projects/misc/luneta/main", main));
        // Someone else's home, home itself, the root, and no path at all.
        assert!(!abbreviates("~luneta/main", main));
        assert!(!abbreviates("~", "/home/you"));
        assert!(!abbreviates("/", "/"));
        assert!(!abbreviates("main", main));
    }

    /// `luneta` has an agent with a topic in pane 29, and one in pane 31 that
    /// has none yet, in a tab named `review`.
    #[test]
    fn an_agent_goes_by_the_name_its_pane_gives_it() {
        let given = [
            agent(103, "luneta", 29, "/w/luneta", "busy"),
            agent(104, "luneta", 31, "/w/luneta", "idle"),
        ];
        let places = Places::of(&[("luneta", &[(29, "/w/luneta"), (31, "/w/luneta")])]);
        let branches = [Branch {
            name: "luneta".to_owned(),
            panes: vec![
                leaf(29, "✳ Name agents by their panes", "Tab #1"),
                leaf(31, "✳ Claude Code", "review"),
            ],
        }];
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let named = |agents: &AgentSet| -> Vec<String> {
            agents.rows.iter().map(AgentRow::label).collect()
        };

        let unlisted = Live::new(Some("luneta"), &[], &places, &[]);
        agents.rebuild("", &unlisted, None, NOW, Selection::SnapToTop);
        assert_eq!(named(&agents), ["luneta:29", "luneta:31"]);

        let live = Live::new(Some("luneta"), &[], &places, &branches);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);
        assert_eq!(named(&agents), ["Name agents by their panes", "review"]);

        agents.rebuild("luneta", &live, None, NOW, Selection::SnapToTop);
        assert_eq!(named(&agents), ["Name agents by their panes", "review"]);
        assert!(agents.rows.iter().all(|row| row.hits().is_empty()));

        agents.rebuild("rev", &live, None, NOW, Selection::SnapToTop);
        assert_eq!(named(&agents), ["review"]);
        assert_eq!(agents.rows[0].hits(), [0, 1, 2]);
    }

    /// `a` and `b` each have an agent in their pane 0, in `/w/luneta`, with no
    /// topic yet, in a tab nobody named: both go by `w/luneta`, and a pane's
    /// id alone names a pane in either.
    #[test]
    fn agents_named_alike_in_two_sessions_are_told_apart_by_their_sessions() {
        let given =
            [agent(105, "a", 0, "/w/luneta", "busy"), agent(106, "b", 0, "/w/luneta", "idle")];
        let places = Places::of(&[("a", &[(0, "/w/luneta")]), ("b", &[(0, "/w/luneta")])]);
        let branch = |name: &str| Branch {
            name: name.to_owned(),
            panes: vec![leaf(0, "✳ Claude Code", "Tab #1")],
        };
        let branches = [branch("a"), branch("b")];
        let sessions = [Session { name: "b".to_owned(), age: Age::from_secs(5) }];
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let live = Live::new(Some("a"), &sessions, &places, &branches);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);

        let labels: Vec<String> = agents.rows.iter().map(AgentRow::label).collect();
        assert_eq!(labels, ["w/luneta (a:0)", "w/luneta (b:0)"]);
    }

    /// `luneta` has two agents with no topic yet in `/w/luneta`, in panes 4
    /// and 6: what tells them apart is what one types to find one.
    #[test]
    fn an_agent_is_found_by_the_pane_that_tells_it_apart() {
        let given = [
            agent(107, "luneta", 4, "/w/luneta", "busy"),
            agent(108, "luneta", 6, "/w/luneta", "idle"),
        ];
        let places = Places::of(&[("luneta", &[(4, "/w/luneta"), (6, "/w/luneta")])]);
        let branches = [Branch {
            name: "luneta".to_owned(),
            panes: vec![leaf(4, "✳ Claude Code", "Tab #1"), leaf(6, "✳ Claude Code", "Tab #1")],
        }];
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let live = Live::new(Some("luneta"), &[], &places, &branches);
        agents.rebuild("luneta:6", &live, None, NOW, Selection::SnapToTop);

        let labels: Vec<String> = agents.rows.iter().map(AgentRow::label).collect();
        assert_eq!(labels, ["w/luneta:6"]);
        assert_eq!(agents.rows[0].hits(), [2, 3, 4, 5, 6, 7, 8, 9]);
    }

    /// The agent in pane 29 of `luneta`, of session `luneta`, started at
    /// `started` seconds after the epoch, if Claude Code says.
    fn in_29(pid: u32, status: &str, started: Option<u64>) -> v1::Agent {
        let started_at = started.map(|secs| given::at(secs * 1000));
        v1::Agent { started_at, ..agent(pid, "luneta", 29, "/w/luneta", status) }
    }

    /// The agent in pane 29 of `luneta` runs a `claude -p`, which has the
    /// pane's ids in its environment, and the daemon puts it in pane 29 too,
    /// listed before it.
    #[test]
    fn a_pane_holds_the_agent_that_started_in_it_first() {
        let given = [in_29(109, "busy", Some(200)), in_29(110, "waiting", Some(100))];
        let places = Places::of(&[("luneta", &[(29, "/w/luneta")])]);
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let live = Live::new(Some("luneta"), &[], &places, &[]);

        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);
        let statuses: Vec<&Status> = agents.rows.iter().map(|row| &row.status).collect();
        assert_eq!(statuses, [&Status::Waiting]);
        assert_eq!(agents.rows[0].label(), "luneta");
        assert_eq!(agents.doubled, 1);

        agents.rebuild("", &live, Some(29), NOW, Selection::SnapToTop);
        assert!(agents.rows.is_empty());
        assert_eq!(agents.current.as_ref().map(|row| &row.status), Some(&Status::Waiting));
        assert_eq!(agents.doubled, 1);
    }

    /// The statuses of the agents `given` puts in `luneta`'s pane 29, as the
    /// list shows them.
    fn held_in_29(given: &[v1::Agent]) -> Vec<Status> {
        let places = Places::of(&[("luneta", &[(29, "/w/luneta")])]);
        let mut agents = AgentSet::default();
        agents.heard(given, NOW);
        let live = Live::new(Some("luneta"), &[], &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);
        assert_eq!(agents.doubled, 1);
        agents.rows.iter().map(|row| row.status.clone()).collect()
    }

    /// A start Claude Code does not say comes after every start it does, and
    /// a start at the epoch is one it does not say.
    #[test]
    fn a_start_claude_code_does_not_say_comes_after_one_it_does() {
        for unsaid in [None, Some(0)] {
            let given = [in_29(1, "waiting", Some(200)), in_29(2, "busy", unsaid)];
            assert_eq!(held_in_29(&given), [Status::Waiting], "{unsaid:?}");
        }
    }

    /// Claude Code says how each agent started, which tells the one you talk
    /// to from a `claude -p` it runs whenever either started.
    #[test]
    fn a_pane_holds_the_agent_started_at_a_prompt_over_a_claude_p() {
        let entered = |entrypoint: &str, agent: v1::Agent| v1::Agent {
            entrypoint: Some(entrypoint.to_owned()),
            ..agent
        };
        let given = [
            entered("sdk-cli", in_29(1, "busy", Some(100))),
            entered("cli", in_29(2, "waiting", Some(200))),
        ];
        assert_eq!(held_in_29(&given), [Status::Waiting]);
    }

    /// `a` and `b` each have an agent in their pane 0, in `/w/luneta`, and
    /// `b` has not said what its panes are yet: its agent, which the daemon
    /// could not follow from `b`, is guessed into `a`'s pane 0, which `a`'s
    /// own agent sits in.
    #[test]
    fn a_pane_holds_the_agent_its_session_names_over_one_guessed_into_it() {
        let started = |secs: u64, agent: v1::Agent| v1::Agent {
            started_at: Some(given::at(secs * 1000)),
            ..agent
        };
        let given = [
            started(200, agent(1, "a", 0, "/w/luneta", "idle")),
            started(100, unfollowed(agent(2, "b", 0, "/w/luneta", "waiting"))),
        ];
        let places = Places::of(&[("a", &[(0, "/w/luneta")])]);
        let sessions = [Session { name: "b".to_owned(), age: Age::ZERO }];
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let live = Live::new(Some("a"), &sessions, &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);

        let held: Vec<(&Status, bool)> =
            agents.rows.iter().map(|row| (&row.status, row.guessed)).collect();
        assert_eq!(held, [(&Status::Idle, false)]);
        assert_eq!((agents.unplaced, agents.doubled), (1, 0));
    }

    /// `x` and `y` were each called `gone` once, and the agents in their pane
    /// 2 still name it, which the daemon cannot follow. `y`'s also runs a
    /// `claude -p`.
    fn once_gone_agents() -> [v1::Agent; 3] {
        let gone = |pid, cwd, status| unfollowed(agent(pid, "gone", 2, cwd, status));
        let started = |secs: u64, agent: v1::Agent| v1::Agent {
            started_at: Some(given::at(secs * 1000)),
            ..agent
        };
        [
            gone(1, "/w/x", "idle"),
            started(100, gone(2, "/w/y", "busy")),
            started(200, gone(3, "/w/y", "busy")),
        ]
    }

    fn once_gone(agents: &mut AgentSet) {
        let places = Places::of(&[("x", &[(2, "/w/x")]), ("y", &[(2, "/w/y")])]);
        let named =
            |name: &str, age: u64| Session { name: name.to_owned(), age: Age::from_secs(age) };
        let sessions = [named("x", 10), named("y", 20)];
        let live = Live::new(None, &sessions, &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::Hold);
    }

    #[test]
    fn the_cursor_holds_to_its_agent_while_another_names_the_same_pane() {
        let mut agents = AgentSet::default();
        agents.heard(&once_gone_agents(), NOW);
        once_gone(&mut agents);
        assert_eq!(seats(&agents), [("x", 2), ("y", 2)]);

        agents.rows.move_selection(1);
        once_gone(&mut agents);
        assert_eq!(agents.rows.selected(), Some(1));
    }

    #[test]
    fn a_closing_pane_takes_its_own_agents_and_no_others_until_the_daemon_says_again() {
        let mut agents = AgentSet::default();
        agents.heard(&once_gone_agents(), NOW);
        once_gone(&mut agents);
        agents.rows.move_selection(1);
        agents.forget_selected();
        once_gone(&mut agents);
        assert_eq!(seats(&agents), [("x", 2)]);

        agents.heard(&once_gone_agents(), NOW);
        once_gone(&mut agents);
        assert_eq!(seats(&agents), [("x", 2), ("y", 2)]);
    }

    #[test]
    fn the_pane_you_are_in_is_pinned_by_its_pane_and_not_its_name() {
        let places = luneta_holds_both();
        let mut agents = AgentSet::default();
        agents.heard(&two_in_one_session(), NOW);
        agents.rebuild(
            "misc",
            &Live::new(Some("luneta"), &[], &places, &[]),
            Some(0),
            NOW,
            Selection::SnapToTop,
        );
        assert!(agents.rows.is_empty());
        assert_eq!(agents.current.as_ref().map(|row| row.pane), Some(0));

        agents.rebuild(
            "",
            &Live::new(Some("luneta"), &[], &places, &[]),
            Some(0),
            NOW,
            Selection::SnapToTop,
        );
        let labels: Vec<String> = agents.rows.iter().map(AgentRow::label).collect();
        assert_eq!(labels, ["luneta:6"]);
        assert_eq!(agents.current.as_ref().map(AgentRow::label).as_deref(), Some("luneta:0"));
    }

    #[test]
    fn a_name_the_user_chose_outlives_a_rename() {
        let handoff = v1::Name {
            text: "handoff".to_owned(),
            source: Some(NameSource::User.into()),
        };
        let given = v1::Agent {
            name: Some(handoff),
            ..unfollowed(agent(111, "misc-luneta", 0, "/w/luneta", "idle"))
        };
        let places = Places::of(&[("luneta", &[(0, "/w/luneta")])]);
        let agents = rows(&[given], Some("luneta"), &places);
        assert_eq!(agents.rows[0].name(), "handoff");
        assert_eq!(agents.rows[0].seat, Seat::Here("luneta".to_owned()));
    }

    #[test]
    fn a_reported_name_that_still_names_a_holder_breaks_the_tie() {
        let given = unfollowed(agent(112, "ghostty", 0, "/w/misc", "idle"));
        let places = Places::of(&[
            ("luneta", &[(0, "/w/misc")]),
            ("ghostty", &[(0, "/w/misc")]),
        ]);
        let agents = rows(&[given], Some("luneta"), &places);
        assert_eq!(agents.rows[0].seat, Seat::There("ghostty".to_owned()));
        assert_eq!(agents.rows[0].name(), "ghostty");
    }

    #[test]
    fn the_session_holding_the_pane_the_agent_works_in_claims_it() {
        let given = unfollowed(agent(113, "bipa.git", 4, "/w/bipa/affiliate", "idle"));
        let places = Places::of(&[
            ("luneta", &[(0, "/w/luneta")]),
            ("affiliate", &[(0, "/w/bipa"), (4, "/w/bipa/affiliate")]),
        ]);
        let agents = rows(&[given], Some("luneta"), &places);
        assert_eq!(agents.rows[0].seat, Seat::There("affiliate".to_owned()));
        assert_eq!(agents.rows[0].name(), "affiliate");
    }

    #[test]
    fn a_pane_two_sessions_could_answer_for_is_left_alone() {
        let given = unfollowed(agent(114, "gone", 0, "/w/repo", "idle"));
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        let agents = rows(&[given], Some("one"), &places);
        assert!(agents.rows.is_empty());
        assert_eq!(agents.unplaced, 1);
    }

    #[test]
    fn an_agent_no_live_pane_answers_for_is_dropped() {
        let given = unfollowed(agent(115, "bipa.git", 4, "/w/bipa", "idle"));
        let places = Places::of(&[("luneta", &[(0, "/w/luneta")]), ("ghostty", &[(0, "/w/misc")])]);
        let agents = rows(&[given], Some("luneta"), &places);
        assert!(agents.rows.is_empty());
        assert_eq!(agents.unplaced, 1);
    }

    /// The daemon can say where an agent is before it says what the panes of
    /// its session are: an agent it followed sits where it says, and one it
    /// could not follow waits for the panes.
    #[test]
    fn before_the_daemon_lists_the_panes_only_the_agents_it_followed_are_placed() {
        let given = [
            agent(116, "ghostty", 0, "/w/misc", "busy"),
            unfollowed(agent(117, "ghostty", 1, "/w/misc", "idle")),
        ];
        let agents = rows(&given, None, &Places::default());
        assert_eq!(seats(&agents), [("ghostty", 0)]);
        assert_eq!(agents.unplaced, 1);
    }

    #[test]
    fn the_search_asks_the_name_the_session_goes_by_now() {
        let places = luneta_holds_both();
        let mut agents = AgentSet::default();
        agents.heard(&two_in_one_session(), NOW);
        agents.rebuild(
            "luneta",
            &Live::new(Some("luneta"), &[], &places, &[]),
            None,
            NOW,
            Selection::SnapToTop,
        );
        assert_eq!(agents.rows.len(), 2);
        agents.rebuild(
            "misc",
            &Live::new(Some("luneta"), &[], &places, &[]),
            None,
            NOW,
            Selection::SnapToTop,
        );
        assert!(agents.rows.is_empty());
    }

    /// An agent outside zellij, or in a session with no name, has no pane
    /// this screen can go to.
    #[test]
    fn an_agent_this_screen_cannot_address_is_dropped() {
        let outside = v1::Agent { zellij: None, ..agent(117, "s", 0, "/w/s", "idle") };
        let nameless = unfollowed(agent(119, "", 0, "/w/s", "idle"));
        let mut agents = AgentSet::default();
        agents.heard(&[outside, nameless, agent(120, "s", 0, "/w/s", "idle")], NOW);
        let Fetch::Ready(read) = &agents.status else { panic!("the daemon said") };
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].housed, Housed::Followed("s".to_owned()));
    }

    /// A status luneta does not know draws by the word Claude Code wrote, and
    /// as unknown where the daemon has no word for it.
    #[test]
    fn an_unknown_status_keeps_its_word_where_it_has_one() {
        let status = given::status("Compacting");
        assert_eq!(status.word(), "Compacting");
        assert_eq!(full_tag(&status, 0), "🛸 COMPACTING");
        assert_eq!(abbr_tag(&status, 0), "[C]");

        let wordless = Status::Unknown(None);
        assert_eq!(full_tag(&wordless, 0), "🛸 UNKNOWN");
        assert_eq!(abbr_tag(&wordless, 0), "🛸");
        assert_ne!(full_tag(&wordless, 0), full_tag(&Status::Unreported, 0));
    }

    /// An agent came to its status when the daemon says it did, as Claude
    /// Code dates it. A status the daemon cannot date came when an answer
    /// first said it, and the moment holds while the status does. A row has
    /// held its status from then until it is built, whenever the daemon
    /// last said.
    #[test]
    fn an_agent_came_to_its_status_when_the_daemon_says_or_else_first_said() {
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let mut agents = AgentSet::default();
        let mut came = |status: &str, since: Option<u64>, now: u64| {
            let given = v1::Agent {
                status_since: since.map(given::at),
                ..agent(100, "s", 0, "/w/s", status)
            };
            agents.heard(&[given], Moment::from_millis(now));
            let live = Live::new(None, &[], &places, &[]);
            agents.rebuild("", &live, None, Moment::from_millis(now + 500), Selection::Hold);
            let Fetch::Ready(read) = &agents.status else { panic!("the daemon said") };
            (read[0].came, agents.rows[0].age)
        };
        let (dated, heard) = (Came::Dated, Came::Heard);
        let (at, held) = (Moment::from_millis, Held::from_millis);
        assert_eq!(came("idle", Some(9_000), 10_400), (dated(at(9_000)), held(1_900)));
        assert_eq!(came("idle", Some(9_000), 20_000), (dated(at(9_000)), held(11_500)));
        assert_eq!(came("busy", None, 21_000), (heard(at(21_000)), held(500)));
        assert_eq!(came("busy", None, 30_000), (heard(at(21_000)), held(9_500)));
        assert_eq!(came("idle", None, 31_000), (heard(at(31_000)), held(500)));
        assert_eq!(came("idle", Some(35_000), 40_000), (dated(at(35_000)), held(5_500)));
    }

    #[test]
    fn an_unreported_status_reads_and_draws_as_itself() {
        assert_eq!(Status::Unreported.word(), "unreported");
        assert_eq!(full_tag(&Status::Unreported, 0), "❔ UNREPORTED");
        assert_eq!(abbr_tag(&Status::Unreported, 0), "❔");
        assert_ne!(
            full_tag(&Status::Unreported, 0),
            full_tag(&given::status("compacting"), 0)
        );
        assert!(!Status::Unreported.is_waiting());
        assert!(!Status::Unreported.is_busy());
    }

    /// Sessions `old` and `new` besides `here`, the one you are in, each with
    /// agents in panes 2 and 5. `status` gives each agent its status and how
    /// many seconds it has held it.
    fn three_sessions(status: impl Fn(&str, u32) -> (&'static str, u64)) -> AgentSet {
        let mut listed = Vec::new();
        for session in ["old", "here", "new"] {
            for pane in [2, 5] {
                let (status, held) = status(session, pane);
                let pid = u32::try_from(listed.len()).expect("a few agents");
                let cwd = format!("/w/{session}");
                let seated = agent(pid, session, pane, &cwd, status);
                listed.push(v1::Agent { status_since: Some(given::ago(held)), ..seated });
            }
        }
        let mut agents = AgentSet::default();
        agents.heard(&listed, NOW);
        rebuild_three(&mut agents);
        agents
    }

    fn rebuild_three(agents: &mut AgentSet) {
        let places = Places::of(&[
            ("old", &[(2, "/w/old"), (5, "/w/old")]),
            ("here", &[(2, "/w/here"), (5, "/w/here")]),
            ("new", &[(2, "/w/new"), (5, "/w/new")]),
        ]);
        let named =
            |name: &str, age: u64| Session { name: name.to_owned(), age: Age::from_secs(age) };
        let sessions = [named("old", 900), named("new", 60)];
        let live = Live::new(Some("here"), &sessions, &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::Hold);
    }

    fn seats(agents: &AgentSet) -> Vec<(&str, u32)> {
        agents.rows.iter().map(|row| (row.seat.session(), row.pane)).collect()
    }

    #[test]
    fn the_rows_stand_in_the_order_of_the_session_list_whatever_the_agents_do() {
        let order = [("here", 2), ("here", 5), ("new", 2), ("new", 5), ("old", 2), ("old", 5)];
        let quiet = three_sessions(|_, _| ("busy", 10));
        assert_eq!(seats(&quiet), order);
        let loud = three_sessions(|session, pane| match (session, pane) {
            ("old", 5) => ("waiting", 1),
            ("new", 2) => ("busy", 1),
            (_, 2) => ("idle", 3000),
            _ => ("waiting", 600),
        });
        assert_eq!(seats(&loud), order);
    }

    #[test]
    fn a_closing_pane_takes_only_its_own_agent_out_of_the_list() {
        let mut agents = three_sessions(|_, _| ("idle", 10));
        agents.rows.move_selection(3);
        agents.forget_selected();
        rebuild_three(&mut agents);
        assert_eq!(seats(&agents), [("here", 2), ("here", 5), ("new", 2), ("old", 2), ("old", 5)]);
    }

    /// `foo` has not said what its panes are yet, and `notes` has a pane 2 in
    /// the directory foo's agent works in, which the daemon could not follow
    /// from `foo`. `misc-notes` is what `notes` was called before a rename the
    /// daemon did not see.
    #[test]
    fn a_pane_found_for_an_agent_that_names_another_running_session_is_a_guess() {
        let given = [
            unfollowed(agent(128, "foo", 2, "/w/foo", "idle")),
            unfollowed(agent(129, "misc-notes", 5, "/w/notes", "idle")),
            agent(130, "notes", 7, "/w/notes", "idle"),
        ];
        let places = Places::of(&[("notes", &[(2, "/w/foo"), (5, "/w/notes"), (7, "/w/notes")])]);
        let named = |name: &str| Session { name: name.to_owned(), age: Age::ZERO };
        let sessions = [named("foo"), named("notes")];
        let mut agents = AgentSet::default();
        agents.heard(&given, NOW);
        let live = Live::new(None, &sessions, &places, &[]);
        agents.rebuild("", &live, None, NOW, Selection::SnapToTop);
        let guessed: Vec<(&str, u32, bool)> =
            agents.rows.iter().map(|row| (row.seat.session(), row.pane, row.guessed)).collect();
        assert_eq!(guessed, [("notes", 2, true), ("notes", 5, false), ("notes", 7, false)]);
    }

    #[test]
    fn only_two_statuses_are_asked_about_by_name() {
        let busy = Status::Busy;
        assert!(busy.is_busy() && !busy.is_waiting());
        let waiting = Status::Waiting;
        assert!(waiting.is_waiting() && !waiting.is_busy());
        assert_ne!(full_tag(&busy, 0), full_tag(&busy, 1));
    }
}
