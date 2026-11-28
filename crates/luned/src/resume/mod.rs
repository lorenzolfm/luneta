pub mod config;
pub mod store;
pub mod synthesize;
pub mod trust;

use lune_domain::agent::Harness;
use lune_protocol::program_resume::{ProcessPlacement, ResumeCwdSource, ResumeRecord};

use crate::api;
use config::{ConfigState, TrustRule};
use store::RecordSource;
use trust::{GlobalUntrustedReason, RuleUntrustedReason, TrustVerdict};

#[must_use]
pub fn running_as_root() -> bool {
    rustix::process::getuid().is_root() || rustix::process::geteuid().is_root()
}

#[must_use]
pub fn allows_legacy_agent(
    config_state: &ConfigState,
    harness: Harness,
    session: &str,
    folder: &str,
    as_root: bool,
) -> bool {
    let Some(config) = config_state.loaded() else {
        return false;
    };
    if as_root && !config.allow_root {
        return false;
    }
    let Some(rule) = config.rules.get(harness.program()) else {
        return false;
    };
    let words = [
        harness.program().to_owned(),
        harness.resumes().to_owned(),
        session.to_owned(),
    ];
    if words.len() != rule.command_len() || !trust::rule_matches_command(&words, rule, true) {
        return false;
    }
    if let Some(dirs) = &rule.dirs {
        return dirs
            .iter()
            .any(|base| trust::dir_allowed(folder, base, true));
    }
    true
}

#[must_use]
pub fn reload(current: &mut ConfigState) -> api::ReloadResumeResponse {
    let next = config::load();
    match next {
        ConfigState::Loaded(config) => {
            let config_path = config.path.as_ref().map(|p| p.display().to_string());
            let warnings = config.warnings.clone();
            let active_rules = u32::try_from(config.rules.len()).unwrap_or(u32::MAX);
            *current = ConfigState::Loaded(config);
            api::ReloadResumeResponse {
                config_path,
                warnings,
                outcome: Some(api::reload_resume_response::Outcome::ReloadedRules(
                    active_rules,
                )),
            }
        }
        ConfigState::Incomplete {
            path,
            error,
            warnings,
        } => {
            let kept_previous_rules = current
                .loaded()
                .map(|cfg| u32::try_from(cfg.rules.len()).unwrap_or(u32::MAX));
            if kept_previous_rules.is_none() {
                *current = ConfigState::Incomplete {
                    path: path.clone(),
                    error: error.clone(),
                    warnings: warnings.clone(),
                };
            }
            api::ReloadResumeResponse {
                config_path: path.map(|p| p.display().to_string()),
                warnings,
                outcome: Some(api::reload_resume_response::Outcome::Failed(
                    api::reload_resume_response::Failed {
                        error,
                        kept_previous_rules,
                    },
                )),
            }
        }
    }
}

#[must_use]
pub fn config_to_api(
    config_state: &ConfigState,
) -> (
    Option<String>,
    Vec<String>,
    api::list_resume_response::Config,
) {
    match config_state {
        ConfigState::Loaded(config) => (
            config.path.as_ref().map(|p| p.display().to_string()),
            config.warnings.clone(),
            api::list_resume_response::Config::Loaded(api::LoadedResumeConfig {
                rules: config.rules.values().map(rule_to_api).collect(),
                disabled_rules: config
                    .disabled
                    .iter()
                    .map(|(name, reason)| api::DisabledRuleInfo {
                        name: name.clone(),
                        reason: reason.clone(),
                    })
                    .collect(),
            }),
        ),
        ConfigState::Incomplete {
            path,
            error,
            warnings,
        } => (
            path.as_ref().map(|p| p.display().to_string()),
            warnings.clone(),
            api::list_resume_response::Config::Error(error.clone()),
        ),
    }
}

#[must_use]
pub fn record_to_api(
    source: RecordSource,
    record: &ResumeRecord,
    chosen: bool,
    persistence: api::ResumePersistence,
    verdict: &TrustVerdict,
) -> api::ResumeRecordInfo {
    let source = match source {
        RecordSource::Osc7577 => api::ResumeRecordSource::Osc7577,
        RecordSource::Synthesized => api::ResumeRecordSource::Synthesized,
    };
    let cwd = record.cwd().map(|source| match source {
        ResumeCwdSource::Explicit(dir) => {
            api::resume_record_info::Cwd::ExplicitCwd(dir.as_str().to_owned())
        }
        ResumeCwdSource::Inferred(dir) => {
            api::resume_record_info::Cwd::InferredCwd(dir.as_str().to_owned())
        }
    });
    let placement = match record.placement() {
        ProcessPlacement::Root => api::ResumePlacement::Root,
        ProcessPlacement::ShellHosted => api::ResumePlacement::ShellHosted,
        ProcessPlacement::Wrapped => api::ResumePlacement::Wrapped,
    };
    let verdict = match verdict {
        TrustVerdict::Trusted { rule } => {
            api::resume_record_info::Verdict::TrustedRule(rule.clone())
        }
        TrustVerdict::UntrustedGlobal(reason) => {
            api::resume_record_info::Verdict::Untrusted(api::resume_record_info::Untrusted {
                detail: Some(api::resume_record_info::untrusted::Detail::Global(
                    global_reason_to_api(*reason).into(),
                )),
            })
        }
        TrustVerdict::UntrustedRule { rule, reason } => {
            api::resume_record_info::Verdict::Untrusted(api::resume_record_info::Untrusted {
                detail: Some(api::resume_record_info::untrusted::Detail::ByRule(
                    api::resume_record_info::RuleRejected {
                        rule: rule.clone(),
                        reason: rule_reason_to_api(*reason).into(),
                    },
                )),
            })
        }
    };
    api::ResumeRecordInfo {
        source: source.into(),
        owner_pid: record.pid().get(),
        owner_name: record.process_name().as_str().to_owned(),
        argv: record.argv().words().to_vec(),
        cwd,
        placement: placement.into(),
        chosen,
        persistence: persistence.into(),
        reported_at: Some(record.reported_at().to_timestamp()),
        verdict: Some(verdict),
    }
}

fn rule_to_api(rule: &TrustRule) -> api::ResumeRuleInfo {
    let flags = rule
        .flag_counts()
        .map(|(kept, dropped)| api::resume_rule_info::FlagSummary { kept, dropped });
    let dirs = rule
        .dirs
        .as_ref()
        .map_or_default(|dirs| dirs.iter().map(|dir| dir.as_str().to_owned()).collect());
    api::ResumeRuleInfo {
        name: rule.name.clone(),
        builtin: rule.is_builtin(),
        command: rule.command_summary(),
        dirs,
        flags,
        overrides: rule.overrides().to_vec(),
    }
}

fn global_reason_to_api(reason: GlobalUntrustedReason) -> api::GlobalTrustReason {
    match reason {
        GlobalUntrustedReason::ConfigIncomplete => api::GlobalTrustReason::ConfigIncomplete,
        GlobalUntrustedReason::Root => api::GlobalTrustReason::Root,
        GlobalUntrustedReason::NoRule => api::GlobalTrustReason::NoRule,
    }
}

fn rule_reason_to_api(reason: RuleUntrustedReason) -> api::RuleTrustReason {
    match reason {
        RuleUntrustedReason::ProgramDiffers => api::RuleTrustReason::ProgramDiffers,
        RuleUntrustedReason::Wrapped => api::RuleTrustReason::Wrapped,
        RuleUntrustedReason::LaunchUnreadable => api::RuleTrustReason::LaunchUnreadable,
        RuleUntrustedReason::FlagNotListed => api::RuleTrustReason::FlagNotListed,
        RuleUntrustedReason::FlagValue => api::RuleTrustReason::FlagValue,
        RuleUntrustedReason::FlagsDiffer => api::RuleTrustReason::FlagsDiffer,
        RuleUntrustedReason::DirNotAllowed => api::RuleTrustReason::DirNotAllowed,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::resume::config::parse_config_toml;

    #[test]
    fn legacy_agent_enforces_command_shape_and_custom_rule_constraints() {
        let uuid = "015d3962-6f9e-45c3-92bc-c7cef5068215";
        let default_cfg = parse_config_toml("", None, Some(Path::new("/home/u")));
        assert!(allows_legacy_agent(
            &default_cfg,
            Harness::ClaudeCode,
            uuid,
            "/tmp",
            false
        ));
        assert!(!allows_legacy_agent(
            &default_cfg,
            Harness::ClaudeCode,
            "not-a-uuid",
            "/tmp",
            false
        ));
        assert!(!allows_legacy_agent(
            &default_cfg,
            Harness::ClaudeCode,
            uuid,
            "/tmp",
            true
        ));

        let pinned_cfg = parse_config_toml(
            "[resume.trust.claude]\ncommand = [\"/opt/claude\", \"--resume\", { match = \"uuid\" }]\n",
            None,
            Some(Path::new("/home/u")),
        );
        assert!(!allows_legacy_agent(
            &pinned_cfg,
            Harness::ClaudeCode,
            uuid,
            "/tmp",
            false
        ));
    }
}
