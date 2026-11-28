use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use lune_protocol::program_resume::{ARGV_BYTES_MAX, ARGV_ELEMENTS_MAX, ResumeArgv, ShellKind};
use rustix::fs::{FileType, Mode, OFlags};

use crate::asked::{AbsoluteCwd, SpawnSpec};

pub const EXEC_FLAG: &str = "--resume-exec";

const RESUME_SUBDIR: &str = "resume";

const LAUNCH_PREFIX: &str = "launch-";

const ARGV_FILE: &str = "argv";

const ARGV_FILE_BYTES_MAX: usize = ARGV_BYTES_MAX + ARGV_ELEMENTS_MAX + 64;

const RANDOM_NAME_BYTES: usize = 16;

const _: () = assert!(ARGV_FILE_BYTES_MAX > ARGV_BYTES_MAX);

pub struct Guard {
    dir: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub struct Prepared {
    pub spec: SpawnSpec,
    pub vars: Vec<(String, String)>,
    pub guard: Guard,
}

pub fn prepare(
    runtime_dir: Option<&Path>,
    cwd: &AbsoluteCwd,
    shell: &str,
    kind: ShellKind,
    resume_argv: &ResumeArgv,
) -> Result<Prepared, String> {
    let exe = env::current_exe()
        .map_err(|error| format!("lune-mux executable path is unknown: {error}"))?;
    let exe = exe
        .into_os_string()
        .into_string()
        .map_err(|_| "lune-mux executable path is not UTF-8".to_owned())?;
    let base = private_resume_base(runtime_dir)
        .map_err(|error| format!("resume staging directory failed: {error}"))?;
    let launch_dir = create_launch_dir(&base)
        .map_err(|error| format!("resume launch directory failed: {error}"))?;
    let guard = Guard {
        dir: launch_dir.clone(),
    };
    let argv_path = launch_dir.join(ARGV_FILE);
    write_argv_file(&argv_path, resume_argv)
        .map_err(|error| format!("resume argv file failed: {error}"))?;
    let argv_path_str = argv_path
        .to_str()
        .ok_or_else(|| "resume argv path is not UTF-8".to_owned())?;
    let launch_dir_str = launch_dir
        .to_str()
        .ok_or_else(|| "resume launch directory is not UTF-8".to_owned())?;
    let mut vars = Vec::new();
    let args = match kind {
        ShellKind::Bash => {
            let rc_path = launch_dir.join("rc");
            let rc_str = rc_path
                .to_str()
                .ok_or_else(|| "resume rc path is not UTF-8".to_owned())?;
            let script = bash_rc_script(&exe, argv_path_str);
            write_private_file(&rc_path, script.as_bytes())
                .map_err(|error| format!("bash resume rc failed: {error}"))?;
            vec!["--rcfile".to_owned(), rc_str.to_owned(), "-i".to_owned()]
        }
        ShellKind::Zsh => {
            let original_zdotdir = read_optional_env_utf8("ZDOTDIR")?;
            let zshenv = zsh_env_script(original_zdotdir.as_deref(), launch_dir_str);
            let zshrc = zsh_rc_script(&exe, argv_path_str);
            write_private_file(&launch_dir.join(".zshenv"), zshenv.as_bytes())
                .map_err(|error| format!("zsh resume .zshenv failed: {error}"))?;
            write_private_file(&launch_dir.join(".zshrc"), zshrc.as_bytes())
                .map_err(|error| format!("zsh resume .zshrc failed: {error}"))?;
            vars.push(("ZDOTDIR".to_owned(), launch_dir_str.to_owned()));
            vec!["-i".to_owned()]
        }
        ShellKind::Fish => {
            let init = fish_init_command(&exe, argv_path_str);
            vec!["-i".to_owned(), "-C".to_owned(), init]
        }
        ShellKind::Posix => {
            let original_env = read_optional_env_utf8("ENV")?;
            let env_path = launch_dir.join("env");
            let env_str = env_path
                .to_str()
                .ok_or_else(|| "resume env path is not UTF-8".to_owned())?;
            let script = posix_env_script(original_env.as_deref(), &exe, argv_path_str);
            write_private_file(&env_path, script.as_bytes())
                .map_err(|error| format!("posix resume env failed: {error}"))?;
            vars.push(("ENV".to_owned(), env_str.to_owned()));
            vec!["-i".to_owned()]
        }
        ShellKind::Nushell => {
            let command = nushell_execute_command(&exe, argv_path_str);
            vec!["-i".to_owned(), "-e".to_owned(), command]
        }
    };
    let spec = SpawnSpec {
        cwd: cwd.clone(),
        program: shell.to_owned(),
        args,
    };
    Ok(Prepared { spec, vars, guard })
}

fn read_optional_env_utf8(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

#[must_use]
pub fn run(argv_path: &Path) -> ExitCode {
    let argv = match consume_argv_file(argv_path) {
        Ok(argv) => argv,
        Err(why) => {
            eprintln!("lune-mux: resume failed: {why}");
            return ExitCode::FAILURE;
        }
    };
    let program = argv.program().to_owned();
    let error = Command::new(&program).args(argv.args()).exec();
    eprintln!("lune-mux: resume failed to start {program}: {error}");
    match error.kind() {
        ErrorKind::NotFound => ExitCode::from(127),
        ErrorKind::PermissionDenied => ExitCode::from(126),
        _ => ExitCode::FAILURE,
    }
}

pub fn consume_argv_file(path: &Path) -> Result<ResumeArgv, String> {
    if !path.is_absolute() {
        return Err("argv path is not absolute".to_owned());
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, Mode::empty())
        .map_err(|error| format!("cannot open argv file: {error}"))?;
    let stat =
        rustix::fs::fstat(&fd).map_err(|error| format!("cannot inspect argv file: {error}"))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err("argv path is not a regular file".to_owned());
    }
    let uid = rustix::process::getuid().as_raw();
    if stat.st_uid != uid {
        return Err("argv file is owned by another user".to_owned());
    }
    if stat.st_mode & 0o077 != 0 {
        return Err("argv file is accessible to other users".to_owned());
    }
    let limit = u64::try_from(ARGV_FILE_BYTES_MAX).expect("argv file limit fits u64");
    let mut raw = Vec::new();
    let file = File::from(fd);
    file.take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|error| format!("cannot read argv file: {error}"))?;
    cleanup_consumed(path).map_err(|error| format!("cannot remove argv file: {error}"))?;
    if raw.len() > ARGV_FILE_BYTES_MAX {
        return Err(format!("argv file exceeds {ARGV_FILE_BYTES_MAX} bytes"));
    }
    assert!(raw.len() <= ARGV_FILE_BYTES_MAX);
    decode_argv_bytes(&raw)
}

fn cleanup_consumed(argv_path: &Path) -> io::Result<()> {
    fs::remove_file(argv_path)?;
    let Some(parent) = argv_path.parent() else {
        return Ok(());
    };
    let Some(name) = parent.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if !name.starts_with(LAUNCH_PREFIX) {
        return Ok(());
    }
    for leaf in ["rc", "env", ".zshenv", ".zshrc"] {
        if let Err(error) = fs::remove_file(parent.join(leaf))
            && error.kind() != ErrorKind::NotFound
        {
            eprintln!("lune-mux: resume cleanup {leaf}: {error}");
        }
    }
    if let Err(error) = fs::remove_dir(parent)
        && error.kind() != ErrorKind::NotFound
    {
        eprintln!("lune-mux: resume cleanup {}: {error}", parent.display());
    }
    Ok(())
}

fn encode_argv_bytes(argv: &ResumeArgv) -> Vec<u8> {
    assert_ne!(argv.words().len(), 0);
    let mut out = Vec::new();
    for word in argv.words() {
        out.extend_from_slice(word.as_bytes());
        out.push(0);
    }
    assert_eq!(out.last(), Some(&0));
    out
}

fn decode_argv_bytes(raw: &[u8]) -> Result<ResumeArgv, String> {
    if raw.is_empty() || *raw.last().expect("non-empty slice has last byte") != 0 {
        return Err("argv file is truncated or malformed".to_owned());
    }
    let mut words = Vec::new();
    for slice in raw[..raw.len() - 1].split(|&byte| byte == 0) {
        if words.len() >= ARGV_ELEMENTS_MAX {
            return Err("argv file has too many arguments".to_owned());
        }
        let word = String::from_utf8(slice.to_vec())
            .map_err(|_| "argv element is not UTF-8".to_owned())?;
        words.push(word);
    }
    ResumeArgv::parse(words)
}

fn write_argv_file(path: &Path, argv: &ResumeArgv) -> io::Result<()> {
    let bytes = encode_argv_bytes(argv);
    assert!(bytes.len() <= ARGV_FILE_BYTES_MAX);
    write_private_file(path, &bytes)
}

fn write_private_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    file.flush()
}

fn private_resume_base(runtime_dir: Option<&Path>) -> io::Result<PathBuf> {
    let base = match runtime_dir {
        Some(dir) => dir.join(RESUME_SUBDIR),
        None => env::temp_dir().join(format!(
            "lune-mux-resume-{}",
            rustix::process::getuid().as_raw()
        )),
    };
    match DirBuilder::new().mode(0o700).create(&base) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(&base)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{} is not a private 0700 directory owned by this user",
                base.display()
            ),
        ));
    }
    Ok(base)
}

fn create_launch_dir(base: &Path) -> io::Result<PathBuf> {
    let mut bytes = [0u8; RANDOM_NAME_BYTES];
    let mut urandom = File::open("/dev/urandom")?;
    urandom.read_exact(&mut bytes)?;
    let mut suffix = String::with_capacity(RANDOM_NAME_BYTES * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(suffix, "{byte:02x}");
    }
    let dir = base.join(format!("{LAUNCH_PREFIX}{suffix}"));
    DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

fn quote_posix_single(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn quote_fish_single(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        match ch {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn quote_nushell_double(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn bash_rc_script(exe: &str, argv_path: &str) -> String {
    let quoted_exe = quote_posix_single(exe);
    let quoted_argv = quote_posix_single(argv_path);
    format!(
        "if [ -r /etc/bash.bashrc ]; then . /etc/bash.bashrc; elif [ -r /etc/bashrc ]; then . /etc/bashrc; fi\n\
         if [ -n \"${{HOME:-}}\" ] && [ -r \"${{HOME}}/.bashrc\" ]; then . \"${{HOME}}/.bashrc\"; fi\n\
         __lune_resume_once() {{\n\
           if [ \"${{__lune_resume_is_array:-0}}\" = \"1\" ]; then\n\
             PROMPT_COMMAND=(\"${{__lune_resume_saved_pc[@]}}\")\n\
           elif [ \"${{__lune_resume_had_pc:-0}}\" = \"1\" ]; then\n\
             PROMPT_COMMAND=\"${{__lune_resume_saved_pc}}\"\n\
           else\n\
             unset PROMPT_COMMAND\n\
           fi\n\
           unset __lune_resume_is_array __lune_resume_had_pc __lune_resume_saved_pc\n\
           unset -f __lune_resume_once\n\
           command {quoted_exe} {EXEC_FLAG} {quoted_argv}\n\
         }}\n\
         case \"$(declare -p PROMPT_COMMAND 2>/dev/null)\" in\n\
           'declare -a'*)\n\
             __lune_resume_is_array=1\n\
             __lune_resume_saved_pc=(\"${{PROMPT_COMMAND[@]}}\")\n\
             PROMPT_COMMAND+=(__lune_resume_once)\n\
             ;;\n\
           *)\n\
             __lune_resume_is_array=0\n\
             if [ \"${{PROMPT_COMMAND+set}}\" = \"set\" ]; then\n\
               __lune_resume_had_pc=1\n\
               __lune_resume_saved_pc=\"${{PROMPT_COMMAND}}\"\n\
               if [ -n \"${{PROMPT_COMMAND}}\" ]; then\n\
                 PROMPT_COMMAND=\"${{PROMPT_COMMAND}}\"$'\\n'\"__lune_resume_once\"\n\
               else\n\
                 PROMPT_COMMAND=\"__lune_resume_once\"\n\
               fi\n\
             else\n\
               __lune_resume_had_pc=0\n\
               PROMPT_COMMAND=\"__lune_resume_once\"\n\
             fi\n\
             ;;\n\
         esac\n"
    )
}

fn zsh_env_script(original_zdotdir: Option<&str>, launch_dir: &str) -> String {
    let quoted_launch_dir = quote_posix_single(launch_dir);
    let restore = match original_zdotdir {
        Some(dir) => format!("export ZDOTDIR={}", quote_posix_single(dir)),
        None => "unset ZDOTDIR".to_owned(),
    };
    format!(
        "{restore}\n\
         __lune_resume_user_zdotdir=\"${{ZDOTDIR:-${{HOME:-}}}}\"\n\
         if [ -n \"${{__lune_resume_user_zdotdir}}\" ] && [ -r \"${{__lune_resume_user_zdotdir}}/.zshenv\" ]; then\n\
           builtin source \"${{__lune_resume_user_zdotdir}}/.zshenv\"\n\
         fi\n\
         if [ \"${{ZDOTDIR+set}}\" = \"set\" ]; then\n\
           __lune_resume_had_zdotdir=1\n\
           __lune_resume_final_zdotdir=\"$ZDOTDIR\"\n\
         else\n\
           __lune_resume_had_zdotdir=0\n\
           __lune_resume_final_zdotdir=\"\"\n\
         fi\n\
         ZDOTDIR={quoted_launch_dir}\n"
    )
}

fn zsh_rc_script(exe: &str, argv_path: &str) -> String {
    let quoted_exe = quote_posix_single(exe);
    let quoted_argv = quote_posix_single(argv_path);
    format!(
        "if [ \"${{__lune_resume_had_zdotdir:-0}}\" = \"1\" ]; then\n\
           export ZDOTDIR=\"$__lune_resume_final_zdotdir\"\n\
         else\n\
           unset ZDOTDIR\n\
         fi\n\
         __lune_resume_user_zdotdir=\"${{ZDOTDIR:-${{HOME:-}}}}\"\n\
         unset __lune_resume_had_zdotdir __lune_resume_final_zdotdir\n\
         if [ -n \"${{__lune_resume_user_zdotdir}}\" ] && [ -r \"${{__lune_resume_user_zdotdir}}/.zshrc\" ]; then\n\
           builtin source \"${{__lune_resume_user_zdotdir}}/.zshrc\"\n\
         fi\n\
         unset __lune_resume_user_zdotdir\n\
         __lune_resume_once() {{\n\
           precmd_functions=(${{precmd_functions:#__lune_resume_once}})\n\
           unfunction __lune_resume_once 2>/dev/null\n\
           command {quoted_exe} {EXEC_FLAG} {quoted_argv}\n\
         }}\n\
         typeset -ga precmd_functions\n\
         precmd_functions+=(__lune_resume_once)\n"
    )
}

fn fish_init_command(exe: &str, argv_path: &str) -> String {
    let quoted_exe = quote_fish_single(exe);
    let quoted_argv = quote_fish_single(argv_path);
    format!(
        "status job-control full 2>/dev/null; \
         command {quoted_exe} {EXEC_FLAG} {quoted_argv}; \
         status job-control interactive 2>/dev/null"
    )
}

fn posix_env_script(original_env: Option<&str>, exe: &str, argv_path: &str) -> String {
    let quoted_exe = quote_posix_single(exe);
    let quoted_argv = quote_posix_single(argv_path);
    let restore = match original_env {
        Some(path) => format!(
            "ENV={}\nexport ENV\nif [ -r \"$ENV\" ]; then . \"$ENV\"; fi",
            quote_posix_single(path)
        ),
        None => "unset ENV".to_owned(),
    };
    format!(
        "{restore}\n\
         set -m 2>/dev/null || true\n\
         command {quoted_exe} {EXEC_FLAG} {quoted_argv}\n"
    )
}

fn nushell_execute_command(exe: &str, argv_path: &str) -> String {
    let quoted_exe = quote_nushell_double(exe);
    let quoted_argv = quote_nushell_double(argv_path);
    format!("^{quoted_exe} {EXEC_FLAG} {quoted_argv}")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn argv_round_trips_hostile_strings_and_empty_args() {
        let words = vec![
            "printf".to_owned(),
            "$(reboot)".to_owned(),
            "a; b | c".to_owned(),
            String::new(),
            "it's \"quoted\" \\ $HOME".to_owned(),
        ];
        let argv = ResumeArgv::parse(words.clone()).unwrap();
        let encoded = encode_argv_bytes(&argv);
        let decoded = decode_argv_bytes(&encoded).unwrap();
        assert_eq!(decoded.words(), words.as_slice());
    }

    #[test]
    fn consume_argv_file_enforces_mode_unlinks_and_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("launch-test");
        fs::create_dir(&launch).unwrap();
        fs::set_permissions(&launch, fs::Permissions::from_mode(0o700)).unwrap();

        let argv_path = launch.join(ARGV_FILE);
        let argv = ResumeArgv::parse(vec![
            "nvim".to_owned(),
            "-S".to_owned(),
            "/tmp/s".to_owned(),
        ])
        .unwrap();
        write_argv_file(&argv_path, &argv).unwrap();
        write_private_file(&launch.join("rc"), b"echo hi").unwrap();

        let consumed = consume_argv_file(&argv_path).unwrap();
        assert_eq!(consumed, argv);
        assert!(!argv_path.exists());
        assert!(!launch.exists());
        assert!(consume_argv_file(&argv_path).is_err());
        assert_eq!(
            cleanup_consumed(&argv_path).unwrap_err().kind(),
            ErrorKind::NotFound
        );

        let open_path = dir.path().join("open-argv");
        write_argv_file(&open_path, &argv).unwrap();
        fs::set_permissions(&open_path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = consume_argv_file(&open_path).unwrap_err();
        assert!(error.contains("other users"), "{error}");

        let target = dir.path().join("target-argv");
        write_argv_file(&target, &argv).unwrap();
        let link = dir.path().join("link-argv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(consume_argv_file(&link).is_err());
    }

    #[test]
    fn shell_kind_matches_all_supported_shells_and_rejects_unknown() {
        for (path, expected) in [
            ("/bin/bash", Some(ShellKind::Bash)),
            ("/usr/bin/zsh", Some(ShellKind::Zsh)),
            ("/run/current-system/sw/bin/fish", Some(ShellKind::Fish)),
            ("/bin/sh", Some(ShellKind::Posix)),
            ("/bin/dash", Some(ShellKind::Posix)),
            ("/bin/ksh", Some(ShellKind::Posix)),
            ("/usr/bin/nu", Some(ShellKind::Nushell)),
            ("/usr/bin/nushell", Some(ShellKind::Nushell)),
            ("/bin/tcsh", None),
            ("/usr/bin/xonsh", None),
        ] {
            assert_eq!(ShellKind::of_path(path), expected, "{path}");
        }
    }

    #[test]
    fn shell_quoting_escapes_special_characters() {
        assert_eq!(quote_posix_single("/tmp/a'b\\c"), "'/tmp/a'\\''b\\c'");
        assert_eq!(quote_fish_single("/tmp/a'b\\c"), "'/tmp/a\\'b\\\\c'");
        assert_eq!(quote_nushell_double("/tmp/a\"b\\c"), "\"/tmp/a\\\"b\\\\c\"");
    }
}
