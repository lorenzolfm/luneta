use crate::api::{
    self, ask_response, list_work_response, make_worktree_response, prune_request,
    prune_response, prunes_response, rebase_response, waiting_response,
};
use lune_protocol::CONFLICTED_NAMED_MAX;

use crate::unanswered::Unanswered;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Waiting {
    pub screen: String,
    pub shown: u64,
    pub asks: Asks,
    pub said: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Asks {
    Words,
    Menu { choices: Vec<String>, chosen: u32 },
    Unread { why: String },
}

impl Waiting {
    pub(crate) fn of(said: api::WaitingResponse) -> Result<Waiting, Unanswered> {
        let asks = match said.asks {
            Some(waiting_response::Asks::Words(())) => Asks::Words,
            Some(waiting_response::Asks::Menu(menu)) => {
                Asks::Menu { choices: menu.choices, chosen: menu.chosen }
            },
            Some(waiting_response::Asks::Unread(why)) => Asks::Unread { why },
            None => return Err(Unanswered::unread("it says nothing of what the agent asks")),
        };
        Ok(Waiting { screen: said.screen, shown: said.shown, asks, said: said.said })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Answer {
    Text { text: String },
    Choice { index: u32, label: String },
    Escape,
}

impl Answer {
    pub(crate) fn into_api(self) -> api::answer_request::Answer {
        use api::answer_request::{self, Choice};
        match self {
            Answer::Text { text } => answer_request::Answer::Text(text),
            Answer::Choice { index, label } => {
                answer_request::Answer::Choice(Choice { index, label })
            },
            Answer::Escape => answer_request::Answer::Escape(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Landed {
    Started { worktree: u64 },
    Found { worktree: u64 },
    Shell { worktree: u64 },
}

impl Landed {
    pub(crate) fn of(said: Option<api::MakeWorktreeResponse>) -> Result<Landed, Unanswered> {
        let landed = said.and_then(|said| said.landed);
        match landed.ok_or_else(|| Unanswered::unread("it ended with no worktree"))? {
            make_worktree_response::Landed::Started(worktree) => Ok(Landed::Started { worktree }),
            make_worktree_response::Landed::Found(worktree) => Ok(Landed::Found { worktree }),
            make_worktree_response::Landed::Made(worktree) => Ok(Landed::Shell { worktree }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Listed {
    Work { issues: Issues, pulls: Vec<Pull>, more_pulls: bool },
    Unhosted,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Issues {
    Open { issues: Vec<Issue>, more: bool },
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Issue {
    pub number: u32,
    pub title: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Pull {
    pub number: u32,
    pub title: String,
    pub draft: bool,
    pub branch: String,
    pub author: Option<String>,
}

impl Listed {
    pub(crate) fn of(said: Option<api::ListWorkResponse>) -> Result<Listed, Unanswered> {
        let listed = said.and_then(|said| said.listed);
        let work = match listed.ok_or_else(|| Unanswered::unread("it ended with no list"))? {
            list_work_response::Listed::Unhosted(()) => return Ok(Listed::Unhosted),
            list_work_response::Listed::Work(work) => work,
        };
        let issues = match work.issues {
            Some(list_work_response::work::Issues::Open(open)) => Issues::Open {
                issues: open.issues.into_iter().map(Issue::of).collect(),
                more: open.more,
            },
            Some(list_work_response::work::Issues::Off(())) => Issues::Off,
            None => return Err(Unanswered::unread("it says nothing of the issues")),
        };
        let pulls = work.pulls.into_iter().map(Pull::of).collect();
        Ok(Listed::Work { issues, pulls, more_pulls: work.more_pulls })
    }
}

impl Issue {
    fn of(said: list_work_response::Issue) -> Issue {
        Issue { number: said.number, title: said.title, labels: said.labels }
    }
}

impl Pull {
    fn of(said: list_work_response::Pull) -> Pull {
        let list_work_response::Pull { number, title, draft, branch, author } = said;
        Pull { number, title, draft, branch, author }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Marked {
    pub issues: Vec<u32>,
    pub pulls: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Order {
    pub seen: Vec<u64>,
    pub wanted: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Step {
    Session { id: String },
    Ran { id: String, command: String },
    Denied { id: String },
    Said { text: String },
}

impl Step {
    pub(crate) fn of(said: api::AskResponse) -> Result<Step, Unanswered> {
        match said.step.ok_or_else(|| Unanswered::unread("a step says nothing"))? {
            ask_response::Step::Session(id) => Ok(Step::Session { id }),
            ask_response::Step::Ran(ask_response::Ran { id, command }) => {
                Ok(Step::Ran { id, command })
            },
            ask_response::Step::Denied(id) => Ok(Step::Denied { id }),
            ask_response::Step::Said(text) => Ok(Step::Said { text }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Plan {
    pub repository: String,
    pub goes: Vec<Goes>,
    pub kept: Vec<Kept>,
    pub unasked: Vec<Unasked>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Goes {
    pub folder: String,
    pub branch: String,
    pub head: String,
    pub number: u32,
    pub pushed: Pushed,
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Pushed {
    Nowhere,
    Goes { remote: String, branch: String },
    Stays { why: String },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Kept {
    pub folder: String,
    pub number: u32,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Unasked {
    pub folder: String,
    pub why: String,
}

impl Plan {
    pub(crate) fn of(said: api::PrunesResponse) -> Plan {
        let api::PrunesResponse { repository, goes, kept, unasked } = said;
        let kept = kept.into_iter().map(|it| Kept { folder: it.folder, number: it.number, why: it.why });
        let unasked = unasked.into_iter().map(|it| Unasked { folder: it.folder, why: it.why });
        Plan {
            repository,
            goes: goes.into_iter().map(Goes::of).collect(),
            kept: kept.collect(),
            unasked: unasked.collect(),
        }
    }
}

impl Goes {
    fn of(said: prunes_response::Goes) -> Goes {
        let pushed = match said.pushed {
            Some(prunes_response::goes::Pushed::Remote(far)) => {
                Pushed::Goes { remote: far.remote, branch: far.branch }
            },
            Some(prunes_response::goes::Pushed::Stays(why)) => Pushed::Stays { why },
            None => Pushed::Nowhere,
        };
        let prunes_response::Goes { folder, branch, head, number, bytes, .. } = said;
        Goes { folder, branch, head, number, pushed, bytes }
    }

    pub(crate) fn deleting(self) -> prune_request::Deleting {
        let remote = match self.pushed {
            Pushed::Goes { remote, branch } => Some(prunes_response::RemoteBranch { remote, branch }),
            Pushed::Nowhere | Pushed::Stays { .. } => None,
        };
        let Goes { folder, branch, head, number, .. } = self;
        prune_request::Deleting { folder, branch, head, number, remote }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Pruned {
    Changed { plan: Plan },
    Went { outcomes: Vec<Outcome> },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Outcome {
    pub folder: String,
    pub came: Came,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Came {
    Gone,
    WentBut { why: String },
    Stays { why: String },
}

impl Pruned {
    pub(crate) fn of(said: Option<api::PruneResponse>) -> Result<Pruned, Unanswered> {
        let answer = said.and_then(|said| said.answer);
        match answer.ok_or_else(|| Unanswered::unread("it says nothing of how it went"))? {
            prune_response::Answer::Changed(plan) => Ok(Pruned::Changed { plan: Plan::of(plan) }),
            prune_response::Answer::Deleted(deleted) => {
                let outcomes = deleted.outcomes.into_iter().map(Outcome::of);
                Ok(Pruned::Went { outcomes: outcomes.collect::<Result<_, _>>()? })
            },
        }
    }
}

impl Outcome {
    fn of(said: prune_response::Outcome) -> Result<Outcome, Unanswered> {
        use prune_response::outcome::Came as Said;
        let folder = said.folder;
        let came = match said.came {
            Some(Said::Gone(())) => Came::Gone,
            Some(Said::WentBut(why)) => Came::WentBut { why },
            Some(Said::Stays(why)) => Came::Stays { why },
            None => return Err(Unanswered::unread(format!("it says nothing of how {folder} went"))),
        };
        Ok(Outcome { folder, came })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Rebased {
    pub branch: String,
    pub onto: String,
    pub came: Rebase,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Rebase {
    Already,
    Clean { ahead: u32 },
    Stopped { at: Option<String>, named: Vec<String>, unnamed: u32 },
}

impl Rebased {
    pub(crate) fn of(said: Option<api::RebaseResponse>) -> Result<Rebased, Unanswered> {
        let said = said.ok_or_else(|| Unanswered::unread("it ended with no rebase"))?;
        let came = match said.came.ok_or_else(|| Unanswered::unread("it says nothing of how it came out"))? {
            rebase_response::Came::Already(()) => Rebase::Already,
            rebase_response::Came::Clean(rebase_response::Clean { ahead }) => Rebase::Clean { ahead },
            rebase_response::Came::Stopped(stopped) => {
                let rebase_response::Stopped { at, conflicted, conflicted_count } = stopped;
                let wanted = usize::try_from(conflicted_count).unwrap_or(usize::MAX);
                let wanted = wanted.min(CONFLICTED_NAMED_MAX);
                if conflicted.len() != wanted {
                    let (named, count) = (conflicted.len(), conflicted_count);
                    let why = format!("it names {named} files of the {count} in conflict, not {wanted}");
                    return Err(Unanswered::unread(why));
                }
                let named = u32::try_from(conflicted.len());
                let named = named.expect("at most CONFLICTED_NAMED_MAX files are named");
                assert!(named <= conflicted_count);
                Rebase::Stopped { at, named: conflicted, unnamed: conflicted_count - named }
            },
        };
        Ok(Rebased { branch: said.branch, onto: said.onto, came })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct IssueRepository {
    pub remote: String,
    pub repository: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Filed {
    pub url: String,
    pub title: String,
}

impl Filed {
    pub(crate) fn of(said: Option<api::FileIssueResponse>) -> Result<Filed, Unanswered> {
        let said = said.ok_or_else(|| Unanswered::unread("it ended with no issue"))?;
        Ok(Filed { url: said.url, title: said.title })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_that_goes_is_deleted_with_what_was_pushed_of_it_and_nothing_else() {
        let far = prunes_response::RemoteBranch { remote: "origin".to_owned(), branch: "fix".to_owned() };
        let said = |pushed| prunes_response::Goes {
            folder: "/w/lune/fix".to_owned(),
            branch: "fix".to_owned(),
            head: "abc".to_owned(),
            number: 7,
            pushed,
            bytes: Some(4096),
        };
        let pushed = Goes::of(said(Some(prunes_response::goes::Pushed::Remote(far.clone()))));
        assert_eq!(pushed.pushed, Pushed::Goes { remote: "origin".to_owned(), branch: "fix".to_owned() });
        assert_eq!(pushed.deleting().remote, Some(far));
        let stays = Goes::of(said(Some(prunes_response::goes::Pushed::Stays("forked".to_owned()))));
        assert_eq!(stays.deleting().remote, None);
        let local = Goes::of(said(None)).deleting();
        assert_eq!((local.folder.as_str(), local.number, local.remote), ("/w/lune/fix", 7, None));
    }

    #[test]
    fn a_worktree_made_by_a_luned_too_old_to_read_what_it_opens_runs_a_shell() {
        use make_worktree_response::Landed as Said;
        let said = |landed| Some(api::MakeWorktreeResponse { landed: Some(landed) });
        assert_eq!(Landed::of(said(Said::Made(4))), Ok(Landed::Shell { worktree: 4 }));
        assert_eq!(Landed::of(said(Said::Started(4))), Ok(Landed::Started { worktree: 4 }));
        assert_eq!(Landed::of(said(Said::Found(4))), Ok(Landed::Found { worktree: 4 }));
    }

    #[test]
    fn a_rebase_stopped_names_its_conflicts_up_to_the_most_and_counts_the_rest() {
        let stopped = |named: usize, count| {
            let conflicted = (0..named).map(|file| format!("f{file}")).collect();
            let stopped = rebase_response::Stopped { at: None, conflicted, conflicted_count: count };
            let came = Some(rebase_response::Came::Stopped(stopped));
            Rebased::of(Some(api::RebaseResponse { came, ..api::RebaseResponse::default() }))
        };
        let unnamed = |rebased: Result<Rebased, Unanswered>| match rebased.unwrap().came {
            Rebase::Stopped { named, unnamed, .. } => (named.len(), unnamed),
            came => panic!("the rebase stopped: {came:?}"),
        };
        assert_eq!(unnamed(stopped(3, 3)), (3, 0));
        assert_eq!(unnamed(stopped(CONFLICTED_NAMED_MAX, 30)), (CONFLICTED_NAMED_MAX, 10));
        for (named, count) in [(3, 2), (2, 3), (CONFLICTED_NAMED_MAX + 1, 30)] {
            let unread = matches!(stopped(named, count), Err(Unanswered::InDoubt { .. }));
            assert!(unread, "{named} of {count}");
        }
    }

    #[test]
    fn an_answer_that_says_nothing_is_in_doubt() {
        assert!(matches!(Landed::of(None), Err(Unanswered::InDoubt { .. })));
        assert!(matches!(Pruned::of(Some(api::PruneResponse::default())), Err(Unanswered::InDoubt { .. })));
        let unsaid = api::RebaseResponse { branch: "fix".to_owned(), ..api::RebaseResponse::default() };
        assert!(matches!(Rebased::of(Some(unsaid)), Err(Unanswered::InDoubt { .. })));
    }
}
