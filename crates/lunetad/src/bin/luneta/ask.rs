use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::asks::{self, Asked, Marked, Step, Turn};
use lunetad::transcripts::SessionId;
use lunetad::{Destination, Directory};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::away::{self, Away};
use crate::brief::{self, Asking};
use crate::due::Due;
use crate::gruvbox;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Cwd, Hint, ViewRow, ProjectTarget, Target};
use crate::sidebar::{self, ACCENT};

const TITLE: &str = "Ask";

const STOPPED: &str = "stopped: the agent was ended where it was";

#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    Send,
    Close,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    Step(Step),
    Ended(Result<(), String>),
}

#[derive(Debug, PartialEq, Eq)]
enum Said {
    You(String),
    Ran { id: String, command: String, denied: bool },
    Agent(String),
    Wrong(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Works {
    Here(PathBuf),
    There { on: Destination, cwd: Option<Directory> },
    Unread { why: String },
}

#[derive(Debug)]
pub struct Ask {
    number: u64,
    works: Works,
    highlighted: String,
    session: Option<SessionId>,
    turned: Option<u64>,
    talk: Vec<Said>,
    typed: String,
    turn: Option<Asking>,
}

impl Ask {
    pub fn new(number: u64, target: &Target, dir: &Path) -> Self {
        let works = match (target, target.machine()) {
            (Target::View(_, ViewRow { view, cwd: Cwd::Unread { why }, .. }), _) => {
                Works::Unread { why: format!("where view {view} works, {why}") }
            },
            (_, Machine::Local) => {
                let cwd = target.cwd().map(|cwd| PathBuf::from(cwd.as_str()));
                Works::Here(cwd.unwrap_or_else(|| dir.to_owned()))
            },
            (_, Machine::Remote(on)) => {
                Works::There { on: on.clone(), cwd: target.cwd().cloned() }
            },
        };
        Ask {
            number,
            works,
            highlighted: highlighted(target),
            session: None,
            turned: None,
            talk: Vec::new(),
            typed: String::new(),
            turn: None,
        }
    }

    pub fn homed(number: u64, on: &Machine, dir: &str, project: &str) -> Self {
        let works = match (on, Directory::parse(dir.to_owned())) {
            (_, Err(why)) => Works::Unread { why: format!("where project {project} is, {why}") },
            (Machine::Local, Ok(dir)) => Works::Here(PathBuf::from(dir.as_str())),
            (Machine::Remote(on), Ok(dir)) => Works::There { on: on.clone(), cwd: Some(dir) },
        };
        let highlighted = format!(
            "The user asks from the work view of project {project}, in {dir}, which lists the \
             project's open issues and pull requests on GitHub: \"this project\" means it, and \
             you run in its folder."
        );
        Ask {
            number,
            works,
            highlighted,
            session: None,
            turned: None,
            talk: Vec::new(),
            typed: String::new(),
            turn: None,
        }
    }

    pub fn rehome(&mut self, on: &Machine, dir: &str, project: &str) {
        if self.session.is_some() {
            return;
        }
        if self.turn.is_some() {
            return;
        }
        let Ask { works, highlighted, .. } = Ask::homed(self.number, on, dir, project);
        self.works = works;
        self.highlighted = highlighted;
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn key(&mut self, key: &KeyEvent) -> Keyed {
        let bare = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let control = key.modifiers == KeyModifiers::CONTROL;
        match key.code {
            KeyCode::Esc if bare => return Keyed::Close,
            KeyCode::Char('c') if control => return Keyed::Close,
            _ if self.turn.is_some() => return Keyed::Nothing,
            KeyCode::Enter if bare && !self.typed.trim().is_empty() => return Keyed::Send,
            KeyCode::Backspace if bare => {
                self.typed.pop();
            },
            KeyCode::Char('u') if control => self.typed.clear(),
            KeyCode::Char(c) if bare => self.typed.push(c),
            _ => {},
        }
        Keyed::Nothing
    }

    pub fn paste(&mut self, text: &str) {
        if self.turn.is_none() {
            self.typed.push_str(&plain(text));
        }
    }

    pub fn dictated(&mut self, text: &str) {
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
        let placed = match &self.works {
            Works::Here(cwd) => Ok((None, Some(cwd.clone()))),
            Works::There { on, cwd } => {
                Ok((Some(on.clone()), cwd.as_ref().map(|cwd| PathBuf::from(cwd.as_str()))))
            },
            Works::Unread { why } => Err(why.clone()),
        };
        let (resumed, highlighted) = (self.session.clone(), self.highlighted.clone());
        let asked = placed.and_then(|(on, cwd)| {
            let asked = Asked::new(cwd, &words, resumed, highlighted, turn)?;
            Ok((on, asked.marking(marked)))
        });
        self.talk.push(Said::You(words));
        let (on, asked) = match asked {
            Ok(asked) => asked,
            Err(why) => return self.talk.push(Said::Wrong(format!("ask: {why}"))),
        };
        self.turned = Some(turn.key());
        let (number, events) = (self.number, events.clone());
        let due = Due::new(&events, move |ended: Result<Result<(), String>, String>| {
            let ended = ended.and_then(|ended| ended).map_err(|why| format!("ask: {why}"));
            Event::Heard { number, heard: Heard::Ended(ended) }
        });
        self.turn = Some(Asking::on_thread(move |wanted| {
            let hear = |step| {
                let _ = events.send(Event::Heard { number, heard: Heard::Step(step) });
            };
            let ended = match on {
                None => asked.talk(wanted, &hear),
                Some(on) => match Away::reach(&on, away::ANSWER) {
                    Ok(mut away) => away.talk(&asked, wanted, &hear),
                    Err(why) => Some(Err(format!("{on}: {why}"))),
                },
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
                self.talk.push(Said::Ran { id, command, denied: false });
            },
            Heard::Step(Step::Denied(id)) => {
                for said in &mut self.talk {
                    if let Said::Ran { id: ran, denied, .. } = said
                        && *ran == id
                    {
                        *denied = true;
                    }
                }
            },
            Heard::Step(Step::Said(said)) => self.talk.push(Said::Agent(said)),
            Heard::Ended(Ok(())) => self.turn = None,
            Heard::Ended(Err(why)) => {
                self.talk.push(Said::Wrong(why));
                self.turn = None;
            },
        }
    }

    pub fn turning(&self) -> bool {
        self.turn.is_some()
    }

    pub fn stop(&mut self, number: u64) {
        assert!(number > self.number, "a stopped talk is numbered past its turn's");
        if self.turn.take().is_some() {
            self.talk.push(Said::Wrong(STOPPED.to_owned()));
        }
        self.number = number;
    }

    pub fn talked(&self, inner: usize, now: SystemTime) -> Vec<Line<'static>> {
        self.lines(inner, sidebar::frame(now))
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) {
        let Some((inner, room)) = brief::room(over) else {
            return;
        };
        let mut lines = self.lines(inner, sidebar::frame(now));
        let keys: &[(Hint, &str)] = match self.turn {
            Some(_) => &[(Hint::Esc, "Close")],
            None => &[(Hint::Enter, "Send"), (Hint::Alt('r'), "Dictate"), (Hint::Esc, "Close")],
        };
        let footer = [Line::default(), menu::keys_line(inner, keys)];
        let lines_room = room.saturating_sub(footer.len());
        if lines.len() > lines_room {
            let kept = lines_room.saturating_sub(1);
            lines.drain(..lines.len() - kept);
            if lines_room > 0 {
                lines.insert(0, Line::styled(" …", Style::new().add_modifier(Modifier::DIM)));
            }
        }
        lines.extend(footer);
        let right = match &self.works {
            Works::Here(cwd) => sidebar::short_cwd(&cwd.to_string_lossy()),
            Works::There { on, cwd: Some(cwd) } => {
                format!("{} on {on}", sidebar::short_cwd(cwd.as_str()))
            },
            Works::There { on, cwd: None } => format!("~ on {on}"),
            Works::Unread { .. } => String::new(),
        };
        brief::boxed(buffer, over, TITLE, &right, &lines);
    }

    fn lines(&self, inner: usize, frame: usize) -> Vec<Line<'static>> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let yours = Style::new().fg(ACCENT);
        let mut lines = Vec::new();
        for (at, said) in self.talk.iter().enumerate() {
            match said {
                Said::You(words) => {
                    if at > 0 {
                        lines.push(Line::default());
                    }
                    lines.extend(menu::wrap(&format!("› {words}"), inner, yours));
                },
                Said::Ran { command, denied, .. } => {
                    let command = sidebar::cut(&plain(command), inner.saturating_sub(5));
                    lines.push(match denied {
                        false => Line::styled(format!("   $ {command}"), dim),
                        true => Line::styled(format!("   ✗ {command}"), Style::new().fg(gruvbox::RED)),
                    });
                },
                Said::Agent(said) => lines.extend(brief::paragraphs(said, inner, Style::new())),
                Said::Wrong(why) => {
                    lines.extend(brief::paragraphs(why, inner, Style::new().fg(gruvbox::RED)));
                },
            }
        }
        if self.turn.is_some() {
            lines.extend(menu::waiting(&format!("Asking {}…", asks::MODEL), inner, dim, frame));
            return lines;
        }
        if !self.talk.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(menu::wrap(&format!("› {}_", self.typed), inner, yours));
        lines
    }
}

fn plain(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn highlighted(target: &Target) -> String {
    let row = match target {
        Target::Project(_, ProjectTarget { project, .. }) => {
            format!("project {}", project.listed().name())
        },
        Target::Worktree(_, row) => {
            format!("worktree {} of project {}", row.name, row.place.project.listed().name())
        },
        Target::View(_, ViewRow { session, view, .. }) => format!("view {}:{view}", session.name()),
        Target::Machine(_) => "the header of this machine".to_owned(),
    };
    let cwd = target.cwd().map_or_default(|cwd| format!(", in {cwd}"));
    let branch = target.branch().map(|branch| format!(", on branch {branch}"));
    let row = format!("{row}{cwd}{}", branch.unwrap_or_default());
    format!(
        "The row highlighted in luneta's sidebar as they opened the box is {row}: \"this\" or \
         \"that one\" may mean it."
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::menu::{ViewRow, Ran};
    use crate::sidebar::{Listed, ViewKey, ProjectRow};

    fn listed() -> Listed {
        Listed::new_project("luneta", 3)
    }

    fn project_on(on: Machine) -> Target {
        let project = ProjectRow::of(Listed::new_project("luneta", 3));
        Target::Project(on, ProjectTarget { project, dir: "/w/luneta".to_owned() })
    }

    fn nixos() -> Machine {
        Machine::Remote("nixos".parse().unwrap())
    }

    fn view_row_on(machine: Machine) -> Target {
        let row = ViewRow {
            session: listed(),
            view: ViewKey::terminal(4),
            terminal: None,
            cwd: Cwd::of(Some("/w/luneta/main".to_owned())),
            branch: Some("feat/ask".to_owned()),
            repository: None,
            pull_request: None,
            ran: Ran::Unhooked,
        };
        Target::View(machine, row)
    }

    fn view_row() -> Target {
        view_row_on(Machine::Local)
    }

    fn unread_on(machine: Machine) -> Target {
        let Target::View(on, row) = view_row_on(machine) else { unreachable!("a view's row") };
        Target::View(on, ViewRow { cwd: Cwd::of(Some("w".to_owned())), ..row })
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(ask: &mut Ask, text: &str) {
        for c in text.chars() {
            assert_eq!(ask.key(&key(KeyCode::Char(c))), Keyed::Nothing);
        }
    }

    fn shown(ask: &Ask, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        ask.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
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
            shown(&ask, 50, 7)[1..6],
            [
                "┌─ Ask ──────────────────────────── luneta/main ─┐",
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
        let ran = |id: &str, command: &str| Step::Ran { id: id.to_owned(), command: command.to_owned() };
        ask.heard(Heard::Step(ran("toolu_1", "luneta close bipa\nelo")));
        ask.heard(Heard::Step(ran("toolu_2", "touch elo")));
        ask.heard(Heard::Step(Step::Said("This kills bipa and elo.\n\nClose them?".to_owned())));
        assert_eq!(ask.session, Some(session));
        assert_eq!(ask.key(&key(KeyCode::Char('y'))), Keyed::Nothing);
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Nothing);
        ask.paste("y");
        assert_eq!(ask.typed, "");
        let mut asked = [
            "┌─ Ask ──────────────────────────── luneta/main ─┐",
            "│ › close bipa and elo                           │",
            "│   $ luneta close bipa elo                      │",
            "│   $ touch elo                                  │",
            "│ This kills bipa and elo.                       │",
            "│                                                │",
            "│ Close them?                                    │",
        ];
        let lines = shown(&ask, 50, 13);
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
        let lines = shown(&ask, 50, 12);
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
        ask.heard(Heard::Ended(Err("ask: claude failed: no such session".to_owned())));
        let lines = shown(&ask, 50, 15);
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
        assert_eq!(ask.key(&interrupt), Keyed::Close);
    }

    #[test]
    fn a_talk_too_long_for_the_box_loses_its_start() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::You("tackle these".to_owned()));
        ask.talk.extend((1..=6).map(|n| Said::Ran {
            id: format!("toolu_{n}"),
            command: format!("luneta open tab {n}"),
            denied: false,
        }));
        ask.talk.push(Said::Agent("Opened six tabs.".to_owned()));
        assert_eq!(
            shown(&ask, 40, 9),
            [
                "┌─ Ask ────────────────── luneta/main ─┐",
                "│ …                                    │",
                "│   $ luneta open tab 6                │",
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
        let top = |ask: &Ask| shown(ask, 40, 7)[1].clone();
        let view = Ask::new(1, &view_row(), Path::new("/w"));
        assert_eq!(view.works, Works::Here(PathBuf::from("/w/luneta/main")));
        let row = "is view luneta:4, in /w/luneta/main, on branch feat/ask: \"this\"";
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&view), "┌─ Ask ────────────────── luneta/main ─┐");
        let session = Ask::new(2, &project_on(Machine::Local), Path::new("/w"));
        assert_eq!(session.works, Works::Here(PathBuf::from("/w")));
        let session_row = "is project luneta: \"this\"";
        assert!(session.highlighted.contains(session_row), "{}", session.highlighted);

        let there = |cwd: Option<&str>| {
            let cwd = cwd.map(|cwd| Directory::parse(cwd.to_owned()).unwrap());
            Works::There { on: "nixos".parse().unwrap(), cwd }
        };
        let view = Ask::new(3, &view_row_on(nixos()), Path::new("/w"));
        assert_eq!(view.works, there(Some("/w/luneta/main")));
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&view), "┌─ Ask ───────── luneta/main on nixos ─┐");
        let session = Ask::new(4, &project_on(nixos()), Path::new("/w"));
        assert_eq!(session.works, there(None));
        assert_eq!(top(&session), "┌─ Ask ─────────────────── ~ on nixos ─┐");
        let header = Ask::new(5, &Target::Machine(nixos()), Path::new("/w"));
        assert_eq!(header.works, there(None));
        let row = "is the header of this machine: \"this\"";
        assert!(header.highlighted.contains(row), "{}", header.highlighted);

        let why = "where view 4 works, \"w\" is no absolute path".to_owned();
        for on in [Machine::Local, nixos()] {
            let unread = Ask::new(6, &unread_on(on), Path::new("/w"));
            assert_eq!(unread.works, Works::Unread { why: why.clone() });
            assert_eq!(top(&unread), "┌─ Ask ────────────────────────────────┐");
        }

        let home = |on: &Machine, dir: &str| Ask::homed(7, on, dir, "luneta").works;
        assert_eq!(home(&Machine::Local, "/w/luneta"), Works::Here(PathBuf::from("/w/luneta")));
        assert_eq!(home(&nixos(), "/w/luneta"), there(Some("/w/luneta")));
        let why = "where project luneta is, \"w\" is no absolute path".to_owned();
        for on in [Machine::Local, nixos()] {
            assert_eq!(home(&on, "w"), Works::Unread { why: why.clone() });
        }
    }

    #[test]
    fn a_stopped_turn_ends_and_its_talk_goes_on_under_a_new_number() {
        let mut ask = Ask::new(1, &view_row(), Path::new("/w"));
        ask.talk.push(Said::You("tackle these".to_owned()));
        ask.turn = Some(at_a_turn());
        let session = SessionId::parse("7c22").unwrap();
        ask.heard(Heard::Step(Step::Session(session.clone())));
        ask.stop(2);
        assert!(!ask.turning());
        assert_eq!((ask.number(), ask.session.clone()), (2, Some(session)), "the next turn resumes it");
        let stopped = [Said::You("tackle these".to_owned()), Said::Wrong(STOPPED.to_owned())];
        assert_eq!(ask.talk, stopped);
        ask.stop(3);
        assert_eq!((ask.number(), ask.talk.len()), (3, 2), "a stop with no turn says nothing");
    }

    #[test]
    fn a_turn_that_is_none_says_why() {
        let mut ask = Ask::new(1, &unread_on(Machine::Local), Path::new("/w"));
        typed(&mut ask, "close it");
        let (events, _heard) = mpsc::channel();
        ask.send(Marked::default(), &events);
        let wrong = "ask: where view 4 works, \"w\" is no absolute path".to_owned();
        assert_eq!(ask.talk, [Said::You("close it".to_owned()), Said::Wrong(wrong)]);
        assert!(!ask.turning());
        assert_eq!(ask.turned, None);
    }
}
