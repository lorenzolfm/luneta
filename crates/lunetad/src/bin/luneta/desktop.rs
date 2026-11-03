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

use crate::alerts::{Alert, Key};
use crate::host::Event;
use crate::peers::{self, Heard, Peers, Reaching};

/// What luneta has the desktop do.
#[derive(Debug)]
enum Order {
    Post(Alert),
    Close(Key),
    /// Put the notifications up from now on, as this luneta holds the lock.
    Hold,
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
    /// It does not know yet, or never will, as it runs over ssh.
    Unsure,
    /// Another holds the lock: this one tells it where it looks.
    Peer(Reaching),
    /// It holds the lock, and puts the notifications up: with the socket it
    /// hears the others on, where it hears on one, and what they said.
    Holder { _lock: File, socket: Option<PathBuf>, peers: Peers },
}

impl Desktop {
    /// The desktop of the machine luneta runs on, which hands `events` the
    /// clicks on its notifications; or none, where luneta runs over ssh.
    pub fn start(events: &Sender<Event>) -> Desktop {
        let elsewhere = ["SSH_CONNECTION", "SSH_TTY"].into_iter().any(|name| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        });
        let events = events.clone();
        if elsewhere {
            return Desktop { serving: None, role: Role::Unsure, events };
        }
        let (orders, taken) = channel::unbounded_channel();
        let serving = events.clone();
        let worker = thread::spawn(move || serve(taken, &serving));
        peers::wait(events.clone());
        Desktop { serving: Some((orders, worker)), role: Role::Unsure, events }
    }

    pub fn post(&self, alert: Alert) {
        self.order(Order::Post(alert));
    }

    pub fn close(&self, key: Key) {
        self.order(Order::Close(key));
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
            Heard::Holding(lock) => {
                // It reaches for no other now.
                self.role = Role::Unsure;
                let socket = match peers::listening(&self.events) {
                    Ok(socket) => Some(socket),
                    Err(why) => {
                        // luneta is on its way out.
                        let _ = self.events.send(Event::Notice(format!(
                            "a click on a notification goes nowhere: {why}"
                        )));
                        None
                    },
                };
                self.order(Order::Hold);
                self.role = Role::Holder { _lock: lock, socket, peers: Peers::default() };
                true
            },
            Heard::Peering => {
                if let (Role::Unsure, Ok(socket)) = (&self.role, peers::socket()) {
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
            Role::Holder { .. } | Role::Unsure => {},
        }
    }

    /// You look at the panes of the agents of `keys` in luneta's window,
    /// which has the focus, or at none.
    pub fn looking(&mut self, keys: &[Key]) {
        if let Role::Peer(reaching) = &mut self.role {
            reaching.looks(keys);
        }
    }

    /// The agents whose panes you look at in another luneta's window, which
    /// has the focus.
    pub fn looks(&self) -> Vec<Key> {
        match &self.role {
            Role::Holder { peers, .. } => peers.looks(),
            Role::Peer(_) | Role::Unsure => Vec::new(),
        }
    }

    /// Hands a click on the notification of `key`'s agent to the luneta whose
    /// window had the focus last, where that is another, which goes to its
    /// pane: whether it did.
    pub fn hand(&mut self, key: &Key) -> bool {
        match &mut self.role {
            Role::Holder { peers, .. } => peers.hand(key),
            Role::Peer(_) | Role::Unsure => false,
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

    use super::{say, Order};
    use crate::alerts::{Alert, Key, Kind};
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
        while let Some(heard) = heard.next().await {
            match heard {
                Heard::Order(Order::Hold) => holding = true,
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
                            if alert.sounds {
                                sound(alert.kind);
                            }
                        },
                        Err(error) => say(events, &mut said, &why(&error)),
                    }
                },
                Heard::Order(Order::Close(key)) => {
                    if let Some(posted) = up.remove(&key) {
                        // One the server has let go of, or a server that has
                        // gone, is gone.
                        let _ = close(proxy.connection(), &posted).await;
                    }
                },
                Heard::Clicked(posted, action) => {
                    let clicked = up.iter().find(|(_, up)| **up == posted).map(|(key, _)| key);
                    if action == CLICK
                        && let Some(key) = clicked
                    {
                        // luneta is on its way out.
                        let _ = events.send(Event::Clicked(key.clone()));
                    }
                },
                Heard::Closed(posted) => up.retain(|_, up| *up != posted),
                Heard::Ended => break,
            }
        }
        for posted in up.into_values() {
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
        let hints = HashMap::from([("urgency", Variant::from(urgency))]);
        let icon = icon();
        let body = escaped(&alert.body);
        let actions = [CLICK, "Go to the pane"];
        let asked = ("luneta", 0_u32, icon.as_str(), alert.title.as_str(), body.as_str());
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

    /// Plays the sound of a notification of `kind`, where `pw-play` can.
    fn sound(kind: Kind) {
        let name = match kind {
            Kind::Asked | Kind::Plan => "window-question",
            Kind::Permission | Kind::Wants => "bell",
            Kind::Done => "complete",
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

    use crate::alerts::{Alert, Kind};
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
        if alert.sounds {
            let sound = match alert.kind {
                Kind::Asked | Kind::Plan => "Glass",
                Kind::Permission | Kind::Wants => "Ping",
                Kind::Done => "Hero",
            };
            args.push(("-sound", sound.to_owned()));
        }
        args.extend(clicked.map(|clicked| ("-execute", clicked.to_owned())));
        args.into_iter().flat_map(|(flag, value)| [flag.to_owned(), value]).collect()
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
                Order::Hold => held = Some(crate::peers::socket().ok()),
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
        use super::*;
        use crate::alerts::Key;
        use crate::machines::Machine;

        fn alert(kind: Kind, body: &str, sounds: bool) -> Alert {
            let (key, title) = (Key { on: Machine::Local, pid: 41 }, "fix — needs permission");
            Alert { key, kind, title: title.to_owned(), body: body.to_owned(), sounds }
        }

        /// A notification goes up by its group, which a click runs luneta
        /// with, quoted for `sh`; what it says is text to macOS whatever it
        /// starts with, and it sounds but where it was up before.
        #[test]
        fn a_notification_goes_up_by_its_group_and_runs_luneta_on_a_click() {
            let luneta = Path::new("/nix/x/bin/luneta");
            let clicked = clicked(luneta, Path::new("/var/it's/alerts.sock"), "luneta-41");
            let socket = r"'/var/it'\''s/alerts.sock'";
            let said = format!("'/nix/x/bin/luneta' clicked --socket {socket} 'luneta-41'");
            assert_eq!(clicked, said);
            let rm = alert(Kind::Permission, "-rf: rm", true);
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
            let again = posting(&alert(Kind::Done, "All done.", false), "luneta-41", None);
            assert_eq!((value(&again, "-sound"), value(&again, "-execute")), (None, None));
        }
    }
}
