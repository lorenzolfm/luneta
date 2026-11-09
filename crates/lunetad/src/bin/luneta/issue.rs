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
//! and the next box for that repository opens with your words in it, to send
//! again: those of each issue not filed, oldest first, until it is sent.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::issues::{self, Filed, Filing, Unfiled};
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
/// works in, by the directory it works in, on its machine, by the directory
/// git said holds it, and by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub on: Machine,
    pub dir: Directory,
    pub home: String,
    pub name: String,
}

impl Repository {
    /// Whether `other` is it, from whichever of its worktrees.
    fn is(&self, other: &Repository) -> bool {
        self.on == other.on && self.home == other.home
    }
}

/// What you said of each issue that was not filed, by the number of the box
/// it was sent from, and so oldest first, with its repository: an issue box
/// for that repository opens with the oldest, until it is sent.
#[derive(Debug, Default)]
pub struct Kept(BTreeMap<u64, (Repository, String)>);

impl Kept {
    /// Keeps `words`, of the issue the box numbered `number` sent to
    /// `repository`, which was not filed.
    pub fn keep(&mut self, number: u64, repository: Repository, words: String) {
        self.0.insert(number, (repository, words));
    }

    /// The oldest words kept for `repository`, by the number they are kept
    /// by.
    fn oldest(&self, repository: &Repository) -> Option<(u64, String)> {
        let mut kept = self.0.iter();
        let (number, (_, words)) = kept.find(|(_, (of, _))| of.is(repository))?;
        Some((*number, words.clone()))
    }
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
    /// The number the words it opened with are kept by, where it opened with
    /// some.
    kept: Option<u64>,
}

impl Issue {
    /// The box numbered `number`, for an issue of `repository`, with the
    /// oldest words `kept` for it in it, or none.
    pub fn new(number: u64, repository: Repository, kept: &Kept) -> Self {
        let (kept, typed) = kept.oldest(&repository).unzip();
        Issue { number, repository, typed: typed.unwrap_or_default(), kept }
    }

    /// Closes it, with nothing sent: the words it opened with stay `kept`,
    /// as you left them, but where you took all of them back.
    pub fn close(self, kept: &mut Kept) {
        let Some(number) = self.kept else {
            return;
        };
        let typed = self.typed.trim();
        match kept.0.get_mut(&number) {
            Some((_, words)) if !typed.is_empty() => typed.clone_into(words),
            Some(_) => {
                kept.0.remove(&number);
            },
            None => {},
        }
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
    /// as the box closes, and the words it opened with are `kept` no more.
    /// What came of it comes back on `events`, as `Due` does, with the words
    /// and the repository, to be kept for it where none was filed. An answer
    /// that never came may have filed one. The box's number, which what comes
    /// of it carries, and the repository's name, for the notes; or, where you
    /// typed nothing, none, as the box closes.
    pub fn send(self, events: &Sender<Event>, kept: &mut Kept) -> Option<(u64, String)> {
        let words = self.typed.trim().to_owned();
        let Ok(filing) = Filing::new(self.repository.dir.clone(), &words) else {
            self.close(kept);
            return None;
        };
        if let Some(number) = self.kept {
            kept.0.remove(&number);
        }
        let Issue { number, repository, .. } = self;
        let (on, name) = (repository.on.clone(), repository.name.clone());
        let due = Due::new(events, move |filed: Result<Result<Filed, Unfiled>, String>| {
            let filed = filed.unwrap_or_else(|why| Err(Unfiled::InDoubt(why)));
            Event::Filed { number, repository, words, filed }
        });
        thread::spawn(move || {
            let filed = match &on {
                Machine::Local => filing.file(),
                Machine::Remote(on) => match Away::reach(on, away::ANSWER) {
                    Ok(mut away) => away.file_issue(&filing),
                    Err(why) => Err(Unfiled::Refused(format!("{on}: {why}"))),
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
        in_worktree(on, "main")
    }

    /// The repository of `repository` by its worktree named `worktree`.
    fn in_worktree(on: Machine, worktree: &str) -> Repository {
        let dir = Directory::parse(format!("/w/luneta/{worktree}")).unwrap();
        Repository { on, dir, home: "/w/luneta".to_owned(), name: "luneta".to_owned() }
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
        let mut issue = Issue::new(1, repository(Machine::Local), &Kept::default());
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
        let mut kept = Kept::default();
        kept.keep(1, repository(far.clone()), "kept words".to_owned());
        let there = Issue::new(2, repository(far), &kept);
        let lines = shown(&there, 50, 9);
        assert_eq!(lines[0], "┌─ File issue ───────────────── luneta on nixos ─┐");
        assert_eq!(lines[4], "│ › kept words_                                  │");
    }

    /// Words of nothing but white space send nothing, and owe no answer.
    #[test]
    fn nothing_typed_sends_nothing() {
        let (events, heard) = mpsc::channel();
        let issue = Issue::new(1, repository(Machine::Local), &Kept::default());
        assert_eq!(issue.send(&events, &mut Kept::default()), None);
        assert!(heard.try_recv().is_err());
    }

    fn typed_in(issue: &Issue) -> &str {
        &issue.typed
    }

    /// The words of each issue not filed are kept for its repository, from
    /// whichever of its worktrees, oldest first: a box for another repository,
    /// or one on another machine, opens with none of them. Each stays until
    /// the box it opened in sends it, as you left it there, but where you
    /// took all of it back; a box opened while they were on their way sends
    /// none of them.
    #[test]
    fn the_words_of_an_issue_not_filed_stay_with_its_repository_until_sent() {
        let (events, _heard) = mpsc::channel();
        let here = repository(Machine::Local);
        let elsewhere = Repository { home: "/w/zed".to_owned(), ..repository(Machine::Local) };
        let far = repository(Machine::Remote("vps".parse().unwrap()));
        let mut kept = Kept::default();
        let open = |number, repository: &Repository, kept: &Kept| {
            Issue::new(number, repository.clone(), kept)
        };

        let blank = open(3, &here, &kept);
        kept.keep(1, here.clone(), "first".to_owned());
        kept.keep(2, here.clone(), "second".to_owned());
        assert_eq!(blank.send(&events, &mut kept), None);
        for other in [&elsewhere, &far] {
            assert_eq!(typed_in(&open(4, other, &kept)), "");
        }
        let mut edited = open(5, &in_worktree(Machine::Local, "feat"), &kept);
        assert_eq!(typed_in(&edited), "first");
        typed(&mut edited, " again");
        edited.close(&mut kept);
        let mut early = open(6, &here, &Kept::default());
        typed(&mut early, "sent while they were kept");
        assert!(early.send(&events, &mut kept).is_some());
        assert_eq!(typed_in(&open(7, &here, &kept)), "first again");

        assert!(open(8, &here, &kept).send(&events, &mut kept).is_some());
        let mut cleared = open(9, &here, &kept);
        assert_eq!(typed_in(&cleared), "second");
        let line_back = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(cleared.key(&line_back), Keyed::Nothing);
        cleared.close(&mut kept);
        assert_eq!(typed_in(&open(10, &here, &kept)), "");
    }
}
