//! Deleting what a menu is for: its pane, or its session. What goes, which
//! agents end with it, and asking zellij for it.
//!
//! zellij closes a tab with its last tiled pane, and the floating panes go
//! with it, but in a session no client is on it keeps the tab, listed with
//! no panes: closing the last pane that holds a tab open closes the tab
//! instead, by its id. A session's only tab takes the session with it, and
//! the menu deletes the session for it.

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use crate::agents::{AgentSet, Seat, Status};
use crate::elapsed::Age;
use crate::fetch::Fetch;
use crate::panes;
use crate::tree::{Leaf, TabId};

/// What a key of the menu's Delete group deletes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Pane,
    Session,
}

impl Scope {
    /// What the menu says the key does.
    pub fn does(self) -> &'static str {
        match self {
            Scope::Pane => "close the pane",
            Scope::Session => "kill the session",
        }
    }
}

/// Whether `pane` keeps its tab open: a tiled pane you can go to.
pub fn holds_tab(pane: &PaneInfo) -> bool {
    pane.is_selectable && !pane.is_suppressed && !pane.is_floating
}

/// Whether nothing among a tab's `panes` but the terminal `pane` keeps the tab
/// open: closing the pane closes the tab, its floating panes and all. A
/// terminal another stands in place of keeps it open too, since zellij brings
/// it back once the other closes.
pub fn alone(panes: &[PaneInfo], pane: u32) -> bool {
    let suspended = |other: &PaneInfo| !other.is_plugin && other.is_suppressed;
    !panes.iter().any(|other| {
        (other.is_plugin || other.id != pane) && (holds_tab(other) || suspended(other))
    })
}

/// What closing a pane closes with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closes {
    /// The pane alone: something else holds its tab open.
    Pane,
    /// Its tab, as nothing else holds the tab open.
    Tab,
    /// Its session, as nothing else holds the tab open, and the session has
    /// no other tab.
    Session,
}

/// What closing the terminal `pane` closes, among its tab's `panes`, in a
/// session of `tabs` tabs.
pub fn closes(panes: &[PaneInfo], pane: u32, tabs: usize) -> Closes {
    match (alone(panes, pane), tabs) {
        (false, _) => Closes::Pane,
        (true, 0 | 1) => Closes::Session,
        (true, _) => Closes::Tab,
    }
}

/// The live session the picker lists first, of `others`: the newest. Where
/// the client goes before the session it is in is killed.
pub fn refuge<'a>(others: impl IntoIterator<Item = (&'a str, Age)>) -> Option<String> {
    others
        .into_iter()
        .min_by(|(a, a_age), (b, b_age)| a_age.cmp(b_age).then_with(|| a.cmp(b)))
        .map(|(name, _)| name.to_owned())
}

/// An agent of a session: where it sits, and what it is doing.
#[derive(Debug)]
pub struct Agent {
    pub sits: Sits,
    pub status: Status,
}

/// Where an agent of a session sits.
#[derive(Debug)]
pub enum Sits {
    /// In pane `id`, in the tab of that id once its session has said.
    Pane { id: u32, tab: Option<TabId> },
    /// In a pane luneta cannot find: its environment names the session, and
    /// any of the session's panes may hold it.
    Unplaced,
}

/// The agents of `session` among every agent `agents` counts, those its rows
/// leave out too, and among `unread`, those of the session files lunetad
/// cannot read, by the session and the pane each runs in: what each of those
/// is doing, no one can tell. Each with the tab of its pane among the
/// session's `leaves`.
pub fn agents_of(
    agents: &AgentSet,
    unread: &[(String, u32)],
    session: &str,
    leaves: &[Leaf],
) -> Fetch<Vec<Agent>> {
    match &agents.status {
        Fetch::Ready(_) => {},
        Fetch::Waiting => return Fetch::Waiting,
        Fetch::Failed(reason) => return Fetch::Failed(reason.clone()),
    }
    let tab = |pane: u32| leaves.iter().find(|leaf| leaf.id == pane).and_then(|leaf| leaf.tab_id);
    let of_session = agents.counted().iter().filter(|counted| counted.session == session);
    let unread = unread.iter().filter(|(at, _)| at == session).map(|(_, id)| Agent {
        sits: Sits::Pane { id: *id, tab: tab(*id) },
        status: Status::Unknown(None),
    });
    Fetch::Ready(
        of_session
            .map(|counted| Agent {
                sits: counted.pane.map_or(Sits::Unplaced, |id| Sits::Pane { id, tab: tab(id) }),
                status: counted.status.clone(),
            })
            .chain(unread)
            .collect(),
    )
}

/// What a menu is for, the pane you came from in the session the menu is in,
/// and what deleting it takes along, as far as the menu knows.
#[derive(Debug)]
pub struct Subject {
    /// The session the menu is in.
    pub session: String,
    pub pane: Pane,
    /// The agents of its session, once the daemon has said.
    pub agents: Fetch<Vec<Agent>>,
    /// The session the client goes to before the session it is in is
    /// killed.
    pub refuge: Option<String>,
}

/// The pane a menu is for.
#[derive(Debug)]
pub struct Pane {
    pub id: u32,
    /// Its tab, by its id, once its session has said.
    pub tab: Option<TabId>,
    /// What closing it closes with it.
    pub closes: Closes,
}

impl Pane {
    /// Pane `id`, among its session's `leaves`. One that is not among them
    /// closes alone.
    pub fn of(id: u32, leaves: &[Leaf]) -> Self {
        let leaf = leaves.iter().find(|leaf| leaf.id == id);
        let tab = leaf.and_then(|leaf| leaf.tab_id);
        Pane { id, tab, closes: leaf.map_or(Closes::Pane, |leaf| leaf.closes) }
    }

    /// Closes the pane, of the session `seat` names, or its tab once nothing
    /// else holds the tab open, which ends the session when it is its only
    /// one.
    pub fn act(&self, seat: Seat) -> Act {
        match (self.closes, self.tab) {
            (Closes::Tab | Closes::Session, Some(tab)) => Act::Tab(seat, tab),
            (Closes::Pane, _) | (Closes::Tab | Closes::Session, None) => Act::Pane(seat, self.id),
        }
    }
}

impl Subject {
    /// Pane `id` of `session`, the session the menu is in, among the
    /// session's `leaves`.
    pub fn new(
        session: String,
        id: u32,
        leaves: &[Leaf],
        agents: Fetch<Vec<Agent>>,
        refuge: Option<String>,
    ) -> Self {
        Subject { session, pane: Pane::of(id, leaves), agents, refuge }
    }

    /// What the key that deletes `scope` does here, or why it does nothing.
    /// The pane closes, or its tab once nothing else holds the tab open. The
    /// last pane that holds the session's only tab open ends the session with
    /// it, and deletes the session: it goes once you are in another, as it
    /// would for a key that kills it.
    pub fn plan(&self, scope: Scope) -> Result<Plan, String> {
        let session = self.session.clone();
        let act = match scope {
            Scope::Pane if self.pane.closes != Closes::Session => {
                self.pane.act(Seat::Here(session))
            },
            Scope::Pane | Scope::Session => {
                let Some(to) = self.refuge.clone() else {
                    return Err(format!(
                        "menu: \"{session}\" is the only session; there is none to go to before \
                         it ends"
                    ));
                };
                Act::Leave { session, to }
            },
        };
        let ending = match &self.agents {
            Fetch::Ready(agents) => Fetch::Ready(Ending::of(agents, &act)),
            Fetch::Waiting => Fetch::Waiting,
            Fetch::Failed(reason) => Fetch::Failed(reason.clone()),
        };
        Ok(Plan { act, ending })
    }
}

/// The agents a delete would end, by what they are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ending {
    agents: usize,
    busy: usize,
    waiting: usize,
    /// Agents Claude Code said a word luneta does not know for, or nothing:
    /// they may be at work.
    unknown: usize,
    /// Agents that may be at work in a pane of the session luneta cannot
    /// find, which may be the one that closes.
    unplaced: usize,
}

impl Ending {
    /// The agents of a session that `act` ends: all of them as the session
    /// ends, and otherwise those of the pane, or of the tab, which takes in
    /// the pane's own, as both have its tab from the session's leaves. An
    /// agent at work that luneta found in no pane may be in the one that
    /// closes.
    fn of(agents: &[Agent], act: &Act) -> Self {
        let mut ending = Ending::default();
        for agent in agents {
            match (act, &agent.sits) {
                (Act::Leave { .. }, _) => ending.count(&agent.status),
                (Act::Pane(_, pane), Sits::Pane { id, .. }) if id == pane => {
                    ending.count(&agent.status);
                },
                (Act::Tab(_, tab), Sits::Pane { tab: at, .. }) if *at == Some(*tab) => {
                    ending.count(&agent.status);
                },
                (Act::Pane(..) | Act::Tab(..), Sits::Unplaced) => {
                    if !matches!(agent.status, Status::Idle | Status::Shell) {
                        ending.unplaced += 1;
                    }
                },
                (Act::Pane(..) | Act::Tab(..), Sits::Pane { .. }) => {},
            }
        }
        ending
    }

    fn count(&mut self, status: &Status) {
        match status {
            Status::Shell => return,
            Status::Busy => self.busy += 1,
            Status::Waiting => self.waiting += 1,
            Status::Unknown(_) | Status::Unreported => self.unknown += 1,
            Status::Idle => {},
        }
        self.agents += 1;
    }

    /// Whether an agent that would end may be in the middle of something:
    /// busy, waiting on you, or doing what luneta cannot tell.
    fn at_work(self) -> bool {
        self.busy + self.waiting + self.unknown + self.unplaced > 0
    }

    /// As `2 agents, 1 busy`, and `1 more at work in a pane luneta cannot
    /// find`.
    fn says(self) -> String {
        let mut said = Vec::new();
        if self.agents > 0 {
            said.push(match self.agents {
                1 => "1 agent".to_owned(),
                agents => format!("{agents} agents"),
            });
        }
        if self.busy > 0 {
            said.push(format!("{} busy", self.busy));
        }
        if self.waiting > 0 {
            said.push(format!("{} waiting", self.waiting));
        }
        if self.unknown > 0 {
            said.push(format!("{} unknown", self.unknown));
        }
        let more = match (self.agents, self.unplaced) {
            (0, 1) => "agent",
            (0, _) => "agents",
            _ => "more",
        };
        match self.unplaced {
            0 => {},
            1 => said.push(format!("1 {more} at work in a pane luneta cannot find")),
            unplaced => said.push(format!("{unplaced} {more} at work in panes luneta cannot find")),
        }
        said.join(", ")
    }
}

/// What a delete key does, and the agents it would end.
#[derive(Debug)]
pub struct Plan {
    pub act: Act,
    ending: Fetch<Ending>,
}

impl Plan {
    /// What to ask before going ahead, as `Kill session elo? 2 agents, 1
    /// busy`. Nothing when luneta knows that no agent busy or waiting on you
    /// would end: an idle agent or a shell goes without asking, and one whose
    /// status luneta cannot read asks.
    pub fn question(&self) -> Option<String> {
        let what = self.act.what();
        match &self.ending {
            Fetch::Ready(ending) if !ending.at_work() => None,
            Fetch::Ready(ending) => Some(format!("{what}? {}", ending.says())),
            Fetch::Waiting => Some(format!("{what}? luneta does not know its agents yet")),
            Fetch::Failed(reason) => {
                Some(format!("{what}? luneta cannot tell which agents it ends: {reason}"))
            },
        }
    }
}

/// What a delete key asks zellij for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Close a terminal pane of the session the seat names.
    Pane(Seat, u32),
    /// Close a tab of the session the seat names, by its id.
    Tab(Seat, TabId),
    /// Go to the session `to`, and kill the session you are in once you have
    /// left it.
    Leave { session: String, to: String },
}

/// What came of asking.
pub enum Ran {
    /// Asked of zellij: the plugin's own session does it at once, and another
    /// says how it went to the command run with the context given.
    Done,
    /// On the way to another session, for the one you leave to end once you
    /// are gone.
    Leaving(Leaving),
}

impl Act {
    /// What it closes, as a question begins.
    fn what(&self) -> String {
        match self {
            Act::Pane(..) => "Close the pane".to_owned(),
            Act::Tab(..) => "Close its tab".to_owned(),
            Act::Leave { session, .. } => format!("Kill session {session}"),
        }
    }

    /// Asks zellij for it. A pane or a tab of another session closes by a
    /// command, which answers with `context`.
    pub fn run(&self, context: BTreeMap<String, String>) -> Ran {
        match self {
            Act::Pane(Seat::Here(_), pane) => close_terminal_pane(*pane),
            Act::Tab(Seat::Here(_), tab) => close_tab_with_id(tab.id() as u64),
            Act::Pane(Seat::There(session), pane) => {
                run_command(&close_pane(session, &panes::pane_id(*pane)), context);
            },
            Act::Tab(Seat::There(session), tab) => {
                run_command(&close_tab(session, &tab.id().to_string()), context);
            },
            Act::Leave { session, to } => return Ran::Leaving(Leaving::go(session, to)),
        }
        Ran::Done
    }
}

/// Closes a pane of another session; the plugin can only close its own
/// session's.
pub fn close_pane<'a>(session: &'a str, pane: &'a str) -> [&'a str; 7] {
    ["zellij", "--session", session, "action", "close-pane", "--pane-id", pane]
}

/// Closes a tab of another session, by its id.
fn close_tab<'a>(session: &'a str, tab: &'a str) -> [&'a str; 6] {
    ["zellij", "--session", session, "action", "close-tab-by-id", tab]
}

fn kill_session(session: &str) -> [&str; 3] {
    ["zellij", "kill-session", session]
}

/// How many ticks the client may take to leave before the session it is in
/// stays after all, or to come to another session before the menu gives up
/// what it would open there.
pub const PATIENCE: u32 = 50;

/// What the command that lists a session's clients is run with, as its
/// context, for its answer to come back to the menu that leaves, or that
/// comes to another session.
pub const CLIENTS_VALUE: &str = "clients";

/// What `zellij action list-clients` heads its list with.
const CLIENTS_HEADER: &str = "CLIENT_ID";

/// Lists the clients of `session`, a client to a line under a header, its id
/// first. zellij's own answer to a plugin leaves out a client whose focus is
/// on that plugin, as the focus of the client leaving is on the menu.
pub fn clients_of(session: &str) -> [&str; 5] {
    ["zellij", "--session", session, "action", "list-clients"]
}

/// The clients a list of them names, by id, or none for what is no such list.
pub fn clients_listed(stdout: &[u8]) -> Option<Vec<ClientId>> {
    let listed = String::from_utf8_lossy(stdout);
    let mut lines = listed.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next()?.split_whitespace().next()?;
    if header != CLIENTS_HEADER {
        return None;
    }
    lines.map(|line| line.split_whitespace().next()?.parse().ok()).collect()
}

/// The client on its way to another session, and the session it leaves, to
/// kill once it has: killed first, the session would take the client with it.
pub struct Leaving {
    session: String,
    to: String,
    ticks: u32,
    /// Whether zellij has yet to answer the last ask for the clients.
    asking: bool,
}

impl Leaving {
    /// Takes the client to `to`.
    fn go(session: &str, to: &str) -> Self {
        switch_session(Some(to));
        Leaving { session: session.to_owned(), to: to.to_owned(), ticks: 0, asking: false }
    }

    /// The session the client is on its way to.
    pub fn to(&self) -> &str {
        &self.to
    }

    /// Asks zellij which clients the session has, with `context`, once it
    /// has answered the last ask, for `listed` to kill the session once the
    /// plugin's client is not among them. Says why the session stays once the
    /// client has taken too long to leave.
    pub fn tick(&mut self, context: BTreeMap<String, String>) -> Result<(), String> {
        self.ticks += 1;
        if self.ticks >= PATIENCE {
            return Err(format!(
                "menu: \"{}\" did not take you, and \"{}\" stays",
                self.to, self.session
            ));
        }
        if !self.asking {
            run_command(&clients_of(&self.session), context);
            self.asking = true;
        }
        Ok(())
    }

    /// Kills the session once zellij lists its clients without `client`, the
    /// plugin's, and says whether it did. That the client is gone is what
    /// zellij says: an answer that is no list of the clients kills nothing.
    /// A `zellij` of its own kills it: the plugin asking would wait on the
    /// session it ends.
    pub fn listed(&mut self, client: ClientId, exit_code: Option<i32>, stdout: &[u8]) -> bool {
        self.asking = false;
        let gone = exit_code == Some(0)
            && clients_listed(stdout).is_some_and(|clients| !clients.contains(&client));
        if gone {
            run_command(&kill_session(&self.session), BTreeMap::new());
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use luneta_protocol::v1;

    use super::*;
    use crate::agents::{given, Live};
    use crate::places::Places;
    use crate::sessions::{Selection, Session};

    fn pane(id: u32) -> PaneInfo {
        PaneInfo { id, is_selectable: true, ..PaneInfo::default() }
    }

    /// zellij closes a tab with its last tiled pane, floating panes and all,
    /// and a session with its only tab. A bar holds no tab open. A pane
    /// another stands in place of comes back once the other closes.
    #[test]
    fn the_last_pane_that_holds_its_tab_open_closes_the_tab() {
        let floating = PaneInfo { is_floating: true, ..pane(5) };
        let bar = PaneInfo { is_plugin: true, is_selectable: false, ..pane(3) };
        // A plugin pane of yours with a terminal's id.
        let other_plugin = PaneInfo { is_plugin: true, ..pane(4) };
        let suspended = PaneInfo { is_suppressed: true, ..pane(7) };
        for (panes, id, closes_tab) in [
            (vec![bar.clone(), pane(4)], 4, true),
            (vec![bar.clone(), pane(4), floating.clone()], 4, true),
            (vec![bar.clone(), pane(4), pane(6)], 4, false),
            (vec![bar.clone(), pane(4), floating.clone()], 5, false),
            (vec![bar.clone(), floating], 5, true),
            (vec![bar.clone(), pane(4), other_plugin], 4, false),
            (vec![bar, suspended, pane(4)], 4, false),
        ] {
            assert_eq!(alone(&panes, id), closes_tab, "pane {id} of {panes:?}");
            let (of_two, of_one) = if closes_tab {
                (Closes::Tab, Closes::Session)
            } else {
                (Closes::Pane, Closes::Pane)
            };
            assert_eq!(closes(&panes, id, 2), of_two, "pane {id} of {panes:?}, of two tabs");
            assert_eq!(closes(&panes, id, 1), of_one, "pane {id} of {panes:?}, of one tab");
        }
    }

    /// The tab whose id is `id`.
    fn tab_id(id: usize) -> TabId {
        TabId::of(&TabInfo { tab_id: id, ..TabInfo::default() })
    }

    fn agent(pane: u32, tab: usize, status: &str) -> Agent {
        let sits = Sits::Pane { id: pane, tab: Some(tab_id(tab)) };
        Agent { sits, status: given::status(status) }
    }

    /// Pane `id` of session `luneta`, with `elo` to go to: pane 2 in tab 7
    /// beside pane 3, pane 5 alone in tab 8, and pane 6 alone in tab 9, as
    /// though that were the session's only tab.
    fn of(id: u32, agents: Fetch<Vec<Agent>>) -> Subject {
        let (tab, closes) = match id {
            5 => (8, Closes::Tab),
            6 => (9, Closes::Session),
            _ => (7, Closes::Pane),
        };
        let pane = Pane { id, tab: Some(tab_id(tab)), closes };
        Subject { session: "luneta".to_owned(), pane, agents, refuge: Some("elo".to_owned()) }
    }

    fn here() -> Seat {
        Seat::Here("luneta".to_owned())
    }

    /// A pane closes alone, or its tab with it once nothing else holds the
    /// tab open, and the picker closes another session's by the same rule.
    /// The session you are in goes once you have gone to another, by its key
    /// and by the last pane that holds its only tab open, which would end it.
    #[test]
    fn each_key_deletes_what_it_names_and_what_goes_with_it() {
        let leave = || Act::Leave { session: "luneta".to_owned(), to: "elo".to_owned() };
        for (id, scope, act) in [
            (2, Scope::Pane, Act::Pane(here(), 2)),
            (5, Scope::Pane, Act::Tab(here(), tab_id(8))),
            (6, Scope::Pane, leave()),
            (2, Scope::Session, leave()),
        ] {
            let plan = of(id, Fetch::Ready(Vec::new())).plan(scope);
            assert_eq!(plan.map(|plan| plan.act), Ok(act), "{scope:?} of {id}");
        }
        let elo = Seat::There("elo".to_owned());
        assert_eq!(of(5, Fetch::Waiting).pane.act(elo.clone()), Act::Tab(elo, tab_id(8)));
    }

    /// With nowhere to go, the session you are in stays: killed, it would
    /// take you out of zellij, and so would closing the last pane that holds
    /// its only tab open. A pane in a tab no one has named yet closes alone.
    #[test]
    fn a_key_with_nothing_it_can_delete_says_why() {
        for (id, scope) in [(2, Scope::Session), (6, Scope::Pane)] {
            let alone = Subject { refuge: None, ..of(id, Fetch::Ready(Vec::new())) };
            let said = alone.plan(scope).err();
            assert!(said.as_deref().is_some_and(|said| said.contains("only session")), "{said:?}");
        }
        let untold = Subject {
            pane: Pane { id: 5, tab: None, closes: Closes::Tab },
            ..of(5, Fetch::Ready(Vec::new()))
        };
        let act = untold.plan(Scope::Pane).map(|plan| plan.act);
        assert_eq!(act, Ok(Act::Pane(here(), 5)));
    }

    /// A key asks first only when an agent it ends is busy or waiting on you,
    /// and says what goes. One that luneta cannot tell about asks too: an
    /// agent with a status luneta does not know or none, or every agent when
    /// the daemon has not said.
    #[test]
    fn a_key_asks_first_only_when_an_agent_at_work_would_end() {
        let agents = || {
            Fetch::Ready(vec![
                agent(2, 7, "idle"),
                agent(3, 7, "busy"),
                agent(5, 8, "waiting"),
                agent(6, 9, "shell"),
                agent(4, 10, "compacting"),
                Agent { status: Status::Unreported, ..agent(9, 11, "idle") },
            ])
        };
        let session = "Kill session luneta? 5 agents, 1 busy, 1 waiting, 2 unknown";
        for (id, scope, asked) in [
            (2, Scope::Pane, None),
            (3, Scope::Pane, Some("Close the pane? 1 agent, 1 busy")),
            (5, Scope::Pane, Some("Close its tab? 1 agent, 1 waiting")),
            (4, Scope::Pane, Some("Close the pane? 1 agent, 1 unknown")),
            (9, Scope::Pane, Some("Close the pane? 1 agent, 1 unknown")),
            (6, Scope::Pane, Some(session)),
            (2, Scope::Session, Some(session)),
        ] {
            let plan = of(id, agents()).plan(scope).expect("a plan");
            assert_eq!(plan.question().as_deref(), asked, "{scope:?} of {id}");
        }
        for agents in [Fetch::Waiting, Fetch::Failed("lunetad does not answer".to_owned())] {
            let plan = of(2, agents).plan(Scope::Pane).expect("a plan");
            assert!(plan.question().is_some());
        }
    }

    /// A key counts every agent the daemon gives, those the rows leave out
    /// too. Pane 3 holds the agent you talk to, idle, and a busy `claude -p`
    /// it runs, which the rows leave out for the pane holding one. A busy
    /// agent that names `luneta` sits in no pane luneta can find, and may be
    /// in any that closes. `elo`'s waiting agent is guessed into `luneta`'s
    /// pane 2, whose own agent refutes the guess: it ends with `elo`.
    #[test]
    fn a_key_counts_the_agents_the_rows_leave_out() {
        let entered = |entrypoint: &str, agent: v1::Agent| v1::Agent {
            entrypoint: Some(entrypoint.to_owned()),
            ..agent
        };
        let given = [
            entered("cli", given::agent(101, "luneta", 3, "/w/luneta", "idle")),
            entered("sdk-cli", given::agent(102, "luneta", 3, "/w/luneta", "busy")),
            given::agent(103, "luneta", 2, "/w/luneta", "idle"),
            given::unfollowed(given::agent(104, "luneta", 9, "/w/gone", "busy")),
            given::unfollowed(given::agent(105, "elo", 2, "/w/luneta", "waiting")),
        ];
        let places = Places::of(&[("luneta", &[(2, "/w/luneta"), (3, "/w/luneta")])]);
        let elo = [Session { name: "elo".to_owned(), age: Age::from_secs(1) }];
        let mut agents = AgentSet::default();
        agents.heard(&given, given::NOW);
        let live = Live::new(Some("luneta"), &elo, &places, &[]);
        agents.rebuild("", &live, None, given::NOW, Selection::SnapToTop);
        assert_eq!((agents.rows.len(), agents.doubled, agents.unplaced), (2, 1, 2));

        let unplaced = "1 more at work in a pane luneta cannot find";
        for (id, scope, asked) in [
            (3, Scope::Pane, format!("Close the pane? 2 agents, 1 busy, {unplaced}")),
            (2, Scope::Pane, format!("Close the pane? 1 agent, {unplaced}")),
            (2, Scope::Session, "Kill session luneta? 4 agents, 2 busy".to_owned()),
        ] {
            let plan = of(id, agents_of(&agents, &[], "luneta", &[])).plan(scope).expect("a plan");
            assert_eq!(plan.question(), Some(asked), "{scope:?} of {id}");
        }

        // The menu in `elo`, with `luneta` to go to.
        let ends = agents_of(&agents, &[], "elo", &[]);
        let refuge = Some("luneta".to_owned());
        let elo = Subject { session: "elo".to_owned(), refuge, ..of(2, ends) };
        let asked = elo.plan(Scope::Session).expect("a plan").question();
        assert_eq!(asked.as_deref(), Some("Kill session elo? 1 agent, 1 waiting"));
    }

    /// The session you leave is killed once zellij lists its clients without
    /// yours, client 1, and not while it lists yours, nor on an answer that
    /// is no list of them. Each tick asks once zellij has answered the last.
    /// A client that takes too long to leave leaves the session be.
    #[test]
    fn the_session_you_leave_ends_once_zellij_lists_you_out_of_it() {
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        let yours = "1         plugin_3       file:/p/luneta.wasm\n";
        let other = "2         terminal_4     N/A\n";
        let mut leaving =
            Leaving { session: "luneta".to_owned(), to: "elo".to_owned(), ticks: 0, asking: false };
        for (exit_code, stdout, killed) in [
            (Some(0), format!("{header}{yours}{other}"), false),
            (Some(0), format!("{header}{other}"), true),
            (Some(0), header.to_owned(), true),
            (Some(0), String::new(), false),
            (Some(0), "Session 'luneta' not found\n".to_owned(), false),
            (Some(1), header.to_owned(), false),
        ] {
            let said = leaving.listed(1, exit_code, stdout.as_bytes());
            assert_eq!(said, killed, "{exit_code:?}: {stdout:?}");
        }
        assert_eq!(leaving.tick(BTreeMap::new()), Ok(()));
        assert!(leaving.asking);
        leaving.listed(1, Some(0), format!("{header}{yours}").as_bytes());
        assert!(!leaving.asking);
        for _ in 2..PATIENCE {
            assert_eq!(leaving.tick(BTreeMap::new()), Ok(()));
        }
        let stayed = leaving.tick(BTreeMap::new()).err();
        assert!(stayed.as_deref().is_some_and(|said| said.contains("stays")), "{stayed:?}");
    }

    #[test]
    fn the_client_goes_first_to_the_session_the_picker_lists_first() {
        let others =
            [("old", Age::from_secs(900)), ("new", Age::from_secs(9)), ("b", Age::from_secs(9))];
        assert_eq!(refuge(others), Some("b".to_owned()));
        assert_eq!(refuge([]), None);
    }
}
