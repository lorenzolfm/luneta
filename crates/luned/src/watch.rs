use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

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
    thread::Builder::new().name(name.to_owned()).spawn(move || {
        loop {
            if watcher.is_none() {
                watcher = watching(&dir, mode, wanted, &changed, &mut troubles);
                troubles.done();
            }
            match heard.recv_timeout(sweep) {
                Ok(()) => settle(&heard),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {},
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
            troubles.say(format!("{} is read on the sweep alone: {error}", dir.display()));
            None
        },
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
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

struct Ends {
    changed: SyncSender<()>,
    hearing: HashMap<u32, JoinHandle<()>>,
    troubles: Troubles,
}

impl Ends {
    fn new(changed: SyncSender<()>) -> Self {
        Ends { changed, hearing: HashMap::new(), troubles: Troubles::default() }
    }

    fn hear(&mut self, pids: &[u32]) {
        self.hearing.retain(|pid, thread| !thread.is_finished() || pids.contains(pid));
        for &pid in pids {
            let Entry::Vacant(place) = self.hearing.entry(pid) else {
                continue;
            };
            let changed = self.changed.clone();
            let hearing = move || match process::ends(pid) {
                Ok(()) => wake(&changed),
                Err(error) => log::say(&unheard(pid, &error)),
            };
            match thread::Builder::new().name("ends".to_owned()).spawn(hearing) {
                Ok(thread) => {
                    place.insert(thread);
                },
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
            Ok(()) => {},
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

#[derive(Default)]
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
        eventually("its hearing ends", || ends.hearing[&pid].is_finished().then_some(()));
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
        watch("test", dir.path().to_owned(), RecursiveMode::NonRecursive, any, OUTLASTING, read, tell)
            .unwrap();
        assert_eq!(lune_waits::heard("the first read", &heard), 0);
        assert_eq!(lune_waits::heard("the read before the sweep", &heard), 1);
    }
}
