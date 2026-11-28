use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::time::{Duration, Instant, SystemTime};

use lune_protocol::program_resume::{
    AcceptedReport, ProcStart, ProcessArgv, ProcessExe, ProcessName, ProcessPid, ProcessPlacement,
    RECORDS_PER_TERMINAL_MAX, ReportedAt, ResumeArgv, ResumeCwd, ResumeCwdSource, ResumeRecord,
    TerminalResumes, Verified, encode_base64_url_nopad, is_shell_ancestor, is_valid_token,
    verify_and_parse,
};

use crate::process::{self, ANCESTOR_DEPTH_MAX, OwnerInfo};

pub const EXIT_GRACE: Duration = Duration::from_secs(2);

pub const RECORDS_MAX: usize = RECORDS_PER_TERMINAL_MAX;

const TOKEN_RANDOM_BYTES: usize = 24;

const LAST_TIME_PIDS_MAX: usize = 256;

const _: () = assert!(RECORDS_MAX > 0);
const _: () = assert!(TOKEN_RANDOM_BYTES >= 16);
const _: () = assert!(EXIT_GRACE.as_secs() == 2);

#[derive(Clone, PartialEq, Eq)]
pub struct ResumeToken(String);

impl ResumeToken {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
        let mut urandom = File::open("/dev/urandom")?;
        urandom.read_exact(&mut bytes)?;
        let token = encode_base64_url_nopad(&bytes);
        assert!(is_valid_token(&token));
        Ok(Self(token))
    }

    #[cfg(test)]
    #[must_use]
    pub fn from_test(token: &str) -> Self {
        assert!(is_valid_token(token));
        Self(token.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResumeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResumeToken(<redacted>)")
    }
}

pub trait ProcessInspector {
    fn owner_info(&self, pid: u32) -> Option<OwnerInfo>;
    fn argv(&self, pid: u32) -> Option<ProcessArgv>;
    fn same_root(&self, pid: u32, root_pid: u32) -> bool;
}

pub struct OsInspector;

impl ProcessInspector for OsInspector {
    fn owner_info(&self, pid: u32) -> Option<OwnerInfo> {
        process::owner_info(pid)
    }

    fn argv(&self, pid: u32) -> Option<ProcessArgv> {
        process::process_argv(pid)
    }

    fn same_root(&self, pid: u32, root_pid: u32) -> bool {
        process::same_root(pid, root_pid)
    }
}

struct StoredRecord {
    record: ResumeRecord,
    exited_at: Option<Instant>,
    seq: u64,
}

struct LastTime {
    time: u64,
    seq: u64,
}

pub struct ProgramResumes {
    token: ResumeToken,
    pty_root_pid: u32,
    pty_device: Option<u64>,
    records: BTreeMap<ProcessPid, StoredRecord>,
    chosen: Option<ProcessPid>,
    last_times: BTreeMap<ProcessPid, LastTime>,
    seq: u64,
    version: u64,
}

impl ProgramResumes {
    #[must_use]
    pub fn new(token: ResumeToken, pty_root_pid: u32, pty_device: Option<u64>) -> Self {
        Self {
            token,
            pty_root_pid,
            pty_device,
            records: BTreeMap::new(),
            chosen: None,
            last_times: BTreeMap::new(),
            seq: 0,
            version: 0,
        }
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub fn has_exiting(&self) -> bool {
        self.records.values().any(|entry| entry.exited_at.is_some())
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<TerminalResumes> {
        assert!(self.records.len() <= RECORDS_MAX);
        if self.records.is_empty() {
            assert!(self.chosen.is_none());
            return None;
        }
        let chosen_pid = self
            .chosen
            .expect("non-empty resume records have a chosen pid");
        let chosen = self
            .records
            .get(&chosen_pid)
            .expect("chosen pid exists in resume records")
            .record
            .clone();
        let others: Vec<ResumeRecord> = self
            .records
            .iter()
            .filter(|&(&pid, _)| pid != chosen_pid)
            .map(|(_, entry)| entry.record.clone())
            .collect();
        Some(TerminalResumes::new(chosen, others).expect("stored resume records are valid"))
    }

    #[cfg(test)]
    pub fn apply_report(
        &mut self,
        body: &[u8],
        now_wall: SystemTime,
        real_uid: u32,
        effective_uid: u32,
        inspect: &impl ProcessInspector,
    ) {
        self.apply_reports([body], now_wall, real_uid, effective_uid, inspect);
    }

    pub fn apply_reports<I, B>(
        &mut self,
        bodies: I,
        now_wall: SystemTime,
        real_uid: u32,
        effective_uid: u32,
        inspect: &impl ProcessInspector,
    ) where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let Ok(reported_at) = ReportedAt::from_system_time(now_wall) else {
            return;
        };
        let mut changed = false;
        for body in bodies {
            changed |= self.apply_one(body.as_ref(), reported_at, real_uid, effective_uid, inspect);
        }
        if changed {
            self.recompute_chosen(inspect);
            self.bump_version();
        }
    }

    fn apply_one(
        &mut self,
        body: &[u8],
        reported_at: ReportedAt,
        real_uid: u32,
        effective_uid: u32,
        inspect: &impl ProcessInspector,
    ) -> bool {
        let verified = verify_and_parse(body, self.token.as_str(), reported_at.secs(), |pid| {
            self.last_times.get(&pid).map(|entry| entry.time)
        });
        match verified {
            Verified::Ignore => false,
            Verified::Reject { owner, .. } => {
                owner.is_some_and(|pid| self.records.remove(&pid).is_some())
            }
            Verified::Accept(AcceptedReport::Clear { pid, time }) => {
                self.record_time(pid, time);
                self.records.remove(&pid).is_some()
            }
            Verified::Accept(AcceptedReport::Set {
                pid,
                time,
                argv,
                cwd,
            }) => {
                self.record_time(pid, time);
                let Some(record) = self.validate_owner(
                    pid,
                    real_uid,
                    effective_uid,
                    argv,
                    cwd,
                    reported_at,
                    inspect,
                ) else {
                    return self.records.remove(&pid).is_some();
                };
                self.insert_record(record);
                true
            }
        }
    }

    pub fn reap_exited(&mut self, now: Instant, inspect: &impl ProcessInspector) {
        if self.records.is_empty() {
            return;
        }
        let mut to_remove = Vec::new();
        let mut exiting_changed = false;
        for (&pid, entry) in &mut self.records {
            let alive = inspect
                .owner_info(pid.get())
                .is_some_and(|info| info.proc_start == entry.record.proc_start().as_str());
            if alive {
                if entry.exited_at.take().is_some() {
                    exiting_changed = true;
                }
                continue;
            }
            let exited_at = *entry.exited_at.get_or_insert(now);
            if now.saturating_duration_since(exited_at) >= EXIT_GRACE {
                to_remove.push(pid);
            } else if exited_at == now {
                exiting_changed = true;
            }
        }
        let removed = !to_remove.is_empty();
        for pid in to_remove {
            let gone = self.records.remove(&pid);
            assert!(gone.is_some());
        }
        if removed || exiting_changed {
            let chosen_changed = self.recompute_chosen(inspect);
            if removed || chosen_changed {
                self.bump_version();
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "owner validation bundles the verified OSC fields and process inspector"
    )]
    fn validate_owner(
        &self,
        pid: ProcessPid,
        real_uid: u32,
        effective_uid: u32,
        argv: ResumeArgv,
        explicit_cwd: Option<ResumeCwd>,
        reported_at: ReportedAt,
        inspect: &impl ProcessInspector,
    ) -> Option<ResumeRecord> {
        if real_uid != effective_uid {
            return None;
        }
        let owner = inspect.owner_info(pid.get())?;
        if owner.uid != real_uid || owner.euid != effective_uid {
            return None;
        }
        let proc_start = ProcStart::parse(owner.proc_start).ok()?;
        let chain = ancestor_chain(pid.get(), inspect);
        assert!(chain.is_empty() || chain[0].pid == pid.get());
        let is_descendant = chain.iter().any(|entry| entry.pid == self.pty_root_pid);
        let same_tty = self.pty_device.is_some() && owner.terminal_device == self.pty_device;
        if !is_descendant && !same_tty {
            return None;
        }
        let placement = if pid.get() == self.pty_root_pid {
            ProcessPlacement::Root
        } else if !inspect.same_root(pid.get(), self.pty_root_pid)
            || is_wrapped(&chain, self.pty_root_pid, inspect)
        {
            ProcessPlacement::Wrapped
        } else {
            ProcessPlacement::ShellHosted
        };
        let cwd = match explicit_cwd {
            Some(dir) => Some(ResumeCwdSource::Explicit(dir)),
            None => owner
                .cwd
                .as_deref()
                .and_then(|dir| ResumeCwd::parse(dir).ok())
                .map(ResumeCwdSource::Inferred),
        };
        let process_name = ProcessName::from_os(&owner.name);
        let process_exe = owner.exe.and_then(|exe| ProcessExe::parse(exe).ok());
        let process_argv = inspect.argv(pid.get());
        Some(ResumeRecord::new(
            pid,
            proc_start,
            owner.uid,
            process_name,
            process_exe,
            process_argv,
            placement,
            argv,
            cwd,
            reported_at,
        ))
    }

    fn insert_record(&mut self, record: ResumeRecord) {
        let pid = record.pid();
        if !self.records.contains_key(&pid) && self.records.len() >= RECORDS_MAX {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(&old_pid, _)| old_pid);
            if let Some(old_pid) = oldest {
                let evicted = self.records.remove(&old_pid);
                assert!(evicted.is_some());
            }
        }
        let seq = self.next_seq();
        self.records.insert(
            pid,
            StoredRecord {
                record,
                exited_at: None,
                seq,
            },
        );
        assert!(self.records.len() <= RECORDS_MAX);
    }

    fn record_time(&mut self, pid: ProcessPid, time: u64) {
        if !self.last_times.contains_key(&pid) && self.last_times.len() >= LAST_TIME_PIDS_MAX {
            let oldest = self
                .last_times
                .iter()
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(&old_pid, _)| old_pid);
            if let Some(old_pid) = oldest {
                let evicted = self.last_times.remove(&old_pid);
                assert!(evicted.is_some());
            }
        }
        let seq = self.next_seq();
        self.last_times.insert(pid, LastTime { time, seq });
        assert!(self.last_times.len() <= LAST_TIME_PIDS_MAX);
    }

    fn recompute_chosen(&mut self, inspect: &impl ProcessInspector) -> bool {
        if self.records.is_empty() {
            return self.chosen.take().is_some();
        }
        let pids_and_starts: Vec<(ProcessPid, String)> = self
            .records
            .iter()
            .map(|(&pid, entry)| (pid, entry.record.proc_start().as_str().to_owned()))
            .collect();
        let mut has_recorded_ancestor = BTreeMap::new();
        for &(pid, _) in &pids_and_starts {
            let chain = ancestor_chain(pid.get(), inspect);
            let under_other = chain.iter().skip(1).any(|ancestor| {
                pids_and_starts.iter().any(|(other_pid, other_start)| {
                    other_pid.get() == ancestor.pid && *other_start == ancestor.proc_start
                })
            });
            has_recorded_ancestor.insert(pid, under_other);
        }
        let best_from = |only_top: bool| -> Option<ProcessPid> {
            self.records
                .iter()
                .filter(|(pid, _)| {
                    !only_top || !has_recorded_ancestor.get(pid).copied().unwrap_or(false)
                })
                .max_by_key(|(_, entry)| (entry.exited_at.is_none(), entry.seq))
                .map(|(&pid, _)| pid)
        };
        let winner = best_from(true).or_else(|| best_from(false));
        assert!(winner.is_some_and(|pid| self.records.contains_key(&pid)));
        let changed = self.chosen != winner;
        self.chosen = winner;
        changed
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self
            .seq
            .checked_add(1)
            .expect("resume records update fewer than 2^64 times");
        self.seq
    }

    fn bump_version(&mut self) {
        self.version = self
            .version
            .checked_add(1)
            .expect("resume records change fewer than 2^64 times");
    }
}

fn ancestor_chain(pid: u32, inspect: &impl ProcessInspector) -> Vec<OwnerInfo> {
    let mut chain = Vec::new();
    let mut current = pid;
    for _ in 0..ANCESTOR_DEPTH_MAX {
        let Some(info) = inspect.owner_info(current) else {
            break;
        };
        let parent = info.parent;
        chain.push(info);
        if parent <= 1 || parent == current || chain.iter().any(|seen| seen.pid == parent) {
            break;
        }
        current = parent;
    }
    assert!(chain.len() <= ANCESTOR_DEPTH_MAX);
    chain
}

fn is_wrapped(chain: &[OwnerInfo], pty_root_pid: u32, inspect: &impl ProcessInspector) -> bool {
    for ancestor in chain.iter().skip(1) {
        let is_pty_root = ancestor.pid == pty_root_pid;
        let argv = inspect.argv(ancestor.pid);
        if !is_shell_ancestor(
            &ancestor.name,
            ancestor.exe.as_deref(),
            argv.as_ref().map(ProcessArgv::words),
            is_pty_root,
        ) {
            return true;
        }
        if is_pty_root {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    use lune_protocol::program_resume::{encode_base64_standard, sha256_body_token_hex};

    use super::*;

    #[derive(Default)]
    struct FakeInspector {
        owners: HashMap<u32, OwnerInfo>,
        argvs: HashMap<u32, ProcessArgv>,
        different_roots: Vec<u32>,
    }

    impl FakeInspector {
        fn add(
            &mut self,
            pid: u32,
            parent: u32,
            uid: u32,
            start: &str,
            name: &str,
            argv: &[&str],
            tty: Option<u64>,
            cwd: Option<&str>,
        ) {
            self.owners.insert(
                pid,
                OwnerInfo {
                    pid,
                    parent,
                    uid,
                    euid: uid,
                    proc_start: start.to_owned(),
                    name: name.to_owned(),
                    exe: Some(format!("/usr/bin/{name}")),
                    cwd: cwd.map(str::to_owned),
                    terminal_device: tty,
                },
            );
            self.argvs.insert(
                pid,
                ProcessArgv::bounded(argv.iter().map(|s| (*s).to_owned()).collect()),
            );
        }
    }

    impl ProcessInspector for FakeInspector {
        fn owner_info(&self, pid: u32) -> Option<OwnerInfo> {
            self.owners.get(&pid).cloned()
        }

        fn argv(&self, pid: u32) -> Option<ProcessArgv> {
            self.argvs.get(&pid).cloned()
        }

        fn same_root(&self, pid: u32, _root_pid: u32) -> bool {
            !self.different_roots.contains(&pid)
        }
    }

    fn sign(prefix: &str, token: &str) -> Vec<u8> {
        let mac = sha256_body_token_hex(prefix.as_bytes(), token.as_bytes());
        let mac = std::str::from_utf8(&mac).unwrap();
        format!("{prefix}:mac={mac}").into_bytes()
    }

    fn report_argv(pid: u32, time: u64, words: &[&str], cwd: Option<&str>, token: &str) -> Vec<u8> {
        let encoded: Vec<String> = words
            .iter()
            .map(|w| encode_base64_standard(w.as_bytes()))
            .collect();
        let mut prefix = format!("pid={pid}:time={time}:argv={}", encoded.join(","));
        if let Some(dir) = cwd {
            prefix.push_str(":cwd=");
            prefix.push_str(&encode_base64_standard(dir.as_bytes()));
        }
        sign(&prefix, token)
    }

    #[test]
    fn generated_tokens_are_valid_unique_and_redacted_in_debug() {
        let first = ResumeToken::generate().unwrap();
        let second = ResumeToken::generate().unwrap();
        assert_ne!(first, second);
        assert!(is_valid_token(first.as_str()));
        assert_eq!(first.as_str().len(), 32);
        let debug = format!("{first:?}");
        assert_eq!(debug, "ResumeToken(<redacted>)");
        assert!(!debug.contains(first.as_str()));
    }

    #[test]
    fn ancestor_is_preferred_over_descendant_and_clear_restores_child() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            20,
            10,
            1000,
            "s20",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            30,
            20,
            1000,
            "s30",
            "nvim",
            &["nvim"],
            Some(7),
            Some("/work/sub"),
        );

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let t = 1_800_000_000;

        let child_rep = report_argv(30, t, &["nvim", "-S", "/tmp/s.vim"], None, token.as_str());
        resumes.apply_report(&child_rep, now_wall, 1000, 1000, &inspector);
        let snap = resumes.snapshot().unwrap();
        assert_eq!(snap.count(), 1);
        assert_eq!(snap.chosen().pid().get(), 30);
        assert_eq!(snap.chosen().placement(), ProcessPlacement::Wrapped);
        assert_eq!(
            snap.chosen().cwd(),
            Some(&ResumeCwdSource::Inferred(
                ResumeCwd::parse("/work/sub").unwrap()
            ))
        );

        let parent_rep = report_argv(
            20,
            t,
            &["claude", "--resume", "015d3962-6f9e-45c3-92bc-c7cef5068215"],
            Some("/work"),
            token.as_str(),
        );
        resumes.apply_report(&parent_rep, now_wall, 1000, 1000, &inspector);
        let snap = resumes.snapshot().unwrap();
        assert_eq!(snap.count(), 2);
        let parent = snap.chosen();
        assert_eq!(parent.pid().get(), 20);
        assert_eq!(snap.others().len(), 1);
        assert_eq!(snap.others()[0].pid().get(), 30);
        assert_eq!(parent.placement(), ProcessPlacement::ShellHosted);
        assert_eq!(
            parent.cwd(),
            Some(&ResumeCwdSource::Explicit(
                ResumeCwd::parse("/work").unwrap()
            ))
        );

        let clear = sign(&format!("pid=20:time={t}:clear=1"), token.as_str());
        resumes.apply_report(&clear, now_wall, 1000, 1000, &inspector);
        let snap = resumes.snapshot().unwrap();
        assert_eq!(snap.count(), 1);
        assert_eq!(snap.chosen().pid().get(), 30);
    }

    #[test]
    fn dead_owner_survives_grace_window_and_is_removed_after() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            20,
            10,
            1000,
            "s20",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let rep = report_argv(20, 1_800_000_000, &["claude"], None, token.as_str());
        resumes.apply_report(&rep, now_wall, 1000, 1000, &inspector);
        assert_eq!(resumes.snapshot().unwrap().count(), 1);

        inspector.owners.remove(&20);
        let t0 = Instant::now();
        resumes.reap_exited(t0, &inspector);
        assert_eq!(
            resumes.snapshot().unwrap().count(),
            1,
            "kept during grace window"
        );
        assert!(resumes.has_exiting());

        resumes.reap_exited(t0 + Duration::from_millis(1900), &inspector);
        assert_eq!(
            resumes.snapshot().unwrap().count(),
            1,
            "still inside 2s grace"
        );

        resumes.reap_exited(t0 + EXIT_GRACE, &inspector);
        assert!(resumes.snapshot().is_none(), "pruned after 2s grace");
    }

    #[test]
    fn reused_pid_with_different_start_time_counts_as_exited() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            20,
            10,
            1000,
            "s20",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let rep = report_argv(20, 1_800_000_000, &["claude"], None, token.as_str());
        resumes.apply_report(&rep, now_wall, 1000, 1000, &inspector);

        inspector.add(
            20,
            10,
            1000,
            "s20-new",
            "sleep",
            &["sleep", "60"],
            Some(7),
            Some("/work"),
        );
        let t0 = Instant::now();
        resumes.reap_exited(t0, &inspector);
        assert!(resumes.has_exiting());
        resumes.reap_exited(t0 + EXIT_GRACE, &inspector);
        assert!(resumes.is_empty());
        assert!(resumes.snapshot().is_none());
    }

    #[test]
    fn rejects_wrong_uid_foreign_process_and_marks_chroot_and_sandbox_wrapped() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            20,
            10,
            1001,
            "s20",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            30,
            1,
            1000,
            "s30",
            "claude",
            &["claude"],
            Some(99),
            Some("/work"),
        );
        inspector.add(
            40,
            10,
            1000,
            "s40",
            "bwrap",
            &["bwrap", "claude"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            41,
            40,
            1000,
            "s41",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            50,
            10,
            1000,
            "s50",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );
        inspector.different_roots.push(50);

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let t = 1_800_000_000;

        resumes.apply_report(
            &report_argv(20, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        resumes.apply_report(
            &report_argv(30, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        assert!(resumes.is_empty());

        resumes.apply_report(
            &report_argv(41, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        resumes.apply_report(
            &report_argv(50, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        let snap = resumes.snapshot().unwrap();
        assert_eq!(snap.count(), 2);
        assert!(
            snap.iter()
                .all(|record| record.placement() == ProcessPlacement::Wrapped)
        );
    }

    #[test]
    fn root_process_and_resumed_shell_host_are_not_wrapped() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash", "--rcfile", "/tmp/rc", "-i"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            20,
            10,
            1000,
            "s20",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let t = 1_800_000_000;
        resumes.apply_report(
            &report_argv(20, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        assert_eq!(
            resumes.snapshot().unwrap().chosen().placement(),
            ProcessPlacement::ShellHosted
        );

        let mut root_resumes = ProgramResumes::new(token.clone(), 20, Some(7));
        root_resumes.apply_report(
            &report_argv(20, t, &["claude"], None, token.as_str()),
            now_wall,
            1000,
            1000,
            &inspector,
        );
        assert_eq!(
            root_resumes.snapshot().unwrap().chosen().placement(),
            ProcessPlacement::Root
        );
    }

    #[test]
    fn reparented_or_overdeep_chain_without_pty_root_is_marked_wrapped() {
        let token = ResumeToken::from_test("dummy-token-replace-me");
        let mut inspector = FakeInspector::default();
        inspector.add(
            10,
            1,
            1000,
            "s10",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            500,
            1,
            1000,
            "s500",
            "bash",
            &["/bin/bash"],
            Some(7),
            Some("/work"),
        );
        inspector.add(
            501,
            500,
            1000,
            "s501",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );

        inspector.add(
            600,
            10,
            1000,
            "s600",
            "bwrap",
            &["bwrap", "sh"],
            Some(7),
            Some("/work"),
        );
        let mut parent = 600;
        for step in 1..=70u32 {
            let pid = 600 + step;
            inspector.add(
                pid,
                parent,
                1000,
                &format!("s{pid}"),
                "sh",
                &["/bin/sh"],
                Some(7),
                Some("/work"),
            );
            parent = pid;
        }
        let deep_claude = 700;
        inspector.add(
            deep_claude,
            parent,
            1000,
            "s700",
            "claude",
            &["claude"],
            Some(7),
            Some("/work"),
        );

        let mut resumes = ProgramResumes::new(token.clone(), 10, Some(7));
        let now_wall = UNIX_EPOCH + Duration::from_hours(500_000);
        let t = 1_800_000_000;
        resumes.apply_reports(
            [
                report_argv(501, t, &["claude"], None, token.as_str()),
                report_argv(deep_claude, t, &["claude"], None, token.as_str()),
            ],
            now_wall,
            1000,
            1000,
            &inspector,
        );
        let snap = resumes.snapshot().unwrap();
        assert_eq!(snap.count(), 2);
        assert!(
            snap.iter()
                .all(|record| record.placement() == ProcessPlacement::Wrapped)
        );
        assert_eq!(resumes.version(), 1);
    }
}
