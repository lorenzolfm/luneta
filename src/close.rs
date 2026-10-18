//! Deleting what a menu is for: its pane, or its session. What goes, which
//! agents end with it, and asking zellij for it.
//!
//! zellij closes a tab with its last tiled pane. The sidebar's column never
//! closes, so a tab that keeps one would stay open with nothing of yours in
//! it: closing the last pane that holds a tab open closes the tab instead.
//! A session's only tab takes the session with it, and the menu deletes the
//! session for it.

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use crate::agents::{AgentSet, Seat, Status};
use crate::dock::{TabId, SIDEBAR_TITLE};
use crate::elapsed::Age;
use crate::fetch::Fetch;
use crate::panes;
use crate::tree::{is_placeholder, Leaf};

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

/// Whether `pane` keeps its tab open: a tiled pane you can go to, and not the
/// sidebar's column. A tab left with none closes, as it would without the
/// column.
pub fn holds_tab(pane: &PaneInfo) -> bool {
    let sidebar = pane.is_plugin && pane.title == SIDEBAR_TITLE;
    pane.is_selectable
        && !pane.is_suppressed
        && !pane.is_floating
        && !is_placeholder(pane)
        && !sidebar
}

/// Whether nothing among a tab's `panes` but the terminal `pane` keeps the tab
/// open: closing the pane closes the tab, its floating panes and all. A
/// terminal another stands in place of keeps it open too, since zellij brings
/// it back once the other closes.
pub fn alone(panes: &[PaneInfo], pane: u32) -> bool {
    let suspended =
        |other: &PaneInfo| !other.is_plugin && other.is_suppressed && !is_placeholder(other);
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

/// An agent of a session: the pane it sits in, the tab that pane is in, and
/// what it is doing.
#[derive(Debug)]
pub struct Agent {
    pub pane: u32,
    pub tab: Option<TabId>,
    pub status: Status,
}

/// The agents of `session` among those `agents` placed, each with the tab of
/// its pane among the session's `leaves`.
pub fn agents_of(agents: &AgentSet, session: &str, leaves: &[Leaf]) -> Fetch<Vec<Agent>> {
    match &agents.status {
        Fetch::Ready(_) => {},
        Fetch::Waiting => return Fetch::Waiting,
        Fetch::Failed(reason) => return Fetch::Failed(reason.clone()),
    }
    let tab = |pane: u32| leaves.iter().find(|leaf| leaf.id == pane).and_then(|leaf| leaf.tab_id);
    let seated = agents.rows.iter().filter(|row| row.seat.session() == session);
    Fetch::Ready(
        seated
            .map(|row| Agent { pane: row.pane, tab: tab(row.pane), status: row.status.clone() })
            .collect(),
    )
}

/// What a menu is for, a pane of a session or the session's own row, and what
/// deleting it takes along, as far as whoever opened the menu knows.
#[derive(Debug)]
pub struct Subject {
    pub seat: Seat,
    /// The pane, or none for the session's row.
    pub pane: Option<Pane>,
    /// The agents of its session, once claude-ps has said.
    pub agents: Fetch<Vec<Agent>>,
    /// Where the client goes before the session it is in is killed.
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
    /// Pane `id` of the session `seat` names, among the session's `leaves`,
    /// or the session's row with no `id`.
    pub fn new(
        seat: Seat,
        id: Option<u32>,
        leaves: &[Leaf],
        agents: Fetch<Vec<Agent>>,
        refuge: Option<String>,
    ) -> Self {
        let pane = id.map(|id| Pane::of(id, leaves));
        Subject { seat, pane, agents, refuge }
    }

    /// What the key that deletes `scope` does here, or why it does nothing.
    /// The pane closes, or its tab once nothing else holds the tab open; a
    /// session's row has only its session to delete, by a key that says so.
    /// The last pane that holds a session's only tab open ends the session
    /// with it, and deletes the session: the session you are in goes once
    /// you are in another, as it would for a key that kills it.
    pub fn plan(&self, scope: Scope) -> Result<Plan, String> {
        let seat = self.seat.clone();
        let act = match (scope, &self.pane) {
            (Scope::Pane, None) => {
                return Err(format!("menu: \"{}\" has no pane here to close", seat.session()));
            },
            (Scope::Pane, Some(pane)) if pane.closes != Closes::Session => pane.act(seat),
            (Scope::Pane, Some(_)) | (Scope::Session, _) => match seat {
                Seat::There(session) => Act::Kill(session),
                Seat::Here(session) => {
                    let Some(to) = self.refuge.clone() else {
                        return Err(format!(
                            "menu: \"{session}\" is the only session; there is none to go to \
                             before it ends"
                        ));
                    };
                    Act::Leave { session, to }
                },
            },
        };
        // The agents of the tab take in the pane's own: both have its tab from
        // the session's leaves.
        let takes = |agent: &&Agent| match &act {
            Act::Pane(_, pane) => agent.pane == *pane,
            Act::Tab(_, tab) => agent.tab == Some(*tab),
            Act::Kill(_) | Act::Leave { .. } => true,
        };
        let ending = match &self.agents {
            Fetch::Ready(agents) => {
                Fetch::Ready(Ending::of(agents.iter().filter(takes).map(|agent| &agent.status)))
            },
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
    /// Agents claude-ps said a word luneta does not know for, or nothing:
    /// they may be at work.
    unknown: usize,
}

impl Ending {
    fn of<'a>(statuses: impl Iterator<Item = &'a Status>) -> Self {
        let mut ending = Ending::default();
        for status in statuses {
            match status {
                Status::Shell => continue,
                Status::Busy => ending.busy += 1,
                Status::Waiting => ending.waiting += 1,
                Status::Unknown(_) | Status::Unreported => ending.unknown += 1,
                Status::Idle => {},
            }
            ending.agents += 1;
        }
        ending
    }

    /// Whether an agent that would end may be in the middle of something:
    /// busy, waiting on you, or doing what luneta cannot tell.
    fn at_work(self) -> bool {
        self.busy + self.waiting + self.unknown > 0
    }

    /// As `2 agents, 1 busy`.
    fn says(self) -> String {
        let mut said = vec![match self.agents {
            1 => "1 agent".to_owned(),
            agents => format!("{agents} agents"),
        }];
        if self.busy > 0 {
            said.push(format!("{} busy", self.busy));
        }
        if self.waiting > 0 {
            said.push(format!("{} waiting", self.waiting));
        }
        if self.unknown > 0 {
            said.push(format!("{} unknown", self.unknown));
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
    /// Kill another session.
    Kill(String),
    /// Go to the session `to`, and kill the session you are in once you have
    /// left it.
    Leave { session: String, to: String },
}

/// What came of asking.
pub enum Ran {
    /// Asked of the plugin's own session, which does it at once.
    Done,
    /// Asked of another session, which says how it went to a command run with
    /// the context given.
    Asked,
    Failed(String),
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
            Act::Kill(session) | Act::Leave { session, .. } => format!("Kill session {session}"),
        }
    }

    /// Asks zellij for it. A pane or a tab of another session closes by a
    /// command, which answers with `context`.
    pub fn run(&self, context: BTreeMap<String, String>) -> Ran {
        match self {
            Act::Pane(Seat::Here(_), pane) => close_terminal_pane(*pane),
            Act::Tab(Seat::Here(_), tab) => close_tab_with_id(tab.0 as u64),
            Act::Pane(Seat::There(session), pane) => {
                run_command(&close_pane(session, &panes::pane_id(*pane)), context);
                return Ran::Asked;
            },
            Act::Tab(Seat::There(session), tab) => {
                run_command(&close_tab(session, &tab.0.to_string()), context);
                return Ran::Asked;
            },
            Act::Kill(session) => {
                if let Err(error) = kill_sessions(&[session.as_str()]) {
                    return Ran::Failed(format!("menu: kill \"{session}\": {error}"));
                }
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
/// stays after all.
const PATIENCE: u32 = 50;

/// The client on its way to another session, and the session it leaves, to
/// kill once it has: killed first, the session would take the client with it.
pub struct Leaving {
    session: String,
    to: String,
    ticks: u32,
}

impl Leaving {
    fn go(session: &str, to: &str) -> Self {
        switch_session(Some(to));
        Leaving { session: session.to_owned(), to: to.to_owned(), ticks: 0 }
    }

    /// The session the client is on its way to.
    pub fn to(&self) -> &str {
        &self.to
    }

    /// Asks zellij which clients the session has, for `listed` to kill it
    /// once this plugin's is not among them. Says why the session stays once
    /// the client has taken too long to leave.
    pub fn tick(&mut self) -> Result<(), String> {
        self.ticks += 1;
        if self.ticks >= PATIENCE {
            return Err(format!(
                "menu: \"{}\" did not take you, and \"{}\" stays",
                self.to, self.session
            ));
        }
        list_clients();
        Ok(())
    }

    /// Kills the session once the `clients` zellij lists in it leave out this
    /// plugin's, and says whether it did. That the client is gone is what
    /// zellij says, not an answer it could not give. A `zellij` of its own
    /// kills it: the plugin asking would wait on the session it ends.
    pub fn listed(&self, clients: &[ClientInfo]) -> bool {
        if clients.iter().any(|client| client.is_current_client) {
            return false;
        }
        run_command(&kill_session(&self.session), BTreeMap::new());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32) -> PaneInfo {
        PaneInfo { id, is_selectable: true, ..PaneInfo::default() }
    }

    /// zellij closes a tab with its last tiled pane, floating panes and all,
    /// and a session with its only tab. The sidebar and its placeholder never
    /// close, and hold no tab open. A pane another stands in place of comes
    /// back once the other closes.
    #[test]
    fn the_last_pane_that_holds_its_tab_open_closes_the_tab() {
        let sidebar = PaneInfo { is_plugin: true, title: SIDEBAR_TITLE.to_owned(), ..pane(1) };
        let placeholder =
            PaneInfo { terminal_command: Some("tail -f /dev/null".to_owned()), ..pane(2) };
        let hidden = PaneInfo { is_suppressed: true, ..placeholder.clone() };
        let floating = PaneInfo { is_floating: true, ..pane(5) };
        let bar = PaneInfo { is_plugin: true, is_selectable: false, ..pane(3) };
        // A plugin pane of yours with a terminal's id.
        let other_plugin = PaneInfo { is_plugin: true, ..pane(4) };
        let suspended = PaneInfo { is_suppressed: true, ..pane(7) };
        let column = vec![sidebar, hidden, bar.clone()];
        for (panes, id, closes_tab) in [
            ([column.clone(), vec![pane(4)]].concat(), 4, true),
            ([column.clone(), vec![pane(4), floating.clone()]].concat(), 4, true),
            ([column.clone(), vec![pane(4), pane(6)]].concat(), 4, false),
            ([column.clone(), vec![pane(4), floating.clone()]].concat(), 5, false),
            (vec![placeholder, bar, floating], 5, true),
            ([column.clone(), vec![pane(4), other_plugin]].concat(), 4, false),
            ([column, vec![suspended, pane(4)]].concat(), 4, false),
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

    fn agent(pane: u32, tab: usize, status: &str) -> Agent {
        Agent { pane, tab: Some(TabId(tab)), status: Status::parse(Some(status)) }
    }

    /// Pane `id` of `seat`, or the session's row with none: pane 2 in tab 7
    /// beside pane 3, pane 5 alone in tab 8, and pane 6 alone in tab 9, as
    /// though that were the session's only tab.
    fn of(seat: Seat, id: Option<u32>, agents: Fetch<Vec<Agent>>) -> Subject {
        let pane = id.map(|id| {
            let (tab, closes) = match id {
                5 => (8, Closes::Tab),
                6 => (9, Closes::Session),
                _ => (7, Closes::Pane),
            };
            Pane { id, tab: Some(TabId(tab)), closes }
        });
        Subject { seat, pane, agents, refuge: Some("elo".to_owned()) }
    }

    fn here() -> Seat {
        Seat::Here("luneta".to_owned())
    }

    /// A pane closes alone, or its tab with it once nothing else holds the
    /// tab open. Another session's goes by a command. A session other than
    /// yours is killed, and yours once you have gone to another; a session's
    /// row deletes its session, and so does the last pane that holds its only
    /// tab open, which would end it.
    #[test]
    fn each_key_deletes_what_it_names_and_what_goes_with_it() {
        let elo = Seat::There("elo".to_owned());
        let leave = || Act::Leave { session: "luneta".to_owned(), to: "elo".to_owned() };
        for (seat, id, scope, act) in [
            (here(), Some(2), Scope::Pane, Act::Pane(here(), 2)),
            (here(), Some(5), Scope::Pane, Act::Tab(here(), TabId(8))),
            (here(), Some(6), Scope::Pane, leave()),
            (elo.clone(), Some(6), Scope::Pane, Act::Kill("elo".to_owned())),
            (elo.clone(), Some(2), Scope::Pane, Act::Pane(elo.clone(), 2)),
            (elo.clone(), Some(2), Scope::Session, Act::Kill("elo".to_owned())),
            (here(), Some(2), Scope::Session, leave()),
            (elo.clone(), None, Scope::Session, Act::Kill("elo".to_owned())),
            (here(), None, Scope::Session, leave()),
        ] {
            let plan = of(seat.clone(), id, Fetch::Ready(Vec::new())).plan(scope);
            assert_eq!(plan.map(|plan| plan.act), Ok(act), "{scope:?} of {id:?} in {seat:?}");
        }
    }

    /// With nowhere to go, the session you are in stays: killed, it would
    /// take you out of zellij, and so would closing the last pane that holds
    /// its only tab open. A session's row has no pane for a key that closes
    /// one, and kills no session for it. A pane in a tab no one has named yet
    /// closes alone.
    #[test]
    fn a_key_with_nothing_it_can_delete_says_why() {
        for (id, scope) in [(Some(2), Scope::Session), (Some(6), Scope::Pane)] {
            let alone = Subject { refuge: None, ..of(here(), id, Fetch::Ready(Vec::new())) };
            let said = alone.plan(scope).err();
            assert!(said.as_deref().is_some_and(|said| said.contains("only session")), "{said:?}");
        }
        let row = of(Seat::There("elo".to_owned()), None, Fetch::Ready(Vec::new()));
        let said = row.plan(Scope::Pane).err();
        assert!(said.as_deref().is_some_and(|said| said.contains("no pane")), "{said:?}");
        let untold = Subject {
            pane: Some(Pane { id: 5, tab: None, closes: Closes::Tab }),
            ..of(here(), None, Fetch::Ready(Vec::new()))
        };
        let act = untold.plan(Scope::Pane).map(|plan| plan.act);
        assert_eq!(act, Ok(Act::Pane(here(), 5)));
    }

    /// A key asks first only when an agent it ends is busy or waiting on you,
    /// and says what goes. One that luneta cannot tell about asks too: an
    /// agent with a status luneta does not know or none, or every agent when
    /// claude-ps has not said.
    #[test]
    fn a_key_asks_first_only_when_an_agent_at_work_would_end() {
        let agents = || {
            Fetch::Ready(vec![
                agent(2, 7, "idle"),
                agent(3, 7, "busy"),
                agent(5, 8, "waiting"),
                agent(6, 9, "shell"),
                agent(4, 10, "compacting"),
                Agent { pane: 9, tab: Some(TabId(11)), status: Status::parse(None) },
            ])
        };
        let session = "Kill session luneta? 5 agents, 1 busy, 1 waiting, 2 unknown";
        for (id, scope, asked) in [
            (Some(2), Scope::Pane, None),
            (Some(3), Scope::Pane, Some("Close the pane? 1 agent, 1 busy")),
            (Some(5), Scope::Pane, Some("Close its tab? 1 agent, 1 waiting")),
            (Some(4), Scope::Pane, Some("Close the pane? 1 agent, 1 unknown")),
            (Some(9), Scope::Pane, Some("Close the pane? 1 agent, 1 unknown")),
            (Some(6), Scope::Pane, Some(session)),
            (Some(2), Scope::Session, Some(session)),
            (None, Scope::Session, Some(session)),
        ] {
            let plan = of(here(), id, agents()).plan(scope).expect("a plan");
            assert_eq!(plan.question().as_deref(), asked, "{scope:?} of {id:?}");
        }
        for agents in [Fetch::Waiting, Fetch::Failed("claude-ps is not available".to_owned())] {
            let plan = of(here(), Some(2), agents).plan(Scope::Pane).expect("a plan");
            assert!(plan.question().is_some());
        }
    }

    /// The session you leave is killed once zellij lists its clients without
    /// yours, and not while it lists yours. A client that takes too long to
    /// leave leaves the session be.
    #[test]
    fn the_session_you_leave_ends_once_zellij_lists_you_out_of_it() {
        let client = |id, is_current_client| ClientInfo {
            client_id: id,
            pane_id: PaneId::Terminal(1),
            running_command: String::new(),
            is_current_client,
        };
        let mut leaving = Leaving { session: "luneta".to_owned(), to: "elo".to_owned(), ticks: 0 };
        assert!(!leaving.listed(&[client(1, true), client(2, false)]));
        assert!(leaving.listed(&[client(2, false)]));
        assert!(leaving.listed(&[]));
        for _ in 1..PATIENCE {
            assert_eq!(leaving.tick(), Ok(()));
        }
        let stayed = leaving.tick().err();
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
