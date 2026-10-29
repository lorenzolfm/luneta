//! A pull request's link, pasted in luneta, and where the menu it opens
//! goes for it: a worktree of the pull request, in a tab of the session that
//! works in its repository, or the same with an agent on a review of it.
//!
//! luneta finds that session as a key of the menu is pressed: the one its
//! zellij is in, where a pane of it works in the repository, or else another
//! that does, or else one it makes, in a clone of the repository that zoxide
//! knows. A repository is the pull request's where any of its remotes is the
//! repository on GitHub the link is of: a fork's clone has the repository it
//! forks among its remotes, and that is the one its pull requests are of.
//! `wt add` makes the worktree, and says where it is, or where one of the
//! pull request's branch is already. A pane that has that branch checked out,
//! as the daemon says, or that works in the worktree `wt add` says, is where
//! a key goes instead of a new tab.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use lunetad::program::{self, Ran};

use crate::dirs;
use crate::host::Event;
use crate::sidebar::Listed;
use crate::zellij::{self, SessionName};

/// The host a pull request's link is on, and the remotes of its repository.
const GITHUB: &str = "github.com";

/// How long `wt add` has to make a worktree: it fetches the pull request's
/// branch across the network, which takes a while of a large repository.
const MAKING: Duration = Duration::from_mins(3);

/// How long git has to say a repository's remotes, or the branch checked out
/// in a directory: it reads files.
const ANSWER: Duration = Duration::from_secs(5);

/// A pull request on GitHub, as its link names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    owner: String,
    name: String,
    number: NonZeroU32,
}

impl Link {
    /// The pull request `text` is the link of, but for the space around it:
    /// of the pull request itself or of a page of it, as its files or a
    /// comment on it, with or without `https://`. None for any other text.
    pub fn parse(text: &str) -> Option<Link> {
        let text = text.trim();
        let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"));
        let rest = rest.unwrap_or(text);
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let rest = rest.strip_prefix(GITHUB)?.strip_prefix('/')?;
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        let mut parts = path.split('/');
        let (owner, name) = (parts.next()?, parts.next()?);
        let (pull, number) = (parts.next()?, parts.next()?);
        let digits = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
        if pull != "pull" || !named(owner) || !named(name) || !digits {
            return None;
        }
        let number = number.parse().ok()?;
        Some(Link { owner: owner.to_owned(), name: name.to_owned(), number })
    }

    /// Its link, as `wt add` and the review take it.
    pub fn url(&self) -> String {
        format!("https://{GITHUB}/{}/{}/pull/{}", self.owner, self.name, self.number)
    }

    /// What its tab is named, and the menu calls it: `#123`.
    pub fn tab(&self) -> String {
        format!("#{}", self.number)
    }

    /// Its repository, as `owner/name`.
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Whether a remote's `url` is of its repository, case aside, as GitHub
    /// takes the names.
    fn of(&self, url: &str) -> bool {
        repository_of(url).is_some_and(|(owner, name)| {
            owner.eq_ignore_ascii_case(&self.owner) && name.eq_ignore_ascii_case(&self.name)
        })
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repository(), self.number)
    }
}

/// Whether `part` can name an owner or a repository on GitHub: letters,
/// digits, `-`, `_` and `.`, and more than dots.
fn named(part: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !part.is_empty() && part.chars().all(allowed) && part.chars().any(|c| c != '.')
}

/// The repository on GitHub a remote's `url` is of, its owner and its name,
/// as git spells it by ssh or https: `git@github.com:owner/name.git`,
/// `https://github.com/owner/name` or `ssh://git@github.com/owner/name.git`.
/// None for one of another host, or a path.
fn repository_of(url: &str) -> Option<(&str, &str)> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = match rest.split_once('@') {
        Some((user, host)) if !user.contains('/') => host,
        _ => rest,
    };
    let rest = rest.strip_prefix(GITHUB)?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    (named(owner) && named(name)).then_some((owner, name))
}

/// How a repository is the pull request's: by its `origin`, or by another of
/// its remotes, as the one a fork's clone forks. One by its origin goes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Remote {
    Origin,
    Other,
}

/// How the remotes `listed` are of the pull request `link`'s repository, if
/// any is: as `git config --get-regexp` prints them, `remote.<name>.url
/// <url>` a line.
fn remote(listed: &str, link: &Link) -> Option<Remote> {
    let of = |line: &str| {
        let (key, url) = line.split_once(' ')?;
        let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
        link.of(url.trim()).then_some(if name == "origin" { Remote::Origin } else { Remote::Other })
    };
    listed.lines().filter_map(of).min()
}

/// A pane as the sidebar knows it, for luneta to find where a key of the
/// menu goes: its session and its id, where it works, the pull request of the
/// branch checked out there, as the daemon says, and whether luneta's zellij
/// is in its session. The sidebar hands them in the order of its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub session: Listed,
    pub pane: u32,
    pub dir: String,
    pub pull: Option<NonZeroU32>,
    pub here: bool,
}

/// Where a key of the menu goes, as luneta found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At {
    /// The pane `pane` of `session`, which has the pull request's branch
    /// checked out.
    There { session: Listed, pane: u32 },
    /// A tab of `session`, of which the pane `pane` works in the pull
    /// request's repository: luneta's zellij goes to that pane first, where
    /// it is in another session, as a tab needs a client of its session.
    Session { session: Listed, pane: u32 },
    /// A tab of a session luneta makes, `name`, that starts in `cwd`, where
    /// the clone of the repository is. No session zellij has is called so.
    Clone { cwd: String, name: SessionName },
}

/// What luneta found for a key of the menu: where it goes, the worktree of
/// the pull request, and the branch checked out there, which an agent is
/// named for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub at: At,
    pub worktree: String,
    pub branch: Option<String>,
}

/// Finds where a key of the menu numbered `menu` goes for the pull request
/// `link`, among the panes the sidebar knows, `worked`, and has `wt add` make
/// the worktree, on a thread of its own, and hands back what it found, or why
/// it found nothing.
pub fn prepare(menu: u64, link: Link, worked: Vec<Worked>, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let ready = ready(&link, &worked);
        // luneta is on its way out.
        let _ = events.send(Event::Pulled { menu, ready });
    });
}

/// Where a key goes for `link`, as `prepare` says.
fn ready(link: &Link, worked: &[Worked]) -> Result<Ready, String> {
    let mut remotes: HashMap<String, Option<Remote>> = HashMap::new();
    let mut remote_of = |dir: &str| {
        let listed = || remotes_in(dir).and_then(|listed| remote(&listed, link));
        *remotes.entry(dir.to_owned()).or_insert_with(listed)
    };
    let (repository, at) = match found(link, worked, &mut remote_of) {
        Some(Found::There(at)) => {
            let Worked { session, pane, dir, .. } = &worked[at];
            let at = At::There { session: session.clone(), pane: *pane };
            return Ok(Ready { at, worktree: dir.clone(), branch: branch(dir) });
        },
        Some(Found::Session(at)) => {
            let Worked { session, pane, dir, .. } = &worked[at];
            (dir.clone(), At::Session { session: session.clone(), pane: *pane })
        },
        None => clone(link, &mut remote_of)?,
    };
    let worktree = worktree(&repository, link)?;
    // The worktree of the pull request's branch, where it was one already,
    // may have a pane that works in it, as the kernel says where.
    let in_it = worked.iter().filter(|it| it.dir == worktree);
    let at = match in_it.min_by_key(|it| !it.here) {
        Some(Worked { session, pane, .. }) => At::There { session: session.clone(), pane: *pane },
        None => at,
    };
    Ok(Ready { at, branch: branch(&worktree), worktree })
}

/// Where a key goes, by where it stands among the panes.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// The pane that has the pull request's branch checked out.
    There(usize),
    /// A pane that works in its repository, for a tab of its session.
    Session(usize),
}

/// Where a key goes for `link` among the panes `worked`, by how `remote_of`
/// says a directory's repository is the pull request's, if it is: the pane
/// that has the pull request's branch checked out, or else a pane that works
/// in its repository. Of either, one in the session luneta's zellij is in
/// goes first, then one whose repository is the pull request's by its origin,
/// then the first row.
fn found(
    link: &Link,
    worked: &[Worked],
    mut remote_of: impl FnMut(&str) -> Option<Remote>,
) -> Option<Found> {
    let mut there = None;
    let mut session = None;
    for (at, it) in worked.iter().enumerate() {
        let Some(by) = remote_of(&it.dir) else {
            continue;
        };
        let rank = (!it.here, by, at);
        let best = |was: Option<(bool, Remote, usize)>| Some(was.map_or(rank, |was| was.min(rank)));
        if it.pull == Some(link.number) {
            there = best(there);
        }
        session = best(session);
    }
    let at = |(_, _, at)| at;
    there.map(at).map(Found::There).or_else(|| session.map(at).map(Found::Session))
}

/// The clone of `link`'s repository among the directories zoxide knows, by
/// how `remote_of` says a directory's repository is the pull request's: a
/// directory of it, for `wt add`, and the session luneta makes for it, in the
/// directory that holds it, named as the picker names that directory, with a
/// `-2` where a session has the name: one that runs, or one that ended, which
/// zellij keeps to bring back. Or why there is none.
fn clone(
    link: &Link,
    remote_of: impl FnMut(&str) -> Option<Remote>,
) -> Result<(String, At), String> {
    let repository = link.repository();
    let dirs: Vec<String> = dirs::visited()
        .map_err(|why| {
            format!("no session works in {repository}, and luneta cannot look for a clone: {why}")
        })?
        .into_iter()
        .map(|dir| dir.path)
        .collect();
    let Some(dir) = cloned(&dirs, remote_of) else {
        return Err(format!(
            "no session works in {repository}, nor does zoxide know a clone of it; wt clone it \
             first"
        ));
    };
    let dir = dirs[dir].clone();
    let cwd = holder(&dir).ok_or_else(|| format!("git does not say what repository {dir} is"))?;
    let name = zellij::session_for(Path::new(&cwd))
        .ok_or_else(|| format!("no session can be named for {cwd}"))?;
    let named =
        zellij::named().ok_or_else(|| "luneta cannot tell which sessions zellij has".to_owned())?;
    let name = name.free(&named.iter().map(String::as_str).collect::<Vec<_>>());
    Ok((dir, At::Clone { cwd, name }))
}

/// Which of `dirs`, best first, is of the pull request's repository, by how
/// `remote_of` says a directory's is: the first by its origin, or else the
/// first by another remote.
fn cloned(dirs: &[String], mut remote_of: impl FnMut(&str) -> Option<Remote>) -> Option<usize> {
    let mut best: Option<(Remote, usize)> = None;
    for (at, dir) in dirs.iter().enumerate() {
        if let Some(by) = remote_of(dir) {
            best = Some(best.map_or((by, at), |was| was.min((by, at))));
            if by == Remote::Origin {
                break;
            }
        }
    }
    best.map(|(_, at)| at)
}

/// The directory that holds the repository `dir` is in: where wt keeps its
/// worktrees, beside its hidden `.bare`, or the work tree of one cloned as
/// git clones, beside its `.git`. A bare repository that is not hidden holds
/// itself.
fn holder(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, "rev-parse", "--path-format=absolute"]))
        .arg("--git-common-dir");
    let common = said(&mut git)?;
    let common = Path::new(&common);
    let hidden = common.file_name()?.to_str()?.starts_with('.');
    let holder = if hidden { common.parent()? } else { common };
    holder.to_str().map(str::to_owned)
}

/// The remotes of the repository `dir` is in, as `git config --get-regexp`
/// prints them; none where it has none, or `dir` is in no repository.
fn remotes_in(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, "config", "--get-regexp", r"^remote\..*\.url$"]));
    let ran = program::run(&mut git, ANSWER).ok()?;
    String::from_utf8(ran.answer().ok()?).ok()
}

/// The branch checked out in `dir`, as `git status` names it: its ref
/// without `refs/heads/`. None for a detached `HEAD`, or where git does not
/// say.
fn branch(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, "symbolic-ref", "--quiet", "HEAD"]));
    Some(said(&mut git)?.strip_prefix("refs/heads/")?.to_owned())
}

/// The one line git said, where it ended well.
fn said(git: &mut Command) -> Option<String> {
    let said = program::run(git, ANSWER).ok()?.answer().ok()?;
    let said = String::from_utf8(said).ok()?;
    Some(said.strip_suffix('\n')?.to_owned()).filter(|said| !said.is_empty())
}

/// Has `wt add` make a worktree of the pull request `link` in the repository
/// `dir` is in, and says where it is, or where one of the pull request's
/// branch is already: the last line wt prints, as the real path of the
/// directory it names. Or why it made none, as wt says it.
fn worktree(dir: &str, link: &Link) -> Result<String, String> {
    let mut wt = Command::new("wt");
    lunetad::local_git(wt.args(["add", "--repo", dir, &link.url()]));
    let ran = program::run(&mut wt, MAKING)
        .map_err(|error| format!("wt add could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            let printed = String::from_utf8_lossy(&stdout);
            let said = printed.lines().map(str::trim).rfind(|line| !line.is_empty());
            let said = said.ok_or_else(|| "wt add said no worktree".to_owned())?;
            directory(said).ok_or_else(|| format!("wt add said {said}, which is no directory"))
        },
        Ran::Ended { status, stderr, .. } => {
            Err(failure(&String::from_utf8_lossy(&stderr)).unwrap_or(format!("wt add: {status}")))
        },
        Ran::Stuck => Err(format!("wt add took longer than {} minutes", MAKING.as_secs() / 60)),
    }
}

/// The real path of the directory `path` names, where it names one, as the
/// kernel says where a pane works.
fn directory(path: &str) -> Option<String> {
    let real = fs::canonicalize(path).ok().filter(|real| real.is_dir())?;
    real.into_os_string().into_string().ok()
}

/// What wt said as it failed, on stderr after what it said of its progress:
/// the lines from its last that starts with `wt: `, which says why, on one.
fn failure(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let why = lines.iter().rposition(|line| line.starts_with("wt: "))?;
    Some(format!("wt add: {}", lines[why..].join(", ").trim_start_matches("wt: ")))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, SystemTime};

    use super::{Found, Link, Remote, Worked, cloned, failure, found, remote, repository_of};
    use crate::sidebar::Listed;

    fn link() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    fn listed(name: &str) -> Listed {
        Listed::new(name, SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    fn worked(session: &str, pane: u32, dir: &str, pull: Option<u32>, here: bool) -> Worked {
        let pull = pull.map(|number| NonZeroU32::new(number).expect("a pull request's number"));
        Worked { session: listed(session), pane, dir: dir.to_owned(), pull, here }
    }

    #[test]
    fn a_link_is_of_a_pull_request_or_of_a_page_of_one() {
        let pulled = |text: &str| Link::parse(text).map(|link| (link.to_string(), link.url()));
        let cdk = Some((
            "cashubtc/cdk#2736".to_owned(),
            "https://github.com/cashubtc/cdk/pull/2736".to_owned(),
        ));
        for text in [
            "https://github.com/cashubtc/cdk/pull/2736",
            "  https://github.com/cashubtc/cdk/pull/2736\n",
            "https://github.com/cashubtc/cdk/pull/2736/files",
            "https://github.com/cashubtc/cdk/pull/2736#issuecomment-1",
            "https://github.com/cashubtc/cdk/pull/2736?w=1",
            "http://www.github.com/cashubtc/cdk/pull/2736",
            "github.com/cashubtc/cdk/pull/2736",
        ] {
            assert_eq!(pulled(text), cdk, "{text:?}");
        }
        for text in [
            "https://github.com/cashubtc/cdk/issues/2736",
            "https://github.com/cashubtc/cdk/pull/0",
            "https://github.com/cashubtc/cdk/pull/+1",
            "https://github.com/cashubtc/cdk/pull/",
            "https://github.com/cashubtc/pull/2736",
            "https://gitlab.com/cashubtc/cdk/pull/2736",
            "https://github.com.evil/cashubtc/cdk/pull/2736",
            "see https://github.com/cashubtc/cdk/pull/2736",
            "https://github.com/cashubtc/cdk/pull/2736 and more",
            "https://github.com/../cdk/pull/2736",
        ] {
            assert_eq!(pulled(text), None, "{text:?}");
        }
        assert_eq!(link().tab(), "#2736");
    }

    #[test]
    fn a_remote_is_of_a_repository_by_ssh_or_https_case_aside() {
        for url in [
            "git@github.com:cashubtc/cdk.git",
            "git@github.com:cashubtc/cdk",
            "https://github.com/cashubtc/cdk.git",
            "https://github.com/CashuBTC/CDK/",
            "ssh://git@github.com/cashubtc/cdk.git",
            "https://token@github.com/cashubtc/cdk",
        ] {
            assert_eq!(
                repository_of(url).map(|(owner, name)| (owner.to_lowercase(), name.to_lowercase())),
                Some(("cashubtc".to_owned(), "cdk".to_owned())),
                "{url}"
            );
            assert!(link().of(url), "{url}");
        }
        for url in ["git@gitlab.com:cashubtc/cdk.git", "/srv/git/cdk.git", "git@github.com:cdk"] {
            assert!(!link().of(url), "{url}");
        }
        assert!(!link().of("git@github.com:lorenzolfm/cdk.git"));
    }

    /// A fork's clone has the repository it forks among its remotes, by any
    /// name: the pull request is of it as well as of a clone of that one.
    #[test]
    fn a_repository_is_the_pull_request_s_by_any_remote_its_origin_first() {
        let fork = "remote.cashubtc.url git@github.com:cashubtc/cdk.git\n\
                    remote.origin.url git@github.com:lorenzolfm/cdk.git\n";
        assert_eq!(remote(fork, &link()), Some(Remote::Other));
        let clone = "remote.origin.url https://github.com/cashubtc/cdk\n\
                     remote.upstream.url https://github.com/cashubtc/cdk\n";
        assert_eq!(remote(clone, &link()), Some(Remote::Origin));
        assert_eq!(remote("remote.origin.url git@github.com:lorenzolfm/cdk.git\n", &link()), None);
        assert_eq!(remote("", &link()), None);
    }

    /// The pane that has the pull request's branch checked out wins over a
    /// session that works in its repository, wherever it is; of each, one in
    /// the session luneta's zellij is in, then one by its origin, then the
    /// first row.
    #[test]
    fn a_key_goes_to_the_pane_with_the_branch_or_else_a_session_of_the_repository() {
        let remotes = |dir: &str| match dir {
            "/cdk/main" | "/cdk/2736" => Some(Remote::Other),
            "/cdk-upstream/master" => Some(Remote::Origin),
            _ => None,
        };
        let number = Some(2736);
        let away = [
            worked("dots", 1, "/dots", None, true),
            worked("cdk", 2, "/cdk/main", None, false),
            worked("upstream", 3, "/cdk-upstream/master", None, false),
        ];
        // Its origin before another remote.
        assert_eq!(found(&link(), &away, remotes), Some(Found::Session(2)));
        let here = [
            worked("cdk", 2, "/cdk/main", None, false),
            worked("dots", 1, "/dots", None, true),
            worked("dots", 4, "/cdk/main", None, true),
            worked("upstream", 3, "/cdk-upstream/master", None, false),
        ];
        // luneta's own session before any.
        assert_eq!(found(&link(), &here, remotes), Some(Found::Session(2)));
        let checked_out = [
            worked("dots", 4, "/cdk/main", None, true),
            worked("upstream", 3, "/cdk-upstream/master", Some(2735), false),
            worked("cdk", 5, "/cdk/2736", number, false),
        ];
        assert_eq!(found(&link(), &checked_out, remotes), Some(Found::There(2)));
        // The number of another repository's pull request is not this one's.
        let elsewhere = [worked("dots", 1, "/dots", number, true)];
        assert_eq!(found(&link(), &elsewhere, remotes), None);
    }

    #[test]
    fn a_clone_is_the_first_by_its_origin_or_else_by_another_remote() {
        let dirs = ["/a", "/fork/main", "/b", "/upstream", "/fork"].map(str::to_owned);
        let remotes = |dir: &str| match dir {
            "/fork/main" | "/fork" => Some(Remote::Other),
            "/upstream" => Some(Remote::Origin),
            _ => None,
        };
        assert_eq!(cloned(&dirs, remotes), Some(3));
        assert_eq!(cloned(&dirs[..3], remotes), Some(1));
        assert_eq!(cloned(&dirs[..1], remotes), None);
    }

    #[test]
    fn wt_s_failure_is_its_last_word_after_its_progress() {
        let stderr = "  pull       #2736 (asking origin for its branch)\n\
                      wt: wt cannot fetch origin/feat\n  run git fetch origin feat to see why\n";
        assert_eq!(
            failure(stderr).as_deref(),
            Some("wt add: wt cannot fetch origin/feat, run git fetch origin feat to see why")
        );
        assert_eq!(failure("  pull  #1\n"), None);
    }
}
