use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lune_domain::names::SessionName;
use luned::program::{self, Ran};
use luned::transcripts::{digest_end, Digest, Reading, SessionId, TranscriptEnd, IDS_MAX};
use luned::{one_answer, Destination};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::moonlight;
use crate::sidebar::{self, ViewKey};

const MODEL: &str = "haiku";

const WRITING: Duration = Duration::from_mins(2);

const CLOCK: Duration = Duration::from_secs(1);

const GIVING_UP: Duration = Duration::from_millis(100);

const UNREAD_HARNESS: &str = "lune reads only Claude Code's and Antigravity's transcripts so far";

const MAX_NOTES: usize = 32;
const _: () = assert!(MAX_NOTES <= IDS_MAX);

const NOTES_BYTES: usize = 100 * 1024;

const LEFT_OUT_BYTES: usize = 4 * 1024;

const WIDTH_MIN: u16 = 60;

const SYSTEM: &str = "You brief a developer who is coming back to their desk after a break. They \
    left Claude Code agents at work in lune's projects, and want to know at a glance how each \
    is doing. The notes you get say where each agent runs, its status and its branch, and end \
    with what its transcript said last. Write plain text for a terminal: no markdown, no \
    headings, no bold, no code fences. Say what each agent is working on and where it stands, \
    in the words of the work and not of the tools it ran. Waiting means it asked the \
    developer something and stopped: say what it needs. Idle means it finished its turn: say \
    what it got done, or what it waits on. Busy means it is still at work: say on what. Speak \
    to the developer as you. Use only what the notes say, and where they say nothing, say so \
    in a few words rather than guess.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Every,
    Session,
    Highlighted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Over {
    Sessions {
        sessions: Vec<(Machine, SessionName)>,
        unknown: Vec<(Destination, &'static str)>,
    },
    Machine(Machine, Vec<SessionName>),
    Session(Machine, SessionName),
    Agent(Machine, SessionName, ViewKey),
}

impl Over {
    fn covers(&self, note: &Note) -> bool {
        match self {
            Over::Sessions { .. } => true,
            Over::Machine(on, _) => note.on == *on,
            Over::Session(on, session) => note.on == *on && note.session == *session,
            Over::Agent(on, session, view) => {
                note.on == *on && note.session == *session && note.view == *view
            },
        }
    }

    fn steps(&self) -> usize {
        match self {
            Over::Sessions { .. } | Over::Machine(..) => 4,
            Over::Session(..) => 8,
            Over::Agent(..) => 24,
        }
    }

    fn ask(&self) -> String {
        match self {
            Over::Sessions { unknown, .. } => per_session("open", !unknown.is_empty()),
            Over::Machine(on, _) => per_session(&format!("open on {}", machine_named(on)), false),
            Over::Session(on, session) => format!(
                "Write a first line that says how many agents session {} has, then one line per \
                 agent, in the order the notes give them, as \"- <agent>: <what it is doing, and \
                 what it needs from you, if anything>\". Each line is one short sentence, under \
                 120 characters. Nothing else.",
                session_on(on, session)
            ),
            Over::Agent(..) => "Write three to five short lines on this one agent: what it was \
                asked to do, what it has done, where it stands now, and what it needs from you, \
                if anything. Nothing else."
                .to_owned(),
        }
    }
}

fn per_session(open: &str, unknown: bool) -> String {
    let unknown = if unknown {
        " Where they say the sessions of a machine are unknown, end with a line that says so."
    } else {
        ""
    };
    format!(
        "Write a first line that says how many sessions the developer has {open}, then one line \
         per session, in the order the notes give them, as \"- <session>: <what is going on \
         there>\". Each line is one short sentence, under 100 characters. A session with \
         several agents gets one line that covers them all, with what waits for the developer \
         first, and one with no agent gets \"- <session>: no agents\". The notes name a session \
         of another machine than this one as \"<session> on <machine>\": name it so.{unknown} \
         Nothing else. For example:\n\
         You have 3 sessions open:\n\
         - cipher: an agent is waiting for CI to finish, to report how it went\n\
         - lune: you were folding sessions in the sidebar; it is done and waits for your \
         review\n\
         - dotfiles on nixos: your nvim config is fixed"
    )
}

fn session_on(on: &Machine, session: &SessionName) -> String {
    match on {
        Machine::Local => session.to_string(),
        Machine::Remote(destination) => format!("{session} on {destination}"),
    }
}

fn machine_named(on: &Machine) -> String {
    match on {
        Machine::Local => "this machine".to_owned(),
        Machine::Remote(destination) => destination.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub on: Machine,
    pub session: SessionName,
    pub view: ViewKey,
    pub name: String,
    pub status: String,
    pub since: Option<SystemTime>,
    pub branch: Option<String>,
    pub cwd: String,
    pub transcript: Transcript,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transcript {
    Named(SessionId),
    Unnamed,
    NotRead,
}

impl Transcript {
    pub fn named(self) -> Option<SessionId> {
        match self {
            Transcript::Named(id) => Some(id),
            Transcript::Unnamed | Transcript::NotRead => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brief {
    over: Over,
    notes: Vec<Note>,
    title: String,
}

impl Brief {
    pub fn new(over: Over, notes: Vec<Note>) -> Result<Self, String> {
        let mut notes: Vec<Note> =
            notes.into_iter().filter(|note| over.covers(note)).collect();
        let alike: Vec<bool> = notes
            .iter()
            .map(|note| {
                let named = |other: &&Note| {
                    other.on == note.on && other.session == note.session && other.name == note.name
                };
                notes.iter().filter(named).count() > 1
            })
            .collect();
        for (note, alike) in notes.iter_mut().zip(alike) {
            if alike {
                note.name = format!("{}:{}", note.name, note.view);
            }
        }
        let Some(first) = notes.first() else {
            return Err(match &over {
                Over::Sessions { unknown, .. } if unknown.is_empty() => {
                    "brief: no agents to brief you on".to_owned()
                },
                Over::Sessions { unknown, .. } => {
                    let unknown: Vec<String> =
                        unknown.iter().map(|(machine, _)| machine.to_string()).collect();
                    format!(
                        "brief: no agents to brief you on; luned does not reach {} now",
                        unknown.join(", ")
                    )
                },
                Over::Machine(on, _) => format!("brief: no agents on {}", machine_named(on)),
                Over::Session(on, session) => {
                    format!("brief: no agents in {}", session_on(on, session))
                },
                Over::Agent(..) => "brief: no agent in this view".to_owned(),
            });
        };
        if notes.iter().all(|note| note.transcript == Transcript::NotRead) {
            return Err(format!("brief: {UNREAD_HARNESS}"));
        }
        let title = match &over {
            Over::Sessions { .. } => "Brief of every session".to_owned(),
            Over::Machine(on, _) => format!("Brief of every session on {}", machine_named(on)),
            Over::Session(on, session) => format!("Brief of {}", session_on(on, session)),
            Over::Agent(on, session, _) => {
                format!("Brief of {}, in {}", first.name, session_on(on, session))
            },
        };
        Ok(Brief { over, notes, title })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    fn told(&self, read: &Reads<'_>) -> Result<Vec<Told>, String> {
        let mut told: Vec<(usize, Told)> = Vec::new();
        let mut machines: HashMap<&Machine, Vec<(usize, SessionId)>> = HashMap::new();
        for (at, note) in self.notes.iter().take(MAX_NOTES).enumerate() {
            match &note.transcript {
                Transcript::Named(id) => {
                    machines.entry(&note.on).or_default().push((at, id.clone()));
                },
                Transcript::Unnamed => told.push((at, Told::NoId)),
                Transcript::NotRead => {
                    told.push((at, Told::Unread(UNREAD_HARNESS.to_owned())));
                },
            }
        }
        let groups = machines.into_iter().map(|(machine, placed)| {
            let ids = placed.iter().map(|(_, id)| id.clone()).collect();
            ((machine, placed), ids)
        });
        let readings = Reading::shared(groups.collect())?;
        let steps = self.over.steps();
        thread::scope(|scope| {
            let reads: Vec<_> = readings
                .into_iter()
                .map(|((machine, placed), reading)| {
                    (placed, scope.spawn(move || read(machine, &reading)))
                })
                .collect();
            let lost = |_| Err("the thread that read it ended without an answer".to_owned());
            for (placed, reading) in reads {
                let read = reading.join().unwrap_or_else(lost);
                told.extend(placed.into_iter().map(|(at, id)| match &read {
                    Ok(ends) => match ends.get(&id) {
                        Some(end) => (at, Told::Read(digest_end(end, steps))),
                        None => (at, Told::NotFound),
                    },
                    Err(why) => (at, Told::Unread(why.clone())),
                }));
            }
        });
        told.sort_by_key(|(at, _)| *at);
        Ok(told.into_iter().map(|(_, told)| told).collect())
    }

    fn prompt(&self, read: &Reads<'_>, now: fn() -> SystemTime) -> Result<String, String> {
        let transcripts = self.told(read)?;
        let now = now();
        let mut prompt = self.over.ask();
        prompt.push_str("\n\nThe notes:\n");
        let sessions: Option<Vec<String>> = match &self.over {
            Over::Sessions { sessions, .. } => {
                Some(sessions.iter().map(|(on, session)| session_on(on, session)).collect())
            },
            Over::Machine(on, sessions) => {
                Some(sessions.iter().map(|session| session_on(on, session)).collect())
            },
            Over::Session(..) | Over::Agent(..) => None,
        };
        if let Some(sessions) = sessions {
            prompt.push_str("\nSessions, in order: ");
            prompt.push_str(&sessions.join(", "));
            prompt.push('\n');
        }
        if let Over::Sessions { unknown, .. } = &self.over
            && !unknown.is_empty()
        {
            let unknown: Vec<String> =
                unknown.iter().map(|(machine, word)| format!("{machine} ({word})")).collect();
            prompt.push_str("Machines whose sessions are unknown, as luned does not reach them \
                             now: ");
            prompt.push_str(&unknown.join(", "));
            prompt.push('\n');
        }
        let mut told = 0;
        for (note, transcript) in self.notes.iter().zip(&transcripts) {
            let written = format!("\n{}", note.written(transcript, now));
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
        Ok(cut_bytes(prompt, NOTES_BYTES))
    }
}

type Reads<'a> =
    dyn Fn(&Machine, &Reading) -> Result<HashMap<SessionId, TranscriptEnd>, String> + Sync + 'a;

pub fn read(on: &Machine, reading: &Reading) -> Result<HashMap<SessionId, TranscriptEnd>, String> {
    Away::on(on, away::ANSWER)?.ends(reading)
}

fn leaves_out(left_out: &[Note]) -> String {
    let mut sessions: Vec<String> =
        left_out.iter().map(|note| session_on(&note.on, &note.session)).collect();
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
    fn written(&self, told: &Told, now: SystemTime) -> String {
        let session = session_on(&self.on, &self.session);
        let mut lines = vec![format!("Agent \"{}\" in session {session}", self.name)];
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
            Told::Unread(why) => lines.push(format!("Its transcript was not read: {why}")),
            Told::Read(digest) => lines.extend(digest.lines()),
        }
        lines.join("\n") + "\n"
    }
}

enum Told {
    NoId,
    NotFound,
    Unread(String),
    Read(Digest),
}

fn cut_bytes(mut text: String, bytes: usize) -> String {
    if text.len() > bytes {
        let at = (0..=bytes).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        text.truncate(at);
    }
    text
}

#[derive(Debug)]
pub struct Written {
    at: Option<String>,
    said: Result<String, String>,
}

pub fn ask(
    number: u64,
    brief: Brief,
    events: &Sender<Event>,
) -> Briefing {
    let title = brief.title().to_owned();
    let due = Due::new(events, move |written: Result<Written, String>| {
        let unanswered = |why| Written { at: None, said: Err(format!("brief: {why}")) };
        Event::Briefed { number, written: written.unwrap_or_else(unanswered) }
    });
    let asking = Asking::on_thread(move |wanted| {
        let at = clock();
        let prompt = brief.prompt(&read, SystemTime::now);
        if !wanted() {
            return;
        }
        let said = match prompt {
            Ok(prompt) => {
                let Some(said) = write(&prompt, wanted) else {
                    return;
                };
                said
            },
            Err(why) => Err(format!("brief: {why}")),
        };
        due.pay(Ok(Written { at, said }));
    });
    Briefing { number, title, written: None, _asking: asking }
}

#[derive(Debug)]
pub struct Asking {
    given_up: Arc<AtomicBool>,
    ended: Receiver<()>,
}

impl Asking {
    pub fn on_thread(ask: impl FnOnce(&dyn Fn() -> bool) + Send + 'static) -> Self {
        let given_up = Arc::new(AtomicBool::new(false));
        let (going, ended) = mpsc::channel();
        let looked_at = Arc::clone(&given_up);
        thread::spawn(move || {
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

fn clock() -> Option<String> {
    let mut date = Command::new("date");
    date.arg("+%H:%M");
    let said = program::run(&mut date, CLOCK).ok()?.answer().ok()?;
    let time = String::from_utf8(said).ok()?.trim().to_owned();
    (!time.is_empty()).then_some(time)
}

fn write(prompt: &str, wanted: &dyn Fn() -> bool) -> Option<Result<String, String>> {
    let ran = one_answer(MODEL, SYSTEM, prompt).and_then(|(mut claude, told)| {
        program::run_while_reading(&mut claude, WRITING, wanted, told)
    });
    ran.transpose().map(wrote)
}

fn wrote(ran: io::Result<Ran>) -> Result<String, String> {
    match ran {
        Ok(Ran::Ended { status, stdout, .. }) if status.success() => {
            let said = String::from_utf8_lossy(&stdout).trim_end().to_owned();
            if said.trim().is_empty() {
                return Err("brief: claude wrote nothing".to_owned());
            }
            Ok(said)
        },
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

#[derive(Debug)]
pub struct Briefing {
    number: u64,
    title: String,
    written: Option<Written>,
    _asking: Asking,
}

impl Briefing {
    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn written(&mut self, written: Written) {
        self.written = Some(written);
    }

    pub fn closes(&self, key: &KeyEvent) -> bool {
        let gives_up = match key.code {
            KeyCode::Esc => key.modifiers.is_empty(),
            KeyCode::Char('c') => key.modifiers == KeyModifiers::CONTROL,
            _ => false,
        };
        self.written.is_some() || gives_up
    }

    pub fn clicked(&self) -> bool {
        self.written.is_some()
    }

    pub fn turning(&self) -> bool {
        self.written.is_none()
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) {
        let Some((inner, room)) = room(over) else {
            return;
        };
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (mut lines, keys, at) = match &self.written {
            None => {
                let asking = format!("Asking {MODEL}…");
                let asking = menu::waiting(&asking, inner, dim, sidebar::frame(now));
                (asking, (Hint::Esc, "Close"), None)
            },
            Some(Written { at, said }) => {
                let lines = match said {
                    Ok(said) => paragraphs(said, inner, Style::new()),
                    Err(why) => paragraphs(why, inner, Style::new().fg(moonlight::RED)),
                };
                (lines, (Hint::AnyKey, "Close"), at.as_ref().map(|at| format!("at {at}")))
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

pub fn room(over: Rect) -> Option<(usize, usize)> {
    let (inner, room) = (width(over).checked_sub(2)?, over.height.checked_sub(2)?);
    Some((usize::from(inner), usize::from(room)))
}

fn width(over: Rect) -> u16 {
    let share = u16::try_from(u32::from(over.width) * 7 / 10).unwrap_or(over.width);
    share.max(WIDTH_MIN).min(over.width)
}

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
    menu::boxed(buffer, area, &menu::rule(title, right, inner), lines);
}

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
    use std::fs;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::sync::Mutex;

    use luned::api::TranscriptEndsRequest;
    use luned::transcripts::{Folders, TAILS_BYTES};
    use serde_json::{json, Value};

    use super::*;

    fn note(session: &str, view: u32, name: &str, transcript: Option<&str>) -> Note {
        Note {
            on: Machine::Local,
            session: named(session),
            view: ViewKey::terminal(view),
            name: name.to_owned(),
            status: "idle".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH),
            branch: (view == 4).then(|| "feat/fold".to_owned()),
            cwd: format!("/w/{session}"),
            transcript: transcript.map_or(Transcript::Unnamed, |id| {
                Transcript::Named(SessionId::parse(id).expect("a UUID"))
            }),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_mins(3)
    }

    fn named(session: &str) -> SessionName {
        SessionName::parse(session).expect("a session's name")
    }

    fn every(sessions: &[&str]) -> Over {
        let sessions = sessions.iter().map(|it| (Machine::Local, named(it))).collect();
        Over::Sessions { sessions, unknown: Vec::new() }
    }

    fn read_here(
        folders: Folders,
    ) -> impl Fn(&Machine, &Reading) -> Result<HashMap<SessionId, TranscriptEnd>, String> + Sync {
        move |_, reading| Ok(reading.ends(&folders))
    }

    #[expect(clippy::unnecessary_wraps, reason = "it is a `Reads`")]
    fn none_read(_: &Machine, _: &Reading) -> Result<HashMap<SessionId, TranscriptEnd>, String> {
        Ok(HashMap::new())
    }

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
        let claude = tempfile::tempdir().expect("a directory");
        let projects = claude.path().join("projects");
        let project = projects.join("-w-b");
        fs::create_dir_all(&project).expect("a directory");
        fs::create_dir_all(projects.join("-w-a")).expect("a directory");
        let cut = r#"me": "cut short", "type":"user"}]}}"#;
        let fold_file = format!("{cut}\n{}", lines(&fold_end));
        fs::write(project.join(format!("{fold}.jsonl")), fold_file).expect("a file");
        fs::write(project.join(format!("{quiet}.jsonl")), lines(&quiet_end)).expect("a file");

        let notes = vec![
            note("a", 1, "fold", None),
            note("b", 4, "fold", Some(fold)),
            note("b", 5, "quiet", Some(quiet)),
            note("b", 6, "spare", Some(gone)),
            Note { since: None, ..note("b", 7, "spare", None) },
        ];
        let brief = Brief::new(every(&["a", "b"]), notes).expect("a brief");
        let folders = Folders { claude: Some(claude.path().to_owned()), agy: None };
        let prompt = brief.prompt(&read_here(folders), now).expect("a reading");
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

    #[test]
    fn a_brief_reads_a_share_of_each_transcript_of_what_the_notes_tell() {
        let claude = tempfile::tempdir().expect("a directory");
        let project = claude.path().join("projects/-w");
        fs::create_dir_all(&project).expect("a directory");
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
            on: Machine::Local,
            session: named("a"),
            view: ViewKey::terminal(n as u32 + 1),
            name: format!("agent {n}"),
            status: "busy".to_owned(),
            since: None,
            branch: None,
            cwd: "/w".to_owned(),
            transcript: Transcript::Named(SessionId::parse(id).expect("a UUID")),
        };
        let notes = ids.iter().enumerate().map(note).collect();
        let brief = Brief::new(every(&["a"]), notes).expect("a brief");
        let kept = Mutex::new(0);
        let folders = Folders { claude: Some(claude.path().to_owned()), agy: None };
        let here = read_here(folders);
        let told = brief.told(&|on, reading| {
            let ends = here(on, reading)?;
            *kept.lock().expect("no panic") += ends.values().map(|end| end.lines().len()).sum::<usize>();
            Ok(ends)
        });

        let kept = kept.into_inner().expect("no panic");
        assert!(kept <= TAILS_BYTES, "{kept}");
        for (n, told) in told.expect("a reading").iter().enumerate() {
            let Told::Read(digest) = told else {
                panic!("agent {n}: its transcript was not read");
            };
            let last = digest.steps.last().map_or_default(String::as_str);
            assert!(last.starts_with(&format!("said: agent {n}, step 63: ")), "{last}");
        }
    }

    #[test]
    fn a_brief_says_how_many_agents_it_has_no_room_for() {
        let note = |session: &str, view: usize, name: String| Note {
            on: Machine::Local,
            session: named(session),
            view: ViewKey::terminal(view as u32 + 1),
            name,
            status: "busy".to_owned(),
            since: None,
            branch: None,
            cwd: "/w".to_owned(),
            transcript: Transcript::Unnamed,
        };
        let session = |view: usize| if view < MAX_NOTES { "a" } else { "b" };
        let many = (0..MAX_NOTES + 2).map(|view| note(session(view), view, format!("agent {view}")));
        let long = |session: &str, view: usize| note(session, view, "x".repeat(NOTES_BYTES / 2));
        for (notes, told, said) in [
            (many.collect(), MAX_NOTES, "The notes leave out 2 more agents for room, in session b."),
            (vec![long("a", 0), long("b", 1)], 1, "leave out 1 more agent for room, in session b."),
        ] {
            let brief = Brief::new(every(&["a", "b"]), notes).expect("a brief");
            let prompt = brief.prompt(&none_read, now).expect("a reading");
            assert_eq!(prompt.matches("\nAgent \"").count(), told, "{said}");
            assert!(prompt.contains(said), "{said}");
            assert!(prompt.len() <= NOTES_BYTES, "{said}");
        }
    }

    #[test]
    fn the_notes_tell_each_agent_on_its_machine() {
        let ids = [
            "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60",
            "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a",
            "9e8d7c6b-5a4f-4e3d-2c1b-0a9f8e7d6c5b",
            "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d",
        ];
        let nixos = || Machine::Remote("nixos".parse().expect("a destination"));
        let vps = || Machine::Remote("me@vps".parse().expect("a destination"));
        let on = |on: Machine, note: Note| Note { on, ..note };
        let notes = vec![
            note("a", 1, "fold", Some(ids[0])),
            on(nixos(), note("a", 1, "fold", Some(ids[1]))),
            on(nixos(), note("b", 2, "gone", Some(ids[2]))),
            on(vps(), note("c", 3, "far", Some(ids[3]))),
        ];
        let sessions = [(Machine::Local, "a"), (nixos(), "a"), (nixos(), "b"), (vps(), "c")];
        let sessions = sessions.into_iter().map(|(on, it)| (on, named(it))).collect();
        let pi = || -> Destination { "pi".parse().expect("a destination") };
        let over = Over::Sessions { sessions, unknown: vec![(pi(), "reaching…")] };
        let brief = Brief::new(over, notes.clone()).expect("a brief");
        let asked = Mutex::new(HashMap::new());
        let said = |text: &str| {
            let block = json!({"type": "text", "text": text});
            TranscriptEnd::Claude(
                json!({"type": "assistant", "message": {"content": [block]}}).to_string(),
            )
        };
        let reads = |machine: &Machine, reading: &Reading| {
            let TranscriptEndsRequest { ids: read, share, .. } = reading.asked(None);
            asked.lock().expect("no panic").insert(machine.clone(), (read, share));
            let (id, end) = match machine {
                Machine::Local => (ids[0], said("here")),
                Machine::Remote(_) if *machine == nixos() => (ids[1], said("there")),
                Machine::Remote(_) => return Err("me@vps: luned did not answer in 6s".to_owned()),
            };
            Ok(HashMap::from([(SessionId::parse(id).expect("a UUID"), end)]))
        };
        let prompt = brief.prompt(&reads, now);
        let share = u32::try_from(TAILS_BYTES / 4).expect("a share");
        let of = |ids: &[&str]| (ids.iter().map(|id| (*id).to_owned()).collect(), share);
        let asked_of = HashMap::from([
            (Machine::Local, of(&ids[..1])),
            (nixos(), of(&ids[1..3])),
            (vps(), of(&ids[3..])),
        ]);
        assert_eq!(asked.into_inner().expect("no panic"), asked_of);
        let prompt = prompt.expect("a reading");
        let notes_said = prompt.split_once("The notes:\n").expect("the notes").1;
        assert_eq!(
            notes_said,
            "
Sessions, in order: a, a on nixos, b on nixos, c on me@vps
Machines whose sessions are unknown, as luned does not reach them now: pi (reaching…)

Agent \"fold\" in session a
Status: idle, for 3m
Works in /w/a
Lately, oldest first:
- said: here

Agent \"fold\" in session a on nixos
Status: idle, for 3m
Works in /w/a
Lately, oldest first:
- said: there

Agent \"gone\" in session b on nixos
Status: idle, for 3m
Works in /w/b
Its transcript was not found.

Agent \"far\" in session c on me@vps
Status: idle, for 3m
Works in /w/c
Its transcript was not read: me@vps: luned did not answer in 6s
"
        );

        assert!(prompt.contains("sessions of a machine are unknown, end with"), "{prompt}");
        let of_nixos = Brief::new(Over::Machine(nixos(), Vec::new()), notes.clone());
        let of_nixos = of_nixos.expect("a brief").prompt(&none_read, now).expect("a reading");
        assert_eq!(of_nixos.matches("\nAgent \"").count(), 2);
        assert!(!of_nixos.contains("unknown"), "{of_nixos}");
        let titled = |over| Brief::new(over, notes.clone()).map(|it| it.title().to_owned());
        let a = || named("a");
        for (over, title) in [
            (Over::Machine(nixos(), vec![a()]), Ok("Brief of every session on nixos")),
            (Over::Session(nixos(), a()), Ok("Brief of a on nixos")),
            (Over::Agent(nixos(), a(), ViewKey::terminal(1)), Ok("Brief of fold, in a on nixos")),
            (Over::Machine(Machine::Local, vec![a()]), Ok("Brief of every session on this machine")),
            (Over::Session(vps(), a()), Err("brief: no agents in a on me@vps")),
            (Over::Machine(Machine::Remote(pi()), Vec::new()), Err("brief: no agents on pi")),
        ] {
            assert_eq!(titled(over), title.map(str::to_owned).map_err(str::to_owned));
        }
        let unknown = ["pi", "me@vps"].map(|it| (it.parse().expect("a destination"), "lost"));
        let nowhere = Over::Sessions { sessions: Vec::new(), unknown: unknown.into() };
        let said = "brief: no agents to brief you on; luned does not reach pi, me@vps now";
        assert_eq!(Brief::new(nowhere, Vec::new()).err().as_deref(), Some(said));
    }

    #[test]
    fn a_brief_tells_of_an_agent_of_another_harness_without_its_transcript() {
        let of = |view: u32, name: &str, transcript: Transcript| Note {
            on: Machine::Local,
            session: named("a"),
            view: ViewKey::terminal(view),
            name: name.to_owned(),
            status: "idle".to_owned(),
            since: None,
            branch: None,
            cwd: "/w/a".to_owned(),
            transcript,
        };
        let notes = vec![of(1, "fold", Transcript::NotRead), of(2, "tree", Transcript::Unnamed)];
        let brief = Brief::new(every(&["a"]), notes).expect("a brief");
        let prompt = brief.prompt(&none_read, SystemTime::now).expect("a reading");
        let notes = prompt.split_once("The notes:\n").expect("the notes").1;
        assert_eq!(
            notes,
            "
Sessions, in order: a

Agent \"fold\" in session a
Status: idle
Works in /w/a
Its transcript was not read: lune reads only Claude Code's and Antigravity's transcripts so far

Agent \"tree\" in session a
Status: idle
Works in /w/a
Its transcript is unknown: Claude Code names none for it.
"
        );
    }

    #[test]
    fn a_run_of_claude_that_wrote_nothing_is_no_brief() {
        let ended = |stdout: &str| {
            let (status, stdout) = (ExitStatus::from_raw(0), stdout.as_bytes().to_vec());
            Ok(Ran::Ended { status, stdout, stderr: Vec::new() })
        };
        let brief = "You have 1 session open:\n- lune: the brief is back";
        assert_eq!(wrote(ended(&format!("{brief}\n"))), Ok(brief.to_owned()));
        assert_eq!(wrote(ended(" \n\n")), Err("brief: claude wrote nothing".to_owned()));
    }

    fn asked(number: u64, title: &str) -> Briefing {
        let asking = Asking { given_up: Arc::default(), ended: mpsc::channel().1 };
        Briefing { number, title: title.to_owned(), written: None, _asking: asking }
    }

    fn shown(briefing: &Briefing, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for row in area.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        briefing.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
        for cell in buffer.content().iter().filter(|cell| cell.symbol() != "z") {
            assert!(moonlight::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn a_brief_shows_over_the_session_until_a_key_closes_it() {
        let mut briefing = asked(1, "Brief of every session");
        assert_eq!(
            shown(&briefing, 66, 9),
            [
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                "zzz┌─ Brief of every session ─────────────────────────────────┐zzz",
                "zzz│ ⠋ Asking haiku…                                          │zzz",
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
        assert!(briefing.turning());

        let said = "You have 2 sessions open:\n- lune: the brief\tis back\x1b[31m\n\n- elo: no agents";
        briefing.written(Written { at: Some("18:42".to_owned()), said: Ok(said.to_owned()) });
        assert_eq!(
            shown(&briefing, 66, 9)[..8],
            [
                "zzz┌─ Brief of every session ────────────────────── at 18:42 ─┐zzz",
                "zzz│ You have 2 sessions open:                                │zzz",
                "zzz│ - lune: the brief is back [31m                           │zzz",
                "zzz│                                                          │zzz",
                "zzz│ - elo: no agents                                         │zzz",
                "zzz│                                                          │zzz",
                "zzz│ <any key> - Close                                        │zzz",
                "zzz└──────────────────────────────────────────────────────────┘zzz",
            ]
        );
        assert!(briefing.closes(&key(KeyCode::Char('j'))));
        assert!(briefing.clicked());
        assert!(!briefing.turning());
    }

    #[test]
    fn a_brief_too_long_for_its_box_is_cut_and_one_that_failed_says_why() {
        let mut briefing = asked(1, "Brief of lune");
        let said = (1..=9).map(|n| format!("- line {n}")).collect::<Vec<_>>().join("\n");
        briefing.written(Written { at: None, said: Ok(said) });
        assert_eq!(
            shown(&briefing, 40, 8),
            [
                "┌─ Brief of lune ──────────────────────┐",
                "│ - line 1                             │",
                "│ - line 2                             │",
                "│ - line 3                             │",
                "│ …                                    │",
                "│                                      │",
                "│ <any key> - Close                    │",
                "└──────────────────────────────────────┘",
            ]
        );
        let mut briefing = asked(2, "Brief of lune");
        let why = "brief: claude is not on the PATH".to_owned();
        briefing.written(Written { at: Some("18:42".to_owned()), said: Err(why) });
        assert_eq!(shown(&briefing, 40, 8)[2], "│ brief: claude is not on the PATH     │");
    }
}
