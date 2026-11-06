//! The pull request of the branch checked out where each pane works, as
//! `gh pr view` finds it on GitHub.
//!
//! GitHub tells no one as a pull request opens, or is merged, so each branch
//! is asked about again once gh has rested from it for a sweep, and a branch
//! it has said nothing of yet at once. gh asks across the network, and each
//! ask counts against the hourly limit GitHub puts on the user, which every
//! other gh they run shares: panes on one branch of one repository are one
//! ask, however many directories of it they work in.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::num::NonZeroU32;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use luneta_protocol::github::{PullRequest, State};
use serde::Deserialize;

use crate::core;
use crate::git::{self, NamedHead};
use crate::links::{self, Remotes};
use crate::program;
use crate::watch::Troubles;

/// How long gh has to say: it asks GitHub, across the network.
const LIMIT: Duration = Duration::from_secs(30);

/// How gh starts what it says of a branch with no pull request, which is no
/// failure to the daemon: gh fails the same way at anything else.
const NONE: [&str; 2] = ["no pull requests found", "GraphQL: Could not resolve to a PullRequest"];

/// The pull request gh found of the branch checked out in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The branch, as it was checked out where gh was asked, when it answered.
    pub branch: String,
    pub pull_request: PullRequest,
}

/// What gh is asked of a pull request: `--json` takes them so.
const VIEWED: &str = "number,state,isDraft,url";

/// A pull request as `gh pr view --json` writes what `VIEWED` asks of it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    number: NonZeroU32,
    state: Stands,
    /// Whether it is a draft, which a closed one can still be.
    is_draft: bool,
    /// Its page, of the repository gh found it in.
    url: String,
}

/// A pull request's state, as GitHub spells it.
#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Stands {
    Open,
    Closed,
    Merged,
}

/// What gh wrote of a pull request, or why it does not read.
fn parse(said: &[u8]) -> Result<PullRequest, String> {
    let viewed: Viewed = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let state = match (viewed.state, viewed.is_draft) {
        (Stands::Open, false) => State::Open,
        (Stands::Open, true) => State::Draft,
        (Stands::Merged, _) => State::Merged,
        (Stands::Closed, _) => State::Closed,
    };
    Ok(PullRequest { number: viewed.number, state, url: Some(viewed.url) })
}

const REPOSITORY: &str = "parent,url,defaultBranchRef";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    default_branch_ref: BranchRef,
    parent: Option<Parent>,
    url: String,
}

#[derive(Deserialize)]
struct BranchRef {
    name: String,
}

#[derive(Deserialize)]
struct Parent {
    name: String,
    owner: Owner,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fork {
    host: String,
    owner: String,
    name: String,
    default_branch: String,
}

impl Fork {
    fn parent(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }

    fn parent_may_have(&self, head: &NamedHead) -> bool {
        match head {
            NamedHead::OnRemote => true,
            NamedHead::PullRef { remote_url } => {
                links::remote_is_of(remote_url, (&self.host, &self.owner, &self.name))
            },
        }
    }
}

fn fork_of(said: &[u8]) -> Result<Option<Fork>, String> {
    let repository: Repository = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let Some(Parent { name, owner: Owner { login } }) = repository.parent else {
        return Ok(None);
    };
    if !links::named(&login) || !links::named(&name) {
        return Err(format!("{login}/{name} names no repository"));
    }
    let host = repository.url.split_once("://").and_then(|(_, page)| page.split('/').next());
    let host = host.filter(|host| !host.is_empty());
    let host = host.ok_or_else(|| format!("{} is no page", repository.url))?;
    Ok(Some(Fork {
        host: host.to_owned(),
        owner: login,
        name,
        default_branch: repository.default_branch_ref.name,
    }))
}

/// What gh said of the branch it was asked about.
enum Answer {
    /// The branch was still checked out as gh answered: the pull request gh
    /// found of it, or none.
    Stayed(Option<PullRequest>),
    Partly {
        own: Option<PullRequest>,
        why: String,
    },
    /// Another branch was checked out by the time gh answered, which git has
    /// yet to see: what gh said was of that one.
    Moved,
}

fn find(dir: &str, base: Option<&str>) -> Result<Option<PullRequest>, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", "--json", VIEWED]).current_dir(dir));
    let asked = match base {
        Some(base) => {
            gh.env("GH_REPO", base);
            format!("gh pr view of {base} in {dir}")
        },
        None => format!("gh pr view in {dir}"),
    };
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    match ran.answer() {
        Ok(said) => parse(&said)
            .map(Some)
            .map_err(|why| format!("{asked} said what does not read: {why}")),
        Err(why) if NONE.iter().any(|none| why.starts_with(none)) => Ok(None),
        Err(why) => Err(format!("{asked}: {why}")),
    }
}

fn fork(dir: &str, repository: &str, forks: &mut Forks) -> Result<Option<Fork>, String> {
    let remotes = match links::remotes_gh_reads(dir) {
        Remotes::Listed(listed) => listed,
        Remotes::Unlisted => return Ok(None),
        Remotes::Unsaid(why) => return Err(format!("git did not say the remotes of {dir}: {why}")),
    };
    if let Some(known) = forks.get(repository)
        && known.remotes == remotes
    {
        return Ok(known.fork.clone());
    }
    let mut gh = Command::new("gh");
    git::local(gh.args(["repo", "view", "--json", REPOSITORY]).current_dir(dir));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh repo view in {dir}: {why}"))?;
    let fork = fork_of(&said)
        .map_err(|why| format!("gh repo view in {dir} said what does not read: {why}"))?;
    forks.insert(repository.to_owned(), Lineage { remotes, fork: fork.clone() });
    Ok(fork)
}

fn in_parent(
    dir: &str,
    repository: &str,
    branch: &str,
    forks: &mut Forks,
) -> Result<Option<PullRequest>, String> {
    let Some(head) = git::named_head(dir, branch)? else {
        return Ok(None);
    };
    let Some(fork) = fork(dir, repository, forks)?.filter(|fork| fork.parent_may_have(&head)) else {
        return Ok(None);
    };
    let done = |found: &PullRequest| matches!(found.state, State::Merged | State::Closed);
    let stale = |found: &PullRequest| done(found) && branch == fork.default_branch;
    Ok(find(dir, Some(&fork.parent()))?.filter(|found| !stale(found)))
}

/// What gh says of the pull request of `branch`, checked out in `dir`: the
/// one it finds, or none, or that `branch` is no longer checked out there.
/// When gh did not say, as it is not logged in, or did not answer in time,
/// what is to be said of it instead.
///
/// gh finds it as it does in a shell in `dir`, or else in the repository that
/// one forks, for a branch git names a head of: by the branch's config for
/// where it pushes the branch. It reads what is checked out as it starts,
/// where git can have last looked a sweep before, so what is checked out is
/// read again once gh has answered.
fn view(
    dir: &str,
    (repository, branch): &Branch,
    forks: &mut Forks,
) -> Result<Answer, String> {
    let own = find(dir, None)?;
    let settled = own.as_ref().is_some_and(|found| found.state != State::Closed);
    let beyond = if settled { Ok(None) } else { in_parent(dir, repository, branch, forks) };
    if git::branch(dir).as_deref() != Some(branch.as_str()) {
        return Ok(Answer::Moved);
    }
    Ok(match beyond {
        Ok(beyond) => Answer::Stayed(beyond.or(own)),
        Err(why) => Answer::Partly { own, why },
    })
}

fn standing(was: Option<PullRequest>, own: Option<PullRequest>) -> Option<PullRequest> {
    match (was, own) {
        (Some(was), Some(own)) if was.url == own.url => Some(own),
        (was, own) => was.or(own),
    }
}

/// A branch of a repository, by the repository's common directory and the
/// branch's name.
type Branch = (String, String);

struct Lineage {
    remotes: String,
    fork: Option<Fork>,
}

type Forks = HashMap<String, Lineage>;

/// Where gh stands with a branch it has been asked about.
struct Asked {
    /// When it is to be asked about again.
    next: Instant,
    /// The pull request it found last, if it found one.
    found: Option<PullRequest>,
    /// What went wrong the last time it was asked about, if anything did.
    trouble: Option<String>,
}

/// Asks gh for the pull request of the branch checked out in each directory
/// a pane works in, in a thread of its own, and tells the core: about each
/// branch once gh has rested from it for a `sweep`, and about one it has said
/// nothing of yet as soon as it is `woken`, before those left. What gh said
/// of a branch it could not answer for stands until it can. The thread ends
/// once the core has.
pub fn follow(
    core: SyncSender<core::Command>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("gh".to_owned()).spawn(move || {
        let mut asked: HashMap<Branch, Asked> = HashMap::new();
        let mut forks = Forks::new();
        let mut troubles = Troubles::default();
        'sweep: loop {
            let Some(branches) = core::ask(&core, core::Command::Branches) else {
                return;
            };
            // The directories each branch is checked out in. git says the
            // repository of each afresh: a directory a pane stays in can be
            // cloned over, with a branch of the same name checked out.
            let mut on: BTreeMap<Branch, Vec<String>> = BTreeMap::new();
            for (dir, branch) in branches {
                if let Some(repository) = git::repository(&dir) {
                    on.entry((repository, branch)).or_default().push(dir);
                }
            }
            asked.retain(|branch, _| on.contains_key(branch));
            forks.retain(|repository, _| on.keys().any(|(of, _)| of == repository));
            // A directory that came to a branch gh has said of already.
            if !tell(&core, &on, &asked) {
                return;
            }
            let now = Instant::now();
            let due = |branch: &&Branch| asked.get(*branch).is_none_or(|was| was.next <= now);
            let mut due: Vec<&Branch> = on.keys().filter(due).collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|branch| asked.get(*branch).map(|was| was.next));
            for branch in due {
                let said = view(&on[branch][0], branch, &mut forks);
                let next = Instant::now() + sweep;
                let was = asked.remove(branch).and_then(|was| was.found);
                let (found, trouble) = match said {
                    Ok(Answer::Stayed(found)) => (found, None),
                    // The core wakes the thread once git sees the checkout.
                    Ok(Answer::Moved) => (was, None),
                    Ok(Answer::Partly { own, why }) => {
                        troubles.say(why.clone());
                        (standing(was, own), Some(why))
                    },
                    Err(trouble) => {
                        troubles.say(trouble.clone());
                        (was, Some(trouble))
                    },
                };
                asked.insert(branch.clone(), Asked { next, found, trouble });
                if !tell(&core, &on, &asked) {
                    return;
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            // What went wrong with one that gh rests from has not gone right
            // since, and is not said again.
            for trouble in asked.values().filter_map(|was| was.trouble.clone()) {
                troubles.say(trouble);
            }
            troubles.done();
            let now = Instant::now();
            let next = asked.values().map(|was| was.next.saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

/// Tells the core the pull request gh found last for the branch in each
/// directory `on` has it checked out in. False once the core has stopped.
fn tell(
    core: &SyncSender<core::Command>,
    on: &BTreeMap<Branch, Vec<String>>,
    asked: &HashMap<Branch, Asked>,
) -> bool {
    let mut found = BTreeMap::new();
    for (branch, dirs) in on {
        let Some(pull_request) = asked.get(branch).and_then(|was| was.found.clone()) else {
            continue;
        };
        let of = Found { branch: branch.1.clone(), pull_request };
        found.extend(dirs.iter().map(|dir| (dir.clone(), of.clone())));
    }
    core.send(core::Command::PullRequests(found)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use luneta_protocol::github::{PullRequest, State};

    use super::{fork_of, parse, standing};

    #[test]
    fn a_parent_that_does_not_say_leaves_its_own_and_takes_news_of_the_fork_s() {
        let pull = |repository: &str, number: u32, state: State| PullRequest {
            number: NonZeroU32::new(number).unwrap(),
            state,
            url: Some(format!("https://github.com/{repository}/pull/{number}")),
        };
        let fork_s = |state| pull("owner/repo", 5, state);
        let parent_s = || pull("upstream/repo", 5, State::Open);
        let closed = || fork_s(State::Closed);
        assert_eq!(standing(Some(fork_s(State::Open)), Some(closed())), Some(closed()));
        assert_eq!(standing(Some(parent_s()), None), Some(parent_s()));
        assert_eq!(standing(Some(parent_s()), Some(closed())), Some(parent_s()));
        assert_eq!(standing(None, Some(closed())), Some(closed()));
        assert_eq!(standing(None, None), None);
    }

    #[test]
    fn a_fork_s_parent_reads_as_gh_writes_it_on_the_fork_s_host() {
        let parent = r#"{"id":"R","name":"cdk","owner":{"id":"O","login":"cashubtc"}}"#;
        let default = r#""defaultBranchRef":{"name":"trunk"}"#;
        let forked = |url: &str| format!(r#"{{{default},"parent":{parent},"url":"{url}"}}"#);
        let read = |said: &str| fork_of(said.as_bytes());
        let parent_of = |said: &str| read(said).map(|fork| fork.map(|fork| fork.parent()));
        let on = |host: &str| Ok(Some(format!("{host}/cashubtc/cdk")));
        assert_eq!(parent_of(&forked("https://github.com/lorenzolfm/cdk")), on("github.com"));
        assert_eq!(
            parent_of(&forked("https://git.example.com/lorenzolfm/cdk")),
            on("git.example.com")
        );
        let default_branch = read(&forked("https://github.com/lorenzolfm/cdk"))
            .map(|fork| fork.map(|fork| fork.default_branch));
        assert_eq!(default_branch, Ok(Some("trunk".to_owned())));
        let none = format!(r#"{{{default},"parent":null,"url":"https://github.com/lorenzolfm/luneta"}}"#);
        assert_eq!(read(&none), Ok(None));
        let unnamed = |name: &str, login: &str| {
            let parent = format!(r#"{{"name":"{name}","owner":{{"login":"{login}"}}}}"#);
            format!(r#"{{{default},"parent":{parent},"url":"https://github.com/lorenzolfm/cdk"}}"#)
        };
        let unread = [
            forked("nowhere"),
            forked("no/where/at"),
            forked("https:///lorenzolfm/cdk"),
            unnamed("", "cashubtc"),
            unnamed("cdk", ""),
            unnamed("cdk", "cashu/btc"),
            format!(r#"{{"parent":{parent},"url":"https://github.com/lorenzolfm/cdk"}}"#),
            String::new(),
        ];
        for unread in unread {
            assert!(read(&unread).is_err(), "{unread:?}");
        }
    }

    /// A draft is open: one closed or merged is closed or merged, though
    /// GitHub still says it is a draft. What does not read, as a state GitHub
    /// has not spelled before, or a number it does not count, or one with no
    /// page, is no pull request.
    #[test]
    fn a_pull_request_reads_as_gh_writes_it() {
        let read = |said: &str| parse(said.as_bytes());
        let state = |said: &str| read(said).map(|pull_request| pull_request.state);
        let number = NonZeroU32::new(126).unwrap();
        let url = "https://github.com/owner/repo/pull/126";
        assert_eq!(
            read(&format!(r#"{{"isDraft":false,"number":126,"state":"OPEN","url":"{url}"}}"#)),
            Ok(PullRequest { number, state: State::Open, url: Some(url.to_owned()) })
        );
        let page = r#""url":"https://github.com/owner/repo/pull/1""#;
        let viewed = |fields: &str| format!("{{{fields},{page}}}");
        let draft = viewed(r#""isDraft":true,"number":1,"state":"OPEN""#);
        assert_eq!(state(&draft), Ok(State::Draft));
        let merged = viewed(r#""isDraft":true,"number":1,"state":"MERGED""#);
        assert_eq!(state(&merged), Ok(State::Merged));
        let closed = viewed(r#""isDraft":true,"number":1,"state":"CLOSED""#);
        assert_eq!(state(&closed), Ok(State::Closed));
        let unread = [
            viewed(r#""isDraft":false,"number":1,"state":"QUEUED""#),
            viewed(r#""number":1,"state":"OPEN""#),
            viewed(r#""isDraft":false,"number":-1,"state":"OPEN""#),
            viewed(r#""isDraft":false,"number":0,"state":"OPEN""#),
            r#"{"isDraft":false,"number":1,"state":"OPEN"}"#.to_owned(),
            String::new(),
        ];
        for said in unread {
            assert!(read(&said).is_err(), "{said:?}");
        }
    }
}
