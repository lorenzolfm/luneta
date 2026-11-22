use std::cmp;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use luneta_protocol::github::State;
use tonic::{Code, Status};

use crate::api::rebase_response::{self, Came as Said};
use crate::api::{On, RebaseRequest, RebaseResponse};
use crate::directory::Directory;
use crate::git::{self, Common, DefaultBranch};
use crate::github::{self, Pulled};
use crate::links::{self, sync};
use crate::program::{self, Ran};

const RUNNING_MAX: Duration = Duration::from_mins(20);

pub const REBASING: Duration = RUNNING_MAX.saturating_add(Duration::from_mins(1));

const GIT_SAYS: Duration = Duration::from_mins(1);

const FETCHING: Duration = Duration::from_mins(3);

const PICKING: Duration = Duration::from_mins(10);

const STOPPED_CODE: i32 = 1;

const UNVERIFIED_CODE: i32 = 1;

const CONFLICTED_NAMED_MAX: usize = 20;

const UNDER_WAY: [(&str, Doing); 5] = [
    ("rebase-merge", Doing::Rebase),
    ("rebase-apply", Doing::Rebase),
    ("MERGE_HEAD", Doing::Merge),
    ("CHERRY_PICK_HEAD", Doing::CherryPick),
    ("REVERT_HEAD", Doing::Revert),
];

const _: () = assert!(FETCHING.as_secs() + PICKING.as_secs() < RUNNING_MAX.as_secs());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rebased {
    pub branch: String,
    pub onto: String,
    pub came: Came,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Came {
    Already,
    Clean { ahead: u32 },
    Stopped(Stopped),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub at: Option<String>,
    pub conflicted: Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflicted {
    named: Vec<String>,
    count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Doing {
    Rebase,
    Merge,
    CherryPick,
    Revert,
}

#[derive(Debug, PartialEq, Eq)]
enum Picked {
    Ended,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unrebased {
    Refused(String),
    InDoubt(String),
}

struct Ready {
    top: String,
    branch: String,
    onto: Onto,
    before: String,
}

enum Onto {
    Default(DefaultBranch),
    Base { remote: String, branch: String },
}

pub fn rebase(dir: &Directory, expected: Option<&str>) -> Result<Rebased, Unrebased> {
    let until = Instant::now() + RUNNING_MAX;
    let Ready { top, branch, onto, before } =
        ready(dir, expected, until).map_err(Unrebased::Refused)?;
    let (onto, reference) = (onto.to_string(), onto.reference());
    let doubt = |why: String| in_doubt(&top, &why);
    let came = match picked(&top, &reference, &branch, until)? {
        Picked::Ended if tip(&top, &branch, until).map_err(doubt)? == before => Came::Already,
        Picked::Ended => {
            Came::Clean { ahead: ahead(&top, &reference, &branch, until).map_err(doubt)? }
        },
        Picked::Stopped => {
            let at = stopped_at(&top, until).map_err(doubt)?;
            Came::Stopped(Stopped { at, conflicted: conflicted(&top, until).map_err(doubt)? })
        },
    };
    Ok(Rebased { branch, onto, came })
}

fn ready(dir: &Directory, expected: Option<&str>, until: Instant) -> Result<Ready, String> {
    let top = says(dir.as_str(), &["rev-parse", "--show-toplevel"], until)?;
    if top.is_empty() {
        return Err(format!("{dir} is in no worktree of a repository"));
    }
    if let Some(doing) = under_way(&top, until)? {
        return Err(format!("{top} is in the middle of {doing}: finish it, or abort it, first"));
    }
    let Some(branch) = branch(&top, until)? else {
        return Err(format!("{top} is on no branch, and a rebase moves a branch"));
    };
    if let Some(expected) = expected
        && expected != branch
    {
        return Err(format!("{top} is on {branch} now, not {expected}"));
    }
    if !committed(&top, until)? {
        return Err(format!("{top} has changes not committed: commit or stash them first"));
    }
    let onto = onto(&top, &branch, until)?;
    if let Some((remote, branch)) = onto.fetched() {
        sync::fetch(&top, remote, branch, left(until, FETCHING)?)?;
    }
    let before = tip(&top, &branch, until)?;
    Ok(Ready { top, branch, onto, before })
}

fn onto(top: &str, branch: &str, until: Instant) -> Result<Onto, String> {
    if let Some(pulled) = pulled(top, branch, until)? {
        return based(top, branch, &pulled, until);
    }
    let default = git::default_branch(top, left(until, GIT_SAYS)?)
        .map_err(|why| format!("{top} has no default branch to rebase onto: {why}"))?;
    Ok(Onto::Default(default))
}

fn pulled(top: &str, branch: &str, until: Instant) -> Result<Option<Pulled>, String> {
    if !links::has_github_remote(top)? {
        return Ok(None);
    }
    let repository = match git::common_within(top, left(until, GIT_SAYS)?) {
        Common::Of(common) => common,
        Common::NoRepository => return Err(format!("{top} is in no repository")),
        Common::Unsaid(why) => return Err(format!("git did not say whose {top} is: {why}")),
    };
    let unasked = |why| format!("no telling whether {branch} has a pull request open: {why}");
    let pulled = github::Asker::default().pull_of(top, &repository, branch).map_err(unasked)?;
    let open = |pulled: &Pulled| matches!(pulled.pull_request.state, State::Open | State::Draft);
    Ok(pulled.filter(open))
}

fn based(top: &str, branch: &str, pulled: &Pulled, until: Instant) -> Result<Onto, String> {
    let (page, base) = (pulled.pull_request.page.url(), &pulled.base_name);
    if !named_branch(top, base, until)? {
        return Err(format!("{page}, the pull request of {branch}, is onto {base:?}, no branch"));
    }
    let repository = pulled.pull_request.page.repository();
    let [host, owner, name] = repository.split('/').collect::<Vec<_>>()[..] else {
        return Err(format!("{page}, the pull request of {branch}, is of no repository"));
    };
    let Some(remote) = links::remote_of_repository(top, (host, owner, name))? else {
        return Err(format!(
            "{page}, the pull request of {branch}, is onto {base} of {repository}, which no \
             remote of {top} fetches"
        ));
    };
    Ok(Onto::Base { remote, branch: base.clone() })
}

fn named_branch(top: &str, name: &str, until: Instant) -> Result<bool, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", top, "check-ref-format", &format!("refs/heads/{name}")]));
    let ran = program::run(&mut git, left(until, GIT_SAYS)?)
        .map_err(|error| format!("git check-ref-format could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, .. } => Ok(status.success()),
        Ran::Stuck => Err("git check-ref-format did not end in time".to_owned()),
    }
}

fn in_doubt(top: &str, why: &str) -> Unrebased {
    Unrebased::InDoubt(format!("{why}; see git status in {top}"))
}

fn left(until: Instant, most: Duration) -> Result<Duration, String> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        let minutes = RUNNING_MAX.as_secs() / 60;
        return Err(format!("the rebase ran out of its {minutes} minutes, and goes no further"));
    }
    Ok(cmp::min(left, most))
}

fn says(dir: &str, args: &[&str], until: Instant) -> Result<String, String> {
    git::says_within(dir, args, left(until, GIT_SAYS)?)
}

fn under_way(top: &str, until: Instant) -> Result<Option<Doing>, String> {
    let mut asked = vec!["rev-parse"];
    for (path, _) in UNDER_WAY {
        asked.extend(["--git-path", path]);
    }
    let said = says(top, &asked, until)?;
    let paths: Vec<&str> = said.lines().collect();
    if paths.len() != UNDER_WAY.len() {
        return Err(format!("git rev-parse said {} paths of {}", paths.len(), UNDER_WAY.len()));
    }
    for (path, (_, doing)) in paths.into_iter().zip(UNDER_WAY) {
        if found(top, path)? {
            return Ok(Some(doing));
        }
    }
    Ok(None)
}

fn found(top: &str, path: &str) -> Result<bool, String> {
    let path = Path::new(top).join(path);
    path.try_exists().map_err(|error| format!("{} does not read: {error}", path.display()))
}

fn branch(top: &str, until: Instant) -> Result<Option<String>, String> {
    let head = says(top, &["rev-parse", "--symbolic-full-name", "HEAD"], until)?;
    Ok(head.strip_prefix("refs/heads/").filter(|branch| !branch.is_empty()).map(str::to_owned))
}

fn committed(top: &str, until: Instant) -> Result<bool, String> {
    let asked = ["--no-optional-locks", "status", "--porcelain", "--untracked-files=no"];
    let said = says(top, &[asked.as_slice(), &["--ignore-submodules"]].concat(), until)?;
    Ok(said.is_empty())
}

fn tip(top: &str, branch: &str, until: Instant) -> Result<String, String> {
    says(top, &["rev-parse", "--verify", &format!("refs/heads/{branch}")], until)
}

fn ahead(top: &str, reference: &str, branch: &str, until: Instant) -> Result<u32, String> {
    let counted = format!("{reference}..refs/heads/{branch}");
    let said = says(top, &["rev-list", "--count", &counted], until)?;
    said.parse().map_err(|_| format!("git rev-list counted {said}, which is no count"))
}

fn picked(top: &str, onto: &str, branch: &str, until: Instant) -> Result<Picked, Unrebased> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", top, "rebase", onto, branch]))
        .env("GIT_EDITOR", ":")
        .env("GIT_SEQUENCE_EDITOR", ":");
    let limit = left(until, PICKING).map_err(Unrebased::Refused)?;
    let ran = program::run_alone(&mut git, limit).map_err(|error| {
        Unrebased::Refused(format!("git rebase could not be run: {error}"))
    })?;
    let code = match &ran {
        Ran::Ended { status, .. } => status.code(),
        Ran::Stuck => return Err(in_doubt(top, "git rebase did not end in time and was stopped")),
    };
    let Err(why) = ran.answer() else {
        return Ok(Picked::Ended);
    };
    let doing = under_way(top, until)
        .map_err(|unread| in_doubt(top, &format!("git rebase: {why}, and {unread}")))?;
    match (code, doing) {
        (Some(STOPPED_CODE), Some(Doing::Rebase)) => Ok(Picked::Stopped),
        (Some(_), None | Some(Doing::Merge | Doing::CherryPick | Doing::Revert)) => {
            Err(Unrebased::Refused(format!("git rebase: {why}")))
        },
        (Some(_), Some(Doing::Rebase)) => {
            Err(in_doubt(top, &format!("git rebase: {why}, and a rebase is under way")))
        },
        (None, _) => Err(in_doubt(top, &format!("git rebase: {why}"))),
    }
}

fn stopped_at(top: &str, until: Instant) -> Result<Option<String>, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-c", "safe.bareRepository=explicit", "-C", top]))
        .args(["rev-parse", "--verify", "--quiet", "REBASE_HEAD"]);
    let ran = program::run(&mut git, left(until, GIT_SAYS)?)
        .map_err(|error| format!("git rev-parse could not be run: {error}"))?;
    if let Ran::Ended { status, stdout, .. } = &ran
        && status.code() == Some(UNVERIFIED_CODE)
        && stdout.is_empty()
    {
        return Ok(None);
    }
    ran.answer().map_err(|why| format!("git rev-parse REBASE_HEAD in {top}: {why}"))?;
    let asked = ["log", "-1", "--no-show-signature", "--format=%h %s", "REBASE_HEAD", "--"];
    says(top, &asked, until).map(Some)
}

fn conflicted(top: &str, until: Instant) -> Result<Conflicted, String> {
    let said = says(top, &["ls-files", "--unmerged", "-z"], until)?;
    if said.is_empty() {
        return Ok(Conflicted::of(Vec::new()));
    }
    let listed = said.strip_suffix('\0').ok_or("git ls-files did not end its list in a NUL")?;
    let mut paths = Vec::new();
    for entry in listed.split('\0') {
        let unnamed = || format!("git ls-files listed {entry:?}, which names no file");
        let (_, path) = entry.split_once('\t').ok_or_else(unnamed)?;
        paths.push(path.to_owned());
    }
    Ok(Conflicted::of(paths))
}

impl Conflicted {
    #[must_use]
    pub fn of(mut paths: Vec<String>) -> Conflicted {
        paths.sort_unstable();
        paths.dedup();
        let count = u32::try_from(paths.len())
            .expect("git says at most SAID_MAX bytes, which name fewer than u32::MAX files");
        paths.truncate(CONFLICTED_NAMED_MAX);
        Conflicted { named: paths, count }
    }

    pub fn new(named: Vec<String>, count: u32) -> Result<Conflicted, String> {
        let wanted = usize::try_from(count).unwrap_or(usize::MAX).min(CONFLICTED_NAMED_MAX);
        if named.len() != wanted {
            let named = named.len();
            return Err(format!("it names {named} files of the {count} in conflict, not {wanted}"));
        }
        Ok(Conflicted { named, count })
    }

    #[must_use]
    pub fn named(&self) -> &[String] {
        &self.named
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    fn unnamed(&self) -> u32 {
        let named = self.named.len();
        let named = u32::try_from(named).expect("at most CONFLICTED_NAMED_MAX files are named");
        self.count.checked_sub(named).expect("a conflicted list names at most its count")
    }
}

impl Unrebased {
    #[must_use]
    pub fn of(why: String, code: Option<Code>) -> Unrebased {
        match code {
            Some(
                Code::FailedPrecondition
                | Code::InvalidArgument
                | Code::NotFound
                | Code::Unimplemented,
            ) => Unrebased::Refused(why),
            _ => Unrebased::InDoubt(why),
        }
    }

    #[must_use]
    pub fn status(self) -> Status {
        match self {
            Unrebased::Refused(why) => Status::failed_precondition(why),
            Unrebased::InDoubt(why) => Status::aborted(why),
        }
    }
}

impl Onto {
    fn fetched(&self) -> Option<(&str, &str)> {
        match self {
            Onto::Default(DefaultBranch::Remote { remote, branch }) => Some((remote, branch)),
            Onto::Base { remote, branch } => Some((remote, branch)),
            Onto::Default(DefaultBranch::Local { .. }) => None,
        }
    }

    fn reference(&self) -> String {
        match self {
            Onto::Default(default) => default.reference(),
            Onto::Base { remote, branch } => format!("refs/remotes/{remote}/{branch}"),
        }
    }
}

impl fmt::Display for Onto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Onto::Default(default) => default.fmt(f),
            Onto::Base { remote, branch } => write!(f, "{remote}/{branch}"),
        }
    }
}

impl fmt::Display for Doing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Doing::Rebase => "a rebase",
            Doing::Merge => "a merge",
            Doing::CherryPick => "a cherry-pick",
            Doing::Revert => "a revert",
        })
    }
}

#[must_use]
pub fn prompt(branch: &str, onto: &str, stopped: &Stopped) -> String {
    let at = stopped.at.as_ref().map(|at| format!(" at {at}"));
    let conflicted = match stopped.conflicted.count {
        0 => None,
        _ => Some(format!(" on conflicts in {}", files(&stopped.conflicted))),
    };
    let how = [at, conflicted].into_iter().flatten().collect::<Vec<_>>().join(",");
    format!(
        "The rebase of {branch} onto {onto} stopped{how}. Resolve it: keep what \
         {branch} changes, on top of what {onto} brought, `git add` what you resolve, and run \
         `git rebase --continue`, resolving each commit that stops the same way, until the rebase \
         ends. Then check that it still builds. Push nothing, and run no `git rebase --abort` \
         unless I ask for it."
    )
}

fn files(conflicted: &Conflicted) -> String {
    let named = conflicted.named.join(", ");
    match conflicted.unnamed() {
        0 => named,
        more => format!("{named}, and {more} more"),
    }
}

#[must_use]
pub fn asked(on: Option<On>, dir: &Directory, branch: Option<&str>) -> RebaseRequest {
    RebaseRequest { on, dir: dir.as_str().to_owned(), branch: branch.map(str::to_owned) }
}

impl Rebased {
    #[must_use]
    pub fn to_api(&self) -> RebaseResponse {
        let came = match &self.came {
            Came::Already => Said::Already(()),
            Came::Clean { ahead } => Said::Clean(rebase_response::Clean { ahead: *ahead }),
            Came::Stopped(Stopped { at, conflicted }) => {
                let (at, named) = (at.clone(), conflicted.named.clone());
                let conflicted_count = conflicted.count;
                Said::Stopped(rebase_response::Stopped { at, conflicted: named, conflicted_count })
            },
        };
        RebaseResponse { branch: self.branch.clone(), onto: self.onto.clone(), came: Some(came) }
    }

    pub fn of_api(said: RebaseResponse) -> Result<Rebased, String> {
        let came = match said.came.ok_or("lunetad said nothing of how the rebase came out")? {
            Said::Already(()) => Came::Already,
            Said::Clean(rebase_response::Clean { ahead }) => Came::Clean { ahead },
            Said::Stopped(rebase_response::Stopped { at, conflicted, conflicted_count }) => {
                let conflicted = Conflicted::new(conflicted, conflicted_count)
                    .map_err(|why| format!("lunetad said the rebase stopped, but {why}"))?;
                Came::Stopped(Stopped { at, conflicted })
            },
        };
        Ok(Rebased { branch: said.branch, onto: said.onto, came })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut git = Command::new("git");
        git::local(&mut git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(["-c", "maintenance.auto=false", "-c", "init.defaultBranch=main"])
            .arg("-C")
            .arg(dir)
            .args(args);
        let ran = git.output().unwrap();
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "git {args:?} in {}: {stderr}", dir.display());
        String::from_utf8(ran.stdout).unwrap().trim_end().to_owned()
    }

    fn commit(dir: &Path, file: &str, text: &str, subject: &str) -> String {
        fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", subject]);
        git(dir, &["rev-parse", "--short", "HEAD"])
    }

    fn own_config(dir: &Path) {
        let config = [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
            ("core.fsmonitor", "false"),
        ];
        for (key, value) in config {
            git(dir, &["config", key, value]);
        }
    }

    fn directory(dir: &Path) -> Directory {
        Directory::parse(dir.to_str().unwrap().to_owned()).unwrap()
    }

    fn refusal(dir: &Path, expected: Option<&str>) -> String {
        match rebase(&directory(dir), expected) {
            Err(Unrebased::Refused(why)) => why,
            other => panic!("{} is not refused: {other:?}", dir.display()),
        }
    }

    struct Cloned {
        _scratch: tempfile::TempDir,
        up: PathBuf,
        dir: PathBuf,
    }

    impl Cloned {
        fn new() -> Cloned {
            let scratch = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(scratch.path()).unwrap();
            let (up, dir) = (root.join("up"), root.join("clone"));
            fs::create_dir(&up).unwrap();
            git(&up, &["init", "-q", "-b", "main"]);
            commit(&up, "base", "a\nb\nc\n", "base");
            git(&root, &["clone", "-q", up.to_str().unwrap(), dir.to_str().unwrap()]);
            own_config(&dir);
            git(&dir, &["switch", "-q", "-c", "issue-429"]);
            Cloned { _scratch: scratch, up, dir }
        }

        fn head(&self, name: &str) -> String {
            git(&self.dir, &["rev-parse", name])
        }
    }

    #[test]
    fn a_branch_is_rebased_onto_the_default_branch_as_its_remote_has_it_now() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        let rebased = rebase(&directory(&cloned.dir), Some("issue-429")).unwrap();
        let onto = ("issue-429".to_owned(), "origin/main".to_owned());
        assert_eq!((rebased.branch, rebased.onto), onto);
        assert_eq!(rebased.came, Came::Clean { ahead: 1 });
        let up = git(&cloned.up, &["rev-parse", "main"]);
        assert_eq!(cloned.head("HEAD~"), up, "the default branch is fetched first");

        assert_eq!(rebase(&directory(&cloned.dir), None).unwrap().came, Came::Already);
        let docs = cloned.dir.join("docs");
        fs::create_dir(&docs).unwrap();
        let below = rebase(&directory(&docs), None).unwrap().came;
        assert_eq!(below, Came::Already, "from a folder below the top");
    }

    #[test]
    fn a_rebase_that_stops_on_conflicts_is_left_stopped_and_says_where() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        let picked = commit(&cloned.dir, "base", "a\nB\nc\n", "own base");
        commit(&cloned.up, "base", "a\nb2\nc\n", "their base");
        let rebased = rebase(&directory(&cloned.dir), None).unwrap();
        let Came::Stopped(stopped) = &rebased.came else {
            panic!("the rebase ran through: {rebased:?}");
        };
        let at = Some(format!("{picked} own base"));
        let conflicted = Conflicted::of(vec!["base".to_owned()]);
        assert_eq!(stopped, &Stopped { at, conflicted });
        assert!(cloned.dir.join(".git/rebase-merge").is_dir(), "the rebase waits, stopped");

        let refused = refusal(&cloned.dir, None);
        let doing = "is in the middle of a rebase: finish it, or abort it, first";
        assert!(refused.ends_with(doing), "{refused}");
        let top = cloned.dir.to_str().unwrap();
        let onto = "refs/remotes/origin/main";
        let later = super::picked(top, onto, "issue-429", Instant::now() + GIT_SAYS);
        let Err(Unrebased::InDoubt(why)) = later else {
            panic!("a rebase git would not start is taken for {later:?}");
        };
        assert!(why.contains("already a rebase-merge directory"), "{why}");
        assert!(why.contains(", and a rebase is under way; see git status in "), "{why}");
        let words = prompt(&rebased.branch, &rebased.onto, stopped);
        let starts = format!(
            "The rebase of issue-429 onto origin/main stopped at {picked} own base, on conflicts \
             in base. Resolve it: "
        );
        assert!(words.starts_with(&starts), "{words}");
    }

    #[test]
    fn changes_not_committed_another_branch_and_no_branch_are_refused_before_anything_moves() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        let tip = cloned.head("HEAD");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        fs::write(cloned.dir.join("own"), "changed\n").unwrap();
        let refused = refusal(&cloned.dir, None);
        assert!(refused.ends_with("has changes not committed: commit or stash them first"));
        assert_eq!(cloned.head("HEAD"), tip);
        assert_eq!(cloned.head("origin/main"), cloned.head("main"), "nothing is fetched");

        git(&cloned.dir, &["checkout", "-q", "own"]);
        fs::write(cloned.dir.join("loose"), "untracked\n").unwrap();
        let refused = refusal(&cloned.dir, Some("fix"));
        assert!(refused.ends_with("is on issue-429 now, not fix"), "{refused}");
        assert_eq!(cloned.head("origin/main"), cloned.head("main"), "nothing is fetched");
        assert_eq!(rebase(&directory(&cloned.dir), None).unwrap().came, Came::Clean { ahead: 1 });

        git(&cloned.dir, &["switch", "-q", "--detach"]);
        let refused = refusal(&cloned.dir, None);
        assert!(refused.ends_with("is on no branch, and a rebase moves a branch"), "{refused}");
    }

    #[test]
    fn the_branch_asked_for_is_rebased_though_another_is_checked_out_by_then() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        git(&cloned.dir, &["fetch", "-q", "origin"]);
        git(&cloned.dir, &["switch", "-q", "-c", "other", "main"]);
        commit(&cloned.dir, "other", "x\n", "other");
        let other = cloned.head("other");
        let top = cloned.dir.to_str().unwrap();
        let onto = "refs/remotes/origin/main";
        let picked = super::picked(top, onto, "issue-429", Instant::now() + GIT_SAYS);
        assert_eq!(picked, Ok(Picked::Ended));
        assert_eq!(cloned.head("other"), other, "the branch checked out by then is not rewritten");
        assert_eq!(cloned.head("issue-429~"), cloned.head("origin/main"));
    }

    #[test]
    fn a_rebase_stopped_for_its_time_is_in_doubt_not_refused() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        git(&cloned.dir, &["fetch", "-q", "origin"]);
        let hooks = cloned.dir.join(".git/waiting");
        fs::create_dir(&hooks).unwrap();
        let draft = hooks.join("pre-rebase.draft");
        fs::write(&draft, "#!/bin/sh\nexec sleep 5\n").unwrap();
        let hook = hooks.join("pre-rebase");
        let installed = Command::new("install").arg("-m755").arg(&draft).arg(&hook).status();
        assert!(installed.unwrap().success());
        git(&cloned.dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let top = cloned.dir.to_str().unwrap();
        let until = Instant::now() + Duration::from_millis(500);
        let picked = super::picked(top, "refs/remotes/origin/main", "issue-429", until);
        let Err(Unrebased::InDoubt(why)) = picked else {
            panic!("a rebase stopped part of the way is taken for {picked:?}");
        };
        let stopped = "git rebase did not end in time and was stopped; see git status in ";
        assert!(why.starts_with(stopped), "{why}");
    }

    #[test]
    fn a_far_rebase_is_refused_only_where_lunetad_says_nothing_ran() {
        let why = || "w".to_owned();
        for code in [Code::FailedPrecondition, Code::InvalidArgument, Code::NotFound] {
            assert_eq!(Unrebased::of(why(), Some(code)), Unrebased::Refused(why()), "{code:?}");
        }
        for code in [Some(Code::Aborted), Some(Code::Unavailable), Some(Code::Internal), None] {
            assert_eq!(Unrebased::of(why(), code), Unrebased::InDoubt(why()), "{code:?}");
        }
        for unrebased in [Unrebased::Refused(why()), Unrebased::InDoubt(why())] {
            let status = unrebased.clone().status();
            assert_eq!(Unrebased::of(why(), Some(status.code())), unrebased);
        }
    }

    #[test]
    fn a_repository_with_no_default_branch_is_not_rebased() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(scratch.path()).unwrap();
        git(&dir, &["init", "-q", "-b", "trunk"]);
        own_config(&dir);
        commit(&dir, "base", "a\n", "base");
        let refused = refusal(&dir, None);
        assert!(refused.contains("has no default branch to rebase onto"), "{refused}");
    }

    fn files(count: u32) -> Vec<String> {
        (1..=count).rev().map(|n| format!("f{n:02}")).collect()
    }

    #[test]
    fn a_prompt_names_at_most_twenty_files() {
        let conflicted = Conflicted::of([files(22), files(3)].concat());
        assert_eq!((conflicted.named().len(), conflicted.count()), (20, 22));
        let words = prompt("b", "origin/main", &Stopped { at: None, conflicted });
        let starts = "The rebase of b onto origin/main stopped on conflicts in f01, f02, f03, ";
        assert!(words.starts_with(starts), "{words}");
        assert!(words.contains(", f20, and 2 more. Resolve it: "), "{words}");
        let stopped = Stopped { at: Some("abc x".to_owned()), conflicted: Conflicted::of(vec![]) };
        let words = prompt("b", "main", &stopped);
        assert!(words.starts_with("The rebase of b onto main stopped at abc x. Resolve it: "));
    }

    #[test]
    fn what_a_rebase_came_to_says_the_same_over_the_wire() {
        let at = Some("abc x".to_owned());
        let stopped = Stopped { at, conflicted: Conflicted::of(files(30)) };
        for came in [Came::Already, Came::Clean { ahead: 3 }, Came::Stopped(stopped)] {
            let rebased = Rebased { branch: "b".to_owned(), onto: "origin/main".to_owned(), came };
            assert_eq!(Rebased::of_api(rebased.to_api()), Ok(rebased));
        }
        let unsaid = RebaseResponse { branch: "b".to_owned(), onto: "main".to_owned(), came: None };
        assert!(Rebased::of_api(unsaid).is_err());
    }

    #[test]
    fn a_far_list_of_conflicts_names_as_many_files_as_it_counts_up_to_twenty() {
        assert!(Conflicted::new(files(3), 3).is_ok());
        assert!(Conflicted::new(files(20), 30).is_ok());
        let more = Conflicted::new(files(3), 2).unwrap_err();
        assert_eq!(more, "it names 3 files of the 2 in conflict, not 2");
        assert!(Conflicted::new(files(2), 3).is_err(), "a file of the three goes unnamed");
        assert!(Conflicted::new(files(21), 30).is_err(), "more than twenty are named");
        let (conflicted, conflicted_count) = (files(3), 2);
        let stopped = rebase_response::Stopped { at: None, conflicted, conflicted_count };
        let said = RebaseResponse {
            branch: "b".to_owned(),
            onto: "main".to_owned(),
            came: Some(Said::Stopped(stopped)),
        };
        let refused = Rebased::of_api(said).unwrap_err();
        let starts = "lunetad said the rebase stopped, but it names 3 files";
        assert!(refused.starts_with(starts), "{refused}");
    }

    #[test]
    fn a_rebase_past_its_time_runs_nothing_more() {
        assert_eq!(left(Instant::now() + RUNNING_MAX, GIT_SAYS), Ok(GIT_SAYS));
        let spent = left(Instant::now(), GIT_SAYS).unwrap_err();
        assert_eq!(spent, "the rebase ran out of its 20 minutes, and goes no further");
    }
}
