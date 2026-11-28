use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Server;
use tonic::transport::server::{Connected, TcpConnectInfo};

use crate::devices::{self, Allowed, DeviceId};
use crate::log;

pub const PORT: u16 = lune_protocol::TAILNET_PORT;

#[cfg(target_os = "macos")]
const TAILSCALED: &str = "/var/run/tailscaled.socket";

#[cfg(not(target_os = "macos"))]
const TAILSCALED: &str = "/var/run/tailscale/tailscaled.sock";

const LOCAL_API: &str = "/localapi/v0/";

const ANSWER_MAX: u32 = 1024 * 1024;

const ASKED_WITHIN: Duration = Duration::from_secs(2);

const ADMITTING_MAX: usize = 16;

const ADMITTED_MAX: usize = 16;

const CONNECTED_MAX: usize = 16;

const PINGED_EVERY: Duration = Duration::from_secs(20);

const PONGED_WITHIN: Duration = Duration::from_secs(10);

const UNACCEPTED_PAUSE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u64")]
pub struct UserId(NonZeroU64);

impl TryFrom<u64> for UserId {
    type Error = &'static str;

    fn try_from(id: u64) -> Result<UserId, &'static str> {
        NonZeroU64::new(id).map(UserId).ok_or("user 0 is no one")
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "user {}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "IpAddr")]
struct TailnetAddress(IpAddr);

impl TryFrom<IpAddr> for TailnetAddress {
    type Error = String;

    fn try_from(address: IpAddr) -> Result<TailnetAddress, String> {
        let inside = match address {
            IpAddr::V4(address) => {
                let [first, second, _, _] = address.octets();
                first == 100 && (64..128).contains(&second)
            }
            IpAddr::V6(address) => {
                let [first, second, third, ..] = address.segments();
                [first, second, third] == [0xfd7a, 0x115c, 0xa1e0]
            }
        };
        if inside {
            Ok(TailnetAddress(address))
        } else {
            Err(format!("{address} is not a tailnet address"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    user: UserId,
    addresses: BTreeSet<TailnetAddress>,
}

enum Seen {
    Owned(Machine),
    Unowned(String),
    Unseen(String),
}

enum Listening {
    Not,
    On {
        machine: Machine,
        accepting: JoinSet<()>,
        _epoch: watch::Sender<()>,
    },
}

impl Listening {
    fn is_for(&self, machine: &Machine) -> bool {
        matches!(self, Listening::On { machine: on, .. } if on == machine)
    }

    async fn stop(&mut self) {
        if let Listening::On { accepting, .. } = self {
            accepting.shutdown().await;
        }
        *self = Listening::Not;
    }
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState")]
    state: String,
    #[serde(rename = "Self")]
    machine: Option<Node>,
}

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "UserID")]
    user: UserId,
    #[serde(rename = "TailscaleIPs", default)]
    addresses: BTreeSet<TailnetAddress>,
    #[serde(rename = "Tags", default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Whois {
    #[serde(rename = "Node")]
    node: Peer,
}

#[derive(Deserialize)]
struct Peer {
    #[serde(rename = "User")]
    user: UserId,
    #[serde(rename = "StableID")]
    id: DeviceId,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Tags", default)]
    tags: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Device {
    user: UserId,
    id: DeviceId,
    name: String,
}

impl Device {
    fn line(&self) -> String {
        let name = self.name.trim_end_matches('.').escape_debug().to_string();
        let line = if name.is_empty() {
            self.id.to_string()
        } else {
            format!("{} {name}", self.id)
        };
        assert!(
            !line.chars().any(char::is_control),
            "{line:?} goes into luned.log as one line"
        );
        line
    }
}

pub struct Admitted {
    stream: TcpStream,
    listened: Listened,
    _connection: OwnedSemaphorePermit,
}

enum Listened {
    On(Pin<Box<dyn Future<Output = ()> + Send>>),
    Ended,
}

#[derive(Default)]
struct Followed(Option<Result<Allowed, String>>);

impl Followed {
    fn changed(&mut self, now: Result<Allowed, String>) -> bool {
        let changed = self.0.as_ref().is_some_and(|before| *before != now);
        self.0 = Some(now);
        changed
    }
}

#[derive(Clone)]
struct Admission {
    tailscaled: &'static Path,
    devices_file: Arc<Path>,
    machine: Arc<Machine>,
    admitting: Arc<Semaphore>,
    connected: Arc<Semaphore>,
    admitted: mpsc::Sender<io::Result<Admitted>>,
    epoch: watch::Receiver<()>,
}

pub fn in_own_home() -> Result<(), String> {
    let uid = rustix::process::getuid().as_raw();
    let Some(you) = uzers::get_user_by_uid(uid) else {
        return Err(format!("user {uid} has no home in the user database"));
    };
    let own = uzers::os::unix::UserExt::home_dir(&you);
    match std::env::var_os("HOME") {
        Some(home) if Path::new(&home) == own => Ok(()),
        Some(home) => Err(format!(
            "HOME is {}, not {}",
            Path::new(&home).display(),
            own.display()
        )),
        None => Err("HOME is unset".to_owned()),
    }
}

pub fn server() -> Server {
    Server::builder()
        .http2_keepalive_interval(Some(PINGED_EVERY))
        .http2_keepalive_timeout(Some(PONGED_WITHIN))
}

pub fn incoming(sweep: Duration, devices_file: PathBuf) -> ReceiverStream<io::Result<Admitted>> {
    assert!(
        !sweep.is_zero(),
        "luned asks tailscaled again some time later, not at once"
    );
    assert!(
        devices_file.is_absolute(),
        "{} is absolute",
        devices_file.display()
    );
    let (admitted, coming) = mpsc::channel(ADMITTED_MAX);
    tokio::spawn(admit_all(
        Path::new(TAILSCALED),
        devices_file.into(),
        admitted,
        sweep,
    ));
    ReceiverStream::new(coming)
}

async fn admit_all(
    tailscaled: &'static Path,
    devices_file: Arc<Path>,
    admitted: mpsc::Sender<io::Result<Admitted>>,
    sweep: Duration,
) {
    let admitting = Arc::new(Semaphore::new(ADMITTING_MAX));
    let connected = Arc::new(Semaphore::new(CONNECTED_MAX));
    let mut listening = Listening::Not;
    let mut followed = Followed::default();
    let mut said: Option<String> = None;
    loop {
        let changed = followed.changed(allowed_in(&devices_file).await);
        if changed && matches!(listening, Listening::On { .. }) {
            let devices_file = devices_file.display();
            log::say(&format!(
                "{devices_file} changed, so each device is admitted again"
            ));
            listening.stop().await;
        }
        let unlistened = match see_machine(tailscaled).await {
            Seen::Owned(machine) if listening.is_for(&machine) => None,
            Seen::Owned(machine) => {
                listening.stop().await;
                match listen(&machine).await {
                    Ok(listeners) => {
                        say_listening(&machine, &devices_file);
                        let (epoch, admitted_in) = watch::channel(());
                        let admission = Admission {
                            tailscaled,
                            devices_file: Arc::clone(&devices_file),
                            machine: Arc::new(machine.clone()),
                            admitting: Arc::clone(&admitting),
                            connected: Arc::clone(&connected),
                            admitted: admitted.clone(),
                            epoch: admitted_in,
                        };
                        let mut accepting = JoinSet::new();
                        for listener in listeners {
                            accepting.spawn(accept(listener, admission.clone()));
                        }
                        listening = Listening::On {
                            machine,
                            accepting,
                            _epoch: epoch,
                        };
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            Seen::Unowned(why) => {
                listening.stop().await;
                Some(why)
            }
            Seen::Unseen(why) => Some(why),
        };
        match unlistened {
            None => said = None,
            Some(why) if said.as_ref() == Some(&why) => {}
            Some(why) => {
                if matches!(listening, Listening::On { .. }) {
                    log::say(&format!("{why}, so luned listens on the tailnet as it did"));
                } else {
                    log::say(&format!(
                        "{why}, so no device reaches luned over the tailnet yet"
                    ));
                }
                said = Some(why);
            }
        }
        tokio::select! {
            () = admitted.closed() => return,
            () = tokio::time::sleep(sweep) => {},
        }
    }
}

fn say_listening(machine: &Machine, devices_file: &Path) {
    let addresses = machine
        .addresses
        .iter()
        .map(|address| SocketAddr::new(address.0, PORT));
    let addresses: Vec<String> = addresses.map(|address| address.to_string()).collect();
    let (addresses, user, file) = (
        addresses.join(" and "),
        machine.user,
        devices_file.display(),
    );
    log::say(&format!(
        "listening on {addresses} for the devices of {user} that {file} lets in"
    ));
}

async fn see_machine(tailscaled: &Path) -> Seen {
    match ask(tailscaled, "/localapi/v0/status?peers=false").await {
        Err(why) => Seen::Unseen(why),
        Ok(body) => match machine_of(&body) {
            Ok(machine) => Seen::Owned(machine),
            Err(why) => Seen::Unowned(why),
        },
    }
}

async fn listen(machine: &Machine) -> Result<Vec<TcpListener>, String> {
    let mut listeners = Vec::with_capacity(machine.addresses.len());
    for address in &machine.addresses {
        let address = SocketAddr::new(address.0, PORT);
        let listener = TcpListener::bind(address).await;
        listeners.push(listener.map_err(|error| format!("{address} is not bound: {error}"))?);
    }
    assert_eq!(listeners.len(), machine.addresses.len());
    Ok(listeners)
}

async fn accept(listener: TcpListener, admission: Admission) {
    loop {
        let accepted = tokio::select! {
            () = admission.admitted.closed() => return,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                log::say(&format!("a tailnet connection is not accepted: {error}"));
                tokio::time::sleep(UNACCEPTED_PAUSE).await;
                continue;
            }
        };
        let Ok(connection) = Arc::clone(&admission.connected).try_acquire_owned() else {
            log::say(&format!(
                "{peer} is turned away: {CONNECTED_MAX} devices are connected"
            ));
            continue;
        };
        let Ok(lookup) = Arc::clone(&admission.admitting).try_acquire_owned() else {
            log::say(&format!(
                "{peer} is turned away: {ADMITTING_MAX} are being looked up"
            ));
            continue;
        };
        if let Err(error) = stream.set_nodelay(true) {
            log::say(&format!(
                "{peer}'s answers wait to be sent with more: {error}"
            ));
        }
        let admission = admission.clone();
        tokio::spawn(async move {
            let (tailscaled, devices_file) = (admission.tailscaled, &*admission.devices_file);
            match admits(tailscaled, devices_file, &admission.machine, peer).await {
                Ok(device) => {
                    log::say(&format!("{peer} is admitted as {}", device.line()));
                    let admitted = Admitted::new(stream, admission.epoch.clone(), connection);
                    let _ = admission.admitted.send(Ok(admitted)).await;
                }
                Err(why) => log::say(&format!("{peer} is turned away: {why}")),
            }
            drop(lookup);
        });
    }
}

async fn allowed_in(devices_file: &Path) -> Result<Allowed, String> {
    let read = devices_file.to_owned();
    let allowed = tokio::task::spawn_blocking(move || devices::allowed(&read)).await;
    allowed.map_err(|error| format!("{} is not read: {error}", devices_file.display()))?
}

async fn admits(
    tailscaled: &Path,
    devices_file: &Path,
    machine: &Machine,
    peer: SocketAddr,
) -> Result<Device, String> {
    let Ok(address) = TailnetAddress::try_from(peer.ip()) else {
        return Err("it is not on the tailnet".to_owned());
    };
    if machine.addresses.contains(&address) {
        return Err("it is this machine, where any of its users may connect".to_owned());
    }
    let user = machine.user;
    let path = format!("/localapi/v0/whois?addr={}", query_of(address, peer.port()));
    let device = device_of(&ask(tailscaled, &path).await?)?;
    if device.user != user {
        return Err(format!(
            "it is {}'s, and luned answers only {user}",
            device.user
        ));
    }
    if allowed_in(devices_file).await?.lets_in(&device.id) {
        return Ok(device);
    }
    let (devices_file, line) = (devices_file.display(), device.line());
    Err(format!(
        "it is not in {devices_file}; the line \"{line}\" there lets it in"
    ))
}

fn query_of(TailnetAddress(address): TailnetAddress, port: u16) -> String {
    let query = match address {
        IpAddr::V4(address) => format!("{address}%3A{port}"),
        IpAddr::V6(address) => {
            let address = address.to_string().replace(':', "%3A");
            format!("%5B{address}%5D%3A{port}")
        }
    };
    let word = |byte: u8| byte.is_ascii_hexdigit() || byte == b'.' || byte == b'%';
    assert!(query.bytes().all(word), "{query} is one query word");
    query
}

async fn ask(tailscaled: &Path, path: &str) -> Result<Vec<u8>, String> {
    assert!(
        path.starts_with(LOCAL_API),
        "{path} is a path of tailscaled's local API"
    );
    let one_word = |c: char| !c.is_ascii_whitespace() && !c.is_ascii_control();
    assert!(
        path.chars().all(one_word),
        "{path:?} is one word of a request line"
    );
    let unasked = |error: io::Error| {
        format!(
            "tailscaled at {} is not asked: {error}",
            tailscaled.display()
        )
    };
    let asked = async {
        let mut stream = UnixStream::connect(tailscaled).await?;
        let request = format!("GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut answer = Vec::new();
        stream
            .take(u64::from(ANSWER_MAX) + 1)
            .read_to_end(&mut answer)
            .await?;
        Ok(answer)
    };
    let Ok(answer) = timeout(ASKED_WITHIN, asked).await else {
        let secs = ASKED_WITHIN.as_secs();
        return Err(format!(
            "tailscaled at {} answered nothing in {secs}s",
            tailscaled.display()
        ));
    };
    let answer = answer.map_err(unasked)?;
    if answer.len() > ANSWER_MAX as usize {
        return Err(format!("tailscaled answered more than {ANSWER_MAX} bytes"));
    }
    Ok(body_of(&answer)?.to_vec())
}

fn body_of(answer: &[u8]) -> Result<&[u8], String> {
    let Some(end) = answer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Err("tailscaled's answer has no end to its headers".to_owned());
    };
    let (head, body) = (&answer[..end], &answer[end + 4..]);
    let head = String::from_utf8_lossy(head);
    let status = head.lines().next().unwrap_or_default();
    if status.split(' ').nth(1) == Some("200") {
        return Ok(body);
    }
    let said = String::from_utf8_lossy(body);
    Err(format!("tailscaled answered {status}: {}", said.trim()))
}

fn machine_of(body: &[u8]) -> Result<Machine, String> {
    let status: Status = serde_json::from_slice(body)
        .map_err(|error| format!("tailscaled's status does not read: {error}"))?;
    if status.state != "Running" {
        return Err(format!("Tailscale is {}, not running", status.state));
    }
    let Some(Node {
        user,
        addresses,
        tags,
    }) = status.machine
    else {
        return Err("tailscaled names no node of this machine".to_owned());
    };
    if !tags.is_empty() {
        return Err(format!(
            "this machine is tagged {}, so no one user owns it",
            tags.join(", ")
        ));
    }
    if addresses.is_empty() {
        return Err("this machine has no tailnet address".to_owned());
    }
    Ok(Machine { user, addresses })
}

fn device_of(body: &[u8]) -> Result<Device, String> {
    let whois: Whois = serde_json::from_slice(body)
        .map_err(|error| format!("tailscaled's whois does not read: {error}"))?;
    let Peer {
        user,
        id,
        name,
        tags,
    } = whois.node;
    if !tags.is_empty() {
        return Err(format!(
            "it is tagged {}, so no one user owns it",
            tags.join(", ")
        ));
    }
    Ok(Device { user, id, name })
}

impl Admitted {
    fn new(
        stream: TcpStream,
        mut epoch: watch::Receiver<()>,
        connection: OwnedSemaphorePermit,
    ) -> Admitted {
        let unlistened = async move { while epoch.changed().await.is_ok() {} };
        Admitted {
            stream,
            listened: Listened::On(Box::pin(unlistened)),
            _connection: connection,
        }
    }

    fn ended(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if let Listened::On(unlistened) = &mut self.listened {
            if unlistened.as_mut().poll(context).is_pending() {
                return Ok(());
            }
            self.listened = Listened::Ended;
        }
        let why = "the machine it was admitted for is no longer listened on";
        Err(io::Error::new(io::ErrorKind::ConnectionAborted, why))
    }
}

impl AsyncRead for Admitted {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let admitted = self.get_mut();
        if let Err(error) = admitted.ended(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut admitted.stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for Admitted {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let admitted = self.get_mut();
        if let Err(error) = admitted.ended(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut admitted.stream).poll_write(context, buffer)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let admitted = self.get_mut();
        if let Err(error) = admitted.ended(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut admitted.stream).poll_write_vectored(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let admitted = self.get_mut();
        if let Err(error) = admitted.ended(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut admitted.stream).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(context)
    }
}

impl Connected for Admitted {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> TcpConnectInfo {
        self.stream.connect_info()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const STATUS: &str = r#"{"BackendState":"Running","Self":{"UserID":7,"HostName":"nixos","TailscaleIPs":["100.101.2.3","fd7a:115c:a1e0::1"]},"Peer":null}"#;

    fn user(id: u64) -> UserId {
        UserId::try_from(id).unwrap()
    }

    fn answered(status: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.0 {status}\r\nContent-Type: application/json\r\n\r\n{body}").into_bytes()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn tailscaled(
        folder: &Path,
        answers: &'static [(&'static str, &'static str)],
    ) -> std::path::PathBuf {
        let socket = folder.join("tailscaled.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut asked = vec![0; 4096];
                let read = stream.read(&mut asked).await.unwrap();
                let asked = String::from_utf8_lossy(&asked[..read]).into_owned();
                let path = asked.split(' ').nth(1).unwrap_or_default().to_owned();
                let found = answers.iter().find(|(wanted, _)| path == *wanted);
                let answer = match found {
                    Some((_, body)) => answered("200 OK", body),
                    None => answered("404 Not Found", "no such path"),
                };
                stream.write_all(&answer).await.unwrap();
            }
        });
        socket
    }

    #[test]
    fn the_body_follows_a_200() {
        let answer = answered("200 OK", "{}");
        assert_eq!(body_of(&answer).unwrap(), b"{}");
    }

    #[test]
    fn another_status_says_what_tailscaled_said() {
        let answer = answered("403 Forbidden", "access denied\n");
        assert_eq!(
            body_of(&answer).unwrap_err(),
            "tailscaled answered HTTP/1.0 403 Forbidden: access denied"
        );
    }

    #[test]
    fn an_answer_cut_in_its_headers_reads_as_none() {
        assert!(body_of(b"HTTP/1.0 200 OK\r\nContent-").is_err());
    }

    #[test]
    fn the_status_names_this_machine_s_user_and_addresses() {
        let machine = machine_of(STATUS.as_bytes()).unwrap();
        let addresses = [
            IpAddr::V4(Ipv4Addr::new(100, 101, 2, 3)),
            "fd7a:115c:a1e0::1".parse().unwrap(),
        ];
        let addresses = addresses.map(|address| TailnetAddress::try_from(address).unwrap());
        assert_eq!(
            machine,
            Machine {
                user: user(7),
                addresses: BTreeSet::from(addresses)
            }
        );
    }

    #[test]
    fn an_address_named_twice_or_in_another_order_is_the_same_machine() {
        let again = r#"{"BackendState":"Running","Self":{"UserID":7,"TailscaleIPs":["fd7a:115c:a1e0::1","100.101.2.3","100.101.2.3"]}}"#;
        assert_eq!(
            machine_of(again.as_bytes()).unwrap(),
            machine_of(STATUS.as_bytes()).unwrap()
        );
    }

    #[test]
    fn what_tailscaled_says_decides_and_what_it_does_not_say_keeps() {
        const ANSWERS: &[(&str, &str)] = &[(
            "/localapi/v0/status?peers=false",
            r#"{"BackendState":"Running","Self":{"UserID":7,"TailscaleIPs":["100.101.2.3"],"Tags":["tag:server"]}}"#,
        )];
        const UNREAD: &[(&str, &str)] =
            &[("/localapi/v0/status?peers=false", r#"{"BackendState":"#)];
        const OWNED: &[(&str, &str)] = &[("/localapi/v0/status?peers=false", STATUS)];
        runtime().block_on(async {
            let tagged = tempfile::tempdir().unwrap();
            let unread = tempfile::tempdir().unwrap();
            let owned = tempfile::tempdir().unwrap();
            let missing = tempfile::tempdir().unwrap();
            let tagged = tailscaled(tagged.path(), ANSWERS);
            let unread = tailscaled(unread.path(), UNREAD);
            let owned = tailscaled(owned.path(), OWNED);
            let missing = missing.path().join("tailscaled.sock");
            assert!(
                matches!(see_machine(&owned).await, Seen::Owned(machine) if machine.user == user(7))
            );
            let Seen::Unowned(why) = see_machine(&tagged).await else {
                panic!("a tagged machine is owned")
            };
            assert_eq!(
                why,
                "this machine is tagged tag:server, so no one user owns it"
            );
            assert!(matches!(see_machine(&unread).await, Seen::Unowned(_)));
            assert!(matches!(see_machine(&missing).await, Seen::Unseen(_)));
        });
    }

    #[test]
    fn a_connection_ends_with_the_machine_it_was_admitted_for() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut device = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let connection = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
            let (epoch, admitted_in) = watch::channel(());
            let mut admitted = Admitted::new(stream, admitted_in, connection);
            device.write_all(b"ping").await.unwrap();
            let mut read = [0; 4];
            admitted.read_exact(&mut read).await.unwrap();
            assert_eq!(&read, b"ping");
            drop(epoch);
            assert_eq!(
                admitted.read(&mut read).await.unwrap_err().kind(),
                io::ErrorKind::ConnectionAborted
            );
            assert_eq!(
                admitted.write(b"pong").await.unwrap_err().kind(),
                io::ErrorKind::ConnectionAborted
            );
            assert_eq!(
                admitted.read(&mut read).await.unwrap_err().kind(),
                io::ErrorKind::ConnectionAborted
            );
        });
    }

    #[test]
    fn a_connection_waiting_to_read_ends_once_its_machine_is_no_longer_listened_on() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _device = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let connection = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
            let (epoch, admitted_in) = watch::channel(());
            let mut admitted = Admitted::new(stream, admitted_in, connection);
            let reading = tokio::spawn(async move { admitted.read(&mut [0; 4]).await });
            tokio::task::yield_now().await;
            assert!(
                !reading.is_finished(),
                "the device sent nothing, so the read waits"
            );
            drop(epoch);
            let read = lune_waits::awaited(
                "the read ends with the epoch, not with the next byte",
                reading,
            )
            .await;
            let read = read.unwrap();
            assert_eq!(read.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        });
    }

    #[test]
    fn a_listening_that_stops_ends_its_connections() {
        runtime().block_on(async {
            let machine = machine_of(STATUS.as_bytes()).unwrap();
            let (epoch, admitted_in) = watch::channel(());
            let mut listening = Listening::On {
                machine,
                accepting: JoinSet::new(),
                _epoch: epoch,
            };
            assert!(listening.is_for(&machine_of(STATUS.as_bytes()).unwrap()));
            listening.stop().await;
            assert!(matches!(listening, Listening::Not));
            assert!(admitted_in.has_changed().is_err());
        });
    }

    #[test]
    fn a_stopped_tailscale_has_no_machine() {
        let stopped = r#"{"BackendState":"Stopped","Self":{"UserID":7,"TailscaleIPs":[]}}"#;
        assert_eq!(
            machine_of(stopped.as_bytes()).unwrap_err(),
            "Tailscale is Stopped, not running"
        );
    }

    #[test]
    fn a_node_with_no_address_is_not_listened_on() {
        let bare = r#"{"BackendState":"Running","Self":{"UserID":7}}"#;
        assert_eq!(
            machine_of(bare.as_bytes()).unwrap_err(),
            "this machine has no tailnet address"
        );
    }

    #[test]
    fn a_tagged_machine_is_not_listened_on() {
        let tagged = r#"{"BackendState":"Running","Self":{"UserID":7,"TailscaleIPs":["100.101.2.3"],"Tags":["tag:server"]}}"#;
        let refused = machine_of(tagged.as_bytes()).unwrap_err();
        assert_eq!(
            refused,
            "this machine is tagged tag:server, so no one user owns it"
        );
    }

    #[test]
    fn user_0_is_no_one() {
        let nobody =
            r#"{"BackendState":"Running","Self":{"UserID":0,"TailscaleIPs":["100.101.2.3"]}}"#;
        let refused = machine_of(nobody.as_bytes()).unwrap_err();
        assert!(refused.contains("user 0 is no one"), "{refused}");
    }

    #[test]
    fn an_address_off_the_tailnet_is_not_listened_on() {
        let lan =
            r#"{"BackendState":"Running","Self":{"UserID":7,"TailscaleIPs":["192.168.1.2"]}}"#;
        let refused = machine_of(lan.as_bytes()).unwrap_err();
        assert!(
            refused.contains("192.168.1.2 is not a tailnet address"),
            "{refused}"
        );
    }

    #[test]
    fn the_tailnet_is_100_64_0_0_10_and_fd7a_115c_a1e0_48() {
        let inside = [
            "100.64.0.0",
            "100.127.255.255",
            "fd7a:115c:a1e0::",
            "fd7a:115c:a1e0:ffff::1",
        ];
        let outside = [
            "100.63.255.255",
            "100.128.0.0",
            "10.0.0.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ];
        let tailnet = |address: &str| TailnetAddress::try_from(address.parse::<IpAddr>().unwrap());
        for address in inside {
            assert!(tailnet(address).is_ok(), "{address}");
        }
        for address in outside {
            assert_eq!(
                tailnet(address),
                Err(format!("{address} is not a tailnet address"))
            );
        }
    }

    #[test]
    fn a_peer_is_asked_for_in_one_query_word() {
        let tailnet =
            |address: &str| TailnetAddress::try_from(address.parse::<IpAddr>().unwrap()).unwrap();
        assert_eq!(
            query_of(tailnet("100.101.2.4"), 51000),
            "100.101.2.4%3A51000"
        );
        let v6 = query_of(tailnet("fd7a:115c:a1e0::2"), 51000);
        assert_eq!(v6, "%5Bfd7a%3A115c%3Aa1e0%3A%3A2%5D%3A51000");
    }

    #[test]
    fn only_a_device_of_this_machine_s_user_that_the_file_lets_in_is_admitted() {
        const ANSWERS: &[(&str, &str)] = &[
            (
                "/localapi/v0/whois?addr=100.101.2.4%3A51000",
                r#"{"Node":{"User":7,"StableID":"nPhone4CNTRL","Name":"phone.tail1.ts.net."}}"#,
            ),
            (
                "/localapi/v0/whois?addr=100.101.2.5%3A51000",
                r#"{"Node":{"User":8,"StableID":"nTheirs5CNTRL","Name":"theirs.tail1.ts.net."}}"#,
            ),
            (
                "/localapi/v0/whois?addr=100.101.2.7%3A51000",
                r#"{"Node":{"User":7,"StableID":"nServer7CNTRL","Name":"server","Tags":["tag:ci"]}}"#,
            ),
            (
                "/localapi/v0/whois?addr=100.101.2.8%3A51000",
                r#"{"Node":{"User":7,"StableID":"nPad8CNTRL","Name":"pad.tail1.ts.net.\nluned: spoofed"}}"#,
            ),
            (
                "/localapi/v0/whois?addr=100.101.2.9%3A51000",
                r#"{"Node":{"User":7,"Name":"unstable"}}"#,
            ),
        ];
        runtime().block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let socket = tailscaled(folder.path(), ANSWERS);
            let file = folder.path().join(devices::FILE);
            let at = |address: &str| address.parse::<SocketAddr>().unwrap();
            let machine = machine_of(STATUS.as_bytes()).unwrap();
            let admits = |peer: &str| admits(&socket, &file, &machine, at(peer));
            let unlisted = |line: &str| format!("it is not in {}; the line \"{line}\" there lets it in", file.display());
            assert_eq!(admits("100.101.2.4:51000").await, Err(unlisted("nPhone4CNTRL phone.tail1.ts.net")));
            std::fs::write(&file, "nPhone4CNTRL my phone\nnTheirs5CNTRL\nnServer7CNTRL\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            let admitted = admits("100.101.2.4:51000").await.map(|device| device.line());
            assert_eq!(admitted, Ok("nPhone4CNTRL phone.tail1.ts.net".to_owned()));
            let theirs = admits("100.101.2.5:51000").await.unwrap_err();
            assert_eq!(theirs, "it is user 8's, and luned answers only user 7");
            let tagged = admits("100.101.2.7:51000").await.unwrap_err();
            assert_eq!(tagged, "it is tagged tag:ci, so no one user owns it");
            let spoofed = admits("100.101.2.8:51000").await.unwrap_err();
            assert_eq!(spoofed, unlisted(r"nPad8CNTRL pad.tail1.ts.net.\nluned: spoofed"));
            let unstable = admits("100.101.2.9:51000").await.unwrap_err();
            assert!(unstable.starts_with("tailscaled's whois does not read"), "{unstable}");
            let unknown = admits("100.101.2.6:51000").await.unwrap_err();
            assert!(unknown.starts_with("tailscaled answered HTTP/1.0 404"), "{unknown}");
            std::fs::write(&file, "nPhone4CNTRL\nphone.tail1.ts.net\n").unwrap();
            let unread = admits("100.101.2.4:51000").await.unwrap_err();
            let why = "line 2: \"phone.tail1.ts.net\" is not a device's StableID: it has a byte that is no letter or digit";
            assert_eq!(unread, format!("{}: {why}", file.display()));
        });
    }

    #[test]
    fn a_change_to_the_devices_file_is_seen_once_and_its_first_read_is_none() {
        let listed = |text: &str| devices::allowed_of(text);
        let mut followed = Followed::default();
        assert!(!followed.changed(listed("")));
        assert!(!followed.changed(listed("")));
        assert!(followed.changed(listed("nPhone4CNTRL")));
        assert!(!followed.changed(listed("nPhone4CNTRL my phone")));
        assert!(followed.changed(Err("it is not read".to_owned())));
        assert!(!followed.changed(Err("it is not read".to_owned())));
        assert!(followed.changed(listed("")));
    }

    #[test]
    fn a_peer_off_the_tailnet_is_turned_away_unasked() {
        runtime().block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let missing = folder.path().join("tailscaled.sock");
            let file = folder.path().join(devices::FILE);
            let lan: SocketAddr = "192.168.1.5:51000".parse().unwrap();
            let machine = machine_of(STATUS.as_bytes()).unwrap();
            assert_eq!(
                admits(&missing, &file, &machine, lan).await.unwrap_err(),
                "it is not on the tailnet"
            );
        });
    }

    #[test]
    fn this_machine_is_turned_away_unasked_as_any_of_its_users_may_connect() {
        runtime().block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let missing = folder.path().join("tailscaled.sock");
            let file = folder.path().join(devices::FILE);
            let machine = machine_of(STATUS.as_bytes()).unwrap();
            let refusal = "it is this machine, where any of its users may connect";
            for own in ["100.101.2.3:51000", "[fd7a:115c:a1e0::1]:51000"] {
                let own: SocketAddr = own.parse().unwrap();
                assert_eq!(
                    admits(&missing, &file, &machine, own).await.unwrap_err(),
                    refusal,
                    "{own}"
                );
            }
        });
    }

    #[test]
    fn a_missing_tailscaled_is_said() {
        runtime().block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let missing = folder.path().join("tailscaled.sock");
            let unasked = ask(&missing, "/localapi/v0/status").await.unwrap_err();
            assert!(unasked.contains("is not asked"), "{unasked}");
        });
    }
}
