use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use lune_domain::projects::layout::Direction;
use lune_domain::projects::{BrowserUrl, ViewId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;

use crate::layouts;
use crate::log;

pub const SOCKET: &str = "terminal-browser.sock";

const REQUEST_BYTES_MAX: usize = 64 * 1024;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

const METHODS: [&str; 6] = [
    "getCurrentPane",
    "split",
    "listPanes",
    "neighbor",
    "sendText",
    "focusPane",
];

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
    SendText {
        pane: ViewId,
        text: String,
        answer: oneshot::Sender<Result<(), String>>,
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
pub fn view_from_browser_tty(tty: &str) -> Option<ViewId> {
    let name = Path::new(tty).file_name()?.to_str()?;
    let hex = name.strip_prefix("b-")?.strip_suffix(".tty")?;
    hex.parse().ok()
}

#[must_use]
pub fn browser_url_from_command(command: &[String]) -> Option<Option<BrowserUrl>> {
    let first = command.first()?;
    let basename = Path::new(first).file_name().and_then(|name| name.to_str()).unwrap_or(first);
    if basename == "terminal-browser" {
        return Some(extract_url_from_args(&command[1..]));
    }
    if matches!(basename, "sh" | "bash" | "zsh")
        && command.get(1).is_some_and(|arg| arg == "-c")
        && let Some(script) = command.get(2)
        && script.contains("terminal-browser")
    {
        let tokens: Vec<String> = script
            .split_whitespace()
            .map(|token| token.trim_matches(|ch| matches!(ch, '\'' | '"' | ';')).to_owned())
            .collect();
        return Some(extract_url_from_args(&tokens));
    }
    None
}

fn extract_url_from_args(args: &[String]) -> Option<BrowserUrl> {
    let mut after_open = false;
    for arg in args {
        if arg == "open" {
            after_open = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        if arg.contains("://") || arg.starts_with("about:") {
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
    identity: Option<(u64, u64)>,
}

#[must_use]
pub fn bind(folder: &Path) -> Option<Bound> {
    let path = folder.join(SOCKET);
    let _ = fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            log::say(&format!("{}: terminal browser socket not bound: {error}", path.display()));
            return None;
        },
    };
    let identity = fs::symlink_metadata(&path).ok().map(|meta| (meta.dev(), meta.ino()));
    Some(Bound { folder: folder.to_owned(), path, listener, identity })
}

pub async fn serve(
    bound: Bound,
    layouts: SyncSender<layouts::Command>,
    mut closed: watch::Receiver<bool>,
) {
    let Bound { folder, path, listener, identity } = bound;
    loop {
        tokio::select! {
            _ = closed.wait_for(|done| *done) => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let layouts = layouts.clone();
                        let runtime = folder.clone();
                        tokio::spawn(async move {
                            let _ = timeout(
                                CONNECTION_TIMEOUT,
                                handle_connection(stream, runtime, layouts),
                            )
                            .await;
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
    if let Some(expected) = identity
        && let Ok(meta) = fs::symlink_metadata(&path)
        && (meta.dev(), meta.ino()) == expected
    {
        let _ = fs::remove_file(&path);
    }
}

async fn handle_connection(
    stream: UnixStream,
    runtime: PathBuf,
    layouts: SyncSender<layouts::Command>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = Vec::new();
    let mut total = 0usize;
    loop {
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break,
            Ok(read) => {
                total = total.saturating_add(read);
                if total > REQUEST_BYTES_MAX || line.ends_with(b"\n") {
                    break;
                }
            },
            Err(_) => return,
        }
    }
    if line.is_empty() || total > REQUEST_BYTES_MAX {
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
    if let Ok(mut encoded) = serde_json::to_string(&reply) {
        encoded.push('\n');
        let _ = write_half.write_all(encoded.as_bytes()).await;
        let _ = write_half.flush().await;
    }
}

async fn dispatch(
    request: Request,
    runtime: PathBuf,
    layouts: &SyncSender<layouts::Command>,
) -> Result<Value, String> {
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
            let command = request
                .params
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| "split requires command array".to_owned())?
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "split command entries must be strings".to_owned())
                })
                .collect::<Result<Vec<String>, String>>()?;
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
            ask_layouts(layouts, |answer| SocketCommand::SendText { pane, text, answer })
                .await??;
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

async fn ask_layouts<T: Send + 'static>(
    layouts: &SyncSender<layouts::Command>,
    make: impl FnOnce(oneshot::Sender<T>) -> SocketCommand,
) -> Result<T, String> {
    let (answer, answered) = oneshot::channel();
    layouts
        .send(layouts::Command::Socket(make(answer)))
        .map_err(|_| "luned layouts stopped".to_owned())?;
    answered.await.map_err(|_| "luned layouts dropped request".to_owned())
}

fn string_field(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn parse_view_field(params: &Value, key: &str) -> Result<ViewId, String> {
    let raw = string_field(params, key).ok_or_else(|| format!("missing field {key}"))?;
    raw.parse().map_err(|why| format!("{key}: {why}"))
}

fn parse_direction_field(params: &Value, key: &str) -> Result<Option<Direction>, String> {
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
        let tty = format!("/run/user/1000/lune/b-{id}.tty");
        assert_eq!(view_from_browser_tty(&tty), Some(id));
        assert_eq!(view_from_browser_tty("/dev/pts/4"), None);

        let direct = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "--no-split".to_owned(),
            "https://example.com".to_owned(),
        ];
        assert_eq!(
            browser_url_from_command(&direct),
            Some(Some(BrowserUrl::parse("https://example.com").unwrap()))
        );

        let wrapped = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exec '/nix/store/x/bin/terminal-browser' open 'http://localhost:3000'".to_owned(),
        ];
        assert_eq!(
            browser_url_from_command(&wrapped),
            Some(Some(BrowserUrl::parse("http://localhost:3000").unwrap()))
        );

        let start_page = vec!["terminal-browser".to_owned(), "open".to_owned()];
        assert_eq!(browser_url_from_command(&start_page), Some(None));

        let other = vec!["vim".to_owned(), "notes.md".to_owned()];
        assert_eq!(browser_url_from_command(&other), None);
    }

    #[test]
    fn socket_serves_hello_and_routes_methods_to_layouts() {
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

            let _ = close_tx.send(true);
            let _ = server.await;
        });
        worker.join().unwrap();
    }
}
