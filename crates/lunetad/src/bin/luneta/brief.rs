//! The brief: what the agents are doing, in a few lines a fast model writes,
//! for coming back to them after a break.
//!
//! The sidebar knows where each agent runs, how it stands and the branch it
//! works on. The end of its transcript says what it is doing: the title Claude
//! Code gave it, what you last asked of it, and what it said and ran since.
//! A key of the menu's Brief group asks for a brief of the agents it covers,
//! by the row highlighted: luneta reads the end of each one's transcript,
//! hands it all to `claude -p`, and shows what comes back in a box over
//! zellij, until a key closes it. Nothing is kept: each key asks again.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lunetad::program::{self, Ran};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::gruvbox;
use crate::host::Event;
use crate::menu;
use crate::sidebar;

/// The model that writes the brief: a fast one, for a brief in seconds.
const MODEL: &str = "haiku";

/// How long the model has to write a brief. One takes a few seconds: one that
/// takes longer than this is stuck, and is killed.
const WRITING: Duration = Duration::from_mins(2);

/// How long `date` has to say the time.
const CLOCK: Duration = Duration::from_secs(1);

/// How long a brief given up waits for the thread it is asked on to end, and
/// `claude` with it: the look at whether it is still wanted, which comes each
/// few milliseconds, and its end once it is killed.
const GIVING_UP: Duration = Duration::from_millis(100);

/// Variables that the model must not have: with them, the daemon places it in
/// the pane they name, as an agent of its own.
const LEFT_OUT: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

/// How much of the end of a transcript to look through. A transcript runs to
/// megabytes, and most of its end is what Claude Code asked of the API and
/// what a tool answered.
const WINDOW_BYTES: u64 = 2 * 1024 * 1024;

/// The most of the records `digest` reads that a brief keeps of the ends of
/// the transcripts it covers, together, each an equal share: enough of each
/// for the steps the notes tell, and no more for a brief of many agents to
/// read through.
const TAILS_BYTES: usize = 512 * 1024;

/// The records of the kinds `digest` reads, by the `type` each names.
const KINDS: [&str; 4] =
    [r#""type":"ai-title""#, r#""type":"last-prompt""#, r#""type":"user""#, r#""type":"assistant""#];

/// The most agents a brief tells of.
const MAX_NOTES: usize = 32;

/// How long a title, a prompt and a step of what an agent did can run in the
/// notes, in characters.
const TITLE_CHARS: usize = 120;
const ASKED_CHARS: usize = 500;
const STEP_CHARS: usize = 300;

/// The most the notes can take, in bytes: they go to `claude` as one argument,
/// and Linux takes no argument longer than 128 KiB.
const NOTES_BYTES: usize = 100 * 1024;

/// What the notes keep of `NOTES_BYTES` for saying which agents they leave
/// out.
const LEFT_OUT_BYTES: usize = 4 * 1024;

/// The fewest columns the box takes, where zellij has as many: room for the
/// lines the model is asked to keep under 100 characters, which then do not
/// wrap.
const WIDTH_MIN: u16 = 60;

/// What the model is told, whatever the brief covers.
const SYSTEM: &str = "You brief a developer who is coming back to their desk after a break. They \
    left Claude Code agents at work in zellij sessions, and want to know at a glance how each \
    is doing. The notes you get say where each agent runs, its status and its branch, and end \
    with what its transcript said last. Write plain text for a terminal: no markdown, no \
    headings, no bold, no code fences. Say what each agent is working on and where it stands, \
    in the words of the work and not of the tools it ran. Waiting means it asked the \
    developer something and stopped: say what it needs. Idle means it finished its turn: say \
    what it got done, or what it waits on. Busy means it is still at work: say on what. Speak \
    to the developer as you. Use only what the notes say, and where they say nothing, say so \
    in a few words rather than guess.";

/// What a key of the Brief group covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every session, a line each.
    Every,
    /// The session of the row highlighted, a line for each agent in it.
    Session,
    /// The row highlighted: the agent in its pane, in a few lines, or a
    /// session's row, as `Session` does.
    Highlighted,
}

impl Scope {
    /// Each, in the order the Brief group shows them.
    pub const ALL: [Scope; 3] = [Scope::Every, Scope::Session, Scope::Highlighted];

    pub const fn key(self) -> char {
        match self {
            Scope::Every => 'z',
            Scope::Session => 's',
            Scope::Highlighted => 'h',
        }
    }

    /// What the menu says the key does.
    pub const fn does(self) -> &'static str {
        match self {
            Scope::Every => "Every session",
            Scope::Session => "This session",
            Scope::Highlighted => "Highlighted",
        }
    }
}

/// What a brief covers, once the sidebar has found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Over {
    /// Every session, in the sidebar's order: one with no agent in it too.
    Sessions(Vec<String>),
    /// A session, a line for each agent in it.
    Session(String),
    /// The agent in a pane of a session, in a few lines.
    Agent(String, u32),
}

impl Over {
    /// Whether the brief covers the agent `note` is of.
    fn covers(&self, note: &Note) -> bool {
        match self {
            Over::Sessions(_) => true,
            Over::Session(session) => note.session == *session,
            Over::Agent(session, pane) => note.session == *session && note.pane == *pane,
        }
    }

    /// How many steps of what each agent did the notes tell: the fewer
    /// agents a brief covers, the more of each.
    fn steps(&self) -> usize {
        match self {
            Over::Sessions(_) => 4,
            Over::Session(_) => 8,
            Over::Agent(..) => 24,
        }
    }

    /// What the model is asked to write.
    fn ask(&self) -> String {
        match self {
            Over::Sessions(_) => "Write a first line that says how many sessions the developer \
                has open, then one line per session, in the order the notes give them, as \
                \"- <session>: <what is going on there>\". Each line is one short sentence, \
                under 100 characters. A session with several agents gets one line that covers \
                them all, with what waits for the developer first, and one with no agent gets \
                \"- <session>: no agents\". Nothing else. For example:\n\
                You have 3 sessions open:\n\
                - cipher: an agent is waiting for CI to finish, to report how it went\n\
                - luneta: you were folding sessions in the sidebar; it is done and waits for \
                your review\n\
                - dotfiles: your nvim config is fixed"
                .to_owned(),
            Over::Session(session) => format!(
                "Write a first line that says how many agents session {session} has, then one \
                 line per agent, in the order the notes give them, as \"- <agent>: <what it is \
                 doing, and what it needs from you, if anything>\". Each line is one short \
                 sentence, under 120 characters. Nothing else."
            ),
            Over::Agent(..) => "Write three to five short lines on this one agent: what it was \
                asked to do, what it has done, where it stands now, and what it needs from you, \
                if anything. Nothing else."
                .to_owned(),
        }
    }
}

/// The id Claude Code gives a session, which names its transcript: a UUID's
/// hex digits and dashes, and nothing that would take the file it names out
/// of its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(id: &str) -> Option<Self> {
        let uuid = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
        uuid.then(|| SessionId(id.to_owned()))
    }
}

/// What the sidebar knows of an agent, for the brief.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub session: String,
    pub pane: u32,
    /// What the agent's row in the sidebar is named.
    pub name: String,
    pub status: String,
    /// Since when it has been in that status, where Claude Code says.
    pub since: Option<SystemTime>,
    pub branch: Option<String>,
    pub cwd: String,
    /// The id of its Claude Code session, which names its transcript.
    pub transcript: Option<SessionId>,
}

/// A brief asked for: what it covers, and the agents it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brief {
    over: Over,
    notes: Vec<Note>,
    /// What the box says the brief is of.
    title: String,
}

impl Brief {
    /// The brief of the agents in `notes` that `over` covers, or what the
    /// sidebar says when there is none. Two agents a session names alike are
    /// told apart by their panes, as the agent list tells them apart:
    /// `luneta/main:4`.
    pub fn new(over: Over, notes: Vec<Note>) -> Result<Self, String> {
        let mut notes: Vec<Note> =
            notes.into_iter().filter(|note| over.covers(note)).collect();
        let alike: Vec<bool> = notes
            .iter()
            .map(|note| {
                let named = |other: &&Note| other.session == note.session && other.name == note.name;
                notes.iter().filter(named).count() > 1
            })
            .collect();
        for (note, alike) in notes.iter_mut().zip(alike) {
            if alike {
                note.name = format!("{}:{}", note.name, note.pane);
            }
        }
        let Some(first) = notes.first() else {
            return Err(match &over {
                Over::Sessions(_) => "brief: no agents to brief you on".to_owned(),
                Over::Session(session) => format!("brief: no agents in {session}"),
                Over::Agent(..) => "brief: no agent in this pane".to_owned(),
            });
        };
        let title = match &over {
            Over::Sessions(_) => "Brief of every session".to_owned(),
            Over::Session(session) => format!("Brief of {session}"),
            // A pane has one agent at the most.
            Over::Agent(session, _) => format!("Brief of {}, in {session}", first.name),
        };
        Ok(Brief { over, notes, title })
    }

    /// What the box says the brief is of.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The end of each transcript the brief has room for, by its id, each
    /// its share of `TAILS_BYTES`, as `projects` has them: in a directory
    /// named for where its session started, which its id alone finds. One
    /// that is not found there, or does not read, has none.
    fn ends(&self, projects: &Path) -> HashMap<String, String> {
        let ids: Vec<&SessionId> = self
            .notes
            .iter()
            .take(MAX_NOTES)
            .filter_map(|note| note.transcript.as_ref())
            .collect();
        let share = TAILS_BYTES / ids.len().max(1);
        let mut dirs: Vec<PathBuf> = fs::read_dir(projects)
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        dirs.sort();
        let mut ends = HashMap::new();
        for SessionId(id) in ids {
            let file = format!("{id}.jsonl");
            let Some(path) = dirs.iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
            else {
                continue;
            };
            if let Ok(end) = end(&path, share) {
                ends.insert(id.clone(), end);
            }
        }
        ends
    }

    /// What the model is asked, with the notes on each agent and what the
    /// end of its transcript in `ends` says of it, for as many agents as the
    /// notes have room for, and how many they leave out. How long each has
    /// been in its status is told as of `now`.
    fn prompt(&self, ends: &HashMap<String, String>, now: SystemTime) -> String {
        let mut prompt = self.over.ask();
        prompt.push_str("\n\nThe notes:\n");
        if let Over::Sessions(sessions) = &self.over {
            prompt.push_str("\nSessions, in order: ");
            prompt.push_str(&sessions.join(", "));
            prompt.push('\n');
        }
        let mut told = 0;
        for note in self.notes.iter().take(MAX_NOTES) {
            let written = format!("\n{}", note.written(&self.told(note, ends), now));
            if prompt.len() + written.len() > NOTES_BYTES - LEFT_OUT_BYTES {
                break;
            }
            prompt.push_str(&written);
            told += 1;
        }
        let left_out = &self.notes[told..];
        if !left_out.is_empty() {
            prompt.push_str(&leaves_out(left_out));
        }
        cut_bytes(prompt, NOTES_BYTES)
    }

    /// What the end of `note`'s transcript, among `ends`, told of it.
    fn told(&self, note: &Note, ends: &HashMap<String, String>) -> Told {
        let Some(SessionId(id)) = &note.transcript else {
            return Told::NoId;
        };
        match ends.get(id) {
            Some(end) => Told::Read(digest(end, self.over.steps())),
            None => Told::NotFound,
        }
    }
}

/// The last records of the kinds `digest` reads among the last `WINDOW_BYTES`
/// of the transcript at `path`, whole ones alone, as many as `share` bytes
/// have room for. The window starts partway into a record, which is no
/// record, and partway into a character, which reads as `�`.
fn end(path: &Path, share: usize) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(WINDOW_BYTES)))?;
    let mut window = Vec::new();
    file.take(WINDOW_BYTES).read_to_end(&mut window)?;
    let window = String::from_utf8_lossy(&window);
    let told_of = |record: &&str| KINDS.iter().any(|kind| record.contains(kind));
    let mut kept: Vec<&str> = Vec::new();
    let mut taken = 0;
    for record in window.lines().rev().filter(told_of) {
        taken += record.len() + 1;
        if taken > share {
            break;
        }
        kept.push(record);
    }
    kept.reverse();
    Ok(kept.join("\n"))
}

/// What the notes say of the agents in `left_out` that they have no room
/// for, so that a session whose agents they leave out is not taken for one
/// with none.
fn leaves_out(left_out: &[Note]) -> String {
    let mut sessions: Vec<&str> = left_out.iter().map(|note| note.session.as_str()).collect();
    sessions.dedup();
    let agents = if left_out.len() == 1 { "agent" } else { "agents" };
    let of = if sessions.len() == 1 { "session" } else { "sessions" };
    format!(
        "\nThe notes leave out {} more {agents} for room, in {of} {}. Say that the brief leaves \
         them out, and do not call their sessions empty.\n",
        left_out.len(),
        sessions.join(", ")
    )
}

impl Note {
    /// The note as the model reads it at `now`, with what its transcript
    /// `told`.
    fn written(&self, told: &Told, now: SystemTime) -> String {
        let mut lines = vec![format!("Agent \"{}\" in session {}", self.name, self.session)];
        lines.push(match self.since {
            Some(since) => format!("Status: {}, for {}", self.status, sidebar::age(since, now)),
            None => format!("Status: {}", self.status),
        });
        lines.push(match &self.branch {
            Some(branch) => format!("Branch: {branch}, in {}", self.cwd),
            None => format!("Works in {}", self.cwd),
        });
        match told {
            Told::NoId => {
                lines.push("Its transcript is unknown: Claude Code names none for it.".to_owned());
            },
            Told::NotFound => lines.push("Its transcript was not found.".to_owned()),
            Told::Read(digest) => lines.extend(digest.lines()),
        }
        lines.join("\n") + "\n"
    }
}

/// What the end of an agent's transcript told of it.
enum Told {
    /// Claude Code gave no id for its session, or one that names no
    /// transcript.
    NoId,
    /// No transcript has its id in the folder the daemon reads Claude Code's
    /// sessions in: one not written yet, or there was no folder to look in.
    NotFound,
    Read(Digest),
}

/// What the end of a transcript says of what its agent is doing.
#[derive(Debug, PartialEq, Eq)]
struct Digest {
    /// The title Claude Code gave the session.
    title: Option<String>,
    /// What you last asked of it.
    asked: Option<String>,
    /// What it said and ran last, oldest first.
    steps: Vec<String>,
}

impl Digest {
    /// What the note says of what the transcript says.
    fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(title) = &self.title {
            lines.push(format!("Title: {title}"));
        }
        if let Some(asked) = &self.asked {
            // The notes name the developer as the model is told of them: a
            // `you` here reads as the agent asking.
            lines.push(format!("The developer last asked it: {asked}"));
        }
        if self.steps.is_empty() {
            lines.push("Its transcript says nothing more.".to_owned());
        } else {
            lines.push("Lately, oldest first:".to_owned());
            lines.extend(self.steps.iter().map(|step| format!("- {step}")));
        }
        lines
    }
}

/// What `end`, the end of a transcript, says, with as many of the last steps
/// as `steps`. A transcript is a JSON record to a line. A subagent's records,
/// what Claude thought and what a tool answered are left out.
fn digest(end: &str, steps: usize) -> Digest {
    let mut title = None;
    let mut asked = None;
    let mut last: VecDeque<String> = VecDeque::with_capacity(steps + 1);
    for line in end.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        match record["type"].as_str() {
            Some("ai-title") => title = record["aiTitle"].as_str().map(str::to_owned).or(title),
            // Claude Code records the last prompt some records after you type
            // it, and a slash command only there: the later of the two is
            // what you last asked.
            Some("last-prompt") => {
                asked = record["lastPrompt"].as_str().map(str::to_owned).or(asked);
            },
            Some("user") if record["isMeta"].as_bool() != Some(true) => {
                asked = typed_by_you(&record["message"]["content"]).or(asked);
            },
            Some("assistant") => {
                let blocks = record["message"]["content"].as_array().into_iter().flatten();
                for step in blocks.filter_map(step) {
                    last.push_back(step);
                    if last.len() > steps {
                        last.pop_front();
                    }
                }
            },
            _ => {},
        }
    }
    Digest {
        title: title.map(|title| one_line(&title, TITLE_CHARS)),
        asked: asked.map(|asked| one_line(&asked, ASKED_CHARS)),
        steps: last.into(),
    }
}

/// How the line Claude Code writes in your place where you stopped it starts.
const INTERRUPTED: &str = "[Request interrupted by user";

/// What you typed, in a user record: its text, and not what a tool answered,
/// what Claude Code wrapped in tags of its own, as a command's output, or the
/// line it writes in your place where you stopped it.
fn typed_by_you(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => Some(text.as_str()),
        Value::Array(blocks) => {
            blocks.iter().find(|b| b["type"] == "text").and_then(|b| b["text"].as_str())
        },
        _ => None,
    }?;
    let text = text.trim();
    let theirs = text.starts_with('<') || text.starts_with(INTERRUPTED);
    (!text.is_empty() && !theirs).then(|| text.to_owned())
}

/// A step of what an agent did, from a block of what it sent: what it said,
/// or what it ran and on what.
fn step(block: &Value) -> Option<String> {
    match block["type"].as_str()? {
        "text" => {
            let text = block["text"].as_str()?.trim();
            (!text.is_empty()).then(|| format!("said: {}", one_line(text, STEP_CHARS)))
        },
        "tool_use" => {
            let name = block["name"].as_str()?;
            Some(match on_what(&block["input"]) {
                Some(what) => format!("ran {name}: {}", one_line(what, STEP_CHARS)),
                None => format!("ran {name}"),
            })
        },
        _ => None,
    }
}

/// What a tool was run on, by the field that says it for the tools Claude
/// Code has: what a command or a subagent is for, which says more than the
/// command does, a command, a file, a pattern, a question asked of you.
fn on_what(input: &Value) -> Option<&str> {
    const FIELDS: [&str; 7] =
        ["description", "command", "file_path", "pattern", "url", "query", "prompt"];
    FIELDS
        .iter()
        .find_map(|field| input[field].as_str())
        .or_else(|| input["questions"][0]["question"].as_str())
}

/// `text` on one line, its runs of white space one space each, and no longer
/// than `chars`, ending in `…` where it was cut.
fn one_line(text: &str, chars: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let line = words.join(" ");
    if line.chars().count() <= chars {
        return line;
    }
    let mut cut: String = line.chars().take(chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `text` no longer than `bytes`, cut at a character.
fn cut_bytes(mut text: String, bytes: usize) -> String {
    if text.len() > bytes {
        let at = (0..=bytes).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        text.truncate(at);
    }
    text
}

/// What came of a brief asked for: the time it was asked at, as hours and
/// minutes, and what the model wrote, or why it wrote nothing.
#[derive(Debug)]
pub struct Written {
    at: Option<String>,
    said: Result<String, String>,
}

/// Asks for `brief`, numbered `number`, on a thread of its own: reads the end
/// of each transcript it covers in `claude_folder`, the folder the daemon
/// reads Claude Code's sessions in, has the model write the brief, and hands
/// back what it wrote, or why it wrote nothing. The briefing that shows it
/// gives it up as it goes, the model with it.
pub fn ask(
    number: u64,
    brief: Brief,
    claude_folder: Option<PathBuf>,
    events: &Sender<Event>,
) -> Briefing {
    let events = events.clone();
    let title = brief.title().to_owned();
    let asking = Asking::on_thread(move |wanted| {
        let at = clock();
        let projects = claude_folder.map(|folder| folder.join("projects"));
        let ends = projects.map(|projects| brief.ends(&projects)).unwrap_or_default();
        let Some(said) = write(&brief.prompt(&ends, SystemTime::now()), wanted) else {
            return;
        };
        // luneta is on its way out.
        let _ = events.send(Event::Briefed { number, written: Written { at, said } });
    });
    Briefing { number, title, written: None, _asking: asking }
}

/// The thread something is asked of `claude` on, which gives it up once it
/// is dropped: `claude` is killed, if it has yet to end, by the time the drop
/// returns, or else soon after, so that a luneta on its way out leaves none
/// behind it.
#[derive(Debug)]
pub struct Asking {
    /// Whether it was given up, which the thread looks at.
    given_up: Arc<AtomicBool>,
    /// Closed as the thread ends.
    ended: Receiver<()>,
}

impl Asking {
    /// Runs `ask` on a thread of its own, which it hands whether what it
    /// asks is still wanted, to look at as it goes: until the asking is
    /// dropped.
    pub fn on_thread(ask: impl FnOnce(&dyn Fn() -> bool) + Send + 'static) -> Self {
        let given_up = Arc::new(AtomicBool::new(false));
        let (going, ended) = mpsc::channel();
        let looked_at = Arc::clone(&given_up);
        thread::spawn(move || {
            // Dropped as the thread ends, which the drop waits for.
            let _going: Sender<()> = going;
            ask(&|| !looked_at.load(Ordering::Relaxed));
        });
        Asking { given_up, ended }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.given_up.store(true, Ordering::Relaxed);
        let _ = self.ended.recv_timeout(GIVING_UP);
    }
}

/// The time as `date` says it, in hours and minutes.
fn clock() -> Option<String> {
    let mut date = Command::new("date");
    date.arg("+%H:%M");
    let said = program::run(&mut date, CLOCK).ok()?.answer().ok()?;
    let time = String::from_utf8(said).ok()?.trim().to_owned();
    (!time.is_empty()).then_some(time)
}

/// What the model wrote on `prompt`, or why it wrote nothing, or none where
/// it was killed as no longer `wanted`. It runs with no tools, and in safe
/// mode, so that none of your hooks, `CLAUDE.md` or MCP servers come into it,
/// and with no zellij in its environment, so that the daemon places it in no
/// pane.
fn write(prompt: &str, wanted: &dyn Fn() -> bool) -> Option<Result<String, String>> {
    let mut claude = Command::new("claude");
    claude.args(["-p", "--model", MODEL, "--tools", "", "--safe-mode"]);
    claude.args(["--no-session-persistence", "--system-prompt", SYSTEM, prompt]);
    for name in LEFT_OUT {
        claude.env_remove(name);
    }
    program::run_while(&mut claude, WRITING, wanted).transpose().map(wrote)
}

/// What the model wrote, as `claude` `ran`, or why it wrote nothing: a brief
/// that says nothing would read as one with nothing to tell.
fn wrote(ran: io::Result<Ran>) -> Result<String, String> {
    match ran {
        Ok(Ran::Ended { status, stdout, .. }) if status.success() => {
            let said = String::from_utf8_lossy(&stdout).trim_end().to_owned();
            if said.trim().is_empty() {
                return Err("brief: claude wrote nothing".to_owned());
            }
            Ok(said)
        },
        // What it said goes on either, as it fails.
        Ok(Ran::Ended { status, stdout, stderr }) => {
            let said = [stderr, stdout].map(|said| String::from_utf8_lossy(&said).trim().to_owned());
            let said = said.into_iter().find(|said| !said.is_empty());
            Err(format!("brief: claude failed: {}", said.unwrap_or_else(|| status.to_string())))
        },
        Ok(Ran::Stuck) => {
            Err(format!("brief: claude wrote nothing in {} seconds", WRITING.as_secs()))
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err("brief: claude is not on the PATH".to_owned())
        },
        Err(error) => Err(format!("brief: claude could not be run: {error}")),
    }
}

/// A brief over zellij, from when it is asked until a key closes it, which
/// gives it up.
#[derive(Debug)]
pub struct Briefing {
    /// Its number among the briefs luneta has asked for, which what comes of
    /// it carries.
    number: u64,
    title: String,
    /// What came of it, once it has.
    written: Option<Written>,
    _asking: Asking,
}

impl Briefing {
    pub fn number(&self) -> u64 {
        self.number
    }

    /// What came of it.
    pub fn written(&mut self, written: Written) {
        self.written = Some(written);
    }

    /// Whether `key` closes it: any key once it is written. While it is
    /// asked, a key you press is not the one that closes it once it shows,
    /// and `Esc` and `Ctrl c` alone close it, unwritten.
    pub fn closes(&self, key: &KeyEvent) -> bool {
        let gives_up = match key.code {
            KeyCode::Esc => key.modifiers.is_empty(),
            KeyCode::Char('c') => key.modifiers == KeyModifiers::CONTROL,
            _ => false,
        };
        self.written.is_some() || gives_up
    }

    /// Whether a click closes it: once it is written, a click anywhere does.
    pub fn clicked(&self) -> bool {
        self.written.is_some()
    }

    /// Draws it in a box over `over`, zellij's columns, as `boxed` does. The
    /// rule along its top says what it is a brief of, and once it is written,
    /// when it was asked. Under it, what the model wrote, or why it wrote
    /// nothing, or that it is asked, and then the keys that close it. What
    /// the box has no room for is cut, its last line a `…`.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect) {
        let Some((inner, room)) = room(over) else {
            return;
        };
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (mut lines, keys, at) = match &self.written {
            None => {
                let asking = Line::styled(format!(" Asking {MODEL}…"), dim);
                (vec![asking], ("<ESC>", "Close"), None)
            },
            Some(Written { at, said }) => {
                let lines = match said {
                    Ok(said) => paragraphs(said, inner, Style::new()),
                    Err(why) => paragraphs(why, inner, Style::new().fg(gruvbox::RED)),
                };
                (lines, ("<any key>", "Close"), at.as_ref().map(|at| format!("at {at}")))
            },
        };
        let footer = [Line::default(), menu::keys_line(inner, &[keys])];
        let lines_room = room.saturating_sub(footer.len());
        if lines.len() > lines_room {
            lines.truncate(lines_room.saturating_sub(1));
            lines.extend((lines_room > 0).then(|| Line::styled(" …", dim)));
        }
        lines.extend(footer);
        boxed(buffer, over, &self.title, at.as_deref().unwrap_or(""), &lines);
    }
}

/// The columns inside a box over `over`, zellij's, and the lines it has room
/// for, inside its edges: across most of zellij's columns, and no fewer than
/// `WIDTH_MIN` where zellij has as many. None where it has no room for one.
pub fn room(over: Rect) -> Option<(usize, usize)> {
    let (inner, room) = (width(over).checked_sub(2)?, over.height.checked_sub(2)?);
    Some((usize::from(inner), usize::from(room)))
}

/// How wide a box over `over` is, edges and all.
fn width(over: Rect) -> u16 {
    let share = u16::try_from(u32::from(over.width) * 7 / 10).unwrap_or(over.width);
    share.max(WIDTH_MIN).min(over.width)
}

/// Draws `lines` in a box over `over`, zellij's columns, as wide as `room`
/// says, in their middle, and as tall as the lines, as many as it has room
/// for, with what was there gone. The rule along its top says `title`, and
/// `right` at its end where it has room.
pub fn boxed(buffer: &mut Buffer, over: Rect, title: &str, right: &str, lines: &[Line<'_>]) {
    let Some((inner, room)) = room(over) else {
        return;
    };
    let lines = &lines[..lines.len().min(room)];
    let width = width(over);
    let height = u16::try_from(lines.len() + 2).map_or(over.height, |tall| tall.min(over.height));
    let x = over.x + (over.width - width) / 2;
    let y = over.y + (over.height - height) / 2;
    let area = Rect::new(x, y, width, height);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let plain = Style::reset().fg(gruvbox::FG1);
    for row in area.rows() {
        buffer.set_string(row.x, row.y, " ".repeat(usize::from(row.width)), plain);
    }
    let mut top = vec![Span::styled("┌", dim)];
    top.extend(menu::rule(title, right, inner).spans);
    top.push(Span::styled("┐", dim));
    buffer.set_line(area.x, area.y, &Line::from(top), area.width);
    let last = area.bottom() - 1;
    for (line, y) in lines.iter().zip(area.y + 1..last) {
        buffer.set_string(area.x, y, "│", dim);
        buffer.set_line(area.x + 1, y, line, area.width - 2);
        buffer.set_string(area.right() - 1, y, "│", dim);
    }
    buffer.set_string(area.x, last, format!("└{}┘", "─".repeat(inner)), dim);
}

/// `text` on lines `width` wide, a space in from the edge: each of its lines
/// broken between words where it is too long, and its blank ones kept. A
/// character that would move the cursor, or tell the terminal anything, is a
/// space.
pub fn paragraphs(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let line = line.trim_end();
        if line.is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(menu::wrap(line, width, style));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use serde_json::json;

    use super::*;

    fn note(session: &str, pane: u32, name: &str, transcript: Option<&str>) -> Note {
        Note {
            session: session.to_owned(),
            pane,
            name: name.to_owned(),
            status: "idle".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH),
            branch: (pane == 4).then(|| "feat/fold".to_owned()),
            cwd: format!("/w/{session}"),
            transcript: transcript.map(|id| SessionId::parse(id).expect("a UUID")),
        }
    }

    /// Three minutes after each note's agent came to its status.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_mins(3)
    }

    /// What the model reads of each agent: what the sidebar knows of it, and
    /// what the end of its transcript says, as luneta reads it in Claude
    /// Code's projects. The title is the last one, and what you last asked
    /// the later of the last prompt Claude Code recorded and the last one you
    /// typed: it records a prompt some records after you type it. The steps
    /// are the last few, oldest first, by what a tool was for over what it
    /// ran. The record the end starts partway into, a subagent's records,
    /// what Claude thought and what a tool answered are left out, and so is
    /// what Claude Code typed in tags of its own or in your place where you
    /// stopped it. A transcript not found says so, and so does an agent
    /// Claude Code names none for. Two agents a session names alike are told
    /// apart by their panes, and an agent of another session named alike is
    /// not.
    #[test]
    fn the_notes_tell_each_agent_by_the_end_of_its_transcript() {
        let fold = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";
        let quiet = "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a";
        let gone = "9e8d7c6b-5a4f-4e3d-2c1b-0a9f8e7d6c5b";
        let said = |text: &str| json!({"type": "assistant", "message": {"content": [
            {"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": text}]}});
        let ran = |name: &str, input: Value| json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": name, "input": input}]}});
        let fold_end = [
            json!({"type": "ai-title", "aiTitle": "Fold sessions"}),
            json!({"type": "user", "message": {"content": "fold the sessions"}}),
            said("Looking at the tree."),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "content": "src/tree.rs"}]}}),
            ran("Bash", json!({"command": "cargo test", "description": "Run the tests"})),
            json!({"type": "assistant", "isSidechain": true, "message": {"content": [
                {"type": "text", "text": "a subagent's step"}]}}),
            ran("Read", json!({"file_path": "/w/b/src/tree.rs"})),
            said("Folded:\n\n  every   session."),
            ran("AskUserQuestion", json!({"questions": [{"question": "Merge it?"}]})),
            json!({"type": "last-prompt", "lastPrompt": "fold them, and keep the cursor"}),
            json!({"type": "ai-title", "aiTitle": "Fold sessions in the sidebar"}),
        ];
        let quiet_end = [
            json!({"type": "last-prompt", "lastPrompt": "make it so"}),
            json!({"type": "user", "message": {"content": "make it so, and test it"}}),
            json!({"type": "user", "message": {"content": [
                {"type": "text", "text": "[Request interrupted by user]"}]}}),
            json!({"type": "user", "message": {"content": "<command-name>/clear</command-name>"}}),
            json!({"type": "user", "isMeta": true, "message": {"content": "a reminder"}}),
        ];
        let lines = |records: &[Value]| -> String {
            records.iter().map(|record| record.to_string() + "\n").collect()
        };
        let projects = tempfile::tempdir().expect("a directory");
        let project = projects.path().join("-w-b");
        fs::create_dir(&project).expect("a directory");
        fs::create_dir(projects.path().join("-w-a")).expect("a directory");
        let cut = r#"me": "cut short", "type":"user"}]}}"#;
        let fold_file = format!("{cut}\n{}", lines(&fold_end));
        fs::write(project.join(format!("{fold}.jsonl")), fold_file).expect("a file");
        fs::write(project.join(format!("{quiet}.jsonl")), lines(&quiet_end)).expect("a file");

        let over = Over::Sessions(vec!["a".to_owned(), "b".to_owned()]);
        let notes = vec![
            note("a", 1, "fold", None),
            note("b", 4, "fold", Some(fold)),
            note("b", 5, "quiet", Some(quiet)),
            note("b", 6, "spare", Some(gone)),
            Note { since: None, ..note("b", 7, "spare", None) },
        ];
        let brief = Brief::new(over, notes).expect("a brief");
        let prompt = brief.prompt(&brief.ends(projects.path()), now());
        let notes = prompt.split_once("The notes:\n").expect("the notes").1;
        assert_eq!(
            notes,
            "
Sessions, in order: a, b

Agent \"fold\" in session a
Status: idle, for 3m
Works in /w/a
Its transcript is unknown: Claude Code names none for it.

Agent \"fold\" in session b
Status: idle, for 3m
Branch: feat/fold, in /w/b
Title: Fold sessions in the sidebar
The developer last asked it: fold them, and keep the cursor
Lately, oldest first:
- ran Bash: Run the tests
- ran Read: /w/b/src/tree.rs
- said: Folded: every session.
- ran AskUserQuestion: Merge it?

Agent \"quiet\" in session b
Status: idle, for 3m
Works in /w/b
The developer last asked it: make it so, and test it
Its transcript says nothing more.

Agent \"spare:6\" in session b
Status: idle, for 3m
Works in /w/b
Its transcript was not found.

Agent \"spare:7\" in session b
Status: idle
Works in /w/b
Its transcript is unknown: Claude Code names none for it.
"
        );
    }

    /// A brief keeps no more than `TAILS_BYTES` of the transcripts it covers
    /// together, however many agents it covers. What it keeps is of the
    /// records the notes tell of, so that what Claude Code asked of the API,
    /// which takes most of the end of a transcript, leaves room for what each
    /// agent did last.
    #[test]
    fn a_brief_reads_a_share_of_each_transcript_of_what_the_notes_tell() {
        let projects = tempfile::tempdir().expect("a directory");
        let project = projects.path().join("-w");
        fs::create_dir(&project).expect("a directory");
        let asked = json!({"type": "api-request", "params": {"system": "x".repeat(64 * 1024)}});
        let ids: Vec<String> =
            (0..MAX_NOTES).map(|n| format!("{n:08x}-0000-4000-8000-000000000000")).collect();
        for (n, id) in ids.iter().enumerate() {
            let said = |step: usize| {
                let text = format!("agent {n}, step {step}: {}", "y".repeat(1024));
                json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
            };
            let mut records: Vec<String> = (0..64).map(|step| said(step).to_string()).collect();
            records.push(asked.to_string());
            let transcript = records.join("\n") + "\n";
            fs::write(project.join(format!("{id}.jsonl")), transcript).expect("a file");
        }

        let note = |(n, id): (usize, &String)| Note {
            session: "a".to_owned(),
            pane: n as u32,
            name: format!("agent {n}"),
            status: "busy".to_owned(),
            since: None,
            branch: None,
            cwd: "/w".to_owned(),
            transcript: Some(SessionId::parse(id).expect("a UUID")),
        };
        let notes = ids.iter().enumerate().map(note).collect();
        let brief = Brief::new(Over::Sessions(vec!["a".to_owned()]), notes).expect("a brief");
        let ends = brief.ends(projects.path());

        let kept: usize = ends.values().map(String::len).sum();
        assert!(kept <= TAILS_BYTES, "{kept}");
        for (n, id) in ids.iter().enumerate() {
            let digest = digest(&ends[id], Over::Sessions(Vec::new()).steps());
            let last = digest.steps.last().map(String::as_str).unwrap_or_default();
            assert!(last.starts_with(&format!("said: agent {n}, step 63: ")), "{last}");
        }
    }

    /// A brief tells of as many agents as it has room for, in the order the
    /// sidebar gives them, and says how many it leaves out and in which
    /// sessions, so that the model does not call a session whose agents it
    /// left out empty: past `MAX_NOTES` agents, and past `NOTES_BYTES` of
    /// notes, which go to `claude` as one argument.
    #[test]
    fn a_brief_says_how_many_agents_it_has_no_room_for() {
        let note = |session: &str, pane: usize, name: String| Note {
            session: session.to_owned(),
            pane: pane as u32,
            name,
            status: "busy".to_owned(),
            since: None,
            branch: None,
            cwd: "/w".to_owned(),
            transcript: None,
        };
        let session = |pane: usize| if pane < MAX_NOTES { "a" } else { "b" };
        let many = (0..MAX_NOTES + 2).map(|pane| note(session(pane), pane, format!("agent {pane}")));
        let long = |session: &str, pane: usize| note(session, pane, "x".repeat(NOTES_BYTES / 2));
        for (notes, told, said) in [
            (many.collect(), MAX_NOTES, "The notes leave out 2 more agents for room, in session b."),
            (vec![long("a", 0), long("b", 1)], 1, "leave out 1 more agent for room, in session b."),
        ] {
            let over = Over::Sessions(vec!["a".to_owned(), "b".to_owned()]);
            let prompt = Brief::new(over, notes).expect("a brief").prompt(&HashMap::new(), now());
            assert_eq!(prompt.matches("\nAgent \"").count(), told, "{said}");
            assert!(prompt.contains(said), "{said}");
            assert!(prompt.len() <= NOTES_BYTES, "{said}");
        }
    }

    /// A transcript's id names a file in Claude Code's folder, and one that
    /// would name a file elsewhere names none.
    #[test]
    fn a_transcript_s_id_is_a_uuid_s_digits_and_dashes() {
        assert!(SessionId::parse("0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60").is_some());
        for id in ["", "../0b7c5e1a", "0b7c5e1a/x", "id-1"] {
            assert_eq!(SessionId::parse(id), None, "{id}");
        }
    }

    /// What `claude` wrote is the brief, and a run that wrote nothing but
    /// blanks is none: an empty box would read as a brief with nothing to
    /// tell.
    #[test]
    fn a_run_of_claude_that_wrote_nothing_is_no_brief() {
        let ended = |stdout: &str| {
            let (status, stdout) = (ExitStatus::from_raw(0), stdout.as_bytes().to_vec());
            Ok(Ran::Ended { status, stdout, stderr: Vec::new() })
        };
        let brief = "You have 1 session open:\n- luneta: the brief is back";
        assert_eq!(wrote(ended(&format!("{brief}\n"))), Ok(brief.to_owned()));
        assert_eq!(wrote(ended(" \n\n")), Err("brief: claude wrote nothing".to_owned()));
    }

    /// The brief numbered `number`, of `title`, asked on a thread that has
    /// already ended.
    fn asked(number: u64, title: &str) -> Briefing {
        let asking = Asking { given_up: Arc::default(), ended: mpsc::channel().1 };
        Briefing { number, title: title.to_owned(), written: None, _asking: asking }
    }

    /// The box over zellij's columns, `width` by `height`, as text.
    fn shown(briefing: &Briefing, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        // What zellij drew there goes under the box.
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        briefing.draw(&mut buffer, area);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// While the brief is asked, its box says so, and only `Esc` closes it,
    /// unwritten. Once it is written, the box shows what the model wrote,
    /// and when it was asked, and any key or a click closes it.
    #[test]
    fn a_brief_shows_over_zellij_until_a_key_closes_it() {
        let mut briefing = asked(1, "Brief of every session");
        assert_eq!(
            shown(&briefing, 66, 9),
            [
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "zzz┌─ Brief of every session ─────────────────────────────────┐zzz",
                "zzz│ Asking haiku…                                            │zzz",
                "zzz│                                                          │zzz",
                "zzz│ <ESC> - Close                                            │zzz",
                "zzz└──────────────────────────────────────────────────────────┘zzz",
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            ]
        );
        assert!(!briefing.closes(&key(KeyCode::Char('j'))));
        assert!(!briefing.clicked());
        assert!(briefing.closes(&key(KeyCode::Esc)));

        let said = "You have 2 sessions open:\n- luneta: the brief\tis back\x1b[31m\n\n- elo: no agents";
        briefing.written(Written { at: Some("18:42".to_owned()), said: Ok(said.to_owned()) });
        assert_eq!(
            shown(&briefing, 66, 9)[..8],
            [
                "zzz┌─ Brief of every session ────────────────────── at 18:42 ─┐zzz",
                "zzz│ You have 2 sessions open:                                │zzz",
                "zzz│ - luneta: the brief is back [31m                         │zzz",
                "zzz│                                                          │zzz",
                "zzz│ - elo: no agents                                         │zzz",
                "zzz│                                                          │zzz",
                "zzz│ <any key> - Close                                        │zzz",
                "zzz└──────────────────────────────────────────────────────────┘zzz",
            ]
        );
        assert!(briefing.closes(&key(KeyCode::Char('j'))));
        assert!(briefing.clicked());
    }

    /// What the box has no room for is cut, with the keys that close it
    /// kept, and what went wrong is said in its place.
    #[test]
    fn a_brief_too_long_for_its_box_is_cut_and_one_that_failed_says_why() {
        let mut briefing = asked(1, "Brief of luneta");
        let said = (1..=9).map(|n| format!("- line {n}")).collect::<Vec<_>>().join("\n");
        briefing.written(Written { at: None, said: Ok(said) });
        assert_eq!(
            shown(&briefing, 40, 8),
            [
                "┌─ Brief of luneta ────────────────────┐",
                "│ - line 1                             │",
                "│ - line 2                             │",
                "│ - line 3                             │",
                "│ …                                    │",
                "│                                      │",
                "│ <any key> - Close                    │",
                "└──────────────────────────────────────┘",
            ]
        );
        let mut briefing = asked(2, "Brief of luneta");
        let why = "brief: claude is not on the PATH".to_owned();
        briefing.written(Written { at: Some("18:42".to_owned()), said: Err(why) });
        assert_eq!(shown(&briefing, 40, 8)[2], "│ brief: claude is not on the PATH     │");
    }
}
