use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;

use crate::log;

pub const PORT: u16 = 7473;

#[cfg(target_os = "macos")]
const TAILSCALED: &str = "/var/run/tailscaled.socket";

#[cfg(not(target_os = "macos"))]
const TAILSCALED: &str = "/var/run/tailscale/tailscaled.sock";

const ANSWER_MAX: usize = 1024 * 1024;

const ASKED_WITHIN: Duration = Duration::from_secs(2);

const ADMITTING_MAX: usize = 16;

const ADMITTED_MAX: usize = 16;

const UNACCEPTED_PAUSE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Login(u64);

impl fmt::Display for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "login {}", self.0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Own {
    login: Login,
    addresses: Vec<IpAddr>,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState")]
    state: String,
    #[serde(rename = "Self")]
    own: Option<Node>,
}

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "UserID")]
    login: Login,
    #[serde(rename = "TailscaleIPs", default)]
    addresses: Vec<IpAddr>,
}

#[derive(Deserialize)]
struct Whois {
    #[serde(rename = "Node")]
    node: Peer,
}

#[derive(Deserialize)]
struct Peer {
    #[serde(rename = "User")]
    login: Login,
}

pub fn in_own_home() -> Result<(), String> {
    let uid = rustix::process::getuid().as_raw();
    let Some(you) = uzers::get_user_by_uid(uid) else {
        return Err(format!("user {uid} has no home in the user database"));
    };
    let own = uzers::os::unix::UserExt::home_dir(&you);
    match std::env::var_os("HOME") {
        Some(home) if Path::new(&home) == own => Ok(()),
        Some(home) => Err(format!("HOME is {}, not {}", Path::new(&home).display(), own.display())),
        None => Err("HOME is unset".to_owned()),
    }
}

pub fn incoming(sweep: Duration) -> ReceiverStream<io::Result<TcpStream>> {
    let (admitted, coming) = mpsc::channel(ADMITTED_MAX);
    tokio::spawn(admit_all(Path::new(TAILSCALED), admitted, sweep));
    ReceiverStream::new(coming)
}

async fn admit_all(
    tailscaled: &'static Path,
    admitted: mpsc::Sender<io::Result<TcpStream>>,
    sweep: Duration,
) {
    let mut said: Option<String> = None;
    let (own, listeners) = loop {
        match bound(tailscaled).await {
            Ok(bound) => break bound,
            Err(why) if said.as_ref() != Some(&why) => {
                log::say(&format!("{why}, so no device reaches luned over the tailnet yet"));
                said = Some(why);
            },
            Err(_) => {},
        }
        tokio::select! {
            () = admitted.closed() => return,
            () = tokio::time::sleep(sweep) => {},
        }
    };
    let addresses = own.addresses.iter().map(|address| SocketAddr::new(*address, PORT));
    let addresses: Vec<String> = addresses.map(|address| address.to_string()).collect();
    let login = own.login;
    log::say(&format!("listening on {} for the devices of {login}", addresses.join(" and ")));
    let admitting = Arc::new(Semaphore::new(ADMITTING_MAX));
    for listener in listeners {
        tokio::spawn(accept(listener, tailscaled, login, Arc::clone(&admitting), admitted.clone()));
    }
}

async fn bound(tailscaled: &Path) -> Result<(Own, Vec<TcpListener>), String> {
    let own = own_of(&ask(tailscaled, "/localapi/v0/status?peers=false").await?)?;
    let mut listeners = Vec::with_capacity(own.addresses.len());
    for address in &own.addresses {
        let address = SocketAddr::new(*address, PORT);
        let listener = TcpListener::bind(address).await;
        listeners.push(listener.map_err(|error| format!("{address} is not bound: {error}"))?);
    }
    Ok((own, listeners))
}

async fn accept(
    listener: TcpListener,
    tailscaled: &'static Path,
    own: Login,
    admitting: Arc<Semaphore>,
    admitted: mpsc::Sender<io::Result<TcpStream>>,
) {
    loop {
        let accepted = tokio::select! {
            () = admitted.closed() => return,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                log::say(&format!("a tailnet connection is not accepted: {error}"));
                tokio::time::sleep(UNACCEPTED_PAUSE).await;
                continue;
            },
        };
        let Ok(permit) = Arc::clone(&admitting).try_acquire_owned() else {
            log::say(&format!("{peer} is turned away: {ADMITTING_MAX} are being looked up"));
            continue;
        };
        let admitted = admitted.clone();
        tokio::spawn(async move {
            let admits = admits(tailscaled, own, peer).await;
            drop(permit);
            match admits {
                Ok(()) => {
                    let _ = admitted.send(Ok(stream)).await;
                },
                Err(why) => log::say(&format!("{peer} is turned away: {why}")),
            }
        });
    }
}

async fn admits(tailscaled: &Path, own: Login, peer: SocketAddr) -> Result<(), String> {
    let path = format!("/localapi/v0/whois?addr={}", query_of(peer));
    let login = login_of(&ask(tailscaled, &path).await?)?;
    if login == own {
        Ok(())
    } else {
        Err(format!("it is {login}'s, and luned answers only {own}"))
    }
}

fn query_of(peer: SocketAddr) -> String {
    peer.to_string().replace('[', "%5B").replace(']', "%5D").replace(':', "%3A")
}

async fn ask(tailscaled: &Path, path: &str) -> Result<Vec<u8>, String> {
    let unasked = |error: io::Error| format!("tailscaled at {} is not asked: {error}", tailscaled.display());
    let asked = async {
        let mut stream = UnixStream::connect(tailscaled).await?;
        let request = format!("GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut answer = Vec::new();
        let limit = u64::try_from(ANSWER_MAX + 1).unwrap_or(u64::MAX);
        stream.take(limit).read_to_end(&mut answer).await?;
        Ok(answer)
    };
    let Ok(answer) = timeout(ASKED_WITHIN, asked).await else {
        let secs = ASKED_WITHIN.as_secs();
        return Err(format!("tailscaled at {} answered nothing in {secs}s", tailscaled.display()));
    };
    let answer = answer.map_err(unasked)?;
    if answer.len() > ANSWER_MAX {
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

fn own_of(body: &[u8]) -> Result<Own, String> {
    let status: Status = serde_json::from_slice(body)
        .map_err(|error| format!("tailscaled's status does not read: {error}"))?;
    if status.state != "Running" {
        return Err(format!("Tailscale is {}, not running", status.state));
    }
    let Some(Node { login, addresses }) = status.own else {
        return Err("tailscaled names no node of this machine".to_owned());
    };
    if addresses.is_empty() {
        return Err("this machine has no tailnet address".to_owned());
    }
    Ok(Own { login, addresses })
}

fn login_of(body: &[u8]) -> Result<Login, String> {
    let whois: Whois = serde_json::from_slice(body)
        .map_err(|error| format!("tailscaled's whois does not read: {error}"))?;
    Ok(whois.node.login)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const STATUS: &str = r#"{"BackendState":"Running","Self":{"UserID":7,"HostName":"nixos","TailscaleIPs":["100.1.2.3","fd7a:115c:a1e0::1"]},"Peer":null}"#;

    fn answered(status: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.0 {status}\r\nContent-Type: application/json\r\n\r\n{body}").into_bytes()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn tailscaled(folder: &Path, answers: &'static [(&'static str, &'static str)]) -> std::path::PathBuf {
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
        assert_eq!(body_of(&answer).unwrap_err(), "tailscaled answered HTTP/1.0 403 Forbidden: access denied");
    }

    #[test]
    fn an_answer_cut_in_its_headers_reads_as_none() {
        assert!(body_of(b"HTTP/1.0 200 OK\r\nContent-").is_err());
    }

    #[test]
    fn the_status_names_this_machine_s_login_and_addresses() {
        let own = own_of(STATUS.as_bytes()).unwrap();
        let addresses = vec![IpAddr::V4(Ipv4Addr::new(100, 1, 2, 3)), "fd7a:115c:a1e0::1".parse().unwrap()];
        assert_eq!(own, Own { login: Login(7), addresses });
    }

    #[test]
    fn a_stopped_tailscale_has_no_own_node() {
        let stopped = r#"{"BackendState":"Stopped","Self":{"UserID":7,"TailscaleIPs":[]}}"#;
        assert_eq!(own_of(stopped.as_bytes()).unwrap_err(), "Tailscale is Stopped, not running");
    }

    #[test]
    fn a_node_with_no_address_is_not_listened_on() {
        let bare = r#"{"BackendState":"Running","Self":{"UserID":7}}"#;
        assert_eq!(own_of(bare.as_bytes()).unwrap_err(), "this machine has no tailnet address");
    }

    #[test]
    fn a_v6_peer_is_asked_for_in_one_query_word() {
        let peer: SocketAddr = "[fd7a:115c:a1e0::2]:51000".parse().unwrap();
        assert_eq!(query_of(peer), "%5Bfd7a%3A115c%3Aa1e0%3A%3A2%5D%3A51000");
    }

    #[test]
    fn only_this_machine_s_login_is_admitted() {
        const ANSWERS: &[(&str, &str)] = &[
            ("/localapi/v0/whois?addr=100.1.2.4%3A51000", r#"{"Node":{"User":7,"Name":"phone"}}"#),
            ("/localapi/v0/whois?addr=100.1.2.5%3A51000", r#"{"Node":{"User":8,"Name":"theirs"}}"#),
        ];
        runtime().block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let socket = tailscaled(folder.path(), ANSWERS);
            let mine: SocketAddr = "100.1.2.4:51000".parse().unwrap();
            let theirs: SocketAddr = "100.1.2.5:51000".parse().unwrap();
            let unknown: SocketAddr = "100.1.2.6:51000".parse().unwrap();
            assert_eq!(admits(&socket, Login(7), mine).await, Ok(()));
            let refused = admits(&socket, Login(7), theirs).await.unwrap_err();
            assert_eq!(refused, "it is login 8's, and luned answers only login 7");
            let unknown = admits(&socket, Login(7), unknown).await.unwrap_err();
            assert!(unknown.starts_with("tailscaled answered HTTP/1.0 404"), "{unknown}");
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
