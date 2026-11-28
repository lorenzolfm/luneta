use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, ErrorKind, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use lune_domain::agent::Harness;
use lune_protocol::program_resume::ProcessArgv;
use lune_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{Agent, Name, Source};
use crate::core::{self, Command};
use crate::process::{self, AgentId, AgentPid};
use crate::resume::trust;
use crate::statuses::SOURCE_AGENTS_MAX;
use crate::transcripts::SessionId;
use crate::watch::Troubles;

const LOCKS: &str = "thread-writer-locks";

const LOCK: &str = "lock";

const NAMES: &str = "session_index.jsonl";

const NAMES_BYTES_MAX: u64 = 1024 * 1024;

const NAME_LINES_MAX: usize = 16 * 1024;

const WRAPPERS_MAX: usize = 4;

const _: () = assert!(NAMES_BYTES_MAX > 0);
const _: () = assert!(NAME_LINES_MAX > 0);
const _: () = assert!(WRAPPERS_MAX > 0);
const _: () = assert!(WRAPPERS_MAX <= process::ANCESTOR_DEPTH_MAX);

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .filter(|folder| folder.is_absolute())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub session: Said,
    pub name: Option<Name>,
}

pub type Held = HashMap<AgentId, Thread>;

fn thread_in(locks: &Path, files: &[PathBuf]) -> Option<SessionId> {
    let held: BTreeSet<&str> = files
        .iter()
        .filter(|file| file.parent() == Some(locks))
        .filter(|file| file.extension().is_some_and(|extension| extension == LOCK))
        .filter_map(|file| file.file_stem()?.to_str())
        .filter(|stem| SessionId::parse(stem).is_some())
        .collect();
    held.first().and_then(|oldest| SessionId::parse(oldest))
}

fn resumed_in(argv: &[String]) -> Option<SessionId> {
    let resumes = Harness::Codex.resumes();
    let mut after = argv
        .iter()
        .skip(1)
        .skip_while(|word| word.as_str() != resumes)
        .skip(1);
    let thread = after.find(|word| trust::is_uuid(word))?;
    SessionId::parse(thread)
}

#[derive(Deserialize)]
struct Indexed {
    id: String,
    thread_name: String,
}

fn names_in(index: &[u8]) -> HashMap<String, Said> {
    let mut names = HashMap::new();
    let lines = index.split(|byte| *byte == b'\n');
    for line in lines.take(NAME_LINES_MAX) {
        let Ok(Indexed { id, thread_name }) = serde_json::from_slice::<Indexed>(line) else {
            continue;
        };
        match Said::new(thread_name.trim().to_owned()) {
            Some(name) => names.insert(id, name),
            None => names.remove(&id),
        };
    }
    assert!(names.len() <= NAME_LINES_MAX);
    names
}

fn index_end(codex: &Path) -> io::Result<Vec<u8>> {
    let mut file = match File::open(codex.join(NAMES)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let bytes = file.metadata()?.len();
    let from = bytes.saturating_sub(NAMES_BYTES_MAX);
    file.seek(SeekFrom::Start(from))?;
    let mut end = Vec::new();
    file.take(NAMES_BYTES_MAX).read_to_end(&mut end)?;
    if from > 0 {
        let whole = end.iter().position(|byte| *byte == b'\n');
        end.drain(..whole.map_or(end.len(), |newline| newline + 1));
    }
    assert!(u64::try_from(end.len()).is_ok_and(|read| read <= NAMES_BYTES_MAX));
    Ok(end)
}

fn wraps(wrapper: AgentPid, pid: u32) -> bool {
    let mut at = pid;
    for _ in 0..WRAPPERS_MAX {
        let Some(parent) = process::parent(at) else {
            return false;
        };
        if parent == wrapper.get() {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        at = parent;
    }
    false
}

pub struct Threads {
    locks: Option<PathBuf>,
    codex: Option<PathBuf>,
    troubles: Troubles,
}

impl Threads {
    #[must_use]
    pub fn new(codex: Option<PathBuf>) -> Threads {
        Threads {
            locks: None,
            codex,
            troubles: Troubles::default(),
        }
    }

    fn locks(&mut self) -> Option<PathBuf> {
        if self.locks.is_none() {
            self.locks = self.codex.as_ref()?.join(LOCKS).canonicalize().ok();
        }
        self.locks.clone()
    }

    fn files(&mut self, pid: u32) -> Vec<PathBuf> {
        match process::open_files(pid) {
            Ok(files) => files,
            Err(process::Unread::Gone) => Vec::new(),
            Err(process::Unread::Failed(why)) => {
                self.troubles.say(format!(
                    "the files Codex {pid} holds open are unread: {why}"
                ));
                Vec::new()
            }
        }
    }

    fn wrapped(&mut self, locks: &Path, agent: AgentPid) -> Option<SessionId> {
        let program = Harness::Codex.program();
        let mut inside: Vec<u32> = process::mine(&[((), program)])
            .found
            .into_iter()
            .map(|found| found.id.pid)
            .filter(|pid| *pid != agent.get() && wraps(agent, *pid))
            .collect();
        inside.sort_unstable();
        inside.truncate(SOURCE_AGENTS_MAX);
        inside
            .into_iter()
            .find_map(|pid| thread_in(locks, &self.files(pid)))
    }

    pub fn held_by(&mut self, agents: &[Agent]) -> Held {
        let mut held = Held::new();
        let codex: Vec<&Agent> = agents
            .iter()
            .filter(|agent| agent.harness == Harness::Codex)
            .take(SOURCE_AGENTS_MAX)
            .collect();
        if codex.is_empty() {
            self.troubles.done();
            return held;
        }
        let Some(locks) = self.locks() else {
            self.troubles.done();
            return held;
        };
        let mut sessions = Vec::with_capacity(codex.len());
        for agent in codex {
            let pid = agent.pid.get();
            let Some(start) = process::start(pid) else {
                continue;
            };
            let Some(runs) = process::meta(pid) else {
                continue;
            };
            let wrapper = runs.name != Harness::Codex.program();
            let direct = thread_in(&locks, &self.files(pid));
            let written = match direct {
                None if wrapper => self.wrapped(&locks, agent.pid),
                direct => direct,
            };
            let argv = runs.argv.as_ref().map(ProcessArgv::words);
            let Some(session) = written.or_else(|| resumed_in(argv?)) else {
                continue;
            };
            sessions.push((AgentId::new(agent.pid, start), session));
        }
        let names = if sessions.is_empty() {
            HashMap::new()
        } else {
            self.names()
        };
        for (agent, session) in sessions {
            let name = names.get(session.as_str()).map(|text| Name {
                text: text.clone(),
                source: Some(Source::Auto),
            });
            let Some(session) = Said::new(session.as_str().to_owned()) else {
                continue;
            };
            held.insert(agent, Thread { session, name });
        }
        self.troubles.done();
        assert!(held.len() <= SOURCE_AGENTS_MAX);
        held
    }

    fn names(&mut self) -> HashMap<String, Said> {
        let Some(codex) = self.codex.clone() else {
            return HashMap::new();
        };
        match index_end(&codex) {
            Ok(index) => names_in(&index),
            Err(error) => {
                let index = codex.join(NAMES);
                self.troubles
                    .say(format!("{} is unread: {error}", index.display()));
                HashMap::new()
            }
        }
    }
}

pub fn follow(
    core: SyncSender<Command>,
    codex: Option<PathBuf>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new()
        .name("codex".to_owned())
        .spawn(move || {
            let mut threads = Threads::new(codex);
            let mut told = Held::new();
            loop {
                let Some(agents) = core::ask(&core, Command::AllAgents) else {
                    return;
                };
                let now = threads.held_by(&agents);
                if now != told {
                    if core.send(Command::CodexThreads(now.clone())).is_err() {
                        return;
                    }
                    told = now;
                }
                thread::sleep(sweep);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use lune_domain::agent::OscReads;
    use lune_protocol::said::Said;

    use super::*;
    use crate::Directory;
    use crate::statuses::ActiveStatus;
    use crate::statuses::tests::decided;

    const OLDER: &str = "01a222f4-cde9-75d3-9267-061cca269c80";

    const NEWER: &str = "01a22300-2f28-7b50-b2bb-9f2fa007c110";

    fn lock(locks: &Path, thread: &str) -> PathBuf {
        locks.join(format!("{thread}.lock"))
    }

    #[test]
    fn a_process_s_thread_is_the_oldest_writer_lock_it_holds_in_codex_s_folder() {
        let locks = Path::new("/home/u/.codex/thread-writer-locks");
        let elsewhere = Path::new("/home/u/other/thread-writer-locks");
        let held =
            |files: &[PathBuf]| thread_in(locks, files).map(|session| session.as_str().to_owned());
        assert_eq!(held(&[]), None);
        assert_eq!(held(&[lock(locks, OLDER)]), Some(OLDER.to_owned()));
        assert_eq!(
            held(&[lock(locks, NEWER), lock(locks, OLDER)]),
            Some(OLDER.to_owned()),
            "a thread Codex spawned later is a subagent's"
        );
        assert_eq!(held(&[lock(elsewhere, OLDER)]), None);
        assert_eq!(held(&[locks.join(".coordination.lock")]), None);
        assert_eq!(held(&[locks.join(format!("{OLDER}.jsonl"))]), None);
        assert_eq!(held(&[locks.join("not a thread.lock")]), None);
        assert_eq!(
            held(&[PathBuf::from("/dev/pts/8"), lock(locks, NEWER)]),
            Some(NEWER.to_owned())
        );
    }

    #[test]
    fn a_codex_started_to_resume_a_thread_runs_it_before_it_writes_to_it() {
        let resumed = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
            resumed_in(&argv).map(|session| session.as_str().to_owned())
        };
        assert_eq!(resumed(&["codex", "resume", OLDER]), Some(OLDER.to_owned()));
        assert_eq!(
            resumed(&[
                "node",
                "/lib/codex.js",
                "-m",
                "gpt",
                "resume",
                "--all",
                OLDER,
                "go on"
            ]),
            Some(OLDER.to_owned())
        );
        assert_eq!(resumed(&["codex", "resume", "lighthouse"]), None);
        assert_eq!(resumed(&["codex", "resume", "--last"]), None);
        assert_eq!(resumed(&["codex", "fork", OLDER]), None);
        assert_eq!(resumed(&["codex", OLDER]), None);
        assert_eq!(
            resumed(&["resume", OLDER]),
            None,
            "the program is no subcommand"
        );
        assert_eq!(resumed(&["codex"]), None);
        assert_eq!(resumed(&[]), None);
    }

    #[test]
    fn a_thread_s_name_is_the_last_one_the_index_gives_it() {
        let index = format!(
            "{{\"id\":\"{OLDER}\",\"thread_name\":\"Reply with hello\",\"updated_at\":\"2026-11-28T16:20:20Z\"}}\n\
             not json\n\
             {{\"id\":\"{NEWER}\",\"thread_name\":\"Guardian review\"}}\n\
             {{\"id\":\"{OLDER}\",\"thread_name\":\"  lighthouse  \"}}\n\
             {{\"id\":\"{NEWER}\",\"thread_name\":\"   \"}}\n\
             {{\"id\":\"{OLDER}\"}}\n"
        );
        let names = names_in(index.as_bytes());
        assert_eq!(names.get(OLDER).map(Said::as_str), Some("lighthouse"));
        assert_eq!(names.get(NEWER), None, "a blank name unnames the thread");
        assert_eq!(names_in(b""), HashMap::new());
    }

    #[test]
    fn the_index_is_read_from_its_end_and_from_a_whole_line() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(index_end(home.path()).unwrap(), b"");
        let line = format!("{{\"id\":\"{OLDER}\",\"thread_name\":\"lighthouse\"}}\n");
        fs::write(home.path().join(NAMES), &line).unwrap();
        assert_eq!(index_end(home.path()).unwrap(), line.as_bytes());

        let most = usize::try_from(NAMES_BYTES_MAX).unwrap();
        let filler = format!(
            "{{\"id\":\"{NEWER}\",\"thread_name\":\"{}\"}}\n",
            "x".repeat(900)
        );
        let mut long = filler.repeat(most / filler.len() + 2);
        long.push_str(&line);
        fs::write(home.path().join(NAMES), &long).unwrap();
        let end = index_end(home.path()).unwrap();
        assert!(end.len() <= most);
        assert!(
            end.starts_with(b"{\"id\""),
            "the torn first line is dropped"
        );
        assert!(end.ends_with(line.as_bytes()));
        let names = names_in(&end);
        assert_eq!(names.get(OLDER).map(Said::as_str), Some("lighthouse"));
    }

    fn agent(harness: Harness) -> Agent {
        Agent {
            pid: AgentPid::new(std::process::id()).unwrap(),
            harness,
            session_id: None,
            cwd: Directory::parse("/w".to_owned()).unwrap(),
            status: decided(
                OscReads::of(Harness::Codex).declared(),
                ActiveStatus::Working,
                None,
            ),
            started_at: None,
            name: None,
            entrypoint: None,
            terminal: None,
        }
    }

    #[test]
    fn a_codex_that_holds_a_writer_lock_runs_that_thread_under_its_last_name() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().canonicalize().unwrap();
        let mut threads = Threads::new(Some(codex.clone()));
        let me = agent(Harness::Codex);
        assert_eq!(
            threads.held_by(std::slice::from_ref(&me)),
            Held::new(),
            "Codex has made no lock folder yet"
        );

        let locks = codex.join(LOCKS);
        fs::create_dir(&locks).unwrap();
        assert_eq!(threads.held_by(std::slice::from_ref(&me)), Held::new());

        let held = File::create(lock(&locks, OLDER)).unwrap();
        let id = AgentId::new(me.pid, process::start(me.pid.get()).unwrap());
        let unnamed = Thread {
            session: Said::new(OLDER.to_owned()).unwrap(),
            name: None,
        };
        let now = threads.held_by(std::slice::from_ref(&me));
        assert_eq!(now, Held::from([(id.clone(), unnamed.clone())]));

        let index = format!("{{\"id\":\"{OLDER}\",\"thread_name\":\"lighthouse\"}}\n");
        fs::write(codex.join(NAMES), index).unwrap();
        let named = threads.held_by(std::slice::from_ref(&me));
        let name = named[&id].name.as_ref().unwrap();
        assert_eq!(name.auto(), Some("lighthouse"));
        assert_eq!(name.given(), None, "Codex names its threads itself too");

        assert_eq!(
            threads.held_by(&[agent(Harness::ClaudeCode)]),
            Held::new(),
            "only Codex's agents hold Codex's threads"
        );
        drop(held);
        assert_eq!(threads.held_by(std::slice::from_ref(&me)), Held::new());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_codex_behind_a_wrapper_runs_the_thread_the_codex_it_started_holds() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::{Command, Stdio};

        let home = tempfile::tempdir().unwrap();
        let codex = home.path().canonicalize().unwrap();
        let locks = codex.join(LOCKS);
        fs::create_dir(&locks).unwrap();
        let held = lock(&locks, OLDER);
        let inner = codex.join("codex");
        let holds = format!("#!/bin/sh\nexec 9>'{}'\nread -r _\n", held.display());
        fs::write(&inner, holds).unwrap();
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();
        let starts = format!("'{}'; :", inner.display());
        let mut shell = Command::new("sh")
            .args(["-c", &starts])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let wrapped = Agent {
            pid: AgentPid::new(shell.id()).unwrap(),
            ..agent(Harness::Codex)
        };
        let id = AgentId::new(wrapped.pid, process::start(shell.id()).unwrap());
        let mut threads = Threads::new(Some(codex));
        let found = lune_waits::eventually("the wrapped codex holds its lock", || {
            let now = threads.held_by(std::slice::from_ref(&wrapped));
            now.get(&id)
                .map(|thread| thread.session.as_str().to_owned())
        });
        assert_eq!(found, OLDER);
        drop(shell.stdin.take());
        shell.wait().unwrap();
    }
}
