use std::fmt::Write as _;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::{self, WatchRequest};
use lunetad::folder::{self, Folder};
use tempfile::TempDir;

pub const LUNETAD: &str = env!("CARGO_BIN_EXE_lunetad");

pub const LUNETA: &str = env!("CARGO_BIN_EXE_luneta");

pub const SOCKET: &str = "luneta/lunetad.sock";

const CACHE: &str = if cfg!(target_os = "macos") {
    "Library/Caches/org.Zellij-Contributors.Zellij"
} else {
    ".cache/zellij"
};

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
            "XDG_STATE_HOME",
            "TMPDIR",
            "HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
            "ZELLIJ_SOCKET_DIR",
        ];
        let panes = [
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "LUNETA_PANE",
            "LUNETA_OWN_SEAT",
            "LUNETA_OWN_PANE",
            "LUNETA_AWAY",
            "LUNETA_MUX_SESSION",
            "LUNETA_MUX_MADE",
            "LUNETA_MUX_PANE",
        ];
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
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "maintenance.auto")
            .env("GIT_CONFIG_VALUE_0", "false");
        command
    }
}

pub fn install(from: &Path, to: &Path) {
    let mut install = Command::new("install");
    let installed = install.args(["-m", "755"]).arg(from).arg(to).status().unwrap();
    assert!(installed.success(), "{} was not installed at {}", from.display(), to.display());
}

pub fn script(path: &Path, text: &str) {
    let draft = path.with_extension("draft");
    fs::write(&draft, format!("#!/bin/sh\n{text}")).unwrap();
    install(&draft, path);
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

pub struct FakeZellij {
    bin: TempDir,
}

impl FakeZellij {
    pub fn new() -> Self {
        let zellij = FakeZellij { bin: tempfile::tempdir().unwrap() };
        fs::create_dir(zellij.panes()).unwrap();
        let text = format!(
            r#"panes="${{0%/*}}/panes"
asked="$*"
case "$1" in
    --session=*) session="${{1#--session=}}"; shift ;;
    --session) session="$2"; shift 2 ;;
    *) session="$2" ;;
esac
if [ "$1" = delete-session ]; then
    for session do :; done
fi
socket="$ZELLIJ_SOCKET_DIR/contract_version_1/$session"
cache="$HOME/{CACHE}"
kept="$cache/contract_version_1/session_info/$session"
if [ "$1" = action ] && [ -f "$panes/$session.$2.silent" ]; then
    rm "$panes/$session.$2.silent"
    case "$2" in
        new-pane | new-tab) echo "${{ZELLIJ_PANE_ID:--}} $asked" >> "$panes/opened" ;;
    esac
    exit 0
fi
if [ "$1" = action ] && [ -f "$panes/$session.$2.once" ]; then
    once=$(cat "$panes/$session.$2.once")
    rm "$panes/$session.$2.once"
    eval "$once"
fi
case "$*" in
    "action list-panes --all --json") asking=panes ;;
    "action list-panes --json")
        asking=laid
        if [ -f "$panes/$session.unlaid" ]; then
            echo "zellij lists no panes of $session" >&2
            exit 1
        fi
        if [ -f "$panes/$session.laid.hung" ]; then
            exec sleep 5
        fi
        if [ -f "$panes/$session.listed.clients" ]; then
            mv "$panes/$session.listed.clients" "$panes/$session.clients"
        fi
        ;;
    "action list-clients")
        asking=clients
        echo "$session" >> "$panes/asked-clients"
        ;;
    "action close-pane --pane-id "* | "action close-tab-by-id "* | "delete-session --force "*)
        if [ -f "$panes/$session.refused" ]; then
            echo "zellij refused to close anything of $session" >&2
            exit 1
        fi
        if [ -f "$panes/$session.gone" ]; then
            echo "Session: \"$session\" not found." >&2
            exit 2
        fi
        echo "$asked" >> "$panes/closed"
        [ "$1" = delete-session ] && [ -n "$session" ] || exit 0
        echo "A session by the name \"$session\" exists and is active, but will be force killed and deleted."
        rm -f "$socket"
        rm -rf "$kept"
        if [ -f "$panes/$session.renews" ]; then
            touch "$ZELLIJ_SOCKET_DIR/contract_version_1/$(cat "$panes/$session.renews")"
        fi
        if [ -f "$panes/$session.rewrites" ]; then
            mkdir -p "$kept"
            : > "$kept/session-layout.kdl"
        fi
        if [ -f "$panes/$session.unkept" ]; then
            echo "Session: \"$session\" not found." >&2
            exit 2
        fi
        exit 0
        ;;
    "action switch-session "* | "action focus-pane-id "*)
        echo "$asked" >> "$panes/went"
        if [ -f "$panes/$session.focused.clients" ]; then
            mv "$panes/$session.focused.clients" "$panes/$session.clients"
        fi
        exit 0
        ;;
    "action stack-panes -- "*" terminal_9 terminal_4294967295")
        echo "$asked" >> "$panes/tucked"
        exit 0
        ;;
    "action stack-panes -- "*)
        echo "$asked" >> "$panes/turned"
        case "$*" in
            *" terminal_4294967295") focuses=no ;;
            *) focuses=yes ;;
        esac
        if [ "$focuses" = yes ] || [ ! -f "$panes/$session.tight" ]; then
            for next in "$panes/$session".stacks.*; do
                [ -f "$next" ] && mv "$next" "$panes/$session.json"
                break
            done
        fi
        if [ "$focuses" = yes ] && [ -f "$panes/$session.stacked.clients" ]; then
            mv "$panes/$session.stacked.clients" "$panes/$session.clients"
        fi
        exit 0
        ;;
    "action toggle-fullscreen "*)
        echo "$asked" >> "$panes/turned"
        exit 0
        ;;
    "action toggle-pane-embed-or-floating "*)
        echo "$asked" >> "$panes/turned"
        exit 0
        ;;
    "action new-pane "*)
        echo "${{ZELLIJ_PANE_ID:--}} $asked" >> "$panes/opened"
        echo terminal_9
        exit 0
        ;;
    "action new-tab "*)
        echo "${{ZELLIJ_PANE_ID:--}} $asked" >> "$panes/opened"
        if [ -f "$panes/$session.opened.json" ]; then
            mv "$panes/$session.opened.json" "$panes/$session.json"
        fi
        echo 3
        exit 0
        ;;
    "action dump-screen "*)
        exec cat "$panes/$session.dump"
        ;;
    "list-sessions --no-formatting")
        listed=
        for dir in "$cache"/contract_version_1/session_info/*/; do
            name=$(basename "$dir")
            [ -f "$dir/session-layout.kdl" ] || continue
            [ -e "$ZELLIJ_SOCKET_DIR/contract_version_1/$name" ] && continue
            echo "$name [Created 1h 2m 3s ago] (EXITED - attach to resurrect)"
            listed=1
        done
        if [ -z "$listed" ]; then
            echo "No active zellij sessions found." >&2
            exit 1
        fi
        exit 0
        ;;
    "action rename-session "*)
        echo "$asked" >> "$panes/renamed"
        exit 0
        ;;
    "attach "*)
        echo "$asked" >> "$panes/attached"
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
        echo "$asked" >> "$panes/closed"
        echo "Session: \"$session\" successfully deleted."
        exit 0
        ;;
    *)
        echo "this zellij knows no such command, and was asked: $*" >&2
        exit 2
        ;;
esac
if [ "$asking" = panes ]; then
    echo "$session" >> "$panes/asked"
fi
if [ -f "$panes/$session.hung" ]; then
    exec sleep 5
fi
if [ ! -f "$panes/$session.json" ]; then
    echo "Session '$session' not found." >&2
    exit 1
fi
if [ "$asking" != clients ]; then
    exec cat "$panes/$session.json"
fi
if [ -f "$panes/$session.clients" ]; then
    exec cat "$panes/$session.clients"
fi
echo "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND"
"#
        );
        shared_script(&zellij.bin().join("zellij"), &text);
        zellij
    }

    pub fn bin(&self) -> &Path {
        self.bin.path()
    }

    fn panes(&self) -> PathBuf {
        self.bin.path().join("panes")
    }

    pub fn asked(&self) -> Vec<String> {
        self.lines("asked")
    }

    pub fn asked_again(&self, session: &str) {
        let times = || self.asked().iter().filter(|asked| *asked == session).count();
        let before = times();
        let deadline = Instant::now() + Duration::from_secs(10);
        while times() == before {
            assert!(Instant::now() < deadline, "zellij was never asked about {session} again");
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn closed(&self) -> Vec<String> {
        self.lines("closed")
    }

    pub fn went(&self) -> Vec<String> {
        self.lines("went")
    }

    pub fn attached(&self) -> Vec<String> {
        self.lines("attached")
    }

    pub fn asked_clients(&self, session: &str) -> usize {
        self.lines("asked-clients").iter().filter(|asked| *asked == session).count()
    }

    pub fn opened(&self) -> Vec<String> {
        self.lines("opened")
    }

    pub fn turned(&self) -> Vec<String> {
        self.lines("turned")
    }

    pub fn tucked(&self) -> Vec<String> {
        self.lines("tucked")
    }

    pub fn renamed(&self) -> Vec<String> {
        self.lines("renamed")
    }

    pub fn dumps(&self, session: &str, text: &str) {
        self.write(&format!("{session}.dump"), text);
    }

    fn lines(&self, name: &str) -> Vec<String> {
        match fs::read_to_string(self.panes().join(name)) {
            Ok(said) => said.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("what zellij was asked does not read: {error}"),
        }
    }

    pub fn list(&self, session: &str, panes: &serde_json::Value) {
        self.write(&format!("{session}.json"), &panes.to_string());
    }

    pub fn opens_into(&self, session: &str, panes: &serde_json::Value) {
        self.write(&format!("{session}.opened.json"), &panes.to_string());
    }

    pub fn stacks_into(&self, session: &str, nth: u32, panes: &serde_json::Value) {
        self.write(&format!("{session}.stacks.{nth}"), &panes.to_string());
    }

    pub fn silences(&self, session: &str, action: &str) {
        self.write(&format!("{session}.{action}.silent"), "");
    }

    pub fn unreaches(&self, session: &str, action: &str, others: usize) {
        let gone = format!("Session '{session}' not found. The following sessions are active:");
        let ends = match others {
            0 => "echo 'There is no active session!' >&2; exit 1".to_owned(),
            1 => format!("echo \"{gone}\" >&2; echo other >&2; exit 1"),
            _ => format!("echo \"{gone}\" >&2; echo other; echo another; exit 0"),
        };
        self.write(&format!("{session}.{action}.once"), &ends);
    }

    pub fn answers_another(&self, session: &str, action: &str, said: &str) {
        self.write(&format!("{session}.{action}.once"), &format!("echo '{said}'; exit 0"));
    }

    pub fn clients(&self, session: &str, clients: &[(u32, &str)]) {
        self.write(&format!("{session}.clients"), &clients_said(clients));
    }

    pub fn says_of_clients(&self, session: &str, said: &str) {
        self.write(&format!("{session}.clients"), said);
    }

    pub fn lists_clients_into(&self, session: &str, clients: &[(u32, &str)]) {
        self.write(&format!("{session}.listed.clients"), &clients_said(clients));
    }

    pub fn stacks_clients_into(&self, session: &str, clients: &[(u32, &str)]) {
        self.write(&format!("{session}.stacked.clients"), &clients_said(clients));
    }

    pub fn leave_little_room(&self, session: &str) {
        self.write(&format!("{session}.tight"), "");
    }

    pub fn focuses_clients_into(&self, session: &str, clients: &[(u32, &str)]) {
        self.write(&format!("{session}.focused.clients"), &clients_said(clients));
    }

    pub fn unlays(&self, session: &str) {
        self.write(&format!("{session}.unlaid"), "");
    }

    pub fn hangs_laying(&self, session: &str) {
        self.write(&format!("{session}.laid.hung"), "");
    }

    pub fn lays(&self, session: &str) {
        fs::remove_file(self.panes().join(format!("{session}.unlaid"))).unwrap();
    }

    pub fn refuse(&self, session: &str) {
        self.write(&format!("{session}.refused"), "");
    }

    pub fn unkept(&self, session: &str) {
        self.write(&format!("{session}.unkept"), "");
    }

    pub fn rewrites(&self, session: &str) {
        self.write(&format!("{session}.rewrites"), "");
    }

    pub fn renews(&self, session: &str, other: &str) {
        self.write(&format!("{session}.renews"), other);
    }

    pub fn gone(&self, session: &str) {
        self.write(&format!("{session}.gone"), "");
    }

    pub fn hang(&self, session: &str) {
        self.write(&format!("{session}.hung"), "");
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.panes().join(name);
        let writing = path.with_extension("writing");
        fs::write(&writing, text).unwrap();
        fs::rename(writing, path).unwrap();
    }
}

fn clients_said(clients: &[(u32, &str)]) -> String {
    let mut said = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n".to_owned();
    for (id, pane) in clients {
        writeln!(said, "{id:<9} {pane:<14} {:<15}", "fish").unwrap();
    }
    said
}

pub struct FakeGh {
    pulls: PathBuf,
}

impl FakeGh {
    fn new(bin: &Path) -> Self {
        let gh = FakeGh { pulls: bin.join("pulls") };
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
if [ "$*" != "pr view --json number,state,isDraft,url,headRefName,headRefOid,author,headRepository,headRepositoryOwner,statusCheckRollup" ]; then
    echo "this gh only views a pull request, and was asked: $*" >&2
    exit 2
fi
if [ -f "$pulls/switch" ]; then
    git switch -q "$(cat "$pulls/switch")" || exit 1
    rm "$pulls/switch"
fi
branch=$(git symbolic-ref --short HEAD) || exit 1
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
        fs::write(self.pulls.join(format!("issue-{number}")), format!("{page}\n")).unwrap();
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
        fs::write(self.pulls.join(format!("checks-{number}")), said.to_string()).unwrap();
    }

    pub fn log(&self, job: u64, log: &str) {
        fs::write(self.pulls.join(format!("log-{job}")), log).unwrap();
    }

    pub fn old(&self) {
        fs::write(self.pulls.join("old"), "").unwrap();
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
        let git = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|it| it.is_file());
        let noted = NotedGit { notes: bin.join("statuses") };
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
        let ran = Command::new(&path).arg("--version").stdout(Stdio::null()).status().unwrap();
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

    pub fn note_git(&self) -> NotedGit {
        NotedGit::new(self.zellij.bin())
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

    fn metadata(&self) -> PathBuf {
        self.home.path().join(CACHE).join("contract_version_1/session_info")
    }

    pub fn hide_kept_sessions(&self) {
        fs::remove_dir_all(self.metadata()).unwrap();
        fs::write(self.metadata(), "").unwrap();
    }

    pub fn write_metadata(&self, session: &str, text: &str) {
        let dir = self.metadata().join(session);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session-metadata.kdl"), text).unwrap();
    }

    pub fn keep(&self, session: &str) {
        let dir = self.metadata().join(session);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session-layout.kdl"), "layout {\n}\n").unwrap();
    }

    pub fn folder(&self) -> PathBuf {
        self.runtime.path().join("luneta")
    }

    pub fn contract(&self) -> PathBuf {
        self.sockets.path().join("contract_version_1")
    }

    pub fn serve(&self, session: &str) -> UnixListener {
        UnixListener::bind(self.contract().join(session)).unwrap()
    }

    pub fn host(&self) -> api::Host {
        let folder = folder_in(self.runtime.path()).expect("no daemon has made its folder");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&folder).await.unwrap();
            let asked = WatchRequest { after: None, kept: None, shown: false };
            daemon.watch(asked).await.unwrap().into_inner()
        })
    }
}

pub fn folder_in(runtime: &Path) -> Option<Folder> {
    folder::open(&runtime.join("luneta")).unwrap()
}

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

    pub fn kill(&mut self) {
        self.child.kill().unwrap();
    }

    pub fn piped(&mut self) -> (ChildStdin, ChildStdout) {
        (self.child.stdin.take().unwrap(), self.child.stdout.take().unwrap())
    }

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

pub struct Daemon {
    process: Running,
    said: Vec<String>,
    heard: Receiver<String>,
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    pub fn ended_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        self.process.ended_within(limit)
    }

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

pub fn start(vars: Vars) -> Result<Daemon, (ExitStatus, String)> {
    start_as(vars.command(LUNETAD))
}

pub fn start_as(mut lunetad: Command) -> Result<Daemon, (ExitStatus, String)> {
    let mut process = Running::spawn(lunetad.stderr(Stdio::piped()));
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

pub fn answered(command: &mut Command) -> Output {
    let child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (tell, heard) = mpsc::channel();
    thread::spawn(move || tell.send(child.wait_with_output()));
    let output = heard.recv_timeout(Duration::from_secs(15));
    output.expect("it never answered, or what it left running holds what it writes").unwrap()
}

pub fn info(vars: Vars) -> String {
    let output = vars.command(LUNETA).arg("info").output().unwrap();
    let said = String::from_utf8(output.stdout).unwrap();
    let why = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "luneta info failed: {why}");
    said
}

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
