use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use lune_domain::mux::TerminalKey;
use lune_domain::projects::layout::Direction;
use lune_domain::projects::{Argv, BrowserUrl, ViewId, WorktreeId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, oneshot, watch};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRef {
    pub id: ViewId,
    pub tab: WorktreeId,
}

impl Serialize for PaneRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire {
            id: String,
            tab: String,
        }
        Wire {
            id: self.id.to_string(),
            tab: self.tab.to_string(),
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneInfo {
    pub id: ViewId,
    pub tab: WorktreeId,
    pub tty: Option<String>,
    pub command: Option<String>,
}

impl Serialize for PaneInfo {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            id: String,
            tab: String,
            tty: Option<&'a str>,
            command: Option<&'a str>,
        }
        Wire {
            id: self.id.to_string(),
            tab: self.tab.to_string(),
            tty: self.tty.as_deref(),
            command: self.command.as_deref(),
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitAction {
    Browser(Option<BrowserUrl>),
    Command(Argv),
}

pub enum SocketCommand {
    CurrentPane {
        tty: Option<String>,
        cwd: Option<String>,
        runtime: PathBuf,
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
        action: SplitAction,
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

#[derive(Deserialize)]
struct CurrentPaneParams {
    #[serde(default)]
    tty: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct ListPanesParams {
    #[serde(default)]
    tty: Option<String>,
}

#[derive(Deserialize)]
struct NeighborParams {
    from: String,
    direction: String,
}

#[derive(Deserialize)]
struct SplitParams {
    from: String,
    #[serde(default)]
    direction: Option<String>,
    command: Vec<String>,
}

#[derive(Deserialize)]
struct SendTextParams {
    pane: String,
    text: String,
}

#[derive(Deserialize)]
struct FocusPaneParams {
    pane: String,
}

#[derive(Serialize)]
struct Reply {
    id: String,
    #[serde(flatten)]
    body: ReplyBody,
}

#[derive(Serialize)]
enum ReplyBody {
    #[serde(rename = "result")]
    Result(Value),
    #[serde(rename = "error")]
    Error(ReplyError),
}

#[derive(Serialize)]
struct ReplyError {
    message: String,
}

#[must_use]
pub fn browser_client_tty_path(runtime: &Path, view: ViewId, pid: u32, sequence: u64) -> PathBuf {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    assert!(pid > 0, "process id is positive");
    let path = runtime.join(format!("b-{view}-{pid}-{sequence}.tty"));
    assert_eq!(
        view_from_browser_tty(runtime, &path.to_string_lossy()),
        Some(view)
    );
    path
}

#[must_use]
pub fn is_browser_client_tty(tty: &str, view: ViewId) -> bool {
    let path = Path::new(tty);
    if !path.is_absolute() {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(parse_browser_client_tty_name)
        == Some(view)
}

#[must_use]
pub fn view_from_browser_tty(runtime: &Path, tty: &str) -> Option<ViewId> {
    assert_ne!(runtime.as_os_str(), "", "runtime folder is non-empty");
    let path = Path::new(tty);
    if path.parent() != Some(runtime) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    parse_browser_client_tty_name(name)
}

fn parse_browser_client_tty_name(name: &str) -> Option<ViewId> {
    let stem = name.strip_prefix("b-")?.strip_suffix(".tty")?;
    let (id, suffix) = stem.split_once('-')?;
    let (pid, sequence) = suffix.split_once('-')?;
    let pid_valid = !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && pid.parse::<u32>().is_ok_and(|pid| pid > 0);
    let seq_valid = !sequence.is_empty()
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.parse::<u64>().is_ok();
    if !pid_valid || !seq_valid {
        return None;
    }
    id.parse().ok()
}

enum BrowserOpen {
    NotOpen,
    Open(Option<BrowserUrl>),
}

pub fn parse_split_action(command: Vec<String>) -> Result<SplitAction, String> {
    if command.len() > SPLIT_ARGS_MAX {
        return Err(format!("split command exceeds {SPLIT_ARGS_MAX} arguments"));
    }
    let Some(first) = command.first() else {
        return Err("split command is empty".to_owned());
    };
    let basename = Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first);
    if basename == "terminal-browser"
        && let BrowserOpen::Open(url) = extract_browser_open_url(&command[1..])?
    {
        return Ok(SplitAction::Browser(url));
    }
    if matches!(basename, "sh" | "bash" | "zsh")
        && command.get(1).is_some_and(|arg| arg == "-c")
        && let Some(script) = command.get(2)
    {
        let tokens: Vec<String> = script
            .split_whitespace()
            .take(SPLIT_ARGS_MAX)
            .map(|token| {
                token
                    .trim_matches(|ch| matches!(ch, '\'' | '"' | ';'))
                    .to_owned()
            })
            .collect();
        assert!(
            tokens.len() <= SPLIT_ARGS_MAX,
            "shell tokens stay within split arg bound"
        );
        let compound = tokens
            .iter()
            .any(|token| matches!(token.as_str(), "&&" | "||" | "|"));
        let program_index = usize::from(tokens.first().is_some_and(|token| token == "exec"));
        if !compound && let Some(program) = tokens.get(program_index) {
            let invoked = Path::new(program)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(program);
            if invoked == "terminal-browser"
                && let BrowserOpen::Open(url) =
                    extract_browser_open_url(&tokens[program_index + 1..])?
            {
                return Ok(SplitAction::Browser(url));
            }
        }
    }
    Argv::parse(command).map(SplitAction::Command)
}

fn takes_separate_value(flag: &str) -> bool {
    matches!(
        flag,
        "--ssh"
            | "--profile"
            | "--session"
            | "--socket"
            | "--title"
            | "--cwd"
            | "--dir"
            | "--width"
            | "--height"
            | "--cols"
            | "--rows"
            | "--port"
            | "-p"
    )
}

fn parse_browser_url_arg(arg: &str) -> Result<Option<BrowserUrl>, String> {
    if arg.eq_ignore_ascii_case("about:blank") {
        return Ok(None);
    }
    let has_about = arg
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("about:"));
    if arg.contains("://") || has_about {
        return BrowserUrl::parse(arg).map(Some);
    }
    BrowserUrl::from_user(arg)
}

fn extract_browser_open_url(args: &[String]) -> Result<BrowserOpen, String> {
    assert!(args.len() <= SPLIT_ARGS_MAX, "split args stay within bound");
    let mut index = 0usize;
    let mut after_open = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut after_double_dash = false;
    while let Some(arg) = args.get(index) {
        index += 1;
        if !after_open {
            if arg == "open" {
                after_open = true;
                continue;
            }
            if arg.starts_with('-') {
                return Ok(BrowserOpen::NotOpen);
            }
            let has_about = arg
                .get(..6)
                .is_some_and(|head| head.eq_ignore_ascii_case("about:"));
            if arg.contains("://") || has_about {
                return parse_browser_url_arg(arg).map(BrowserOpen::Open);
            }
            return Ok(BrowserOpen::NotOpen);
        }
        if !after_double_dash {
            if arg == "--" {
                after_double_dash = true;
                continue;
            }
            if arg.starts_with('-') {
                if !arg.contains('=') && takes_separate_value(arg) && index < args.len() {
                    index += 1;
                }
                continue;
            }
        }
        positionals.push(arg);
    }
    if !after_open {
        return Ok(BrowserOpen::NotOpen);
    }
    match positionals.last() {
        None => Ok(BrowserOpen::Open(None)),
        Some(target) => parse_browser_url_arg(target).map(BrowserOpen::Open),
    }
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
        log::say(&format!(
            "{}: stale terminal browser socket not removed: {error}",
            path.display()
        ));
    }
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            log::say(&format!(
                "{}: terminal browser socket not bound: {error}",
                path.display()
            ));
            return None;
        }
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            log::say(&format!(
                "{}: terminal browser socket metadata unread: {error}",
                path.display()
            ));
            if let Err(remove_error) = fs::remove_file(&path)
                && remove_error.kind() != ErrorKind::NotFound
            {
                log::say(&format!(
                    "{}: terminal browser socket not removed: {remove_error}",
                    path.display()
                ));
            }
            return None;
        }
    };
    let identity = (metadata.dev(), metadata.ino());
    assert!(
        path.starts_with(folder),
        "bound socket lives inside its runtime folder"
    );
    Some(Bound {
        folder: folder.to_owned(),
        path,
        listener,
        identity,
    })
}

pub async fn serve(
    bound: Bound,
    layouts: SyncSender<layouts::Command>,
    mut closed: watch::Receiver<bool>,
) {
    let Bound {
        folder,
        path,
        listener,
        identity,
    } = bound;
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
        log::say(&format!(
            "{}: terminal browser socket not removed: {error}",
            path.display()
        ));
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
        }
    };
    assert!(
        line.len() <= REQUEST_BYTES_MAX + 1,
        "request read stays within REQUEST_BYTES_MAX + 1"
    );
    if read == 0 {
        return;
    }
    if line.len() > REQUEST_BYTES_MAX {
        log::say(&format!(
            "terminal browser socket request exceeded {REQUEST_BYTES_MAX} bytes"
        ));
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
            let body = match dispatch(request, runtime, &layouts).await {
                Ok(result) => ReplyBody::Result(result),
                Err(message) => ReplyBody::Error(ReplyError { message }),
            };
            Reply { id, body }
        }
        Err(error) => Reply {
            id: String::new(),
            body: ReplyBody::Error(ReplyError {
                message: format!("invalid request: {error}"),
            }),
        },
    };
    let mut encoded = match serde_json::to_string(&reply) {
        Ok(encoded) => encoded,
        Err(error) => {
            log::say(&format!(
                "terminal browser socket reply did not serialize: {error}"
            ));
            return;
        }
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
            let params: CurrentPaneParams = parse_params(request.params)?;
            let cwd = params
                .cwd
                .or_else(|| request.caller.and_then(|caller| caller.cwd));
            let pane = ask_layouts(layouts, |answer| SocketCommand::CurrentPane {
                tty: params.tty,
                cwd,
                runtime,
                answer,
            })
            .await??;
            Ok(json!({ "pane": pane }))
        }
        "listPanes" => {
            let params: ListPanesParams = parse_params(request.params)?;
            let panes = ask_layouts(layouts, |answer| SocketCommand::ListPanes {
                tty: params.tty,
                runtime,
                answer,
            })
            .await??;
            Ok(json!({ "panes": panes }))
        }
        "neighbor" => {
            let params: NeighborParams = parse_params(request.params)?;
            let from = parse_view(&params.from, "from")?;
            let direction = parse_direction(&params.direction)?;
            let pane = ask_layouts(layouts, |answer| SocketCommand::Neighbor {
                from,
                direction,
                answer,
            })
            .await??;
            Ok(json!({ "pane": pane }))
        }
        "split" => {
            let params: SplitParams = parse_params(request.params)?;
            let from = parse_view(&params.from, "from")?;
            let direction = params
                .direction
                .as_deref()
                .map(parse_direction)
                .transpose()?;
            let action = parse_split_action(params.command)?;
            ask_layouts(layouts, |answer| SocketCommand::Split {
                from,
                direction,
                action,
                answer,
            })
            .await??;
            Ok(json!({}))
        }
        "sendText" => {
            let params: SendTextParams = parse_params(request.params)?;
            let pane = parse_view(&params.pane, "pane")?;
            let key = ask_layouts(layouts, |answer| SocketCommand::TerminalKey {
                pane,
                answer,
            })
            .await??;
            if !params.text.is_empty() {
                Terminals
                    .paste_text(key, &params.text)
                    .await
                    .map_err(|failure| failure.to_string())?;
            }
            Ok(json!({}))
        }
        "focusPane" => {
            let params: FocusPaneParams = parse_params(request.params)?;
            let pane = parse_view(&params.pane, "pane")?;
            ask_layouts(layouts, |answer| SocketCommand::FocusPane { pane, answer }).await??;
            Ok(json!({}))
        }
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
    answered
        .await
        .map_err(|_| "luned layouts dropped request".to_owned())
}

fn parse_params<T: DeserializeOwned>(params: Value) -> Result<T, String> {
    let params = match params {
        Value::Null => Value::Object(serde_json::Map::new()),
        other => other,
    };
    serde_json::from_value(params).map_err(|error| format!("invalid params: {error}"))
}

fn parse_view(raw: &str, key: &str) -> Result<ViewId, String> {
    assert_ne!(key, "", "view field key is non-empty");
    raw.parse().map_err(|why| format!("{key}: {why}"))
}

fn parse_direction(raw: &str) -> Result<Direction, String> {
    match raw {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        other => Err(format!("unknown direction: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn browser_tty_and_split_commands_parse_cleanly() {
        let runtime = Path::new("/run/user/1000/lune");
        let id = ViewId::of(0x2a).unwrap();
        let client_tty = browser_client_tty_path(runtime, id, 4242, 7);
        assert_eq!(
            view_from_browser_tty(runtime, &client_tty.to_string_lossy()),
            Some(id)
        );
        assert!(is_browser_client_tty(&client_tty.to_string_lossy(), id));
        assert!(!is_browser_client_tty(
            &client_tty.to_string_lossy(),
            ViewId::of(0x2b).unwrap()
        ));
        assert!(!is_browser_client_tty(&format!("b-{id}-4242-7.tty"), id));
        assert_eq!(
            view_from_browser_tty(Path::new("/tmp"), &client_tty.to_string_lossy()),
            None,
            "browser tty outside runtime is rejected"
        );
        assert_eq!(
            view_from_browser_tty(runtime, &format!("/run/user/1000/lune/b-{id}.tty")),
            None,
            "fabricated unsuffixed tty is rejected"
        );
        assert_eq!(view_from_browser_tty(runtime, "/dev/pts/4"), None);
        assert_eq!(
            view_from_browser_tty(runtime, &format!("/run/user/1000/lune/b-{id}-0-7.tty")),
            None,
            "zero pid is rejected"
        );
        assert_eq!(
            view_from_browser_tty(runtime, &format!("/run/user/1000/lune/b-{id}-bad.tty")),
            None
        );
        assert_eq!(
            view_from_browser_tty(runtime, &format!("/run/user/1000/lune/b-{id}-12-xx.tty")),
            None
        );

        let direct = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "--no-split".to_owned(),
            "https://example.com".to_owned(),
        ];
        assert_eq!(
            parse_split_action(direct),
            Ok(SplitAction::Browser(Some(
                BrowserUrl::parse("https://example.com").unwrap()
            )))
        );

        let wrapped = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exec '/nix/store/x/bin/terminal-browser' open 'http://localhost:3000'".to_owned(),
        ];
        assert_eq!(
            parse_split_action(wrapped),
            Ok(SplitAction::Browser(Some(
                BrowserUrl::parse("http://localhost:3000").unwrap()
            )))
        );

        let start_page = vec!["terminal-browser".to_owned(), "open".to_owned()];
        assert_eq!(
            parse_split_action(start_page),
            Ok(SplitAction::Browser(None))
        );

        let blank_page = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "about:blank".to_owned(),
        ];
        assert_eq!(
            parse_split_action(blank_page),
            Ok(SplitAction::Browser(None))
        );

        let bad_url = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "javascript:alert(1)".to_owned(),
        ];
        assert!(
            parse_split_action(bad_url).is_err(),
            "invalid browser URL fails instead of defaulting"
        );

        let other = vec!["vim".to_owned(), "notes.md".to_owned()];
        assert_eq!(
            parse_split_action(other.clone()),
            Ok(SplitAction::Command(Argv::parse(other).unwrap()))
        );

        let shell_mention = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "rg terminal-browser crates/".to_owned(),
        ];
        assert_eq!(
            parse_split_action(shell_mention.clone()),
            Ok(SplitAction::Command(Argv::parse(shell_mention).unwrap()))
        );

        let with_profile = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "--profile".to_owned(),
            "work".to_owned(),
            "https://example.com".to_owned(),
        ];
        assert_eq!(
            parse_split_action(with_profile),
            Ok(SplitAction::Browser(Some(
                BrowserUrl::parse("https://example.com").unwrap()
            )))
        );

        let profile_only = vec![
            "terminal-browser".to_owned(),
            "open".to_owned(),
            "--profile".to_owned(),
            "work".to_owned(),
        ];
        assert_eq!(
            parse_split_action(profile_only),
            Ok(SplitAction::Browser(None))
        );

        let subcommand = vec!["terminal-browser".to_owned(), "status".to_owned()];
        assert_eq!(
            parse_split_action(subcommand.clone()),
            Ok(SplitAction::Command(Argv::parse(subcommand).unwrap()))
        );

        let compound = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "terminal-browser open https://example.com && echo done".to_owned(),
        ];
        assert_eq!(
            parse_split_action(compound.clone()),
            Ok(SplitAction::Command(Argv::parse(compound).unwrap()))
        );

        assert!(parse_split_action(Vec::new()).is_err());
    }

    #[test]
    fn socket_serves_hello_routes_methods_and_rejects_oversized_requests() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (layouts_tx, layouts_rx) = mpsc::sync_channel(8);
        let (close_tx, close_rx) = watch::channel(false);
        let folder = dir.path().to_owned();
        let socket_path = folder.join(SOCKET);

        let worker = std::thread::spawn(move || {
            while let Ok(layouts::Command::Socket(cmd)) = layouts_rx.recv() {
                match cmd {
                    SocketCommand::CurrentPane { answer, .. } => {
                        let _ = answer.send(Ok(Some(PaneRef {
                            id: ViewId::of(4).unwrap(),
                            tab: WorktreeId::of(2).unwrap(),
                        })));
                    }
                    SocketCommand::TerminalKey { pane, answer } => {
                        let _ = answer.send(Err(format!("view {pane} runs no terminal")));
                    }
                    _ => {}
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
            assert!(hello.get("error").is_none());

            let current = ask(
                "{\"id\":\"2\",\"method\":\"getCurrentPane\",\"params\":{\"tty\":\"/dev/pts/1\"}}\n",
            )
            .await;
            assert_eq!(current["result"]["pane"]["id"], "0000000000000004");
            assert_eq!(current["result"]["pane"]["tab"], "0000000000000002");

            let bad_tty = ask(
                "{\"id\":\"3\",\"method\":\"getCurrentPane\",\"params\":{\"tty\":42}}\n",
            )
            .await;
            assert!(bad_tty["error"]["message"].is_string(), "wrong-type tty returns error");
            assert!(bad_tty.get("result").is_none());

            let bad_dir = ask(
                "{\"id\":\"4\",\"method\":\"split\",\"params\":{\"from\":\"0000000000000004\",\"direction\":123,\"command\":[\"vim\"]}}\n",
            )
            .await;
            assert!(bad_dir["error"]["message"].is_string(), "wrong-type direction returns error");

            let bad_url = ask(
                "{\"id\":\"5\",\"method\":\"split\",\"params\":{\"from\":\"0000000000000004\",\"command\":[\"terminal-browser\",\"open\",\"javascript:alert(1)\"]}}\n",
            )
            .await;
            assert!(bad_url["error"]["message"].is_string(), "invalid URL in split returns error");

            let empty_send = ask(
                "{\"id\":\"6\",\"method\":\"sendText\",\"params\":{\"pane\":\"0000000000000004\",\"text\":\"\"}}\n",
            )
            .await;
            assert!(
                empty_send["error"]["message"].as_str().is_some_and(|m| m.contains("no terminal")),
                "empty sendText still validates pane: {empty_send}"
            );

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
