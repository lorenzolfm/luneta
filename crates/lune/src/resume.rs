use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::ExitCode;

use lune_protocol::program_resume::escape_for_display;
use luned::api::{
    self, DisabledRuleInfo, GlobalTrustReason, ListResumeRequest, ListResumeResponse,
    ReloadResumeRequest, ReloadResumeResponse, ResumePersistence, ResumePlacement,
    ResumeRecordInfo, ResumeRecordSource, ResumeRuleInfo, RuleTrustReason, TerminalResumeRecords,
};
use luned::folder::SOCKET;
use luned::log::causes;
use luned::resume::config::{self, ConfigState};

use crate::daemon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Rules,
    Reload,
    List { terminal: Option<NonZeroU32> },
}

pub fn run(action: Action, folder: &Path) -> ExitCode {
    if matches!(action, Action::Rules) {
        return show_rules_local();
    }
    let tasks = luned::tasks::shared();
    let daemon = match tasks.block_on(daemon::running(folder)) {
        Ok(daemon) => daemon,
        Err(unreached) => {
            eprintln!("lune: {unreached}");
            return unreached.exit_code();
        },
    };
    match action {
        Action::Rules => unreachable!("handled above"),
        Action::Reload => {
            let response = tasks.block_on(async {
                daemon.reload_resume(luned::ANSWER, ReloadResumeRequest { on: None }).await
            });
            match response {
                Ok(reply) => print_reload(&reply),
                Err(error) => {
                    eprintln!("lune: {}: {}", folder.join(SOCKET).display(), causes(&error));
                    ExitCode::FAILURE
                },
            }
        },
        Action::List { terminal } => {
            let terminal_id = terminal.map(NonZeroU32::get);
            let response = tasks.block_on(async {
                daemon.list_resume(luned::ANSWER, ListResumeRequest { on: None, terminal_id }).await
            });
            match response {
                Ok(reply) => print_list(&reply, terminal),
                Err(error) => {
                    eprintln!("lune: {}: {}", folder.join(SOCKET).display(), causes(&error));
                    ExitCode::FAILURE
                },
            }
        },
    }
}

fn show_rules_local() -> ExitCode {
    let state = config::load();
    let (path, warnings, cfg) = luned::resume::config_to_api(&state);
    let rendered = format_rules(path.as_deref(), &warnings, &cfg);
    print!("{rendered}");
    if matches!(state, ConfigState::Incomplete { .. }) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_reload(response: &ReloadResumeResponse) -> ExitCode {
    if let Some(path) = &response.config_path {
        println!("config: {}", escape_for_display(path));
    }
    for warning in &response.warnings {
        eprintln!("warning: {}", escape_for_display(warning));
    }
    match &response.outcome {
        Some(api::reload_resume_response::Outcome::ReloadedRules(active_rules)) => {
            println!("reloaded resume rules ({active_rules} active)");
            ExitCode::SUCCESS
        },
        Some(api::reload_resume_response::Outcome::Failed(failed)) => {
            let note = match failed.kept_previous_rules {
                Some(kept) => format!(" (kept {kept} previous active rule(s))"),
                None => " (program resume disabled)".to_owned(),
            };
            eprintln!("lune: reload failed: {}{note}", escape_for_display(&failed.error));
            ExitCode::FAILURE
        },
        None => {
            eprintln!("lune: luned returned a reload_resume response without an outcome");
            ExitCode::FAILURE
        },
    }
}

fn print_list(response: &ListResumeResponse, terminal: Option<NonZeroU32>) -> ExitCode {
    match format_list(response, terminal) {
        Ok(rendered) => {
            print!("{rendered}");
            if terminal.is_some() && response.terminals.is_empty() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        },
        Err(why) => {
            eprintln!("lune: luned returned a malformed resume listing: {why}");
            ExitCode::FAILURE
        },
    }
}

fn format_rules(
    path: Option<&str>,
    warnings: &[String],
    config: &api::list_resume_response::Config,
) -> String {
    let mut out = String::new();
    if let Some(path) = path {
        let _ = writeln!(out, "config: {}", escape_for_display(path));
    }
    let (rules, disabled): (&[ResumeRuleInfo], &[DisabledRuleInfo]) = match config {
        api::list_resume_response::Config::Loaded(loaded) => {
            (loaded.rules.as_slice(), loaded.disabled_rules.as_slice())
        },
        api::list_resume_response::Config::Error(error) => {
            let _ =
                writeln!(out, "error: {} (program resume disabled)", escape_for_display(error));
            (&[], &[])
        },
    };
    for warning in warnings {
        let _ = writeln!(out, "warning: {}", escape_for_display(warning));
    }
    if rules.is_empty() {
        let _ = writeln!(out, "active rules: none");
    } else {
        let _ = writeln!(out, "active rules ({}):", rules.len());
        for rule in rules {
            let origin = if rule.builtin { "built-in" } else { "custom" };
            let name = escape_for_display(&rule.name);
            let cmd = escape_for_display(&rule.command);
            let _ = write!(out, "  {name} [{origin}]: {cmd}");
            if let Some(flags) = &rule.flags {
                let _ = write!(out, "  (flags: {} kept, {} dropped)", flags.kept, flags.dropped);
            }
            out.push('\n');
            if !rule.dirs.is_empty() {
                let dirs: Vec<String> = rule.dirs.iter().map(|d| escape_for_display(d)).collect();
                let _ = writeln!(out, "    dirs: {}", dirs.join(", "));
            }
            if !rule.overrides.is_empty() {
                let overrides: Vec<String> =
                    rule.overrides.iter().map(|o| escape_for_display(o)).collect();
                let _ = writeln!(out, "    overrides: {}", overrides.join(", "));
            }
        }
    }
    if !disabled.is_empty() {
        let _ = writeln!(out, "disabled rules ({}):", disabled.len());
        for item in disabled {
            let _ = writeln!(
                out,
                "  {}: {}",
                escape_for_display(&item.name),
                escape_for_display(&item.reason)
            );
        }
    }
    out
}

fn format_list(
    response: &ListResumeResponse,
    wanted: Option<NonZeroU32>,
) -> Result<String, String> {
    let config = response
        .config
        .as_ref()
        .ok_or_else(|| "missing config status".to_owned())?;
    let mut out = String::new();
    if let Some(path) = &response.config_path {
        let _ = writeln!(out, "config: {}", escape_for_display(path));
    }
    let active_rules = match config {
        api::list_resume_response::Config::Loaded(loaded) => loaded.rules.len(),
        api::list_resume_response::Config::Error(error) => {
            let _ =
                writeln!(out, "error: {} (program resume disabled)", escape_for_display(error));
            0
        },
    };
    for warning in &response.config_warnings {
        let _ = writeln!(out, "warning: {}", escape_for_display(warning));
    }
    if response.terminals.is_empty() {
        if let Some(id) = wanted {
            let _ = writeln!(out, "no terminal {id}");
        } else {
            let _ = writeln!(
                out,
                "no terminals hold a resume record ({active_rules} active rule(s))"
            );
        }
        return Ok(out);
    }
    for entry in &response.terminals {
        format_terminal(&mut out, entry)?;
    }
    Ok(out)
}

fn format_terminal(out: &mut String, entry: &TerminalResumeRecords) -> Result<(), String> {
    let label = match entry.owner.as_ref().ok_or_else(|| "terminal entry missing owner".to_owned())? {
        api::terminal_resume_records::Owner::Layout(layout) => match &layout.terminal {
            Some(key) => format!("terminal {}", key.id),
            None => format!("view {}", layout.view_id),
        },
        api::terminal_resume_records::Owner::Untracked(key) => format!("terminal {}", key.id),
    };
    let folder = escape_for_display(&entry.folder);
    let _ = writeln!(out, "{label} ({folder}):");
    if entry.records.is_empty() {
        let _ = writeln!(out, "  no resume records");
        return Ok(());
    }
    for record in &entry.records {
        format_record(out, record)?;
    }
    Ok(())
}

fn format_record(out: &mut String, record: &ResumeRecordInfo) -> Result<(), String> {
    let source = match ResumeRecordSource::try_from(record.source) {
        Ok(ResumeRecordSource::Osc7577) => "osc7577",
        Ok(ResumeRecordSource::Synthesized) => "synthesized",
        Ok(ResumeRecordSource::Unspecified) | Err(_) => {
            return Err("resume record has unspecified source".to_owned());
        },
    };
    let placement = match ResumePlacement::try_from(record.placement) {
        Ok(ResumePlacement::Root) => ResumePlacement::Root,
        Ok(ResumePlacement::ShellHosted) => ResumePlacement::ShellHosted,
        Ok(ResumePlacement::Wrapped) => ResumePlacement::Wrapped,
        Ok(ResumePlacement::Unspecified) | Err(_) => {
            return Err("resume record has unspecified placement".to_owned());
        },
    };
    let persistence = match ResumePersistence::try_from(record.persistence) {
        Ok(ResumePersistence::Unsaved) => ResumePersistence::Unsaved,
        Ok(ResumePersistence::Saved) => ResumePersistence::Saved,
        Ok(ResumePersistence::Tried) => ResumePersistence::Tried,
        Ok(ResumePersistence::Unspecified) | Err(_) => {
            return Err("resume record has unspecified persistence".to_owned());
        },
    };
    let trust = format_verdict(record.verdict.as_ref())?;
    let mut tags = Vec::new();
    if record.chosen {
        tags.push("chosen");
    }
    match persistence {
        ResumePersistence::Unsaved => {},
        ResumePersistence::Saved => tags.push("saved"),
        ResumePersistence::Tried => {
            tags.push("saved");
            tags.push("tried");
        },
        ResumePersistence::Unspecified => unreachable!("validated above"),
    }
    match placement {
        ResumePlacement::Root => tags.push("root"),
        ResumePlacement::ShellHosted => {},
        ResumePlacement::Wrapped => tags.push("wrapped"),
        ResumePlacement::Unspecified => unreachable!("validated above"),
    }
    let tag_suffix = if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join(", "))
    };
    let owner = escape_for_display(&record.owner_name);
    let _ = writeln!(
        out,
        "  [{source}] pid {} ({owner}) {trust}{tag_suffix}",
        record.owner_pid
    );
    let argv = format_argv(&record.argv);
    let _ = writeln!(out, "    argv: {argv}");
    if let Some(cwd) = &record.cwd {
        let (kind, path) = match cwd {
            api::resume_record_info::Cwd::ExplicitCwd(path) => ("explicit", path.as_str()),
            api::resume_record_info::Cwd::InferredCwd(path) => ("inferred", path.as_str()),
        };
        let _ = writeln!(out, "    cwd ({kind}): {}", escape_for_display(path));
    }
    Ok(())
}

fn format_verdict(verdict: Option<&api::resume_record_info::Verdict>) -> Result<String, String> {
    match verdict.ok_or_else(|| "resume record missing verdict".to_owned())? {
        api::resume_record_info::Verdict::TrustedRule(rule) => {
            if rule.is_empty() {
                return Err("trusted resume record has empty rule name".to_owned());
            }
            Ok(format!("trusted (rule {})", escape_for_display(rule)))
        },
        api::resume_record_info::Verdict::Untrusted(untrusted) => match untrusted
            .detail
            .as_ref()
            .ok_or_else(|| "untrusted resume record missing detail".to_owned())?
        {
            api::resume_record_info::untrusted::Detail::Global(raw) => {
                let reason = format_global_reason(*raw)?;
                Ok(format!("untrusted ({reason})"))
            },
            api::resume_record_info::untrusted::Detail::ByRule(rejected) => {
                if rejected.rule.is_empty() {
                    return Err("rule-rejected resume record has empty rule name".to_owned());
                }
                let reason = format_rule_reason(rejected.reason)?;
                Ok(format!(
                    "untrusted ({reason}, rule {})",
                    escape_for_display(&rejected.rule)
                ))
            },
        },
    }
}

fn format_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|word| {
            let escaped = escape_for_display(word);
            if escaped.is_empty() || escaped.contains(char::is_whitespace) {
                format!("{escaped:?}")
            } else {
                escaped
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_global_reason(raw: i32) -> Result<&'static str, String> {
    match GlobalTrustReason::try_from(raw) {
        Ok(GlobalTrustReason::ConfigIncomplete) => Ok("config_incomplete"),
        Ok(GlobalTrustReason::Root) => Ok("root"),
        Ok(GlobalTrustReason::NoRule) => Ok("no_rule"),
        Ok(GlobalTrustReason::Unspecified) | Err(_) => {
            Err("resume record has unspecified global trust reason".to_owned())
        },
    }
}

fn format_rule_reason(raw: i32) -> Result<&'static str, String> {
    match RuleTrustReason::try_from(raw) {
        Ok(RuleTrustReason::ProgramDiffers) => Ok("program_differs"),
        Ok(RuleTrustReason::Wrapped) => Ok("wrapped"),
        Ok(RuleTrustReason::LaunchUnreadable) => Ok("launch_unreadable"),
        Ok(RuleTrustReason::FlagNotListed) => Ok("flag_not_listed"),
        Ok(RuleTrustReason::FlagValue) => Ok("flag_value"),
        Ok(RuleTrustReason::FlagsDiffer) => Ok("flags_differ"),
        Ok(RuleTrustReason::DirNotAllowed) => Ok("dir_not_allowed"),
        Ok(RuleTrustReason::Unspecified) | Err(_) => {
            Err("resume record has unspecified rule trust reason".to_owned())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luned::api::{LoadedResumeConfig, TerminalKey};

    #[test]
    fn displayed_resume_output_escapes_control_and_bidi_override_characters() {
        let response = ListResumeResponse {
            config_path: Some("/home/u/.config/lune/config.toml".to_owned()),
            config_warnings: vec!["bad\x1b[31mwarn".to_owned()],
            config: Some(api::list_resume_response::Config::Loaded(LoadedResumeConfig {
                rules: Vec::new(),
                disabled_rules: Vec::new(),
            })),
            terminals: vec![TerminalResumeRecords {
                owner: Some(api::terminal_resume_records::Owner::Layout(
                    api::terminal_resume_records::LayoutTerminal {
                        view_id: 7,
                        terminal: Some(TerminalKey { id: 3, made_at: None }),
                    },
                )),
                folder: "/tmp/dir\u{202e}spoof".to_owned(),
                records: vec![ResumeRecordInfo {
                    source: ResumeRecordSource::Osc7577.into(),
                    owner_pid: 4242,
                    owner_name: "evil\x07proc".to_owned(),
                    argv: vec![
                        "nvim".to_owned(),
                        "-S".to_owned(),
                        "/tmp/\x1b]52;c;YQ==\x07\u{202e}sess.vim".to_owned(),
                    ],
                    cwd: Some(api::resume_record_info::Cwd::ExplicitCwd(
                        "/tmp/cwd\r\ninject".to_owned(),
                    )),
                    placement: ResumePlacement::ShellHosted.into(),
                    chosen: true,
                    persistence: ResumePersistence::Saved.into(),
                    reported_at: None,
                    verdict: Some(api::resume_record_info::Verdict::Untrusted(
                        api::resume_record_info::Untrusted {
                            detail: Some(api::resume_record_info::untrusted::Detail::ByRule(
                                api::resume_record_info::RuleRejected {
                                    rule: "nvim".to_owned(),
                                    reason: RuleTrustReason::DirNotAllowed.into(),
                                },
                            )),
                        },
                    )),
                }],
            }],
        };

        let rendered = format_list(&response, None).unwrap();
        assert!(!rendered.contains('\x1b'), "raw ESC leaked: {rendered:?}");
        assert!(!rendered.contains('\x07'), "raw BEL leaked: {rendered:?}");
        assert!(!rendered.contains('\r'), "raw CR leaked: {rendered:?}");
        assert!(!rendered.contains('\u{202e}'), "raw RLO bidi override leaked: {rendered:?}");
        assert!(rendered.contains("\\x1b"), "{rendered}");
        assert!(rendered.contains("\\u{202e}"), "{rendered}");
        assert!(rendered.contains("untrusted (dir_not_allowed, rule nvim)"), "{rendered}");
        assert!(rendered.contains("[chosen, saved]"), "{rendered}");
    }

    #[test]
    fn malformed_list_resume_response_is_rejected() {
        let missing_config = ListResumeResponse {
            config_path: None,
            config_warnings: Vec::new(),
            config: None,
            terminals: Vec::new(),
        };
        assert!(format_list(&missing_config, None).is_err());

        let unspecified_source = ListResumeResponse {
            config_path: None,
            config_warnings: Vec::new(),
            config: Some(api::list_resume_response::Config::Loaded(LoadedResumeConfig::default())),
            terminals: vec![TerminalResumeRecords {
                owner: Some(api::terminal_resume_records::Owner::Untracked(TerminalKey {
                    id: 1,
                    made_at: None,
                })),
                folder: "/tmp".to_owned(),
                records: vec![ResumeRecordInfo {
                    source: ResumeRecordSource::Unspecified.into(),
                    owner_pid: 1,
                    owner_name: "x".to_owned(),
                    argv: vec!["x".to_owned()],
                    cwd: None,
                    placement: ResumePlacement::Root.into(),
                    chosen: true,
                    persistence: ResumePersistence::Unsaved.into(),
                    reported_at: None,
                    verdict: Some(api::resume_record_info::Verdict::TrustedRule("x".to_owned())),
                }],
            }],
        };
        assert!(format_list(&unspecified_source, None).is_err());
    }
}
