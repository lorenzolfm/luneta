//! A host of a test's own: the variables a daemon finds everything by, folders
//! for them to name, and the daemon itself.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub const LUNETAD: &str = env!("CARGO_BIN_EXE_lunetad");

pub const LUNETA: &str = env!("CARGO_BIN_EXE_luneta");

/// Where the socket is, under the root a variable names. The shell hook keeps
/// its commands beside it, in the same `luneta`.
pub const SOCKET: &str = "luneta/lunetad.sock";

/// The variables that the daemon and the CLI find their folders by: their
/// own, Claude Code's and zellij's. `None` leaves one unset, so that nothing a
/// test runs ever reads the folders of whoever runs the tests.
#[derive(Clone, Copy, Default)]
pub struct Vars<'a> {
    pub runtime: Option<&'a Path>,
    pub temp: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub zellij: Option<&'a Path>,
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
        command
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
