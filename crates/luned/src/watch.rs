use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::watch as tokio_watch;

use crate::{log, process};

pub const SETTLE: Duration = Duration::from_millis(25);

const SETTLE_MAX: Duration = Duration::from_millis(250);

const _: () = assert!(SETTLE.as_millis() < SETTLE_MAX.as_millis());

pub fn watch<T: 'static>(
    name: &str,
    dir: PathBuf,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    sweep: Duration,
    read: impl FnMut() -> T + Send + 'static,
    tell: impl FnMut(T) -> bool + Send + 'static,
) -> io::Result<()> {
    watch_ending(name, dir, mode, wanted, sweep, read, |_| Vec::new(), tell)
}

pub fn watch_ending<T: 'static>(
    name: &str,
    dir: PathBuf,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    sweep: Duration,
    mut read: impl FnMut() -> T + Send + 'static,
    pids: fn(&T) -> Vec<u32>,
    mut tell: impl FnMut(T) -> bool + Send + 'static,
) -> io::Result<()> {
    let (changed, heard) = mpsc::sync_channel(1);
    let mut ends = Ends::new(changed.clone());
    let mut read = move || {
        let found = read();
        ends.hear(&pids(&found));
        found
    };
    let mut troubles = Troubles::default();
    let mut watcher = watching(&dir, mode, wanted, &changed, &mut troubles);
    troubles.done();
    if !tell(read()) {
        return Ok(());
    }
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            loop {
                if watcher.is_none() {
                    watcher = watching(&dir, mode, wanted, &changed, &mut troubles);
                    troubles.done();
                }
                match heard.recv_timeout(sweep) {
                    Ok(()) => settle(&heard),
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
                }
                if !tell(read()) {
                    return;
                }
            }
        })?;
    Ok(())
}

pub fn watching(
    dir: &Path,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    changed: &SyncSender<()>,
    troubles: &mut Troubles,
) -> Option<RecommendedWatcher> {
    match watcher(dir, mode, wanted, changed.clone()) {
        Ok(watcher) => Some(watcher),
        Err(error) if missing(&error) => None,
        Err(error) => {
            troubles.say(format!(
                "{} is read on the sweep alone: {error}",
                dir.display()
            ));
            None
        }
    }
}

fn watcher(
    dir: &Path,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    changed: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event.is_ok_and(|event| !worth_a_read(&event, wanted)) {
            return;
        }
        wake(&changed);
    })?;
    watcher.watch(dir, mode)?;
    Ok(watcher)
}

fn wake(changed: &SyncSender<()>) {
    match changed.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
    }
}

struct Ends {
    changed: SyncSender<()>,
    hearing: HashMap<u32, JoinHandle<()>>,
    troubles: Troubles,
}

impl Ends {
    fn new(changed: SyncSender<()>) -> Self {
        Ends {
            changed,
            hearing: HashMap::new(),
            troubles: Troubles::default(),
        }
    }

    fn hear(&mut self, pids: &[u32]) {
        self.hearing
            .retain(|pid, thread| !thread.is_finished() || pids.contains(pid));
        for &pid in pids {
            let Entry::Vacant(place) = self.hearing.entry(pid) else {
                continue;
            };
            let changed = self.changed.clone();
            let hearing = move || match process::ends(pid) {
                Ok(()) => wake(&changed),
                Err(error) => log::say(&unheard(pid, &error)),
            };
            match thread::Builder::new()
                .name("ends".to_owned())
                .spawn(hearing)
            {
                Ok(thread) => {
                    place.insert(thread);
                }
                Err(error) => self.troubles.say(unheard(pid, &error)),
            }
        }
        self.troubles.done();
    }
}

fn unheard(pid: u32, error: &io::Error) -> String {
    format!("the end of process {pid} is left to the sweep: {error}")
}

pub fn worth_a_read(event: &Event, wanted: impl Fn(&Path) -> bool) -> bool {
    if event.kind.is_access() {
        return false;
    }
    event.paths.is_empty() || event.paths.iter().any(|path| wanted(path))
}

pub fn missing(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::PathNotFound => true,
        notify::ErrorKind::Io(error) => error.kind() == ErrorKind::NotFound,
        _ => false,
    }
}

pub fn settle(heard: &Receiver<()>) {
    let deadline = Instant::now() + SETTLE_MAX;
    while Instant::now() < deadline {
        match heard.recv_timeout(SETTLE) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub fn listed(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    entries.map(|entry| Ok(entry?.path())).collect()
}

#[derive(Debug, Default)]
pub struct Troubles {
    said: HashSet<String>,
    now: HashSet<String>,
}

impl Troubles {
    pub fn say(&mut self, what: String) {
        if !self.said.contains(&what) && !self.now.contains(&what) {
            log::say(&what);
        }
        self.now.insert(what);
    }

    pub fn done(&mut self) {
        self.said = mem::take(&mut self.now);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderNames {
    socket: &'static str,
    lock: &'static str,
}

impl FolderNames {
    #[must_use]
    pub const fn try_new(socket: &'static str, lock: &'static str) -> Option<Self> {
        if !is_entry_name(socket)
            || !is_entry_name(lock)
            || same_bytes(socket.as_bytes(), lock.as_bytes())
        {
            return None;
        }
        Some(Self { socket, lock })
    }
}

const fn is_entry_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || same_bytes(bytes, b".") || same_bytes(bytes, b"..") {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'/' || bytes[at] == 0 {
            return false;
        }
        at += 1;
    }
    true
}

const fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut at = 0;
    while at < left.len() {
        if left[at] != right[at] {
            return false;
        }
        at += 1;
    }
    true
}

pub struct FolderWatch {
    _watcher: RecommendedWatcher,
    _sender: tokio_watch::Sender<u64>,
    events: tokio_watch::Receiver<u64>,
}

pub struct FolderMark<'a>(&'a mut tokio_watch::Receiver<u64>);

impl FolderWatch {
    #[must_use]
    pub fn on(folder: &Path, names: FolderNames) -> Option<FolderWatch> {
        let (changed, events) = tokio_watch::channel(0_u64);
        let notify = changed.clone();
        let handler = move |event: notify::Result<Event>| {
            if folder_event(&event, names) {
                notify.send_modify(|generation| {
                    *generation = generation.wrapping_add(1);
                });
            }
        };
        let config = notify::Config::default().with_follow_symlinks(false);
        let watcher = match RecommendedWatcher::new(handler, config) {
            Ok(mut watcher) => match watcher.watch(folder, RecursiveMode::NonRecursive) {
                Ok(()) => watcher,
                Err(error) => {
                    log::say(&format!(
                        "{} is polled on fallback alone: {error}",
                        folder.display()
                    ));
                    return None;
                }
            },
            Err(error) => {
                log::say(&format!(
                    "{} is polled on fallback alone: {error}",
                    folder.display()
                ));
                return None;
            }
        };
        Some(FolderWatch {
            _watcher: watcher,
            _sender: changed,
            events,
        })
    }

    #[must_use]
    pub fn mark(&mut self) -> FolderMark<'_> {
        self.events.mark_unchanged();
        FolderMark(&mut self.events)
    }
}

impl FolderMark<'_> {
    pub async fn changed(self) {
        let Ok(()) = self.0.changed().await else {
            unreachable!("FolderWatch holds its own sender");
        };
    }
}

fn folder_event(event: &notify::Result<Event>, names: FolderNames) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if event.need_rescan() || event.paths.is_empty() {
        return true;
    }
    let matches_name = |target: &str| {
        event
            .paths
            .iter()
            .any(|path| path.file_name().and_then(OsStr::to_str) == Some(target))
    };
    if matches!(
        event.kind,
        EventKind::Access(AccessKind::Close(AccessMode::Write))
    ) {
        return matches_name(names.lock);
    }
    if event.kind.is_access() {
        return false;
    }
    matches_name(names.socket)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use lune_waits::{OUTLASTING, eventually, never};

    use super::*;

    #[test]
    fn a_process_wakes_the_reader_as_it_ends_and_no_more() {
        let (changed, heard) = mpsc::sync_channel(1);
        let mut ends = Ends::new(changed);
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        ends.hear(&[pid]);
        ends.hear(&[pid]);
        never("an end is heard while it runs", || heard.try_recv().ok());

        child.kill().unwrap();
        child.wait().unwrap();
        let () = lune_waits::heard("it ended", &heard);
        eventually("its hearing ends", || {
            ends.hearing[&pid].is_finished().then_some(())
        });
        ends.hear(&[pid]);
        never("one end is heard twice", || heard.try_recv().ok());

        ends.hear(&[]);
        ends.hear(&[pid]);
        let () = lune_waits::heard("no process by that pid has ended", &heard);
    }

    #[test]
    fn a_change_as_the_first_read_is_told_is_read_before_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        let read = move || fs::read_dir(&path).unwrap().count();
        let made = dir.path().join("made");
        let (found, heard) = mpsc::channel();
        let tell = move |count| {
            if count == 0 {
                fs::write(&made, "").unwrap();
            }
            found.send(count).is_ok()
        };
        let any = |_: &Path| true;
        watch(
            "test",
            dir.path().to_owned(),
            RecursiveMode::NonRecursive,
            any,
            OUTLASTING,
            read,
            tell,
        )
        .unwrap();
        assert_eq!(lune_waits::heard("the first read", &heard), 0);
        assert_eq!(lune_waits::heard("the read before the sweep", &heard), 1);
    }

    #[test]
    fn a_folder_watch_hears_its_socket_and_lock_release_alone() {
        use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind};

        let names = FolderNames::try_new("luned.sock", "luned.lock").unwrap();
        let read = |kind, name: &str| {
            let event = Ok(Event::new(kind).add_path(PathBuf::from("/run/lune").join(name)));
            folder_event(&event, names)
        };
        assert!(read(EventKind::Create(CreateKind::File), "luned.sock"));
        assert!(!read(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            "luned.log"
        ));
        assert!(read(
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            "luned.lock"
        ));
        assert!(!read(
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            "luned.lock"
        ));
        assert!(!read(
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            "luned.log"
        ));
        assert!(!read(EventKind::Create(CreateKind::File), "other"));
    }

    #[test]
    fn folder_names_reject_empty_duplicate_dot_slash_and_nul() {
        assert_eq!(FolderNames::try_new("", "luned.lock"), None);
        assert_eq!(FolderNames::try_new("luned.sock", ""), None);
        assert_eq!(FolderNames::try_new("same", "same"), None);
        assert_eq!(FolderNames::try_new("a/b", "luned.lock"), None);
        assert_eq!(FolderNames::try_new(".", "luned.lock"), None);
        assert_eq!(FolderNames::try_new("luned.sock", ".."), None);
        assert_eq!(FolderNames::try_new("a\0b", "luned.lock"), None);
        assert!(FolderNames::try_new("luned.sock", "luned.lock").is_some());
    }

    #[test]
    fn a_folder_watch_wakes_on_a_marked_socket_creation() {
        let scratch = tempfile::tempdir().unwrap();
        let names = FolderNames::try_new("luned.sock", "luned.lock").unwrap();
        let mut watch = FolderWatch::on(scratch.path(), names).expect("scratch folder is watched");
        let mark = watch.mark();
        fs::write(scratch.path().join("luned.sock"), "").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(lune_waits::awaited(
            "creating the watched socket wakes FolderWatch",
            mark.changed(),
        ));
        assert!(FolderWatch::on(&scratch.path().join("missing"), names).is_none());
    }

    #[test]
    fn troubles_tracks_active_messages_until_they_clear() {
        let mut troubles = Troubles::default();
        troubles.say("bad entry".to_owned());
        troubles.say("bad entry".to_owned());
        assert_eq!(troubles.now.len(), 1);
        assert!(troubles.said.is_empty());
        troubles.done();
        assert!(troubles.said.contains("bad entry"));
        assert!(troubles.now.is_empty());
        troubles.say("bad entry".to_owned());
        troubles.say("another bad entry".to_owned());
        troubles.done();
        assert_eq!(troubles.said.len(), 2);
        troubles.done();
        assert!(troubles.said.is_empty());
    }
}
