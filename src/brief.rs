//! The brief: what the agents are doing, in a few lines a fast model writes,
//! for coming back to them after a break.
//!
//! The sidebar knows where each agent runs, how it stands and the branch it
//! works on. The end of its transcript says what it is doing: the title Claude
//! Code gave it, what you last asked of it, and what it said and ran since.
//! The brief reads that end of every transcript it covers, then opens a
//! floating pane that hands it all to `claude -p` and shows what comes back,
//! until a key closes it. Nothing is kept: each key asks again.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use serde_json::Value;
use zellij_tile::prelude::*;

use crate::agents::SessionId;

pub const CONTEXT_VALUE: &str = "brief";
/// The number of the brief the ends of the transcripts are for.
pub const NUMBER_KEY: &str = "luneta_brief";

/// The model that writes the brief: a fast one, for a brief in seconds.
const MODEL: &str = "haiku";

/// How much of the end of a transcript to look through. A transcript runs to
/// megabytes, and most of its end is what Claude Code asked of the API and
/// what a tool answered.
const WINDOW_BYTES: usize = 2 * 1024 * 1024;

/// The most the ends of the transcripts a brief covers can take together,
/// each an equal share. zellij hands them to the plugin as a JSON array of
/// numbers, some four bytes to a byte, which the plugin reads into a string
/// that doubles as it grows, in a memory zellij stops the plugin at past
/// 16 MiB.
const TAILS_BYTES: usize = 512 * 1024;

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

/// Prints, for each transcript id after the first two arguments, the id and
/// the last bytes of its records of the kinds `digest` reads, as many as the
/// second argument says, among as many last bytes of the transcript as the
/// first says, each ending in a NUL. A transcript is in a directory named for
/// where its session started, which the id alone finds. One that is not found
/// prints nothing, not even its id. `grep` reads in the C locale, where an end
/// that starts partway into a character is still text.
const TAILS: &str = r#"window=$1
bytes=$2
shift 2
projects=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects
for id do
    for f in "$projects"/*/"$id".jsonl; do
        if [ -f "$f" ]; then
            printf '%s\0' "$id"
            tail -c "$window" "$f" |
                LC_ALL=C grep -E '"type":"(ai-title|last-prompt|user|assistant)"' |
                tail -c "$bytes"
            printf '\0'
        fi
        break
    done
done"#;

/// The brief's pane: what it is a brief of, `claude -p` on the notes with the
/// zellij of the pane kept from it, so that claude-ps places no agent in the
/// pane, and then any key closes the pane. `claude -p` with a stdin waits for
/// it to end. Keys pressed while the model writes are not the one that closes
/// the pane.
const PANE: &str = r#"stty -echo -icanon 2>/dev/null
printf '\033[1m%s\033[0m, at %s\n\n\033[2mAsking %s…\033[0m' "$1" "$(date +%H:%M)" "$2"
said=$(env -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID \
    claude -p --model "$2" --tools '' --safe-mode --no-session-persistence \
    --system-prompt "$3" "$4" </dev/null 2>&1)
printf '\r\033[K%s\n\n\033[2m%s\033[0m' "$said" '<any key> - Close'
stty min 0 time 0 2>/dev/null && cat >/dev/null
stty min 1 2>/dev/null
dd bs=1 count=1 >/dev/null 2>&1
stty sane 2>/dev/null
zellij action close-pane --pane-id "terminal_$ZELLIJ_PANE_ID""#;

/// Whether `pane` is a brief's, by the command `Brief::pane` runs in it, as
/// zellij spells a command: its path, then each argument after a space. It
/// is gone once a key closes it, and the focus with it, back to the sidebar:
/// no pane anyone goes to, or comes from.
pub fn is_brief(pane: &PaneInfo) -> bool {
    let command = pane.terminal_command.as_deref().and_then(|c| c.strip_prefix("sh -c "));
    !pane.is_plugin && command.is_some_and(|script| script.starts_with(PANE))
}

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

/// What a key of the brief covers. Each is a `Command` of the sidebar's, in
/// the group `b` opens, which has its key and what help says of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every session, a line each.
    Every,
    /// The session of the row the cursor is on, a line for each agent in it.
    Session,
    /// The row the cursor is on: the agent in its pane, in a few lines, or a
    /// session's row, as `Session` does.
    Highlighted,
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

/// What the sidebar knows of an agent, for the brief.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub session: String,
    pub pane: u32,
    /// What the agent's row in the sidebar is named.
    pub name: String,
    pub status: String,
    /// How long it has been in that status, as the sidebar says it.
    pub held: String,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    /// The id of its Claude Code session, which names its transcript.
    pub transcript: Option<SessionId>,
}

/// A brief asked for, while the sidebar reads the end of each transcript it
/// covers.
pub struct Brief {
    /// Its number among the briefs the sidebar has asked for, which the ends
    /// of the transcripts come back with.
    pub number: u64,
    over: Over,
    notes: Vec<Note>,
}

impl Brief {
    /// The brief numbered `number` of the agents in `notes` that `over`
    /// covers, or what the sidebar says when there is none. Two agents a
    /// session names alike are told apart by their panes, as the agent list
    /// tells them apart: `luneta/main:4`.
    pub fn new(number: u64, over: Over, notes: Vec<Note>) -> Result<Self, String> {
        let mut notes: Vec<Note> =
            notes.into_iter().filter(|note| over.covers(note)).collect();
        if notes.is_empty() {
            return Err(match &over {
                Over::Sessions(_) => "brief: no agents to brief you on".to_owned(),
                Over::Session(session) => format!("brief: no agents in {session}"),
                Over::Agent(..) => "brief: no agent in this pane".to_owned(),
            });
        }
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
        Ok(Brief { number, over, notes })
    }

    /// The command that prints the end of each transcript the brief has room
    /// for, each its share of `TAILS_BYTES`.
    pub fn tails(&self) -> Vec<String> {
        let ids: Vec<&str> = self
            .notes
            .iter()
            .take(MAX_NOTES)
            .filter_map(|note| note.transcript.as_ref().map(SessionId::as_str))
            .collect();
        let share = TAILS_BYTES / ids.len().max(1);
        let mut command: Vec<String> =
            ["sh", "-c", TAILS, "sh"].iter().map(|arg| (*arg).to_owned()).collect();
        command.push(WINDOW_BYTES.to_string());
        command.push(share.to_string());
        command.extend(ids.into_iter().map(str::to_owned));
        command
    }

    /// Opens the brief's pane, floating over the tab, on what `tails` printed.
    /// zellij answers which pane it opened on stdin, which no test has.
    pub fn open(&self, stdout: &[u8]) {
        let title = self.title();
        let command = self.pane(&title, stdout);
        // A few lines, wide ones: room for a dozen agents, and lines that do
        // not wrap.
        let [x, y, width, height] = ["15%", "25%", "70%", "50%"].map(|at| Some(at.to_owned()));
        let coordinates = FloatingPaneCoordinates::new(x, y, width, height, None, None);
        if let Some(PaneId::Terminal(pane)) =
            open_command_pane_floating(command, coordinates, BTreeMap::new())
        {
            rename_terminal_pane(pane, format!("luneta: {}", title.to_lowercase()));
        }
    }

    /// What the brief's pane runs, titled `title`, on what `tails` printed.
    pub fn pane(&self, title: &str, stdout: &[u8]) -> CommandToRun {
        let args = [
            "-c".to_owned(),
            PANE.to_owned(),
            "luneta".to_owned(),
            title.to_owned(),
            MODEL.to_owned(),
            SYSTEM.to_owned(),
            self.prompt(&tails(stdout)),
        ];
        CommandToRun { path: PathBuf::from("sh"), args: args.to_vec(), cwd: None }
    }

    /// What the pane says the brief is of.
    fn title(&self) -> String {
        match &self.over {
            Over::Sessions(_) => "Brief of every session".to_owned(),
            Over::Session(session) => format!("Brief of {session}"),
            Over::Agent(session, _) => match self.notes.first() {
                Some(note) => format!("Brief of {}, in {session}", note.name),
                None => format!("Brief of an agent in {session}"),
            },
        }
    }

    /// What the model is asked, with the notes on each agent and what the
    /// end of its transcript in `ends` says of it, for as many agents as the
    /// notes have room for, and how many they leave out.
    fn prompt(&self, ends: &HashMap<&str, String>) -> String {
        let mut prompt = self.over.ask();
        prompt.push_str("\n\nThe notes:\n");
        if let Over::Sessions(sessions) = &self.over {
            prompt.push_str("\nSessions, in order: ");
            prompt.push_str(&sessions.join(", "));
            prompt.push('\n');
        }
        let mut told = 0;
        for note in self.notes.iter().take(MAX_NOTES) {
            let written = format!("\n{}", note.written(&self.told(note, ends)));
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
    fn told(&self, note: &Note, ends: &HashMap<&str, String>) -> Told {
        let Some(id) = &note.transcript else {
            return Told::NoId;
        };
        match ends.get(id.as_str()) {
            Some(end) => Told::Read(digest(end, self.over.steps())),
            None => Told::NotFound,
        }
    }
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
    /// The note as the model reads it, with what its transcript `told`.
    fn written(&self, told: &Told) -> String {
        let mut lines = vec![format!("Agent \"{}\" in session {}", self.name, self.session)];
        lines.push(format!("Status: {}, for {}", self.status, self.held));
        match (&self.branch, &self.cwd) {
            (Some(branch), Some(cwd)) => lines.push(format!("Branch: {branch}, in {cwd}")),
            (Some(branch), None) => lines.push(format!("Branch: {branch}")),
            (None, Some(cwd)) => lines.push(format!("Works in {cwd}")),
            (None, None) => {},
        }
        match told {
            Told::NoId => {
                lines.push("Its transcript is unknown: claude-ps names none for it.".to_owned());
            },
            Told::NotFound => lines.push("Its transcript was not found.".to_owned()),
            Told::Read(digest) => lines.extend(digest.lines()),
        }
        lines.join("\n") + "\n"
    }
}

/// What the end of an agent's transcript told of it.
enum Told {
    /// claude-ps gave no id for its session, or one that names no
    /// transcript.
    NoId,
    /// No transcript has its id where `TAILS` looks: one written under
    /// another `CLAUDE_CONFIG_DIR`, or one not written yet.
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
/// as `steps`. A transcript is a JSON record to a line, and its end starts
/// partway into one, which is no record. A subagent's records, what Claude
/// thought and what a tool answered are left out.
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

/// The end of each transcript, by its id, as `TAILS` printed them.
fn tails(stdout: &[u8]) -> HashMap<&str, String> {
    let mut fields = stdout.split(|byte| *byte == 0);
    let mut ends = HashMap::new();
    while let (Some(id), Some(end)) = (fields.next(), fields.next()) {
        let Ok(id) = std::str::from_utf8(id) else {
            continue;
        };
        if !id.is_empty() {
            ends.insert(id, String::from_utf8_lossy(end).into_owned());
        }
    }
    ends
}

// The test runs `TAILS` through `sh`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// What the model reads of each agent: what the sidebar knows of it, and
    /// what the end of its transcript says, as `TAILS` reads it through the
    /// `sh` zellij would run. The title is the last one, and what you last
    /// asked the later of the last prompt Claude Code recorded and the last
    /// one you typed: it records a prompt some records after you type it. The
    /// steps are the last few, oldest first, by what a tool was for over what
    /// it ran. The record the end starts partway into, a subagent's records,
    /// what Claude thought and what a tool answered are left out, and so is
    /// what Claude Code typed in tags of its own or in your place where you
    /// stopped it. A transcript not found says so, and so does an agent
    /// claude-ps names none for. Two agents a session names alike are told
    /// apart by their panes, and an agent of another session named alike is
    /// not.
    #[test]
    fn the_notes_tell_each_agent_by_the_end_of_its_transcript() {
        use serde_json::json;

        let note = |session: &str, pane: u32, name: &str, transcript: Option<&str>| Note {
            session: session.to_owned(),
            pane,
            name: name.to_owned(),
            status: "idle".to_owned(),
            held: "3m".to_owned(),
            branch: (pane == 4).then(|| "feat/fold".to_owned()),
            cwd: Some(format!("/w/{session}")),
            transcript: transcript.map(|id| SessionId::parse(id).expect("a UUID")),
        };
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
        let root = std::env::temp_dir().join(format!("luneta-brief-{}", std::process::id()));
        let project = root.join("projects").join("-w-b");
        std::fs::create_dir_all(&project).expect("a directory");
        let cut = r#"me": "cut short"}]}}"#;
        let fold_file = format!("{cut}\n{}", lines(&fold_end));
        std::fs::write(project.join(format!("{fold}.jsonl")), fold_file).expect("a file");
        std::fs::write(project.join(format!("{quiet}.jsonl")), lines(&quiet_end)).expect("a file");

        let over = Over::Sessions(vec!["a".to_owned(), "b".to_owned()]);
        let notes = vec![
            note("a", 1, "fold", None),
            note("b", 4, "fold", Some(fold)),
            note("b", 5, "quiet", Some(quiet)),
            note("b", 6, "spare", Some(gone)),
            note("b", 7, "spare", None),
        ];
        let brief = Brief::new(1, over, notes).expect("a brief");
        let tails = brief.tails();
        let output = std::process::Command::new(&tails[0])
            .args(&tails[1..])
            .env("CLAUDE_CONFIG_DIR", &root)
            .output()
            .expect("sh runs");
        std::fs::remove_dir_all(&root).expect("the directories go");

        assert!(output.status.success());
        let prompt = brief.prompt(&super::tails(&output.stdout));
        let notes = prompt.split_once("The notes:\n").expect("the notes").1;
        assert_eq!(
            notes,
            "
Sessions, in order: a, b

Agent \"fold\" in session a
Status: idle, for 3m
Works in /w/a
Its transcript is unknown: claude-ps names none for it.

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
Status: idle, for 3m
Works in /w/b
Its transcript is unknown: claude-ps names none for it.
"
        );
    }

    /// A brief reads no more than `TAILS_BYTES` of the transcripts it covers
    /// together, however many agents it covers: zellij hands what it read to
    /// the plugin in a memory it stops at 16 MiB. What it reads is of the
    /// records the notes tell of, so that what Claude Code asked of the API,
    /// which takes most of the end of a transcript, leaves room for what each
    /// agent did last.
    #[test]
    fn a_brief_reads_a_share_of_each_transcript_of_what_the_notes_tell() {
        use serde_json::json;

        let root = std::env::temp_dir().join(format!("luneta-shares-{}", std::process::id()));
        let project = root.join("projects").join("-w");
        std::fs::create_dir_all(&project).expect("a directory");
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
            std::fs::write(project.join(format!("{id}.jsonl")), transcript).expect("a file");
        }

        let note = |(n, id): (usize, &String)| Note {
            session: "a".to_owned(),
            pane: n as u32,
            name: format!("agent {n}"),
            status: "busy".to_owned(),
            held: "1m".to_owned(),
            branch: None,
            cwd: None,
            transcript: Some(SessionId::parse(id).expect("a UUID")),
        };
        let notes = ids.iter().enumerate().map(note).collect();
        let brief = Brief::new(1, Over::Sessions(vec!["a".to_owned()]), notes).expect("a brief");
        let tails = brief.tails();
        let output = std::process::Command::new(&tails[0])
            .args(&tails[1..])
            .env("CLAUDE_CONFIG_DIR", &root)
            .output()
            .expect("sh runs");
        std::fs::remove_dir_all(&root).expect("the directories go");

        assert!(output.status.success());
        // Each end comes with its id and two NULs.
        let framing = MAX_NOTES * (ids[0].len() + 2);
        assert!(output.stdout.len() <= TAILS_BYTES + framing, "{}", output.stdout.len());
        let ends = super::tails(&output.stdout);
        for (n, id) in ids.iter().enumerate() {
            let digest = digest(&ends[id.as_str()], Over::Sessions(Vec::new()).steps());
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
            held: "1m".to_owned(),
            branch: None,
            cwd: None,
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
            let prompt = Brief::new(1, over, notes).expect("a brief").prompt(&HashMap::new());
            assert_eq!(prompt.matches("\nAgent \"").count(), told, "{said}");
            assert!(prompt.contains(said), "{said}");
            assert!(prompt.len() <= NOTES_BYTES, "{said}");
        }
    }
}
