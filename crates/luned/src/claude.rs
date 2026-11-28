use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use lune_domain::agent::{Declared, Harness, Rung};
use lune_domain::mux::TerminalKey;
use lune_protocol::said::Said;
use serde::Deserialize;

use crate::Directory;
use crate::agent::{
    FILE_BYTES_MAX, Name, Sighting, Source, Unread, absolute, moment, named_pid, nonempty,
    read_file_text, terminal_or_trouble,
};
use crate::log;
use crate::mux;
use crate::process::{self, AgentId, AgentPid};
use crate::statuses::{self, Proof, ReportedAgent, RungUpdate, SOURCE_AGENTS_MAX};
use crate::watch::{self, Troubles};

pub const TORN: Duration = Duration::from_secs(1);

pub(crate) const READS: Declared = Declared::of(Harness::ClaudeCode, Rung::Feed);

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "absolute")]
    cwd: Directory,
    proc_start: RawStart,
    pid_domain: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    status_updated_at: Option<u64>,
    started_at: Option<u64>,
    name: Option<String>,
    name_source: Option<String>,
    entrypoint: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawStart {
    Spelled(String),
    Counted(u64),
}

impl RawStart {
    fn into_start(self) -> Option<process::Start> {
        let spelled = match self {
            RawStart::Spelled(start) => start,
            RawStart::Counted(start) => start.to_string(),
        };
        process::Start::spelled(spelled)
    }
}

#[derive(Clone)]
enum Feed {
    Sdk {
        entrypoint: Said,
    },
    Interactive {
        entrypoint: Option<Said>,
        update: RungUpdate,
    },
}

impl Feed {
    fn parse(
        entrypoint: Option<String>,
        status: Option<&str>,
        waiting_for: Option<&str>,
        status_updated_at: Option<u64>,
    ) -> Self {
        let entrypoint = entrypoint.and_then(|text| Said::new(text.trim().to_owned()));
        match entrypoint {
            Some(entrypoint) if has_sdk_prefix(entrypoint.as_str()) => Feed::Sdk { entrypoint },
            entrypoint => {
                let update = match statuses::parse_word(status, waiting_for) {
                    Some(status) => RungUpdate::Active {
                        status,
                        at: status_updated_at.map(moment),
                    },
                    None => RungUpdate::Clear,
                };
                Feed::Interactive { entrypoint, update }
            }
        }
    }

    fn entrypoint(&self) -> Option<Said> {
        match self {
            Feed::Sdk { entrypoint } => Some(entrypoint.clone()),
            Feed::Interactive { entrypoint, .. } => entrypoint.clone(),
        }
    }

    fn update(&self) -> RungUpdate {
        match self {
            Feed::Sdk { .. } => RungUpdate::Clear,
            Feed::Interactive { update, .. } => update.clone(),
        }
    }
}

#[derive(Clone)]
struct Session {
    id: Said,
    cwd: Directory,
    proc_start: process::Start,
    pid_domain: Option<String>,
    started_at: Option<SystemTime>,
    name: Option<Name>,
    feed: Feed,
    written: SystemTime,
}

impl Session {
    fn parse(raw: RawFile, written: SystemTime) -> io::Result<Self> {
        let proc_start = raw
            .proc_start
            .into_start()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "procStart is empty"))?;
        let name_source = raw
            .name_source
            .as_deref()
            .and_then(|text| Said::new(text.trim().to_owned()))
            .map(|word| Source::of(word.as_str()));
        let name = raw
            .name
            .and_then(|text| Said::new(text.trim().to_owned()))
            .map(|text| Name {
                text,
                source: name_source,
            });
        let feed = Feed::parse(
            raw.entrypoint,
            raw.status.as_deref(),
            raw.waiting_for.as_deref(),
            raw.status_updated_at,
        );
        Ok(Session {
            id: raw.session_id,
            cwd: raw.cwd,
            proc_start,
            pid_domain: raw.pid_domain,
            started_at: raw.started_at.map(moment),
            name,
            feed,
            written,
        })
    }
}

enum Seen {
    Whole(Session),
    Torn {
        since: Instant,
        before: Option<Session>,
    },
    Unread,
}

pub struct Scan {
    pub agents: Vec<ReportedAgent>,
    pub unread: Vec<Unread>,
}

impl Scan {
    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        let Scan { agents, unread } = self;
        let agents = agents.iter().map(|agent| agent.seen().id.pid().get());
        agents
            .chain(unread.iter().map(|unread| unread.pid))
            .collect()
    }
}

pub struct SessionFiles {
    dir: PathBuf,
    domain: process::Domain,
    seen: HashMap<AgentPid, Seen>,
    terminals: HashMap<AgentId, Result<Option<TerminalKey>, String>>,
    troubles: Troubles,
    scanned: bool,
}

impl SessionFiles {
    #[must_use]
    pub fn new(claude: &Path) -> Self {
        SessionFiles {
            dir: claude.join("sessions"),
            domain: process::Domain::here(),
            seen: HashMap::new(),
            terminals: HashMap::new(),
            troubles: Troubles::default(),
            scanned: false,
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn scan(&mut self) -> Scan {
        let mut paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles
                    .say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            }
        };
        if paths.len() > SOURCE_AGENTS_MAX {
            self.troubles.say(format!(
                "{}: {} files exceed the limit of {SOURCE_AGENTS_MAX}",
                self.dir.display(),
                paths.len()
            ));
            paths.sort();
            paths.truncate(SOURCE_AGENTS_MAX);
        }
        self.troubles.done();
        let first = !self.scanned;
        self.scanned = true;
        let mut seen = HashMap::new();
        for path in paths {
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let was = self.seen.remove(&pid);
            let now = match written_text(&path) {
                Ok((text, written)) => match parse_leading_file(&text, written) {
                    Ok(file) => Seen::Whole(file),
                    Err(error) => torn(was, first, &path, &error),
                },
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => torn(was, first, &path, &error),
            };
            seen.insert(pid, now);
        }
        self.seen = seen;

        let mut terminals = HashMap::with_capacity(self.seen.len());
        let mut terminal_of = |id: AgentId| {
            let terminal = self
                .terminals
                .remove(&id)
                .unwrap_or_else(|| mux::terminal_of(&id.to_process_id()));
            terminals.insert(id, terminal.clone());
            terminal
        };
        let mut agents = Vec::with_capacity(self.seen.len());
        let mut unread = Vec::new();
        for (&pid, found) in &self.seen {
            let session = match found {
                Seen::Whole(session)
                | Seen::Torn {
                    before: Some(session),
                    ..
                } => session,
                Seen::Torn { before: None, .. } => continue,
                Seen::Unread => {
                    if let Some(start) = process::start(pid.get()) {
                        let terminal = terminal_of(AgentId::new(pid, start)).ok().flatten();
                        unread.push(Unread {
                            pid: pid.get(),
                            terminal,
                        });
                    }
                    continue;
                }
            };
            if !local(&self.domain, session) {
                continue;
            }
            let Some(start) =
                process::start(pid.get()).filter(|start| *start == session.proc_start)
            else {
                continue;
            };
            let id = AgentId::new(pid, start);
            let read = terminal_of(id.clone());
            let terminal = terminal_or_trouble(pid, read, &mut self.troubles);
            agents.push(reported(id, session, terminal));
        }
        self.terminals = terminals;
        assert!(agents.len() + unread.len() <= self.seen.len());
        Scan { agents, unread }
    }
}

fn local(domain: &process::Domain, session: &Session) -> bool {
    session
        .pid_domain
        .as_deref()
        .is_none_or(|recorded| domain.admits(recorded))
}

fn torn(was: Option<Seen>, first: bool, path: &Path, error: &impl fmt::Display) -> Seen {
    let unread = || {
        log::say(&format!(
            "{} does not read as a session file: {error}",
            path.display()
        ));
        Seen::Unread
    };
    match was {
        Some(Seen::Whole(session)) => Seen::Torn {
            since: Instant::now(),
            before: Some(session),
        },
        None if first => unread(),
        None => Seen::Torn {
            since: Instant::now(),
            before: None,
        },
        Some(Seen::Torn { since, before }) if since.elapsed() < TORN => {
            Seen::Torn { since, before }
        }
        Some(Seen::Torn { .. }) => unread(),
        Some(Seen::Unread) => Seen::Unread,
    }
}

const SDK_ENTRYPOINT_PREFIX: &str = "sdk-";
const _: () = assert!(SDK_ENTRYPOINT_PREFIX.is_ascii());

fn written_text(path: &Path) -> io::Result<(Vec<u8>, SystemTime)> {
    let file = fs::File::open(path)?;
    let written = file.metadata()?.modified()?;
    Ok((read_file_text(file)?, written))
}

fn parse_leading_file(bytes: &[u8], written: SystemTime) -> io::Result<Session> {
    let bytes_len = u64::try_from(bytes.len()).expect("a slice length fits a u64");
    assert!(bytes_len <= FILE_BYTES_MAX);
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<RawFile>();
    let raw = match stream.next() {
        Some(Ok(raw)) => raw,
        Some(Err(error)) => return Err(io::Error::new(ErrorKind::InvalidData, error)),
        None => {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "empty session file",
            ));
        }
    };
    assert!(stream.byte_offset() > 0);
    assert!(stream.byte_offset() <= bytes.len());
    Session::parse(raw, written)
}

#[cfg(test)]
pub(crate) fn update_of_file(bytes: &[u8]) -> io::Result<RungUpdate> {
    Ok(parse_leading_file(bytes, SystemTime::UNIX_EPOCH)?
        .feed
        .update())
}

fn has_sdk_prefix(text: &str) -> bool {
    text.as_bytes()
        .get(..SDK_ENTRYPOINT_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SDK_ENTRYPOINT_PREFIX.as_bytes()))
}

fn reported(id: AgentId, session: &Session, terminal: Option<TerminalKey>) -> ReportedAgent {
    let sighting = Sighting {
        session_id: Some(session.id.clone()),
        cwd: session.cwd.clone(),
        started_at: session.started_at,
        name: session.name.clone(),
        entrypoint: session.feed.entrypoint(),
        terminal,
    };
    let proof = Proof::Started {
        written: session.written,
    };
    ReportedAgent::new(READS, id, sighting, session.feed.update(), proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{AgentPid, Start as ProcessStart};

    #[test]
    fn parse_leading_file_tolerates_stray_tails_and_rejects_truncated_objects() {
        let base = br#"{"sessionId":"s1","cwd":"/w","procStart":"42","status":"idle"}"#;
        let tails: [&[u8]; 4] = [
            b"",
            br#"rompt","statusUpdatedAt":1792859293927}"#,
            br#"{"sessionId":"old","cwd":"/old","procStart":"99","status":"busy"}"#,
            b"\0\xff\xfe",
        ];
        for tail in tails {
            let mut bytes = base.to_vec();
            bytes.extend_from_slice(tail);
            let parsed = parse_leading_file(&bytes, SystemTime::UNIX_EPOCH).unwrap();
            assert_eq!(parsed.id.as_str(), "s1");
            assert_eq!(
                parsed.feed.update(),
                RungUpdate::Active {
                    status: statuses::ActiveStatus::Idle,
                    at: None
                }
            );
        }
        for invalid in [
            &b""[..],
            br#"{"sessionId":"s1","cwd":"/w""#,
            b"[]",
            b"null",
            br#"{"sessionId":"","cwd":"/w","procStart":"42"}"#,
            br#"{"sessionId":"   ","cwd":"/w","procStart":"42"}"#,
            br#"{"sessionId":"s1","cwd":"/w","procStart":""}"#,
            br#"{"sessionId":"s1","cwd":"/w","procStart":"   "}"#,
        ] {
            assert!(parse_leading_file(invalid, SystemTime::UNIX_EPOCH).is_err());
        }
    }

    #[test]
    fn sdk_entrypoints_clear_feed_status_while_keeping_agent_metadata() {
        let id = AgentId::new(
            AgentPid::new(42).unwrap(),
            ProcessStart::spelled("100".to_owned()).unwrap(),
        );
        let session = |entrypoint: Option<&str>| {
            let raw = RawFile {
                session_id: Said::new("s1".to_owned()).unwrap(),
                cwd: Directory::parse("/w".to_owned()).unwrap(),
                proc_start: RawStart::Spelled("100".to_owned()),
                pid_domain: None,
                status: Some("busy".to_owned()),
                waiting_for: None,
                status_updated_at: Some(1_000),
                started_at: Some(500),
                name: None,
                name_source: None,
                entrypoint: entrypoint.map(str::to_owned),
            };
            Session::parse(raw, SystemTime::UNIX_EPOCH).unwrap()
        };
        for sdk in ["sdk-cli", "sdk-ts", "  SDK-py  ", "sdk-"] {
            let entry = reported(id.clone(), &session(Some(sdk)), None);
            assert_eq!(entry.report().update(), &RungUpdate::Clear, "{sdk}");
            assert_eq!(
                entry.seen().entrypoint.as_ref().map(Said::as_str),
                Some(sdk.trim())
            );
        }
        for interactive in [Some("cli"), Some("claude-vscode"), Some("sdk"), None] {
            let entry = reported(id.clone(), &session(interactive), None);
            let update = entry.report().update();
            assert!(
                matches!(update, RungUpdate::Active { .. }),
                "{interactive:?}"
            );
        }
    }
}
