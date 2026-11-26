use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use toml::Value;

const CONFIG_BYTES_MAX: usize = 1024 * 1024;

const RULES_MAX: usize = 256;

const COMMAND_ELEMENTS_MAX: usize = 64;

const CHOICES_PER_ELEMENT_MAX: usize = 64;

const FLAGS_PER_RULE_MAX: usize = 256;

const DIRS_PER_RULE_MAX: usize = 64;

const _: () = assert!(CONFIG_BYTES_MAX > 0);
const _: () = assert!(RULES_MAX >= BuiltinRule::ALL.len());
const _: () = assert!(CHOICES_PER_ELEMENT_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuiltinRule {
    Agy,
    Claude,
    Codex,
    Opencode,
    Pi,
}

impl BuiltinRule {
    pub const ALL: [BuiltinRule; 5] = [
        BuiltinRule::Agy,
        BuiltinRule::Claude,
        BuiltinRule::Codex,
        BuiltinRule::Opencode,
        BuiltinRule::Pi,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BuiltinRule::Agy => "agy",
            BuiltinRule::Claude => "claude",
            BuiltinRule::Codex => "codex",
            BuiltinRule::Opencode => "opencode",
            BuiltinRule::Pi => "pi",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "agy" => Some(BuiltinRule::Agy),
            "claude" => Some(BuiltinRule::Claude),
            "codex" => Some(BuiltinRule::Codex),
            "opencode" => Some(BuiltinRule::Opencode),
            "pi" => Some(BuiltinRule::Pi),
            _ => None,
        }
    }

    #[must_use]
    pub fn rule(self) -> TrustRule {
        match self {
            BuiltinRule::Claude => claude_builtin_rule(),
            BuiltinRule::Pi => pi_builtin_rule(),
            BuiltinRule::Agy => agy_builtin_rule(),
            BuiltinRule::Codex => TrustRule {
                name: "codex".to_owned(),
                origin: RuleOrigin::Builtin { overrides: Vec::new() },
                program: RuleProgram::bare("codex"),
                args: vec![CommandElement::Literal("resume".to_owned()), CommandElement::Uuid],
                dirs: None,
                flags: None,
            },
            BuiltinRule::Opencode => TrustRule {
                name: "opencode".to_owned(),
                origin: RuleOrigin::Builtin { overrides: Vec::new() },
                program: RuleProgram::bare("opencode"),
                args: vec![CommandElement::Literal("--session".to_owned()), CommandElement::Id],
                dirs: None,
                flags: None,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyVec<T>(Vec<T>);

impl<T> NonEmptyVec<T> {
    #[must_use]
    pub fn new(items: Vec<T>) -> Option<Self> {
        (!items.is_empty()).then_some(Self(items))
    }

    #[must_use]
    pub fn single(item: T) -> Self {
        Self(vec![item])
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<'a, T> IntoIterator for &'a NonEmptyVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedAbsPath(String);

impl NormalizedAbsPath {
    pub fn parse_absolute(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("path must not be empty".to_owned());
        }
        if raw.chars().any(char::is_control) {
            return Err("path contains a control character".to_owned());
        }
        if !raw.starts_with('/') {
            return Err(format!("path {raw:?} must be absolute"));
        }
        if has_parent_component(raw) {
            return Err(format!("path {raw:?} must not contain '..'"));
        }
        let normalized = normalize_lexical(raw);
        assert!(normalized.starts_with('/'));
        assert!(!has_parent_component(&normalized));
        Ok(Self(normalized))
    }

    pub fn parse_expanded(raw: &str, home: Option<&Path>) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("directory path must not be empty".to_owned());
        }
        if raw.chars().any(char::is_control) {
            return Err("directory path contains a control character".to_owned());
        }
        let expanded = if raw == "~" || raw.starts_with("~/") {
            let Some(home) = home else {
                return Err(format!("cannot expand {raw:?}: HOME is not set to an absolute path"));
            };
            let home_str = home
                .to_str()
                .ok_or_else(|| "HOME is not valid UTF-8".to_owned())?;
            if raw == "~" {
                home_str.to_owned()
            } else {
                format!("{}{}", home_str.trim_end_matches('/'), &raw[1..])
            }
        } else if raw.starts_with('/') {
            raw.to_owned()
        } else {
            return Err(format!("directory {raw:?} must be an absolute path or start with ~/"));
        };
        if has_parent_component(&expanded) {
            return Err(format!("directory {raw:?} must not contain '..'"));
        }
        let normalized = normalize_lexical(&expanded);
        assert!(normalized.starts_with('/'));
        assert!(!has_parent_component(&normalized));
        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NormalizedAbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleProgram {
    Bare(String),
    Absolute(NormalizedAbsPath),
}

impl RuleProgram {
    pub fn parse(program: &str) -> Result<Self, String> {
        if program.is_empty() {
            return Err("command[0] must not be empty".to_owned());
        }
        if program.chars().any(char::is_control) {
            return Err("command[0] contains a control character".to_owned());
        }
        if program.starts_with('/') {
            if has_parent_component(program) {
                return Err("command[0] must not contain '..'".to_owned());
            }
            return NormalizedAbsPath::parse_absolute(program).map(RuleProgram::Absolute);
        }
        if program.contains('/') {
            return Err("command[0] must be a bare name or an absolute path".to_owned());
        }
        if program.starts_with('-') {
            return Err("command[0] must not start with '-'".to_owned());
        }
        Ok(RuleProgram::Bare(program.to_owned()))
    }

    fn bare(name: &str) -> Self {
        assert!(!name.is_empty() && !name.contains('/') && !name.starts_with('-'));
        RuleProgram::Bare(name.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            RuleProgram::Bare(name) => name.as_str(),
            RuleProgram::Absolute(path) => path.as_str(),
        }
    }
}

impl fmt::Display for RuleProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagAction {
    Keep,
    Drop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagValues {
    None,
    One,
    Optional,
    Many,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagRule {
    pub action: FlagAction,
    pub values: FlagValues,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandElement {
    Literal(String),
    OneOf(NonEmptyVec<String>),
    Uuid,
    Id,
    Under(NormalizedAbsPath),
}

impl fmt::Display for CommandElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandElement::Literal(word) => f.write_str(word),
            CommandElement::OneOf(options) => write!(f, "({})", options.as_slice().join("|")),
            CommandElement::Uuid => f.write_str("<uuid>"),
            CommandElement::Id => f.write_str("<id>"),
            CommandElement::Under(dir) => write!(f, "<path under {dir}>"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleOrigin {
    Builtin { overrides: Vec<String> },
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustRule {
    pub name: String,
    pub origin: RuleOrigin,
    pub program: RuleProgram,
    pub args: Vec<CommandElement>,
    pub dirs: Option<NonEmptyVec<NormalizedAbsPath>>,
    pub flags: Option<BTreeMap<String, FlagRule>>,
}

impl TrustRule {
    #[must_use]
    pub fn program(&self) -> &RuleProgram {
        &self.program
    }

    #[must_use]
    pub fn command_len(&self) -> usize {
        1 + self.args.len()
    }

    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self.origin, RuleOrigin::Builtin { .. })
    }

    #[must_use]
    pub fn overrides(&self) -> &[String] {
        match &self.origin {
            RuleOrigin::Builtin { overrides } => overrides.as_slice(),
            RuleOrigin::Custom => &[],
        }
    }

    #[must_use]
    pub fn command_summary(&self) -> String {
        let mut parts = Vec::with_capacity(self.command_len());
        parts.push(self.program.as_str().to_owned());
        parts.extend(self.args.iter().map(ToString::to_string));
        parts.join(" ")
    }

    #[must_use]
    pub fn flag_counts(&self) -> Option<(u32, u32)> {
        let flags = self.flags.as_ref()?;
        let mut kept: u32 = 0;
        let mut dropped: u32 = 0;
        for rule in flags.values() {
            match rule.action {
                FlagAction::Keep => kept += 1,
                FlagAction::Drop => dropped += 1,
            }
        }
        Some((kept, dropped))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeConfig {
    pub path: Option<PathBuf>,
    pub defaults: bool,
    pub allow_root: bool,
    pub rules: BTreeMap<String, TrustRule>,
    pub disabled: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigState {
    Loaded(ResumeConfig),
    Incomplete { path: Option<PathBuf>, error: String, warnings: Vec<String> },
}

impl ConfigState {
    #[must_use]
    pub fn loaded(&self) -> Option<&ResumeConfig> {
        match self {
            ConfigState::Loaded(config) => Some(config),
            ConfigState::Incomplete { .. } => None,
        }
    }

    #[must_use]
    pub fn is_incomplete(&self) -> bool {
        matches!(self, ConfigState::Incomplete { .. })
    }
}

#[must_use]
pub fn config_path() -> Option<PathBuf> {
    let abs = |name: &str| {
        env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
    };
    let dir = abs("XDG_CONFIG_HOME")
        .or_else(|| abs("HOME").map(|home| home.join(".config")))?;
    Some(dir.join("lune").join("config.toml"))
}

#[must_use]
pub fn load() -> ConfigState {
    let path = config_path();
    let home = env::var_os("HOME").map(PathBuf::from).filter(|path| path.is_absolute());
    load_from(path, home.as_deref())
}

#[must_use]
pub fn load_from(path: Option<PathBuf>, home: Option<&Path>) -> ConfigState {
    let Some(path) = path else {
        return ConfigState::Loaded(default_config(None));
    };
    let text = match read_config_file(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return ConfigState::Loaded(default_config(Some(path))),
        Err(error) => {
            return ConfigState::Incomplete { path: Some(path), error, warnings: Vec::new() };
        },
    };
    parse_config_toml(&text, Some(path), home)
}

#[must_use]
pub fn parse_config_toml(text: &str, path: Option<PathBuf>, home: Option<&Path>) -> ConfigState {
    let root = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => {
            return ConfigState::Incomplete {
                path,
                error: format!("invalid TOML: {error}"),
                warnings: Vec::new(),
            };
        },
    };
    let Some(resume_val) = root.get("resume") else {
        return ConfigState::Loaded(default_config(path));
    };
    let Some(resume) = resume_val.as_table() else {
        return ConfigState::Incomplete {
            path,
            error: "[resume] must be a table".to_owned(),
            warnings: Vec::new(),
        };
    };

    let mut defaults = true;
    let mut allow_root = false;
    let mut trust_table: Option<&toml::Table> = None;

    for (key, value) in resume {
        match key.as_str() {
            "defaults" => {
                let Some(flag) = value.as_bool() else {
                    return ConfigState::Incomplete {
                        path,
                        error: "resume.defaults must be a boolean".to_owned(),
                        warnings: Vec::new(),
                    };
                };
                defaults = flag;
            },
            "allow_root" => {
                let Some(flag) = value.as_bool() else {
                    return ConfigState::Incomplete {
                        path,
                        error: "resume.allow_root must be a boolean".to_owned(),
                        warnings: Vec::new(),
                    };
                };
                allow_root = flag;
            },
            "trust" => {
                let Some(table) = value.as_table() else {
                    return ConfigState::Incomplete {
                        path,
                        error: "resume.trust must be a table".to_owned(),
                        warnings: Vec::new(),
                    };
                };
                trust_table = Some(table);
            },
            other => {
                return ConfigState::Incomplete {
                    path,
                    error: format!("unknown setting resume.{other}"),
                    warnings: Vec::new(),
                };
            },
        }
    }

    let mut rules = BTreeMap::new();
    let mut disabled = BTreeMap::new();
    let mut warnings = Vec::new();

    if defaults {
        for builtin in BuiltinRule::ALL {
            rules.insert(builtin.name().to_owned(), builtin.rule());
        }
    } else {
        for builtin in BuiltinRule::ALL {
            disabled.insert(
                builtin.name().to_owned(),
                "off because resume.defaults is false".to_owned(),
            );
        }
    }

    if let Some(trust) = trust_table {
        if trust.len() > RULES_MAX {
            return ConfigState::Incomplete {
                path,
                error: format!("resume.trust defines {} rules, over {RULES_MAX}", trust.len()),
                warnings,
            };
        }
        for (name, spec) in trust {
            match evaluate_rule_entry(name, spec, defaults, home) {
                RuleOutcome::KeepExisting => {},
                RuleOutcome::WarnOnly { warning } => warnings.push(warning),
                RuleOutcome::Disable { reason } => {
                    rules.remove(name);
                    disabled.insert(name.clone(), reason);
                },
                RuleOutcome::Invalid { warning, reason } => {
                    rules.remove(name);
                    warnings.push(warning);
                    disabled.insert(name.clone(), reason);
                },
                RuleOutcome::Enable(rule) => {
                    if !rules.contains_key(name) && rules.len() >= RULES_MAX {
                        return ConfigState::Incomplete {
                            path,
                            error: format!(
                                "resume.trust results in more than {RULES_MAX} active rules"
                            ),
                            warnings,
                        };
                    }
                    disabled.remove(name);
                    rules.insert(name.clone(), rule);
                },
            }
        }
    }

    assert!(rules.len() <= RULES_MAX);
    ConfigState::Loaded(ResumeConfig { path, defaults, allow_root, rules, disabled, warnings })
}

fn default_config(path: Option<PathBuf>) -> ResumeConfig {
    let mut rules = BTreeMap::new();
    for builtin in BuiltinRule::ALL {
        rules.insert(builtin.name().to_owned(), builtin.rule());
    }
    assert_eq!(rules.len(), BuiltinRule::ALL.len());
    ResumeConfig {
        path,
        defaults: true,
        allow_root: false,
        rules,
        disabled: BTreeMap::new(),
        warnings: Vec::new(),
    }
}

fn read_config_file(path: &Path) -> Result<Option<String>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let meta = file.metadata().map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let uid = rustix::process::getuid().as_raw();
    let owner = meta.uid();
    if owner != uid && owner != 0 {
        return Err(format!("{} is owned by another user (uid {owner})", path.display()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!("{} is writable by group or others", path.display()));
    }
    let limit = u64::try_from(CONFIG_BYTES_MAX).expect("config byte limit fits u64");
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > CONFIG_BYTES_MAX {
        return Err(format!("{} exceeds {CONFIG_BYTES_MAX} bytes", path.display()));
    }
    assert!(bytes.len() <= CONFIG_BYTES_MAX);
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{} is not valid UTF-8", path.display()))
}

enum RuleOutcome {
    KeepExisting,
    Enable(TrustRule),
    Disable { reason: String },
    Invalid { warning: String, reason: String },
    WarnOnly { warning: String },
}

fn evaluate_rule_entry(
    name: &str,
    spec: &Value,
    defaults: bool,
    home: Option<&Path>,
) -> RuleOutcome {
    if name.is_empty()
        || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return RuleOutcome::WarnOnly {
            warning: format!("resume.trust.{name}: invalid rule name, ignored"),
        };
    }
    let builtin = BuiltinRule::parse(name);

    if let Some(flag) = spec.as_bool() {
        if flag {
            if builtin.is_some() && !defaults {
                return RuleOutcome::WarnOnly {
                    warning: format!(
                        "resume.trust.{name}: cannot enable built-in rule with boolean when resume.defaults is false"
                    ),
                };
            }
            if builtin.is_none() {
                let warning = format!(
                    "resume.trust.{name}: custom rule requires command table, not true, so this rule is ignored"
                );
                let reason = format!("left out for a mistake: {warning}");
                return RuleOutcome::Invalid { warning, reason };
            }
            return RuleOutcome::KeepExisting;
        }
        return RuleOutcome::Disable { reason: "disabled in config".to_owned() };
    }

    let Some(table) = spec.as_table() else {
        let warning =
            format!("resume.trust.{name}: expected a table or false, so this rule is ignored");
        let reason = format!("left out for a mistake: {warning}");
        return RuleOutcome::Invalid { warning, reason };
    };

    for key in table.keys() {
        if !matches!(key.as_str(), "enabled" | "command" | "dir" | "flags") {
            let warning =
                format!("resume.trust.{name}: unknown key {key:?}, so this rule is ignored");
            let reason = format!("left out for a mistake: {warning}");
            return RuleOutcome::Invalid { warning, reason };
        }
    }

    if let Some(enabled_val) = table.get("enabled") {
        let Some(enabled) = enabled_val.as_bool() else {
            let warning = format!(
                "resume.trust.{name}: enabled must be a boolean, so this rule is ignored"
            );
            let reason = format!("left out for a mistake: {warning}");
            return RuleOutcome::Invalid { warning, reason };
        };
        if !enabled {
            return RuleOutcome::Disable { reason: "disabled in config".to_owned() };
        }
    }

    let parsed = if table.contains_key("command") {
        parse_full_rule(name, table, home)
    } else if let Some(builtin) = builtin {
        if !defaults {
            return RuleOutcome::WarnOnly {
                warning: format!(
                    "resume.trust.{name}: modifies built-in rule while resume.defaults is false, so it does not apply"
                ),
            };
        }
        patch_builtin_rule(builtin, table, home)
    } else {
        let warning = format!(
            "resume.trust.{name}: custom rule requires command, so this rule is ignored"
        );
        let reason = format!("left out for a mistake: {warning}");
        return RuleOutcome::Invalid { warning, reason };
    };

    match parsed {
        Ok(rule) => RuleOutcome::Enable(rule),
        Err(why) => RuleOutcome::Invalid {
            warning: format!("resume.trust.{name}: {why}, so this rule is ignored"),
            reason: format!("left out for a mistake: {why}"),
        },
    }
}

fn parse_full_rule(
    name: &str,
    table: &toml::Table,
    home: Option<&Path>,
) -> Result<TrustRule, String> {
    let command_val = table.get("command").ok_or_else(|| "missing command".to_owned())?;
    let (program, args) = parse_command(command_val, home)?;
    let dirs = match table.get("dir") {
        Some(dir_val) => Some(parse_dirs(dir_val, home)?),
        None => None,
    };
    let flags = match table.get("flags") {
        Some(flags_val) => Some(parse_new_flags(flags_val)?),
        None => None,
    };
    Ok(TrustRule {
        name: name.to_owned(),
        origin: RuleOrigin::Custom,
        program,
        args,
        dirs,
        flags,
    })
}

fn patch_builtin_rule(
    builtin: BuiltinRule,
    table: &toml::Table,
    home: Option<&Path>,
) -> Result<TrustRule, String> {
    let mut rule = builtin.rule();
    let mut overrides = Vec::new();
    if let Some(dir_val) = table.get("dir") {
        let dirs = parse_dirs(dir_val, home)?;
        let summary: Vec<&str> = dirs.iter().map(NormalizedAbsPath::as_str).collect();
        overrides.push(format!("dir: {}", summary.join(", ")));
        rule.dirs = Some(dirs);
    }
    if let Some(flags_val) = table.get("flags") {
        let Some(flags_table) = flags_val.as_table() else {
            return Err("flags must be a table".to_owned());
        };
        let target = rule.flags.get_or_insert_with(BTreeMap::new);
        for (flag_name, spec) in flags_table {
            validate_flag_name(flag_name)?;
            if spec.as_bool() == Some(false) {
                target.remove(flag_name);
                overrides.push(format!("{flag_name} removed"));
                continue;
            }
            let parsed = parse_flag_spec(flag_name, spec)?;
            if !target.contains_key(flag_name) && target.len() >= FLAGS_PER_RULE_MAX {
                return Err(format!("too many flags (over {FLAGS_PER_RULE_MAX})"));
            }
            target.insert(flag_name.clone(), parsed);
            overrides.push(format!("{flag_name} updated"));
        }
        assert!(target.len() <= FLAGS_PER_RULE_MAX);
    }
    rule.origin = RuleOrigin::Builtin { overrides };
    Ok(rule)
}

fn parse_command(
    value: &Value,
    home: Option<&Path>,
) -> Result<(RuleProgram, Vec<CommandElement>), String> {
    let Some(items) = value.as_array() else {
        return Err("command must be a non-empty array".to_owned());
    };
    if items.is_empty() {
        return Err("command must not be empty".to_owned());
    }
    if items.len() > COMMAND_ELEMENTS_MAX {
        return Err(format!("command has {} elements, over {COMMAND_ELEMENTS_MAX}", items.len()));
    }
    let Some(raw_program) = items[0].as_str() else {
        return Err("command[0] must be a string program name or absolute path".to_owned());
    };
    let program = RuleProgram::parse(raw_program)?;
    let mut args = Vec::with_capacity(items.len() - 1);
    for (index, item) in items.iter().enumerate().skip(1) {
        args.push(parse_command_element(index, item, home)?);
    }
    assert!(args.len() < COMMAND_ELEMENTS_MAX);
    Ok((program, args))
}

fn parse_command_element(
    index: usize,
    value: &Value,
    home: Option<&Path>,
) -> Result<CommandElement, String> {
    if let Some(literal) = value.as_str() {
        if literal.chars().any(char::is_control) {
            return Err(format!("command[{index}] contains a control character"));
        }
        return Ok(CommandElement::Literal(literal.to_owned()));
    }
    if let Some(list) = value.as_array() {
        if list.len() > CHOICES_PER_ELEMENT_MAX {
            return Err(format!(
                "command[{index}] choice list has {} entries, over {CHOICES_PER_ELEMENT_MAX}",
                list.len()
            ));
        }
        let mut options = Vec::with_capacity(list.len());
        for entry in list {
            let Some(text) = entry.as_str() else {
                return Err(format!("command[{index}] choices must be strings"));
            };
            if text.chars().any(char::is_control) {
                return Err(format!("command[{index}] choice contains a control character"));
            }
            options.push(text.to_owned());
        }
        let options = NonEmptyVec::new(options)
            .ok_or_else(|| format!("command[{index}] choice list must not be empty"))?;
        assert!(options.as_slice().len() <= CHOICES_PER_ELEMENT_MAX);
        return Ok(CommandElement::OneOf(options));
    }
    if let Some(table) = value.as_table() {
        if table.len() != 1 {
            return Err(format!("command[{index}] matcher table must have exactly one key"));
        }
        if let Some(kind) = table.get("match") {
            let Some(kind) = kind.as_str() else {
                return Err(format!("command[{index}].match must be a string"));
            };
            return match kind {
                "uuid" => Ok(CommandElement::Uuid),
                "id" => Ok(CommandElement::Id),
                other => Err(format!("command[{index}].match unknown matcher {other:?}")),
            };
        }
        if let Some(under) = table.get("under") {
            let Some(raw) = under.as_str() else {
                return Err(format!("command[{index}].under must be a path string"));
            };
            let expanded = NormalizedAbsPath::parse_expanded(raw, home)?;
            return Ok(CommandElement::Under(expanded));
        }
        return Err(format!("command[{index}] matcher must use 'match' or 'under'"));
    }
    Err(format!("command[{index}] must be a string, string array, or matcher table"))
}

fn parse_dirs(
    value: &Value,
    home: Option<&Path>,
) -> Result<NonEmptyVec<NormalizedAbsPath>, String> {
    if let Some(single) = value.as_str() {
        return Ok(NonEmptyVec::single(NormalizedAbsPath::parse_expanded(single, home)?));
    }
    let Some(list) = value.as_array() else {
        return Err("dir must be a string or array of strings".to_owned());
    };
    if list.len() > DIRS_PER_RULE_MAX {
        return Err(format!("dir has {} entries, over {DIRS_PER_RULE_MAX}", list.len()));
    }
    let mut dirs = Vec::with_capacity(list.len());
    for entry in list {
        let Some(raw) = entry.as_str() else {
            return Err("dir entries must be strings".to_owned());
        };
        dirs.push(NormalizedAbsPath::parse_expanded(raw, home)?);
    }
    let dirs = NonEmptyVec::new(dirs).ok_or_else(|| "dir array must not be empty".to_owned())?;
    assert!(dirs.as_slice().len() <= DIRS_PER_RULE_MAX);
    Ok(dirs)
}

#[must_use]
pub(crate) fn has_parent_component(path: &str) -> bool {
    Path::new(path).components().any(|component| matches!(component, Component::ParentDir))
}

#[must_use]
pub(crate) fn normalize_lexical(path: &str) -> String {
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => {
                if let Some(text) = segment.to_str() {
                    parts.push(text);
                }
            },
            Component::RootDir | Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
            },
        }
    }
    if parts.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn parse_new_flags(value: &Value) -> Result<BTreeMap<String, FlagRule>, String> {
    let Some(table) = value.as_table() else {
        return Err("flags must be a table".to_owned());
    };
    if table.len() > FLAGS_PER_RULE_MAX {
        return Err(format!("flags has {} entries, over {FLAGS_PER_RULE_MAX}", table.len()));
    }
    let mut flags = BTreeMap::new();
    for (flag_name, spec) in table {
        validate_flag_name(flag_name)?;
        if spec.as_bool() == Some(false) {
            continue;
        }
        flags.insert(flag_name.clone(), parse_flag_spec(flag_name, spec)?);
    }
    assert!(flags.len() <= FLAGS_PER_RULE_MAX);
    Ok(flags)
}

fn validate_flag_name(flag_name: &str) -> Result<(), String> {
    if !flag_name.starts_with('-') || flag_name == "-" || flag_name == "--" {
        return Err(format!("flag name {flag_name:?} must start with '-'"));
    }
    if flag_name.contains('=') || flag_name.chars().any(char::is_control) {
        return Err(format!("flag name {flag_name:?} is invalid"));
    }
    Ok(())
}

fn parse_flag_spec(flag_name: &str, spec: &Value) -> Result<FlagRule, String> {
    if let Some(action_str) = spec.as_str() {
        let action = parse_flag_action(flag_name, action_str)?;
        return Ok(FlagRule { action, values: FlagValues::None });
    }
    let Some(table) = spec.as_table() else {
        return Err(format!("flags.{flag_name}: expected \"keep\", \"drop\", false, or a table"));
    };
    for key in table.keys() {
        if !matches!(key.as_str(), "action" | "values") {
            return Err(format!("flags.{flag_name}: unknown key {key:?}"));
        }
    }
    let action_str = table
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("flags.{flag_name}: missing string 'action'"))?;
    let action = parse_flag_action(flag_name, action_str)?;
    let values = match table.get("values") {
        None => FlagValues::None,
        Some(val) => match val.as_integer() {
            Some(0) => FlagValues::None,
            Some(1) => FlagValues::One,
            Some(other) => {
                return Err(format!(
                    "flags.{flag_name}: unsupported values count {other}, want 0, 1, \"optional\", or \"many\""
                ));
            },
            None => match val.as_str() {
                Some("optional") => FlagValues::Optional,
                Some("many") => FlagValues::Many,
                Some(other) => {
                    return Err(format!(
                        "flags.{flag_name}: unknown values {other:?}, want 0, 1, \"optional\", or \"many\""
                    ));
                },
                None => {
                    return Err(format!(
                        "flags.{flag_name}: values must be 0, 1, \"optional\", or \"many\""
                    ));
                },
            },
        },
    };
    Ok(FlagRule { action, values })
}

fn parse_flag_action(flag_name: &str, raw: &str) -> Result<FlagAction, String> {
    match raw {
        "keep" => Ok(FlagAction::Keep),
        "drop" => Ok(FlagAction::Drop),
        other => Err(format!(
            "flags.{flag_name}: unknown action {other:?}, want \"keep\" or \"drop\""
        )),
    }
}

fn insert_flags(
    flags: &mut BTreeMap<String, FlagRule>,
    names: &[&str],
    action: FlagAction,
    values: FlagValues,
) {
    for &name in names {
        let prev = flags.insert(name.to_owned(), FlagRule { action, values });
        assert!(prev.is_none(), "duplicate built-in flag {name}");
    }
}

fn claude_builtin_rule() -> TrustRule {
    let mut flags = BTreeMap::new();
    insert_flags(
        &mut flags,
        &[
            "--allow-dangerously-skip-permissions",
            "--bare",
            "--brief",
            "--chrome",
            "--dangerously-skip-permissions",
            "--disable-slash-commands",
            "--ide",
            "--no-chrome",
            "--replay-user-messages",
            "--strict-mcp-config",
            "--verbose",
        ],
        FlagAction::Keep,
        FlagValues::None,
    );
    insert_flags(
        &mut flags,
        &[
            "--agent",
            "--agents",
            "--append-system-prompt",
            "--append-system-prompt-file",
            "--channels",
            "--dangerously-load-development-channels",
            "--debug-file",
            "--effort",
            "--fallback-model",
            "--input-format",
            "--json-schema",
            "--max-budget-usd",
            "--max-turns",
            "--model",
            "--output-format",
            "--permission-mode",
            "--permission-prompt-tool",
            "--plugin-dir",
            "--remote-control-session-name-prefix",
            "--setting-sources",
            "--settings",
            "--system-prompt",
            "--system-prompt-file",
        ],
        FlagAction::Keep,
        FlagValues::One,
    );
    insert_flags(
        &mut flags,
        &["-d", "--debug", "--tmux", "-w", "--worktree"],
        FlagAction::Keep,
        FlagValues::Optional,
    );
    insert_flags(
        &mut flags,
        &[
            "--add-dir",
            "--allowed-tools",
            "--allowedTools",
            "--betas",
            "--disallowed-tools",
            "--disallowedTools",
            "--file",
            "--mcp-config",
            "--tools",
        ],
        FlagAction::Keep,
        FlagValues::Many,
    );
    insert_flags(
        &mut flags,
        &["-c", "--continue", "--fork-session", "--init", "--init-only", "--maintenance"],
        FlagAction::Drop,
        FlagValues::None,
    );
    insert_flags(
        &mut flags,
        &["--session-id", "-n", "--name", "--remote"],
        FlagAction::Drop,
        FlagValues::One,
    );
    insert_flags(
        &mut flags,
        &["--from-pr", "-r", "--resume", "--teleport"],
        FlagAction::Drop,
        FlagValues::Optional,
    );
    assert!(flags.len() <= FLAGS_PER_RULE_MAX);
    TrustRule {
        name: "claude".to_owned(),
        origin: RuleOrigin::Builtin { overrides: Vec::new() },
        program: RuleProgram::bare("claude"),
        args: vec![CommandElement::Literal("--resume".to_owned()), CommandElement::Uuid],
        dirs: None,
        flags: Some(flags),
    }
}

fn pi_builtin_rule() -> TrustRule {
    let mut flags = BTreeMap::new();
    insert_flags(
        &mut flags,
        &[
            "--no-extensions",
            "--no-prompt-templates",
            "--no-skills",
            "--no-themes",
            "--no-tools",
            "--offline",
            "--verbose",
        ],
        FlagAction::Keep,
        FlagValues::None,
    );
    insert_flags(
        &mut flags,
        &[
            "-e",
            "--api-key",
            "--append-system-prompt",
            "--extension",
            "--model",
            "--models",
            "--prompt-template",
            "--provider",
            "--session-dir",
            "--skill",
            "--system-prompt",
            "--theme",
            "--thinking",
            "--tools",
        ],
        FlagAction::Keep,
        FlagValues::One,
    );
    insert_flags(
        &mut flags,
        &["-c", "--continue", "-r", "--resume"],
        FlagAction::Drop,
        FlagValues::None,
    );
    insert_flags(
        &mut flags,
        &["--fork", "--session"],
        FlagAction::Drop,
        FlagValues::One,
    );
    assert!(flags.len() <= FLAGS_PER_RULE_MAX);
    TrustRule {
        name: "pi".to_owned(),
        origin: RuleOrigin::Builtin { overrides: Vec::new() },
        program: RuleProgram::bare("pi"),
        args: vec![CommandElement::Literal("--session".to_owned()), CommandElement::Id],
        dirs: None,
        flags: Some(flags),
    }
}

fn agy_builtin_rule() -> TrustRule {
    let mut flags = BTreeMap::new();
    insert_flags(
        &mut flags,
        &["--dangerously-skip-permissions", "--no-sandbox", "--sandbox", "--verbose", "--yolo"],
        FlagAction::Keep,
        FlagValues::None,
    );
    insert_flags(
        &mut flags,
        &[
            "-m",
            "--add-dir",
            "--append-system-prompt",
            "--effort",
            "--model",
            "--permission-mode",
            "--profile",
            "--system-prompt",
            "--workspace",
        ],
        FlagAction::Keep,
        FlagValues::One,
    );
    insert_flags(
        &mut flags,
        &["--allowed-tools", "--disallowed-tools"],
        FlagAction::Keep,
        FlagValues::Many,
    );
    insert_flags(&mut flags, &["-c", "--continue"], FlagAction::Drop, FlagValues::None);
    insert_flags(
        &mut flags,
        &["--conversation", "-i", "-r", "--prompt-interactive", "--resume", "--session-id"],
        FlagAction::Drop,
        FlagValues::One,
    );
    assert!(flags.len() <= FLAGS_PER_RULE_MAX);
    TrustRule {
        name: "agy".to_owned(),
        origin: RuleOrigin::Builtin { overrides: Vec::new() },
        program: RuleProgram::bare("agy"),
        args: vec![CommandElement::Literal("--resume".to_owned()), CommandElement::Id],
        dirs: None,
        flags: Some(flags),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn default_config_ships_all_five_builtin_rules() {
        let ConfigState::Loaded(config) = parse_config_toml("", None, Some(Path::new("/home/u")))
        else {
            panic!("empty config loads");
        };
        assert!(config.defaults);
        assert!(!config.allow_root);
        let names: Vec<&str> = config.rules.keys().map(String::as_str).collect();
        assert_eq!(names, ["agy", "claude", "codex", "opencode", "pi"]);
        assert!(config.disabled.is_empty());
        assert_eq!(config.warnings, Vec::<String>::new());
    }

    #[test]
    fn defaults_false_disables_builtins_warns_on_patches_and_keeps_full_replacements() {
        let text = r#"
[resume]
defaults = false

[resume.trust.claude]
dir = "~/code"

[resume.trust.nvim]
command = ["nvim", "-S", { under = "~/.local/state/nvim/sessions" }]
"#;
        let ConfigState::Loaded(config) =
            parse_config_toml(text, None, Some(Path::new("/home/u")))
        else {
            panic!("valid config loads");
        };
        assert!(!config.defaults);
        assert_eq!(config.rules.keys().map(String::as_str).collect::<Vec<_>>(), ["nvim"]);
        assert_eq!(config.disabled.len(), 5);
        assert_eq!(config.warnings.len(), 1);
        assert!(config.warnings[0].contains("resume.trust.claude"));
    }

    #[test]
    fn per_rule_mistake_omits_only_that_rule_while_invalid_toml_fails_closed() {
        let partial = r#"
[resume.trust.codex]
enabled = false

[resume.trust.claude.flags]
"--model" = "keeep"
"#;
        let ConfigState::Loaded(config) =
            parse_config_toml(partial, None, Some(Path::new("/home/u")))
        else {
            panic!("partial mistake still loads valid rules");
        };
        assert!(!config.rules.contains_key("claude"));
        assert!(!config.rules.contains_key("codex"));
        assert!(config.rules.contains_key("pi"));
        assert!(config.rules.contains_key("agy"));
        assert!(config.rules.contains_key("opencode"));
        assert_eq!(config.warnings.len(), 1);

        let broken = "[resume\ndefaults = true";
        assert!(parse_config_toml(broken, None, None).is_incomplete());
    }

    #[test]
    fn group_or_world_writable_config_file_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[resume]\ndefaults = true\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o664)).unwrap();
        let state = load_from(Some(file.clone()), Some(dir.path()));
        assert!(state.is_incomplete());

        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let state = load_from(Some(file), Some(dir.path()));
        assert!(!state.is_incomplete());
    }

    #[test]
    fn defaults_plus_max_custom_rules_fails_closed_without_panicking() {
        use std::fmt::Write as _;

        let mut text = String::from("[resume]\ndefaults = true\n");
        for index in 0..RULES_MAX {
            let _ = writeln!(text, "[resume.trust.custom_{index}]\ncommand = [\"tool_{index}\"]");
        }
        assert!(parse_config_toml(&text, None, Some(Path::new("/home/u"))).is_incomplete());
    }

    #[test]
    fn custom_rule_set_to_true_and_overlong_choice_list_warn_and_disable() {
        let choices = (0..=CHOICES_PER_ELEMENT_MAX)
            .map(|index| format!("\"c{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "[resume.trust]\nnvim = true\n\n[resume.trust.many]\ncommand = [\"tool\", [{choices}]]\n"
        );
        let ConfigState::Loaded(config) =
            parse_config_toml(&text, None, Some(Path::new("/home/u")))
        else {
            panic!("per-rule mistakes still load valid rules");
        };
        assert!(!config.rules.contains_key("nvim"));
        assert!(!config.rules.contains_key("many"));
        assert!(config.warnings.iter().any(|w| w.contains("resume.trust.nvim")));
        assert!(config.warnings.iter().any(|w| w.contains("resume.trust.many")));
        assert!(config.disabled.contains_key("nvim"));
        assert!(config.disabled.contains_key("many"));
    }

    #[test]
    fn pi_builtin_flags_match_typescript_extension_table() {
        let ts = include_str!("../pi/lune.ts");
        let start = ts.find("const PI_RESUME_FLAGS:").expect("PI_RESUME_FLAGS exists");
        let block = &ts[start..];
        let open = block.find("= {").expect("opening brace") + 3;
        let close = block[open..].find("};").expect("closing brace");
        let mut ts_flags = BTreeMap::new();
        for line in block[open..open + close].lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let (raw_name, rest) = trimmed.split_once(':').expect("key: spec");
            let name = raw_name.trim().trim_matches('"').to_owned();
            let action = if rest.contains("action: \"keep\"") {
                FlagAction::Keep
            } else if rest.contains("action: \"drop\"") {
                FlagAction::Drop
            } else {
                panic!("unexpected action in {trimmed}");
            };
            let values = if rest.contains("values: \"none\"") {
                FlagValues::None
            } else if rest.contains("values: \"one\"") {
                FlagValues::One
            } else if rest.contains("values: \"many\"") {
                FlagValues::Many
            } else {
                panic!("unexpected values in {trimmed}");
            };
            assert!(ts_flags.insert(name, FlagRule { action, values }).is_none());
        }
        let rust_flags = pi_builtin_rule().flags.expect("pi builtin has flags");
        assert_eq!(ts_flags, rust_flags);
    }
}
