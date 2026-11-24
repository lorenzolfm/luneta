use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lune_protocol::said::Said;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::agent::{
    named_pid, terminal_or_trouble, Agent, Harness, Name, Source, Status, WaitingFor,
    FILE_BYTES_MAX,
};
use crate::mux::{self, TerminalKey};
use crate::watch::{self, Troubles};
use crate::{log, process, Directory};

const SCRIPT_BODY: &str = r#"if [ -n "$LUNE_AGY_TITLE_DEPTH" ]; then
    exit 0
fi
LUNE_AGY_TITLE_DEPTH=1
export LUNE_AGY_TITLE_DEPTH
wrote=0
buf=""
if [ -d "$dir" ] && [ ! -L "$dir" ] && [ -O "$dir" ]; then
    tmp="$dir/.$$.tmp"
    buf="$dir/.$$.in"
    if head -c 65000 > "$buf" 2>/dev/null; then
        wrote=1
        if [ -s "$buf" ]; then
            start=""
            if [ -r "/proc/$PPID/stat" ] && IFS= read -r line < "/proc/$PPID/stat" 2>/dev/null; then
                rest=${line##*)}
                set -- $rest
                shift 19 2>/dev/null && start=$1
                case $start in ''|*[!0-9]*) start="" ;; esac
            fi
            if [ -n "$start" ]; then
                printf '{"procStart":"%s","state":' "$start" > "$tmp" 2>/dev/null
            else
                printf '{"procStart":null,"state":' > "$tmp" 2>/dev/null
            fi && cat "$buf" >> "$tmp" 2>/dev/null && printf '}\n' >> "$tmp" 2>/dev/null && mv -f "$tmp" "$dir/$PPID.json" 2>/dev/null
            rm -f "$tmp" 2>/dev/null
        fi
    else
        rm -f "$buf" 2>/dev/null
        buf=""
    fi
fi
if [ -n "$chain" ]; then
    if [ "$wrote" = 1 ]; then
        sh -c "$chain" < "$buf"
        code=$?
        rm -f "$buf" 2>/dev/null
        exit "$code"
    fi
    exec sh -c "$chain"
fi
[ -n "$buf" ] && rm -f "$buf" 2>/dev/null
exit 0
"#;

const SCRIPT_FILE: &str = "lune-title.sh";

const SETTINGS_FILE: &str = "settings.json";

const CHAIN_PREFIX: &str = "# lune-chain: ";

const FOLDER: &str = "agy";

const DATED_LATE: Duration = Duration::from_secs(1);

const AGENT_FILES_MAX: usize = 1024;

const SCRIPT_BYTES_MAX: u64 = 64 * 1024;

const SETTINGS_BYTES_MAX: u64 = 1024 * 1024;

const _: () = assert!(AGENT_FILES_MAX > 0);
const _: () = assert!(SCRIPT_BYTES_MAX <= SETTINGS_BYTES_MAX);
const _: () = assert!(DATED_LATE.as_secs() > 0);

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("HOME").map(|home| PathBuf::from(home).join(".gemini/antigravity-cli"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    proc_start: Option<String>,
    state: Payload,
}

#[derive(Deserialize)]
struct Payload {
    cwd: Option<String>,
    workspace: Option<Workspace>,
    conversation_id: Option<String>,
    session_id: Option<String>,
    conversation_title: Option<String>,
    agent_state: Option<String>,
    tool_confirmation_pending: Option<bool>,
}

#[derive(Deserialize)]
struct Workspace {
    current_dir: Option<String>,
}

#[derive(Clone)]
struct Process {
    started: SystemTime,
    terminal: Option<TerminalKey>,
    status: Status,
    status_since: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    ino: u64,
    len: u64,
    mode: u32,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InstalledStamp {
    settings: Option<FileStamp>,
    script: Option<FileStamp>,
}

pub struct AgentFiles {
    lune: PathBuf,
    dir: PathBuf,
    agy: Option<PathBuf>,
    installed: Option<InstalledStamp>,
    processes: HashMap<process::Id, Process>,
    troubles: Troubles,
}

impl AgentFiles {
    #[must_use]
    pub fn new(lune: &Path, agy: Option<PathBuf>) -> Self {
        let dir = lune.join(FOLDER);
        if let Err(error) = fs::create_dir(&dir)
            && error.kind() != ErrorKind::AlreadyExists
        {
            log::say(&format!("{}: {error}", dir.display()));
        }
        AgentFiles {
            lune: lune.to_owned(),
            dir,
            agy,
            installed: None,
            processes: HashMap::new(),
            troubles: Troubles::default(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ensure_installed(&mut self) {
        let Some(agy) = self.agy.as_ref().filter(|agy| agy.is_dir()) else {
            self.installed = None;
            return;
        };
        let current = current_install_stamp(agy);
        if self.installed == Some(current) {
            return;
        }
        match install(agy, &self.lune) {
            Ok(stamp) => self.installed = Some(stamp),
            Err(error) => {
                self.installed = None;
                self.troubles.say(format!("agy's title hook in {}: {error}", agy.display()));
            },
        }
    }

    fn reap(&mut self, path: &Path) {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != ErrorKind::NotFound
        {
            self.troubles.say(format!("{}: {error}", path.display()));
        }
    }

    pub fn scan(&mut self) -> Vec<Agent> {
        self.ensure_installed();
        let mut paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles.say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            },
        };
        if paths.len() > AGENT_FILES_MAX {
            self.troubles.say(format!(
                "{}: {} files exceed the limit of {AGENT_FILES_MAX}",
                self.dir.display(),
                paths.len()
            ));
            paths.sort();
            paths.truncate(AGENT_FILES_MAX);
        }
        assert!(paths.len() <= AGENT_FILES_MAX);
        let paths_count = paths.len();
        let mut processes = HashMap::with_capacity(paths_count);
        let mut agents = Vec::with_capacity(paths_count);
        for path in paths {
            if let Some(temp_pid) = temp_file_pid(&path) {
                if process::start(temp_pid).is_none() {
                    self.reap(&path);
                }
                continue;
            }
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let Some(start) = process::start(pid) else {
                self.reap(&path);
                continue;
            };
            let id = process::Id { pid, start };
            let cached = self.processes.remove(&id);
            let Some(started) =
                cached.as_ref().map(|it| it.started).or_else(|| process::started(pid))
            else {
                continue;
            };
            match read(&path, id.start.as_str(), started) {
                Ok(Some((state, modified))) => {
                    let Some(cwd) = state.cwd() else {
                        self.troubles.say(format!("{}: cwd is missing or relative", path.display()));
                        continue;
                    };
                    let status = status_of(&state);
                    let status_since = match cached.as_ref() {
                        Some(was) if was.status == status => was.status_since,
                        Some(_) | None => modified,
                    };
                    let terminal = cached.map_or_else(
                        || terminal_or_trouble(pid, mux::terminal_of(&id), &mut self.troubles),
                        |it| it.terminal,
                    );
                    let found = Process {
                        started,
                        terminal,
                        status: status.clone(),
                        status_since,
                    };
                    processes.insert(id, found.clone());
                    agents.push(agent(pid, &state, cwd, found));
                },
                Ok(None) => {
                    self.reap(&path);
                },
                Err(error) => {
                    processes.extend(cached.map(|it| (id, it)));
                    if error.kind() != ErrorKind::NotFound {
                        self.troubles.say(format!("{}: {error}", path.display()));
                    }
                },
            }
        }
        assert!(agents.len() <= paths_count);
        assert!(processes.len() <= paths_count);
        self.processes = processes;
        self.troubles.done();
        agents
    }
}

impl Payload {
    fn cwd(&self) -> Option<Directory> {
        let parse = |raw: &str| Directory::parse(raw.trim().to_owned()).ok();
        self.cwd
            .as_deref()
            .and_then(parse)
            .or_else(|| self.workspace.as_ref()?.current_dir.as_deref().and_then(parse))
    }

    fn session_id(&self, pid: u32) -> Said {
        assert!(pid > 1);
        let id = self
            .conversation_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .or(self.session_id.as_deref())
            .and_then(|id| Said::new(id.trim().to_owned()));
        id.unwrap_or_else(|| {
            Said::new(format!("-{pid}")).expect("a pid with a dash is not empty")
        })
    }

    fn name(&self) -> Option<Name> {
        let text = Said::new(self.conversation_title.as_deref()?.trim().to_owned())?;
        Some(Name { text, source: Some(Source::User) })
    }
}

fn status_of(state: &Payload) -> Status {
    if state.tool_confirmation_pending == Some(true) {
        return Status::Waiting(Some(WaitingFor::PermissionPrompt));
    }
    let Some(word) = state.agent_state.as_deref().map(str::trim).filter(|word| !word.is_empty())
    else {
        return Status::Unreported;
    };
    match word.to_ascii_lowercase().as_str() {
        "idle" => Status::Idle,
        "working" | "tool_use" | "initializing" => Status::Busy,
        "reviewing" => Status::Waiting(Some(WaitingFor::InputNeeded)),
        "authenticating" => Status::Waiting(Some(WaitingFor::DialogOpen)),
        _ => Status::of(Some(word), None),
    }
}

fn agent(pid: u32, state: &Payload, cwd: Directory, process: Process) -> Agent {
    assert!(pid > 1);
    Agent {
        pid,
        harness: Harness::Antigravity,
        session_id: state.session_id(pid),
        cwd,
        status: process.status,
        status_since: Some(process.status_since),
        started_at: Some(process.started),
        name: state.name(),
        entrypoint: Some(Said::new("tui".to_owned()).expect("tui is not empty")),
        terminal: process.terminal,
    }
}

fn temp_file_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix('.')?;
    let stem = rest.strip_suffix(".tmp").or_else(|| rest.strip_suffix(".in"))?;
    let pid = stem.parse::<u32>().ok()?;
    if pid.to_string() != stem {
        return None;
    }
    (pid > 1).then_some(pid)
}

fn read_bounded_file(file: File, bytes_max: u64) -> io::Result<Vec<u8>> {
    assert!(bytes_max > 0);
    let mut bytes = Vec::new();
    file.take(bytes_max + 1).read_to_end(&mut bytes)?;
    let len = u64::try_from(bytes.len()).expect("a length under a u64 limit fits a u64");
    if len <= bytes_max {
        Ok(bytes)
    } else {
        let why = format!("it holds more than {bytes_max} bytes");
        Err(io::Error::new(ErrorKind::FileTooLarge, why))
    }
}

fn read_bounded(path: &Path, bytes_max: u64) -> io::Result<Vec<u8>> {
    read_bounded_file(File::open(path)?, bytes_max)
}

fn read(path: &Path, start: &str, started: SystemTime) -> io::Result<Option<(Payload, SystemTime)>> {
    assert_ne!(start, "");
    let file = File::open(path)?;
    let modified = file.metadata()?.modified()?;
    let bytes = read_bounded_file(file, FILE_BYTES_MAX)?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let stands = if let Some(recorded) = &envelope.proc_start {
        recorded == start
    } else {
        dated_since(modified, started)
    };
    Ok(stands.then_some((envelope.state, modified)))
}

fn dated_since(modified: SystemTime, started: SystemTime) -> bool {
    modified.checked_add(DATED_LATE).is_none_or(|late| started <= late)
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    Some(FileStamp {
        ino: meta.ino(),
        len: meta.len(),
        mode: meta.permissions().mode() & 0o777,
        modified: meta.modified().ok(),
    })
}

fn current_install_stamp(agy: &Path) -> InstalledStamp {
    InstalledStamp {
        settings: file_stamp(&agy.join(SETTINGS_FILE)),
        script: file_stamp(&agy.join(SCRIPT_FILE)),
    }
}

fn shell_quote(text: &str) -> String {
    let quoted = format!("'{}'", text.replace('\'', r"'\''"));
    assert!(quoted.starts_with('\''));
    assert!(quoted.ends_with('\''));
    quoted
}

fn script(lune: &Path, chain: Option<&str>) -> io::Result<String> {
    let dir = lune.join(FOLDER);
    let Some(dir) = dir.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "the daemon's folder is not UTF-8"));
    };
    assert_ne!(dir, "");
    let chain_json = serde_json::to_string(&chain)?;
    let dir_sh = shell_quote(dir);
    let chain_sh = shell_quote(chain.unwrap_or(""));
    let rendered = format!(
        "#!/bin/sh\n{CHAIN_PREFIX}{chain_json}\nset -f\ndir={dir_sh}\nchain={chain_sh}\n{SCRIPT_BODY}"
    );
    assert!(rendered.lines().nth(1).is_some_and(|line| line.starts_with(CHAIN_PREFIX)));
    Ok(rendered)
}

fn parse_chain(bytes: &[u8]) -> io::Result<Option<String>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let Some(line) = text.lines().take(4).find_map(|line| line.strip_prefix(CHAIN_PREFIX)) else {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{SCRIPT_FILE} is missing its {CHAIN_PREFIX}header"),
        ));
    };
    serde_json::from_str::<Option<String>>(line)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

fn read_script_and_chain(script_path: &Path) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
    let bytes = match read_bounded(script_path, SCRIPT_BYTES_MAX) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let chain = parse_chain(&bytes)?;
    Ok(Some((bytes, chain)))
}

fn own_command(script_path: &Path) -> io::Result<String> {
    let Some(path) = script_path.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "agy's folder is not UTF-8"));
    };
    assert_ne!(path, "");
    Ok(format!("exec {}", shell_quote(path)))
}

fn is_own_command(command: &str, script_path: &Path, ours: &str) -> bool {
    let Some(path) = script_path.to_str() else {
        return false;
    };
    if command == ours || command == path || command == format!("exec {path}") || command.contains(path) {
        return true;
    }
    let suffix = format!("/{SCRIPT_FILE}");
    command
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ';' | '&' | '|' | '(' | ')'))
        .any(|token| token == SCRIPT_FILE || token.ends_with(&suffix))
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    assert_eq!(mode & !0o777, 0);
    let dir = path.parent().expect("atomic write target has a parent directory");
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("atomic write target has a UTF-8 file name");
    let new = dir.join(format!(".{file_name}.{}.new", std::process::id()));
    let write_and_rename = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&new)?;
        file.write_all(bytes)?;
        file.set_permissions(Permissions::from_mode(mode))?;
        drop(file);
        fs::rename(&new, path)
    };
    write_and_rename().inspect_err(|_| {
        if let Err(error) = fs::remove_file(&new)
            && error.kind() != ErrorKind::NotFound
        {
            log::say(&format!("{}: {error}", new.display()));
        }
    })
}

fn write_script(
    script_path: &Path,
    text: &str,
    existing: Option<&[u8]>,
) -> io::Result<()> {
    let matches = match existing {
        Some(there) => there == text.as_bytes(),
        None => match read_bounded(script_path, SCRIPT_BYTES_MAX) {
            Ok(there) => there == text.as_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        },
    };
    let executable = match fs::metadata(script_path) {
        Ok(meta) => meta.permissions().mode() & 0o100 != 0,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    if matches && executable {
        return Ok(());
    }
    atomic_write(script_path, text.as_bytes(), 0o700)
}

fn write_settings(settings_path: &Path, object: &Map<String, Value>) -> io::Result<()> {
    let mode = match fs::metadata(settings_path) {
        Ok(meta) => meta.permissions().mode() & 0o777,
        Err(error) if error.kind() == ErrorKind::NotFound => 0o600,
        Err(error) => return Err(error),
    };
    let mut text = serde_json::to_string_pretty(object)?;
    text.push('\n');
    atomic_write(settings_path, text.as_bytes(), mode)
}

enum TitleAction {
    LeaveUntouched,
    RefreshScript,
    Wire { chain: Option<String>, keep_chain_from_script: bool },
}

fn decide_title_action(
    settings: &Map<String, Value>,
    script_path: &Path,
    ours: &str,
) -> io::Result<TitleAction> {
    match settings.get("title") {
        Some(Value::Bool(false)) => Ok(TitleAction::LeaveUntouched),
        Some(Value::Bool(true)) | None => {
            Ok(TitleAction::Wire { chain: None, keep_chain_from_script: false })
        },
        Some(Value::Object(title)) => {
            if title.get("enabled").and_then(Value::as_bool) == Some(false) {
                return Ok(TitleAction::LeaveUntouched);
            }
            if title.get("type").and_then(Value::as_str).is_some_and(|kind| kind != "command") {
                return Ok(TitleAction::LeaveUntouched);
            }
            let command = title
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|command| !command.is_empty());
            match command {
                Some(command) if command == ours => Ok(TitleAction::RefreshScript),
                Some(command) if is_own_command(command, script_path, ours) => {
                    Ok(TitleAction::Wire { chain: None, keep_chain_from_script: true })
                },
                Some(command) => Ok(TitleAction::Wire {
                    chain: Some(command.to_owned()),
                    keep_chain_from_script: false,
                }),
                None => Ok(TitleAction::Wire { chain: None, keep_chain_from_script: false }),
            }
        },
        Some(_) => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{SETTINGS_FILE}: title is neither an object nor a boolean"),
        )),
    }
}

fn validate_chain(
    chain: Option<String>,
    script_path: &Path,
    ours: &str,
) -> io::Result<Option<String>> {
    if let Some(ref command) = chain
        && is_own_command(command, script_path, ours)
    {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{SCRIPT_FILE} chains back into itself"),
        ));
    }
    Ok(chain)
}

fn install(agy: &Path, lune: &Path) -> io::Result<InstalledStamp> {
    assert!(agy.is_dir());
    let script_path = agy.join(SCRIPT_FILE);
    let settings_path = agy.join(SETTINGS_FILE);
    let ours = own_command(&script_path)?;
    assert_ne!(ours, "");

    let mut settings: Map<String, Value> = match read_bounded(&settings_path, SETTINGS_BYTES_MAX) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
            if text.trim().is_empty() {
                Map::new()
            } else {
                match serde_json::from_str::<Value>(text) {
                    Ok(Value::Object(map)) => map,
                    Ok(_) => {
                        let why = format!("{} is not a JSON object", settings_path.display());
                        return Err(io::Error::new(ErrorKind::InvalidData, why));
                    },
                    Err(error) => return Err(io::Error::new(ErrorKind::InvalidData, error)),
                }
            }
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(error),
    };

    match decide_title_action(&settings, &script_path, &ours)? {
        TitleAction::LeaveUntouched => Ok(current_install_stamp(agy)),
        TitleAction::RefreshScript => {
            let loaded = read_script_and_chain(&script_path)?;
            let existing = loaded.as_ref().map(|(bytes, _)| bytes.as_slice());
            let chain = validate_chain(
                loaded.as_ref().and_then(|(_, chain)| chain.clone()),
                &script_path,
                &ours,
            )?;
            if let Some(ref command) = chain {
                assert!(!is_own_command(command, &script_path, &ours));
            }
            let text = script(lune, chain.as_deref())?;
            write_script(&script_path, &text, existing)?;
            Ok(current_install_stamp(agy))
        },
        TitleAction::Wire { chain, keep_chain_from_script } => {
            let (existing, chain) = if keep_chain_from_script {
                let loaded = read_script_and_chain(&script_path)?;
                let chain = validate_chain(
                    loaded.as_ref().and_then(|(_, chain)| chain.clone()),
                    &script_path,
                    &ours,
                )?;
                (loaded.map(|(bytes, _)| bytes), chain)
            } else {
                (None, validate_chain(chain, &script_path, &ours)?)
            };
            if let Some(ref command) = chain {
                assert!(!is_own_command(command, &script_path, &ours));
            }
            let text = script(lune, chain.as_deref())?;
            write_script(&script_path, &text, existing.as_deref())?;

            let mut title = settings
                .remove("title")
                .and_then(|value| match value {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default();
            title.entry("type".to_owned()).or_insert_with(|| Value::String("command".to_owned()));
            title.insert("command".to_owned(), Value::String(ours));
            settings.insert("title".to_owned(), Value::Object(title));
            write_settings(&settings_path, &settings)?;
            Ok(current_install_stamp(agy))
        },
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use serde_json::json;

    use super::*;

    #[test]
    fn agy_states_map_to_lune_statuses_and_wait_reasons() {
        let state = |agent_state: Option<&str>, pending: Option<bool>| Payload {
            cwd: Some("/w".to_owned()),
            workspace: None,
            conversation_id: Some("abc".to_owned()),
            session_id: None,
            conversation_title: None,
            agent_state: agent_state.map(str::to_owned),
            tool_confirmation_pending: pending,
        };
        assert_eq!(status_of(&state(Some("idle"), None)), Status::Idle);
        assert_eq!(status_of(&state(Some("working"), None)), Status::Busy);
        assert_eq!(status_of(&state(Some("tool_use"), None)), Status::Busy);
        assert_eq!(status_of(&state(Some("initializing"), None)), Status::Busy);
        assert_eq!(
            status_of(&state(Some("working"), Some(true))),
            Status::Waiting(Some(WaitingFor::PermissionPrompt))
        );
        assert_eq!(
            status_of(&state(Some("reviewing"), None)),
            Status::Waiting(Some(WaitingFor::InputNeeded))
        );
        assert_eq!(
            status_of(&state(Some("authenticating"), None)),
            Status::Waiting(Some(WaitingFor::DialogOpen))
        );
        assert_eq!(
            status_of(&state(Some("error"), None)),
            Status::Other(Said::new("error".to_owned()).unwrap())
        );
        assert_eq!(status_of(&state(None, None)), Status::Unreported);
    }

    fn chain_of(script_path: &Path) -> Option<String> {
        read_script_and_chain(script_path).unwrap().and_then(|(_, chain)| chain)
    }

    #[test]
    fn install_wires_title_and_chains_an_existing_user_command() {
        let scratch = tempfile::tempdir().unwrap();
        let agy = scratch.path().join("agy");
        let lune = scratch.path().join("lune");
        fs::create_dir_all(&agy).unwrap();
        fs::create_dir_all(lune.join(FOLDER)).unwrap();

        let existing = json!({
            "trustedWorkspaces": ["/w"],
            "title": {
                "type": "command",
                "command": "printf 'custom-title: '; cat | tr -d '\\n'"
            }
        });
        fs::write(agy.join(SETTINGS_FILE), existing.to_string()).unwrap();

        install(&agy, &lune).unwrap();
        let updated: Value =
            serde_json::from_str(&fs::read_to_string(agy.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(updated["trustedWorkspaces"], json!(["/w"]));
        let cmd = updated["title"]["command"].as_str().unwrap().to_owned();
        assert!(cmd.contains(SCRIPT_FILE), "{cmd}");
        assert_eq!(
            chain_of(&agy.join(SCRIPT_FILE)).as_deref(),
            Some("printf 'custom-title: '; cat | tr -d '\\n'")
        );

        let payload = json!({"cwd": "/w", "conversation_id": "c1", "agent_state": "idle"});
        let mut child = Command::new("sh")
            .args(["-c", &cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.starts_with("custom-title: "), "{stdout}");
        assert!(stdout.contains("\"agent_state\":\"idle\""), "{stdout}");

        let mine = lune.join(FOLDER).join(format!("{}.json", std::process::id()));
        let wrote: Envelope = serde_json::from_slice(&fs::read(&mine).unwrap()).unwrap();
        assert_eq!(wrote.state.conversation_id.as_deref(), Some("c1"));
        assert_eq!(wrote.state.agent_state.as_deref(), Some("idle"));

        let other = scratch.path().join("other");
        fs::create_dir_all(other.join(FOLDER)).unwrap();
        install(&agy, &other).unwrap();
        assert_eq!(
            chain_of(&agy.join(SCRIPT_FILE)).as_deref(),
            Some("printf 'custom-title: '; cat | tr -d '\\n'")
        );
    }

    #[test]
    fn install_respects_disabled_title_and_invalid_settings() {
        let scratch = tempfile::tempdir().unwrap();
        let agy = scratch.path().join("agy");
        let lune = scratch.path().join("lune");
        fs::create_dir_all(&agy).unwrap();
        fs::create_dir_all(lune.join(FOLDER)).unwrap();

        let disabled = json!({"title": {"type": "command", "command": "echo hi", "enabled": false}});
        fs::write(agy.join(SETTINGS_FILE), disabled.to_string()).unwrap();
        install(&agy, &lune).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(agy.join(SETTINGS_FILE)).unwrap())
                .unwrap(),
            disabled
        );

        let non_command = json!({"title": {"type": "static", "text": "keep"}});
        fs::write(agy.join(SETTINGS_FILE), non_command.to_string()).unwrap();
        install(&agy, &lune).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(agy.join(SETTINGS_FILE)).unwrap())
                .unwrap(),
            non_command
        );

        fs::write(agy.join(SETTINGS_FILE), json!({"title": "plain"}).to_string()).unwrap();
        assert!(install(&agy, &lune).is_err());

        fs::write(agy.join(SETTINGS_FILE), "not json").unwrap();
        assert!(install(&agy, &lune).is_err());
        assert_eq!(fs::read_to_string(agy.join(SETTINGS_FILE)).unwrap(), "not json");
    }

    #[test]
    fn install_preserves_chain_across_script_errors_and_upgrades_without_self_chaining() {
        let scratch = tempfile::tempdir().unwrap();
        let agy = scratch.path().join("agy");
        let lune = scratch.path().join("lune");
        fs::create_dir_all(&agy).unwrap();
        fs::create_dir_all(lune.join(FOLDER)).unwrap();

        let existing = json!({"title": {"type": "command", "command": "printf user-title"}});
        fs::write(agy.join(SETTINGS_FILE), existing.to_string()).unwrap();
        install(&agy, &lune).unwrap();
        let script_path = agy.join(SCRIPT_FILE);
        assert_eq!(chain_of(&script_path).as_deref(), Some("printf user-title"));

        let unquoted = json!({
            "title": {
                "type": "command",
                "command": script_path.to_str().unwrap()
            }
        });
        fs::write(agy.join(SETTINGS_FILE), unquoted.to_string()).unwrap();
        install(&agy, &lune).unwrap();
        assert_eq!(chain_of(&script_path).as_deref(), Some("printf user-title"));

        let corrupted = "#!/bin/sh\necho corrupted\n";
        fs::write(&script_path, corrupted).unwrap();
        assert!(install(&agy, &lune).is_err());
        assert_eq!(fs::read_to_string(&script_path).unwrap(), corrupted);

        let self_chain = format!(
            "#!/bin/sh\n{CHAIN_PREFIX}{}\n",
            serde_json::to_string(&script_path.to_str().unwrap()).unwrap()
        );
        fs::write(&script_path, &self_chain).unwrap();
        assert!(install(&agy, &lune).is_err());
    }

    #[test]
    fn payload_cwd_trims_and_falls_back_to_workspace_current_dir() {
        let make = |cwd: Option<&str>, current_dir: Option<&str>| Payload {
            cwd: cwd.map(str::to_owned),
            workspace: current_dir.map(|dir| Workspace { current_dir: Some(dir.to_owned()) }),
            conversation_id: None,
            session_id: None,
            conversation_title: None,
            agent_state: None,
            tool_confirmation_pending: None,
        };
        assert_eq!(make(Some("  /w/one  "), Some("/w/two")).cwd().unwrap().as_str(), "/w/one");
        assert_eq!(make(Some("relative/path"), Some(" /w/two ")).cwd().unwrap().as_str(), "/w/two");
        assert_eq!(make(Some("  "), Some("/w/two")).cwd().unwrap().as_str(), "/w/two");
        assert!(make(Some("relative"), Some("also-relative")).cwd().is_none());
    }

    #[test]
    fn scan_reaps_dead_pid_files_and_caches_install_verification() {
        let scratch = tempfile::tempdir().unwrap();
        let agy = scratch.path().join("agy");
        let lune = scratch.path().join("lune");
        fs::create_dir_all(&agy).unwrap();
        fs::create_dir_all(&lune).unwrap();

        let mut files = AgentFiles::new(&lune, Some(agy.clone()));
        let dead_json = files.dir().join("99999999.json");
        let dead_tmp = files.dir().join(".99999999.tmp");
        fs::write(&dead_json, "{}").unwrap();
        fs::write(&dead_tmp, "{}").unwrap();

        assert_eq!(files.scan(), []);
        assert!(!dead_json.exists());
        assert!(!dead_tmp.exists());
        let first_stamp = files.installed.expect("hook installed and stamped");
        assert_eq!(files.scan(), []);
        assert_eq!(files.installed, Some(first_stamp));
    }
}
