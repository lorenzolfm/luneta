use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::projects::{ProjectId, ViewId};
use serde::{Deserialize, Serialize};

use crate::state;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Session {
    Project(ProjectId),
}

const PROJECT_PREFIX: &str = "project-";

impl Session {
    fn name(self) -> String {
        match self {
            Session::Project(project) => format!("{PROJECT_PREFIX}{project}"),
        }
    }

    fn read(name: &str) -> Option<Self> {
        let session = Session::Project(name.strip_prefix(PROJECT_PREFIX)?.parse().ok()?);
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
    pub view: ViewId,
}

impl Key {
    fn name(self) -> String {
        format!("{}-{}", self.session.name(), self.view)
    }

    fn read(name: &str) -> Option<Self> {
        let (session, view) = name.rsplit_once('-')?;
        let key = Key { session: Session::read(session)?, view: view.parse().ok()? };
        (key.name() == name).then_some(key)
    }

    fn of_file(name: &str) -> Option<Self> {
        Key::read(name.split_once('.').map_or(name, |(view, _)| view))
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
        Key::read(&name).ok_or_else(|| format!("no view is named {name:?}"))
    }
}

fn swept_of_old_names(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str().and_then(Key::of_file).is_none() {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn nanos(at: SystemTime) -> Option<u64> {
    u64::try_from(at.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos()).ok()
}

pub fn moment(nanos: u64) -> SystemTime {
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
            Ok(()) => {
                swept_of_old_names(&dir);
                Shared::Through(dir)
            },
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
        let first = ViewId::of(1).expect("1 is a view's id");
        let last = ViewId::of(u64::MAX).expect("u64::MAX is a view's id");
        let views = Key { session, view: first }..=Key { session, view: last };
        self.moments.range(views).map(|(_, moment)| *moment).max()
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
    assert_eq!(Key::of_file(&name), Some(key), "a ledger opening never sweeps a view's file");
    let file = dir.join(&name);
    let kept = match moment {
        Some(moment) => {
            let unnamed = || format!("{}: a moment from before 1970 or after 2554", dir.display());
            let nanos = nanos(moment).ok_or_else(unnamed)?;
            let unfinished = format!("{name}.{unfinished}");
            let swept = Key::of_file(&unfinished);
            assert_eq!(swept, Some(key), "a ledger opening never sweeps a file half written");
            let unfinished = dir.join(unfinished);
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

    fn session(project: u64) -> Session {
        Session::Project(ProjectId::of(project).unwrap())
    }

    fn key(project: u64, view: u64) -> Key {
        Key { session: session(project), view: ViewId::of(view).unwrap() }
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
        here.set(key(20, u64::MAX), Some(at(7)));
        assert!(there.reload());
        let both = (there.get(key(10, 3)), there.get(key(20, u64::MAX)));
        assert_eq!(both, (Some(precise), Some(at(7))));
        assert_eq!(Ledger::within(dir.clone()).get(key(10, 3)), Some(precise));

        there.set(key(10, 3), None);
        here.set(key(10, 3), None);
        assert!(!here.reload());
        assert_eq!((here.get(key(10, 3)), here.get(key(20, u64::MAX))), (None, Some(at(7))));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    #[test]
    fn only_a_view_s_file_that_reads_as_a_moment_is_one() {
        let scratch = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::within(scratch.path().to_owned());
        ledger.set(key(10, 1), Some(at(5)));
        let name = |view| key(10, view).name();
        let other = |name: String| fs::write(scratch.path().join(name), "5").unwrap();
        fs::write(scratch.path().join(name(2)), "\n").unwrap();
        other(format!("{}.{}", name(3), state::unfinished()));
        other("4".to_owned());
        other(format!("+{}", name(5)));
        other(format!("0{}", name(6)));
        other(format!("{PROJECT_PREFIX}{}-7", nanos(at(10)).unwrap()));
        other(format!("{}-8", session(10).name()));
        ledger.reload();
        assert_eq!(keys(&ledger), [key(10, 1)]);
    }

    #[test]
    fn files_of_an_old_name_are_swept_when_a_ledger_opens() {
        let scratch = tempfile::tempdir().unwrap();
        let file = |name: String| scratch.path().join(name);
        let old = file(format!("{}-1-3", nanos(at(10)).unwrap()));
        let own = file(format!("own-{}-1-3", nanos(at(10)).unwrap()));
        let half = file(format!("own-10-1-3.{}", state::unfinished()));
        let by_mux_id = file(format!("{}-3", session(10).name()));
        let half_by_mux_id = file(format!("{}-3.{}", session(10).name(), state::unfinished()));
        let writing = file(format!("{}.{}", key(10, 2).name(), state::unfinished()));
        let swept = [&old, &own, &half, &by_mux_id, &half_by_mux_id];
        for file in swept.into_iter().chain([&writing]) {
            fs::write(file, "5").unwrap();
        }
        let mut ledger = Ledger::within(scratch.path().to_owned());
        ledger.set(key(10, 1), Some(at(5)));
        assert_eq!(swept.map(|file| file.exists()), [false; 5]);
        assert!(writing.exists());
        assert_eq!(Ledger::within(scratch.path().to_owned()).get(key(10, 1)), Some(at(5)));
        assert_eq!(Session::read(&format!("{}-1", nanos(at(10)).unwrap())), None);
    }

    #[test]
    fn a_project_s_views_are_named_for_its_id() {
        let project = session(0x2a);
        assert_eq!(project.name(), "project-000000000000002a");
        assert_eq!(Session::read("project-000000000000002a"), Some(project));
        assert_eq!(Session::read("project-2a"), None);
    }

    #[test]
    fn a_view_is_named_for_its_project_s_id_and_its_own() {
        let name = "project-000000000000002a-00000000000000ff";
        assert_eq!(key(0x2a, 0xff).name(), name);
        assert_eq!(Key::read(name), Some(key(0x2a, 0xff)));
        assert_eq!(Key::read("project-000000000000002a-255"), None);
        assert_eq!(Key::read("project-000000000000002a-0000000000000000"), None);
    }

    #[test]
    fn two_lunetas_writing_one_view_at_once_both_go_on_sharing() {
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
    fn a_session_s_last_moment_is_the_last_of_its_views() {
        let mut ledger = Ledger::default();
        let (session_of, twin) = (session(10), session(11));
        assert_eq!(ledger.latest(session_of), None);
        let view = |session, view| Key { session, view: ViewId::of(view).unwrap() };
        ledger.set(view(session_of, 1), Some(at(7)));
        ledger.set(view(session_of, 5), Some(at(9)));
        ledger.set(view(session_of, u64::MAX), Some(at(8)));
        ledger.set(view(twin, 1), Some(at(20)));
        assert_eq!(ledger.latest(session_of), Some(at(9)));
    }

    #[test]
    fn a_session_forgotten_takes_its_moments_and_leaves_the_rest() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = Ledger::within(scratch.path().to_owned());
        let twin = key(11, 1);
        here.set(key(10, 1), Some(at(5)));
        here.set(twin, Some(at(5)));
        here.set(key(20, 1), Some(at(6)));
        let left = [key(10, 2), key(20, 2)]
            .map(|key| scratch.path().join(format!("{}.{}", key.name(), state::unfinished())));
        for file in &left {
            fs::write(file, "5").unwrap();
        }
        here.forget(&BTreeSet::from([session(10)]));
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
        let gone = BTreeSet::from([session(30), session(40)]);
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
