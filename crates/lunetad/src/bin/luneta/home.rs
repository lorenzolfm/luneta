use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::asks::Marked;
use lunetad::projects::ProjectId;
use lunetad::work::{self, Listed, Work};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::ask::{self, Ask};
use crate::gruvbox;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::sidebar::{self, ACCENT};

pub const AGAIN: Duration = Duration::from_secs(60);

pub const KEPT_MAX: usize = 16;

const _: () = assert!(work::FRESH.as_millis() < AGAIN.as_millis());

const ASK_TITLE: &str = "Ask";

const MARKED: &str = "◆";

const UNMARKED: &str = "◇";

const ASKING: &str = "asking gh for the open issues and pull requests…";

const UNHOSTED: &str = "no remote on GitHub, so nothing is listed here";

const ASK_ROWS_MIN: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    Issue(NonZeroU32),
    Pull(NonZeroU32),
}

#[derive(Debug)]
enum Listing {
    Asking,
    Came { shown: Shown, next: Next },
}

#[derive(Debug)]
enum Shown {
    Listed { work: Work, stale: Option<String> },
    Unhosted,
    Unlisted(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Asking,
    At(Instant),
}

#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "a home holds one, which goes from Not to As once")]
enum Named {
    Not { asks: u64 },
    As { name: String, dir: String, ask: Ask },
}

#[derive(Debug)]
pub struct Home {
    number: u64,
    on: Machine,
    project: ProjectId,
    named: Named,
    listing: Listing,
    cursor: Option<Item>,
    marked: BTreeSet<Item>,
}

impl Home {
    pub fn new(number: u64, asks: u64, on: Machine, project: ProjectId) -> Home {
        Home {
            number,
            on,
            project,
            named: Named::Not { asks },
            listing: Listing::Asking,
            cursor: None,
            marked: BTreeSet::new(),
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

    pub fn is_of(&self, on: &Machine, project: ProjectId) -> bool {
        self.on == *on && self.project == project
    }

    pub fn ask_numbered(&mut self, number: u64) -> Option<&mut Ask> {
        match &mut self.named {
            Named::As { ask, .. } if ask.number() == number => Some(ask),
            Named::As { .. } | Named::Not { .. } => None,
        }
    }

    pub fn named(&mut self, name: &str, dir: &str) {
        match &mut self.named {
            Named::Not { asks } => {
                let ask = Ask::homed(*asks, &self.on, dir, name);
                self.named = Named::As { name: name.to_owned(), dir: dir.to_owned(), ask };
            },
            Named::As { name: named, dir: homed, ask } => {
                if (named.as_str(), homed.as_str()) != (name, dir) {
                    name.clone_into(named);
                    dir.clone_into(homed);
                    ask.rehome(&self.on, dir, name);
                }
            },
        }
    }

    pub fn known(&self) -> bool {
        matches!(self.named, Named::As { .. })
    }

    pub fn fetching(&self) -> bool {
        match self.listing {
            Listing::Asking | Listing::Came { next: Next::Asking, .. } => true,
            Listing::Came { next: Next::At(_), .. } => false,
        }
    }

    pub fn relist(&mut self, now: Instant) -> bool {
        let Listing::Came { next, .. } = &mut self.listing else {
            return false;
        };
        let due = matches!(*next, Next::At(at) if now >= at + AGAIN);
        if due {
            *next = Next::Asking;
        }
        due
    }

    pub fn due(&self) -> Option<Instant> {
        match self.listing {
            Listing::Came { next: Next::At(at), .. } => Some(at + AGAIN),
            Listing::Came { next: Next::Asking, .. } | Listing::Asking => None,
        }
    }

    pub fn listed(&mut self, listed: Result<Listed, String>, now: Instant) {
        let was = self.at(&self.items());
        let last = match std::mem::replace(&mut self.listing, Listing::Asking) {
            Listing::Asking => None,
            Listing::Came { shown, next: Next::Asking } => Some(shown),
            Listing::Came { next: Next::At(_), .. } => {
                unreachable!("a listing comes only when one was asked")
            },
        };
        let shown = match (listed, last) {
            (Ok(Listed::Work(work)), _) => Shown::Listed { work, stale: None },
            (Ok(Listed::Unhosted), _) => Shown::Unhosted,
            (Err(why), Some(Shown::Listed { work, .. })) => Shown::Listed { work, stale: Some(why) },
            (Err(why), None | Some(Shown::Unhosted | Shown::Unlisted(_))) => Shown::Unlisted(why),
        };
        self.listing = Listing::Came { shown, next: Next::At(now) };
        let items = self.items();
        self.marked.retain(|item| items.contains(item));
        let near = was.map_or(0, |was| was.min(items.len().saturating_sub(1)));
        let kept = self.cursor.filter(|item| items.contains(item));
        self.cursor = kept.or_else(|| items.get(near).copied());
        self.held_on(&items);
    }

    fn held_on(&self, items: &[Item]) {
        assert_eq!(self.cursor.is_some(), !items.is_empty(), "the cursor is on an item once any is listed");
        if let Some(cursor) = self.cursor {
            assert!(items.contains(&cursor), "the cursor is on a listed item");
        }
        assert!(self.marked.iter().all(|marked| items.contains(marked)), "only what is listed is marked");
    }

    fn at(&self, items: &[Item]) -> Option<usize> {
        self.cursor.and_then(|cursor| items.iter().position(|item| *item == cursor))
    }

    fn items(&self) -> Vec<Item> {
        let Listing::Came { shown: Shown::Listed { work, .. }, .. } = &self.listing else {
            return Vec::new();
        };
        let pulls = work.pulls().iter().map(|pull| Item::Pull(pull.number));
        pulls.chain(work.issues().iter().map(|issue| Item::Issue(issue.number))).collect()
    }

    pub fn key(&mut self, key: &KeyEvent, asks: &mut u64, events: &Sender<Event>) {
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
            _ => self.prompt(key, asks, events),
        }
    }

    fn prompt(&mut self, key: &KeyEvent, asks: &mut u64, events: &Sender<Event>) {
        let Named::As { name, dir, ask } = &mut self.named else {
            return;
        };
        match ask.key(key) {
            ask::Keyed::Nothing => {},
            ask::Keyed::Send => ask.send(marks(&self.marked), events),
            ask::Keyed::Close if ask.turning() => {
                *asks += 1;
                ask.stop(*asks);
            },
            ask::Keyed::Close => {
                *asks += 1;
                *ask = Ask::homed(*asks, &self.on, dir, name);
                self.marked.clear();
            },
        }
    }

    fn step_up(&mut self) {
        let items = self.items();
        let at = self.at(&items).map_or(0, |at| at.saturating_sub(1));
        self.cursor = items.get(at).copied();
        self.held_on(&items);
    }

    fn step_down(&mut self) {
        let items = self.items();
        let last = items.len().saturating_sub(1);
        let at = self.at(&items).map_or(0, |at| (at + 1).min(last));
        self.cursor = items.get(at).copied();
        self.held_on(&items);
    }

    fn mark(&mut self) {
        let Some(item) = self.cursor else {
            return;
        };
        assert!(self.items().contains(&item), "only a listed item is marked");
        if !self.marked.remove(&item) {
            self.marked.insert(item);
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Named::As { ask, .. } = &mut self.named {
            ask.paste(text);
        }
    }

    pub fn turning(&self) -> bool {
        matches!(&self.named, Named::As { ask, .. } if ask.turning())
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
        let Named::As { dir, ask, .. } = &self.named else {
            return self.draw_lists(buffer, area);
        };
        let most = (area.height / 2).max(ASK_ROWS_MIN).min(area.height);
        let room = usize::from(most.saturating_sub(3));
        let mut talk = ask.talked(inner, now);
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
            Machine::Local => sidebar::short_cwd(dir),
            Machine::Remote(on) => format!("{} on {on}", sidebar::short_cwd(dir)),
        };
        menu::boxed(buffer, asked, &menu::rule(ASK_TITLE, &right, inner), &talk);
        let lists = Rect::new(area.x, area.y, area.width, area.height - rows);
        self.draw_lists(buffer, lists);
    }

    fn hints(&self) -> Vec<(Hint, &'static str)> {
        if self.turning() {
            return vec![(Hint::Moves, "Move"), (Hint::Tab, "Mark"), (Hint::Esc, "Stop")];
        }
        vec![(Hint::Moves, "Move"), (Hint::Tab, "Mark"), (Hint::Enter, "Send"), (Hint::Esc, "Start over")]
    }

    fn draw_lists(&self, buffer: &mut Buffer, area: Rect) {
        let width = usize::from(area.width);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let head = match &self.named {
            Named::As { name, dir, .. } => Line::from(vec![
                Span::raw(" "),
                Span::styled(name.clone(), Style::new().fg(gruvbox::FG1).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", sidebar::short_cwd(dir)), dim),
            ]),
            Named::Not { .. } => Line::styled(" …", dim),
        };
        let mut head = vec![head];
        let (lines, at) = self.list_lines(width);
        if let Listing::Came { shown: Shown::Listed { stale: Some(why), .. }, .. } = &self.listing {
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
        let shown = match &self.listing {
            Listing::Asking => return (vec![Line::styled(format!("   {ASKING}"), dim)], None),
            Listing::Came { shown, .. } => shown,
        };
        let work = match shown {
            Shown::Unhosted => return (menu::wrap(&format!(" {UNHOSTED}"), width, dim), None),
            Shown::Unlisted(why) => {
                let said = format!(" gh lists nothing here: {why}");
                let red = Style::new().fg(gruvbox::RED);
                return (menu::wrap(&said, width, red), None);
            },
            Shown::Listed { work, .. } => work,
        };
        let numbered = |number: NonZeroU32| format!("#{number}");
        let numbers = work.issues().iter().map(|it| it.number).chain(work.pulls().iter().map(|it| it.number));
        let number_width = numbers.map(|number| numbered(number).len()).max().unwrap_or(0);
        let (mut lines, mut at) = (Vec::new(), None);
        let heading = |title: &str, count: usize, more: bool| {
            let more = if more { "+" } else { "" };
            Line::from(vec![
                Span::styled(format!(" {title}"), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" · {count}{more}"), dim),
            ])
        };
        lines.push(heading("Pull requests", work.pulls().len(), work.more().pulls));
        if work.pulls().is_empty() {
            lines.push(Line::styled("   no open pull requests", dim));
        }
        for pull in work.pulls() {
            let here = self.cursor == Some(Item::Pull(pull.number));
            at = at.or(here.then_some(lines.len()));
            let marked = self.marked.contains(&Item::Pull(pull.number));
            let side = if pull.draft { format!("{} · draft", pull.branch) } else { pull.branch.clone() };
            lines.push(row(width, (here, marked), (&numbered(pull.number), number_width), &pull.title, &side));
        }
        lines.push(Line::default());
        lines.push(heading("Issues", work.issues().len(), work.more().issues));
        if work.issues().is_empty() {
            lines.push(Line::styled("   no open issues", dim));
        }
        for issue in work.issues() {
            let here = self.cursor == Some(Item::Issue(issue.number));
            at = at.or(here.then_some(lines.len()));
            let marked = self.marked.contains(&Item::Issue(issue.number));
            let side = issue.labels.join(", ");
            lines.push(row(width, (here, marked), (&numbered(issue.number), number_width), &issue.title, &side));
        }
        (lines, at)
    }
}

#[derive(Debug, Default)]
pub struct Kept {
    homes: Vec<Home>,
}

impl Kept {
    pub fn take(&mut self, on: &Machine, project: ProjectId) -> Option<Home> {
        let at = self.homes.iter().position(|home| home.is_of(on, project))?;
        let taken = self.homes.remove(at);
        assert!(!self.homes.iter().any(|home| home.is_of(on, project)), "a project's home is kept once");
        Some(taken)
    }

    #[must_use]
    pub fn keep(&mut self, home: Home) -> Option<Home> {
        let of = |kept: &Home| kept.is_of(home.machine(), home.project());
        assert!(!self.homes.iter().any(of), "a project's home is kept once");
        assert!(self.homes.len() <= KEPT_MAX, "luneta keeps so many homes at most");
        let mut dropped = None;
        if self.homes.len() == KEPT_MAX {
            let idle = self.homes.iter().position(|kept| !kept.turning());
            dropped = Some(self.homes.remove(idle.unwrap_or(0)));
        }
        self.homes.push(home);
        assert!(self.homes.len() <= KEPT_MAX, "luneta keeps so many homes at most");
        dropped
    }

    pub fn forget(&mut self, gone: impl Fn(&Home) -> bool) {
        self.homes.retain(|home| !gone(home));
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Home> {
        self.homes.iter_mut()
    }
}

fn marks(marked: &BTreeSet<Item>) -> Marked {
    let numbers = |issues: bool| {
        let each = marked.iter().filter_map(move |item| match (item, issues) {
            (Item::Issue(number), true) | (Item::Pull(number), false) => Some(*number),
            (Item::Issue(_), false) | (Item::Pull(_), true) => None,
        });
        each.collect::<Vec<_>>()
    };
    let marked = Marked::new(numbers(true), numbers(false));
    marked.expect("only what is listed is marked, and gh lists only so many")
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
    use std::sync::mpsc;

    use lunetad::work::{Issue, More, Pull};

    use super::*;

    fn number(at: u32) -> NonZeroU32 {
        NonZeroU32::new(at).unwrap()
    }

    fn work(issues: &[(u32, &str, &[&str])], pulls: &[(u32, &str, &str, bool)]) -> Listed {
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
        Listed::Work(Work::new(issues.collect(), pulls.collect(), More::default()).unwrap())
    }

    fn listed() -> Listed {
        let issues: [(u32, &str, &[&str]); 2] =
            [(507, "Open a project on a home view", &["enhancement"]), (98, "Menu says no", &[])];
        work(&issues, &[(524, "Say view, not pane", "issue-511", true)])
    }

    fn home() -> Home {
        let mut home = Home::new(1, 1, Machine::Local, ProjectId::of(3).unwrap());
        home.named("luneta", "/w/luneta");
        home
    }

    fn press(home: &mut Home, code: KeyCode) {
        let (events, _heard) = mpsc::channel();
        let mut asks = 1;
        home.key(&KeyEvent::from(code), &mut asks, &events);
    }

    fn start_over(home: &mut Home, asks: &mut u64) {
        let (events, _heard) = mpsc::channel();
        home.key(&KeyEvent::from(KeyCode::Esc), asks, &events);
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
        assert!(!home.relist(now), "a new home is asking already");
        assert_eq!(shown(&home, 60, 12)[2], "   asking gh for the open issues and pull requests…");
        home.listed(Ok(listed()), now);
        press(&mut home, KeyCode::Tab);
        press(&mut home, KeyCode::Tab);
        assert_eq!(marks(&home.marked), Marked::new([number(507)], [number(524)]).unwrap());
        press(&mut home, KeyCode::Up);
        press(&mut home, KeyCode::BackTab);
        assert_eq!(marks(&home.marked), Marked::new([], [number(524)]).unwrap(), "a second mark takes it off");
        press(&mut home, KeyCode::Down);
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
            press(&mut home, KeyCode::Char(typed));
        }
        let typed = "│ › go_                                                    │";
        assert_eq!(shown(&home, 60, 14)[11], typed, "the rest of the keys are the prompt's");
    }

    #[test]
    fn the_work_is_asked_once_at_a_time_and_again_a_minute_after_it_came() {
        let (mut home, now) = (home(), Instant::now());
        assert_eq!(home.due(), None);
        assert!(home.fetching(), "a new home is fetched for");
        assert!(!home.relist(now + AGAIN), "a new home asks no more while its first ask is out");
        home.listed(Ok(listed()), now);
        assert!(!home.fetching());
        assert_eq!(home.due(), Some(now + AGAIN));
        assert!(!home.relist(now + AGAIN.saturating_sub(Duration::from_millis(1))));
        assert!(home.relist(now + AGAIN));
        assert!(home.fetching(), "a home due again is fetched for");
        assert_eq!(home.due(), None);
        assert!(!home.relist(now + AGAIN * 2), "and not again while it is asked");
    }

    #[test]
    fn a_listing_that_fails_keeps_the_last_and_one_that_comes_drops_the_marks_of_what_went() {
        let (mut home, now) = (home(), Instant::now());
        home.listed(Err("gh is not on the PATH".to_owned()), now);
        assert_eq!(shown(&home, 60, 10)[2], " gh lists nothing here: gh is not on the PATH");
        home.relist(now + AGAIN);
        home.listed(Ok(listed()), now + AGAIN);
        press(&mut home, KeyCode::Tab);
        press(&mut home, KeyCode::Tab);
        assert_eq!(marks(&home.marked), Marked::new([number(507)], [number(524)]).unwrap());
        home.relist(now + AGAIN * 2);
        home.listed(Err("HTTP 502".to_owned()), now + AGAIN * 2);
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[1], " not listed again: HTTP 502");
        assert!(lines[4].contains("#524"), "{lines:?}");
        home.relist(now + AGAIN * 3);
        let issues: [(u32, &str, &[&str]); 1] = [(98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &[(524, "Say view", "issue-511", false)])), now + AGAIN * 3);
        assert_eq!(marks(&home.marked), Marked::new([], [number(524)]).unwrap(), "#507 was closed");
        assert_eq!(home.cursor, Some(Item::Issue(number(98))), "the cursor stays on its row");
        press(&mut home, KeyCode::Down);
        assert_eq!(home.cursor, Some(Item::Issue(number(98))), "and no further than the last");
    }

    #[test]
    fn a_new_pull_request_on_top_leaves_the_cursor_on_its_item_and_one_that_closed_gives_its_place() {
        let (mut home, now) = (home(), Instant::now());
        home.listed(Ok(listed()), now);
        press(&mut home, KeyCode::Down);
        assert_eq!(home.cursor, Some(Item::Issue(number(507))));
        home.relist(now + AGAIN);
        let pulls = [(545, "Fresh", "issue-544", false), (524, "Say view, not pane", "issue-511", true)];
        let issues: [(u32, &str, &[&str]); 2] = [(507, "Open a project", &[]), (98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &pulls)), now + AGAIN);
        assert_eq!(home.cursor, Some(Item::Issue(number(507))), "a row above came, and the cursor kept #507");
        press(&mut home, KeyCode::Tab);
        assert_eq!(marks(&home.marked), Marked::new([number(507)], []).unwrap());
        assert_eq!(home.cursor, Some(Item::Issue(number(98))));
        press(&mut home, KeyCode::Up);
        home.relist(now + AGAIN * 2);
        let issues: [(u32, &str, &[&str]); 1] = [(98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &pulls)), now + AGAIN * 2);
        assert_eq!(home.cursor, Some(Item::Issue(number(98))), "#507 closed, and #98 took its place");
        assert_eq!(marks(&home.marked), Marked::default());
    }

    #[test]
    fn a_full_list_says_there_are_more_and_a_project_off_github_says_it_lists_nothing() {
        let (mut home, now) = (home(), Instant::now());
        let issues = (1..=50).map(|at| Issue { number: number(at), title: "many".to_owned(), labels: vec![] });
        let more = More { issues: true, pulls: false };
        let full = Work::new(issues.collect(), vec![], more).unwrap();
        home.listed(Ok(Listed::Work(full)), now);
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[2], " Pull requests · 0");
        assert_eq!(lines[5], " Issues · 50+");
        home.relist(now + AGAIN);
        home.listed(Ok(Listed::Unhosted), now + AGAIN);
        assert_eq!(home.cursor, None, "nothing is listed to be on");
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[2], " no remote on GitHub, so nothing is listed here");
        press(&mut home, KeyCode::Tab);
        assert_eq!(marks(&home.marked), Marked::default());
        assert_eq!(home.due(), Some(now + AGAIN * 2), "it asks again, as a remote may be added");
        home.relist(now + AGAIN * 2);
        home.listed(Err("git did not say the remotes".to_owned()), now + AGAIN * 2);
        assert_eq!(shown(&home, 60, 14)[2], " gh lists nothing here: git did not say the remotes");
    }

    #[test]
    fn a_home_takes_no_prompt_until_it_knows_its_project_s_folder() {
        let mut home = Home::new(1, 1, Machine::Local, ProjectId::of(3).unwrap());
        assert_eq!((home.known(), home.ask_numbered(1).is_some()), (false, false));
        for typed in "go".chars() {
            press(&mut home, KeyCode::Char(typed));
        }
        press(&mut home, KeyCode::Enter);
        let unnamed = shown(&home, 60, 14);
        assert_eq!(unnamed[..3], [" …", "", "   asking gh for the open issues and pull requests…"]);
        assert!(unnamed[3..].iter().all(String::is_empty), "and no prompt shows: {unnamed:?}");
        home.named("luneta", "/w/luneta");
        assert_eq!((home.known(), home.ask_numbered(1).is_some()), (true, true));
        assert!(home.ask_numbered(2).is_none(), "a prompt is heard only by its own number");
        let named = shown(&home, 60, 14);
        assert!(named.contains(&"│ › _                                                      │".to_owned()), "{named:?}");
    }

    #[test]
    fn a_home_taken_back_never_makes_room_and_the_oldest_idle_one_does() {
        let of = |id: u64| ProjectId::of(id).unwrap();
        let home_of = |id: u64| Home::new(id, id, Machine::Local, of(id));
        let mut kept = Kept::default();
        for id in 1..=u64::try_from(KEPT_MAX).unwrap() {
            assert!(kept.keep(home_of(id)).is_none(), "room is left up to the most");
        }
        let taken = kept.take(&Machine::Local, of(1));
        assert_eq!(taken.map(|home| home.number()), Some(1), "the oldest is taken back, not dropped");
        assert!(kept.keep(home_of(17)).is_none(), "the one taken back made room");
        let dropped = kept.keep(home_of(18));
        assert_eq!(dropped.map(|home| home.number()), Some(2), "the oldest idle home makes room");
        assert!(kept.take(&Machine::Local, of(2)).is_none());
        let far = Machine::Remote("nixos".parse().unwrap());
        assert!(kept.take(&far, of(3)).is_none(), "a home is of its machine as well as its project");
        kept.forget(|home| home.project() == of(3));
        assert!(kept.take(&Machine::Local, of(3)).is_none(), "a forgotten home is kept no more");
        assert_eq!(kept.take(&Machine::Local, of(4)).map(|home| home.number()), Some(4));
    }

    #[test]
    fn esc_starts_the_talk_over_and_forgets_the_marks() {
        let (mut home, now) = (home(), Instant::now());
        home.listed(Ok(listed()), now);
        press(&mut home, KeyCode::Tab);
        press(&mut home, KeyCode::Char('x'));
        let mut asks = 1;
        start_over(&mut home, &mut asks);
        assert_eq!(asks, 2, "a new talk takes the next number");
        assert_eq!(marks(&home.marked), Marked::default());
        assert!(home.ask_numbered(1).is_none() && home.ask_numbered(2).is_some());
        assert!(shown(&home, 60, 14).contains(&"│ › _                                                      │".to_owned()));
    }
}
