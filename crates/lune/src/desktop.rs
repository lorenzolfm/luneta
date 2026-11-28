use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::{self as channel, UnboundedReceiver, UnboundedSender};

use luned::issues::{Filed, Unfiled};
use luned::links::Link;

use crate::alerts::{Alert, Key, Sound};
use crate::host::Event;
use crate::peers::{self, Heard, NO_READER, Peers, Reaching};

pub fn elsewhere() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[derive(Debug)]
enum Order {
    Post(Alert),
    Close(Key),
    Tell(Told),
    Hold(Option<PathBuf>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Told {
    pub title: String,
    pub body: String,
    pub came: Came,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Came {
    Filed(Link),
    Unfiled,
}

impl Came {
    fn sound(&self) -> Option<Sound> {
        match self {
            Came::Filed(_) => None,
            Came::Unfiled => Some(Sound::Bell),
        }
    }
}

impl Told {
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
            Err(Unfiled::Refused(why)) => unfiled(
                format!("No issue filed in {repository}"),
                why,
                "i has your words again",
            ),
            Err(Unfiled::InDoubt(why)) => unfiled(
                format!("An issue in {repository} may be filed"),
                why,
                "i has your words, look first",
            ),
        }
    }

    pub fn flow(title: String, body: String) -> Told {
        Told {
            title,
            body,
            came: Came::Unfiled,
        }
    }
}

pub struct Desktop {
    serving: Option<(UnboundedSender<Order>, JoinHandle<()>)>,
    role: Role,
    events: Sender<Event>,
}

enum Role {
    Undecided,
    Elsewhere,
    Peer(Reaching),
    Holder {
        _lock: File,
        socket: Option<PathBuf>,
        peers: Peers,
    },
}

const NO_SPEAKERS: &str = "this lune reads nothing aloud: it has no speakers of yours";

pub enum Reader<'a> {
    This,
    Another(&'a Reaching),
    Nobody(&'static str),
}

impl Desktop {
    pub fn start(events: &Sender<Event>, luned: Result<PathBuf, String>) -> Desktop {
        let events = events.clone();
        if elsewhere() {
            return Desktop {
                serving: None,
                role: Role::Elsewhere,
                events,
            };
        }
        let (orders, taken) = channel::unbounded_channel();
        let serving = events.clone();
        let worker = thread::spawn(move || serve(taken, &serving));
        peers::wait(events.clone(), luned);
        Desktop {
            serving: Some((orders, worker)),
            role: Role::Undecided,
            events,
        }
    }

    pub fn unserved(events: &Sender<Event>) -> Desktop {
        Desktop {
            serving: None,
            role: Role::Undecided,
            events: events.clone(),
        }
    }

    pub fn post(&self, alert: Alert) {
        self.order(Order::Post(alert));
    }

    pub fn close(&self, key: Key) {
        self.order(Order::Close(key));
    }

    pub fn tell(&self, told: Told) {
        self.order(Order::Tell(told));
    }

    fn order(&self, order: Order) {
        if let Some((orders, _)) = &self.serving {
            let _ = orders.send(order);
        }
    }

    pub fn heard(&mut self, heard: Heard, focused: bool) -> bool {
        match heard {
            Heard::Holding(lock, socket) => {
                self.role = Role::Undecided;
                let socket = socket
                    .and_then(|socket| peers::listening(&socket, &self.events).map(|()| socket));
                let socket = match socket {
                    Ok(socket) => Some(socket),
                    Err(why) => {
                        let _ = self.events.send(Event::Notice(format!(
                            "a click on a notification goes nowhere: {why}"
                        )));
                        None
                    }
                };
                self.order(Order::Hold(socket.clone()));
                self.role = Role::Holder {
                    _lock: lock,
                    socket,
                    peers: Peers::default(),
                };
                true
            }
            Heard::Peering(socket) => {
                if matches!(self.role, Role::Undecided) {
                    self.role = Role::Peer(Reaching::to(socket, focused, self.events.clone()));
                }
                false
            }
            Heard::Peer(number, peered) => {
                if let Role::Holder { peers, .. } = &mut self.role {
                    peers.heard(number, peered);
                }
                false
            }
        }
    }

    pub fn focus(&mut self, focused: bool) {
        match &mut self.role {
            Role::Peer(reaching) => reaching.focus(focused),
            Role::Holder { peers, .. } if focused => peers.here(),
            Role::Holder { .. } | Role::Undecided | Role::Elsewhere => {}
        }
    }

    pub fn reads_aloud(&self) -> bool {
        matches!(self.reader(), Reader::This)
    }

    pub fn reader(&self) -> Reader<'_> {
        match &self.role {
            Role::Holder { .. } => Reader::This,
            Role::Peer(reaching) => Reader::Another(reaching),
            Role::Undecided => Reader::Nobody(NO_READER),
            Role::Elsewhere => Reader::Nobody(NO_SPEAKERS),
        }
    }

    pub fn dictating(&mut self, dictating: bool) {
        if let Role::Peer(reaching) = &mut self.role {
            reaching.dictating(dictating);
        }
    }

    pub fn another_dictates(&self) -> bool {
        match &self.role {
            Role::Holder { peers, .. } => peers.dictating(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }

    pub fn looking(&mut self, keys: &[Key]) {
        if let Role::Peer(reaching) = &mut self.role {
            reaching.looks(keys);
        }
    }

    pub fn another_focused(&self) -> bool {
        match &self.role {
            Role::Holder { peers, .. } => peers.focused(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }

    pub fn looks(&self) -> Vec<Key> {
        match &self.role {
            Role::Holder { peers, .. } => peers.looks(),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => Vec::new(),
        }
    }

    pub fn hand(&mut self, key: &Key) -> bool {
        match &mut self.role {
            Role::Holder { peers, .. } => peers.hand(key),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => false,
        }
    }

    pub fn nothing_to_read(&mut self, number: u64) {
        match &mut self.role {
            Role::Holder { peers, .. } => peers.nothing_to_read(number),
            Role::Peer(_) | Role::Undecided | Role::Elsewhere => {
                unreachable!("only the lune that holds the lock hears another window's keys")
            }
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if let Some((orders, worker)) = self.serving.take() {
            drop(orders);
            let _ = worker.join();
        }
        if let Role::Holder {
            socket: Some(socket),
            ..
        } = &self.role
        {
            let _ = fs::remove_file(socket);
        }
    }
}

fn say(events: &Sender<Event>, said: &mut bool, why: &str) {
    if !*said {
        *said = true;
        let _ = events.send(Event::Notice(format!("no notifications: {why}")));
    }
}

#[cfg(not(target_os = "macos"))]
fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
    luned::tasks::shared().block_on(freedesktop::serve(orders, events));
}

#[cfg(target_os = "macos")]
fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
    notifier::serve(orders, events);
}

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

    use super::{Came, Order, Told, say};
    use crate::alerts::{Alert, Key, Kind, Sound};
    use crate::host::Event;

    const SERVER: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

    const ANSWER: Duration = Duration::from_secs(5);

    const CLOSING: Duration = Duration::from_secs(1);

    const SOUNDS: &str = "/run/current-system/sw/share/sounds/freedesktop/stereo";

    const CLICK: &str = "default";

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Posted {
        by: UniqueName<'static>,
        id: NonZeroU32,
    }

    enum Heard {
        Order(Order),
        Clicked(Posted, String),
        Closed(Posted),
        Ended,
    }

    pub async fn serve(orders: UnboundedReceiver<Order>, events: &Sender<Event>) {
        let mut said = false;
        let (proxy, signals) = match connect().await {
            Ok(connected) => connected,
            Err(error) => return say(events, &mut said, &why(&error)),
        };
        let orders = UnboundedReceiverStream::new(orders).map(Heard::Order);
        let mut heard = orders.chain(stream::once(Heard::Ended)).merge(signals);
        let (mut up, mut holding) = (HashMap::<Key, Posted>::new(), false);
        let mut told: Vec<(Posted, Came)> = Vec::new();
        while let Some(heard) = heard.next().await {
            match heard {
                Heard::Order(Order::Hold(_socket)) => holding = true,
                Heard::Order(Order::Post(alert)) => {
                    if !holding {
                        continue;
                    }
                    match notify(&proxy, &alert).await {
                        Ok(posted) => {
                            up.retain(|_, was| was.by == posted.by);
                            up.insert(alert.key.clone(), posted);
                            if let Some(sound) = alert.sound {
                                play(sound);
                            }
                        }
                        Err(error) => say(events, &mut said, &why(&error)),
                    }
                }
                Heard::Order(Order::Tell(telling)) => match tell(&proxy, &telling).await {
                    Ok(posted) => {
                        told.retain(|(was, _)| was.by == posted.by);
                        if let Some(sound) = telling.came.sound() {
                            play(sound);
                        }
                        told.push((posted, telling.came));
                    }
                    Err(error) => say(events, &mut said, &why(&error)),
                },
                Heard::Order(Order::Close(key)) => {
                    if let Some(posted) = up.remove(&key) {
                        let _ = close(proxy.connection(), &posted).await;
                    }
                }
                Heard::Clicked(posted, action) => {
                    if action != CLICK {
                        continue;
                    }
                    let clicked = up.iter().find(|(_, up)| **up == posted).map(|(key, _)| key);
                    if let Some(key) = clicked {
                        let _ = events.send(Event::Clicked(key.clone()));
                    } else if let Some(at) = told.iter().position(|(up, _)| *up == posted) {
                        let _ = events.send(Event::IssueClicked(told.swap_remove(at).1));
                    }
                }
                Heard::Closed(posted) => {
                    up.retain(|_, up| *up != posted);
                    told.retain(|(up, _)| *up != posted);
                }
                Heard::Ended => break,
            }
        }
        let told = told.into_iter().map(|(posted, _)| posted);
        for posted in up.into_values().chain(told) {
            let _ = tokio::time::timeout(CLOSING, close(proxy.connection(), &posted)).await;
        }
    }

    type Signals = std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Heard> + Send>>;

    async fn connect() -> zbus::Result<(Proxy<'static>, Signals)> {
        let connection = Builder::session().method_timeout(ANSWER).build().await?;
        let proxy = Proxy::new(&connection, SERVER, PATH, SERVER).await?;
        let signals = proxy.receive_all_signals().await?.filter_map(|signal| {
            let (header, body) = (signal.header(), signal.body());
            let by = header.sender()?.to_owned();
            match header.member()?.as_str() {
                "ActionInvoked" => {
                    let (id, action) = body.deserialize::<(u32, String)>().ok()?;
                    Some(Heard::Clicked(
                        Posted {
                            by,
                            id: NonZeroU32::new(id)?,
                        },
                        action,
                    ))
                }
                "NotificationClosed" => {
                    let (id, _why) = body.deserialize::<(u32, u32)>().ok()?;
                    Some(Heard::Closed(Posted {
                        by,
                        id: NonZeroU32::new(id)?,
                    }))
                }
                _ => None,
            }
        });
        Ok((proxy, Box::pin(signals)))
    }

    async fn notify(proxy: &Proxy<'_>, alert: &Alert) -> zbus::Result<Posted> {
        let urgency: u8 = match alert.kind {
            Kind::Done => 1,
            Kind::Asked
            | Kind::Plan
            | Kind::Permission
            | Kind::Auth
            | Kind::Wants
            | Kind::Error => 2,
        };
        post(proxy, &alert.title, &alert.body, "Go to the view", urgency).await
    }

    async fn tell(proxy: &Proxy<'_>, told: &Told) -> zbus::Result<Posted> {
        let (click, urgency) = match told.came {
            Came::Filed(_) => ("Open the issue", 1),
            Came::Unfiled => ("Go to lune", 2),
        };
        post(proxy, &told.title, &told.body, click, urgency).await
    }

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
        let asked = ("lune", 0_u32, icon.as_str(), title, body.as_str());
        let told = (
            asked.0,
            asked.1,
            asked.2,
            asked.3,
            asked.4,
            &actions[..],
            hints,
            -1_i32,
        );
        let answer = proxy.call_method("Notify", &told).await?;
        let unsaid = |what: &str| zbus::Error::Failure(format!("the server's answer {what}"));
        let by = answer
            .header()
            .sender()
            .ok_or_else(|| unsaid("names no sender"))?
            .to_owned();
        let id = answer.body().deserialize::<u32>()?;
        let id = NonZeroU32::new(id).ok_or_else(|| unsaid("gives the notification no id"))?;
        Ok(Posted { by, id })
    }

    fn why(error: &zbus::Error) -> String {
        match error {
            zbus::Error::InputOutput(error) if error.kind() == io::ErrorKind::TimedOut => {
                format!(
                    "the notification server did not answer in {}s",
                    ANSWER.as_secs()
                )
            }
            error => error.to_string(),
        }
    }

    async fn close(connection: &Connection, posted: &Posted) -> zbus::Result<()> {
        let (to, id) = (Some(&posted.by), (posted.id.get(),));
        connection
            .call_method(to, PATH, Some(SERVER), "CloseNotification", &id)
            .await?;
        Ok(())
    }

    fn icon() -> String {
        let home = std::env::var_os("HOME").unwrap_or_default();
        let icon = Path::new(&home).join(".local/share/icons/notifications/claude.png");
        if icon.is_file() {
            icon.to_string_lossy().into_owned()
        } else {
            String::new()
        }
    }

    fn escaped(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

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
        play.arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = play.spawn();
    }
}

pub fn raise(title: Option<&str>) {
    if cfg!(target_os = "macos") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let Some(title) = title.map(str::to_owned) else {
        return;
    };
    thread::spawn(move || {
        let clients = std::process::Command::new("hyprctl")
            .args(["clients", "-j"])
            .output();
        let Ok(clients) = clients else {
            return;
        };
        let clients: Vec<serde_json::Value> =
            serde_json::from_slice(&clients.stdout).unwrap_or_default();
        let window = clients
            .iter()
            .find(|client| client["title"].as_str() == Some(&title));
        let Some(address) = window.and_then(|window| window["address"].as_str()) else {
            return;
        };
        let at = format!("address:{address}");
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &at])
            .stdout(std::process::Stdio::null())
            .status();
    });
}

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
    #[cfg(target_os = "macos")]
    use crate::alerts::Key;
    use crate::alerts::{Alert, Sound};
    #[cfg(target_os = "macos")]
    use crate::host::Event;
    #[cfg(target_os = "macos")]
    use crate::peers::group;

    const TERMINAL: &str = "com.mitchellh.ghostty";

    pub fn posting(alert: &Alert, group: &str, clicked: Option<&str>) -> Vec<String> {
        let mut args = vec![
            ("-group", group.to_owned()),
            ("-title", guarded(&alert.title)),
            ("-message", guarded(&alert.body)),
            ("-activate", TERMINAL.to_owned()),
        ];
        args.extend(
            alert
                .sound
                .map(|sound| ("-sound", sounded(sound).to_owned())),
        );
        args.extend(clicked.map(|clicked| ("-execute", clicked.to_owned())));
        args.into_iter()
            .flat_map(|(flag, value)| [flag.to_owned(), value])
            .collect()
    }

    pub fn telling(told: &Told) -> Vec<String> {
        let mut args = vec![
            ("-title", guarded(&told.title)),
            ("-message", guarded(&told.body)),
        ];
        args.push(match &told.came {
            Came::Filed(link) => ("-open", link.url()),
            Came::Unfiled => ("-activate", TERMINAL.to_owned()),
        });
        args.extend(
            told.came
                .sound()
                .map(|sound| ("-sound", sounded(sound).to_owned())),
        );
        args.into_iter()
            .flat_map(|(flag, value)| [flag.to_owned(), value])
            .collect()
    }

    fn sounded(sound: Sound) -> &'static str {
        match sound {
            Sound::Question => "Glass",
            Sound::Bell => "Ping",
        }
    }

    fn guarded(text: &str) -> String {
        format!("\\{text}")
    }

    pub fn clicked(lune: &Path, socket: &Path, group: &str) -> String {
        let quoted = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
        let (lune, socket) = (lune.to_string_lossy(), socket.to_string_lossy());
        format!(
            "{} clicked --socket {} {}",
            quoted(&lune),
            quoted(&socket),
            quoted(group)
        )
    }

    #[cfg(target_os = "macos")]
    fn run(args: &[String]) -> Result<(), String> {
        let mut notifier = Command::new("terminal-notifier");
        notifier.args(args).stdin(Stdio::null());
        match notifier.output() {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let said = String::from_utf8_lossy(&output.stderr);
                match said
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    Some(line) => Err(format!("terminal-notifier: {line}")),
                    None => Err(format!("terminal-notifier {}", output.status)),
                }
            }
            Err(error) => Err(format!("terminal-notifier: {error}")),
        }
    }

    #[cfg(target_os = "macos")]
    fn remove(key: &Key) {
        let _ = run(&["-remove".to_owned(), group(key)]);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn serve(
        mut orders: tokio::sync::mpsc::UnboundedReceiver<super::Order>,
        events: &Sender<Event>,
    ) {
        use super::{Order, say};

        let mut said = false;
        let mut held: Option<Option<PathBuf>> = None;
        let mut posted: HashSet<Key> = HashSet::new();
        let lune = std::env::current_exe();
        while let Some(order) = orders.blocking_recv() {
            match order {
                Order::Hold(socket) => held = Some(socket),
                Order::Post(alert) => {
                    let Some(socket) = &held else {
                        continue;
                    };
                    let group = group(&alert.key);
                    let clicked = match (socket, &lune) {
                        (Some(at), Ok(lune)) => Some(clicked(lune, at, &group)),
                        (None, _) | (_, Err(_)) => None,
                    };
                    posted.insert(alert.key.clone());
                    if let Err(why) = run(&posting(&alert, &group, clicked.as_deref())) {
                        say(events, &mut said, &why);
                    }
                }
                Order::Tell(told) => {
                    if let Err(why) = run(&telling(&told)) {
                        say(events, &mut said, &why);
                    }
                }
                Order::Close(key) => {
                    if held.is_some() {
                        posted.remove(&key);
                        remove(&key);
                    }
                }
            }
        }
        for key in &posted {
            remove(key);
        }
    }

    #[cfg(test)]
    mod tests {
        use luned::links::Link;

        use super::*;
        use crate::alerts::{Key, Kind};
        use crate::machines::Machine;

        fn alert(kind: Kind, body: &str, sound: Option<Sound>) -> Alert {
            let (key, title) = (
                Key {
                    on: Machine::Local,
                    pid: 41,
                },
                "fix — needs permission",
            );
            Alert {
                key,
                kind,
                title: title.to_owned(),
                body: body.to_owned(),
                sound,
            }
        }

        #[test]
        fn a_notification_goes_up_by_its_group_and_runs_lune_on_a_click() {
            let lune = Path::new("/nix/x/bin/lune");
            let clicked = clicked(lune, Path::new("/var/it's/alerts.sock"), "lune-41");
            let socket = r"'/var/it'\''s/alerts.sock'";
            let said = format!("'/nix/x/bin/lune' clicked --socket {socket} 'lune-41'");
            assert_eq!(clicked, said);
            let rm = alert(Kind::Permission, "-rf: rm", Some(Sound::Bell));
            let args = posting(&rm, "lune-41", Some(&clicked));
            let value = |args: &[String], flag: &str| {
                let at = args.iter().position(|arg| arg == flag)?;
                Some(args[at + 1].clone())
            };
            let said = |flag| value(&args, flag).expect("the flag is there");
            assert_eq!(said("-group"), "lune-41");
            assert_eq!(said("-message"), r"\-rf: rm");
            assert_eq!(said("-title"), r"\fix — needs permission");
            assert_eq!(
                (said("-sound").as_str(), said("-activate").as_str()),
                ("Ping", TERMINAL)
            );
            assert_eq!(said("-execute"), clicked);
            for text in [
                "[x] done",
                "(wip) fix",
                "{a = b;}",
                "<b>it</b>",
                "\"Done\" said",
                r"\n",
            ] {
                assert_eq!(guarded(text), format!("\\{text}"));
            }
            let again = posting(&alert(Kind::Done, "All done.", None), "lune-41", None);
            assert_eq!(
                (value(&again, "-sound"), value(&again, "-execute")),
                (None, None)
            );
        }

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
                args.chunks(2)
                    .map(|pair| (pair[0].clone(), pair[1].clone()))
                    .collect()
            };
            let owned = |said: &[(&str, &str)]| -> Vec<(String, String)> {
                said.iter()
                    .map(|(flag, value)| ((*flag).to_owned(), (*value).to_owned()))
                    .collect()
            };
            let said = [
                ("-title", r"\Filed #362 in owner/repo"),
                ("-message", r"\[wip] a red button"),
                ("-open", url),
            ];
            assert_eq!(pairs(&filed), owned(&said));
            let refused = Told {
                came: Came::Unfiled,
                ..filed
            };
            let said = [("-activate", TERMINAL), ("-sound", "Ping")];
            assert_eq!(pairs(&refused)[2..], owned(&said));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn the_keys_of_what_is_read_aloud_go_to_the_reader_and_only_ssh_blames_the_speakers() {
        let folder = tempfile::tempdir().expect("a folder");
        let (events, _heard) = mpsc::channel();
        let desktop = |role| Desktop {
            serving: None,
            role,
            events: events.clone(),
        };

        let lock = File::create(folder.path().join("lock")).expect("a lock");
        let holder = desktop(Role::Holder {
            _lock: lock,
            socket: None,
            peers: Peers::default(),
        });
        assert!(matches!(holder.reader(), Reader::This));
        let socket = folder.path().join("nobody-hears.sock");
        let reaching = Reaching::to(socket, false, events.clone());
        assert!(matches!(
            desktop(Role::Peer(reaching)).reader(),
            Reader::Another(_)
        ));

        let why = |role| match desktop(role).reader() {
            Reader::Nobody(why) => why,
            Reader::This | Reader::Another(_) => panic!("it says why nothing reads"),
        };
        assert!(
            why(Role::Elsewhere).contains("speakers"),
            "{}",
            why(Role::Elsewhere)
        );
        assert!(
            !why(Role::Undecided).contains("speakers"),
            "{}",
            why(Role::Undecided)
        );
    }

    #[test]
    fn what_came_of_the_lock_says_which_lune_the_keys_are_for() {
        let folder = tempfile::tempdir().expect("a folder");
        let socket = folder.path().join("alerts.sock");
        let (events, _heard) = mpsc::channel();

        let mut desktop = Desktop {
            serving: None,
            role: Role::Undecided,
            events: events.clone(),
        };
        assert!(!desktop.heard(Heard::Peering(socket.clone()), false));
        assert!(matches!(desktop.reader(), Reader::Another(_)));
        let lock = File::create(folder.path().join("lock")).expect("a lock");
        assert!(desktop.heard(Heard::Holding(lock, Ok(socket.clone())), false));
        assert!(matches!(desktop.reader(), Reader::This));

        let mut over_ssh = Desktop {
            serving: None,
            role: Role::Elsewhere,
            events,
        };
        assert!(!over_ssh.heard(Heard::Peering(socket), false));
        assert!(matches!(over_ssh.reader(), Reader::Nobody(NO_SPEAKERS)));
    }

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

        let refused = Told::issue("lune", &Err(Unfiled::Refused("gh: no labels".to_owned())));
        let said = Told {
            title: "No issue filed in lune".to_owned(),
            body: "gh: no labels; i has your words again".to_owned(),
            came: Came::Unfiled,
        };
        assert_eq!(refused, said);

        let doubt = Unfiled::InDoubt("gh: it did not answer in time".to_owned());
        let doubted = Told::issue("lune", &Err(doubt));
        assert_eq!(doubted.title, "An issue in lune may be filed");
        assert_eq!(
            doubted.body,
            "gh: it did not answer in time; i has your words, look first"
        );
        assert_eq!(doubted.came, Came::Unfiled);
    }

    #[test]
    fn a_lune_that_may_not_hear_in_the_folder_still_reads_aloud_and_says_why() {
        let folder = tempfile::tempdir().expect("a folder");
        let (events, heard) = mpsc::channel();
        let mut desktop = Desktop {
            serving: None,
            role: Role::Undecided,
            events,
        };

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
