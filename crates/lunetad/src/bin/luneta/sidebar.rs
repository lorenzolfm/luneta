//! The sidebar: every session on the host, the panes in each, and what the
//! agent in each pane is doing, as the daemon says, beside the zellij luneta
//! runs. A row takes zellij's client there.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use luneta_protocol::git::{Changes, Head, Upstream, WorkTree};
use luneta_protocol::said::Said;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Host, Session, Status};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How many columns the sidebar takes, as the plugin's column did.
pub const COLUMNS: u16 = 32;

const UNFOLDED: &str = "▾";
const FOLDED: &str = "▸";
const RULE: &str = "─";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How long the spinner shows each of its frames.
pub const SPIN: Duration = Duration::from_millis(100);

/// The colour of what wants you: an agent that waits for you, and the session
/// luneta's zellij is in.
const ACCENT: Color = Color::Magenta;
/// The colour of the session luneta's zellij is in.
const NAME: Color = Color::Cyan;
/// The colour of an agent at work.
const TAG: Color = Color::Yellow;

/// A session as the sidebar tells it from another of its name: zellij
/// versions with sockets of their own can each run one under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    name: String,
    started: SystemTime,
}

impl Listed {
    /// `session`, or none for one with no start: a daemon of another build's.
    fn of(session: &Session) -> Option<Self> {
        let started = SystemTime::try_from(session.started_at?).ok()?;
        Some(Listed { name: session.name.clone(), started })
    }
}

/// What a row stands for, by which the highlight keeps to it as rows come and
/// go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Session(Listed),
    Pane { session: Listed, pane: u32 },
}

impl Place {
    /// The name of its session. zellij's CLI goes to a session by its name
    /// alone, and to its own version's of two that share one.
    pub fn session(&self) -> &str {
        &self.listed().name
    }

    pub fn pane(&self) -> Option<u32> {
        match self {
            Place::Session(_) => None,
            Place::Pane { pane, .. } => Some(*pane),
        }
    }

    fn listed(&self) -> &Listed {
        match self {
            Place::Session(session) | Place::Pane { session, .. } => session,
        }
    }
}

/// What a pane's agent is doing, as its row marks it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mark {
    Waiting,
    Idle,
    Busy,
    Shell,
    /// A word the daemon passed on as Claude Code wrote it, by its initial.
    Other(Said),
    /// An agent that says nothing.
    Unreported,
}

impl Mark {
    fn of(agent: &Agent) -> Self {
        match &agent.status {
            Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
                Ok(Status::Waiting) => Mark::Waiting,
                Ok(Status::Idle) => Mark::Idle,
                Ok(Status::Busy) => Mark::Busy,
                Ok(Status::Shell) => Mark::Shell,
                Ok(Status::Unspecified) | Err(_) => Mark::Unreported,
            },
            Some(Reported::OtherStatus(word)) => {
                Said::new(word.clone()).map_or(Mark::Unreported, Mark::Other)
            },
            None => Mark::Unreported,
        }
    }

    /// Its glyph, which the spinner shows at `frame`, and its style.
    fn glyph(&self, frame: usize) -> (String, Style) {
        let style = Style::new();
        match self {
            Mark::Waiting => ("●".to_owned(), style.fg(ACCENT).add_modifier(Modifier::BOLD)),
            Mark::Idle => ("✓".to_owned(), style.fg(Color::Green).add_modifier(Modifier::BOLD)),
            Mark::Busy => (SPINNER[frame].to_owned(), style.fg(TAG)),
            Mark::Shell => ("$".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Other(word) => {
                let initial = word.as_str().chars().take(1).flat_map(char::to_uppercase);
                (initial.collect(), style.fg(TAG))
            },
            Mark::Unreported => ("?".to_owned(), style.add_modifier(Modifier::DIM)),
        }
    }
}

/// The agent in a pane, as its row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seat {
    mark: Mark,
    /// Since when it has been doing what it does.
    since: Option<SystemTime>,
}

impl Seat {
    fn of(agent: &Agent) -> Self {
        Seat { mark: Mark::of(agent), since: agent.status_since.and_then(|at| at.try_into().ok()) }
    }
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Session {
        session: Listed,
        folded: bool,
        /// Whether luneta's zellij is in it.
        here: bool,
        /// How many of its agents wait for you, and whether one is at work.
        waiting: usize,
        busy: bool,
        /// Whether it lists no pane under it: folded, or with none listed.
        bare: bool,
    },
    Pane {
        session: Listed,
        pane: u32,
        seat: Option<Seat>,
        label: String,
        /// The branch checked out where it works, and how its work tree
        /// stands, as starship shows it.
        checkout: Option<(String, String)>,
    },
}

impl Row {
    fn place(&self) -> Place {
        match self {
            Row::Session { session, .. } => Place::Session(session.clone()),
            Row::Pane { session, pane, .. } => {
                Place::Pane { session: session.clone(), pane: *pane }
            },
        }
    }

    /// How many lines it takes: a pane in a repository has its branch under it.
    fn height(&self) -> usize {
        match self {
            Row::Pane { checkout: Some(_), .. } => 2,
            _ => 1,
        }
    }

    /// Whether the highlight stops on it: a pane, or a session that stands in
    /// for its panes, folded or with none to list. A session's row heads its
    /// panes, and is no place to go.
    fn stops(&self) -> bool {
        match self {
            Row::Session { bare, .. } => *bare,
            Row::Pane { .. } => true,
        }
    }
}

/// What the sidebar asks of luneta.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// Nothing: draw again, at most.
    Nothing,
    /// Take zellij's client there.
    Go(Place),
    /// Give the focus back to zellij.
    Leave,
}

/// The rows the top of the sidebar takes: its rule, and a blank line.
const HEADER: usize = 2;

/// What the rule along the top counts, of the rows: the sessions, and the
/// agents in their panes that wait for you and that are at work, a folded
/// session's among them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    sessions: usize,
    waiting: usize,
    busy: usize,
}

#[derive(Default)]
pub struct Sidebar {
    host: Option<Host>,
    /// Why the daemon does not answer, while it does not.
    away: Option<String>,
    /// The title zellij gives luneta's terminal: the name of the session it
    /// is in, and then ` | ` and the title of the pane it has the focus on,
    /// when that has one.
    title: Option<String>,
    /// The names of the sessions folded. A fold keeps to the name, so that a
    /// session started again under it comes back folded, and the sessions
    /// that share one fold together.
    folded: BTreeSet<String>,
    rows: Vec<Row>,
    counts: Counts,
    /// How many sessions the daemon lists with no start, which the rows leave
    /// out, as they tell sessions apart by theirs.
    unlisted: usize,
    /// The row the highlight is on.
    cursor: Option<Place>,
    /// The first line of the rows on screen.
    top: usize,
    /// The last thing that went wrong, until the next key.
    notice: Option<String>,
}

impl Sidebar {
    pub fn heard(&mut self, host: Host) {
        self.host = Some(host);
        self.away = None;
        self.rebuild();
    }

    /// The daemon does not answer, for `why`. What it said last stands.
    pub fn lost(&mut self, why: String) {
        self.away = Some(why);
    }

    /// zellij titled luneta's terminal `title`.
    pub fn titled(&mut self, title: String) {
        if self.title.as_ref() == Some(&title) {
            return;
        }
        self.title = Some(title);
        self.rebuild();
    }

    /// The session zellij's client is in, by its title: the longest name of
    /// a session the daemon knows that the title is, or starts with before a
    /// ` | `, as a session's name can hold one too. Until the daemon says,
    /// what comes before the first.
    pub fn here(&self) -> Option<&str> {
        let title = self.title.as_deref()?;
        let named = |name: &&str| {
            title.strip_prefix(*name).is_some_and(|rest| rest.is_empty() || rest.starts_with(" | "))
        };
        let sessions = self.host.iter().flat_map(|host| &host.sessions);
        sessions
            .map(|session| session.name.as_str())
            .filter(named)
            .max_by_key(|name| name.len())
            .or_else(|| Some(title.split_once(" | ").map_or(title, |(session, _)| session)))
    }

    pub fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    /// Whether a spinner turns: the rule along the top has one while an agent
    /// on the rows is at work.
    pub fn turning(&self) -> bool {
        self.counts.busy > 0
    }

    fn rebuild(&mut self) {
        let Some(host) = &self.host else {
            return;
        };
        let here = self.here().map(str::to_owned);
        let agents = seated(&host.agents);
        let trees: HashMap<&str, &lunetad::api::WorkTree> =
            host.work_trees.iter().map(|tree| (tree.dir.as_str(), tree)).collect();
        let (sessions, unlisted) = oldest_first(&host.sessions);
        let mut counts = Counts { sessions: sessions.len(), ..Counts::default() };
        let mut rows = Vec::new();
        for (listed, session) in sessions {
            let mut panes: Vec<_> = session.panes.iter().flat_map(|panes| &panes.panes).collect();
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            let agent = |pane: u32| agents.get(&(session.name.as_str(), pane)).copied();
            let seats: Vec<Option<Seat>> =
                panes.iter().map(|pane| agent(pane.id).map(Seat::of)).collect();
            let marks = || seats.iter().flatten().map(|seat| &seat.mark);
            let waiting = marks().filter(|mark| **mark == Mark::Waiting).count();
            let busy = marks().filter(|mark| **mark == Mark::Busy).count();
            counts.waiting += waiting;
            counts.busy += busy;
            let folded = self.folded.contains(&listed.name);
            rows.push(Row::Session {
                session: listed.clone(),
                folded,
                here: here.as_deref() == Some(listed.name.as_str()),
                waiting,
                busy: busy > 0,
                bare: folded || panes.is_empty(),
            });
            if folded {
                continue;
            }
            for (pane, seat) in panes.into_iter().zip(seats) {
                let cwd = pane.cwd.as_deref();
                let checkout = cwd
                    .and_then(|cwd| trees.get(cwd))
                    .and_then(|tree| WorkTree::of_api(tree))
                    .and_then(|tree| checkout(&tree));
                rows.push(Row::Pane {
                    session: listed.clone(),
                    pane: pane.id,
                    seat,
                    label: label(agent(pane.id), pane.command.as_deref(), cwd, pane.id),
                    checkout,
                });
            }
        }
        self.rows = rows;
        self.counts = counts;
        self.unlisted = unlisted;
        self.settle();
    }

    /// Puts the highlight back on a row it stops on: the one it was on, or
    /// else the one where that was.
    fn settle(&mut self) {
        let at = self.cursor.as_ref().and_then(|place| {
            self.rows.iter().position(|row| row.place() == *place).or_else(|| {
                // A pane gone, or folded away, leaves the highlight on its
                // session.
                let session = Place::Session(place.listed().clone());
                self.rows.iter().position(|row| row.place() == session)
            })
        });
        let at = at.unwrap_or(0);
        let stop =
            (at..self.rows.len()).chain((0..at).rev()).find(|index| self.rows[*index].stops());
        self.cursor = stop.map(|index| self.rows[index].place());
    }

    fn cursor_index(&self) -> Option<usize> {
        let place = self.cursor.as_ref()?;
        // Each time the rows are built, `settle` puts the highlight on one of
        // them, and `step` and a click move it only onto one.
        let at = self.rows.iter().position(|row| row.place() == *place);
        Some(at.expect("the highlight is on a row"))
    }

    /// Moves the highlight `by` rows it stops on, down or up, and no further
    /// than the ends.
    fn step(&mut self, by: isize) {
        let stops: Vec<usize> = (0..self.rows.len()).filter(|at| self.rows[*at].stops()).collect();
        let Some(last) = stops.len().checked_sub(1) else {
            return;
        };
        let now = self.cursor_index().and_then(|at| stops.iter().position(|stop| *stop == at));
        let next = match now {
            Some(now) => now.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.cursor = Some(self.rows[stops[next]].place());
    }

    /// Folds the session of the highlighted row the other way.
    fn toggle(&mut self) {
        let Some(place) = &self.cursor else {
            return;
        };
        let folded = self.folded.contains(place.session());
        self.fold(!folded);
    }

    /// Folds the session of the highlighted row, or unfolds it. The highlight
    /// goes to the session's row as it folds.
    fn fold(&mut self, folded: bool) {
        let Some(place) = &self.cursor else {
            return;
        };
        let session = place.listed().clone();
        let changed = if folded {
            self.folded.insert(session.name.clone())
        } else {
            self.folded.remove(&session.name)
        };
        if !changed {
            return;
        }
        if folded {
            self.cursor = Some(Place::Session(session));
        }
        self.rebuild();
    }

    /// What a key does while the sidebar has the focus.
    pub fn key(&mut self, key: &KeyEvent) -> Asked {
        self.notice = None;
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return Asked::Nothing;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Left | KeyCode::Char('h') => self.fold(true),
            KeyCode::Right | KeyCode::Char('l') => self.fold(false),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Enter => {
                if let Some(place) = self.cursor.clone() {
                    return Asked::Go(place);
                }
            },
            KeyCode::Esc => return Asked::Leave,
            _ => {},
        }
        Asked::Nothing
    }

    /// What the mouse does over the sidebar, drawn in `area`: a click on a
    /// pane goes there, and on a session's `▾` or `▸` folds it. A folded
    /// session goes to the session. The wheel scrolls.
    pub fn mouse(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.top = (self.top + 3).min(self.lines().saturating_sub(1));
                return Asked::Nothing;
            },
            MouseEventKind::ScrollUp => {
                self.top = self.top.saturating_sub(3);
                return Asked::Nothing;
            },
            MouseEventKind::Down(MouseButton::Left) => {},
            _ => return Asked::Nothing,
        }
        let line = usize::from(mouse.row.saturating_sub(area.y));
        let Some(at) = line.checked_sub(HEADER).and_then(|line| self.row_at(self.top + line))
        else {
            return Asked::Nothing;
        };
        let row = &self.rows[at];
        let place = row.place();
        if let Row::Session { folded, .. } = row {
            let on_triangle = mouse.column.saturating_sub(area.x) <= 2;
            if on_triangle {
                self.cursor = Some(place);
                self.toggle();
                return Asked::Nothing;
            }
            // The name of a session that lists its panes is no place to go.
            if !folded {
                return Asked::Nothing;
            }
        }
        self.cursor = Some(place.clone());
        Asked::Go(place)
    }

    /// How many lines all the rows take.
    fn lines(&self) -> usize {
        self.rows.iter().map(Row::height).sum()
    }

    /// The row on `line` of all the rows, if one is.
    fn row_at(&self, line: usize) -> Option<usize> {
        let mut start = 0;
        for (at, row) in self.rows.iter().enumerate() {
            let end = start + row.height();
            if line < end {
                return Some(at);
            }
            start = end;
        }
        None
    }

    /// Scrolls so that the highlighted row shows on `room` lines.
    fn reveal(&mut self, room: usize) {
        let Some(at) = self.cursor_index() else {
            return;
        };
        let start: usize = self.rows[..at].iter().map(Row::height).sum();
        let end = start + self.rows[at].height();
        if start < self.top {
            self.top = start;
        } else if end > self.top + room {
            self.top = end.saturating_sub(room);
        }
        // The row shows whole, where it fits.
        if end - start <= room {
            assert!(self.top <= start);
            assert!(end <= self.top + room);
        }
    }

    /// Draws the sidebar into `area`, the highlight on its row while it has
    /// the focus. Its last column is left blank, to keep it off what is
    /// beside it.
    pub fn draw(&mut self, buffer: &mut Buffer, area: Rect, focused: bool, now: SystemTime) {
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let frame = frame(now);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        lines.push(self.header(width, frame));
        lines.push(Line::default());
        let notes = self.notes(width, focused);
        let room = height.saturating_sub(HEADER + notes.len());
        if focused {
            self.reveal(room);
        }
        if self.host.is_none() {
            let waiting = if self.away.is_some() { "" } else { "asking lunetad…" };
            lines.push(Line::styled(
                format!(" {waiting}"),
                Style::new().add_modifier(Modifier::DIM),
            ));
        } else if self.rows.is_empty() {
            lines.push(Line::styled(" no sessions", Style::new().add_modifier(Modifier::DIM)));
        }
        let cursor = if focused { self.cursor_index() } else { None };
        let mut drawn: Vec<Line<'static>> = Vec::new();
        for (at, row) in self.rows.iter().enumerate() {
            let lit = cursor == Some(at);
            for mut line in row_lines(row, width, frame, now) {
                if lit {
                    line = line.patch_style(Style::new().add_modifier(Modifier::REVERSED));
                }
                drawn.push(line);
            }
        }
        self.top = self.top.min(drawn.len().saturating_sub(1));
        lines.extend(drawn.into_iter().skip(self.top).take(room));
        lines.resize(height.saturating_sub(notes.len()), Line::default());
        lines.extend(notes);
        for (line, y) in lines.iter().zip(area.top()..area.bottom()) {
            buffer.set_style(Rect::new(area.x, y, area.width, 1), Style::reset());
            buffer.set_line(area.x, y, line, area.width.saturating_sub(1));
        }
    }

    /// The rule along the top: how many sessions the rows list, how many
    /// agents on them wait for you, and how many are at work.
    fn header(&self, width: usize, frame: usize) -> Line<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Counts { sessions, waiting, busy } = self.counts;
        let mut counts: Vec<Span<'static>> = Vec::new();
        for (count, mark) in [(waiting, Mark::Waiting), (busy, Mark::Busy)] {
            if count == 0 {
                continue;
            }
            if !counts.is_empty() {
                counts.push(Span::raw(" "));
            }
            let (glyph, style) = mark.glyph(frame);
            counts.push(Span::styled(glyph, style));
            counts.push(Span::styled(count.to_string(), style));
        }
        let word = match sessions {
            0 => String::new(),
            1 => " 1 session ".to_owned(),
            n => format!(" {n} sessions "),
        };
        let counted: usize = counts.iter().map(|span| span.content.width()).sum();
        let tail = if counted > 0 { counted + 3 } else { 0 };
        let rule = width.saturating_sub(1 + word.width() + tail);
        let mut spans = vec![Span::styled(RULE, dim), Span::styled(word, dim)];
        spans.push(Span::styled(RULE.repeat(rule), dim));
        if counted > 0 {
            spans.push(Span::raw(" "));
            spans.extend(counts);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(RULE, dim));
        }
        Line::from(spans)
    }

    /// What goes at the bottom: why the daemon does not answer, the sessions
    /// it lists that the rows leave out, what went wrong last, and the keys
    /// while the sidebar has the focus.
    fn notes(&self, width: usize, focused: bool) -> Vec<Line<'static>> {
        let mut notes = Vec::new();
        let error = Style::new().fg(Color::Red);
        if let Some(away) = &self.away {
            notes.extend(wrapped(&format!("lunetad: {away}"), width, error));
        }
        if self.unlisted > 0 {
            let count = self.unlisted;
            let sessions = if count == 1 { "session" } else { "sessions" };
            let unlisted = format!("lunetad: {count} {sessions} with no start, not listed");
            notes.extend(wrapped(&unlisted, width, error));
        }
        if let Some(notice) = &self.notice {
            notes.extend(wrapped(notice, width, error));
        }
        if focused {
            let dim = Style::new().add_modifier(Modifier::DIM);
            notes.push(Line::styled(cut(" ↵ go  h/l fold  esc back", width), dim));
        }
        notes
    }
}

/// The sessions the daemon lists, as the sidebar tells them apart, oldest
/// first, so that a row never moves under your eye or your mouse; and how
/// many it lists with no start, which are left out.
fn oldest_first(sessions: &[Session]) -> (Vec<(Listed, &Session)>, usize) {
    let mut listed: Vec<(Listed, &Session)> = sessions
        .iter()
        .filter_map(|session| Some((Listed::of(session)?, session)))
        .collect();
    let unlisted = sessions.len() - listed.len();
    listed.sort_by(|(a, _), (b, _)| (a.started, &a.name).cmp(&(b.started, &b.name)));
    (listed, unlisted)
}

/// The agent in each pane, by its session and the pane's id. A pane holds one:
/// the agent started at a prompt, or else the one that started first.
fn seated(agents: &[Agent]) -> HashMap<(&str, u32), &Agent> {
    let mut seated: HashMap<(&str, u32), &Agent> = HashMap::new();
    for agent in agents {
        let Some(zellij) = &agent.zellij else {
            continue;
        };
        let Some(session) = zellij.session.as_deref() else {
            continue;
        };
        let rank = |agent: &Agent| {
            let started = agent.started_at.as_ref().map(|at| (at.seconds, at.nanos));
            (agent.entrypoint.as_deref() != Some("cli"), started)
        };
        seated
            .entry((session, zellij.pane))
            .and_modify(|seat| {
                if rank(agent) < rank(seat) {
                    *seat = agent;
                }
            })
            .or_insert(agent);
    }
    seated
}

/// The shells, whose name says only that a pane is at its prompt.
const SHELLS: [&str; 7] = ["bash", "dash", "fish", "nu", "sh", "zsh", "elvish"];

/// What a pane's row is named: its agent by the name it was given, or else by
/// where it works; a program by its command line, the program by its binary's
/// name; a shell at its prompt by its directory's last two parts.
fn label(agent: Option<&Agent>, command: Option<&str>, cwd: Option<&str>, id: u32) -> String {
    if let Some(agent) = agent {
        return match &agent.name {
            Some(name) => name.text.clone(),
            None => short_cwd(&agent.cwd),
        };
    }
    let mut words = command.into_iter().flat_map(str::split_whitespace);
    let program = words.next().map(|program| {
        let program = program.rsplit('/').next().unwrap_or(program);
        program.strip_prefix('-').unwrap_or(program)
    });
    match program {
        Some(program) if !SHELLS.contains(&program) => {
            [program].into_iter().chain(words).collect::<Vec<_>>().join(" ")
        },
        _ => cwd.map_or_else(|| format!("pane {id}"), short_cwd),
    }
}

/// A directory as luneta shows one: its last two parts.
fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_owned(),
        [only] => (*only).to_owned(),
        [.., parent, base] => format!("{parent}/{base}"),
    }
}

/// The branch checked out in a work tree, and how the tree stands, or none
/// for a detached `HEAD` or a directory in no repository.
fn checkout(tree: &WorkTree) -> Option<(String, String)> {
    let WorkTree::Checkout { head: Head::Branch { name, upstream }, changes, stashes } = tree
    else {
        return None;
    };
    Some((name.as_str().to_owned(), glyphs(changes, *stashes, *upstream)))
}

/// The glyph of each kind of change there is, then the one for where the
/// branch stands against its upstream, where it has one: what starship's
/// `git_status` shows, with its default glyphs and in its order.
fn glyphs(changes: &Changes, stashes: u32, upstream: Option<Upstream>) -> String {
    let kinds = [
        (changes.conflicted, '='),
        (stashes, '$'),
        (changes.deleted, '✘'),
        (changes.renamed, '»'),
        (changes.modified, '!'),
        (changes.staged, '+'),
        (changes.untracked, '?'),
    ];
    let mut glyphs: String =
        kinds.iter().filter(|(count, _)| *count > 0).map(|(_, glyph)| *glyph).collect();
    match upstream.map(|upstream| (upstream.ahead, upstream.behind)) {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => glyphs.push('⇕'),
        Some((ahead, _)) if ahead > 0 => glyphs.push('⇡'),
        Some((_, behind)) if behind > 0 => glyphs.push('⇣'),
        _ => {},
    }
    glyphs
}

/// How long ago `since` was, in the unit that says the most of it.
fn age(since: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(since).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

/// The frame the spinner shows at `now`: the same on every row, and the next
/// one each `SPIN`.
fn frame(now: SystemTime) -> usize {
    let since = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let turns = since.as_millis() / SPIN.as_millis();
    let frames = u128::try_from(SPINNER.len()).expect("a spinner of ten frames");
    usize::try_from(turns % frames).expect("a frame of the spinner's")
}

/// The lines a row is drawn on, `width` wide.
fn row_lines(row: &Row, width: usize, frame: usize, now: SystemTime) -> Vec<Line<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match row {
        Row::Session { session, folded, here, waiting, busy, .. } => {
            let badge = badge(*waiting, *folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let name_style = if *here {
                Style::new().fg(NAME).add_modifier(Modifier::BOLD)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            let name = cut(&session.name, width.saturating_sub(4 + reserve + 1));
            let rule = width.saturating_sub(3 + name.width() + 1 + reserve + 1);
            let rule_style = if *here { Style::new().fg(ACCENT) } else { dim };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if *folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" "),
                Span::styled(RULE.repeat(rule), rule_style),
            ];
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            vec![Line::from(spans)]
        },
        Row::Pane { seat, label, checkout, .. } => {
            let mark = seat.as_ref().map(|seat| &seat.mark);
            let since = seat.as_ref().and_then(|seat| seat.since);
            let age = since.map(|since| age(since, now)).unwrap_or_default();
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            let label_style = match mark {
                Some(Mark::Waiting) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(_) => Style::new(),
                None => dim,
            };
            let mut spans = vec![Span::raw("   ")];
            match mark {
                Some(mark) => {
                    let (glyph, style) = mark.glyph(frame);
                    spans.push(Span::styled(glyph, style));
                },
                None => spans.push(Span::raw(" ")),
            }
            spans.push(Span::raw(" "));
            let label = cut(label, width.saturating_sub(5 + reserve + 1));
            let gap = width.saturating_sub(5 + label.width() + age.width() + 1);
            spans.push(Span::styled(label, label_style));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(age, dim));
            let mut lines = vec![Line::from(spans)];
            if let Some((branch, status)) = checkout {
                lines.push(checkout_line(branch, status, width));
            }
            lines
        },
    }
}

/// What a session's row ends in: how many of its agents wait for you, or else
/// the spinner, while it is `spinning`.
fn badge(waiting: usize, spinning: bool, frame: usize) -> Option<Span<'static>> {
    if waiting > 0 {
        let (dot, style) = Mark::Waiting.glyph(frame);
        return Some(Span::styled(format!("{dot}{waiting}"), style));
    }
    if spinning {
        let (spinner, style) = Mark::Busy.glyph(frame);
        return Some(Span::styled(spinner, style));
    }
    None
}

/// The line under a pane's row, `width` wide: the branch checked out where it
/// works, under its label, and how its work tree stands.
fn checkout_line(branch: &str, status: &str, width: usize) -> Line<'static> {
    let reserve = if status.is_empty() { 0 } else { status.width() + 1 };
    let branch = cut(branch, width.saturating_sub(5 + reserve + 1));
    let gap = width.saturating_sub(5 + branch.width() + status.width() + 1);
    Line::from(vec![
        Span::raw("     "),
        Span::styled(branch, Style::new().fg(Color::DarkGray)),
        Span::raw(" ".repeat(gap)),
        Span::raw(status.to_owned()),
    ])
}

/// `text` cut to `room` columns, with a `…` for what it took.
fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut taken = 0;
    for c in text.chars() {
        let width = c.width().unwrap_or(0);
        if taken + width + 1 > room {
            break;
        }
        kept.push(c);
        taken += width;
    }
    if room > 0 {
        kept.push('…');
    }
    kept
}

/// `text` on as many lines of `width` as it takes, a space in from the edge.
fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for c in text.chars() {
        if line.width() + c.width().unwrap_or(0) > room {
            lines.push(Line::styled(format!(" {line}"), style));
            line.clear();
        }
        line.push(c);
    }
    if !line.is_empty() {
        lines.push(Line::styled(format!(" {line}"), style));
    }
    lines
}

#[cfg(test)]
mod tests {
    use lunetad::api::{self, Pane, Panes, Version, Zellij};
    use prost_types::Timestamp;

    use super::*;

    fn pane(id: u32, tab: u32, cwd: &str, command: &str) -> Pane {
        Pane {
            id,
            tab,
            tab_name: format!("Tab #{}", tab + 1),
            cwd: Some(cwd.to_owned()),
            command: Some(command.to_owned()),
        }
    }

    fn session(name: &str, started: i64, panes: Vec<Pane>) -> Session {
        Session {
            name: name.to_owned(),
            started_at: Some(Timestamp { seconds: started, nanos: 0 }),
            panes: Some(Panes { panes }),
        }
    }

    fn listed(name: &str, started: u64) -> Listed {
        Listed {
            name: name.to_owned(),
            started: SystemTime::UNIX_EPOCH + Duration::from_secs(started),
        }
    }

    fn agent(session: &str, pane: u32, status: Status, name: Option<&str>) -> Agent {
        Agent {
            pid: pane,
            session_id: format!("id-{pane}"),
            cwd: "/w/luneta/main".to_owned(),
            status: Some(Reported::KnownStatus(status.into())),
            status_since: Some(Timestamp { seconds: 100, nanos: 0 }),
            started_at: Some(Timestamp { seconds: 50, nanos: 0 }),
            name: name.map(|text| api::Name { text: text.to_owned(), source: None }),
            entrypoint: Some("cli".to_owned()),
            zellij: Some(Zellij {
                named: session.to_owned(),
                session: Some(session.to_owned()),
                pane,
            }),
        }
    }

    fn host(sessions: Vec<Session>, agents: Vec<Agent>) -> Host {
        Host {
            version: Some(Version { pid: 1, seq: 1 }),
            sessions,
            work_trees: Vec::new(),
            agents,
            unread: 0,
        }
    }

    /// The sidebar `width` columns wide and `height` tall, as text.
    fn screen(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        sidebar.draw(&mut buffer, area, focused, now);
        (0..height)
            .map(|y| {
                let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn two_sessions() -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session(
                    "older",
                    10,
                    vec![
                        pane(2, 1, "/w/c", "nvim src/main.rs"),
                        pane(1, 0, "/w/luneta/main", "claude"),
                    ],
                ),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        ));
        sidebar.titled("older | ~/w/luneta".to_owned());
        sidebar
    }

    #[test]
    fn sessions_go_oldest_first_with_their_panes_in_tab_order() {
        let mut sidebar = two_sessions();
        assert_eq!(
            screen(&mut sidebar, false, 32, 7),
            [
                "─ 2 sessions ───────────── ●1 ─",
                "",
                " ▾ older ────────────────── ●1",
                "   ● fold                   1m",
                "     nvim src/main.rs",
                " ▾ newer ─────────────────────",
                "     w/b",
            ]
        );
    }

    /// The rule along the top counts the agents on the rows, a folded
    /// session's among them, and none that no row holds.
    #[test]
    fn the_top_rule_counts_the_agents_the_rows_hold() {
        let mut sidebar = Sidebar::default();
        let mut outside = agent("s", 2, Status::Waiting, None);
        outside.zellij = None;
        sidebar.heard(host(
            vec![session("s", 10, vec![pane(1, 0, "/w/a", "claude")])],
            vec![agent("s", 1, Status::Busy, None), outside],
        ));
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let rule = screen(&mut sidebar, false, 32, 3)[0].clone();
        assert_eq!((rule.as_str(), sidebar.turning()), ("─ 1 session ────────────── ⠋1 ─", true));
    }

    /// A daemon of another build can list a session with no start, which the
    /// sidebar cannot tell from another of its name.
    #[test]
    fn a_session_with_no_start_is_left_out_and_said() {
        let mut sidebar = Sidebar::default();
        let mut unstarted = session("x", 0, vec![]);
        unstarted.started_at = None;
        sidebar.heard(host(vec![unstarted, session("y", 20, vec![])], Vec::new()));
        let lines = screen(&mut sidebar, false, 60, 5);
        assert_eq!(lines[0], format!("─ 1 session {}", "─".repeat(47)));
        assert_eq!(lines[2], format!(" ▾ y {}", "─".repeat(53)));
        assert_eq!(lines[4], " lunetad: 1 session with no start, not listed");
    }

    #[test]
    fn the_highlight_skips_a_session_s_row_and_goes_no_further_than_the_ends() {
        let mut sidebar = two_sessions();
        let older = Some(Place::Pane { session: listed("older", 10), pane: 1 });
        let newer = Some(Place::Pane { session: listed("newer", 20), pane: 1 });
        assert_eq!(sidebar.cursor, older);
        sidebar.step(1);
        sidebar.step(1);
        assert_eq!(sidebar.cursor, newer);
        sidebar.step(1);
        assert_eq!(sidebar.cursor, newer);
        sidebar.step(-5);
        assert_eq!(sidebar.cursor, older);
    }

    /// zellij versions with sockets of their own can each run a session under
    /// one name, and the daemon lists them both, with no panes.
    #[test]
    fn the_highlight_goes_on_past_a_second_session_of_a_name() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
            vec![
                session("x", 10, vec![]),
                session("y", 20, vec![pane(1, 0, "/w/y", "fish")]),
                session("x", 30, vec![]),
                session("z", 40, vec![]),
            ],
            Vec::new(),
        ));
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        for _ in 0..3 {
            sidebar.key(&down);
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter), Asked::Go(Place::Session(listed("z", 40))));
    }

    #[test]
    fn folding_a_pane_s_session_stops_the_highlight_on_its_row() {
        let mut sidebar = two_sessions();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let older = Place::Session(listed("older", 10));
        assert_eq!(sidebar.cursor, Some(older.clone()));
        assert_eq!(sidebar.key(&enter), Asked::Go(older));
        let lines = screen(&mut sidebar, false, 32, 5);
        assert!(lines[2].starts_with(" ▸ older"), "{lines:?}");
        assert!(lines[3].starts_with(" ▾ newer"), "{lines:?}");
        sidebar.key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(sidebar.rows.len(), 5);
    }

    #[test]
    fn a_gone_pane_leaves_the_highlight_in_its_session() {
        let mut sidebar = two_sessions();
        sidebar.step(1);
        sidebar.heard(host(
            vec![session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")])],
            Vec::new(),
        ));
        assert_eq!(sidebar.cursor, Some(Place::Pane { session: listed("older", 10), pane: 1 }));
    }

    /// Focused, on fewer lines than its rows take, the sidebar scrolls them
    /// to keep the highlight on screen as it moves down, and back up.
    #[test]
    fn the_rows_scroll_to_keep_the_highlight_on_screen() {
        let mut sidebar = two_sessions();
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        // Two lines, between the rule along the top and the keys at the bottom.
        let shown = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 5)[2..4].to_vec();
        assert_eq!(
            shown(&mut sidebar),
            [" ▾ older ────────────────── ●1", "   ● fold                   1m"]
        );
        sidebar.key(&down);
        sidebar.key(&down);
        assert_eq!(shown(&mut sidebar), [" ▾ newer ─────────────────────", "     w/b"]);
        sidebar.key(&up);
        assert_eq!(
            shown(&mut sidebar),
            ["     nvim src/main.rs", " ▾ newer ─────────────────────"]
        );
    }

    #[test]
    fn a_click_goes_to_a_pane_and_folds_on_the_triangle() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 10);
        let click = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let pane = Place::Pane { session: listed("older", 10), pane: 2 };
        assert_eq!(sidebar.mouse(click(10, 4), area), Asked::Go(pane));
        assert_eq!(sidebar.mouse(click(10, 2), area), Asked::Nothing);
        assert!(!sidebar.folded.contains("older"));
        assert_eq!(sidebar.mouse(click(1, 2), area), Asked::Nothing);
        assert!(sidebar.folded.contains("older"));
        let older = Place::Session(listed("older", 10));
        assert_eq!(sidebar.mouse(click(10, 2), area), Asked::Go(older));
    }

    #[test]
    fn the_session_is_the_one_zellij_titles_the_terminal_for() {
        let mut sidebar = Sidebar::default();
        sidebar.titled("odd | name | ~/w".to_owned());
        assert_eq!(sidebar.here(), Some("odd"));
        sidebar
            .heard(host(vec![session("odd | name", 1, vec![]), session("odd", 2, vec![])], vec![]));
        assert_eq!(sidebar.here(), Some("odd | name"));
        sidebar.titled("odd".to_owned());
        assert_eq!(sidebar.here(), Some("odd"));
        sidebar.titled("oddly | ~/w".to_owned());
        assert_eq!(sidebar.here(), Some("oddly"));
    }

    #[test]
    fn a_pane_is_named_for_what_runs_in_it() {
        assert_eq!(label(None, Some("-fish"), Some("/w/luneta/main"), 3), "luneta/main");
        assert_eq!(label(None, Some("/bin/zsh -l"), Some("/w/x"), 3), "w/x");
        assert_eq!(label(None, Some("cargo test"), Some("/w/x"), 3), "cargo test");
        let nvim = "/nix/store/a1p5-neovim/bin/nvim --clean";
        assert_eq!(label(None, Some(nvim), Some("/w/x"), 3), "nvim --clean");
        assert_eq!(label(None, None, None, 3), "pane 3");
        let named = agent("s", 1, Status::Idle, Some("fold"));
        assert_eq!(label(Some(&named), Some("claude"), Some("/w/x"), 1), "fold");
        let unnamed = agent("s", 1, Status::Idle, None);
        assert_eq!(label(Some(&unnamed), Some("claude"), Some("/w/x"), 1), "luneta/main");
    }

    #[test]
    fn a_pane_holds_the_agent_started_at_a_prompt() {
        let mut prompt = agent("s", 1, Status::Idle, Some("prompt"));
        prompt.started_at = Some(Timestamp { seconds: 90, nanos: 0 });
        let mut headless = agent("s", 1, Status::Busy, Some("headless"));
        headless.entrypoint = Some("sdk-cli".to_owned());
        let agents = [headless, prompt];
        let seated = seated(&agents);
        assert_eq!(
            seated
                .get(&("s", 1))
                .and_then(|agent| agent.name.as_ref())
                .map(|name| name.text.as_str()),
            Some("prompt")
        );
    }

    /// Starship's `git_status`, with the defaults.
    #[test]
    fn the_work_tree_shows_what_starship_shows() {
        let changes =
            Changes { conflicted: 1, deleted: 1, renamed: 1, modified: 1, staged: 1, untracked: 1 };
        let both = Some(Upstream { ahead: 1, behind: 2 });
        assert_eq!(glyphs(&changes, 1, both), "=$✘»!+?⇕");
        assert_eq!(glyphs(&Changes::default(), 0, Some(Upstream { ahead: 0, behind: 2 })), "⇣");
        assert_eq!(glyphs(&Changes::default(), 0, None), "");
    }

    #[test]
    fn a_cut_leaves_room_for_its_ellipsis() {
        assert_eq!(cut("handoff-eng-11259", 8), "handoff…");
        assert_eq!(cut("short", 8), "short");
        assert_eq!(cut("日本語", 4), "日…");
    }
}
