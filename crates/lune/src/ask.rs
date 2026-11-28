use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lune_diff::SelectionExcerpt;
use lune_domain::projects::Dir;
use luned::asks::{self, Asked, Marked, Step, Turn};
use luned::transcripts::SessionId;
use luned::{Destination, Directory};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::brief::{self, Asking};
use crate::cadence::Cadence;
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint, ProjectTarget, Target, ViewRow};
use crate::moonlight;
use crate::reach::{self, Luned};
use crate::sidebar::{self, ACCENT};

const TITLE: &str = "Ask";

const STOPPED: &str = "stopped: the agent was ended where it was";

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

#[derive(Debug)]
pub struct Ask {
    number: u64,
    works: Works,
    highlighted: String,
    context: Option<String>,
    review_only: bool,
    session: Option<SessionId>,
    turned: Option<u64>,
    talk: Vec<Said>,
    typed: String,
    turn: Option<Asking>,
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
            talk: Vec::new(),
            typed: String::new(),
            turn: None,
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
            talk: Vec::new(),
            typed: String::new(),
            turn: None,
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
            talk: Vec::new(),
            typed: initial.unwrap_or("").to_owned(),
            turn: None,
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
            Heard::Step(Step::Denied(id)) => {
                for said in &mut self.talk {
                    if let Said::Ran {
                        id: ran, denied, ..
                    } = said
                        && *ran == id
                    {
                        *denied = true;
                    }
                }
            }
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
        if self.turn.take().is_some() {
            self.talk.push(Said::Wrong(STOPPED.to_owned()));
            self.turned = None;
        }
        self.number = number;
    }

    pub fn talked(&self, inner: usize, wall: SystemTime) -> (Vec<Line<'static>>, Option<usize>) {
        self.lines(inner, sidebar::frame(wall))
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, wall: SystemTime) -> Cadence {
        let mut cadence = Cadence::default();
        let Some((inner, room)) = brief::room(over) else {
            return cadence;
        };
        let (mut lines, spinner) = self.lines(inner, sidebar::frame(wall));
        let keys: &[(Hint, &str)] = match self.turn {
            Some(_) => &[(Hint::Esc, "Close")],
            None => &[
                (Hint::Enter, "Send"),
                (Hint::Alt('r'), "Dictate"),
                (Hint::Esc, "Close"),
            ],
        };
        let footer = [Line::default(), menu::keys_line(inner, keys)];
        let lines_room = room.saturating_sub(footer.len());
        let mut kept_spinner = inner > 0 && spinner.is_some_and(|at| at < lines_room);
        if lines.len() > lines_room {
            let kept = lines_room.saturating_sub(1);
            let drained = lines.len() - kept;
            kept_spinner = inner > 0 && spinner.is_some_and(|at| at >= drained);
            lines.drain(..drained);
            if lines_room > 0 {
                lines.insert(
                    0,
                    Line::styled(" …", Style::new().add_modifier(Modifier::DIM)),
                );
            }
        }
        lines.extend(footer);
        let right = match &self.works {
            Works::Here(cwd) => sidebar::short_cwd(&cwd.to_string_lossy()),
            Works::There { on, cwd: Some(cwd) } => {
                format!("{} on {on}", sidebar::short_cwd(cwd.as_str()))
            }
            Works::There { on, cwd: None } => format!("~ on {on}"),
        };
        brief::boxed(buffer, over, TITLE, &right, &lines);
        if kept_spinner {
            cadence.spin();
        }
        cadence
    }

    fn lines(&self, inner: usize, frame: usize) -> (Vec<Line<'static>>, Option<usize>) {
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
                }
                Said::Ran {
                    command, denied, ..
                } => {
                    let command = sidebar::cut(&plain(command), inner.saturating_sub(5));
                    lines.push(match denied {
                        false => Line::styled(format!("   $ {command}"), dim),
                        true => {
                            Line::styled(format!("   ✗ {command}"), Style::new().fg(moonlight::RED))
                        }
                    });
                }
                Said::Agent(said) => lines.extend(brief::paragraphs(said, inner, Style::new())),
                Said::Wrong(why) => {
                    lines.extend(brief::paragraphs(
                        why,
                        inner,
                        Style::new().fg(moonlight::RED),
                    ));
                }
            }
        }
        if self.turn.is_some() {
            let start = lines.len();
            lines.extend(menu::waiting(
                &format!("Asking {}…", asks::MODEL),
                inner,
                dim,
                frame,
            ));
            let spinner = (lines.len() > start).then_some(start);
            return (lines, spinner);
        }
        if !self.talk.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(menu::wrap(&format!("› {}_", self.typed), inner, yours));
        (lines, None)
    }
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
            flow: None,
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

    fn shown(ask: &Ask, width: u16, height: u16) -> Vec<String> {
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
        ask.heard(Heard::Ended(Err(
            "ask: claude failed: no such session".to_owned()
        )));
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
            shown(&ask, 40, 9),
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
        let top = |ask: &Ask| shown(ask, 40, 7)[1].clone();
        let view = Ask::new(1, &view_row(), Path::new("/w"));
        assert_eq!(view.works, Works::Here(PathBuf::from("/w/lune/main")));
        let row = "is view lune:4, in /w/lune/main, on branch feat/ask: \"this\"";
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&view), "┌─ Ask ──────────────────── lune/main ─┐");
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
        let view = Ask::new(3, &view_row_on(nixos()), Path::new("/w"));
        assert_eq!(view.works, there(Some("/w/lune/main")));
        assert!(view.highlighted.contains(row), "{}", view.highlighted);
        assert_eq!(top(&view), "┌─ Ask ─────────── lune/main on nixos ─┐");
        let session = Ask::new(4, &project_on(nixos()), Path::new("/w"));
        assert_eq!(session.works, there(None));
        assert_eq!(top(&session), "┌─ Ask ─────────────────── ~ on nixos ─┐");
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
        let paint = |width: u16, height: u16| {
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
}
