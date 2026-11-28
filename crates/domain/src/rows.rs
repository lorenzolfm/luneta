use std::collections::{BTreeMap, BTreeSet};

use lune_protocol::said::Said;
use lune_protocol::shell::Doing;
use lune_protocol::v1::{self as api, terminal};

use crate::flows::{
    BADGE_BYTES_MAX, FlowBadgeKind, FlowControl, FlowMode, NAME_BYTES_MAX, REASON_BYTES_MAX,
    SESSION_ID_BYTES_MAX, STAGES_MAX,
};
use crate::mux::TerminalKey;
use crate::projects::{Arranged, Dir, ProjectId, ProjectName, Seen, Subfolder, Used, ViewId};
use crate::projects::{CLONE_WORKTREES_MAX, VIEWS_MAX, WORKTREES_MAX, WorktreeId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowRow {
    pub flow_name: String,
    pub mode: FlowMode,
    pub step_number: u32,
    pub step_total: u32,
    pub stage_label: String,
    pub badge: String,
    pub badge_kind: FlowBadgeKind,
    pub pause_reason: Option<String>,
    pub detail: Option<String>,
    pub active_view: Option<ViewId>,
    pub resume_session: Option<String>,
    pub controls: Vec<FlowControl>,
}

impl FlowRow {
    pub fn of_api(said: &api::FlowSummary) -> Result<FlowRow, String> {
        if said.flow_name.trim().is_empty() {
            return Err("flow has an empty name".to_owned());
        }
        if said.stage_label.trim().is_empty() {
            return Err("flow has an empty stage label".to_owned());
        }
        if said.badge.trim().is_empty() {
            return Err("flow has an empty badge".to_owned());
        }
        let bounded = [
            ("name", Some(&said.flow_name), NAME_BYTES_MAX),
            ("stage label", Some(&said.stage_label), NAME_BYTES_MAX),
            ("badge", Some(&said.badge), BADGE_BYTES_MAX),
            ("pause reason", said.pause_reason.as_ref(), REASON_BYTES_MAX),
            ("detail", said.detail.as_ref(), REASON_BYTES_MAX),
            (
                "resume session",
                said.resume_session.as_ref(),
                SESSION_ID_BYTES_MAX,
            ),
        ];
        for (field, text, bytes_max) in bounded {
            if let Some(text) = text
                && text.len() > bytes_max
            {
                return Err(format!("flow {field} exceeds {bytes_max} bytes"));
            }
        }
        let step_total = usize::try_from(said.step_total)
            .map_err(|_| format!("flow step_total {} out of range", said.step_total))?;
        if step_total == 0 || step_total > STAGES_MAX {
            return Err(format!(
                "flow step_total {} must be in 1..={STAGES_MAX}",
                said.step_total
            ));
        }
        if said.step_number == 0 || said.step_number > said.step_total {
            return Err(format!(
                "flow step_number {} must be in 1..={}",
                said.step_number, said.step_total
            ));
        }
        let mode = FlowMode::of_api(said.mode)?;
        let badge_kind = match api::FlowBadgeKind::try_from(said.badge_kind) {
            Ok(api::FlowBadgeKind::Running) => FlowBadgeKind::Running,
            Ok(api::FlowBadgeKind::WaitingInteractive) => FlowBadgeKind::WaitingInteractive,
            Ok(api::FlowBadgeKind::Paused) => FlowBadgeKind::Paused,
            Ok(api::FlowBadgeKind::Merged) => FlowBadgeKind::Merged,
            Ok(api::FlowBadgeKind::ReadyToMerge) => FlowBadgeKind::ReadyToMerge,
            Ok(api::FlowBadgeKind::Unspecified) | Err(_) => {
                return Err(format!("flow has unknown badge kind {}", said.badge_kind));
            }
        };
        if said.controls.len() > FlowControl::ALL.len() {
            return Err(format!(
                "flow lists {} controls, exceeding {}",
                said.controls.len(),
                FlowControl::ALL.len()
            ));
        }
        let mut controls = Vec::with_capacity(said.controls.len());
        for &raw in &said.controls {
            let control = FlowControl::of_api(raw)?;
            if controls.contains(&control) {
                return Err(format!("flow lists {} twice", control.label()));
            }
            controls.push(control);
        }
        if said.pause_reason.is_some() != (badge_kind == FlowBadgeKind::Paused) {
            return Err("flow has a pause reason exactly when it is paused".to_owned());
        }
        let active_view = match said.active_view {
            None => None,
            Some(raw) => {
                let id = ViewId::of(raw).ok_or_else(|| "flow active view has id 0".to_owned())?;
                Some(id)
            }
        };
        let row = FlowRow {
            flow_name: said.flow_name.clone(),
            mode,
            step_number: said.step_number,
            step_total: said.step_total,
            stage_label: said.stage_label.clone(),
            badge: said.badge.clone(),
            badge_kind,
            pause_reason: said.pause_reason.clone(),
            detail: said.detail.clone(),
            active_view,
            resume_session: said.resume_session.clone(),
            controls,
        };
        assert!(row.step_number >= 1);
        assert!(row.step_number <= row.step_total);
        assert!(row.controls.len() <= FlowControl::ALL.len());
        Ok(row)
    }

    #[must_use]
    pub fn has_control(&self, control: FlowControl) -> bool {
        self.controls.contains(&control)
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(
            self.badge_kind,
            FlowBadgeKind::Merged | FlowBadgeKind::ReadyToMerge
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub id: WorktreeId,
    pub name: String,
    pub folder: Dir,
    pub tab: u32,
    pub terminals: Vec<Terminal>,
    pub used: Option<Used>,
    pub seen: Seen,
    pub arranged: Option<Arranged>,
    pub flow: Option<Box<FlowRow>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Titled {
    pub topic: Option<Said>,
}

impl Titled {
    pub fn of_api(titled: &api::Titled) -> Result<Self, String> {
        let topic = match &titled.topic {
            None => None,
            Some(topic) => {
                let topic = Said::new(topic.clone()).ok_or("titled: an empty topic")?;
                Some(topic)
            }
        };
        Ok(Titled { topic })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Running {
        key: TerminalKey,
        command: Option<Said>,
        titled: Option<Titled>,
        told: Told,
    },
    Ended {
        key: TerminalKey,
        titled: Option<Titled>,
    },
    Unlisted(TerminalKey),
    Lost,
    Looking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub id: ViewId,
    pub cwd: Dir,
    pub found: Found,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Told {
    Unhooked,
    Unread,
    Doing(Doing),
}

impl Told {
    pub fn of(shell: Option<&api::Shell>) -> Told {
        match shell.map(Doing::of_api) {
            None => Told::Unhooked,
            Some(Err(_)) => Told::Unread,
            Some(Ok(doing)) => Told::Doing(doing),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holds {
    Folder(Worktree),
    Clone {
        worktrees: Vec<Worktree>,
        unopened: Unopened,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unopened {
    Listed(Vec<Dir>),
    Unlisted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
    pub dir: Dir,
    pub holds: Holds,
}

impl Terminal {
    #[must_use]
    pub fn key(&self) -> Option<TerminalKey> {
        match &self.found {
            Found::Running { key, .. } | Found::Ended { key, .. } | Found::Unlisted(key) => {
                Some(*key)
            }
            Found::Lost | Found::Looking => None,
        }
    }

    fn title(&self) -> Option<String> {
        match &self.found {
            Found::Running {
                titled, command, ..
            } => titled
                .as_ref()
                .and_then(|titled| titled.topic.as_ref())
                .or(command.as_ref())
                .map(|said| said.as_str().to_owned()),
            Found::Ended { titled, .. } => titled
                .as_ref()
                .and_then(|titled| titled.topic.as_ref())
                .map(|s| s.as_str().to_owned()),
            Found::Unlisted(_) | Found::Lost | Found::Looking => None,
        }
    }
}

impl Worktree {
    #[must_use]
    pub fn titles(&self) -> BTreeMap<ViewId, String> {
        let titled = self.terminals.iter();
        titled.filter_map(|it| Some((it.id, it.title()?))).collect()
    }
}

impl Project {
    #[must_use]
    pub fn worktrees(&self) -> &[Worktree] {
        match &self.holds {
            Holds::Folder(own) => std::slice::from_ref(own),
            Holds::Clone { worktrees, .. } => worktrees,
        }
    }

    pub fn uses(&self) -> impl Iterator<Item = (WorktreeId, Option<Used>)> + '_ {
        self.worktrees().iter().map(|it| (it.id, it.used))
    }

    #[must_use]
    pub fn terminal_key(&self, view: ViewId) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        held.find(|it| it.id == view)?.key()
    }

    #[must_use]
    pub fn first_keyed(&self) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        held.find_map(Terminal::key)
    }

    #[must_use]
    pub fn view_of(&self, terminal: TerminalKey) -> Option<(WorktreeId, ViewId)> {
        self.worktrees().iter().find_map(|worktree| {
            let held = worktree
                .terminals
                .iter()
                .find(|it| it.key() == Some(terminal))?;
            Some((worktree.id, held.id))
        })
    }
}

pub fn listed(kept: &[api::Project]) -> Result<Vec<Project>, String> {
    let projects = kept
        .iter()
        .map(project)
        .collect::<Result<Vec<Project>, String>>()?;
    let worktrees: Vec<&Worktree> = projects.iter().flat_map(Project::worktrees).collect();
    let count = worktrees.len();
    if count > WORKTREES_MAX {
        return Err(format!(
            "they hold {count} worktrees, over the {WORKTREES_MAX} kept"
        ));
    }
    let views: usize = worktrees.iter().map(|it| it.terminals.len()).sum();
    if views > VIEWS_MAX {
        return Err(format!(
            "they hold {views} views, over the {VIEWS_MAX} kept"
        ));
    }
    let mut keyed = BTreeSet::new();
    for key in worktrees
        .iter()
        .flat_map(|it| &it.terminals)
        .filter_map(Terminal::key)
    {
        if !keyed.insert(key) {
            return Err(format!("two views hold terminal {}", key.id));
        }
    }
    assert!(keyed.len() <= views, "each view holds at most one terminal");
    Ok(projects)
}

fn command_of(command: Option<&String>) -> Result<Option<Said>, String> {
    match command {
        None => Ok(None),
        Some(command) => Said::new(command.clone())
            .map(Some)
            .ok_or_else(|| "an empty command".to_owned()),
    }
}

fn found_of(terminal: &api::Terminal) -> Result<Found, String> {
    if let Some(state) = &terminal.state {
        return match state {
            terminal::State::Running(running) => {
                let key = running.key.as_ref().ok_or("running terminal has no key")?;
                let key = TerminalKey::of(key)?;
                let command = command_of(running.command.as_ref())?;
                let titled = running.titled.as_ref().map(Titled::of_api).transpose()?;
                let told = Told::of(running.shell.as_ref());
                Ok(Found::Running {
                    key,
                    command,
                    titled,
                    told,
                })
            }
            terminal::State::Ended(ended) => {
                let key = ended.key.as_ref().ok_or("ended terminal has no key")?;
                let key = TerminalKey::of(key)?;
                let titled = ended.titled.as_ref().map(Titled::of_api).transpose()?;
                Ok(Found::Ended { key, titled })
            }
            terminal::State::Unlisted(key) => Ok(Found::Unlisted(TerminalKey::of(key)?)),
            terminal::State::StateLost(()) => Ok(Found::Lost),
            terminal::State::StateLooking(()) => Ok(Found::Looking),
        };
    }
    let reject_unfound_fields = |what: &str| {
        if terminal.held
            || terminal.command.is_some()
            || terminal.titled.is_some()
            || terminal.shell.is_some()
        {
            Err(format!("a {what} terminal carries live fields"))
        } else {
            Ok(())
        }
    };
    match &terminal.found {
        Some(terminal::Found::Key(key)) => {
            let key = TerminalKey::of(key)?;
            let titled = terminal.titled.as_ref().map(Titled::of_api).transpose()?;
            if terminal.held {
                if terminal.command.is_some() || terminal.shell.is_some() {
                    return Err("an ended terminal carries a command or shell".to_owned());
                }
                Ok(Found::Ended { key, titled })
            } else {
                let command = command_of(terminal.command.as_ref())?;
                let told = Told::of(terminal.shell.as_ref());
                Ok(Found::Running {
                    key,
                    command,
                    titled,
                    told,
                })
            }
        }
        Some(terminal::Found::Lost(())) => {
            reject_unfound_fields("lost")?;
            Ok(Found::Lost)
        }
        Some(terminal::Found::Looking(())) => {
            reject_unfound_fields("looking")?;
            Ok(Found::Looking)
        }
        None => Err("says not whether it is found".to_owned()),
    }
}

fn worktree(worktree: &api::Worktree, tab: u32, of: &str) -> Result<Worktree, String> {
    let named = &worktree.name;
    let id = WorktreeId::of(worktree.id);
    let id = id.ok_or_else(|| format!("worktree {named} of project {of} has id 0"))?;
    let mut terminals = Vec::with_capacity(worktree.terminals.len());
    for terminal in &worktree.terminals {
        let held = ViewId::of(terminal.id);
        let id = held.ok_or_else(|| format!("a terminal of worktree {named} has id 0"))?;
        let found = found_of(terminal).map_err(|why| format!("terminal {id}: {why}"))?;
        let cwd = Dir::parse(&terminal.folder);
        let cwd = cwd.map_err(|why| format!("terminal {id}'s folder: {why}"))?;
        terminals.push(Terminal { id, cwd, found });
    }
    assert_eq!(
        terminals.len(),
        worktree.terminals.len(),
        "every terminal said is read"
    );
    let folder = Dir::parse(&worktree.folder);
    let folder = folder.map_err(|why| format!("worktree {named}'s folder: {why}"))?;
    let used = worktree.used_at.map(Used::of_api).transpose();
    let used = used.map_err(|why| format!("worktree {named}'s last use: {why}"))?;
    let seen = worktree.seen.and_then(Seen::of_api);
    let seen = seen.ok_or_else(|| format!("worktree {named} says no stage lune knows"))?;
    let arranged = worktree.arranged.map(Arranged::said).transpose();
    let arranged = arranged.map_err(|why| format!("worktree {named}'s arrangement: {why}"))?;
    let flow = worktree
        .flow
        .as_ref()
        .map(FlowRow::of_api)
        .transpose()
        .map_err(|why| format!("worktree {named}'s flow: {why}"))?
        .map(Box::new);
    let name = named.clone();
    Ok(Worktree {
        id,
        name,
        folder,
        tab,
        terminals,
        used,
        seen,
        arranged,
        flow,
    })
}

fn unopened(clone: &api::project::Clone, dir: &Dir, open: &[Worktree]) -> Result<Unopened, String> {
    use api::project::clone::Listing;
    let listed = match &clone.listing {
        Some(Listing::Unopened(listed)) => &listed.subfolders,
        Some(Listing::Unlisted(why)) => return Ok(Unopened::Unlisted(why.clone())),
        None => return Ok(Unopened::Unlisted("luned says nothing of them".to_owned())),
    };
    let (count, max) = (listed.len(), CLONE_WORKTREES_MAX);
    if count > max {
        return Err(format!(
            "it lists {count} worktrees not open, over the {max} a clone holds"
        ));
    }
    let open: BTreeSet<&Dir> = open.iter().map(|it| &it.folder).collect();
    let mut unopened = Vec::new();
    for said in listed {
        let subfolder = Subfolder::parse(said).map_err(|why| format!("{said}: {why}"))?;
        let folder = dir.at(&subfolder);
        if open.contains(&folder) {
            return Err(format!(
                "{said} is said not open, and a worktree is open there"
            ));
        }
        unopened.push(folder);
    }
    assert_eq!(
        unopened.len(),
        count,
        "each subfolder said is a folder not open"
    );
    Ok(Unopened::Listed(unopened))
}

fn project(project: &api::Project) -> Result<Project, String> {
    let named = &project.name;
    let id = ProjectId::of(project.id).ok_or_else(|| format!("project {named} has id 0"))?;
    let dir = Dir::parse(&project.dir).map_err(|why| format!("project {named}'s folder: {why}"))?;
    let holds = match project.holds.as_ref() {
        Some(api::project::Holds::Folder(own)) => Holds::Folder(worktree(own, 0, named)?),
        Some(api::project::Holds::Clone(clone)) => {
            let rows = clone
                .worktrees
                .iter()
                .zip(0..)
                .map(|(it, tab)| worktree(it, tab, named));
            let worktrees = rows.collect::<Result<Vec<_>, String>>()?;
            let unopened = unopened(clone, &dir, &worktrees);
            let unopened = unopened.map_err(|why| format!("project {named}'s clone: {why}"))?;
            Holds::Clone {
                worktrees,
                unopened,
            }
        }
        None => return Err(format!("project {named} says not what it holds")),
    };
    let name = ProjectName::parse(&project.name)?;
    Ok(Project {
        id,
        name,
        dir,
        holds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey {
            id,
            made_at: Some(prost_types::Timestamp::default()),
        }
    }

    fn keyed(id: u32) -> TerminalKey {
        TerminalKey::of(&key(id)).unwrap()
    }

    fn terminal(id: u64, found: terminal::Found, topic: Option<&str>) -> api::Terminal {
        let keyed = matches!(found, terminal::Found::Key(_));
        let titled = topic.map(|topic| api::Titled {
            topic: Some(topic.to_owned()),
        });
        api::Terminal {
            id,
            found: Some(found),
            folder: "/w/a".to_owned(),
            command: keyed.then(|| "sleep 600".to_owned()),
            titled,
            ..api::Terminal::default()
        }
    }

    fn row(id: u64, name: &str) -> api::Worktree {
        api::Worktree {
            id,
            name: name.to_owned(),
            folder: format!("/w/{name}"),
            seen: Some(api::worktree::Seen::Unread(())),
            ..api::Worktree::default()
        }
    }

    fn kept() -> api::Project {
        let first = api::Worktree {
            terminals: vec![
                terminal(21, terminal::Found::Key(key(4)), Some("Fixing")),
                terminal(22, terminal::Found::Lost(()), None),
            ],
            ..row(11, "main")
        };
        let second = api::Worktree {
            terminals: vec![terminal(23, terminal::Found::Key(key(5)), None)],
            ..row(12, "docs")
        };
        api::Project {
            id: 7,
            name: "bitwarden backup".to_owned(),
            dir: "/w".to_owned(),
            holds: Some(api::project::Holds::Clone(api::project::Clone {
                worktrees: vec![first, second],
                listing: None,
            })),
            ..api::Project::default()
        }
    }

    fn read(kept: &api::Project) -> Result<Project, String> {
        let [project] = listed(std::slice::from_ref(kept))?.try_into().unwrap();
        Ok(project)
    }

    fn rows(project: &mut api::Project) -> &mut Vec<api::Worktree> {
        let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
            panic!("the project is a clone");
        };
        &mut clone.worktrees
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    #[test]
    fn a_view_of_a_project_row_leads_back_to_its_worktree_and_terminal() {
        let project = read(&kept()).unwrap();
        let terminal = ViewId::of(21).unwrap();
        assert_eq!(project.view_of(keyed(4)), Some((worktree(11), terminal)));
        assert_eq!(project.terminal_key(terminal), Some(keyed(4)));
        assert_eq!(
            project.terminal_key(ViewId::of(22).unwrap()),
            None,
            "a lost one is no view"
        );
        assert_eq!(project.first_keyed(), Some(keyed(4)));
    }

    #[test]
    fn a_terminal_made_again_under_an_id_in_use_leads_to_its_own_view() {
        let mut kept = kept();
        let made_at = Some(prost_types::Timestamp {
            seconds: 1,
            nanos: 0,
        });
        let remade = api::TerminalKey { id: 5, made_at };
        rows(&mut kept)[1]
            .terminals
            .push(terminal(25, terminal::Found::Key(remade), None));
        let project = read(&kept).unwrap();
        let view = |id| Some((worktree(12), ViewId::of(id).unwrap()));
        assert_eq!(project.view_of(keyed(5)), view(23));
        assert_eq!(project.view_of(TerminalKey::of(&remade).unwrap()), view(25));
    }

    #[test]
    fn projects_that_list_one_terminal_in_two_views_are_refused() {
        let mut kept = kept();
        rows(&mut kept)[1]
            .terminals
            .push(terminal(24, terminal::Found::Key(key(4)), None));
        assert_eq!(read(&kept), Err("two views hold terminal 4".to_owned()));
    }

    #[test]
    fn a_project_worktree_or_terminal_of_id_0_or_a_key_unread_is_refused() {
        let zeroed: [fn(&mut api::Project); 3] = [
            |project| project.id = 0,
            |project| rows(project)[1].id = 0,
            |project| rows(project)[0].terminals[1].id = 0,
        ];
        for zero in zeroed {
            let mut kept = kept();
            zero(&mut kept);
            let refused = read(&kept).unwrap_err();
            assert!(refused.contains("has id 0"), "{refused}");
        }
        let mut kept = kept();
        let unmade = api::TerminalKey {
            id: 4,
            made_at: None,
        };
        rows(&mut kept)[0].terminals[0].found = Some(terminal::Found::Key(unmade));
        let refused = read(&kept).unwrap_err();
        assert!(refused.starts_with("terminal "), "{refused}");
    }

    #[test]
    fn projects_that_list_more_worktrees_than_luned_keeps_are_refused() {
        let listing = |count: usize| {
            let mut kept = kept();
            let each = |id: usize| row(u64::try_from(id).unwrap(), &format!("t{id}"));
            *rows(&mut kept) = (1..=count).map(each).collect();
            read(&kept)
        };
        assert_eq!(
            listing(WORKTREES_MAX).unwrap().worktrees().len(),
            WORKTREES_MAX
        );
        let refused = listing(WORKTREES_MAX + 1).unwrap_err();
        assert!(
            refused.contains(&format!("over the {WORKTREES_MAX} kept")),
            "{refused}"
        );
    }

    #[test]
    fn projects_that_list_more_views_than_luned_keeps_are_refused() {
        let listing = |count: u64| {
            let mut kept = kept();
            let lost = |id| terminal(id, terminal::Found::Lost(()), None);
            rows(&mut kept)[0].terminals = (1..=count).map(lost).collect();
            read(&kept)
        };
        let most = u64::try_from(VIEWS_MAX).unwrap() - 1;
        assert!(
            listing(most).is_ok(),
            "{VIEWS_MAX} views, with the second worktree's one"
        );
        let over = format!(
            "they hold {} views, over the {VIEWS_MAX} kept",
            VIEWS_MAX + 1
        );
        assert_eq!(listing(most + 1), Err(over));
    }

    #[test]
    fn a_terminal_reads_its_state_sum_type_and_refuses_illegal_legacy_shapes() {
        let mut typed = kept();
        rows(&mut typed)[0].terminals = vec![
            api::Terminal {
                id: 30,
                folder: "/w/a".to_owned(),
                state: Some(terminal::State::Running(terminal::Running {
                    key: Some(key(8)),
                    command: Some("claude".to_owned()),
                    titled: Some(api::Titled {
                        topic: Some("Fixing".to_owned()),
                    }),
                    shell: Some(Doing::Ready.to_api()),
                })),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 31,
                folder: "/w/a".to_owned(),
                state: Some(terminal::State::Ended(terminal::Ended {
                    key: Some(key(9)),
                    titled: Some(api::Titled {
                        topic: Some("Done".to_owned()),
                    }),
                })),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 32,
                folder: "/w/a".to_owned(),
                state: Some(terminal::State::Unlisted(key(10))),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 33,
                folder: "/w/a".to_owned(),
                state: Some(terminal::State::StateLost(())),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 34,
                folder: "/w/a".to_owned(),
                state: Some(terminal::State::StateLooking(())),
                ..api::Terminal::default()
            },
        ];
        let project = read(&typed).unwrap();
        let terminals = &project.worktrees()[0].terminals;
        assert!(matches!(
            &terminals[0].found,
            Found::Running {
                key,
                command: Some(cmd),
                titled: Some(Titled { topic: Some(topic) }),
                told: Told::Doing(Doing::Ready),
            } if *key == keyed(8) && cmd.as_str() == "claude" && topic.as_str() == "Fixing"
        ));
        assert!(matches!(
            &terminals[1].found,
            Found::Ended { key, titled: Some(Titled { topic: Some(topic) }) }
                if *key == keyed(9) && topic.as_str() == "Done"
        ));
        assert_eq!(terminals[2].found, Found::Unlisted(keyed(10)));
        assert_eq!(terminals[3].found, Found::Lost);
        assert_eq!(terminals[4].found, Found::Looking);

        let refused = [
            api::Terminal {
                id: 40,
                folder: "/w/a".to_owned(),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 40,
                found: Some(terminal::Found::Lost(())),
                folder: "/w/a".to_owned(),
                command: Some("sleep 600".to_owned()),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 40,
                found: Some(terminal::Found::Key(key(11))),
                folder: "/w/a".to_owned(),
                command: Some("sleep 600".to_owned()),
                held: true,
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 40,
                found: Some(terminal::Found::Key(key(11))),
                folder: "/w/a".to_owned(),
                command: Some(String::new()),
                ..api::Terminal::default()
            },
            api::Terminal {
                id: 40,
                found: Some(terminal::Found::Key(key(11))),
                folder: "/w/a".to_owned(),
                titled: Some(api::Titled {
                    topic: Some(String::new()),
                }),
                ..api::Terminal::default()
            },
        ];
        for bad in refused {
            let mut project = kept();
            rows(&mut project)[0].terminals = vec![bad.clone()];
            assert!(read(&project).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_project_whose_folder_is_no_absolute_path_is_refused() {
        let mut unrooted = kept();
        unrooted.dir = "w".to_owned();
        let refused = read(&unrooted).unwrap_err();
        assert_eq!(
            refused,
            "project bitwarden backup's folder: \"w\" is no absolute path"
        );
        assert_eq!(read(&kept()).unwrap().dir.as_str(), "/w");
    }

    #[test]
    fn a_project_holds_its_folder_or_its_clone_s_worktrees_and_says_which() {
        let mut folder = kept();
        let own = rows(&mut folder).remove(0);
        folder.holds = Some(api::project::Holds::Folder(own));
        let project = read(&folder).unwrap();
        let Holds::Folder(own) = &project.holds else {
            panic!("a folder holds its own worktree: {:?}", project.holds);
        };
        assert_eq!((own.id, project.worktrees().len()), (worktree(11), 1));
        let clone = read(&kept()).unwrap();
        let ids: Vec<WorktreeId> = clone.worktrees().iter().map(|it| it.id).collect();
        assert_eq!(ids, [worktree(11), worktree(12)]);
        folder.holds = None;
        assert_eq!(
            read(&folder),
            Err("project bitwarden backup says not what it holds".to_owned())
        );
    }

    #[test]
    fn a_worktree_said_with_no_stage_folder_or_arrangement_lune_reads_is_refused() {
        type Broke = fn(&mut api::Worktree);
        let unknown = "worktree docs says no stage lune knows";
        let broken: [(Broke, &str); 4] = [
            (|it| it.seen = None, unknown),
            (|it| it.seen = Some(api::worktree::Seen::Stage(99)), unknown),
            (
                |it| it.folder = "w/docs".to_owned(),
                "worktree docs's folder: \"w/docs\" is no absolute path",
            ),
            (
                |it| it.arranged = Some(99),
                "worktree docs's arrangement: it says neither tabs nor panes",
            ),
        ];
        for (broke, why) in broken {
            let mut kept = kept();
            broke(&mut rows(&mut kept)[1]);
            assert_eq!(read(&kept), Err(why.to_owned()));
        }
    }

    #[test]
    fn a_terminal_whose_folder_is_no_folder_luned_keeps_is_refused() {
        let unnamed = "a path holds a part that is no name: empty, `.` or `..`";
        let unkept = [
            ("", "\"\" is no absolute path".to_owned()),
            ("w/a", "\"w/a\" is no absolute path".to_owned()),
            ("/w/../a", format!("\"/w/../a\": {unnamed}")),
        ];
        for (folder, why) in unkept {
            let mut kept = kept();
            rows(&mut kept)[0].terminals[1].folder = folder.to_owned();
            let refused = format!("terminal 0000000000000016's folder: {why}");
            assert_eq!(read(&kept), Err(refused), "{folder:?}");
        }
    }

    #[test]
    fn a_clone_lists_the_worktrees_not_open_under_its_folder_or_is_refused() {
        use api::project::clone::Listing;
        let listing = |subfolders: Vec<String>| {
            let mut kept = kept();
            let Some(api::project::Holds::Clone(clone)) = &mut kept.holds else {
                panic!("the project is a clone");
            };
            clone.listing = Some(Listing::Unopened(api::project::Unopened { subfolders }));
            read(&kept).map(|it| it.holds)
        };
        let unopened = |holds: Holds| {
            let Holds::Clone { unopened, .. } = holds else {
                panic!("a clone: {holds:?}");
            };
            unopened
        };
        let spike = listing(vec!["spike".to_owned()]).map(unopened);
        assert_eq!(
            spike,
            Ok(Unopened::Listed(vec![Dir::parse("/w/spike").unwrap()]))
        );
        let unsaid = unopened(read(&kept()).unwrap().holds);
        assert_eq!(
            unsaid,
            Unopened::Unlisted("luned says nothing of them".to_owned())
        );
        let clone = "project bitwarden backup's clone";
        let open = format!("{clone}: docs is said not open, and a worktree is open there");
        assert_eq!(listing(vec!["docs".to_owned()]), Err(open));
        let root = format!("{clone}: : a worktree is below its clone's root, and never the root");
        assert_eq!(listing(vec![String::new()]), Err(root));
        let max = CLONE_WORKTREES_MAX;
        let many = |count: usize| (0..count).map(|it| format!("s{it}")).collect();
        assert!(listing(many(max)).is_ok(), "a clone lists {max} not open");
        let over = max + 1;
        let over = format!("it lists {over} worktrees not open, over the {max} a clone holds");
        let over = format!("{clone}: {over}");
        assert_eq!(listing(many(max + 1)), Err(over));
    }

    #[test]
    fn flow_row_accepts_valid_summary_and_rejects_illegal_wire_values() {
        let valid = || api::FlowSummary {
            worktree: 11,
            flow_name: "default".to_owned(),
            mode: api::FlowMode::Unattended.into(),
            harness: api::Harness::ClaudeCode.into(),
            step_number: 2,
            step_total: 4,
            stage_label: "tigerstyle".to_owned(),
            badge: "2/4 tigerstyle".to_owned(),
            badge_kind: api::FlowBadgeKind::Running.into(),
            pause_reason: None,
            detail: None,
            active_view: Some(21),
            resume_session: None,
            controls: vec![
                api::FlowControl::Pause.into(),
                api::FlowControl::Abort.into(),
            ],
        };
        let row = FlowRow::of_api(&valid()).unwrap();
        assert_eq!(row.step_number, 2);
        assert_eq!(row.step_total, 4);
        assert_eq!(row.active_view, ViewId::of(21));

        let mut bad_mode = valid();
        bad_mode.mode = api::FlowMode::Unspecified.into();
        assert!(FlowRow::of_api(&bad_mode).is_err());

        let mut bad_badge = valid();
        bad_badge.badge_kind = api::FlowBadgeKind::Unspecified.into();
        assert!(FlowRow::of_api(&bad_badge).is_err());

        let mut zero_view = valid();
        zero_view.active_view = Some(0);
        assert!(FlowRow::of_api(&zero_view).is_err());

        let mut bad_step = valid();
        bad_step.step_number = 5;
        assert!(FlowRow::of_api(&bad_step).is_err());

        let mut too_many_controls = valid();
        too_many_controls.controls =
            vec![api::FlowControl::Pause.into(); FlowControl::ALL.len() + 1];
        assert!(FlowRow::of_api(&too_many_controls).is_err());

        let mut twice = valid();
        twice.controls = vec![api::FlowControl::Pause.into(); 2];
        assert!(FlowRow::of_api(&twice).is_err());

        let mut long_name = valid();
        long_name.flow_name = "n".repeat(NAME_BYTES_MAX + 1);
        assert!(FlowRow::of_api(&long_name).is_err());

        let mut running_with_reason = valid();
        running_with_reason.pause_reason = Some("paused".to_owned());
        assert!(FlowRow::of_api(&running_with_reason).is_err());

        let mut paused_without_reason = valid();
        paused_without_reason.badge_kind = api::FlowBadgeKind::Paused.into();
        assert!(FlowRow::of_api(&paused_without_reason).is_err());
    }
}
