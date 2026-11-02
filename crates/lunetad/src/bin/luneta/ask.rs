//! The ask box: what you want done, in words, for an agent to do with
//! luneta's commands, as the menu would: "make me a session to work on
//! floresta", "tackle these four issues in parallel", "close every session
//! but this one". `a` opens it over zellij, for the row highlighted then.
//!
//! What you type goes to `claude -p`, as `lunetad::asks` runs it, on the
//! highlighted row's machine: luneta runs it for a row of its own, and the
//! daemon of another machine for a row of that one's, through the daemon
//! luneta watches. The box shows what the agent runs, and what Claude Code
//! denied it, and what it says as it goes. Once it is done, what you type
//! next goes to it on the same Claude session, so that it can ask you
//! before it closes anything, as the skill has it ask, or which of two
//! things you meant. A close it runs takes a yes only in the turn after the
//! one that said its plan, so that your answer comes between them, as Claude
//! Code runs the close without asking you. Closing the box ends it, and
//! nothing is kept.
//!
//! The agent goes by where you are as each turn starts: the pane luneta's
//! zellij is on, where that is on the agent's machine, or another machine.
//! luneta's zellij is the last client to type in its pane as each turn
//! starts, so that `go` and `open session` move it.

use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::asks::{self, Asked, Step, Turn, User};
use lunetad::transcripts::SessionId;
use lunetad::Destination;
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
use crate::menu::{self, Hint, PaneRow, Target};
use crate::sidebar::{self, ACCENT};

/// What the rule along the box's top names it.
const TITLE: &str = "Ask";

/// What a key does to the box, for luneta to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    /// Hand what you typed to the agent, with `send`.
    Send,
    /// Close the box, which gives up the turn the agent is at.
    Close,
}

/// What a turn of the agent is heard to do: a step at a time, as `claude`
/// says it, and then that it ended, after which nothing more of it is heard.
#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    Step(Step),
    /// The turn ended: well, or why not.
    Ended(Result<(), String>),
}

/// A line of the talk, as the box shows it.
#[derive(Debug, PartialEq, Eq)]
enum Said {
    /// What you asked.
    You(String),
    /// A command the agent ran, or that Claude Code denied it, by the id of
    /// the call.
    Ran { id: String, command: String, denied: bool },
    /// What the agent said.
    Agent(String),
    /// Why a turn went wrong.
    Wrong(String),
}

/// Where the agent works.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Works {
    /// In a directory of this machine's: the highlighted row's pane's, or
    /// luneta's.
    Here(PathBuf),
    /// On another machine, in the highlighted row's pane's directory there,
    /// or else in the home of the user its daemon runs as.
    There { on: Destination, cwd: Option<String> },
}

/// The ask box over zellij, from when `a` opens it until a key closes it.
#[derive(Debug)]
pub struct Ask {
    /// Its number among the boxes luneta has opened, which what a turn hears
    /// carries.
    number: u64,
    /// Where the agent runs, each turn, as the box opened.
    works: Works,
    /// What the agent is told of the row highlighted as the box opened.
    highlighted: String,
    /// The Claude session the talk goes on on, once its first turn says.
    session: Option<SessionId>,
    /// The key of the last turn handed to the agent, which the next is told
    /// for the turn before it, for a close to take a yes to a plan it said.
    turned: Option<u64>,
    talk: Vec<Said>,
    /// What you are typing, for the next turn.
    typed: String,
    /// The turn the agent is at, while it is: dropped, it is given up.
    turn: Option<Asking>,
}

impl Ask {
    /// The box numbered `number`, for `target`, the row highlighted as it
    /// opens: the agent works on its machine, in its pane's directory, or on
    /// a session's row or a machine's header, in `dir`, luneta's, on this
    /// machine, and in the home of the user the daemon there runs as on
    /// another.
    pub fn new(number: u64, target: &Target, dir: &Path) -> Self {
        let works = match target.machine() {
            Machine::Local => {
                Works::Here(target.cwd().map_or_else(|| dir.to_owned(), PathBuf::from))
            },
            Machine::Remote(on) => {
                Works::There { on: on.clone(), cwd: target.cwd().map(str::to_owned) }
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

    pub fn number(&self) -> u64 {
        self.number
    }

    /// The machine the agent runs on.
    pub fn machine(&self) -> Machine {
        match &self.works {
            Works::Here(_) => Machine::Local,
            Works::There { on, .. } => Machine::Remote(on.clone()),
        }
    }

    /// What `key` does: `Esc` and `Ctrl c` close the box, whatever the agent
    /// is doing. While it is at a turn, no other key does anything. Else a
    /// character types, `Backspace` takes one back, `Ctrl u` takes back all
    /// of them, and `Enter` sends what you typed, if anything.
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

    /// Types `text`, pasted, on one line, but while the agent is at a turn.
    pub fn paste(&mut self, text: &str) {
        if self.turn.is_none() {
            self.typed.push_str(&plain(text));
        }
    }

    /// Types `text`, as you dictated it, on one line after what you typed, a
    /// space between them, but while the agent is at a turn.
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

    /// Hands what you typed to the agent, on a thread of its own, for you
    /// where `user` says, and takes it from what you are typing. What the
    /// turn hears comes back on `events`, and how it ended, as `Due` does.
    pub fn send(&mut self, user: User, events: &Sender<Event>) {
        if self.turn.is_some() {
            return;
        }
        let words = std::mem::take(&mut self.typed).trim().to_owned();
        if words.is_empty() {
            return;
        }
        let turn = self.next_turn();
        let (on, cwd) = match &self.works {
            Works::Here(cwd) => (None, Some(cwd.clone())),
            Works::There { on, cwd } => (Some(on.clone()), cwd.as_ref().map(PathBuf::from)),
        };
        let resumed = self.session.clone();
        let asked = Asked::new(cwd, &words, resumed, self.highlighted.clone(), user, turn);
        self.talk.push(Said::You(words));
        let asked = match asked {
            Ok(asked) => asked,
            // No turn went to the agent, for the next to come after.
            Err(why) => {
                self.turned = turn.before;
                return self.talk.push(Said::Wrong(format!("ask: {why}")));
            },
        };
        let (number, events) = (self.number, events.clone());
        // How the turn ended, or else that it ended with no answer.
        let due = Due::new(&events, move |ended: Result<Result<(), String>, String>| {
            let ended = ended.and_then(|ended| ended).map_err(|why| format!("ask: {why}"));
            Event::Heard { number, heard: Heard::Ended(ended) }
        });
        self.turn = Some(Asking::on_thread(move |wanted| {
            let hear = |step| {
                // luneta is on its way out.
                let _ = events.send(Event::Heard { number, heard: Heard::Step(step) });
            };
            let ended = match on {
                None => asked.talk(wanted, &hear),
                Some(on) => match Away::reach(&on, away::ANSWER) {
                    Ok(mut away) => away.talk(&asked, wanted, &hear),
                    Err(why) => Some(Err(format!("{on}: {why}"))),
                },
            };
            // Given up, it is owed to a box that is gone.
            if let Some(ended) = ended {
                due.pay(Ok(ended));
            }
        }));
    }

    /// The turn after the last, by a key of its own: one nothing the agent
    /// runs can tell, as std seeds the keys of its hashers at random.
    fn next_turn(&mut self) -> Turn {
        let key = RandomState::new().build_hasher().finish();
        Turn { key, before: self.turned.replace(key) }
    }

    /// Takes in what the turn the agent is at heard.
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

    /// Whether its spinner turns: while the agent is at a turn.
    pub fn turning(&self) -> bool {
        self.turn.is_some()
    }

    /// Draws it in a box over `over`, zellij's columns, at `now`, as
    /// `brief::boxed` does. The rule along its top says where the agent
    /// works, and on which machine, where it is another. Under it, the talk
    /// so far, and then what you type, or that the agent is at a turn, after
    /// a spinner that turns, and the keys. Where the box has no room for the
    /// talk, its start goes, its first line a `…`.
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
            Works::There { on, cwd: Some(cwd) } => format!("{} on {on}", sidebar::short_cwd(cwd)),
            Works::There { on, cwd: None } => format!("~ on {on}"),
        };
        brief::boxed(buffer, over, TITLE, &right, &lines);
    }

    /// The talk, on lines `inner` wide, and what you type under it, or that
    /// the agent is at a turn, after the spinner at `frame`. Each of your
    /// turns has a blank line before it. A command Claude Code denied the
    /// agent is in red, after a `✗`.
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

/// `text` on one line: each run of white space, and each character that would
/// move the cursor or tell the terminal anything, a space.
fn plain(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the agent is told of `target`, the row highlighted as the box
/// opened, for words that point at it: a row of the machine the agent runs
/// on.
fn highlighted(target: &Target) -> String {
    let row = match target {
        Target::Session(_, listed) => format!("session {}", listed.name()),
        Target::Pane(_, PaneRow { session, pane, .. }) => format!("pane {}:{pane}", session.name()),
        Target::Machine(_) => "the header of this machine".to_owned(),
    };
    let cwd = target.cwd().map(|cwd| format!(", in {cwd}")).unwrap_or_default();
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
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::menu::{PaneRow, Ran};
    use crate::sidebar::Listed;

    fn listed() -> Listed {
        Listed::new("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    fn nixos() -> Machine {
        Machine::Remote("nixos".parse().unwrap())
    }

    fn pane_row_on(machine: Machine) -> Target {
        let row = PaneRow {
            session: listed(),
            pane: 4,
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/ask".to_owned()),
            ran: Ran::Unhooked,
        };
        Target::Pane(machine, row)
    }

    fn pane_row() -> Target {
        pane_row_on(Machine::Local)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(ask: &mut Ask, text: &str) {
        for c in text.chars() {
            assert_eq!(ask.key(&key(KeyCode::Char(c))), Keyed::Nothing);
        }
    }

    /// The box `ask` over zellij's columns, `width` by `height`, as text.
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

    /// A turn the agent is at, on a thread that has already ended.
    fn at_a_turn() -> Asking {
        Asking::on_thread(|_| {})
    }

    /// What you dictate goes on the line after what you typed, a space
    /// between them, but while the agent is at a turn.
    #[test]
    fn what_you_dictate_goes_on_the_line_after_what_you_typed() {
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
        ask.dictated("Open a tab.");
        assert_eq!(ask.typed, "Open a tab.");
        ask.dictated("Then\nrun the tests.");
        assert_eq!(ask.typed, "Open a tab. Then run the tests.");
        ask.turn = Some(at_a_turn());
        ask.dictated("Close it.");
        assert_eq!(ask.typed, "Open a tab. Then run the tests.");
    }

    /// What you type shows on the box's last line, and `Enter` sends it once
    /// there is anything to send. While the agent is at its turn, the box
    /// shows what you asked, what it ran and what it said as it hears it, and
    /// no key but `Esc` and `Ctrl c` does anything. As the turn ends, what
    /// Claude Code denied it is marked so. Then you can answer it, and what a
    /// turn that failed said is in its place.
    #[test]
    fn the_box_shows_the_talk_and_takes_what_you_type_between_turns() {
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
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

        // As `send` leaves it, with the turn's thread already gone.
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

    /// A talk longer than the box has room for loses its start, and keeps
    /// what you type and the keys.
    #[test]
    fn a_talk_too_long_for_the_box_loses_its_start() {
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
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

    /// The agent works on the highlighted row's machine: in its pane's
    /// directory, or on a session's row or a machine's header, in luneta's on
    /// this machine, and in the home of the daemon's user on another, as the
    /// rule along the box's top says. It is told which row that is, of its
    /// own machine.
    #[test]
    fn the_agent_works_on_the_row_s_machine_where_the_row_is() {
        let top = |ask: &Ask| shown(ask, 40, 7)[1].clone();
        let pane = Ask::new(1, &pane_row(), Path::new("/w"));
        assert_eq!(pane.works, Works::Here(PathBuf::from("/w/luneta/main")));
        assert_eq!(pane.machine(), Machine::Local);
        let row = "is pane luneta:4, in /w/luneta/main, on branch feat/ask: \"this\"";
        assert!(pane.highlighted.contains(row), "{}", pane.highlighted);
        assert_eq!(top(&pane), "┌─ Ask ────────────────── luneta/main ─┐");
        let session = Ask::new(2, &Target::Session(Machine::Local, listed()), Path::new("/w"));
        assert_eq!(session.works, Works::Here(PathBuf::from("/w")));
        let session_row = "is session luneta: \"this\"";
        assert!(session.highlighted.contains(session_row), "{}", session.highlighted);

        let there = |cwd: Option<&str>| {
            Works::There { on: "nixos".parse().unwrap(), cwd: cwd.map(str::to_owned) }
        };
        let pane = Ask::new(3, &pane_row_on(nixos()), Path::new("/w"));
        assert_eq!(pane.works, there(Some("/w/luneta/main")));
        assert_eq!(pane.machine(), nixos());
        assert!(pane.highlighted.contains(row), "{}", pane.highlighted);
        assert_eq!(top(&pane), "┌─ Ask ───────── luneta/main on nixos ─┐");
        let session = Ask::new(4, &Target::Session(nixos(), listed()), Path::new("/w"));
        assert_eq!(session.works, there(None));
        assert_eq!(top(&session), "┌─ Ask ─────────────────── ~ on nixos ─┐");
        let header = Ask::new(5, &Target::Machine(nixos()), Path::new("/w"));
        assert_eq!(header.works, there(None));
        let row = "is the header of this machine: \"this\"";
        assert!(header.highlighted.contains(row), "{}", header.highlighted);
    }

    /// A turn that is no turn, as one in a directory by no absolute path,
    /// says why in the talk, and the box takes what you type again, for a
    /// turn that comes after the last that went to the agent.
    #[test]
    fn a_turn_that_is_none_says_why() {
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
        ask.works = Works::There { on: "nixos".parse().unwrap(), cwd: Some("w".to_owned()) };
        typed(&mut ask, "close it");
        let (events, _heard) = mpsc::channel();
        ask.send(User::Untold, &events);
        let wrong = "ask: cwd: w is no absolute path".to_owned();
        assert_eq!(ask.talk, [Said::You("close it".to_owned()), Said::Wrong(wrong)]);
        assert!(!ask.turning());
        assert_eq!(ask.turned, None);
    }

    /// Each turn has a key of its own, and the key of the turn before it, the
    /// first none.
    #[test]
    fn a_turn_goes_by_its_own_key_and_the_one_before() {
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
        let [first, second, third] = [(); 3].map(|()| ask.next_turn());
        assert_eq!([first.before, second.before, third.before], [None, Some(first.key), Some(second.key)]);
        assert!(first.key != second.key && second.key != third.key);
    }
}
