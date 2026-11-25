use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lune_domain::projects::{Dir, ProjectId};
use luned::asks::Marked;
use luned::work::{self, Issues, Item, Listed, Work};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::ask::{self, Ask};
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::moonlight;
use crate::sidebar::{self, ACCENT};

pub const AGAIN: Duration = Duration::from_secs(60);

pub const KEPT_MAX: usize = 16;

const _: () = assert!(work::FRESH.as_millis() < AGAIN.as_millis());

const ASK_TITLE: &str = "Ask";

const MARKED: &str = "◆";

const UNMARKED: &str = "◇";

const ASKING: &str = "asking gh for the open issues and pull requests…";

const UNHOSTED: &str = "no remote on GitHub, so nothing is listed here";

const ISSUES_OFF: &str = "   the repository has its issues turned off on GitHub";

const ASK_ROWS_MIN: u16 = 4;

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
    As { name: String, dir: Dir, ask: Ask },
}

#[derive(Debug)]
pub struct Home {
    number: u64,
    on: Machine,
    project: ProjectId,
    named: Named,
    listing: Listing,
    cursor: Option<Item>,
    marked: Marked,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub on: Machine,
    pub project: ProjectId,
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
            marked: Marked::default(),
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

    pub fn which(&self) -> Key {
        Key { on: self.on.clone(), project: self.project }
    }

    pub fn ask_numbered(&mut self, number: u64) -> Option<&mut Ask> {
        match &mut self.named {
            Named::As { ask, .. } if ask.number() == number => Some(ask),
            Named::As { .. } | Named::Not { .. } => None,
        }
    }

    pub fn named(&mut self, name: &str, dir: &Dir) {
        match &mut self.named {
            Named::Not { asks } => {
                let ask = Ask::homed(*asks, &self.on, dir, name);
                self.named = Named::As { name: name.to_owned(), dir: dir.clone(), ask };
            },
            Named::As { name: named, dir: homed, ask } => {
                if named != name || homed != dir {
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
        pulls.chain(work.issues().open().iter().map(|issue| Item::Issue(issue.number))).collect()
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
            ask::Keyed::Send => ask.send(self.marked.clone(), events),
            ask::Keyed::Close | ask::Keyed::Interrupt if ask.turning() => {
                *asks += 1;
                ask.stop(*asks);
            },
            ask::Keyed::Nothing | ask::Keyed::Interrupt => {},
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
        self.marked.toggle(item);
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
            Machine::Local => sidebar::short_cwd(dir.as_str()),
            Machine::Remote(on) => format!("{} on {on}", sidebar::short_cwd(dir.as_str())),
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
                Span::styled(name.clone(), Style::new().fg(moonlight::TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", sidebar::short_cwd(dir.as_str())), dim),
            ]),
            Named::Not { .. } => Line::styled(" …", dim),
        };
        let mut head = vec![head];
        let (lines, at) = self.list_lines(width);
        if let Listing::Came { shown: Shown::Listed { stale: Some(why), .. }, .. } = &self.listing {
            let why = format!(" not listed again: {why}");
            head.push(Line::styled(sidebar::cut(&why, width), Style::new().fg(moonlight::RED)));
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
                let red = Style::new().fg(moonlight::RED);
                return (menu::wrap(&said, width, red), None);
            },
            Shown::Listed { work, .. } => work,
        };
        let numbered = |number: NonZeroU32| format!("#{number}");
        let numbers = work.issues().open().iter().map(|it| it.number).chain(work.pulls().iter().map(|it| it.number));
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
        let issues = match work.issues() {
            Issues::Open(issues) => issues,
            Issues::Off => {
                lines.push(Line::styled(" Issues", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
                lines.push(Line::styled(ISSUES_OFF, dim));
                return (lines, at);
            },
        };
        lines.push(heading("Issues", issues.len(), work.more().issues));
        if issues.is_empty() {
            lines.push(Line::styled("   no open issues", dim));
        }
        for issue in issues {
            let here = self.cursor == Some(Item::Issue(issue.number));
            at = at.or(here.then_some(lines.len()));
            let marked = self.marked.contains(&Item::Issue(issue.number));
            let side = issue.labels.join(", ");
            lines.push(row(width, (here, marked), (&numbered(issue.number), number_width), &issue.title, &side));
        }
        (lines, at)
    }
}

#[derive(Debug)]
struct Held {
    shown: u64,
    home: Home,
}

#[derive(Debug, Default)]
pub struct Homes {
    held: HashMap<Key, Held>,
    shown: u64,
    made: u64,
}

impl Homes {
    #[must_use]
    pub fn show(&mut self, key: &Key, make: impl FnOnce(u64) -> Home) -> Option<Home> {
        self.shown += 1;
        if let Some(held) = self.held.get_mut(key) {
            held.shown = self.shown;
            return None;
        }
        assert!(self.held.len() <= KEPT_MAX, "lune keeps so many homes at most");
        let dropped = (self.held.len() == KEPT_MAX).then(|| self.make_room());
        self.made += 1;
        let home = make(self.made);
        assert_eq!(home.which(), *key, "a home is made for the project it is shown for");
        self.held.insert(key.clone(), Held { shown: self.shown, home });
        dropped
    }

    fn make_room(&mut self) -> Home {
        let oldest = |idle: bool| {
            let held = self.held.iter().filter(|(_, held)| !idle || !held.home.turning());
            held.min_by_key(|(_, held)| held.shown).map(|(key, _)| key.clone())
        };
        let oldest = oldest(true).or_else(|| oldest(false));
        let dropped = oldest.and_then(|key| self.held.remove(&key));
        dropped.expect("a full list of homes holds one to drop").home
    }

    pub fn shown(&self, key: &Key) -> &Home {
        let held = self.held.get(key).expect("a shown home is held");
        &held.home
    }

    pub fn shown_mut(&mut self, key: &Key) -> &mut Home {
        let held = self.held.get_mut(key).expect("a shown home is held");
        &mut held.home
    }

    pub fn forget(&mut self, gone: impl Fn(&Home) -> bool) {
        self.held.retain(|_, held| !gone(&held.home));
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Home> {
        self.held.values_mut().map(|held| &mut held.home)
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
    let number = Span::styled(format!(" {number:<number_width$}  "), Style::new().fg(moonlight::YELLOW));
    let lead = margin.width() + mark.width() + number.width();
    let room = width.saturating_sub(lead + 1);
    let side_room = if side.is_empty() { 0 } else { (side.width() + 2).min(room / 3) };
    let title_room = room.saturating_sub(side_room);
    let title = sidebar::cut(title, title_room);
    let gap = title_room.saturating_sub(title.width());
    let title_style = if here { Style::new().fg(moonlight::TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(moonlight::TEXT) };
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

    use luned::work::{Issue, More, Pull};

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
        Listed::Work(Work::new(Issues::Open(issues.collect()), pulls.collect(), More::default()).unwrap())
    }

    fn listed() -> Listed {
        let issues: [(u32, &str, &[&str]); 2] =
            [(507, "Open a project on a home view", &["enhancement"]), (98, "Menu says no", &[])];
        work(&issues, &[(524, "Say view, not pane", "issue-511", true)])
    }

    fn home() -> Home {
        let mut home = Home::new(1, 1, Machine::Local, ProjectId::of(3).unwrap());
        home.named("lune", &Dir::parse("/w/lune").unwrap());
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
            assert!(moonlight::ALL.contains(&cell.fg) || cell.fg == ratatui::style::Color::Reset);
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
        assert_eq!(home.marked, Marked::new([number(507)], [number(524)]).unwrap());
        press(&mut home, KeyCode::Up);
        press(&mut home, KeyCode::BackTab);
        assert_eq!(home.marked, Marked::new([], [number(524)]).unwrap(), "a second mark takes it off");
        press(&mut home, KeyCode::Down);
        assert_eq!(
            shown(&home, 60, 14)[..9],
            [
                " lune  w/lune",
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
                "┌─ Ask ─────────────────────────────────────────── w/lune ─┐",
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
        assert_eq!(home.marked, Marked::new([number(507)], [number(524)]).unwrap());
        home.relist(now + AGAIN * 2);
        home.listed(Err("HTTP 502".to_owned()), now + AGAIN * 2);
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[1], " not listed again: HTTP 502");
        assert!(lines[4].contains("#524"), "{lines:?}");
        home.relist(now + AGAIN * 3);
        let issues: [(u32, &str, &[&str]); 1] = [(98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &[(524, "Say view", "issue-511", false)])), now + AGAIN * 3);
        assert_eq!(home.marked, Marked::new([], [number(524)]).unwrap(), "#507 was closed");
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
        assert_eq!(home.marked, Marked::new([number(507)], []).unwrap());
        assert_eq!(home.cursor, Some(Item::Issue(number(98))));
        press(&mut home, KeyCode::Up);
        home.relist(now + AGAIN * 2);
        let issues: [(u32, &str, &[&str]); 1] = [(98, "Menu says no", &[])];
        home.listed(Ok(work(&issues, &pulls)), now + AGAIN * 2);
        assert_eq!(home.cursor, Some(Item::Issue(number(98))), "#507 closed, and #98 took its place");
        assert_eq!(home.marked, Marked::default());
    }

    #[test]
    fn a_repository_with_its_issues_off_lists_its_pull_requests_and_says_its_issues_are_off() {
        let (mut home, now) = (home(), Instant::now());
        let pull = Pull {
            number: number(524),
            title: "Say view, not pane".to_owned(),
            draft: false,
            branch: "issue-511".to_owned(),
            author: None,
        };
        home.listed(Ok(Listed::Work(Work::new(Issues::Off, vec![pull], More::default()).unwrap())), now);
        let lines = shown(&home, 60, 14);
        assert_eq!(lines[2], " Pull requests · 1");
        assert!(lines[3].starts_with(" › ◇ #524  Say view, not pane") && lines[3].ends_with("issue-511"), "{lines:?}");
        assert_eq!(lines[5..7], [" Issues", ISSUES_OFF]);
        press(&mut home, KeyCode::Tab);
        assert_eq!(home.marked, Marked::new([], [number(524)]).unwrap());
        press(&mut home, KeyCode::Down);
        assert_eq!(home.cursor, Some(Item::Pull(number(524))), "no issue is listed to go down to");
    }

    #[test]
    fn a_full_list_says_there_are_more_and_a_project_off_github_says_it_lists_nothing() {
        let (mut home, now) = (home(), Instant::now());
        let issues = (1..=50).map(|at| Issue { number: number(at), title: "many".to_owned(), labels: vec![] });
        let more = More { issues: true, pulls: false };
        let full = Work::new(Issues::Open(issues.collect()), vec![], more).unwrap();
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
        assert_eq!(home.marked, Marked::default());
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
        home.named("lune", &Dir::parse("/w/lune").unwrap());
        assert_eq!((home.known(), home.ask_numbered(1).is_some()), (true, true));
        assert!(home.ask_numbered(2).is_none(), "a prompt is heard only by its own number");
        let named = shown(&home, 60, 14);
        assert!(named.contains(&"│ › _                                                      │".to_owned()), "{named:?}");
    }

    #[test]
    fn a_home_shown_again_is_not_made_again_and_the_one_shown_longest_ago_makes_room() {
        let of = |id: u64| ProjectId::of(id).unwrap();
        let key = |id: u64| Key { on: Machine::Local, project: of(id) };
        let made = |id: u64| move |number| Home::new(number, number, Machine::Local, of(id));
        let mut homes = Homes::default();
        for id in 1..=u64::try_from(KEPT_MAX).unwrap() {
            assert!(homes.show(&key(id), made(id)).is_none(), "room is left up to the most");
        }
        let again = homes.show(&key(1), |_| panic!("a held home is shown again, not made"));
        assert!(again.is_none());
        assert_eq!(homes.shown(&key(1)).number(), 1);
        let dropped = homes.show(&key(17), made(17));
        assert_eq!(dropped.map(|home| home.number()), Some(2), "1 was shown since, so 2 is the oldest");
        assert_eq!(homes.shown(&key(17)).number(), 17);
        homes.forget(|home| home.project() == of(3));
        let far = Key { on: Machine::Remote("nixos".parse().unwrap()), project: of(3) };
        let dropped = homes.show(&far, |number| Home::new(number, number, far.on.clone(), far.project));
        assert!(dropped.is_none(), "the forgotten home made room");
        assert_eq!(homes.shown(&far).machine(), &far.on, "a home is of its machine as well as its project");
        assert_eq!(homes.iter_mut().count(), KEPT_MAX);
    }

    #[test]
    fn esc_starts_the_talk_over_and_forgets_the_marks_and_ctrl_c_with_no_turn_does_not() {
        let (mut home, now) = (home(), Instant::now());
        home.listed(Ok(listed()), now);
        press(&mut home, KeyCode::Tab);
        press(&mut home, KeyCode::Char('x'));
        let mut asks = 1;
        let (events, _heard) = mpsc::channel();
        home.key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), &mut asks, &events);
        assert_eq!((asks, home.marked.iter().count()), (1, 1), "Ctrl c stops a turn, and there is none");
        assert!(home.ask_numbered(1).is_some());
        assert!(shown(&home, 60, 14).contains(&"│ › x_                                                     │".to_owned()));
        start_over(&mut home, &mut asks);
        assert_eq!(asks, 2, "a new talk takes the next number");
        assert_eq!(home.marked, Marked::default());
        assert!(home.ask_numbered(1).is_none() && home.ask_numbered(2).is_some());
        assert!(shown(&home, 60, 14).contains(&"│ › _                                                      │".to_owned()));
    }
}
