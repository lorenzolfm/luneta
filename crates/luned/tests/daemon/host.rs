use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use lune_waits::{PATIENCE, Patience, Span};
use luned::api;
use luned::folder::{self, Folder};
use luned::{Answer, RoundTrip};
use tempfile::TempDir;

pub const LUNED: &str = env!("CARGO_BIN_EXE_luned");

pub static LUNE: LazyLock<String> = LazyLock::new(|| {
    let beside = Path::new(LUNED).with_file_name("lune");
    let built = beside.is_file();
    assert!(
        built,
        "no lune is built beside luned, at {}: test with -p lune too",
        beside.display()
    );
    beside
        .into_os_string()
        .into_string()
        .expect("cargo builds lune at a UTF-8 path")
});

pub const SOCKET: &str = "lune/luned.sock";

pub const PATIENT: Answer = Answer::of(PATIENCE);

pub const PATIENT_ROUND_TRIP: RoundTrip = RoundTrip::of(PATIENCE);

#[derive(Clone, Copy, Default)]
pub struct Vars<'a> {
    pub runtime: Option<&'a Path>,
    pub temp: Option<&'a Path>,
    pub home: Option<&'a Path>,
    pub bin: Option<&'a Path>,
}

impl<'a> Vars<'a> {
    pub fn runtime(root: &'a Path) -> Self {
        Vars {
            runtime: Some(root),
            ..Vars::default()
        }
    }

    pub fn command(self, program: &str) -> Command {
        let mut command = Command::new(program);
        let names = [
            "XDG_RUNTIME_DIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "TMPDIR",
            "HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
        ];
        let views = [
            "LUNE_MUX_SESSION",
            "LUNE_MUX_MADE",
            "LUNE_MUX_PANE",
            "LUNE_MUX_TERMINAL",
            "LUNE_MUX_TERMINAL_MADE",
            "OSC7577_TOKEN",
        ];
        let turns = ["LUNE_TURN", "LUNE_TURN_BEFORE"];
        for name in names.into_iter().chain(views).chain(turns) {
            command.env_remove(name);
        }
        let set = [
            ("XDG_RUNTIME_DIR", self.runtime),
            ("TMPDIR", self.temp),
            ("HOME", self.home),
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
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "maintenance.auto")
            .env("GIT_CONFIG_VALUE_0", "false");
        command
    }
}

pub fn install(from: &Path, to: &Path) {
    let mut install = Command::new("install");
    let installed = install
        .args(["-m", "755"])
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(
        installed.success(),
        "{} was not installed at {}",
        from.display(),
        to.display()
    );
}

pub fn script(path: &Path, text: &str) {
    let draft = path.with_extension("draft");
    fs::write(&draft, format!("#!/bin/sh\n{text}")).unwrap();
    let staged = path.with_extension("staged");
    install(&draft, &staged);
    fs::rename(&staged, path).unwrap();
}

pub fn shared_script(path: &Path, text: &str) {
    symlink(shared_copy(text), path).unwrap();
}

fn shared_copy(text: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let tools = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tools");
    let copy = tools.join(format!("{:016x}", hasher.finish()));
    if copy.exists() {
        return copy;
    }
    fs::create_dir_all(&tools).unwrap();
    let draft = tempfile::tempdir_in(&tools).unwrap();
    let program = draft.path().join("tool");
    script(&program, text);
    Command::new(&program).arg("--version").output().unwrap();
    fs::rename(&program, &copy).unwrap();
    copy
}

pub struct FakeGh {
    pulls: PathBuf,
}

impl FakeGh {
    fn new(bin: &Path) -> Self {
        let gh = FakeGh {
            pulls: bin.join("pulls"),
        };
        fs::create_dir(&gh.pulls).unwrap();
        let text = r#"pulls="${0%/*}/pulls"
if [ "$1 $2" = "label list" ] || [ "$1 $2 $3 $4" = "issue list --author @me" ]; then
    echo "$(pwd -P) $*" >> "$pulls/listed"
    if [ -f "$pulls/lists-fail" ]; then
        cat "$pulls/lists-fail" >&2
        exit 1
    fi
    [ "$1" = label ] && echo '[{"name":"bug"}]'
    [ "$1" = issue ] && echo '[{"title":"An older one","body":"It was older.","labels":[]}]'
    exit 0
fi
if [ "$1 $2 $3 $4" = "issue list --state open" ] || [ "$1 $2 $3 $4" = "pr list --state open" ]; then
    echo "$(pwd -P) $*" >> "$pulls/worked"
    [ -f "$pulls/work-slow" ] && sleep 1
    if [ -f "$pulls/work-fails" ]; then
        cat "$pulls/work-fails" >&2
        exit 1
    fi
    if [ "$1" = issue ] && [ -f "$pulls/issues-off" ]; then
        echo "the 'o/n' repository has disabled issues" >&2
        exit 1
    fi
    [ -f "$pulls/work-$1" ] && exec cat "$pulls/work-$1"
    echo '[]'
    exit 0
fi
if [ "$*" = "repo view --json hasIssuesEnabled" ]; then
    echo "$(pwd -P) $*" >> "$pulls/worked"
    if [ -f "$pulls/issues-off" ]; then
        echo '{"hasIssuesEnabled":false}'
        exit 0
    fi
    echo '{"hasIssuesEnabled":true}'
    exit 0
fi
if [ "$1 $2" = "issue create" ]; then
    { echo "$(pwd -P)"; echo "$*"; cat; } > "$pulls/created"
    [ -f "$pulls/create-says" ] && exec cat "$pulls/create-says"
    if [ -f "$pulls/create-fails" ]; then
        printf '\nCreating issue in o/n\n\n' >&2
        cat "$pulls/create-fails" >&2
        exit 1
    fi
    echo "https://github.com/o/n/issues/360"
    exit 0
fi
if [ "$1 $2" = "issue view" ]; then
    echo "$*" >> "$pulls/issues"
    [ -f "$pulls/issue-$3" ] && exec cat "$pulls/issue-$3"
    echo "GraphQL: Could not resolve to an issue or pull request with the number of $3." >&2
    exit 1
fi
if [ "$1 $2 $4" = "pr view --repo" ]; then
    echo "$*" >> "$pulls/viewed"
    [ -f "$pulls/pull-$3" ] && exec cat "$pulls/pull-$3"
    echo "GraphQL: Could not resolve to a PullRequest with the number of $3." >&2
    exit 1
fi
if [ "$1 $2 $4 $5" = "pr view --json state,headRefOid,headRepository,headRepositoryOwner,statusCheckRollup" ]; then
    echo "$*" >> "$pulls/checked"
    [ -f "$pulls/checks-${3##*/}" ] && exec cat "$pulls/checks-${3##*/}"
    echo "GraphQL: Could not resolve to a PullRequest with the number of ${3##*/}." >&2
    exit 1
fi
if [ "$1 $2" = "api --hostname" ] && case "$*" in */actions/jobs/*/logs) true;; *) false;; esac; then
    echo "$*" >> "$pulls/checked"
    if [ "$4" = --allow-escape-sequences ]; then
        if [ -f "$pulls/old" ]; then
            echo "unknown flag: --allow-escape-sequences" >&2
            exit 1
        fi
        shift
    fi
    job=${4#*/actions/jobs/}
    job=${job%/logs}
    [ -f "$pulls/log-$job" ] && exec cat "$pulls/log-$job"
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1
fi
if [ "$1" = api ]; then
    [ -f "$pulls/api" ] && exec cat "$pulls/api"
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1
fi
if [ "$*" = "repo view --json parent,url,defaultBranchRef" ]; then
    echo "$*" >> "$pulls/repositories"
    [ -f "$pulls/repository" ] && exec cat "$pulls/repository"
    echo '{"defaultBranchRef":{"name":"main"},"parent":null,"url":"https://github.com/owner/repo"}'
    exit 0
fi
if [ "$1 $2 $3" = "pr list --base" ]; then
    echo "${GH_REPO:-} $4" >> "$pulls/based"
    [ -f "$pulls/based-$4" ] && exec cat "$pulls/based-$4"
    echo '[]'
    exit 0
fi
if [ "$*" != "pr view --json state,isDraft,url,baseRefName,headRefName,headRefOid,author,headRepository,headRepositoryOwner,statusCheckRollup" ]; then
    echo "this gh only views a pull request, and was asked: $*" >&2
    exit 2
fi
if [ -f "$pulls/switch" ]; then
    git switch -q "$(cat "$pulls/switch")" || exit 1
    rm "$pulls/switch"
fi
branch=$(git symbolic-ref --quiet HEAD) || exit 1
branch=${branch#refs/heads/}
echo "${GH_REPO:+$GH_REPO }$branch" >> "$pulls/asked"
while [ -f "$pulls/$branch.hung" ]; do sleep 0.05; done
if [ -f "$pulls/$branch.fails" ]; then
    echo "HTTP 401: Requires authentication (https://api.github.com/graphql)" >&2
    exit 1
fi
said="$pulls/${GH_REPO:+$GH_REPO/}$branch.json"
if [ ! -f "$said" ]; then
    merge=$(git config "branch.$branch.merge")
    case "$merge" in
    refs/pull/*/head)
        number=${merge#refs/pull/}
        echo "GraphQL: Could not resolve to a PullRequest with the number of ${number%/head}." >&2
        exit 1
        ;;
    esac
    echo "no pull requests found for branch \"$branch\"" >&2
    exit 1
fi
exec cat "$said"
"#;
        shared_script(&bin.join("gh"), text);
        gh
    }

    pub fn switch(&self, branch: &str) {
        fs::write(self.pulls.join("switch"), branch).unwrap();
    }

    pub fn hang(&self, branch: &str) {
        fs::write(self.pulls.join(format!("{branch}.hung")), "").unwrap();
    }

    pub fn answer(&self, branch: &str) {
        fs::remove_file(self.pulls.join(format!("{branch}.hung"))).unwrap();
    }

    pub fn fail(&self, branch: &str) {
        fs::write(self.pulls.join(format!("{branch}.fails")), "").unwrap();
    }

    pub fn mend(&self, branch: &str) {
        fs::remove_file(self.pulls.join(format!("{branch}.fails"))).unwrap();
    }

    pub fn asked(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("asked")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    pub fn issue(&self, number: u32, page: &str) {
        fs::write(
            self.pulls.join(format!("issue-{number}")),
            format!("{page}\n"),
        )
        .unwrap();
    }

    pub fn issues(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("issues")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    pub fn pull(&self, number: u32, base: &str, head: &str) {
        let said = serde_json::json!({ "baseRefName": base, "headRefOid": head });
        fs::write(self.pulls.join(format!("pull-{number}")), said.to_string()).unwrap();
    }

    pub fn viewed(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("viewed")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    pub fn checks(&self, number: u32, state: &str, head: &str, rollup: &serde_json::Value) {
        let said = serde_json::json!({
            "state": state,
            "headRefOid": head,
            "statusCheckRollup": rollup,
        });
        fs::write(
            self.pulls.join(format!("checks-{number}")),
            said.to_string(),
        )
        .unwrap();
    }

    pub fn log(&self, job: u64, log: &str) {
        fs::write(self.pulls.join(format!("log-{job}")), log).unwrap();
    }

    pub fn old(&self) {
        fs::write(self.pulls.join("old"), "").unwrap();
    }

    pub fn work(&self, issues: &serde_json::Value, pulls: &serde_json::Value) {
        fs::write(self.pulls.join("work-issue"), issues.to_string()).unwrap();
        fs::write(self.pulls.join("work-pr"), pulls.to_string()).unwrap();
    }

    pub fn slow_work(&self) {
        fs::write(self.pulls.join("work-slow"), "").unwrap();
    }

    pub fn issues_off(&self, off: bool) {
        let file = self.pulls.join("issues-off");
        if off {
            fs::write(file, "").unwrap();
        } else {
            fs::remove_file(file).unwrap();
        }
    }

    pub fn refuse_work(&self, why: &str) {
        fs::write(self.pulls.join("work-fails"), format!("{why}\n")).unwrap();
    }

    pub fn worked(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("worked")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    pub fn refuse_lists(&self, why: &str) {
        fs::write(self.pulls.join("lists-fail"), format!("{why}\n")).unwrap();
    }

    pub fn listed(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("listed")) {
            Ok(listed) => listed.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh listed does not read: {error}"),
        }
    }

    pub fn refuse_issues(&self, why: &str) {
        fs::write(self.pulls.join("create-fails"), format!("{why}\n")).unwrap();
    }

    pub fn say_on_create(&self, said: &str) {
        fs::write(self.pulls.join("create-says"), said).unwrap();
    }

    pub fn created(&self) -> Option<Vec<String>> {
        match fs::read_to_string(self.pulls.join("created")) {
            Ok(created) => Some(created.lines().map(str::to_owned).collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("what gh filed does not read: {error}"),
        }
    }

    pub fn checked(&self) -> Vec<String> {
        match fs::read_to_string(self.pulls.join("checked")) {
            Ok(asked) => asked.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }

    pub fn api(&self, said: &str) {
        fs::write(self.pulls.join("api"), said).unwrap();
    }

    pub fn open(&self, branch: &str, said: &serde_json::Value) {
        let path = self.pulls.join(format!("{branch}.json"));
        let writing = path.with_extension("writing");
        fs::write(&writing, with_head(branch, said).to_string()).unwrap();
        fs::rename(writing, path).unwrap();
    }

    pub fn open_in(&self, repository: &str, branch: &str, said: &serde_json::Value) {
        let path = self.pulls.join(format!("{repository}/{branch}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let writing = path.with_extension("writing");
        fs::write(&writing, with_head(branch, said).to_string()).unwrap();
        fs::rename(writing, path).unwrap();
    }

    pub fn based_on(&self, branch: &str, number: u32) {
        let said = serde_json::json!([{ "number": number, "state": "OPEN" }]);
        fs::write(self.pulls.join(format!("based-{branch}")), said.to_string()).unwrap();
    }

    pub fn merged_into(&self, branch: &str, number: u32) {
        let said = serde_json::json!([{ "number": number, "state": "MERGED" }]);
        fs::write(self.pulls.join(format!("based-{branch}")), said.to_string()).unwrap();
    }

    pub fn fork_of(&self, owner: &str, name: &str) {
        self.fork_of_defaulting_to(owner, name, "main");
    }

    pub fn fork_of_defaulting_to(&self, owner: &str, name: &str, default: &str) {
        let said = serde_json::json!({
            "defaultBranchRef": { "name": default },
            "parent": { "name": name, "owner": { "login": owner } },
            "url": "https://github.com/owner/repo",
        });
        fs::write(self.pulls.join("repository"), said.to_string()).unwrap();
    }

    pub fn repositories(&self) -> usize {
        match fs::read_to_string(self.pulls.join("repositories")) {
            Ok(asked) => asked.lines().count(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("what gh was asked does not read: {error}"),
        }
    }
}

fn with_head(branch: &str, said: &serde_json::Value) -> serde_json::Value {
    let mut said = said.clone();
    let fields = said.as_object_mut().unwrap();
    fields.entry("baseRefName").or_insert("main".into());
    fields.entry("headRefName").or_insert(branch.into());
    fields.entry("headRefOid").or_insert("0".repeat(40).into());
    said
}

pub struct NotedGit {
    notes: PathBuf,
}

impl NotedGit {
    fn new(bin: &Path) -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let git = std::env::split_paths(&path)
            .map(|dir| dir.join("git"))
            .find(|it| it.is_file());
        let noted = NotedGit {
            notes: bin.join("statuses"),
        };
        let text = format!(
            r#"for arg; do
    if [ "$arg" = status ]; then
        echo "$*" >> "${{0%/*}}/statuses"
        break
    fi
done
exec "{git}" "$@"
"#,
            git = git.expect("git is on the PATH").display()
        );
        let path = bin.join("git");
        shared_script(&path, &text);
        let ran = Command::new(&path)
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(ran.success(), "the git of the test's own does not run");
        noted
    }

    pub fn statuses(&self) -> usize {
        match fs::read_to_string(&self.notes) {
            Ok(notes) => notes.lines().count(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("what git was asked does not read: {error}"),
        }
    }
}

pub struct Host {
    pub runtime: TempDir,
    pub home: TempDir,
    bin: TempDir,
    pub gh: FakeGh,
}

impl Host {
    pub fn new() -> Self {
        let bin = tempfile::tempdir().unwrap();
        let gh = FakeGh::new(bin.path());
        Host {
            runtime: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            bin,
            gh,
        }
    }

    pub fn bin(&self) -> &Path {
        self.bin.path()
    }

    pub fn note_git(&self) -> NotedGit {
        NotedGit::new(self.bin())
    }

    pub fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            home: Some(self.home.path()),
            bin: Some(self.bin()),
            ..Vars::default()
        }
    }

    pub fn folder(&self) -> PathBuf {
        self.runtime.path().join("lune")
    }

    pub fn host(&self) -> api::Host {
        let folder = folder_in(self.runtime.path()).expect("no daemon has made its folder");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            let asked = api::WatchChangesRequest {
                after: None,
                waiter: api::ClientWaiter::Client.into(),
            };
            daemon
                .watch_changes(asked)
                .await
                .unwrap()
                .into_inner()
                .read_host()
                .unwrap()
                .into_host()
        })
    }
}

pub fn folder_in(runtime: &Path) -> Option<Folder> {
    folder::open(&runtime.join("lune")).unwrap()
}

pub struct Running {
    child: Child,
}

impl Running {
    pub fn spawn(command: &mut Command) -> Self {
        Running {
            child: command.spawn().unwrap(),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn kill(&mut self) {
        self.child.kill().unwrap();
    }

    pub fn piped(&mut self) -> (ChildStdin, ChildStdout) {
        (
            self.child.stdin.take().unwrap(),
            self.child.stdout.take().unwrap(),
        )
    }

    pub fn ended(&mut self, what: &str) -> ExitStatus {
        lune_waits::ended(what, &mut self.child)
    }

    pub async fn ended_async(&mut self, what: &str) -> ExitStatus {
        lune_waits::ended_async(what, &mut self.child).await
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().unwrap()
    }

    pub fn runs_on(&mut self, what: &str) {
        lune_waits::runs_on(what, &mut self.child);
    }

    pub fn runs_through(&mut self, span: Span, what: &str) {
        lune_waits::through(span, what, || self.child.try_wait().unwrap());
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Daemon {
    process: Running,
    said: Vec<String>,
    heard: Receiver<String>,
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    pub fn ended(&mut self, what: &str) -> ExitStatus {
        self.process.ended(what)
    }

    pub async fn ended_async(&mut self, what: &str) -> ExitStatus {
        self.process.ended_async(what).await
    }

    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        self.process.exit_status()
    }

    pub fn runs_on(&mut self, what: &str) {
        self.process.runs_on(what);
    }

    pub fn runs_through(&mut self, span: Span, what: &str) {
        self.process.runs_through(span, what);
    }

    pub fn said(&mut self) -> &[String] {
        self.said.extend(self.heard.try_iter());
        &self.said
    }

    pub fn says(&mut self, what: &str, said: impl Fn(&str) -> bool) -> &[String] {
        self.says_at_least(what, 1, said)
    }

    pub fn says_at_least(
        &mut self,
        what: &str,
        count_min: usize,
        said: impl Fn(&str) -> bool,
    ) -> &[String] {
        assert!(count_min > 0);
        let patience = Patience::begun();
        while self.said.iter().filter(|line| said(line)).count() < count_min {
            let before = &self.said;
            let waited = fmt::from_fn(|f| write!(f, "{what}, after {before:?}"));
            let line = patience.heard(waited, &self.heard);
            self.said.push(line);
        }
        &self.said
    }
}

pub fn start(vars: Vars) -> Result<Daemon, (ExitStatus, String)> {
    start_as(vars.command(LUNED))
}

pub fn start_as(mut luned: Command) -> Result<Daemon, (ExitStatus, String)> {
    let mut process = Running::spawn(luned.stderr(Stdio::piped()));
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
        if line.starts_with("luned: listening on ") {
            return Ok(Daemon {
                process,
                said,
                heard,
            });
        }
        said.push(line);
    }
    Err((process.child.wait().unwrap(), said.join("\n")))
}

pub fn answered(command: &mut Command) -> Output {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tell, heard) = mpsc::channel();
    thread::spawn(move || tell.send(child.wait_with_output()));
    lune_waits::heard(
        "it answers, and leaves nothing running that holds what it writes",
        &heard,
    )
    .unwrap()
}

pub fn info(vars: Vars) -> String {
    let output = vars.command(&LUNE).arg("info").output().unwrap();
    let said = String::from_utf8(output.stdout).unwrap();
    let why = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "lune info failed: {why}");
    said
}

const SIDE_BY_SIDE_MAX: usize = 8;

const NEXTEST_OVERRIDES_MAX: usize = 64;

const NEXTEST_EXACT_FILTERS_MAX: usize = 64;

const DAEMON_TESTS_MAX: usize = 2048;

pub struct Slot(());

pub type Wait = (&'static str, fn(Slot));

#[derive(Clone, Copy)]
pub struct Waits<const N: usize>([Wait; N]);

impl<const N: usize> Waits<N> {
    #[must_use]
    pub const fn of(items: [Wait; N]) -> Self {
        assert!(N >= 2, "side_by_side overlaps at least two waits");
        assert!(
            N <= SIDE_BY_SIDE_MAX,
            "side_by_side runs too many waits in one test slot"
        );
        let mut index = 0;
        while index < N {
            let name = items[index].0.as_bytes();
            assert!(!name.is_empty(), "a side_by_side wait has a name");
            let mut has_non_space = false;
            let mut byte_index = 0;
            while byte_index < name.len() {
                let byte = name[byte_index];
                assert!(byte != b'\0', "a side_by_side wait name has no NUL byte");
                if !byte.is_ascii_whitespace() {
                    has_non_space = true;
                }
                byte_index += 1;
            }
            assert!(
                has_non_space,
                "a side_by_side wait name is not only whitespace"
            );
            let mut prior = 0;
            while prior < index {
                assert!(
                    !bytes_eq(name, items[prior].0.as_bytes()),
                    "duplicate side_by_side wait name"
                );
                prior += 1;
            }
            index += 1;
        }
        Self(items)
    }
}

const fn bytes_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

pub fn side_by_side<const N: usize>(waits: Waits<N>) {
    for (index, (name, wait)) in waits.0.iter().enumerate() {
        for (prior_name, prior_wait) in &waits.0[..index] {
            assert!(
                !std::ptr::fn_addr_eq(*wait, *prior_wait),
                "duplicate side_by_side wait function under {name:?} and {prior_name:?}"
            );
        }
    }
    let failed: Vec<&str> = thread::scope(|scope| {
        let running = waits.0.map(|(name, wait)| {
            let named = thread::Builder::new().name(name.to_owned());
            let handle = named
                .spawn_scoped(scope, move || wait(Slot(())))
                .unwrap_or_else(|error| panic!("could not spawn wait {name:?}: {error}"));
            (name, handle)
        });
        running
            .into_iter()
            .filter_map(|(name, handle)| handle.join().is_err().then_some(name))
            .collect()
    });
    assert_eq!(failed, [] as [&str; 0]);
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NextestConfig {
    profile: NextestProfiles,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NextestProfiles {
    default: DefaultProfile,
    quick: QuickProfile,
    ci: CiProfile,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultProfile {
    #[serde(default, rename = "fail-fast")]
    _fail_fast: Option<bool>,
    #[serde(default)]
    overrides: Vec<NextestOverride>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuickProfile {
    #[serde(rename = "default-filter")]
    default_filter: ExactExclusionFilter,
    #[serde(default)]
    overrides: Vec<NextestOverride>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CiProfile {
    #[serde(default, rename = "retries")]
    _retries: Option<u32>,
    #[serde(default, rename = "slow-timeout")]
    _slow_timeout: Option<toml::Value>,
    #[serde(default, rename = "failure-output")]
    _failure_output: Option<String>,
    #[serde(default, rename = "final-status-level")]
    _final_status_level: Option<String>,
    #[serde(default, rename = "junit")]
    _junit: Option<toml::Value>,
    #[serde(default)]
    overrides: Vec<NextestOverride>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NextestOverride {
    filter: ExactUnionFilter,
    #[serde(default)]
    priority: Option<NextestPriority>,
    #[serde(default, rename = "retries")]
    _retries: Option<u32>,
    #[serde(default, rename = "slow-timeout")]
    _slow_timeout: Option<toml::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NextestPriority(i8);

impl NextestPriority {
    const MIN: i8 = -100;
    const MAX: i8 = 100;

    const fn is_positive(self) -> bool {
        self.0 > 0
    }
}

const _: () = assert!(NextestPriority::MIN < 0 && NextestPriority::MAX > 0);

impl<'de> serde::Deserialize<'de> for NextestPriority {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i8::deserialize(deserializer)?;
        if !(Self::MIN..=Self::MAX).contains(&value) {
            return Err(serde::de::Error::custom(format!(
                "priority {value} outside {}..={}",
                Self::MIN,
                Self::MAX
            )));
        }
        Ok(Self(value))
    }
}

struct ExactUnionFilter(Vec<String>);

impl<'de> serde::Deserialize<'de> for ExactUnionFilter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        if tokens.is_empty() || tokens.len().is_multiple_of(2) {
            return Err(serde::de::Error::custom(
                "override filter must be `test(=...)` clauses joined by `or`",
            ));
        }
        let mut names = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            if !index.is_multiple_of(2) {
                if *token != "or" {
                    return Err(serde::de::Error::custom(format!(
                        "expected `or`, got {token:?}"
                    )));
                }
                continue;
            }
            let name = parse_exact_test_token(token).map_err(serde::de::Error::custom)?;
            if names.contains(&name) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate test(={name}) in one nextest filter"
                )));
            }
            names.push(name);
        }
        if names.len() > NEXTEST_EXACT_FILTERS_MAX {
            return Err(serde::de::Error::custom(format!(
                "nextest filter exceeds NEXTEST_EXACT_FILTERS_MAX ({NEXTEST_EXACT_FILTERS_MAX})"
            )));
        }
        Ok(Self(names))
    }
}

struct ExactExclusionFilter(Vec<String>);

impl<'de> serde::Deserialize<'de> for ExactExclusionFilter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        if tokens.len() < 2 || !(tokens.len() + 1).is_multiple_of(3) {
            return Err(serde::de::Error::custom(
                "default-filter must be `not test(=...)` clauses joined by `and`",
            ));
        }
        let mut names = Vec::new();
        for (index, chunk) in tokens.chunks(3).enumerate() {
            if chunk[0] != "not" {
                return Err(serde::de::Error::custom(format!(
                    "expected `not`, got {:?}",
                    chunk[0]
                )));
            }
            let name = parse_exact_test_token(chunk[1]).map_err(serde::de::Error::custom)?;
            if names.contains(&name) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate test(={name}) in one nextest filter"
                )));
            }
            names.push(name);
            let is_last = (index + 1) * 3 >= tokens.len();
            if !is_last && chunk[2] != "and" {
                return Err(serde::de::Error::custom(format!(
                    "expected `and`, got {:?}",
                    chunk[2]
                )));
            }
        }
        if names.len() > NEXTEST_EXACT_FILTERS_MAX {
            return Err(serde::de::Error::custom(format!(
                "nextest filter exceeds NEXTEST_EXACT_FILTERS_MAX ({NEXTEST_EXACT_FILTERS_MAX})"
            )));
        }
        Ok(Self(names))
    }
}

fn parse_exact_test_token(token: &str) -> Result<String, String> {
    let name = token
        .strip_prefix("test(=")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or_else(|| format!("expected exact test(=...) filter, got {token:?}"))?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'))
    {
        return Err(format!("invalid daemon test name {name:?}"));
    }
    Ok(name.to_owned())
}

fn effective_priority(test_name: &str, layers: &[&[NextestOverride]]) -> NextestPriority {
    for overrides in layers {
        for entry in *overrides {
            if entry.filter.0.iter().any(|name| name == test_name)
                && let Some(priority) = entry.priority
            {
                return priority;
            }
        }
    }
    NextestPriority(0)
}

fn listed_daemon_tests(extra_args: &[&str]) -> BTreeSet<String> {
    let exe = std::env::current_exe().expect("test binary knows its own path");
    let output = Command::new(&exe)
        .args(["--list", "--format=terse"])
        .args(extra_args)
        .output()
        .expect("test binary lists its tests");
    assert!(
        output.status.success(),
        "test binary --list failed: {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("test names are UTF-8");
    let mut tests = BTreeSet::new();
    for (line_index, line) in stdout.lines().enumerate() {
        assert!(
            line_index < DAEMON_TESTS_MAX,
            "daemon test listing exceeds DAEMON_TESTS_MAX ({DAEMON_TESTS_MAX})"
        );
        if let Some(name) = line.strip_suffix(": test") {
            assert!(tests.insert(name.to_owned()), "duplicate test name {name}");
        }
    }
    tests
}

fn active_daemon_tests() -> BTreeSet<String> {
    let all = listed_daemon_tests(&[]);
    let ignored = listed_daemon_tests(&["--ignored"]);
    assert!(
        ignored.is_subset(&all),
        "ignored daemon tests are a subset of all listed tests"
    );
    let active: BTreeSet<String> = all.difference(&ignored).cloned().collect();
    assert!(
        !active.is_empty(),
        "daemon test binary lists at least one active test"
    );
    active
}

#[test]
fn nextest_toml_names_only_existing_daemon_tests() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.config/nextest.toml");
    let text = fs::read_to_string(&path).expect("nextest.toml is in the source tree");
    let config: NextestConfig = toml::from_str(&text).expect("nextest.toml matches typed schema");

    let active = active_daemon_tests();
    let profile_overrides: [(&str, &[NextestOverride]); 3] = [
        ("default", &config.profile.default.overrides),
        ("quick", &config.profile.quick.overrides),
        ("ci", &config.profile.ci.overrides),
    ];
    for (profile_name, overrides) in profile_overrides {
        assert!(
            overrides.len() <= NEXTEST_OVERRIDES_MAX,
            "profile {profile_name} override count exceeds NEXTEST_OVERRIDES_MAX ({NEXTEST_OVERRIDES_MAX})"
        );
        for entry in overrides {
            for name in &entry.filter.0 {
                assert!(
                    active.contains(name),
                    "nextest.toml [profile.{profile_name}.overrides] names {name}, which is no active daemon test"
                );
            }
        }
    }

    let default_layers = [config.profile.default.overrides.as_slice()];
    let ci_layers = [
        config.profile.ci.overrides.as_slice(),
        config.profile.default.overrides.as_slice(),
    ];
    let prioritized_default: BTreeSet<&str> = active
        .iter()
        .filter(|name| effective_priority(name, &default_layers).is_positive())
        .map(String::as_str)
        .collect();
    let prioritized_ci: BTreeSet<&str> = active
        .iter()
        .filter(|name| effective_priority(name, &ci_layers).is_positive())
        .map(String::as_str)
        .collect();
    assert!(
        !prioritized_default.is_empty(),
        "nextest.toml assigns positive priority to at least one active daemon test"
    );
    assert_eq!(
        prioritized_default, prioritized_ci,
        "nextest.toml [profile.ci] overrides change which daemon tests get positive priority"
    );

    let quick_excluded = &config.profile.quick.default_filter.0;
    assert!(
        !quick_excluded.is_empty(),
        "nextest.toml [profile.quick] excludes at least one test"
    );
    for name in quick_excluded {
        assert!(
            active.contains(name),
            "nextest.toml [profile.quick].default-filter names {name}, which is no active daemon test"
        );
        assert!(
            prioritized_default.contains(name.as_str()),
            "nextest.toml [profile.quick] excludes {name}, which lacks positive priority in [profile.default.overrides]"
        );
    }
}

#[test]
fn nextest_schema_rejects_malformed_filters_priorities_and_shadowed_ci_threads() {
    let invalid_configs = [
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one) and test(=b::two)"
priority = 100
[profile.quick]
default-filter = "not test(=a::one)"
[profile.ci]
"#,
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(a::one)"
priority = 100
[profile.quick]
default-filter = "not test(=a::one)"
[profile.ci]
"#,
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one)"
priority = 101
[profile.quick]
default-filter = "not test(=a::one)"
[profile.ci]
"#,
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one)"
priority = 100
[profile.quick]
default-filter = "test(=a::one)"
[profile.ci]
"#,
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one)"
priority = 100
[profile.quick]
default-filter = "not test(=a::one) or not test(=b::two)"
[profile.ci]
"#,
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one)"
priority = 100
[profile.quick]
default-filter = "not test(=a::one)"
[profile.ci]
test-threads = 4
"#,
    ];
    for raw in invalid_configs {
        assert!(
            toml::from_str::<NextestConfig>(raw).is_err(),
            "expected rejection for:\n{raw}"
        );
    }

    let shadowed: NextestConfig = toml::from_str(
        r#"
[profile.default]
[[profile.default.overrides]]
filter = "test(=a::one)"
priority = 100
[profile.quick]
default-filter = "not test(=a::one)"
[profile.ci]
[[profile.ci.overrides]]
filter = "test(=a::one)"
priority = 0
"#,
    )
    .expect("syntactically valid nextest config");
    assert!(
        effective_priority("a::one", &[&shadowed.profile.default.overrides]).is_positive(),
        "default profile prioritizes a::one"
    );
    assert!(
        !effective_priority(
            "a::one",
            &[
                &shadowed.profile.ci.overrides,
                &shadowed.profile.default.overrides
            ]
        )
        .is_positive(),
        "ci override priority = 0 shadows default priority = 100"
    );
}
