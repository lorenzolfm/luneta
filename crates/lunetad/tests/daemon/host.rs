//! A host of a test's own: the variables a daemon finds everything by, folders
//! for them to name, and the daemon itself.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub const LUNETAD: &str = env!("CARGO_BIN_EXE_lunetad");

pub const LUNETA: &str = env!("CARGO_BIN_EXE_luneta");

/// Where the socket is, under the root a variable names. The shell hook keeps
/// its commands beside it, in the same `luneta`.
pub const SOCKET: &str = "luneta/lunetad.sock";

/// The variables that the daemon and the CLI find their folders by: their
/// own, Claude Code's and zellij's. `None` leaves one unset, so that nothing a
/// test runs ever reads the folders of whoever runs the tests. `bin` comes
/// first on the `PATH`, for a test's own programs to stand in for those of
/// whoever runs the tests, and git reads no config of theirs either.
#[derive(Clone, Copy, Default)]
pub struct Vars<'a> {
    pub runtime: Option<&'a Path>,
    pub temp: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub zellij: Option<&'a Path>,
    pub bin: Option<&'a Path>,
}

impl<'a> Vars<'a> {
    pub fn runtime(root: &'a Path) -> Self {
        Vars { runtime: Some(root), ..Vars::default() }
    }

    pub fn command(self, program: &str) -> Command {
        let mut command = Command::new(program);
        let names = ["XDG_RUNTIME_DIR", "TMPDIR", "HOME", "CLAUDE_CONFIG_DIR", "ZELLIJ_SOCKET_DIR"];
        for name in names {
            command.env_remove(name);
        }
        let set = [
            ("XDG_RUNTIME_DIR", self.runtime),
            ("TMPDIR", self.temp),
            ("HOME", self.home),
            ("ZELLIJ_SOCKET_DIR", self.zellij),
        ];
        for (name, value) in set {
            if let Some(value) = value {
                command.env(name, value);
            }
        }
        if let Some(bin) = self.bin {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let paths = std::iter::once(bin.to_owned()).chain(std::env::split_paths(&path));
            command.env("PATH", std::env::join_paths(paths).unwrap());
        }
        command.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null");
        command
    }
}

/// A `zellij` of a test's own, in a folder to put first on the `PATH`, that
/// lists the panes of a session as a test has it: what it wrote with `list`.
/// It knows no other command, and of a session it was given no panes for, it
/// says there is no such session, as zellij does.
pub struct FakeZellij {
    bin: TempDir,
}

impl FakeZellij {
    pub fn new() -> Self {
        let zellij = FakeZellij { bin: tempfile::tempdir().unwrap() };
        fs::create_dir(zellij.panes()).unwrap();
        let script = format!(
            r#"#!/bin/sh
if [ "$1" != --session ] || [ "$3 $4 $5 $6" != "action list-panes --all --json" ]; then
    echo "this zellij only lists panes, and was asked: $*" >&2
    exit 2
fi
if [ ! -f "{panes}/$2.json" ]; then
    echo "Session '$2' not found." >&2
    exit 1
fi
exec cat "{panes}/$2.json"
"#,
            panes = zellij.panes().display()
        );
        let path = zellij.bin().join("zellij");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        zellij
    }

    /// The folder to put first on the `PATH`.
    pub fn bin(&self) -> &Path {
        self.bin.path()
    }

    fn panes(&self) -> PathBuf {
        self.bin.path().join("panes")
    }

    /// Has zellij list `panes` for the session `session` from now on, as
    /// `list-panes --json` spells them. The file is written whole before
    /// zellij can read it.
    pub fn list(&self, session: &str, panes: &serde_json::Value) {
        let path = self.panes().join(format!("{session}.json"));
        let writing = path.with_extension("writing");
        fs::write(&writing, panes.to_string()).unwrap();
        fs::rename(writing, path).unwrap();
    }
}

/// A process of a test's, killed as it goes out of scope, as a crash would end
/// it: with no chance to clear up after itself.
pub struct Running {
    child: Child,
}

impl Running {
    pub fn spawn(command: &mut Command) -> Self {
        Running { child: command.spawn().unwrap() }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A daemon that said it listens.
pub struct Daemon {
    process: Running,
    /// What the daemon has said, a line at a time.
    said: Vec<String>,
    /// What it says next, which a thread of the test's reads as it comes, so
    /// that it always has somewhere to go.
    heard: Receiver<String>,
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /// All the daemon has said, once `wait` has passed.
    pub fn said_after(&mut self, wait: Duration) -> &[String] {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(line) = self.heard.recv_timeout(left) else {
                return &self.said;
            };
            self.said.push(line);
        }
    }
}

/// Starts a daemon, and waits for it to say that it listens. One that ends
/// first has refused to run, and gives how it ended and all it said.
pub fn start(vars: Vars) -> Result<Daemon, (ExitStatus, String)> {
    let mut process = Running::spawn(vars.command(LUNETAD).stderr(Stdio::piped()));
    let stderr = BufReader::new(process.child.stderr.take().unwrap());
    let (tell, heard) = mpsc::channel();
    thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if tell.send(line).is_err() {
                return;
            }
        }
    });
    let mut said = Vec::new();
    while let Ok(line) = heard.recv() {
        if line.starts_with("lunetad: listening on ") {
            return Ok(Daemon { process, said, heard });
        }
        said.push(line);
    }
    Err((process.child.wait().unwrap(), said.join("\n")))
}

/// What `luneta info` says, run with `vars`.
pub fn info(vars: Vars) -> String {
    let output = vars.command(LUNETA).arg("info").output().unwrap();
    let said = String::from_utf8(output.stdout).unwrap();
    let why = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "luneta info failed: {why}");
    said
}

/// What `check` finds, once it finds it: the daemon reads what changes in a
/// thread of its own, and a test sees the change some moments after making it.
pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "never came to be: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}
