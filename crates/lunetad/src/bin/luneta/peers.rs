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
use crate::aloud::NothingToRead;
use crate::host::Event;
use crate::machines::Machine;

const LOCK: &str = "alerts.lock";

const SOCKET: &str = "alerts.sock";

const GROUPS: &str = "luneta-";

const AGAIN: Duration = Duration::from_millis(500);

const WRITES: Duration = Duration::from_millis(200);

const LINE_BYTES: u64 = 64 * 1024;

const SELECTED_BYTES: usize = 40 * 1024;

pub const NO_READER: &str = "this luneta reads nothing aloud, and reaches no other that does";

pub enum Heard {
    Holding(File, Result<PathBuf, String>),
    Peering(PathBuf),
    Peer(u64, Peered),
}

pub enum Peered {
    Came(UnixStream),
    Unfocuses,
    Focused,
    Unfocused,
    Looks(Vec<Key>),
    Dictating(bool),
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToHolder {
    Unfocuses,
    Focused,
    Unfocused,
    Looks(Vec<Key>),
    Clicked(Key),
    Selected(String),
    Stopped,
    Dictating(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FromHolder {
    Clicked(Key),
    NothingToRead,
}

const SELECTION: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

impl ToHolder {
    fn parse(text: &str) -> Option<ToHolder> {
        let mut words = text.split_whitespace();
        let line = match words.next()? {
            "unfocuses" => ToHolder::Unfocuses,
            "focused" => ToHolder::Focused,
            "unfocused" => ToHolder::Unfocused,
            "looks" => ToHolder::Looks(words.by_ref().map(key_of).collect::<Option<_>>()?),
            "clicked" => ToHolder::Clicked(key_of(words.next()?)?),
            "selected" => {
                let selected = SELECTION.decode(words.next().unwrap_or_default()).ok()?;
                ToHolder::Selected(String::from_utf8(selected).ok()?)
            },
            "stopped" => ToHolder::Stopped,
            "dictating" => ToHolder::Dictating(true),
            "dictated" => ToHolder::Dictating(false),
            _ => return None,
        };
        words.next().is_none().then_some(line)
    }

    fn said(&self) -> String {
        match self {
            ToHolder::Unfocuses => "unfocuses\n".to_owned(),
            ToHolder::Focused => "focused\n".to_owned(),
            ToHolder::Unfocused => "unfocused\n".to_owned(),
            ToHolder::Looks(keys) => {
                keys.iter().fold("looks".to_owned(), |line, key| line + " " + &group(key)) + "\n"
            },
            ToHolder::Clicked(key) => clicked(key),
            ToHolder::Selected(selected) => format!("selected {}\n", SELECTION.encode(selected)),
            ToHolder::Stopped => "stopped\n".to_owned(),
            ToHolder::Dictating(true) => "dictating\n".to_owned(),
            ToHolder::Dictating(false) => "dictated\n".to_owned(),
        }
    }
}

impl FromHolder {
    fn parse(text: &str) -> Option<FromHolder> {
        let mut words = text.split_whitespace();
        let line = match words.next()? {
            "clicked" => FromHolder::Clicked(key_of(words.next()?)?),
            "nothing-to-read" => FromHolder::NothingToRead,
            _ => return None,
        };
        words.next().is_none().then_some(line)
    }

    fn said(&self) -> String {
        match self {
            FromHolder::Clicked(key) => clicked(key),
            FromHolder::NothingToRead => "nothing-to-read\n".to_owned(),
        }
    }
}

fn clicked(key: &Key) -> String {
    format!("clicked {}\n", group(key))
}

pub fn group(key: &Key) -> String {
    match &key.on {
        Machine::Local => format!("{GROUPS}{}", key.pid),
        Machine::Remote(destination) => format!("{GROUPS}{}-{destination}", key.pid),
    }
}

fn key_of(group: &str) -> Option<Key> {
    let named = group.strip_prefix(GROUPS)?;
    let (pid, on) = match named.split_once('-') {
        Some((pid, destination)) => (pid, Machine::Remote(destination.parse().ok()?)),
        None => (named, Machine::Local),
    };
    Some(Key { on, pid: pid.parse().ok()? })
}

fn checked(path: &Path) -> Result<PathBuf, String> {
    let folder = lunetad::folder::make(path).map_err(|refusal| refusal.to_string())?;
    Ok(folder.path().to_owned())
}

pub fn wait(events: Sender<Event>) {
    thread::spawn(move || {
        let found = lunetad::folder::find();
        let found = found.map_err(|unfound| unfound.to_string());
        if let Err(why) = found.and_then(|folder| take(&folder, &events)) {
            let why = format!("no notifications, and nothing read aloud: {why}");
            let _ = events.send(Event::Notice(why));
        }
    });
}

fn take(path: &Path, events: &Sender<Event>) -> Result<(), String> {
    let folder = checked(path)?;
    let at = folder.join(LOCK);
    let lock = File::create(&at).map_err(|error| format!("{}: {error}", at.display()))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(TryLockError::WouldBlock) => {
            if events.send(Event::Peers(Heard::Peering(folder.join(SOCKET)))).is_err() {
                return Ok(());
            }
            lock.lock().map_err(|error| error.to_string())?;
        },
        Err(TryLockError::Error(error)) => return Err(error.to_string()),
    }
    let socket = checked(path).map(|folder| folder.join(SOCKET));
    let _ = events.send(Event::Peers(Heard::Holding(lock, socket)));
    Ok(())
}

pub fn listening(socket: &Path, events: &Sender<Event>) -> Result<(), String> {
    listen(socket, events.clone()).map_err(|error| format!("{}: {error}", socket.display()))
}

fn listen(socket: &Path, events: Sender<Event>) -> io::Result<()> {
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    thread::spawn(move || {
        for (number, stream) in (0_u64..).zip(listener.incoming().flatten()) {
            let Ok(writes) = stream.try_clone() else {
                continue;
            };
            let _ = writes.set_write_timeout(Some(WRITES));
            if events.send(Event::Peers(Heard::Peer(number, Peered::Came(writes)))).is_err() {
                return;
            }
            let events = events.clone();
            thread::spawn(move || hear(number, stream, &events));
        }
    });
    Ok(())
}

fn hear(number: u64, stream: UnixStream, events: &Sender<Event>) {
    for line in lines(stream, ToHolder::parse) {
        let event = match line {
            ToHolder::Clicked(key) => Event::Clicked(key),
            ToHolder::Selected(selected) => Event::ReadAloud { from: number, selected },
            ToHolder::Stopped => Event::StopAloud,
            ToHolder::Unfocuses => Event::Peers(Heard::Peer(number, Peered::Unfocuses)),
            ToHolder::Focused => Event::Peers(Heard::Peer(number, Peered::Focused)),
            ToHolder::Unfocused => Event::Peers(Heard::Peer(number, Peered::Unfocused)),
            ToHolder::Looks(keys) => Event::Peers(Heard::Peer(number, Peered::Looks(keys))),
            ToHolder::Dictating(now) => Event::Peers(Heard::Peer(number, Peered::Dictating(now))),
        };
        if events.send(event).is_err() {
            return;
        }
    }
    let _ = events.send(Event::Peers(Heard::Peer(number, Peered::Gone)));
}

fn lines<L>(stream: UnixStream, parse: fn(&str) -> Option<L>) -> impl Iterator<Item = L> {
    let mut reader = BufReader::new(stream);
    std::iter::from_fn(move || loop {
        let mut text = String::new();
        match (&mut reader).take(LINE_BYTES).read_line(&mut text) {
            Ok(_) if text.ends_with('\n') => {
                if let Some(line) = parse(&text) {
                    return Some(line);
                }
            },
            Ok(_) | Err(_) => return None,
        }
    })
}

pub fn tell(socket: &Path, group: &str) -> io::Result<()> {
    let key = key_of(group).ok_or_else(|| io::Error::other(format!("{group:?} names no agent")))?;
    UnixStream::connect(socket)?.write_all(ToHolder::Clicked(key).said().as_bytes())
}

#[derive(Default)]
pub struct Peers {
    peers: HashMap<u64, Peer>,
    last: Option<Last>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Last {
    number: u64,
    focused: bool,
}

struct Peer {
    stream: UnixStream,
    looks: Vec<Key>,
    dictating: bool,
    unfocuses: bool,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Peers {
    pub fn heard(&mut self, number: u64, peered: Peered) {
        match peered {
            Peered::Came(stream) => {
                let peer = Peer { stream, looks: Vec::new(), dictating: false, unfocuses: false };
                let other = self.peers.insert(number, peer);
                assert!(other.is_none(), "another connection has number {number}");
            },
            Peered::Unfocuses => {
                if let Some(peer) = self.peers.get_mut(&number) {
                    peer.unfocuses = true;
                }
            },
            Peered::Focused => {
                if let Some(peer) = self.peers.get(&number) {
                    self.last = Some(Last { number, focused: peer.unfocuses });
                }
            },
            Peered::Unfocused => {
                if let Some(last) = &mut self.last
                    && last.number == number
                {
                    last.focused = false;
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
        if self.last.is_some_and(|last| last.number == number) {
            self.last = None;
        }
    }

    pub fn here(&mut self) {
        self.last = None;
    }

    pub fn focused(&self) -> bool {
        self.last.is_some_and(|last| last.focused)
    }

    pub fn looks(&self) -> Vec<Key> {
        self.peers.values().flat_map(|peer| peer.looks.iter().cloned()).collect()
    }

    pub fn dictating(&self) -> bool {
        self.peers.values().any(|peer| peer.dictating)
    }

    pub fn hand(&mut self, key: &Key) -> bool {
        let Some(Last { number, .. }) = self.last else {
            return false;
        };
        self.tell(number, &FromHolder::Clicked(key.clone()))
    }

    pub fn nothing_to_read(&mut self, number: u64) {
        self.tell(number, &FromHolder::NothingToRead);
    }

    fn tell(&mut self, number: u64, line: &FromHolder) -> bool {
        let line = line.said();
        let told = (self.peers.get_mut(&number))
            .is_some_and(|peer| peer.stream.write_all(line.as_bytes()).is_ok());
        if !told {
            self.gone(number);
        }
        told
    }
}

pub struct Reaching {
    told: Sender<Tell>,
    looks: Vec<Key>,
    dictating: bool,
}

enum Tell {
    Focus(bool),
    Looks(Vec<Key>),
    Selected(String),
    Stopped,
    Dictating(bool),
}

impl Reaching {
    pub fn to(socket: PathBuf, focused: bool, events: Sender<Event>) -> Reaching {
        let (told, telling) = mpsc::channel();
        thread::spawn(move || reach(&socket, &telling, focused, &events));
        Reaching { told, looks: Vec::new(), dictating: false }
    }

    pub fn focus(&mut self, focused: bool) {
        let _ = self.told.send(Tell::Focus(focused));
    }

    pub fn selected(&self, selected: &str) {
        let end = selected.floor_char_boundary(SELECTED_BYTES);
        let _ = self.told.send(Tell::Selected(selected[..end].to_owned()));
    }

    pub fn stopped(&self) {
        let _ = self.told.send(Tell::Stopped);
    }

    pub fn looks(&mut self, looks: &[Key]) {
        if self.looks != looks {
            self.looks = looks.to_vec();
            let _ = self.told.send(Tell::Looks(looks.to_vec()));
        }
    }

    pub fn dictating(&mut self, dictating: bool) {
        if self.dictating != dictating {
            self.dictating = dictating;
            let _ = self.told.send(Tell::Dictating(dictating));
        }
    }
}

fn unreached(events: &Sender<Event>) {
    let _ = events.send(Event::Notice(NO_READER.to_owned()));
}

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
            Ok(Tell::Selected(_) | Tell::Stopped) => unreached(events),
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

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
            for line in lines(reads, FromHolder::parse) {
                let event = match line {
                    FromHolder::Clicked(key) => Event::Clicked(key),
                    FromHolder::NothingToRead => Event::Notice(NothingToRead.to_string()),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            gone.store(true, Ordering::Relaxed);
        });
    }
    let tell = |line: ToHolder| (&*stream).write_all(line.said().as_bytes()).is_ok();
    let key = |line: ToHolder| {
        let told = tell(line);
        if !told {
            unreached(events);
        }
        told
    };
    let mut talking = tell(ToHolder::Unfocuses)
        && tell(ToHolder::Looks(looks.clone()))
        && (!*focused || tell(ToHolder::Focused))
        && (!*dictating || tell(ToHolder::Dictating(true)));
    while talking {
        match telling.recv_timeout(AGAIN) {
            Ok(Tell::Focus(now)) => {
                *focused = now;
                talking = tell(if now { ToHolder::Focused } else { ToHolder::Unfocused });
            },
            Ok(Tell::Looks(now)) => {
                *looks = now;
                talking = tell(ToHolder::Looks(looks.clone()));
            },
            Ok(Tell::Selected(selected)) => talking = key(ToHolder::Selected(selected)),
            Ok(Tell::Stopped) => talking = key(ToHolder::Stopped),
            Ok(Tell::Dictating(now)) => {
                *dictating = now;
                talking = tell(ToHolder::Dictating(now));
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
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt as _;

    use luneta_waits::{Patience, never};

    use super::*;

    fn key(pid: u32) -> Key {
        Key { on: Machine::Local, pid }
    }

    fn far(pid: u32) -> Key {
        Key { on: Machine::Remote("me@my-vps".parse().expect("a destination")), pid }
    }

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

    #[test]
    fn a_line_reads_back_as_what_it_said() {
        let lines = [
            ToHolder::Unfocuses,
            ToHolder::Focused,
            ToHolder::Unfocused,
            ToHolder::Looks(Vec::new()),
            ToHolder::Looks(vec![key(41), far(7)]),
            ToHolder::Selected(String::new()),
            ToHolder::Selected("two words\nand a line é".to_owned()),
            ToHolder::Stopped,
            ToHolder::Dictating(true),
            ToHolder::Dictating(false),
            ToHolder::Clicked(far(7)),
        ];
        for line in lines {
            assert_eq!(ToHolder::parse(&line.said()), Some(line.clone()), "{:?}", line.said());
        }
        assert_eq!(ToHolder::Looks(Vec::new()).said(), "looks\n");
        let unsaid = ["", "hello", "looks other-41", "clicked", "clicked luneta-1 luneta-2"];
        for text in unsaid.into_iter().chain(["focused x"]) {
            assert_eq!(ToHolder::parse(text), None, "{text:?}");
        }
        for line in [FromHolder::Clicked(far(7)), FromHolder::NothingToRead] {
            assert_eq!(FromHolder::parse(&line.said()), Some(line.clone()), "{:?}", line.said());
        }
        assert_eq!(FromHolder::Clicked(key(41)).said(), ToHolder::Clicked(key(41)).said());
        assert_eq!(ToHolder::parse(&FromHolder::NothingToRead.said()), None);
        for text in [ToHolder::Stopped.said().as_str(), "clicked", "nothing-to-read x"] {
            assert_eq!(FromHolder::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_line_cut_short_is_never_read() {
        let (mut holder, window) = UnixStream::pair().expect("a connection");
        holder.write_all(b"clicked luneta-41\nclicked luneta-4").expect("it writes");
        holder.shutdown(Shutdown::Both).expect("it ends");
        let read: Vec<_> = lines(window, FromHolder::parse).collect();
        assert_eq!(read, [FromHolder::Clicked(key(41))]);
    }

    fn next(heard: &Receiver<Event>) -> (u64, Peered) {
        next_within(Patience::begun(), heard)
    }

    fn next_within(patience: Patience, heard: &Receiver<Event>) -> (u64, Peered) {
        patience.heard_first("the luneta holding the lock hears it", heard, |event| match event {
            Event::Peers(Heard::Peer(number, peered)) => Some((number, peered)),
            _ => None,
        })
    }

    fn a_click(event: Event) -> Option<Key> {
        match event {
            Event::Clicked(key) => Some(key),
            _ => None,
        }
    }

    fn clicked(heard: &Receiver<Event>) -> Key {
        Patience::begun().heard_first("a click", heard, a_click)
    }

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
        hear_until(&heard, &mut peers, |peers| !peers.looks().is_empty() && peers.last.is_some());
        assert_eq!(peers.looks(), [key(41)]);
        assert!(peers.hand(&far(7)));
        assert_eq!(clicked(&heard_clicks), far(7));

        peers.here();
        assert!(!peers.hand(&key(41)), "this luneta's window had the focus last");
        reaching.looks(&[]);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        assert_eq!(peers.looks(), []);
        reaching.focus(true);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        assert_eq!(peers.last, Some(Last { number, focused: true }), "its window has it again");
        reaching.focus(false);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        assert!(!peers.focused(), "its window let the focus go");
        assert!(peers.hand(&key(41)), "its window had the focus last");
        assert_eq!(clicked(&heard_clicks), key(41));
        reaching.focus(true);
        let (number, peered) = next(&heard);
        peers.heard(number, peered);
        peers.here();
        assert!(!peers.focused(), "this luneta's window took the focus");
        drop(reaching);
        let (number, peered) = next(&heard);
        assert!(matches!(peered, Peered::Gone));
        peers.heard(number, peered);
        assert!(!peers.hand(&key(41)), "the luneta that went is told nothing");
    }

    #[test]
    fn an_older_luneta_s_window_is_not_taken_to_have_the_focus() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        let mut older = UnixStream::connect(&socket).expect("it reaches the one that listens");
        older.write_all(b"looks\nfocused\n").expect("it tells");

        let mut peers = Peers::default();
        hear_until(&heard, &mut peers, |peers| peers.last.is_some());
        assert!(!peers.focused());
        assert!(peers.hand(&key(41)), "its window had the focus last");
        assert_eq!(lines(older, FromHolder::parse).next(), Some(FromHolder::Clicked(key(41))));
    }

    fn hear_until(heard: &Receiver<Event>, peers: &mut Peers, done: impl Fn(&Peers) -> bool) {
        let patience = Patience::begun();
        while !done(peers) {
            let (number, peered) = next_within(patience, heard);
            peers.heard(number, peered);
        }
    }

    #[test]
    fn the_key_that_stops_reaches_the_luneta_that_holds_the_lock() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        let (clicks, _clicked) = mpsc::channel();
        let reaching = Reaching::to(socket, false, clicks);
        reaching.stopped();
        let stops = |event| matches!(event, Event::StopAloud).then_some(());
        let () = Patience::begun().heard_first("the key that stops", &heard, stops);
    }

    fn note(heard: &Receiver<Event>) -> String {
        Patience::begun().heard_first("it says why", heard, |event| match event {
            Event::Notice(note) => Some(note),
            _ => None,
        })
    }

    #[test]
    fn a_key_of_what_is_read_aloud_that_reaches_no_luneta_says_so() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        let reaching = Reaching::to(socket.clone(), false, events);
        reaching.stopped();
        assert_eq!(note(&heard), NO_READER);
        reaching.selected("words");
        assert_eq!(note(&heard), NO_READER);

        let listener = UnixListener::bind(&socket).expect("it listens");
        let (stream, _) = listener.accept().expect("the other comes");
        let mut said = lines(stream, ToHolder::parse);
        assert_eq!(said.next(), Some(ToHolder::Unfocuses));
        assert_eq!(said.next(), Some(ToHolder::Looks(Vec::new())));
        drop((said, listener));
        reaching.stopped();
        assert_eq!(note(&heard), NO_READER);
    }

    fn came(heard: &Receiver<Event>, peers: &mut Peers) -> u64 {
        let patience = Patience::begun();
        loop {
            let (number, peered) = next_within(patience, heard);
            let came = matches!(peered, Peered::Came(_));
            peers.heard(number, peered);
            if came {
                return number;
            }
        }
    }

    fn read_aloud(heard: &Receiver<Event>) -> (u64, String) {
        Patience::begun().heard_first("what to read aloud", heard, |event| match event {
            Event::ReadAloud { from, selected } => Some((from, selected)),
            _ => None,
        })
    }

    #[test]
    fn the_luneta_that_holds_the_lock_tells_only_the_one_that_asked_it_has_nothing_to_read() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        let mut peers = Peers::default();
        let (asker_events, asker_heard) = mpsc::channel();
        let asker = Reaching::to(socket.clone(), false, asker_events);
        let asker_number = came(&heard, &mut peers);
        let (other_events, other_heard) = mpsc::channel();
        let _other = Reaching::to(socket, false, other_events);
        assert_ne!(came(&heard, &mut peers), asker_number);

        asker.selected("");
        assert_eq!(read_aloud(&heard), (asker_number, String::new()));
        peers.nothing_to_read(asker_number);
        assert_eq!(note(&asker_heard), NothingToRead.to_string());
        never("the other luneta is told something", || other_heard.try_recv().ok().map(drop));
    }

    fn relay(at: &Path, holder: PathBuf, stopped: Receiver<()>) {
        let listener = UnixListener::bind(at).expect("it listens");
        thread::spawn(move || {
            for mut window in listener.incoming().flatten() {
                let mut to_holder = UnixStream::connect(&holder).expect("it reaches the holder");
                let mut from_window = window.try_clone().expect("a clone");
                let mut up = to_holder.try_clone().expect("a clone");
                thread::spawn(move || io::copy(&mut from_window, &mut up));
                let _ = stopped.recv();
                thread::spawn(move || {
                    let _ = io::copy(&mut to_holder, &mut window);
                    let _ = window.shutdown(Shutdown::Both);
                });
            }
        });
    }

    #[test]
    fn a_luneta_that_stopped_reading_until_a_line_to_it_failed_is_heard_again() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        let (stop, stopped) = mpsc::channel();
        let relayed = folder.path().join("relayed.sock");
        relay(&relayed, socket, stopped);
        let (clicks, _clicked) = mpsc::channel();
        let mut reaching = Reaching::to(relayed, true, clicks);
        reaching.looks(&[key(41)]);
        reaching.dictating(true);

        let mut peers = Peers::default();
        let known = |peers: &Peers| {
            peers.focused() && peers.looks() == [key(41)] && peers.dictating()
        };
        hear_until(&heard, &mut peers, known);
        let Some(Last { number: first, .. }) = peers.last else {
            panic!("its window has the focus");
        };
        let failed = (0..1_000_000).any(|_| !peers.hand(&key(41)));
        assert!(failed, "a line to the luneta that reads nothing fails");
        assert!(!known(&peers), "the luneta it failed to tell is forgotten");
        drop(stop);
        hear_until(&heard, &mut peers, |peers| {
            known(peers) && peers.last.is_some_and(|last| last.number != first)
        });
    }

    fn socket_after_waiting(path: &Path, change: impl FnOnce()) -> Result<PathBuf, String> {
        let another = File::create(checked(path).expect("a folder").join(LOCK)).expect("a lock");
        another.try_lock().expect("another holds it");
        let (events, heard) = mpsc::channel();
        let taking = path.to_owned();
        thread::spawn(move || take(&taking, &events));
        let waits = luneta_waits::heard("it finds the lock held", &heard);
        assert!(matches!(waits, Event::Peers(Heard::Peering(_))));
        change();
        drop(another);
        match luneta_waits::heard("it comes to hold the lock", &heard) {
            Event::Peers(Heard::Holding(_, socket)) => socket,
            _ => panic!("it holds the lock"),
        }
    }

    #[test]
    fn a_luneta_that_waited_for_the_lock_looks_at_the_folder_again() {
        let root = tempfile::tempdir().expect("a folder");
        let folder = root.path().join("luneta");

        let gone = || fs::remove_dir_all(&folder).expect("it goes");
        assert_eq!(socket_after_waiting(&folder, gone), Ok(folder.join(SOCKET)));
        assert!(folder.is_dir(), "it makes the folder again");

        let open = || {
            fs::set_permissions(&folder, Permissions::from_mode(0o755)).expect("it opens");
        };
        let refused = format!("{} is open to other users", folder.display());
        assert_eq!(socket_after_waiting(&folder, open), Err(refused));
    }

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

    #[test]
    fn a_luneta_that_comes_to_hold_the_lock_is_told_you_dictate_in_another() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (clicks, _clicked) = mpsc::channel();
        let mut reaching = Reaching::to(socket.clone(), false, clicks);
        let first = UnixListener::bind(&socket).expect("it listens");
        let (stream, _) = first.accept().expect("the other comes");
        reaching.dictating(true);
        assert!(lines(stream, ToHolder::parse).any(|line| line == ToHolder::Dictating(true)));
        drop(first);

        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("another listens");
        let mut second = Peers::default();
        hear_until(&heard, &mut second, Peers::dictating);
    }

    #[test]
    fn a_click_told_on_the_socket_is_the_click_of_its_agent() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join(SOCKET);
        let (events, heard) = mpsc::channel();
        listen(&socket, events).expect("it listens");
        assert!(tell(&socket, "luneta-x").is_err());
        UnixStream::connect(&socket).unwrap().write_all(b"clicked other-41\n").unwrap();
        tell(&socket, &group(&far(41))).expect("it is told");
        assert_eq!(clicked(&heard), far(41));
        never("a second click is heard", || heard.try_recv().ok().and_then(a_click));
    }
}
