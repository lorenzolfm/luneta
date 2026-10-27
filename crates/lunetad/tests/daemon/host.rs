//! A host of a test's own: the variables a daemon finds everything by, folders
//! for them to name, and the daemon itself.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::{self, WatchRequest};
use lunetad::folder::{self, Folder};
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
/// It lists the session's clients too, as the test wrote them with `clients`,
/// and none until it has. It knows no other command, and of a session it was
/// given no panes for, it says there is no such session, as zellij does. It
/// notes each session it is asked for the panes of, for `asked`.
pub struct FakeZellij {
    bin: TempDir,
}

impl FakeZellij {
    pub fn new() -> Self {
        let zellij = FakeZellij { bin: tempfile::tempdir().unwrap() };
        fs::create_dir(zellij.panes()).unwrap();
        let script = format!(
            r#"#!/bin/sh
session="$2"
[ "$1" = --session ] && shift 2
case "$*" in
    "action list-panes --all --json") asking=panes ;;
    "action list-clients") asking=clients ;;
    *)
        echo "this zellij only lists panes and clients, and was asked: $*" >&2
        exit 2
        ;;
esac
if [ "$asking" = panes ]; then
    echo "$session" >> "{panes}/asked"
fi
if [ ! -f "{panes}/$session.json" ]; then
    echo "Session '$session' not found." >&2
    exit 1
fi
if [ "$asking" = panes ]; then
    exec cat "{panes}/$session.json"
fi
if [ -f "{panes}/$session.clients" ]; then
    exec cat "{panes}/$session.clients"
fi
echo "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND"
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

    /// The sessions zellij has been asked about, in the order it was asked.
    pub fn asked(&self) -> Vec<String> {
        match fs::read_to_string(self.panes().join("asked")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what zellij was asked does not read: {error}"),
        }
    }

    /// Has zellij list `panes` for the session `session` from now on, as
    /// `list-panes --json` spells them. The file is written whole before
    /// zellij can read it.
    pub fn list(&self, session: &str, panes: &serde_json::Value) {
        self.write(&format!("{session}.json"), &panes.to_string());
    }

    /// Has zellij list `clients` for the session `session` from now on, each
    /// an id and a pane as `list-clients` spells them, `terminal_3` or
    /// `plugin_2`, and padded as zellij 0.45.1 pads them.
    pub fn clients(&self, session: &str, clients: &[(u32, &str)]) {
        let mut said = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n".to_owned();
        for (id, pane) in clients {
            writeln!(said, "{id:<9} {pane:<14} {:<15}", "fish").unwrap();
        }
        self.write(&format!("{session}.clients"), &said);
    }

    /// Writes `text` to the file `name`, whole before zellij can read it.
    fn write(&self, name: &str, text: &str) {
        let path = self.panes().join(name);
        let writing = path.with_extension("writing");
        fs::write(&writing, text).unwrap();
        fs::rename(writing, path).unwrap();
    }
}

/// A `gh` of a test's own, beside its `zellij`, that finds the pull request
/// of the branch checked out where it runs as a test has it: what it wrote
/// with `open` for the branch's name, whatever the repository. Of a branch it
/// was given none for, it says there is none, as gh does. It knows no other
/// command. It notes the branch of each pull request it is asked for, for
/// `asked`. Given a branch with `switch`, it checks that out first, the next
/// time it is asked.
pub struct FakeGh {
    pulls: PathBuf,
}

impl FakeGh {
    /// Writes the `gh` in `bin`, where the test's `zellij` is.
    fn new(bin: &Path) -> Self {
        let gh = FakeGh { pulls: bin.join("pulls") };
        fs::create_dir(&gh.pulls).unwrap();
        let script = format!(
            r#"#!/bin/sh
if [ "$*" != "pr view --json number,state,isDraft" ]; then
    echo "this gh only views a pull request, and was asked: $*" >&2
    exit 2
fi
if [ -f "{pulls}/switch" ]; then
    git switch -q "$(cat "{pulls}/switch")" || exit 1
    rm "{pulls}/switch"
fi
branch=$(git symbolic-ref --short HEAD) || exit 1
echo "$branch" >> "{pulls}/asked"
if [ ! -f "{pulls}/$branch.json" ]; then
    echo "no pull requests found for branch \"$branch\"" >&2
    exit 1
fi
exec cat "{pulls}/$branch.json"
"#,
            pulls = gh.pulls.display()
        );
        let path = bin.join("gh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        gh
    }

    /// Has gh check out `branch` where it is asked next, before it reads what
    /// is checked out there: as one checks it out in a shell just before gh
    /// is asked, where git has yet to see it.
    pub fn switch(&self, branch: &str) {
        fs::write(self.pulls.join("switch"), branch).unwrap();
    }

    /// The branches gh has been asked about, in the order it was asked.
    pub fn asked(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("asked")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    /// Has gh find `said` for `branch` from now on, as `gh pr view --json`
    /// writes it. The file is written whole before gh can read it.
    pub fn open(&self, branch: &str, said: &serde_json::Value) {
        let path = self.pulls.join(format!("{branch}.json"));
        let writing = path.with_extension("writing");
        fs::write(&writing, said.to_string()).unwrap();
        fs::rename(writing, path).unwrap();
    }
}

/// The folders of a host of a test's own: the daemon's, and zellij's sockets,
/// with a zellij that lists the panes the test gives it, and a gh that finds
/// the pull requests it gives it.
pub struct Host {
    pub runtime: TempDir,
    pub sockets: TempDir,
    pub zellij: FakeZellij,
    pub gh: FakeGh,
}

impl Host {
    pub fn new() -> Self {
        let zellij = FakeZellij::new();
        let host = Host {
            runtime: tempfile::tempdir().unwrap(),
            sockets: tempfile::tempdir().unwrap(),
            gh: FakeGh::new(zellij.bin()),
            zellij,
        };
        fs::create_dir_all(host.contract()).unwrap();
        host
    }

    pub fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            zellij: Some(self.sockets.path()),
            bin: Some(self.zellij.bin()),
            ..Vars::default()
        }
    }

    /// The daemon's folder.
    pub fn folder(&self) -> PathBuf {
        self.runtime.path().join("luneta")
    }

    /// The folder zellij 0.45 keeps its sessions' sockets in.
    pub fn contract(&self) -> PathBuf {
        self.sockets.path().join("contract_version_1")
    }

    /// A session's server, by the socket it listens on.
    pub fn serve(&self, session: &str) -> UnixListener {
        UnixListener::bind(self.contract().join(session)).unwrap()
    }

    /// The host, as the daemon answers a first look.
    pub fn host(&self) -> api::Host {
        let folder = folder_in(self.runtime.path()).expect("no daemon has made its folder");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&folder).await.unwrap();
            daemon.watch(WatchRequest { after: None }).await.unwrap().into_inner()
        })
    }
}

/// The folder in `runtime` that a daemon reaches, if one has made it.
pub fn folder_in(runtime: &Path) -> Option<Folder> {
    folder::open(&runtime.join("luneta")).unwrap()
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

    /// How it ended, if it does within `limit`.
    pub fn ended_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(ended) = self.child.try_wait().unwrap() {
                return Some(ended);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }
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

    /// How it ended, if it does within `limit`.
    pub fn ended_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        self.process.ended_within(limit)
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

/// How `command` ended and what it wrote, once it has ended and what it
/// writes to has closed, as zellij reads what a plugin's command says: a
/// process it left running that held either would keep it from ever
/// answering.
pub fn answered(command: &mut Command) -> Output {
    let child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (tell, heard) = mpsc::channel();
    thread::spawn(move || tell.send(child.wait_with_output()));
    let output = heard.recv_timeout(Duration::from_secs(15));
    output.expect("it never answered, or what it left running holds what it writes").unwrap()
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
