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
use crate::git;
use crate::program;
use crate::watch::Troubles;

/// How long gh has to say: it asks GitHub, across the network.
const LIMIT: Duration = Duration::from_secs(30);

/// How gh starts what it says of a branch with no pull request, which is no
/// failure to the daemon: gh fails the same way at anything else.
const NONE: &str = "no pull requests found";

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

const REPOSITORY: &str = "parent,url";

#[derive(Deserialize)]
struct Repository {
    parent: Option<Parent>,
    url: String,
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

fn parent_of(said: &[u8]) -> Result<Option<String>, String> {
    let repository: Repository = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let Some(parent) = repository.parent else {
        return Ok(None);
    };
    let host = repository.url.split('/').nth(2).filter(|host| !host.is_empty());
    let host = host.ok_or_else(|| format!("{} is no page", repository.url))?;
    Ok(Some(format!("{host}/{}/{}", parent.owner.login, parent.name)))
}

/// What gh said of the branch it was asked about.
enum Answer {
    /// The branch was still checked out as gh answered: the pull request gh
    /// found of it, or none.
    Stayed(Option<PullRequest>),
    /// Another branch was checked out by the time gh answered, which git has
    /// yet to see: what gh said was of that one.
    Moved,
}

fn find(dir: &str, base: Option<&str>) -> Result<Option<PullRequest>, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", "--json", VIEWED]).current_dir(dir));
    if let Some(base) = base {
        gh.env("GH_REPO", base);
    }
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh pr view in {dir} could not be run: {error}"))?;
    match ran.answer() {
        Ok(said) => parse(&said)
            .map(Some)
            .map_err(|why| format!("gh pr view in {dir} said what does not read: {why}")),
        Err(why) if why.starts_with(NONE) => Ok(None),
        Err(why) => Err(format!("gh pr view in {dir}: {why}")),
    }
}

fn parent(dir: &str, repository: &str, parents: &mut Parents) -> Result<Option<String>, String> {
    if let Some(parent) = parents.get(repository) {
        return Ok(parent.clone());
    }
    let mut gh = Command::new("gh");
    git::local(gh.args(["repo", "view", "--json", REPOSITORY]).current_dir(dir));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh repo view in {dir}: {why}"))?;
    let parent = parent_of(&said)
        .map_err(|why| format!("gh repo view in {dir} said what does not read: {why}"))?;
    parents.insert(repository.to_owned(), parent.clone());
    Ok(parent)
}

/// What gh says of the pull request of `branch`, checked out in `dir`: the
/// one it finds, or none, or that `branch` is no longer checked out there.
/// When gh did not say, as it is not logged in, or did not answer in time,
/// what is to be said of it instead.
///
/// gh finds it as it does in a shell in `dir`, or else in the repository that
/// one forks: by the branch's config for where it pushes the branch. It reads
/// what is checked out as it starts, where git can have last looked a sweep
/// before, so what is checked out is read again once gh has answered.
fn view(
    dir: &str,
    (repository, branch): &Branch,
    parents: &mut Parents,
) -> Result<Answer, String> {
    let mut found = find(dir, None)?;
    if found.is_none()
        && let Some(parent) = parent(dir, repository, parents)?
    {
        found = find(dir, Some(&parent))?;
    }
    if git::branch(dir).as_deref() != Some(branch.as_str()) {
        return Ok(Answer::Moved);
    }
    Ok(Answer::Stayed(found))
}

/// A branch of a repository, by the repository's common directory and the
/// branch's name.
type Branch = (String, String);

type Parents = HashMap<String, Option<String>>;

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
        let mut parents = Parents::new();
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
            parents.retain(|repository, _| on.keys().any(|(of, _)| of == repository));
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
                let said = view(&on[branch][0], branch, &mut parents);
                let next = Instant::now() + sweep;
                let was = asked.remove(branch).and_then(|was| was.found);
                let (found, trouble) = match said {
                    Ok(Answer::Stayed(found)) => (found, None),
                    // The core wakes the thread once git sees the checkout.
                    Ok(Answer::Moved) => (was, None),
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

    use super::{parent_of, parse};

    #[test]
    fn a_fork_s_parent_reads_as_gh_writes_it_on_the_fork_s_host() {
        let parent = r#"{"id":"R","name":"cdk","owner":{"id":"O","login":"cashubtc"}}"#;
        let forked = |url: &str| format!(r#"{{"parent":{parent},"url":"{url}"}}"#);
        let read = |said: &str| parent_of(said.as_bytes());
        assert_eq!(
            read(&forked("https://github.com/lorenzolfm/cdk")),
            Ok(Some("github.com/cashubtc/cdk".to_owned()))
        );
        assert_eq!(
            read(&forked("https://git.example.com/lorenzolfm/cdk")),
            Ok(Some("git.example.com/cashubtc/cdk".to_owned()))
        );
        let none = r#"{"parent":null,"url":"https://github.com/lorenzolfm/luneta"}"#;
        assert_eq!(read(none), Ok(None));
        for unread in [forked("nowhere"), String::new()] {
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
