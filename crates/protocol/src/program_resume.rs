use std::collections::BTreeSet;
use std::fmt;
use std::hint::black_box;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mux::v1 as mux;
use crate::program_status::is_invisible_formatting;

pub const OSC_PREFIX: &[u8] = b"7577;";

pub const TOKEN_VAR: &str = "OSC7577_TOKEN";

pub const TOKEN_LEN_MIN: usize = 22;

pub const TOKEN_LEN_MAX: usize = 64;

pub const BODY_BYTES_MAX: usize = 16 * 1024;

pub const ARGV_ELEMENTS_MAX: usize = 256;

pub const ARGV_BYTES_MAX: usize = 16 * 1024;

pub const CWD_BYTES_MAX: usize = 4096;

pub const RECORDS_PER_TERMINAL_MAX: usize = 32;

pub const TIME_WINDOW_SECS: u64 = 60;

pub const PROCESS_NAME_BYTES_MAX: usize = 256;

pub const PROC_START_BYTES_MAX: usize = 128;

pub const SHELL_NAMES: [&str; 17] = [
    "sh", "bash", "zsh", "fish", "dash", "ash", "ksh", "mksh", "oksh", "pdksh", "yash", "tcsh",
    "csh", "nu", "nushell", "xonsh", "elvish",
];

const MAC_HEX_LEN: usize = 64;

const MAC_SUFFIX_PREFIX: &[u8] = b":mac=";

const KNOWN_KEYS: [&[u8]; 7] = [b"pid", b"time", b"mac", b"argv", b"cwd", b"clear", b"need"];

const _: () = assert!(BODY_BYTES_MAX >= 2048);
const _: () = assert!(ARGV_ELEMENTS_MAX >= 64);
const _: () = assert!(TOKEN_LEN_MIN == 22);
const _: () = assert!(TOKEN_LEN_MAX == 64);
const _: () = assert!(TIME_WINDOW_SECS == 60);
const _: () = assert!(RECORDS_PER_TERMINAL_MAX > 0);
const _: () = assert!(MAC_HEX_LEN == 64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Posix,
    Nushell,
}

impl ShellKind {
    #[must_use]
    pub fn of_path(shell_path: &str) -> Option<Self> {
        let name = Path::new(shell_path).file_name()?.to_str()?;
        let name = name.trim_start_matches('-');
        match name {
            "bash" => Some(ShellKind::Bash),
            "zsh" => Some(ShellKind::Zsh),
            "fish" => Some(ShellKind::Fish),
            "sh" | "dash" | "ash" | "ksh" | "mksh" | "oksh" | "pdksh" | "yash" => {
                Some(ShellKind::Posix)
            },
            "nu" | "nushell" => Some(ShellKind::Nushell),
            _ => None,
        }
    }
}

#[must_use]
pub fn is_shell_ancestor(
    name: &str,
    exe: Option<&str>,
    argv: Option<&[String]>,
    is_pty_root: bool,
) -> bool {
    let name_is_shell = is_shell_basename(name);
    let exe_is_shell = exe.is_some_and(is_shell_path);
    let argv0_is_shell = argv
        .and_then(|words| words.first())
        .is_some_and(|word| is_shell_path(word.trim_start_matches('-')));
    if !(name_is_shell || exe_is_shell || argv0_is_shell) {
        return false;
    }
    let Some(words) = argv else {
        return false;
    };
    if is_pty_root {
        return is_interactive_or_resume_shell_argv(words);
    }
    is_interactive_shell_argv(words)
}

fn is_shell_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_shell_basename)
}

fn shell_basename(name: &str) -> &str {
    let trimmed = name.trim_start_matches('-');
    trimmed
        .strip_prefix('.')
        .and_then(|inner| inner.strip_suffix("-wrapped"))
        .unwrap_or(trimmed)
}

fn is_shell_basename(name: &str) -> bool {
    SHELL_NAMES.contains(&shell_basename(name))
}

fn shell_name_from_argv(first: &str) -> &str {
    Path::new(first.trim_start_matches('-'))
        .file_name()
        .and_then(|name| name.to_str())
        .map_or("", shell_basename)
}

fn is_posix_option_shell(shell_name: &str) -> bool {
    matches!(
        shell_name,
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "mksh" | "oksh" | "pdksh" | "yash"
    )
}

fn is_interactive_or_resume_shell_argv(words: &[String]) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    let shell_name = shell_name_from_argv(first);
    let mut index = 0;
    while index < rest.len() {
        let word = rest[index].as_str();
        let takes_resume_arg = (shell_name == "bash" && matches!(word, "--rcfile" | "--init-file"))
            || (shell_name == "fish" && matches!(word, "-C" | "--init-command"))
            || (matches!(shell_name, "nu" | "nushell") && matches!(word, "-e" | "--execute"))
            || (is_posix_option_shell(shell_name) && word == "-o");
        if takes_resume_arg {
            if index + 1 >= rest.len() {
                return false;
            }
            index += 2;
            continue;
        }
        if !is_pure_shell_flag(word, shell_name) {
            return false;
        }
        index += 1;
    }
    assert_eq!(index, rest.len());
    true
}

fn is_interactive_shell_argv(words: &[String]) -> bool {
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    let shell_name = shell_name_from_argv(first);
    let mut index = 0;
    while index < rest.len() {
        let word = rest[index].as_str();
        if is_posix_option_shell(shell_name) && word == "-o" {
            if index + 1 >= rest.len() {
                return false;
            }
            index += 2;
            continue;
        }
        if !is_pure_shell_flag(word, shell_name) {
            return false;
        }
        index += 1;
    }
    assert_eq!(index, rest.len());
    true
}

fn is_pure_shell_flag(word: &str, shell_name: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return matches!(
            long,
            "login"
                | "interactive"
                | "norc"
                | "noprofile"
                | "no-config"
                | "no-config-file"
                | "no-rcs"
                | "no-globalrcs"
                | "no-global-rcs"
                | "posix"
                | "private"
                | "noediting"
                | "restricted"
                | "verbose"
        );
    }
    let Some(short) = word.strip_prefix('-') else {
        return false;
    };
    if short.is_empty() {
        return false;
    }
    match shell_name {
        "fish" => short.bytes().all(|b| matches!(b, b'i' | b'l' | b'N' | b'P')),
        "nu" | "nushell" => short.bytes().all(|b| matches!(b, b'i' | b'l' | b'n')),
        _ => short
            .bytes()
            .all(|b| matches!(b, b'i' | b'l' | b'm' | b'f' | b'd' | b'p' | b'r' | b's' | b'v' | b'x' | b'V')),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessPid(NonZeroU32);

impl ProcessPid {
    #[must_use]
    pub const fn new(pid: u32) -> Option<Self> {
        if pid == 0 || pid > i32::MAX.cast_unsigned() {
            return None;
        }
        match NonZeroU32::new(pid) {
            Some(pid) => Some(Self(pid)),
            None => None,
        }
    }

    pub fn parse(pid: u32) -> Result<Self, String> {
        Self::new(pid).ok_or_else(|| format!("resume record pid {pid} is invalid"))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for ProcessPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcStart(String);

impl ProcStart {
    pub fn parse(raw: String) -> Result<Self, String> {
        if raw.trim().is_empty() || raw.len() > PROC_START_BYTES_MAX {
            return Err("resume record proc_start is invalid".to_owned());
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessName(String);

impl ProcessName {
    pub fn parse(raw: String) -> Result<Self, String> {
        if raw.len() > PROCESS_NAME_BYTES_MAX {
            return Err("resume record process_name is too long".to_owned());
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn from_os(raw: &str) -> Self {
        if raw.len() <= PROCESS_NAME_BYTES_MAX {
            return Self(raw.to_owned());
        }
        let cut = raw.floor_char_boundary(PROCESS_NAME_BYTES_MAX);
        Self(raw[..cut].to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExe(String);

impl ProcessExe {
    pub fn parse(raw: String) -> Result<Self, String> {
        if raw.is_empty() || raw.len() > CWD_BYTES_MAX || raw.contains('\0') {
            return Err("resume record process_exe is invalid".to_owned());
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessArgv(Vec<String>);

impl ProcessArgv {
    pub fn parse(words: Vec<String>) -> Result<Self, String> {
        let mut total: usize = 0;
        for arg in &words {
            total = total.saturating_add(arg.len() + 1);
            if total > ARGV_BYTES_MAX {
                return Err("resume record process_argv exceeds byte limit".to_owned());
            }
        }
        Ok(Self(words))
    }

    #[must_use]
    pub fn bounded(words: Vec<String>) -> Self {
        let mut out = Vec::new();
        let mut bytes: usize = 0;
        for arg in words {
            let left = ARGV_BYTES_MAX - bytes;
            if left == 0 {
                break;
            }
            let need = arg.len().saturating_add(1);
            if need <= left {
                bytes += need;
                out.push(arg);
                continue;
            }
            let cut = arg.floor_char_boundary(left - 1);
            if cut > 0 {
                bytes += cut + 1;
                out.push(arg[..cut].to_owned());
            }
            break;
        }
        assert!(bytes <= ARGV_BYTES_MAX);
        Self(out)
    }

    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.0
    }

    #[must_use]
    pub fn into_words(self) -> Vec<String> {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessPlacement {
    Root,
    ShellHosted,
    Wrapped,
}

impl ProcessPlacement {
    #[must_use]
    pub const fn is_root(self) -> bool {
        matches!(self, ProcessPlacement::Root)
    }

    #[must_use]
    pub const fn is_wrapped(self) -> bool {
        matches!(self, ProcessPlacement::Wrapped)
    }

    #[must_use]
    pub const fn to_mux(self) -> mux::ProcessPlacement {
        match self {
            ProcessPlacement::Root => mux::ProcessPlacement::Root,
            ProcessPlacement::ShellHosted => mux::ProcessPlacement::ShellHosted,
            ProcessPlacement::Wrapped => mux::ProcessPlacement::Wrapped,
        }
    }

    pub fn of_mux(raw: i32) -> Result<Self, String> {
        match mux::ProcessPlacement::try_from(raw) {
            Ok(mux::ProcessPlacement::Root) => Ok(ProcessPlacement::Root),
            Ok(mux::ProcessPlacement::ShellHosted) => Ok(ProcessPlacement::ShellHosted),
            Ok(mux::ProcessPlacement::Wrapped) => Ok(ProcessPlacement::Wrapped),
            Ok(mux::ProcessPlacement::Unspecified) | Err(_) => {
                Err("resume record placement is invalid".to_owned())
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReportedAt {
    secs: u64,
    nanos: u32,
}

impl ReportedAt {
    const NANOS_PER_SEC: u32 = 1_000_000_000;

    pub fn from_parts(secs: u64, nanos: u32) -> Result<Self, String> {
        if nanos >= Self::NANOS_PER_SEC {
            return Err("resume record reported_at nanos is out of range".to_owned());
        }
        if i64::try_from(secs).is_err() {
            return Err("resume record reported_at seconds is out of range".to_owned());
        }
        if UNIX_EPOCH.checked_add(Duration::new(secs, nanos)).is_none() {
            return Err("resume record reported_at is out of range".to_owned());
        }
        Ok(Self { secs, nanos })
    }

    pub fn from_system_time(time: SystemTime) -> Result<Self, String> {
        let elapsed = time
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "resume record reported_at precedes unix epoch".to_owned())?;
        Self::from_parts(elapsed.as_secs(), elapsed.subsec_nanos())
    }

    pub fn of_timestamp(ts: prost_types::Timestamp) -> Result<Self, String> {
        let secs = u64::try_from(ts.seconds)
            .map_err(|_| "resume record reported_at is out of range".to_owned())?;
        let nanos = u32::try_from(ts.nanos)
            .map_err(|_| "resume record reported_at nanos is out of range".to_owned())?;
        Self::from_parts(secs, nanos)
    }

    #[must_use]
    pub fn to_timestamp(self) -> prost_types::Timestamp {
        let seconds = i64::try_from(self.secs).expect("ReportedAt secs fits i64");
        let nanos = i32::try_from(self.nanos).expect("ReportedAt nanos fits i32");
        prost_types::Timestamp { seconds, nanos }
    }

    #[must_use]
    pub fn system_time(self) -> SystemTime {
        UNIX_EPOCH
            .checked_add(Duration::new(self.secs, self.nanos))
            .expect("ReportedAt is within SystemTime bounds")
    }

    #[must_use]
    pub const fn secs(self) -> u64 {
        self.secs
    }

    #[must_use]
    pub const fn nanos(self) -> u32 {
        self.nanos
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeArgv(Vec<String>);

impl ResumeArgv {
    pub fn parse(words: Vec<String>) -> Result<Self, String> {
        if words.is_empty() {
            return Err("resume argv is empty".to_owned());
        }
        if words.len() > ARGV_ELEMENTS_MAX {
            return Err(format!(
                "resume argv has {} elements, over {ARGV_ELEMENTS_MAX}",
                words.len()
            ));
        }
        let mut total_bytes: usize = 0;
        for (index, word) in words.iter().enumerate() {
            if word.chars().any(char::is_control) {
                return Err(format!("resume argv[{index}] holds a control character"));
            }
            total_bytes = total_bytes.saturating_add(word.len());
            if total_bytes > ARGV_BYTES_MAX {
                return Err(format!("resume argv is over {ARGV_BYTES_MAX} bytes"));
            }
        }
        validate_program_name(&words[0])?;
        assert!(words.len() <= ARGV_ELEMENTS_MAX);
        Ok(Self(words))
    }

    #[must_use]
    pub fn program(&self) -> &str {
        assert_ne!(self.0.len(), 0);
        &self.0[0]
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        assert_ne!(self.0.len(), 0);
        &self.0[1..]
    }

    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.0
    }

    #[must_use]
    pub fn into_words(self) -> Vec<String> {
        self.0
    }
}

fn validate_program_name(program: &str) -> Result<(), String> {
    if program.is_empty() {
        return Err("resume argv[0] is empty".to_owned());
    }
    if program.starts_with('/') {
        return Ok(());
    }
    if program.contains('/') {
        return Err("resume argv[0] is a relative path".to_owned());
    }
    if program.starts_with('-') {
        return Err("resume argv[0] starts with '-'".to_owned());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeCwd(String);

impl ResumeCwd {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("resume cwd is empty".to_owned());
        }
        if raw.len() > CWD_BYTES_MAX {
            return Err(format!("resume cwd is {} bytes, over {CWD_BYTES_MAX}", raw.len()));
        }
        if !raw.starts_with('/') {
            return Err("resume cwd is not an absolute path".to_owned());
        }
        if raw.chars().any(char::is_control) {
            return Err("resume cwd holds a control character".to_owned());
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<ResumeCwd> for String {
    fn from(cwd: ResumeCwd) -> Self {
        cwd.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeCwdSource {
    Explicit(ResumeCwd),
    Inferred(ResumeCwd),
}

impl ResumeCwdSource {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            ResumeCwdSource::Explicit(cwd) | ResumeCwdSource::Inferred(cwd) => cwd.as_str(),
        }
    }

    #[must_use]
    pub const fn is_explicit(&self) -> bool {
        matches!(self, ResumeCwdSource::Explicit(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRecord {
    pid: ProcessPid,
    proc_start: ProcStart,
    uid: u32,
    process_name: ProcessName,
    process_exe: Option<ProcessExe>,
    process_argv: Option<ProcessArgv>,
    placement: ProcessPlacement,
    argv: ResumeArgv,
    cwd: Option<ResumeCwdSource>,
    reported_at: ReportedAt,
}

impl ResumeRecord {
    #[expect(
        clippy::too_many_arguments,
        reason = "every field of ResumeRecord is required and has a distinct validated type"
    )]
    #[must_use]
    pub fn new(
        pid: ProcessPid,
        proc_start: ProcStart,
        uid: u32,
        process_name: ProcessName,
        process_exe: Option<ProcessExe>,
        process_argv: Option<ProcessArgv>,
        placement: ProcessPlacement,
        argv: ResumeArgv,
        cwd: Option<ResumeCwdSource>,
        reported_at: ReportedAt,
    ) -> Self {
        Self {
            pid,
            proc_start,
            uid,
            process_name,
            process_exe,
            process_argv,
            placement,
            argv,
            cwd,
            reported_at,
        }
    }

    #[must_use]
    pub const fn pid(&self) -> ProcessPid {
        self.pid
    }

    #[must_use]
    pub fn proc_start(&self) -> &ProcStart {
        &self.proc_start
    }

    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub fn process_name(&self) -> &ProcessName {
        &self.process_name
    }

    #[must_use]
    pub fn process_exe(&self) -> Option<&ProcessExe> {
        self.process_exe.as_ref()
    }

    #[must_use]
    pub fn process_argv(&self) -> Option<&ProcessArgv> {
        self.process_argv.as_ref()
    }

    #[must_use]
    pub const fn placement(&self) -> ProcessPlacement {
        self.placement
    }

    #[must_use]
    pub fn argv(&self) -> &ResumeArgv {
        &self.argv
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&ResumeCwdSource> {
        self.cwd.as_ref()
    }

    #[must_use]
    pub const fn reported_at(&self) -> ReportedAt {
        self.reported_at
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: Option<ResumeCwdSource>) -> Self {
        self.cwd = cwd;
        self
    }

    #[must_use]
    pub fn to_mux(&self) -> mux::ResumeRecord {
        let cwd = self.cwd.as_ref().map(|source| match source {
            ResumeCwdSource::Explicit(dir) => {
                mux::resume_record::Cwd::ExplicitCwd(dir.as_str().to_owned())
            },
            ResumeCwdSource::Inferred(dir) => {
                mux::resume_record::Cwd::InferredCwd(dir.as_str().to_owned())
            },
        });
        mux::ResumeRecord {
            pid: self.pid.get(),
            proc_start: self.proc_start.as_str().to_owned(),
            uid: self.uid,
            process_name: self.process_name.as_str().to_owned(),
            process_exe: self.process_exe.as_ref().map(|exe| exe.as_str().to_owned()),
            process_argv: self
                .process_argv
                .as_ref()
                .map(|argv| mux::Program { argv: argv.words().to_vec() }),
            placement: self.placement.to_mux().into(),
            argv: self.argv.words().to_vec(),
            cwd,
            reported_at: Some(self.reported_at.to_timestamp()),
        }
    }

    pub fn of_mux(raw: mux::ResumeRecord) -> Result<Self, String> {
        let pid = ProcessPid::parse(raw.pid)?;
        let proc_start = ProcStart::parse(raw.proc_start)?;
        let process_name = ProcessName::parse(raw.process_name)?;
        let process_exe = raw.process_exe.map(ProcessExe::parse).transpose()?;
        let process_argv = raw
            .process_argv
            .map(|program| ProcessArgv::parse(program.argv))
            .transpose()?;
        let placement = ProcessPlacement::of_mux(raw.placement)?;
        let argv = ResumeArgv::parse(raw.argv)?;
        let cwd = match raw.cwd {
            Some(mux::resume_record::Cwd::ExplicitCwd(dir)) => {
                Some(ResumeCwdSource::Explicit(ResumeCwd::parse(&dir)?))
            },
            Some(mux::resume_record::Cwd::InferredCwd(dir)) => {
                Some(ResumeCwdSource::Inferred(ResumeCwd::parse(&dir)?))
            },
            None => None,
        };
        let reported_at = raw
            .reported_at
            .ok_or_else(|| "resume record reported_at is missing".to_owned())
            .and_then(ReportedAt::of_timestamp)?;
        Ok(ResumeRecord {
            pid,
            proc_start,
            uid: raw.uid,
            process_name,
            process_exe,
            process_argv,
            placement,
            argv,
            cwd,
            reported_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalResumes {
    chosen: ResumeRecord,
    others: Vec<ResumeRecord>,
}

impl TerminalResumes {
    pub fn new(chosen: ResumeRecord, others: Vec<ResumeRecord>) -> Result<Self, String> {
        if others.len() >= RECORDS_PER_TERMINAL_MAX {
            return Err(format!(
                "terminal resumes has {} records, over {RECORDS_PER_TERMINAL_MAX}",
                others.len() + 1
            ));
        }
        let mut seen = BTreeSet::new();
        seen.insert(chosen.pid());
        for record in &others {
            if !seen.insert(record.pid()) {
                return Err(format!("terminal resumes repeats pid {}", record.pid()));
            }
        }
        assert!(others.len() < RECORDS_PER_TERMINAL_MAX);
        Ok(Self { chosen, others })
    }

    #[must_use]
    pub fn chosen(&self) -> &ResumeRecord {
        &self.chosen
    }

    #[must_use]
    pub fn others(&self) -> &[ResumeRecord] {
        &self.others
    }

    pub fn iter(&self) -> impl Iterator<Item = &ResumeRecord> {
        std::iter::once(&self.chosen).chain(self.others.iter())
    }

    #[must_use]
    pub fn count(&self) -> usize {
        1 + self.others.len()
    }

    #[must_use]
    pub fn to_mux(&self) -> mux::TerminalResumes {
        mux::TerminalResumes {
            chosen: Some(self.chosen.to_mux()),
            others: self.others.iter().map(ResumeRecord::to_mux).collect(),
        }
    }

    #[must_use]
    pub fn of_mux(
        raw: Option<mux::TerminalResumes>,
        terminal_id: u32,
    ) -> (Option<Self>, Vec<String>) {
        let Some(raw) = raw else {
            return (None, Vec::new());
        };
        let mut warnings = Vec::new();
        let Some(raw_chosen) = raw.chosen else {
            warnings.push(format!("terminal {terminal_id} sent resumes without a chosen record"));
            return (None, warnings);
        };
        let chosen = match ResumeRecord::of_mux(raw_chosen) {
            Ok(record) => record,
            Err(why) => {
                warnings.push(format!("terminal {terminal_id}'s chosen resume record: {why}"));
                return (None, warnings);
            },
        };
        let max_others = RECORDS_PER_TERMINAL_MAX - 1;
        if raw.others.len() > max_others {
            warnings.push(format!(
                "terminal {terminal_id} reported {} resume records, over {RECORDS_PER_TERMINAL_MAX}",
                raw.others.len() + 1
            ));
        }
        let mut seen_pids = BTreeSet::new();
        seen_pids.insert(chosen.pid());
        let mut others = Vec::with_capacity(raw.others.len().min(max_others));
        for item in raw.others.into_iter().take(max_others) {
            match ResumeRecord::of_mux(item) {
                Ok(record) => {
                    if seen_pids.insert(record.pid()) {
                        others.push(record);
                    } else {
                        warnings.push(format!(
                            "terminal {terminal_id} listed resume record for pid {} twice",
                            record.pid()
                        ));
                    }
                },
                Err(why) => {
                    warnings.push(format!("terminal {terminal_id}'s resume record: {why}"));
                },
            }
        }
        assert!(others.len() < RECORDS_PER_TERMINAL_MAX);
        (Some(Self { chosen, others }), warnings)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceptedReport {
    Set { pid: ProcessPid, time: u64, argv: ResumeArgv, cwd: Option<ResumeCwd> },
    Clear { pid: ProcessPid, time: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    MalformedPair,
    InvalidKey,
    InvalidValue,
    DuplicateKey,
    MissingPid,
    InvalidPid,
    ArgvClearConflict,
    InvalidClear,
    InvalidArgv,
    InvalidCwd,
    InvalidNeed,
    UnknownNeed,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RejectReason::MalformedPair => "malformed key=value pair",
            RejectReason::InvalidKey => "invalid key name",
            RejectReason::InvalidValue => "invalid value characters",
            RejectReason::DuplicateKey => "duplicate key",
            RejectReason::MissingPid => "missing pid key",
            RejectReason::InvalidPid => "invalid pid value",
            RejectReason::ArgvClearConflict => "report must have exactly one of argv or clear",
            RejectReason::InvalidClear => "clear value must be 1",
            RejectReason::InvalidArgv => "invalid argv value",
            RejectReason::InvalidCwd => "invalid cwd value",
            RejectReason::InvalidNeed => "invalid need value",
            RejectReason::UnknownNeed => "unsupported key in need",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verified {
    Ignore,
    Reject { owner: Option<ProcessPid>, reason: RejectReason },
    Accept(AcceptedReport),
}

#[must_use]
pub fn is_valid_token(token: &str) -> bool {
    (TOKEN_LEN_MIN..=TOKEN_LEN_MAX).contains(&token.len())
        && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn verify_and_parse(
    body: &[u8],
    token: &str,
    now_unix_secs: u64,
    last_accepted_time: impl FnOnce(ProcessPid) -> Option<u64>,
) -> Verified {
    assert!(is_valid_token(token));
    if body.is_empty() || body == b"?" || body.len() > BODY_BYTES_MAX {
        return Verified::Ignore;
    }
    let Some((prefix, mac_hex)) = split_trailing_mac(body) else {
        return Verified::Ignore;
    };
    assert!(prefix.len() <= BODY_BYTES_MAX);
    assert_eq!(mac_hex.len(), MAC_HEX_LEN);
    let expected = sha256_body_token_hex(prefix, token.as_bytes());
    if !constant_time_eq(mac_hex, &expected) {
        return Verified::Ignore;
    }
    let Some(time) = precheck_time(prefix, now_unix_secs) else {
        return Verified::Ignore;
    };
    let owner = extract_single_owner_pid(prefix);
    if let Some(pid) = owner
        && let Some(last) = last_accepted_time(pid)
        && time < last
    {
        return Verified::Ignore;
    }
    match parse_verified_pairs(prefix, time) {
        Ok(accepted) => Verified::Accept(accepted),
        Err(reason) => Verified::Reject { owner, reason },
    }
}

fn split_trailing_mac(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let suffix_len = MAC_SUFFIX_PREFIX.len() + MAC_HEX_LEN;
    if body.len() <= suffix_len {
        return None;
    }
    let split_at = body.len() - suffix_len;
    let prefix = &body[..split_at];
    let rest = &body[split_at..];
    let mac_hex = rest.strip_prefix(MAC_SUFFIX_PREFIX)?;
    if mac_hex.len() != MAC_HEX_LEN || !mac_hex.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    Some((prefix, mac_hex))
}

fn precheck_time(prefix: &[u8], now_unix_secs: u64) -> Option<u64> {
    assert!(prefix.len() <= BODY_BYTES_MAX);
    let mut found: Option<u64> = None;
    for pair in prefix.split(|&b| b == b':') {
        if let Some(raw_time) = pair.strip_prefix(b"time=") {
            let parsed = parse_positive_u64(raw_time)?;
            if parsed.abs_diff(now_unix_secs) > TIME_WINDOW_SECS {
                return None;
            }
            if found.is_none() {
                found = Some(parsed);
            }
        }
    }
    found
}

fn extract_single_owner_pid(prefix: &[u8]) -> Option<ProcessPid> {
    assert!(prefix.len() <= BODY_BYTES_MAX);
    let mut found: Option<ProcessPid> = None;
    let mut count: usize = 0;
    for pair in prefix.split(|&b| b == b':') {
        if let Some(raw_pid) = pair.strip_prefix(b"pid=") {
            count += 1;
            if count > 1 {
                return None;
            }
            found = parse_positive_pid(raw_pid);
        }
    }
    found
}

fn parse_verified_pairs(prefix: &[u8], time: u64) -> Result<AcceptedReport, RejectReason> {
    assert!(prefix.len() <= BODY_BYTES_MAX);
    if prefix.is_empty() {
        return Err(RejectReason::MalformedPair);
    }
    let mut seen_keys: BTreeSet<&[u8]> = BTreeSet::new();
    let mut pid: Option<ProcessPid> = None;
    let mut argv_raw: Option<&[u8]> = None;
    let mut cwd_raw: Option<&[u8]> = None;
    let mut clear_raw: Option<&[u8]> = None;
    let mut need_raw: Option<&[u8]> = None;

    for pair in prefix.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            return Err(RejectReason::MalformedPair);
        };
        let key = &pair[..eq];
        let value = &pair[eq + 1..];
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            return Err(RejectReason::InvalidKey);
        }
        if !value.iter().copied().all(is_value_byte) {
            return Err(RejectReason::InvalidValue);
        }
        if key == b"mac" || !seen_keys.insert(key) {
            return Err(RejectReason::DuplicateKey);
        }
        match key {
            b"pid" => {
                pid = Some(parse_positive_pid(value).ok_or(RejectReason::InvalidPid)?);
            },
            b"argv" => argv_raw = Some(value),
            b"cwd" => cwd_raw = Some(value),
            b"clear" => clear_raw = Some(value),
            b"need" => need_raw = Some(value),
            _ => {},
        }
    }

    if let Some(need) = need_raw {
        validate_need(need)?;
    }

    let pid = pid.ok_or(RejectReason::MissingPid)?;
    let cwd = match cwd_raw {
        Some(raw) => Some(decode_cwd(raw)?),
        None => None,
    };

    match (argv_raw, clear_raw) {
        (Some(raw_argv), None) => {
            let argv = decode_argv(raw_argv)?;
            Ok(AcceptedReport::Set { pid, time, argv, cwd })
        },
        (None, Some(raw_clear)) => {
            if raw_clear != b"1" {
                return Err(RejectReason::InvalidClear);
            }
            Ok(AcceptedReport::Clear { pid, time })
        },
        (Some(_), Some(_)) | (None, None) => Err(RejectReason::ArgvClearConflict),
    }
}

fn validate_need(need: &[u8]) -> Result<(), RejectReason> {
    if need.is_empty() {
        return Err(RejectReason::InvalidNeed);
    }
    for name in need.split(|&b| b == b',') {
        if name.is_empty() || !name.iter().all(u8::is_ascii_lowercase) {
            return Err(RejectReason::InvalidNeed);
        }
        if !KNOWN_KEYS.contains(&name) {
            return Err(RejectReason::UnknownNeed);
        }
    }
    Ok(())
}

fn decode_argv(raw: &[u8]) -> Result<ResumeArgv, RejectReason> {
    if raw.is_empty() {
        return Err(RejectReason::InvalidArgv);
    }
    let mut words = Vec::new();
    let mut total_bytes: usize = 0;
    for (index, element) in raw.split(|&b| b == b',').enumerate() {
        if words.len() >= ARGV_ELEMENTS_MAX {
            return Err(RejectReason::InvalidArgv);
        }
        if element.is_empty() {
            if index == 0 {
                return Err(RejectReason::InvalidArgv);
            }
            words.push(String::new());
            continue;
        }
        let decoded = decode_base64_standard(element).ok_or(RejectReason::InvalidArgv)?;
        let text = String::from_utf8(decoded).map_err(|_| RejectReason::InvalidArgv)?;
        if text.chars().any(char::is_control) {
            return Err(RejectReason::InvalidArgv);
        }
        total_bytes = total_bytes.saturating_add(text.len());
        if total_bytes > ARGV_BYTES_MAX {
            return Err(RejectReason::InvalidArgv);
        }
        words.push(text);
    }
    assert!(words.len() <= ARGV_ELEMENTS_MAX);
    ResumeArgv::parse(words).map_err(|_| RejectReason::InvalidArgv)
}

fn decode_cwd(raw: &[u8]) -> Result<ResumeCwd, RejectReason> {
    if raw.is_empty() {
        return Err(RejectReason::InvalidCwd);
    }
    let decoded = decode_base64_standard(raw).ok_or(RejectReason::InvalidCwd)?;
    let text = String::from_utf8(decoded).map_err(|_| RejectReason::InvalidCwd)?;
    ResumeCwd::parse(&text).map_err(|_| RejectReason::InvalidCwd)
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

fn parse_positive_u64(raw: &[u8]) -> Option<u64> {
    let first = *raw.first()?;
    if !matches!(first, b'1'..=b'9') || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(raw).ok()?.parse::<u64>().ok()
}

fn parse_positive_pid(raw: &[u8]) -> Option<ProcessPid> {
    let value = parse_positive_u64(raw)?;
    let pid = u32::try_from(value).ok()?;
    ProcessPid::new(pid)
}

#[must_use]
pub fn escape_for_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if is_invisible_formatting(ch) {
            let code = u32::from(ch);
            if code <= 0xFF {
                use fmt::Write as _;
                let _ = write!(out, "\\x{code:02x}");
            } else {
                use fmt::Write as _;
                let _ = write!(out, "\\u{{{code:04x}}}");
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff = black_box(diff | (x ^ y));
    }
    diff == 0
}

#[must_use]
pub fn decode_base64_standard(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() {
        return Some(Vec::new());
    }
    let has_padding = input.contains(&b'=');
    let pad_len = if has_padding {
        if !input.len().is_multiple_of(4) {
            return None;
        }
        let trailing_eq = input.iter().rev().take_while(|&&b| b == b'=').count();
        if trailing_eq == 0 || trailing_eq > 2 {
            return None;
        }
        trailing_eq
    } else {
        if input.len() % 4 == 1 {
            return None;
        }
        0
    };
    let data = &input[..input.len() - pad_len];
    if data.contains(&b'=') {
        return None;
    }
    let (groups, tail) = data.as_chunks::<4>();
    let out_len = groups.len() * 3
        + match tail.len() {
            0 => 0,
            2 => 1,
            3 => 2,
            _ => return None,
        };
    let mut out = Vec::with_capacity(out_len);
    for group in groups {
        let a = base64_value(group[0])?;
        let b = base64_value(group[1])?;
        let c = base64_value(group[2])?;
        let d = base64_value(group[3])?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
        out.push((c << 6) | d);
    }
    match tail.len() {
        0 => {},
        2 => {
            let a = base64_value(tail[0])?;
            let b = base64_value(tail[1])?;
            if (b & 0x0F) != 0 {
                return None;
            }
            out.push((a << 2) | (b >> 4));
        },
        3 => {
            let a = base64_value(tail[0])?;
            let b = base64_value(tail[1])?;
            let c = base64_value(tail[2])?;
            if (c & 0x03) != 0 {
                return None;
            }
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
        },
        _ => return None,
    }
    assert_eq!(out.len(), out_len);
    Some(out)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[must_use]
pub fn encode_base64_standard(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    encode_base64_with(input, TABLE, true)
}

#[must_use]
pub fn encode_base64_url_nopad(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    encode_base64_with(input, TABLE, false)
}

fn encode_base64_with(input: &[u8], table: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 { u32::from(chunk[1]) } else { 0 };
        let b2 = if chunk.len() > 2 { u32::from(chunk[2]) } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let index0 = usize::try_from((triple >> 18) & 0x3F).expect("6-bit index fits usize");
        let index1 = usize::try_from((triple >> 12) & 0x3F).expect("6-bit index fits usize");
        out.push(char::from(table[index0]));
        out.push(char::from(table[index1]));
        if chunk.len() > 1 {
            let index2 = usize::try_from((triple >> 6) & 0x3F).expect("6-bit index fits usize");
            out.push(char::from(table[index2]));
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            let index3 = usize::try_from(triple & 0x3F).expect("6-bit index fits usize");
            out.push(char::from(table[index3]));
        } else if pad {
            out.push('=');
        }
    }
    if pad {
        assert!(out.len().is_multiple_of(4));
    }
    out
}

#[must_use]
pub fn sha256_body_token_hex(body: &[u8], token: &[u8]) -> [u8; 64] {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hasher.update(b":");
    hasher.update(token);
    digest_to_hex(&hasher.finalize())
}

#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

#[must_use]
pub fn digest_to_hex(digest: &[u8; 32]) -> [u8; 64] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 64];
    for (index, &byte) in digest.iter().enumerate() {
        out[index * 2] = HEX[usize::from(byte >> 4)];
        out[index * 2 + 1] = HEX[usize::from(byte & 0x0F)];
    }
    out
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Sha256 {
    const INIT: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    fn new() -> Self {
        Self { state: Self::INIT, buffer: [0; 64], buffer_len: 0, total_len: 0 }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.total_len = self
            .total_len
            .checked_add(u64::try_from(input.len()).expect("slice len fits u64"))
            .expect("sha256 message length fits u64");
        if self.buffer_len > 0 {
            let take = (64 - self.buffer_len).min(input.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&input[..take]);
            self.buffer_len += take;
            input = &input[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
        while input.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&input[..64]);
            self.compress(&block);
            input = &input[64..];
        }
        if !input.is_empty() {
            self.buffer[..input.len()].copy_from_slice(input);
            self.buffer_len = input.len();
        }
        assert!(self.buffer_len < 64);
    }

    fn finalize(mut self) -> [u8; 32] {
        assert!(self.buffer_len < 64);
        let bit_len = self.total_len.checked_mul(8).expect("sha256 bit length fits u64");
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);
        let mut digest = [0u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0u32; 64];
        for (index, &chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            schedule[index] = u32::from_be_bytes(chunk);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut r0, mut r1, mut r2, mut r3, mut r4, mut r5, mut r6, mut r7] = self.state;
        for (&round_k, &word) in Self::K.iter().zip(&schedule) {
            let s1 = r4.rotate_right(6) ^ r4.rotate_right(11) ^ r4.rotate_right(25);
            let ch = (r4 & r5) ^ ((!r4) & r6);
            let temp1 = r7
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(round_k)
                .wrapping_add(word);
            let s0 = r0.rotate_right(2) ^ r0.rotate_right(13) ^ r0.rotate_right(22);
            let maj = (r0 & r1) ^ (r0 & r2) ^ (r1 & r2);
            let temp2 = s0.wrapping_add(maj);

            r7 = r6;
            r6 = r5;
            r5 = r4;
            r4 = r3.wrapping_add(temp1);
            r3 = r2;
            r2 = r1;
            r1 = r0;
            r0 = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(r0);
        self.state[1] = self.state[1].wrapping_add(r1);
        self.state[2] = self.state[2].wrapping_add(r2);
        self.state[3] = self.state[3].wrapping_add(r3);
        self.state[4] = self.state[4].wrapping_add(r4);
        self.state[5] = self.state[5].wrapping_add(r5);
        self.state[6] = self.state[6].wrapping_add(r6);
        self.state[7] = self.state[7].wrapping_add(r7);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_TOKEN: &str = "dummy-token-replace-me";
    const SPEC_TIME: u64 = 1_795_543_200;

    fn signed(prefix: &str, token: &str) -> Vec<u8> {
        let mac = sha256_body_token_hex(prefix.as_bytes(), token.as_bytes());
        let mac_str = std::str::from_utf8(&mac).unwrap();
        format!("{prefix}:mac={mac_str}").into_bytes()
    }

    #[test]
    fn sha256_matches_nist_vectors() {
        let empty = digest_to_hex(&sha256(b""));
        assert_eq!(
            std::str::from_utf8(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = digest_to_hex(&sha256(b"abc"));
        assert_eq!(
            std::str::from_utf8(&abc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let two_blocks = digest_to_hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
        ));
        assert_eq!(
            std::str::from_utf8(&two_blocks).unwrap(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    fn pid(raw: u32) -> ProcessPid {
        ProcessPid::new(raw).unwrap()
    }

    #[test]
    fn spec_rev_0_5_valid_test_vectors_pass() {
        let nvim = b"pid=74877:time=1795543200:argv=bnZpbQ==,LVM=,L2hvbWUvdXNlci8ubG9jYWwvc3RhdGUvbnZpbS9zZXNzaW9ucy84ZjJjMWQudmlt:mac=9dbb99bfb856749a87563839d7ce9062818d78cf25909db93f309e0cd3b1909b";
        assert_eq!(
            verify_and_parse(nvim, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Accept(AcceptedReport::Set {
                pid: pid(74877),
                time: SPEC_TIME,
                argv: ResumeArgv::parse(vec![
                    "nvim".to_owned(),
                    "-S".to_owned(),
                    "/home/user/.local/state/nvim/sessions/8f2c1d.vim".to_owned(),
                ])
                .unwrap(),
                cwd: None,
            })
        );

        let clear = b"pid=74877:time=1795543200:clear=1:mac=c30603369f98fef581a4098be889175b0c74f0641e49f8b25f4326cb38f97c93";
        assert_eq!(
            verify_and_parse(clear, SPEC_TOKEN, SPEC_TIME, |_| Some(SPEC_TIME)),
            Verified::Accept(AcceptedReport::Clear { pid: pid(74877), time: SPEC_TIME })
        );

        let lazygit = b"pid=74877:time=1795543200:argv=bGF6eWdpdA==:cwd=L2hvbWUvdXNlci9zcmMvcHJvamVjdA==:mac=35cb909805c380336efd3689a125acc774a9dec8422a1d21492ebec06ca758e1";
        assert_eq!(
            verify_and_parse(lazygit, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Accept(AcceptedReport::Set {
                pid: pid(74877),
                time: SPEC_TIME,
                argv: ResumeArgv::parse(vec!["lazygit".to_owned()]).unwrap(),
                cwd: Some(ResumeCwd::parse("/home/user/src/project").unwrap()),
            })
        );

        let wrap = b"pid=90412:time=1795543200:argv=cmVzdW1lLXdyYXA=,c3No,ZGV2Ym94:mac=e76149d0ba54a83570ffab0583867dacdfa23928ba9d18604f0badea88b62b65";
        assert_eq!(
            verify_and_parse(wrap, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Accept(AcceptedReport::Set {
                pid: pid(90412),
                time: SPEC_TIME,
                argv: ResumeArgv::parse(vec![
                    "resume-wrap".to_owned(),
                    "ssh".to_owned(),
                    "devbox".to_owned(),
                ])
                .unwrap(),
                cwd: None,
            })
        );
    }

    #[test]
    fn spec_rev_0_5_invalid_test_vectors_distinguish_ignore_and_reject() {
        let wrong_mac = format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:mac={}", "0".repeat(64));
        assert_eq!(verify_and_parse(wrong_mac.as_bytes(), SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let no_mac = format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==");
        assert_eq!(verify_and_parse(no_mac.as_bytes(), SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let body_before_mac = format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==");
        let mac = sha256_body_token_hex(body_before_mac.as_bytes(), SPEC_TOKEN.as_bytes());
        let mac_not_last = format!("mac={}:{body_before_mac}", std::str::from_utf8(&mac).unwrap());
        assert_eq!(verify_and_parse(mac_not_last.as_bytes(), SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let old_time = signed("pid=74877:time=1000000000:argv=bnZpbQ==", SPEC_TOKEN);
        assert_eq!(verify_and_parse(&old_time, SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let no_time = signed("pid=74877:argv=bnZpbQ==", SPEC_TOKEN);
        assert_eq!(verify_and_parse(&no_time, SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let rel_argv = signed(&format!("pid=74877:time={SPEC_TIME}:argv=Li9ydW4uc2g="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&rel_argv, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let both = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:clear=1"), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&both, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::ArgvClearConflict }
        );

        let rel_cwd = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:cwd=cHJvamVjdA=="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&rel_cwd, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidCwd }
        );

        let unknown_need = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:need=later"), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&unknown_need, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::UnknownNeed }
        );
    }

    #[test]
    fn parser_enforces_edge_cases_and_monotonic_time() {
        assert_eq!(verify_and_parse(b"?", SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let earlier = signed(&format!("pid=74877:time={}:argv=bnZpbQ==", SPEC_TIME - 1), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&earlier, SPEC_TOKEN, SPEC_TIME, |_| Some(SPEC_TIME)),
            Verified::Ignore
        );

        let edge_future = signed(&format!("pid=74877:time={}:argv=bnZpbQ==", SPEC_TIME + 60), SPEC_TOKEN);
        assert!(matches!(verify_and_parse(&edge_future, SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Accept(_)));
        let past_future = signed(&format!("pid=74877:time={}:argv=bnZpbQ==", SPEC_TIME + 61), SPEC_TOKEN);
        assert_eq!(verify_and_parse(&past_future, SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Ignore);

        let unpadded_empty_arg = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ,,LVM:future=ok"), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&unpadded_empty_arg, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Accept(AcceptedReport::Set {
                pid: pid(74877),
                time: SPEC_TIME,
                argv: ResumeArgv::parse(vec!["nvim".to_owned(), String::new(), "-S".to_owned()]).unwrap(),
                cwd: None,
            })
        );

        let empty_first = signed(&format!("pid=74877:time={SPEC_TIME}:argv=,bnZpbQ=="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&empty_first, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let bad_pad = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&bad_pad, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let dash_program = signed(&format!("pid=74877:time={SPEC_TIME}:argv=LXNo"), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&dash_program, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let control_in_arg = signed(
            &format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==,{}", encode_base64_standard(b"a\nb")),
            SPEC_TOKEN,
        );
        assert_eq!(
            verify_and_parse(&control_in_arg, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let dup_key = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:argv=bnZpbQ=="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&dup_key, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::DuplicateKey }
        );

        let huge_pid = signed(&format!("pid=999999999999999999999:time={SPEC_TIME}:argv=bnZpbQ=="), SPEC_TOKEN);
        assert_eq!(
            verify_and_parse(&huge_pid, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: None, reason: RejectReason::InvalidPid }
        );

        let many_args: Vec<String> = (0..=ARGV_ELEMENTS_MAX).map(|_| "YQ==".to_owned()).collect();
        let over_args = signed(
            &format!("pid=74877:time={SPEC_TIME}:argv={}", many_args.join(",")),
            SPEC_TOKEN,
        );
        assert_eq!(
            verify_and_parse(&over_args, SPEC_TOKEN, SPEC_TIME, |_| None),
            Verified::Reject { owner: Some(pid(74877)), reason: RejectReason::InvalidArgv }
        );

        let known_need = signed(&format!("pid=74877:time={SPEC_TIME}:argv=bnZpbQ==:need=pid,time,argv"), SPEC_TOKEN);
        assert!(matches!(verify_and_parse(&known_need, SPEC_TOKEN, SPEC_TIME, |_| None), Verified::Accept(_)));
    }

    #[test]
    fn validated_record_types_and_terminal_resumes_enforce_invariants() {
        assert!(ProcessPid::parse(0).is_err());
        assert!(ProcessPid::parse(u32::MAX).is_err());
        assert_eq!(ProcessPid::parse(42).unwrap().get(), 42);

        assert!(ProcStart::parse("   ".to_owned()).is_err());
        assert!(ProcStart::parse("a".repeat(PROC_START_BYTES_MAX + 1)).is_err());

        let long_name = "é".repeat(PROCESS_NAME_BYTES_MAX);
        let truncated = ProcessName::from_os(&long_name);
        assert!(truncated.as_str().len() <= PROCESS_NAME_BYTES_MAX);
        assert!(long_name.starts_with(truncated.as_str()));

        assert!(ProcessExe::parse(String::new()).is_err());
        assert!(ProcessExe::parse("/usr/bin/a\0b".to_owned()).is_err());

        let huge = "é".repeat(ARGV_BYTES_MAX);
        let bounded = ProcessArgv::bounded(vec!["abc".to_owned(), huge.clone(), "d".to_owned()]);
        assert_eq!(bounded.words().len(), 2);
        assert_eq!(bounded.words()[0], "abc");
        assert!(huge.starts_with(&bounded.words()[1]));
        assert!(ProcessArgv::parse(bounded.into_words()).is_ok());

        assert!(ProcessPlacement::of_mux(mux::ProcessPlacement::Unspecified.into()).is_err());
        assert_eq!(
            ProcessPlacement::of_mux(mux::ProcessPlacement::ShellHosted.into()).unwrap(),
            ProcessPlacement::ShellHosted
        );

        assert!(ReportedAt::from_parts(100, 1_000_000_000).is_err());
        assert!(ReportedAt::from_parts(u64::MAX, 0).is_err());
        let ts = ReportedAt::from_parts(1_800_000_000, 123_456_789).unwrap();
        assert_eq!(ReportedAt::of_timestamp(ts.to_timestamp()).unwrap(), ts);

        let mk = |p: u32| {
            ResumeRecord::new(
                pid(p),
                ProcStart::parse(format!("s{p}")).unwrap(),
                1000,
                ProcessName::from_os("claude"),
                Some(ProcessExe::parse("/usr/bin/claude".to_owned()).unwrap()),
                Some(ProcessArgv::bounded(vec!["claude".to_owned()])),
                ProcessPlacement::ShellHosted,
                ResumeArgv::parse(vec!["claude".to_owned()]).unwrap(),
                None,
                ts,
            )
        };
        let (none, warnings) = TerminalResumes::of_mux(
            Some(mux::TerminalResumes { chosen: None, others: vec![mk(20).to_mux()] }),
            7,
        );
        assert!(none.is_none());
        assert_eq!(warnings.len(), 1);

        let (deduped, warnings) = TerminalResumes::of_mux(
            Some(mux::TerminalResumes {
                chosen: Some(mk(20).to_mux()),
                others: vec![mk(20).to_mux(), mk(30).to_mux()],
            }),
            7,
        );
        let deduped = deduped.unwrap();
        assert_eq!(deduped.chosen().pid(), pid(20));
        assert_eq!(deduped.others().len(), 1);
        assert_eq!(deduped.others()[0].pid(), pid(30));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn escape_for_display_neutralizes_controls_and_bidi_overrides() {
        let hostile = "nvim \u{202e}evil\u{202c}\x1b[31m\n\u{200b}ok";
        let escaped = escape_for_display(hostile);
        assert_eq!(escaped, "nvim \\u{202e}evil\\u{202c}\\x1b[31m\\x0a\\u{200b}ok");
        assert!(!escaped.chars().any(is_invisible_formatting));
    }

    #[test]
    fn is_shell_ancestor_fails_closed_on_missing_argv_and_non_interactive_flags() {
        let own = |words: &[&str]| words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();

        assert!(!is_shell_ancestor("bash", Some("/bin/bash"), None, true));
        assert!(!is_shell_ancestor("bash", Some("/bin/bash"), None, false));

        for (name, exe, words, is_root) in [
            ("fish", "/usr/bin/fish", &["fish", "--command=claude"][..], false),
            ("fish", "/usr/bin/fish", &["fish", "--command=claude"][..], true),
            ("fish", "/usr/bin/fish", &["fish", "--init-command=claude"][..], false),
            ("fish", "/usr/bin/fish", &["fish", "--init-command=claude"][..], true),
            ("fish", "/usr/bin/fish", &["fish", "-Cpi"][..], false),
            ("fish", "/usr/bin/fish", &["fish", "-Cpi"][..], true),
            ("fish", "/usr/bin/fish", &["fish", "-i", "-C", "claude"][..], false),
            ("nu", "/usr/bin/nu", &["nu", "--commands=claude"][..], false),
            ("nu", "/usr/bin/nu", &["nu", "-epi"][..], false),
            ("bash", "/bin/bash", &["bash", "--rcfile", "/tmp/rc", "-i"][..], false),
            ("bash", "/bin/bash", &["bash", "-c", "claude"][..], true),
        ] {
            assert!(
                !is_shell_ancestor(name, Some(exe), Some(&own(words)), is_root),
                "{words:?} (root={is_root}) must be treated as wrapped"
            );
        }

        for (name, exe, words, is_root) in [
            ("bash", "/bin/bash", &["/bin/bash", "--rcfile", "/tmp/rc", "-i"][..], true),
            ("fish", "/usr/bin/fish", &["/usr/bin/fish", "-i", "-C", "status job-control full"][..], true),
            ("nu", "/usr/bin/nu", &["/usr/bin/nu", "-i", "-e", "^lune-mux"][..], true),
            ("bash", "/bin/bash", &["-bash", "-il"][..], false),
            ("fish", "/usr/bin/fish", &["fish", "--login"][..], false),
            (".bash-wrapped", "/nix/store/x/bin/.bash-wrapped", &[".bash-wrapped", "-i"][..], true),
        ] {
            assert!(
                is_shell_ancestor(name, Some(exe), Some(&own(words)), is_root),
                "{words:?} (root={is_root}) should be treated as shell"
            );
        }
    }
}
