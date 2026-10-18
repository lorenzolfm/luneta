//! Closing what a menu is for: the pane, its tab or its session. What goes,
//! which agents end with it, and asking zellij for it.
//!
//! zellij closes a tab with its last tiled pane. The sidebar's column never
//! closes, so a tab that keeps one would stay open with nothing of yours in
//! it: closing the last pane that holds a tab open closes the tab instead.

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use crate::agents::{AgentSet, Seat, Status};
use crate::dock::{self, SIDEBAR_TITLE};
use crate::elapsed::Age;
use crate::fetch::Fetch;
use crate::panes;
use crate::tree::{is_placeholder, Leaf};

/// What a close key closes, as its `close="pane"`, `"tab"` or `"session"`
/// names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Pane,
    Tab,
    Session,
}

impl Scope {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "pane" => Some(Scope::Pane),
            "tab" => Some(Scope::Tab),
            "session" => Some(Scope::Session),
            _ => None,
        }
    }

    /// What the menu says a key that closes it does.
    pub fn does(self) -> &'static str {
        match self {
            Scope::Pane => "close the pane",
            Scope::Tab => "close its tab",
            Scope::Session => "kill its session",
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
/// open: closing the pane closes the tab, its floating panes and all.
pub fn alone(panes: &[PaneInfo], pane: u32) -> bool {
    !panes.iter().any(|other| (other.is_plugin || other.id != pane) && holds_tab(other))
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
    pub tab: Option<usize>,
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

/// The pane a menu is for, and what closing it, its tab or its session takes
/// along, as far as whoever opened the menu knows.
#[derive(Debug)]
pub struct Pane {
    pub seat: Seat,
    pub id: u32,
    /// Its tab, by its id, once its session has said.
    pub tab: Option<usize>,
    /// Whether nothing else keeps its tab open.
    pub alone: bool,
    /// The agents of its session, once claude-ps has said.
    pub agents: Fetch<Vec<Agent>>,
    /// Where the client goes before the session it is in is killed.
    pub refuge: Option<String>,
}

/// Which agents a close takes along.
#[derive(Clone, Copy)]
enum Reach {
    Pane,
    Tab(usize),
    Session,
}

impl Pane {
    /// Pane `id` of the session `seat` names, among the session's `leaves`.
    pub fn new(
        seat: Seat,
        id: u32,
        leaves: &[Leaf],
        agents: Fetch<Vec<Agent>>,
        refuge: Option<String>,
    ) -> Self {
        let leaf = leaves.iter().find(|leaf| leaf.id == id);
        Pane {
            seat,
            id,
            tab: leaf.and_then(|leaf| leaf.tab_id),
            alone: leaf.is_some_and(|leaf| leaf.alone),
            agents,
            refuge,
        }
    }

    /// What the key that closes `scope` does here, or why it does nothing.
    /// The pane closes, or its tab once nothing else holds the tab open.
    pub fn plan(&self, scope: Scope) -> Result<Plan, String> {
        let seat = self.seat.clone();
        let (act, reach) = match (scope, self.tab.filter(|_| self.alone)) {
            (Scope::Pane, Some(tab)) => (Act::Tab(seat, tab), Reach::Tab(tab)),
            (Scope::Pane, None) => (Act::Pane(seat, self.id), Reach::Pane),
            (Scope::Tab, _) => {
                let Some(tab) = self.tab else {
                    return Err(format!(
                        "menu: luneta does not know yet which tab pane {} is in; try again in a \
                         moment",
                        self.id
                    ));
                };
                (Act::Tab(seat, tab), Reach::Tab(tab))
            },
            (Scope::Session, _) => match seat {
                Seat::There(session) => (Act::Kill(session), Reach::Session),
                Seat::Here(session) => {
                    let Some(to) = self.refuge.clone() else {
                        return Err(format!(
                            "menu: \"{session}\" is the only session; there is none to go to \
                             before it ends"
                        ));
                    };
                    (Act::Leave { session, to }, Reach::Session)
                },
            },
        };
        let takes = |agent: &&Agent| match reach {
            Reach::Pane => agent.pane == self.id,
            Reach::Tab(tab) => agent.pane == self.id || agent.tab == Some(tab),
            Reach::Session => true,
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

/// The agents a close would end, by what they are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ending {
    agents: usize,
    busy: usize,
    waiting: usize,
}

impl Ending {
    fn of<'a>(statuses: impl Iterator<Item = &'a Status>) -> Self {
        let mut ending = Ending::default();
        for status in statuses {
            match status {
                Status::Shell => continue,
                Status::Busy => ending.busy += 1,
                Status::Waiting => ending.waiting += 1,
                Status::Idle | Status::Unknown(_) | Status::Unreported => {},
            }
            ending.agents += 1;
        }
        ending
    }

    /// Whether an agent that would end is in the middle of something: busy,
    /// or waiting on you.
    fn at_work(self) -> bool {
        self.busy + self.waiting > 0
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
        said.join(", ")
    }
}

/// What a close key does, and the agents it would end.
#[derive(Debug)]
pub struct Plan {
    pub act: Act,
    ending: Fetch<Ending>,
}

impl Plan {
    /// What to ask before going ahead, as `Kill session elo? 2 agents, 1
    /// busy`. Nothing when luneta knows that no agent busy or waiting on you
    /// would end: an idle agent or a shell goes without asking.
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

/// What a close key asks zellij for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Close a terminal pane of the session the seat names.
    Pane(Seat, u32),
    /// Close a tab of the session the seat names, by its id.
    Tab(Seat, usize),
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
            Act::Tab(Seat::Here(_), tab) => close_tab_with_id(*tab as u64),
            Act::Pane(Seat::There(session), pane) => {
                run_command(&close_pane(session, &panes::pane_id(*pane)), context);
                return Ran::Asked;
            },
            Act::Tab(Seat::There(session), tab) => {
                run_command(&close_tab(session, &tab.to_string()), context);
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

/// What a tick found.
#[derive(Debug, PartialEq, Eq)]
pub enum Left {
    /// The client is still here.
    Not,
    /// The client is gone, and the session it left is being killed.
    Killed,
    /// The client never left, and the session stays.
    Stayed(String),
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

    /// Kills the session once zellij knows this plugin's client no longer in
    /// it. A `zellij` of its own kills it: the plugin asking would wait on the
    /// session it ends.
    pub fn tick(&mut self) -> Left {
        if dock::attached() == Some(false) {
            run_command(&kill_session(&self.session), BTreeMap::new());
            return Left::Killed;
        }
        self.ticks += 1;
        if self.ticks < PATIENCE {
            return Left::Not;
        }
        Left::Stayed(format!(
            "menu: \"{}\" did not take you, and \"{}\" stays",
            self.to, self.session
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32) -> PaneInfo {
        PaneInfo { id, is_selectable: true, ..PaneInfo::default() }
    }

    /// zellij closes a tab with its last tiled pane, floating panes and all.
    /// The sidebar and its placeholder never close, and hold no tab open.
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
        let column = vec![sidebar, hidden, bar.clone()];
        for (panes, id, closes_tab) in [
            ([column.clone(), vec![pane(4)]].concat(), 4, true),
            ([column.clone(), vec![pane(4), floating.clone()]].concat(), 4, true),
            ([column.clone(), vec![pane(4), pane(6)]].concat(), 4, false),
            ([column.clone(), vec![pane(4), floating.clone()]].concat(), 5, false),
            (vec![placeholder, bar, floating], 5, true),
            ([column, vec![pane(4), other_plugin]].concat(), 4, false),
        ] {
            assert_eq!(alone(&panes, id), closes_tab, "pane {id} of {panes:?}");
        }
    }

    fn agent(pane: u32, tab: usize, status: &str) -> Agent {
        Agent { pane, tab: Some(tab), status: Status::parse(Some(status)) }
    }

    /// Pane 2 of `seat`, in tab 7 beside pane 3, and pane 5 alone in tab 8.
    fn of(seat: Seat, id: u32, agents: Fetch<Vec<Agent>>) -> Pane {
        let (tab, alone) = if id == 5 { (8, true) } else { (7, false) };
        Pane { seat, id, tab: Some(tab), alone, agents, refuge: Some("elo".to_owned()) }
    }

    fn here() -> Seat {
        Seat::Here("luneta".to_owned())
    }

    /// A pane closes alone, or its tab with it once nothing else holds the
    /// tab open. Another session's goes by a command; a session other than
    /// yours is killed, and yours once you have gone to another.
    #[test]
    fn each_key_closes_what_it_names_and_what_goes_with_it() {
        let elo = Seat::There("elo".to_owned());
        for (seat, id, scope, act) in [
            (here(), 2, Scope::Pane, Act::Pane(here(), 2)),
            (here(), 5, Scope::Pane, Act::Tab(here(), 8)),
            (here(), 2, Scope::Tab, Act::Tab(here(), 7)),
            (elo.clone(), 2, Scope::Pane, Act::Pane(elo.clone(), 2)),
            (elo.clone(), 2, Scope::Session, Act::Kill("elo".to_owned())),
            (
                here(),
                2,
                Scope::Session,
                Act::Leave { session: "luneta".to_owned(), to: "elo".to_owned() },
            ),
        ] {
            let plan = of(seat.clone(), id, Fetch::Ready(Vec::new())).plan(scope);
            assert_eq!(plan.map(|plan| plan.act), Ok(act), "{scope:?} of pane {id} in {seat:?}");
        }
    }

    /// With nowhere to go, the session you are in stays: killed, it would
    /// take you out of zellij. A tab no one has named yet stays too.
    #[test]
    fn a_key_with_nothing_it_can_close_says_why() {
        let alone = Pane { refuge: None, ..of(here(), 2, Fetch::Ready(Vec::new())) };
        let said = alone.plan(Scope::Session).err();
        assert!(said.as_deref().is_some_and(|said| said.contains("only session")), "{said:?}");
        let untold = Pane { tab: None, ..of(here(), 2, Fetch::Ready(Vec::new())) };
        assert!(untold.plan(Scope::Tab).is_err());
        let act = untold.plan(Scope::Pane).map(|plan| plan.act);
        assert_eq!(act, Ok(Act::Pane(here(), 2)));
    }

    /// A key asks first only when an agent it ends is busy or waiting on you,
    /// and says what goes. One that luneta cannot tell about asks too.
    #[test]
    fn a_key_asks_first_only_when_an_agent_at_work_would_end() {
        let agents = || {
            Fetch::Ready(vec![
                agent(2, 7, "idle"),
                agent(3, 7, "busy"),
                agent(5, 8, "waiting"),
                agent(6, 9, "shell"),
            ])
        };
        for (id, scope, asked) in [
            (2, Scope::Pane, None),
            (2, Scope::Tab, Some("Close its tab? 2 agents, 1 busy")),
            (5, Scope::Pane, Some("Close its tab? 1 agent, 1 waiting")),
            (2, Scope::Session, Some("Kill session luneta? 3 agents, 1 busy, 1 waiting")),
        ] {
            let plan = of(here(), id, agents()).plan(scope).expect("a plan");
            assert_eq!(plan.question().as_deref(), asked, "{scope:?} of pane {id}");
        }
        for agents in [Fetch::Waiting, Fetch::Failed("claude-ps is not available".to_owned())] {
            let plan = of(here(), 2, agents).plan(Scope::Pane).expect("a plan");
            assert!(plan.question().is_some());
        }
    }

    #[test]
    fn the_client_goes_first_to_the_session_the_picker_lists_first() {
        let others =
            [("old", Age::from_secs(900)), ("new", Age::from_secs(9)), ("b", Age::from_secs(9))];
        assert_eq!(refuge(others), Some("b".to_owned()));
        assert_eq!(refuge([]), None);
    }
}
