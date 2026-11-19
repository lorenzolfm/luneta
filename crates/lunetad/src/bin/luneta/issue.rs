use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::issues::{self, Filed, Filing, OnGitHub, Remote, Unfiled};
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
use crate::sidebar::{self, ACCENT, LIT};

const TITLE: &str = "File issue";

const LISTING: &str = "Finding the remotes on GitHub…";

const WHICH: &str = "Which repository on GitHub is it for?";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub on: Machine,
    pub dir: Directory,
    pub home: String,
    pub name: String,
}

impl Repository {
    fn is(&self, other: &Repository) -> bool {
        self.on == other.on && self.home == other.home
    }
}

#[derive(Debug, Default)]
pub struct Kept(BTreeMap<u64, (Repository, OnGitHub, String)>);

impl Kept {
    pub fn keep(&mut self, number: u64, repository: Repository, to: OnGitHub, words: String) {
        self.0.insert(number, (repository, to, words));
    }

    fn oldest(&self, repository: &Repository) -> Option<(u64, OnGitHub, String)> {
        let mut kept = self.0.iter();
        let (number, (_, to, words)) = kept.find(|(_, (of, ..))| of.is(repository))?;
        Some((*number, to.clone(), words.clone()))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Keyed {
    Nothing,
    File,
    Close,
}

#[derive(Debug)]
enum Stage {
    Listing,
    Nowhere(String),
    Found { remotes: Vec<Remote>, at: usize, picking: bool },
}

#[derive(Debug)]
pub struct Issue {
    number: u64,
    repository: Repository,
    typed: String,
    kept: Option<u64>,
    sent_to: Option<OnGitHub>,
    stage: Stage,
}

impl Issue {
    pub fn open(number: u64, repository: Repository, kept: &Kept, events: &Sender<Event>) -> Self {
        let due = Due::new(events, move |remotes: Result<Result<Vec<Remote>, String>, String>| {
            Event::Remotes { number, remotes: remotes.and_then(|remotes| remotes) }
        });
        let (on, dir) = (repository.on.clone(), repository.dir.clone());
        thread::spawn(move || {
            let remotes = match &on {
                Machine::Local => issues::remotes(&dir),
                Machine::Remote(on) => match Away::reach(on, away::ANSWER) {
                    Ok(mut away) => away.issue_remotes(&dir),
                    Err(why) => Err(format!("{on}: {why}")),
                },
            };
            due.pay(Ok(remotes));
        });
        Issue::new(number, repository, kept)
    }

    fn new(number: u64, repository: Repository, kept: &Kept) -> Self {
        let (kept, sent_to, typed) = match kept.oldest(&repository) {
            Some((number, to, words)) => (Some(number), Some(to), words),
            None => (None, None, String::new()),
        };
        Issue { number, repository, typed, kept, sent_to, stage: Stage::Listing }
    }

    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn turning(&self) -> bool {
        matches!(self.stage, Stage::Listing)
    }

    pub fn listed(&mut self, remotes: Result<Vec<Remote>, String>) {
        if !self.turning() {
            return;
        }
        self.stage = match remotes {
            Err(why) => Stage::Nowhere(why),
            Ok(remotes) if remotes.is_empty() => {
                Stage::Nowhere(format!("{} has no remote on GitHub.", self.repository.name))
            },
            Ok(remotes) => {
                let sent_to = self.sent_to.as_ref();
                let sent = |remote: &Remote| sent_to.is_some_and(|to| to.is(&remote.on));
                let at = remotes.iter().position(sent).unwrap_or(0);
                let picking = remotes.len() > 1;
                Stage::Found { remotes, at, picking }
            },
        };
    }

    fn picked(&self) -> Option<&OnGitHub> {
        match &self.stage {
            Stage::Found { remotes, at, picking: false } => Some(&remotes[*at].on),
            Stage::Found { .. } | Stage::Listing | Stage::Nowhere(_) => None,
        }
    }

    pub fn ready(&self) -> bool {
        self.picked().is_some() && !self.typed.trim().is_empty()
    }

    pub fn close(self, kept: &mut Kept) {
        let Some(number) = self.kept else {
            return;
        };
        let typed = self.typed.trim();
        match kept.0.get_mut(&number) {
            Some((_, _, words)) if !typed.is_empty() => typed.clone_into(words),
            Some(_) => {
                kept.0.remove(&number);
            },
            None => {},
        }
    }

    pub fn key(&mut self, key: &KeyEvent) -> Keyed {
        let bare = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let control = key.modifiers == KeyModifiers::CONTROL;
        if (key.code == KeyCode::Esc && bare) || (key.code == KeyCode::Char('c') && control) {
            return Keyed::Close;
        }
        let ready = self.ready();
        let Stage::Found { remotes, at, picking } = &mut self.stage else {
            return Keyed::Nothing;
        };
        if *picking {
            let last = remotes.len() - 1;
            match key.code {
                KeyCode::Up | KeyCode::Char('k') if bare => *at = at.saturating_sub(1),
                KeyCode::Char('p') if control => *at = at.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') if bare => *at = (*at + 1).min(last),
                KeyCode::Char('n') if control => *at = (*at + 1).min(last),
                KeyCode::Enter if bare => *picking = false,
                KeyCode::Char(digit @ '1'..='9') if bare => {
                    let number = digit.to_digit(10).and_then(|digit| usize::try_from(digit).ok());
                    if let Some(number) = number.filter(|number| *number <= remotes.len()) {
                        (*at, *picking) = (number - 1, false);
                    }
                },
                _ => {},
            }
            return Keyed::Nothing;
        }
        match key.code {
            KeyCode::Enter if bare && ready => return Keyed::File,
            KeyCode::Tab if bare && remotes.len() > 1 => *picking = true,
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
        self.typed.push_str(&plain(text));
    }

    pub fn dictated(&mut self, text: &str) {
        let text = plain(text);
        if !self.typed.is_empty() && !self.typed.ends_with(' ') && !text.is_empty() {
            self.typed.push(' ');
        }
        self.typed.push_str(&text);
    }

    pub fn send(
        self,
        events: &Sender<Event>,
        kept: &mut Kept,
    ) -> Option<(u64, OnGitHub, JoinHandle<()>)> {
        let words = self.typed.trim().to_owned();
        let filing = self.picked().cloned().and_then(|to| {
            let filing = Filing::new(self.repository.dir.clone(), to.clone(), &words);
            Some((to, filing.ok()?))
        });
        let Some((to, filing)) = filing else {
            self.close(kept);
            return None;
        };
        if let Some(number) = self.kept {
            kept.0.remove(&number);
        }
        let Issue { number, repository, .. } = self;
        let on = repository.on.clone();
        let sent_to = to.clone();
        let due = Due::new(events, move |filed: Result<Result<Filed, Unfiled>, String>| {
            let filed = filed.unwrap_or_else(|why| Err(Unfiled::InDoubt(why)));
            Event::Filed { number, repository, to: sent_to, words, filed }
        });
        let filing = thread::spawn(move || {
            let filed = match &on {
                Machine::Local => filing.file(),
                Machine::Remote(on) => match Away::reach(on, away::ANSWER) {
                    Ok(mut away) => away.file_issue(&filing),
                    Err(why) => Err(Unfiled::Refused(format!("{on}: {why}"))),
                },
            };
            due.pay(Ok(filed));
        });
        Some((number, to, filing))
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) {
        let Some((inner, room)) = brief::room(over) else {
            return;
        };
        let dim = Style::new().add_modifier(Modifier::DIM);
        let typed = |style| menu::wrap(&format!("› {}", self.typed), inner, style);
        let (mut lines, keys) = match &self.stage {
            Stage::Listing => {
                let mut lines = menu::waiting(LISTING, inner, dim, sidebar::frame(now));
                if !self.typed.is_empty() {
                    lines.push(Line::default());
                    lines.extend(typed(dim));
                }
                (lines, vec![(Hint::Esc, "Close")])
            },
            Stage::Nowhere(why) => {
                (brief::paragraphs(why, inner, Style::new()), vec![(Hint::Esc, "Close")])
            },
            Stage::Found { remotes, at, picking: true } => {
                let mut lines = brief::paragraphs(WHICH, inner, dim);
                for (number, remote) in remotes.iter().enumerate() {
                    let named = format!("{} {}  {}", number + 1, remote.on, remote.name);
                    let named = menu::wrap(&named, inner, Style::new()).into_iter();
                    let lit = |line: Line<'static>| line.patch_style(Style::new().bg(LIT));
                    lines.extend(named.map(|line| if number == *at { lit(line) } else { line }));
                }
                if !self.typed.is_empty() {
                    lines.push(Line::default());
                    lines.extend(typed(dim));
                }
                (lines, vec![(Hint::Moves, "Move"), (Hint::Enter, "Pick"), (Hint::Esc, "Close")])
            },
            Stage::Found { remotes, at, picking: false } => {
                let says = format!(
                    "{} writes it as {} does, and gh files it there.",
                    issues::MODEL,
                    remotes[*at].on
                );
                let mut lines = brief::paragraphs(&says, inner, dim);
                lines.push(Line::default());
                let typing = format!("› {}_", self.typed);
                lines.extend(menu::wrap(&typing, inner, Style::new().fg(ACCENT)));
                let mut keys = vec![(Hint::Enter, "File"), (Hint::Alt('r'), "Dictate")];
                if remotes.len() > 1 {
                    keys.push((Hint::Tab, "Repository"));
                }
                keys.push((Hint::Esc, "Close"));
                (lines, keys)
            },
        };
        let footer = [Line::default(), menu::keys_line(inner, &keys)];
        let lines_room = room.saturating_sub(footer.len());
        if lines.len() > lines_room {
            lines.drain(..lines.len() - lines_room);
        }
        lines.extend(footer);
        let named = self.picked().map_or_else(|| self.repository.name.clone(), ToString::to_string);
        let right = match &self.repository.on {
            Machine::Local => named,
            Machine::Remote(on) => format!("{named} on {on}"),
        };
        brief::boxed(buffer, over, TITLE, &right, &lines);
    }
}

pub fn on_quit(to: &OnGitHub, words: &str, filed: &Result<Filed, Unfiled>) -> String {
    match filed {
        Ok(filed) => {
            let (number, title, url) = (filed.number(), filed.title(), filed.link().url());
            format!("luneta: filed #{number} in {to}: {title}\n{url}")
        },
        Err(Unfiled::Refused(why)) => {
            format!("luneta: the issue in {to} is not filed: {why}. Its words:\n{words}")
        },
        Err(Unfiled::InDoubt(why)) => {
            let doubt = format!("luneta: the issue in {to} may be filed: {why}");
            format!("{doubt}. Look first. Its words:\n{words}")
        },
    }
}

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

    fn in_worktree(on: Machine, subfolder: &str) -> Repository {
        let dir = Directory::parse(format!("/w/luneta/{subfolder}")).unwrap();
        Repository { on, dir, home: "/w/luneta".to_owned(), name: "luneta".to_owned() }
    }

    fn remote(name: &str, on: &str) -> Remote {
        Remote { name: name.to_owned(), on: github(on) }
    }

    fn github(on: &str) -> OnGitHub {
        OnGitHub::parse(on).unwrap()
    }

    fn fork() -> Vec<Remote> {
        vec![remote("upstream", "cashubtc/luneta"), remote("origin", "me/luneta")]
    }

    fn open(number: u64, repository: &Repository, kept: &Kept, remotes: Vec<Remote>) -> Issue {
        let mut issue = Issue::new(number, repository.clone(), kept);
        issue.listed(Ok(remotes));
        issue
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(issue: &mut Issue, text: &str) {
        for c in text.chars() {
            assert_eq!(issue.key(&key(KeyCode::Char(c))), Keyed::Nothing);
        }
    }

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

    #[test]
    fn the_box_takes_what_you_type_and_files_it_on_enter() {
        let only = vec![remote("origin", "o/luneta")];
        let mut issue = open(1, &repository(Machine::Local), &Kept::default(), only.clone());
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        typed(&mut issue, "a red button x");
        assert_eq!(issue.key(&key(KeyCode::Backspace)), Keyed::Nothing);
        issue.paste("that\n is\x1b[31m");
        issue.dictated("too red.");
        assert_eq!(issue.typed, "a red button that is [31m too red.");
        let line_back = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(issue.key(&line_back), Keyed::Nothing);
        issue.dictated("A red button.");
        assert_eq!(issue.key(&key(KeyCode::Tab)), Keyed::Nothing);
        assert_eq!(
            shown(&issue, 50, 9)[..8],
            [
                "┌─ File issue ──────────────────────── o/luneta ─┐",
                "│ sonnet writes it as o/luneta does, and gh      │",
                "│ files it there.                                │",
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
        kept.keep(1, repository(far.clone()), github("o/luneta"), "kept words".to_owned());
        let there = open(2, &repository(far), &kept, only);
        let lines = shown(&there, 50, 9);
        assert_eq!(lines[0], "┌─ File issue ─────────────── o/luneta on nixos ─┐");
        assert_eq!(lines[4], "│ › kept words_                                  │");
    }

    #[test]
    fn you_pick_the_repository_on_github_where_the_remotes_name_more_than_one() {
        let mut issue = open(1, &repository(Machine::Local), &Kept::default(), fork());
        assert_eq!(
            shown(&issue, 50, 7),
            [
                "┌─ File issue ────────────────────────── luneta ─┐",
                "│ Which repository on GitHub is it for?          │",
                "│ 1 cashubtc/luneta  upstream                    │",
                "│ 2 me/luneta  origin                            │",
                "│                                                │",
                "│ <↓↑> - Move, <ENTER> - Pick, <ESC> - Close     │",
                "└────────────────────────────────────────────────┘",
            ]
        );
        typed(&mut issue, "kjjk");
        assert_eq!((issue.typed.as_str(), issue.picked()), ("", None));
        let down = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(issue.key(&down), Keyed::Nothing);
        assert_eq!(issue.key(&key(KeyCode::Down)), Keyed::Nothing);
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        assert_eq!(issue.picked(), Some(&github("me/luneta")));
        typed(&mut issue, "red");
        let lines = shown(&issue, 64, 9);
        assert!(lines[0].ends_with("─ me/luneta ─┐zz"), "{lines:?}");
        let keys = "│ <ENTER> File <Alt r> Dictate <TAB> Repository <ESC> Close│";
        assert!(lines[6].contains(keys), "{lines:?}");
        assert_eq!(issue.key(&key(KeyCode::Tab)), Keyed::Nothing);
        assert_eq!(issue.picked(), None);
        assert!(!issue.ready());
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        assert_eq!(issue.key(&key(KeyCode::Tab)), Keyed::Nothing);
        let up = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(issue.key(&up), Keyed::Nothing);
        typed(&mut issue, "9");
        assert_eq!(issue.picked(), None);
        typed(&mut issue, "1");
        assert_eq!(issue.picked(), Some(&github("cashubtc/luneta")));
        assert_eq!(issue.typed, "red");
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::File);
    }

    #[test]
    fn the_box_waits_for_the_remotes_and_says_where_none_is_on_github() {
        let mut issue = Issue::new(1, repository(Machine::Local), &Kept::default());
        assert!(issue.turning());
        typed(&mut issue, "red");
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        issue.paste("a red button");
        let lines = shown(&issue, 50, 7);
        assert_eq!(lines[1], "│ ⠋ Finding the remotes on GitHub…               │");
        assert_eq!(lines[3], "│ › a red button                                 │");
        assert_eq!(lines[5], "│ <ESC> - Close                                  │");
        issue.listed(Ok(Vec::new()));
        assert!(!issue.turning());
        assert_eq!(shown(&issue, 50, 5)[1], "│ luneta has no remote on GitHub.                │");
        typed(&mut issue, "x");
        assert_eq!(issue.key(&key(KeyCode::Enter)), Keyed::Nothing);
        assert_eq!(issue.key(&key(KeyCode::Esc)), Keyed::Close);
        issue.listed(Ok(fork()));
        assert_eq!(issue.picked(), None);

        let mut unsaid = Issue::new(2, repository(Machine::Local), &Kept::default());
        unsaid.listed(Err("git did not say".to_owned()));
        assert_eq!(shown(&unsaid, 50, 5)[1], "│ git did not say                                │");
    }

    #[test]
    fn a_filing_luneta_quit_during_says_how_it_ended_and_keeps_words_not_surely_filed() {
        let to = github("me/luneta");
        let filed = Filed::page("https://github.com/me/luneta/issues/12", "Red button").unwrap();
        assert_eq!(
            on_quit(&to, "a red button", &Ok(filed)),
            "luneta: filed #12 in me/luneta: Red button\nhttps://github.com/me/luneta/issues/12"
        );
        let refused = Err(Unfiled::Refused("gh is not logged in".to_owned()));
        assert_eq!(
            on_quit(&to, "a red button", &refused),
            "luneta: the issue in me/luneta is not filed: gh is not logged in. Its words:\n\
             a red button"
        );
        let doubt = Err(Unfiled::InDoubt("gh did not answer in time".to_owned()));
        assert_eq!(
            on_quit(&to, "a red button", &doubt),
            "luneta: the issue in me/luneta may be filed: gh did not answer in time. Look first. \
             Its words:\na red button"
        );
    }

    #[test]
    fn nothing_typed_sends_nothing() {
        let (events, heard) = mpsc::channel();
        let issue = open(1, &repository(Machine::Local), &Kept::default(), fork()[..1].to_vec());
        assert!(issue.send(&events, &mut Kept::default()).is_none());
        let mut unpicked = open(2, &repository(Machine::Local), &Kept::default(), fork());
        unpicked.paste("a red button");
        assert!(!unpicked.ready());
        assert!(unpicked.send(&events, &mut Kept::default()).is_none());
        assert!(heard.try_recv().is_err());
    }

    fn typed_in(issue: &Issue) -> &str {
        &issue.typed
    }

    #[test]
    fn the_words_of_an_issue_not_filed_stay_with_its_repository_until_sent() {
        let (events, _heard) = mpsc::channel();
        let here = repository(Machine::Local);
        let elsewhere = Repository { home: "/w/zed".to_owned(), ..repository(Machine::Local) };
        let far = repository(Machine::Remote("vps".parse().unwrap()));
        let mut kept = Kept::default();
        let picked = |number, repository: &Repository, kept: &Kept| {
            let mut issue = open(number, repository, kept, fork());
            issue.key(&key(KeyCode::Enter));
            issue
        };

        let blank = picked(3, &here, &kept);
        kept.keep(1, here.clone(), github("me/luneta"), "first".to_owned());
        kept.keep(2, here.clone(), github("cashubtc/luneta"), "second".to_owned());
        assert!(blank.send(&events, &mut kept).is_none());
        for other in [&elsewhere, &far] {
            assert_eq!(typed_in(&open(4, other, &kept, fork())), "");
        }
        let mut edited = open(5, &in_worktree(Machine::Local, "feat"), &kept, fork());
        assert_eq!(typed_in(&edited), "first");
        assert_eq!(edited.key(&key(KeyCode::Enter)), Keyed::Nothing);
        assert_eq!(edited.picked(), Some(&github("me/luneta")));
        typed(&mut edited, " again");
        edited.close(&mut kept);
        let mut early = picked(6, &here, &Kept::default());
        typed(&mut early, "sent while they were kept");
        let sent = early.send(&events, &mut kept).map(|(number, to, _)| (number, to));
        assert_eq!(sent, Some((6, github("cashubtc/luneta"))));
        assert_eq!(typed_in(&open(7, &here, &kept, fork())), "first again");

        assert!(picked(8, &here, &kept).send(&events, &mut kept).is_some());
        let mut cleared = picked(9, &here, &kept);
        assert_eq!(typed_in(&cleared), "second");
        let line_back = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(cleared.key(&line_back), Keyed::Nothing);
        cleared.close(&mut kept);
        assert_eq!(typed_in(&open(10, &here, &kept, fork())), "");
    }
}
