//! A pull request's link, pasted in luneta, and where the menu it opens
//! goes for it: a worktree of the pull request, in a tab of the session that
//! works in its repository, or the same with an agent on a review of it. An
//! issue's link opens it as well, for a worktree of a branch of its own, or
//! the same with an agent on the issue.
//!
//! luneta finds that session as a key of the menu is pressed: the one its
//! zellij is in, where a pane of it works in the repository, or else another
//! that does, or else one it makes, in a clone of the repository that zoxide
//! knows. A repository is the pull request's where any of its remotes is the
//! repository on GitHub the link is of: a fork's clone has the repository it
//! forks among its remotes, and that is the one its pull requests are of.
//! `wt add` makes the worktree, and says where it is, or where one of the
//! pull request's branch is already, in a repository wt cloned: in one git
//! cloned, wt would make it in its `.git`. An issue's branch is `issue-123`,
//! so a second paste of the link finds the first's worktree. Where neither
//! the repository nor origin has it yet, luneta makes it from the default
//! branch of the issue's repository as GitHub has it then, by the remote
//! that is of it: wt would make it from origin's, as the repository last
//! fetched it, and a fork's clone has the fork for its origin. gh says first
//! that GitHub has an issue of its number, and no pull request: a link can
//! say an issue of any number, and luneta would make a branch of each. A
//! pane that has the pull request's branch checked out, as the daemon says
//! by its page, or that works in the worktree `wt add` says, is where a key
//! goes instead of a new tab.

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

/// How long `wt add` has to make a worktree, as it fetches the pull
/// request's branch, or git to ask a remote of an issue's branch, or to fetch
/// the branch it is made from: across the network, which takes a while of a
/// large repository.
const MAKING: Duration = Duration::from_mins(3);

/// How long git has to say a repository's remotes, or the branch checked out
/// in a directory: it reads files.
const ANSWER: Duration = Duration::from_secs(5);

/// How long gh has to say what an issue's number is on GitHub: it asks
/// GitHub, across the network.
const ASKING: Duration = Duration::from_secs(30);

/// The remote of the repository a clone was cloned from, as git names it:
/// a branch wt makes goes there.
const ORIGIN: &str = "origin";

/// A pull request or an issue on GitHub, as its link names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    owner: String,
    name: String,
    kind: Kind,
    number: NonZeroU32,
}

/// What a link is the page of: a pull request, or an issue. GitHub counts
/// the two in one sequence, and takes a link of either page for the other's
/// number, so a link can say an issue of a pull request's number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pull,
    Issue,
}

impl Kind {
    /// Each, as a link may name it.
    const ALL: [Kind; 2] = [Kind::Pull, Kind::Issue];

    /// What its link names it by, after the repository.
    const fn path(self) -> &'static str {
        match self {
            Kind::Pull => "pull",
            Kind::Issue => "issues",
        }
    }
}

impl Link {
    /// The pull request or the issue `text` is the link of, but for the space
    /// around it: of it itself or of a page of it, as a pull request's files
    /// or a comment on it, with or without `https://`. None for any other
    /// text.
    pub fn parse(text: &str) -> Option<Link> {
        let text = text.trim();
        let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"));
        let rest = rest.unwrap_or(text);
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let rest = rest.strip_prefix(GITHUB)?.strip_prefix('/')?;
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        let mut parts = path.split('/');
        let (owner, name) = (parts.next()?, parts.next()?);
        let (page, number) = (parts.next()?, parts.next()?);
        let kind = Kind::ALL.into_iter().find(|kind| kind.path() == page)?;
        let digits = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
        if !named(owner) || !named(name) || !digits {
            return None;
        }
        let number = number.parse().ok()?;
        Some(Link { owner: owner.to_owned(), name: name.to_owned(), kind, number })
    }

    /// Its link, as the agent on it takes it.
    pub fn url(&self) -> String {
        let Link { owner, name, kind, number } = self;
        format!("https://{GITHUB}/{owner}/{name}/{}/{number}", kind.path())
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The branch luneta has wt make for it: an issue's, `issue-123`. None
    /// for a pull request, which has its own.
    fn branch(&self) -> Option<String> {
        match self.kind {
            Kind::Pull => None,
            Kind::Issue => Some(format!("issue-{}", self.number)),
        }
    }

    /// What `wt add` takes for it: a pull request's link, for wt to find its
    /// branch, or an issue's branch.
    fn wt(&self) -> String {
        self.branch().unwrap_or_else(|| self.url())
    }

    /// What its tab is named, and the menu calls it: `#123`.
    pub fn tab(&self) -> String {
        format!("#{}", self.number)
    }

    /// Its repository, as `owner/name`.
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Whether `other` is the same pull request or issue, case aside in the
    /// names of its repository, as GitHub takes them: by its number, of
    /// whichever page either link is, as GitHub has one of the number.
    pub fn is(&self, other: &Link) -> bool {
        self.number == other.number
            && self.owner.eq_ignore_ascii_case(&other.owner)
            && self.name.eq_ignore_ascii_case(&other.name)
    }

    /// Whether a remote's `url` is of its repository, case aside, as GitHub
    /// takes the names: on GitHub, by its name, or by ssh, by another that
    /// ssh's config gives it, as `ssh_host` says what host ssh takes a name
    /// for.
    fn of(&self, url: &str, ssh_host: impl FnOnce(&str) -> Option<String>) -> bool {
        let Some(Spelled { host, ssh, owner, name }) = spelled(url) else {
            return false;
        };
        let github = |host: &str| host.eq_ignore_ascii_case(GITHUB);
        owner.eq_ignore_ascii_case(&self.owner)
            && name.eq_ignore_ascii_case(&self.name)
            && (github(host) || (ssh && ssh_host(host).is_some_and(|host| github(&host))))
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

/// A remote's url, as git spells it by ssh or https: `git@host:owner/name.git`,
/// `https://host/owner/name` or `ssh://git@host:22/owner/name.git`. The host
/// it names, and whether by ssh, whose config can give a host another name,
/// and the owner and the name of the repository there.
#[derive(Debug, PartialEq, Eq)]
struct Spelled<'a> {
    host: &'a str,
    ssh: bool,
    owner: &'a str,
    name: &'a str,
}

/// How git spells the remote's `url`, as `Spelled` has it. None for a path,
/// or a repository with no owner.
fn spelled(url: &str) -> Option<Spelled<'_>> {
    let (scheme, rest) = url.split_once("://").map_or((None, url), |(it, rest)| (Some(it), rest));
    let rest = match rest.split_once('@') {
        Some((user, host)) if !user.contains('/') => host,
        _ => rest,
    };
    // As scp spells it, `host:path`, with no `/` before the `:`, which git
    // takes for a path; or else `host:port/path`.
    let (host, path) = if scheme.is_some() {
        let (host, path) = rest.split_once('/')?;
        (host.split_once(':').map_or(host, |(host, _)| host), path)
    } else {
        rest.split_once(':').filter(|(host, _)| !host.contains('/'))?
    };
    let ssh = matches!(scheme, None | Some("ssh" | "git+ssh" | "ssh+git"));
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (named(owner) && named(name)).then_some(Spelled { host, ssh, owner, name })
}

/// The host ssh connects to for `host`, which its config can give another
/// name, as its `Host` and `HostName` do: as `ssh -G` says, which reads the
/// config and connects nowhere. None for what names no host, as what starts
/// with `-`, which ssh would take for an option.
fn ssh_host(host: &str) -> Option<String> {
    let spelled = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_');
    if host.is_empty() || host.starts_with('-') || !host.chars().all(spelled) {
        return None;
    }
    let mut ssh = Command::new("ssh");
    ssh.args(["-G", host]);
    let said = String::from_utf8(program::run(&mut ssh, ANSWER).ok()?.answer().ok()?).ok()?;
    said.lines().find_map(|line| line.strip_prefix("hostname ")).map(str::to_owned)
}

/// How a repository is the pull request's: by its `origin`, or by another of
/// its remotes, as the one a fork's clone forks. One by its origin goes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Remote {
    Origin,
    Other,
}

impl Remote {
    /// How the remote named `name` is, where it is of the pull request.
    fn of(name: &str) -> Remote {
        if name == ORIGIN { Remote::Origin } else { Remote::Other }
    }
}

/// A repository that is the pull request's: `by` which of its remotes, and
/// whether it is `bare`, as wt clones one, with its worktrees beside its
/// hidden `.bare`, or in it. Of one git cloned, wt would make the worktree in
/// its `.git`, and luneta has it make none there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Repository {
    by: Remote,
    bare: bool,
}

/// How the repository `dir` is in is the pull request `link`'s, if it is,
/// as `ssh_host` says what host ssh takes a name of its remotes for.
fn repository(
    dir: &str,
    link: &Link,
    ssh_host: impl FnMut(&str) -> Option<String>,
) -> Option<Repository> {
    let remote = remote(&remotes_in(dir)?, link, ssh_host)?;
    Some(Repository { by: Remote::of(&remote), bare: bare(dir) })
}

/// The name of the remote of `listed` that is of the pull request `link`'s
/// repository, if any is: origin, or else the first, as `git config
/// --get-regexp` prints them, `remote.<name>.url <url>` a line, as
/// `ssh_host` says what host ssh takes a name for.
fn remote(
    listed: &str,
    link: &Link,
    mut ssh_host: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let of = |line: &str| {
        let (key, url) = line.split_once(' ')?;
        let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
        link.of(url.trim(), &mut ssh_host).then(|| name.to_owned())
    };
    listed.lines().filter_map(of).min_by_key(|name| Remote::of(name))
}

/// A pane as the sidebar knows it, for luneta to find where a key of the
/// menu goes: its session and its id, where it works, the pull request of the
/// branch checked out there, by its page, as the daemon says, and whether
/// luneta's zellij is in its session. The sidebar hands them in the order of
/// its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub session: Listed,
    pub pane: u32,
    pub dir: String,
    pub pull: Option<Link>,
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
/// or the issue `link`, among the panes the sidebar knows, `worked`, and has
/// `wt add` make the worktree, on a thread of its own, and hands back what it
/// found, or why it found nothing.
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
    // wt asks GitHub for a pull request's branch, and finds none for an
    // issue's number; but it makes an issue's branch whatever the number is.
    if link.kind() == Kind::Issue {
        let page = page(link)?;
        if page.kind() == Kind::Pull {
            return Err(format!("it is a pull request on GitHub: paste {}", page.url()));
        }
    }
    let mut seen: HashMap<String, Option<Repository>> = HashMap::new();
    let mut hosts: HashMap<String, Option<String>> = HashMap::new();
    let mut repository_of = |dir: &str| {
        let host_of =
            |host: &str| hosts.entry(host.to_owned()).or_insert_with(|| ssh_host(host)).clone();
        *seen.entry(dir.to_owned()).or_insert_with(|| repository(dir, link, host_of))
    };
    let (repository, at) = match found(link, worked, &mut repository_of) {
        Some(Found::There(at)) => {
            let Worked { session, pane, dir, .. } = &worked[at];
            let at = At::There { session: session.clone(), pane: *pane };
            return Ok(Ready { at, worktree: dir.clone(), branch: branch(dir) });
        },
        Some(Found::Session(at)) => {
            let Worked { session, pane, dir, .. } = &worked[at];
            (dir.clone(), At::Session { session: session.clone(), pane: *pane })
        },
        None => match clone(link, &mut repository_of)? {
            Some(clone) => clone,
            None => return Err(no_clone(link, &seen)),
        },
    };
    if let Some(branch) = link.branch() {
        base(&repository, link, &branch)?;
    }
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

/// Where a key goes for `link` among the panes `worked`: the pane that has
/// the pull request's branch checked out, or else a pane that works in its
/// repository, where wt cloned it, by how `repository_of` says a directory's
/// repository is the pull request's, if it is. Of either, one in the session
/// luneta's zellij is in goes first; of a repository's, then one that is the
/// pull request's by its origin; then the first row.
fn found(
    link: &Link,
    worked: &[Worked],
    mut repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Option<Found> {
    let checked_out = |(_, it): &(usize, &Worked)| it.pull.as_ref().is_some_and(|it| it.is(link));
    let there = worked.iter().enumerate().filter(checked_out);
    let there = there.min_by_key(|(at, it)| (!it.here, *at));
    if let Some((at, _)) = there {
        return Some(Found::There(at));
    }
    let mut session: Option<(bool, Remote, usize)> = None;
    for (at, it) in worked.iter().enumerate() {
        let Some(Repository { by, bare: true }) = repository_of(&it.dir) else {
            continue;
        };
        let rank = (!it.here, by, at);
        session = Some(session.map_or(rank, |was| was.min(rank)));
    }
    session.map(|(_, _, at)| Found::Session(at))
}

/// The clone of `link`'s repository that wt made among the directories
/// zoxide knows, by how `repository_of` says a directory's repository is the
/// pull request's: a directory of it, for `wt add`, and the session luneta
/// makes for it, in the directory that holds it, named as the picker names
/// that directory, with a `-2` where a session has the name: one that runs,
/// or one that ended, which zellij keeps to bring back. None where zoxide
/// knows none; or why luneta cannot tell.
fn clone(
    link: &Link,
    repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Result<Option<(String, At)>, String> {
    let repository = link.repository();
    let dirs: Vec<String> = dirs::visited()
        .map_err(|why| {
            format!("no session works in {repository}, and luneta cannot look for a clone: {why}")
        })?
        .into_iter()
        .map(|dir| dir.path)
        .collect();
    let Some(dir) = cloned(&dirs, repository_of) else {
        return Ok(None);
    };
    let dir = dirs[dir].clone();
    let cwd = holder(&dir).ok_or_else(|| format!("git does not say what repository {dir} is"))?;
    let name = zellij::session_for(Path::new(&cwd))
        .ok_or_else(|| format!("no session can be named for {cwd}"))?;
    let named =
        zellij::named().ok_or_else(|| "luneta cannot tell which sessions zellij has".to_owned())?;
    let name = name.free(&named.iter().map(String::as_str).collect::<Vec<_>>());
    Ok(Some((dir, At::Clone { cwd, name })))
}

/// Why no clone of `link`'s repository is for `wt add`, as what luneta has
/// `seen` of each directory it looked in says: there is none, or one git
/// cloned, in whose `.git` wt would make the worktree.
fn no_clone(link: &Link, seen: &HashMap<String, Option<Repository>>) -> String {
    let repository = link.repository();
    let by_git = seen.iter().filter(|(_, it)| it.is_some_and(|it| !it.bare));
    match by_git.map(|(dir, _)| dir).min() {
        Some(dir) => format!(
            "{dir} is in a clone of {repository} that git made, and wt would make the worktree \
             in its .git; wt clone it first"
        ),
        None => format!(
            "no session works in {repository}, nor does zoxide know a clone of it; wt clone it \
             first"
        ),
    }
}

/// Which of `dirs`, best first, is of the pull request's repository, where
/// wt cloned it, by how `repository_of` says a directory's is: the first by
/// its origin, or else the first by another remote.
fn cloned(
    dirs: &[String],
    mut repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Option<usize> {
    let mut best: Option<(Remote, usize)> = None;
    for (at, dir) in dirs.iter().enumerate() {
        if let Some(Repository { by, bare: true }) = repository_of(dir) {
            best = Some(best.map_or((by, at), |was| was.min((by, at))));
            if by == Remote::Origin {
                break;
            }
        }
    }
    best.map(|(_, at)| at)
}

/// The directory that holds the bare repository `dir` is in, as wt keeps its
/// worktrees: beside its hidden `.bare`, or in it, where it is not hidden.
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

/// Whether the repository `dir` is in is bare, as git says.
fn bare(dir: &str) -> bool {
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, "config", "--type=bool", "--get", "core.bare"]));
    said(&mut git).as_deref() == Some("true")
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

/// The page on GitHub of `link`'s number, as gh says: `gh issue view` answers
/// of a pull request's number too, with the pull request's page. Or why gh
/// did not say, as that GitHub has none of the number, or that gh is not
/// logged in.
fn page(link: &Link) -> Result<Link, String> {
    let number = link.number.to_string();
    let repository = format!("{GITHUB}/{}", link.repository());
    let mut gh = Command::new("gh");
    gh.args(["issue", "view", &number, "--repo", &repository, "--json", "url", "--jq", ".url"]);
    let ran = program::run(&mut gh, ASKING)
        .map_err(|error| format!("gh issue view could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh issue view: {why}"))?;
    let said = String::from_utf8_lossy(&said);
    let said = said.trim();
    Link::parse(said).ok_or_else(|| format!("gh issue view said {said}, which is no link"))
}

/// Makes `branch`, the issue `link`'s, in the repository `dir` is in, where
/// neither it nor origin has one yet: from the default branch of the issue's
/// repository as the remote that is of it has it now, with no upstream, as
/// wt makes a new branch. wt would make it from origin's, as the repository
/// last fetched it, which can be weeks behind, and of the fork in a fork's
/// clone. Where either has the branch, wt goes on with it: one origin has
/// may be one pushed from another machine. Or why it made none.
fn base(dir: &str, link: &Link, branch: &str) -> Result<(), String> {
    let head = format!("refs/heads/{branch}");
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, "rev-parse", "--verify", "--quiet", &head]));
    if said(&mut git).is_some() {
        return Ok(());
    }
    let remote = remotes_in(dir).and_then(|listed| remote(&listed, link, ssh_host));
    let remote = remote
        .ok_or_else(|| format!("git no longer says a remote of {dir} is {}", link.repository()))?;
    let pushed = across(dir, "ls-remote", &[ORIGIN, &head])?;
    let pushed = String::from_utf8_lossy(&pushed);
    if pushed.lines().any(|line| line.split('\t').nth(1) == Some(head.as_str())) {
        return Ok(());
    }
    across(dir, "fetch", &[&remote, &format!("HEAD:{head}")]).map(drop)
}

/// What git says, as it runs `command` with `args` in the repository `dir`
/// is in, which asks a remote across the network: alone, as ssh may ask for
/// a passphrase. Or why it said nothing.
fn across(dir: &str, command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    lunetad::local_git(git.args(["-C", dir, command]).args(args));
    let ran = program::run_alone(&mut git, MAKING)
        .map_err(|error| format!("git {command} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("git {command}: {why}"))
}

/// Has `wt add` make a worktree of the pull request or the issue `link` in
/// the repository `dir` is in, and says where it is, or where one of its
/// branch is already: the last line wt prints, as the real path of the
/// directory it names. Or why it made none, as wt says it.
fn worktree(dir: &str, link: &Link) -> Result<String, String> {
    let mut wt = Command::new("wt");
    lunetad::local_git(wt.args(["add", "--repo", dir, &link.wt()]));
    // It fetches across the network, by ssh as like as not.
    let ran = program::run_alone(&mut wt, MAKING)
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
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    use super::{
        Found, Kind, Link, Remote, Repository, Worked, base, cloned, failure, found, no_clone,
        remote,
    };
    use crate::sidebar::Listed;

    fn link() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    fn listed(name: &str) -> Listed {
        Listed::new(name, SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    /// A pane in `dir`, with the pull request of the page `pull` checked out.
    fn worked(session: &str, pane: u32, dir: &str, pull: Option<&str>, here: bool) -> Worked {
        let pull = pull.map(|page| Link::parse(page).expect("a pull request's page"));
        Worked { session: listed(session), pane, dir: dir.to_owned(), pull, here }
    }

    /// A repository wt cloned, that is the pull request's `by` a remote.
    fn by_wt(by: Remote) -> Repository {
        Repository { by, bare: true }
    }

    /// One git cloned, that is the pull request's by its origin.
    const BY_GIT: Repository = Repository { by: Remote::Origin, bare: false };

    fn issue() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link")
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
            "https://github.com/cashubtc/cdk/discussions/2736",
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
        assert_eq!(link().wt(), "https://github.com/cashubtc/cdk/pull/2736");
    }

    /// An issue's link names it as a pull request's does, but for `issues`:
    /// the agent takes it, and `wt add` the branch luneta has it make, or
    /// find where a paste made it before. It is the pull request of its
    /// number, as GitHub counts the two in one sequence.
    #[test]
    fn a_link_is_of_an_issue_or_of_a_page_of_one() {
        for text in [
            "https://github.com/cashubtc/cdk/issues/2736",
            "github.com/CashuBTC/cdk/issues/2736#issuecomment-1\n",
        ] {
            let link = Link::parse(text).expect("an issue's link");
            assert_eq!(link.kind(), Kind::Issue, "{text:?}");
            assert!(link.is(&issue()), "{text:?}");
        }
        assert_eq!(issue().to_string(), "cashubtc/cdk#2736");
        assert_eq!(issue().url(), "https://github.com/cashubtc/cdk/issues/2736");
        assert_eq!(issue().tab(), "#2736");
        assert_eq!(issue().wt(), "issue-2736");
        // GitHub has one #2736 of cdk, of whichever page a link is.
        assert!(issue().is(&link()));
        assert_eq!(Link::parse("https://github.com/cashubtc/cdk/issues/0"), None);
    }

    /// ssh's config gives GitHub another name, `github-work`, as its `Host`
    /// and `HostName` do.
    fn ssh_host(host: &str) -> Option<String> {
        (host == "github-work").then(|| "github.com".to_owned())
    }

    /// A remote is of the pull request's repository by ssh or https, on
    /// GitHub by its name, or by ssh by another that ssh's config gives it,
    /// which https takes no account of.
    #[test]
    fn a_remote_is_of_a_repository_by_ssh_or_https_case_aside() {
        for url in [
            "git@github.com:cashubtc/cdk.git",
            "git@github.com:cashubtc/cdk",
            "https://github.com/cashubtc/cdk.git",
            "https://github.com/CashuBTC/CDK/",
            "ssh://git@github.com/cashubtc/cdk.git",
            "https://token@github.com/cashubtc/cdk",
            "git@github-work:cashubtc/cdk.git",
            "ssh://git@github-work:22/cashubtc/cdk",
        ] {
            assert!(link().of(url, ssh_host), "{url}");
        }
        for url in [
            "git@gitlab.com:cashubtc/cdk.git",
            "/srv/git/cdk.git",
            "/srv/github.com:cashubtc/cdk",
            "git@github.com:cdk",
            "git@github.com:lorenzolfm/cdk.git",
            "https://github-work/cashubtc/cdk",
        ] {
            assert!(!link().of(url, ssh_host), "{url}");
        }
    }

    /// A fork's clone has the repository it forks among its remotes, by any
    /// name: the pull request is of it as well as of a clone of that one. Of
    /// those that are, origin goes first, and then the first.
    #[test]
    fn a_repository_is_the_pull_request_s_by_any_remote_its_origin_first() {
        let fork = "remote.cashubtc.url git@github.com:cashubtc/cdk.git\n\
                    remote.origin.url git@github.com:lorenzolfm/cdk.git\n\
                    remote.upstream.url https://github.com/cashubtc/cdk\n";
        assert_eq!(remote(fork, &link(), ssh_host).as_deref(), Some("cashubtc"));
        let clone = "remote.upstream.url https://github.com/cashubtc/cdk\n\
                     remote.origin.url https://github.com/cashubtc/cdk\n";
        assert_eq!(remote(clone, &link(), ssh_host).as_deref(), Some("origin"));
        let other = "remote.origin.url git@github.com:lorenzolfm/cdk.git\n";
        assert_eq!(remote(other, &link(), ssh_host), None);
        assert_eq!(remote("", &link(), ssh_host), None);
    }

    /// The pane that has the pull request's branch checked out wins over a
    /// session that works in its repository, wherever it is: by the pull
    /// request's page, case aside, as the daemon says it, so that one of
    /// another repository's with its number, as a fork's own, is not it. Of
    /// each, one in the session luneta's zellij is in goes first; of a
    /// session, then one whose repository is the pull request's by its
    /// origin, then the first row. A repository git cloned has no session for
    /// `wt add`, though a pane with the branch checked out in it is still
    /// where the branch is.
    #[test]
    fn a_key_goes_to_the_pane_with_the_branch_or_else_a_session_of_the_repository() {
        let remotes = |dir: &str| match dir {
            "/cdk/main" | "/cdk/2736" => Some(by_wt(Remote::Other)),
            "/cdk-upstream/master" => Some(by_wt(Remote::Origin)),
            "/cdk-git" => Some(BY_GIT),
            _ => None,
        };
        let page = |repository, number| format!("https://github.com/{repository}/pull/{number}");
        let (cdk, before) = (page("CashuBTC/cdk", 2736), page("cashubtc/cdk", 2735));
        let fork = page("lorenzolfm/cdk", 2736);
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
        let by_git =
            [worked("dots", 1, "/cdk-git", None, true), worked("cdk", 2, "/cdk/main", None, false)];
        assert_eq!(found(&link(), &by_git, remotes), Some(Found::Session(1)));
        assert_eq!(found(&link(), &by_git[..1], remotes), None);
        let checked_out = [
            worked("dots", 4, "/cdk/main", None, true),
            worked("upstream", 3, "/cdk-upstream/master", Some(&before), false),
            worked("git", 5, "/cdk-git", Some(&cdk), false),
        ];
        assert_eq!(found(&link(), &checked_out, remotes), Some(Found::There(2)));
        let fork = [worked("cdk", 1, "/cdk/main", Some(&fork), true)];
        assert_eq!(found(&link(), &fork, remotes), Some(Found::Session(0)));
    }

    /// Of the directories zoxide knows, the clone wt made that is the pull
    /// request's by its origin goes first, or else the first by another
    /// remote. One git cloned is none, by any remote.
    #[test]
    fn a_clone_is_the_first_by_its_origin_or_else_by_another_remote() {
        let dirs = ["/a", "/git", "/fork/main", "/b", "/upstream", "/fork"].map(str::to_owned);
        let remotes = |dir: &str| match dir {
            "/fork/main" | "/fork" => Some(by_wt(Remote::Other)),
            "/upstream" => Some(by_wt(Remote::Origin)),
            "/git" => Some(BY_GIT),
            _ => None,
        };
        assert_eq!(cloned(&dirs, remotes), Some(4));
        assert_eq!(cloned(&dirs[..4], remotes), Some(2));
        assert_eq!(cloned(&dirs[..2], remotes), None);
    }

    /// With no clone for `wt add`, luneta says to have wt clone one, and
    /// names one git made where it saw one, as wt would make the worktree in
    /// its `.git`.
    #[test]
    fn with_no_clone_for_wt_luneta_says_of_one_git_made() {
        let seen = |seen: &[(&str, Option<Repository>)]| -> HashMap<String, Option<Repository>> {
            seen.iter().map(|(dir, it)| ((*dir).to_owned(), *it)).collect()
        };
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", None)])),
            "no session works in cashubtc/cdk, nor does zoxide know a clone of it; wt clone it \
             first"
        );
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", None), ("/w/cdk/src", Some(BY_GIT))])),
            "/w/cdk/src is in a clone of cashubtc/cdk that git made, and wt would make the \
             worktree in its .git; wt clone it first"
        );
    }

    /// git as the test runs it, on the repository at `dir`, with no config but
    /// its own, as no one in particular: what it said, where it ended well.
    fn git(dir: impl AsRef<Path>, args: &[&str]) -> Option<String> {
        let mut git = Command::new("git");
        lunetad::local_git(&mut git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(dir.as_ref())
            .args(args);
        let ran = git.output().unwrap();
        let said = String::from_utf8(ran.stdout).unwrap();
        ran.status.success().then(|| said.trim_end().to_owned())
    }

    /// An issue's branch is made from the default branch of the issue's
    /// repository as it is now, by the remote that is of it, and not from
    /// origin's, the fork's, as the clone last fetched it. It has no
    /// upstream. Made once, it stays where it was made; and one origin has,
    /// as pushed from another machine, is left for wt.
    #[test]
    fn an_issue_s_branch_is_made_from_its_repository_s_default_branch_as_it_is_now() {
        let scratch = tempfile::tempdir().unwrap();
        let path = |name: &str| scratch.path().join(name).to_str().unwrap().to_owned();
        let (cdk, fork, clone) = (path("cdk"), path("fork.git"), path("clone"));
        let empty = |dir: &str, message: &str| {
            assert!(git(dir, &["commit", "-q", "--allow-empty", "-m", message]).is_some());
        };
        assert!(git(scratch.path(), &["init", "-q", "-b", "main", &cdk]).is_some());
        empty(&cdk, "one");
        assert!(git(scratch.path(), &["clone", "-q", "--bare", &cdk, &fork]).is_some());
        assert!(git(scratch.path(), &["clone", "-q", &fork, &clone]).is_some());
        // cdk's remote in the fork's clone, by its name on GitHub, which git
        // takes for the scratch cdk.
        let github = "git@github.com:cashubtc/cdk.git";
        assert!(git(&clone, &["remote", "add", "cashubtc", github]).is_some());
        assert!(git(&clone, &["config", &format!("url.{cdk}.insteadOf"), github]).is_some());
        empty(&cdk, "two");
        let now = git(&cdk, &["rev-parse", "HEAD"]);
        let made = || git(&clone, &["rev-parse", "refs/heads/issue-2736"]);
        assert_eq!(base(&clone, &issue(), "issue-2736"), Ok(()));
        assert_eq!(made(), now);
        assert_eq!(git(&clone, &["config", "branch.issue-2736.remote"]), None);
        empty(&cdk, "three");
        assert_eq!(base(&clone, &issue(), "issue-2736"), Ok(()));
        assert_eq!(made(), now);
        assert!(git(&fork, &["branch", "issue-7", "main"]).is_some());
        let pushed = Link::parse("https://github.com/cashubtc/cdk/issues/7").expect("a link");
        assert_eq!(base(&clone, &pushed, "issue-7"), Ok(()));
        assert_eq!(git(&clone, &["rev-parse", "--verify", "-q", "refs/heads/issue-7"]), None);
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
