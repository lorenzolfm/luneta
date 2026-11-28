use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::SystemTime;

use lune_protocol::agent_status::{AgentStatus, BlockedReason};
use lune_protocol::git::{GitStatus, Head, ParsedGitStatuses};
use lune_protocol::github::ParsedPullRequests;
use lune_protocol::made::Made;

use crate::api::project::clone;
use crate::api::{self, project, projects, remote, terminal, worktree};

#[derive(Debug, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Eq, Hash)]
pub struct TerminalKey {
    on: Option<String>,
    id: NonZeroU32,
    made: Made,
}

impl TerminalKey {
    fn of(on: Option<&str>, said: &api::TerminalKey) -> Result<Arc<TerminalKey>, String> {
        let id = NonZeroU32::new(said.id).ok_or("0 is no terminal's id")?;
        let made = said
            .made_at
            .ok_or_else(|| format!("terminal {id} has no made_at"))?;
        let made = Made::read(made).map_err(|why| format!("terminal {id}'s made_at: {why}"))?;
        let key = TerminalKey {
            on: on.map(str::to_owned),
            id,
            made,
        };
        assert_eq!(key.to_api().1, *said);
        Ok(Arc::new(key))
    }

    pub(crate) fn to_api(&self) -> (Option<api::On>, api::TerminalKey) {
        let on = self.on.clone().map(|destination| api::On { destination });
        (
            on,
            api::TerminalKey {
                id: self.id.get(),
                made_at: Some(self.made.into()),
            },
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Model {
    pub here: Held,
    pub far: Vec<Far>,
    pub inbox: Vec<Agent>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Far {
    pub on: String,
    pub reached: Reached,
    pub held: Held,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Reached {
    Reaching,
    Reached,
    Unreached {
        why: String,
        since: Option<SystemTime>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Held {
    Kept { projects: Vec<Project> },
    Unkept { why: String },
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub dir: String,
    pub missing: bool,
    pub holds: Holds,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Holds {
    Folder {
        worktree: Worktree,
    },
    Clone {
        worktrees: Vec<Worktree>,
        unopened: Unopened,
    },
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Unopened {
    Listed { subfolders: Vec<String> },
    Unlisted { why: String },
    Untold,
}

impl Holds {
    #[must_use]
    pub fn worktrees(&self) -> &[Worktree] {
        match self {
            Holds::Folder { worktree } => std::slice::from_ref(worktree),
            Holds::Clone { worktrees, .. } => worktrees,
            Holds::Untold => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Worktree {
    pub id: u64,
    pub name: String,
    pub renamed: bool,
    pub folder: String,
    pub branch: Option<String>,
    pub seen: Seen,
    pub views: Vec<View>,
    pub flow: Option<Flow>,
    pub browsers: Option<Vec<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum FlowBadgeKind {
    Running,
    WaitingInteractive,
    ConfirmMerge,
    Paused,
    Merged,
    ReadyToMerge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum FlowControl {
    Advance,
    AutoRest,
    Pause,
    Resume,
    RetryStep,
    SkipStep,
    TakeOver,
    Abort,
}

impl FlowControl {
    pub(crate) fn to_api(self) -> api::FlowControl {
        match self {
            FlowControl::Advance => api::FlowControl::Advance,
            FlowControl::AutoRest => api::FlowControl::AutoRest,
            FlowControl::Pause => api::FlowControl::Pause,
            FlowControl::Resume => api::FlowControl::Resume,
            FlowControl::RetryStep => api::FlowControl::RetryStep,
            FlowControl::SkipStep => api::FlowControl::SkipStep,
            FlowControl::TakeOver => api::FlowControl::TakeOver,
            FlowControl::Abort => api::FlowControl::Abort,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct Flow {
    pub flow_name: String,
    pub badge: String,
    pub badge_kind: FlowBadgeKind,
    pub detail: Option<String>,
    pub controls: Vec<FlowControl>,
    pub interactive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Seen {
    Missing,
    Unread,
    NoBranch,
    Working,
    PullRequestOpen { conflicting: bool },
    ChecksFailing { conflicting: bool },
    Merged,
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct View {
    pub id: u64,
    pub found: Found,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub command: Option<String>,
    pub folder: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
#[uniffi::export(Eq, Hash)]
pub enum Found {
    Key { key: Arc<TerminalKey> },
    Lost,
    Looking,
    Unread { why: String },
    Untold,
}

impl View {
    fn key(&self) -> Option<&Arc<TerminalKey>> {
        match &self.found {
            Found::Key { key } => Some(key),
            Found::Lost | Found::Looking | Found::Unread { .. } | Found::Untold => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
#[uniffi::export(Eq, Hash)]
pub struct Agent {
    pub on: Option<String>,
    pub terminal: Option<Arc<TerminalKey>>,
    pub name: String,
    pub placed: Option<Placed>,
    pub status: Status,
    pub since: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct Placed {
    pub project: u64,
    pub project_name: String,
    pub worktree: u64,
    pub worktree_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Status {
    Busy,
    Idle,
    Stopped,
    Done { message: Option<String> },
    Waiting { why: Why, message: Option<String> },
    Error { message: Option<String> },
    Shell,
    Other { said: String },
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Why {
    Permission,
    Input,
    Auth,
    Dialog,
    Other { said: String },
    Untold,
}

struct Seat<'a> {
    on: Option<&'a str>,
    agents: &'a [api::Agent],
    git_statuses: &'a [api::GitStatus],
    pull_requests: &'a [api::PullRequest],
    projects: Option<&'a api::Projects>,
}

impl Model {
    #[must_use]
    pub fn of(host: &api::Host) -> Model {
        let here = Seat {
            on: None,
            agents: &host.agents,
            git_statuses: &host.git_statuses,
            pull_requests: &host.pull_requests,
            projects: host.projects.as_ref(),
        };
        let mut seats = Vec::with_capacity(host.remotes.len());
        for remote in &host.remotes {
            let on = remote.destination.as_str();
            let seat = |agents, git_statuses, pull_requests, projects| Seat {
                on: Some(on),
                agents,
                git_statuses,
                pull_requests,
                projects,
            };
            match &remote.reached {
                Some(remote::Reached::Here(())) => {}
                Some(remote::Reached::Host(far)) => {
                    let seat = seat(
                        &far.agents,
                        &far.git_statuses,
                        &far.pull_requests,
                        far.projects.as_ref(),
                    );
                    seats.push((on, seat, Reached::Reached));
                }
                Some(remote::Reached::Unreached(unreached)) => {
                    let since = unreached
                        .since
                        .and_then(|since| SystemTime::try_from(since).ok());
                    let why = unreached.why.clone();
                    let seat = seat(&[], &[], &[], unreached.projects.as_ref());
                    seats.push((on, seat, Reached::Unreached { why, since }));
                }
                Some(remote::Reached::Reaching(())) | None => {
                    seats.push((on, seat(&[], &[], &[], None), Reached::Reaching));
                }
            }
        }
        let far_agents: usize = seats.iter().map(|(_, seat, _)| seat.agents.len()).sum();
        let agents_count = here.agents.len() + far_agents;
        let mut inbox = Vec::with_capacity(agents_count);
        let mut seated = |seat: &Seat<'_>| {
            let held = held_of(seat);
            inbox.extend(seat.agents.iter().map(|agent| agent_of(seat, &held, agent)));
            held
        };
        let here = seated(&here);
        let far = seats
            .into_iter()
            .map(|(on, seat, reached)| Far {
                on: on.to_owned(),
                reached,
                held: seated(&seat),
            })
            .collect();
        inbox.sort_by_key(inbox_place);
        assert_eq!(inbox.len(), agents_count);
        Model { here, far, inbox }
    }
}

fn inbox_place(agent: &Agent) -> (bool, bool, Option<SystemTime>) {
    match agent.status {
        Status::Waiting { .. } => (false, agent.since.is_none(), agent.since),
        Status::Busy
        | Status::Idle
        | Status::Stopped
        | Status::Done { .. }
        | Status::Error { .. }
        | Status::Shell
        | Status::Other { .. }
        | Status::Untold => (true, false, None),
    }
}

fn held_of(seat: &Seat<'_>) -> Held {
    let kept = match seat.projects.and_then(|projects| projects.held.as_ref()) {
        Some(projects::Held::Kept(kept)) => kept,
        Some(projects::Held::Unkept(why)) => return Held::Unkept { why: why.clone() },
        None => return Held::Untold,
    };
    let statuses = ParsedGitStatuses::of_api(seat.git_statuses);
    let branches: HashMap<&str, &str> = statuses
        .by_dir
        .iter()
        .filter_map(|(&dir, status)| match status {
            GitStatus::Checkout {
                head: Head::Branch { name, .. },
                ..
            } => Some((dir, name.as_str())),
            GitStatus::Outside
            | GitStatus::NoCheckout { .. }
            | GitStatus::Checkout {
                head: Head::Detached,
                ..
            } => None,
        })
        .collect();
    let pulls = ParsedPullRequests::of_api(seat.pull_requests);
    let conflicts: HashSet<&str> = pulls
        .by_dir
        .iter()
        .filter(|(_, pull)| pull.has_conflicts())
        .map(|(&dir, _)| dir)
        .collect();
    let projects = kept
        .projects
        .iter()
        .map(|it| project_of(seat.on, it, &branches, &conflicts))
        .collect();
    Held::Kept { projects }
}

fn project_of(
    on: Option<&str>,
    project: &api::Project,
    branches: &HashMap<&str, &str>,
    conflicts: &HashSet<&str>,
) -> Project {
    let holds = match &project.holds {
        Some(project::Holds::Folder(worktree)) => Holds::Folder {
            worktree: worktree_of(on, worktree, branches, conflicts),
        },
        Some(project::Holds::Clone(clone)) => {
            let worktrees = clone
                .worktrees
                .iter()
                .map(|it| worktree_of(on, it, branches, conflicts))
                .collect();
            let unopened = match &clone.listing {
                Some(clone::Listing::Unopened(unopened)) => Unopened::Listed {
                    subfolders: unopened.subfolders.clone(),
                },
                Some(clone::Listing::Unlisted(why)) => Unopened::Unlisted { why: why.clone() },
                None => Unopened::Untold,
            };
            Holds::Clone {
                worktrees,
                unopened,
            }
        }
        None => Holds::Untold,
    };
    Project {
        id: project.id,
        name: project.name.clone(),
        dir: project.dir.clone(),
        missing: project.missing,
        holds,
    }
}

fn worktree_of(
    on: Option<&str>,
    worktree: &api::Worktree,
    branches: &HashMap<&str, &str>,
    conflicts: &HashSet<&str>,
) -> Worktree {
    let conflicting = conflicts.contains(worktree.folder.as_str());
    let seen = match worktree.seen {
        Some(worktree::Seen::Missing(())) => Seen::Missing,
        Some(worktree::Seen::Unread(())) => Seen::Unread,
        Some(worktree::Seen::Stage(stage)) => match api::Stage::try_from(stage) {
            Ok(api::Stage::NoBranch) => Seen::NoBranch,
            Ok(api::Stage::Working) => Seen::Working,
            Ok(api::Stage::PullRequestOpen) => Seen::PullRequestOpen { conflicting },
            Ok(api::Stage::ChecksFailing) => Seen::ChecksFailing { conflicting },
            Ok(api::Stage::Merged) => Seen::Merged,
            Ok(api::Stage::Unspecified) | Err(_) => Seen::Untold,
        },
        None => Seen::Untold,
    };
    let browsers = worktree.views.as_ref().map(|views| {
        views
            .items
            .iter()
            .filter_map(|view| match &view.kind {
                Some(worktree::view::Kind::Browser(browser)) => Some(browser.id),
                Some(worktree::view::Kind::TerminalId(_)) | None => None,
            })
            .collect()
    });
    Worktree {
        id: worktree.id,
        name: worktree.name.clone(),
        renamed: worktree.renamed,
        folder: worktree.folder.clone(),
        branch: branches
            .get(worktree.folder.as_str())
            .map(|&branch| branch.to_owned()),
        seen,
        views: worktree
            .terminals
            .iter()
            .map(|it| view_of(on, it))
            .collect(),
        flow: worktree.flow.as_ref().and_then(flow_of),
        browsers,
    }
}

fn flow_of(said: &api::FlowSummary) -> Option<Flow> {
    let badge_kind = match api::FlowBadgeKind::try_from(said.badge_kind) {
        Ok(api::FlowBadgeKind::Running) => FlowBadgeKind::Running,
        Ok(api::FlowBadgeKind::WaitingInteractive) => FlowBadgeKind::WaitingInteractive,
        Ok(api::FlowBadgeKind::ConfirmMerge) => FlowBadgeKind::ConfirmMerge,
        Ok(api::FlowBadgeKind::Paused) => FlowBadgeKind::Paused,
        Ok(api::FlowBadgeKind::Merged) => FlowBadgeKind::Merged,
        Ok(api::FlowBadgeKind::ReadyToMerge) => FlowBadgeKind::ReadyToMerge,
        Ok(api::FlowBadgeKind::Unspecified) | Err(_) => return None,
    };
    let controls = said
        .controls
        .iter()
        .filter_map(|raw| match api::FlowControl::try_from(*raw) {
            Ok(api::FlowControl::Advance) => Some(FlowControl::Advance),
            Ok(api::FlowControl::AutoRest) => Some(FlowControl::AutoRest),
            Ok(api::FlowControl::Pause) => Some(FlowControl::Pause),
            Ok(api::FlowControl::Resume) => Some(FlowControl::Resume),
            Ok(api::FlowControl::RetryStep) => Some(FlowControl::RetryStep),
            Ok(api::FlowControl::SkipStep) => Some(FlowControl::SkipStep),
            Ok(api::FlowControl::TakeOver) => Some(FlowControl::TakeOver),
            Ok(api::FlowControl::Abort) => Some(FlowControl::Abort),
            Ok(api::FlowControl::Unspecified) | Err(_) => None,
        })
        .collect();
    let interactive = matches!(
        api::FlowMode::try_from(said.mode),
        Ok(api::FlowMode::Interactive)
    );
    Some(Flow {
        flow_name: said.flow_name.clone(),
        badge: said.badge.clone(),
        badge_kind,
        detail: said.detail.clone(),
        controls,
        interactive,
    })
}

fn view_of(on: Option<&str>, view: &api::Terminal) -> View {
    let key_of = |key: Option<&api::TerminalKey>| match key {
        Some(key) => match TerminalKey::of(on, key) {
            Ok(key) => Found::Key { key },
            Err(why) => Found::Unread { why },
        },
        None => Found::Unread {
            why: "no terminal key".to_owned(),
        },
    };
    let (found, topic, command) = match &view.state {
        Some(terminal::State::Running(running)) => (
            key_of(running.key.as_ref()),
            running
                .titled
                .as_ref()
                .and_then(|titled| titled.topic.clone()),
            running.command.clone(),
        ),
        Some(terminal::State::Ended(ended)) => (
            key_of(ended.key.as_ref()),
            ended
                .titled
                .as_ref()
                .and_then(|titled| titled.topic.clone()),
            None,
        ),
        Some(terminal::State::Unlisted(key)) => (key_of(Some(key)), None, None),
        Some(terminal::State::StateLost(())) => (Found::Lost, None, None),
        Some(terminal::State::StateLooking(())) => (Found::Looking, None, None),
        None => {
            let found = match &view.found {
                Some(terminal::Found::Key(key)) => key_of(Some(key)),
                Some(terminal::Found::Lost(())) => Found::Lost,
                Some(terminal::Found::Looking(())) => Found::Looking,
                None => Found::Untold,
            };
            let topic = view.titled.as_ref().and_then(|titled| titled.topic.clone());
            (found, topic, view.command.clone())
        }
    };
    View {
        id: view.id,
        found,
        name: view.name.clone(),
        topic,
        command,
        folder: view.folder.clone(),
    }
}

fn agent_of(seat: &Seat<'_>, held: &Held, agent: &api::Agent) -> Agent {
    let terminal = agent
        .terminal
        .as_ref()
        .and_then(|key| TerminalKey::of(seat.on, key).ok());
    let placed = match (held, &terminal) {
        (Held::Kept { projects }, Some(terminal)) => projects
            .iter()
            .flat_map(|project| {
                project
                    .holds
                    .worktrees()
                    .iter()
                    .map(move |it| (project, it))
            })
            .find(|(_, worktree)| worktree.views.iter().any(|it| it.key() == Some(terminal)))
            .map(|(project, worktree)| Placed {
                project: project.id,
                project_name: project.name.clone(),
                worktree: worktree.id,
                worktree_name: worktree.name.clone(),
            }),
        _ => None,
    };
    Agent {
        on: seat.on.map(str::to_owned),
        terminal,
        name: name_of(agent),
        placed,
        status: status_of(agent),
        since: agent
            .status_since
            .and_then(|since| SystemTime::try_from(since).ok()),
    }
}

fn name_of(agent: &api::Agent) -> String {
    let given = agent.name.as_ref().and_then(api::Name::given);
    let topic = || agent.live_topic();
    let auto = || agent.name.as_ref().and_then(api::Name::auto);
    let folder = || agent.cwd.rsplit('/').find(|part| !part.is_empty());
    let name = given
        .or_else(topic)
        .or_else(auto)
        .or_else(folder)
        .unwrap_or(agent.cwd.as_str())
        .to_owned();
    if !agent.cwd.is_empty() {
        assert!(
            !name.is_empty(),
            "an agent with a cwd gets a non-empty name"
        );
    }
    name
}

fn status_of(agent: &api::Agent) -> Status {
    match AgentStatus::of_api(agent) {
        AgentStatus::Working => Status::Busy,
        AgentStatus::Idle => Status::Idle,
        AgentStatus::Stopped => Status::Stopped,
        AgentStatus::Done { message } => Status::Done {
            message: message.map(Into::into),
        },
        AgentStatus::Error { message } => Status::Error {
            message: message.map(Into::into),
        },
        AgentStatus::Shell => Status::Shell,
        AgentStatus::Blocked { reason, message } => Status::Waiting {
            why: why_of(reason),
            message: message.map(Into::into),
        },
        AgentStatus::Other(said) => Status::Other { said: said.into() },
        AgentStatus::Unreported => Status::Untold,
    }
}

fn why_of(reason: BlockedReason) -> Why {
    match reason {
        BlockedReason::Permission => Why::Permission,
        BlockedReason::Question => Why::Input,
        BlockedReason::Auth => Why::Auth,
        BlockedReason::Dialog => Why::Dialog,
        BlockedReason::Other(said) => Why::Other { said: said.into() },
        BlockedReason::Unspecified => Why::Untold,
    }
}

#[cfg(test)]
mod tests {
    use prost_types::Timestamp;

    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey {
            id,
            made_at: Some(Timestamp {
                seconds: 1_700_000_000,
                nanos: 7,
            }),
        }
    }

    fn agent(pid: u32, status: api::Status, since: i64, terminal: Option<u32>) -> api::Agent {
        api::Agent {
            pid,
            cwd: format!("/w/lune/agent-{pid}"),
            status: Some(api::agent::Status::KnownStatus(status.into())),
            status_since: Some(Timestamp {
                seconds: since,
                nanos: 0,
            }),
            terminal: terminal.map(key),
            waiting_for: (status == api::Status::Waiting).then_some(
                api::agent::WaitingFor::KnownWaitingFor(api::WaitingFor::PermissionPrompt.into()),
            ),
            ..api::Agent::default()
        }
    }

    fn kept(projects: Vec<api::Project>) -> api::Projects {
        let kept = projects::Kept {
            projects,
            version: None,
        };
        api::Projects {
            held: Some(projects::Held::Kept(kept)),
        }
    }

    fn clone_of(worktrees: Vec<api::Worktree>) -> api::Project {
        let clone = project::Clone {
            worktrees,
            listing: None,
        };
        api::Project {
            id: 1,
            name: "lune".to_owned(),
            dir: "/w/lune".to_owned(),
            holds: Some(project::Holds::Clone(clone)),
            ..api::Project::default()
        }
    }

    fn worktree_with(id: u64, name: &str, terminal: u32) -> api::Worktree {
        let view = api::Terminal {
            id: u64::from(terminal) + 100,
            found: Some(terminal::Found::Key(key(terminal))),
            folder: format!("/w/lune/{name}"),
            ..api::Terminal::default()
        };
        api::Worktree {
            id,
            name: name.to_owned(),
            folder: format!("/w/lune/{name}"),
            terminals: vec![view],
            ..api::Worktree::default()
        }
    }

    #[test]
    fn the_inbox_puts_the_agents_that_wait_first_the_longest_waiting_on_top() {
        let host = api::Host {
            agents: vec![
                agent(1, api::Status::Busy, 10, None),
                agent(2, api::Status::Waiting, 30, None),
                agent(3, api::Status::Idle, 5, None),
                agent(4, api::Status::Waiting, 20, None),
                api::Agent {
                    status_since: None,
                    ..agent(5, api::Status::Waiting, 0, None)
                },
            ],
            ..api::Host::default()
        };
        let model = Model::of(&host);
        let names: Vec<&str> = model
            .inbox
            .iter()
            .map(|agent| agent.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["agent-4", "agent-2", "agent-5", "agent-1", "agent-3"]
        );
        assert_eq!(
            model.inbox[0].status,
            Status::Waiting {
                why: Why::Permission,
                message: None
            }
        );
    }

    #[test]
    fn an_agent_name_prefers_given_then_topic_then_auto_then_folder() {
        let named = |text: &str, source: api::NameSource| {
            Some(api::Name {
                text: text.to_owned(),
                source: Some(source.into()),
            })
        };
        let mut given = agent(1, api::Status::Idle, 1, None);
        given.name = named("fold", api::NameSource::User);
        given.topic = Some("Session folds".to_owned());
        assert_eq!(name_of(&given), "fold");

        let mut auto_with_topic = agent(2, api::Status::Idle, 2, None);
        auto_with_topic.name = named("Initial title", api::NameSource::Auto);
        auto_with_topic.topic = Some("View tabs".to_owned());
        assert_eq!(name_of(&auto_with_topic), "View tabs");

        let mut auto_only = agent(3, api::Status::Idle, 3, None);
        auto_only.name = named("Initial title", api::NameSource::Auto);
        auto_only.topic = Some("   ".to_owned());
        assert_eq!(name_of(&auto_only), "Initial title");

        let mut derived_only = agent(4, api::Status::Idle, 4, None);
        derived_only.name = named("lune-main", api::NameSource::Derived);
        assert_eq!(name_of(&derived_only), "agent-4");
    }

    #[test]
    fn an_agent_is_placed_in_the_worktree_whose_view_holds_its_terminal() {
        let worktrees = vec![worktree_with(5, "fix", 9), worktree_with(6, "feat", 8)];
        let host = api::Host {
            agents: vec![agent(1, api::Status::Waiting, 1, Some(8))],
            projects: Some(kept(vec![clone_of(worktrees)])),
            ..api::Host::default()
        };
        let model = Model::of(&host);
        let agent = &model.inbox[0];
        let placed = Placed {
            project: 1,
            project_name: "lune".to_owned(),
            worktree: 6,
            worktree_name: "feat".to_owned(),
        };
        assert_eq!(agent.placed, Some(placed));
        assert_eq!(agent.terminal.as_ref().map(|it| it.id.get()), Some(8));
        assert_eq!(
            agent.terminal.as_ref().map(|it| it.to_api()),
            Some((None, key(8)))
        );
        let Held::Kept { projects } = &model.here else {
            panic!("the projects are kept: {:?}", model.here);
        };
        let Holds::Clone { worktrees, .. } = &projects[0].holds else {
            panic!("the project is a clone: {:?}", projects[0].holds);
        };
        assert_eq!(worktrees[1].views[0].key(), agent.terminal.as_ref());
    }

    #[test]
    fn a_worktree_s_branch_is_the_one_its_folder_has_checked_out() {
        let branch = api::Branch {
            name: "issue-723".to_owned(),
            ..api::Branch::default()
        };
        let head = Some(api::checkout::Head::Branch(branch));
        let checkout = api::Checkout {
            head,
            changes: Some(api::Changes::default()),
            ..api::Checkout::default()
        };
        let host = api::Host {
            git_statuses: vec![api::GitStatus {
                dir: "/w/lune/fix".to_owned(),
                stands: Some(api::git_status::Stands::Checkout(checkout)),
            }],
            projects: Some(kept(vec![clone_of(vec![worktree_with(5, "fix", 9)])])),
            ..api::Host::default()
        };
        let Held::Kept { projects } = &Model::of(&host).here else {
            panic!("the projects are kept");
        };
        assert_eq!(
            projects[0].holds.worktrees()[0].branch.as_deref(),
            Some("issue-723")
        );
    }

    #[test]
    fn a_worktree_says_when_its_open_pull_request_has_merge_conflicts() {
        use lune_protocol::github::{Mergeability, PullPage, PullRequest, State};

        let open_pull = |dir: &str, draft, mergeability| {
            PullRequest {
                page: PullPage::parse("https://github.com/o/r/pull/1").unwrap(),
                state: State::Open {
                    draft,
                    checks: None,
                    mergeability,
                },
                stale: false,
            }
            .to_api(dir)
        };
        let worktree = |id, name: &str, seen| api::Worktree {
            seen,
            ..worktree_with(id, name, u32::try_from(id).unwrap())
        };
        let stage = |stage: api::Stage| Some(worktree::Seen::Stage(stage.into()));
        let worktrees = vec![
            worktree(1, "open", stage(api::Stage::PullRequestOpen)),
            worktree(2, "draft", stage(api::Stage::ChecksFailing)),
            worktree(3, "clean", stage(api::Stage::PullRequestOpen)),
            worktree(4, "merged", stage(api::Stage::Merged)),
            worktree(5, "gone", Some(worktree::Seen::Missing(()))),
            worktree(6, "working", stage(api::Stage::Working)),
            worktree(7, "unread", Some(worktree::Seen::Unread(()))),
            worktree(8, "detached", stage(api::Stage::NoBranch)),
            worktree(9, "untold", None),
            worktree(10, "dup", stage(api::Stage::PullRequestOpen)),
            worktree(11, "legacy", stage(api::Stage::PullRequestOpen)),
        ];
        let conflicting = Some(Mergeability::Conflicting);
        let mergeable = Some(Mergeability::Mergeable);
        let corrupt_merged = api::PullRequest {
            dir: "/w/lune/merged".to_owned(),
            state: api::PullRequestState::Merged.into(),
            url: "https://github.com/o/r/pull/1".to_owned(),
            checks: None,
            stale: false,
            detail: Some(api::pull_request::Detail::Open(api::pull_request::Open {
                draft: false,
                checks: None,
                mergeability: api::Mergeability::Conflicting.into(),
            })),
        };
        let legacy_open = api::PullRequest {
            dir: "/w/lune/legacy".to_owned(),
            state: api::PullRequestState::Open.into(),
            url: "https://github.com/o/r/pull/2".to_owned(),
            checks: None,
            stale: false,
            detail: None,
        };
        let host = api::Host {
            pull_requests: vec![
                open_pull("/w/lune/open", false, conflicting),
                open_pull("/w/lune/draft", true, conflicting),
                open_pull("/w/lune/clean", false, mergeable),
                corrupt_merged,
                open_pull("/w/lune/gone", false, conflicting),
                open_pull("/w/lune/working", false, conflicting),
                open_pull("/w/lune/unread", false, conflicting),
                open_pull("/w/lune/detached", false, conflicting),
                open_pull("/w/lune/untold", false, conflicting),
                open_pull("/w/lune/dup", false, mergeable),
                open_pull("/w/lune/dup", false, conflicting),
                legacy_open,
            ],
            projects: Some(kept(vec![clone_of(worktrees)])),
            ..api::Host::default()
        };
        let Held::Kept { projects } = &Model::of(&host).here else {
            panic!("the projects are kept");
        };
        let seen: Vec<(&str, Seen)> = projects[0]
            .holds
            .worktrees()
            .iter()
            .map(|worktree| (worktree.name.as_str(), worktree.seen))
            .collect();
        assert_eq!(
            seen,
            [
                ("open", Seen::PullRequestOpen { conflicting: true }),
                ("draft", Seen::ChecksFailing { conflicting: true }),
                ("clean", Seen::PullRequestOpen { conflicting: false }),
                ("merged", Seen::Merged),
                ("gone", Seen::Missing),
                ("working", Seen::Working),
                ("unread", Seen::Unread),
                ("detached", Seen::NoBranch),
                ("untold", Seen::Untold),
                ("dup", Seen::PullRequestOpen { conflicting: false }),
                ("legacy", Seen::PullRequestOpen { conflicting: false }),
            ]
        );
    }

    #[test]
    fn a_terminal_key_names_the_machine_its_terminal_runs_on() {
        let made = key(17);
        let here = TerminalKey::of(None, &made).unwrap();
        let far = TerminalKey::of(Some("far"), &made).unwrap();
        assert_eq!(here.to_api(), (None, made));
        assert_eq!(
            far.to_api(),
            (
                Some(api::On {
                    destination: "far".to_owned()
                }),
                made
            )
        );
        assert_ne!(here, far, "terminal 17 here is not terminal 17 on far");
    }

    #[test]
    fn a_view_says_whether_its_terminal_is_found_lost_looked_for_or_unread() {
        let found = |found| {
            view_of(
                Some("far"),
                &api::Terminal {
                    found,
                    ..api::Terminal::default()
                },
            )
            .found
        };
        let three = TerminalKey::of(Some("far"), &key(3)).unwrap();
        assert_eq!(
            found(Some(terminal::Found::Key(key(3)))),
            Found::Key { key: three.clone() }
        );
        assert_eq!(found(Some(terminal::Found::Lost(()))), Found::Lost);
        assert_eq!(found(Some(terminal::Found::Looking(()))), Found::Looking);
        assert_eq!(found(None), Found::Untold);
        let unmade = [
            api::TerminalKey {
                made_at: None,
                ..key(3)
            },
            key(0),
        ];
        for unmade in unmade {
            let unread = found(Some(terminal::Found::Key(unmade)));
            assert!(
                matches!(unread, Found::Unread { .. }),
                "{unmade:?}: {unread:?}"
            );
        }

        let running = view_of(
            Some("far"),
            &api::Terminal {
                state: Some(terminal::State::Running(terminal::Running {
                    key: Some(key(3)),
                    command: Some("claude".to_owned()),
                    titled: Some(api::Titled {
                        topic: Some("Fixing".to_owned()),
                    }),
                    shell: None,
                })),
                ..api::Terminal::default()
            },
        );
        assert_eq!(running.found, Found::Key { key: three.clone() });
        assert_eq!(running.command.as_deref(), Some("claude"));
        assert_eq!(running.topic.as_deref(), Some("Fixing"));

        let ended = view_of(
            Some("far"),
            &api::Terminal {
                command: Some("stale".to_owned()),
                state: Some(terminal::State::Ended(terminal::Ended {
                    key: Some(key(3)),
                    titled: Some(api::Titled {
                        topic: Some("Done".to_owned()),
                    }),
                })),
                ..api::Terminal::default()
            },
        );
        assert_eq!(ended.found, Found::Key { key: three.clone() });
        assert_eq!(ended.command, None);
        assert_eq!(ended.topic.as_deref(), Some("Done"));

        let unlisted = view_of(
            Some("far"),
            &api::Terminal {
                state: Some(terminal::State::Unlisted(key(3))),
                ..api::Terminal::default()
            },
        );
        assert_eq!(unlisted.found, Found::Key { key: three });
        assert_eq!(unlisted.command, None);
        assert_eq!(unlisted.topic, None);

        let lost = view_of(
            Some("far"),
            &api::Terminal {
                state: Some(terminal::State::StateLost(())),
                ..api::Terminal::default()
            },
        );
        assert_eq!(lost.found, Found::Lost);
        let looking = view_of(
            Some("far"),
            &api::Terminal {
                state: Some(terminal::State::StateLooking(())),
                ..api::Terminal::default()
            },
        );
        assert_eq!(looking.found, Found::Looking);
    }

    #[test]
    fn a_clone_whose_worktrees_luned_could_not_list_says_why() {
        let unlisted = clone::Listing::Unlisted("/w/lune is missing".to_owned());
        let listed = clone::Listing::Unopened(api::project::Unopened {
            subfolders: vec!["old".to_owned()],
        });
        let unopened = |listing| {
            let mut project = clone_of(vec![worktree_with(5, "fix", 9)]);
            let Some(project::Holds::Clone(clone)) = &mut project.holds else {
                panic!("the project is a clone");
            };
            clone.listing = listing;
            let host = api::Host {
                projects: Some(kept(vec![project])),
                ..api::Host::default()
            };
            match Model::of(&host).here {
                Held::Kept { mut projects } => match projects.remove(0).holds {
                    Holds::Clone {
                        worktrees,
                        unopened,
                    } => (worktrees.len(), unopened),
                    holds => panic!("the project is a clone: {holds:?}"),
                },
                held => panic!("the projects are kept: {held:?}"),
            }
        };
        let why = Unopened::Unlisted {
            why: "/w/lune is missing".to_owned(),
        };
        assert_eq!(unopened(Some(unlisted)), (1, why));
        let subfolders = vec!["old".to_owned()];
        assert_eq!(unopened(Some(listed)), (1, Unopened::Listed { subfolders }));
        assert_eq!(unopened(None), (1, Unopened::Untold));
    }

    #[test]
    fn each_machine_watched_is_a_machine_but_this_one_named_again() {
        let far = remote::Host {
            agents: vec![agent(7, api::Status::Waiting, 3, None)],
            ..remote::Host::default()
        };
        let unreached = remote::Unreached {
            why: "ssh ended".to_owned(),
            ..remote::Unreached::default()
        };
        let remote = |destination: &str, reached| api::Remote {
            destination: destination.to_owned(),
            reached: Some(reached),
        };
        let host = api::Host {
            remotes: vec![
                remote("far", remote::Reached::Host(far)),
                remote("me@here", remote::Reached::Here(())),
                remote("vps", remote::Reached::Unreached(unreached)),
                remote("mac", remote::Reached::Reaching(())),
            ],
            ..api::Host::default()
        };
        let model = Model::of(&host);
        let far: Vec<(&str, &Reached)> = model
            .far
            .iter()
            .map(|it| (it.on.as_str(), &it.reached))
            .collect();
        let unreached = Reached::Unreached {
            why: "ssh ended".to_owned(),
            since: None,
        };
        assert_eq!(
            far,
            [
                ("far", &Reached::Reached),
                ("vps", &unreached),
                ("mac", &Reached::Reaching)
            ]
        );
        assert_eq!(model.inbox[0].on.as_deref(), Some("far"));
        assert_eq!(model.here, Held::Untold);
    }

    #[test]
    fn done_error_auth_and_status_messages_map_onto_the_phone_model() {
        let with_reported = |pid, reported_status: api::agent::ReportedStatus| api::Agent {
            pid,
            cwd: format!("/w/a-{pid}"),
            reported_status: Some(reported_status),
            ..api::Agent::default()
        };
        let host = api::Host {
            agents: vec![
                with_reported(
                    1,
                    api::agent::ReportedStatus::Done(api::DoneStatus {
                        message: Some("All tests pass".to_owned()),
                    }),
                ),
                with_reported(
                    2,
                    api::agent::ReportedStatus::Error(api::ErrorStatus {
                        message: Some("Token expired".to_owned()),
                    }),
                ),
                with_reported(
                    3,
                    api::agent::ReportedStatus::Blocked(api::BlockedStatus {
                        reason: Some(api::blocked_status::Reason::Known(
                            api::WaitingFor::Auth.into(),
                        )),
                        message: Some("Log in".to_owned()),
                    }),
                ),
                with_reported(4, api::agent::ReportedStatus::Stopped(())),
            ],
            ..api::Host::default()
        };
        let model = Model::of(&host);
        assert_eq!(
            model.inbox[0].status,
            Status::Waiting {
                why: Why::Auth,
                message: Some("Log in".to_owned())
            }
        );
        assert_eq!(
            model.inbox[1].status,
            Status::Done {
                message: Some("All tests pass".to_owned())
            }
        );
        assert_eq!(
            model.inbox[2].status,
            Status::Error {
                message: Some("Token expired".to_owned())
            }
        );
        assert_eq!(model.inbox[3].status, Status::Stopped);
    }

    #[test]
    fn a_worktree_flow_maps_badge_controls_and_pause_detail_onto_the_phone_model() {
        let mut wt = worktree_with(5, "fix", 9);
        wt.flow = Some(api::FlowSummary {
            worktree: 5,
            flow_name: "default".to_owned(),
            mode: api::FlowMode::Unattended.into(),
            harness: api::Harness::ClaudeCode.into(),
            step_number: 2,
            step_total: 4,
            stage_label: "illegal-states".to_owned(),
            badge: "paused".to_owned(),
            badge_kind: api::FlowBadgeKind::Paused.into(),
            pause_reason: Some(api::FlowPauseReason::DirtyTree.into()),
            detail: Some("working tree has uncommitted changes".to_owned()),
            active_view: Some(109),
            resume_session: Some("sess-123".to_owned()),
            controls: vec![
                api::FlowControl::Resume.into(),
                api::FlowControl::TakeOver.into(),
                api::FlowControl::Abort.into(),
            ],
        });
        let host = api::Host {
            projects: Some(kept(vec![clone_of(vec![wt])])),
            ..api::Host::default()
        };
        let Held::Kept { projects } = &Model::of(&host).here else {
            panic!("the projects are kept");
        };
        let flow = projects[0].holds.worktrees()[0].flow.as_ref().unwrap();
        assert_eq!(flow.flow_name, "default");
        assert_eq!(flow.badge, "paused");
        assert_eq!(flow.badge_kind, FlowBadgeKind::Paused);
        assert_eq!(
            flow.detail.as_deref(),
            Some("working tree has uncommitted changes")
        );
        assert_eq!(
            flow.controls,
            vec![
                FlowControl::Resume,
                FlowControl::TakeOver,
                FlowControl::Abort
            ]
        );
        assert!(!flow.interactive);
    }

    #[test]
    fn a_flow_of_a_badge_kind_the_phone_does_not_know_is_not_shown_as_paused() {
        let summary = |badge_kind: api::FlowBadgeKind| api::FlowSummary {
            worktree: 5,
            flow_name: "default".to_owned(),
            mode: api::FlowMode::Interactive.into(),
            harness: api::Harness::ClaudeCode.into(),
            step_number: 4,
            step_total: 4,
            stage_label: "land".to_owned(),
            badge: "4/4 merge?".to_owned(),
            badge_kind: badge_kind.into(),
            pause_reason: None,
            detail: None,
            active_view: None,
            resume_session: None,
            controls: vec![api::FlowControl::Advance.into()],
        };
        let confirming = flow_of(&summary(api::FlowBadgeKind::ConfirmMerge)).unwrap();
        assert_eq!(confirming.badge_kind, FlowBadgeKind::ConfirmMerge);
        assert_eq!(flow_of(&summary(api::FlowBadgeKind::Unspecified)), None);
        let mut unknown = summary(api::FlowBadgeKind::Paused);
        unknown.badge_kind = 99;
        assert_eq!(flow_of(&unknown), None);
    }

    #[test]
    fn a_worktree_tracks_browser_view_ids_only_when_luned_sends_views() {
        use api::worktree::{View as ApiView, Views, view::Kind};

        let legacy = worktree_with(1, "legacy", 1);
        let mut terminal_only = worktree_with(2, "term", 2);
        terminal_only.views = Some(Views {
            items: vec![ApiView {
                kind: Some(Kind::TerminalId(2)),
            }],
        });
        let mut with_browsers = worktree_with(3, "browse", 3);
        with_browsers.views = Some(Views {
            items: vec![
                ApiView {
                    kind: Some(Kind::TerminalId(3)),
                },
                ApiView {
                    kind: Some(Kind::Browser(api::Browser {
                        id: 20,
                        url: Some("https://example.com".to_owned()),
                    })),
                },
                ApiView {
                    kind: Some(Kind::Browser(api::Browser { id: 21, url: None })),
                },
            ],
        });
        let host = api::Host {
            projects: Some(kept(vec![clone_of(vec![
                legacy,
                terminal_only,
                with_browsers,
            ])])),
            ..api::Host::default()
        };
        let Held::Kept { projects } = Model::of(&host).here else {
            panic!("the projects are kept");
        };
        let worktrees = projects[0].holds.worktrees();
        assert_eq!(worktrees[0].browsers, None);
        assert_eq!(worktrees[1].browsers, Some(Vec::new()));
        assert_eq!(worktrees[2].browsers, Some(vec![20, 21]));
    }
}
