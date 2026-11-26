use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

use lune_domain::mux::TerminalKey;
use lune_domain::projects::layout::Direction;
use lune_domain::projects::{BrowserUrl, ViewId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch, Semaphore};
use tokio::time::timeout;

use crate::layouts;
use crate::log;
use crate::mux::own::server::Terminals;

pub const SOCKET: &str = lune_protocol::TERMINAL_BROWSER_SOCKET;

const REQUEST_BYTES_MAX: usize = 64 * 1024;

const CONNECTIONS_MAX: usize = 32;

const SPLIT_ARGS_MAX: usize = 64;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

const METHODS: [&str; 6] = [
    "getCurrentPane",
    "split",
    "listPanes",
    "neighbor",
    "sendText",
    "focusPane",
];

const _: () = assert!(matches!(SOCKET.as_bytes(), [_, ..]));
const _: () = assert!(REQUEST_BYTES_MAX > 0);
const _: () = assert!(CONNECTIONS_MAX > 0);
const _: () = assert!(SPLIT_ARGS_MAX > 0);
const _: () = assert!(CONNECTION_TIMEOUT.as_millis() > 0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneRef {
    pub id: String,
    pub tab: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneInfo {
    pub id: String,
    pub tab: String,
    pub tty: Option<String>,
    pub command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitTarget {
    Browser(Option<BrowserUrl>),
    Command,
}

pub enum SocketCommand {
    CurrentPane {
        tty: Option<String>,
        cwd: Option<String>,
        answer: oneshot::Sender<Result<Option<PaneRef>, String>>,
    },
    ListPanes {
        tty: Option<String>,
        runtime: PathBuf,
        answer: oneshot::Sender<Result<Vec<PaneInfo>, String>>,
    },
    Neighbor {
        from: ViewId,
        direction: Direction,
        answer: oneshot::Sender<Result<Option<PaneRef>, String>>,
    },
    Split {
        from: ViewId,
        direction: Option<Direction>,
        command: Vec<String>,
        answer: oneshot::Sender<Result<(), String>>,
    },
    TerminalKey {
        pane: ViewId,
        answer: oneshot::Sender<Result<TerminalKey, String>>,
    },
    FocusPane {
        pane: ViewId,
        answer: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Deserialize)]
struct Request {
    id: String,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    caller: Option<Caller>,
}

#[derive(Deserialize)]
struct Caller {
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Serialize)]
struct Reply {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ReplyError>,
}

#[derive(Serialize)]
struct ReplyError {
    message: String,
}

#[must_use]
pub fn browser_tty_path(runtime: &Path, view: ViewId) -> PathBuf {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    let path = runtime.join(format!("b-{view}.tty"));
    assert_eq!(view_from_browser_tty(&path.to_string_lossy()), Some(view));
    path
}

#[must_use]
pub fn browser_client_tty_path(runtime: &Path, view: ViewId, pid: u32, sequence: u64) -> PathBuf {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    assert!(pid > 0, "process id is positive");
    let path = runtime.join(format!("b-{view}-{pid}-{sequence}.tty"));
    assert_eq!(view_from_browser_tty(&path.to_string_lossy()), Some(view));
    path
}

#[must_use]
pub fn view_from_browser_tty(tty: &str) -> Option<ViewId> {
    let name = Path::new(tty).file_name()?.to_str()?;
    let stem = name.strip_prefix("b-")?.strip_suffix(".tty")?;
    let hex = match stem.split_once('-') {
        Some((id, suffix)) => {
            let (pid, sequence) = suffix.split_once('-')?;
            let pid_valid = !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit());
            let seq_valid =
                !sequence.is_empty() && sequence.bytes().all(|byte| byte.is_ascii_digit());
            if !pid_valid || !seq_valid {
                return None;
            }
            id
        },
        None => stem,
    };
    hex.parse().ok()
}

#[must_use]
pub fn split_target(command: &[String]) -> SplitTarget {
    let Some(first) = command.first() else {
        return SplitTarget::Command;
    };
    let basename = Path::new(first).file_name().and_then(|name| name.to_str()).unwrap_or(first);
    if basename == "terminal-browser" {
        let rest = &command[1..command.len().min(SPLIT_ARGS_MAX + 1)];
        return SplitTarget::Browser(extract_url_from_args(rest));
    }
    if matches!(basename, "sh" | "bash" | "zsh")
        && command.get(1).is_some_and(|arg| arg == "-c")
        && let Some(script) = command.get(2)
    {
        let tokens: Vec<String> = script
            .split_whitespace()
            .take(SPLIT_ARGS_MAX)
            .map(|token| token.trim_matches(|ch| matches!(ch, '\'' | '"' | ';')).to_owned())
            .collect();
        assert!(tokens.len() <= SPLIT_ARGS_MAX, "shell tokens stay within split arg bound");
        let program_index = usize::from(tokens.first().is_some_and(|token| token == "exec"));
        if let Some(program) = tokens.get(program_index) {
            let invoked =
                Path::new(program).file_name().and_then(|name| name.to_str()).unwrap_or(program);
            if invoked == "terminal-browser" {
                return SplitTarget::Browser(extract_url_from_args(&tokens[program_index + 1..]));
            }
        }
    }
    SplitTarget::Command
}

fn extract_url_from_args(args: &[String]) -> Option<BrowserUrl> {
    assert!(args.len() <= SPLIT_ARGS_MAX, "split args stay within bound");
    let mut after_open = false;
    for arg in args {
        if arg == "open" {
            after_open = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        let has_about = arg.get(..6).is_some_and(|head| head.eq_ignore_ascii_case("about:"));
        if arg.contains("://") || has_about {
            if let Ok(url) = BrowserUrl::parse(arg) {
                return Some(url);
            }
        } else if after_open
            && let Ok(Some(url)) = BrowserUrl::from_user(arg)
        {
            return Some(url);
        }
    }
    None
}

pub struct Bound {
    folder: PathBuf,
    path: PathBuf,
    listener: UnixListener,
    identity: (u64, u64),
}

#[must_use]
pub fn bind(folder: &Path) -> Option<Bound> {
    assert_ne!(folder.as_os_str(), "", "socket folder is non-empty");
    let path = folder.join(SOCKET);
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != ErrorKind::NotFound
    {
        log::say(&format!("{}: stale terminal browser socket not removed: {error}", path.display()));
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            log::say(&format!("{}: terminal browser socket not bound: {error}", path.display()));
            return None;
        },
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            log::say(&format!("{}: terminal browser socket metadata unread: {error}", path.display()));
            if let Err(remove_error) = fs::remove_file(&path)
                && remove_error.kind() != ErrorKind::NotFound
            {
                log::say(&format!(
                    "{}: terminal browser socket not removed: {remove_error}",
                    path.display()
                ));
            }
            return None;
        },
    };
    let identity = (metadata.dev(), metadata.ino());
    assert!(path.starts_with(folder), "bound socket lives inside its runtime folder");
    Some(Bound { folder: folder.to_owned(), path, listener, identity })
}

pub async fn serve(
    bound: Bound,
    layouts: SyncSender<layouts::Command>,
    mut closed: watch::Receiver<bool>,
) {
    let Bound { folder, path, listener, identity } = bound;
    let connections = Arc::new(Semaphore::new(CONNECTIONS_MAX));
    loop {
        assert!(
            connections.available_permits() <= CONNECTIONS_MAX,
            "connection permits never exceed CONNECTIONS_MAX"
        );
        tokio::select! {
            _ = closed.wait_for(|done| *done) => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                            log::say("terminal browser socket at connection limit; dropping client");
                            continue;
                        };
                        let layouts = layouts.clone();
                        let runtime = folder.clone();
                        tokio::spawn(async move {
                            let _held = permit;
                            if timeout(
                                CONNECTION_TIMEOUT,
                                handle_connection(stream, runtime, layouts),
                            )
                            .await
                            .is_err()
                            {
                                log::say("terminal browser socket connection timed out");
                            }
                        });
                    },
                    Err(error) => {
                        log::say(&format!("terminal browser socket accept failed: {error}"));
                        break;
                    },
                }
            }
        }
    }
    if let Ok(meta) = fs::symlink_metadata(&path)
        && (meta.dev(), meta.ino()) == identity
        && let Err(error) = fs::remove_file(&path)
        && error.kind() != ErrorKind::NotFound
    {
        log::say(&format!("{}: terminal browser socket not removed: {error}", path.display()));
    }
}

async fn handle_connection(
    stream: UnixStream,
    runtime: PathBuf,
    layouts: SyncSender<layouts::Command>,
) {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    let (read_half, mut write_half) = stream.into_split();
    let reader = BufReader::new(read_half);
    let mut limited = reader.take((REQUEST_BYTES_MAX + 1) as u64);
    let mut line = Vec::new();
    let read = match limited.read_until(b'\n', &mut line).await {
        Ok(read) => read,
        Err(error) => {
            log::say(&format!("terminal browser socket read failed: {error}"));
            return;
        },
    };
    assert!(line.len() <= REQUEST_BYTES_MAX + 1, "request read stays within REQUEST_BYTES_MAX + 1");
    if read == 0 {
        return;
    }
    if line.len() > REQUEST_BYTES_MAX {
        log::say(&format!("terminal browser socket request exceeded {REQUEST_BYTES_MAX} bytes"));
        return;
    }
    let text = String::from_utf8_lossy(&line);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let reply = match serde_json::from_str::<Request>(trimmed) {
        Ok(request) => {
            let id = request.id.clone();
            match dispatch(request, runtime, &layouts).await {
                Ok(result) => Reply { id, result: Some(result), error: None },
                Err(message) => Reply { id, result: None, error: Some(ReplyError { message }) },
            }
        },
        Err(error) => Reply {
            id: String::new(),
            result: None,
            error: Some(ReplyError { message: format!("invalid request: {error}") }),
        },
    };
    let mut encoded = match serde_json::to_string(&reply) {
        Ok(encoded) => encoded,
        Err(error) => {
            log::say(&format!("terminal browser socket reply did not serialize: {error}"));
            return;
        },
    };
    encoded.push('\n');
    if let Err(error) = write_half.write_all(encoded.as_bytes()).await {
        log::say(&format!("terminal browser socket write failed: {error}"));
        return;
    }
    if let Err(error) = write_half.flush().await {
        log::say(&format!("terminal browser socket flush failed: {error}"));
    }
}

async fn dispatch(
    request: Request,
    runtime: PathBuf,
    layouts: &SyncSender<layouts::Command>,
) -> Result<Value, String> {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    match request.method.as_str() {
        "hello" => Ok(json!({ "name": "lune", "methods": METHODS })),
        "getCurrentPane" => {
            let tty = string_field(&request.params, "tty");
            let cwd = string_field(&request.params, "cwd")
                .or_else(|| request.caller.and_then(|caller| caller.cwd));
            let pane = ask_layouts(layouts, |answer| {
                SocketCommand::CurrentPane { tty, cwd, answer }
            })
            .await??;
            Ok(json!({ "pane": pane }))
        },
        "listPanes" => {
            let tty = string_field(&request.params, "tty");
            let panes = ask_layouts(layouts, |answer| {
                SocketCommand::ListPanes { tty, runtime, answer }
            })
            .await??;
            Ok(json!({ "panes": panes }))
        },
        "neighbor" => {
            let from = parse_view_field(&request.params, "from")?;
            let direction = parse_direction_field(&request.params, "direction")?
                .ok_or_else(|| "neighbor requires direction".to_owned())?;
            let pane = ask_layouts(layouts, |answer| {
                SocketCommand::Neighbor { from, direction, answer }
            })
            .await??;
            Ok(json!({ "pane": pane }))
        },
        "split" => {
            let from = parse_view_field(&request.params, "from")?;
            let direction = parse_direction_field(&request.params, "direction")?;
            let array = request
                .params
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| "split requires command array".to_owned())?;
            if array.len() > SPLIT_ARGS_MAX {
                return Err(format!("split command exceeds {SPLIT_ARGS_MAX} arguments"));
            }
            let command = array
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "split command entries must be strings".to_owned())
                })
                .collect::<Result<Vec<String>, String>>()?;
            assert!(command.len() <= SPLIT_ARGS_MAX, "split command fits SPLIT_ARGS_MAX");
            ask_layouts(layouts, |answer| SocketCommand::Split {
                from,
                direction,
                command,
                answer,
            })
            .await??;
            Ok(json!({}))
        },
        "sendText" => {
            let pane = parse_view_field(&request.params, "pane")?;
            let text = string_field(&request.params, "text")
                .ok_or_else(|| "sendText requires text".to_owned())?;
            if text.is_empty() {
                return Ok(json!({}));
            }
            let key = ask_layouts(layouts, |answer| SocketCommand::TerminalKey { pane, answer })
                .await??;
            Terminals.paste_text(key, &text).await.map_err(|failure| failure.to_string())?;
            Ok(json!({}))
        },
        "focusPane" => {
            let pane = parse_view_field(&request.params, "pane")?;
            ask_layouts(layouts, |answer| SocketCommand::FocusPane { pane, answer }).await??;
            Ok(json!({}))
        },
        other => Err(format!("unknown method: {other}")),
    }
}

async fn ask_layouts<T>(
    layouts: &SyncSender<layouts::Command>,
    make: impl FnOnce(oneshot::Sender<T>) -> SocketCommand,
) -> Result<T, String> {
    let (answer, answered) = oneshot::channel();
    let command = layouts::Command::Socket(make(answer));
    let layouts = layouts.clone();
    let sent = tokio::task::spawn_blocking(move || layouts.send(command))
        .await
        .map_err(|error| format!("luned layouts send task failed: {error}"))?;
    sent.map_err(|_| "luned layouts stopped".to_owned())?;
    answered.await.map_err(|_| "luned layouts dropped request".to_owned())
}

fn string_field(params: &Value, key: &str) -> Option<String> {
    assert_ne!(key, "", "field key is non-empty");
    params.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn parse_view_field(params: &Value, key: &str) -> Result<ViewId, String> {
    assert_ne!(key, "", "view field key is non-empty");
    let raw = string_field(params, key).ok_or_else(|| format!("missing field {key}"))?;
    raw.parse().map_err(|why| format!("{key}: {why}"))
}

fn parse_direction_field(params: &Value, key: &str) -> Result<Option<Direction>, String> {
    assert_ne!(key, "", "direction field key is non-empty");
    let Some(raw) = string_field(params, key) else {
        return Ok(None);
    };
    match raw.as_str() {
        "left" => Ok(Some(Direction::Left)),
        "right" => Ok(Some(Direction::Right)),
        "up" => Ok(Some(Direction::Up)),
        "down" => Ok(Some(Direction::Down)),
        other => Err(format!("unknown direction: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn browser_tty_and_split_commands_parse_cleanly() {
        let id = ViewId::of(0x2a).unwrap();
        let tty = browser_tty_path(Path::new("/run/user/1000/lune"), id);
        assert_eq!(view_from_browser_tty(&tty.to_string_lossy()), Some(id));
        let client_tty = browser_client_tty_path(Path::new("/run/user/1000/lune"), id, 4242, 7);
        assert_eq!(view_from_browser_tty(&client_tty.to_string_lossy()), Some(id));
        assert_eq!(view_from_browser_tty("/dev/pts/4"), None);
        assert_eq!(view_from_browser_tty(&format!("/tmp/b-{id}-bad.tty")), None);
        assert_eq!(view_from_browser_tty(&format!("/tmp/b-{id}-12-xx.tty")), None);

        let direct = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "--no-split".to_owned(),
            "https://example.com".to_owned(),
        ];
        assert_eq!(
            split_target(&direct),
            SplitTarget::Browser(Some(BrowserUrl::parse("https://example.com").unwrap()))
        );

        let wrapped = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exec '/nix/store/x/bin/terminal-browser' open 'http://localhost:3000'".to_owned(),
        ];
        assert_eq!(
            split_target(&wrapped),
            SplitTarget::Browser(Some(BrowserUrl::parse("http://localhost:3000").unwrap()))
        );

        let start_page = vec!["terminal-browser".to_owned(), "open".to_owned()];
        assert_eq!(split_target(&start_page), SplitTarget::Browser(None));

        let other = vec!["vim".to_owned(), "notes.md".to_owned()];
        assert_eq!(split_target(&other), SplitTarget::Command);

        let shell_mention = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "rg terminal-browser crates/".to_owned(),
        ];
        assert_eq!(split_target(&shell_mention), SplitTarget::Command);
    }

    #[test]
    fn socket_serves_hello_routes_methods_and_rejects_oversized_requests() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (layouts_tx, layouts_rx) = mpsc::sync_channel(8);
        let (close_tx, close_rx) = watch::channel(false);
        let folder = dir.path().to_owned();
        let socket_path = folder.join(SOCKET);

        let worker = std::thread::spawn(move || {
            while let Ok(layouts::Command::Socket(cmd)) = layouts_rx.recv() {
                if let SocketCommand::CurrentPane { answer, .. } = cmd {
                    let _ = answer.send(Ok(Some(PaneRef {
                        id: "0000000000000004".to_owned(),
                        tab: "0000000000000002".to_owned(),
                    })));
                }
            }
        });

        runtime.block_on(async move {
            let bound = bind(&folder).expect("terminal-browser socket binds");
            let server = tokio::spawn(serve(bound, layouts_tx, close_rx));

            let ask = |payload: &str| {
                let path = socket_path.clone();
                let payload = payload.to_owned();
                async move {
                    let mut stream = UnixStream::connect(&path).await.unwrap();
                    stream.write_all(payload.as_bytes()).await.unwrap();
                    stream.flush().await.unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut reply = String::new();
                    reader.read_line(&mut reply).await.unwrap();
                    serde_json::from_str::<Value>(&reply).unwrap()
                }
            };

            let hello = ask("{\"id\":\"1\",\"method\":\"hello\",\"params\":{\"protocol\":1}}\n").await;
            assert_eq!(hello["id"], "1");
            assert_eq!(hello["result"]["name"], "lune");

            let current = ask(
                "{\"id\":\"2\",\"method\":\"getCurrentPane\",\"params\":{\"tty\":\"/dev/pts/1\"}}\n",
            )
            .await;
            assert_eq!(current["result"]["pane"]["id"], "0000000000000004");
            assert_eq!(current["result"]["pane"]["tab"], "0000000000000002");

            let mut oversized = UnixStream::connect(&socket_path).await.unwrap();
            let junk = vec![b'a'; REQUEST_BYTES_MAX + 512];
            let _ = oversized.write_all(&junk).await;
            let _ = oversized.shutdown().await;
            let mut reply = Vec::new();
            let mut reader = BufReader::new(oversized);
            let n = reader.read_to_end(&mut reply).await.unwrap();
            assert_eq!(n, 0, "oversized request closes without reply");

            let _ = close_tx.send(true);
            let _ = server.await;
        });
        worker.join().unwrap();
    }
}
