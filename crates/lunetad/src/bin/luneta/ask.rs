//! The ask box: what you want done, in words, for an agent to do with
//! luneta's commands, as the menu would: "make me a session to work on
//! floresta", "tackle these four issues in parallel", "close every session
//! but this one". `a` opens it over zellij, for the row highlighted then.
//!
//! What you type goes to `claude -p`, with luneta's skill, and with luneta's
//! commands and the few its skill reads with as the only ones it may run,
//! and those your own settings let it: a pane it opens runs your shell or
//! Claude Code, and no other command. The box shows what it runs, and what
//! Claude Code denied it, and what it says as it goes. Once it is done,
//! what you type next goes to it on the same Claude session, so that it can
//! ask you before it closes anything, as the skill has it ask, or which of
//! two things you meant. A close it runs takes a yes only in the turn after
//! the one that said its plan, so that your answer comes between them, as
//! Claude Code runs the close without asking you. Closing the box ends it,
//! and nothing is kept.
//!
//! The agent runs in no pane of zellij, and goes by the one you asked from:
//! the pane luneta's zellij is on, which luneta's commands take for the one
//! they run in, by `zellij::ASKED_VAR`. luneta's zellij is the last client to
//! type there as each turn starts, so that `go` and `open session` move it.

use std::env;
use std::ffi::OsString;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::program;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use serde_json::Value;

use crate::brief::{self, Asking};
use crate::drive::Turn;
use crate::due::Due;
use crate::gruvbox;
use crate::host::Event;
use crate::menu::{self, Hint, Target};
use crate::sidebar::{self, ACCENT};
use crate::zellij::{self, Pane};

/// The model the agent runs on: one that works out what words point at, and
/// runs a few commands in a row, without slips.
const MODEL: &str = "sonnet";

/// How long a turn has. One that opens a tab with a worktree for each of a
/// few issues takes a minute or two: one that takes longer than this is
/// stuck, and is killed.
const TURN: Duration = Duration::from_mins(10);

/// How often a turn looks at whether it is still wanted, while `claude` says
/// nothing.
const LOOK: Duration = Duration::from_millis(5);

/// What the rule along the box's top names it.
const TITLE: &str = "Ask";

/// Variables the agent must not have: with them, the daemon places it in the
/// pane they name, as an agent of its own, and luneta's commands take that
/// pane for the one they run in.
const LEFT_OUT: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

/// The commands the agent may run, as Claude Code's rules name them: the
/// verbs of luneta's its skill has, and the commands its skill reads what
/// your words point at with, and makes worktrees with. In `dontAsk` mode,
/// Claude Code denies it any other but those your own settings allow, and a
/// line that runs one inside another, as through `$(…)`.
const ALLOWED: [&str; 14] = [
    "Bash(luneta here:*)",
    "Bash(luneta sessions:*)",
    "Bash(luneta agents:*)",
    "Bash(luneta open:*)",
    "Bash(luneta go:*)",
    "Bash(luneta close:*)",
    "Bash(zoxide query:*)",
    "Bash(gh issue view:*)",
    "Bash(gh issue list:*)",
    "Bash(gh pr view:*)",
    "Bash(gh pr list:*)",
    "Bash(luneta worktree:*)",
    "Bash(git worktree add:*)",
    "Bash(git worktree list:*)",
];

/// luneta's skill, which says how an agent goes about what you ask: the one
/// this luneta's commands are of, whatever skills Claude Code has installed.
const SKILL: &str = include_str!("../../../../../skills/luneta/SKILL.md");

/// What the agent is told, whatever you ask, before what it is told of the
/// row highlighted, and the skill.
const SYSTEM: &str = "You run in luneta's ask box. The user typed what they want in luneta's \
    sidebar, and reads what you say in a small box over zellij: write plain text for a \
    terminal, with no markdown, no headings, no tables and no code fences, and keep it short. \
    Do what they ask with luneta's commands, as the luneta skill below says, and end with a \
    line or two on what you did, and where. Run one command in each call to Bash, with \
    nothing around it: no $(…), no pipes, no && and no ;. Only these run: luneta here, \
    sessions, agents, open, go and close, zoxide query, gh issue view and list, gh pr view \
    and list, luneta worktree, and git worktree add and list. Claude Code denies any other. A \
    pane you open runs the user's shell, or Claude Code as claude [--name <name>] [<prompt>], \
    with no other option of claude's: luneta refuses any other command. You cannot wait for \
    the user in the middle of a turn: to ask them anything, as the skill has you ask before \
    a close, or which of two things they meant, end your turn with the question. Their \
    answer comes as the next message. So a close takes --yes only in the turn after the one \
    that said its plan.\n\nEach message of the user's starts with a line of luneta's in \
    brackets, which says the pane they are in as they send it, or that luneta cannot tell: \
    that goes for the turn, whatever one before said. `luneta here` names that pane, and a \
    close's plan marks the agent there (this runs here). You run in no pane yourself, so \
    closing it does not end you, but it is the user's: leave it and its session out of \
    \"all\", as the skill says.";

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

/// A step of a turn of the agent, as `claude` says it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// The Claude session the turn runs on, which the next one goes on with.
    Session(String),
    /// A command the agent runs, or a tool it uses, by its name, and the id
    /// Claude Code gives the call.
    Ran { id: String, command: String },
    /// Claude Code denied the agent the call of this id, which ran nothing.
    Denied(String),
    /// What the agent said.
    Said(String),
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

/// The ask box over zellij, from when `a` opens it until a key closes it.
#[derive(Debug)]
pub struct Ask {
    /// Its number among the boxes luneta has opened, which what a turn hears
    /// carries.
    number: u64,
    /// Where the agent runs, each turn: the directory of the highlighted
    /// row's pane as the box opened, or else luneta's.
    cwd: PathBuf,
    /// What the agent is told of the row highlighted as the box opened.
    highlighted: String,
    /// The Claude session the talk goes on on, once its first turn says.
    session: Option<String>,
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
    /// opens: the agent works in its pane's directory, or in `dir`, luneta's,
    /// on a session's row.
    pub fn new(number: u64, target: &Target, dir: &Path) -> Self {
        Ask {
            number,
            cwd: target.cwd().map_or_else(|| dir.to_owned(), PathBuf::from),
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

    /// Hands what you typed to the agent, on a thread of its own, for you in
    /// the pane `here`, if luneta can tell it, as `told` tells it, and takes
    /// it from what you are typing. What the turn hears comes back on
    /// `events`, and how it ended, as `Due` does.
    pub fn send(&mut self, here: Option<&Pane>, events: &Sender<Event>) {
        if self.turn.is_some() {
            return;
        }
        let words = std::mem::take(&mut self.typed).trim().to_owned();
        if words.is_empty() {
            return;
        }
        let turn = self.next_turn();
        let claude = claude(&self.cwd, self.session.as_deref(), here, &self.highlighted, turn);
        let message = told(here, &words);
        self.talk.push(Said::You(words));
        let (number, events) = (self.number, events.clone());
        // How the turn ended, or else that it ended with no answer.
        let due = Due::new(&events, move |ended: Result<Result<(), String>, String>| {
            let ended = ended.unwrap_or_else(|why| Err(format!("ask: {why}")));
            Event::Heard { number, heard: Heard::Ended(ended) }
        });
        self.turn = Some(Asking::on_thread(move |wanted| {
            let hear = |step| {
                // luneta is on its way out.
                let _ = events.send(Event::Heard { number, heard: Heard::Step(step) });
            };
            // Given up, it is owed to a box that is gone.
            if let Some(ended) = talk(claude, &message, wanted, &hear) {
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
    /// works. Under it, the talk so far, and then what you type, or that the
    /// agent is at a turn, after a spinner that turns, and the keys. Where
    /// the box has no room for the talk, its start goes, its first line a
    /// `…`.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) {
        let Some((inner, room)) = brief::room(over) else {
            return;
        };
        let mut lines = self.lines(inner, sidebar::frame(now));
        let keys: &[(Hint, &str)] = match self.turn {
            Some(_) => &[(Hint::Esc, "Close")],
            None => &[(Hint::Enter, "Send"), (Hint::Esc, "Close")],
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
        let right = sidebar::short_cwd(&self.cwd.to_string_lossy());
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
            lines.extend(menu::waiting(&format!("Asking {MODEL}…"), inner, dim, frame));
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
/// opened, for words that point at it.
fn highlighted(target: &Target) -> String {
    let place = target.place();
    let session = place.session();
    let row = match place.pane() {
        Some(pane) => format!("pane {session}:{pane}"),
        None => format!("session {session}"),
    };
    let cwd = target.cwd().map(|cwd| format!(", in {cwd}")).unwrap_or_default();
    let branch = target.branch().map(|branch| format!(", on branch {branch}"));
    let row = format!("{row}{cwd}{}", branch.unwrap_or_default());
    format!(
        "The row highlighted in luneta's sidebar as they opened the box is {row}: \"this\" or \
         \"that one\" may mean it."
    )
}

/// What the agent is told, with the row `highlighted` as the box opened, and
/// the skill: the same each turn, as Claude Code keeps what a talk's first
/// turn was told for the rest of it.
fn system(highlighted: &str) -> String {
    format!("{SYSTEM}\n\n{highlighted}\n\n{SKILL}")
}

/// The message of a turn: `words`, what you typed, after a line that says
/// which pane you are in as you send it, `here`, if luneta can tell it.
fn told(here: Option<&Pane>, words: &str) -> String {
    let here = match here {
        Some(here) => format!("[luneta: the user is in pane {here}.]"),
        None => "[luneta: it cannot tell which pane the user is in just now, so `luneta here`, \
                 `go`, `open session` and a close fail, and so do `open pane` without --beside \
                 and `open tab` without --session. Tell the user so where you need them.]"
            .to_owned(),
    };
    format!("{here}\n\n{words}")
}

/// `claude`, for `turn` in `cwd`, on the Claude session `resumed` if one
/// went before, for you in the pane `here`, with the row `highlighted` as the
/// box opened. It runs on the model the box has, in safe mode, so that none
/// of your hooks, `CLAUDE.md` or MCP servers come into it, with Bash its one
/// tool, and `ALLOWED` all it runs but what your own settings allow: not
/// what those of the directory it runs in do, which a repository brings. It
/// has no zellij in its environment, so that the daemon places it in no
/// pane, nor what tells git where a repository is, so that the git it runs,
/// and `luneta worktree`, find one by the directory they run in. The
/// `luneta` it runs is this one, and its closes know the turn.
fn claude(
    cwd: &Path,
    resumed: Option<&str>,
    here: Option<&Pane>,
    highlighted: &str,
    turn: Turn,
) -> Command {
    let mut claude = Command::new("claude");
    claude.args(["-p", "--model", MODEL, "--safe-mode", "--setting-sources", "user"]);
    claude.args(["--permission-mode", "dontAsk", "--tools", "Bash"]);
    claude.args(["--output-format", "stream-json", "--verbose"]);
    claude.arg("--append-system-prompt").arg(system(highlighted));
    if let Some(session) = resumed {
        claude.args(["--resume", session]);
    }
    // It takes every word after it for one more rule: the words you typed go
    // on its stdin.
    claude.arg("--allowedTools").args(ALLOWED);
    for name in LEFT_OUT {
        claude.env_remove(name);
    }
    lunetad::local_git(&mut claude);
    match here {
        Some(here) => claude.env(zellij::ASKED_VAR, here.to_string()),
        None => claude.env_remove(zellij::ASKED_VAR),
    };
    turn.name_in(&mut claude);
    if let Some(path) = env::current_exe().ok().and_then(|exe| path_with(&exe)) {
        claude.env("PATH", path);
    }
    claude.current_dir(cwd);
    claude
}

/// The `PATH` with the directory of `exe` first, where it is a `luneta`, so
/// that the `luneta` the agent runs is that one, as old or as new as the
/// skill the agent is told.
fn path_with(exe: &Path) -> Option<OsString> {
    if exe.file_name()? != "luneta" {
        return None;
    }
    let path = env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(exe.parent()?.to_owned()).chain(env::split_paths(&path));
    env::join_paths(dirs).ok()
}

/// What a line `claude` writes says, of what the box shows.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Step(Step),
    /// The turn's end, as `claude` says it: what the agent said last, or why
    /// the turn failed.
    Ended(Result<String, String>),
}

/// What `line`, a JSON record of what `claude` does, says: the session it
/// starts on, what the agent ran and said, and the turn's end, with the
/// calls Claude Code denied it, as it says only then. A subagent's records
/// are left out, and so are what the agent thought, what a tool answered,
/// and the rest.
fn read(line: &str) -> Vec<Read> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match record["type"].as_str() {
        Some("system") if record["subtype"] == "init" => {
            let session = record["session_id"].as_str().map(str::to_owned);
            session.map(|session| Read::Step(Step::Session(session))).into_iter().collect()
        },
        Some("assistant") if record["parent_tool_use_id"].is_null() => {
            let blocks = record["message"]["content"].as_array().into_iter().flatten();
            blocks.filter_map(step).map(Read::Step).collect()
        },
        Some("result") => {
            let denials = record["permission_denials"].as_array().into_iter().flatten();
            let denied = denials.filter_map(|denial| denial["tool_use_id"].as_str());
            let mut read: Vec<_> = denied.map(|id| Read::Step(Step::Denied(id.to_owned()))).collect();
            let said = record["result"].as_str().unwrap_or_default().trim().to_owned();
            let failed = record["is_error"].as_bool() != Some(false) || record["subtype"] != "success";
            let why = || match record["subtype"].as_str() {
                _ if !said.is_empty() => said.clone(),
                Some(subtype) => subtype.to_owned(),
                None => "the turn failed".to_owned(),
            };
            read.push(Read::Ended(if failed { Err(why()) } else { Ok(said.clone()) }));
            read
        },
        _ => Vec::new(),
    }
}

/// A step of what the agent did, from a block of what it sent: what it said,
/// or the command it ran, or another tool it used, by its name.
fn step(block: &Value) -> Option<Step> {
    match block["type"].as_str()? {
        "text" => {
            let text = block["text"].as_str()?.trim();
            (!text.is_empty()).then(|| Step::Said(text.to_owned()))
        },
        "tool_use" => {
            let ran = block["input"]["command"].as_str().or_else(|| block["name"].as_str())?;
            let id = block["id"].as_str().unwrap_or_default().to_owned();
            Some(Step::Ran { id, command: ran.to_owned() })
        },
        _ => None,
    }
}

/// Runs `claude`, as `command` has it, on `words`, which go on its stdin, and
/// hands each step it hears to `hear` as `claude` says it, for as long as the
/// turn is `wanted`: none once it is not, as `claude` is killed then. Or how
/// the turn ended: well, or why not, as `claude` says, or as it failed, or as
/// it was still at it after `TURN`. An agent that said nothing all turn says
/// what `claude` says it said last, if anything.
fn talk(
    mut command: Command,
    words: &str,
    wanted: &dyn Fn() -> bool,
    hear: &dyn Fn(Step),
) -> Option<Result<(), String>> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        // A program not on the PATH is not found, and so is a directory to
        // start in that is gone, as a pane's is once its worktree is removed.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let gone = command.get_current_dir().filter(|cwd| !cwd.is_dir());
            return Some(Err(match gone {
                Some(cwd) => format!("ask: {}, where the agent works, is gone", cwd.display()),
                None => "ask: claude is not on the PATH".to_owned(),
            }));
        },
        Err(error) => return Some(Err(format!("ask: claude could not be run: {error}"))),
    };
    // `claude -p` reads its stdin to the end before it starts. One that ends
    // without reading it says why on its stderr.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(words.as_bytes());
    }
    let stdout = child.stdout.take().expect("its stdout is piped");
    let (said, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if said.send(line).is_err() {
                return;
            }
        }
    });
    let stderr = child.stderr.take().expect("its stderr is piped");
    let stderr = thread::spawn(move || {
        let mut said = Vec::new();
        let _ = stderr.take(program::SAID_MAX).read_to_end(&mut said);
        said
    });
    let deadline = Instant::now() + TURN;
    let (mut ended, mut spoke) = (None, false);
    loop {
        match lines.recv_timeout(LOOK) {
            Ok(Ok(line)) => {
                for read in read(&line) {
                    match read {
                        Read::Step(step) => {
                            spoke |= matches!(step, Step::Said(_));
                            hear(step);
                        },
                        Read::Ended(end) => ended = Some(end),
                    }
                }
            },
            // Its stdout ended, or does not read.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {},
        }
        let stuck = Instant::now() >= deadline;
        if stuck || !wanted() {
            // One that ended just now is not waited for yet, and takes the
            // signal all the same.
            let _ = child.kill();
            let _ = child.wait();
            let minutes = TURN.as_secs() / 60;
            return stuck.then(|| Err(format!("ask: claude was still at it after {minutes} minutes")));
        }
    }
    let status = child.wait();
    let stderr = stderr.join().unwrap_or_default();
    Some(match (ended, status) {
        (Some(Err(why)), _) => Err(format!("ask: {why}")),
        (Some(Ok(last)), Ok(status)) if status.success() => {
            if !spoke && !last.is_empty() {
                hear(Step::Said(last));
            }
            Ok(())
        },
        (_, Ok(status)) => Err(failed(&stderr, status)),
        (_, Err(error)) => Err(format!("ask: claude could not be waited for: {error}")),
    })
}

/// Why `claude` failed, as it ended with `status` and said `stderr`: the
/// first line of what it said, or how it ended, or that it said nothing of
/// the turn's end.
fn failed(stderr: &[u8], status: ExitStatus) -> String {
    let said = String::from_utf8_lossy(stderr);
    match said.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => format!("ask: claude failed: {line}"),
        None if status.success() => "ask: claude ended without saying how the turn went".to_owned(),
        None => format!("ask: claude failed: {status}"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::SystemTime;

    use serde_json::json;

    use super::*;
    use crate::drive::{BEFORE_VAR, TURN_VAR};
    use crate::menu::{PaneRow, Ran};
    use crate::sidebar::Listed;

    fn listed() -> Listed {
        Listed::new("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    fn pane_row() -> Target {
        Target::Pane(PaneRow {
            session: listed(),
            pane: 4,
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/ask".to_owned()),
            ran: Ran::Untold,
        })
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
                "│ <ENTER> - Send, <ESC> - Close                  │",
                "└────────────────────────────────────────────────┘",
            ]
        );
        assert_eq!(ask.key(&key(KeyCode::Enter)), Keyed::Send);

        // As `send` leaves it, with the turn's thread already gone.
        ask.talk.push(Said::You(std::mem::take(&mut ask.typed)));
        ask.turn = Some(at_a_turn());
        ask.heard(Heard::Step(Step::Session("7c22".to_owned())));
        let ran = |id: &str, command: &str| Step::Ran { id: id.to_owned(), command: command.to_owned() };
        ask.heard(Heard::Step(ran("toolu_1", "luneta close bipa\nelo")));
        ask.heard(Heard::Step(ran("toolu_2", "touch elo")));
        ask.heard(Heard::Step(Step::Said("This kills bipa and elo.\n\nClose them?".to_owned())));
        assert_eq!(ask.session.as_deref(), Some("7c22"));
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
                "│ <ENTER> - Send, <ESC> - Close        │",
                "└──────────────────────────────────────┘",
            ]
        );
    }

    /// The agent runs in the highlighted pane's directory, or luneta's on a
    /// session's row, and is told which row that is. It is told the pane you
    /// are in each turn, in what the turn sends it, as Claude Code keeps the
    /// system prompt of a talk's first turn. It runs on the box's model, in
    /// safe mode, with the permission rules of your settings alone, no tool
    /// but Bash and nothing it may run but `ALLOWED`, and the words go on
    /// its stdin, as `--allowedTools` takes every word after it. It has no
    /// zellij in its environment, nor what tells git where a repository is,
    /// and the pane you are in in luneta's, where luneta can tell it, and the
    /// turn it is at. A turn after the first goes on with the session the
    /// first said.
    #[test]
    fn the_agent_runs_where_the_row_is_for_the_pane_you_are_in() {
        let args = |command: &Command| -> Vec<String> {
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
        };
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        let mut ask = Ask::new(1, &pane_row(), Path::new("/w"));
        let here = Pane { session: "luneta".to_owned(), pane: 2 };
        let turn = ask.next_turn();
        let first = claude(&ask.cwd, None, Some(&here), &ask.highlighted, turn);
        assert_eq!(first.get_current_dir(), Some(Path::new("/w/luneta/main")));
        let said = args(&first);
        let model = ["-p", "--model", "sonnet", "--safe-mode", "--setting-sources", "user"];
        let permissions = ["--permission-mode", "dontAsk", "--tools", "Bash"];
        let output = ["--output-format", "stream-json", "--verbose"];
        assert_eq!(said[..13], [&model[..], &permissions, &output].concat());
        assert_eq!(said[13], "--append-system-prompt");
        let system = &said[14];
        assert!(system.starts_with(SYSTEM), "{system}");
        assert!(!system.contains("pane luneta:2"), "{system}");
        let row = "is pane luneta:4, in /w/luneta/main, on branch feat/ask: \"this\"";
        assert!(system.contains(row), "{system}");
        assert!(system.ends_with(SKILL), "{system}");
        assert_eq!(said[15..], [&["--allowedTools"][..], &ALLOWED[..]].concat());
        assert_eq!(told(Some(&here), "close it"), "[luneta: the user is in pane luneta:2.]\n\nclose it");
        let unseated = told(None, "close it");
        assert!(unseated.starts_with("[luneta: it cannot tell which pane the user is in"), "{unseated}");
        assert!(unseated.ends_with("]\n\nclose it"), "{unseated}");
        for name in LEFT_OUT.into_iter().chain(lunetad::GIT_REPOSITORY_VARS) {
            assert_eq!(var(&first, name), Some(None), "{name}");
        }
        assert_eq!(var(&first, zellij::ASKED_VAR), Some(Some("luneta:2".to_owned())));
        assert!(var(&first, TURN_VAR).is_some_and(|key| key.is_some()));
        assert_eq!(var(&first, BEFORE_VAR), Some(None));

        let session = Target::Session(listed());
        let ask = Ask::new(2, &session, Path::new("/w"));
        let turn = Turn { key: 255, before: Some(10) };
        let next = claude(&ask.cwd, Some("7c22"), None, &ask.highlighted, turn);
        assert_eq!(next.get_current_dir(), Some(Path::new("/w")));
        let said = args(&next);
        assert_eq!(said[15..17], ["--resume", "7c22"]);
        assert!(said[14].contains("is session luneta: \"this\""), "{}", said[14]);
        assert_eq!(var(&next, zellij::ASKED_VAR), Some(None));
        assert_eq!(var(&next, BEFORE_VAR), Some(Some("000000000000000a".to_owned())));
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

    /// The agent runs this luneta, where it is one, ahead of any other on the
    /// `PATH`.
    #[test]
    fn the_agent_runs_this_luneta() {
        let path = path_with(Path::new("/nix/store/x-luneta/bin/luneta")).expect("a PATH");
        let first = env::split_paths(&path).next();
        assert_eq!(first.as_deref(), Some(Path::new("/nix/store/x-luneta/bin")));
        assert_eq!(path_with(Path::new("/w/target/debug/deps/luneta-0a1b")), None);
    }

    /// What the agent ran and said comes out of what `claude` writes, a
    /// record to a line, and so does the session it runs on and how its turn
    /// ended, with what Claude Code denied it. A subagent's records, what it
    /// thought and the rest are left out.
    #[test]
    fn what_claude_writes_says_what_the_agent_ran_and_said() {
        let line = |record: Value| record.to_string();
        let assistant = |content: Value| json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": content}});
        assert_eq!(
            read(&line(json!({"type": "system", "subtype": "init", "session_id": "7c22"}))),
            [Read::Step(Step::Session("7c22".to_owned()))]
        );
        assert_eq!(
            read(&line(assistant(json!([
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "  Looking.  "},
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "luneta sessions", "description": "List"}},
                {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/w/x"}},
                {"type": "text", "text": " "},
            ])))),
            [
                Read::Step(Step::Said("Looking.".to_owned())),
                Read::Step(Step::Ran { id: "toolu_1".to_owned(), command: "luneta sessions".to_owned() }),
                Read::Step(Step::Ran { id: "toolu_2".to_owned(), command: "Read".to_owned() }),
            ]
        );
        let mut subagent = assistant(json!([{"type": "text", "text": "a subagent's"}]));
        subagent["parent_tool_use_id"] = json!("toolu_1");
        for record in [
            subagent,
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "hi"}]}}),
            json!({"type": "rate_limit_event"}),
            json!({"type": "system", "subtype": "status"}),
        ] {
            assert_eq!(read(&line(record.clone())), [], "{record}");
        }
        assert_eq!(read("not json"), []);
        let result = |subtype: &str, is_error: bool, result: &str| {
            line(json!({"type": "result", "subtype": subtype, "is_error": is_error, "result": result}))
        };
        assert_eq!(read(&result("success", false, "Done.\n")), [Read::Ended(Ok("Done.".to_owned()))]);
        assert_eq!(read(&result("success", true, "Not logged in")), [Read::Ended(Err("Not logged in".to_owned()))]);
        assert_eq!(read(&result("error_max_turns", true, "")), [Read::Ended(Err("error_max_turns".to_owned()))]);
        let denied = json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "Done.",
            "permission_denials": [
                {"tool_name": "Bash", "tool_use_id": "toolu_3", "tool_input": {"command": "touch x"}},
            ],
        });
        assert_eq!(
            read(&line(denied)),
            [Read::Step(Step::Denied("toolu_3".to_owned())), Read::Ended(Ok("Done.".to_owned()))]
        );
    }

    /// A turn hands on what `claude` writes as it writes it, and ends as it
    /// says: well, or with why not. One that says how it ended without having
    /// said a word says that.
    #[test]
    fn a_turn_hands_on_what_claude_writes_and_ends_as_it_says() {
        let heard = |script: &str, words: &str| {
            let mut sh = Command::new("sh");
            sh.args(["-c", script]);
            let heard = RefCell::new(Vec::new());
            let ended = talk(sh, words, &|| true, &|said| heard.borrow_mut().push(said));
            (heard.into_inner(), ended)
        };
        let init = r#"{"type":"system","subtype":"init","session_id":"7c22"}"#;
        let ran = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"luneta here"}}]}}"#;
        let done = r#"{"type":"result","subtype":"success","is_error":false,"result":"You are in luneta:2."}"#;
        let script = format!("read words; echo '{init}'; echo '{ran}'; echo \"$words\" >&2; echo '{done}'");
        let (said, ended) = heard(&script, "where am I\n");
        assert_eq!(said, [
            Step::Session("7c22".to_owned()),
            Step::Ran { id: "toolu_1".to_owned(), command: "luneta here".to_owned() },
            Step::Said("You are in luneta:2.".to_owned()),
        ]);
        assert_eq!(ended, Some(Ok(())));

        let failed = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}"#;
        assert_eq!(heard(&format!("echo '{failed}'"), "").1, Some(Err("ask: Not logged in".to_owned())));
        let ended = heard("echo 'No conversation found' >&2; exit 1", "").1;
        assert_eq!(ended, Some(Err("ask: claude failed: No conversation found".to_owned())));
        let ended = heard("exit 0", "").1;
        assert_eq!(ended, Some(Err("ask: claude ended without saying how the turn went".to_owned())));

        let mut gone = Command::new("sh");
        gone.current_dir("/w/gone");
        let ended = talk(gone, "", &|| true, &|_| {});
        assert_eq!(ended, Some(Err("ask: /w/gone, where the agent works, is gone".to_owned())));
        let ended = talk(Command::new("/w/gone/claude"), "", &|| true, &|_| {});
        assert_eq!(ended, Some(Err("ask: claude is not on the PATH".to_owned())));
    }

    /// A turn given up kills `claude`, and ends with nothing to say.
    #[test]
    fn a_turn_given_up_kills_claude() {
        let mut sh = Command::new("sh");
        sh.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let ended = talk(sh, "", &|| started.elapsed() < Duration::from_millis(50), &|_| {});
        assert_eq!(ended, None);
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }
}
