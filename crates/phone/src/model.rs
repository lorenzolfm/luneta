use std::collections::HashMap;
use std::time::SystemTime;

use crate::api::{self, project, projects, remote, terminal, worktree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct TerminalKey {
    pub id: u32,
    pub made_at: SystemTime,
}

impl TerminalKey {
    fn of(key: &api::TerminalKey) -> Option<TerminalKey> {
        let made_at = SystemTime::try_from(key.made_at?).ok()?;
        Some(TerminalKey { id: key.id, made_at })
    }

    pub(crate) fn to_api(self) -> api::TerminalKey {
        api::TerminalKey { id: self.id, made_at: Some(self.made_at.into()) }
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
    Unreached { why: String, since: Option<SystemTime> },
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
    Folder { worktree: Worktree },
    Clone { worktrees: Vec<Worktree> },
    Untold,
}

impl Holds {
    #[must_use]
    pub fn worktrees(&self) -> &[Worktree] {
        match self {
            Holds::Folder { worktree } => std::slice::from_ref(worktree),
            Holds::Clone { worktrees } => worktrees,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Seen {
    Missing,
    Unread,
    NoBranch,
    Working,
    PullRequestOpen,
    ChecksFailing,
    Merged,
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct View {
    pub id: u64,
    pub terminal: Option<TerminalKey>,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub command: Option<String>,
    pub folder: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Agent {
    pub on: Option<String>,
    pub terminal: Option<TerminalKey>,
    pub name: String,
    pub placed: Option<Placed>,
    pub status: Status,
    pub since: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Placed {
    pub project: u64,
    pub project_name: String,
    pub worktree: u64,
    pub worktree_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Status {
    Busy,
    Idle,
    Waiting { why: Why },
    Shell,
    Other { said: String },
    Untold,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Why {
    Permission,
    Input,
    Dialog,
    Other { said: String },
    Untold,
}

struct Seat<'a> {
    on: Option<&'a str>,
    agents: &'a [api::Agent],
    git_statuses: &'a [api::GitStatus],
    projects: Option<&'a api::Projects>,
}

impl Model {
    #[must_use]
    pub fn of(host: &api::Host) -> Model {
        let here = Seat {
            on: None,
            agents: &host.agents,
            git_statuses: &host.git_statuses,
            projects: host.projects.as_ref(),
        };
        let mut seats = Vec::with_capacity(host.remotes.len());
        for remote in &host.remotes {
            let on = remote.destination.as_str();
            let seat =
                |agents, git_statuses, projects| Seat { on: Some(on), agents, git_statuses, projects };
            match &remote.reached {
                Some(remote::Reached::Here(())) => {},
                Some(remote::Reached::Host(far)) => {
                    let seat = seat(&far.agents, &far.git_statuses, far.projects.as_ref());
                    seats.push((on, seat, Reached::Reached));
                },
                Some(remote::Reached::Unreached(unreached)) => {
                    let since = unreached.since.and_then(|since| SystemTime::try_from(since).ok());
                    let why = unreached.why.clone();
                    let seat = seat(&[], &[], unreached.projects.as_ref());
                    seats.push((on, seat, Reached::Unreached { why, since }));
                },
                Some(remote::Reached::Reaching(())) | None => {
                    seats.push((on, seat(&[], &[], None), Reached::Reaching));
                },
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
            .map(|(on, seat, reached)| Far { on: on.to_owned(), reached, held: seated(&seat) })
            .collect();
        inbox.sort_by_key(inbox_place);
        assert_eq!(inbox.len(), agents_count);
        Model { here, far, inbox }
    }
}

fn inbox_place(agent: &Agent) -> (bool, bool, Option<SystemTime>) {
    match agent.status {
        Status::Waiting { .. } => (false, agent.since.is_none(), agent.since),
        Status::Busy | Status::Idle | Status::Shell | Status::Other { .. } | Status::Untold => {
            (true, false, None)
        },
    }
}

fn held_of(seat: &Seat<'_>) -> Held {
    let branches: HashMap<&str, &str> = seat
        .git_statuses
        .iter()
        .filter_map(|status| match &status.stands {
            Some(api::git_status::Stands::Checkout(api::Checkout {
                head: Some(api::checkout::Head::Branch(branch)),
                ..
            })) => Some((status.dir.as_str(), branch.name.as_str())),
            _ => None,
        })
        .collect();
    match seat.projects.and_then(|projects| projects.held.as_ref()) {
        Some(projects::Held::Kept(kept)) => {
            let projects = kept.projects.iter().map(|it| project_of(it, &branches)).collect();
            Held::Kept { projects }
        },
        Some(projects::Held::Unkept(why)) => Held::Unkept { why: why.clone() },
        None => Held::Untold,
    }
}

fn project_of(project: &api::Project, branches: &HashMap<&str, &str>) -> Project {
    let holds = match &project.holds {
        Some(project::Holds::Folder(worktree)) => {
            Holds::Folder { worktree: worktree_of(worktree, branches) }
        },
        Some(project::Holds::Clone(clone)) => {
            let worktrees = clone.worktrees.iter().map(|it| worktree_of(it, branches)).collect();
            Holds::Clone { worktrees }
        },
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

fn worktree_of(worktree: &api::Worktree, branches: &HashMap<&str, &str>) -> Worktree {
    let seen = match worktree.seen {
        Some(worktree::Seen::Missing(())) => Seen::Missing,
        Some(worktree::Seen::Unread(())) => Seen::Unread,
        Some(worktree::Seen::Stage(stage)) => match api::Stage::try_from(stage) {
            Ok(api::Stage::NoBranch) => Seen::NoBranch,
            Ok(api::Stage::Working) => Seen::Working,
            Ok(api::Stage::PullRequestOpen) => Seen::PullRequestOpen,
            Ok(api::Stage::ChecksFailing) => Seen::ChecksFailing,
            Ok(api::Stage::Merged) => Seen::Merged,
            Ok(api::Stage::Unspecified) | Err(_) => Seen::Untold,
        },
        None => Seen::Untold,
    };
    Worktree {
        id: worktree.id,
        name: worktree.name.clone(),
        renamed: worktree.renamed,
        folder: worktree.folder.clone(),
        branch: branches.get(worktree.folder.as_str()).map(|&branch| branch.to_owned()),
        seen,
        views: worktree.terminals.iter().map(view_of).collect(),
    }
}

fn view_of(view: &api::Terminal) -> View {
    let terminal = match &view.found {
        Some(terminal::Found::Key(key)) => TerminalKey::of(key),
        Some(terminal::Found::Lost(()) | terminal::Found::Looking(())) | None => None,
    };
    View {
        id: view.id,
        terminal,
        name: view.name.clone(),
        topic: view.titled.as_ref().and_then(|titled| titled.topic.clone()),
        command: view.command.clone(),
        folder: view.folder.clone(),
    }
}

fn agent_of(seat: &Seat<'_>, held: &Held, agent: &api::Agent) -> Agent {
    let terminal = agent.terminal.as_ref().and_then(TerminalKey::of);
    let placed = match (held, terminal) {
        (Held::Kept { projects }, Some(terminal)) => projects
            .iter()
            .flat_map(|project| project.holds.worktrees().iter().map(move |it| (project, it)))
            .find(|(_, worktree)| worktree.views.iter().any(|it| it.terminal == Some(terminal)))
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
        since: agent.status_since.and_then(|since| SystemTime::try_from(since).ok()),
    }
}

fn name_of(agent: &api::Agent) -> String {
    let named = agent.name.as_ref().map(|name| name.text.trim()).filter(|text| !text.is_empty());
    let topic = || agent.topic.as_deref().map(str::trim).filter(|topic| !topic.is_empty());
    let folder = || agent.cwd.rsplit('/').find(|part| !part.is_empty());
    named.or_else(topic).or_else(folder).unwrap_or(agent.cwd.as_str()).to_owned()
}

fn status_of(agent: &api::Agent) -> Status {
    let known = match &agent.status {
        Some(api::agent::Status::KnownStatus(known)) => api::Status::try_from(*known),
        Some(api::agent::Status::OtherStatus(said)) => return Status::Other { said: said.clone() },
        None => return Status::Untold,
    };
    match known {
        Ok(api::Status::Busy) => Status::Busy,
        Ok(api::Status::Idle) => Status::Idle,
        Ok(api::Status::Shell) => Status::Shell,
        Ok(api::Status::Waiting) => Status::Waiting { why: why_of(agent) },
        Ok(api::Status::Unspecified) | Err(_) => Status::Untold,
    }
}

fn why_of(agent: &api::Agent) -> Why {
    match &agent.waiting_for {
        Some(api::agent::WaitingFor::KnownWaitingFor(known)) => {
            match api::WaitingFor::try_from(*known) {
                Ok(api::WaitingFor::PermissionPrompt) => Why::Permission,
                Ok(api::WaitingFor::InputNeeded) => Why::Input,
                Ok(api::WaitingFor::DialogOpen) => Why::Dialog,
                Ok(api::WaitingFor::Unspecified) | Err(_) => Why::Untold,
            }
        },
        Some(api::agent::WaitingFor::OtherWaitingFor(said)) => Why::Other { said: said.clone() },
        None => Why::Untold,
    }
}

#[cfg(test)]
mod tests {
    use prost_types::Timestamp;

    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey { id, made_at: Some(Timestamp { seconds: 1_700_000_000, nanos: 7 }) }
    }

    fn agent(pid: u32, status: api::Status, since: i64, terminal: Option<u32>) -> api::Agent {
        api::Agent {
            pid,
            cwd: format!("/w/lune/agent-{pid}"),
            status: Some(api::agent::Status::KnownStatus(status.into())),
            status_since: Some(Timestamp { seconds: since, nanos: 0 }),
            terminal: terminal.map(key),
            waiting_for: (status == api::Status::Waiting).then_some(
                api::agent::WaitingFor::KnownWaitingFor(api::WaitingFor::PermissionPrompt.into()),
            ),
            ..api::Agent::default()
        }
    }

    fn kept(projects: Vec<api::Project>) -> api::Projects {
        let kept = projects::Kept { projects, version: None };
        api::Projects { held: Some(projects::Held::Kept(kept)) }
    }

    fn clone_of(worktrees: Vec<api::Worktree>) -> api::Project {
        let clone = project::Clone { worktrees, listing: None };
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
                api::Agent { status_since: None, ..agent(5, api::Status::Waiting, 0, None) },
            ],
            ..api::Host::default()
        };
        let model = Model::of(&host);
        let names: Vec<&str> = model.inbox.iter().map(|agent| agent.name.as_str()).collect();
        assert_eq!(names, ["agent-4", "agent-2", "agent-5", "agent-1", "agent-3"]);
        assert_eq!(model.inbox[0].status, Status::Waiting { why: Why::Permission });
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
        assert_eq!(agent.terminal.map(|it| it.id), Some(8));
        let Held::Kept { projects } = &model.here else {
            panic!("the projects are kept: {:?}", model.here);
        };
        let Holds::Clone { worktrees } = &projects[0].holds else {
            panic!("the project is a clone: {:?}", projects[0].holds);
        };
        assert_eq!(worktrees[1].views[0].terminal, agent.terminal);
    }

    #[test]
    fn a_worktree_s_branch_is_the_one_its_folder_has_checked_out() {
        let branch = api::Branch { name: "issue-723".to_owned(), ..api::Branch::default() };
        let head = Some(api::checkout::Head::Branch(branch));
        let checkout = api::Checkout { head, ..api::Checkout::default() };
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
        assert_eq!(projects[0].holds.worktrees()[0].branch.as_deref(), Some("issue-723"));
    }

    #[test]
    fn each_machine_watched_is_a_machine_but_this_one_named_again() {
        let far = remote::Host {
            agents: vec![agent(7, api::Status::Waiting, 3, None)],
            ..remote::Host::default()
        };
        let unreached = remote::Unreached { why: "ssh ended".to_owned(), ..remote::Unreached::default() };
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
        let far: Vec<(&str, &Reached)> =
            model.far.iter().map(|it| (it.on.as_str(), &it.reached)).collect();
        let unreached = Reached::Unreached { why: "ssh ended".to_owned(), since: None };
        assert_eq!(far, [("far", &Reached::Reached), ("vps", &unreached), ("mac", &Reached::Reaching)]);
        assert_eq!(model.inbox[0].on.as_deref(), Some("far"));
        assert_eq!(model.here, Held::Untold);
    }
}
