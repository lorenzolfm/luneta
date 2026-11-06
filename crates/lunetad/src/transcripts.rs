//! The ends of Claude Code's transcripts, as luneta's brief reads them: of
//! each, the last records that tell what its agent is doing, whole ones
//! alone, as many as its share of what a brief keeps has room for. luneta
//! reads them for this machine's agents; a daemon reads them here for a
//! luneta on another machine, whose brief tells of agents of this one, and
//! for the topics it keeps of its own.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::api::{On, TranscriptEndsRequest};

/// How much of the end of a transcript to look through. A transcript runs to
/// megabytes, and most of its end is what Claude Code asked of the API and
/// what a tool answered.
const WINDOW_BYTES: u64 = 2 * 1024 * 1024;

/// The most of the records the brief reads that it keeps of the ends of the
/// transcripts it covers, together, each an equal share: enough of each for
/// the steps its notes tell, and no more for a brief of many agents to read
/// through.
pub const TAILS_BYTES: usize = 512 * 1024;

/// The most transcripts a reading reads the ends of.
pub const IDS_MAX: usize = 32;

/// How long a title, a prompt and a step of what an agent did can run in what
/// a digest says, in characters.
const TITLE_CHARS: usize = 120;
const ASKED_CHARS: usize = 500;
const STEP_CHARS: usize = 300;

/// The fewest characters a `last-prompt` record that Claude Code cut short runs
/// to. It cuts at about 200, so one shorter than this that ends in `…` is what
/// you typed.
const CUT_CHARS: usize = 100;

/// The records of the kinds the brief reads, by the `type` each names.
const KINDS: [&str; 4] =
    [r#""type":"ai-title""#, r#""type":"last-prompt""#, r#""type":"user""#, r#""type":"assistant""#];

/// The id Claude Code gives a session, which names its transcript: a UUID's
/// hex digits and dashes, a hex digit first, and nothing that would take the
/// file it names out of its folder, nor read as an option to `claude
/// --resume`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let first = id.bytes().next().is_some_and(|byte| byte.is_ascii_hexdigit());
        let uuid = first && id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
        uuid.then(|| SessionId(id.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// The ends of some transcripts to read, by their sessions' ids, each no
/// longer than a share of `TAILS_BYTES`, of some bytes: no more than
/// `IDS_MAX` of them, and no more than `TAILS_BYTES` together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    ids: Vec<SessionId>,
    share: usize,
}

impl Reading {
    /// The ends of the transcripts of `ids`, `share` bytes of each at the
    /// most, or why not: they are too many, or together too long, or it
    /// keeps nothing of each, which would read as an end that says nothing.
    pub fn new(ids: Vec<SessionId>, share: usize) -> Result<Reading, String> {
        if ids.len() > IDS_MAX {
            return Err(too_many(ids.len()));
        }
        if share == 0 {
            return Err("share: no bytes of each transcript".to_owned());
        }
        if ids.len().saturating_mul(share) > TAILS_BYTES {
            return Err(format!(
                "share: {share} bytes of each of {} transcripts, of {TAILS_BYTES} together at the \
                 most",
                ids.len()
            ));
        }
        Ok(Reading { ids, share })
    }

    /// The ends of the transcripts of each of `groups`, as readings handed
    /// back each with what its group came with, which keep an equal share of
    /// `TAILS_BYTES` of each transcript of them all, and so no more of them
    /// together than one reading of them all would; or why not: they are
    /// more than `IDS_MAX` together.
    pub fn shared<K>(groups: Vec<(K, Vec<SessionId>)>) -> Result<Vec<(K, Reading)>, String> {
        let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
        if count > IDS_MAX {
            return Err(too_many(count));
        }
        let share = TAILS_BYTES / count.max(1);
        Ok(groups.into_iter().map(|(with, ids)| (with, Reading { ids, share })).collect())
    }

    /// The reading a question asks for, of the transcripts of `ids`, `share`
    /// bytes of each, or why the question is invalid: an id names no
    /// session, or they are too many, or together too long, or it asks for
    /// no bytes, as one that does not say how many does.
    pub fn of(ids: &[String], share: u32) -> Result<Reading, String> {
        let parsed: Option<Vec<SessionId>> = ids.iter().map(|id| SessionId::parse(id)).collect();
        let Some(parsed) = parsed else {
            let named = ids.iter().find(|id| SessionId::parse(id).is_none());
            return Err(format!("ids: {:?} names no session", named.map_or("", String::as_str)));
        };
        Reading::new(parsed, usize::try_from(share).unwrap_or(usize::MAX))
    }

    /// The question that has the daemon of the machine `on` names read it,
    /// or the daemon asked, where it names none.
    #[must_use]
    pub fn asked(&self, on: Option<On>) -> TranscriptEndsRequest {
        let ids = self.ids.iter().map(|id| id.as_str().to_owned()).collect();
        let share = u32::try_from(self.share).unwrap_or(u32::MAX);
        TranscriptEndsRequest { on, ids, share }
    }

    /// The end of each transcript it reads, by its session's id, as
    /// `projects` has them: in a directory named for where its session
    /// started, which its id alone finds. One that is not found there, or
    /// does not read, has none.
    #[must_use]
    pub fn ends(&self, projects: &Path) -> HashMap<SessionId, String> {
        let dirs = project_dirs(projects);
        let mut ends = HashMap::new();
        for id in &self.ids {
            let Some(path) = found_in(&dirs, id) else {
                continue;
            };
            if let Ok(end) = end(&path, self.share) {
                ends.insert(id.clone(), end);
            }
        }
        ends
    }
}

/// The directories of `projects`, each named for where a session started.
fn project_dirs(projects: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(projects)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    dirs.sort();
    dirs
}

/// The transcript of `id`, in one of `dirs`, if one is there.
fn found_in(dirs: &[PathBuf], id: &SessionId) -> Option<PathBuf> {
    let file = format!("{}.jsonl", id.as_str());
    dirs.iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
}

/// The transcript of `id` that `projects` has, if it has one, which its id
/// alone finds.
#[must_use]
pub fn find(projects: &Path, id: &SessionId) -> Option<PathBuf> {
    found_in(&project_dirs(projects), id)
}

/// What the end of the transcript at `path` says, with as many of the last
/// steps as `steps`, of `share` bytes of its records at the most.
pub fn told(path: &Path, share: usize, steps: usize) -> io::Result<Digest> {
    Ok(digest(&end(path, share)?, steps))
}

/// Why a reading of `count` transcripts is none: they are too many.
fn too_many(count: usize) -> String {
    format!("ids: {count} transcripts, of {IDS_MAX} at the most")
}

/// The last records of the kinds the brief reads among the last
/// `WINDOW_BYTES` of the transcript at `path`, whole ones alone, as many as
/// `share` bytes have room for. The window starts partway into a record,
/// which is no record, and partway into a character, which reads as `�`.
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

/// What the end of a transcript says of what its agent is doing.
#[derive(Debug, PartialEq, Eq)]
pub struct Digest {
    /// The title Claude Code gave the session.
    pub title: Option<String>,
    /// What you last asked of it.
    pub asked: Option<Asked>,
    /// What it said and ran last, oldest first.
    pub steps: Vec<String>,
}

impl Digest {
    /// What a model is told of what the transcript says.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
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
/// what Claude thought and what a tool answered are left out, and so is the
/// summary Claude Code goes on from as it compacts a session, which no one
/// asked.
#[must_use]
pub fn digest(end: &str, steps: usize) -> Digest {
    let mut title = None;
    let mut asked: Option<Asked> = None;
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
                if let Some(last) = record["lastPrompt"].as_str().map(Asked::recorded)
                    && !asked.as_ref().is_some_and(|typed| last.cuts(typed))
                {
                    asked = Some(last);
                }
            },
            Some("user")
                if record["isMeta"].as_bool() != Some(true)
                    && record["isCompactSummary"].as_bool() != Some(true) =>
            {
                let typed = typed_by_you(&record["message"]["content"]);
                asked = typed.as_deref().map(Asked::typed).or(asked);
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
    Digest { title: title.map(|title| one_line(&title, TITLE_CHARS)), asked, steps: last.into() }
}

/// What you last asked of an agent, on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// All of it.
    Whole(String),
    /// How it starts, where it was cut short.
    Cut(String),
}

impl Asked {
    /// What you typed, cut short past `ASKED_CHARS`.
    fn typed(text: &str) -> Self {
        let line = spaced(text);
        if line.chars().count() <= ASKED_CHARS {
            return Asked::Whole(line);
        }
        Asked::Cut(line.chars().take(ASKED_CHARS - 1).collect())
    }

    /// What a `last-prompt` record says, which Claude Code ends in `…` where it
    /// cut a long prompt short.
    fn recorded(text: &str) -> Self {
        let line = spaced(text);
        match line.strip_suffix('…') {
            Some(start) if line.chars().count() >= CUT_CHARS => {
                Asked::Cut(start.chars().take(ASKED_CHARS - 1).collect())
            },
            _ => Asked::typed(&line),
        }
    }

    /// Whether it is `whole`, or `whole` cut short.
    fn cuts(&self, whole: &Asked) -> bool {
        match (self, whole) {
            (Asked::Whole(_), _) => self == whole,
            (Asked::Cut(start), Asked::Whole(text) | Asked::Cut(text)) => text.starts_with(start),
        }
    }

    /// Whether it and `other` are one prompt: the same words, but for where
    /// either was cut short.
    #[must_use]
    pub fn same(&self, other: &Asked) -> bool {
        self.cuts(other) || other.cuts(self)
    }
}

impl fmt::Display for Asked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Asked::Whole(text) => formatter.write_str(text),
            Asked::Cut(start) => write!(formatter, "{start}…"),
        }
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
    let text = without_paste_marks(text);
    let text = text.trim();
    let theirs = text.starts_with('<') || text.starts_with(INTERRUPTED);
    (!text.is_empty() && !theirs).then(|| text.to_owned())
}

/// `text` without the marks Claude Code puts around what you pasted in a user
/// record, which its `last-prompt` record leaves out.
fn without_paste_marks(text: &str) -> String {
    let mut bare = String::with_capacity(text.len());
    let mut left = text;
    while let Some((before, after)) = left.split_once("<pasted_content id=\"") {
        bare.push_str(before);
        left = after.split_once("\">").map_or(after, |(_, rest)| rest);
    }
    bare.push_str(left);
    bare.replace("</pasted_content>", "")
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
#[must_use]
pub fn on_what(input: &Value) -> Option<&str> {
    const FIELDS: [&str; 7] =
        ["description", "command", "file_path", "pattern", "url", "query", "prompt"];
    FIELDS
        .iter()
        .find_map(|field| input[field].as_str())
        .or_else(|| input["questions"][0]["question"].as_str())
}

/// `text` on one line, its runs of white space one space each, and no longer
/// than `chars`, ending in `…` where it was cut.
#[must_use]
pub fn one_line(text: &str, chars: usize) -> String {
    let line = spaced(text);
    if line.chars().count() <= chars {
        return line;
    }
    let mut cut: String = line.chars().take(chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `text` on one line, its runs of white space one space each.
fn spaced(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A transcript's id names a file in Claude Code's folder, and one that
    /// would name a file elsewhere names none, nor one that reads as an
    /// option.
    #[test]
    fn a_transcript_s_id_is_a_uuid_s_digits_and_dashes() {
        assert!(SessionId::parse("0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60").is_some());
        for id in ["", "../0b7c5e1a", "0b7c5e1a/x", "id-1", "-0b7c5e1a", "--"] {
            assert_eq!(SessionId::parse(id), None, "{id}");
        }
    }

    /// What a transcript of these records says you last asked.
    fn asked(records: &[serde_json::Value]) -> Option<String> {
        let end: Vec<String> = records.iter().map(ToString::to_string).collect();
        digest(&end.join("\n"), 1).asked.as_ref().map(ToString::to_string)
    }

    fn typed(text: &str) -> serde_json::Value {
        json!({"type": "user", "message": {"content": text}})
    }

    fn last(text: &str) -> serde_json::Value {
        json!({"type": "last-prompt", "lastPrompt": text})
    }

    /// What you last asked is the later of what you typed and what Claude Code
    /// recorded of a prompt, which is all it says of a slash command. It cuts a
    /// long one short in the record, which says no more than what you typed.
    #[test]
    fn a_prompt_claude_code_cut_short_is_the_one_you_typed() {
        let long = format!("{}and the rest", "fold ".repeat(50));
        let cut = format!("{}…", &long[..199]);
        let spaced = "fold  the\n\nsessions";
        assert_eq!(asked(&[typed(&long), last(&cut)]), Some(long));
        let one = "fold the sessions";
        assert_eq!(asked(&[typed(spaced), last(one)]).unwrap(), one);
        assert_eq!(asked(&[typed("fold them"), last("/review this")]).unwrap(), "/review this");
        assert_eq!(asked(&[last("fold them"), typed("now the tabs")]).unwrap(), "now the tabs");
        assert_eq!(asked(&[last(&cut)]), Some(cut));
    }

    /// What you paste comes in marks in what you typed, which Claude Code's
    /// record of the prompt leaves out: it is one prompt, with words before the
    /// paste or none.
    #[test]
    fn a_prompt_with_a_paste_is_the_one_claude_code_recorded() {
        let paste = "<pasted_content id=\"c6de\">\n- the first\n- the second\n</pasted_content>";
        let after_words = format!("file these:\n{paste}\n");
        let one = "file these: - the first - the second";
        assert_eq!(asked(&[typed(&after_words)]).unwrap(), one);
        assert_eq!(asked(&[typed(&after_words), last(one)]).unwrap(), one);
        assert_eq!(asked(&[typed(paste)]).unwrap(), "- the first - the second");
    }

    /// The summary Claude Code goes on from as it compacts a session is no
    /// prompt, though it comes as a record of yours.
    #[test]
    fn a_compacted_session_s_summary_is_not_what_you_asked() {
        let summary = json!({"type": "user", "isCompactSummary": true, "message": {
            "content": "This session is being continued from a previous conversation"}});
        assert_eq!(asked(&[typed("fold them"), summary.clone()]).unwrap(), "fold them");
        assert_eq!(asked(&[summary]), None);
    }

    /// Two digests say one prompt where the words are the same but for where
    /// either was cut short, and no two others do, a prompt that starts another
    /// among them.
    #[test]
    fn a_prompt_is_the_same_where_only_a_cut_tells_it_apart() {
        let whole = |text: &str| Asked::Whole(text.to_owned());
        let cut = |text: &str| Asked::Cut(text.to_owned());
        assert!(whole("fold them").same(&whole("fold them")));
        assert!(cut("fold").same(&whole("fold them")));
        assert!(whole("fold them").same(&cut("fold")));
        assert!(cut("fold").same(&cut("fold the")));
        assert!(!whole("fold").same(&whole("fold them")));
        assert!(!whole("fold them").same(&whole("fold")));
        assert!(!cut("fold").same(&whole("sort them")));
    }

    /// A short prompt that ends in `…` is all you asked, and only a long record
    /// that does is one Claude Code cut short.
    #[test]
    fn a_prompt_you_ended_in_an_ellipsis_is_not_cut_short() {
        assert_eq!(Asked::recorded("fold…"), Asked::Whole("fold…".to_owned()));
        assert!(!Asked::recorded("fold…").same(&Asked::typed("fold them")));
        assert_eq!(asked(&[typed("fold them"), last("fold…")]).unwrap(), "fold…");
        let start = "fold ".repeat(CUT_CHARS / 5);
        assert_eq!(Asked::recorded(&format!("{start}…")), Asked::Cut(start));
    }

    /// What you typed is cut short only past `ASKED_CHARS`, and says so at that
    /// length with its `…`.
    #[test]
    fn a_long_prompt_you_typed_is_cut_at_its_most() {
        let most = "é".repeat(ASKED_CHARS);
        assert_eq!(Asked::typed(&most), Asked::Whole(most.clone()));
        let over = Asked::typed(&format!("{most}é"));
        assert_eq!(over, Asked::Cut("é".repeat(ASKED_CHARS - 1)));
        assert_eq!(over.to_string(), one_line(&format!("{most}é"), ASKED_CHARS));
    }

    /// A question reads the ends of no more than `IDS_MAX` transcripts, of
    /// no more than `TAILS_BYTES` together, and of some bytes of each, each
    /// by its session's id: one that asks for more, or for nothing, or names
    /// a file by anything else, is invalid.
    #[test]
    fn a_reading_is_of_a_few_transcripts_by_their_ids() {
        let ids = |count: usize| -> Vec<String> {
            (0..count).map(|n| format!("{n:08x}-0000-4000-8000-000000000000")).collect()
        };
        let share = |count| u32::try_from(TAILS_BYTES / count).unwrap();
        let most = Reading::of(&ids(IDS_MAX), share(IDS_MAX)).unwrap();
        let asked = most.asked(None);
        assert_eq!(Reading::of(&asked.ids, asked.share), Ok(most));
        assert!(Reading::of(&ids(IDS_MAX + 1), 1).unwrap_err().starts_with("ids: 33 transcripts"));
        let long = Reading::of(&ids(2), share(1)).unwrap_err();
        assert!(long.starts_with("share: 524288 bytes of each of 2"), "{long}");
        assert_eq!(Reading::of(&ids(1), 0), Err("share: no bytes of each transcript".to_owned()));
        let named = Reading::of(&["../x".to_owned()], 1).unwrap_err();
        assert_eq!(named, r#"ids: "../x" names no session"#);
    }

    /// Readings shared out of some transcripts keep an equal share of each
    /// of them all, and so no more together than one reading of them all
    /// would, each handed back with what its group came with, and each one a
    /// daemon takes. Of more than `IDS_MAX` transcripts together there are
    /// none.
    #[test]
    fn shared_readings_keep_no_more_than_one_reading_does() {
        let ids = |from: usize, count: usize| -> Vec<SessionId> {
            let id = |n: usize| format!("{n:08x}-0000-4000-8000-000000000000");
            (from..from + count).map(|n| SessionId::parse(&id(n)).unwrap()).collect()
        };
        let shared = Reading::shared(vec![("here", ids(0, 3)), ("there", ids(3, 1))]).unwrap();
        let share = TAILS_BYTES / 4;
        let here = Reading { ids: ids(0, 3), share };
        assert_eq!(shared, [("here", here), ("there", Reading { ids: ids(3, 1), share })]);
        for (_, reading) in shared {
            let asked = reading.asked(None);
            assert_eq!(Reading::of(&asked.ids, asked.share), Ok(reading));
        }
        let many = Reading::shared(vec![(1, ids(0, IDS_MAX)), (2, ids(IDS_MAX, 1))]);
        assert_eq!(many, Err("ids: 33 transcripts, of 32 at the most".to_owned()));
    }
}
