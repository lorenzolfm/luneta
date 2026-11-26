use std::borrow::{Borrow, Cow};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::api::{self, On, TranscriptEndsRequest};

const WINDOW_BYTES: u64 = 2 * 1024 * 1024;

pub const TAILS_BYTES: usize = 512 * 1024;

pub const IDS_MAX: usize = 32;

pub const STEPS_MAX: usize = 64;

const TITLE_CHARS: usize = 120;
const ASKED_CHARS: usize = 500;
const STEP_CHARS: usize = 300;

const CUT_CHARS: usize = 100;

const KINDS: [&str; 4] =
    [r#""type":"ai-title""#, r#""type":"last-prompt""#, r#""type":"user""#, r#""type":"assistant""#];

const AGY_KINDS: [&str; 2] = [r#""type":"USER_INPUT""#, r#""type":"PLANNER_RESPONSE""#];

const ALL_KINDS: [&str; 6] = [
    r#""type":"ai-title""#,
    r#""type":"last-prompt""#,
    r#""type":"user""#,
    r#""type":"assistant""#,
    r#""type":"USER_INPUT""#,
    r#""type":"PLANNER_RESPONSE""#,
];

const ON_WHAT_FIELDS: [&str; 13] = [
    "description",
    "command",
    "file_path",
    "pattern",
    "url",
    "query",
    "prompt",
    "toolSummary",
    "CommandLine",
    "Path",
    "Description",
    "Query",
    "Url",
];

const _: () = assert!(ALL_KINDS.len() == KINDS.len() + AGY_KINDS.len());
const _: () = assert!(WINDOW_BYTES > 0);
const _: () = assert!(WINDOW_BYTES < u64::MAX);
const _: () = assert!(WINDOW_BYTES < usize::MAX as u64);
const _: () = assert!(TAILS_BYTES > 0);
const _: () = assert!(TAILS_BYTES as u64 <= WINDOW_BYTES);
const _: () = assert!(STEPS_MAX > 0);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folders {
    pub claude: Option<PathBuf>,
    pub agy: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEnd {
    Claude(String),
    Antigravity(String),
}

impl TranscriptEnd {
    #[must_use]
    pub fn lines(&self) -> &str {
        match self {
            TranscriptEnd::Claude(lines) | TranscriptEnd::Antigravity(lines) => lines,
        }
    }

    #[must_use]
    pub fn to_api(&self) -> api::transcript_ends_response::TranscriptEnd {
        let (harness, lines) = match self {
            TranscriptEnd::Claude(lines) => (api::Harness::ClaudeCode, lines),
            TranscriptEnd::Antigravity(lines) => (api::Harness::Antigravity, lines),
        };
        api::transcript_ends_response::TranscriptEnd {
            harness: harness.into(),
            lines: lines.clone(),
        }
    }

    pub fn of_api(said: api::transcript_ends_response::TranscriptEnd) -> Result<Self, String> {
        if said.lines.len() > TAILS_BYTES {
            return Err(format!(
                "transcript end: {} bytes, of {TAILS_BYTES} at the most",
                said.lines.len()
            ));
        }
        let end = match api::Harness::try_from(said.harness) {
            Ok(api::Harness::ClaudeCode) => TranscriptEnd::Claude(said.lines),
            Ok(api::Harness::Antigravity) => TranscriptEnd::Antigravity(said.lines),
            Ok(api::Harness::Pi) => return Err("pi keeps no transcript this lune reads".to_owned()),
            Ok(api::Harness::Unspecified) => return Err("it names no harness".to_owned()),
            Err(_) => {
                return Err(format!("harness {} is one this lune does not know", said.harness));
            },
        };
        assert!(end.lines().len() <= TAILS_BYTES);
        Ok(end)
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    ids: Vec<SessionId>,
    share: usize,
}

impl Reading {
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

    pub fn shared<K>(groups: Vec<(K, Vec<SessionId>)>) -> Result<Vec<(K, Reading)>, String> {
        let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
        if count > IDS_MAX {
            return Err(too_many(count));
        }
        let share = TAILS_BYTES / count.max(1);
        Ok(groups.into_iter().map(|(with, ids)| (with, Reading { ids, share })).collect())
    }

    pub fn of(ids: &[String], share: u32) -> Result<Reading, String> {
        let parsed: Option<Vec<SessionId>> = ids.iter().map(|id| SessionId::parse(id)).collect();
        let Some(parsed) = parsed else {
            let named = ids.iter().find(|id| SessionId::parse(id).is_none());
            return Err(format!("ids: {:?} names no session", named.map_or("", String::as_str)));
        };
        Reading::new(parsed, usize::try_from(share).unwrap_or(usize::MAX))
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>) -> TranscriptEndsRequest {
        let ids = self.ids.iter().map(|id| id.as_str().to_owned()).collect();
        let share = u32::try_from(self.share).unwrap_or(u32::MAX);
        TranscriptEndsRequest { on, ids, share }
    }

    #[must_use]
    pub(crate) fn ends(&self, folders: &Folders) -> HashMap<SessionId, TranscriptEnd> {
        let dirs = folders.claude.as_deref().map(|claude| project_dirs(&claude.join("projects")));
        let mut ends = HashMap::new();
        for id in &self.ids {
            if let Some(path) = dirs.as_deref().and_then(|dirs| found_in(dirs, id))
                && let Ok(end) = end(&path, self.share, &KINDS)
            {
                ends.insert(id.clone(), TranscriptEnd::Claude(end));
                continue;
            }
            if let Some(path) = folders.agy.as_deref().and_then(|agy| agy_file(agy, id))
                && let Ok(end) = end(&path, self.share, &AGY_KINDS)
            {
                ends.insert(id.clone(), TranscriptEnd::Antigravity(end));
            }
        }
        ends
    }
}

fn agy_file(agy: &Path, id: &SessionId) -> Option<PathBuf> {
    assert_ne!(id.as_str(), "");
    assert!(!id.as_str().contains('/'));
    let path = agy.join("brain").join(id.as_str()).join(".system_generated/logs/transcript.jsonl");
    path.is_file().then_some(path)
}

fn project_dirs(projects: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(projects)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    dirs.sort();
    dirs
}

fn found_in(dirs: &[PathBuf], id: &SessionId) -> Option<PathBuf> {
    let file = format!("{}.jsonl", id.as_str());
    dirs.iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
}

#[must_use]
pub fn find(projects: &Path, id: &SessionId) -> Option<PathBuf> {
    found_in(&project_dirs(projects), id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TailLimits {
    pub share_bytes_max: usize,
    pub steps_max: usize,
}

#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    share_bytes_max: usize,
    steps_max: usize,
    file_bytes_seen: Option<u64>,
    read_offset: u64,
    torn: bool,
    kept: VecDeque<KeptRecord>,
    kept_bytes: usize,
}

#[derive(Debug)]
struct KeptRecord {
    offset: u64,
    bytes: usize,
    note: Note,
}

struct ReadPlan {
    reset: bool,
    read_offset: u64,
    torn: bool,
    read_bytes: u64,
}

impl Tail {
    #[must_use]
    pub fn new(path: PathBuf, limits: TailLimits) -> Tail {
        assert!(!path.as_os_str().is_empty(), "a tail reads a named path");
        assert!(limits.share_bytes_max > 0, "a tail keeps at least one byte");
        assert!(
            u64::try_from(limits.share_bytes_max).is_ok_and(|bytes| bytes <= WINDOW_BYTES),
            "a tail's share fits within the window"
        );
        assert!(limits.steps_max <= STEPS_MAX, "a tail's steps fit within the maximum");
        Tail {
            path,
            share_bytes_max: limits.share_bytes_max,
            steps_max: limits.steps_max,
            file_bytes_seen: None,
            read_offset: 0,
            torn: false,
            kept: VecDeque::new(),
            kept_bytes: 0,
        }
    }

    pub fn grown(&mut self) -> io::Result<Option<Digest>> {
        assert!(self.kept_bytes <= self.share_bytes_max);
        if self.torn {
            assert!(self.kept.is_empty(), "a torn tail keeps no records");
        }
        if self.kept.is_empty() {
            assert_eq!(self.kept_bytes, 0);
        }
        if self.file_bytes_seen == Some(fs::metadata(&self.path)?.len()) {
            return Ok(None);
        }
        let mut file = File::open(&self.path)?;
        let file_bytes = file.metadata()?.len();
        let plan = self.plan_read(file_bytes);
        file.seek(SeekFrom::Start(plan.read_offset))?;
        let read_capacity = usize::try_from(plan.read_bytes).expect("window read fits in memory");
        let mut appended_bytes = Vec::with_capacity(read_capacity);
        file.take(plan.read_bytes).read_to_end(&mut appended_bytes)?;
        let read_actual = u64::try_from(appended_bytes.len()).expect("slice length fits in u64");
        if read_actual < plan.read_bytes {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "transcript shrank while reading",
            ));
        }
        assert_eq!(read_actual, plan.read_bytes);
        self.apply_read(&plan, &appended_bytes, file_bytes);
        let digest =
            fold(self.kept.iter().map(|record| Cow::Borrowed(&record.note)), self.steps_max);
        assert!(digest.steps.len() <= self.steps_max);
        Ok(Some(digest))
    }

    fn plan_read(&self, file_bytes: u64) -> ReadPlan {
        if let Some(seen_bytes) = self.file_bytes_seen {
            assert!(self.read_offset <= seen_bytes, "the read cursor stays within what was seen");
        } else {
            assert_eq!(self.read_offset, 0, "an unread tail starts at byte zero");
        }
        let grew_from_seen =
            self.file_bytes_seen.is_none_or(|seen_bytes| file_bytes >= seen_bytes);
        let within_window = if grew_from_seen {
            assert!(file_bytes >= self.read_offset);
            file_bytes - self.read_offset <= WINDOW_BYTES
        } else {
            false
        };
        let plan = if within_window {
            ReadPlan {
                reset: false,
                read_offset: self.read_offset,
                torn: self.torn,
                read_bytes: file_bytes - self.read_offset,
            }
        } else {
            let window_offset = file_bytes.saturating_sub(WINDOW_BYTES);
            let read_offset = window_offset.saturating_sub(1);
            ReadPlan {
                reset: true,
                read_offset,
                torn: window_offset > 0,
                read_bytes: file_bytes - read_offset,
            }
        };
        assert!(plan.read_offset <= file_bytes, "a read starts within the file");
        assert!(plan.read_bytes <= WINDOW_BYTES + 1, "a look reads no more than the window");
        plan
    }

    fn apply_read(&mut self, plan: &ReadPlan, appended_bytes: &[u8], file_bytes: u64) {
        let actual = u64::try_from(appended_bytes.len()).expect("slice length fits in u64");
        assert_eq!(actual, plan.read_bytes);
        assert_eq!(plan.read_offset + plan.read_bytes, file_bytes);
        if plan.reset {
            self.kept.clear();
            self.kept_bytes = 0;
            self.read_offset = plan.read_offset;
            self.torn = plan.torn;
        } else {
            assert_eq!(self.read_offset, plan.read_offset);
            assert_eq!(self.torn, plan.torn);
        }
        self.take_in(appended_bytes);
        let window_offset = file_bytes.saturating_sub(WINDOW_BYTES);
        while self.kept.front().is_some_and(|record| record.offset < window_offset) {
            self.forget_first();
        }
        self.file_bytes_seen = Some(file_bytes);
        assert!(self.read_offset <= file_bytes);
        assert!(self.kept_bytes <= self.share_bytes_max);
        if let Some(first) = self.kept.front() {
            assert!(first.offset >= window_offset);
        }
        if self.kept.is_empty() {
            assert_eq!(self.kept_bytes, 0);
        }
    }

    fn take_in(&mut self, appended_bytes: &[u8]) {
        let read_max = usize::try_from(WINDOW_BYTES + 1).expect("window fits in memory");
        assert!(appended_bytes.len() <= read_max, "a look takes no more than the window");
        let read_before = self.read_offset;
        let mut rest = appended_bytes;
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            if self.torn {
                self.torn = false;
            } else {
                self.keep(self.read_offset, &rest[..end]);
            }
            let line_bytes = u64::try_from(end).expect("line length fits in u64") + 1;
            self.read_offset += line_bytes;
            rest = &rest[end + 1..];
        }
        let consumed = self.read_offset - read_before;
        let trailing = u64::try_from(rest.len()).expect("slice length fits in u64");
        let total = u64::try_from(appended_bytes.len()).expect("slice length fits in u64");
        assert_eq!(consumed + trailing, total, "every byte read is consumed or awaits newline");
    }

    fn keep(&mut self, offset: u64, line: &[u8]) {
        assert!(!self.torn, "a torn record is never kept");
        if let Some(last) = self.kept.back() {
            assert!(offset > last.offset, "records arrive at strictly increasing offsets");
        }
        let record = String::from_utf8_lossy(line);
        let record = record.strip_suffix('\r').unwrap_or(&record);
        if !ALL_KINDS.iter().any(|kind| record.contains(kind)) {
            return;
        }
        let bytes = record.len() + 1;
        assert!(bytes > 0);
        if bytes > self.share_bytes_max {
            self.kept.clear();
            self.kept_bytes = 0;
            return;
        }
        self.kept.push_back(KeptRecord { offset, bytes, note: noted(record, note_any) });
        self.kept_bytes += bytes;
        while self.kept_bytes > self.share_bytes_max {
            self.forget_first();
        }
        assert!(self.kept_bytes <= self.share_bytes_max);
    }

    fn forget_first(&mut self) {
        let forgot = self.kept.pop_front().expect("a tail forgetting a record keeps at least one");
        assert!(self.kept_bytes >= forgot.bytes, "kept bytes cover every kept record");
        self.kept_bytes -= forgot.bytes;
        if self.kept.is_empty() {
            assert_eq!(self.kept_bytes, 0);
        }
    }
}

pub fn last_said(path: &Path, chars_max: usize) -> io::Result<Option<String>> {
    Ok(last_said_in(&end(path, TAILS_BYTES, &ALL_KINDS)?, chars_max))
}

fn last_said_in(end: &str, chars_max: usize) -> Option<String> {
    assert!(chars_max > 0, "something of what was said is kept");
    for line in end.lines().rev() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        if record["type"] == "PLANNER_RESPONSE" {
            if let Some(said) =
                record["content"].as_str().map(str::trim).filter(|text| !text.is_empty())
            {
                return Some(cut(said, chars_max));
            }
            continue;
        }
        if record["type"] != "assistant" {
            continue;
        }
        let blocks = record["message"]["content"].as_array().into_iter().flatten().rev();
        let texts = blocks.filter(|block| block["type"] == "text");
        let mut texts = texts.filter_map(|block| block["text"].as_str()).map(str::trim);
        if let Some(said) = texts.find(|text| !text.is_empty()) {
            return Some(cut(said, chars_max));
        }
    }
    None
}

fn cut(text: &str, chars_max: usize) -> String {
    let kept = match text.char_indices().nth(chars_max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    };
    assert!(kept.chars().count() <= chars_max + 1, "the cut keeps {chars_max} and an ellipsis");
    if !text.is_empty() {
        assert!(!kept.is_empty(), "a cut of some text keeps some");
    }
    kept
}

fn too_many(count: usize) -> String {
    format!("ids: {count} transcripts, of {IDS_MAX} at the most")
}

fn end(path: &Path, share: usize, kinds: &[&str]) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(WINDOW_BYTES)))?;
    let mut window = Vec::new();
    file.take(WINDOW_BYTES).read_to_end(&mut window)?;
    let window = String::from_utf8_lossy(&window);
    let told_of = |record: &&str| kinds.iter().any(|kind| record.contains(kind));
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

#[derive(Debug, PartialEq, Eq)]
pub struct Digest {
    pub title: Option<String>,
    pub asked: Option<Asked>,
    pub steps: Vec<String>,
}

impl Digest {
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(title) = &self.title {
            lines.push(format!("Title: {title}"));
        }
        if let Some(asked) = &self.asked {
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

#[must_use]
pub fn digest_end(end: &TranscriptEnd, steps_max: usize) -> Digest {
    assert!(end.lines().len() <= TAILS_BYTES, "a transcript end fits within the tail share");
    assert!(steps_max <= STEPS_MAX, "steps kept fit within the maximum");
    let digest = match end {
        TranscriptEnd::Claude(lines) => fold(notes(lines, note_claude), steps_max),
        TranscriptEnd::Antigravity(lines) => fold(notes(lines, note_agy), steps_max),
    };
    assert!(digest.steps.len() <= steps_max);
    digest
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    Silent,
    Titled(String),
    Recorded(Asked),
    Typed(Asked),
    Stepped(Vec<String>),
}

fn notes(end: &str, of: fn(&Value) -> Note) -> impl Iterator<Item = Cow<'static, Note>> + '_ {
    assert!(end.len() <= TAILS_BYTES, "notes read no more than the tail share");
    end.lines().map(move |line| Cow::Owned(noted(line, of)))
}

fn noted(line: &str, of: fn(&Value) -> Note) -> Note {
    let line_max = usize::try_from(WINDOW_BYTES + 1).expect("window fits in memory");
    assert!(line.len() <= line_max, "a record fits within the window");
    assert!(!line.as_bytes().contains(&b'\n'), "a record is one line");
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Note::Silent;
    };
    if record["isSidechain"].as_bool() == Some(true) {
        return Note::Silent;
    }
    of(&record)
}

fn fold<'a>(notes: impl IntoIterator<Item = Cow<'a, Note>>, steps_max: usize) -> Digest {
    assert!(steps_max <= STEPS_MAX, "steps kept fit within the maximum");
    let mut title: Option<Cow<'a, str>> = None;
    let mut asked: Option<Cow<'a, Asked>> = None;
    let mut last: VecDeque<Cow<'a, str>> = VecDeque::with_capacity(steps_max);
    for note in notes {
        match note {
            Cow::Borrowed(Note::Silent) | Cow::Owned(Note::Silent) => {},
            Cow::Borrowed(Note::Titled(said)) => title = Some(Cow::Borrowed(said.as_str())),
            Cow::Owned(Note::Titled(said)) => title = Some(Cow::Owned(said)),
            Cow::Borrowed(Note::Recorded(prompt)) => {
                if asked.as_deref().is_none_or(|typed| !prompt.cuts(typed)) {
                    asked = Some(Cow::Borrowed(prompt));
                }
            },
            Cow::Owned(Note::Recorded(prompt)) => {
                if asked.as_deref().is_none_or(|typed| !prompt.cuts(typed)) {
                    asked = Some(Cow::Owned(prompt));
                }
            },
            Cow::Borrowed(Note::Typed(typed)) => asked = Some(Cow::Borrowed(typed)),
            Cow::Owned(Note::Typed(typed)) => asked = Some(Cow::Owned(typed)),
            Cow::Borrowed(Note::Stepped(stepped)) => {
                for step in stepped {
                    push_step(&mut last, Cow::Borrowed(step.as_str()), steps_max);
                }
            },
            Cow::Owned(Note::Stepped(stepped)) => {
                for step in stepped {
                    push_step(&mut last, Cow::Owned(step), steps_max);
                }
            },
        }
    }
    assert!(last.len() <= steps_max);
    Digest {
        title: title.map(Cow::into_owned),
        asked: asked.map(Cow::into_owned),
        steps: last.into_iter().map(Cow::into_owned).collect(),
    }
}

fn note_any(record: &Value) -> Note {
    assert_ne!(record["isSidechain"].as_bool(), Some(true));
    match note_claude(record) {
        Note::Silent => note_agy(record),
        note => note,
    }
}

fn note_claude(record: &Value) -> Note {
    assert_ne!(record["isSidechain"].as_bool(), Some(true));
    match record["type"].as_str() {
        Some("ai-title") => record["aiTitle"]
            .as_str()
            .map_or(Note::Silent, |title| Note::Titled(one_line(title, TITLE_CHARS))),
        Some("last-prompt") => {
            let prompt = record["lastPrompt"].as_str().and_then(Asked::recorded);
            prompt.map_or(Note::Silent, Note::Recorded)
        },
        Some("user") => {
            let is_meta = record["isMeta"].as_bool() == Some(true);
            let is_compact_summary = record["isCompactSummary"].as_bool() == Some(true);
            if is_meta || is_compact_summary {
                return Note::Silent;
            }
            typed_by_you(&record["message"]["content"])
                .map_or(Note::Silent, |typed| Note::Typed(Asked::typed(&typed)))
        },
        Some("assistant") => {
            let blocks = record["message"]["content"].as_array().into_iter().flatten();
            Note::Stepped(blocks.filter_map(step).collect())
        },
        _ => Note::Silent,
    }
}

fn note_agy(record: &Value) -> Note {
    assert_ne!(record["isSidechain"].as_bool(), Some(true));
    match record["type"].as_str() {
        Some("USER_INPUT") => {
            if record["source"].as_str() == Some("SYSTEM") {
                return Note::Silent;
            }
            record["content"]
                .as_str()
                .and_then(agy_user_text)
                .map_or(Note::Silent, |typed| Note::Typed(Asked::typed(typed)))
        },
        Some("PLANNER_RESPONSE") => {
            let mut stepped = Vec::new();
            if let Some(text) =
                record["content"].as_str().map(str::trim).filter(|text| !text.is_empty())
            {
                stepped.push(format!("said: {}", one_line(text, STEP_CHARS)));
            }
            for call in record["tool_calls"].as_array().into_iter().flatten() {
                let Some(name) = call["name"].as_str() else {
                    continue;
                };
                let args = agy_args(&call["args"]);
                stepped.push(match on_what(&args) {
                    Some(what) => format!("ran {name}: {}", one_line(what, STEP_CHARS)),
                    None => format!("ran {name}"),
                });
            }
            Note::Stepped(stepped)
        },
        _ => Note::Silent,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    pub tool: Option<(String, Value)>,
    pub said: Option<String>,
}

struct ClaudeCall {
    id: String,
    name: String,
    input: Value,
}

impl ClaudeCall {
    fn of(block: &Value) -> Option<Self> {
        let (id, name) = (block["id"].as_str()?, block["name"].as_str()?);
        Some(ClaudeCall { id: id.to_owned(), name: name.to_owned(), input: block["input"].clone() })
    }
}

impl Turn {
    #[must_use]
    pub fn of(end: &TranscriptEnd) -> Turn {
        match end {
            TranscriptEnd::Claude(lines) => turn_of_claude(lines),
            TranscriptEnd::Antigravity(lines) => turn_of_agy(lines),
        }
    }
}

fn turn_of_claude(end: &str) -> Turn {
    let mut calls: Vec<ClaudeCall> = Vec::new();
    let mut said = None;
    for line in end.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let blocks = record["message"]["content"].as_array().into_iter().flatten();
        match record["type"].as_str() {
            Some("assistant") => {
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().map(str::trim).filter(|t| !t.is_empty());
                            said = text.map(str::to_owned).or(said);
                        },
                        Some("tool_use") => calls.extend(ClaudeCall::of(block)),
                        _ => {},
                    }
                }
            },
            Some("user") => {
                for block in blocks.filter(|block| block["type"] == "tool_result") {
                    if let Some(answered) = block["tool_use_id"].as_str() {
                        calls.retain(|call| call.id != answered);
                    }
                }
            },
            _ => {},
        }
    }
    let tool = calls.pop().map(|call| (call.name, call.input));
    Turn { tool, said }
}

fn turn_of_agy(end: &str) -> Turn {
    let mut tool = None;
    let mut said = None;
    for line in end.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        match record["type"].as_str() {
            Some("USER_INPUT") => {
                tool = None;
            },
            Some("PLANNER_RESPONSE") => {
                if let Some(text) =
                    record["content"].as_str().map(str::trim).filter(|text| !text.is_empty())
                {
                    said = Some(text.to_owned());
                }
                tool = None;
                for call in record["tool_calls"].as_array().into_iter().flatten() {
                    if let Some(name) = call["name"].as_str() {
                        let input = agy_args(&call["args"]);
                        tool = Some((name.to_owned(), input));
                    }
                }
            },
            _ => {},
        }
    }
    Turn { tool, said }
}

fn push_step<T>(last: &mut VecDeque<T>, step: T, steps_max: usize) {
    assert!(last.len() <= steps_max);
    if steps_max == 0 {
        return;
    }
    if last.len() == steps_max {
        last.pop_front();
    }
    last.push_back(step);
    assert!(last.len() <= steps_max);
}

fn agy_user_text(content: &str) -> Option<&str> {
    let text = if let Some((_, rest)) = content.split_once("<USER_REQUEST>") {
        rest.split_once("</USER_REQUEST>").map_or(rest, |(inner, _)| inner)
    } else if content.trim_start().starts_with("<SYSTEM_MESSAGE>") {
        return None;
    } else {
        content
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    assert_ne!(text, "");
    Some(text)
}

fn agy_args(raw: &Value) -> Value {
    let Value::Object(map) = raw else {
        return Value::Null;
    };
    let mut decoded = serde_json::Map::new();
    for key in ON_WHAT_FIELDS {
        let Some(value) = map.get(key) else {
            continue;
        };
        let value = match value {
            Value::String(text) => Value::String(
                serde_json::from_str::<String>(text).unwrap_or_else(|_| text.clone()),
            ),
            other => other.clone(),
        };
        decoded.insert(key.to_owned(), value);
    }
    if let Some(value) = map.get("questions") {
        let value = match value {
            Value::String(text) => {
                serde_json::from_str::<Value>(text).unwrap_or_else(|_| value.clone())
            },
            other => other.clone(),
        };
        decoded.insert("questions".to_owned(), value);
    }
    assert!(decoded.len() <= ON_WHAT_FIELDS.len() + 1);
    Value::Object(decoded)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Whole(String),
    Cut(String),
}

impl Asked {
    fn typed(text: &str) -> Self {
        let line = spaced(text);
        if line.chars().count() <= ASKED_CHARS {
            return Asked::Whole(line);
        }
        Asked::Cut(line.chars().take(ASKED_CHARS - 1).collect())
    }

    fn recorded(text: &str) -> Option<Self> {
        let line = spaced(text);
        if line.is_empty() {
            return None;
        }
        Some(match line.strip_suffix('…') {
            Some(start) if line.chars().count() >= CUT_CHARS => {
                Asked::Cut(start.chars().take(ASKED_CHARS - 1).collect())
            },
            _ => Asked::typed(&line),
        })
    }

    fn cuts(&self, whole: &Asked) -> bool {
        match (self, whole) {
            (Asked::Whole(_), _) => self == whole,
            (Asked::Cut(start), Asked::Whole(text) | Asked::Cut(text)) => text.starts_with(start),
        }
    }

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

const INTERRUPTED: &str = "[Request interrupted by user";

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

#[must_use]
pub fn on_what(input: &Value) -> Option<&str> {
    ON_WHAT_FIELDS
        .iter()
        .find_map(|field| input[field].as_str())
        .or_else(|| input["questions"][0]["question"].as_str())
}

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

fn spaced(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_transcript_s_id_is_a_uuid_s_digits_and_dashes() {
        assert!(SessionId::parse("0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60").is_some());
        for id in ["", "../0b7c5e1a", "0b7c5e1a/x", "id-1", "-0b7c5e1a", "--"] {
            assert_eq!(SessionId::parse(id), None, "{id}");
        }
    }

    fn digest(end: &str, steps: usize) -> Digest {
        fold(notes(end, note_any), steps)
    }

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

    #[test]
    fn a_prompt_with_a_paste_is_the_one_claude_code_recorded() {
        let paste = "<pasted_content id=\"c6de\">\n- the first\n- the second\n</pasted_content>";
        let after_words = format!("file these:\n{paste}\n");
        let one = "file these: - the first - the second";
        assert_eq!(asked(&[typed(&after_words)]).unwrap(), one);
        assert_eq!(asked(&[typed(&after_words), last(one)]).unwrap(), one);
        assert_eq!(asked(&[typed(paste)]).unwrap(), "- the first - the second");
    }

    #[test]
    fn a_compacted_session_s_summary_is_not_what_you_asked() {
        let summary = json!({"type": "user", "isCompactSummary": true, "message": {
            "content": "This session is being continued from a previous conversation"}});
        assert_eq!(asked(&[typed("fold them"), summary.clone()]).unwrap(), "fold them");
        assert_eq!(asked(&[summary]), None);
    }

    #[test]
    fn a_blank_record_of_a_prompt_is_not_what_you_asked() {
        assert_eq!(asked(&[typed("fold them"), last(" \n ")]).unwrap(), "fold them");
        assert_eq!(asked(&[last("")]), None);
    }

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

    #[test]
    fn a_prompt_you_ended_in_an_ellipsis_is_not_cut_short() {
        assert_eq!(Asked::recorded("fold…"), Some(Asked::Whole("fold…".to_owned())));
        assert!(!Asked::recorded("fold…").unwrap().same(&Asked::typed("fold them")));
        assert_eq!(asked(&[typed("fold them"), last("fold…")]).unwrap(), "fold…");
        let start = "fold ".repeat(CUT_CHARS / 5);
        assert_eq!(Asked::recorded(&format!("{start}…")), Some(Asked::Cut(start)));
    }

    #[test]
    fn a_long_prompt_you_typed_is_cut_at_its_most() {
        let most = "é".repeat(ASKED_CHARS);
        assert_eq!(Asked::typed(&most), Asked::Whole(most.clone()));
        let over = Asked::typed(&format!("{most}é"));
        assert_eq!(over, Asked::Cut("é".repeat(ASKED_CHARS - 1)));
        assert_eq!(over.to_string(), one_line(&format!("{most}é"), ASKED_CHARS));
    }

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

    #[test]
    fn the_last_thing_said_is_the_last_text_of_the_main_line() {
        let said = |text: &str, sidechain: bool| {
            let content = serde_json::json!([{ "type": "text", "text": text }, { "type": "tool_use", "name": "Bash", "input": {} }]);
            serde_json::json!({ "type": "assistant", "isSidechain": sidechain, "message": { "content": content } }).to_string()
        };
        let user = serde_json::json!({ "type": "user", "message": { "content": "go" } }).to_string();
        let end = [said("first", false), said("  last  ", false), said("a subagent", true), user].join("\n");
        assert_eq!(last_said_in(&end, 100), Some("last".to_owned()));
        assert_eq!(last_said_in(&end, 2), Some("la…".to_owned()));
        assert_eq!(last_said_in("", 100), None);
    }

    #[test]
    fn an_antigravity_transcript_digests_user_requests_and_planner_steps() {
        let user = json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "content": "<USER_REQUEST>\nfix the sidebar\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nnow\n</ADDITIONAL_METADATA>"
        });
        let tool = json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "tool_calls": [
                {
                    "name": "run_command",
                    "args": {
                        "CommandLine": "\"cargo test\"",
                        "toolSummary": "\"Run tests\""
                    }
                },
                {
                    "name": "ask_question",
                    "args": {
                        "questions": "[{\"question\":\"Keep or drop?\"}]"
                    }
                }
            ]
        });
        let reply = json!({
            "step_index": 2,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "content": "  Fixed the sidebar.  "
        });
        let end = [user, tool, reply].map(|v| v.to_string()).join("\n");
        let parsed = digest(&end, 4);
        assert_eq!(parsed.asked, Some(Asked::Whole("fix the sidebar".to_owned())));
        assert_eq!(parsed.steps, [
            "ran run_command: Run tests",
            "ran ask_question: Keep or drop?",
            "said: Fixed the sidebar.",
        ]);
        assert_eq!(last_said_in(&end, 100), Some("Fixed the sidebar.".to_owned()));
    }

    #[test]
    fn a_reading_finds_antigravity_transcripts_beside_claude_s() {
        let scratch = tempfile::tempdir().unwrap();
        let agy = scratch.path().join("agy");
        let id = SessionId::parse("9c696c75-660f-4097-934f-e1cc290e4bd1").unwrap();
        let path = agy.join("brain").join(id.as_str()).join(".system_generated/logs/transcript.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = json!({"type": "USER_INPUT", "source": "USER_EXPLICIT", "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>"});
        let generic = json!({"type": "GENERIC", "content": "big output"});
        fs::write(&path, format!("{line}\n{generic}\n")).unwrap();

        let reading = Reading::new(vec![id.clone()], 4096).unwrap();
        let folders = Folders { claude: None, agy: Some(agy) };
        let ends = reading.ends(&folders);
        assert_eq!(ends.get(&id), Some(&TranscriptEnd::Antigravity(line.to_string())));
    }

    #[test]
    fn a_tail_and_last_said_read_antigravity_transcript_files_and_ignore_non_summary_args() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let user = json!({
            "type": "USER_INPUT",
            "source": "USER_EXPLICIT",
            "content": "<USER_REQUEST>\naudit this\n</USER_REQUEST>"
        });
        let tool = json!({
            "type": "PLANNER_RESPONSE",
            "tool_calls": [{
                "name": "write_file",
                "args": {
                    "Path": "\"/w/a.rs\"",
                    "Content": "\"huge body that is not parsed\""
                }
            }]
        });
        let reply = json!({
            "type": "PLANNER_RESPONSE",
            "content": "Done auditing."
        });
        fs::write(&path, format!("{user}\n{tool}\n{reply}\n")).unwrap();

        let limits = TailLimits { share_bytes_max: 8192, steps_max: 4 };
        let summary = Tail::new(path.clone(), limits).grown().unwrap().unwrap();
        assert_eq!(summary.asked, Some(Asked::Whole("audit this".to_owned())));
        assert_eq!(summary.steps, ["ran write_file: /w/a.rs", "said: Done auditing."]);
        assert_eq!(last_said(&path, 100).unwrap(), Some("Done auditing.".to_owned()));
        assert_eq!(digest(&format!("{reply}\n"), 0).steps, Vec::<String>::new());

        let decoded = agy_args(&tool["tool_calls"][0]["args"]);
        assert_eq!(decoded["Path"], json!("/w/a.rs"));
        assert!(decoded.get("Content").is_none());

        let literal_args = agy_args(&json!({"Query": "true", "Description": "123"}));
        assert_eq!(on_what(&literal_args), Some("123"));
        assert_eq!(literal_args["Query"], json!("true"));
    }

    const TEST_LIMITS: TailLimits = TailLimits { share_bytes_max: 8192, steps_max: 4 };

    fn said(text: &str) -> String {
        let content = json!([{"type": "text", "text": text}]);
        format!("{}\n", json!({"type": "assistant", "message": {"content": content}}))
    }

    fn unsaid(bytes: usize) -> String {
        let bare = json!({"type": "progress", "data": ""}).to_string().len() + 1;
        let data = "x".repeat(bytes - bare);
        format!("{}\n", json!({"type": "progress", "data": data}))
    }

    fn added(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        io::Write::write_all(&mut file, text.as_bytes()).unwrap();
    }

    fn steps(tail: &mut Tail) -> Vec<String> {
        tail.grown().unwrap().expect("the transcript grew").steps
    }

    #[test]
    fn a_tail_reads_only_what_was_added_since_its_last_look() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let mut tail = Tail::new(path.clone(), TEST_LIMITS);
        added(&path, &said("first"));
        assert_eq!(steps(&mut tail), ["said: first"]);
        assert_eq!(tail.grown().unwrap(), None);

        let length = usize::try_from(fs::metadata(&path).unwrap().len()).unwrap();
        fs::write(&path, format!("{}\n", " ".repeat(length - 1))).unwrap();
        assert_eq!(tail.grown().unwrap(), None);
        added(&path, &said("second"));
        assert_eq!(steps(&mut tail), ["said: first", "said: second"]);
    }

    #[test]
    fn a_record_half_written_waits_for_its_end() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let mut tail = Tail::new(path.clone(), TEST_LIMITS);
        let first = said("first");
        let (start, end) = first.split_at(first.len() / 2);
        added(&path, start);
        assert_eq!(steps(&mut tail), Vec::<String>::new());
        added(&path, end);
        assert_eq!(steps(&mut tail), ["said: first"]);
    }

    #[test]
    fn a_tail_tells_what_a_whole_read_tells() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let limits = TailLimits { share_bytes_max: 700, steps_max: 3 };
        let mut tail = Tail::new(path.clone(), limits);
        let aside = json!({"type": "user", "isSidechain": true, "message": {"content": "aside"}});
        let records = |n: usize| match n % 5 {
            0 => format!("{}\n", typed(&format!("prompt {n}"))),
            1 => said(&format!("step {n}")),
            2 => unsaid(100 + n),
            3 => format!("{}\n", json!({"type": "ai-title", "aiTitle": format!("title {n}")})),
            _ => format!("{aside}\n"),
        };
        for n in 0..40 {
            added(&path, &records(n));
            let whole = fold(
                notes(&end(&path, limits.share_bytes_max, &ALL_KINDS).unwrap(), note_any),
                limits.steps_max,
            );
            assert_eq!(tail.grown().unwrap(), Some(whole), "after record {n}");
        }
    }

    #[test]
    fn a_tail_forgets_what_falls_out_of_its_window() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let window = usize::try_from(WINDOW_BYTES).unwrap();
        let mut looked_between = Tail::new(path.clone(), TEST_LIMITS);
        let mut looked_after = Tail::new(path.clone(), TEST_LIMITS);
        added(&path, &said("old"));
        assert_eq!(steps(&mut looked_between), ["said: old"]);
        assert_eq!(steps(&mut looked_after), ["said: old"]);

        added(&path, &unsaid(window - 10));
        assert_eq!(steps(&mut looked_between), Vec::<String>::new());
        added(&path, &said("new"));
        assert_eq!(steps(&mut looked_between), ["said: new"]);
        assert_eq!(steps(&mut looked_after), ["said: new"]);
        let mut fresh = Tail::new(path.clone(), TEST_LIMITS);
        assert_eq!(steps(&mut fresh), ["said: new"]);
    }

    #[test]
    fn a_transcript_written_anew_is_read_anew() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let mut tail = Tail::new(path.clone(), TEST_LIMITS);
        added(&path, &format!("{}{}", said("first"), said("second")));
        assert_eq!(steps(&mut tail), ["said: first", "said: second"]);
        fs::write(&path, said("other")).unwrap();
        assert_eq!(steps(&mut tail), ["said: other"]);
    }

    #[test]
    fn a_transcript_that_shrank_with_a_half_written_record_is_read_anew() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let mut tail = Tail::new(path.clone(), TEST_LIMITS);
        let first = said("first");
        let trailing = " ".repeat(200);
        added(&path, &format!("{first}{trailing}"));
        assert_eq!(steps(&mut tail), ["said: first"]);

        let replaced = said("replaced and longer than first");
        assert!(replaced.len() > first.len());
        assert!(replaced.len() < first.len() + trailing.len());
        fs::write(&path, &replaced).unwrap();
        assert_eq!(steps(&mut tail), ["said: replaced and longer than first"]);
    }

    #[test]
    fn a_record_over_the_share_clears_earlier_records_without_keeping_it() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let limits = TailLimits { share_bytes_max: 200, steps_max: 4 };
        let mut tail = Tail::new(path.clone(), limits);
        added(&path, &said("before"));
        assert_eq!(steps(&mut tail), ["said: before"]);

        let oversized = said(&"x".repeat(300));
        assert!(oversized.len() > limits.share_bytes_max);
        added(&path, &oversized);
        let whole = fold(
            notes(&end(&path, limits.share_bytes_max, &ALL_KINDS).unwrap(), note_any),
            limits.steps_max,
        );
        assert_eq!(tail.grown().unwrap(), Some(whole));
        assert_eq!(tail.kept.len(), 0);
        assert_eq!(tail.kept_bytes, 0);

        added(&path, &said("after"));
        assert_eq!(steps(&mut tail), ["said: after"]);
    }

    #[test]
    fn a_failed_read_leaves_a_tail_unchanged() {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("transcript.jsonl");
        let mut tail = Tail::new(path.clone(), TEST_LIMITS);
        added(&path, &said("first"));
        assert_eq!(steps(&mut tail), ["said: first"]);

        let read_before = tail.read_offset;
        let seen_before = tail.file_bytes_seen;
        added(&path, &said("second"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(tail.grown().is_err());
        assert_eq!(tail.read_offset, read_before);
        assert_eq!(tail.file_bytes_seen, seen_before);
        assert_eq!(tail.kept.len(), 1);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(steps(&mut tail), ["said: first", "said: second"]);
    }

    #[test]
    fn a_transcript_end_over_the_tail_share_is_refused() {
        let oversized = api::transcript_ends_response::TranscriptEnd {
            harness: api::Harness::ClaudeCode.into(),
            lines: "x".repeat(TAILS_BYTES + 1),
        };
        let refused = TranscriptEnd::of_api(oversized).unwrap_err();
        assert!(refused.contains(&TAILS_BYTES.to_string()), "{refused}");
    }
}
