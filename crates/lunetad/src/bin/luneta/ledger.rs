use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::mux::own::{OwnId, OwnMade};
use lunetad::SocketId;
use serde::{Deserialize, Serialize};

use crate::state;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Session {
    Zellij { started_ns: u64, socket: NonZeroU64 },
    Own { made_ns: u64, id: OwnId },
}

const OWN_PREFIX: &str = "own-";

impl Session {
    pub fn new(started: SystemTime, socket: NonZeroU64) -> Option<Self> {
        Some(Session::Zellij { started_ns: nanos(started)?, socket })
    }

    pub fn own(made: OwnMade, id: OwnId) -> Self {
        Session::Own { made_ns: made.nanos(), id }
    }

    pub fn of(id: SocketId) -> Option<Self> {
        Session::new(id.started(), NonZeroU64::new(id.socket())?)
    }

    pub fn started(self) -> SystemTime {
        match self {
            Session::Zellij { started_ns, .. } => moment(started_ns),
            Session::Own { made_ns, .. } => moment(made_ns),
        }
    }

    fn name(self) -> String {
        match self {
            Session::Zellij { started_ns, socket } => format!("{started_ns}-{socket}"),
            Session::Own { made_ns, id } => format!("{OWN_PREFIX}{made_ns}-{id}"),
        }
    }

    fn read(name: &str) -> Option<Self> {
        let session = if let Some(own) = name.strip_prefix(OWN_PREFIX) {
            let (made_ns, id) = own.split_once('-')?;
            Session::Own { made_ns: made_ns.parse().ok()?, id: OwnId::new(id.parse().ok()?)? }
        } else {
            let (started_ns, socket) = name.split_once('-')?;
            let (started_ns, socket) = (started_ns.parse().ok()?, socket.parse().ok()?);
            Session::Zellij { started_ns, socket }
        };
        (session.name() == name).then_some(session)
    }
}

impl From<Session> for String {
    fn from(session: Session) -> Self {
        session.name()
    }
}

impl TryFrom<String> for Session {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Session::read(&name).ok_or_else(|| format!("no session is named {name:?}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Key {
    pub session: Session,
    pub pane: u32,
}

impl Key {
    fn name(self) -> String {
        format!("{}-{}", self.session.name(), self.pane)
    }

    fn read(name: &str) -> Option<Self> {
        let (session, pane) = name.rsplit_once('-')?;
        let key = Key { session: Session::read(session)?, pane: pane.parse().ok()? };
        (key.name() == name).then_some(key)
    }

    fn of_file(name: &str) -> Option<Self> {
        Key::read(name.split_once('.').map_or(name, |(pane, _)| pane))
    }
}

impl From<Key> for String {
    fn from(key: Key) -> Self {
        key.name()
    }
}

impl TryFrom<String> for Key {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Key::read(&name).ok_or_else(|| format!("no pane is named {name:?}"))
    }
}

fn nanos(at: SystemTime) -> Option<u64> {
    u64::try_from(at.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos()).ok()
}

fn moment(nanos: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos)
}

enum Shared {
    Through(PathBuf),
    Lost(String),
    Alone,
}

pub struct Ledger {
    moments: BTreeMap<Key, SystemTime>,
    leftovers: BTreeSet<Session>,
    shared: Shared,
    unfinished: String,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            moments: BTreeMap::new(),
            leftovers: BTreeSet::new(),
            shared: Shared::Alone,
            unfinished: state::unfinished(),
        }
    }
}

impl Ledger {
    pub fn within(dir: PathBuf) -> Self {
        let shared = match fs::create_dir_all(&dir) {
            Ok(()) => Shared::Through(dir),
            Err(error) => Shared::Lost(format!("{}: {error}", dir.display())),
        };
        let mut ledger = Ledger { shared, ..Ledger::default() };
        ledger.reload();
        ledger
    }

    pub fn unshared(why: String) -> Self {
        Ledger { shared: Shared::Lost(why), ..Ledger::default() }
    }

    pub fn dir(&self) -> Option<&Path> {
        match &self.shared {
            Shared::Through(dir) => Some(dir),
            Shared::Lost(_) | Shared::Alone => None,
        }
    }

    pub fn lost(&self) -> Option<&str> {
        match &self.shared {
            Shared::Lost(why) => Some(why),
            Shared::Through(_) | Shared::Alone => None,
        }
    }

    pub fn get(&self, key: Key) -> Option<SystemTime> {
        self.moments.get(&key).copied()
    }

    pub fn latest(&self, session: Session) -> Option<SystemTime> {
        let panes = Key { session, pane: u32::MIN }..=Key { session, pane: u32::MAX };
        self.moments.range(panes).map(|(_, moment)| *moment).max()
    }

    pub fn sessions(&self) -> impl Iterator<Item = Session> + '_ {
        self.moments.keys().map(|key| key.session).chain(self.leftovers.iter().copied())
    }

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

    pub fn reload(&mut self) -> bool {
        let Shared::Through(dir) = &self.shared else {
            return false;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let mut moments = BTreeMap::new();
        let mut leftovers = BTreeSet::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let kept = Key::read(name).and_then(|key| {
                let nanos = fs::read_to_string(entry.path()).ok()?.trim_end().parse().ok()?;
                Some((key, moment(nanos)))
            });
            match kept {
                Some((key, moment)) => {
                    moments.insert(key, moment);
                },
                None => leftovers.extend(Key::of_file(name).map(|key| key.session)),
            }
        }
        self.leftovers = leftovers;
        if moments == self.moments {
            return false;
        }
        self.moments = moments;
        true
    }

    pub fn forget(&mut self, gone: &BTreeSet<Session>) {
        let keys: Vec<Key> =
            self.moments.keys().filter(|key| gone.contains(&key.session)).copied().collect();
        for key in keys {
            self.set(key, None);
        }
        self.leftovers.retain(|session| !gone.contains(session));
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
            if Key::of_file(name).is_some_and(|key| gone.contains(&key.session)) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn write(dir: &Path, unfinished: &str, key: Key, moment: Option<SystemTime>) -> Result<(), String> {
    let name = key.name();
    let file = dir.join(&name);
    let kept = match moment {
        Some(moment) => {
            let unnamed = || format!("{}: a moment from before 1970 or after 2554", dir.display());
            let nanos = nanos(moment).ok_or_else(unnamed)?;
            let unfinished = dir.join(format!("{name}.{unfinished}"));
            fs::write(&unfinished, format!("{nanos}\n"))
                .and_then(|()| fs::rename(&unfinished, &file))
        },
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
        Session::new(at(started), NonZeroU64::new(socket).unwrap()).unwrap()
    }

    fn key(started: u64, pane: u32) -> Key {
        Key { session: session(started, 1), pane }
    }

    fn keys(ledger: &Ledger) -> Vec<Key> {
        ledger.moments.keys().copied().collect()
    }

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
        here.set(key(10, 3), None);
        assert!(!here.reload());
        assert_eq!((here.get(key(10, 3)), here.get(key(20, 0))), (None, Some(at(7))));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    #[test]
    fn only_a_pane_s_file_that_reads_as_a_moment_is_one() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        ledger.set(key(10, 1), Some(at(5)));
        let name = |pane| key(10, pane).name();
        let other = |name: String| fs::write(scratch.path().join(name), "5").unwrap();
        fs::write(scratch.path().join(name(2)), "\n").unwrap();
        other(format!("{}.{}", name(3), state::unfinished()));
        other("4".to_owned());
        other(format!("+{}", name(5)));
        other(format!("0{}", name(6)));
        other(format!("{}-7", nanos(at(10)).unwrap()));
        other(format!("{OWN_PREFIX}{}-0-8", nanos(at(10)).unwrap()));
        ledger.reload();
        assert_eq!(keys(&ledger), [key(10, 1)]);
    }

    #[test]
    fn an_own_session_s_panes_are_kept_apart_from_others() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        let id = OwnId::new(1).unwrap();
        let made = |seconds| OwnMade::of(at(seconds)).unwrap();
        let (own, next) = (Session::own(made(10), id), Session::own(made(20), id));
        let zellij = session(10, 1);
        ledger.set(Key { session: own, pane: 3 }, Some(at(5)));
        ledger.set(Key { session: next, pane: 3 }, Some(at(6)));
        ledger.set(Key { session: zellij, pane: 3 }, Some(at(7)));
        let again = Ledger::within(scratch.path().to_owned());
        assert_eq!(again.latest(own), Some(at(5)));
        assert_eq!(again.latest(next), Some(at(6)));
        assert_eq!(again.latest(zellij), Some(at(7)));
        assert_eq!(own.started(), at(10));
    }

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

    #[test]
    fn a_session_s_last_moment_is_the_last_of_its_panes() {
        let mut ledger = Ledger::default();
        let (session_of, twin) = (session(10, 1), session(10, 2));
        assert_eq!(ledger.latest(session_of), None);
        ledger.set(Key { session: session_of, pane: u32::MIN }, Some(at(7)));
        ledger.set(Key { session: session_of, pane: 5 }, Some(at(9)));
        ledger.set(Key { session: session_of, pane: u32::MAX }, Some(at(8)));
        ledger.set(Key { session: twin, pane: 1 }, Some(at(20)));
        assert_eq!(ledger.latest(session_of), Some(at(9)));
    }

    #[test]
    fn a_session_forgotten_takes_its_moments_and_leaves_the_rest() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = Ledger::within(scratch.path().to_owned());
        let twin = Key { session: session(10, 2), pane: 1 };
        here.set(key(10, 1), Some(at(5)));
        here.set(twin, Some(at(5)));
        here.set(key(20, 1), Some(at(6)));
        let left = [key(10, 2), key(20, 2)]
            .map(|key| scratch.path().join(format!("{}.{}", key.name(), state::unfinished())));
        for file in &left {
            fs::write(file, "5").unwrap();
        }
        here.forget(&BTreeSet::from([session(10, 1)]));
        for ledger in [&here, &Ledger::within(scratch.path().to_owned())] {
            assert_eq!(keys(ledger), [twin, key(20, 1)]);
        }
        assert_eq!(left.map(|file| file.exists()), [false, true]);
    }

    #[test]
    fn a_session_only_leftovers_are_named_for_is_one_to_forget() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        let half = format!("{}.{}", key(30, 1).name(), state::unfinished());
        let [half, empty] = [half, key(40, 1).name()].map(|name| scratch.path().join(name));
        fs::write(&half, "5").unwrap();
        fs::write(&empty, "").unwrap();
        ledger.reload();
        let gone = BTreeSet::from([session(30, 1), session(40, 1)]);
        assert_eq!(ledger.sessions().collect::<BTreeSet<_>>(), gone);
        ledger.forget(&gone);
        assert_eq!((half.exists(), empty.exists(), ledger.sessions().count()), (false, false, 0));
    }

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
