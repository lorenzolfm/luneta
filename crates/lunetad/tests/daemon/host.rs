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
/// own, Claude Code's and zellij's, its cache among them. `None` leaves one
/// unset, so that nothing a
/// test runs ever reads the folders of whoever runs the tests. `bin` comes
/// first on the `PATH`, for a test's own programs to stand in for those of
/// whoever runs the tests, and git reads no config of theirs either. Nothing
/// runs in a pane of theirs: zellij's names of one are left out, and the
/// one luneta's ask box gives its agent, with the turn it is at.
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
        let names = [
            "XDG_RUNTIME_DIR",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "HOME",
            "CLAUDE_CONFIG_DIR",
            "ZELLIJ_SOCKET_DIR",
        ];
        let panes = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "LUNETA_PANE"];
        let turns = ["LUNETA_TURN", "LUNETA_TURN_BEFORE"];
        for name in names.into_iter().chain(panes).chain(turns) {
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
/// and none until it has. Of a session it was given no panes for, it says
/// there is no such session, as zellij does. It notes each session the daemon
/// asks it for the panes of, for `asked`, and each pane, tab and session it is
/// asked to close, for `closed`, and closes nothing; in a session a test had
/// it `refuse`, it fails to, as zellij can. Of one a test had it `hang`, it
/// lists nothing, and answers nothing. It deletes a session as zellij does:
/// takes its socket away, as its server does once killed, and what it keeps
/// of it in the home, which a session a test had `rewrites` has back at
/// once; and one a test had `unkept` or `gone` as zellij deletes those. As it
/// deletes one a test had `renews`, another session is made by a name. It
/// lets go of what it keeps of one with no socket, as of one that ended. It
/// knows no other command.
pub struct FakeZellij {
    bin: TempDir,
}

impl FakeZellij {
    pub fn new() -> Self {
        let zellij = FakeZellij { bin: tempfile::tempdir().unwrap() };
        fs::create_dir(zellij.panes()).unwrap();
        let script = format!(
            r#"#!/bin/sh
asked="$*"
session="$2"
[ "$1" = --session ] && shift 2
[ "$1" = delete-session ] && session="${{3:-$2}}"
socket="$ZELLIJ_SOCKET_DIR/contract_version_1/$session"
case "$(uname)" in
    Darwin) cache="$HOME/Library/Caches/org.Zellij-Contributors.Zellij" ;;
    *) cache="$HOME/.cache/zellij" ;;
esac
kept="$cache/contract_version_1/session_info/$session"
case "$*" in
    "action list-panes --all --json") asking=panes ;;
    "action list-panes --json") asking=laid ;;
    "action list-clients") asking=clients ;;
    "action close-pane --pane-id "* | "action close-tab-by-id "* | "delete-session --force "*)
        if [ -f "{panes}/$session.refused" ]; then
            echo "zellij refused to close anything of $session" >&2
            exit 1
        fi
        if [ -f "{panes}/$session.gone" ]; then
            echo "Session: \"$session\" not found." >&2
            exit 2
        fi
        echo "$asked" >> "{panes}/closed"
        [ "$1" = delete-session ] && [ -n "$session" ] || exit 0
        echo "A session by the name \"$session\" exists and is active, but will be force killed and deleted."
        rm -f "$socket"
        rm -rf "$kept"
        if [ -f "{panes}/$session.renews" ]; then
            touch "$ZELLIJ_SOCKET_DIR/contract_version_1/$(cat "{panes}/$session.renews")"
        fi
        if [ -f "{panes}/$session.rewrites" ]; then
            mkdir -p "$kept"
            : > "$kept/session-layout.kdl"
        fi
        if [ -f "{panes}/$session.unkept" ]; then
            echo "Session: \"$session\" not found." >&2
            exit 2
        fi
        exit 0
        ;;
    "delete-session "*)
        if [ -S "$socket" ]; then
            echo "A session by the name \"$session\" exists and is active, use --force to delete it."
            exit 1
        fi
        if [ ! -d "$kept" ]; then
            echo "Session: \"$session\" not found." >&2
            exit 2
        fi
        rm -r "$kept"
        echo "$asked" >> "{panes}/closed"
        echo "Session: \"$session\" successfully deleted."
        exit 0
        ;;
    *)
        echo "this zellij only lists and closes, and was asked: $*" >&2
        exit 2
        ;;
esac
if [ "$asking" = panes ]; then
    echo "$session" >> "{panes}/asked"
fi
if [ -f "{panes}/$session.hung" ]; then
    exec sleep 5
fi
if [ ! -f "{panes}/$session.json" ]; then
    echo "Session '$session' not found." >&2
    exit 1
fi
if [ "$asking" != clients ]; then
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
        self.lines("asked")
    }

    /// Waits for zellij to be asked about the panes of `session` once more,
    /// and returns as soon as it is: the daemon asks again a sweep later at
    /// the soonest, with no word that anything changed.
    pub fn asked_again(&self, session: &str) {
        let times = || self.asked().iter().filter(|asked| *asked == session).count();
        let before = times();
        let deadline = Instant::now() + Duration::from_secs(10);
        while times() == before {
            assert!(Instant::now() < deadline, "zellij was never asked about {session} again");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// What zellij has been asked to close, each as its arguments were, in
    /// the order it was asked.
    pub fn closed(&self) -> Vec<String> {
        self.lines("closed")
    }

    /// The lines of the file `name`, none while there is none.
    fn lines(&self, name: &str) -> Vec<String> {
        match fs::read_to_string(self.panes().join(name)) {
            Ok(said) => said.lines().map(str::to_owned).collect(),
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

    /// Has zellij fail from now on to close any pane, tab or session of the
    /// session `session`, and note none of them.
    pub fn refuse(&self, session: &str) {
        self.write(&format!("{session}.refused"), "");
    }

    /// Has zellij kill the session `session` from now on and keep nothing of
    /// it, as it does a session that ran for less than a second: it says it
    /// kills it, notes it, and fails to delete it.
    pub fn unkept(&self, session: &str) {
        self.write(&format!("{session}.unkept"), "");
    }

    /// Has zellij write what it keeps of the session `session` anew from now
    /// on as it deletes it, as the server's writer does where it ticks
    /// before the server has ended.
    pub fn rewrites(&self, session: &str) {
        self.write(&format!("{session}.rewrites"), "");
    }

    /// Has a session be made by the name `other` from now on as zellij
    /// deletes the session `session`: the socket of `other` tells of a start
    /// since, as the socket of one made by its name does.
    pub fn renews(&self, session: &str, other: &str) {
        self.write(&format!("{session}.renews"), other);
    }

    /// Has zellij find nothing of the session `session` from now on, as of
    /// one that ended and that it keeps nothing of: it fails to delete it, and
    /// notes nothing.
    pub fn gone(&self, session: &str) {
        self.write(&format!("{session}.gone"), "");
    }

    /// Has zellij answer nothing from now on of what it is asked to list of
    /// the session `session`, for longer than the daemon waits, as the
    /// server of one that hangs does.
    pub fn hang(&self, session: &str) {
        self.write(&format!("{session}.hung"), "");
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

/// The folders of a host of a test's own: the daemon's, zellij's sockets,
/// and a home with zellij's cache in it, with a zellij that lists the panes
/// the test gives it, and a gh that finds the pull requests it gives it.
pub struct Host {
    pub runtime: TempDir,
    pub sockets: TempDir,
    pub home: TempDir,
    pub zellij: FakeZellij,
    pub gh: FakeGh,
}

impl Host {
    pub fn new() -> Self {
        let zellij = FakeZellij::new();
        let host = Host {
            runtime: tempfile::tempdir().unwrap(),
            sockets: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            gh: FakeGh::new(zellij.bin()),
            zellij,
        };
        fs::create_dir_all(host.contract()).unwrap();
        fs::create_dir_all(host.metadata()).unwrap();
        host
    }

    pub fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            home: Some(self.home.path()),
            zellij: Some(self.sockets.path()),
            bin: Some(self.zellij.bin()),
            ..Vars::default()
        }
    }

    /// The folder zellij 0.45 writes what it keeps of each session in, in
    /// its cache in the home.
    fn metadata(&self) -> PathBuf {
        let cache = if cfg!(target_os = "macos") {
            "Library/Caches/org.Zellij-Contributors.Zellij"
        } else {
            ".cache/zellij"
        };
        self.home.path().join(cache).join("contract_version_1/session_info")
    }

    /// Has zellij write `text` as what it keeps of `session`, in place, as
    /// zellij does.
    pub fn write_metadata(&self, session: &str, text: &str) {
        let dir = self.metadata().join(session);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session-metadata.kdl"), text).unwrap();
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
