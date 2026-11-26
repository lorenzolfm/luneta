use std::fmt;
use std::num::NonZeroUsize;
use std::ops::Deref;

use crate::highlight::{Language, TokenSpan, highlight_line};
use crate::words::{ByteSpan, pair_emphasis};

pub const PATCH_BYTES_MAX: usize = 8 * 1024 * 1024;

pub const FILES_MAX: usize = 1024;

pub const HUNKS_MAX: usize = 4096;

pub const LINES_MAX: usize = 200_000;

pub const LINE_BYTES_MAX: usize = 8192;

const _: () = assert!(PATCH_BYTES_MAX > 0);
const _: () = assert!(FILES_MAX > 0);
const _: () = assert!(HUNKS_MAX >= FILES_MAX);
const _: () = assert!(LINES_MAX >= HUNKS_MAX);
const _: () = assert!(LINE_BYTES_MAX >= 256);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FilePath(String);

impl FilePath {
    #[must_use]
    pub fn parse(raw: &str) -> Option<FilePath> {
        let bounded = bound_line(raw);
        bounded
            .split('/')
            .any(|part| !part.is_empty())
            .then_some(FilePath(bounded))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|part| !part.is_empty())
    }
}

impl AsRef<str> for FilePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for FilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for FilePath {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for FilePath {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmpty<T> {
    items: Vec<T>,
}

impl<T> NonEmpty<T> {
    #[must_use]
    pub fn new(items: Vec<T>) -> Option<NonEmpty<T>> {
        (!items.is_empty()).then_some(NonEmpty { items })
    }

    #[must_use]
    pub fn count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.items.len()).expect("NonEmpty contains at least one item")
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.items
    }
}

impl<T> Deref for NonEmpty<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileStatus {
    #[must_use]
    pub const fn badge(self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Added(FilePath),
    Deleted(FilePath),
    Modified(FilePath),
    Renamed { old: FilePath, new: FilePath },
}

impl FileChange {
    #[must_use]
    pub fn path(&self) -> &FilePath {
        match self {
            FileChange::Added(path)
            | FileChange::Deleted(path)
            | FileChange::Modified(path)
            | FileChange::Renamed { new: path, .. } => path,
        }
    }

    #[must_use]
    pub fn old_path(&self) -> Option<&FilePath> {
        match self {
            FileChange::Added(_) => None,
            FileChange::Deleted(path)
            | FileChange::Modified(path)
            | FileChange::Renamed { old: path, .. } => Some(path),
        }
    }

    #[must_use]
    pub fn new_path(&self) -> Option<&FilePath> {
        match self {
            FileChange::Deleted(_) => None,
            FileChange::Added(path)
            | FileChange::Modified(path)
            | FileChange::Renamed { new: path, .. } => Some(path),
        }
    }

    #[must_use]
    pub const fn status(&self) -> FileStatus {
        match self {
            FileChange::Added(_) => FileStatus::Added,
            FileChange::Deleted(_) => FileStatus::Deleted,
            FileChange::Modified(_) => FileStatus::Modified,
            FileChange::Renamed { .. } => FileStatus::Renamed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextLine {
    pub old_line: u32,
    pub new_line: u32,
    pub text: String,
    pub syntax: Vec<TokenSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedLine {
    pub line: u32,
    pub text: String,
    pub syntax: Vec<TokenSpan>,
    pub emphasis: Vec<ByteSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangedBlock {
    Deleted(NonEmpty<ChangedLine>),
    Added(NonEmpty<ChangedLine>),
    Modified {
        deleted: NonEmpty<ChangedLine>,
        added: NonEmpty<ChangedLine>,
    },
}

impl ChangedBlock {
    #[must_use]
    pub fn deleted(&self) -> &[ChangedLine] {
        match self {
            ChangedBlock::Deleted(lines) | ChangedBlock::Modified { deleted: lines, .. } => lines,
            ChangedBlock::Added(_) => &[],
        }
    }

    #[must_use]
    pub fn added(&self) -> &[ChangedLine] {
        match self {
            ChangedBlock::Added(lines) | ChangedBlock::Modified { added: lines, .. } => lines,
            ChangedBlock::Deleted(_) => &[],
        }
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.deleted().len() + self.added().len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Context(NonEmpty<ContextLine>),
    Changed(ChangedBlock),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub heading: String,
    pub blocks: Vec<Block>,
}

impl Hunk {
    #[must_use]
    pub fn header_text(&self) -> String {
        let base = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        );
        if self.heading.is_empty() {
            base
        } else {
            format!("{base} {}", self.heading)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileBody {
    Binary,
    Text(Vec<Hunk>),
}

impl FileBody {
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        matches!(self, FileBody::Binary)
    }

    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        match self {
            FileBody::Binary => &[],
            FileBody::Text(hunks) => hunks,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePatch {
    pub change: FileChange,
    pub body: FileBody,
    pub added: u32,
    pub deleted: u32,
}

impl FilePatch {
    #[must_use]
    pub fn path(&self) -> &FilePath {
        self.change.path()
    }

    #[must_use]
    pub const fn status(&self) -> FileStatus {
        self.change.status()
    }

    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.body.is_binary()
    }

    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        self.body.hunks()
    }

    #[must_use]
    pub fn language(&self) -> Language {
        Language::from_path(self.path().as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub files: Vec<FilePatch>,
    pub added: u32,
    pub deleted: u32,
    pub truncated: bool,
}

impl Patch {
    #[must_use]
    pub fn parse(raw: &[u8]) -> Patch {
        let truncated = raw.len() > PATCH_BYTES_MAX;
        let slice = &raw[..raw.len().min(PATCH_BYTES_MAX)];
        let text = String::from_utf8_lossy(slice);
        let mut parser = Parser::new(truncated);
        for line in text.lines() {
            if !parser.feed(line) {
                break;
            }
        }
        let patch = parser.finish();
        assert!(patch.files.len() <= FILES_MAX, "parsed file count stays bounded");
        patch
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.files
            .iter()
            .flat_map(FilePatch::hunks)
            .flat_map(|hunk| &hunk.blocks)
            .map(|block| match block {
                Block::Context(lines) => lines.len(),
                Block::Changed(changed) => changed.line_count(),
            })
            .sum()
    }

    pub fn push_untracked(&mut self, path: FilePath, content: &str) {
        if self.files.len() >= FILES_MAX {
            self.truncated = true;
            return;
        }
        if self.files.iter().any(|file| file.path() == &path) {
            return;
        }
        let existing_lines = self.line_count();
        if existing_lines >= LINES_MAX {
            self.truncated = true;
            return;
        }
        let remaining_lines = LINES_MAX - existing_lines;
        let language = Language::from_path(path.as_str());
        let mut added_lines = Vec::new();
        for (index, line) in content.lines().enumerate() {
            if added_lines.len() >= remaining_lines {
                self.truncated = true;
                break;
            }
            let text = bound_line(line);
            let syntax = highlight_line(&text, language);
            let line_number = u32::try_from(index + 1).expect("bounded line index fits u32");
            added_lines.push(ChangedLine {
                line: line_number,
                text,
                syntax,
                emphasis: Vec::new(),
            });
        }
        assert!(
            existing_lines + added_lines.len() <= LINES_MAX,
            "cumulative patch lines stay within LINES_MAX"
        );
        let count = u32::try_from(added_lines.len()).expect("bounded line count fits u32");
        let hunks = match NonEmpty::new(added_lines) {
            None => Vec::new(),
            Some(lines) => vec![Hunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: count,
                heading: String::new(),
                blocks: vec![Block::Changed(ChangedBlock::Added(lines))],
            }],
        };
        self.added = self.added.saturating_add(count);
        self.files.push(FilePatch {
            change: FileChange::Added(path),
            body: FileBody::Text(hunks),
            added: count,
            deleted: 0,
        });
        assert!(self.files.len() <= FILES_MAX, "file count stays within FILES_MAX");
    }
}

struct FileBuilder {
    old_path: Option<FilePath>,
    new_path: Option<FilePath>,
    status: FileStatus,
    binary: bool,
    hunks: Vec<Hunk>,
    added: u32,
    deleted: u32,
}

impl FileBuilder {
    fn language(&self) -> Language {
        let path = self
            .new_path
            .as_ref()
            .or(self.old_path.as_ref())
            .map_or("", FilePath::as_str);
        Language::from_path(path)
    }

    fn finish(self) -> Option<FilePatch> {
        let change = match self.status {
            FileStatus::Added => FileChange::Added(self.new_path.or(self.old_path)?),
            FileStatus::Deleted => FileChange::Deleted(self.old_path.or(self.new_path)?),
            FileStatus::Renamed => match (self.old_path, self.new_path) {
                (Some(old), Some(new)) => FileChange::Renamed { old, new },
                (Some(path), None) | (None, Some(path)) => FileChange::Modified(path),
                (None, None) => return None,
            },
            FileStatus::Modified => FileChange::Modified(self.new_path.or(self.old_path)?),
        };
        let (body, added, deleted) = if self.binary {
            (FileBody::Binary, 0, 0)
        } else {
            (FileBody::Text(self.hunks), self.added, self.deleted)
        };
        Some(FilePatch {
            change,
            body,
            added,
            deleted,
        })
    }
}

struct Parser {
    files: Vec<FilePatch>,
    current_file: Option<FileBuilder>,
    current_hunk: Option<HunkBuilder>,
    total_hunks: usize,
    total_lines: usize,
    truncated: bool,
}

struct HunkBuilder {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
    heading: String,
    old_next: u32,
    new_next: u32,
    blocks: Vec<Block>,
    pending_context: Vec<ContextLine>,
    pending_deleted: Vec<ChangedLine>,
    pending_added: Vec<ChangedLine>,
}

impl HunkBuilder {
    fn new(
        old_start: u32,
        old_count: u32,
        new_start: u32,
        new_count: u32,
        heading: String,
    ) -> Self {
        HunkBuilder {
            old_start,
            old_count,
            new_start,
            new_count,
            heading,
            old_next: old_start,
            new_next: new_start,
            blocks: Vec::new(),
            pending_context: Vec::new(),
            pending_deleted: Vec::new(),
            pending_added: Vec::new(),
        }
    }

    fn flush_changed(&mut self) {
        if self.pending_deleted.is_empty() && self.pending_added.is_empty() {
            return;
        }
        let mut deleted = std::mem::take(&mut self.pending_deleted);
        let mut added = std::mem::take(&mut self.pending_added);
        let deleted_refs: Vec<&str> = deleted.iter().map(|line| line.text.as_str()).collect();
        let added_refs: Vec<&str> = added.iter().map(|line| line.text.as_str()).collect();
        let (deleted_emphasis, added_emphasis) = pair_emphasis(&deleted_refs, &added_refs);
        assert_eq!(deleted_emphasis.len(), deleted.len(), "deleted emphasis matches lines");
        assert_eq!(added_emphasis.len(), added.len(), "added emphasis matches lines");
        for (line, emphasis) in deleted.iter_mut().zip(deleted_emphasis) {
            line.emphasis = emphasis;
        }
        for (line, emphasis) in added.iter_mut().zip(added_emphasis) {
            line.emphasis = emphasis;
        }
        let changed = match (NonEmpty::new(deleted), NonEmpty::new(added)) {
            (Some(deleted), Some(added)) => ChangedBlock::Modified { deleted, added },
            (Some(deleted), None) => ChangedBlock::Deleted(deleted),
            (None, Some(added)) => ChangedBlock::Added(added),
            (None, None) => return,
        };
        self.blocks.push(Block::Changed(changed));
    }

    fn flush_context(&mut self) {
        let Some(lines) = NonEmpty::new(std::mem::take(&mut self.pending_context)) else {
            return;
        };
        self.blocks.push(Block::Context(lines));
    }

    fn push_context(&mut self, text: String, language: Language) {
        self.flush_changed();
        let syntax = highlight_line(&text, language);
        let old_line = self.old_next;
        let new_line = self.new_next;
        self.old_next = self.old_next.saturating_add(1);
        self.new_next = self.new_next.saturating_add(1);
        self.pending_context.push(ContextLine {
            old_line,
            new_line,
            text,
            syntax,
        });
    }

    fn push_deleted(&mut self, text: String, language: Language) {
        self.flush_context();
        let syntax = highlight_line(&text, language);
        let line = self.old_next;
        self.old_next = self.old_next.saturating_add(1);
        self.pending_deleted.push(ChangedLine {
            line,
            text,
            syntax,
            emphasis: Vec::new(),
        });
    }

    fn push_added(&mut self, text: String, language: Language) {
        self.flush_context();
        let syntax = highlight_line(&text, language);
        let line = self.new_next;
        self.new_next = self.new_next.saturating_add(1);
        self.pending_added.push(ChangedLine {
            line,
            text,
            syntax,
            emphasis: Vec::new(),
        });
    }

    fn finish(mut self) -> Hunk {
        self.flush_context();
        self.flush_changed();
        assert!(self.pending_context.is_empty(), "all pending context flushed");
        assert!(self.pending_deleted.is_empty(), "all pending deletions flushed");
        assert!(self.pending_added.is_empty(), "all pending additions flushed");
        Hunk {
            old_start: self.old_start,
            old_count: self.old_count,
            new_start: self.new_start,
            new_count: self.new_count,
            heading: self.heading,
            blocks: self.blocks,
        }
    }
}

impl Parser {
    fn new(truncated: bool) -> Self {
        Parser {
            files: Vec::new(),
            current_file: None,
            current_hunk: None,
            total_hunks: 0,
            total_lines: 0,
            truncated,
        }
    }

    fn feed(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.finish_file();
            if self.files.len() >= FILES_MAX {
                self.truncated = true;
                return false;
            }
            let (old_path, new_path) = parse_git_paths(rest);
            self.current_file = Some(FileBuilder {
                old_path,
                new_path,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
                added: 0,
                deleted: 0,
            });
            return true;
        }
        let Some(current) = self.current_file.as_ref() else {
            return true;
        };
        if current.binary {
            return true;
        }
        if let Some(hunk) = parse_hunk_header(line) {
            self.finish_hunk();
            if self.total_hunks >= HUNKS_MAX {
                self.truncated = true;
                return false;
            }
            self.total_hunks += 1;
            self.current_hunk = Some(hunk);
            return true;
        }
        if let Some(hunk) = self.current_hunk.as_mut() {
            if self.total_lines >= LINES_MAX {
                self.truncated = true;
                return false;
            }
            let file = self
                .current_file
                .as_mut()
                .expect("a hunk belongs to a file");
            let language = file.language();
            if let Some(body) = line.strip_prefix('+') {
                self.total_lines += 1;
                file.added = file.added.saturating_add(1);
                hunk.push_added(bound_line(body), language);
                return true;
            }
            if let Some(body) = line.strip_prefix('-') {
                self.total_lines += 1;
                file.deleted = file.deleted.saturating_add(1);
                hunk.push_deleted(bound_line(body), language);
                return true;
            }
            if let Some(body) = line.strip_prefix(' ') {
                self.total_lines += 1;
                hunk.push_context(bound_line(body), language);
                return true;
            }
            if line.is_empty() {
                self.total_lines += 1;
                hunk.push_context(String::new(), language);
                return true;
            }
            if line.starts_with("\\ No newline at end of file") {
                return true;
            }
            return true;
        }
        let file = self
            .current_file
            .as_mut()
            .expect("current_file checked above");
        if line.starts_with("new file mode ") {
            file.status = FileStatus::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode ") {
            file.status = FileStatus::Deleted;
            file.new_path = None;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = FilePath::parse(&unquote_path(from));
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.status = FileStatus::Renamed;
            file.new_path = FilePath::parse(&unquote_path(to));
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
            file.hunks.clear();
        } else if let Some(old) = line.strip_prefix("--- ") {
            if old == "/dev/null" {
                file.status = FileStatus::Added;
                file.old_path = None;
            } else {
                file.old_path = FilePath::parse(&strip_prefix_ab(&unquote_path(old), "a/"));
            }
        } else if let Some(new) = line.strip_prefix("+++ ") {
            if new == "/dev/null" {
                file.status = FileStatus::Deleted;
                file.new_path = None;
            } else {
                file.new_path = FilePath::parse(&strip_prefix_ab(&unquote_path(new), "b/"));
            }
        }
        true
    }

    fn finish_hunk(&mut self) {
        if let Some(hunk) = self.current_hunk.take()
            && let Some(file) = self.current_file.as_mut()
            && !file.binary
        {
            file.hunks.push(hunk.finish());
        }
    }

    fn finish_file(&mut self) {
        self.finish_hunk();
        if let Some(file) = self.current_file.take().and_then(FileBuilder::finish) {
            self.files.push(file);
        }
    }

    fn finish(mut self) -> Patch {
        self.finish_file();
        assert!(self.files.len() <= FILES_MAX, "parsed files respect FILES_MAX");
        assert!(self.total_hunks <= HUNKS_MAX, "parsed hunks respect HUNKS_MAX");
        assert!(self.total_lines <= LINES_MAX, "parsed lines respect LINES_MAX");
        let added = self
            .files
            .iter()
            .fold(0u32, |acc, file| acc.saturating_add(file.added));
        let deleted = self
            .files
            .iter()
            .fold(0u32, |acc, file| acc.saturating_add(file.deleted));
        Patch {
            files: self.files,
            added,
            deleted,
            truncated: self.truncated,
        }
    }
}

fn bound_line(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let slice = if line.len() <= LINE_BYTES_MAX {
        line
    } else {
        let mut end = LINE_BYTES_MAX;
        while end > 0 && !line.is_char_boundary(end) {
            end -= 1;
        }
        assert!(end <= LINE_BYTES_MAX, "bounded line stays within LINE_BYTES_MAX");
        assert!(line.is_char_boundary(end), "bounded line ends on UTF-8 boundary");
        &line[..end]
    };
    if slice.as_bytes().contains(&0) {
        slice.replace('\0', "")
    } else {
        slice.to_owned()
    }
}

fn parse_hunk_header(line: &str) -> Option<HunkBuilder> {
    let rest = line.strip_prefix("@@ -")?;
    let (range, tail) = rest.split_once(" @@")?;
    let (old_part, new_part) = range.split_once(" +")?;
    let (old_start, old_count) = parse_range(old_part)?;
    let (new_start, new_count) = parse_range(new_part)?;
    let heading = bound_line(tail.strip_prefix(' ').unwrap_or(tail));
    Some(HunkBuilder::new(
        old_start, old_count, new_start, new_count, heading,
    ))
}

fn parse_range(part: &str) -> Option<(u32, u32)> {
    match part.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((part.parse().ok()?, 1)),
    }
}

fn parse_git_paths(rest: &str) -> (Option<FilePath>, Option<FilePath>) {
    if let Some((first, after)) = split_git_path_token(rest) {
        let second = split_git_path_token(after.trim_start()).map(|(token, _)| token);
        let old = FilePath::parse(&strip_prefix_ab(&first, "a/"));
        let new = second.and_then(|path| FilePath::parse(&strip_prefix_ab(&path, "b/")));
        return (old, new);
    }
    (None, None)
}

fn split_git_path_token(input: &str) -> Option<(String, &str)> {
    if let Some(quoted) = input.strip_prefix('"') {
        let bytes = quoted.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\\' && at + 1 < bytes.len() {
                at += 2;
                continue;
            }
            if bytes[at] == b'"' {
                let raw = &input[..at + 2];
                return Some((unquote_path(raw), &quoted[at + 1..]));
            }
            at += 1;
        }
        return None;
    }
    match input.find(" b/") {
        Some(pos) => Some((bound_line(&input[..pos]), &input[pos + 1..])),
        None => match input.split_once(' ') {
            Some((first, rest)) => Some((bound_line(first), rest)),
            None if !input.is_empty() => Some((bound_line(input), "")),
            None => None,
        },
    }
}

fn strip_prefix_ab(path: &str, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(path).to_owned()
}

fn unquote_path(raw: &str) -> String {
    let raw = raw.split('\t').next().unwrap_or(raw);
    let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return bound_line(raw);
    };
    let mut bytes = Vec::with_capacity(inner.len().min(LINE_BYTES_MAX));
    let source = inner.as_bytes();
    let mut at = 0;
    while at < source.len() && bytes.len() < LINE_BYTES_MAX {
        if source[at] == b'\\' && at + 1 < source.len() {
            at += 1;
            match source[at] {
                b'n' => bytes.push(b'\n'),
                b't' => bytes.push(b'\t'),
                b'r' => bytes.push(b'\r'),
                b'\\' => bytes.push(b'\\'),
                b'"' => bytes.push(b'"'),
                b'0'..=b'7'
                    if at + 2 < source.len()
                        && matches!(source[at + 1], b'0'..=b'7')
                        && matches!(source[at + 2], b'0'..=b'7') =>
                {
                    let octal = u16::from(source[at] - b'0') * 64
                        + u16::from(source[at + 1] - b'0') * 8
                        + u16::from(source[at + 2] - b'0');
                    if let Ok(value) = u8::try_from(octal) {
                        bytes.push(value);
                        at += 2;
                    } else {
                        bytes.push(source[at]);
                    }
                }
                other => bytes.push(other),
            }
        } else {
            bytes.push(source[at]);
        }
        at += 1;
    }
    assert!(bytes.len() <= LINE_BYTES_MAX, "unquoted path stays bounded");
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/crates/repo/src/commit_stats.rs b/crates/repo/src/commit_stats.rs
index 1234567..89abcdef 100644
--- a/crates/repo/src/commit_stats.rs
+++ b/crates/repo/src/commit_stats.rs
@@ -14,7 +14,7 @@
 const FETCH_MONTHS: u32 = 12;
-const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
+const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
 const DEFAULT_CAPACITY: usize = 4_096;
@@ -24,9 +24,7 @@ struct CommitMonth {
 struct CommitMonth {
-    #[serde(rename = \"repoCount\")]
-    repo_count: i64,
-    repos: Vec<RepoCommits>,
+    groups: Vec<CommitGroup>,
 }
";

    #[test]
    fn parses_hunks_blocks_line_numbers_and_word_emphasis() {
        let patch = Patch::parse(SAMPLE.as_bytes());
        assert!(!patch.truncated);
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.added, 2);
        assert_eq!(patch.deleted, 4);
        let file = &patch.files[0];
        assert_eq!(file.path(), &"crates/repo/src/commit_stats.rs");
        assert_eq!(file.status(), FileStatus::Modified);
        assert_eq!(file.hunks().len(), 2);

        let hunk0 = &file.hunks()[0];
        assert_eq!(hunk0.blocks.len(), 3);
        let Block::Changed(ChangedBlock::Modified { deleted, added }) = &hunk0.blocks[1] else {
            panic!("expected modified changed block");
        };
        assert_eq!(deleted[0].line, 15);
        assert_eq!(added[0].line, 15);
        assert_eq!(added[0].emphasis.len(), 1);
        assert_eq!(
            &added[0].text[added[0].emphasis[0].start..added[0].emphasis[0].end],
            "4 * "
        );

        let hunk1 = &file.hunks()[1];
        assert_eq!(hunk1.heading, "struct CommitMonth {");
        let Block::Changed(ChangedBlock::Modified { deleted, added }) = &hunk1.blocks[1] else {
            panic!("expected modified changed block");
        };
        assert_eq!(deleted.len(), 3);
        assert_eq!(added.len(), 1);
    }

    #[test]
    fn untracked_file_is_appended_as_added_file() {
        let mut patch = Patch::parse(b"");
        patch.push_untracked(FilePath::parse("src/new.rs").unwrap(), "fn main() {}\n");
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].status(), FileStatus::Added);
        assert_eq!(patch.added, 1);
        assert_eq!(patch.deleted, 0);
    }

    #[test]
    fn handles_added_deleted_renamed_and_binary_files() {
        let raw = "\
diff --git a/old name.rs b/new name.rs
similarity index 100%
rename from old name.rs
rename to new name.rs
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-bye
";
        let patch = Patch::parse(raw.as_bytes());
        assert_eq!(patch.files.len(), 3);
        assert_eq!(patch.files[0].status(), FileStatus::Renamed);
        assert_eq!(patch.files[0].change.old_path().map(FilePath::as_str), Some("old name.rs"));
        assert_eq!(patch.files[0].change.new_path().map(FilePath::as_str), Some("new name.rs"));
        assert!(patch.files[1].is_binary());
        assert_eq!(patch.files[1].hunks(), []);
        assert_eq!(patch.files[1].status(), FileStatus::Added);
        assert_eq!(patch.files[2].status(), FileStatus::Deleted);
        assert_eq!(patch.files[2].deleted, 1);
    }

    #[test]
    fn malformed_pathless_headers_are_skipped_and_binary_ignores_hunks() {
        assert!(FilePath::parse("").is_none());
        assert!(FilePath::parse("///").is_none());
        let raw = [
            "diff --git \"unclosed",
            "diff --git a/bin.dat b/bin.dat",
            "Binary files a/bin.dat and b/bin.dat differ",
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(raw.as_bytes());
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].path(), &"bin.dat");
        assert!(patch.files[0].is_binary());
        assert_eq!(patch.files[0].hunks(), []);
        assert_eq!((patch.added, patch.deleted), (0, 0));
        assert_eq!(bound_line("a\0b"), "ab");
    }

    #[test]
    fn untracked_files_respect_cumulative_lines_max_and_safe_octal_unquote() {
        let mut patch = Patch::parse(b"");
        let chunk = "a\n".repeat(LINES_MAX / 2 + 10);
        patch.push_untracked(FilePath::parse("one.txt").unwrap(), &chunk);
        patch.push_untracked(FilePath::parse("two.txt").unwrap(), &chunk);
        assert!(patch.truncated);
        assert!(patch.line_count() <= LINES_MAX);
        assert_eq!(unquote_path("\"\\0—x\""), "0—x");
    }
}
