use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use lune_domain::flows::{
    DEFAULT_CI_SETTLE_SECS, DEFAULT_FIX_CHECKS_ATTEMPTS_MAX, DEFAULT_FLOW_NAME,
    DEFAULT_REBASE_ATTEMPTS_MAX, DEFAULT_STEP_TIMEOUT_SECS, FlowSpec, InteractiveLandPolicy,
    StageKind, StageSpec,
};
use toml::Value;

const CONFIG_BYTES_MAX: usize = 1024 * 1024;

const FLOWS_DEFINED_MAX: usize = 64;

const _: () = assert!(CONFIG_BYTES_MAX > 0);
const _: () = assert!(FLOWS_DEFINED_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedFlows {
    pub specs: BTreeMap<String, FlowSpec>,
    pub warnings: Vec<String>,
}

impl Default for LoadedFlows {
    fn default() -> Self {
        let mut specs = BTreeMap::new();
        specs.insert(DEFAULT_FLOW_NAME.to_owned(), FlowSpec::default_spec());
        LoadedFlows {
            specs,
            warnings: Vec::new(),
        }
    }
}

impl LoadedFlows {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&FlowSpec> {
        self.specs.get(name)
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.specs.keys().cloned().collect()
    }
}

#[must_use]
pub fn config_path() -> Option<PathBuf> {
    let set = |name: &str| {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|value| value.is_absolute())
    };
    set("XDG_CONFIG_HOME")
        .map(|dir| dir.join("lune").join("config.toml"))
        .or_else(|| set("HOME").map(|home| home.join(".config").join("lune").join("config.toml")))
}

#[must_use]
pub fn load() -> LoadedFlows {
    let Some(path) = config_path() else {
        return LoadedFlows::default();
    };
    load_from(&path)
}

#[must_use]
pub fn load_from(path: &Path) -> LoadedFlows {
    let content = match read_config_file(path) {
        Ok(Some(content)) => content,
        Ok(None) => return LoadedFlows::default(),
        Err(why) => {
            let mut defaults = LoadedFlows::default();
            defaults.warnings.push(format!("{}: {why}", path.display()));
            return defaults;
        }
    };
    parse_flows_toml(&content)
}

#[must_use]
pub fn parse_flows_toml(content: &str) -> LoadedFlows {
    let mut loaded = LoadedFlows::default();
    let table: Value = match toml::from_str(content) {
        Ok(value) => value,
        Err(error) => {
            loaded
                .warnings
                .push(format!("config.toml did not parse: {error}"));
            return loaded;
        }
    };
    let Some(flows) = table.get("flows") else {
        return loaded;
    };
    let Some(flows_table) = flows.as_table() else {
        loaded
            .warnings
            .push("[flows] must be a table of named flows".to_owned());
        return loaded;
    };
    for (name, value) in flows_table {
        if loaded.specs.len() >= FLOWS_DEFINED_MAX && !loaded.specs.contains_key(name) {
            loaded.warnings.push(format!(
                "flow {name:?} skipped: exceeds limit of {FLOWS_DEFINED_MAX} flows"
            ));
            break;
        }
        match parse_flow_spec(name, value) {
            Ok(spec) => {
                loaded.specs.insert(name.clone(), spec);
            }
            Err(why) => {
                loaded.warnings.push(format!("flow {name:?}: {why}"));
            }
        }
    }
    loaded
}

fn parse_flow_spec(name: &str, value: &Value) -> Result<FlowSpec, String> {
    let table = value
        .as_table()
        .ok_or_else(|| "must be a table".to_owned())?;
    let default = FlowSpec::default_spec();
    let stages = match table.get("stages") {
        Some(raw) => parse_stages(raw)?,
        None => default.stages,
    };
    let rebase_attempts_max = read_u32(table, "rebase_attempts_max", DEFAULT_REBASE_ATTEMPTS_MAX)?;
    let fix_checks_attempts_max = read_u32(
        table,
        "fix_checks_attempts_max",
        DEFAULT_FIX_CHECKS_ATTEMPTS_MAX,
    )?;
    let wait_for_all_checks = read_bool(table, "wait_for_all_checks", true)?;
    let step_timeout_secs = read_u64(table, "step_timeout_secs", DEFAULT_STEP_TIMEOUT_SECS)?;
    let ci_settle_secs = read_u64(table, "ci_settle_secs", DEFAULT_CI_SETTLE_SECS)?;
    let interactive_land = match table.get("interactive_land") {
        None => InteractiveLandPolicy::CheckThenConfirmMerge,
        Some(raw) => match raw.as_str() {
            Some("confirm" | "check") => InteractiveLandPolicy::CheckThenConfirmMerge,
            Some("stop" | "ready") => InteractiveLandPolicy::StopReadyToMerge,
            _ => {
                return Err("interactive_land must be \"confirm\" or \"stop\"".to_owned());
            }
        },
    };
    let spec = FlowSpec {
        name: name.to_owned(),
        stages,
        rebase_attempts_max,
        fix_checks_attempts_max,
        wait_for_all_checks,
        step_timeout_secs,
        ci_settle_secs,
        interactive_land,
    };
    spec.validate()?;
    Ok(spec)
}

fn parse_stages(value: &Value) -> Result<Vec<StageSpec>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| "stages must be an array".to_owned())?;
    let mut stages = Vec::with_capacity(items.len());
    for item in items {
        stages.push(parse_stage(item)?);
    }
    Ok(stages)
}

fn parse_stage(value: &Value) -> Result<StageSpec, String> {
    if let Some(shorthand) = value.as_str() {
        return Ok(stage_from_shorthand(shorthand));
    }
    let table = value
        .as_table()
        .ok_or_else(|| "each stage must be a string or table".to_owned())?;
    let kind_name = table.get("kind").and_then(Value::as_str);
    let skill = table.get("skill").and_then(Value::as_str);
    let label = table.get("label").and_then(Value::as_str);
    match (kind_name, skill) {
        (Some("implement"), None) => Ok(StageSpec {
            label: label.unwrap_or("implement").to_owned(),
            kind: StageKind::Implement,
        }),
        (Some("land"), None) => Ok(StageSpec {
            label: label.unwrap_or("land").to_owned(),
            kind: StageKind::Land,
        }),
        (None | Some("review" | "skill"), Some(skill)) => {
            let trimmed = skill.trim().trim_start_matches('/');
            let fallback = default_skill_label(trimmed);
            Ok(StageSpec {
                label: label.unwrap_or(fallback).to_owned(),
                kind: StageKind::ReviewSkill {
                    skill: trimmed.to_owned(),
                },
            })
        }
        _ => Err(
            "stage table must specify kind = \"implement\" | \"land\" or skill = \"...\""
                .to_owned(),
        ),
    }
}

fn stage_from_shorthand(raw: &str) -> StageSpec {
    let trimmed = raw.trim().trim_start_matches('/');
    match trimmed {
        "implement" => StageSpec {
            label: "implement".to_owned(),
            kind: StageKind::Implement,
        },
        "land" => StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        },
        skill => StageSpec {
            label: default_skill_label(skill).to_owned(),
            kind: StageKind::ReviewSkill {
                skill: skill.to_owned(),
            },
        },
    }
}

fn default_skill_label(skill: &str) -> &str {
    match skill {
        "make-illegal-states-unrepresentable" => "illegal-states",
        "thermo-nuclear-review" => "thermo-nuclear",
        other => other,
    }
}

fn read_u32(table: &toml::map::Map<String, Value>, key: &str, default: u32) -> Result<u32, String> {
    let Some(value) = table.get(key) else {
        return Ok(default);
    };
    let raw = value
        .as_integer()
        .ok_or_else(|| format!("{key} must be an integer"))?;
    u32::try_from(raw).map_err(|_| format!("{key} must be a non-negative integer"))
}

fn read_u64(table: &toml::map::Map<String, Value>, key: &str, default: u64) -> Result<u64, String> {
    let Some(value) = table.get(key) else {
        return Ok(default);
    };
    let raw = value
        .as_integer()
        .ok_or_else(|| format!("{key} must be an integer"))?;
    u64::try_from(raw).map_err(|_| format!("{key} must be a non-negative integer"))
}

fn read_bool(
    table: &toml::map::Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    let Some(value) = table.get(key) else {
        return Ok(default);
    };
    value
        .as_bool()
        .ok_or_else(|| format!("{key} must be a boolean"))
}

fn read_config_file(path: &Path) -> Result<Option<String>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not open: {error}")),
    };
    let meta = file
        .metadata()
        .map_err(|error| format!("could not stat: {error}"))?;
    if !meta.file_type().is_file() {
        return Err("config path is not a regular file".to_owned());
    }
    let our_uid = rustix::process::getuid().as_raw();
    if meta.uid() != our_uid {
        return Err("config file is not owned by the current user".to_owned());
    }
    if meta.mode() & 0o022 != 0 {
        return Err("config file must not be group- or world-writable".to_owned());
    }
    let limit = u64::try_from(CONFIG_BYTES_MAX).expect("usize fits u64");
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read: {error}"))?;
    if bytes.len() > CONFIG_BYTES_MAX {
        return Err(format!("exceeds {CONFIG_BYTES_MAX} bytes"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("not valid UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_custom_flows_alongside_resume_without_breaking_defaults() {
        let toml = r#"
[resume]
allow_root = false

[flows.fast]
stages = ["implement", "/tigerstyle", "land"]
rebase_attempts_max = 2
fix_checks_attempts_max = 1
wait_for_all_checks = false
step_timeout_secs = 600
ci_settle_secs = 15
interactive_land = "stop"
"#;
        let loaded = parse_flows_toml(toml);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.get("default").is_some());
        let fast = loaded.get("fast").unwrap();
        assert_eq!(fast.stages.len(), 3);
        assert_eq!(fast.stages[1].label, "tigerstyle");
        assert_eq!(fast.rebase_attempts_max, 2);
        assert_eq!(fast.fix_checks_attempts_max, 1);
        assert!(!fast.wait_for_all_checks);
        assert_eq!(fast.step_timeout_secs, 600);
        assert_eq!(fast.ci_settle_secs, 15);
        assert_eq!(
            fast.interactive_land,
            InteractiveLandPolicy::StopReadyToMerge
        );
    }

    #[test]
    fn invalid_custom_flow_warns_and_preserves_default() {
        let toml = r#"
[flows.broken]
stages = ["land", "tigerstyle"]
"#;
        let loaded = parse_flows_toml(toml);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.get("broken").is_none());
        assert!(loaded.get("default").is_some());
    }
}
