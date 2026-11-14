use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::hash::Hash;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use lunetad::Destination;
use notify::RecommendedWatcher;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::host::Event;
use crate::ledger::{self, Key};
use crate::live::{Lists, Missing};
use crate::machines::Machine;
use crate::state;

const LOCAL: &str = "-";

const LOCK: &str = "order.lock";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    sessions: Vec<String>,
    #[serde(
        deserialize_with = "remotes_known",
        serialize_with = "remotes_named",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    remote_sessions: BTreeMap<Destination, Vec<String>>,
    #[serde(
        deserialize_with = "machines_known",
        serialize_with = "machines_named",
        skip_serializing_if = "Vec::is_empty"
    )]
    machines: Vec<Machine>,
    #[serde(deserialize_with = "known", skip_serializing_if = "BTreeMap::is_empty")]
    colours: BTreeMap<ledger::Session, Colour>,
    #[serde(deserialize_with = "panes_known", skip_serializing_if = "BTreeMap::is_empty")]
    panes: BTreeMap<ledger::Session, Vec<Key>>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Colour {
    Blue,
    Yellow,
    Purple,
    Aqua,
    Orange,
    Green,
}

impl Colour {
    pub const ALL: [Colour; 6] =
        [Colour::Blue, Colour::Yellow, Colour::Purple, Colour::Aqua, Colour::Orange, Colour::Green];
}

#[derive(Default)]
pub struct Order {
    kept: Kept,
    missing: Missing,
    heard: BTreeSet<ledger::Session>,
    file: Option<PathBuf>,
    lost: Option<String>,
    unread: Option<String>,
}

impl Order {
    pub fn shared() -> Self {
        match state::path("order") {
            Ok(file) => Order::within(file),
            Err(unset) => Order { lost: Some(unset.to_string()), ..Order::default() },
        }
    }

    pub fn within(file: PathBuf) -> Self {
        let mut order = Order::default();
        match file.parent().map_or(Ok(()), fs::create_dir_all) {
            Ok(()) => order.file = Some(file),
            Err(error) => order.lost = Some(format!("{}: {error}", file.display())),
        }
        order.reload();
        order
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub fn lost(&self) -> Option<&str> {
        self.lost.as_deref()
    }

    pub fn unread(&self) -> Option<&str> {
        self.unread.as_deref()
    }

    pub fn reload(&mut self) -> bool {
        let Some(file) = &self.file else {
            return false;
        };
        let read = match fs::read_to_string(file) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Kept::default()),
            Err(error) => Err(error.to_string()),
        };
        let kept = match read {
            Ok(kept) => kept,
            Err(why) => {
                self.unread = Some(format!("{}: {why}", file.display()));
                return false;
            },
        };
        self.unread = None;
        if kept == self.kept {
            return false;
        }
        self.kept = kept;
        true
    }

    pub fn sessions<T>(&self, machine: &Machine, sessions: &mut [T], name: impl Fn(&T) -> &str) {
        let order = self.kept.sessions_of(machine);
        sessions.sort_by_key(|session| rank(order, name(session)));
    }

    pub fn keep_sessions(&mut self, machine: &Machine, sessions: Vec<String>) {
        let sessions = once_each(sessions);
        self.keep(|kept| {
            let order = kept.sessions_in(machine);
            *order = merged(order, &sessions);
        });
    }

    pub fn machines<T>(&self, machines: &mut [T], machine: impl Fn(&T) -> &Machine) {
        machines.sort_by_key(|it| rank(&self.kept.machines, machine(it)));
    }

    pub fn keep_machines(&mut self, machines: &[Machine]) {
        let machines = once_each(machines.to_vec());
        self.keep(|kept| kept.machines = merged(&kept.machines, &machines));
    }

    pub fn placed<T: Copy>(&self, sessions: &[(ledger::Session, Vec<Tabbed<T>>)]) -> Vec<Vec<T>> {
        let all: HashMap<Key, (u32, T)> = (sessions.iter().flat_map(|(_, panes)| panes))
            .map(|(key, tab, pane)| (*key, (*tab, *pane)))
            .collect();
        let mut shown: HashSet<Key> = HashSet::new();
        let mut placed: Vec<Vec<Tabbed<T>>> = Vec::with_capacity(sessions.len());
        for (session, _) in sessions {
            let kept = self.kept.panes.get(session).map_or(&[][..], Vec::as_slice);
            let under = kept.iter().filter_map(|key| {
                let (tab, pane) = *all.get(key)?;
                shown.insert(*key).then_some((*key, tab, pane))
            });
            placed.push(under.collect());
        }
        for ((session, own), under) in sessions.iter().zip(&mut placed) {
            for &(key, tab, pane) in own {
                if shown.insert(key) {
                    under.insert(slot(under, *session, tab), (key, tab, pane));
                }
            }
        }
        let bare = |under: Vec<Tabbed<T>>| under.into_iter().map(|(.., pane)| pane).collect();
        placed.into_iter().map(bare).collect()
    }

    pub fn keep_panes(&mut self, panes: Vec<(ledger::Session, Vec<Key>)>) {
        self.keep(|kept| {
            for (session, under) in panes {
                let under = once_each(under);
                for others in kept.panes.values_mut() {
                    others.retain(|key| !under.contains(key));
                }
                kept.panes.insert(session, under);
            }
            kept.panes.retain(|_, under| !under.is_empty());
        });
    }

    pub fn colour_sessions(&mut self, running: &[ledger::Session]) -> bool {
        self.heard.extend(running);
        let mut colours = self.kept.colours.clone();
        give(&mut colours, running, &self.heard);
        if colours == self.kept.colours {
            return false;
        }
        let heard = self.heard.clone();
        self.keep_read(|kept| give(&mut kept.colours, running, &heard));
        true
    }

    pub fn colours(&self, running: &[ledger::Session]) -> Vec<Colour> {
        let mut colours = self.kept.colours.clone();
        give(&mut colours, running, &self.heard);
        running.iter().map(|session| colours[session]).collect()
    }

    pub fn listed(&mut self, live: Option<&Lists>, at: Instant) -> bool {
        let moved = self.kept.panes.iter().flat_map(|(session, under)| {
            std::iter::once(session).chain(under.iter().map(|key| &key.session))
        });
        let kept: BTreeSet<ledger::Session> =
            self.kept.colours.keys().chain(&self.heard).chain(moved).copied().collect();
        let gone = self.missing.gone(&kept, live, at);
        self.heard.retain(|session| !gone.contains(session));
        let ended = |session: &ledger::Session| gone.contains(session);
        let panes_gone = self.kept.panes.iter().any(|(session, under)| {
            ended(session) || under.iter().any(|key| ended(&key.session))
        });
        if !self.kept.colours.keys().any(ended) && !panes_gone {
            return false;
        }
        self.keep_read(|kept| {
            kept.colours.retain(|session, _| !ended(session));
            kept.panes.retain(|session, _| !ended(session));
            for under in kept.panes.values_mut() {
                under.retain(|key| !ended(&key.session));
            }
            kept.panes.retain(|_, under| !under.is_empty());
        });
        true
    }

    fn keep(&mut self, change: impl FnOnce(&mut Kept)) {
        let _held = self.lock();
        self.reload();
        self.write_kept(change);
    }

    fn keep_read(&mut self, change: impl FnOnce(&mut Kept)) {
        let _held = self.lock();
        self.reload();
        if self.unread.is_some() {
            change(&mut self.kept);
            return;
        }
        self.write_kept(change);
    }

    fn lock(&self) -> Option<File> {
        let lock = File::create(self.file.as_ref()?.with_file_name(LOCK)).ok()?;
        lock.lock().ok()?;
        Some(lock)
    }

    fn write_kept(&mut self, change: impl FnOnce(&mut Kept)) {
        let mut kept = self.kept.clone();
        change(&mut kept);
        if let Some(file) = &self.file
            && let Err(why) = write(file, &kept)
        {
            self.lost = Some(why);
            self.file = None;
        }
        self.unread = None;
        self.kept = kept;
    }
}

impl Kept {
    fn sessions_of(&self, machine: &Machine) -> &[String] {
        match machine {
            Machine::Local => &self.sessions,
            Machine::Remote(destination) => {
                self.remote_sessions.get(destination).map_or(&[], Vec::as_slice)
            },
        }
    }

    fn sessions_in(&mut self, machine: &Machine) -> &mut Vec<String> {
        match machine {
            Machine::Local => &mut self.sessions,
            Machine::Remote(destination) => {
                self.remote_sessions.entry(destination.clone()).or_default()
            },
        }
    }
}

fn once_each<T: Eq + Hash + Clone>(mut items: Vec<T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
    items
}

pub type Tabbed<T> = (Key, u32, T);

fn slot<T>(under: &[Tabbed<T>], session: ledger::Session, tab: u32) -> usize {
    let tab_of = |(key, of, _): &Tabbed<T>| (key.session == session).then_some(*of);
    if let Some(last) = under.iter().rposition(|pane| tab_of(pane) == Some(tab)) {
        return last + 1;
    }
    under.iter().position(|pane| tab_of(pane).is_some_and(|of| of > tab)).unwrap_or(under.len())
}

fn rank<T: PartialEq<U>, U: ?Sized>(order: &[T], item: &U) -> usize {
    order.iter().position(|it| it == item).unwrap_or(usize::MAX)
}

fn merged<T: PartialEq + Clone>(kept: &[T], running: &[T]) -> Vec<T> {
    let runs = |name: &&T| running.contains(name);
    let mut merged: Vec<T> = kept.iter().take_while(|name| !runs(name)).cloned().collect();
    for name in running {
        merged.push(name.clone());
        if let Some(at) = kept.iter().position(|it| it == name) {
            merged.extend(kept[at + 1..].iter().take_while(|name| !runs(name)).cloned());
        }
    }
    merged
}

fn give(
    colours: &mut BTreeMap<ledger::Session, Colour>,
    running: &[ledger::Session],
    heard: &BTreeSet<ledger::Session>,
) {
    let holds = |session: &ledger::Session| running.contains(session) || heard.contains(session);
    for session in running {
        if colours.contains_key(session) {
            continue;
        }
        let had = |colour: Colour| {
            colours.iter().filter(|(holder, it)| **it == colour && holds(holder)).count()
        };
        let fewest = Colour::ALL.into_iter().fold(Colour::ALL[0], |fewest, colour| {
            if had(colour) < had(fewest) { colour } else { fewest }
        });
        colours.insert(*session, fewest);
    }
}

fn known<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ledger::Session, Colour>, D::Error> {
    let kept = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    let known = kept.into_iter().filter_map(|(session, colour)| {
        Some((ledger::Session::try_from(session).ok()?, serde_json::from_value(colour).ok()?))
    });
    Ok(known.collect())
}

fn panes_known<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ledger::Session, Vec<Key>>, D::Error> {
    let kept = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    let known = kept.into_iter().filter_map(|(session, under)| {
        let names = Vec::<String>::deserialize(under).ok()?;
        let under = names.into_iter().filter_map(|name| Key::try_from(name).ok());
        Some((ledger::Session::try_from(session).ok()?, under.collect()))
    });
    Ok(known.collect())
}

fn remotes_known<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<Destination, Vec<String>>, D::Error> {
    let kept = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    let known = kept.into_iter().filter_map(|(destination, sessions)| {
        Some((destination.parse().ok()?, serde_json::from_value(sessions).ok()?))
    });
    Ok(known.collect())
}

fn remotes_named<S: Serializer>(
    remotes: &BTreeMap<Destination, Vec<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let named = remotes.iter().map(|(destination, sessions)| (destination.to_string(), sessions));
    serializer.collect_map(named)
}

fn machines_known<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Machine>, D::Error> {
    let kept = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let known = kept.iter().filter_map(|named| match named.as_str()? {
        LOCAL => Some(Machine::Local),
        destination => Some(Machine::Remote(destination.parse().ok()?)),
    });
    Ok(once_each(known.collect()))
}

fn machines_named<S: Serializer>(machines: &[Machine], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(machines.iter().map(|machine| match machine {
        Machine::Local => LOCAL.to_owned(),
        Machine::Remote(destination) => destination.to_string(),
    }))
}

fn write(file: &Path, kept: &Kept) -> Result<(), String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", file.display());
    let mut text = serde_json::to_string_pretty(kept).map_err(|error| failed(&error))?;
    text.push('\n');
    let unfinished = file.with_file_name(format!("order.{}", state::unfinished()));
    let written = fs::write(&unfinished, text).and_then(|()| fs::rename(&unfinished, file));
    if written.is_err() {
        let _ = fs::remove_file(&unfinished);
    }
    written.map_err(|error| failed(&error))
}

pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Order)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::live::GONE;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn session(secs: u64) -> ledger::Session {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        ledger::Session::new(started, NonZeroU64::new(secs).unwrap()).unwrap()
    }

    fn listing(running: &[ledger::Session]) -> Lists {
        let mut live = Lists::default();
        live.lists(&Machine::Local, running.iter().copied(), true);
        live
    }

    fn colour(order: &mut Order, running: &[ledger::Session]) -> Vec<Colour> {
        order.colour_sessions(running);
        order.colours(running)
    }

    fn sorted(order: &Order, listed: &[&str]) -> Vec<String> {
        let mut sessions = names(listed);
        order.sessions(&Machine::Local, &mut sessions, String::as_str);
        sessions
    }

    #[test]
    fn a_move_in_one_luneta_shows_in_every_other_and_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join("order");
        let mut here = Order::within(file.clone());
        let mut there = Order::within(file.clone());
        assert_eq!(sorted(&there, &["a", "b", "c"]), ["a", "b", "c"]);

        here.keep_sessions(&Machine::Local, names(&["b", "a"]));
        assert!(there.reload());
        assert!(!there.reload());
        assert_eq!(sorted(&there, &["a", "b", "c"]), ["b", "a", "c"]);
        assert_eq!(sorted(&Order::within(file), &["a", "b", "c"]), ["b", "a", "c"]);
        assert_eq!((here.lost(), here.unread(), there.lost(), there.unread()), (None, None, None, None));
    }

    #[test]
    fn a_move_keeps_what_it_did_not_move() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut here = Order::within(file.clone());
        Order::within(file.clone()).keep_sessions(&Machine::Local, names(&["x", "b", "a"]));
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, text.replacen('{', r#"{"tabs": {"a": [2, 1]},"#, 1)).unwrap();

        here.keep_sessions(&Machine::Local, names(&["a", "b"]));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept, serde_json::json!({"sessions": ["x", "a", "b"], "tabs": {"a": [2, 1]}}));
    }

    #[test]
    fn a_session_that_is_not_running_keeps_its_place() {
        let mut order = Order::default();
        order.keep_sessions(&Machine::Local, names(&["y", "a", "b", "x", "c"]));
        order.keep_sessions(&Machine::Local, names(&["c", "a", "b"]));
        let listed = ["a", "b", "c", "x", "y", "new"];
        assert_eq!(sorted(&order, &listed), ["y", "c", "a", "b", "x", "new"]);
    }

    #[test]
    fn each_machine_s_sessions_and_the_machines_keep_orders_of_their_own() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut order = Order::within(file.clone());
        let nixos = Machine::Remote("nixos".parse().unwrap());
        let vps = Machine::Remote("me@vps".parse().unwrap());
        order.keep_sessions(&Machine::Local, names(&["b", "a"]));
        order.keep_sessions(&nixos, names(&["a", "c", "b"]));
        order.keep_machines(&[nixos.clone(), Machine::Local]);

        let later = Order::within(file.clone());
        let in_order = |machine: &Machine| {
            let mut sessions = names(&["a", "b", "c"]);
            later.sessions(machine, &mut sessions, String::as_str);
            sessions
        };
        assert_eq!(in_order(&Machine::Local), ["b", "a", "c"]);
        assert_eq!(in_order(&nixos), ["a", "c", "b"]);
        assert_eq!(in_order(&vps), ["a", "b", "c"]);
        let mut machines = [Machine::Local, vps.clone(), nixos.clone()];
        later.machines(&mut machines, |machine| machine);
        assert_eq!(machines, [nixos, Machine::Local, vps]);

        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let expected = serde_json::json!({
            "sessions": ["b", "a"],
            "remote_sessions": {"nixos": ["a", "c", "b"]},
            "machines": ["nixos", "-"],
        });
        assert_eq!(kept, expected);
    }

    #[test]
    fn a_machine_that_is_not_listed_keeps_its_place() {
        let mut order = Order::default();
        let remote = |it: &str| Machine::Remote(it.parse().unwrap());
        let [nixos, vps, pi] = ["nixos", "me@vps", "pi"].map(remote);
        order.keep_machines(&[nixos.clone(), vps.clone(), Machine::Local]);
        order.keep_machines(&[Machine::Local, nixos.clone()]);
        let mut machines = [pi.clone(), vps.clone(), nixos.clone(), Machine::Local];
        order.machines(&mut machines, |machine| machine);
        assert_eq!(machines, [Machine::Local, nixos, vps, pi]);
    }

    #[test]
    fn what_names_no_machine_reads_as_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let text = r#"{
            "sessions": ["b", "a"],
            "remote_sessions": {"-": ["a", "b"], "a b": ["x"], "nixos": ["d", "c"], "pi": 7},
            "machines": ["pi", "", "-", 3, "pi", "nixos"]
        }"#;
        fs::write(&file, text).unwrap();
        let mut order = Order::within(file.clone());
        assert_eq!(order.unread(), None);
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
        let [nixos, pi] = ["nixos", "pi"].map(|it| Machine::Remote(it.parse().unwrap()));
        let mut machines = [Machine::Local, nixos.clone(), pi.clone()];
        order.machines(&mut machines, |machine| machine);
        assert_eq!(machines, [pi.clone(), Machine::Local, nixos]);

        order.keep_sessions(&pi, names(&["e"]));
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        let expected = serde_json::json!({
            "sessions": ["b", "a"],
            "remote_sessions": {"nixos": ["d", "c"], "pi": ["e"]},
            "machines": ["pi", "-", "nixos"],
        });
        assert_eq!(kept, expected);
    }

    #[test]
    fn a_change_waits_for_the_lock_and_keeps_what_was_written_under_it() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut here = Order::within(file.clone());
        let lock = File::create(scratch.path().join(LOCK)).unwrap();
        lock.lock().unwrap();
        let moving = thread::spawn(move || here.keep_sessions(&Machine::Local, names(&["b", "a"])));
        thread::sleep(Duration::from_millis(100));
        assert!(!file.exists());
        fs::write(&file, r#"{"remote_sessions": {"nixos": ["d", "c"]}}"#).unwrap();
        drop(lock);
        moving.join().unwrap();
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        let expected = serde_json::json!({
            "sessions": ["b", "a"],
            "remote_sessions": {"nixos": ["d", "c"]},
        });
        assert_eq!(kept, expected);
    }

    #[test]
    fn a_lock_that_cannot_be_had_leaves_the_file_shared() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        fs::create_dir(scratch.path().join(LOCK)).unwrap();
        let mut here = Order::within(file.clone());
        let [a, b] = [1, 2].map(session);
        assert_eq!(colour(&mut here, &[a, b]), [Colour::Blue, Colour::Yellow]);
        here.keep_sessions(&Machine::Local, names(&["b", "a"]));
        assert_eq!((here.lost(), here.file()), (None, Some(file.as_path())));
        let mut later = Order::within(file);
        assert_eq!(colour(&mut later, &[b, a]), [Colour::Yellow, Colour::Blue]);
        assert_eq!(sorted(&later, &["a", "b"]), ["b", "a"]);
    }

    #[test]
    fn a_file_that_fails_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut order = Order::within(file.clone());
        assert_eq!(order.unread(), None);
        order.keep_sessions(&Machine::Local, names(&["b", "a"]));
        fs::write(&file, "not JSON").unwrap();
        assert!(!order.reload());
        assert!(order.unread().is_some_and(|why| why.contains("order")));
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
        order.keep_sessions(&Machine::Local, names(&["a", "b"]));
        assert_eq!(order.unread(), None);
        assert_eq!(sorted(&Order::within(file.clone()), &["b", "a"]), ["a", "b"]);

        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(!order.reload());
        assert!(order.unread().is_some_and(|why| why.contains("order")));
        assert_eq!(sorted(&order, &["b", "a"]), ["a", "b"]);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let mut order = Order::within(blocker.join("order"));
        assert!(order.lost().is_some_and(|lost| lost.contains("not a folder")));
        order.keep_sessions(&Machine::Local, names(&["b", "a"]));
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
    }

    #[test]
    fn a_session_takes_the_first_colour_no_other_has_and_keeps_it() {
        use Colour::{Aqua, Blue, Green, Orange, Purple, Yellow};
        let [a, b, c, d] = [1, 2, 3, 4].map(session);
        let mut order = Order::default();
        assert_eq!(colour(&mut order, &[a, b, c]), [Blue, Yellow, Purple]);
        let start = Instant::now();
        let listed = listing(&[b, c]);
        order.listed(Some(&listed), start);
        assert!(order.listed(Some(&listed), start + GONE));
        assert_eq!(colour(&mut order, &[c, d, b]), [Purple, Blue, Yellow]);
        let started = [5, 6, 7, 8].map(session);
        let running: Vec<ledger::Session> = [b, c, d].into_iter().chain(started).collect();
        let colours = colour(&mut order, &running);
        assert_eq!(colours, [Yellow, Purple, Blue, Aqua, Orange, Green, Blue]);
    }

    #[test]
    fn a_session_left_out_of_a_list_keeps_its_colour_until_it_is_gone() {
        use Colour::{Blue, Purple, Yellow};
        let [a, b, c, d] = [1, 2, 3, 4].map(session);
        let mut order = Order::default();
        order.colour_sessions(&[a, b]);
        let start = Instant::now();
        let only_b = listing(&[b]);
        order.listed(Some(&only_b), start);
        assert!(!order.listed(None, start + GONE));
        assert!(!order.listed(Some(&only_b), start + GONE));
        assert_eq!(colour(&mut order, &[b, c]), [Yellow, Purple]);
        assert!(order.listed(Some(&listing(&[b, c])), start + GONE * 2));
        assert_eq!(colour(&mut order, &[b, c, d]), [Yellow, Purple, Blue]);
    }

    #[test]
    fn a_colour_is_the_same_in_every_luneta() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        fs::write(&file, r#"{"sessions": ["b", "a"]}"#).unwrap();
        let mut here = Order::within(file.clone());
        let mut there = Order::within(file.clone());
        let [a, b] = [1, 2].map(session);
        assert_eq!(colour(&mut here, &[b, a]), [Colour::Blue, Colour::Yellow]);
        assert!(there.reload());
        there.keep_sessions(&Machine::Local, names(&["a", "b"]));
        let mut later = Order::within(file.clone());
        assert_eq!(colour(&mut later, &[a, b]), [Colour::Yellow, Colour::Blue]);
        assert_eq!(sorted(&later, &["b", "a"]), ["a", "b"]);
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let colours = serde_json::json!({"1000000000-1": "yellow", "2000000000-2": "blue"});
        assert_eq!(kept["colours"], colours);
    }

    #[test]
    fn a_colour_this_build_does_not_have_reads_as_none() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let kept = r#"{"sessions": ["b", "a"], "colours": {
            "1000000000-1": "pink", "2000000000-2": "purple", "a": "blue"
        }}"#;
        fs::write(&file, kept).unwrap();
        let mut order = Order::within(file.clone());
        assert_eq!(order.unread(), None);
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
        let colours = colour(&mut order, &[session(1), session(2)]);
        assert_eq!(colours, [Colour::Blue, Colour::Purple]);
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let colours = serde_json::json!({"1000000000-1": "blue", "2000000000-2": "purple"});
        assert_eq!(kept["colours"], colours);
    }

    fn key(session: ledger::Session, pane: u32) -> Key {
        Key { session, pane }
    }

    fn tabbed(
        session: ledger::Session,
        panes: &[(u32, u32)],
    ) -> (ledger::Session, Vec<Tabbed<Key>>) {
        let each = panes.iter().map(|(pane, tab)| (key(session, *pane), *tab, key(session, *pane)));
        (session, each.collect())
    }

    fn own(session: ledger::Session, panes: &[u32]) -> (ledger::Session, Vec<Tabbed<Key>>) {
        let panes: Vec<(u32, u32)> = panes.iter().map(|pane| (*pane, *pane)).collect();
        tabbed(session, &panes)
    }

    #[test]
    fn a_pane_shows_where_the_order_keeps_it_and_else_under_its_own() {
        let [a, b, c] = [1, 2, 3].map(session);
        let mut order = Order::default();
        let sessions = [own(a, &[0, 1]), own(b, &[0])];
        assert_eq!(order.placed(&sessions), [vec![key(a, 0), key(a, 1)], vec![key(b, 0)]]);

        order.keep_panes(vec![(b, vec![key(a, 1), key(b, 0), key(a, 7)])]);
        order.kept.panes.insert(a, vec![key(b, 0), key(a, 1)]);
        order.kept.panes.insert(c, vec![key(a, 0)]);
        let placed = order.placed(&sessions);
        assert_eq!(placed, [vec![key(b, 0), key(a, 0), key(a, 1)], Vec::new()]);
    }

    #[test]
    fn a_pane_the_order_does_not_keep_goes_with_its_tab() {
        let [a, b] = [1, 2].map(session);
        let mut order = Order::default();
        order.kept.panes.insert(a, vec![key(a, 1), key(b, 0), key(a, 0), key(a, 5)]);
        let panes = [(0, 0), (4, 0), (1, 1), (2, 1), (5, 2), (6, 2), (3, 3), (7, 9)];
        let sessions = [tabbed(a, &panes), tabbed(b, &[(0, 0)])];
        let placed = [(a, 1), (a, 2), (b, 0), (a, 0), (a, 4), (a, 5), (a, 6), (a, 3), (a, 7)];
        let placed = placed.map(|(session, pane)| key(session, pane)).to_vec();
        assert_eq!(order.placed(&sessions), [placed, Vec::new()]);

        order.kept.panes.insert(a, vec![key(b, 0), key(a, 5)]);
        let sessions = [tabbed(a, &[(1, 1), (5, 2)]), tabbed(b, &[(0, 0)])];
        assert_eq!(order.placed(&sessions)[0], [key(b, 0), key(a, 1), key(a, 5)]);
    }

    #[test]
    fn a_pane_moved_is_kept_under_one_session_alone() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut here = Order::within(file.clone());
        let [a, b] = [1, 2].map(session);
        here.keep_panes(vec![(a, vec![key(b, 3), key(a, 0)]), (b, vec![key(b, 4)])]);
        here.keep_panes(vec![(b, vec![key(b, 4), key(b, 3)])]);
        here.keep_panes(vec![(a, vec![key(b, 4)]), (b, Vec::new())]);
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept["panes"], serde_json::json!({"1000000000-1": ["2000000000-2-4"]}));
        let there = Order::within(file);
        let sessions = [own(a, &[0]), own(b, &[3, 4])];
        assert_eq!(there.placed(&sessions), [vec![key(b, 4), key(a, 0)], vec![key(b, 3)]]);
    }

    #[test]
    fn a_pane_s_place_goes_with_its_session_and_the_one_it_shows_under() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut order = Order::within(file.clone());
        let [a, b, c] = [1, 2, 3].map(session);
        order.keep_panes(vec![(a, vec![key(b, 1), key(c, 1)]), (c, vec![key(a, 1)])]);
        let start = Instant::now();
        let a_and_c = listing(&[a, c]);
        assert!(!order.listed(Some(&a_and_c), start));
        assert!(order.listed(Some(&a_and_c), start + GONE));
        let kept = |order: &Order| order.kept.panes.clone();
        let left = BTreeMap::from([(a, vec![key(c, 1)]), (c, vec![key(a, 1)])]);
        assert_eq!(kept(&order), left);
        let only_a = listing(&[a]);
        assert!(!order.listed(Some(&only_a), start + GONE * 2));
        assert!(order.listed(Some(&only_a), start + GONE * 3));
        assert_eq!(kept(&order), BTreeMap::new());
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept.get("panes"), None);
    }

    #[test]
    fn a_pane_kept_by_no_name_reads_as_none() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let kept = r#"{"panes": {
            "1000000000-1": ["2000000000-2-3", "pane"], "a": ["1000000000-1-1"],
            "2000000000-2": "1000000000-1-1"
        }}"#;
        fs::write(&file, kept).unwrap();
        let order = Order::within(file);
        assert_eq!(order.unread(), None);
        let (a, b) = (session(1), session(2));
        assert_eq!(order.kept.panes, BTreeMap::from([(a, vec![key(b, 3)])]));
    }

    #[test]
    fn a_colour_kept_of_a_session_never_heard_of_is_no_others() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        fs::write(&file, r#"{"colours": {"1000000000-1": "blue"}}"#).unwrap();
        let mut order = Order::within(file.clone());
        let b = session(2);
        assert_eq!(colour(&mut order, &[b]), [Colour::Blue]);
        let start = Instant::now();
        order.listed(Some(&listing(&[b])), start);
        assert!(order.listed(Some(&listing(&[b])), start + GONE));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept["colours"], serde_json::json!({"2000000000-2": "blue"}));
    }

    #[test]
    fn a_colour_leaves_a_file_that_does_not_read_as_it_is() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let unread = "{\"sessions\": [\"b\", \"a\"],}\n";
        fs::write(&file, unread).unwrap();
        let mut order = Order::within(file.clone());
        let [a, b] = [1, 2].map(session);
        assert_eq!(colour(&mut order, &[a, b]), [Colour::Blue, Colour::Yellow]);
        let start = Instant::now();
        order.listed(Some(&listing(&[b])), start);
        assert!(order.listed(Some(&listing(&[b])), start + GONE));
        assert_eq!(fs::read_to_string(&file).unwrap(), unread);
        assert!(order.unread().is_some_and(|why| why.contains("order")));

        fs::write(&file, r#"{"sessions": ["b", "a"]}"#).unwrap();
        assert!(order.reload());
        assert!(order.colour_sessions(&[b]));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let colours = serde_json::json!({"2000000000-2": "blue"});
        assert_eq!(kept, serde_json::json!({"sessions": ["b", "a"], "colours": colours}));
    }

    #[test]
    fn a_move_made_elsewhere_is_heard() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut here = Order::within(file.clone());
        let (events, heard) = mpsc::channel();
        let _watcher = watch(&file, events).unwrap();
        fs::write(scratch.path().join("width"), "40\n").unwrap();
        here.reload();
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());

        Order::within(file).keep_sessions(&Machine::Local, names(&["b", "a"]));
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Order)));
        assert!(here.reload());
    }
}
