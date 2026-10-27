//! A moment for each pane of each session, the same in every luneta on the
//! host, and kept for the next.
//!
//! A pane goes by its session, which its socket tells: when the socket was
//! made and its inode, both of which a rename keeps and a session made again
//! under its name does not; and by its id, which zellij gives no other pane of
//! the session. Its moment is a file in the ledger's folder, named for the
//! three. A file is written whole, under a name of its writer's own, before
//! it takes its pane's, so that no luneta reads one halfway written, and two
//! that write one pane at once each write a file of their own: the last to
//! take the pane's name holds it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::io::ErrorKind;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A session, as a ledger keeps it: when its socket was made, and its inode.
/// Two sockets can be made in one tick of the clock that dates them, and a
/// socket made later can take the inode of one gone, but not both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Session {
    pub started: SystemTime,
    pub socket: NonZeroU64,
}

/// A pane, as a ledger keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub session: Session,
    pub pane: u32,
}

impl Key {
    /// The name of its file: when its session started, in nanoseconds since
    /// the epoch, its socket's inode, and its id. None for a session that
    /// started before then.
    fn name(self) -> Option<String> {
        let Session { started, socket } = self.session;
        Some(format!("{}-{socket}-{}", nanos(started)?, self.pane))
    }

    /// The pane whose file `name` is, if it is one's: spelled as `name`
    /// spells it. A file still being written is not, nor one a person spelled
    /// another way, as `+10-3-1` or `010-3-1`, which no luneta writes or
    /// takes away.
    fn read(name: &str) -> Option<Self> {
        let mut parts = name.splitn(3, '-');
        let started = moment(parts.next()?.parse().ok()?);
        let socket = parts.next()?.parse().ok()?;
        let key = Key { session: Session { started, socket }, pane: parts.next()?.parse().ok()? };
        (key.name()? == name).then_some(key)
    }
}

/// `at` in nanoseconds since the epoch, for one from then until 2554.
fn nanos(at: SystemTime) -> Option<u64> {
    u64::try_from(at.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos()).ok()
}

fn moment(nanos: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos)
}

/// How the name a file takes while it is written ends, before it takes its
/// pane's.
const UNFINISHED: &str = "new";

/// Who the moments of a ledger are kept for.
enum Shared {
    /// Every luneta, through this folder.
    Through(PathBuf),
    /// This luneta alone, as their folder failed them, for this reason.
    Lost(String),
    /// This luneta alone, as a test's are.
    Alone,
}

pub struct Ledger {
    moments: BTreeMap<Key, SystemTime>,
    shared: Shared,
    /// What this ledger's files are called after their pane while it writes
    /// them, and no other ledger's are.
    unfinished: String,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger { moments: BTreeMap::new(), shared: Shared::Alone, unfinished: unfinished() }
    }
}

/// A name no other ledger's files take while they are written: each
/// `RandomState` is seeded from the system's randomness. A pane's name does
/// not read from it, so no luneta takes it for a moment.
fn unfinished() -> String {
    format!("{:016x}.{UNFINISHED}", RandomState::new().hash_one(()))
}

impl Ledger {
    /// The moments every luneta keeps in `dir`, as they stand.
    pub fn within(dir: PathBuf) -> Self {
        let shared = match fs::create_dir_all(&dir) {
            Ok(()) => Shared::Through(dir),
            Err(error) => Shared::Lost(format!("{}: {error}", dir.display())),
        };
        let mut ledger = Ledger { shared, ..Ledger::default() };
        ledger.reload();
        ledger
    }

    /// One whose moments are this luneta's alone, for `why`.
    pub fn unshared(why: String) -> Self {
        Ledger { shared: Shared::Lost(why), ..Ledger::default() }
    }

    /// The folder they are shared through, while they are.
    pub fn dir(&self) -> Option<&Path> {
        match &self.shared {
            Shared::Through(dir) => Some(dir),
            Shared::Lost(_) | Shared::Alone => None,
        }
    }

    /// Why the moments are this luneta's alone, where a folder failed them.
    pub fn lost(&self) -> Option<&str> {
        match &self.shared {
            Shared::Lost(why) => Some(why),
            Shared::Through(_) | Shared::Alone => None,
        }
    }

    pub fn get(&self, key: Key) -> Option<SystemTime> {
        self.moments.get(&key).copied()
    }

    /// The sessions it keeps a moment of.
    pub fn sessions(&self) -> impl Iterator<Item = Session> + '_ {
        self.moments.keys().map(|key| key.session)
    }

    /// Keeps `moment` for `key`, for every luneta, or with none takes the
    /// pane's away. A folder that fails a write, or has no name for it, is
    /// given up, and the moments are this luneta's from then on, rather than
    /// undone by the next `reload`.
    pub fn set(&mut self, key: Key, moment: Option<SystemTime>) {
        if let Shared::Through(dir) = &self.shared
            && let Err(why) = write(dir, &self.unfinished, key, moment)
        {
            self.shared = Shared::Lost(why);
        }
        match moment {
            Some(moment) => self.moments.insert(key, moment),
            None => self.moments.remove(&key),
        };
    }

    /// Takes up what the other lunetas kept since, and says whether that
    /// changed anything. A folder that cannot be read leaves the moments as
    /// they were, and a file that does not read as one is none.
    pub fn reload(&mut self) -> bool {
        let Shared::Through(dir) = &self.shared else {
            return false;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let moments: BTreeMap<Key, SystemTime> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let key = Key::read(entry.file_name().to_str()?)?;
                let nanos = fs::read_to_string(entry.path()).ok()?.trim_end().parse().ok()?;
                Some((key, moment(nanos)))
            })
            .collect();
        if moments == self.moments {
            return false;
        }
        self.moments = moments;
        true
    }

    /// Takes away the moments of every session of `gone`, for every luneta,
    /// and what was left of them in the folder: a file that does not read as
    /// a moment, and one a luneta was writing as it ended.
    pub fn forget(&mut self, gone: &BTreeSet<Session>) {
        let keys: Vec<Key> =
            self.moments.keys().filter(|key| gone.contains(&key.session)).copied().collect();
        for key in keys {
            self.set(key, None);
        }
        let Shared::Through(dir) = &self.shared else {
            return;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let pane = name.split_once('.').map_or(name, |(pane, _)| pane);
            if Key::read(pane).is_some_and(|key| gone.contains(&key.session)) {
                // Another luneta may have taken it away first, and what is
                // left is no moment either way.
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Keeps `moment` for `key` in `dir`, with `unfinished` ending the name its
/// file takes while it is written, or with none takes the pane's away. Says
/// why it could not: the folder failed, or has no name for a moment, or a
/// session's start, from before 1970 or after 2554.
fn write(dir: &Path, unfinished: &str, key: Key, moment: Option<SystemTime>) -> Result<(), String> {
    let unnamed = || format!("{}: a moment from before 1970 or after 2554", dir.display());
    let name = key.name().ok_or_else(unnamed)?;
    let file = dir.join(&name);
    let kept = match moment {
        Some(moment) => {
            let nanos = nanos(moment).ok_or_else(unnamed)?;
            let unfinished = dir.join(format!("{name}.{unfinished}"));
            fs::write(&unfinished, format!("{nanos}\n"))
                .and_then(|()| fs::rename(&unfinished, &file))
        },
        // Another luneta may have taken it away first.
        None => fs::remove_file(&file).or_else(|error| match error.kind() {
            ErrorKind::NotFound => Ok(()),
            _ => Err(error),
        }),
    };
    kept.map_err(|error| format!("{}: {error}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn session(started: u64, socket: u64) -> Session {
        Session { started: at(started), socket: NonZeroU64::new(socket).unwrap() }
    }

    fn key(started: u64, pane: u32) -> Key {
        Key { session: session(started, 1), pane }
    }

    fn keys(ledger: &Ledger) -> Vec<Key> {
        ledger.moments.keys().copied().collect()
    }

    /// Two lunetas: what one keeps or takes away, the other has once it
    /// reloads, and a luneta started later starts with it, to the nanosecond.
    #[test]
    fn a_moment_one_luneta_keeps_is_read_by_every_other_and_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("seen");
        let mut here = Ledger::within(dir.clone());
        let mut there = Ledger::within(dir.clone());
        let precise = at(100) + Duration::from_nanos(123_456_789);

        here.set(key(10, 3), Some(precise));
        here.set(key(20, 0), Some(at(7)));
        assert!(there.reload());
        assert_eq!((there.get(key(10, 3)), there.get(key(20, 0))), (Some(precise), Some(at(7))));
        assert_eq!(Ledger::within(dir.clone()).get(key(10, 3)), Some(precise));

        there.set(key(10, 3), None);
        // Taken away twice, as two lunetas may before they hear of each other.
        here.set(key(10, 3), None);
        assert!(!here.reload());
        assert_eq!((here.get(key(10, 3)), here.get(key(20, 0))), (None, Some(at(7))));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    /// A file that does not read as a moment, or whose name is no pane's as
    /// a luneta spells it, as one still being written, is none.
    #[test]
    fn only_a_pane_s_file_that_reads_as_a_moment_is_one() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        ledger.set(key(10, 1), Some(at(5)));
        let name = |pane| key(10, pane).name().unwrap();
        let other = |name: String| fs::write(scratch.path().join(name), "5").unwrap();
        fs::write(scratch.path().join(name(2)), "\n").unwrap();
        other(format!("{}.{}", name(3), unfinished()));
        other("4".to_owned());
        other(format!("+{}", name(5)));
        other(format!("0{}", name(6)));
        other(format!("{}-7", nanos(at(10)).unwrap()));
        ledger.reload();
        assert_eq!(keys(&ledger), [key(10, 1)]);
    }

    /// Two lunetas that write one pane's moment at the same time, as two do
    /// that both see you there, both go on sharing it.
    #[test]
    fn two_lunetas_writing_one_pane_at_once_both_go_on_sharing() {
        let scratch = tempfile::tempdir().unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let (dir, start) = (scratch.path().to_owned(), start.clone());
                std::thread::spawn(move || {
                    let mut ledger = Ledger::within(dir);
                    start.wait();
                    for nth in 0..2000 {
                        ledger.set(key(10, 3), Some(at(nth)));
                    }
                    ledger
                })
            })
            .collect();
        for writer in writers {
            let mut ledger = writer.join().unwrap();
            assert_eq!(ledger.lost(), None);
            ledger.set(key(10, 4), Some(at(1)));
            assert_eq!(Ledger::within(scratch.path().to_owned()).get(key(10, 4)), Some(at(1)));
        }
    }

    /// A session forgotten takes its panes' moments with it, for every
    /// luneta, and what a luneta left of them as it ended, and leaves another
    /// that started at the same moment its own.
    #[test]
    fn a_session_forgotten_takes_its_moments_and_leaves_the_rest() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = Ledger::within(scratch.path().to_owned());
        let twin = Key { session: session(10, 2), pane: 1 };
        here.set(key(10, 1), Some(at(5)));
        here.set(twin, Some(at(5)));
        here.set(key(20, 1), Some(at(6)));
        let left = [key(10, 2), key(20, 2)]
            .map(|key| scratch.path().join(format!("{}.{}", key.name().unwrap(), unfinished())));
        for file in &left {
            fs::write(file, "5").unwrap();
        }
        here.forget(&BTreeSet::from([session(10, 1)]));
        for ledger in [&here, &Ledger::within(scratch.path().to_owned())] {
            assert_eq!(keys(ledger), [twin, key(20, 1)]);
        }
        assert_eq!(left.map(|file| file.exists()), [false, true]);
    }

    /// A folder luneta cannot keep the moments in leaves them to this luneta,
    /// and says why.
    #[test]
    fn a_folder_that_fails_keeps_the_moments_here() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("not a folder");
        fs::write(&file, "").unwrap();
        let mut ledger = Ledger::within(file.join("seen"));
        assert!(ledger.lost().is_some_and(|lost| lost.contains("not a folder")));
        ledger.set(key(10, 1), Some(at(5)));
        assert_eq!(ledger.get(key(10, 1)), Some(at(5)));
    }

    /// A moment the folder has no name for, from before 1970, leaves the
    /// moments to this luneta, rather than take the pane's away from every
    /// other.
    #[test]
    fn a_moment_the_folder_cannot_spell_keeps_the_moments_here() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        ledger.set(key(10, 1), Some(at(5)));
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
        ledger.set(key(10, 1), Some(before));
        assert!(ledger.lost().is_some_and(|lost| lost.contains("before 1970")));
        assert_eq!(ledger.get(key(10, 1)), Some(before));
        assert_eq!(Ledger::within(scratch.path().to_owned()).get(key(10, 1)), Some(at(5)));
    }
}
