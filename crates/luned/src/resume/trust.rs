use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lune_protocol::program_resume::{ProcessArgv, ProcessExe, ResumeCwdSource, ResumeRecord};

use super::config::{
    CommandElement, ConfigState, FlagAction, FlagRule, FlagValues, NormalizedAbsPath, ResumeConfig,
    RuleProgram, TrustRule, has_parent_component, normalize_lexical,
};

const INTERPRETERS: [&str; 10] = [
    "node", "nodejs", "bun", "deno", "python", "python3", "ruby", "perl", "tsx", "ts-node",
];

const PATH_COMPONENTS_MAX: usize = 256;

const _: () = assert!(PATH_COMPONENTS_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalUntrustedReason {
    ConfigIncomplete,
    Root,
    NoRule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleUntrustedReason {
    ProgramDiffers,
    Wrapped,
    LaunchUnreadable,
    FlagNotListed,
    FlagValue,
    FlagsDiffer,
    DirNotAllowed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustVerdict {
    Trusted {
        rule: String,
    },
    UntrustedGlobal(GlobalUntrustedReason),
    UntrustedRule {
        rule: String,
        reason: RuleUntrustedReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FlagOccurrence {
    name: String,
    values: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagError {
    NotListed,
    Value,
}

#[must_use]
pub fn evaluate(
    record: &ResumeRecord,
    config_state: &ConfigState,
    running_as_root: bool,
    fallback_cwd: Option<&str>,
    check_symlinks: bool,
) -> TrustVerdict {
    let config = match config_state {
        ConfigState::Loaded(config) => config,
        ConfigState::Incomplete { .. } => {
            return TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::ConfigIncomplete);
        }
    };
    if (running_as_root || record.uid() == 0) && !config.allow_root {
        return TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::Root);
    }
    let Some(rule) = matching_rule(record.argv().words(), config, check_symlinks) else {
        return TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule);
    };
    let rule_name = rule.name.clone();
    if !process_matches_program(
        record.process_name().as_str(),
        record.process_exe().map(ProcessExe::as_str),
        record.process_argv().map(ProcessArgv::words),
        rule.program(),
        check_symlinks,
    ) {
        return TrustVerdict::UntrustedRule {
            rule: rule_name,
            reason: RuleUntrustedReason::ProgramDiffers,
        };
    }
    if record.placement().is_wrapped() {
        return TrustVerdict::UntrustedRule {
            rule: rule_name,
            reason: RuleUntrustedReason::Wrapped,
        };
    }
    if let Some(allowed_dirs) = &rule.dirs {
        let cwd = record.cwd().map(ResumeCwdSource::as_str).or(fallback_cwd);
        let allowed = cwd.is_some_and(|dir| {
            allowed_dirs
                .iter()
                .any(|base| dir_allowed(dir, base, check_symlinks))
        });
        if !allowed {
            return TrustVerdict::UntrustedRule {
                rule: rule_name,
                reason: RuleUntrustedReason::DirNotAllowed,
            };
        }
    }
    if let Some(flags_spec) = &rule.flags {
        let Some(launch_argv) = record.process_argv().map(ProcessArgv::words) else {
            return TrustVerdict::UntrustedRule {
                rule: rule_name,
                reason: RuleUntrustedReason::LaunchUnreadable,
            };
        };
        let Some(args_start) = program_args_start(launch_argv, rule.program()) else {
            return TrustVerdict::UntrustedRule {
                rule: rule_name,
                reason: RuleUntrustedReason::ProgramDiffers,
            };
        };
        assert!(args_start <= launch_argv.len());
        let expected = match parse_launch_flags(&launch_argv[args_start..], flags_spec) {
            Ok(kept) => kept,
            Err(FlagError::NotListed) => {
                return TrustVerdict::UntrustedRule {
                    rule: rule_name,
                    reason: RuleUntrustedReason::FlagNotListed,
                };
            }
            Err(FlagError::Value) => {
                return TrustVerdict::UntrustedRule {
                    rule: rule_name,
                    reason: RuleUntrustedReason::FlagValue,
                };
            }
        };
        assert!(record.argv().words().len() >= rule.command_len());
        let trailing = &record.argv().words()[rule.command_len()..];
        let reported = match parse_reported_flags(trailing, flags_spec) {
            Ok(kept) => kept,
            Err(FlagError::Value) => {
                return TrustVerdict::UntrustedRule {
                    rule: rule_name,
                    reason: RuleUntrustedReason::FlagValue,
                };
            }
            Err(FlagError::NotListed) => {
                return TrustVerdict::UntrustedRule {
                    rule: rule_name,
                    reason: RuleUntrustedReason::FlagsDiffer,
                };
            }
        };
        if reported != expected {
            return TrustVerdict::UntrustedRule {
                rule: rule_name,
                reason: RuleUntrustedReason::FlagsDiffer,
            };
        }
    }
    assert!(!record.placement().is_wrapped());
    if running_as_root || record.uid() == 0 {
        assert!(config.allow_root);
    }
    TrustVerdict::Trusted { rule: rule_name }
}

pub fn extract_kept_args(
    launch_argv: &[String],
    program: &RuleProgram,
    flags_spec: &BTreeMap<String, FlagRule>,
) -> Result<Vec<String>, FlagError> {
    let Some(args_start) = program_args_start(launch_argv, program) else {
        return Ok(Vec::new());
    };
    assert!(args_start <= launch_argv.len());
    let kept = parse_launch_flags(&launch_argv[args_start..], flags_spec)?;
    let mut out = Vec::new();
    for occurrence in kept {
        if occurrence.name.starts_with("--")
            && occurrence.values.len() == 1
            && occurrence.values[0].starts_with('-')
        {
            out.push(format!("{}={}", occurrence.name, occurrence.values[0]));
        } else {
            out.push(occurrence.name);
            out.extend(occurrence.values);
        }
    }
    Ok(out)
}

#[must_use]
pub fn process_matches_program(
    process_name: &str,
    process_exe: Option<&str>,
    process_argv: Option<&[String]>,
    target: &RuleProgram,
    check_symlinks: bool,
) -> bool {
    let target = match target {
        RuleProgram::Absolute(path) => {
            return matches_absolute_target(process_exe, process_argv, path, check_symlinks);
        }
        RuleProgram::Bare(name) => name.as_str(),
    };
    if matches_bare_name(process_name, target) {
        return true;
    }
    if let Some(exe) = process_exe
        && matches_path_basename(exe, target)
    {
        return true;
    }
    let Some(argv) = process_argv else {
        return false;
    };
    let Some(first) = argv.first() else {
        return false;
    };
    if matches_path_basename(first, target) {
        return true;
    }
    if let Some(script_index) = interpreter_script_index(argv)
        && let Some(script) = argv.get(script_index)
        && matches_script_target(script, target)
    {
        return true;
    }
    false
}

fn matches_absolute_target(
    process_exe: Option<&str>,
    process_argv: Option<&[String]>,
    target: &NormalizedAbsPath,
    check_symlinks: bool,
) -> bool {
    let target = target.as_str();
    let target_canonical = if check_symlinks {
        fs::canonicalize(target).ok()
    } else {
        None
    };
    let path_eq = |candidate: &str| {
        if !candidate.starts_with('/') {
            return false;
        }
        if candidate == target {
            return true;
        }
        if let Some(target_path) = &target_canonical
            && let Ok(candidate_canonical) = fs::canonicalize(candidate)
        {
            return &candidate_canonical == target_path;
        }
        false
    };
    if let Some(exe) = process_exe {
        if path_eq(exe) {
            return true;
        }
        if is_interpreter_path(exe)
            && let Some(argv) = process_argv
            && let Some(script_index) = interpreter_script_index(argv)
            && let Some(script) = argv.get(script_index)
            && path_eq(script)
        {
            return true;
        }
        return false;
    }
    if let Some(argv) = process_argv {
        if let Some(first) = argv.first()
            && path_eq(first)
        {
            return true;
        }
        if let Some(script_index) = interpreter_script_index(argv)
            && let Some(script) = argv.get(script_index)
            && path_eq(script)
        {
            return true;
        }
    }
    false
}

fn is_interpreter_name(name: &str) -> bool {
    let unwrapped = name
        .strip_prefix('.')
        .and_then(|s| s.strip_suffix("-wrapped"))
        .unwrap_or(name);
    INTERPRETERS.contains(&unwrapped)
        || unwrapped.starts_with("python3.")
        || unwrapped.starts_with("node-")
}

fn is_interpreter_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_interpreter_name)
}

fn matches_path_basename(path: &str, target: &str) -> bool {
    let Some(base) = Path::new(path).file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    matches_bare_name(base, target)
}

fn matches_bare_name(candidate: &str, target: &str) -> bool {
    if candidate == target {
        return true;
    }
    if let Some(inner) = candidate
        .strip_prefix('.')
        .and_then(|s| s.strip_suffix("-wrapped"))
        && inner == target
    {
        return true;
    }
    false
}

fn matches_script_target(script: &str, target: &str) -> bool {
    if matches_path_basename(script, target) {
        return true;
    }
    if let Some(stem) = Path::new(script).file_stem().and_then(|n| n.to_str())
        && matches_bare_name(stem, target)
    {
        return true;
    }
    false
}

fn program_args_start(launch_argv: &[String], target: &RuleProgram) -> Option<usize> {
    let first = launch_argv.first()?;
    match target {
        RuleProgram::Absolute(_) => {
            if let Some(script_index) = interpreter_script_index(launch_argv) {
                return Some(script_index + 1);
            }
            Some(1)
        }
        RuleProgram::Bare(name) => {
            if matches_path_basename(first, name) {
                return Some(1);
            }
            if let Some(script_index) = interpreter_script_index(launch_argv)
                && launch_argv
                    .get(script_index)
                    .is_some_and(|script| matches_script_target(script, name))
            {
                return Some(script_index + 1);
            }
            Some(1)
        }
    }
}

fn interpreter_script_index(argv: &[String]) -> Option<usize> {
    let mut index = 0;
    let first_base = Path::new(argv.first()?).file_name()?.to_str()?;
    if first_base == "env" {
        index = 1;
        while let Some(word) = argv.get(index) {
            if word == "--" {
                index += 1;
                break;
            }
            if word.starts_with('-') || word.contains('=') {
                index += 1;
                continue;
            }
            break;
        }
    }
    let interpreter = Path::new(argv.get(index)?).file_name()?.to_str()?;
    if !is_interpreter_name(interpreter) {
        return None;
    }
    let mut scan = index + 1;
    while let Some(word) = argv.get(scan) {
        if word == "--" {
            return (scan + 1 < argv.len()).then_some(scan + 1);
        }
        if matches!(word.as_str(), "-e" | "--eval" | "-p" | "--print" | "-c") {
            return None;
        }
        if matches!(
            word.as_str(),
            "-r" | "--require"
                | "--import"
                | "--loader"
                | "--conditions"
                | "-C"
                | "--max-old-space-size"
                | "-m"
        ) {
            scan += 2;
            continue;
        }
        if word.starts_with('-') {
            scan += 1;
            continue;
        }
        return Some(scan);
    }
    None
}

fn matching_rule<'a>(
    words: &[String],
    config: &'a ResumeConfig,
    check_symlinks: bool,
) -> Option<&'a TrustRule> {
    config
        .rules
        .values()
        .find(|rule| rule_matches_command(words, rule, check_symlinks))
}

pub(crate) fn rule_matches_command(
    words: &[String],
    rule: &TrustRule,
    check_symlinks: bool,
) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    if first != rule.program.as_str() {
        return false;
    }
    if rule.flags.is_none() {
        if rest.len() != rule.args.len() {
            return false;
        }
    } else if rest.len() < rule.args.len() {
        return false;
    }
    for (word, pattern) in rest.iter().zip(&rule.args) {
        if !element_matches(word, pattern, check_symlinks) {
            return false;
        }
    }
    true
}

fn element_matches(word: &str, pattern: &CommandElement, check_symlinks: bool) -> bool {
    match pattern {
        CommandElement::Literal(expected) => word == expected,
        CommandElement::OneOf(options) => options.iter().any(|opt| opt == word),
        CommandElement::Uuid => is_uuid(word),
        CommandElement::Id => is_safe_id(word),
        CommandElement::Under(base) => is_strictly_under(word, base, check_symlinks),
    }
}

#[must_use]
pub fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, &byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[must_use]
pub fn is_safe_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn is_strictly_under(candidate: &str, base: &NormalizedAbsPath, check_symlinks: bool) -> bool {
    if !candidate.starts_with('/') || has_parent_component(candidate) {
        return false;
    }
    let normalized_candidate = normalize_lexical(candidate);
    let normalized_base = base.as_str();
    if !path_strictly_inside(&normalized_candidate, normalized_base) {
        return false;
    }
    if !check_symlinks {
        return true;
    }
    let Ok(canonical_base) = fs::canonicalize(normalized_base) else {
        return false;
    };
    let Ok(canonical_candidate) = canonicalize_existing_or_parent(&normalized_candidate) else {
        return false;
    };
    canonical_candidate != canonical_base && canonical_candidate.starts_with(&canonical_base)
}

pub(crate) fn dir_allowed(candidate: &str, base: &NormalizedAbsPath, check_symlinks: bool) -> bool {
    if !candidate.starts_with('/') || has_parent_component(candidate) {
        return false;
    }
    let normalized_candidate = normalize_lexical(candidate);
    let normalized_base = base.as_str();
    if normalized_candidate != normalized_base
        && !path_strictly_inside(&normalized_candidate, normalized_base)
    {
        return false;
    }
    if !check_symlinks {
        return true;
    }
    let Ok(canonical_base) = fs::canonicalize(normalized_base) else {
        return false;
    };
    let Ok(canonical_candidate) = fs::canonicalize(&normalized_candidate) else {
        return false;
    };
    canonical_candidate == canonical_base || canonical_candidate.starts_with(&canonical_base)
}

fn path_strictly_inside(candidate: &str, base: &str) -> bool {
    assert!(candidate.starts_with('/'));
    assert!(base.starts_with('/'));
    if candidate == base {
        return false;
    }
    if base == "/" {
        return candidate.starts_with('/') && candidate.len() > 1;
    }
    candidate
        .strip_prefix(base)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

fn canonicalize_existing_or_parent(path: &str) -> Result<std::path::PathBuf, std::io::Error> {
    assert!(path.starts_with('/'));
    assert!(!has_parent_component(path));
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    let mut tail = Vec::new();
    let mut cursor = Path::new(path);
    for _ in 0..PATH_COMPONENTS_MAX {
        match fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut full = fs::canonicalize(cursor)?;
                for part in tail.into_iter().rev() {
                    full.push(part);
                }
                return Ok(full);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(error);
                };
                tail.push(name.to_os_string());
                let Some(parent) = cursor.parent() else {
                    return Err(error);
                };
                cursor = parent;
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "path exceeds component depth limit",
    ))
}

fn parse_launch_flags(
    args: &[String],
    flags_spec: &BTreeMap<String, FlagRule>,
) -> Result<Vec<FlagOccurrence>, FlagError> {
    let mut kept = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            index += 1;
            continue;
        }
        let (flag_name, inline_value) = split_long_inline(arg);
        let Some(&rule) = flags_spec.get(flag_name) else {
            return Err(FlagError::NotListed);
        };
        let (values, next_index) = consume_flag_values(args, index, inline_value, rule.values)?;
        assert!(next_index > index);
        if rule.action == FlagAction::Keep {
            kept.push(FlagOccurrence {
                name: flag_name.to_owned(),
                values,
            });
        }
        index = next_index;
    }
    Ok(kept)
}

fn parse_reported_flags(
    args: &[String],
    flags_spec: &BTreeMap<String, FlagRule>,
) -> Result<Vec<FlagOccurrence>, FlagError> {
    let mut kept = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--" || !arg.starts_with('-') || arg == "-" {
            return Err(FlagError::NotListed);
        }
        let (flag_name, inline_value) = split_long_inline(arg);
        let Some(&rule) = flags_spec.get(flag_name) else {
            return Err(FlagError::NotListed);
        };
        if rule.action != FlagAction::Keep {
            return Err(FlagError::NotListed);
        }
        let (values, next_index) = consume_flag_values(args, index, inline_value, rule.values)?;
        assert!(next_index > index);
        kept.push(FlagOccurrence {
            name: flag_name.to_owned(),
            values,
        });
        index = next_index;
    }
    Ok(kept)
}

fn split_long_inline(arg: &str) -> (&str, Option<&str>) {
    if arg.starts_with("--")
        && let Some((flag, value)) = arg.split_once('=')
    {
        return (flag, Some(value));
    }
    (arg, None)
}

fn consume_flag_values(
    args: &[String],
    index: usize,
    inline_value: Option<&str>,
    spec: FlagValues,
) -> Result<(Vec<String>, usize), FlagError> {
    assert!(index < args.len());
    let (values, next_index) = match (spec, inline_value) {
        (FlagValues::None, Some(_)) => return Err(FlagError::Value),
        (FlagValues::None, None) => (Vec::new(), index + 1),
        (FlagValues::One | FlagValues::Optional | FlagValues::Many, Some(value)) => {
            (vec![value.to_owned()], index + 1)
        }
        (FlagValues::One, None) => {
            let Some(next) = args.get(index + 1) else {
                return Err(FlagError::Value);
            };
            (vec![next.clone()], index + 2)
        }
        (FlagValues::Optional, None) => {
            if let Some(next) = args.get(index + 1)
                && !next.starts_with('-')
            {
                (vec![next.clone()], index + 2)
            } else {
                (Vec::new(), index + 1)
            }
        }
        (FlagValues::Many, None) => {
            let mut values = Vec::new();
            let mut scan = index + 1;
            while let Some(next) = args.get(scan) {
                if next.starts_with('-') {
                    break;
                }
                values.push(next.clone());
                scan += 1;
            }
            if values.is_empty() {
                return Err(FlagError::Value);
            }
            (values, scan)
        }
    };
    assert!(next_index > index);
    assert!(next_index <= args.len());
    Ok((values, next_index))
}

#[cfg(test)]
mod tests {
    use lune_protocol::program_resume::{
        ProcStart, ProcessName, ProcessPid, ProcessPlacement, ReportedAt, ResumeArgv, ResumeCwd,
        ResumeCwdSource,
    };

    use super::*;
    use crate::resume::config::parse_config_toml;

    const UUID: &str = "015d3962-6f9e-45c3-92bc-c7cef5068215";

    fn sample_record_with_placement(
        process_name: &str,
        process_exe: Option<&str>,
        process_argv: Option<&[&str]>,
        reported_argv: &[&str],
        cwd: Option<&str>,
        placement: ProcessPlacement,
    ) -> ResumeRecord {
        ResumeRecord::new(
            ProcessPid::new(42).unwrap(),
            ProcStart::parse("100".to_owned()).unwrap(),
            1000,
            ProcessName::from_os(process_name),
            process_exe.map(|exe| ProcessExe::parse(exe.to_owned()).unwrap()),
            process_argv.map(|a| ProcessArgv::bounded(a.iter().map(|s| (*s).to_owned()).collect())),
            placement,
            ResumeArgv::parse(reported_argv.iter().map(|s| (*s).to_owned()).collect()).unwrap(),
            cwd.map(|d| ResumeCwdSource::Explicit(ResumeCwd::parse(d).unwrap())),
            ReportedAt::from_parts(0, 0).unwrap(),
        )
    }

    fn sample_record(
        process_name: &str,
        process_exe: Option<&str>,
        process_argv: Option<&[&str]>,
        reported_argv: &[&str],
        cwd: Option<&str>,
    ) -> ResumeRecord {
        sample_record_with_placement(
            process_name,
            process_exe,
            process_argv,
            reported_argv,
            cwd,
            ProcessPlacement::ShellHosted,
        )
    }

    #[test]
    fn evaluates_claude_with_kept_dropped_flags_versioned_binary_and_node_wrapper() {
        let state = parse_config_toml("", None, Some(Path::new("/home/u")));
        let native = sample_record(
            "2.1.332",
            Some("/home/u/.local/share/claude/versions/2.1.332"),
            Some(&[
                "claude",
                "--continue",
                "--model=opus",
                "--add-dir",
                "/a",
                "/b",
                "--",
                "hi",
            ]),
            &[
                "claude",
                "--resume",
                UUID,
                "--model",
                "opus",
                "--add-dir",
                "/a",
                "/b",
            ],
            Some("/home/u/code/app"),
        );
        assert_eq!(
            evaluate(&native, &state, false, None, false),
            TrustVerdict::Trusted {
                rule: "claude".to_owned()
            }
        );

        let via_node = sample_record(
            "node",
            Some("/opt/homebrew/bin/node"),
            Some(&["node", "/opt/homebrew/bin/claude", "--model", "sonnet"]),
            &["claude", "--resume", UUID, "--model", "sonnet"],
            Some("/home/u/code/app"),
        );
        assert_eq!(
            evaluate(&via_node, &state, false, None, false),
            TrustVerdict::Trusted {
                rule: "claude".to_owned()
            }
        );
    }

    #[test]
    fn reports_all_nine_untrusted_reasons_and_root_denial() {
        let restricted = r#"
[resume.trust.claude]
dir = "~/code"
"#;
        let state = parse_config_toml(restricted, None, Some(Path::new("/home/u")));

        let no_rule = sample_record(
            "vim",
            Some("/usr/bin/vim"),
            Some(&["vim"]),
            &["vim"],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&no_rule, &state, false, None, false),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule)
        );

        let prog_differs = sample_record(
            "ssh",
            Some("/usr/bin/ssh"),
            Some(&["ssh", "box"]),
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&prog_differs, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::ProgramDiffers,
            }
        );

        let wrapped = sample_record_with_placement(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude"]),
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
            ProcessPlacement::Wrapped,
        );
        assert_eq!(
            evaluate(&wrapped, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::Wrapped,
            }
        );

        let dir_denied = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude"]),
            &["claude", "--resume", UUID],
            Some("/tmp/other"),
        );
        assert_eq!(
            evaluate(&dir_denied, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::DirNotAllowed,
            }
        );

        let unreadable = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            None,
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&unreadable, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::LaunchUnreadable,
            }
        );

        let unlisted = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude", "--print", "hi"]),
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&unlisted, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::FlagNotListed,
            }
        );

        let bad_val = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude", "--verbose=yes"]),
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&bad_val, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::FlagValue,
            }
        );

        let differs = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude", "--model", "opus"]),
            &["claude", "--resume", UUID, "--model", "haiku"],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&differs, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "claude".to_owned(),
                reason: RuleUntrustedReason::FlagsDiffer,
            }
        );

        let incomplete = parse_config_toml("[resume", None, None);
        assert_eq!(
            evaluate(&dir_denied, &incomplete, false, None, false),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::ConfigIncomplete)
        );

        let ok_rec = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&["claude"]),
            &["claude", "--resume", UUID],
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&ok_rec, &state, true, None, false),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::Root)
        );
    }

    #[test]
    fn under_matcher_rejects_symlink_escape_at_restore_time() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("evil.vim");
        fs::write(&secret, "").unwrap();
        let escape = sessions.join("escape.vim");
        std::os::unix::fs::symlink(&secret, &escape).unwrap();
        let safe = sessions.join("work.vim");
        fs::write(&safe, "").unwrap();

        let toml_text = format!(
            "[resume.trust.nvim]\ncommand = [\"nvim\", \"-S\", {{ under = {:?} }}]\n",
            sessions.to_str().unwrap()
        );
        let state = parse_config_toml(&toml_text, None, Some(dir.path()));

        let escape_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", escape.to_str().unwrap()],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&escape_rec, &state, false, None, false),
            TrustVerdict::Trusted {
                rule: "nvim".to_owned()
            }
        );
        assert_eq!(
            evaluate(&escape_rec, &state, false, None, true),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule)
        );

        let safe_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", safe.to_str().unwrap()],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&safe_rec, &state, false, None, true),
            TrustVerdict::Trusted {
                rule: "nvim".to_owned()
            }
        );

        let dangling = sessions.join("dangling.vim");
        std::os::unix::fs::symlink(outside.join("nonexistent.vim"), &dangling).unwrap();
        let dangling_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", dangling.to_str().unwrap()],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&dangling_rec, &state, false, None, true),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule)
        );

        let dangling_dir = sessions.join("dangling_dir");
        std::os::unix::fs::symlink(outside.join("nonexistent_dir"), &dangling_dir).unwrap();
        let dangling_child = dangling_dir.join("new.vim");
        let dangling_child_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", dangling_child.to_str().unwrap()],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&dangling_child_rec, &state, false, None, true),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule)
        );

        let fresh = sessions.join("fresh.vim");
        let fresh_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", fresh.to_str().unwrap()],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&fresh_rec, &state, false, None, true),
            TrustVerdict::Trusted {
                rule: "nvim".to_owned()
            }
        );

        let too_deep = format!(
            "{}/{}",
            sessions.to_str().unwrap(),
            vec!["a"; PATH_COMPONENTS_MAX + 5].join("/")
        );
        let deep_rec = sample_record(
            "nvim",
            Some("/usr/bin/nvim"),
            Some(&["nvim"]),
            &["nvim", "-S", &too_deep],
            Some(dir.path().to_str().unwrap()),
        );
        assert_eq!(
            evaluate(&deep_rec, &state, false, None, true),
            TrustVerdict::UntrustedGlobal(GlobalUntrustedReason::NoRule)
        );
    }

    #[test]
    fn repeatable_single_value_flags_never_swallow_prompts_and_dash_inline_round_trips() {
        let ConfigState::Loaded(config) = parse_config_toml("", None, Some(Path::new("/home/u")))
        else {
            panic!("default config loads");
        };
        let state = ConfigState::Loaded(config.clone());

        let pi_rule = config.rules.get("pi").unwrap();
        let pi_launch: Vec<String> = ["pi", "-e", "./a.ts", "-e", "./b.ts", "delete staging db"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let pi_kept = extract_kept_args(
            &pi_launch,
            pi_rule.program(),
            pi_rule.flags.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(pi_kept, ["-e", "./a.ts", "-e", "./b.ts"]);

        let claude_rule = config.rules.get("claude").unwrap();
        let claude_launch: Vec<String> = [
            "claude",
            "-r",
            "--plugin-dir",
            "./my-plugin",
            "delete prod db",
            "--tmux=classic",
            "--betas",
            "b1",
            "b2",
            "--debug=-hooks",
            "--add-dir=-weird",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let claude_kept = extract_kept_args(
            &claude_launch,
            claude_rule.program(),
            claude_rule.flags.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(
            claude_kept,
            [
                "--plugin-dir",
                "./my-plugin",
                "--tmux",
                "classic",
                "--betas",
                "b1",
                "b2",
                "--debug=-hooks",
                "--add-dir=-weird",
            ]
        );
        let mut reported = vec!["claude".to_owned(), "--resume".to_owned(), UUID.to_owned()];
        reported.extend(claude_kept);
        let launch_refs: Vec<&str> = claude_launch.iter().map(String::as_str).collect();
        let rep_refs: Vec<&str> = reported.iter().map(String::as_str).collect();
        let claude_rec = sample_record(
            "claude",
            Some("/usr/bin/claude"),
            Some(&launch_refs),
            &rep_refs,
            Some("/home/u/code"),
        );
        assert_eq!(
            evaluate(&claude_rec, &state, false, None, false),
            TrustVerdict::Trusted {
                rule: "claude".to_owned()
            }
        );

        let agy_rule = config.rules.get("agy").unwrap();
        let agy_launch: Vec<String> = [
            "agy",
            "--conversation",
            "conv-1",
            "--add-dir",
            "/extra",
            "do not replay",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let agy_kept = extract_kept_args(
            &agy_launch,
            agy_rule.program(),
            agy_rule.flags.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(agy_kept, ["--add-dir", "/extra"]);
    }

    #[test]
    fn absolute_target_rejects_spoofed_argv0_and_relative_paths() {
        let toml = r#"
[resume.trust.mytool]
command = ["/usr/local/bin/mytool", "--resume", { match = "id" }]
"#;
        let state = parse_config_toml(toml, None, Some(Path::new("/home/u")));

        let spoofed = sample_record(
            "evil",
            Some("/tmp/evil"),
            Some(&["/usr/local/bin/mytool"]),
            &["/usr/local/bin/mytool", "--resume", "abc"],
            Some("/home/u"),
        );
        assert_eq!(
            evaluate(&spoofed, &state, false, None, false),
            TrustVerdict::UntrustedRule {
                rule: "mytool".to_owned(),
                reason: RuleUntrustedReason::ProgramDiffers,
            }
        );

        let relative_argv = sample_record(
            "mytool",
            None,
            Some(&["mytool"]),
            &["/usr/local/bin/mytool", "--resume", "abc"],
            Some("/home/u"),
        );
        assert_eq!(
            evaluate(&relative_argv, &state, false, None, true),
            TrustVerdict::UntrustedRule {
                rule: "mytool".to_owned(),
                reason: RuleUntrustedReason::ProgramDiffers,
            }
        );

        let via_python = sample_record(
            "python3",
            Some("/usr/bin/python3"),
            Some(&["python3", "/usr/local/bin/mytool"]),
            &["/usr/local/bin/mytool", "--resume", "abc"],
            Some("/home/u"),
        );
        assert_eq!(
            evaluate(&via_python, &state, false, None, false),
            TrustVerdict::Trusted {
                rule: "mytool".to_owned()
            }
        );
    }
}
