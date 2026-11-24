use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, Permissions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lune_protocol::said::Said;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::agent::{
    file_text, named_pid, terminal_or_trouble, Agent, Harness, Name, Source, Status, WaitingFor,
};
use crate::mux::{self, TerminalKey};
use crate::watch::{self, Troubles};
use crate::{log, process, Directory};

const SCRIPT: &str = r#"wrote=0
buf=""
if [ -d "$dir" ] && [ ! -L "$dir" ] && [ -O "$dir" ]; then
    tmp="$dir/.$$.tmp"
    buf="$dir/.$$.in"
    if cat > "$buf" 2>/dev/null; then
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

pub struct AgentFiles {
    lune: PathBuf,
    dir: PathBuf,
    agy: Option<PathBuf>,
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
            processes: HashMap::new(),
            troubles: Troubles::default(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn scan(&mut self) -> Vec<Agent> {
        if let Some(agy) = self.agy.as_ref().filter(|agy| agy.is_dir())
            && let Err(error) = install(agy, &self.lune)
        {
            self.troubles.say(format!("agy's title hook in {}: {error}", agy.display()));
        }
        let paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles.say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            },
        };
        let mut processes = HashMap::with_capacity(paths.len());
        let mut agents = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let Some(start) = process::start(pid) else {
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
                    let why = format!("{}: no process of its pid wrote it", path.display());
                    self.troubles.say(why);
                },
                Err(error) => {
                    processes.extend(cached.map(|it| (id, it)));
                    if error.kind() != ErrorKind::NotFound {
                        self.troubles.say(format!("{}: {error}", path.display()));
                    }
                },
            }
        }
        self.processes = processes;
        self.troubles.done();
        agents
    }
}

impl Payload {
    fn cwd(&self) -> Option<Directory> {
        let dir = self
            .cwd
            .as_deref()
            .filter(|dir| !dir.trim().is_empty())
            .or_else(|| self.workspace.as_ref()?.current_dir.as_deref())?;
        Directory::parse(dir.to_owned()).ok()
    }

    fn session_id(&self, pid: u32) -> Said {
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

fn read(path: &Path, start: &str, started: SystemTime) -> io::Result<Option<(Payload, SystemTime)>> {
    let modified = fs::metadata(path)?.modified()?;
    let envelope: Envelope = serde_json::from_slice(&file_text(path)?)
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

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn script(lune: &Path, chain: Option<&str>) -> io::Result<String> {
    let dir = lune.join(FOLDER);
    let Some(dir) = dir.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "the daemon's folder is not UTF-8"));
    };
    let chain_json = serde_json::to_string(&chain)?;
    let dir_sh = shell_quote(dir);
    let chain_sh = shell_quote(chain.unwrap_or(""));
    Ok(format!(
        "#!/bin/sh\n{CHAIN_PREFIX}{chain_json}\nset -f\ndir={dir_sh}\nchain={chain_sh}\n{SCRIPT}"
    ))
}

fn read_chain(script_path: &Path) -> Option<String> {
    let text = fs::read_to_string(script_path).ok()?;
    let line = text.lines().find_map(|line| line.strip_prefix(CHAIN_PREFIX))?;
    serde_json::from_str::<Option<String>>(line).ok().flatten()
}

fn own_command(script_path: &Path) -> io::Result<String> {
    let Some(path) = script_path.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "agy's folder is not UTF-8"));
    };
    Ok(format!("exec {}", shell_quote(path)))
}

fn is_own_command(command: &str, script_path: &Path, ours: &str) -> bool {
    let Some(path) = script_path.to_str() else {
        return false;
    };
    command == ours || command == path || command == format!("exec {path}")
}

fn write_script(script_path: &Path, text: &str) -> io::Result<()> {
    let matches = fs::read(script_path).is_ok_and(|there| there == text.as_bytes());
    let executable = fs::metadata(script_path)
        .is_ok_and(|meta| meta.permissions().mode() & 0o100 != 0);
    if matches && executable {
        return Ok(());
    }
    let dir = script_path.parent().expect("lune-title.sh is in agy's folder");
    let new = dir.join(format!(".{SCRIPT_FILE}.{}.new", std::process::id()));
    fs::write(&new, text)?;
    fs::set_permissions(&new, Permissions::from_mode(0o700))?;
    fs::rename(&new, script_path).inspect_err(|_| {
        let _ = fs::remove_file(&new);
    })
}

fn write_settings(settings_path: &Path, object: &Map<String, Value>) -> io::Result<()> {
    let mode = fs::metadata(settings_path)
        .map_or(0o600, |meta| meta.permissions().mode() & 0o777);
    let mut text = serde_json::to_string_pretty(object)?;
    text.push('\n');
    let dir = settings_path.parent().expect("settings.json is in agy's folder");
    let new = dir.join(format!(".{SETTINGS_FILE}.{}.new", std::process::id()));
    fs::write(&new, text)?;
    fs::set_permissions(&new, Permissions::from_mode(mode))?;
    fs::rename(&new, settings_path).inspect_err(|_| {
        let _ = fs::remove_file(&new);
    })
}

fn install(agy: &Path, lune: &Path) -> io::Result<()> {
    let script_path = agy.join(SCRIPT_FILE);
    let settings_path = agy.join(SETTINGS_FILE);
    let ours = own_command(&script_path)?;

    let mut settings: Map<String, Value> = match fs::read_to_string(&settings_path) {
        Ok(text) if text.trim().is_empty() => Map::new(),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                let why = format!("{} is not a JSON object", settings_path.display());
                return Err(io::Error::new(ErrorKind::InvalidData, why));
            },
            Err(error) => return Err(io::Error::new(ErrorKind::InvalidData, error)),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(error),
    };

    match settings.get("title") {
        Some(Value::Bool(false)) => return Ok(()),
        Some(Value::Object(title))
            if title.get("enabled").and_then(Value::as_bool) == Some(false) =>
        {
            return Ok(());
        },
        Some(Value::Object(title))
            if title.get("command").and_then(Value::as_str) == Some(ours.as_str()) =>
        {
            let chain = read_chain(&script_path);
            let text = script(lune, chain.as_deref())?;
            return write_script(&script_path, &text);
        },
        _ => {},
    }

    let existing_command = settings
        .get("title")
        .and_then(Value::as_object)
        .and_then(|title| title.get("command"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty() && !is_own_command(command, &script_path, &ours))
        .map(str::to_owned);

    let chain = existing_command;
    let text = script(lune, chain.as_deref())?;
    write_script(&script_path, &text)?;

    let mut title = settings
        .remove("title")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    title.entry("type".to_owned()).or_insert_with(|| Value::String("command".to_owned()));
    title.insert("command".to_owned(), Value::String(ours));
    settings.insert("title".to_owned(), Value::Object(title));
    write_settings(&settings_path, &settings)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
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
            read_chain(&agy.join(SCRIPT_FILE)).as_deref(),
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
            read_chain(&agy.join(SCRIPT_FILE)).as_deref(),
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

        fs::write(agy.join(SETTINGS_FILE), "not json").unwrap();
        assert!(install(&agy, &lune).is_err());
        assert_eq!(fs::read_to_string(agy.join(SETTINGS_FILE)).unwrap(), "not json");
    }
}
