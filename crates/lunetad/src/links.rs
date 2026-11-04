//! A pull request's link, pasted in luneta, and where the key of the menu
//! it opens goes for it, on the machine of the row it was pasted on: a
//! worktree of the pull request, in a tab of the session that works in its
//! repository, or the same with an agent on a review of it. An issue's link
//! opens it as well, for a worktree of a branch of its own, or the same with
//! an agent on the issue. luneta finds that for a row of its own machine,
//! and a daemon for a luneta on another, whose link was pasted on a row of
//! this one: git, gh, ssh's config, zoxide and the worktree are the
//! machine's.
//!
//! The key goes, of the panes luneta says of the machine, to the one in the
//! session its zellij is in, where a pane of it works in the repository, or
//! else another that does, or else to a session to make, in a clone of the
//! repository that zoxide knows. A repository is the pull request's where
//! any of its remotes is the repository on GitHub the link is of: a fork's
//! clone has the repository it forks among its remotes, and that is the one
//! its pull requests are of. wt's code makes the worktree, as `wt add` does,
//! and says where it is, or where one of the pull request's branch is
//! already, in a repository wt cloned: in one git cloned, wt would make it in
//! its `.git`. luneta has it in its own program, which it runs as `luneta
//! worktree`, and needs no `wt` on the `PATH`. An issue's branch is
//! `issue-123`, so a second paste of the link finds the first's worktree.
//! Where neither the repository nor origin has it yet, it is made from the
//! default branch of the issue's repository as GitHub has it then, by the
//! remote that is of it: wt would make it from origin's, as the repository
//! last fetched it, and a fork's clone has the fork for its origin. gh says
//! first that GitHub has an issue of its number, and no pull request: a link
//! can say an issue of any number, and luneta would make a branch of each. A
//! pane that has the pull request's branch checked out, as the daemon says by
//! its page, or that works in the worktree wt's code says, is where a key
//! goes instead of a new tab.
//!
//! A `.md` file's path opens it too, for work written down there and not on
//! GitHub. It names no repository: the one the file is in is its own, by what
//! git says of the directory it is in, and a file in none goes by the
//! repository of the row it was pasted on. A pane works in that repository
//! where git says the same of its directory, and a session luneta makes
//! starts in the directory that holds it. The branch and the tab are named for
//! the file, so a second paste finds the first's worktree, and so do two files
//! of one name. Where neither the repository nor origin has the branch yet, it
//! is made from origin's default branch as it is then. The agent reads the file
//! where it is, by its real path: the worktree may not have it.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use wt::cmd::add::{self, Fetch};
use wt::repo::Repo;

use crate::api::paste_request::{self, File as AskedFile, Worked as AskedWorked};
use crate::api::paste_response::{self, Clone as MadeClone};
use crate::api::{On, PasteRequest, PasteResponse};
use crate::program::{self, Ran};
use crate::{git, github, names, zoxide, SessionName};

/// The host a pull request's link is on, and the remotes of its repository.
const GITHUB: &str = "github.com";

/// How long `luneta worktree` has to make a worktree, as wt fetches the pull
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

/// How long luneta waits for where a key goes, found on another machine:
/// gh asks GitHub once, and git and wt go across the network three times at
/// the most, each for as long as `MAKING`; and a minute for the rest, which
/// reads files.
pub const PATIENCE: Duration = Duration::from_secs(3 * MAKING.as_secs() + ASKING.as_secs() + 60);

/// The most panes a daemon is asked to find where a key goes among: more
/// than any machine has, and as many directories git is asked about.
const WORKED_MAX: usize = 1024;

/// The remote of the repository a clone was cloned from, as git names it:
/// a branch wt makes goes there.
const ORIGIN: &str = "origin";

/// What `luneta worktree` starts the line with that says why it made no
/// worktree, on stderr, after what wt and git say there as they go, as
/// luneta's commands say why they failed.
pub const WHY: &str = "luneta: ";

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
    #[must_use]
    pub fn parse(text: &str) -> Option<Link> {
        let rest = on_github(text)?;
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
    #[must_use]
    pub fn url(&self) -> String {
        let Link { owner, name, kind, number } = self;
        format!("https://{GITHUB}/{owner}/{name}/{}/{number}", kind.path())
    }

    #[must_use]
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

    /// What `luneta worktree` makes the worktree of for it: the pull request,
    /// for wt to find its branch, or an issue's branch.
    fn wt(&self) -> Of {
        match self.branch() {
            Some(branch) => Of::Branch(branch),
            None => Of::Pull(self.clone()),
        }
    }

    /// What its tab is named, and the menu calls it: `#123`.
    #[must_use]
    pub fn tab(&self) -> String {
        format!("#{}", self.number)
    }

    /// Its repository, as `owner/name`.
    #[must_use]
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Whether `other` is the same pull request or issue, case aside in the
    /// names of its repository, as GitHub takes them: by its number, of
    /// whichever page either link is, as GitHub has one of the number.
    #[must_use]
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

/// The end of the name of a file a paste opens the menu for.
const MARKDOWN: &str = ".md";

/// A `.md` file, by its path as it was pasted on a row of a machine, which is
/// where it is read: from the root, from `~/`, or else from the directory
/// that row works in, where it works in one. The branch and the tab of its
/// worktree are named for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    path: String,
    from: Option<String>,
    branch: String,
}

impl File {
    /// The file `text` is a path of, but for the space around it, pasted on a
    /// row that works in `from`: one line that ends in `.md`, whether or not
    /// any file has that path, which only its machine can say. None for any
    /// other text, an address too, or for a file whose name holds nothing to
    /// name a branch for.
    #[must_use]
    pub fn parse(text: &str, from: Option<&str>) -> Option<File> {
        let path = text.trim();
        if path.contains("://") || path.chars().any(char::is_control) {
            return None;
        }
        let name = path.rsplit('/').next()?;
        let branch = slug(name.strip_suffix(MARKDOWN)?);
        let (path, from) = (path.to_owned(), from.map(str::to_owned));
        (!branch.is_empty()).then_some(File { path, from, branch })
    }

    /// Its path, as it was pasted.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Its real path, on the machine this runs on; or why it names no file
    /// there.
    fn real(&self) -> Result<String, String> {
        let real = fs::canonicalize(self.absolute()?).map_err(|error| match error.kind() {
            ErrorKind::NotFound => "no such file".to_owned(),
            _ => error.to_string(),
        })?;
        if !real.is_file() {
            return Err("it is no file".to_owned());
        }
        real.into_os_string().into_string().map_err(|_| "its real path is no text".to_owned())
    }

    /// Its path from the root, on the machine this runs on, as the machine's
    /// `HOME` starts one from `~/`, and the row it was pasted on a relative
    /// one; or why it has none.
    fn absolute(&self) -> Result<PathBuf, String> {
        if let Some(rest) = self.path.strip_prefix("~/") {
            let home = env::var_os("HOME").map(PathBuf::from);
            let home = home.filter(|home| home.is_absolute());
            let home = home.ok_or("HOME is not set to an absolute path, for its ~/")?;
            return Ok(home.join(rest));
        }
        if self.path.starts_with('/') {
            return Ok(PathBuf::from(&self.path));
        }
        let from = self.from.as_ref().ok_or(
            "its path is relative, and the row highlighted works in no directory to go from",
        )?;
        Ok(Path::new(from).join(&self.path))
    }

    /// The repository its worktree is made in, once `real` is where it is: a
    /// directory of it, and its common directory, by which a pane works in
    /// it. The file's own, or else, for a file in none, the one the row it was
    /// pasted on works in. Or why it has neither.
    fn repository(&self, real: &str) -> Result<(String, String), String> {
        let beside = Path::new(real).parent().and_then(Path::to_str).unwrap_or(real);
        if let Some(common) = common_dir(beside) {
            return Ok((beside.to_owned(), common));
        }
        let from = self.from.as_ref().ok_or(
            "it is in no repository, and the row highlighted works in no directory to go by",
        )?;
        let common = common_dir(from)
            .ok_or_else(|| format!("it is in no repository, nor is {from}, the row highlighted"))?;
        Ok((from.clone(), common))
    }
}

/// `stem` as a branch and a tab are named for it: in lower case, with `-` for
/// each run of what is neither a letter, a digit nor `_`, and none at either
/// end. Nothing git refuses in a branch's name is left, as `..`, a space or a
/// `.lock` at the end. Empty where nothing is left.
fn slug(stem: &str) -> String {
    let mut slug = String::with_capacity(stem.len());
    for c in stem.chars() {
        if c.is_alphanumeric() || c == '_' {
            slug.extend(c.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(slug.trim_end_matches('-').len());
    slug
}

/// What a paste opens the menu for, whose keys make a worktree for it: a pull
/// request or an issue, by its link, or a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    Link(Link),
    File(File),
}

impl Work {
    /// What `text` pasted names, on a row that works in `from`: a link, as
    /// `Link::parse` takes one, or else a file, as `File::parse` takes one.
    #[must_use]
    pub fn parse(text: &str, from: Option<&str>) -> Option<Work> {
        let link = Link::parse(text).map(Work::Link);
        link.or_else(|| File::parse(text, from).map(Work::File))
    }

    /// What its tab is named, and the menu calls it.
    #[must_use]
    pub fn tab(&self) -> String {
        match self {
            Work::Link(link) => link.tab(),
            Work::File(file) => file.branch.clone(),
        }
    }
}

impl fmt::Display for Work {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Work::Link(link) => link.fmt(f),
            Work::File(file) => f.write_str(&file.path),
        }
    }
}

/// What `text` names on GitHub, after its host and a `/`, where it is an
/// address there, with or without `https://` or `www.`, but for the space
/// around it.
fn on_github(text: &str) -> Option<&str> {
    let text = text.trim();
    let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"));
    let rest = rest.unwrap_or(text);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.strip_prefix(GITHUB)?.strip_prefix('/')
}

/// What `luneta worktree` makes the worktree of: a pull request, whose
/// branch wt finds by its link, or a branch, by its name. git takes a link
/// without its `https://` for a branch's name, and wt, which tells a link by
/// its `://`, would make a new branch of that name, with none of the pull
/// request's commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Of {
    Pull(Link),
    Branch(String),
}

impl Of {
    /// What `text` names: a pull request, where it is one's link as
    /// `Link::parse` takes one, or else a branch, by any name but an address
    /// on GitHub or another link. Not an issue's link: luneta's paste makes
    /// its branch, from the default branch of its repository as GitHub has
    /// it, before it has the worktree made.
    pub fn parse(text: &str) -> Result<Of, String> {
        let Some(link) = Link::parse(text) else {
            if text.contains("://") || on_github(text).is_some() {
                return Err(format!("{} is no pull request's link on {GITHUB}", text.trim()));
            }
            return Ok(Of::Branch(text.to_owned()));
        };
        match link.wt() {
            Of::Branch(branch) => {
                Err(format!("{link} is an issue: make the worktree of its branch, {branch}"))
            },
            pull @ Of::Pull(_) => Ok(pull),
        }
    }
}

impl fmt::Display for Of {
    /// As wt takes it: a pull request's link in full, which wt tells from a
    /// branch by its `://`, or the branch.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Of::Pull(link) => f.write_str(&link.url()),
            Of::Branch(branch) => f.write_str(branch),
        }
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

/// How a repository is laid out, as git says by its `core.bare`: bare, as wt
/// clones one, with its worktrees beside its hidden `.bare`, or in it; or
/// with a work tree, as git clones one, in whose `.git` wt would make the
/// worktree, and luneta has it make none. Or git did not say, as where it
/// did not answer in time, which is neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Wt,
    Git,
    Unsaid,
}

/// A repository that is the pull request's: `by` which of its remotes, and
/// how it is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Repository {
    by: Remote,
    layout: Layout,
}

/// How the repository `dir` is in is the pull request `link`'s, if it is,
/// as `ssh_host` says what host ssh takes a name of its remotes for.
fn repository(
    dir: &str,
    link: &Link,
    ssh_host: impl FnMut(&str) -> Option<String>,
) -> Option<Repository> {
    let remote = remote(&remotes_in(dir)?, link, ssh_host)?;
    Some(Repository { by: Remote::of(&remote), layout: layout(dir) })
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

/// A pane of the machine, as luneta knows it, for where a key of the menu
/// goes: where it works, the pull request of the branch checked out there,
/// by its page, as the daemon says, and whether luneta's zellij is in its
/// session. luneta says them in the order of its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub dir: String,
    pub pull: Option<Link>,
    pub here: bool,
}

/// Where a key of the menu goes, as found among the panes of a machine,
/// each of which `P` names: by its place among them, as the daemon says it,
/// or as luneta knows the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At<P> {
    /// The pane that has the pull request's branch checked out.
    There(P),
    /// A tab of the session of a pane that works in the pull request's
    /// repository: luneta's zellij goes to that pane first, where it is in
    /// another session, as a tab needs a client of its session.
    Session(P),
    /// A tab of a session luneta makes, `name`, that starts in `cwd`, where
    /// the clone of the repository is. No session the machine's zellij has
    /// was called so as it was named: luneta has it named again just before
    /// it makes the session, as a while can go by first.
    Clone { cwd: String, name: SessionName },
}

/// What a key of the menu goes on with: where it goes, the worktree of the
/// pull request, the branch checked out there, which an agent is named for,
/// and the real path of the file, where a file was pasted, which an agent
/// reads where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready<P> {
    pub at: At<P>,
    pub worktree: String,
    pub branch: Option<String>,
    pub file: Option<String>,
}

impl Ready<usize> {
    /// Where it goes, with each pane named as `among` names the pane of its
    /// place there; or the place of one that `among` has none of.
    pub fn placed<P: Clone>(self, among: &[P]) -> Result<Ready<P>, usize> {
        let Ready { at, worktree, branch, file } = self;
        let pane = |at: usize| among.get(at).cloned().ok_or(at);
        let at = match at {
            At::There(at) => At::There(pane(at)?),
            At::Session(at) => At::Session(pane(at)?),
            At::Clone { cwd, name } => At::Clone { cwd, name },
        };
        Ok(Ready { at, worktree, branch, file })
    }

    /// The answer that says it.
    #[must_use]
    pub fn answer(self) -> PasteResponse {
        let Ready { at, worktree, branch, file } = self;
        // A place is of a question's panes, which are no more than
        // `WORKED_MAX`.
        let place = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        let at = match at {
            At::There(at) => paste_response::At::There(place(at)),
            At::Session(at) => paste_response::At::Session(place(at)),
            At::Clone { cwd, name } => {
                paste_response::At::Clone(MadeClone { cwd, name: name.to_string() })
            },
        };
        PasteResponse { at: Some(at), worktree, branch, file }
    }

    /// Where `answer` says the key goes, or why it says nothing luneta can
    /// go by: it names a session zellij would not take, or a worktree, a file
    /// or a directory for the session by no absolute path, or it says where
    /// in words of a newer daemon's.
    pub fn of(answer: PasteResponse) -> Result<Self, String> {
        let PasteResponse { at, worktree, branch, file } = answer;
        if !worktree.starts_with('/') {
            return Err(format!("worktree: {worktree:?} is no absolute path"));
        }
        if let Some(file) = file.as_ref().filter(|file| !file.starts_with('/')) {
            return Err(format!("file: {file:?} is no absolute path"));
        }
        let place = |at: u32| usize::try_from(at).unwrap_or(usize::MAX);
        let at = match at {
            Some(paste_response::At::There(at)) => At::There(place(at)),
            Some(paste_response::At::Session(at)) => At::Session(place(at)),
            Some(paste_response::At::Clone(MadeClone { cwd, name })) => {
                if !cwd.starts_with('/') {
                    return Err(format!("clone: {cwd:?} is no absolute path"));
                }
                let name = SessionName::parse(&name)
                    .map_err(|why| format!("clone: \"{name}\" names no session: {why}"))?;
                At::Clone { cwd, name }
            },
            None => {
                return Err("it says where the key goes in words this luneta cannot read, as a \
                            newer lunetad does"
                    .to_owned());
            },
        };
        Ok(Ready { at, worktree, branch, file })
    }
}

/// A link or a file pasted on a row of a machine, and the panes of that
/// machine, as luneta knows them, for where a key of the menu goes: on that
/// machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pasted {
    work: Work,
    worked: Vec<Worked>,
}

impl Pasted {
    #[must_use]
    pub fn new(work: Work, worked: Vec<Worked>) -> Self {
        Pasted { work, worked }
    }

    /// What `request` asks, and of which machine, or why it is invalid: it
    /// says neither a link nor a path of a `.md` file, or a row's directory
    /// by no absolute path, or more panes than `WORKED_MAX`, or a pane by no
    /// absolute path, or a pull request's page that is no link.
    pub fn of(request: PasteRequest) -> Result<(Option<On>, Pasted), String> {
        let PasteRequest { on, work, worked } = request;
        let work = match work {
            Some(paste_request::Work::Link(link)) => {
                Work::Link(Link::parse(&link).ok_or_else(|| {
                    format!(
                        "link: {link:?} is no link of a pull request or of an issue on {GITHUB}"
                    )
                })?)
            },
            Some(paste_request::Work::File(AskedFile { path, from })) => {
                if let Some(from) = from.as_ref().filter(|from| !from.starts_with('/')) {
                    return Err(format!("from: {from:?} is no absolute path"));
                }
                let file = File::parse(&path, from.as_deref());
                Work::File(file.ok_or_else(|| format!("file: {path:?} is no path of a .md file"))?)
            },
            None => return Err("it says neither a link nor a file".to_owned()),
        };
        if worked.len() > WORKED_MAX {
            return Err(format!("worked: more than {WORKED_MAX} panes"));
        }
        let pane = |AskedWorked { dir, pull, here }| {
            if !dir.starts_with('/') {
                return Err(format!("worked: {dir:?} is no absolute path"));
            }
            let pull = pull
                .map(|page| Link::parse(&page).ok_or(page))
                .transpose()
                .map_err(|page| format!("worked: {page:?} is no pull request's page"))?;
            Ok(Worked { dir, pull, here })
        };
        let worked = worked.into_iter().map(pane).collect::<Result<_, String>>()?;
        Ok((on, Pasted { work, worked }))
    }

    /// The question that asks it of the machine `on` names, or of the
    /// daemon's own, where it names none.
    #[must_use]
    pub fn asked(&self, on: Option<On>) -> PasteRequest {
        let pane = |Worked { dir, pull, here }: &Worked| AskedWorked {
            dir: dir.clone(),
            pull: pull.as_ref().map(Link::url),
            here: *here,
        };
        let worked = self.worked.iter().map(pane).collect();
        let work = match &self.work {
            Work::Link(link) => paste_request::Work::Link(link.url()),
            Work::File(File { path, from, .. }) => {
                paste_request::Work::File(AskedFile { path: path.clone(), from: from.clone() })
            },
        };
        PasteRequest { on, work: Some(work), worked }
    }

    /// Where a key goes, on the machine this runs on, by the place of its
    /// pane among those luneta said, and the worktree, which it has `luneta
    /// worktree` make there, or where one of the branch is already; or why
    /// it found nothing.
    pub fn ready(&self) -> Result<Ready<usize>, String> {
        match &self.work {
            Work::Link(link) => ready(link, &self.worked),
            Work::File(file) => ready_file(file, &self.worked),
        }
    }
}

/// Where a key goes for `link` among the panes `worked`, as `Pasted::ready`
/// says.
fn ready(link: &Link, worked: &[Worked]) -> Result<Ready<usize>, String> {
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
    let (repository, going) = match found(link, worked, &mut repository_of) {
        Some(Found::There(at)) => {
            let dir = &worked[at].dir;
            let (worktree, branch) = (dir.clone(), branch(dir));
            return Ok(Ready { at: At::There(at), worktree, branch, file: None });
        },
        Some(Found::Session(at)) => (worked[at].dir.clone(), Going::Session(at)),
        None => match clone(link, &mut repository_of)? {
            Some((dir, cwd, name)) => (dir, Going::Clone { cwd, name }),
            None => return Err(no_clone(link, &seen)),
        },
    };
    if let Some(branch) = link.branch() {
        let remote = remotes_in(&repository).and_then(|listed| remote(&listed, link, ssh_host));
        let remote = remote.ok_or_else(|| {
            format!("git no longer says a remote of {repository} is {}", link.repository())
        })?;
        base(&repository, &remote, &branch)?;
    }
    made_worktree(&repository, &link.wt(), going, worked)
}

/// Where a key goes for `file` among the panes `worked`, as `Pasted::ready`
/// says: the file, read where it is, says the repository, and any pane that
/// works in that is of it, as in no other, whichever remotes it has.
fn ready_file(file: &File, worked: &[Worked]) -> Result<Ready<usize>, String> {
    let real = file.real()?;
    let (repository, common) = file.repository(&real)?;
    wt_made(&repository)?;
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    let session = session_among(worked, |dir| {
        let of = seen.entry(dir.to_owned()).or_insert_with(|| common_dir(dir));
        let same = of.as_ref() == Some(&common);
        same.then_some(Repository { by: Remote::Origin, layout: Layout::Wt })
    });
    let going = if let Some(at) = session {
        Going::Session(at)
    } else {
        let (cwd, name) = session_for(&repository)?;
        Going::Clone { cwd, name }
    };
    base(&repository, ORIGIN, &file.branch)?;
    let ready = made_worktree(&repository, &Of::Branch(file.branch.clone()), going, worked)?;
    Ok(Ready { file: Some(real), ..ready })
}

/// Where a key goes once the worktree `of` is made in the repository `dir` is
/// in: `going`, or a pane among `worked` that works in the worktree, where it
/// was one already, as the kernel says where.
fn made_worktree(
    dir: &str,
    of: &Of,
    going: Going,
    worked: &[Worked],
) -> Result<Ready<usize>, String> {
    let worktree = worktree(dir, of)?;
    let in_it = worked.iter().enumerate().filter(|(_, it)| it.dir == worktree);
    let at = match (in_it.min_by_key(|(at, it)| (!it.here, *at)), going) {
        (Some((at, _)), _) => At::There(at),
        (None, Going::Session(at)) => At::Session(at),
        (None, Going::Clone { cwd, name }) => At::Clone { cwd, name: free(&name)? },
    };
    Ok(Ready { at, branch: branch(&worktree), worktree, file: None })
}

/// Where a key goes, by where it stands among the panes.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// The pane that has the pull request's branch checked out.
    There(usize),
    /// A pane that works in its repository, for a tab of its session.
    Session(usize),
}

/// Where a key goes once the worktree is made, where no pane works in it: a
/// tab of the session of the pane of this place, or of a session to make
/// in `cwd`, for its name, as the directory names it, made free once the
/// worktree is, when the session is about to be made.
enum Going {
    Session(usize),
    Clone { cwd: String, name: SessionName },
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
    repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Option<Found> {
    let checked_out = |(_, it): &(usize, &Worked)| it.pull.as_ref().is_some_and(|it| it.is(link));
    let there = worked.iter().enumerate().filter(checked_out);
    let there = there.min_by_key(|(at, it)| (!it.here, *at));
    if let Some((at, _)) = there {
        return Some(Found::There(at));
    }
    session_among(worked, repository_of).map(Found::Session)
}

/// The place among `worked` of the pane whose session a tab opens in: one that
/// works in the repository, where wt cloned it, by how `repository_of` says a
/// directory's repository is it, if it is. One in the session luneta's zellij
/// is in goes first, then one that is it by its origin, then the first row.
fn session_among(
    worked: &[Worked],
    mut repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Option<usize> {
    let mut session: Option<(bool, Remote, usize)> = None;
    for (at, it) in worked.iter().enumerate() {
        let Some(Repository { by, layout: Layout::Wt }) = repository_of(&it.dir) else {
            continue;
        };
        let rank = (!it.here, by, at);
        session = Some(session.map_or(rank, |was| was.min(rank)));
    }
    session.map(|(_, _, at)| at)
}

/// The clone of `link`'s repository that wt made among the directories
/// zoxide knows, by how `repository_of` says a directory's repository is the
/// pull request's: a directory of it, for `wt add`, and the directory that
/// holds it, which the session made for it starts in, with the name the
/// picker gives that directory. None where zoxide knows none; or why it
/// cannot tell, or the directory names no session.
fn clone(
    link: &Link,
    repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Result<Option<(String, String, SessionName)>, String> {
    let repository = link.repository();
    let dirs: Vec<String> = zoxide::visited()
        .map_err(|why| {
            format!("no session works in {repository}, and luneta cannot look for a clone: {why}")
        })?
        .into_iter()
        .map(|dir| dir.path.into())
        .collect();
    let Some(dir) = cloned(&dirs, repository_of) else {
        return Ok(None);
    };
    let dir = dirs[dir].clone();
    let (cwd, name) = session_for(&dir)?;
    Ok(Some((dir, cwd, name)))
}

/// The directory a session made for the repository `dir` is in starts in,
/// which holds it, and the name the picker gives that directory; or why it
/// has none.
fn session_for(dir: &str) -> Result<(String, SessionName), String> {
    let cwd = holder(dir).ok_or_else(|| format!("git does not say what repository {dir} is"))?;
    let name = names::session_for(Path::new(&cwd))
        .ok_or_else(|| format!("no session can be named for {cwd}"))?;
    Ok((cwd, name))
}

/// `name`, or the first of it with a `-2` and on that no session has, one
/// that runs or one that ended, which zellij keeps to bring back, as zellij's
/// folders say now; or why luneta cannot tell which names are free.
fn free(name: &SessionName) -> Result<SessionName, String> {
    let named = names::named().ok_or("luneta cannot tell which sessions zellij has")?;
    Ok(name.free(&named.iter().map(String::as_str).collect::<Vec<_>>()))
}

/// Why no clone of `link`'s repository is for `wt add`, as what luneta has
/// `seen` of each directory it looked in says: there is none, or one git
/// cloned, in whose `.git` wt would make the worktree, or one git did not say
/// how it is laid out.
fn no_clone(link: &Link, seen: &HashMap<String, Option<Repository>>) -> String {
    let repository = link.repository();
    let laid_out = move |layout: Layout| {
        let dirs = seen.iter().filter(move |(_, it)| it.is_some_and(|it| it.layout == layout));
        dirs.map(|(dir, _)| dir).min()
    };
    match (laid_out(Layout::Git), laid_out(Layout::Unsaid)) {
        (Some(dir), _) => format!(
            "{dir} is in a clone of {repository} that git made, and wt would make the worktree \
             in its .git; wt clone it first"
        ),
        (None, Some(dir)) => {
            format!("git did not say whether {dir}, of {repository}, is in a clone wt made")
        },
        (None, None) => format!(
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
        if let Some(Repository { by, layout: Layout::Wt }) = repository_of(dir) {
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
    let common = common_dir(dir)?;
    let common = Path::new(&common);
    let hidden = common.file_name()?.to_str()?.starts_with('.');
    let holder = if hidden { common.parent()? } else { common };
    holder.to_str().map(str::to_owned)
}

/// The directory all the worktrees of the repository `dir` is in have in
/// common, by its real path, which tells that repository from another; none
/// where `dir` is in no repository.
fn common_dir(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "rev-parse", "--path-format=absolute"]))
        .arg("--git-common-dir");
    said(&mut git)
}

/// The remotes of the repository `dir` is in, as `git config --get-regexp`
/// prints them; none where it has none, or `dir` is in no repository.
fn remotes_in(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "config", "--get-regexp", r"^remote\..*\.url$"]));
    let ran = program::run(&mut git, ANSWER).ok()?;
    String::from_utf8(ran.answer().ok()?).ok()
}

/// How the repository `dir` is in is laid out, as git says.
fn layout(dir: &str) -> Layout {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "config", "--type=bool", "--get", "core.bare"]));
    match said(&mut git).as_deref() {
        Some("true") => Layout::Wt,
        Some("false") => Layout::Git,
        _ => Layout::Unsaid,
    }
}

/// The branch checked out in `dir`, as `git status` names it: its ref
/// without `refs/heads/`. None for a detached `HEAD`, or where git does not
/// say.
fn branch(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "symbolic-ref", "--quiet", "HEAD"]));
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

/// Makes `branch`, an issue's or a file's, in the repository `dir` is in,
/// where neither it nor origin has one yet: from the default branch of
/// `remote` as it has it now, the one of the issue's repository, or origin
/// for a file, with no upstream, as wt makes a new branch. wt would make it
/// from origin's, as the repository last fetched it, which can be weeks
/// behind, and of the fork in a fork's clone. Where either has the branch, wt
/// goes on with it: one origin has may be one pushed from another machine. Or
/// why it made none.
fn base(dir: &str, remote: &str, branch: &str) -> Result<(), String> {
    let head = format!("refs/heads/{branch}");
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "rev-parse", "--verify", "--quiet", &head]));
    if said(&mut git).is_some() {
        return Ok(());
    }
    let pushed = across(dir, "ls-remote", &[ORIGIN, &head])?;
    let pushed = String::from_utf8_lossy(&pushed);
    if pushed.lines().any(|line| line.split('\t').nth(1) == Some(head.as_str())) {
        return Ok(());
    }
    across(dir, "fetch", &[remote, &format!("HEAD:{head}")]).map(drop)
}

/// What git says, as it runs `command` with `args` in the repository `dir`
/// is in, which asks a remote across the network: alone, as ssh may ask for
/// a passphrase. Or why it said nothing.
fn across(dir: &str, command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, command]).args(args));
    let ran = program::run_alone(&mut git, MAKING)
        .map_err(|error| format!("git {command} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("git {command}: {why}"))
}

/// Has a worktree made `of` a pull request or a branch in the repository
/// `dir` is in, as `luneta worktree` makes it, and says where it is, or where
/// one of its branch is already: the line it prints, as the real path of the
/// directory it names. Or why it made none, as it says. A program of its own,
/// and not a thread: wt says on stderr how it goes, and has git say it there
/// too, which in luneta's sidebar is the terminal luneta draws; and alone, as
/// git may ask for a passphrase on the terminal of its session.
fn worktree(dir: &str, of: &Of) -> Result<String, String> {
    let mut luneta = Command::new(luneta()?);
    git::local(luneta.args(["worktree", "--repo", dir, &of.to_string()]));
    // It fetches across the network, by ssh as like as not.
    let ran = program::run_alone(&mut luneta, MAKING)
        .map_err(|error| format!("luneta worktree could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            let printed = String::from_utf8_lossy(&stdout);
            let said = printed.strip_suffix('\n').filter(|said| !said.is_empty());
            let said = said.ok_or_else(|| "luneta worktree said no worktree".to_owned())?;
            directory(said)
                .ok_or_else(|| format!("luneta worktree said {said}, which is no directory"))
        },
        Ran::Ended { status, stderr, .. } => Err(failure(&String::from_utf8_lossy(&stderr))
            .unwrap_or(format!("luneta worktree: {status}"))),
        Ran::Stuck => Err(format!(
            "the worktree took longer than {} minutes to make",
            MAKING.as_secs() / 60
        )),
    }
}

/// The pull request of the branch checked out in `dir`, as gh says now, where
/// it says one, for what cannot wait for the daemon's gh, which rests from a
/// branch for a while. It asks GitHub, across the network.
#[must_use]
pub fn pull_request_in(dir: &str) -> Option<Link> {
    Link::parse(&github::pull_request_in(dir).ok()??.url?)
}

/// The `luneta` that makes the worktree: the one this runs as, or else the
/// one beside it, as beside `lunetad`, as old or as new as it is; or why
/// there is none.
fn luneta() -> Result<PathBuf, String> {
    let this = env::current_exe()
        .map_err(|error| format!("luneta cannot tell where it is: {error}"))?;
    if this.file_name().is_some_and(|name| name == "luneta") {
        return Ok(this);
    }
    let beside = this.with_file_name("luneta");
    if beside.is_file() {
        return Ok(beside);
    }
    Err(format!("no luneta is beside {}, to make the worktree with", this.display()))
}

/// Where a worktree `of` a branch or a pull request is, once made in the
/// repository `repo` is a directory of, as `wt add` makes one, by wt's own
/// code, or where one of the branch is already; or why it made none. wt says
/// how it goes on stderr, and has git say it there. In a repository git
/// cloned it makes none: wt would make it in its `.git`. Nor where a variable
/// tells git where a repository is: wt's git would read the repository it
/// names, and `layout`'s the one `repo` is in, so that the clone luneta sees
/// wt cloned may not be the one wt makes the worktree in.
pub fn made(repo: &str, of: &Of) -> Result<PathBuf, String> {
    let named = git::REPOSITORY_VARS.into_iter().find(|name| env::var_os(name).is_some());
    if let Some(name) = named {
        return Err(format!("{name} is set, and git would take another repository for {repo}'s"));
    }
    let wt_add = |error| format!("wt add: {error:#}");
    let found = Repo::discover(Some(Path::new(repo))).map_err(wt_add)?;
    wt_made(repo)?;
    let made = add::run(&found, &of.to_string(), None, Fetch::Lazy).map_err(wt_add)?;
    Ok(made.path().to_owned())
}

/// Why wt makes no worktree in the repository `dir` is in, where it does not:
/// one git cloned, in whose `.git` wt would make it, or one git does not say
/// how it is laid out.
fn wt_made(dir: &str) -> Result<(), String> {
    match layout(dir) {
        Layout::Wt => Ok(()),
        Layout::Git => Err(format!(
            "{dir} is in a clone git made, and wt would make the worktree in its .git; wt clone \
             it first"
        )),
        Layout::Unsaid => Err(format!("git did not say whether {dir} is in a clone wt made")),
    }
}

/// The real path of the directory `path` names, where it names one, as the
/// kernel says where a pane works.
fn directory(path: &str) -> Option<String> {
    let real = fs::canonicalize(path).ok().filter(|real| real.is_dir())?;
    real.into_os_string().into_string().ok()
}

/// Why `luneta worktree` made no worktree, as it says on stderr after what wt
/// and git say there as they go: the lines from its last that starts with
/// `WHY`, on one, as what wt says can take more than one.
fn failure(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let why = lines.iter().rposition(|line| line.starts_with(WHY))?;
    lines[why..].join(", ").strip_prefix(WHY).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    /// A pane in `dir`, with the pull request of the page `pull` checked out,
    /// in the session luneta's zellij is in, or another.
    fn worked(dir: &str, pull: Option<&str>, here: bool) -> Worked {
        let pull = pull.map(|page| Link::parse(page).expect("a pull request's page"));
        Worked { dir: dir.to_owned(), pull, here }
    }

    /// A repository wt cloned, that is the pull request's `by` a remote.
    fn by_wt(by: Remote) -> Repository {
        Repository { by, layout: Layout::Wt }
    }

    /// One git cloned, that is the pull request's by its origin.
    const BY_GIT: Repository = Repository { by: Remote::Origin, layout: Layout::Git };

    /// One that is the pull request's by its origin, where git did not say
    /// how it is laid out.
    const UNSAID: Repository = Repository { by: Remote::Origin, layout: Layout::Unsaid };

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
        assert_eq!(link().wt(), Of::Pull(link()));
    }

    /// An issue's link names it as a pull request's does, but for `issues`:
    /// the agent takes it, and `luneta worktree` the branch luneta has it
    /// make, or find where a paste made it before. It is the pull request of its
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
        assert_eq!(issue().wt(), Of::Branch("issue-2736".to_owned()));
        // GitHub has one #2736 of cdk, of whichever page a link is.
        assert!(issue().is(&link()));
        assert_eq!(Link::parse("https://github.com/cashubtc/cdk/issues/0"), None);
    }

    /// `luneta worktree` takes a pull request's link as a paste does, with
    /// no `https://` too, and hands wt the link in full, which wt tells from
    /// a branch by its `://`: git would take the link with none for a
    /// branch's name, and wt would make a new branch of it, with none of the
    /// pull request's commits. An issue's link is no branch's, nor is any
    /// other address.
    #[test]
    fn a_pull_request_s_link_is_one_with_no_scheme_and_no_address_is_a_branch() {
        let handed = |text: &str| Of::parse(text).map(|of| of.to_string());
        for text in [
            "github.com/cashubtc/cdk/pull/2736",
            "https://github.com/cashubtc/cdk/pull/2736/files",
        ] {
            assert_eq!(Of::parse(text), Ok(Of::Pull(link())), "{text:?}");
            assert_eq!(handed(text), Ok(link().url()), "{text:?}");
        }
        assert_eq!(handed("feat/of"), Ok("feat/of".to_owned()));
        assert_eq!(
            Of::parse("github.com/cashubtc/cdk/issues/2736"),
            Err("cashubtc/cdk#2736 is an issue: make the worktree of its branch, issue-2736"
                .to_owned())
        );
        for text in [
            "github.com/cashubtc/cdk/discussions/1",
            "www.github.com/cashubtc",
            "https://gitlab.com/cashubtc/cdk/-/merge_requests/1",
        ] {
            assert_eq!(
                Of::parse(text),
                Err(format!("{text} is no pull request's link on github.com")),
                "{text:?}"
            );
        }
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
            worked("/dots", None, true),
            worked("/cdk/main", None, false),
            worked("/cdk-upstream/master", None, false),
        ];
        // Its origin before another remote.
        assert_eq!(found(&link(), &away, remotes), Some(Found::Session(2)));
        let here = [
            worked("/cdk/main", None, false),
            worked("/dots", None, true),
            worked("/cdk/main", None, true),
            worked("/cdk-upstream/master", None, false),
        ];
        // luneta's own session before any.
        assert_eq!(found(&link(), &here, remotes), Some(Found::Session(2)));
        let by_git = [worked("/cdk-git", None, true), worked("/cdk/main", None, false)];
        assert_eq!(found(&link(), &by_git, remotes), Some(Found::Session(1)));
        assert_eq!(found(&link(), &by_git[..1], remotes), None);
        let checked_out = [
            worked("/cdk/main", None, true),
            worked("/cdk-upstream/master", Some(&before), false),
            worked("/cdk-git", Some(&cdk), false),
        ];
        assert_eq!(found(&link(), &checked_out, remotes), Some(Found::There(2)));
        let fork = [worked("/cdk/main", Some(&fork), true)];
        assert_eq!(found(&link(), &fork, remotes), Some(Found::Session(0)));
    }

    /// Of the directories zoxide knows, the clone wt made that is the pull
    /// request's by its origin goes first, or else the first by another
    /// remote. One git cloned is none, by any remote, nor is one git did not
    /// say how it is laid out.
    #[test]
    fn a_clone_is_the_first_by_its_origin_or_else_by_another_remote() {
        let dirs = ["/a", "/git", "/unsaid", "/fork/main", "/b", "/upstream", "/fork"]
            .map(str::to_owned);
        let remotes = |dir: &str| match dir {
            "/fork/main" | "/fork" => Some(by_wt(Remote::Other)),
            "/upstream" => Some(by_wt(Remote::Origin)),
            "/git" => Some(BY_GIT),
            "/unsaid" => Some(UNSAID),
            _ => None,
        };
        assert_eq!(cloned(&dirs, remotes), Some(5));
        assert_eq!(cloned(&dirs[..5], remotes), Some(3));
        assert_eq!(cloned(&dirs[..3], remotes), None);
    }

    /// With no clone for `wt add`, luneta says to have wt clone one, and
    /// names one git made where it saw one, as wt would make the worktree in
    /// its `.git`; or else one git did not say how it is laid out, which it
    /// takes for neither.
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
        let by_git = "/w/cdk/src is in a clone of cashubtc/cdk that git made, and wt would make \
                      the worktree in its .git; wt clone it first";
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", None), ("/w/cdk/src", Some(BY_GIT))])),
            by_git
        );
        assert_eq!(
            no_clone(&link(), &seen(&[("/a/cdk", Some(UNSAID)), ("/w/cdk/src", Some(BY_GIT))])),
            by_git
        );
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", None), ("/a/cdk", Some(UNSAID))])),
            "git did not say whether /a/cdk, of cashubtc/cdk, is in a clone wt made"
        );
    }

    /// git as the test runs it, on the repository at `dir`, with no config but
    /// its own, as no one in particular: what it said, where it ended well.
    fn git(dir: impl AsRef<Path>, args: &[&str]) -> Option<String> {
        let mut git = Command::new("git");
        git::local(&mut git)
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
        assert_eq!(base(&clone, "cashubtc", "issue-2736"), Ok(()));
        assert_eq!(made(), now);
        assert_eq!(git(&clone, &["config", "branch.issue-2736.remote"]), None);
        empty(&cdk, "three");
        assert_eq!(base(&clone, "cashubtc", "issue-2736"), Ok(()));
        assert_eq!(made(), now);
        assert!(git(&fork, &["branch", "issue-7", "main"]).is_some());
        assert_eq!(base(&clone, "cashubtc", "issue-7"), Ok(()));
        assert_eq!(git(&clone, &["rev-parse", "--verify", "-q", "refs/heads/issue-7"]), None);
    }

    /// Why `luneta worktree` made no worktree is what it says last on stderr,
    /// after `luneta: `, on one line, though wt says it on two, after what wt
    /// and git say there as they go.
    #[test]
    fn why_no_worktree_was_made_comes_last_after_what_wt_says_as_it_goes() {
        let stderr = "  pull       #2736 (asking origin for its branch)\n\
                      luneta: wt add: wt cannot fetch origin/feat\n  run git fetch origin feat\n";
        assert_eq!(
            failure(stderr).as_deref(),
            Some("wt add: wt cannot fetch origin/feat, run git fetch origin feat")
        );
        assert_eq!(failure("  pull  #1\n"), None);
    }

    /// What a daemon is asked for a paste names a link, and the panes of its
    /// machine, each by an absolute path, with the page of the pull request
    /// of its branch, if any: no more of them than a machine has. Anything
    /// else is refused, as it is no question luneta asks.
    #[test]
    fn a_paste_is_asked_of_a_link_and_of_panes_by_their_absolute_paths() {
        let cdk = "https://github.com/cashubtc/cdk/pull/2736";
        let worked = vec![worked("/w/cdk/main", Some(cdk), true), worked("/dots", None, false)];
        let pasted = Pasted::new(Work::Link(issue()), worked);
        let on = Some(On { destination: "nixos".to_owned() });
        assert_eq!(Pasted::of(pasted.asked(on.clone())), Ok((on, pasted.clone())));
        let refused = |request: PasteRequest| Pasted::of(request).map(drop).unwrap_err();
        let asked = |worked: Vec<AskedWorked>| PasteRequest { worked, ..pasted.asked(None) };
        let link = paste_request::Work::Link("github.com/cashubtc".to_owned());
        let unlinked = PasteRequest { work: Some(link), ..asked(Vec::new()) };
        assert_eq!(
            refused(unlinked),
            "link: \"github.com/cashubtc\" is no link of a pull request or of an issue on \
             github.com"
        );
        let pane = |dir: &str, pull: Option<&str>| AskedWorked {
            dir: dir.to_owned(),
            pull: pull.map(str::to_owned),
            here: false,
        };
        let relative = asked(vec![pane("w/cdk", None)]);
        assert_eq!(refused(relative), "worked: \"w/cdk\" is no absolute path");
        let unpaged = asked(vec![pane("/w/cdk", Some("main"))]);
        assert_eq!(refused(unpaged), "worked: \"main\" is no pull request's page");
        let many = asked(vec![pane("/w", None); 1025]);
        assert_eq!(refused(many), "worked: more than 1024 panes");
        assert_eq!(
            refused(PasteRequest { work: None, ..asked(Vec::new()) }),
            "it says neither a link nor a file"
        );
    }

    /// A file is asked of by its path as it was pasted, and the directory of
    /// the row, which only the machine reads. What is no path of a `.md` file
    /// is refused, or a row's directory by no absolute path.
    #[test]
    fn a_paste_is_asked_of_a_file_by_its_path_and_its_row_s_directory() {
        for from in [Some("/w/cdk/main"), None] {
            let file = File::parse("~/notes/plan.md", from).unwrap();
            let pasted = Pasted::new(Work::File(file), Vec::new());
            assert_eq!(Pasted::of(pasted.asked(None)), Ok((None, pasted)));
        }
        let refused = |path: &str, from: Option<&str>| {
            let file = AskedFile { path: path.to_owned(), from: from.map(str::to_owned) };
            let work = Some(paste_request::Work::File(file));
            let request = PasteRequest { on: None, work, worked: Vec::new() };
            Pasted::of(request).map(drop).unwrap_err()
        };
        assert_eq!(refused("plan.txt", None), "file: \"plan.txt\" is no path of a .md file");
        assert_eq!(refused("plan.md", Some("w/cdk")), "from: \"w/cdk\" is no absolute path");
    }

    /// A file's path is one line that ends in `.md`, but for the space around
    /// it, wherever it says the file is. Its name gives its branch and its
    /// tab, of what git takes in a branch's name, in lower case. An address
    /// is no path, nor is a name with nothing to name a branch for.
    #[test]
    fn a_file_s_path_is_one_line_that_ends_in_md_and_names_its_branch() {
        for (text, path, branch) in [
            ("docs/plans/sync-rework.md", "docs/plans/sync-rework.md", "sync-rework"),
            ("  /w/plan.md\n", "/w/plan.md", "plan"),
            ("~/notes/My Plan (v2).md", "~/notes/My Plan (v2).md", "my-plan-v2"),
            ("README.md", "README.md", "readme"),
            ("2026-11-03_plan.md", "2026-11-03_plan.md", "2026-11-03_plan"),
            ("a..b.md", "a..b.md", "a-b"),
            ("x.lock.md", "x.lock.md", "x-lock"),
            ("--force.md", "--force.md", "force"),
            ("revisão.md", "revisão.md", "revisão"),
        ] {
            let file = File::parse(text, None).unwrap_or_else(|| panic!("{text:?}"));
            assert_eq!((file.path(), file.branch.as_str()), (path, branch), "{text:?}");
            let check = git(".", &["check-ref-format", "--branch", branch]);
            assert_eq!(check.as_deref(), Some(branch), "{text:?}");
            assert_eq!(Work::File(file.clone()).tab(), branch, "{text:?}");
        }
        for text in [
            "plan.txt",
            "plan.md and more",
            "plan.md/",
            "two\nlines.md",
            "https://example.com/plan.md",
            ".md",
            "docs/.md",
            "---.md",
            "",
        ] {
            assert_eq!(File::parse(text, None), None, "{text:?}");
        }
        // A link is a link, whatever it ends in.
        let issue = "https://github.com/cashubtc/cdk/issues/2736";
        assert_eq!(Work::parse(issue, None), Some(Work::Link(self::issue())));
        let file = Work::parse("/w/plan.md", Some("/w"));
        assert_eq!(file, File::parse("/w/plan.md", Some("/w")).map(Work::File));
    }

    /// A file's real path comes of its path from the root, or from the row it
    /// was pasted on, through any link. What names no file says so, and so
    /// does a relative path with no row to go from. The repository is the
    /// file's own, untracked or not, or else the one the row works in; with
    /// neither, it says so.
    #[test]
    fn a_file_is_read_where_it_is_and_its_repository_is_its_own_or_else_the_row_s() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let path = |name: &str| scratch.join(name).to_str().unwrap().to_owned();
        let (repo, row, notes) = (path("repo"), path("row"), path("notes"));
        for dir in [&repo, &row] {
            assert!(git(&scratch, &["init", "-q", "-b", "main", dir]).is_some());
        }
        fs::create_dir_all(scratch.join("repo/docs")).unwrap();
        fs::create_dir_all(&notes).unwrap();
        fs::write(scratch.join("repo/docs/plan.md"), "plan").unwrap();
        fs::write(scratch.join("notes/plan.md"), "plan").unwrap();
        fs::create_dir(scratch.join("notes/dir.md")).unwrap();
        std::os::unix::fs::symlink(scratch.join("repo/docs/plan.md"), scratch.join("notes/l.md"))
            .unwrap();
        let file = |text: &str, from: Option<&str>| File::parse(text, from).unwrap();

        let inside = path("repo/docs/plan.md");
        assert_eq!(file(&inside, None).real().as_ref(), Ok(&inside));
        assert_eq!(file("docs/plan.md", Some(&repo)).real().as_ref(), Ok(&inside));
        let root = scratch.to_str();
        assert_eq!(file("notes/l.md", root).real().as_ref(), Ok(&inside));
        let gone = file(&path("repo/gone.md"), None).real();
        assert_eq!(gone, Err("no such file".to_owned()));
        assert_eq!(file(&path("notes/dir.md"), None).real(), Err("it is no file".to_owned()));
        assert_eq!(
            file("docs/plan.md", None).real(),
            Err("its path is relative, and the row highlighted works in no directory to go from"
                .to_owned())
        );

        let common = |dir: &str| format!("{dir}/.git");
        let docs = path("repo/docs");
        let own = file(&inside, Some(&row)).repository(&inside);
        assert_eq!(own, Ok((docs, common(&repo))));
        let outside = path("notes/plan.md");
        let by_row = file(&outside, Some(&row)).repository(&outside);
        assert_eq!(by_row, Ok((row.clone(), common(&row))));
        assert_eq!(
            file(&outside, None).repository(&outside),
            Err("it is in no repository, and the row highlighted works in no directory to go by"
                .to_owned())
        );
        assert_eq!(
            file(&outside, Some(&notes)).repository(&outside),
            Err(format!("it is in no repository, nor is {notes}, the row highlighted"))
        );
    }

    /// Where a key goes is answered as it was found: a pane by its place
    /// among those asked of, or a session to make, by its name. An answer
    /// luneta cannot go by is refused: of a worktree by no absolute path, a
    /// session zellij would not take, or of where in words of a newer
    /// daemon's.
    #[test]
    fn where_a_key_goes_is_answered_as_it_was_found() {
        let name = SessionName::parse("cdk").unwrap();
        let clone = At::Clone { cwd: "/w/cdk".to_owned(), name };
        for at in [At::There(1), At::Session(0), clone] {
            let (worktree, branch) = ("/w/cdk/issue-2736", Some("issue-2736".to_owned()));
            let file = Some("/w/cdk/main/plan.md".to_owned());
            let ready = Ready { at, worktree: worktree.to_owned(), branch, file };
            assert_eq!(Ready::of(ready.clone().answer()), Ok(ready));
        }
        let there =
            || Ready { at: At::There(1), worktree: "/w/x".to_owned(), branch: None, file: None };
        let answer = || there().answer();
        let refused = |answer: PasteResponse| Ready::of(answer).unwrap_err();
        let relative = PasteResponse { worktree: "x".to_owned(), ..answer() };
        assert_eq!(refused(relative), "worktree: \"x\" is no absolute path");
        let relative = PasteResponse { file: Some("plan.md".to_owned()), ..answer() };
        assert_eq!(refused(relative), "file: \"plan.md\" is no absolute path");
        assert!(refused(PasteResponse { at: None, ..answer() }).contains("a newer lunetad does"));
        let made = |cwd: &str, name: &str| PasteResponse {
            at: Some(paste_response::At::Clone(MadeClone {
                cwd: cwd.to_owned(),
                name: name.to_owned(),
            })),
            ..answer()
        };
        let unnamed = refused(made("/w", "a/b"));
        assert_eq!(unnamed, "clone: \"a/b\" names no session: name cannot contain '/'");
        let relative = refused(made("w", "cdk"));
        assert_eq!(relative, "clone: \"w\" is no absolute path");
    }

    /// A pane named by its place among those luneta said is the pane it
    /// knows of that place; a place it said none of is refused.
    #[test]
    fn a_place_names_the_pane_luneta_said_of_it() {
        fn ready<P>(at: At<P>) -> Ready<P> {
            let (worktree, file) = ("/w".to_owned(), Some("/w/plan.md".to_owned()));
            Ready { at, worktree, branch: None, file }
        }
        let panes = ["dots:1", "cdk:4"];
        assert_eq!(ready(At::Session(1)).placed(&panes), Ok(ready(At::Session("cdk:4"))));
        assert_eq!(ready(At::There(2)).placed(&panes), Err(2));
    }
}
