use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lune_diff::SelectionExcerpt;
use lune_domain::projects::Dir;
use luned::asks::{self, Asked, Marked, Step, Turn};
use luned::transcripts::SessionId;
use luned::{Destination, Directory};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::brief::{self, Asking, BoxRoom};
use crate::cadence::Cadence;
use crate::copied::Excerpt;
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint, ProjectTarget, Target, ViewRow};
use crate::moonlight;
use crate::reach::{self, Luned};
use crate::sidebar::{self, ACCENT};
use crate::spots::Spots;
use crate::sweep::{self, Continuation, Join, NonEmptyRect, Page, Sweep};

const TITLE: &str = "Ask";

const STOPPED: &str = "stopped: the agent was ended where it was";

const RAN_PREFIX_CELLS: usize = 4;

const TALK_MAX: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Mouse {
    Ignored,
    Started,
    Active,
    Copied(Excerpt),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    Send,
    Close,
    Interrupt,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    Step(Step),
    Ended(Result<(), String>),
}

#[derive(Debug, PartialEq, Eq)]
enum Said {
    You(String),
    Ran {
        id: String,
        command: String,
        denied: bool,
    },
    Agent(String),
    Wrong(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Works {
    Here(PathBuf),
    There {
        on: Destination,
        cwd: Option<Directory>,
    },
}

#[derive(Debug, Default)]
struct Talk {
    entries: VecDeque<Said>,
    version: u64,
}

impl Talk {
    fn push(&mut self, said: Said) {
        if self.entries.len() == TALK_MAX {
            self.entries.pop_front();
        }
        self.entries.push_back(said);
        self.version += 1;
    }

    fn mark_denied(&mut self, id: &str) {
        let mut changed = false;
        for said in &mut self.entries {
            if let Said::Ran {
                id: ran, denied, ..
            } = said
                && ran == id
                && !*denied
            {
                *denied = true;
                changed = true;
            }
        }
        if changed {
            self.version += 1;
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn iter(&self) -> std::collections::vec_deque::Iter<'_, Said> {
        self.entries.iter()
    }

    const fn version(&self) -> u64 {
        self.version
    }
}

#[cfg(test)]
impl Extend<Said> for Talk {
    fn extend<T: IntoIterator<Item = Said>>(&mut self, iter: T) {
        for said in iter {
            self.push(said);
        }
    }
}

#[cfg(test)]
impl<const N: usize> PartialEq<[Said; N]> for Talk {
    fn eq(&self, other: &[Said; N]) -> bool {
        self.entries == *other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BodyVersion {
    talk: u64,
    turning: bool,
}

#[derive(Debug, Default)]
enum Selecting {
    #[default]
    Still,
    Pressed {
        from: Position,
        snapshot: Box<Snapshot>,
    },
    Sweeping {
        sweep: Sweep,
        snapshot: Box<Snapshot>,
    },
    Swept {
        sweep: Sweep,
        snapshot: Box<Snapshot>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: NonEmptyRect,
    first_line: usize,
    first: Line<'static>,
    rest: Vec<Line<'static>>,
    version: BodyVersion,
}

impl Snapshot {
    fn of_range(first_y: u16, last_y: u16, shown: &ShownBody) -> Snapshot {
        let text = shown.text();
        let area = text.rect();
        assert!(
            first_y >= area.y && last_y < area.bottom() && first_y <= last_y,
            "snapshot rows fall inside the drawn text"
        );
        let first_line = usize::from(first_y - area.y);
        let last_line = usize::from(last_y - area.y);
        let slice = &shown.page.lines()[first_line..=last_line];
        let (first, tail) = slice
            .split_first()
            .expect("an inclusive row range covers at least one line");
        Snapshot {
            text,
            first_line,
            first: first.clone(),
            rest: tail.to_vec(),
            version: shown.version(),
        }
    }

    fn of_row(y: u16, shown: &ShownBody) -> Snapshot {
        Snapshot::of_range(y, y, shown)
    }

    fn of(sweep: Sweep, shown: &ShownBody) -> Snapshot {
        let (first, last) = sweep.ends();
        Snapshot::of_range(first.y, last.y, shown)
    }

    fn keeps(&self, shown: &ShownBody) -> bool {
        if self.version != shown.version() {
            return false;
        }
        if self.text != shown.text() {
            return false;
        }
        let end = self.first_line + 1 + self.rest.len();
        let Some(lines) = shown.page.lines().get(self.first_line..end) else {
            return false;
        };
        let Some((first, rest)) = lines.split_first() else {
            return false;
        };
        first == &self.first && rest == self.rest.as_slice()
    }
}

#[derive(Debug)]
struct ShownBody {
    page: Page,
    version: BodyVersion,
}

impl ShownBody {
    fn new(page: Page, version: BodyVersion) -> Self {
        assert!(
            page.area().x() >= 1,
            "a shown ask body leaves room for its one-cell margin"
        );
        assert_eq!(
            page.lines().len(),
            usize::from(page.area().height().get()),
            "each shown row has a page line"
        );
        Self { page, version }
    }

    const fn text(&self) -> NonEmptyRect {
        self.page.area()
    }

    const fn version(&self) -> BodyVersion {
        self.version
    }

    fn hit(&self) -> Rect {
        let text = self.page.area();
        Rect::new(
            text.x() - 1,
            text.y(),
            text.width().get() + 1,
            text.height().get(),
        )
    }
}

#[derive(Debug, Default)]
enum DrawnAsk {
    #[default]
    Unshown,
    Shown {
        body: ShownBody,
        selecting: Selecting,
    },
}

struct RowCut {
    column: usize,
    took: String,
}

struct BodyRow {
    line: Line<'static>,
    join: Option<Join>,
    cut: Option<RowCut>,
    spinner: bool,
}

struct Body {
    rows: Vec<BodyRow>,
}

impl Body {
    fn lines(&self) -> impl Iterator<Item = &Line<'static>> {
        self.rows.iter().map(|row| &row.line)
    }

    fn truncate(&mut self, rows_max: usize) -> bool {
        if self.rows.len() > rows_max {
            let kept = rows_max.saturating_sub(1);
            let drained = self.rows.len() - kept;
            self.rows.drain(..drained);
            if rows_max > 0 {
                if let Some(first_kept) = self.rows.first_mut() {
                    first_kept.join = None;
                }
                self.rows.insert(
                    0,
                    BodyRow {
                        line: Line::styled(" …", Style::new().add_modifier(Modifier::DIM)),
                        join: None,
                        cut: None,
                        spinner: false,
                    },
                );
            }
        }
        assert!(
            self.rows.len() <= rows_max,
            "truncated body fits its row budget"
        );
        assert!(
            self.rows.first().is_none_or(|row| row.join.is_none()),
            "the first visible body row never joins upward"
        );
        self.rows.iter().any(|row| row.spinner)
    }

    fn into_page(self, text: NonEmptyRect) -> Page {
        assert_eq!(
            self.rows.len(),
            usize::from(text.height().get()),
            "each body row has a line"
        );
        let mut lines = Vec::with_capacity(self.rows.len());
        let mut joins = BTreeMap::new();
        let mut cuts = Spots::default();
        for (line_index, row) in self.rows.into_iter().enumerate() {
            lines.push(unpad(row.line));
            if let Some(join) = row.join {
                joins.insert(line_index, Continuation { join, indent: 0 });
            }
            if let Some(cut) = row.cut {
                cuts.add(line_index, cut.column..cut.column + 1, cut.took);
            }
        }
        Page::new(text, lines, joins, cuts)
    }
}

#[derive(Debug)]
pub struct Ask {
    number: u64,
    works: Works,
    highlighted: String,
    context: Option<String>,
    review_only: bool,
    session: Option<SessionId>,
    turned: Option<u64>,
    talk: Talk,
    typed: String,
    turn: Option<Asking>,
    drawn: DrawnAsk,
}

impl Ask {
    pub fn new(number: u64, target: &Target, dir: &Path) -> Self {
        let works = match (target, target.machine()) {
            (_, Machine::Local) => {
                let cwd = target.cwd().map(|cwd| PathBuf::from(cwd.as_str()));
                Works::Here(cwd.unwrap_or_else(|| dir.to_owned()))
            }
            (_, Machine::Remote(on)) => Works::There {
                on: on.clone(),
                cwd: target.cwd().cloned(),
            },
        };
        Ask {
            number,
            works,
            highlighted: highlighted(target),
            context: None,
            review_only: false,
            session: None,
            turned: None,
            talk: Talk::default(),
            typed: String::new(),
            turn: None,
            drawn: DrawnAsk::Unshown,
        }
    }

    pub fn homed(number: u64, on: &Machine, dir: &Dir, project: &str) -> Self {
        let works = match on {
            Machine::Local => Works::Here(PathBuf::from(dir.as_str())),
            Machine::Remote(on) => Works::There {
                on: on.clone(),
                cwd: Some(Directory::from(dir)),
            },
        };
        let dir = dir.as_str();
        let highlighted = format!(
            "The user asks from the work view of project {project}, in {dir}, which lists the \
             project's open issues and pull requests on GitHub: \"this project\" means it, and \
             you run in its folder."
        );
        Ask {
            number,
            works,
            highlighted,
            context: None,
            review_only: false,
            session: None,
            turned: None,
            talk: Talk::default(),
            typed: String::new(),
            turn: None,
            drawn: DrawnAsk::Unshown,
        }
    }

    pub fn reviewed(
        number: u64,
        on: &Machine,
        dir: &Dir,
        excerpt: &SelectionExcerpt,
        initial: Option<&str>,
    ) -> Self {
        let works = match on {
            Machine::Local => Works::Here(PathBuf::from(dir.as_str())),
            Machine::Remote(on) => Works::There {
                on: on.clone(),
                cwd: Some(Directory::from(dir)),
            },
        };
        let range = match excerpt.lines {
            Some(lines) if lines.start() == lines.end() => {
                format!("{}:{}", excerpt.path, lines.start())
            }
            Some(lines) => format!("{}:{}-{}", excerpt.path, lines.start(), lines.end()),
            None => excerpt.path.as_str().to_owned(),
        };
        let safe_range = plain(&range);
        let safe_dir = plain(dir.as_str());
        let highlighted = format!("The user is reviewing changes at {safe_range} in {safe_dir}.");
        let snippet = excerpt.snippet.replace('\0', "");
        let context = if snippet.is_empty() {
            None
        } else {
            let fence = backtick_fence(&snippet);
            Some(format!(
                "<untrusted-diff-excerpt location=\"{safe_range}\">\n{fence}patch\n{snippet}\n{fence}\n</untrusted-diff-excerpt>"
            ))
        };
        Ask {
            number,
            works,
            highlighted,
            context,
            review_only: true,
            session: None,
            turned: None,
            talk: Talk::default(),
            typed: initial.unwrap_or("").to_owned(),
            turn: None,
            drawn: DrawnAsk::Unshown,
        }
    }

    pub fn rehome(&mut self, on: &Machine, dir: &Dir, project: &str) {
        if self.session.is_some() {
            return;
        }
        if self.turn.is_some() {
            return;
        }
        let Ask {
            works, highlighted, ..
        } = Ask::homed(self.number, on, dir, project);
        self.works = works;
        self.highlighted = highlighted;
        self.context = None;
        self.review_only = false;
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn key(&mut self, key: &KeyEvent) -> Keyed {
        self.clear_selection();
        let bare = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let control = key.modifiers == KeyModifiers::CONTROL;
        match key.code {
            KeyCode::Esc if bare => return Keyed::Close,
            KeyCode::Char('c') if control => return Keyed::Interrupt,
            _ if self.turn.is_some() => return Keyed::Nothing,
            KeyCode::Enter if bare && !self.typed.trim().is_empty() => return Keyed::Send,
            KeyCode::Backspace if bare => {
                self.typed.pop();
            }
            KeyCode::Char('u') if control => self.typed.clear(),
            KeyCode::Char(c) if bare => self.typed.push(c),
            _ => {}
        }
        Keyed::Nothing
    }

    pub fn paste(&mut self, text: &str) {
        self.clear_selection();
        if self.turn.is_none() {
            self.typed.push_str(&plain(text));
        }
    }

    pub fn dictated(&mut self, text: &str) {
        self.clear_selection();
        if self.turn.is_some() {
            return;
        }
        let text = plain(text);
        if !self.typed.is_empty() && !self.typed.ends_with(' ') && !text.is_empty() {
            self.typed.push(' ');
        }
        self.typed.push_str(&text);
    }

    pub fn send(&mut self, marked: Marked, events: &Sender<Event>) {
        if self.turn.is_some() {
            return;
        }
        let words = std::mem::take(&mut self.typed).trim().to_owned();
        if words.is_empty() {
            return;
        }
        let turn = Turn::after(self.turned);
        let (on, cwd) = match &self.works {
            Works::Here(cwd) => (Machine::Local, Some(cwd.clone())),
            Works::There { on, cwd } => {
                let cwd = cwd.as_ref().map(|cwd| PathBuf::from(cwd.as_str()));
                (Machine::Remote(on.clone()), cwd)
            }
        };
        let (resumed, highlighted) = (self.session.clone(), self.highlighted.clone());
        let sent_words = match self.context.as_deref() {
            Some(context) => format!("{context}\n\n{words}"),
            None => words.clone(),
        };
        let review_only = self.review_only;
        let asked = Asked::new(cwd, &sent_words, resumed, highlighted, turn).map(|asked| {
            let asked = asked.marking(marked);
            let asked = if review_only {
                asked.review_only()
            } else {
                asked
            };
            (on, asked)
        });
        self.talk.push(Said::You(words));
        let (on, asked) = match asked {
            Ok(asked) => asked,
            Err(why) => return self.talk.push(Said::Wrong(format!("ask: {why}"))),
        };
        self.context = None;
        self.turned = Some(turn.key());
        let (number, events) = (self.number, events.clone());
        let due = Due::new(&events, move |ended: Result<Result<(), String>, String>| {
            let ended = ended
                .and_then(|ended| ended)
                .map_err(|why| format!("ask: {why}"));
            Event::Heard {
                number,
                heard: Heard::Ended(ended),
            }
        });
        self.turn = Some(Asking::on_thread(move |wanted| {
            let hear = |step| {
                let _ = events.send(Event::Heard {
                    number,
                    heard: Heard::Step(step),
                });
            };
            let ended = match Luned::on(&on, reach::ANSWER) {
                Ok(mut luned) => luned.talk(&asked, wanted, &hear),
                Err(why) => Some(Err(why)),
            };
            if let Some(ended) = ended {
                due.pay(Ok(ended));
            }
        }));
    }

    pub fn heard(&mut self, heard: Heard) {
        match heard {
            Heard::Step(Step::Session(session)) => self.session = Some(session),
            Heard::Step(Step::Ran { id, command }) => {
                self.talk.push(Said::Ran {
                    id,
                    command,
                    denied: false,
                });
            }
            Heard::Step(Step::Denied(id)) => self.talk.mark_denied(&id),
            Heard::Step(Step::Said(said)) => self.talk.push(Said::Agent(said)),
            Heard::Ended(Ok(())) => self.turn = None,
            Heard::Ended(Err(why)) => {
                self.talk.push(Said::Wrong(why));
                self.turn = None;
            }
        }
    }

    pub fn turning(&self) -> bool {
        self.turn.is_some()
    }

    pub fn stop(&mut self, number: u64) {
        assert!(
            number > self.number,
            "a stopped talk is numbered past its turn's"
        );
        self.clear_selection();
        if self.turn.take().is_some() {
            self.talk.push(Said::Wrong(STOPPED.to_owned()));
            self.turned = None;
        }
        self.number = number;
    }

    const fn body_version(&self) -> BodyVersion {
        BodyVersion {
            talk: self.talk.version(),
            turning: self.turn.is_some(),
        }
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Mouse {
        let at = Position::new(mouse.column, mouse.row);
        let DrawnAsk::Shown { body, selecting } = &mut self.drawn else {
            return Mouse::Ignored;
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if body.hit().contains(at) {
                    let from = body.text().clamp(at);
                    let snapshot = Box::new(Snapshot::of_row(from.y, body));
                    *selecting = Selecting::Pressed { from, snapshot };
                    Mouse::Started
                } else {
                    *selecting = Selecting::Still;
                    Mouse::Ignored
                }
            }
            MouseEventKind::Down(_) => {
                *selecting = Selecting::Still;
                Mouse::Ignored
            }
            MouseEventKind::Drag(MouseButton::Left) => match selecting {
                Selecting::Pressed { from, .. } => {
                    let from = *from;
                    let to = body.text().clamp(at);
                    if to != from {
                        let sweep = Sweep::new(from, to, body.text());
                        let snapshot = Box::new(Snapshot::of(sweep, body));
                        *selecting = Selecting::Sweeping { sweep, snapshot };
                    }
                    Mouse::Active
                }
                Selecting::Sweeping { sweep, snapshot } => {
                    sweep.reach(at, body.text());
                    **snapshot = Snapshot::of(*sweep, body);
                    Mouse::Active
                }
                Selecting::Still | Selecting::Swept { .. } => Mouse::Ignored,
            },
            MouseEventKind::Up(MouseButton::Left) => {
                let sweep = match std::mem::take(selecting) {
                    Selecting::Pressed { from, .. } => {
                        let to = body.text().clamp(at);
                        (to != from).then(|| Sweep::new(from, to, body.text()))
                    }
                    Selecting::Sweeping { mut sweep, .. } => {
                        sweep.reach(at, body.text());
                        let (first, last) = sweep.ends();
                        (first != last).then_some(sweep)
                    }
                    kept @ Selecting::Swept { .. } => {
                        *selecting = kept;
                        return Mouse::Ignored;
                    }
                    Selecting::Still => return Mouse::Ignored,
                };
                let Some(sweep) = sweep else {
                    return Mouse::Active;
                };
                let Some(excerpt) = Excerpt::of(sweep.text(&body.page)) else {
                    return Mouse::Active;
                };
                let snapshot = Box::new(Snapshot::of(sweep, body));
                *selecting = Selecting::Swept { sweep, snapshot };
                Mouse::Copied(excerpt)
            }
            MouseEventKind::Up(_) | MouseEventKind::Moved => match selecting {
                Selecting::Pressed { .. } | Selecting::Sweeping { .. } => {
                    *selecting = Selecting::Still;
                    Mouse::Active
                }
                Selecting::Still | Selecting::Swept { .. } => Mouse::Ignored,
            },
            _ => Mouse::Ignored,
        }
    }

    pub fn pressed(&self) -> bool {
        matches!(
            self.drawn,
            DrawnAsk::Shown {
                selecting: Selecting::Pressed { .. } | Selecting::Sweeping { .. },
                ..
            }
        )
    }

    pub fn unsweep(&mut self) {
        if let DrawnAsk::Shown {
            selecting: selecting @ Selecting::Swept { .. },
            ..
        } = &mut self.drawn
        {
            *selecting = Selecting::Still;
        }
    }

    pub fn let_go(&mut self) -> bool {
        let was = self.pressed();
        if let DrawnAsk::Shown { selecting, .. } = &mut self.drawn
            && was
        {
            *selecting = Selecting::Still;
        }
        was
    }

    pub fn clear_selection(&mut self) {
        if let DrawnAsk::Shown { selecting, .. } = &mut self.drawn {
            *selecting = Selecting::Still;
        }
    }

    pub fn unshown(&mut self) {
        self.drawn = DrawnAsk::Unshown;
    }

    pub fn draw(&mut self, buffer: &mut Buffer, over: Rect, wall: SystemTime) -> Cadence {
        let mut cadence = Cadence::default();
        let Some(box_room) = BoxRoom::of(over) else {
            self.unshown();
            return cadence;
        };
        let (inner, room) = (box_room.inner(), box_room.room());
        let keys: &[(Hint, &str)] = match self.turn {
            Some(_) => &[(Hint::Esc, "Close")],
            None => &[
                (Hint::Enter, "Send"),
                (Hint::Alt('r'), "Dictate"),
                (Hint::Esc, "Close"),
            ],
        };
        let footer = [Line::default(), menu::keys_line(inner, keys)];
        let body_rows_max = room.saturating_sub(footer.len());
        let mut body = self.body(inner, body_rows_max, sidebar::frame(wall));
        let kept_spinner = inner > 0 && body.truncate(body_rows_max);
        if room >= 2 {
            assert!(
                body.rows.len() + footer.len() <= room,
                "ask overlay body and footer fit in room"
            );
        }
        let right = match &self.works {
            Works::Here(cwd) => sidebar::short_cwd(&cwd.to_string_lossy()),
            Works::There { on, cwd: Some(cwd) } => {
                format!("{} on {on}", sidebar::short_cwd(cwd.as_str()))
            }
            Works::There { on, cwd: None } => format!("~ on {on}"),
        };
        let box_area = box_room.draw(buffer, TITLE, &right, body.lines().chain(&footer));
        assert!(box_area.width >= 2, "a drawn ask box has border columns");
        assert!(box_area.height >= 2, "a drawn ask box has border rows");
        self.finish_draw(buffer, box_area, body);
        if kept_spinner {
            cadence.spin();
        }
        cadence
    }

    pub fn draw_docked(
        &mut self,
        buffer: &mut Buffer,
        area: Rect,
        most: u16,
        right: &str,
        hints: &[(Hint, &str)],
        wall: SystemTime,
    ) -> (u16, Cadence) {
        assert!(
            most <= area.height,
            "docked ask row budget fits in the home area"
        );
        let mut cadence = Cadence::default();
        let Some(inner) = area
            .width
            .checked_sub(2)
            .map(usize::from)
            .filter(|inner| *inner > 0)
        else {
            self.unshown();
            return (0, cadence);
        };
        let body_rows_max = usize::from(most.saturating_sub(3));
        let mut body = self.body(inner, body_rows_max, sidebar::frame(wall));
        let kept_spinner = body.truncate(body_rows_max);
        let footer = [menu::keys_line(inner, hints)];
        let total_lines = body.rows.len() + footer.len();
        let rows = u16::try_from(total_lines + 2).map_or(most, |rows| rows.min(most));
        assert!(rows <= area.height, "the prompt's box fits in the home");
        let asked = Rect::new(area.x, area.bottom() - rows, area.width, rows);
        let lines: Vec<&Line<'_>> = body.lines().chain(&footer).collect();
        menu::boxed_lines(buffer, asked, &menu::rule(TITLE, right, inner), &lines);
        self.finish_draw(buffer, asked, body);
        if kept_spinner {
            cadence.spin();
        }
        (rows, cadence)
    }

    fn finish_draw(&mut self, buffer: &mut Buffer, box_area: Rect, body: Body) {
        let body_rows = u16::try_from(body.rows.len()).expect("body lines fit in box height");
        let inner = box_area.width.saturating_sub(2);
        let Some(text) = NonEmptyRect::of(Rect::new(
            box_area.x + 2,
            box_area.y + 1,
            inner.saturating_sub(1),
            body_rows,
        )) else {
            self.unshown();
            return;
        };
        assert!(
            body_rows <= box_area.height.saturating_sub(2),
            "body rows fit inside the box borders"
        );
        let shown = ShownBody::new(body.into_page(text), self.body_version());
        let selecting = match std::mem::take(&mut self.drawn) {
            DrawnAsk::Unshown => Selecting::Still,
            DrawnAsk::Shown { selecting, .. } => match selecting {
                Selecting::Still => Selecting::Still,
                Selecting::Pressed { ref snapshot, .. }
                | Selecting::Sweeping { ref snapshot, .. }
                | Selecting::Swept { ref snapshot, .. }
                    if !snapshot.keeps(&shown) =>
                {
                    Selecting::Still
                }
                kept @ (Selecting::Pressed { .. }
                | Selecting::Sweeping { .. }
                | Selecting::Swept { .. }) => kept,
            },
        };
        if let Selecting::Sweeping { sweep, .. } | Selecting::Swept { sweep, .. } = &selecting {
            sweep.draw(buffer, shown.text());
        }
        self.drawn = DrawnAsk::Shown {
            body: shown,
            selecting,
        };
    }

    fn body(&self, inner: usize, rows_max: usize, frame: usize) -> Body {
        if rows_max == 0 {
            return Body { rows: Vec::new() };
        }
        let dim = Style::new().add_modifier(Modifier::DIM);
        let yours = Style::new().fg(ACCENT);
        let tail = if self.turn.is_some() {
            menu::waiting(&format!("Asking {}…", asks::MODEL), inner, dim, frame)
                .into_iter()
                .enumerate()
                .map(|(index, line)| BodyRow {
                    line,
                    join: None,
                    cut: None,
                    spinner: index == 0,
                })
                .collect()
        } else {
            prompt_rows(&self.typed, !self.talk.is_empty(), inner, yours)
        };
        let mut total_rows = tail.len();
        let mut chunks = vec![tail];
        for (at, said) in self.talk.iter().enumerate().rev() {
            if total_rows > rows_max {
                break;
            }
            let chunk = said_rows(at, said, inner, dim, yours);
            total_rows += chunk.len();
            chunks.push(chunk);
        }
        let mut rows = Vec::with_capacity(total_rows);
        for chunk in chunks.into_iter().rev() {
            rows.extend(chunk);
        }
        assert_eq!(rows.len(), total_rows, "assembled rows match chunk total");
        Body { rows }
    }
}

fn prompt_rows(typed: &str, after_talk: bool, inner: usize, style: Style) -> Vec<BodyRow> {
    let mut rows = Vec::new();
    if after_talk {
        rows.push(BodyRow {
            line: Line::default(),
            join: None,
            cut: None,
            spinner: false,
        });
    }
    for (line, join) in menu::wrap_joined(&format!("› {typed}_"), inner, style) {
        rows.push(BodyRow {
            line,
            join,
            cut: None,
            spinner: false,
        });
    }
    let last_row = rows
        .last_mut()
        .expect("wrapped prompt has at least one line");
    let last_grapheme = last_row
        .line
        .styled_graphemes(Style::new())
        .last()
        .expect("wrapped prompt line has at least one grapheme");
    let without_cursor = last_grapheme
        .symbol
        .strip_suffix('_')
        .expect("wrapped prompt ends with the input cursor");
    let width = sweep::line_cells(&last_row.line)
        .checked_sub(1)
        .expect("wrapped prompt line includes its one-cell margin");
    let cursor_cells = sweep::cells(last_grapheme.symbol);
    assert!(
        width >= cursor_cells,
        "prompt line width covers its cursor glyph"
    );
    last_row.cut = Some(RowCut {
        column: width - cursor_cells,
        took: without_cursor.to_owned(),
    });
    rows
}

fn said_rows(at: usize, said: &Said, inner: usize, dim: Style, yours: Style) -> Vec<BodyRow> {
    let mut rows = Vec::new();
    let push_joined = |rows: &mut Vec<BodyRow>, added: Vec<(Line<'static>, Option<Join>)>| {
        for (line, join) in added {
            rows.push(BodyRow {
                line,
                join,
                cut: None,
                spinner: false,
            });
        }
    };
    match said {
        Said::You(words) => {
            if at > 0 {
                rows.push(BodyRow {
                    line: Line::default(),
                    join: None,
                    cut: None,
                    spinner: false,
                });
            }
            push_joined(
                &mut rows,
                menu::wrap_joined(&format!("› {words}"), inner, yours),
            );
        }
        Said::Ran {
            command, denied, ..
        } => {
            let command = plain(command);
            let cut = sidebar::cut_tail(&command, inner.saturating_sub(5));
            let (prefix, style) = if *denied {
                ("  ✗ ", Style::new().fg(moonlight::RED))
            } else {
                ("  $ ", dim)
            };
            assert_eq!(
                sweep::cells(prefix),
                RAN_PREFIX_CELLS,
                "command prefix width matches constant"
            );
            let row_cut = cut.elided.map(|elided| RowCut {
                column: RAN_PREFIX_CELLS + elided.cell_offset,
                took: elided.took,
            });
            rows.push(BodyRow {
                line: Line::styled(format!(" {prefix}{}", cut.shown), style),
                join: None,
                cut: row_cut,
                spinner: false,
            });
        }
        Said::Agent(said) => {
            push_joined(
                &mut rows,
                brief::paragraphs_joined(said, inner, Style::new()),
            );
        }
        Said::Wrong(why) => {
            push_joined(
                &mut rows,
                brief::paragraphs_joined(why, inner, Style::new().fg(moonlight::RED)),
            );
        }
    }
    rows
}

fn unpad(mut line: Line<'static>) -> Line<'static> {
    let Some(first) = line.spans.first_mut() else {
        return line;
    };
    assert!(
        first.content.starts_with(' '),
        "every non-empty ask box line starts with a one-cell margin"
    );
    match &mut first.content {
        Cow::Borrowed(text) => *text = &text[1..],
        Cow::Owned(text) => {
            text.remove(0);
        }
    }
    if first.content.is_empty() {
        line.spans.remove(0);
    }
    line
}

fn backtick_fence(text: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

fn plain(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn highlighted(target: &Target) -> String {
    let row = match target {
        Target::Project(_, ProjectTarget { project, .. }) => {
            format!("project {}", project.listed().name())
        }
        Target::Worktree(_, row) => {
            format!(
                "worktree {} of project {}",
                row.name,
                row.place.project.listed().name()
            )
        }
        Target::View(_, ViewRow { place, .. }) => {
            format!("view {}:{}", place.listed().name(), place.view())
        }
        Target::Machine(_) => "the header of this machine".to_owned(),
    };
    let cwd = target.cwd().map_or_default(|cwd| format!(", in {cwd}"));
    let branch = target
        .branch()
        .map(|branch| format!(", on branch {branch}"));
    let row = format!("{row}{cwd}{}", branch.unwrap_or_default());
    format!(
        "The row highlighted in lune's sidebar as they opened the box is {row}: \"this\" or \
         \"that one\" may mean it."
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::menu::{Cwd, Folder, Ran, ViewRow};
    use crate::sidebar::{Listed, ProjectRow, ViewPlace};

    fn project_on(on: Machine) -> Target {
        let project = ProjectRow::of(Listed::new_project("lune", 3));
        Target::Project(
            on,
            ProjectTarget {
                project,
                dir: "/w/lune".to_owned(),
            },
        )
    }

    fn nixos() -> Machine {
        Machine::Remote("nixos".parse().unwrap())
    }

    fn view_row_on(machine: Machine) -> Target {
        let row = ViewRow {
            place: ViewPlace::new_in("lune", 3, 1, 4),
            terminal: None,
            cwd: Cwd::of(
                Some(Directory::parse("/w/lune/main".to_owned()).unwrap()),
                |dir| {
                    let branch = lune_protocol::said::Said::new("feat/ask".to_owned());
                    Folder {
                        dir,
                        branch,
                        repository: None,
                        pull_request: None,
                    }
                },
            ),
            ran: Ran::Unhooked,
            arranged: None,
        };
        Target::View(machine, row)
    }

    fn view_row() -> Target {
        view_row_on(Machine::Local)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(ask: &mut Ask, text: &str) {
        for c in text.chars() {
            assert_eq!(ask.key(&key(KeyCode::Char(c))), Keyed::Nothing);
        }
    }

    fn shown(ask: &mut Ask, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        ask.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(
                moonlight::ALL.contains(&cell.fg),
                "{:?} in {:?}",
                cell.symbol(),
                cell.fg
            );
        }
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn reversed_cells(ask: &mut Ask, area: Rect, wall: SystemTime) -> Vec<(u16, u16)> {
        let mut buffer = Buffer::empty(area);
        ask.draw(&mut buffer, area, wall);
        let mut cells = Vec::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if buffer[(x, y)].modifier.contains(Modifier::REVERSED) {
                    cells.push((x, y));
                }
            }
        }
        cells
    }

    fn sweep(ask: &mut Ask, from: (u16, u16), to: (u16, u16)) -> Option<Excerpt> {
        let left = MouseButton::Left;
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Down(left), from.0, from.1)),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), to.0, to.1)),
            Mouse::Active
        );
        match ask.mouse(mouse(MouseEventKind::Up(left), to.0, to.1)) {
            Mouse::Copied(excerpt) => Some(excerpt),
            Mouse::Active => None,
            other => panic!("unexpected mouse result on release: {other:?}"),
        }
    }

    fn at_a_turn() -> Asking {
        Asking::on_thread(|_| {})
    }

    #[test]
    fn what_you_dictate_goes_on_the_line_after_what_you_typed() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.dictated("Open a tab.");
        assert_eq!(ask.typed, "Open a tab.");
        ask.dictated("Then\nrun the tests.");
        assert_eq!(ask.typed, "Open a tab. Then run the tests.");
        ask.turn = Some(at_a_turn());
        ask.dictated("Close it.");
        assert_eq!(ask.typed, "Open a tab. Then run the tests.");
    }

    #[test]
    fn the_box_shows_the_talk_and_takes_what_you_type_between_turns() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Nothing);
        typed(&mut ask, "close bipa x");
        assert_eq!(ask.key(&key(KeyCode::Backspace)), Keyed::Nothing);
        ask.paste("and\n elo\x1b[31m");
        assert_eq!(ask.typed, "close bipa and elo [31m");
        let line_back = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(ask.key(&line_back), Keyed::Nothing);
        typed(&mut ask, "close bipa and elo");
        assert_eq!(
            shown(&mut ask, 50, 7)[1..6],
            [
                "┌─ Ask ────────────────────────────── lune/main ─┐",
                "│ › close bipa and elo_                          │",
                "│                                                │",
                "│ <ENTER> Send  <Alt r> Dictate  <ESC> Close     │",
                "└────────────────────────────────────────────────┘",
            ]
        );
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Send);

        ask.talk.push(Said::You(std::mem::take(&mut ask.typed)));
        ask.turn = Some(at_a_turn());
        let session = SessionId::parse("7c22").unwrap();
        ask.heard(Heard::Step(Step::Session(session.clone())));
        let ran = |id: &str, command: &str| Step::Ran {
            id: id.to_owned(),
            command: command.to_owned(),
        };
        ask.heard(Heard::Step(ran("toolu_1", "lune close bipa\nelo")));
        ask.heard(Heard::Step(ran("toolu_2", "touch elo")));
        ask.heard(Heard::Step(Step::Said(
            "This kills bipa and elo.\n\nClose them?".to_owned(),
        )));
        assert_eq!(ask.session, Some(session));
        assert_eq!(ask.key(&key(KeyCode::Char('y'))), Keyed::Nothing);
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Nothing);
        ask.paste("y");
        assert_eq!(ask.typed, "");
        let mut asked = [
            "┌─ Ask ────────────────────────────── lune/main ─┐",
            "│ › close bipa and elo                           │",
            "│   $ lune close bipa elo                        │",
            "│   $ touch elo                                  │",
            "│ This kills bipa and elo.                       │",
            "│                                                │",
            "│ Close them?                                    │",
        ];
        let lines = shown(&mut ask, 50, 13);
        assert_eq!(lines[1..8], asked);
        assert_eq!(
            lines[8..12],
            [
                "│ ⠋ Asking sonnet…                               │",
                "│                                                │",
                "│ <ESC> - Close                                  │",
                "└────────────────────────────────────────────────┘",
            ]
        );
        assert!(ask.turning());

        ask.heard(Heard::Step(Step::Denied("toolu_2".to_owned())));
        ask.heard(Heard::Ended(Ok(())));
        assert!(!ask.turning());
        asked[3] = "│   ✗ touch elo                                  │";
        typed(&mut ask, "yes");
        let lines = shown(&mut ask, 50, 12);
        assert_eq!(lines[..7], asked);
        assert_eq!(
            lines[7..9],
            [
                "│                                                │",
                "│ › yes_                                         │",
            ]
        );
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Send);
        ask.talk.push(Said::You(std::mem::take(&mut ask.typed)));
        ask.turn = Some(at_a_turn());
        ask.heard(Heard::Ended(Err(
            "ask: claude failed: no such session".to_owned()
        )));
        let lines = shown(&mut ask, 50, 15);
        assert_eq!(lines[..7], asked);
        assert_eq!(
            lines[7..12],
            [
                "│                                                │",
                "│ › yes                                          │",
                "│ ask: claude failed: no such session            │",
                "│                                                │",
                "│ › _                                            │",
            ]
        );
        assert!(ask.turn.is_none());
        assert_eq!(ask.key(&key(KeyCode::Esc)), Keyed::Close);
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(ask.key(&interrupt), Keyed::Interrupt);
    }

    #[test]
    fn a_talk_too_long_for_the_box_loses_its_start() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::You("tackle these".to_owned()));
        ask.talk.extend((1..=6).map(|n| Said::Ran {
            id: format!("toolu_{n}"),
            command: format!("lune open tab {n}"),
            denied: false,
        }));
        ask.talk.push(Said::Agent("Opened six tabs.".to_owned()));
        assert_eq!(
            shown(&mut ask, 40, 9),
            [
                "┌─ Ask ──────────────────── lune/main ─┐",
                "│ …                                    │",
                "│   $ lune open tab 6                  │",
                "│ Opened six tabs.                     │",
                "│                                      │",
                "│ › _                                  │",
                "│                                      │",
                "│ <ENTER>/<Alt r>/<ESC>                │",
                "└──────────────────────────────────────┘",
            ]
        );
    }

    #[test]
    fn the_agent_works_on_the_row_s_machine_where_the_row_is() {
        let top = |ask: &mut Ask| shown(ask, 40, 7)[1].clone();
        let mut view = Ask::new(1, &view_row(), Path::new("/w"));
        assert_eq!(view.works, Works::Here(PathBuf::from("/w/lune/main")));
        let row = "is view lune:4, in /w/lune/main, on branch feat/ask: \"this\"";
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&mut view), "┌─ Ask ──────────────────── lune/main ─┐");
        let session = Ask::new(2, &project_on(Machine::Local), Path::new("/w"));
        assert_eq!(session.works, Works::Here(PathBuf::from("/w")));
        let session_row = "is project lune: \"this\"";
        assert!(
            session.highlighted.contains(session_row),
            "{}",
            session.highlighted
        );

        let there = |cwd: Option<&str>| {
            let cwd = cwd.map(|cwd| Directory::parse(cwd.to_owned()).unwrap());
            Works::There {
                on: "nixos".parse().unwrap(),
                cwd,
            }
        };
        let mut view = Ask::new(3, &view_row_on(nixos()), Path::new("/w"));
        assert_eq!(view.works, there(Some("/w/lune/main")));
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&mut view), "┌─ Ask ─────────── lune/main on nixos ─┐");
        let mut session = Ask::new(4, &project_on(nixos()), Path::new("/w"));
        assert_eq!(session.works, there(None));
        assert_eq!(
            top(&mut session),
            "┌─ Ask ─────────────────── ~ on nixos ─┐"
        );
        let header = Ask::new(5, &Target::Machine(nixos()), Path::new("/w"));
        assert_eq!(header.works, there(None));
        let row = "is the header of this machine: \"this\"";
        assert!(header.highlighted.contains(row), "{}", header.highlighted);

        let home = |on: &Machine| Ask::homed(7, on, &Dir::parse("/w/lune").unwrap(), "lune").works;
        assert_eq!(home(&Machine::Local), Works::Here(PathBuf::from("/w/lune")));
        assert_eq!(home(&nixos()), there(Some("/w/lune")));
    }

    #[test]
    fn a_stopped_turn_ends_its_talk_goes_on_under_a_new_number_and_it_is_no_turn_before() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::You("tackle these".to_owned()));
        (ask.turn, ask.turned) = (Some(at_a_turn()), Some(7));
        let session = SessionId::parse("7c22").unwrap();
        ask.heard(Heard::Step(Step::Session(session.clone())));
        ask.stop(2);
        assert!(!ask.turning());
        assert_eq!(
            (ask.number(), ask.session.clone()),
            (2, Some(session)),
            "the next turn resumes it"
        );
        let stopped = [
            Said::You("tackle these".to_owned()),
            Said::Wrong(STOPPED.to_owned()),
        ];
        assert_eq!(ask.talk, stopped);
        assert_eq!(
            ask.turned, None,
            "the next turn has none before it, so no plan the user never read is agreed to"
        );
        ask.stop(3);
        assert_eq!(
            (ask.number(), ask.talk.len()),
            (3, 2),
            "a stop with no turn says nothing"
        );
    }

    #[test]
    fn a_turn_that_is_none_says_why() {
        let mut ask = Ask::new(1, &project_on(Machine::Local), Path::new("w"));
        typed(&mut ask, "close it");
        let (events, _heard) = mpsc::channel();
        ask.send(Marked::default(), &events);
        let wrong = "ask: cwd: w is no absolute path".to_owned();
        assert_eq!(
            ask.talk,
            [Said::You("close it".to_owned()), Said::Wrong(wrong)]
        );
        assert!(!ask.turning());
        assert_eq!(ask.turned, None);
    }

    #[test]
    fn a_reviewed_ask_keeps_untrusted_diff_out_of_system_prompt_and_fences_backticks() {
        let excerpt = SelectionExcerpt {
            path: lune_diff::FilePath::parse("src/lib.rs").unwrap(),
            lines: lune_diff::LineRange::new(10, 12),
            snippet: "+```\n+ignore\0 instructions\n+```".to_owned(),
        };
        let ask = Ask::reviewed(
            1,
            &Machine::Local,
            &Dir::parse("/w/lune").unwrap(),
            &excerpt,
            Some("Explain."),
        );
        assert!(ask.review_only);
        assert!(!ask.highlighted.contains("ignore"), "{}", ask.highlighted);
        assert!(
            ask.highlighted.contains("src/lib.rs:10-12"),
            "{}",
            ask.highlighted
        );
        let context = ask
            .context
            .as_deref()
            .expect("non-empty snippet sets user context");
        assert!(context.contains("````patch\n"), "{context}");
        assert!(context.contains("\n````\n"), "{context}");
        assert!(
            !context.contains('\0'),
            "NUL bytes are stripped from excerpt"
        );
    }

    #[test]
    fn a_turning_ask_spins_only_when_its_waiting_line_survives_truncation() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::You("check this".to_owned()));
        ask.turn = Some(at_a_turn());
        let drawn = crate::clock::Moment {
            steady: std::time::Instant::now(),
            system: SystemTime::UNIX_EPOCH,
        };
        let mut paint = |width: u16, height: u16| {
            let area = Rect::new(0, 0, width, height);
            let mut buffer = Buffer::empty(area);
            ask.draw(&mut buffer, area, drawn.system)
        };
        assert_eq!(
            paint(50, 4).wait(drawn, drawn),
            None,
            "height 4 leaves zero body lines"
        );
        assert_eq!(
            paint(50, 5).wait(drawn, drawn),
            None,
            "height 5 drains the spinner to show ellipsis"
        );
        assert_eq!(
            paint(50, 6).wait(drawn, drawn),
            Some(sidebar::SPIN),
            "height 6 keeps the spinner on screen"
        );
    }

    fn drawn_body(ask: &Ask) -> &ShownBody {
        let DrawnAsk::Shown { body, .. } = &ask.drawn else {
            panic!("body drawn");
        };
        body
    }

    #[test]
    fn dragging_copies_wrapped_paragraphs_uncut_commands_and_typed_text_without_cursor_or_chrome() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        let full_command = "cargo nextest run --workspace --profile quick --verbose";
        ask.talk.push(Said::You("check it".to_owned()));
        ask.talk.push(Said::Ran {
            id: "toolu_1".to_owned(),
            command: full_command.to_owned(),
            denied: false,
        });
        ask.talk.push(Said::Agent(
            "See https://github.com/lorenzolfm/lune/issues/852 for details.\n\nSecond line."
                .to_owned(),
        ));
        typed(&mut ask, "follow up");
        let area = Rect::new(0, 0, 36, 16);
        let mut buffer = Buffer::empty(area);
        ask.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        let (hit, text) = (drawn_body(&ask).hit(), drawn_body(&ask).text().rect());

        let command_y = text.y + 1;
        assert_eq!(
            sweep(
                &mut ask,
                (text.x + 4, command_y),
                (area.right() + 5, command_y)
            )
            .as_deref(),
            Some(full_command),
            "dragging over an elided command copies the uncut command without borders"
        );

        let url_first_y = text.y + 2;
        let second_para_y = text.y + 6;
        assert_eq!(
            sweep(
                &mut ask,
                (hit.x, url_first_y),
                (area.right() + 5, area.bottom() + 5)
            )
            .as_deref(),
            Some(
                "See https://github.com/lorenzolfm/lune/issues/852 for details.\n\nSecond line.\n\n› follow up"
            ),
            "word-wrapped and hard-wrapped lines rejoin cleanly, margin gutter clamps, cursor and footer stay out"
        );
        assert!(
            !reversed_cells(&mut ask, area, SystemTime::UNIX_EPOCH).is_empty(),
            "a completed sweep stays highlighted"
        );
        assert_eq!(
            sweep(
                &mut ask,
                (text.x + 2, second_para_y),
                (text.x + 2, second_para_y)
            ),
            None,
            "a single click copies nothing"
        );
        assert_eq!(reversed_cells(&mut ask, area, SystemTime::UNIX_EPOCH), []);

        assert_eq!(
            sweep(
                &mut ask,
                (hit.x, text.bottom() - 1),
                (area.right(), text.bottom() - 1)
            )
            .as_deref(),
            Some("› follow up"),
            "dragging to the end of the prompt omits the trailing cursor"
        );
        let only_cursor_x = text.x + u16::try_from(sweep::cells("› follow up")).unwrap();
        assert_eq!(
            sweep(
                &mut ask,
                (only_cursor_x, text.bottom() - 1),
                (area.right(), text.bottom() - 1)
            ),
            None,
            "dragging only over the prompt cursor copies nothing"
        );
    }

    #[test]
    fn a_sweep_survives_spinner_ticks_below_it_and_clears_when_text_shifts_or_drag_is_cancelled() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::Agent("alpha beta".to_owned()));
        ask.turn = Some(at_a_turn());
        let area = Rect::new(0, 0, 44, 10);
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + sidebar::SPIN;
        assert_eq!(reversed_cells(&mut ask, area, t0), []);
        let text = drawn_body(&ask).text().rect();

        assert_eq!(
            sweep(&mut ask, (text.x, text.y), (text.x + 4, text.y)).as_deref(),
            Some("alpha")
        );
        let expected: Vec<(u16, u16)> = (text.x..=text.x + 4).map(|x| (x, text.y)).collect();
        assert_eq!(reversed_cells(&mut ask, area, t0), expected);
        assert_eq!(
            reversed_cells(&mut ask, area, t1),
            expected,
            "spinner animation below the selection keeps the highlight"
        );

        ask.heard(Heard::Step(Step::Said(
            "more text that shifts the box".to_owned(),
        )));
        assert_eq!(
            reversed_cells(&mut ask, area, t1),
            [],
            "new output shifting the box clears the kept highlight"
        );

        let text = drawn_body(&ask).text().rect();
        let left = MouseButton::Left;
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Down(left), text.x, text.y)),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), text.x + 4, text.y)),
            Mouse::Active
        );
        assert!(ask.pressed());
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Moved, text.x + 4, text.y)),
            Mouse::Active
        );
        assert!(!ask.pressed());
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Up(left), text.x + 4, text.y)),
            Mouse::Ignored
        );
        assert_eq!(reversed_cells(&mut ask, area, t1), []);

        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Down(left), text.x, text.y)),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), text.x + 4, text.y)),
            Mouse::Active
        );
        assert!(ask.pressed());
        let taller = Rect::new(0, 0, 44, 20);
        assert_eq!(
            reversed_cells(&mut ask, taller, t1),
            [],
            "resizing the box mid-drag cancels the drag so release never reads stale coordinates"
        );
        assert!(!ask.pressed());
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Up(left), text.x + 4, text.y)),
            Mouse::Ignored
        );
    }

    #[test]
    fn top_truncation_shifts_cuts_and_does_not_join_onto_the_ellipsis() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        let long_command = "cargo nextest run --workspace --profile quick --verbose";
        ask.talk.push(Said::You(
            "first line one two three four five six".to_owned(),
        ));
        ask.talk.push(Said::Agent(
            "paragraph one two three four five six seven eight nine ten eleven twelve".to_owned(),
        ));
        ask.talk.push(Said::Ran {
            id: "toolu_1".to_owned(),
            command: long_command.to_owned(),
            denied: false,
        });
        let area = Rect::new(0, 0, 36, 9);
        let mut buffer = Buffer::empty(area);
        ask.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        let text = drawn_body(&ask).text().rect();
        assert_eq!(buffer[(text.x, text.y)].symbol(), "…");
        assert_eq!(
            sweep(&mut ask, (text.x, text.y), (text.right() - 1, text.y + 1))
                .as_deref()
                .map(|excerpt| excerpt.starts_with("…\n")),
            Some(true),
            "first kept line after top ellipsis starts on a new line"
        );
        let ran_y = (text.y..text.bottom())
            .find(|&y| buffer[(text.x + 2, y)].symbol() == "$")
            .expect("elided command line is visible");
        assert_eq!(
            sweep(&mut ask, (text.x + 4, ran_y), (text.right() - 1, ran_y)).as_deref(),
            Some(long_command),
            "cut offset shifts with top truncation so the full command still copies"
        );
    }

    #[test]
    fn denied_elided_command_copies_uncut_and_talk_stays_capped() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        let long_command = "rm -rf /tmp/very-long-command-path-for-testing-elision";
        for n in 0..(TALK_MAX + 12) {
            ask.heard(Heard::Step(Step::Said(format!("turn {n}"))));
        }
        assert_eq!(ask.talk.len(), TALK_MAX);
        ask.heard(Heard::Step(Step::Ran {
            id: "toolu_denied".to_owned(),
            command: long_command.to_owned(),
        }));
        ask.heard(Heard::Step(Step::Denied("toolu_denied".to_owned())));
        assert_eq!(ask.talk.len(), TALK_MAX);
        let area = Rect::new(0, 0, 36, 9);
        let mut buffer = Buffer::empty(area);
        ask.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        let text = drawn_body(&ask).text().rect();
        let denied_y = (text.y..text.bottom())
            .find(|&y| buffer[(text.x + 2, y)].symbol() == "✗")
            .expect("denied elided command line is visible");
        assert_eq!(
            sweep(
                &mut ask,
                (text.x + 4, denied_y),
                (text.right() - 1, denied_y)
            )
            .as_deref(),
            Some(long_command),
            "dragging over a denied elided command restores the uncut command"
        );
    }

    #[test]
    fn active_drag_survives_spinner_ticks_but_cancels_on_capped_scroll_or_in_place_denial() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.heard(Heard::Step(Step::Said("line one".to_owned())));
        ask.heard(Heard::Step(Step::Said("line two".to_owned())));
        ask.heard(Heard::Step(Step::Said("line three".to_owned())));
        ask.heard(Heard::Step(Step::Ran {
            id: "toolu_1".to_owned(),
            command: "cargo test".to_owned(),
        }));
        ask.turn = Some(at_a_turn());

        let area = Rect::new(0, 0, 36, 9);
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + sidebar::SPIN;
        assert_eq!(reversed_cells(&mut ask, area, t0), []);
        let text_before = drawn_body(&ask).text().rect();
        let left = MouseButton::Left;

        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Down(left),
                text_before.x,
                text_before.y + 1
            )),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Drag(left),
                text_before.x + 5,
                text_before.y + 1
            )),
            Mouse::Active
        );
        assert!(
            !reversed_cells(&mut ask, area, t1).is_empty(),
            "spinner tick below the drag keeps the active drag"
        );
        assert!(ask.pressed());

        ask.heard(Heard::Step(Step::Denied("toolu_1".to_owned())));
        assert_eq!(
            reversed_cells(&mut ask, area, t1),
            [],
            "in-place command denial cancels an active drag even when box geometry is unchanged"
        );
        assert_eq!(drawn_body(&ask).text().rect(), text_before);
        assert!(!ask.pressed());
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Up(left),
                text_before.x + 5,
                text_before.y + 1
            )),
            Mouse::Ignored
        );

        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Down(left),
                text_before.x,
                text_before.y + 1
            )),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Drag(left),
                text_before.x + 5,
                text_before.y + 1
            )),
            Mouse::Active
        );
        assert!(ask.pressed());

        ask.heard(Heard::Step(Step::Said("scrolled new output".to_owned())));
        assert_eq!(
            reversed_cells(&mut ask, area, t1),
            [],
            "capped-height scrolling cancels an active drag instead of copying shifted text"
        );
        assert_eq!(drawn_body(&ask).text().rect(), text_before);
        assert!(!ask.pressed());
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Up(left),
                text_before.x + 5,
                text_before.y + 1
            )),
            Mouse::Ignored
        );
    }

    #[test]
    fn mid_frame_mouse_events_and_same_top_vertical_shrink_still_invalidate_selection() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.heard(Heard::Step(Step::Said("same line".to_owned())));
        ask.heard(Heard::Step(Step::Said("same line".to_owned())));
        ask.heard(Heard::Step(Step::Said("same line".to_owned())));
        ask.heard(Heard::Step(Step::Ran {
            id: "toolu_1".to_owned(),
            command: "cargo test".to_owned(),
        }));
        ask.turn = Some(at_a_turn());

        let area = Rect::new(0, 0, 36, 9);
        let t0 = SystemTime::UNIX_EPOCH;
        assert_eq!(reversed_cells(&mut ask, area, t0), []);
        let text = drawn_body(&ask).text().rect();
        let left = MouseButton::Left;

        // Drag arriving after in-place denial and before the next paint must not
        // stamp the old drawn body with the new BodyVersion.
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Down(left), text.x, text.y + 1)),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), text.x + 4, text.y + 1)),
            Mouse::Active
        );
        ask.heard(Heard::Step(Step::Denied("toolu_1".to_owned())));
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), text.x + 6, text.y + 1)),
            Mouse::Active
        );
        assert_eq!(
            reversed_cells(&mut ask, area, t0),
            [],
            "an active drag is still cancelled on the next paint even when Drag arrived after Denied"
        );
        assert!(!ask.pressed());

        // Release arriving after a capped scroll of identical lines and before
        // paint copies what was on screen, then clears the stale highlight on paint.
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Down(left), text.x, text.y + 1)),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Drag(left), text.x + 4, text.y + 1)),
            Mouse::Active
        );
        ask.heard(Heard::Step(Step::Said("same line".to_owned())));
        assert_eq!(
            ask.mouse(mouse(MouseEventKind::Up(left), text.x + 4, text.y + 1)),
            Mouse::Copied(Excerpt::of("same".to_owned()).unwrap())
        );
        assert_eq!(
            reversed_cells(&mut ask, area, t0),
            [],
            "a highlight completed before paint clears once the scrolled BodyVersion paints"
        );

        // Shrinking a height-capped box keeps text.x, text.y, and text.width equal
        // while reducing text.height; row 0 ("…") must not keep a stale drag.
        let before_shrink = drawn_body(&ask).text().rect();
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Down(left),
                before_shrink.x,
                before_shrink.y
            )),
            Mouse::Started
        );
        assert_eq!(
            ask.mouse(mouse(
                MouseEventKind::Drag(left),
                before_shrink.x + 1,
                before_shrink.y
            )),
            Mouse::Active
        );
        let shorter = Rect::new(0, 0, 36, 8);
        assert_eq!(
            reversed_cells(&mut ask, shorter, t0),
            [],
            "vertical shrink cancels an active drag even when text_y is unchanged"
        );
        let after_shrink = drawn_body(&ask).text().rect();
        assert_eq!(
            (after_shrink.x, after_shrink.y, after_shrink.width),
            (before_shrink.x, before_shrink.y, before_shrink.width)
        );
        assert!(after_shrink.height < before_shrink.height);
        assert!(!ask.pressed());
    }
}
