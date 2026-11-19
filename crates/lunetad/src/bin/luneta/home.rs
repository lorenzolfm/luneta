use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::asks::Marked;
use lunetad::projects::ProjectId;
use lunetad::work::Work;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::ask::{self, Ask, Heard};
use crate::gruvbox;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::sidebar::{self, ACCENT};

pub const AGAIN: Duration = Duration::from_secs(60);

const ASK_TITLE: &str = "Ask";

const MARKED: &str = "◆";

const UNMARKED: &str = "◇";

const ASKING: &str = "asking gh for the open issues and pull requests…";

const ASK_ROWS_MIN: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    Issue(NonZeroU32),
    Pull(NonZeroU32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    Send,
    StartOver,
}

#[derive(Debug)]
enum Listing {
    Asking,
    Listed { work: Work, stale: Option<String> },
    Unlisted(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fetch {
    Due,
    Asking,
    Got(Instant),
}

#[derive(Debug)]
pub struct Home {
    number: u64,
    on: Machine,
    project: ProjectId,
    name: String,
    dir: String,
    known: bool,
    listing: Listing,
    fetch: Fetch,
    cursor: usize,
    marked: BTreeSet<Item>,
    ask: Ask,
}

impl Home {
    pub fn new(number: u64, asks: u64, on: Machine, project: ProjectId) -> Home {
        let ask = Ask::homed(asks, &on, "", "");
        Home {
            number,
            on,
            project,
            name: String::new(),
            dir: String::new(),
            known: false,
            listing: Listing::Asking,
            fetch: Fetch::Due,
            cursor: 0,
            marked: BTreeSet::new(),
            ask,
        }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn machine(&self) -> &Machine {
        &self.on
    }

    pub fn project(&self) -> ProjectId {
        self.project
    }

    pub fn ask_number(&self) -> u64 {
        self.ask.number()
    }

    pub fn named(&mut self, name: &str, dir: &str) {
        self.known = true;
        if self.name != name || self.dir != dir {
            name.clone_into(&mut self.name);
            dir.clone_into(&mut self.dir);
            self.ask.rehome(&self.on, dir, name);
        }
    }

    pub fn known(&self) -> bool {
        self.known
    }

    pub fn asks(&mut self, now: Instant) -> bool {
        let due = match self.fetch {
            Fetch::Due => true,
            Fetch::Asking => false,
            Fetch::Got(at) => now >= at + AGAIN,
        };
        if due {
            self.fetch = Fetch::Asking;
        }
        due
    }

    pub fn due(&self) -> Option<Instant> {
        match self.fetch {
            Fetch::Got(at) => Some(at + AGAIN),
            Fetch::Due | Fetch::Asking => None,
        }
    }

    pub fn listed(&mut self, listed: Result<Work, String>, now: Instant) {
        assert_eq!(self.fetch, Fetch::Asking, "a listing comes only when one was asked");
        self.fetch = Fetch::Got(now);
        let listing = std::mem::replace(&mut self.listing, Listing::Asking);
        self.listing = match (listed, listing) {
            (Ok(work), _) => Listing::Listed { work, stale: None },
            (Err(why), Listing::Listed { work, .. }) => Listing::Listed { work, stale: Some(why) },
            (Err(why), Listing::Asking | Listing::Unlisted(_)) => Listing::Unlisted(why),
        };
        let items = self.items();
        self.marked.retain(|item| items.contains(item));
        self.cursor = self.cursor.min(items.len().saturating_sub(1));
    }

    fn items(&self) -> Vec<Item> {
        let Listing::Listed { work, .. } = &self.listing else {
            return Vec::new();
        };
        let pulls = work.pulls().iter().map(|pull| Item::Pull(pull.number));
        pulls.chain(work.issues().iter().map(|issue| Item::Issue(issue.number))).collect()
    }

    pub fn key(&mut self, key: &KeyEvent) -> Keyed {
        let bare = key.modifiers.is_empty();
        match key.code {
            KeyCode::Up if bare => self.step_up(),
            KeyCode::Down if bare => self.step_down(),
            KeyCode::Tab if bare => {
                self.mark();
                self.step_down();
            },
            KeyCode::BackTab if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.mark();
                self.step_up();
            },
            _ => {
                return match self.ask.key(key) {
                    ask::Keyed::Nothing => Keyed::Nothing,
                    ask::Keyed::Send => Keyed::Send,
                    ask::Keyed::Close => Keyed::StartOver,
                };
            },
        }
        Keyed::Nothing
    }

    fn step_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn step_down(&mut self) {
        let last = self.items().len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(last);
    }

    fn mark(&mut self) {
        let items = self.items();
        let Some(item) = items.get(self.cursor).copied() else {
            assert!(items.is_empty(), "the cursor is on a listed row while there is one");
            return;
        };
        if !self.marked.remove(&item) {
            self.marked.insert(item);
        }
    }

    fn marks(&self) -> Marked {
        let numbers = |issues: bool| {
            let each = self.marked.iter().filter_map(move |item| match (item, issues) {
                (Item::Issue(number), true) | (Item::Pull(number), false) => Some(*number),
                (Item::Issue(_), false) | (Item::Pull(_), true) => None,
            });
            each.collect::<Vec<_>>()
        };
        let marked = Marked::new(numbers(true), numbers(false));
        marked.expect("only what is listed is marked, and gh lists only so many")
    }

    pub fn send(&mut self, events: &Sender<Event>) {
        let marked = self.marks();
        self.ask.send(marked, events);
    }

    pub fn start_over(&mut self, asks: u64) {
        assert!(asks > self.ask.number(), "a new talk is numbered past the one it replaces");
        self.ask = Ask::homed(asks, &self.on, &self.dir, &self.name);
        self.marked.clear();
    }

    pub fn heard(&mut self, heard: Heard) {
        self.ask.heard(heard);
    }

    pub fn paste(&mut self, text: &str) {
        self.ask.paste(text);
    }

    pub fn turning(&self) -> bool {
        self.ask.turning()
    }

    pub fn draw(&self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
        let Some(inner) = area.width.checked_sub(2).map(usize::from).filter(|inner| *inner > 0)
        else {
            return;
        };
        let most = (area.height / 2).max(ASK_ROWS_MIN).min(area.height);
        let room = usize::from(most.saturating_sub(3));
        let mut talk = self.ask.talked(inner, now);
        if talk.len() > room {
            let kept = room.saturating_sub(1);
            talk.drain(..talk.len() - kept);
            if room > 0 {
                talk.insert(0, Line::styled(" …", Style::new().add_modifier(Modifier::DIM)));
            }
        }
        talk.push(menu::keys_line(inner, &self.hints()));
        let rows = u16::try_from(talk.len() + 2).map_or(most, |rows| rows.min(most));
        assert!(rows <= area.height, "the prompt's box fits in the home");
        let asked = Rect::new(area.x, area.bottom() - rows, area.width, rows);
        let right = match &self.on {
            Machine::Local => sidebar::short_cwd(&self.dir),
            Machine::Remote(on) => format!("{} on {on}", sidebar::short_cwd(&self.dir)),
        };
        menu::boxed(buffer, asked, &menu::rule(ASK_TITLE, &right, inner), &talk);
        let lists = Rect::new(area.x, area.y, area.width, area.height - rows);
        self.draw_lists(buffer, lists);
    }

    fn hints(&self) -> Vec<(Hint, &'static str)> {
        if self.ask.turning() {
            return vec![(Hint::Moves, "Move"), (Hint::Tab, "Mark"), (Hint::Esc, "Stop")];
        }
        vec![(Hint::Moves, "Move"), (Hint::Tab, "Mark"), (Hint::Enter, "Send"), (Hint::Esc, "Start over")]
    }

    fn draw_lists(&self, buffer: &mut Buffer, area: Rect) {
        let width = usize::from(area.width);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut head = vec![Line::from(vec![
            Span::raw(" "),
            Span::styled(self.name.clone(), Style::new().fg(gruvbox::FG1).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", sidebar::short_cwd(&self.dir)), dim),
        ])];
        let (lines, at) = self.list_lines(width);
        if let Listing::Listed { stale: Some(why), .. } = &self.listing {
            let why = format!(" not listed again: {why}");
            head.push(Line::styled(sidebar::cut(&why, width), Style::new().fg(gruvbox::RED)));
        }
        head.push(Line::default());
        let rows = usize::from(area.height);
        let below = rows.saturating_sub(head.len());
        let top = match at {
            Some(at) if at >= below => at + 1 - below,
            Some(_) | None => 0,
        };
        let shown = head.into_iter().chain(lines.into_iter().skip(top)).take(rows);
        for (line, y) in shown.zip(area.top()..area.bottom()) {
            buffer.set_line(area.x, y, &line, area.width);
        }
    }

    fn list_lines(&self, width: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let work = match &self.listing {
            Listing::Asking => return (vec![Line::styled(format!("   {ASKING}"), dim)], None),
            Listing::Unlisted(why) => {
                let said = format!(" gh lists nothing here: {why}");
                let red = Style::new().fg(gruvbox::RED);
                return (menu::wrap(&said, width, red), None);
            },
            Listing::Listed { work, .. } => work,
        };
        let numbered = |number: NonZeroU32| format!("#{number}");
        let numbers = work.issues().iter().map(|it| it.number).chain(work.pulls().iter().map(|it| it.number));
        let number_width = numbers.map(|number| numbered(number).len()).max().unwrap_or(0);
        let (mut lines, mut at, mut item) = (Vec::new(), None, 0);
        let heading = |title: &str, count: usize| {
            Line::from(vec![
                Span::styled(format!(" {title}"), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" · {count}"), dim),
            ])
        };
        lines.push(heading("Pull requests", work.pulls().len()));
        if work.pulls().is_empty() {
            lines.push(Line::styled("   no open pull requests", dim));
        }
        for pull in work.pulls() {
            let here = item == self.cursor;
            at = at.or(here.then_some(lines.len()));
            let marked = self.marked.contains(&Item::Pull(pull.number));
            let side = if pull.draft { format!("{} · draft", pull.branch) } else { pull.branch.clone() };
            lines.push(row(width, (here, marked), (&numbered(pull.number), number_width), &pull.title, &side));
            item += 1;
        }
        lines.push(Line::default());
        lines.push(heading("Issues", work.issues().len()));
        if work.issues().is_empty() {
            lines.push(Line::styled("   no open issues", dim));
        }
        for issue in work.issues() {
            let here = item == self.cursor;
            at = at.or(here.then_some(lines.len()));
            let marked = self.marked.contains(&Item::Issue(issue.number));
            let side = issue.labels.join(", ");
            lines.push(row(width, (here, marked), (&numbered(issue.number), number_width), &issue.title, &side));
            item += 1;
        }
        (lines, at)
    }
}

fn row(
    width: usize,
    (here, marked): (bool, bool),
    (number, number_width): (&str, usize),
    title: &str,
    side: &str,
) -> Line<'static> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let margin = if here { Span::styled(" › ", Style::new().fg(ACCENT)) } else { Span::raw("   ") };
    let mark = if marked {
        Span::styled(MARKED, Style::new().fg(ACCENT))
    } else {
        Span::styled(UNMARKED, dim)
    };
    let number = Span::styled(format!(" {number:<number_width$}  "), Style::new().fg(gruvbox::YELLOW));
    let lead = margin.width() + mark.width() + number.width();
    let room = width.saturating_sub(lead + 1);
    let side_room = if side.is_empty() { 0 } else { (side.width() + 2).min(room / 3) };
    let title_room = room.saturating_sub(side_room);
    let title = sidebar::cut(title, title_room);
    let gap = title_room.saturating_sub(title.width());
    let title_style = if here { Style::new().fg(gruvbox::FG1).add_modifier(Modifier::BOLD) } else { Style::new().fg(gruvbox::FG1) };
    let mut spans = vec![margin, mark, number, Span::styled(title, title_style)];
    if side_room > 2 {
        spans.push(Span::raw(" ".repeat(gap + 2)));
        spans.push(Span::styled(sidebar::cut(side, side_room - 2), dim));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use lunetad::work::{Issue, Pull};

    use super::*;

    fn number(at: u32) -> NonZeroU32 {
        NonZeroU32::new(at).unwrap()
    }

    fn work(issues: &[(u32, &str, &[&str])], pulls: &[(u32, &str, &str, bool)]) -> Work {
        let issues = issues.iter().map(|(at, title, labels)| Issue {
            number: number(*at),
            title: (*title).to_owned(),
            labels: labels.iter().map(|it| (*it).to_owned()).collect(),
        });
        let pulls = pulls.iter().map(|(at, title, branch, draft)| Pull {
            number: number(*at),
            title: (*title).to_owned(),
            draft: *draft,
            branch: (*branch).to_owned(),
            author: None,
        });
        Work::new(issues.collect(), pulls.collect()).unwrap()
    }

    fn listed() -> Work {
        let issues: [(u32, &str, &[&str]); 2] =
            [(507, "Open a project on a home view", &["enhancement"]), (98, "Menu says no", &[])];
        work(&issues, &[(524, "Say view, not pane", "issue-511", true)])
    }

    fn home() -> Home {
        let mut home = Home::new(1, 1, Machine::Local, ProjectId::of(3).unwrap());
        home.named("luneta", "/w/luneta");
        home
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn shown(home: &Home, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        home.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != " ") {
            assert!(gruvbox::ALL.contains(&cell.fg) || cell.fg == ratatui::style::Color::Reset);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .map(|line| line.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn the_lists_show_pull_requests_then_issues_and_tab_marks_and_steps_on() {
        let (mut home, now) = (home(), Instant::now());
        assert!(home.asks(now));
        assert_eq!(shown(&home, 60, 12)[2], "   asking gh for the open issues and pull requests…");
        home.listed(Ok(listed()), now);
        assert_eq!(home.key(&key(KeyCode::Tab)), Keyed::Nothing);
        assert_eq!(home.key(&key(KeyCode::Tab)), Keyed::Nothing);
        assert_eq!(home.marks(), Marked::new([number(507)], [number(524)]).unwrap());
        assert_eq!(home.key(&key(KeyCode::Up)), Keyed::Nothing);
        assert_eq!(home.key(&key(KeyCode::BackTab)), Keyed::Nothing);
        assert_eq!(home.marks(), Marked::new([], [number(524)]).unwrap(), "a second mark takes it off");
        assert_eq!(home.key(&key(KeyCode::Down)), Keyed::Nothing);
        assert_eq!(
            shown(&home, 60, 14)[..9],
            [
                " luneta  w/luneta",
                "",
                " Pull requests · 1",
                "   ◆ #524  Say view, not pane                issue-511 · d…",
                "",
                " Issues · 2",
                " › ◇ #507  Open a project on a home view        enhancement",
                "   ◇ #98   Menu says no",
                "",
            ]
        );
        assert_eq!(
            shown(&home, 60, 14)[10..],
            [
                "┌─ Ask ───────────────────────────────────────── w/luneta ─┐",
                "│ › _                                                      │",
                "│ <↓↑> Move  <TAB> Mark  <ENTER> Send  <ESC> Start over    │",
                "└──────────────────────────────────────────────────────────┘",
            ]
        );
        for typed in "go".chars() {
            assert_eq!(home.key(&key(KeyCode::Char(typed))), Keyed::Nothing);
        }
        assert_eq!(home.key(&key(KeyCode::Enter)), Keyed::Send, "the rest of the keys are the prompt's");
    }

    #[test]
    fn the_work_is_asked_once_at_a_time_and_again_a_minute_after_it_came() {
        let (mut home, now) = (home(), Instant::now());
        assert_eq!(home.due(), None);
        assert!(home.asks(now), "a new home asks at once");
        assert!(!home.asks(now + AGAIN), "and not again while it is asked");
        home.listed(Ok(listed()), now);
        assert_eq!(home.due(), Some(now + AGAIN));
        assert!(!home.asks(now + AGAIN.saturating_sub(Duration::from_millis(1))));
        assert!(home.asks(now + AGAIN));
    }

    #[test]
    fn a_listing_that_fails_keeps_the_last_and_one_that_comes_drops_the_marks_of_what_went() {
        let (mut home, now) = (home(), Instant::now());
        home.asks(now);
        home.listed(Err("gh is not on the PATH".to_owned()), now);
        assert_eq!(shown(&home, 60, 10)[2], " gh lists nothing here: gh is not on the PATH");
        home.asks(now + AGAIN);
        home.listed(Ok(listed()), now + AGAIN);
        home.key(&key(KeyCode::Tab));
        home.key(&key(KeyCode::Tab));
        assert_eq!(home.marks(), Marked::new([number(507)], [number(524)]).unwrap());
        home.asks(now + AGAIN * 2);
        home.listed(Err("HTTP 502".to_owned()), now + AGAIN * 2);
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[1], " not listed again: HTTP 502");
        assert!(lines[4].contains("#524"), "{lines:?}");
        home.asks(now + AGAIN * 3);
        let issues: [(u32, &str, &[&str]); 1] = [(98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &[(524, "Say view", "issue-511", false)])), now + AGAIN * 3);
        assert_eq!(home.marks(), Marked::new([], [number(524)]).unwrap(), "#507 was closed");
        assert_eq!(home.cursor, 1, "the cursor stays on a row that is listed");
        home.key(&key(KeyCode::Down));
        assert_eq!(home.cursor, 1, "and no further than the last");
    }

    #[test]
    fn esc_starts_the_talk_over_and_forgets_the_marks() {
        let (mut home, now) = (home(), Instant::now());
        home.asks(now);
        home.listed(Ok(listed()), now);
        home.key(&key(KeyCode::Tab));
        home.key(&key(KeyCode::Char('x')));
        assert_eq!(home.key(&key(KeyCode::Esc)), Keyed::StartOver);
        home.start_over(2);
        assert_eq!((home.ask_number(), home.marks()), (2, Marked::default()));
        assert!(shown(&home, 60, 14).contains(&"│ › _                                                      │".to_owned()));
    }
}
