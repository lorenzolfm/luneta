//! The picker: the sessions on this host, those that run and those zellij
//! keeps to bring back, and the agents in their panes, in a box over zellij,
//! as the plugin's picker floated over a tab. `Ctrl j` opens it on the
//! sessions and `Ctrl a` on the agents, `Tab` goes from one list to the
//! other, and what you type narrows both. `Enter` takes zellij to the
//! session highlighted, bringing back one that ended, or to the pane of the
//! agent highlighted, or makes the session you typed the name of. `Del`
//! kills the session highlighted, or lets go of one that ended, or closes
//! the agent's pane, asking first as the menu does while an agent at work
//! would end; `Ctrl r` renames the session highlighted. Beside the list, the
//! preview shows the pane highlighted, or the one a client coming to the
//! session highlighted is on, as zellij dumps its screen.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions as _, GridCell as _};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::close::{Act, Deletes, Plan};
use crate::gruvbox;
use crate::host::Event;
use crate::hosted;
use crate::menu;
use crate::sidebar::{self, ACCENT, ERROR, Finder, Fit, LIT, Mark, NAME, Place};
use crate::zellij::{self, Closes, Exited, SessionName};

/// What the rule over the box, and the rule over the list, name it.
const TITLE: &str = "luneta";

/// How long the highlight stays on a row before the preview asks zellij for
/// its screen, so that going down the rows asks for none of those passed.
const PREVIEW_DELAY: Duration = Duration::from_millis(200);

/// The most screens the previews keep. Once they have that many, they forget
/// them all, and ask again for those shown next.
const PEEKS_MAX: usize = 32;

/// The most lines of a screen a preview keeps, the last of them.
const PEEK_LINES_MAX: usize = 200;

/// The most columns of a screen a preview keeps, the first of them.
const PEEK_COLUMNS_MAX: usize = 1000;

/// The rows the search takes, under the list: its box.
const INPUT_HEIGHT: u16 = 3;

/// The fewest rows the list takes, its box's two included, where it shows.
const MIN_RESULTS: u16 = 3;

/// The fewest columns the list and the preview each take, where both show.
const MIN_HALF: u16 = 26;

/// The fewest columns that the action `Enter` takes, after the search,
/// takes to show.
const MIN_ACTION: usize = 12;

/// What the picker lists of the host, as the sidebar knows it.
#[derive(Debug, Default)]
pub struct Listing {
    /// The session luneta's zellij is in, or on its way to.
    pub here: Option<String>,
    /// The pane luneta's zellij is on, where the sidebar knows it.
    pub here_pane: Option<Place>,
    /// The sessions that run, newest first.
    pub sessions: Vec<Live>,
    /// The agents, those of the session luneta's zellij is in first, then
    /// those of the others newest first, each session's in the order their
    /// panes opened.
    pub agents: Vec<Seated>,
    /// Why the daemon does not answer, while it does not.
    pub away: Option<String>,
}

/// A session that runs.
#[derive(Clone, Debug)]
pub struct Live {
    pub place: Place,
    pub started: SystemTime,
    /// Its panes, in the order of their tabs.
    pub panes: Vec<Shown>,
    /// The pane a client of it is on, which a client coming there is on
    /// too, if a client is in it.
    pub on: Option<u32>,
    /// How many of its agents wait for you.
    pub waiting: usize,
}

/// A pane of a session, as its preview says what it shows.
#[derive(Clone, Debug)]
pub struct Shown {
    pub id: u32,
    pub tab: String,
    /// What the sidebar names its row.
    pub label: String,
}

/// An agent, in its pane.
#[derive(Clone, Debug)]
pub struct Seated {
    pub place: Place,
    /// What the sidebar names its row.
    pub label: String,
    pub mark: Mark,
    /// What it is doing, in the word Claude Code says it in.
    pub status: String,
    /// Since when it has been doing it.
    pub since: Option<SystemTime>,
    /// Where it works, as its row and its preview show it, and a search finds
    /// it: its directory's last two parts, as a whole path has the letters for
    /// a few typed ones to land on nearly any of it.
    pub dir: String,
}

/// A list of the picker's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Sessions,
    Agents,
}

impl List {
    /// The other list, which `Tab` goes to.
    fn other(self) -> List {
        match self {
            List::Sessions => List::Agents,
            List::Agents => List::Sessions,
        }
    }

    fn title(self) -> &'static str {
        match self {
            List::Sessions => "Sessions",
            List::Agents => "Agents",
        }
    }
}

/// What the picker asks of luneta.
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    /// Close the picker.
    Close,
    /// Take zellij's client there, and close the picker.
    Go(Place),
    /// Take zellij's client to the session of this name, which zellij makes,
    /// or brings back where it kept one that ended, and close the picker.
    Start(SessionName),
    /// Plan deleting what `deletes` deletes of `session`, and hand the plan
    /// to `planned`.
    Plan { session: String, deletes: Deletes },
    /// Have zellij do what a delete planned.
    Delete(Act),
}

/// What zellij answered what the picker asked of it, on threads of their
/// own.
pub enum Heard {
    /// The sessions zellij keeps to bring back, or why it did not list them.
    Exited(Result<Vec<Exited>, String>),
    /// The screen of the terminal `asked` of `session`, or of the one a
    /// client coming there would be on, where it asked of none: the terminal,
    /// and what it shows.
    Peek { session: String, asked: Option<u32>, peek: Result<(u32, String), String> },
    /// What closing the pane at `place` closes, for its delete.
    Closes { place: Place, closes: Result<Closes, String> },
    /// Whether the session `from` is called `to` now.
    Renamed { from: String, to: SessionName, renamed: Result<(), String> },
    /// Whether zellij let go of the session `session`, one that ended.
    Deleted { session: SessionName, deleted: Result<(), String> },
}

/// What the picker asks of zellij, which luneta asks on a thread of its own
/// with `ask`, and hands back what zellij answers as `Heard`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The sessions zellij keeps to bring back.
    Exited,
    /// The screen the preview shows: of the terminal `asked` of `session`,
    /// or else of the one a client coming there would be on.
    Peek { session: String, asked: Option<u32> },
    /// What closing the pane at `place` closes.
    Closes(Place),
    /// To call the session `from` `to`.
    Rename { from: String, to: SessionName },
    /// To let go of the session `session`, one that ended.
    Delete(SessionName),
}

/// Asks zellij what `ask` asks, for the picker numbered `number`, on a
/// thread of its own, and hands back what it answers.
pub fn ask(number: u64, ask: Ask, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let heard = match ask {
            Ask::Exited => Heard::Exited(zellij::exited()),
            Ask::Peek { session, asked } => {
                let pane = asked.map_or_else(|| zellij::focused(&session), Ok);
                let peek = pane.and_then(|pane| Ok((pane, zellij::dumped(&session, pane)?)));
                Heard::Peek { session, asked, peek }
            },
            Ask::Closes(place) => {
                let pane = place.pane().ok_or_else(|| "no pane to close".to_owned());
                let closes = pane.and_then(|pane| zellij::closes_now(place.session(), pane));
                Heard::Closes { place, closes }
            },
            Ask::Rename { from, to } => {
                let renamed = zellij::renamed(&from, &to);
                Heard::Renamed { from, to, renamed }
            },
            Ask::Delete(session) => {
                let deleted = zellij::deleted(session.as_str());
                Heard::Deleted { session, deleted }
            },
        };
        // luneta is on its way out.
        let _ = events.send(Event::Picker { number, heard });
    });
}

/// What a row of a list stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    /// A session that runs, the one luneta's zellij is in among them.
    Live(Place),
    /// A session that ended, which zellij keeps to bring back.
    Exited(SessionName),
    /// The pane of an agent.
    Agent(Place),
}

/// A row of a list: what it stands for, the name it shows, and the
/// characters of that name a search found.
#[derive(Clone, Debug)]
struct Row {
    key: Key,
    name: String,
    hits: Vec<usize>,
}

/// The rows of a list, and the one highlighted: none while there are none.
#[derive(Debug, Default)]
struct Rows {
    rows: Vec<Row>,
    at: Option<usize>,
}

impl Rows {
    /// `rows` in the place of these, the highlight on the row of `held`
    /// where one is; or else, where that row went, on the row that took its
    /// place, as the one after a row deleted; or else on the row `start`. No
    /// further than the last.
    fn replace(&mut self, rows: Vec<Row>, held: Option<&Key>, start: usize) {
        let at = held.and_then(|held| rows.iter().position(|row| row.key == *held));
        let was = self.at.filter(|_| held.is_some()).unwrap_or(start);
        let last = rows.len().checked_sub(1);
        self.at = at.or_else(|| last.map(|last| was.min(last)));
        self.rows = rows;
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.at?)
    }

    /// Moves the highlight `by` rows down, or up, and no further than the
    /// last row or the first.
    fn step(&mut self, by: isize) {
        let Some(at) = self.at else {
            return;
        };
        let last = self.rows.len().saturating_sub(1);
        let to = if by < 0 { at.saturating_sub(by.unsigned_abs()) } else { at + by.unsigned_abs() };
        self.at = Some(to.min(last));
    }
}

/// Where the picker is at.
#[derive(Debug)]
enum Stage {
    /// Narrowing the lists by what is typed.
    Search,
    /// Naming the session called `session` again, `input` typed so far.
    Rename { session: String, input: String },
    /// Waiting on zellij, or on the plan, to delete what `target` stands
    /// for: going ahead with the plan once it is `agreed`, what you said yes
    /// to, or else asking first, if it asks.
    Planning { target: Key, agreed: Option<Plan> },
    /// Asking whether to go ahead with `plan`, as `asks`.
    Asking { target: Key, plan: Plan, asks: String },
}

/// What a preview shows of a terminal's screen.
enum Peek {
    /// zellij is asked for it.
    Reading,
    /// The terminal `pane`'s screen, and the lines of it that show anything:
    /// from `first` to before `end`.
    Ready { pane: u32, screen: Box<Term<VoidListener>>, first: usize, end: usize },
    /// Why zellij did not dump it.
    Failed(String),
}

impl fmt::Debug for Peek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Peek::Reading => f.write_str("Reading"),
            Peek::Ready { pane, first, end, .. } => {
                write!(f, "Ready {{ pane: {pane}, lines: {first}..{end} }}")
            },
            Peek::Failed(why) => write!(f, "Failed({why:?})"),
        }
    }
}

impl Peek {
    /// The screen zellij dumped of `pane`, `dumped`.
    fn of(pane: u32, dumped: &str) -> Peek {
        let lines: Vec<&str> = dumped.lines().collect();
        let lines = &lines[lines.len().saturating_sub(PEEK_LINES_MAX)..];
        // A line's bytes are at least as many as the columns it takes.
        let widest = lines.iter().map(|line| line.len()).max().unwrap_or(0);
        let columns = widest.clamp(1, PEEK_COLUMNS_MAX);
        let size = TermSize::new(columns, lines.len().max(1));
        let mut screen = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, lines.join("\r\n").as_bytes());
        let mut used = vec![false; screen.screen_lines()];
        for indexed in screen.renderable_content().display_iter {
            let line = usize::try_from(indexed.point.line.0).ok();
            if let Some(used) = line.and_then(|line| used.get_mut(line)) {
                *used |= !indexed.cell.is_empty();
            }
        }
        let first = used.iter().position(|used| *used).unwrap_or(0);
        let end = used.iter().rposition(|used| *used).map_or(first, |last| last + 1);
        Peek::Ready { pane, screen: Box::new(screen), first, end }
    }
}

/// What a preview shows: the terminal `pane` of `session`, or, of none, the
/// one a client coming there would be on.
type Looked = (String, Option<u32>);

/// The picker.
#[derive(Debug)]
pub struct Picker {
    /// Its number, which what zellij answers it carries.
    number: u64,
    list: List,
    /// What is typed in the search.
    term: String,
    stage: Stage,
    listing: Listing,
    /// The sessions zellij keeps to bring back, once it says, or why it did
    /// not list them.
    exited: Option<Result<Vec<Exited>, String>>,
    /// When `exited` was listed, which its ages count back from.
    exited_at: SystemTime,
    sessions: Rows,
    agents: Rows,
    peeks: HashMap<Looked, Peek>,
    /// What the preview shows now, and since when.
    wanted: Option<(Looked, Instant)>,
    /// What went wrong last, until the next key.
    error: Option<String>,
    /// What the picker has yet to ask of zellij, which `asks` hands over.
    asking: Vec<Ask>,
}

impl Picker {
    /// The picker numbered `number`, open on `list`, of `listing`, which asks
    /// zellij for the sessions it keeps to bring back.
    pub fn open(number: u64, list: List, listing: Listing) -> Picker {
        let mut picker = Picker {
            number,
            list,
            term: String::new(),
            stage: Stage::Search,
            listing,
            exited: None,
            exited_at: SystemTime::now(),
            sessions: Rows::default(),
            agents: Rows::default(),
            peeks: HashMap::new(),
            wanted: None,
            error: None,
            asking: vec![Ask::Exited],
        };
        picker.rebuild(true);
        picker
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    /// What the picker has yet to ask of zellij, for luneta to ask it.
    pub fn asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asking)
    }

    /// Takes in what the sidebar knows of the host now. The sessions zellij
    /// keeps to bring back are asked for again once a session ran that runs
    /// no more, as zellij keeps one killed.
    pub fn listed(&mut self, listing: Listing) {
        let names = |listing: &Listing| -> HashSet<String> {
            listing.sessions.iter().map(|live| live.place.session().to_owned()).collect()
        };
        let ended = !names(&self.listing).is_subset(&names(&listing));
        self.listing = listing;
        if ended {
            self.asking.push(Ask::Exited);
        }
        self.rebuild(false);
    }

    /// Takes in what zellij answered, and says what that leaves luneta to do.
    pub fn heard(&mut self, heard: Heard) -> Picked {
        match heard {
            Heard::Exited(exited) => {
                self.exited = Some(exited);
                self.exited_at = SystemTime::now();
                self.rebuild(false);
            },
            Heard::Peek { session, asked, peek } => {
                let peek = match peek {
                    Ok((pane, dumped)) => Peek::of(pane, &dumped),
                    Err(why) => Peek::Failed(why),
                };
                self.peeks.insert((session, asked), peek);
            },
            Heard::Closes { place, closes } => {
                let planning = matches!(
                    &self.stage,
                    Stage::Planning { target: Key::Agent(at), .. } if *at == place
                );
                if !planning {
                    return Picked::Nothing;
                }
                let Some(pane) = place.pane() else {
                    return Picked::Nothing;
                };
                return match closes {
                    Ok(closes) => Picked::Plan {
                        session: place.session().to_owned(),
                        deletes: Deletes::Pane { pane, closes },
                    },
                    Err(why) => {
                        self.stage = Stage::Search;
                        self.error = Some(format!("nothing closed: {why}"));
                        Picked::Nothing
                    },
                };
            },
            Heard::Renamed { from, to, renamed } => {
                if let Err(why) = renamed {
                    self.error = Some(format!("{from} is not called {to}: {why}"));
                }
            },
            Heard::Deleted { session, deleted } => {
                if let Err(why) = deleted {
                    self.error = Some(format!("{session} stays: {why}"));
                }
                self.asking.push(Ask::Exited);
            },
        }
        Picked::Nothing
    }

    /// What `key` does, and what it leaves luneta to do.
    pub fn key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        if control && key.code == KeyCode::Char('c') {
            return Picked::Close;
        }
        self.error = None;
        let picked = match std::mem::replace(&mut self.stage, Stage::Search) {
            Stage::Search => self.search_key(key),
            Stage::Rename { session, input } => self.rename_key(key, session, input),
            // Only `Esc` does anything while zellij is asked, or the plan:
            // it gives up the delete.
            Stage::Planning { target, agreed } => {
                if key.code != KeyCode::Esc {
                    self.stage = Stage::Planning { target, agreed };
                }
                Picked::Nothing
            },
            // `y` goes ahead, once the plan is the same again, and any other
            // key does not.
            Stage::Asking { target, plan, .. } => {
                let yes = key.code == KeyCode::Char('y') && plain(key);
                if yes { self.plan(target, Some(plan)) } else { Picked::Nothing }
            },
        };
        self.follow();
        picked
    }

    /// Types `text` where you type: in the search, or the new name.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match &mut self.stage {
            Stage::Search => {
                self.term.push_str(&text);
                self.rebuild(true);
            },
            Stage::Rename { input, .. } => input.push_str(&text),
            Stage::Planning { .. } | Stage::Asking { .. } => {},
        }
    }

    /// Moves the highlight `by` rows, as the wheel does.
    pub fn scroll(&mut self, by: isize) {
        if matches!(self.stage, Stage::Search) {
            self.rows_mut().step(by);
            self.follow();
        }
    }

    /// What the delete waited on does, as `plan` says: asks first, or goes
    /// ahead, or says why it does nothing. Going ahead after you said yes
    /// does what you were asked about; once that changed while you read, it
    /// asks again.
    pub fn planned(&mut self, plan: Result<Plan, String>) -> Picked {
        let Stage::Planning { target, agreed } = std::mem::replace(&mut self.stage, Stage::Search)
        else {
            return Picked::Nothing;
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(why) => {
                self.error = Some(why);
                return Picked::Nothing;
            },
        };
        let asks = if agreed.as_ref() == Some(&plan) { None } else { plan.question() };
        if let Some(asks) = asks {
            self.stage = Stage::Asking { target, plan, asks };
            return Picked::Nothing;
        }
        Picked::Delete(plan.act)
    }

    /// When the preview asks zellij for the screen it shows, if it is to ask.
    pub fn due(&self) -> Option<Instant> {
        let (looked, since) = self.wanted.as_ref()?;
        (!self.peeks.contains_key(looked)).then(|| *since + PREVIEW_DELAY)
    }

    /// Asks zellij for the screen the preview shows, once it is `due`.
    pub fn ticked(&mut self, now: Instant) {
        if self.due().is_none_or(|due| now < due) {
            return;
        }
        let Some((looked, _)) = self.wanted.clone() else {
            return;
        };
        if self.peeks.len() >= PEEKS_MAX {
            self.peeks.clear();
        }
        self.peeks.insert(looked.clone(), Peek::Reading);
        let (session, asked) = looked;
        self.asking.push(Ask::Peek { session, asked });
    }

    /// The box over zellij's side, `over`, the picker is drawn in: the
    /// middle three fifths of it each way, as the plugin's floated over a
    /// tab.
    pub fn area(over: Rect) -> Rect {
        Rect::new(
            over.x + over.width / 5,
            over.y + over.height / 5,
            over.width * 3 / 5,
            over.height * 3 / 5,
        )
    }

    /// Draws the picker over `over`, zellij's side, at `now`.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) {
        let area = Picker::area(over);
        let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2))
        else {
            return;
        };
        framed(buffer, area, TITLE, "", Vec::new());
        let layout = Layout::of(Rect::new(area.x + 1, area.y + 1, width, height));
        if let Stage::Rename { session, input } = &self.stage {
            self.draw_rename(buffer, &layout, session, input);
            return;
        }
        if let Some(results) = layout.results {
            let inner = usize::from(results.width.saturating_sub(2));
            let rows = usize::from(results.height.saturating_sub(2));
            let notes = self.notes(inner);
            let body = self.body(inner, rows.saturating_sub(notes.len()), now);
            let count = count(self.rows().at, self.rows().rows.len());
            let lines = anchored(notes, body, rows);
            framed(buffer, results, TITLE, &count, lines);
        }
        if let Some(preview) = layout.preview {
            self.draw_preview(buffer, preview, now);
        }
        let prompt = self.prompt(usize::from(layout.input.width.saturating_sub(2)));
        draw_input(buffer, &layout, self.list.title(), prompt);
        self.draw_help(buffer, layout.help);
    }

    /// The rows of the list shown.
    fn rows(&self) -> &Rows {
        match self.list {
            List::Sessions => &self.sessions,
            List::Agents => &self.agents,
        }
    }

    fn rows_mut(&mut self) -> &mut Rows {
        match self.list {
            List::Sessions => &mut self.sessions,
            List::Agents => &mut self.agents,
        }
    }

    /// What a key does to the search, and what it leaves luneta to do.
    fn search_key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        match key.code {
            KeyCode::Esc => Picked::Close,
            KeyCode::Tab | KeyCode::BackTab => {
                self.list = self.list.other();
                Picked::Nothing
            },
            KeyCode::Char('j') if control => {
                self.list = List::Sessions;
                Picked::Nothing
            },
            KeyCode::Char('a') if control => {
                self.list = List::Agents;
                Picked::Nothing
            },
            KeyCode::Char('r') if control => {
                self.rename();
                Picked::Nothing
            },
            KeyCode::Enter => self.enter(),
            KeyCode::Delete => self.delete(),
            KeyCode::Down => {
                self.rows_mut().step(1);
                Picked::Nothing
            },
            KeyCode::Up => {
                self.rows_mut().step(-1);
                Picked::Nothing
            },
            KeyCode::Backspace => {
                if self.term.pop().is_some() {
                    self.rebuild(true);
                }
                Picked::Nothing
            },
            KeyCode::Char(c) if plain(key) => {
                self.term.push(c);
                self.rebuild(true);
                Picked::Nothing
            },
            _ => Picked::Nothing,
        }
    }

    /// What `Enter` does: takes zellij to the row highlighted, or makes the
    /// session typed, where no session is highlighted. Where no agent is, it
    /// does nothing: what is typed there names none.
    fn enter(&mut self) -> Picked {
        let Some(row) = self.rows().selected() else {
            return match self.new_name() {
                Ok(Some(name)) if self.list == List::Sessions => Picked::Start(name),
                Ok(_) | Err(_) => Picked::Nothing,
            };
        };
        match &row.key {
            Key::Live(place) if self.is_here(place) => Picked::Close,
            Key::Live(place) | Key::Agent(place) => Picked::Go(place.clone()),
            Key::Exited(name) => Picked::Start(name.clone()),
        }
    }

    /// The session typed, which `Enter` makes: none of nothing typed, and
    /// none of the name of the session luneta's zellij is in.
    fn new_name(&self) -> Result<Option<SessionName>, &'static str> {
        if self.term.is_empty() {
            return Ok(None);
        }
        if self.listing.here.as_deref() == Some(self.term.as_str()) {
            return Err("you are in it");
        }
        SessionName::parse(&self.term).map(Some)
    }

    /// What `Del` does: plans killing the session highlighted, or closing the
    /// pane of the agent highlighted, or lets go of the session highlighted
    /// that ended. What is typed stays, and the highlight goes on to the row
    /// after once the row goes: `Del` again deletes the row it shows, not
    /// one off the top of the list.
    fn delete(&mut self) -> Picked {
        let Some(key) = self.rows().selected().map(|row| row.key.clone()) else {
            return Picked::Nothing;
        };
        match key {
            Key::Exited(session) => {
                self.forget_exited(&session);
                self.asking.push(Ask::Delete(session));
                self.rebuild(false);
                Picked::Nothing
            },
            target => self.plan(target, None),
        }
    }

    /// Plans deleting what `target` stands for, going ahead once the plan is
    /// `agreed`: of a session, at once; of a pane, once zellij says what
    /// closing it closes.
    fn plan(&mut self, target: Key, agreed: Option<Plan>) -> Picked {
        let picked = match &target {
            Key::Live(place) => Picked::Plan {
                session: place.session().to_owned(),
                deletes: Deletes::Session,
            },
            Key::Agent(place) => {
                self.asking.push(Ask::Closes(place.clone()));
                Picked::Nothing
            },
            Key::Exited(_) => return Picked::Nothing,
        };
        self.stage = Stage::Planning { target, agreed };
        picked
    }

    /// Takes a session that ended off the list, once zellij is asked to let
    /// go of it.
    fn forget_exited(&mut self, session: &SessionName) {
        if let Some(Ok(exited)) = &mut self.exited {
            exited.retain(|exited| exited.name != *session);
        }
    }

    /// Starts naming the session highlighted again, which must run: zellij
    /// renames none that ended.
    fn rename(&mut self) {
        if self.list != List::Sessions {
            return;
        }
        match self.sessions.selected().map(|row| &row.key) {
            Some(Key::Live(place)) => {
                let session = place.session().to_owned();
                self.stage = Stage::Rename { session, input: String::new() };
            },
            Some(Key::Exited(name)) => {
                self.error = Some(format!(
                    "{name} ended, and zellij renames only a session that runs: Enter brings \
                     it back"
                ));
            },
            Some(Key::Agent(_)) | None => {},
        }
    }

    /// What a key does to the new name of `session`, `input` typed so far.
    fn rename_key(
        &mut self,
        key: &KeyEvent,
        session: String,
        mut input: String,
    ) -> Picked {
        match key.code {
            KeyCode::Esc => return Picked::Nothing,
            KeyCode::Enter => {
                let Ok(to) = self.rename_to(&session, &input) else {
                    self.stage = Stage::Rename { session, input };
                    return Picked::Nothing;
                };
                self.asking.push(Ask::Rename { from: session, to });
                return Picked::Nothing;
            },
            KeyCode::Backspace => {
                input.pop();
            },
            KeyCode::Char(c) if plain(key) => input.push(c),
            _ => {},
        }
        self.stage = Stage::Rename { session, input };
        Picked::Nothing
    }

    /// The name `session` takes, of what was typed, or why it takes none.
    fn rename_to(&self, session: &str, name: &str) -> Result<SessionName, &'static str> {
        let name = SessionName::parse(name)?;
        if name.as_str() == session {
            return Err("already called that");
        }
        let live = self.listing.sessions.iter().map(|live| live.place.session());
        let exited = self.exited_list().iter().map(|exited| exited.name.as_str());
        if live.chain(exited).any(|taken| taken == name.as_str()) {
            return Err("a session by that name already exists");
        }
        Ok(name)
    }

    /// The sessions zellij keeps to bring back, as far as it said.
    fn exited_list(&self) -> &[Exited] {
        match &self.exited {
            Some(Ok(exited)) => exited,
            Some(Err(_)) | None => &[],
        }
    }

    /// Builds both lists again, of what is typed: the highlight on the row
    /// it was on, or the one that took its place where that went; or else,
    /// `snap`, as what is typed changed, on the best match, or the first
    /// session after yours.
    fn rebuild(&mut self, snap: bool) {
        let finder = (!self.term.is_empty()).then(|| Finder::new(&self.term));
        let here = self.listing.here.clone();
        let mut sessions = Vec::new();
        for live in &self.listing.sessions {
            let name = live.place.session();
            let rank = u8::from(here.as_deref() != Some(name));
            sessions.push((rank, name.to_owned(), Key::Live(live.place.clone())));
        }
        let mut exited: Vec<&Exited> = self.exited_list().iter().collect();
        exited.sort_by_key(|exited| exited.ago);
        for exited in exited {
            sessions.push((2, exited.name.to_string(), Key::Exited(exited.name.clone())));
        }
        // Yours first, then those that run, then those that ended, each
        // newest first, as they are listed.
        sessions.sort_by_key(|(rank, ..)| *rank);
        let sessions = sessions.into_iter().map(|(_, name, key)| (name, Vec::new(), key));
        let sessions = matched(finder.as_ref(), sessions);
        let yours = self.listing.here_pane.as_ref();
        let agents = self.listing.agents.iter().filter(|seated| Some(&seated.place) != yours);
        // An agent is found by its session's name, and by its directory.
        let agents: Vec<(String, Vec<Option<&str>>, Key)> = agents
            .map(|seated| {
                let also = vec![Some(seated.place.session()), Some(seated.dir.as_str())];
                (self.agent_name(seated), also, Key::Agent(seated.place.clone()))
            })
            .collect();
        let agents = matched(finder.as_ref(), agents.into_iter());
        let start = |rows: &[Row]| {
            let first = rows.first().map(|row| &row.key);
            let yours = matches!(first, Some(Key::Live(place)) if self.is_here(place));
            usize::from(yours && self.term.is_empty())
        };
        let (session_start, agent_start) = (start(&sessions), start(&agents));
        let held = |rows: &Rows| rows.selected().map(|row| row.key.clone()).filter(|_| !snap);
        let (held_session, held_agent) = (held(&self.sessions), held(&self.agents));
        self.sessions.replace(sessions, held_session.as_ref(), session_start);
        self.agents.replace(agents, held_agent.as_ref(), agent_start);
        self.follow();
    }

    /// The name an agent's row shows: its label, and where another agent
    /// shows it too, its pane, and its session where the two sit in two, as
    /// `luneta/main:4`, or `luneta/main (review:4)`.
    fn agent_name(&self, seated: &Seated) -> String {
        let alike = self.listing.agents.iter().filter(|other| other.label == seated.label);
        let sessions: HashSet<&str> = alike.clone().map(|other| other.place.session()).collect();
        let pane = seated.place.pane().unwrap_or_default();
        match (alike.count(), sessions.len()) {
            (0 | 1, _) => seated.label.clone(),
            (_, 1) => format!("{}:{pane}", seated.label),
            _ => format!("{} ({}:{pane})", seated.label, seated.place.session()),
        }
    }

    /// Points the preview at the row highlighted now.
    fn follow(&mut self) {
        let looked = match self.rows().selected().map(|row| &row.key) {
            Some(Key::Live(place)) => {
                let live = self.listing.sessions.iter().find(|live| live.place == *place);
                Some((place.session().to_owned(), live.and_then(|live| live.on)))
            },
            Some(Key::Agent(place)) => Some((place.session().to_owned(), place.pane())),
            Some(Key::Exited(_)) | None => None,
        };
        match (looked, &self.wanted) {
            (Some(looked), Some((wanted, _))) if looked == *wanted => {},
            (Some(looked), _) => self.wanted = Some((looked, Instant::now())),
            (None, _) => self.wanted = None,
        }
    }

    /// What goes over the list, `width` wide: what went wrong, what the
    /// picker asks, and why it cannot tell.
    fn notes(&self, width: usize) -> Vec<Line<'static>> {
        let error = Style::new().fg(ERROR);
        let mut notes = Vec::new();
        if let Stage::Asking { asks, .. } = &self.stage {
            let asking = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
            notes.extend(menu::wrap(asks, width, asking));
        }
        if let Some(why) = &self.error {
            notes.extend(menu::wrap(why, width, error));
        }
        match self.list {
            List::Sessions => {
                if let Some(Err(why)) = &self.exited {
                    notes.extend(menu::wrap(&format!("no ended sessions: {why}"), width, error));
                }
            },
            List::Agents => {
                if let Some(why) = &self.listing.away {
                    let why = format!("lunetad does not answer: {why}");
                    notes.extend(menu::wrap(&why, width, error));
                }
            },
        }
        notes
    }

    /// The lines of the list shown, `width` wide, `room` of them at most: the
    /// highlight's row among them.
    fn body(&self, width: usize, room: usize, now: SystemTime) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut room = room;
        if self.list == List::Agents
            && let Some(yours) = self.yours()
        {
            if room == 0 {
                return lines;
            }
            lines.push(here_line(&self.agent_name(yours), width));
            room -= 1;
        }
        if room == 0 {
            return lines;
        }
        let rows = self.rows();
        if rows.rows.is_empty() {
            lines.extend(menu::wrap(&self.empty(), width, Style::new().fg(gruvbox::GREY)));
            return lines;
        }
        // The rule over the sessions that ended takes a line of its own.
        let exited_at = rows.rows.iter().position(|row| matches!(row.key, Key::Exited(_)));
        let line_of = |row: usize| row + usize::from(exited_at.is_some_and(|at| row >= at));
        let total = rows.rows.len() + usize::from(exited_at.is_some());
        let (start, end) = viewport(rows.at.map_or(0, line_of), total, room);
        let frame = sidebar::frame(now);
        for line in start..end {
            let row = match exited_at {
                Some(at) if line == at => {
                    lines.push(separator("ended", width));
                    continue;
                },
                Some(at) if line > at => line - 1,
                _ => line,
            };
            let lit = rows.at == Some(row);
            let line = self.row_line(&rows.rows[row], width, frame, now);
            lines.push(if lit { line.patch_style(Style::new().bg(LIT)) } else { line });
        }
        lines
    }

    /// Whether `place` is of the session luneta's zellij is in.
    fn is_here(&self, place: &Place) -> bool {
        self.listing.here.as_deref() == Some(place.session())
    }

    /// The agent in the pane luneta's zellij is on, if one is.
    fn yours(&self) -> Option<&Seated> {
        let here = self.listing.here_pane.as_ref()?;
        self.listing.agents.iter().find(|seated| seated.place == *here)
    }

    /// The line of `row`, `width` wide, at `now`, the spinner at `frame`.
    fn row_line(&self, row: &Row, width: usize, frame: usize, now: SystemTime) -> Line<'static> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (lead, name_style, right) = match &row.key {
            Key::Live(place) => {
                let live = self.listing.sessions.iter().find(|live| live.place == *place);
                if self.is_here(place) {
                    let style = Style::new().fg(NAME).add_modifier(Modifier::BOLD);
                    let rule = Span::styled("here", Style::new().fg(ACCENT));
                    (None, style, vec![rule])
                } else {
                    let mut right = Vec::new();
                    if let Some(live) = live.filter(|live| live.waiting > 0) {
                        let (dot, style) = Mark::Waiting.glyph(frame);
                        right.push(Span::styled(format!("{dot}{} ", live.waiting), style));
                    }
                    if let Some(live) = live {
                        let age = format!("{} ago", sidebar::age(live.started, now));
                        right.push(Span::styled(age, Style::new().fg(gruvbox::GREY)));
                    }
                    (None, bold, right)
                }
            },
            Key::Exited(name) => {
                let exited = self.exited_list().iter().find(|exited| exited.name == *name);
                let made = exited.and_then(|exited| self.exited_at.checked_sub(exited.ago));
                let age = made.map(|made| format!("{} ago", sidebar::age(made, now)));
                let grey = Style::new().fg(gruvbox::GREY);
                (None, dim, age.into_iter().map(|age| Span::styled(age, grey)).collect())
            },
            Key::Agent(place) => {
                let seated = self.listing.agents.iter().find(|seated| seated.place == *place);
                let Some(seated) = seated else {
                    return Line::from(Span::raw(" ".repeat(width)));
                };
                let (glyph, style) = seated.mark.glyph(frame);
                let name_style = if seated.mark == Mark::Waiting {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                let age = seated.since.map(|since| sidebar::age(since, now)).unwrap_or_default();
                let mut right = Vec::new();
                // The directory goes first, where the row is short of room.
                if width >= row.name.width() + seated.dir.width() + age.width() + 10 {
                    right.push(Span::styled(seated.dir.clone(), Style::new().fg(gruvbox::GREY)));
                    right.push(Span::raw("  "));
                }
                right.push(Span::styled(age, dim));
                (Some(Span::styled(glyph, style)), name_style, right)
            },
        };
        let right_width: usize = right.iter().map(|span| span.content.width()).sum();
        let lead_width = lead.as_ref().map_or(0, |lead| lead.content.width() + 1);
        // A space either side, and a space between the name and the right.
        let room = width.saturating_sub(2 + lead_width + right_width + 1);
        let name = sidebar::cut(&row.name, room);
        let mut spans = vec![Span::raw(" ")];
        if let Some(lead) = lead {
            spans.push(lead);
            spans.push(Span::raw(" "));
        }
        spans.extend(hit_spans(&name, &row.hits, name_style));
        let gap = width.saturating_sub(1 + lead_width + name.width() + right_width + 1);
        if matches!(&row.key, Key::Live(place) if self.is_here(place)) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("─".repeat(gap.saturating_sub(2)), Style::new().fg(ACCENT)));
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.extend(right);
        spans.push(Span::raw(" "));
        Line::from(spans)
    }

    /// What the list says while it has no rows.
    fn empty(&self) -> String {
        if !self.term.is_empty() {
            return format!("no match for \"{}\"", self.term);
        }
        match self.list {
            List::Sessions if self.exited.is_none() => "asking zellij…".to_owned(),
            List::Sessions => "no sessions".to_owned(),
            List::Agents if self.listing.away.is_some() => "no agents".to_owned(),
            List::Agents if self.yours().is_some() => "no other agents".to_owned(),
            List::Agents => "no agents running".to_owned(),
        }
    }

    /// Draws the preview of the row highlighted into `area`.
    fn draw_preview(&self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        let inner = usize::from(area.width.saturating_sub(2));
        let grey = Style::new().fg(gruvbox::GREY);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Some(row) = self.rows().selected() else {
            let lines = vec![
                Line::styled(" nothing highlighted", grey),
                Line::default(),
                Line::styled(" Enter takes what you type", grey),
            ];
            framed(buffer, area, "Preview", "", lines);
            return;
        };
        let (right, caption, looked) = match &row.key {
            Key::Exited(_) => {
                let mut lines = vec![Line::styled(" ended", grey), Line::default()];
                let said = "zellij kept how it was laid out to bring it back from, and nothing \
                            runs in it to look at";
                lines.extend(menu::wrap(said, inner, grey));
                framed(buffer, area, &row.name, "", lines);
                return;
            },
            Key::Live(place) => {
                let live = self.listing.sessions.iter().find(|live| live.place == *place);
                let panes = live.map_or(0, |live| live.panes.len());
                let right = if panes == 1 { "1 pane".to_owned() } else { format!("{panes} panes") };
                let looked = (place.session().to_owned(), live.and_then(|live| live.on));
                (right, None, looked)
            },
            Key::Agent(place) => {
                let seated = self.listing.agents.iter().find(|seated| seated.place == *place);
                let caption = seated.map(|seated| {
                    let status = if seated.mark == Mark::Waiting {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(gruvbox::FG1)
                    };
                    let age = seated.since.map(|since| sidebar::age(since, now));
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(seated.status.clone(), status),
                        Span::styled(" · ", dim),
                        Span::styled(age.unwrap_or_default(), dim),
                        Span::styled(" · ", dim),
                        Span::styled(seated.dir.clone(), grey),
                    ])
                });
                (String::new(), caption, (place.session().to_owned(), place.pane()))
            },
        };
        let peek = self.peeks.get(&looked);
        // A session's caption names the pane it shows, once zellij says
        // which.
        let caption = caption.or_else(|| {
            let pane = match peek {
                Some(Peek::Ready { pane, .. }) => Some(*pane),
                _ => looked.1,
            }?;
            let live = self.listing.sessions.iter().find(|live| live.place.session() == looked.0)?;
            let shown = live.panes.iter().find(|shown| shown.id == pane)?;
            Some(Line::from(vec![
                Span::raw(" "),
                Span::styled(shown.tab.clone(), grey),
                Span::styled(" · ", dim),
                Span::styled(shown.label.clone(), dim),
            ]))
        });
        let mut lines = vec![caption.unwrap_or_default(), Line::default()];
        match peek {
            None | Some(Peek::Reading) => lines.push(Line::styled(" reading…", grey)),
            Some(Peek::Failed(why)) => {
                lines.extend(menu::wrap(why, inner, Style::new().fg(ERROR)));
            },
            Some(Peek::Ready { first, end, .. }) if first == end => {
                lines.push(Line::styled(" nothing on this screen", grey));
            },
            Some(Peek::Ready { .. }) => {},
        }
        framed(buffer, area, &row.name, &right, lines);
        if let Some(Peek::Ready { screen, first, end, .. }) = peek
            && first != end
        {
            // Under the caption, a column in from the box's edge, the last
            // lines of the screen that fit.
            let height = area.height.saturating_sub(2 + 2);
            let shown = Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), height);
            let top = end.saturating_sub(usize::from(height)).max(*first);
            hosted::show(screen, buffer, shown, top);
        }
    }

    /// The search, `width` wide: what is typed, and what `Enter` does, on
    /// the right, where it has the room.
    fn prompt(&self, width: usize) -> Line<'static> {
        let action = match self.rows().selected().map(|row| &row.key) {
            Some(Key::Live(place)) if self.is_here(place) => Ok("you are in it".to_owned()),
            Some(Key::Live(_)) => Ok("Attach".to_owned()),
            Some(Key::Exited(_)) => Ok("Bring back".to_owned()),
            Some(Key::Agent(_)) => Ok("Go to agent".to_owned()),
            None if self.list == List::Agents => Ok(String::new()),
            None => match self.new_name() {
                Ok(Some(name)) => Ok(format!("Make \"{name}\"")),
                Ok(None) => Ok(String::new()),
                Err(why) => Err(why.to_owned()),
            },
        };
        typed(&self.term, action, width)
    }

    /// Draws the keys the help line names into `help`, if it has the room.
    fn draw_help(&self, buffer: &mut Buffer, help: Option<Rect>) {
        if let Some(help) = help {
            let keys = menu::keys_line(usize::from(help.width), &self.keys());
            buffer.set_line(help.x, help.y, &keys, help.width);
        }
    }

    /// The keys the help line names.
    fn keys(&self) -> Vec<(&'static str, &'static str)> {
        match (&self.stage, self.list) {
            (Stage::Asking { .. }, _) => vec![("<y>", "Yes"), ("<any other key>", "No")],
            (Stage::Planning { .. }, _) => vec![("<ESC>", "Give up")],
            (Stage::Rename { .. }, _) => vec![("<ENTER>", "Rename"), ("<ESC>", "Cancel")],
            (Stage::Search, List::Sessions) => vec![
                ("<↓↑>", "Move"),
                ("<ENTER>", "Go"),
                ("<TAB>", "Agents"),
                ("<Ctrl r>", "Rename"),
                ("<Del>", "Kill"),
                ("<ESC>", "Close"),
            ],
            (Stage::Search, List::Agents) => vec![
                ("<↓↑>", "Move"),
                ("<ENTER>", "Go"),
                ("<TAB>", "Sessions"),
                ("<Del>", "Close its pane"),
                ("<ESC>", "Close"),
            ],
        }
    }

    /// Draws the box the new name of `session` is typed in, `input` so far.
    fn draw_rename(&self, buffer: &mut Buffer, layout: &Layout, session: &str, input: &str) {
        if let Some(full) = layout.full {
            let inner = usize::from(full.width.saturating_sub(2));
            let mut lines = vec![Line::default()];
            let what = if self.listing.here.as_deref() == Some(session) {
                format!("renaming \"{session}\", the session you are in")
            } else {
                format!("renaming \"{session}\"")
            };
            lines.extend(menu::wrap(&what, inner, Style::new().fg(gruvbox::GREY)));
            framed(buffer, full, "Rename", "", lines);
        }
        let width = usize::from(layout.input.width.saturating_sub(2));
        let action = self.rename_to(session, input).map(|_| "Rename".to_owned());
        draw_input(buffer, layout, "Rename", typed(input, action.map_err(str::to_owned), width));
        self.draw_help(buffer, layout.help);
    }
}

/// Whether `key` types what it says: with no modifier, or only `Shift`.
fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

/// The rows of `candidates`, each a name, what else a search finds it by,
/// and what it stands for, that `finder` finds: all of them, in their order,
/// for no search; or else as the sidebar's search weighs them, the best
/// first, and those that fit alike in their order.
fn matched<'a>(
    finder: Option<&Finder<'_>>,
    candidates: impl Iterator<Item = (String, Vec<Option<&'a str>>, Key)>,
) -> Vec<Row> {
    let Some(finder) = finder else {
        return candidates.map(|(name, _, key)| Row { key, name, hits: Vec::new() }).collect();
    };
    let mut found: Vec<(Fit, Row)> = Vec::new();
    for (name, also, key) in candidates {
        if let Some((fit, hits)) = finder.find(&name, &also) {
            found.push((fit, Row { key, name, hits }));
        }
    }
    // A stable sort, which keeps those that fit alike in their order.
    found.sort_by_key(|(fit, _)| Reverse(*fit));
    found.into_iter().map(|(_, row)| row).collect()
}

/// `name` in `style`, the characters at `hits` in the accent, as a search
/// found them.
fn hit_spans(name: &str, hits: &[usize], style: Style) -> Vec<Span<'static>> {
    let hit = style.fg(ACCENT).add_modifier(Modifier::BOLD);
    name.chars()
        .enumerate()
        .map(|(at, c)| Span::styled(c.to_string(), if hits.contains(&at) { hit } else { style }))
        .collect()
}

/// The line of what is in the pane luneta's zellij is on, pinned over the
/// list, `width` wide.
fn here_line(name: &str, width: usize) -> Line<'static> {
    let name = sidebar::cut(name, width.saturating_sub(8));
    let rule = width.saturating_sub(1 + name.width() + 1 + 1 + 4 + 1);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(name, Style::new().fg(NAME).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().fg(ACCENT)),
        Span::raw(" "),
        Span::styled("here", Style::new().fg(ACCENT)),
        Span::raw(" "),
    ])
}

/// The rule over the rows of the sessions that ended, `width` wide.
fn separator(label: &str, width: usize) -> Line<'static> {
    let rule = width.saturating_sub(1 + label.width() + 1 + 1);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(label.to_owned(), Style::new().fg(gruvbox::GREY)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().add_modifier(Modifier::DIM)),
    ])
}

/// Which of `total` lines show in `visible`, the line `selected` among them:
/// from the first, until it goes past the last that shows.
fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = (selected + 1).saturating_sub(visible);
    (start, (start + visible).min(total))
}

/// `notes` and `body` at the bottom of `rows` lines, blank ones over them,
/// as the list grows up from the search.
fn anchored(
    notes: Vec<Line<'static>>,
    body: Vec<Line<'static>>,
    rows: usize,
) -> Vec<Line<'static>> {
    let notes: Vec<Line<'static>> = notes.into_iter().take(rows).collect();
    let body: Vec<Line<'static>> = body.into_iter().take(rows - notes.len()).collect();
    let blank = notes.len() + body.len()..rows;
    let mut lines: Vec<Line<'static>> = blank.map(|_| Line::default()).collect();
    lines.extend(notes);
    lines.extend(body);
    lines
}

/// Which row of how many the highlight is on, as `3/7`.
fn count(at: Option<usize>, total: usize) -> String {
    match at {
        Some(at) if total > 0 => format!("{}/{total}", at + 1),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

/// What is typed, `width` wide, after a `/`, and what `Enter` does with it
/// on the right, or why it does nothing, where it has the room.
fn typed(input: &str, action: Result<String, String>, width: usize) -> Line<'static> {
    let mut spans = sidebar::query(input, width).spans;
    let used: usize = spans.iter().map(|span| span.content.width()).sum();
    let (action, style) = match action {
        Ok(action) if !action.is_empty() => (format!("<ENTER> {action}"), Style::new().fg(NAME)),
        Ok(_) => (String::new(), Style::new()),
        Err(why) => (why, Style::new().fg(ERROR)),
    };
    let room = width.saturating_sub(used + 2);
    if !action.is_empty() && room >= MIN_ACTION {
        let action = sidebar::cut(&action, room);
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + action.width() + 1))));
        spans.push(Span::styled(action, style));
    }
    Line::from(spans)
}

/// Draws `lines` in a box at `area`, under a rule that names it `title`, and
/// says `right` at its right: its sides run down every line it holds, as
/// many as `lines` or not.
fn framed(buffer: &mut Buffer, area: Rect, title: &str, right: &str, mut lines: Vec<Line<'_>>) {
    let inner = usize::from(area.width.saturating_sub(2));
    lines.resize(usize::from(area.height.saturating_sub(2)), Line::default());
    menu::boxed(buffer, area, &menu::rule(title, right, inner), &lines);
}

/// Draws the search's box, titled `title`, with `prompt` in it.
fn draw_input(buffer: &mut Buffer, layout: &Layout, title: &str, prompt: Line<'static>) {
    let input = layout.input;
    if layout.bordered {
        framed(buffer, input, title, "", vec![prompt]);
    } else {
        buffer.set_line(input.x, input.y, &prompt, input.width);
    }
}

/// Where the picker's parts go in the box, as the plugin's went in its pane:
/// the search over the help line at the bottom, and the list over it, with
/// the preview at its right where both have the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    results: Option<Rect>,
    preview: Option<Rect>,
    /// The list and the preview, together.
    full: Option<Rect>,
    input: Rect,
    /// Whether the search has its box, as it does where it has the rows.
    bordered: bool,
    help: Option<Rect>,
}

impl Layout {
    fn of(area: Rect) -> Layout {
        let help = (area.height > INPUT_HEIGHT)
            .then(|| Rect::new(area.x, area.bottom() - 1, area.width, 1));
        let bordered = area.height >= INPUT_HEIGHT;
        let height = if bordered { INPUT_HEIGHT } else { area.height.min(1) };
        let input_y = area.bottom().saturating_sub(u16::from(help.is_some()) + height);
        let input = Rect::new(area.x, input_y, area.width, height);
        let above = input_y - area.y;
        let full = (above >= MIN_RESULTS).then(|| Rect::new(area.x, area.y, area.width, above));
        let split = full.filter(|full| full.width >= MIN_HALF * 2).map(|full| {
            let left = full.width / 2;
            let right = Rect { x: full.x + left, width: full.width - left, ..full };
            (Rect { width: left, ..full }, right)
        });
        let (results, preview) = match split {
            Some((results, preview)) => (Some(results), Some(preview)),
            None => (full, None),
        };
        Layout { results, preview, full, input, bordered, help }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::close::Doing;
    use crate::sidebar::Listed;

    const NOW: Duration = Duration::from_secs(1000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    /// A session that started `started` seconds after the epoch.
    fn listed(name: &str, started: u64) -> Listed {
        Listed::new(name, UNIX_EPOCH + Duration::from_secs(started))
    }

    /// A session that runs, with one pane, and the client on it, if `on`.
    fn live(name: &str, started: u64, on: Option<u32>) -> Live {
        Live {
            place: Place::Session(listed(name, started)),
            started: UNIX_EPOCH + Duration::from_secs(started),
            panes: vec![Shown { id: 0, tab: "Tab #1".to_owned(), label: format!("{name}/w") }],
            on,
            waiting: 0,
        }
    }

    /// An agent in the pane `pane` of the session `name`, which started at
    /// `started`.
    fn seated(name: &str, started: u64, pane: u32, label: &str, mark: Mark) -> Seated {
        Seated {
            place: Place::Pane { session: listed(name, started), pane },
            label: label.to_owned(),
            mark,
            status: "idle".to_owned(),
            since: Some(now() - Duration::from_mins(1)),
            dir: "luneta/main".to_owned(),
        }
    }

    fn ended(name: &str, ago: u64) -> Exited {
        Exited { name: SessionName::running(name).unwrap(), ago: Duration::from_secs(ago) }
    }

    /// Three sessions that run, `b` the one luneta's zellij is in, `c` the
    /// newest, and two that ended, `y` later than `x`.
    fn three() -> Picker {
        let listing = Listing {
            here: Some("b".to_owned()),
            sessions: vec![live("c", 30, None), live("b", 20, Some(1)), live("a", 10, None)],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Sessions, listing);
        picker.heard(Heard::Exited(Ok(vec![ended("x", 100), ended("y", 10)])));
        picker
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Picked {
        picker.key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn control(picker: &mut Picker, c: char) -> Picked {
        picker.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            press(picker, KeyCode::Char(c));
        }
    }

    /// The picker drawn over a zellij side of 200 by 50.
    fn drawn(picker: &Picker) -> Buffer {
        let over = Rect::new(0, 0, 200, 50);
        let mut buffer = Buffer::empty(over);
        picker.draw(&mut buffer, over, now());
        buffer
    }

    fn text(buffer: &Buffer, area: Rect) -> Vec<String> {
        let line = |y| (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        (area.y..area.bottom()).map(|y| line(y).trim().to_owned()).collect()
    }

    /// Where the list is drawn, and the preview, over that zellij side.
    fn parts() -> (Rect, Rect) {
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        let inner = |rect: Rect| Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
        let layout = Layout::of(inner(area));
        (inner(layout.results.unwrap()), inner(layout.preview.unwrap()))
    }

    /// The lines of the list, top to bottom, and the one highlighted.
    fn list(picker: &Picker) -> (Vec<String>, Option<String>) {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lines = text(&buffer, results);
        let lit = (results.y..results.bottom()).position(|y| buffer[(results.x, y)].bg == LIT);
        let lit = lit.map(|at| lines[at].clone());
        (lines.into_iter().filter(|line| !line.is_empty()).collect(), lit)
    }

    /// The lines of the list, top to bottom, each with what of it is in the
    /// accent, as a search lights what it found.
    fn found(picker: &Picker) -> Vec<(String, String)> {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lit = |y| -> String {
            let cells = (results.x..results.right()).map(|x| &buffer[(x, y)]);
            cells.filter(|cell| cell.fg == ACCENT).map(ratatui::buffer::Cell::symbol).collect()
        };
        let lines = (results.y..results.bottom()).zip(text(&buffer, results));
        lines.map(|(y, line)| (line, lit(y))).filter(|(line, _)| !line.is_empty()).collect()
    }

    /// The first word of each line of the list.
    fn names(picker: &Picker) -> Vec<String> {
        let (lines, _) = list(picker);
        lines.iter().map(|line| line.split(' ').next().unwrap_or("").to_owned()).collect()
    }

    fn lit(picker: &Picker) -> String {
        let (_, lit) = list(picker);
        lit.unwrap_or_default().split(' ').next().unwrap_or("").to_owned()
    }

    /// The picker asks zellij for the sessions it keeps to bring back as it
    /// opens, and lists the one luneta's zellij is in first, then those that
    /// run, then those that ended under a rule, each newest first, the
    /// highlight on the newest after yours. `Enter` takes zellij to a session
    /// that runs, brings back one that ended, and on yours closes.
    #[test]
    fn the_sessions_go_yours_first_then_those_that_run_then_those_that_ended() {
        let listing = Listing { here: Some("b".to_owned()), ..Listing::default() };
        assert_eq!(Picker::open(1, List::Sessions, listing).asks(), vec![Ask::Exited]);
        let mut picker = three();
        assert_eq!(names(&picker), ["b", "c", "a", "ended", "y", "x"]);
        assert_eq!(lit(&picker), "c");
        let c = Place::Session(listed("c", 30));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Go(c));
        press(&mut picker, KeyCode::Up);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        for _ in 0..3 {
            press(&mut picker, KeyCode::Down);
        }
        assert_eq!(lit(&picker), "y");
        let y = SessionName::running("y").unwrap();
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Start(y));
    }

    /// What is typed narrows the list, a name that is what is typed first,
    /// and `Enter` makes a session of a name no row has, but of one zellij
    /// refuses, or of yours, or on the agents.
    #[test]
    fn what_is_typed_narrows_the_list_and_enter_makes_a_session_no_row_has() {
        let listing = Listing {
            here: Some("elo".to_owned()),
            sessions: vec![
                live("luneta-main", 30, None),
                live("luneta", 20, None),
                live("elo", 10, None),
            ],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Sessions, listing);
        typed(&mut picker, "luneta");
        assert_eq!(names(&picker), ["luneta", "luneta-main"]);
        typed(&mut picker, "-new");
        assert_eq!(list(&picker).0, [r#"no match for "luneta-new""#]);
        let new = SessionName::parse("luneta-new").unwrap();
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Start(new));
        press(&mut picker, KeyCode::Tab);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        press(&mut picker, KeyCode::Tab);
        for (term, why) in [("a/b", "name cannot contain '/'"), ("elo", "you are in it")] {
            for _ in 0..20 {
                press(&mut picker, KeyCode::Backspace);
            }
            typed(&mut picker, term);
            if term == "elo" {
                // Yours is listed: Enter on it closes, as it would.
                assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
                continue;
            }
            assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
            let buffer = drawn(&picker);
            assert!(text(&buffer, buffer.area).iter().any(|line| line.contains(why)), "{term}");
        }
    }

    /// `Del` on a session plans killing it, and asks first, as the menu
    /// does, while the plan ends an agent at work: `y` plans again, and goes
    /// ahead with the plan it asked about, but asks again of one that changed
    /// meanwhile. Any other key gives it up.
    #[test]
    fn del_asks_before_ending_an_agent_at_work_and_goes_ahead_on_yes_to_the_same_plan() {
        let mut picker = three();
        let planning = Picked::Plan { session: "c".to_owned(), deletes: Deletes::Session };
        let plan = |doing: Doing| Plan::of(Act::Kill("c".to_owned()), Ok(&[(0, doing)]));
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Busy))), Picked::Nothing);
        let asks = |picker: &Picker| {
            let buffer = drawn(picker);
            text(&buffer, buffer.area).iter().any(|line| line.contains("Kill session c?"))
        };
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('n')), Picked::Nothing);
        assert!(!asks(&picker));

        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        picker.planned(Ok(plan(Doing::Busy)));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), Picked::Nothing);
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        let killed = Picked::Delete(Act::Kill("c".to_owned()));
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), killed);

        // An idle agent ends without a question.
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Idle))), killed);
    }

    /// `Del` on an agent asks zellij first what closing its pane closes, and
    /// plans by what it answers: only for that pane, while it waits on it.
    #[test]
    fn del_on_an_agent_asks_zellij_what_closing_its_pane_closes_first() {
        let place = Place::Pane { session: listed("c", 30), pane: 4 };
        let listing = Listing {
            agents: vec![seated("c", 30, 4, "fold", Mark::Idle)],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing);
        picker.asks();
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), vec![Ask::Closes(place.clone())]);
        let other = Place::Pane { session: listed("c", 30), pane: 5 };
        let heard = Heard::Closes { place: other, closes: Ok(Closes::Pane) };
        assert_eq!(picker.heard(heard), Picked::Nothing);
        let heard = Heard::Closes { place, closes: Ok(Closes::Pane) };
        let deletes = Deletes::Pane { pane: 4, closes: Closes::Pane };
        assert_eq!(picker.heard(heard), Picked::Plan { session: "c".to_owned(), deletes });
    }

    /// `Del` on a session that ended lets go of it at once, as nothing runs
    /// in it to end, and asks zellij again for those it keeps once it did.
    /// The highlight goes on to the row after, and what is typed stays, so
    /// that `Del` again deletes no row off the top of the list.
    #[test]
    fn del_lets_go_of_a_session_that_ended_at_once() {
        let mut picker = three();
        picker.asks();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "y");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), vec![Ask::Delete(SessionName::running("y").unwrap())]);
        assert!(!names(&picker).contains(&"y".to_owned()));
        assert_eq!(lit(&picker), "x");
        let session = SessionName::running("y").unwrap();
        picker.heard(Heard::Deleted { session, deleted: Err("zellij: no".to_owned()) });
        assert_eq!(picker.asks(), vec![Ask::Exited]);
        let (lines, _) = list(&picker);
        assert!(lines.iter().any(|line| line.contains("y stays: zellij: no")), "{lines:?}");

        typed(&mut picker, "x");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), vec![Ask::Delete(SessionName::running("x").unwrap())]);
        assert_eq!(list(&picker).0, [r#"no match for "x""#]);
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert!(picker.asks().is_empty());
    }

    /// `Ctrl r` renames the session highlighted, yours or another, to a name
    /// no session has, those that ended among them. zellij renames none that
    /// ended.
    #[test]
    fn ctrl_r_renames_the_highlighted_session_to_a_name_no_session_has() {
        let mut picker = three();
        picker.asks();
        let renaming = |picker: &Picker| {
            let buffer = drawn(picker);
            text(&buffer, buffer.area).iter().any(|line| line.contains(r#"renaming "c""#))
        };
        control(&mut picker, 'r');
        assert!(renaming(&picker));
        for taken in ["a", "x", "c"] {
            typed(&mut picker, taken);
            press(&mut picker, KeyCode::Enter);
            assert!(picker.asks().is_empty(), "{taken}");
            press(&mut picker, KeyCode::Backspace);
        }
        typed(&mut picker, "d");
        press(&mut picker, KeyCode::Enter);
        let to = SessionName::parse("d").unwrap();
        assert_eq!(picker.asks(), vec![Ask::Rename { from: "c".to_owned(), to }]);
        assert!(!renaming(&picker));

        typed(&mut picker, "x");
        control(&mut picker, 'r');
        let buffer = drawn(&picker);
        let said = text(&buffer, buffer.area).join(" ");
        assert!(!said.contains("renaming"));
        assert!(said.contains("zellij renames only a session that runs"), "{said}");
    }

    /// Agents named alike are told apart by their panes, and by their
    /// sessions too where they sit in two; the one in the pane luneta's
    /// zellij is on is pinned over the list, and is no row; and a search
    /// finds an agent by its session's name, lit where its row shows it.
    #[test]
    fn agents_named_alike_are_told_apart_and_yours_is_pinned_over_them() {
        let listing = Listing {
            here: Some("work".to_owned()),
            here_pane: Some(Place::Pane { session: listed("work", 20), pane: 9 }),
            agents: vec![
                seated("work", 20, 9, "mine", Mark::Busy),
                seated("work", 20, 1, "fix", Mark::Waiting),
                seated("work", 20, 2, "fix", Mark::Idle),
                seated("work", 20, 3, "docs", Mark::Idle),
                seated("review", 10, 4, "docs", Mark::Idle),
            ],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing);
        let (lines, lit) = list(&picker);
        assert!(lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"), "{lines:?}");
        // Past each row's mark and the space after it, up to the gap before
        // where it works.
        let name = |line: &String| {
            let rest: String = line.chars().skip(2).collect();
            rest.split("  ").next().unwrap_or("").to_owned()
        };
        let named: Vec<String> = lines[1..].iter().map(name).collect();
        assert_eq!(named, ["fix:1", "fix:2", "docs (work:3)", "docs (review:4)"]);
        assert!(lit.is_some_and(|lit| lit.contains("fix:1")));
        typed(&mut picker, "review");
        let rows = found(&picker);
        assert_eq!(rows.len(), 2, "{rows:?}");
        // `review` fits the session's name better than the row only by
        // starting it, and lights up where the row shows it.
        let (line, lit) = &rows[1];
        assert!(line.contains("docs (review:4)") && lit == "review", "{rows:?}");
    }

    /// A search finds an agent by its directory as its row and its preview
    /// show it, and weighs it by what fits best, its name or another: its
    /// name lights up only where it fits as well. `zellij` lands on `zellij
    /// panics` whole, on `src/zellij` after its `/`, and on `Zero-length lines
    /// in jobs` letter by letter.
    #[test]
    fn a_search_weighs_an_agent_by_its_name_or_its_directory_whichever_fits_best() {
        let at = |pane, label, dir: &str| Seated {
            dir: dir.to_owned(),
            ..seated("work", 20, pane, label, Mark::Idle)
        };
        let listing = Listing {
            agents: vec![
                at(1, "Zero-length lines in jobs", "src/zellij"),
                at(2, "fix", "src/zellij"),
                at(3, "zellij panics", "misc/luneta"),
                at(4, "docs", "luneta/main"),
            ],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing);
        typed(&mut picker, "zellij");
        let rows = found(&picker);
        let shown = [("zellij panics", "zellij"), ("Zero-length lines in jobs", ""), ("fix", "")];
        assert_eq!(rows.len(), shown.len(), "{rows:?}");
        for ((line, lit), (label, lights)) in rows.iter().zip(shown) {
            assert!(line.contains(label) && lit == lights, "{rows:?}");
        }
    }

    /// The preview asks zellij for the screen it shows only once the
    /// highlight has stayed on its row a while: of the pane a client of the
    /// session is on, or else of the one zellij says a client coming there
    /// would be on. It asks once for each.
    #[test]
    fn the_preview_asks_zellij_once_the_highlight_rests() {
        let mut picker = three();
        picker.asks();
        let due = picker.due().unwrap();
        picker.ticked(due.checked_sub(Duration::from_millis(1)).unwrap());
        assert!(picker.asks().is_empty());
        picker.ticked(due);
        assert_eq!(picker.asks(), vec![Ask::Peek { session: "c".to_owned(), asked: None }]);
        assert_eq!(picker.due(), None);
        press(&mut picker, KeyCode::Up);
        let due = picker.due().unwrap();
        picker.ticked(due);
        assert_eq!(picker.asks(), vec![Ask::Peek { session: "b".to_owned(), asked: Some(1) }]);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.due(), None);
    }

    /// The picker floats in the middle three fifths of zellij's side, the
    /// list at the left and the preview at the right, which shows the last
    /// lines of what zellij dumped of the pane, inside its box.
    #[test]
    fn the_preview_shows_the_last_lines_zellij_dumped_inside_its_box() {
        let mut picker = three();
        let dumped = (1..=40).map(|n| format!("\x1b[mline {n}")).collect::<Vec<_>>().join("\n");
        let peek = Ok((0, format!("{dumped}\n\n\n")));
        picker.heard(Heard::Peek { session: "c".to_owned(), asked: None, peek });
        let buffer = drawn(&picker);
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        assert_eq!((area.x, area.y, area.width, area.height), (40, 10, 120, 30));
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(buffer[(area.right() - 1, area.bottom() - 1)].symbol(), "┘");
        let (_, preview) = parts();
        let lines = text(&buffer, preview);
        assert_eq!(lines[0], "Tab #1 · c/w");
        assert_eq!(lines.last().map(String::as_str), Some("line 40"));
        assert_eq!(lines[2], format!("line {}", 40 - (lines.len() - 3)));
        for y in preview.y..preview.bottom() {
            assert_eq!(buffer[(preview.x - 1, y)].symbol(), "│");
            assert_eq!(buffer[(preview.right(), y)].symbol(), "│");
        }
    }
}
