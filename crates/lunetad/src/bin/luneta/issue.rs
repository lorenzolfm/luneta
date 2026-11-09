//! The issue box: what an issue is to be, in words, for an agent to write as
//! the repository of the highlighted row writes its issues, and for gh to
//! file there, as `lunetad::issues` does. `i` opens it over zellij, on a
//! pane's row in a repository, for that repository: luneta files it for a
//! row of its own machine, and the daemon of another machine for a row of
//! that one's, through the daemon luneta watches.
//!
//! `Enter` sends what you typed, or dictated, and closes the box, and the
//! keys go back to where they were: the issue is written and filed as you go
//! on, which the notes say, and then its number and its title, as its link
//! goes on the clipboard. Where it could not be filed, the notes say why,
//! and the next box opens with your words in it, to send again.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::issues::{self, Filed, Filing};
use lunetad::Directory;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::away::{self, Away};
use crate::brief;
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::sidebar::ACCENT;

/// What the rule along the box's top names it.
const TITLE: &str = "File issue";

/// The repository an issue goes to: the one the highlighted row's pane
/// works in, by the directory it works in, on its machine, and by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub on: Machine,
    pub dir: Directory,
    pub name: String,
}

/// What a key does to the box, for luneta to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    /// Send what you typed to be written and filed, and close the box.
    File,
    /// Close the box, and what you typed with it.
    Close,
}

/// The issue box over zellij, from when `i` opens it until a key closes it.
#[derive(Debug)]
pub struct Issue {
    /// Its number among the boxes luneta has opened, which what comes of its
    /// issue carries.
    number: u64,
    repository: Repository,
    /// What you are typing, for the issue.
    typed: String,
}

impl Issue {
    /// The box numbered `number`, for an issue of `repository`, with `typed`
    /// in it: the words of an issue that was not filed, or none.
    pub fn new(number: u64, repository: Repository, typed: String) -> Self {
        Issue { number, repository, typed }
    }

    /// What `key` does: `Esc` and `Ctrl c` close the box. A character types,
    /// `Backspace` takes one back, `Ctrl u` takes back all of them, and
    /// `Enter` sends what you typed, if anything.
    pub fn key(&mut self, key: &KeyEvent) -> Keyed {
        let bare = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let control = key.modifiers == KeyModifiers::CONTROL;
        match key.code {
            KeyCode::Esc if bare => return Keyed::Close,
            KeyCode::Char('c') if control => return Keyed::Close,
            KeyCode::Enter if bare && !self.typed.trim().is_empty() => return Keyed::File,
            KeyCode::Backspace if bare => {
                self.typed.pop();
            },
            KeyCode::Char('u') if control => self.typed.clear(),
            KeyCode::Char(c) if bare => self.typed.push(c),
            _ => {},
        }
        Keyed::Nothing
    }

    /// Types `text`, pasted, on one line.
    pub fn paste(&mut self, text: &str) {
        self.typed.push_str(&plain(text));
    }

    /// Types `text`, as you dictated it, on one line after what you typed, a
    /// space between them.
    pub fn dictated(&mut self, text: &str) {
        let text = plain(text);
        if !self.typed.is_empty() && !self.typed.ends_with(' ') && !text.is_empty() {
            self.typed.push(' ');
        }
        self.typed.push_str(&text);
    }

    /// Sends what you typed to be written and filed, on a thread of its own,
    /// as the box closes. What came of it comes back on `events`, as `Due`
    /// does, with the words, which the next box opens with where it was not
    /// filed. The repository's name, for the notes, and the box's number,
    /// which what comes of it carries; or, where you typed nothing, none.
    pub fn send(self, events: &Sender<Event>) -> Option<(u64, String)> {
        let Issue { number, repository: Repository { on, dir, name }, typed } = self;
        let words = typed.trim().to_owned();
        let filing = Filing::new(dir, &words).ok()?;
        let due = Due::new(events, move |filed: Result<Result<Filed, String>, String>| {
            Event::Filed { number, words, filed: filed.and_then(|filed| filed) }
        });
        thread::spawn(move || {
            let filed = match &on {
                Machine::Local => filing.file(),
                Machine::Remote(on) => match Away::reach(on, away::ANSWER) {
                    Ok(mut away) => away.file_issue(&filing),
                    Err(why) => Err(format!("{on}: {why}")),
                },
            };
            due.pay(Ok(filed));
        });
        Some((number, name))
    }

    /// Draws it in a box over `over`, zellij's columns, at `now`, as
    /// `brief::boxed` does. The rule along its top names the repository, and
    /// the machine, where it is another. Under it, what the agent will do,
    /// what you type, and the keys.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, _now: SystemTime) {
        let Some((inner, room)) = brief::room(over) else {
            return;
        };
        let dim = Style::new().add_modifier(Modifier::DIM);
        let says = format!("{} writes it as this repository does, and gh files it.", issues::MODEL);
        let mut lines = brief::paragraphs(&says, inner, dim);
        lines.push(Line::default());
        lines.extend(menu::wrap(&format!("› {}_", self.typed), inner, Style::new().fg(ACCENT)));
        let keys = [(Hint::Enter, "File"), (Hint::Alt('r'), "Dictate"), (Hint::Esc, "Close")];
        let footer = [Line::default(), menu::keys_line(inner, &keys)];
        let lines_room = room.saturating_sub(footer.len());
        if lines.len() > lines_room {
            lines.drain(..lines.len() - lines_room);
        }
        lines.extend(footer);
        let right = match &self.repository.on {
            Machine::Local => self.repository.name.clone(),
            Machine::Remote(on) => format!("{} on {on}", self.repository.name),
        };
        brief::boxed(buffer, over, TITLE, &right, &lines);
    }
}

/// `text` on one line: each run of white space, and each character that would
/// move the cursor or tell the terminal anything, a space.
fn plain(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::gruvbox;

    fn repository(on: Machine) -> Repository {
        let dir = Directory::parse("/w/luneta/main".to_owned()).unwrap();
        Repository { on, dir, name: "luneta".to_owned() }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(issue: &mut Issue, text: &str) {
        for c in text.chars() {
            assert_eq!(issue.key(&key(KeyCode::Char(c))), Keyed::Nothing);
        }
    }

    /// The box `issue` over zellij's columns, `width` by `height`, as text.
    fn shown(issue: &Issue, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        issue.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    /// What you type, paste and dictate shows on the box's line, and `Enter`
    /// files it once there is anything to file. The rule names the
    /// repository, and its machine where that is another.
    #[test]
    fn the_box_takes_what_you_type_and_files_it_on_enter() {
        let mut issue = Issue::new(1, repository(Machine::Local), String::new());
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        typed(&mut issue, "a red button x");
        assert_eq!(issue.key(&key(KeyCode::Backspace)), Keyed::Nothing);
        issue.paste("that\n is\x1b[31m");
        issue.dictated("too red.");
        assert_eq!(issue.typed, "a red button that is [31m too red.");
        let line_back = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(issue.key(&line_back), Keyed::Nothing);
        issue.dictated("A red button.");
        assert_eq!(
            shown(&issue, 50, 9)[..8],
            [
                "┌─ File issue ────────────────────────── luneta ─┐",
                "│ sonnet writes it as this repository does, and  │",
                "│ gh files it.                                   │",
                "│                                                │",
                "│ › A red button._                               │",
                "│                                                │",
                "│ <ENTER> File  <Alt r> Dictate  <ESC> Close     │",
                "└────────────────────────────────────────────────┘",
            ]
        );
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::File);
        assert_eq!(issue.key(&key(KeyCode::Esc)), Keyed::Close);
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(issue.key(&interrupt), Keyed::Close);

        let far = Machine::Remote("nixos".parse().unwrap());
        let there = Issue::new(2, repository(far), "kept words".to_owned());
        let lines = shown(&there, 50, 9);
        assert_eq!(lines[0], "┌─ File issue ───────────────── luneta on nixos ─┐");
        assert_eq!(lines[4], "│ › kept words_                                  │");
    }

    /// Words of nothing but white space send nothing, and owe no answer.
    #[test]
    fn nothing_typed_sends_nothing() {
        let (events, heard) = mpsc::channel();
        let issue = Issue::new(1, repository(Machine::Local), "  ".to_owned());
        assert_eq!(issue.send(&events), None);
        assert!(heard.try_recv().is_err());
    }
}
