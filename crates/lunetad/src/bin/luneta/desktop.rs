//! The desktop that the notifications of `alerts` go up on: freedesktop's
//! notification server, over D-Bus, and on macOS the Notification Center,
//! through `terminal-notifier`, as nix-darwin puts it on the `PATH`. A click
//! on one comes back to luneta as `Event::Clicked`.
//!
//! One luneta of a machine puts them up, the first to have one to put up,
//! which holds a lock for as long as it runs: another's would be the same
//! again. And none puts any up while it runs over ssh: the desktop of the
//! machine it runs on is not the one you sit at.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::{self as channel, UnboundedReceiver, UnboundedSender};

use crate::alerts::{Alert, Key};
use crate::host::Event;

/// The file the luneta that puts notifications up holds a lock on, in
/// luneta's runtime folder.
const LOCK: &str = "alerts.lock";

/// What luneta has the desktop do.
#[derive(Debug)]
enum Order {
    Post(Alert),
    Close(Key),
}

/// The desktop, through a thread of its own, which takes luneta's orders in
/// turn: a notification taken down after it was put up goes, however long
/// putting it up took.
pub struct Desktop {
    /// What takes the orders to the thread, and the thread: none where
    /// luneta runs over ssh, nor once it ends.
    serving: Option<(UnboundedSender<Order>, JoinHandle<()>)>,
}

impl Desktop {
    /// The desktop of the machine luneta runs on, which hands `events` the
    /// clicks on its notifications; or none, where luneta runs over ssh.
    pub fn start(events: &Sender<Event>) -> Desktop {
        let elsewhere = ["SSH_CONNECTION", "SSH_TTY"].into_iter().any(|name| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        });
        if elsewhere {
            return Desktop { serving: None };
        }
        let (orders, taken) = channel::unbounded_channel();
        let events = events.clone();
        let worker = thread::spawn(move || serve(taken, &events));
        Desktop { serving: Some((orders, worker)) }
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
}

impl Drop for Desktop {
    /// Takes down every notification this luneta put up, and waits until it
    /// has, as luneta ends.
    fn drop(&mut self) {
        if let Some((orders, worker)) = self.serving.take() {
            drop(orders);
            // What a thread that panicked left up stays so.
            let _ = worker.join();
        }
    }
}

/// Whether this luneta puts notifications up, as it does once it holds the
/// lock, which it takes on the first it has, and holds in `held` from then
/// on.
fn ours(held: &mut Option<File>) -> bool {
    if held.is_some() {
        return true;
    }
    let Some(folder) = lunetad::folder::find() else {
        return false;
    };
    let Ok(folder) = lunetad::folder::make(&folder) else {
        return false;
    };
    let Ok(lock) = File::create(folder.path().join(LOCK)) else {
        return false;
    };
    // Let go of as `lock` closes, as luneta ends.
    if lock.try_lock().is_err() {
        return false;
    }
    *held = Some(lock);
    true
}

/// Says what went wrong with the desktop, once: the sidebar says it.
fn say(events: &Sender<Event>, said: &mut bool, why: &str) {
    if !*said {
        *said = true;
        // luneta is on its way out.
        let _ = events.send(Event::Notice(format!("no notifications: {why}")));
    }
}

/// Tells the luneta listening on `socket` that the notification of the
/// group `group` was clicked, as `luneta clicked` does, which a click on one
/// runs on macOS.
pub fn tell(socket: &Path, group: &str) -> io::Result<()> {
    writeln!(UnixStream::connect(socket)?, "{group}")
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
    use std::num::NonZeroU32;
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tokio_stream::{self as stream, StreamExt as _};
    use zbus::names::UniqueName;
    use zbus::{Connection, Proxy, Variant};

    use super::{ours, say, Order};
    use crate::alerts::{Alert, Key, Kind};
    use crate::host::Event;

    const SERVER: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

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
            Err(error) => return say(events, &mut said, &error.to_string()),
        };
        let orders = UnboundedReceiverStream::new(orders).map(Heard::Order);
        let mut heard = orders.chain(stream::once(Heard::Ended)).merge(signals);
        let (mut up, mut held) = (HashMap::<Key, Posted>::new(), None);
        while let Some(heard) = heard.next().await {
            match heard {
                Heard::Order(Order::Post(alert)) => {
                    if !ours(&mut held) {
                        continue;
                    }
                    match notify(&proxy, &alert).await {
                        Ok(posted) => {
                            // What a server that has gone since put up went
                            // with it.
                            up.retain(|_, was| was.by == posted.by);
                            up.insert(alert.key.clone(), posted);
                            sound(alert.kind);
                        },
                        Err(error) => say(events, &mut said, &error.to_string()),
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
        let connection = Connection::session().await?;
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
/// of the click, through a socket of its. A notification goes by a group
/// named for its agent, which the click names: one that a luneta left up as
/// it was killed goes to that agent's pane too, and goes once the luneta
/// that puts them up now takes that agent's down.
#[cfg(any(target_os = "macos", test))]
mod notifier {
    #[cfg(target_os = "macos")]
    use std::collections::HashSet;
    use std::fs;
    use std::io::{self, BufRead as _, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    #[cfg(target_os = "macos")]
    use std::path::PathBuf;
    #[cfg(target_os = "macos")]
    use std::process::{Command, Stdio};
    use std::sync::mpsc::Sender;
    use std::thread;

    use crate::alerts::{Alert, Key, Kind};
    use crate::host::Event;
    use crate::machines::Machine;

    /// The terminal a click brings forward: Ghostty.
    const TERMINAL: &str = "com.mitchellh.ghostty";

    /// What each group luneta names is led by.
    const GROUPS: &str = "luneta-";

    /// The socket a click is told on, in luneta's runtime folder: the luneta
    /// that puts notifications up listens on it, which holds their lock.
    #[cfg(target_os = "macos")]
    const SOCKET: &str = "alerts.sock";

    /// The group the notification of `key`'s agent goes by: its pid, and the
    /// machine it runs on, where that is another.
    pub fn group(key: &Key) -> String {
        match &key.on {
            Machine::Local => format!("{GROUPS}{}", key.pid),
            Machine::Remote(destination) => format!("{GROUPS}{}-{destination}", key.pid),
        }
    }

    /// The agent `group` names, where it names one.
    pub fn key_of(group: &str) -> Option<Key> {
        let named = group.strip_prefix(GROUPS)?;
        let (pid, on) = match named.split_once('-') {
            Some((pid, destination)) => (pid, Machine::Remote(destination.parse().ok()?)),
            None => (named, Machine::Local),
        };
        Some(Key { on, pid: pid.parse().ok()? })
    }

    /// The arguments that put `alert` up as the group `group`, in the place
    /// of the one of the group up still, if any, and run `clicked` in `sh` on
    /// a click.
    pub fn posting(alert: &Alert, group: &str, clicked: &str) -> Vec<String> {
        let sound = match alert.kind {
            Kind::Asked | Kind::Plan => "Glass",
            Kind::Permission | Kind::Wants => "Ping",
            Kind::Done => "Hero",
        };
        let args = [
            ("-group", group),
            ("-title", &guarded(&alert.title)),
            ("-message", &guarded(&alert.body)),
            ("-sound", sound),
            ("-activate", TERMINAL),
            ("-execute", clicked),
        ];
        args.into_iter().flat_map(|(flag, value)| [flag.to_owned(), value.to_owned()]).collect()
    }

    /// `text` as terminal-notifier takes it for a value: macOS reads its
    /// arguments as defaults, which take a word led by a `-` for an option,
    /// and one led by a `(`, a `{` or a `<` for a list.
    fn guarded(text: &str) -> String {
        match text.chars().next() {
            Some('-' | '(' | '{' | '<') => format!(" {text}"),
            _ => text.to_owned(),
        }
    }

    /// The command a click runs, in `sh`: `luneta`, which tells the luneta
    /// listening on `socket` that `group` was clicked.
    pub fn clicked(luneta: &Path, socket: &Path, group: &str) -> String {
        let quoted = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
        let (luneta, socket) = (luneta.to_string_lossy(), socket.to_string_lossy());
        format!("{} clicked --socket {} {}", quoted(&luneta), quoted(&socket), quoted(group))
    }

    /// Hears on `socket`, for as long as luneta runs, each group clicked,
    /// which it hands `events` as the click of the agent it names. One that
    /// names none it leaves.
    pub fn listen(socket: &Path, events: Sender<Event>) -> io::Result<()> {
        // One left by a luneta that held the lock before is no one's.
        let _ = fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_err() {
                    continue;
                }
                let Some(key) = key_of(line.trim_end()) else {
                    continue;
                };
                // luneta is on its way out.
                if events.send(Event::Clicked(key)).is_err() {
                    return;
                }
            }
        });
        Ok(())
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
        use super::{ours, say, Order};

        let (mut said, mut held) = (false, None);
        // The agents whose notifications this luneta put up, to take down as
        // it ends.
        let mut posted: HashSet<Key> = HashSet::new();
        let mut socket: Option<PathBuf> = None;
        while let Some(order) = orders.blocking_recv() {
            match order {
                Order::Post(alert) => {
                    if !ours(&mut held) {
                        continue;
                    }
                    if socket.is_none() {
                        match listening(events) {
                            Ok(listening) => socket = Some(listening),
                            Err(why) => say(events, &mut said, &why),
                        }
                    }
                    let (Some(at), Ok(luneta)) = (&socket, std::env::current_exe()) else {
                        continue;
                    };
                    let group = group(&alert.key);
                    posted.insert(alert.key.clone());
                    if let Err(why) = run(&posting(&alert, &group, &clicked(&luneta, at, &group))) {
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
        if let Some(socket) = socket {
            let _ = fs::remove_file(socket);
        }
    }

    /// The socket a click is told on, which luneta hears on now, in luneta's
    /// runtime folder; or why there is none.
    #[cfg(target_os = "macos")]
    fn listening(events: &Sender<Event>) -> Result<PathBuf, String> {
        let folder = lunetad::folder::find().ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set")?;
        let folder = lunetad::folder::make(&folder).map_err(|refusal| refusal.to_string())?;
        let socket = folder.path().join(SOCKET);
        listen(&socket, events.clone()).map_err(|error| error.to_string())?;
        Ok(socket)
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc;
        use std::time::Duration;

        use super::*;

        fn key(pid: u32) -> Key {
            Key { on: Machine::Local, pid }
        }

        fn alert(kind: Kind, body: &str) -> Alert {
            let title = "fix — needs permission".to_owned();
            Alert { key: key(41), kind, title, body: body.to_owned() }
        }

        /// The group of an agent names it back: its pid, and another
        /// machine, whatever its name has in it. What names no agent, as
        /// another program's group, names none.
        #[test]
        fn an_agent_s_group_names_it() {
            let remote = Key { on: Machine::Remote("me@my-vps".parse().unwrap()), pid: 7 };
            assert_eq!(group(&key(41)), "luneta-41");
            assert_eq!(group(&remote), "luneta-7-me@my-vps");
            for key in [key(41), remote] {
                assert_eq!(key_of(&group(&key)), Some(key));
            }
            for named in ["luneta-x", "luneta-", "other-41", "luneta-7--x"] {
                assert_eq!(key_of(named), None, "{named}");
            }
        }

        /// A notification goes up by its group, which a click runs luneta
        /// with, quoted for `sh`; and what reads as an option or a list to
        /// macOS is not one.
        #[test]
        fn a_notification_goes_up_by_its_group_and_runs_luneta_on_a_click() {
            let luneta = Path::new("/nix/x/bin/luneta");
            let clicked = clicked(luneta, Path::new("/var/it's/alerts.sock"), "luneta-41");
            let socket = r"'/var/it'\''s/alerts.sock'";
            let said = format!("'/nix/x/bin/luneta' clicked --socket {socket} 'luneta-41'");
            assert_eq!(clicked, said);
            let args = posting(&alert(Kind::Permission, "-rf: rm"), "luneta-41", &clicked);
            let value = |flag: &str| {
                let at = args.iter().position(|arg| arg == flag).expect("the flag is there");
                args[at + 1].clone()
            };
            assert_eq!(value("-group"), "luneta-41");
            assert_eq!(value("-message"), " -rf: rm");
            assert_eq!(value("-title"), "fix — needs permission");
            assert_eq!((value("-sound").as_str(), value("-activate").as_str()), ("Ping", TERMINAL));
            assert_eq!(value("-execute"), clicked);
        }

        /// What a click tells the socket reaches luneta as the click of the
        /// agent its group names; one that names none reaches nothing.
        #[test]
        fn a_click_told_on_the_socket_is_the_click_of_its_agent() {
            let folder = tempfile::tempdir().expect("a folder");
            let socket = folder.path().join("alerts.sock");
            let (events, heard) = mpsc::channel();
            listen(&socket, events).expect("it listens");
            super::super::tell(&socket, "luneta-x").expect("it is told");
            super::super::tell(&socket, &group(&key(41))).expect("it is told");
            let Ok(Event::Clicked(clicked)) = heard.recv_timeout(Duration::from_secs(5)) else {
                panic!("luneta heard no click");
            };
            assert_eq!(clicked, key(41));
            assert!(heard.try_recv().is_err());
        }
    }
}
