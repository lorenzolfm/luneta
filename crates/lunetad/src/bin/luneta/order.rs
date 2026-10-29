//! The order the sidebar lists the sessions in, as you moved them, and the
//! colour of each, the same in every luneta on the host, and kept for the
//! next.
//!
//! A session goes by its name, as a fold does, so that one started again under
//! it comes back to its place, and the sessions that share one go together. A
//! session the order does not have goes after those it has, oldest first: with
//! nothing moved, the rows go as they always went. A session's panes go in the
//! order of their tabs: zellij numbers a session's panes from 0 each time it
//! starts, so no id would keep a pane's place in one started again.
//!
//! A session takes a colour as a browser's group of tabs does as it opens:
//! the first that no other session has. A colour goes by the session itself,
//! as the news does, rather than by its name: a session keeps its own as it
//! is renamed, and two of one name have one each. It keeps it for as long as
//! it runs, as sessions close and move around it, and gives it up once the
//! daemon has not listed it for `GONE`, as a rename made while the daemon
//! reads zellij's folder leaves a session out of one list. One that ended
//! while no luneta ran, as the one luneta ends with does, holds its colour
//! against no other.
//!
//! It is kept as JSON in the file `luneta/order` of `$XDG_STATE_HOME`, or of
//! `~/.local/state` where that is not set. The file is written whole, under a
//! name of its writer's own, before it takes its own, so that no luneta reads
//! it halfway written. A move is made to the order as the file has it then,
//! and what the file has that this luneta does not know of, as a luneta of a
//! later build may keep there, is written back as it was. Each luneta watches
//! the file, and takes up a move another makes as it makes it. Of two that
//! move sessions at once, the last to write holds the order.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use notify::RecommendedWatcher;
use serde::{Deserialize, Deserializer, Serialize};

use crate::host::Event;
use crate::ledger;
use crate::news::GONE;
use crate::state;

/// The order as its file has it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    /// The sessions, by name.
    sessions: Vec<String>,
    /// The colour of each session that runs, by the session's name in the
    /// ledger.
    #[serde(deserialize_with = "known", skip_serializing_if = "BTreeMap::is_empty")]
    colours: BTreeMap<ledger::Session, Colour>,
    /// What else the file has, as a luneta of another build keeps there,
    /// written back as it was read.
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// The colour of a session, as gruvbox names it.
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
    /// Every colour, in the order sessions take them: those started one after
    /// the other take them in this order, which keeps the ones that look
    /// alike apart, round the wrap too. No session is red, which says what
    /// went wrong.
    pub const ALL: [Colour; 6] =
        [Colour::Blue, Colour::Yellow, Colour::Purple, Colour::Aqua, Colour::Orange, Colour::Green];
}

#[derive(Default)]
pub struct Order {
    kept: Kept,
    /// The sessions with a colour that the daemon does not list, and since
    /// when, by luneta's own clock.
    missing: BTreeMap<ledger::Session, Instant>,
    /// The sessions this luneta has heard the daemon list, until they are
    /// gone: one of them holds its colour while the daemon leaves it out.
    /// One it never heard of, as one that ended before it started, holds
    /// none.
    heard: BTreeSet<ledger::Session>,
    /// The file it is shared through. Without one, the order is this luneta's
    /// alone: a test's, or one whose file failed it.
    file: Option<PathBuf>,
    /// Why the order is this luneta's alone, where its file failed it.
    lost: Option<String>,
    /// Why the file does not read as an order, until it does, or is written
    /// over.
    unread: Option<String>,
}

impl Order {
    /// The order every luneta on the host shares, as it stands.
    pub fn shared() -> Self {
        match state::path("order") {
            Some(file) => Order::within(file),
            None => Order {
                lost: Some("neither XDG_STATE_HOME nor HOME is set, to keep it in".to_owned()),
                ..Order::default()
            },
        }
    }

    /// The order shared through `file`, as it stands.
    pub fn within(file: PathBuf) -> Self {
        let mut order = Order::default();
        match file.parent().map_or(Ok(()), fs::create_dir_all) {
            Ok(()) => order.file = Some(file),
            Err(error) => order.lost = Some(format!("{}: {error}", file.display())),
        }
        order.reload();
        order
    }

    /// The file it is shared through, while it is.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Why the order is this luneta's alone, where its file failed it.
    pub fn lost(&self) -> Option<&str> {
        self.lost.as_deref()
    }

    /// Why its file does not read as an order, while it does not.
    pub fn unread(&self) -> Option<&str> {
        self.unread.as_deref()
    }

    /// Takes up the order as another luneta kept it since, and says whether
    /// that changed anything. No file is no order, and one that does not read
    /// as one leaves the order as it was.
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

    /// Puts `sessions` in the order: those it has first, as it has them, by
    /// the `name` of each, and the rest after, as they came.
    pub fn sessions<T>(&self, sessions: &mut [T], name: impl Fn(&T) -> &str) {
        sessions.sort_by_key(|session| rank(&self.kept.sessions, name(session)));
    }

    /// Keeps `sessions`, the names of those that run, in their order now, for
    /// every luneta. A session the order has that does not run stays after
    /// the one it came after, to come back there once it runs again. Those
    /// that run go as `sessions` has them, whatever the file has: a move is
    /// to be made to them as `reload` takes them up just before, or it undoes
    /// one made elsewhere since this luneta last heard.
    pub fn keep_sessions(&mut self, mut sessions: Vec<String>) {
        let mut named = BTreeSet::new();
        sessions.retain(|name| named.insert(name.clone()));
        self.keep(|kept| kept.sessions = merged(&kept.sessions, &sessions));
    }

    /// Gives each of `running`, the sessions that run, in its order, that
    /// has no colour the first that the fewest of the others hold, kept for
    /// every luneta. None gives its own up here: a session the daemon leaves
    /// out of a list may still run, and `listed` takes the colour of one that
    /// has gone. Says whether the order was kept, and so taken up as another
    /// luneta left it.
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

    /// The colours of `running`, in its order, as they are kept: one with
    /// none has the one `colour_sessions` would give it, here alone.
    pub fn colours(&self, running: &[ledger::Session]) -> Vec<Colour> {
        let mut colours = self.kept.colours.clone();
        give(&mut colours, running, &self.heard);
        running.iter().map(|session| colours[session]).collect()
    }

    /// The daemon lists the sessions of `live` at `at`, by luneta's own
    /// clock, or none where its list may leave out one that runs. The colour
    /// of a session it has not listed for `GONE` goes, for every luneta. Says
    /// whether the order was kept, and so taken up as another luneta left it.
    pub fn listed(&mut self, live: Option<&BTreeSet<ledger::Session>>, at: Instant) -> bool {
        let Some(live) = live else {
            self.missing.clear();
            return false;
        };
        let unlisted: BTreeSet<ledger::Session> =
            self.kept.colours.keys().filter(|session| !live.contains(session)).copied().collect();
        self.missing.retain(|session, _| unlisted.contains(session));
        for session in unlisted {
            self.missing.entry(session).or_insert(at);
        }
        let gone: BTreeSet<ledger::Session> = self
            .missing
            .iter()
            .filter(|(_, since)| at.duration_since(**since) >= GONE)
            .map(|(session, _)| *session)
            .collect();
        self.missing.retain(|session, _| !gone.contains(session));
        self.heard.retain(|session| live.contains(session) || self.missing.contains_key(session));
        if gone.is_empty() {
            return false;
        }
        self.keep_read(|kept| kept.colours.retain(|session, _| !gone.contains(session)));
        true
    }

    /// Makes `change` to the order as its file has it now, rather than as
    /// this luneta last took it up, and keeps that for every luneta: so what
    /// it does not change, as the sessions that do not run and what a luneta
    /// of another build keeps there, stays as another luneta left it, though
    /// this one has yet to hear of that, or never will, as one whose watcher
    /// failed. A file that does not read as an order is written over. One
    /// that fails a write is given up, and the order is this luneta's from
    /// then on, rather than undone by the next `reload`.
    fn keep(&mut self, change: impl FnOnce(&mut Kept)) {
        self.reload();
        self.write_kept(change);
    }

    /// Makes `change` as `keep` does, to a file that reads as an order. One
    /// that does not, it leaves as it is, for you to mend or a move to write
    /// over, and the change is this luneta's alone until then: luneta keeps
    /// a colour without being asked, which is not to undo what the file has.
    fn keep_read(&mut self, change: impl FnOnce(&mut Kept)) {
        self.reload();
        if self.unread.is_some() {
            change(&mut self.kept);
            return;
        }
        self.write_kept(change);
    }

    /// Makes `change` to the order as this luneta last took it up, and
    /// writes that to the file, as `keep` says.
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

/// Where `item` goes in `order`: where it stands there, or after all of it.
fn rank<T: PartialEq<U>, U: ?Sized>(order: &[T], item: &U) -> usize {
    order.iter().position(|it| it == item).unwrap_or(usize::MAX)
}

/// `running`, the sessions that run in their order now, with each of `kept`
/// that does not run after the one it came after there, or first where none
/// came before it.
fn merged(kept: &[String], running: &[String]) -> Vec<String> {
    let runs = |name: &&String| running.contains(name);
    let mut merged: Vec<String> = kept.iter().take_while(|name| !runs(name)).cloned().collect();
    for name in running {
        merged.push(name.clone());
        if let Some(at) = kept.iter().position(|it| it == name) {
            merged.extend(kept[at + 1..].iter().take_while(|name| !runs(name)).cloned());
        }
    }
    merged
}

/// Gives each of `running` that `colours` has none for, in the order
/// `running` goes, the first colour that the fewest hold: of the sessions
/// that run, and of those `heard` that the daemon leaves out since. A colour
/// kept of a session never heard of, as one that ended while no luneta ran,
/// is held by none.
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

/// The colours kept, by session. One of a colour this build does not have,
/// as of a later build's palette, or kept for what names no session, as a
/// person may spell it, reads as none, and its session takes one of this
/// build's, rather than the whole file not reading.
fn known<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<ledger::Session, Colour>, D::Error> {
    let kept = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    let known = kept.into_iter().filter_map(|(session, colour)| {
        Some((ledger::Session::try_from(session).ok()?, serde_json::from_value(colour).ok()?))
    });
    Ok(known.collect())
}

/// Writes `kept` to `file`, first under a name of this luneta's own beside
/// it. Says why it could not.
fn write(file: &Path, kept: &Kept) -> Result<(), String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", file.display());
    let mut text = serde_json::to_string_pretty(kept).map_err(|error| failed(&error))?;
    text.push('\n');
    let unfinished = file.with_file_name(format!("order.{}", state::unfinished()));
    let written = fs::write(&unfinished, text).and_then(|()| fs::rename(&unfinished, file));
    if written.is_err() {
        // What is left of it, if anything, is no order either way.
        let _ = fs::remove_file(&unfinished);
    }
    written.map_err(|error| failed(&error))
}

/// Tells luneta's loop each time the order is kept, by another luneta or by
/// this one, for as long as the watcher it returns is kept.
pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Order)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// A session of the tests', whose socket was made `secs` after the epoch,
    /// and whose inode is `secs` too.
    fn session(secs: u64) -> ledger::Session {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        ledger::Session::new(started, NonZeroU64::new(secs).unwrap()).unwrap()
    }

    /// The colours `order` gives `running`, kept for every luneta.
    fn colour(order: &mut Order, running: &[ledger::Session]) -> Vec<Colour> {
        order.colour_sessions(running);
        order.colours(running)
    }

    /// The sessions `order` puts `listed` in, as they came oldest first.
    fn sorted(order: &Order, listed: &[&str]) -> Vec<String> {
        let mut sessions = names(listed);
        order.sessions(&mut sessions, String::as_str);
        sessions
    }

    /// What one luneta moves, the other takes up once it reloads, and a
    /// luneta started later starts with it. What the order does not have goes
    /// after, as it came.
    #[test]
    fn a_move_in_one_luneta_shows_in_every_other_and_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join("order");
        let mut here = Order::within(file.clone());
        let mut there = Order::within(file.clone());
        assert_eq!(sorted(&there, &["a", "b", "c"]), ["a", "b", "c"]);

        here.keep_sessions(names(&["b", "a"]));
        assert!(there.reload());
        assert!(!there.reload());
        assert_eq!(sorted(&there, &["a", "b", "c"]), ["b", "a", "c"]);
        assert_eq!(sorted(&Order::within(file), &["a", "b", "c"]), ["b", "a", "c"]);
        assert_eq!((here.lost(), here.unread(), there.lost(), there.unread()), (None, None, None, None));
    }

    /// A move is made to the order as its file has it: a luneta that has not
    /// taken up a move made elsewhere keeps the place of a session that no
    /// longer runs, and what a luneta of a later build keeps in the file. The
    /// sessions that run go as it says: the sidebar takes up the file first.
    #[test]
    fn a_move_keeps_what_it_did_not_move() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut here = Order::within(file.clone());
        // Another luneta puts x first, and x ends, before this one hears.
        Order::within(file.clone()).keep_sessions(names(&["x", "b", "a"]));
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, text.replacen('{', r#"{"tabs": {"a": [2, 1]},"#, 1)).unwrap();

        here.keep_sessions(names(&["a", "b"]));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept, serde_json::json!({"sessions": ["x", "a", "b"], "tabs": {"a": [2, 1]}}));
    }

    /// A session that does not run as the others move keeps its place after
    /// the one that runs it came after, or first, where none came before it,
    /// and comes back there.
    #[test]
    fn a_session_that_is_not_running_keeps_its_place() {
        let mut order = Order::default();
        order.keep_sessions(names(&["y", "a", "b", "x", "c"]));
        order.keep_sessions(names(&["c", "a", "b"]));
        let listed = ["a", "b", "c", "x", "y", "new"];
        assert_eq!(sorted(&order, &listed), ["y", "c", "a", "b", "x", "new"]);
    }

    /// A file that does not read as an order leaves the order as it was, and
    /// says why until a move writes over it. One that cannot be written
    /// leaves the order to this luneta, and says why.
    #[test]
    fn a_file_that_fails_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let mut order = Order::within(file.clone());
        order.keep_sessions(names(&["b", "a"]));
        fs::write(&file, "not JSON").unwrap();
        assert!(!order.reload());
        assert!(order.unread().is_some_and(|why| why.contains("order")));
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
        order.keep_sessions(names(&["a", "b"]));
        assert_eq!(order.unread(), None);
        assert_eq!(sorted(&Order::within(file), &["b", "a"]), ["a", "b"]);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let mut order = Order::within(blocker.join("order"));
        assert!(order.lost().is_some_and(|lost| lost.contains("not a folder")));
        order.keep_sessions(names(&["b", "a"]));
        assert_eq!(sorted(&order, &["a", "b"]), ["b", "a"]);
    }

    /// A session takes the first colour that no other has, and keeps it as
    /// sessions end and move around it. One that ends gives its own up once
    /// the daemon has not listed it for `GONE`, for the next to take. With
    /// every colour taken, the next goes round them.
    #[test]
    fn a_session_takes_the_first_colour_no_other_has_and_keeps_it() {
        use Colour::{Aqua, Blue, Green, Orange, Purple, Yellow};
        let [a, b, c, d] = [1, 2, 3, 4].map(session);
        let mut order = Order::default();
        assert_eq!(colour(&mut order, &[a, b, c]), [Blue, Yellow, Purple]);
        let start = Instant::now();
        let listed = BTreeSet::from([b, c]);
        order.listed(Some(&listed), start);
        assert!(order.listed(Some(&listed), start + GONE));
        assert_eq!(colour(&mut order, &[c, d, b]), [Purple, Blue, Yellow]);
        let started = [5, 6, 7, 8].map(session);
        let running: Vec<ledger::Session> = [b, c, d].into_iter().chain(started).collect();
        let colours = colour(&mut order, &running);
        assert_eq!(colours, [Yellow, Purple, Blue, Aqua, Orange, Green, Blue]);
    }

    /// A session the daemon leaves out of a list, as a rename made while it
    /// reads zellij's folder can, keeps its colour, for none to take, until
    /// the daemon has not listed it for `GONE`. A list that may leave out one
    /// that runs counts for none, and starts the wait again.
    #[test]
    fn a_session_left_out_of_a_list_keeps_its_colour_until_it_is_gone() {
        use Colour::{Blue, Purple, Yellow};
        let [a, b, c, d] = [1, 2, 3, 4].map(session);
        let mut order = Order::default();
        order.colour_sessions(&[a, b]);
        let start = Instant::now();
        let only_b = BTreeSet::from([b]);
        order.listed(Some(&only_b), start);
        assert!(!order.listed(None, start + GONE));
        assert!(!order.listed(Some(&only_b), start + GONE));
        assert_eq!(colour(&mut order, &[b, c]), [Yellow, Purple]);
        assert!(order.listed(Some(&BTreeSet::from([b, c])), start + GONE * 2));
        assert_eq!(colour(&mut order, &[b, c, d]), [Yellow, Purple, Blue]);
    }

    /// The colours one luneta gives, another takes up once it reloads, and a
    /// luneta started later starts with them. A file kept before there were
    /// colours reads as one with none, and a move leaves them be. A colour is
    /// kept by the session's name in the ledger, and gruvbox's.
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
        there.keep_sessions(names(&["a", "b"]));
        let mut later = Order::within(file.clone());
        assert_eq!(colour(&mut later, &[a, b]), [Colour::Yellow, Colour::Blue]);
        assert_eq!(sorted(&later, &["b", "a"]), ["a", "b"]);
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let colours = serde_json::json!({"1000000000-1": "yellow", "2000000000-2": "blue"});
        assert_eq!(kept["colours"], colours);
    }

    /// A colour of a later build's palette, or one kept for what no session
    /// is named, reads as none, and the rest of the file as it was: the
    /// session takes a colour of this build's.
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

    /// The colour kept of a session this luneta never heard the daemon list,
    /// as one that ended while no luneta ran, holds it against none: the
    /// first to start takes it, rather than the next. It goes once the daemon
    /// has not listed that session for `GONE`, as any does.
    #[test]
    fn a_colour_kept_of_a_session_never_heard_of_is_no_others() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        fs::write(&file, r#"{"colours": {"1000000000-1": "blue"}}"#).unwrap();
        let mut order = Order::within(file.clone());
        let b = session(2);
        assert_eq!(colour(&mut order, &[b]), [Colour::Blue]);
        let start = Instant::now();
        order.listed(Some(&BTreeSet::from([b])), start);
        assert!(order.listed(Some(&BTreeSet::from([b])), start + GONE));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(kept["colours"], serde_json::json!({"2000000000-2": "blue"}));
    }

    /// A colour given, or given up, leaves a file that does not read as an
    /// order as it is, for you to mend, and says why still: it is this
    /// luneta's alone, until the file reads, or a move writes over it.
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
        order.listed(Some(&BTreeSet::from([b])), start);
        assert!(order.listed(Some(&BTreeSet::from([b])), start + GONE));
        assert_eq!(fs::read_to_string(&file).unwrap(), unread);
        assert!(order.unread().is_some_and(|why| why.contains("order")));

        // Mended, the file has no colour, and b takes one there.
        fs::write(&file, r#"{"sessions": ["b", "a"]}"#).unwrap();
        assert!(order.reload());
        assert!(order.colour_sessions(&[b]));
        let text = fs::read_to_string(&file).unwrap();
        let kept: serde_json::Value = serde_json::from_str(&text).unwrap();
        let colours = serde_json::json!({"2000000000-2": "blue"});
        assert_eq!(kept, serde_json::json!({"sessions": ["b", "a"], "colours": colours}));
    }

    /// What another luneta keeps wakes this one's loop, and nothing else in
    /// the folder does, nor a read of the file.
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

        Order::within(file).keep_sessions(names(&["b", "a"]));
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Order)));
        assert!(here.reload());
    }
}
