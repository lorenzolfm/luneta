use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime};

use lune_domain::agent::Harness;
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{TerminalViewId, VIEWS_MAX};
use lune_protocol::program_resume::{
    ProcStart, ProcessArgv, ProcessExe, ProcessName, ProcessPid, ProcessPlacement, ReportedAt,
    ResumeArgv, ResumeCwd, ResumeCwdSource, ResumeRecord,
};

use super::config::{BuiltinRule, ConfigState};
use super::trust::{extract_kept_args, process_matches_program};
use crate::agent::Agent;
use crate::process::{self, AgentPid, ProcessMeta};

pub const EXIT_GRACE: Duration = Duration::from_secs(2);

pub struct LiveTerminal {
    pub terminal: TerminalViewId,
    pub key: TerminalKey,
    pub root_pid: u32,
    pub front_pid: Option<u32>,
    pub has_osc_7577: bool,
}

struct CachedAgent {
    proc_start: ProcStart,
    meta: ProcessMeta,
    placement: ProcessPlacement,
}

struct ActiveSynthesized {
    record: ResumeRecord,
    exited_at: Option<Instant>,
}

#[derive(Default)]
pub struct Synthesizer {
    cache: HashMap<AgentPid, CachedAgent>,
    active: BTreeMap<TerminalViewId, ActiveSynthesized>,
}

impl Synthesizer {
    #[must_use]
    pub fn get(&self, terminal: TerminalViewId) -> Option<&ResumeRecord> {
        self.active.get(&terminal).map(|entry| &entry.record)
    }

    pub fn refresh(
        &mut self,
        terminals: &[LiveTerminal],
        agents: &[Agent],
        config: &ConfigState,
        now: Instant,
    ) {
        assert!(terminals.len() <= VIEWS_MAX);
        let mut live_pids = Vec::new();
        let mut seen_terminals = BTreeMap::new();
        let mut parent_cache = HashMap::new();

        for item in terminals {
            if item.has_osc_7577 {
                self.active.remove(&item.terminal);
                continue;
            }
            let mut candidates: Vec<&Agent> = agents
                .iter()
                .filter(|agent| {
                    synthesized_builtin(agent.harness).is_some()
                        && agent.session_id.is_some()
                        && agent.terminal.is_none_or(|key| key == item.key)
                        && (agent.pid.get() == item.root_pid
                            || is_in_terminal(agent.pid.get(), item.root_pid, &mut parent_cache))
                })
                .collect();
            candidates.sort_by_key(|agent| {
                let is_front = item.front_pid == Some(agent.pid.get());
                let is_sdk = agent
                    .entrypoint
                    .as_ref()
                    .is_some_and(|e| e.as_str().to_ascii_lowercase().starts_with("sdk-"));
                (!is_front, is_sdk, agent.pid.get())
            });
            let Some(&agent) = candidates.first() else {
                continue;
            };
            live_pids.push(agent.pid);
            if let Some(record) = self.build_record(agent, item.root_pid, config) {
                seen_terminals.insert(item.terminal, record);
            }
        }

        for (terminal, record) in seen_terminals {
            self.active.insert(terminal, ActiveSynthesized { record, exited_at: None });
        }

        let live_terminal_set: BTreeMap<TerminalViewId, bool> =
            terminals.iter().map(|item| (item.terminal, item.has_osc_7577)).collect();
        self.active.retain(|terminal, entry| {
            let Some(&has_osc) = live_terminal_set.get(terminal) else {
                return false;
            };
            if has_osc {
                return false;
            }
            if live_pids.iter().any(|pid| pid.get() == entry.record.pid().get()) {
                entry.exited_at = None;
                return true;
            }
            let exited_at = *entry.exited_at.get_or_insert(now);
            now.saturating_duration_since(exited_at) < EXIT_GRACE
        });

        self.cache.retain(|pid, _| live_pids.contains(pid));
        assert!(self.active.len() <= VIEWS_MAX);
    }

    pub fn remove(&mut self, terminal: TerminalViewId) {
        self.active.remove(&terminal);
    }

    fn build_record(
        &mut self,
        agent: &Agent,
        root_pid: u32,
        config: &ConfigState,
    ) -> Option<ResumeRecord> {
        let builtin = synthesized_builtin(agent.harness)?;
        let session_id = agent.session_id.as_ref()?.as_str();
        assert_ne!(session_id, "");
        let pid = ProcessPid::new(agent.pid.get())?;
        let proc_start = ProcStart::parse(process::start(pid.get())?.as_str().to_owned()).ok()?;
        let cached = self.cache.get(&agent.pid).filter(|c| c.proc_start == proc_start);
        let (meta, placement) = if let Some(cached) = cached {
            (cached.meta.clone(), cached.placement)
        } else {
            let meta = process::meta(pid.get())?;
            let placement = process::placement(pid.get(), root_pid);
            self.cache.insert(
                agent.pid,
                CachedAgent { proc_start: proc_start.clone(), meta: meta.clone(), placement },
            );
            (meta, placement)
        };
        let default_rule = builtin.rule();
        let active_rule = config
            .loaded()
            .and_then(|loaded| loaded.rules.get(builtin.name()));
        let rule_program = active_rule.map_or(&default_rule.program, |rule| &rule.program);
        if !process_matches_program(
            &meta.name,
            meta.exe.as_deref(),
            meta.argv.as_ref().map(ProcessArgv::words),
            rule_program,
            true,
        ) {
            return None;
        }
        let mut words = vec![
            rule_program.as_str().to_owned(),
            agent.harness.resumes().to_owned(),
            session_id.to_owned(),
        ];
        let rule_flags = active_rule.map_or(default_rule.flags.as_ref(), |rule| rule.flags.as_ref());
        if let Some(flags_spec) = rule_flags
            && let Some(launch_argv) = meta.argv.as_ref()
        {
            let kept = extract_kept_args(launch_argv.words(), rule_program, flags_spec).ok()?;
            words.extend(kept);
        }
        let argv = ResumeArgv::parse(words).ok()?;
        let cwd = ResumeCwd::parse(agent.cwd.as_str()).ok().map(ResumeCwdSource::Explicit);
        let reported_at =
            ReportedAt::from_system_time(agent.started_at.unwrap_or_else(SystemTime::now)).ok()?;
        let process_name = ProcessName::from_os(&meta.name);
        let process_exe = meta.exe.and_then(|exe| ProcessExe::parse(exe).ok());
        Some(ResumeRecord::new(
            pid,
            proc_start,
            meta.uid,
            process_name,
            process_exe,
            meta.argv,
            placement,
            argv,
            cwd,
            reported_at,
        ))
    }
}

fn synthesized_builtin(harness: Harness) -> Option<BuiltinRule> {
    match harness {
        Harness::ClaudeCode => Some(BuiltinRule::Claude),
        Harness::Antigravity => Some(BuiltinRule::Agy),
        Harness::Pi => None,
    }
}

fn parent_of(pid: u32, cache: &mut HashMap<u32, Option<u32>>) -> Option<u32> {
    if let Some(&cached) = cache.get(&pid) {
        return cached;
    }
    let parent = process::parent(pid);
    cache.insert(pid, parent);
    parent
}

fn is_in_terminal(pid: u32, root_pid: u32, parent_cache: &mut HashMap<u32, Option<u32>>) -> bool {
    if pid == root_pid {
        return true;
    }
    let mut current = pid;
    let mut visited = vec![pid];
    for _ in 0..process::ANCESTOR_DEPTH_MAX {
        let Some(parent) = parent_of(current, parent_cache) else {
            return false;
        };
        if parent == root_pid {
            return true;
        }
        if parent <= 1 || visited.contains(&parent) {
            return false;
        }
        visited.push(parent);
        current = parent;
    }
    false
}
