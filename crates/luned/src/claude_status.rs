use std::collections::HashMap;
use std::fs::{self, File, Metadata, OpenOptions, Permissions, TryLockError};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lune_protocol::said::Said;
use lune_protocol::telemetry::{
    AgentTelemetry, AgentUsage, ContextWindow, TelemetryText, UsageWindow, UsedPermille,
    cost_usd_to_micro_usd, strip_effort_suffix,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::agent::{FILE_BYTES_MAX, named_pid};
use crate::log;
use crate::process::{self, AgentId};
use crate::statuses::SOURCE_AGENTS_MAX;
use crate::watch::{self, Troubles};

const SCRIPT_BODY: &str = r#"if [ -n "$LUNE_CLAUDE_STATUS_DEPTH" ]; then
    exit 0
fi
LUNE_CLAUDE_STATUS_DEPTH=1
export LUNE_CLAUDE_STATUS_DEPTH
wrote=0
size=0
buf=""
if [ -d "$dir" ] && [ ! -L "$dir" ] && [ -O "$dir" ]; then
    tmp="$dir/.$$.tmp"
    buf="$dir/.$$.in"
    if if [ -n "$chain" ]; then dd ibs=1 obs=4096 count=65001; else head -c 65001; fi > "$buf" 2>/dev/null; then
        wrote=1
        set -- $(wc -c < "$buf" 2>/dev/null)
        size=$1
        case $size in ''|*[!0-9]*) size=65001 ;; esac
        if [ "$size" -gt 0 ] && [ "$size" -le 65000 ]; then
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
        if [ "$size" -gt 65000 ]; then
            { cat "$buf"; cat; } | sh -c "$chain"
        else
            sh -c "$chain" < "$buf"
        fi
        code=$?
        rm -f "$buf" 2>/dev/null
        exit "$code"
    fi
    exec sh -c "$chain"
fi
[ -n "$buf" ] && rm -f "$buf" 2>/dev/null
exit 0
"#;

const SCRIPT_FILE: &str = "lune-statusline.sh";

const SETTINGS_FILE: &str = "settings.json";

const LOCK_FILE: &str = ".lune-statusline.lock";

const CHAIN_PREFIX: &str = "# lune-chain: ";

const FOLDER: &str = "claude";

const DATED_LATE: Duration = Duration::from_secs(1);

const TORN: Duration = Duration::from_secs(1);

const STATUS_FILES_MAX: usize = SOURCE_AGENTS_MAX;

const SCRIPT_BYTES_MAX: u64 = 64 * 1024;

const SETTINGS_BYTES_MAX: u64 = 1024 * 1024;

const MODEL_EFFORTS_MAX: usize = 64;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

const _: () = assert!(STATUS_FILES_MAX > 0);
const _: () = assert!(SCRIPT_BYTES_MAX <= SETTINGS_BYTES_MAX);
const _: () = assert!(DATED_LATE.as_secs() > 0);
const _: () = assert!(TORN.as_secs() > 0);
const _: () = assert!(MODEL_EFFORTS_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeTelemetry {
    pub agent: AgentId,
    pub session_id: Option<Said>,
    pub telemetry: AgentTelemetry,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    proc_start: Option<String>,
    state: Payload,
}

#[derive(Deserialize)]
struct Payload {
    session_id: Option<String>,
    model: Option<RawModel>,
    effort: Option<String>,
    #[serde(alias = "effortLevel")]
    effort_level: Option<String>,
    context_window: Option<RawContextWindow>,
    rate_limits: Option<RawRateLimits>,
    cost: Option<RawCost>,
}

#[derive(Deserialize)]
struct RawModel {
    id: Option<String>,
    display_name: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct RawContextWindow {
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    current_usage: Option<RawCurrentUsage>,
}

#[derive(Deserialize)]
struct RawCurrentUsage {
    #[serde(rename = "input_tokens")]
    input: Option<u64>,
    #[serde(rename = "cache_creation_input_tokens")]
    cache_creation: Option<u64>,
    #[serde(rename = "cache_read_input_tokens")]
    cache_read: Option<u64>,
}

#[derive(Deserialize)]
struct RawRateLimits {
    five_hour: Option<RawRateLimit>,
    seven_day: Option<RawRateLimit>,
}

#[derive(Deserialize)]
struct RawRateLimit {
    used_percentage: Option<f64>,
    resets_at: Option<u64>,
}

#[derive(Deserialize)]
struct RawCost {
    total_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EffortSettings {
    default_effort: Option<TelemetryText>,
    by_model: HashMap<String, TelemetryText>,
}

impl EffortSettings {
    fn from_map(map: &Map<String, Value>) -> Self {
        let default_effort = map
            .get("effortLevel")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .and_then(|text| TelemetryText::parse(&text));
        let mut by_model = HashMap::new();
        if let Some(Value::Object(models)) = map.get("modelSettings") {
            for (model_id, entry) in models.iter().take(MODEL_EFFORTS_MAX) {
                let Some(effort) = entry
                    .get("effortLevel")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase)
                    .and_then(|text| TelemetryText::parse(&text))
                else {
                    continue;
                };
                if !model_id.trim().is_empty() {
                    by_model.insert(model_id.trim().to_owned(), effort);
                }
            }
        }
        assert!(by_model.len() <= MODEL_EFFORTS_MAX);
        Self {
            default_effort,
            by_model,
        }
    }

    fn resolve(&self, model_id: Option<&str>) -> Option<TelemetryText> {
        model_id
            .map(str::trim)
            .and_then(|id| self.by_model.get(id))
            .cloned()
            .or_else(|| self.default_effort.clone())
    }
}

#[derive(Clone)]
struct Process {
    started: SystemTime,
    entry: ClaudeTelemetry,
    torn_since: Option<Instant>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum InstallOutcome {
    Installed {
        stamp: InstalledStamp,
        efforts: EffortSettings,
    },
    Retry,
}

pub struct StatusFiles {
    lune: PathBuf,
    dir: PathBuf,
    claude: Option<PathBuf>,
    installed: Option<InstalledStamp>,
    efforts: EffortSettings,
    processes: HashMap<AgentId, Process>,
    troubles: Troubles,
}

impl StatusFiles {
    #[must_use]
    pub fn new(lune: &Path, claude: Option<PathBuf>) -> Self {
        let dir = lune.join(FOLDER);
        if let Err(error) = fs::create_dir(&dir)
            && error.kind() != ErrorKind::AlreadyExists
        {
            log::say(&format!("{}: {error}", dir.display()));
        }
        Self {
            lune: lune.to_owned(),
            dir,
            claude,
            installed: None,
            efforts: EffortSettings::default(),
            processes: HashMap::new(),
            troubles: Troubles::default(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ensure_installed(&mut self) {
        let Some(claude) = self.claude.as_ref().filter(|claude| claude.is_dir()) else {
            self.installed = None;
            self.efforts = EffortSettings::default();
            return;
        };
        let current = current_install_stamp(claude);
        if self.installed == Some(current) {
            return;
        }
        match install(claude, &self.lune) {
            Ok(InstallOutcome::Installed { stamp, efforts }) => {
                self.installed = Some(stamp);
                self.efforts = efforts;
            }
            Ok(InstallOutcome::Retry) => self.installed = None,
            Err(error) => {
                self.installed = None;
                self.troubles.say(format!(
                    "claude's statusLine hook in {}: {error}",
                    claude.display()
                ));
            }
        }
    }

    fn reap(&mut self, path: &Path) {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != ErrorKind::NotFound
        {
            self.troubles.say(format!("{}: {error}", path.display()));
        }
    }

    pub fn scan(&mut self) -> Vec<ClaudeTelemetry> {
        self.ensure_installed();
        let mut paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles
                    .say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            }
        };
        if paths.len() > STATUS_FILES_MAX {
            self.troubles.say(format!(
                "{}: {} files exceed the limit of {STATUS_FILES_MAX}",
                self.dir.display(),
                paths.len()
            ));
            paths.sort();
            paths.truncate(STATUS_FILES_MAX);
        }
        assert!(paths.len() <= STATUS_FILES_MAX);
        let paths_count = paths.len();
        let mut processes = HashMap::with_capacity(paths_count);
        let mut entries = Vec::with_capacity(paths_count);
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
            let Some(start) = process::start(pid.get()) else {
                self.reap(&path);
                continue;
            };
            let id = AgentId::new(pid, start);
            let cached = self.processes.remove(&id);
            let Some(started) = cached
                .as_ref()
                .map(|it| it.started)
                .or_else(|| process::started(pid.get()))
            else {
                continue;
            };
            match read(&path, id.start().as_str(), started, &self.efforts) {
                Ok(ReadOutcome::Live(Some((session_id, telemetry)))) => {
                    let entry = ClaudeTelemetry {
                        agent: id.clone(),
                        session_id,
                        telemetry,
                    };
                    processes.insert(
                        id,
                        Process {
                            started,
                            entry: entry.clone(),
                            torn_since: None,
                        },
                    );
                    entries.push(entry);
                }
                Ok(ReadOutcome::Live(None)) => {}
                Ok(ReadOutcome::Stale) => self.reap(&path),
                Err(error) => {
                    if error.kind() == ErrorKind::NotFound {
                        continue;
                    }
                    if let Some(mut was) = cached {
                        let since = *was.torn_since.get_or_insert_with(Instant::now);
                        if since.elapsed() < TORN {
                            entries.push(was.entry.clone());
                            processes.insert(id, was);
                            continue;
                        }
                    }
                    self.troubles.say(format!("{}: {error}", path.display()));
                }
            }
        }
        assert!(entries.len() <= paths_count);
        assert_eq!(entries.len(), processes.len());
        self.processes = processes;
        self.troubles.done();
        entries
    }
}

enum ReadOutcome {
    Stale,
    Live(Option<(Option<Said>, AgentTelemetry)>),
}

fn read(
    path: &Path,
    start: &str,
    started: SystemTime,
    efforts: &EffortSettings,
) -> io::Result<ReadOutcome> {
    assert_ne!(start, "");
    let file = File::open(path)?;
    let modified = file.metadata()?.modified()?;
    let bytes = read_bounded_file(file, FILE_BYTES_MAX)?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let valid = match &envelope.proc_start {
        Some(recorded) => recorded == start,
        None => dated_since(modified, started),
    };
    if !valid {
        return Ok(ReadOutcome::Stale);
    }
    let session_id = envelope
        .state
        .session_id
        .as_deref()
        .and_then(|id| Said::new(id.trim().to_owned()));
    let telemetry = envelope.state.telemetry(efforts);
    Ok(ReadOutcome::Live(
        telemetry.map(|telemetry| (session_id, telemetry)),
    ))
}

impl Payload {
    fn telemetry(&self, efforts: &EffortSettings) -> Option<AgentTelemetry> {
        let model_id = self.model.as_ref().and_then(|model| model.id.as_deref());
        let effort = self
            .effort
            .as_deref()
            .or(self.effort_level.as_deref())
            .or_else(|| {
                self.model
                    .as_ref()
                    .and_then(|model| model.effort.as_deref())
            })
            .map(str::to_ascii_lowercase)
            .and_then(|text| TelemetryText::parse(&text))
            .or_else(|| efforts.resolve(model_id));
        let model = self
            .model
            .as_ref()
            .and_then(|model| model.display_name.as_deref().or(model.id.as_deref()))
            .map(|raw| clean_claude_model(raw, effort.as_ref().map(TelemetryText::as_str)))
            .and_then(TelemetryText::parse);
        let context = self.context_window.as_ref().and_then(parse_context);
        let usage = parse_usage(self.rate_limits.as_ref(), self.cost.as_ref());
        AgentTelemetry::new(model, effort, context, usage)
    }
}

fn clean_claude_model<'a>(raw: &'a str, effort: Option<&str>) -> &'a str {
    let trimmed = raw.trim();
    let without_brand = trimmed
        .strip_prefix("Claude ")
        .or_else(|| trimmed.strip_prefix("claude "))
        .filter(|rest| !rest.trim().is_empty())
        .unwrap_or(trimmed);
    let without_context = strip_context_suffix(without_brand);
    match effort {
        Some(effort) => strip_effort_suffix(without_context, effort),
        None => without_context,
    }
}

fn strip_context_suffix(model: &str) -> &str {
    let Some(without_close) = model.strip_suffix(" context)") else {
        return model;
    };
    let Some((prefix, size)) = without_close.rsplit_once('(') else {
        return model;
    };
    let prefix = prefix.trim_end();
    let size = size.trim();
    let valid_size = size.len() >= 2
        && size
            .as_bytes()
            .last()
            .is_some_and(|unit| matches!(unit, b'k' | b'K' | b'm' | b'M'))
        && size[..size.len() - 1]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.');
    if !prefix.is_empty() && valid_size {
        prefix
    } else {
        model
    }
}

fn parse_context(raw: &RawContextWindow) -> Option<ContextWindow> {
    let window_tokens = raw.context_window_size.and_then(NonZeroU64::new);
    let used_tokens = raw.current_usage.as_ref().and_then(|usage| {
        if usage.input.is_none() && usage.cache_creation.is_none() && usage.cache_read.is_none() {
            return None;
        }
        Some(
            usage
                .input
                .unwrap_or(0)
                .saturating_add(usage.cache_creation.unwrap_or(0))
                .saturating_add(usage.cache_read.unwrap_or(0)),
        )
    });
    let used_permille = raw
        .used_percentage
        .and_then(UsedPermille::of_percent)
        .or_else(|| Some(UsedPermille::of_ratio(used_tokens?, window_tokens?)))?;
    Some(ContextWindow {
        used_permille,
        window_tokens,
        used_tokens,
    })
}

fn parse_rate_limit(label: &str, raw: &RawRateLimit) -> Option<UsageWindow> {
    let used_permille = UsedPermille::of_percent(raw.used_percentage?)?;
    let resets_at = raw
        .resets_at
        .filter(|secs| *secs > 0)
        .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
    let label = TelemetryText::parse(label)?;
    Some(UsageWindow {
        label,
        used_permille,
        resets_at,
    })
}

fn parse_usage(limits: Option<&RawRateLimits>, cost: Option<&RawCost>) -> Option<AgentUsage> {
    let mut windows = Vec::new();
    if let Some(limits) = limits {
        if let Some(five_hour) = limits
            .five_hour
            .as_ref()
            .and_then(|raw| parse_rate_limit("5h", raw))
        {
            windows.push(five_hour);
        }
        if let Some(seven_day) = limits
            .seven_day
            .as_ref()
            .and_then(|raw| parse_rate_limit("7d", raw))
        {
            windows.push(seven_day);
        }
    }
    let session_cost_micro_usd = cost
        .and_then(|cost| cost.total_cost_usd)
        .and_then(cost_usd_to_micro_usd);
    AgentUsage::new(windows, session_cost_micro_usd)
}

fn dated_since(modified: SystemTime, started: SystemTime) -> bool {
    modified
        .checked_add(DATED_LATE)
        .is_none_or(|late| started <= late)
}

fn temp_file_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix('.')?;
    let stem = rest
        .strip_suffix(".tmp")
        .or_else(|| rest.strip_suffix(".in"))?;
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

fn file_stamp_of(meta: &Metadata) -> FileStamp {
    FileStamp {
        ino: meta.ino(),
        len: meta.len(),
        mode: meta.permissions().mode() & 0o777,
        modified: meta.modified().ok(),
    }
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = fs::metadata(path).ok()?;
    Some(file_stamp_of(&meta))
}

fn current_install_stamp(claude: &Path) -> InstalledStamp {
    InstalledStamp {
        settings: file_stamp(&claude.join(SETTINGS_FILE)),
        script: file_stamp(&claude.join(SCRIPT_FILE)),
    }
}

fn shell_quote(text: &str) -> String {
    let quoted = format!("'{}'", text.replace('\'', r"'\''"));
    assert!(quoted.starts_with('\''));
    assert!(quoted.ends_with('\''));
    quoted
}

struct ExternalCommand(String);

impl ExternalCommand {
    fn parse(raw: &str, script_path: &Path, ours: &str) -> io::Result<Option<Self>> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if is_own_command(trimmed, script_path, ours) {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("{SCRIPT_FILE} chains back into itself"),
            ));
        }
        Ok(Some(Self(trimmed.to_owned())))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn script(lune: &Path, chain: Option<&ExternalCommand>) -> io::Result<String> {
    let dir = lune.join(FOLDER);
    let Some(dir) = dir.to_str() else {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "the daemon's folder is not UTF-8",
        ));
    };
    assert_ne!(dir, "");
    let chain = chain.map(ExternalCommand::as_str);
    let chain_json = serde_json::to_string(&chain)?;
    let dir_sh = shell_quote(dir);
    let chain_sh = shell_quote(chain.unwrap_or(""));
    let rendered = format!(
        "#!/bin/sh\n{CHAIN_PREFIX}{chain_json}\nset -f\ndir={dir_sh}\nchain={chain_sh}\n{SCRIPT_BODY}"
    );
    assert!(
        rendered
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with(CHAIN_PREFIX))
    );
    Ok(rendered)
}

fn parse_chain(
    bytes: &[u8],
    script_path: &Path,
    ours: &str,
) -> io::Result<Option<ExternalCommand>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let Some(line) = text
        .lines()
        .take(4)
        .find_map(|line| line.strip_prefix(CHAIN_PREFIX))
    else {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{SCRIPT_FILE} is missing its {CHAIN_PREFIX}header"),
        ));
    };
    let raw = serde_json::from_str::<Option<String>>(line)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    match raw {
        Some(command) => ExternalCommand::parse(&command, script_path, ours),
        None => Ok(None),
    }
}

fn read_script_and_chain(
    script_path: &Path,
    ours: &str,
) -> io::Result<Option<(Vec<u8>, Option<ExternalCommand>)>> {
    let bytes = match read_bounded(script_path, SCRIPT_BYTES_MAX) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let chain = parse_chain(&bytes, script_path, ours)?;
    Ok(Some((bytes, chain)))
}

fn own_command(script_path: &Path) -> io::Result<String> {
    let Some(path) = script_path.to_str() else {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "claude's folder is not UTF-8",
        ));
    };
    assert_ne!(path, "");
    Ok(format!("exec {}", shell_quote(path)))
}

fn is_own_command(command: &str, script_path: &Path, ours: &str) -> bool {
    let Some(path) = script_path.to_str() else {
        return false;
    };
    if command == ours
        || command == path
        || command == format!("exec {path}")
        || command.contains(path)
    {
        return true;
    }
    let suffix = format!("/{SCRIPT_FILE}");
    command
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ';' | '&' | '|' | '(' | ')'))
        .any(|token| token == SCRIPT_FILE || token.ends_with(&suffix))
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<FileStamp> {
    assert_eq!(mode & !0o777, 0);
    let dir = path
        .parent()
        .expect("atomic write target has a parent directory");
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("atomic write target has a UTF-8 file name");
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let new = dir.join(format!(".{file_name}.{}.{seq}.new", std::process::id()));
    let write_and_rename = || -> io::Result<FileStamp> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&new)?;
        file.write_all(bytes)?;
        file.set_permissions(Permissions::from_mode(mode))?;
        let stamp = file_stamp_of(&file.metadata()?);
        drop(file);
        fs::rename(&new, path)?;
        Ok(stamp)
    };
    write_and_rename().inspect_err(|_| {
        if let Err(error) = fs::remove_file(&new)
            && error.kind() != ErrorKind::NotFound
        {
            log::say(&format!("{}: {error}", new.display()));
        }
    })
}

fn write_script(script_path: &Path, text: &str, existing: Option<&[u8]>) -> io::Result<FileStamp> {
    let matches = match existing {
        Some(there) => there == text.as_bytes(),
        None => match read_bounded(script_path, SCRIPT_BYTES_MAX) {
            Ok(there) => there == text.as_bytes(),
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        },
    };
    if matches {
        match fs::metadata(script_path) {
            Ok(meta) if meta.permissions().mode() & 0o100 != 0 => {
                return Ok(file_stamp_of(&meta));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    atomic_write(script_path, text.as_bytes(), 0o700)
}

enum SettingsSnapshot {
    Missing,
    Present {
        map: Map<String, Value>,
        stamp: FileStamp,
        mode: u32,
    },
}

impl SettingsSnapshot {
    fn map(&self) -> Option<&Map<String, Value>> {
        match self {
            SettingsSnapshot::Missing => None,
            SettingsSnapshot::Present { map, .. } => Some(map),
        }
    }

    fn into_map(self) -> Map<String, Value> {
        match self {
            SettingsSnapshot::Missing => Map::new(),
            SettingsSnapshot::Present { map, .. } => map,
        }
    }

    fn stamp(&self) -> Option<FileStamp> {
        match self {
            SettingsSnapshot::Missing => None,
            SettingsSnapshot::Present { stamp, .. } => Some(*stamp),
        }
    }

    fn mode(&self) -> u32 {
        match self {
            SettingsSnapshot::Missing => 0o600,
            SettingsSnapshot::Present { mode, .. } => *mode,
        }
    }
}

fn read_settings(settings_path: &Path) -> io::Result<SettingsSnapshot> {
    let file = match File::open(settings_path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(SettingsSnapshot::Missing),
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    let stamp = file_stamp_of(&meta);
    let mode = stamp.mode;
    let bytes = read_bounded_file(file, SETTINGS_BYTES_MAX)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let map = if text.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                let why = format!("{} is not a JSON object", settings_path.display());
                return Err(io::Error::new(ErrorKind::InvalidData, why));
            }
            Err(error) => return Err(io::Error::new(ErrorKind::InvalidData, error)),
        }
    };
    Ok(SettingsSnapshot::Present { map, stamp, mode })
}

fn write_settings(
    settings_path: &Path,
    object: &Map<String, Value>,
    mode: u32,
) -> io::Result<FileStamp> {
    let mut text = serde_json::to_string_pretty(object)?;
    text.push('\n');
    atomic_write(settings_path, text.as_bytes(), mode)
}

enum ChainSource {
    None,
    External(ExternalCommand),
    FromExistingScript,
}

enum StatusLineAction {
    LeaveUntouched,
    RefreshScript,
    Wire(ChainSource),
}

fn decide_status_line_action(
    settings: Option<&Map<String, Value>>,
    script_path: &Path,
    ours: &str,
) -> io::Result<StatusLineAction> {
    match settings.and_then(|map| map.get("statusLine")) {
        Some(Value::Bool(false)) => Ok(StatusLineAction::LeaveUntouched),
        Some(Value::Bool(true)) | None => Ok(StatusLineAction::Wire(ChainSource::None)),
        Some(Value::Object(status_line)) => {
            match status_line.get("enabled") {
                None | Some(Value::Bool(true)) => {}
                Some(Value::Bool(false)) => return Ok(StatusLineAction::LeaveUntouched),
                Some(_) => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("{SETTINGS_FILE}: statusLine.enabled is not a boolean"),
                    ));
                }
            }
            match status_line.get("type") {
                None => {}
                Some(Value::String(kind)) if kind == "command" => {}
                Some(Value::String(_)) => return Ok(StatusLineAction::LeaveUntouched),
                Some(_) => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("{SETTINGS_FILE}: statusLine.type is not a string"),
                    ));
                }
            }
            match status_line.get("command") {
                None => Ok(StatusLineAction::Wire(ChainSource::None)),
                Some(Value::String(raw)) => {
                    let trimmed = raw.trim();
                    if trimmed == ours {
                        Ok(StatusLineAction::RefreshScript)
                    } else if is_own_command(trimmed, script_path, ours) {
                        Ok(StatusLineAction::Wire(ChainSource::FromExistingScript))
                    } else {
                        match ExternalCommand::parse(trimmed, script_path, ours)? {
                            Some(cmd) => Ok(StatusLineAction::Wire(ChainSource::External(cmd))),
                            None => Ok(StatusLineAction::Wire(ChainSource::None)),
                        }
                    }
                }
                Some(_) => Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("{SETTINGS_FILE}: statusLine.command is not a string"),
                )),
            }
        }
        Some(_) => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{SETTINGS_FILE}: statusLine is neither an object nor a boolean"),
        )),
    }
}

struct FileLock(File);

impl FileLock {
    fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(FileLock(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn install(claude: &Path, lune: &Path) -> io::Result<InstallOutcome> {
    assert!(claude.is_dir());
    let Some(_lock) = FileLock::try_acquire(&claude.join(LOCK_FILE))? else {
        return Ok(InstallOutcome::Retry);
    };

    let script_path = claude.join(SCRIPT_FILE);
    let settings_path = claude.join(SETTINGS_FILE);
    let ours = own_command(&script_path)?;
    assert_ne!(ours, "");

    let snapshot = read_settings(&settings_path)?;
    let efforts = snapshot
        .map()
        .map(EffortSettings::from_map)
        .unwrap_or_default();
    let action = decide_status_line_action(snapshot.map(), &script_path, &ours)?;

    match action {
        StatusLineAction::LeaveUntouched => Ok(InstallOutcome::Installed {
            stamp: InstalledStamp {
                settings: snapshot.stamp(),
                script: file_stamp(&script_path),
            },
            efforts,
        }),
        StatusLineAction::RefreshScript => {
            let loaded = read_script_and_chain(&script_path, &ours)?;
            let existing = loaded.as_ref().map(|(bytes, _)| bytes.as_slice());
            let chain = loaded.as_ref().and_then(|(_, chain)| chain.as_ref());
            let text = script(lune, chain)?;
            let script_stamp = write_script(&script_path, &text, existing)?;
            Ok(InstallOutcome::Installed {
                stamp: InstalledStamp {
                    settings: snapshot.stamp(),
                    script: Some(script_stamp),
                },
                efforts,
            })
        }
        StatusLineAction::Wire(source) => {
            let (existing, chain) = match source {
                ChainSource::None => (None, None),
                ChainSource::External(cmd) => (None, Some(cmd)),
                ChainSource::FromExistingScript => {
                    let loaded = read_script_and_chain(&script_path, &ours)?;
                    match loaded {
                        Some((bytes, chain)) => (Some(bytes), chain),
                        None => (None, None),
                    }
                }
            };
            let text = script(lune, chain.as_ref())?;
            let script_stamp = write_script(&script_path, &text, existing.as_deref())?;

            if file_stamp(&settings_path) != snapshot.stamp() {
                return Ok(InstallOutcome::Retry);
            }
            let mode = snapshot.mode();
            let mut settings = snapshot.into_map();
            let mut status_line = settings
                .remove("statusLine")
                .and_then(|value| match value {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default();
            status_line
                .entry("type".to_owned())
                .or_insert_with(|| Value::String("command".to_owned()));
            status_line.insert("command".to_owned(), Value::String(ours));
            status_line
                .entry("padding".to_owned())
                .or_insert_with(|| Value::Number(0.into()));
            settings.insert("statusLine".to_owned(), Value::Object(status_line));
            let settings_stamp = write_settings(&settings_path, &settings, mode)?;
            Ok(InstallOutcome::Installed {
                stamp: InstalledStamp {
                    settings: Some(settings_stamp),
                    script: Some(script_stamp),
                },
                efforts,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use serde_json::json;

    use super::*;

    fn chain_of(script_path: &Path) -> Option<String> {
        let ours = own_command(script_path).unwrap();
        read_script_and_chain(script_path, &ours)
            .unwrap()
            .and_then(|(_, chain)| chain.map(|cmd| cmd.as_str().to_owned()))
    }

    #[test]
    fn install_wires_status_line_silently_when_unset_and_chains_existing_command() {
        let scratch = tempfile::tempdir().unwrap();
        let claude = scratch.path().join("claude");
        let lune = scratch.path().join("lune");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(lune.join(FOLDER)).unwrap();

        let initial = json!({
            "effortLevel": "high",
            "modelSettings": {
                "claude-opus-4-7": { "effortLevel": "max" }
            }
        });
        fs::write(claude.join(SETTINGS_FILE), initial.to_string()).unwrap();

        let InstallOutcome::Installed { efforts, .. } = install(&claude, &lune).unwrap() else {
            panic!("expected installed");
        };
        assert_eq!(
            efforts.resolve(Some("claude-opus-4-7")).unwrap().as_str(),
            "max"
        );
        assert_eq!(
            efforts.resolve(Some("claude-sonnet-4-6")).unwrap().as_str(),
            "high"
        );

        let updated: Value =
            serde_json::from_str(&fs::read_to_string(claude.join(SETTINGS_FILE)).unwrap()).unwrap();
        let cmd = updated["statusLine"]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(cmd.contains(SCRIPT_FILE), "{cmd}");
        assert_eq!(chain_of(&claude.join(SCRIPT_FILE)), None);

        let payload = json!({
            "session_id": "s1",
            "model": { "id": "claude-opus-4-7", "display_name": "Claude Opus 4.7 (1M context)" },
            "context_window": { "context_window_size": 1_000_000, "used_percentage": 19.4 },
            "rate_limits": {
                "five_hour": { "used_percentage": 35.0, "resets_at": 1_780_000_000_u64 },
                "seven_day": { "used_percentage": 63.0, "resets_at": 1_780_200_000_u64 }
            },
            "cost": { "total_cost_usd": 0.26 }
        });
        let mut child = Command::new("sh")
            .args(["-c", &cmd])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "",
            "without a chained command, stdout stays empty"
        );

        let mut files = StatusFiles::new(&lune, Some(claude.clone()));
        let entries = files.scan();
        assert_eq!(entries.len(), 1);
        let telemetry = &entries[0].telemetry;
        assert_eq!(entries[0].session_id.as_deref(), Some("s1"));
        assert_eq!(
            telemetry.model().map(TelemetryText::as_str),
            Some("Opus 4.7")
        );
        assert_eq!(telemetry.effort().map(TelemetryText::as_str), Some("max"));
        let context = telemetry.context().unwrap();
        assert_eq!(context.used_permille.get(), 194);
        assert_eq!(context.window_tokens.map(NonZeroU64::get), Some(1_000_000));
        let usage = telemetry.usage().unwrap();
        assert_eq!(usage.windows().len(), 2);
        assert_eq!(usage.windows()[0].label.as_str(), "5h");
        assert_eq!(usage.windows()[0].used_permille.get(), 350);
        assert_eq!(usage.windows()[1].label.as_str(), "7d");
        assert_eq!(usage.windows()[1].used_permille.get(), 630);
        assert_eq!(usage.session_cost_micro_usd(), Some(260_000));

        let scratch2 = tempfile::tempdir().unwrap();
        let claude2 = scratch2.path().join("claude");
        let lune2 = scratch2.path().join("lune");
        fs::create_dir_all(&claude2).unwrap();
        fs::create_dir_all(lune2.join(FOLDER)).unwrap();
        let with_user = json!({
            "statusLine": {
                "type": "command",
                "command": "printf 'user-bar'"
            }
        });
        fs::write(claude2.join(SETTINGS_FILE), with_user.to_string()).unwrap();
        install(&claude2, &lune2).unwrap();
        let updated2: Value =
            serde_json::from_str(&fs::read_to_string(claude2.join(SETTINGS_FILE)).unwrap())
                .unwrap();
        let cmd2 = updated2["statusLine"]["command"].as_str().unwrap();
        assert_eq!(
            chain_of(&claude2.join(SCRIPT_FILE)).as_deref(),
            Some("printf 'user-bar'")
        );
        let mut child = Command::new("sh")
            .args(["-c", cmd2])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "user-bar");
    }

    #[test]
    fn clean_claude_model_strips_brand_context_and_matching_effort() {
        assert_eq!(
            clean_claude_model("Claude Opus 4.7 (1M context)", Some("high")),
            "Opus 4.7"
        );
        assert_eq!(
            clean_claude_model("Claude Sonnet 4.6 (200K context)", None),
            "Sonnet 4.6"
        );
        assert_eq!(
            clean_claude_model("Opus 4.7 (high)", Some("high")),
            "Opus 4.7"
        );
        assert_eq!(clean_claude_model("Claude ", None), "Claude");
    }
}
