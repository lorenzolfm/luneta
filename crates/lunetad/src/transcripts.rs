//! The ends of Claude Code's transcripts, as luneta's brief reads them: of
//! each, the last records that tell what its agent is doing, whole ones
//! alone, as many as its share of what a brief keeps has room for. luneta
//! reads them for this machine's agents; a daemon reads them here for a
//! luneta on another machine, whose brief tells of agents of this one.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

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

/// The records of the kinds the brief reads, by the `type` each names.
const KINDS: [&str; 4] =
    [r#""type":"ai-title""#, r#""type":"last-prompt""#, r#""type":"user""#, r#""type":"assistant""#];

/// The id Claude Code gives a session, which names its transcript: a UUID's
/// hex digits and dashes, and nothing that would take the file it names out
/// of its folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let uuid = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
        uuid.then(|| SessionId(id.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
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
            return Err(format!("ids: {} transcripts, of {IDS_MAX} at the most", ids.len()));
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

    /// What reading the ends of `count` transcripts together keeps of each:
    /// an equal share of `TAILS_BYTES`.
    #[must_use]
    pub fn share_of(count: usize) -> usize {
        TAILS_BYTES / count.max(1)
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
        let mut dirs: Vec<PathBuf> = fs::read_dir(projects)
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        dirs.sort();
        let mut ends = HashMap::new();
        for id in &self.ids {
            let file = format!("{}.jsonl", id.as_str());
            let Some(path) = dirs.iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
            else {
                continue;
            };
            if let Ok(end) = end(&path, self.share) {
                ends.insert(id.clone(), end);
            }
        }
        ends
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript's id names a file in Claude Code's folder, and one that
    /// would name a file elsewhere names none.
    #[test]
    fn a_transcript_s_id_is_a_uuid_s_digits_and_dashes() {
        assert!(SessionId::parse("0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60").is_some());
        for id in ["", "../0b7c5e1a", "0b7c5e1a/x", "id-1"] {
            assert_eq!(SessionId::parse(id), None, "{id}");
        }
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
        let share = |count| u32::try_from(Reading::share_of(count)).unwrap();
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
}
