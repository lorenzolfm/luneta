//! The desktop that the notifications of `alerts` go up on: freedesktop's
//! notification server, over D-Bus, and on macOS the Notification Center,
//! through `terminal-notifier`, as nix-darwin puts it on the `PATH`. A click
//! on one comes back to luneta as `Event::Clicked`.
//!
//! One luneta of a machine puts them up, which holds a lock for as long as
//! it runs, and the others have their say through it, as `peers` says:
//! another's would be the same again. And none puts any up while it runs
//! over ssh: the desktop of the machine it runs on is not the one you sit
//! at.

use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::{self as channel, UnboundedReceiver, UnboundedSender};

use lunetad::issues::{Filed, Unfiled};
use lunetad::links::Link;

use crate::alerts::{Alert, Key, Sound};
use crate::host::Event;
use crate::peers::{self, Heard, Peers, Reaching, NO_READER};

/// Whether luneta runs over ssh, as sshd says in the variables it sets: the
/// machine it runs on is not the one you sit at.
pub fn elsewhere() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// What luneta has the desktop do.
#[derive(Debug)]
enum Order {
    Post(Alert),
    Close(Key),
    /// Put this up, whether or not this luneta holds the lock.
    Tell(Told),
    /// Put the notifications up from now on, as this luneta holds the lock:
    /// with the socket it hears on, where it does, which a click on one tells
    /// on macOS.
    Hold(Option<PathBuf>),
}

/// A notification of what came of an issue the issue box sent. The luneta
/// that sent it puts it up, whether or not it holds the lock: no other heard
/// of the issue, to put it up again. It goes up whether or not a window of
/// luneta's has the focus, as you asked for it, and the note that says the
/// same is easy to miss while you type in zellij.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Told {
    pub title: String,
    pub body: String,
    pub came: Came,
}

/// What came of an issue the issue box sent, which says what its
/// notification sounds and what a click on it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Came {
    /// It was filed: silent, and a click opens its page.
    Filed(Link),
    /// None was, or one may be: it sounds, is urgent, and a click brings
    /// luneta's window forward, where the issue box has your words again.
    Unfiled,
}

impl Came {
    /// What its notification sounds as it goes up.
    fn sound(&self) -> Option<Sound> {
        match self {
            Came::Filed(_) => None,
            Came::Unfiled => Some(Sound::Bell),
        }
    }
}

impl Told {
    /// What came of the issue sent to be filed in `repository`, as `filed`
    /// says: its number and title, which a click opens; or why gh filed none,
    /// or may have, which sounds.
    pub fn issue(repository: &str, filed: &Result<Filed, Unfiled>) -> Told {
        let unfiled = |title: String, why: &str, then: &str| Told {
            title,
            body: format!("{why}; {then}"),
            came: Came::Unfiled,
        };
        match filed {
            Ok(filed) => Told {
                title: format!("Filed #{} in {repository}", filed.number()),
                body: filed.title().to_owned(),
                came: Came::Filed(filed.link().clone()),
            },
            Err(Unfiled::Refused(why)) => {
                unfiled(format!("No issue filed in {repository}"), why, "i has your words again")
            },
            Err(Unfiled::InDoubt(why)) => unfiled(
                format!("An issue in {repository} may be filed"),
                why,
                "i has your words, look first",
            ),
        }
    }
}

/// The desktop, through a thread of its own, which takes luneta's orders in
/// turn: a notification taken down after it was put up goes, however long
/// putting it up took.
pub struct Desktop {
    /// What takes the orders to the thread, and the thread: none where
    /// luneta runs over ssh, nor once it ends.
    serving: Option<(UnboundedSender<Order>, JoinHandle<()>)>,
    /// What this luneta is of those of its machine.
    role: Role,
    /// Where luneta hears what its threads say.
    events: Sender<Event>,
}

/// What a luneta is of those of its machine.
enum Role {
    /// It does not know yet, and never will where the lock cannot be had.
    Undecided,
    /// It runs over ssh, and is none of them: the machine it runs on is not
    /// the one you sit at.
    Elsewhere,
    /// Another holds the lock: this one tells it where it looks.
    Peer(Reaching),
    /// It holds the lock, and puts the notifications up: with the socket it
    /// hears the others on, where it hears on one, and what they said.
    Holder { _lock: File, socket: Option<PathBuf>, peers: Peers },
}

/// Why the keys of what is read aloud do nothing, in a luneta over ssh.
const NO_SPEAKERS: &str = "this luneta reads nothing aloud: it has no speakers of yours";

/// Which luneta the keys of what is read aloud are for.
pub enum Reader<'a> {
    /// This one.
    This,
    /// Another of its machine, which this one tells of them, or says it could
    /// not.
    Another(&'a Reaching),
    /// None that this one can tell of them, and why, in words.
    Nobody(&'static str),
}

impl Desktop {
    /// The desktop of the machine luneta runs on, which hands `events` the
    /// clicks on its notifications; or none, where luneta runs over ssh.
    pub fn start(events: &Sender<Event>) -> Desktop {
        let events = events.clone();
        if elsewhere() {
            return Desktop { serving: None, role: Role::Elsewhere, events };
        }
        let (orders, taken) = channel::unbounded_channel();
        let serving = events.clone();
        let worker = thread::spawn(move || serve(taken, &serving));
        peers::wait(events.clone());
        Desktop { serving: Some((orders, worker)), role: Role::Undecided, events }
    }

    pub fn post(&self, alert: Alert) {
        self.order(Order::Post(alert));
    }

    pub fn close(&self, key: Key) {
        self.order(Order::Close(key));
    }

    /// Puts `told` up, whether or not this luneta holds the lock. A click on
    /// it comes back as `Event::IssueClicked`, on Linux: on macOS, the
    /// Notification Center does what it says on its own.
    pub fn tell(&self, told: Told) {
        self.order(Order::Tell(told));
    }

    fn order(&self, order: Order) {
        if let Some((orders, _)) = &self.serving {
            // A desktop that failed said so, and takes no more.
            let _ = orders.send(order);
        }
    }

    /// What came of the lock, or of another luneta of this machine, whose
    /// window has the focus as `focused` says: whether this one has come to
    /// hold the lock, and so is to put up again what another put up, which
    /// that one took down as it ended.
    pub fn heard(&mut self, heard: Heard, focused: bool) -> bool {
        match heard {
            Heard::Holding(lock, socket) => {
                // It reaches for no other now.
                self.role = Role::Undecided;
                let socket = socket
                    .and_then(|socket| peers::listening(&socket, &self.events).map(|()| socket));
                let socket = match socket {
                    Ok(socket) => Some(socket),
                    Err(why) => {
                        // luneta is on its way out.
                        let _ = self.events.send(Event::Notice(format!(
                            "a click on a notification goes nowhere: {why}"
                        )));
                        None
                    },
                };
                self.order(Order::Hold(socket.clone()));
                self.role = Role::Holder { _lock: lock, socket, peers: Peers::default() };
                true
            },
            Heard::Peering(socket) => {
                if matches!(self.role, Role::Undecided) {
                    self.role = Role::Peer(Reaching::to(socket, focused, self.events.clone()));
                }
                false
            },
            Heard::Peer(number, peered) => {
                if let Role::Holder { peers, .. } = &mut self.role {
                    peers.heard(number, peered);
                }
                false
            },
        }
    }

    /// luneta's window took the focus, or let it go, as `focused` says.
    pub fn focus(&mut self, focused: bool) {
        match &mut self.role {
            Role::Peer(reaching) => reaching.focus(focused),
            Role::Holder { peers, .. } if focused => peers.here(),
            Role::Holder { .. } | Role::Undecided | Role::Elsewhere => {},
        }
    }

    /// Whether this luneta is the one of its machine that reads aloud: the one
    /// that holds the lock, which none over ssh ever does.
    pub fn reads_aloud(&self) -> bool {
        matches!(self.reader(), Reader::This)
    }

    /// Which luneta the keys of what is read aloud are for.
    pub fn reader(&self) -> Reader<'_> {
        match &self.role {
            Role::Holder { .. } => Reader::This,
            Role::Peer(reaching) => Reader::Another(reaching),
            Role::Undecided => Reader::Nobody(NO_READER),
            Role::Elsewhere => Reader::Nobody(NO_SPEAKERS),
        }
    }

    /// You dictate in this luneta, or no longer do, as `dictating` says, for
    /// the one of its machine that reads aloud to know, where that is another.
    pub fn dictating(&mut self, dictating: bool) {
        if let Role::Peer(reaching) = &mut self.role {
            reaching.dictating(dictating);
        }
    }

    /// Whether you dictate in another luneta of this machine, which this one
    /// reads aloud for.
    pub fn another_dictates(&self) -> bool {
        match &self.role {
            Role::Holder { peers, .. } => peers.dictating(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }

    /// You look at the panes of the agents of `keys` in luneta's window,
    /// which has the focus, or at none.
    pub fn looking(&mut self, keys: &[Key]) {
        if let Role::Peer(reaching) = &mut self.role {
            reaching.looks(keys);
        }
    }

    /// Whether another luneta's window of this machine has the focus, as the
    /// one that holds the lock heard: none that this one knows of otherwise.
    pub fn another_focused(&self) -> bool {
        match &self.role {
            Role::Holder { peers, .. } => peers.focused(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }

    /// The agents whose panes you look at in another luneta's window, which
    /// has the focus.
    pub fn looks(&self) -> Vec<Key> {
        match &self.role {
            Role::Holder { peers, .. } => peers.looks(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => Vec::new(),
        }
    }

    /// Hands a click on the notification of `key`'s agent to the luneta whose
    /// window had the focus last, where that is another, which goes to its
    /// pane: whether it did.
    pub fn hand(&mut self, key: &Key) -> bool {
        match &mut self.role {
            Role::Holder { peers, .. } => peers.hand(key),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }
}

impl Drop for Desktop {
    /// Takes down every notification this luneta put up, and waits until it
    /// has, as luneta ends; and only then lets go of the lock, so that the
    /// luneta that takes it next puts up what still wants you once these
    /// went.
    fn drop(&mut self) {
        if let Some((orders, worker)) = self.serving.take() {
            drop(orders);
            // What a thread that panicked left up stays so.
            let _ = worker.join();
        }
        // The next to hold the lock hears on a socket of the same path.
        if let Role::Holder { socket: Some(socket), .. } = &self.role {
            let _ = fs::remove_file(socket);
        }
    }
}

/// Says what went wrong with the desktop, once: the sidebar says it.
fn say(events: &Sender<Event>, said: &mut bool, why: &str) {
    if !*said {
        *said = true;
        // luneta is on its way out.
        let _ = events.send(Event::Notice(format!("no notifications: {why}")));
    }
}

#[cfg(not(target_os = "macos"))]
fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(freedesktop::serve(orders, events)),
        Err(error) => say(events, &mut false, &format!("no runtime: {error}")),
    }
}

#[cfg(target_os = "macos")]
fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
    notifier::serve(orders, events);
}

/// The notification server freedesktop names, which the desktop of a Linux
/// machine runs, as swaync, dunst and mako are.
#[cfg(not(target_os = "macos"))]
mod freedesktop {
    use std::collections::HashMap;
    use std::io;
    use std::num::NonZeroU32;
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tokio_stream::{self as stream, StreamExt as _};
    use zbus::connection::Builder;
    use zbus::names::UniqueName;
    use zbus::{Connection, Proxy, Variant};

    use super::{say, Came, Order, Told};
    use crate::alerts::{Alert, Key, Kind, Sound};
    use crate::host::Event;

    const SERVER: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

    /// How long luneta waits for the server to answer what it asks of it,
    /// as to put a notification up: one that does not answer holds up what
    /// luneta has it do next no longer, nor luneta's end.
    const ANSWER: Duration = Duration::from_secs(5);

    /// How long luneta waits, as it ends, for each notification it takes
    /// down to go: a server that does not answer keeps it from ending no
    /// longer.
    const CLOSING: Duration = Duration::from_secs(1);

    /// The sounds freedesktop names, as NixOS installs them.
    const SOUNDS: &str = "/run/current-system/sw/share/sounds/freedesktop/stereo";

    /// The action a click on a notification is.
    const CLICK: &str = "default";

    /// A notification up, by the server that put it up: its id means nothing
    /// to another, as one that started since, which gives out the same ids
    /// again.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Posted {
        by: UniqueName<'static>,
        id: NonZeroU32,
    }

    /// What the thread hears: an order of luneta's, what a server says of a
    /// notification, or that luneta ended.
    enum Heard {
        Order(Order),
        Clicked(Posted, String),
        Closed(Posted),
        Ended,
    }

    /// Takes `orders` in turn, until luneta ends, and takes down what is up
    /// then.
    pub async fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
        let mut said = false;
        let (proxy, signals) = match connect().await {
            Ok(connected) => connected,
            Err(error) => return say(events, &mut said, &why(&error)),
        };
        let orders = UnboundedReceiverStream::new(orders).map(Heard::Order);
        let mut heard = orders.chain(stream::once(Heard::Ended)).merge(signals);
        let (mut up, mut holding) = (HashMap::<Key, Posted>::new(), false);
        // The notifications of issues up, and what a click on each does.
        let mut told: Vec<(Posted, Came)> = Vec::new();
        while let Some(heard) = heard.next().await {
            match heard {
                // A click comes back here, and not by the socket.
                Heard::Order(Order::Hold(_socket)) => holding = true,
                Heard::Order(Order::Post(alert)) => {
                    if !holding {
                        continue;
                    }
                    match notify(&proxy, &alert).await {
                        Ok(posted) => {
                            // What a server that has gone since put up went
                            // with it.
                            up.retain(|_, was| was.by == posted.by);
                            up.insert(alert.key.clone(), posted);
                            if let Some(sound) = alert.sound {
                                play(sound);
                            }
                        },
                        Err(error) => say(events, &mut said, &why(&error)),
                    }
                },
                Heard::Order(Order::Tell(telling)) => match tell(&proxy, &telling).await {
                    Ok(posted) => {
                        told.retain(|(was, _)| was.by == posted.by);
                        if let Some(sound) = telling.came.sound() {
                            play(sound);
                        }
                        told.push((posted, telling.came));
                    },
                    Err(error) => say(events, &mut said, &why(&error)),
                },
                Heard::Order(Order::Close(key)) => {
                    if let Some(posted) = up.remove(&key) {
                        // One the server has let go of, or a server that has
                        // gone, is gone.
                        let _ = close(proxy.connection(), &posted).await;
                    }
                },
                Heard::Clicked(posted, action) => {
                    if action != CLICK {
                        continue;
                    }
                    let clicked = up.iter().find(|(_, up)| **up == posted).map(|(key, _)| key);
                    // luneta is on its way out where these fail.
                    if let Some(key) = clicked {
                        let _ = events.send(Event::Clicked(key.clone()));
                    } else if let Some(at) = told.iter().position(|(up, _)| *up == posted) {
                        let _ = events.send(Event::IssueClicked(told.swap_remove(at).1));
                    }
                },
                Heard::Closed(posted) => {
                    up.retain(|_, up| *up != posted);
                    told.retain(|(up, _)| *up != posted);
                },
                Heard::Ended => break,
            }
        }
        // A click on one left up would go nowhere once luneta ends.
        let told = told.into_iter().map(|(posted, _)| posted);
        for posted in up.into_values().chain(told) {
            let _ = tokio::time::timeout(CLOSING, close(proxy.connection(), &posted)).await;
        }
    }

    type Signals = std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Heard> + Send>>;

    /// The server, and what it says of clicks and of what it closed, in the
    /// order it says them: one stream, as a click is said before the close
    /// it makes, and would find nothing to go to after it.
    async fn connect() -> zbus::Result<(Proxy<'static>, Signals)> {
        let connection = Builder::session().method_timeout(ANSWER).build().await?;
        let proxy = Proxy::new(&connection, SERVER, PATH, SERVER).await?;
        let signals = proxy.receive_all_signals().await?.filter_map(|signal| {
            let (header, body) = (signal.header(), signal.body());
            let by = header.sender()?.to_owned();
            match header.member()?.as_str() {
                "ActionInvoked" => {
                    let (id, action) = body.deserialize::<(u32, String)>().ok()?;
                    Some(Heard::Clicked(Posted { by, id: NonZeroU32::new(id)? }, action))
                },
                "NotificationClosed" => {
                    let (id, _why) = body.deserialize::<(u32, u32)>().ok()?;
                    Some(Heard::Closed(Posted { by, id: NonZeroU32::new(id)? }))
                },
                _ => None,
            }
        });
        Ok((proxy, Box::pin(signals)))
    }

    /// Puts `alert` up: the server that did, and the id it gave it. It goes
    /// up in the place of none, as luneta takes down what was up of its
    /// agent before it puts up another.
    async fn notify(proxy: &Proxy<'_>, alert: &Alert) -> zbus::Result<Posted> {
        let urgency: u8 = match alert.kind {
            Kind::Done => 1,
            Kind::Asked | Kind::Plan | Kind::Permission | Kind::Wants => 2,
        };
        post(proxy, &alert.title, &alert.body, "Go to the pane", urgency).await
    }

    /// Puts `told` up: the server that did, and the id it gave it.
    async fn tell(proxy: &Proxy<'_>, told: &Told) -> zbus::Result<Posted> {
        let (click, urgency) = match told.came {
            Came::Filed(_) => ("Open the issue", 1),
            Came::Unfiled => ("Go to luneta", 2),
        };
        post(proxy, &told.title, &told.body, click, urgency).await
    }

    /// Puts up a notification of `title` and `body`, of `urgency` as
    /// freedesktop counts it, which a click on does what `click` says.
    async fn post(
        proxy: &Proxy<'_>,
        title: &str,
        body: &str,
        click: &str,
        urgency: u8,
    ) -> zbus::Result<Posted> {
        let hints = HashMap::from([("urgency", Variant::from(urgency))]);
        let icon = icon();
        let body = escaped(body);
        let actions = [CLICK, click];
        let asked = ("luneta", 0_u32, icon.as_str(), title, body.as_str());
        let told = (asked.0, asked.1, asked.2, asked.3, asked.4, &actions[..], hints, -1_i32);
        let answer = proxy.call_method("Notify", &told).await?;
        let unsaid = |what: &str| zbus::Error::Failure(format!("the server's answer {what}"));
        let by = answer.header().sender().ok_or_else(|| unsaid("names no sender"))?.to_owned();
        let id = answer.body().deserialize::<u32>()?;
        let id = NonZeroU32::new(id).ok_or_else(|| unsaid("gives the notification no id"))?;
        Ok(Posted { by, id })
    }

    /// What went wrong, in words: a server that did not answer in time says
    /// so.
    fn why(error: &zbus::Error) -> String {
        match error {
            zbus::Error::InputOutput(error) if error.kind() == io::ErrorKind::TimedOut => {
                format!("the notification server did not answer in {}s", ANSWER.as_secs())
            },
            error => error.to_string(),
        }
    }

    /// Takes `posted` down: through the server that put it up, and no other,
    /// so that one that started since takes down none of its own of that id.
    async fn close(connection: &Connection, posted: &Posted) -> zbus::Result<()> {
        let (to, id) = (Some(&posted.by), (posted.id.get(),));
        connection.call_method(to, PATH, Some(SERVER), "CloseNotification", &id).await?;
        Ok(())
    }

    /// Claude's mark, where the dotfiles keep it for notifications, or none.
    fn icon() -> String {
        let home = std::env::var_os("HOME").unwrap_or_default();
        let icon = Path::new(&home).join(".local/share/icons/notifications/claude.png");
        if icon.is_file() { icon.to_string_lossy().into_owned() } else { String::new() }
    }

    /// `text` as a body that can hold markup says it: what a server reads as
    /// markup is escaped.
    fn escaped(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    /// Plays `sound`, where `pw-play` can.
    fn play(sound: Sound) {
        let name = match sound {
            Sound::Question => "window-question",
            Sound::Bell => "bell",
        };
        let file = Path::new(SOUNDS).join(format!("{name}.oga"));
        if !file.is_file() {
            return;
        }
        let mut play = tokio::process::Command::new("pw-play");
        play.arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // tokio waits for it once it ends; one that does not start plays
        // nothing.
        let _ = play.spawn();
    }
}

/// Brings the window luneta runs in forward, as a click on a notification
/// does: Hyprland's window of luneta's terminal, by the title luneta gave
/// it, `title`. macOS's `terminal-notifier` brings the terminal forward on
/// its own.
pub fn raise(title: Option<&str>) {
    if cfg!(target_os = "macos") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let Some(title) = title.map(str::to_owned) else {
        return;
    };
    thread::spawn(move || {
        let clients = std::process::Command::new("hyprctl").args(["clients", "-j"]).output();
        let Ok(clients) = clients else {
            return;
        };
        let clients: Vec<serde_json::Value> =
            serde_json::from_slice(&clients.stdout).unwrap_or_default();
        let window = clients.iter().find(|client| client["title"].as_str() == Some(&title));
        let Some(address) = window.and_then(|window| window["address"].as_str()) else {
            return;
        };
        let at = format!("address:{address}");
        // A window that went meanwhile is not brought forward.
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &at])
            .stdout(std::process::Stdio::null())
            .status();
    });
}

/// The Notification Center of macOS, through `terminal-notifier`, which puts
/// a notification up and ends: a click on it has the Notification Center
/// start it again, which runs the command it was given then, as `sh` does.
/// That is luneta's own, which tells the luneta that puts notifications up
/// of the click, through the socket it hears the others on, as `peers` says.
/// A notification goes by the group `peers` names its agent by, which the
/// click names: one that a luneta left up as it was killed goes to that
/// agent's pane too, and goes once the luneta that puts them up now takes
/// that agent's down.
#[cfg(any(target_os = "macos", test))]
mod notifier {
    #[cfg(target_os = "macos")]
    use std::collections::HashSet;
    use std::path::Path;
    #[cfg(target_os = "macos")]
    use std::path::PathBuf;
    #[cfg(target_os = "macos")]
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    use std::sync::mpsc::Sender;

    use super::{Came, Told};
    use crate::alerts::{Alert, Sound};
    #[cfg(target_os = "macos")]
    use crate::alerts::Key;
    #[cfg(target_os = "macos")]
    use crate::host::Event;
    #[cfg(target_os = "macos")]
    use crate::peers::group;

    /// The terminal a click brings forward: Ghostty.
    const TERMINAL: &str = "com.mitchellh.ghostty";

    /// The arguments that put `alert` up as the group `group`, in the place
    /// of the one of the group up still, if any, and run `clicked` in `sh` on
    /// a click, where there is one to run.
    pub fn posting(alert: &Alert, group: &str, clicked: Option<&str>) -> Vec<String> {
        let mut args = vec![
            ("-group", group.to_owned()),
            ("-title", guarded(&alert.title)),
            ("-message", guarded(&alert.body)),
            ("-activate", TERMINAL.to_owned()),
        ];
        args.extend(alert.sound.map(|sound| ("-sound", sounded(sound).to_owned())));
        args.extend(clicked.map(|clicked| ("-execute", clicked.to_owned())));
        args.into_iter().flat_map(|(flag, value)| [flag.to_owned(), value]).collect()
    }

    /// The arguments that put `told` up, in no group, for each issue's to
    /// stay up: a click opens the issue's page, or brings the terminal
    /// forward, with no luneta to tell, so one left up as luneta ends still
    /// does.
    pub fn telling(told: &Told) -> Vec<String> {
        let mut args = vec![("-title", guarded(&told.title)), ("-message", guarded(&told.body))];
        args.push(match &told.came {
            Came::Filed(link) => ("-open", link.url()),
            Came::Unfiled => ("-activate", TERMINAL.to_owned()),
        });
        args.extend(told.came.sound().map(|sound| ("-sound", sounded(sound).to_owned())));
        args.into_iter().flat_map(|(flag, value)| [flag.to_owned(), value]).collect()
    }

    /// The sound of macOS that `sound` is.
    fn sounded(sound: Sound) -> &'static str {
        match sound {
            Sound::Question => "Glass",
            Sound::Bell => "Ping",
        }
    }

    /// `text` as terminal-notifier takes it for a value, whatever it starts
    /// with: macOS reads its arguments as defaults, which take a word led by
    /// a `-` for an option, and one led by a `[`, a `(`, a `{`, a `<` or a
    /// quote for a list or another kind than text, and terminal-notifier
    /// reads none of those. A word led by a `\` is text, and terminal-notifier
    /// takes that `\` off.
    fn guarded(text: &str) -> String {
        format!("\\{text}")
    }

    /// The command a click runs, in `sh`: `luneta`, which tells the luneta
    /// listening on `socket` that `group` was clicked.
    pub fn clicked(luneta: &Path, socket: &Path, group: &str) -> String {
        let quoted = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
        let (luneta, socket) = (luneta.to_string_lossy(), socket.to_string_lossy());
        format!("{} clicked --socket {} {}", quoted(&luneta), quoted(&socket), quoted(group))
    }

    /// Runs terminal-notifier with `args`: whether it ran well, or why not,
    /// in the first line it says, as that notifications are turned off for
    /// it in the system's settings.
    #[cfg(target_os = "macos")]
    fn run(args: &[String]) -> Result<(), String> {
        let mut notifier = Command::new("terminal-notifier");
        notifier.args(args).stdin(Stdio::null());
        match notifier.output() {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let said = String::from_utf8_lossy(&output.stderr);
                match said.lines().next().map(str::trim).filter(|line| !line.is_empty()) {
                    Some(line) => Err(format!("terminal-notifier: {line}")),
                    None => Err(format!("terminal-notifier {}", output.status)),
                }
            },
            Err(error) => Err(format!("terminal-notifier: {error}")),
        }
    }

    /// Takes the group of `key`'s agent down, whoever put it up.
    #[cfg(target_os = "macos")]
    fn remove(key: &Key) {
        // One clicked, or gone, is gone.
        let _ = run(&["-remove".to_owned(), group(key)]);
    }

    /// Takes `orders` in turn, until luneta ends, and takes down what is up
    /// then.
    #[cfg(target_os = "macos")]
    pub(super) fn serve(
        mut orders: tokio::sync::mpsc::UnboundedReceiver<super::Order>,
        events: &Sender<Event>,
    ) {
        use super::{say, Order};

        let mut said = false;
        // The socket a click is told on, once this luneta holds the lock,
        // where there is one.
        let mut held: Option<Option<PathBuf>> = None;
        // The agents whose notifications this luneta put up, to take down as
        // it ends.
        let mut posted: HashSet<Key> = HashSet::new();
        let luneta = std::env::current_exe();
        while let Some(order) = orders.blocking_recv() {
            match order {
                Order::Hold(socket) => held = Some(socket),
                Order::Post(alert) => {
                    let Some(socket) = &held else {
                        continue;
                    };
                    let group = group(&alert.key);
                    let clicked = match (socket, &luneta) {
                        (Some(at), Ok(luneta)) => Some(clicked(luneta, at, &group)),
                        (None, _) | (_, Err(_)) => None,
                    };
                    posted.insert(alert.key.clone());
                    if let Err(why) = run(&posting(&alert, &group, clicked.as_deref())) {
                        say(events, &mut said, &why);
                    }
                },
                Order::Tell(told) => {
                    if let Err(why) = run(&telling(&told)) {
                        say(events, &mut said, &why);
                    }
                },
                // The luneta that puts them up takes them down, and with
                // them what another left up of the agent.
                Order::Close(key) => {
                    if held.is_some() {
                        posted.remove(&key);
                        remove(&key);
                    }
                },
            }
        }
        for key in &posted {
            remove(key);
        }
    }

    #[cfg(test)]
    mod tests {
        use lunetad::links::Link;

        use super::*;
        use crate::alerts::{Key, Kind};
        use crate::machines::Machine;

        fn alert(kind: Kind, body: &str, sound: Option<Sound>) -> Alert {
            let (key, title) = (Key { on: Machine::Local, pid: 41 }, "fix — needs permission");
            Alert { key, kind, title: title.to_owned(), body: body.to_owned(), sound }
        }

        /// A notification goes up by its group, which a click runs luneta
        /// with, quoted for `sh`; what it says is text to macOS whatever it
        /// starts with, and it sounds as its alert says.
        #[test]
        fn a_notification_goes_up_by_its_group_and_runs_luneta_on_a_click() {
            let luneta = Path::new("/nix/x/bin/luneta");
            let clicked = clicked(luneta, Path::new("/var/it's/alerts.sock"), "luneta-41");
            let socket = r"'/var/it'\''s/alerts.sock'";
            let said = format!("'/nix/x/bin/luneta' clicked --socket {socket} 'luneta-41'");
            assert_eq!(clicked, said);
            let rm = alert(Kind::Permission, "-rf: rm", Some(Sound::Bell));
            let args = posting(&rm, "luneta-41", Some(&clicked));
            let value = |args: &[String], flag: &str| {
                let at = args.iter().position(|arg| arg == flag)?;
                Some(args[at + 1].clone())
            };
            let said = |flag| value(&args, flag).expect("the flag is there");
            assert_eq!(said("-group"), "luneta-41");
            assert_eq!(said("-message"), r"\-rf: rm");
            assert_eq!(said("-title"), r"\fix — needs permission");
            assert_eq!((said("-sound").as_str(), said("-activate").as_str()), ("Ping", TERMINAL));
            assert_eq!(said("-execute"), clicked);
            for text in ["[x] done", "(wip) fix", "{a = b;}", "<b>it</b>", "\"Done\" said", r"\n"] {
                assert_eq!(guarded(text), format!("\\{text}"));
            }
            let again = posting(&alert(Kind::Done, "All done.", None), "luneta-41", None);
            assert_eq!((value(&again, "-sound"), value(&again, "-execute")), (None, None));
        }

        /// The notification of an issue filed opens its page on a click, and
        /// one of an issue that did not go through brings the terminal
        /// forward and sounds. Neither has a group, nor a luneta to run.
        #[test]
        fn an_issue_notification_opens_its_page_or_brings_the_terminal_forward() {
            let url = "https://github.com/owner/repo/issues/362";
            let link = Link::parse(url).expect("a link");
            let filed = Told {
                title: "Filed #362 in owner/repo".to_owned(),
                body: "[wip] a red button".to_owned(),
                came: Came::Filed(link),
            };
            let pairs = |told: &Told| -> Vec<(String, String)> {
                let args = telling(told);
                args.chunks(2).map(|pair| (pair[0].clone(), pair[1].clone())).collect()
            };
            let owned = |said: &[(&str, &str)]| -> Vec<(String, String)> {
                said.iter().map(|(flag, value)| ((*flag).to_owned(), (*value).to_owned())).collect()
            };
            let said = [
                ("-title", r"\Filed #362 in owner/repo"),
                ("-message", r"\[wip] a red button"),
                ("-open", url),
            ];
            assert_eq!(pairs(&filed), owned(&said));
            let refused = Told { came: Came::Unfiled, ..filed };
            let said = [("-activate", TERMINAL), ("-sound", "Ping")];
            assert_eq!(pairs(&refused)[2..], owned(&said));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// The keys of what is read aloud are for the luneta that holds the lock,
    /// and only one over ssh says your speakers are why they do nothing: one
    /// that does not know what it is may have them right there.
    #[test]
    fn the_keys_of_what_is_read_aloud_go_to_the_reader_and_only_ssh_blames_the_speakers() {
        let folder = tempfile::tempdir().expect("a folder");
        let (events, _heard) = mpsc::channel();
        let desktop = |role| Desktop { serving: None, role, events: events.clone() };

        let lock = File::create(folder.path().join("lock")).expect("a lock");
        let holder = desktop(Role::Holder { _lock: lock, socket: None, peers: Peers::default() });
        assert!(matches!(holder.reader(), Reader::This));
        let socket = folder.path().join("nobody-hears.sock");
        let reaching = Reaching::to(socket, false, events.clone());
        assert!(matches!(desktop(Role::Peer(reaching)).reader(), Reader::Another(_)));

        let why = |role| match desktop(role).reader() {
            Reader::Nobody(why) => why,
            Reader::This | Reader::Another(_) => panic!("it says why nothing reads"),
        };
        assert!(why(Role::Elsewhere).contains("speakers"), "{}", why(Role::Elsewhere));
        assert!(!why(Role::Undecided).contains("speakers"), "{}", why(Role::Undecided));
    }

    /// What came of the lock takes a luneta from not knowing what it is to
    /// reaching the one that holds it, and from there to holding it. One over
    /// ssh is none of them, and stays so.
    #[test]
    fn what_came_of_the_lock_says_which_luneta_the_keys_are_for() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join("alerts.sock");
        let (events, _heard) = mpsc::channel();

        let mut desktop = Desktop { serving: None, role: Role::Undecided, events: events.clone() };
        assert!(!desktop.heard(Heard::Peering(socket.clone()), false));
        assert!(matches!(desktop.reader(), Reader::Another(_)));
        let lock = File::create(folder.path().join("lock")).expect("a lock");
        assert!(desktop.heard(Heard::Holding(lock, Ok(socket.clone())), false));
        assert!(matches!(desktop.reader(), Reader::This));

        let mut over_ssh = Desktop { serving: None, role: Role::Elsewhere, events };
        assert!(!over_ssh.heard(Heard::Peering(socket), false));
        assert!(matches!(over_ssh.reader(), Reader::Nobody(NO_SPEAKERS)));
    }

    /// The notification of an issue filed gives its number and title, and a
    /// click opens its page, with no sound. One of an issue gh did not file,
    /// or may have, says why and that the box has your words again, sounds,
    /// and a click brings luneta's window forward.
    #[test]
    fn an_issue_notification_says_what_came_of_it() {
        let url = "https://github.com/owner/repo/issues/362";
        let filed = Filed::page(url, "A red button").expect("an issue");
        let told = Told::issue("owner/repo", &Ok(filed));
        let open = Came::Filed(Link::parse(url).expect("a link"));
        let said = Told {
            title: "Filed #362 in owner/repo".to_owned(),
            body: "A red button".to_owned(),
            came: open,
        };
        assert_eq!(told, said);

        let refused = Told::issue("luneta", &Err(Unfiled::Refused("gh: no labels".to_owned())));
        let said = Told {
            title: "No issue filed in luneta".to_owned(),
            body: "gh: no labels; i has your words again".to_owned(),
            came: Came::Unfiled,
        };
        assert_eq!(refused, said);

        let doubt = Unfiled::InDoubt("gh: it did not answer in time".to_owned());
        let doubted = Told::issue("luneta", &Err(doubt));
        assert_eq!(doubted.title, "An issue in luneta may be filed");
        assert_eq!(doubted.body, "gh: it did not answer in time; i has your words, look first");
        assert_eq!(doubted.came, Came::Unfiled);
    }

    /// A luneta that holds the lock, in a folder it may not hear in, reads
    /// aloud all the same, and says why a click goes nowhere.
    #[test]
    fn a_luneta_that_may_not_hear_in_the_folder_still_reads_aloud_and_says_why() {
        let folder = tempfile::tempdir().expect("a folder");
        let (events, heard) = mpsc::channel();
        let mut desktop = Desktop { serving: None, role: Role::Undecided, events };

        let lock = File::create(folder.path().join("lock")).expect("a lock");
        assert!(desktop.heard(Heard::Holding(lock, Err("why".to_owned())), false));
        assert!(matches!(desktop.reader(), Reader::This));
        assert!(matches!(desktop.role, Role::Holder { socket: None, .. }));
        let Ok(Event::Notice(said)) = heard.try_recv() else {
            panic!("it says why");
        };
        assert_eq!(said, "a click on a notification goes nowhere: why");
    }
}
