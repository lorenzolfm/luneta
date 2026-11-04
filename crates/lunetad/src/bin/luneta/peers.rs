//! The lunetas of one machine, each in a window of its own. One holds a lock
//! for as long as it runs, and puts the notifications up for them all. Each
//! other waits for the lock, to take it once that one ends, and to put up
//! again what that one took down as it ended; and until then it tells that
//! one, on a socket of that one's, what it looks at and when its window
//! takes the focus. So no notification comes of a pane you look at in any
//! of them, and a click on one goes to the luneta whose window had the
//! focus last.
//!
//! What they say is a line each: `focused`, as a window takes the focus;
//! `looks` and the groups of the agents of the pane a luneta looks at, while
//! its window has the focus, or none; and `clicked` and the group of an
//! agent, which `luneta clicked` tells the luneta that holds the lock, and
//! which that one tells the one whose window had the focus last, to go to
//! that agent's pane. A group names an agent, as `group` says, and names
//! its notification on macOS too. And `selected` and what you selected, in
//! base64, or nothing, as you press `Alt u` in a luneta that does not hold the
//! lock: the one that does reads aloud for them all. And `dictating` and
//! `dictated`, as you begin and end dictating in one, as what plays waits on
//! the microphone, wherever it is you dictate.

use std::collections::HashMap;
use base64::Engine as _;
use std::fs::{self, File, TryLockError};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::alerts::Key;
use crate::host::Event;
use crate::machines::Machine;

/// The file the luneta that puts the notifications up holds a lock on, in
/// luneta's runtime folder.
const LOCK: &str = "alerts.lock";

/// The socket that luneta hears the others on, in the same folder.
const SOCKET: &str = "alerts.sock";

/// What each group luneta names is led by.
const GROUPS: &str = "luneta-";

/// How long a luneta that does not hold the lock waits before it tries again
/// to reach the one that does, and between its looks at whether it reaches
/// it still.
const AGAIN: Duration = Duration::from_millis(500);

/// How long a line to another luneta may take to write: one that reads
/// nothing of what it is told holds up the one that tells it no longer.
const WRITES: Duration = Duration::from_millis(200);

/// The longest line luneta reads of another: room for the groups of all the
/// agents of a pane.
const LINE_BYTES: u64 = 64 * 1024;

/// The most of a selection a luneta hands on, so that its line, in base64, is
/// under `LINE_BYTES`: half an hour of speech or so.
const SELECTED_BYTES: usize = 40 * 1024;

/// What came of the lock, or of another luneta of this machine.
pub enum Heard {
    /// This luneta holds the lock now: the file it holds it on, which lets
    /// go of it as it closes.
    Holding(File),
    /// Another luneta holds it, which this one waits for.
    Peering,
    /// What another luneta did, by the number this one, which holds the
    /// lock, gave it.
    Peer(u64, Peered),
}

/// What another luneta did, as the one that holds the lock heard it.
pub enum Peered {
    /// It came: what this one writes to it on.
    Came(UnixStream),
    /// Its window took the focus.
    Focused,
    /// It looks at the panes of these agents, in its window, which has the
    /// focus.
    Looks(Vec<Key>),
    /// You dictate in it, or no longer do.
    Dictating(bool),
    /// It went, as it ended, or as it holds the lock now.
    Gone,
}

/// What one luneta tells another.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    Focused,
    Looks(Vec<Key>),
    Clicked(Key),
    /// `Alt u`, with what is selected there, which may be nothing.
    Selected(String),
    /// You dictate there, or no longer do.
    Dictating(bool),
}

/// Encodes what is selected for a line, and back.
const SELECTION: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

impl Line {
    /// The line `text` says, or none: one of a word not known here, or of a
    /// group that names no agent.
    fn parse(text: &str) -> Option<Line> {
        let mut words = text.split_whitespace();
        let line = match words.next()? {
            "focused" => Line::Focused,
            "looks" => Line::Looks(words.by_ref().map(key_of).collect::<Option<_>>()?),
            "clicked" => Line::Clicked(key_of(words.next()?)?),
            "selected" => {
                let selected = SELECTION.decode(words.next().unwrap_or_default()).ok()?;
                Line::Selected(String::from_utf8(selected).ok()?)
            },
            "dictating" => Line::Dictating(true),
            "dictated" => Line::Dictating(false),
            _ => return None,
        };
        words.next().is_none().then_some(line)
    }

    /// Its words, and the end of the line.
    fn said(&self) -> String {
        match self {
            Line::Focused => "focused\n".to_owned(),
            Line::Looks(keys) => {
                keys.iter().fold("looks".to_owned(), |line, key| line + " " + &group(key)) + "\n"
            },
            Line::Clicked(key) => format!("clicked {}\n", group(key)),
            Line::Selected(selected) => format!("selected {}\n", SELECTION.encode(selected)),
            Line::Dictating(true) => "dictating\n".to_owned(),
            Line::Dictating(false) => "dictated\n".to_owned(),
        }
    }
}

/// The group of `key`'s agent: its pid, and the machine it runs on, where
/// that is another.
pub fn group(key: &Key) -> String {
    match &key.on {
        Machine::Local => format!("{GROUPS}{}", key.pid),
        Machine::Remote(destination) => format!("{GROUPS}{}-{destination}", key.pid),
    }
}

/// The agent `group` names, where it names one.
fn key_of(group: &str) -> Option<Key> {
    let named = group.strip_prefix(GROUPS)?;
    let (pid, on) = match named.split_once('-') {
        Some((pid, destination)) => (pid, Machine::Remote(destination.parse().ok()?)),
        None => (named, Machine::Local),
    };
    Some(Key { on, pid: pid.parse().ok()? })
}

/// luneta's runtime folder, or why there is none.
fn folder() -> Result<PathBuf, String> {
    let folder = lunetad::folder::find().ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set")?;
    let folder = lunetad::folder::make(&folder).map_err(|refusal| refusal.to_string())?;
    Ok(folder.path().to_owned())
}

/// The socket the luneta that holds the lock hears the others on, or why
/// there is none.
pub fn socket() -> Result<PathBuf, String> {
    Ok(folder()?.join(SOCKET))
}

/// Waits for the lock on a thread of its own, and hands `events` what came
/// of it: at once whether this luneta holds it, and once it does, where it
/// did not at first. Where it cannot be had, luneta puts no notification up,
/// and the sidebar says why.
pub fn wait(events: Sender<Event>) {
    thread::spawn(move || {
        let lock = folder().and_then(|folder| {
            let path = folder.join(LOCK);
            File::create(&path).map_err(|error| format!("{}: {error}", path.display()))
        });
        let held = lock.and_then(|lock| match lock.try_lock() {
            Ok(()) => Ok(Some(lock)),
            Err(TryLockError::WouldBlock) => {
                // luneta is on its way out.
                if events.send(Event::Peers(Heard::Peering)).is_err() {
                    return Ok(None);
                }
                lock.lock().map(|()| Some(lock)).map_err(|error| error.to_string())
            },
            Err(TryLockError::Error(error)) => Err(error.to_string()),
        });
        // luneta is on its way out.
        let _ = match held {
            Ok(Some(lock)) => events.send(Event::Peers(Heard::Holding(lock))),
            Ok(None) => Ok(()),
            Err(why) => events.send(Event::Notice(format!("no notifications: {why}"))),
        };
    });
}

/// Hears the other lunetas of this machine, and `luneta clicked`, on the
/// socket the luneta that holds the lock hears on, for as long as this one
/// runs, which holds it: the socket, or why it hears on none.
pub fn listening(events: &Sender<Event>) -> Result<PathBuf, String> {
    let socket = socket()?;
    listen(&socket, events.clone()).map_err(|error| format!("{}: {error}", socket.display()))?;
    Ok(socket)
}

/// Hears on `socket` each luneta that comes, as one numbered in turn, what
/// it says, and that it went, which it hands `events`.
fn listen(socket: &Path, events: Sender<Event>) -> io::Result<()> {
    // One left by a luneta that held the lock before is no one's.
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    thread::spawn(move || {
        for (number, stream) in (0_u64..).zip(listener.incoming().flatten()) {
            let Ok(writes) = stream.try_clone() else {
                continue;
            };
            let _ = writes.set_write_timeout(Some(WRITES));
            // luneta is on its way out.
            if events.send(Event::Peers(Heard::Peer(number, Peered::Came(writes)))).is_err() {
                return;
            }
            let events = events.clone();
            thread::spawn(move || hear(number, stream, &events));
        }
    });
    Ok(())
}

/// Hears what the luneta numbered `number` says on `stream`, until it goes.
fn hear(number: u64, stream: UnixStream, events: &Sender<Event>) {
    for line in lines(stream) {
        let event = match line {
            Line::Clicked(key) => Event::Clicked(key),
            Line::Selected(selected) => Event::ReadAloud(selected),
            Line::Focused => Event::Peers(Heard::Peer(number, Peered::Focused)),
            Line::Looks(keys) => Event::Peers(Heard::Peer(number, Peered::Looks(keys))),
            Line::Dictating(now) => Event::Peers(Heard::Peer(number, Peered::Dictating(now))),
        };
        // luneta is on its way out.
        if events.send(event).is_err() {
            return;
        }
    }
    let _ = events.send(Event::Peers(Heard::Peer(number, Peered::Gone)));
}

/// The lines read on `stream` until it ends, or one is too long, or cut off
/// as it ends; those that say nothing known here are left out.
fn lines(stream: UnixStream) -> impl Iterator<Item = Line> {
    let mut reader = BufReader::new(stream);
    std::iter::from_fn(move || loop {
        let mut text = String::new();
        match (&mut reader).take(LINE_BYTES).read_line(&mut text) {
            Ok(_) if text.ends_with('\n') => {
                if let Some(line) = Line::parse(&text) {
                    return Some(line);
                }
            },
            Ok(_) | Err(_) => return None,
        }
    })
}

/// Tells the luneta listening on `socket` that the notification of `group`
/// was clicked, as `luneta clicked` does, which a click on one runs on macOS.
pub fn tell(socket: &Path, group: &str) -> io::Result<()> {
    let key = key_of(group).ok_or_else(|| io::Error::other(format!("{group:?} names no agent")))?;
    UnixStream::connect(socket)?.write_all(Line::Clicked(key).said().as_bytes())
}

/// The other lunetas of this machine, as the one that holds the lock heard
/// them.
#[derive(Default)]
pub struct Peers {
    peers: HashMap<u64, Peer>,
    /// The one whose window took the focus last, where one did since this
    /// luneta's did.
    last: Option<u64>,
}

struct Peer {
    /// What this luneta tells it on.
    stream: UnixStream,
    /// The agents of the pane it looks at, while its window has the focus.
    looks: Vec<Key>,
    /// Whether you dictate in it: this goes as it goes, however it ended.
    dictating: bool,
}

impl Peers {
    /// What the luneta numbered `number` did.
    pub fn heard(&mut self, number: u64, peered: Peered) {
        match peered {
            Peered::Came(stream) => {
                self.peers.insert(number, Peer { stream, looks: Vec::new(), dictating: false });
            },
            Peered::Focused => {
                if self.peers.contains_key(&number) {
                    self.last = Some(number);
                }
            },
            Peered::Looks(looks) => {
                if let Some(peer) = self.peers.get_mut(&number) {
                    peer.looks = looks;
                }
            },
            Peered::Dictating(now) => {
                if let Some(peer) = self.peers.get_mut(&number) {
                    peer.dictating = now;
                }
            },
            Peered::Gone => self.gone(number),
        }
    }

    fn gone(&mut self, number: u64) {
        self.peers.remove(&number);
        if self.last == Some(number) {
            self.last = None;
        }
    }

    /// This luneta's window took the focus.
    pub fn here(&mut self) {
        self.last = None;
    }

    /// The agents of the panes the others look at.
    pub fn looks(&self) -> Vec<Key> {
        self.peers.values().flat_map(|peer| peer.looks.iter().cloned()).collect()
    }

    /// Whether you dictate in another.
    pub fn dictating(&self) -> bool {
        self.peers.values().any(|peer| peer.dictating)
    }

    /// Tells the luneta whose window had the focus last, where that is
    /// another, that the notification of `key`'s agent was clicked: whether
    /// it was told. One that cannot be told is gone.
    pub fn hand(&mut self, key: &Key) -> bool {
        let Some(number) = self.last else {
            return false;
        };
        let line = Line::Clicked(key.clone()).said();
        let told = (self.peers.get_mut(&number))
            .is_some_and(|peer| peer.stream.write_all(line.as_bytes()).is_ok());
        if !told {
            self.gone(number);
        }
        told
    }
}

/// What a luneta that does not hold the lock tells the one that does,
/// through a thread of its own, which reaches that one, and again whenever it
/// lost it, as when another came to hold the lock. It ends with this.
pub struct Reaching {
    told: Sender<Tell>,
    /// What it told last of where you look, to tell only what changed.
    looks: Vec<Key>,
    /// And of whether you dictate.
    dictating: bool,
}

/// What the thread that reaches the luneta that holds the lock is told.
enum Tell {
    Focus(bool),
    Looks(Vec<Key>),
    Selected(String),
    Dictating(bool),
}

impl Reaching {
    /// Reaches the luneta that hears on `socket`, as one whose window has
    /// the focus where `focused` says, and hands `events` the clicks it is
    /// told of.
    pub fn to(socket: PathBuf, focused: bool, events: Sender<Event>) -> Reaching {
        let (told, telling) = mpsc::channel();
        thread::spawn(move || reach(&socket, &telling, focused, &events));
        Reaching { told, looks: Vec::new(), dictating: false }
    }

    /// luneta's window took the focus, or let it go, as `focused` says.
    pub fn focus(&mut self, focused: bool) {
        // A thread that ended takes no more.
        let _ = self.told.send(Tell::Focus(focused));
    }

    /// You pressed `Alt u` with `selected` selected: as much of it as a line
    /// carries, with the character it ends in whole.
    pub fn selected(&self, selected: &str) {
        let end = selected.floor_char_boundary(SELECTED_BYTES);
        // A thread that ended takes no more.
        let _ = self.told.send(Tell::Selected(selected[..end].to_owned()));
    }

    /// You look at the panes of the agents of `looks`, in luneta's window,
    /// which has the focus, or at none.
    pub fn looks(&mut self, looks: &[Key]) {
        if self.looks != looks {
            self.looks = looks.to_vec();
            // A thread that ended takes no more.
            let _ = self.told.send(Tell::Looks(looks.to_vec()));
        }
    }

    /// You dictate in this luneta, or no longer do, as `dictating` says.
    pub fn dictating(&mut self, dictating: bool) {
        if self.dictating != dictating {
            self.dictating = dictating;
            // A thread that ended takes no more.
            let _ = self.told.send(Tell::Dictating(dictating));
        }
    }
}

/// Tells the luneta that hears on `socket` what `telling` says, and all of
/// it again whenever it reaches one anew, until `telling` ends; and hands
/// `events` the clicks it is told of.
fn reach(socket: &Path, telling: &Receiver<Tell>, mut focused: bool, events: &Sender<Event>) {
    let (mut looks, mut dictating) = (Vec::new(), false);
    loop {
        if let Ok(stream) = UnixStream::connect(socket)
            && !talk(&stream, telling, &mut focused, &mut looks, &mut dictating, events)
        {
            return;
        }
        match telling.recv_timeout(AGAIN) {
            Ok(Tell::Focus(now)) => focused = now,
            Ok(Tell::Looks(now)) => looks = now,
            Ok(Tell::Dictating(now)) => dictating = now,
            // The luneta it was for is gone, and its voice with it.
            Ok(Tell::Selected(_)) | Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Talks to the luneta at the other end of `stream`, which holds the lock,
/// until it goes: tells it at once what `focused`, `looks` and `dictating` say,
/// and then what `telling` says, as it comes. Whether to reach for one again,
/// as `telling` has not ended.
fn talk(
    stream: &UnixStream,
    telling: &Receiver<Tell>,
    focused: &mut bool,
    looks: &mut Vec<Key>,
    dictating: &mut bool,
    events: &Sender<Event>,
) -> bool {
    let _ = stream.set_write_timeout(Some(WRITES));
    let gone = Arc::new(AtomicBool::new(false));
    if let Ok(reads) = stream.try_clone() {
        let (gone, events) = (Arc::clone(&gone), events.clone());
        thread::spawn(move || {
            for line in lines(reads) {
                // luneta is on its way out.
                if let Line::Clicked(key) = line
                    && events.send(Event::Clicked(key)).is_err()
                {
                    break;
                }
            }
            gone.store(true, Ordering::Relaxed);
        });
    }
    let tell = |line: Line| (&*stream).write_all(line.said().as_bytes()).is_ok();
    let mut talking = tell(Line::Looks(looks.clone()))
        && (!*focused || tell(Line::Focused))
        && (!*dictating || tell(Line::Dictating(true)));
    while talking {
        match telling.recv_timeout(AGAIN) {
            Ok(Tell::Focus(now)) => {
                *focused = now;
                talking = !now || tell(Line::Focused);
            },
            Ok(Tell::Looks(now)) => {
                *looks = now;
                talking = tell(Line::Looks(looks.clone()));
            },
            Ok(Tell::Selected(selected)) => talking = tell(Line::Selected(selected)),
            Ok(Tell::Dictating(now)) => {
                *dictating = now;
                talking = tell(Line::Dictating(now));
            },
            Err(RecvTimeoutError::Timeout) => talking = !gone.load(Ordering::Relaxed),
            Err(RecvTimeoutError::Disconnected) => {
                let _ = stream.shutdown(Shutdown::Both);
                return false;
            },
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    true
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn key(pid: u32) -> Key {
        Key { on: Machine::Local, pid }
    }

    fn far(pid: u32) -> Key {
        Key { on: Machine::Remote("me@my-vps".parse().expect("a destination")), pid }
    }

    /// The group of an agent names it back: its pid, and another machine,
    /// whatever its name has in it. What names no agent, as another
    /// program's group, names none.
    #[test]
    fn an_agent_s_group_names_it() {
        assert_eq!(group(&key(41)), "luneta-41");
        assert_eq!(group(&far(7)), "luneta-7-me@my-vps");
        for key in [key(41), far(7)] {
            assert_eq!(key_of(&group(&key)), Some(key));
        }
        for named in ["luneta-x", "luneta-", "other-41", "luneta-7--x"] {
            assert_eq!(key_of(named), None, "{named}");
        }
    }

    /// Each line reads back as what it said; one of a word not known here,
    /// or of a group that names no agent, or with more than it takes, says
    /// nothing.
    #[test]
    fn a_line_reads_back_as_what_it_said() {
        let lines = [
            Line::Focused,
            Line::Looks(Vec::new()),
            Line::Looks(vec![key(41), far(7)]),
            Line::Selected(String::new()),
            Line::Selected("two words\nand a line é".to_owned()),
            Line::Dictating(true),
            Line::Dictating(false),
            Line::Clicked(far(7)),
        ];
        for line in lines {
            assert_eq!(Line::parse(&line.said()), Some(line.clone()), "{:?}", line.said());
        }
        assert_eq!(Line::Looks(Vec::new()).said(), "looks\n");
        let unsaid = ["", "hello", "looks other-41", "clicked", "clicked luneta-1 luneta-2"];
        for text in unsaid.into_iter().chain(["focused x"]) {
            assert_eq!(Line::parse(text), None, "{text:?}");
        }
    }

    /// The next of what the luneta that holds the lock heard of the others,
    /// within a few seconds, leaving out the rest of what luneta hears.
    fn next(heard: &Receiver<Event>) -> (u64, Peered) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let event = heard.recv_timeout(left).expect("the luneta holding the lock hears it");
            if let Event::Peers(Heard::Peer(number, peered)) = event {
                return (number, peered);
            }
        }
    }

    /// The next click a luneta hears of, `within` a while.
    fn clicked(heard: &Receiver<Event>, within: Duration) -> Option<Key> {
        let until = Instant::now() + within;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if let Event::Clicked(key) = heard.recv_timeout(left).ok()? {
                return Some(key);
            }
        }
    }

    /// A luneta that does not hold the lock reaches the one that does once
    /// it hears, and tells it where it looks, and that its window has the
    /// focus, and what changes of that since; it hears from it the clicks it
    /// is to go to; and it goes once it ends.
    #[test]
    fn another_luneta_tells_the_one_that_holds_the_lock_where_it_looks() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (clicks, heard_clicks) = mpsc::channel();
        let mut reaching = Reaching::to(socket.clone(), true, clicks);
        reaching.looks(&[key(41)]);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");

        let mut peers = Peers::default();
        let (number, Peered::Came(stream)) = next(&heard) else {
            panic!("the other came first");
        };
        peers.heard(number, Peered::Came(stream));
        // It may come before it takes in where it looks, and tell that then.
        while peers.looks().is_empty() || peers.last.is_none() {
            let (number, peered) = next(&heard);
            peers.heard(number, peered);
        }
        assert_eq!(peers.looks(), [key(41)]);
        assert!(peers.hand(&far(7)));
        assert_eq!(clicked(&heard_clicks, Duration::from_secs(5)), Some(far(7)));

        peers.here();
        assert!(!peers.hand(&key(41)), "this luneta's window had the focus last");
        reaching.looks(&[]);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        assert_eq!(peers.looks(), []);
        reaching.focus(true);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        assert_eq!(peers.last, Some(number), "its window has the focus again");
        drop(reaching);
        let (number, peered) = next(&heard);
        assert!(matches!(peered, Peered::Gone));
        peers.heard(number, peered);
        assert!(!peers.hand(&key(41)), "the luneta that went is told nothing");
    }

    /// Takes in what the others say until `done`, within a few seconds.
    fn hear_until(heard: &Receiver<Event>, peers: &mut Peers, done: impl Fn(&Peers) -> bool) {
        while !done(peers) {
            let (number, peered) = next(heard);
            peers.heard(number, peered);
        }
    }

    /// The luneta that holds the lock knows you dictate in another, whichever
    /// came first; hears that you stopped, and lets go once it goes, however it
    /// ended, so that no window that died keeps what plays waiting for good.
    #[test]
    fn the_luneta_that_holds_the_lock_knows_you_dictate_in_another_until_it_goes() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (clicks, _clicked) = mpsc::channel();
        let mut reaching = Reaching::to(socket.clone(), false, clicks);
        reaching.dictating(true);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");

        let mut peers = Peers::default();
        assert!(!peers.dictating());
        hear_until(&heard, &mut peers, Peers::dictating);
        reaching.dictating(false);
        hear_until(&heard, &mut peers, |peers| !peers.dictating());
        reaching.dictating(true);
        hear_until(&heard, &mut peers, Peers::dictating);
        drop(reaching);
        hear_until(&heard, &mut peers, |peers| !peers.dictating());
        assert!(peers.peers.is_empty());
    }

    /// A luneta that comes to hold the lock after another ended hears of it
    /// anew from those it reaches: it has told no one but the one that went.
    #[test]
    fn a_luneta_that_comes_to_hold_the_lock_is_told_you_dictate_in_another() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (clicks, _clicked) = mpsc::channel();
        let mut reaching = Reaching::to(socket.clone(), false, clicks);
        let first = UnixListener::bind(&socket).expect("it listens");
        let (stream, _) = first.accept().expect("the other comes");
        reaching.dictating(true);
        assert!(lines(stream).any(|line| line == Line::Dictating(true)));
        drop(first);

        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("another listens");
        let mut second = Peers::default();
        hear_until(&heard, &mut second, Peers::dictating);
    }

    /// What `luneta clicked` tells the luneta that holds the lock reaches
    /// it as the click of the agent its group names; one that names none
    /// is told nothing.
    #[test]
    fn a_click_told_on_the_socket_is_the_click_of_its_agent() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        assert!(tell(&socket, "luneta-x").is_err());
        UnixStream::connect(&socket).unwrap().write_all(b"clicked other-41\n").unwrap();
        tell(&socket, &group(&far(41))).expect("it is told");
        assert_eq!(clicked(&heard, Duration::from_secs(5)), Some(far(41)));
        assert_eq!(clicked(&heard, Duration::from_millis(300)), None);
    }
}
