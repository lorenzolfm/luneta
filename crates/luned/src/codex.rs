use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, ErrorKind, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, SystemTime};

use lune_domain::agent::Harness;
use lune_protocol::program_resume::ProcessArgv;
use lune_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{Agent, Name, Source};
use crate::core::{self, Command};
use crate::process::{self, AgentId};
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

const WRAPPED_MAX: usize = 16;

const _: () = assert!(NAMES_BYTES_MAX > 0);
const _: () = assert!(NAME_LINES_MAX > 0);
const _: () = assert!(WRAPPERS_MAX > 0);
const _: () = assert!(WRAPPERS_MAX <= process::ANCESTOR_DEPTH_MAX);
const _: () = assert!(WRAPPED_MAX > 0);
const _: () = assert!(WRAPPED_MAX <= SOURCE_AGENTS_MAX);

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
    assert!(
        locks.is_absolute(),
        "open files are named from the root, and so is the folder they are sought in"
    );
    let held = files
        .iter()
        .filter(|file| file.parent() == Some(locks))
        .filter(|file| file.extension().is_some_and(|extension| extension == LOCK))
        .filter_map(|file| file.file_stem()?.to_str())
        .filter(|stem| trust::is_uuid(stem));
    SessionId::parse(held.min()?)
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
    let newest_first = index.rsplit(|byte| *byte == b'\n');
    let newest: Vec<&[u8]> = newest_first.take(NAME_LINES_MAX).collect();
    for line in newest.into_iter().rev() {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    inode: u64,
    bytes: u64,
    modified: SystemTime,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Index {
    stamp: Option<Stamp>,
    names: HashMap<String, Said>,
}

fn index_since(codex: &Path, known: Option<Stamp>) -> io::Result<Option<Index>> {
    let mut file = match File::open(codex.join(NAMES)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(known.map(|_| Index::default()));
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    let stamp = Stamp {
        inode: metadata.ino(),
        bytes: metadata.len(),
        modified: metadata.modified()?,
    };
    if known == Some(stamp) {
        return Ok(None);
    }
    let end = index_end(&mut file, stamp.bytes)?;
    Ok(Some(Index {
        stamp: Some(stamp),
        names: names_in(&end),
    }))
}

fn index_end(file: &mut File, bytes: u64) -> io::Result<Vec<u8>> {
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

fn locks_in(codex: &Path) -> io::Result<Option<PathBuf>> {
    match codex.join(LOCKS).canonicalize() {
        Ok(locks) => Ok(Some(locks)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn wrappers(pid: u32) -> Vec<u32> {
    let mut wrappers = Vec::with_capacity(WRAPPERS_MAX);
    let mut at = pid;
    for _ in 0..WRAPPERS_MAX {
        match process::parent(at) {
            Some(parent) if parent > 1 => {
                wrappers.push(parent);
                at = parent;
            }
            Some(_) | None => break,
        }
    }
    assert!(wrappers.len() <= WRAPPERS_MAX);
    wrappers
}

type Inside = HashMap<u32, Vec<u32>>;

fn inside_each() -> Inside {
    let program = Harness::Codex.program();
    let process::Mine { found, unreadable } = process::mine(&[((), program)]);
    let found = found.iter().map(|found| found.id.pid);
    let unreadable = unreadable.iter().map(|unreadable| unreadable.id.pid);
    let mut running: Vec<u32> = found.chain(unreadable).collect();
    running.sort_unstable();
    running.truncate(SOURCE_AGENTS_MAX);
    let mut inside = Inside::new();
    for pid in running {
        for wrapper in wrappers(pid) {
            let wrapped = inside.entry(wrapper).or_default();
            if wrapped.len() < WRAPPED_MAX {
                wrapped.push(pid);
            }
        }
    }
    assert!(inside.len() <= SOURCE_AGENTS_MAX * WRAPPERS_MAX);
    inside
}

pub struct Threads {
    locks: Option<PathBuf>,
    codex: Option<PathBuf>,
    index: Index,
    troubles: Troubles,
}

impl Threads {
    #[must_use]
    pub fn new(codex: Option<PathBuf>) -> Threads {
        Threads {
            locks: None,
            codex,
            index: Index::default(),
            troubles: Troubles::default(),
        }
    }

    fn locks(&mut self) -> Option<PathBuf> {
        if self.locks.is_none() {
            let codex = self.codex.as_deref()?;
            match locks_in(codex) {
                Ok(locks) => self.locks = locks,
                Err(error) => {
                    let locks = codex.join(LOCKS);
                    self.troubles
                        .say(format!("{} is unread: {error}", locks.display()));
                }
            }
        }
        self.locks.clone()
    }

    fn files(&mut self, pid: u32) -> Vec<PathBuf> {
        match process::open_files(pid, process::OPEN_FILES_MAX) {
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

    fn wrapped(&mut self, locks: &Path, inside: &[u32]) -> Option<SessionId> {
        assert!(inside.len() <= WRAPPED_MAX);
        inside
            .iter()
            .find_map(|pid| thread_in(locks, &self.files(*pid)))
    }

    pub fn held_by(&mut self, agents: &[Agent]) -> Held {
        let codex: Vec<&Agent> = agents
            .iter()
            .filter(|agent| agent.harness == Harness::Codex)
            .take(SOURCE_AGENTS_MAX)
            .collect();
        let locks = if codex.is_empty() { None } else { self.locks() };
        let held = match locks {
            Some(locks) => self.held_in(&locks, &codex),
            None => Held::new(),
        };
        self.troubles.done();
        assert!(held.len() <= codex.len());
        assert!(held.len() <= SOURCE_AGENTS_MAX);
        held
    }

    fn held_in(&mut self, locks: &Path, codex: &[&Agent]) -> Held {
        let mut inside: Option<Inside> = None;
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
            let direct = thread_in(locks, &self.files(pid));
            let written = match direct {
                None if wrapper => {
                    let inside = inside.get_or_insert_with(inside_each);
                    let inside = inside.get(&pid).map(Vec::as_slice);
                    self.wrapped(locks, inside.unwrap_or_default())
                }
                direct => direct,
            };
            let argv = runs.argv.as_ref().map(ProcessArgv::words);
            let Some(session) = written.or_else(|| resumed_in(argv?)) else {
                continue;
            };
            sessions.push((AgentId::new(agent.pid, start), session));
        }
        let mut held = Held::new();
        if sessions.is_empty() {
            return held;
        }
        let names = self.names();
        for (agent, session) in sessions {
            let name = names.get(session.as_str()).map(|text| Name {
                text: text.clone(),
                source: Some(Source::Auto),
            });
            let session =
                Said::new(session.as_str().to_owned()).expect("a session id is never empty");
            held.insert(agent, Thread { session, name });
        }
        held
    }

    fn names(&mut self) -> &HashMap<String, Said> {
        if let Some(codex) = self.codex.as_deref() {
            match index_since(codex, self.index.stamp) {
                Ok(Some(index)) => self.index = index,
                Ok(None) => {}
                Err(error) => {
                    let index = codex.join(NAMES);
                    self.troubles
                        .say(format!("{} is unread: {error}", index.display()));
                    self.index = Index::default();
                }
            }
        }
        &self.index.names
    }
}

pub fn follow(
    core: SyncSender<Command>,
    codex: Option<PathBuf>,
    sweep: Duration,
) -> io::Result<()> {
    assert!(
        !sweep.is_zero(),
        "a sweep that takes no time asks the core without a rest"
    );
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
    use crate::process::AgentPid;
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
        for no_uuid in ["abc", "DEAD-BEEF", "01a222f4cde975d39267061cca269c80"] {
            assert!(SessionId::parse(no_uuid).is_some(), "{no_uuid}");
            let named = locks.join(format!("{no_uuid}.lock"));
            assert_eq!(held(&[named]), None, "{no_uuid} orders no threads by age");
        }
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
    fn an_index_of_more_lines_than_are_read_is_read_from_its_newest() {
        let line = |thread: &str, name: &str| {
            format!("{{\"id\":\"{thread}\",\"thread_name\":\"{name}\"}}\n")
        };
        let index = [
            line(NEWER, "only in the lines left unread"),
            line(OLDER, "an older name"),
            "\n".repeat(NAME_LINES_MAX),
            line(OLDER, "lighthouse"),
        ]
        .concat();
        let names = names_in(index.as_bytes());
        assert_eq!(names.get(OLDER).map(Said::as_str), Some("lighthouse"));
        assert_eq!(names.get(NEWER), None);
    }

    #[test]
    fn the_index_is_read_again_only_once_it_has_changed() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path();
        let named = |name: &str| format!("{{\"id\":\"{OLDER}\",\"thread_name\":\"{name}\"}}\n");
        assert_eq!(
            index_since(codex, None).unwrap(),
            None,
            "an index Codex has not written names nothing, as before"
        );

        fs::write(codex.join(NAMES), named("lighthouse")).unwrap();
        let first = index_since(codex, None).unwrap().unwrap();
        assert_eq!(first.names.get(OLDER).map(Said::as_str), Some("lighthouse"));
        assert!(first.stamp.is_some());
        assert_eq!(index_since(codex, first.stamp).unwrap(), None);

        let longer = [named("lighthouse"), named("the keeper")].concat();
        fs::write(codex.join(NAMES), longer).unwrap();
        let second = index_since(codex, first.stamp).unwrap().unwrap();
        assert_eq!(
            second.names.get(OLDER).map(Said::as_str),
            Some("the keeper")
        );
        assert_ne!(second.stamp, first.stamp);

        let aside = codex.join("written aside");
        fs::write(&aside, named("LIGHTHOUSE")).unwrap();
        fs::rename(&aside, codex.join(NAMES)).unwrap();
        let replaced = index_since(codex, second.stamp).unwrap().unwrap();
        assert_eq!(
            replaced.names.get(OLDER).map(Said::as_str),
            Some("LIGHTHOUSE")
        );

        fs::remove_file(codex.join(NAMES)).unwrap();
        assert_eq!(
            index_since(codex, replaced.stamp).unwrap(),
            Some(Index::default()),
            "an index that is gone unnames every thread"
        );
        fs::create_dir(codex.join(NAMES)).unwrap();
        assert!(index_since(codex, None).is_err());
    }

    #[test]
    fn a_process_s_wrappers_are_the_processes_above_it_nearest_first() {
        let above = wrappers(std::process::id());
        assert_eq!(above.first(), Some(&std::os::unix::process::parent_id()));
        assert!(above.len() <= WRAPPERS_MAX);
        assert!(!above.contains(&std::process::id()));
        assert_eq!(wrappers(1), Vec::<u32>::new());
    }

    #[test]
    fn a_lock_folder_codex_has_not_made_is_none_and_one_that_does_not_read_says_why() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().canonicalize().unwrap();
        assert_eq!(locks_in(&codex).unwrap(), None);
        fs::create_dir(codex.join(LOCKS)).unwrap();
        assert_eq!(locks_in(&codex).unwrap(), Some(codex.join(LOCKS)));

        let file = codex.join("a file, not Codex's folder");
        fs::write(&file, "").unwrap();
        let unread = locks_in(&file).unwrap_err();
        assert_ne!(unread.kind(), ErrorKind::NotFound, "{unread}");
    }

    #[test]
    fn the_index_is_read_from_its_end_and_from_a_whole_line() {
        let home = tempfile::tempdir().unwrap();
        let index_end = |codex: &Path| {
            let mut file = File::open(codex.join(NAMES)).unwrap();
            let bytes = file.metadata().unwrap().len();
            index_end(&mut file, bytes).unwrap()
        };
        let line = format!("{{\"id\":\"{OLDER}\",\"thread_name\":\"lighthouse\"}}\n");
        fs::write(home.path().join(NAMES), &line).unwrap();
        assert_eq!(index_end(home.path()), line.as_bytes());

        let most = usize::try_from(NAMES_BYTES_MAX).unwrap();
        let filler = format!(
            "{{\"id\":\"{NEWER}\",\"thread_name\":\"{}\"}}\n",
            "x".repeat(900)
        );
        let mut long = filler.repeat(most / filler.len() + 2);
        long.push_str(&line);
        fs::write(home.path().join(NAMES), &long).unwrap();
        let end = index_end(home.path());
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
