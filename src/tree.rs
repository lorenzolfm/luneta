//! The sidebar's model: every live session, the panes in it, and the agent in
//! a pane where there is one.
//!
//! Sessions keep their place — oldest first, the order `zellij list-sessions`
//! prints — so a row never moves under the eye or the mouse. Saying which agent
//! wants you is the job of the glyph and of the `next` key, not of the order.
//! A search hides the rows that do not match and leaves the rest in that
//! order; the cursor, not the order, says which matches best.

use std::collections::{BTreeMap, HashMap};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use zellij_tile::prelude::{PaneInfo, PaneManifest, TabInfo};

use crate::agents::{AgentRow, AgentSet, Status};
use crate::brief;
use crate::checkouts::{Checkout, Checkouts, Said};
use crate::close::{self, Act, Closes};
use crate::cursor::{Cursor, Stops};
use crate::dirs::short_cwd;
use crate::dock::TabId;
use crate::elapsed::{Age, Held};
use crate::folds::Folds;
use crate::icons::{self, Icons};
use crate::menu::{Legend, SidebarMenu};
use crate::news::News;
use crate::places::Places;

/// What a placeholder runs: the column each tab keeps for the sidebar, which
/// the dock opens it into. Nothing anyone goes to.
pub const PLACEHOLDER: [&str; 3] = ["tail", "-f", "/dev/null"];

pub fn is_placeholder(pane: &PaneInfo) -> bool {
    !pane.is_plugin
        && pane.terminal_command.as_deref().is_some_and(|c| c.split(' ').eq(PLACEHOLDER))
}

/// The terminal you are in, of a tab's panes: the focused tiled one, or else
/// the focused floating one. The picker and the menu float over the tab with
/// the focus, and are no terminal. A brief is one, but you are not in it: it
/// hands the focus back to the sidebar as it closes.
pub fn origin(panes: &[PaneInfo]) -> Option<u32> {
    let focused = |floating: bool| {
        panes.iter().find(move |p| {
            p.is_focused
                && !p.is_plugin
                && !p.is_suppressed
                && p.is_floating == floating
                && !brief::is_brief(p)
        })
    };
    focused(false).or_else(|| focused(true)).map(|p| p.id)
}

/// A live session and its panes, as the sidebar is handed it.
pub struct Branch {
    pub name: String,
    pub age: Age,
    pub panes: Vec<Leaf>,
}

/// A terminal pane, the name of the tab that holds it and the id zellij keeps
/// for that tab, the directory it works in and what it runs in the
/// foreground, as its session last said, and what is checked out there, as
/// git last said. For the few seconds after a `cd` that git has yet to answer
/// for the new directory, it is what was checked out in the old one.
pub struct Leaf {
    pub id: u32,
    pub title: String,
    pub tab: String,
    pub tab_id: Option<TabId>,
    pub cwd: Option<String>,
    /// The binary its terminal runs in the foreground, by its name: the
    /// process group's in front, or the shell's itself at its prompt.
    pub runs: Option<String>,
    pub checkout: Option<Checkout>,
    /// What closing it closes with it: its tab once nothing else holds the
    /// tab open, and its session once that is the session's only tab.
    pub closes: Closes,
}

impl Branch {
    /// `before` is the session's panes as the sidebar last had them.
    pub fn new(
        name: String,
        age: Age,
        tabs: &[TabInfo],
        manifest: &PaneManifest,
        places: &Places,
        checkouts: &Checkouts,
        before: &[Leaf],
    ) -> Self {
        let panes = leaves(&name, tabs, manifest, places, checkouts, before);
        Branch { name, age, panes }
    }
}

/// The sessions in the order the sidebar draws them: the oldest first, so that
/// a row never moves.
pub fn in_order(branches: &[Branch]) -> Vec<&Branch> {
    let mut order: Vec<&Branch> = branches.iter().collect();
    order.sort_by(|a, b| b.age.cmp(&a.age).then_with(|| a.name.cmp(&b.name)));
    order
}

/// The panes a person can go to, in tab order, the tiled ones of a tab before
/// its floating ones, and in the order they were opened within each. The
/// sidebar's placeholders are not among them, nor a brief. A pane keeps what
/// was checked out where it worked, as it stood `before`, until git says what
/// is checked out where it works now, so that a `cd` does not take its
/// branch's line away and back. A session that leaves a pane out of one
/// answer has not said it moved, and the pane keeps its directory too, for
/// git to be asked about, and what it runs, unless it said what runs there
/// now: zellij leaves that out of an answer it could not get in time.
pub fn leaves(
    session: &str,
    tabs: &[TabInfo],
    manifest: &PaneManifest,
    places: &Places,
    checkouts: &Checkouts,
    before: &[Leaf],
) -> Vec<Leaf> {
    let mut keyed: Vec<((usize, bool, u32), Leaf)> = Vec::new();
    for (position, panes) in &manifest.panes {
        let tab = tabs.iter().find(|tab| tab.position == *position);
        let tab_id = tab.map(TabId::of);
        let tab = tab.map_or("", |tab| tab.name.as_str());
        for pane in panes {
            if pane.is_plugin
                || !pane.is_selectable
                || pane.is_suppressed
                || is_placeholder(pane)
                || brief::is_brief(pane)
            {
                continue;
            }
            let held = before.iter().find(|leaf| leaf.id == pane.id);
            let cwd = places
                .cwd(session, pane.id)
                .or_else(|| held.and_then(|leaf| leaf.cwd.as_deref()));
            let runs = match places.command(session, pane.id) {
                Some(command) => icons::program(command),
                None => held.and_then(|leaf| leaf.runs.as_deref()),
            };
            let checkout = match cwd.map_or(Said::Nothing, |cwd| checkouts.said(cwd)) {
                Said::Branch(checkout) => Some(checkout.clone()),
                Said::NoBranch => None,
                Said::Nothing => held.and_then(|leaf| leaf.checkout.clone()),
            };
            keyed.push((
                (*position, pane.is_floating, pane.id),
                Leaf {
                    id: pane.id,
                    title: pane.title.clone(),
                    tab: tab.to_owned(),
                    tab_id,
                    cwd: cwd.map(str::to_owned),
                    runs: runs.map(str::to_owned),
                    checkout,
                    closes: close::closes(panes, pane.id, manifest.panes.len()),
                },
            ));
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, leaf)| leaf).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Session(String),
    Pane(String, u32),
}

const SESSION_ARG: &str = "session";

const PANE_ARG: &str = "pane";

impl Place {
    /// The place as the arguments of a pipe message, by which the sidebar and
    /// the picker hand each other the row they have highlighted.
    pub fn to_args(&self) -> BTreeMap<String, String> {
        let (session, pane) = match self {
            Place::Session(session) => (session, None),
            Place::Pane(session, pane) => (session, Some(pane)),
        };
        let mut args = BTreeMap::from([(SESSION_ARG.to_owned(), session.clone())]);
        if let Some(pane) = pane {
            args.insert(PANE_ARG.to_owned(), pane.to_string());
        }
        args
    }

    pub fn from_args(args: &BTreeMap<String, String>) -> Option<Self> {
        let session = args.get(SESSION_ARG)?.clone();
        match args.get(PANE_ARG) {
            Some(pane) => Some(Place::Pane(session, pane.parse().ok()?)),
            None => Some(Place::Session(session)),
        }
    }
}

pub enum Node {
    Session(SessionNode),
    Pane(PaneNode),
}

pub struct SessionNode {
    pub name: String,
    pub current: bool,
    pub folded: bool,
    pub waiting: usize,
    /// The mark of the pane that wants you most, for a folded session to
    /// show in place of the rows it hides.
    pub loudest: Option<Mark>,
    /// The characters of the name a search lands on.
    pub hits: Vec<usize>,
}

pub struct PaneNode {
    pub session: String,
    pub id: u32,
    /// The id zellij keeps for its tab, once its session has said.
    pub tab: Option<TabId>,
    pub label: String,
    pub mark: Option<Mark>,
    /// What the pane's terminal runs in the foreground, by its binary's name.
    pub runs: Option<String>,
    /// What is checked out where the pane works, for the line under its row.
    pub checkout: Option<Checkout>,
    pub here: bool,
    /// The characters of the label a search lands on.
    pub hits: Vec<usize>,
}

impl PaneNode {
    /// The program that runs in the pane, by its binary's name, for its icon:
    /// Claude Code for an agent claude-ps finds there, whatever its terminal
    /// has in front. A shell that finished a command is back at its prompt.
    pub fn program(&self) -> Option<&str> {
        match self.mark {
            Some(Mark::Agent { .. } | Mark::Finished { .. }) => Some(icons::AGENT),
            Some(Mark::Ran { .. } | Mark::Running { .. } | Mark::Asking { .. }) | None => {
                self.runs.as_deref()
            },
        }
    }
}

/// What a pane's row shows of what goes on in it, beside its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// An agent in its status, and how long it has held it. An idle one has
    /// been seen since it finished.
    Agent { status: Status, age: Held },
    /// An agent that finished since you were last in its pane, and how long
    /// it has been idle.
    Finished { age: Held },
    /// A command its shell finished since you were last in its pane, how
    /// long ago, and whether it ended well, once its shell's hook says.
    Ran { age: Held, ok: Option<bool> },
    /// A command its shell has run for a while, and for how long.
    Running { age: Held },
    /// A command that waits for you to type, and how long it has run.
    Asking { age: Held },
}

impl Mark {
    /// The mark of `leaf`, with `agent` in it, as `news` has it, and its
    /// label: a command its shell runs, or finished, goes by the command. The
    /// pane you are in, as `here` says, has nothing new: you see it.
    fn of(
        news: &News,
        session: &str,
        leaf: &Leaf,
        agent: Option<&AgentRow>,
        here: bool,
    ) -> (Option<Mark>, String) {
        let Some(agent) = agent else {
            return match news.command(session, leaf.id, here) {
                Some((mark, command)) => (Some(mark), command.to_owned()),
                None => (None, label(leaf, None)),
            };
        };
        let mark = if !here && news.finished(session, leaf.id, agent) {
            Mark::Finished { age: agent.age }
        } else {
            Mark::Agent { status: agent.status.clone(), age: agent.age }
        };
        (Some(mark), label(leaf, Some(agent)))
    }

    pub fn age(&self) -> Held {
        match self {
            Mark::Agent { age, .. }
            | Mark::Finished { age }
            | Mark::Ran { age, .. }
            | Mark::Running { age }
            | Mark::Asking { age } => *age,
        }
    }

    /// Whether what it shows is new since you were last in its pane.
    pub fn is_new(&self) -> bool {
        matches!(self, Mark::Finished { .. } | Mark::Ran { .. })
    }

    /// Whether it waits for you: an agent that asked you something, or a
    /// command that waits for you to type.
    pub fn is_waiting(&self) -> bool {
        matches!(self, Mark::Agent { status: Status::Waiting, .. } | Mark::Asking { .. })
    }

    /// How much its pane wants you, the most first: what waits for you, then
    /// what is new, then what is at work, and then an agent you have seen
    /// finish.
    fn rank(&self) -> u8 {
        match self {
            Mark::Agent { status: Status::Waiting, .. } | Mark::Asking { .. } => 0,
            Mark::Finished { .. } | Mark::Ran { .. } => 1,
            Mark::Agent { status: Status::Busy, .. } | Mark::Running { .. } => 2,
            Mark::Agent { status: Status::Idle, .. } => 3,
            Mark::Agent { status: Status::Shell, .. } => 4,
            Mark::Agent { status: Status::Unknown(_), .. } => 5,
            Mark::Agent { status: Status::Unreported, .. } => 6,
        }
    }
}

impl Node {
    pub fn session(&self) -> &str {
        match self {
            Node::Session(node) => &node.name,
            Node::Pane(node) => &node.session,
        }
    }

    pub fn place(&self) -> Place {
        match self {
            Node::Session(node) => Place::Session(node.name.clone()),
            Node::Pane(node) => Place::Pane(node.session.clone(), node.id),
        }
    }

    fn is(&self, place: &Place) -> bool {
        match (self, place) {
            (Node::Session(node), Place::Session(name)) => node.name == *name,
            (Node::Pane(node), Place::Pane(session, id)) => {
                node.session == *session && node.id == *id
            },
            _ => false,
        }
    }
}

impl Stops for Node {
    /// The row of a session that lists its panes is their heading, and no
    /// place to go to. The cursor stops on a pane's row, and on a session's
    /// that lists none of its panes, folded or with none to list, which
    /// stands in for them. A session lists its panes in the rows right after
    /// its own, as `Tree::rebuild`, the one maker of the `Rows`, puts them.
    fn heads(&self, next: &Node) -> bool {
        matches!((self, next), (Node::Session(_), Node::Pane(_)))
    }
}

/// The rows of the tree and the cursor on them: read as a `Cursor`, moved by
/// `select` and `move_selection`, and made by `Tree::rebuild` alone.
#[derive(Default)]
pub struct Rows(Cursor<Node>);

impl std::ops::Deref for Rows {
    type Target = Cursor<Node>;

    fn deref(&self) -> &Cursor<Node> {
        &self.0
    }
}

impl Rows {
    /// `Cursor::select`.
    pub fn select(&mut self, index: usize) -> bool {
        self.0.select(index)
    }

    /// `Cursor::move_selection`.
    pub fn move_selection(&mut self, delta: isize) {
        self.0.move_selection(delta);
    }
}

/// The live sessions, and how many of the panes in them show a mark worth
/// counting, folded or not: what the sidebar's header says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub sessions: usize,
    /// Agents and commands that wait for you.
    pub waiting: usize,
    /// Agents and commands that finished since you were last in their panes.
    pub unseen: usize,
    /// Agents and commands at work.
    pub busy: usize,
}

impl Summary {
    fn count(&mut self, mark: &Mark) {
        match mark {
            Mark::Agent { status: Status::Waiting, .. } | Mark::Asking { .. } => self.waiting += 1,
            Mark::Finished { .. } | Mark::Ran { .. } => self.unseen += 1,
            Mark::Agent { status: Status::Busy, .. } | Mark::Running { .. } => self.busy += 1,
            Mark::Agent { .. } => {},
        }
    }
}

#[derive(Default)]
pub struct Tree {
    pub rows: Rows,
    /// Counts every session and agent, whatever a search hides.
    pub summary: Summary,
    /// The menu, while it is open at the bottom of the sidebar.
    pub menu: Option<SidebarMenu>,
    /// What the bottom of the sidebar shows while it has the focus, and
    /// neither the menu nor the search box is open. None in a test that
    /// leaves it out.
    pub legend: Option<Legend>,
    /// The icons on the panes' rows, none once the configuration turns them
    /// off.
    pub icons: Icons,
    /// What is wrong with the icons the configuration gives, for a note under
    /// the tree.
    pub icons_wrong: Option<String>,
    /// What is new in each pane since you were last in it. Boxed: it would
    /// make the sidebar twice the size of every other role.
    pub news: Box<News>,
    folded: Folds,
    search: Option<Search>,
    /// Boxed: a matcher is larger than the rest of the tree, and a sidebar
    /// that is never searched needs none.
    matcher: Option<Box<SkimMatcherV2>>,
    /// Whether the last `rebuild` had the cursor follow the pane you are in.
    following: bool,
    /// Where the cursor was when the sidebar last lost the focus, for the
    /// picker the focus went to.
    left: Option<Place>,
    /// The row the picker had highlighted when it handed the focus over.
    handed: Option<Place>,
    /// Where the wheel left the view.
    scrolled: Option<Scrolled>,
    /// The row the cursor was on when a delete key closed it, and the row to
    /// go to once it is gone.
    closed: Option<Closed>,
}

/// A row a delete key closed, and the first row after it that stays, or else
/// the last before it.
struct Closed {
    row: Place,
    next: Option<Place>,
}

/// The row at the top of the view the wheel moved, and the row the cursor was
/// on as it did, or since by a click. The view stays there while the cursor
/// stays on that row, with the focus or without: without, the cursor is on the
/// pane you are in, or on its session's row. Rows that come and go above it
/// leave it on its row. The cursor on another row, the row at the top gone,
/// or a key, ends it, and the view shows the lit row again.
struct Scrolled {
    top: Place,
    cursor: Option<Place>,
}

/// The search box, while it is open.
enum Search {
    /// Nothing typed: the tree under it is the tree without it.
    Empty,
    /// A term narrows the tree, and `seek` until the cursor has gone to the
    /// row that matches it best.
    Typed { term: String, seek: bool },
}

impl Tree {
    /// A tree whose folds and news are every session's sidebar's, over
    /// `legend`, and that draws `icons`, and says what is wrong with the ones
    /// the configuration gives.
    pub fn shared(legend: Legend, icons: Icons, icons_wrong: Option<String>) -> Self {
        let (folded, news) = (Folds::shared(), Box::new(News::shared()));
        Tree { folded, news, legend: Some(legend), icons, icons_wrong, ..Tree::default() }
    }

    /// A tree of `nodes` as they stand, the cursor on row `selected` or the
    /// next it stops on: for a test that draws rows `rebuild` would need far
    /// more to make. They take the shape `rebuild` gives them: each session's
    /// row, and then the panes it lists.
    #[cfg(test)]
    pub fn of(nodes: Vec<Node>, selected: usize) -> Self {
        let mut tree = Tree::default();
        tree.rows.0.replace_at(nodes, Some(selected));
        tree
    }

    /// Takes up what the sidebars of other sessions folded and unfolded
    /// since. The rows are only redrawn by the next `rebuild`.
    pub fn refold(&mut self) {
        self.folded.reload();
    }

    /// `follow` puts the cursor on the pane you are in, which is where it
    /// belongs while the sidebar does not have the focus. Otherwise the cursor
    /// stays on the row it was on, or on its session once that is folded. A
    /// session's row that is no stop, as a session the picker handed over or
    /// one unfolded under the cursor, has the cursor on its first pane.
    ///
    /// While the search box holds a term, only the rows that match it are
    /// kept, with the session of each pane that does, unfolded. A session
    /// found by its name keeps all its panes. The cursor goes to the best
    /// match when the term changes, and stays on its row otherwise. A session
    /// that matches best by its name has it on the pane of it that matches
    /// best, or else on its first.
    ///
    /// A search lasts while the sidebar has the focus, so `follow` closes it.
    /// One left behind would keep the rows it hides out of sight. So does the
    /// menu, which is for the row the cursor was on, but one that takes the
    /// client out of the session: the focus leaves with the client.
    ///
    /// The row the cursor was on when a delete key closed it hands the cursor
    /// to the row after it, once it is gone.
    ///
    /// The row the cursor is on when `follow` comes is kept for the picker,
    /// and a row the picker handed over holds the cursor against `follow`
    /// until the sidebar has the focus.
    pub fn rebuild(
        &mut self,
        branches: &[Branch],
        current: Option<&str>,
        agents: &AgentSet,
        here: Option<u32>,
        follow: bool,
    ) {
        if follow && !self.following {
            self.left = self.rows.selected_row().map(Node::place);
        }
        if !follow {
            self.left = None;
        }
        self.following = follow;
        if follow {
            self.search = None;
            self.menu = self.menu.take().filter(SidebarMenu::lasts);
        }
        let seated: HashMap<(&str, u32), &AgentRow> =
            agents.rows.iter().map(|row| ((row.seat.session(), row.pane), row)).collect();

        let order = in_order(branches);

        let (term, seek) = match &mut self.search {
            Some(Search::Typed { term, seek }) => (Some(term.as_str()), std::mem::take(seek)),
            Some(Search::Empty) | None => (None, false),
        };
        let finder = term.map(|term| Finder {
            matcher: self
                .matcher
                .get_or_insert_with(|| Box::new(SkimMatcherV2::default().use_cache(true))),
            term,
        });

        let mut summary = Summary { sessions: branches.len(), ..Summary::default() };
        let mut rows = Vec::new();
        let mut best: Option<(usize, Fit)> = None;
        let mut weigh = |at: usize, fit: Fit| {
            if best.is_none_or(|(_, top)| fit > top) {
                best = Some((at, fit));
            }
        };
        for branch in order {
            let is_current = current == Some(branch.name.as_str());
            let folded = self.folded.contains(&branch.name);
            let panes: Vec<(&Leaf, bool, Option<Mark>, String)> = branch
                .panes
                .iter()
                .map(|leaf| {
                    let agent = seated.get(&(branch.name.as_str(), leaf.id)).copied();
                    let is_here = is_current && here == Some(leaf.id);
                    let (mark, label) = Mark::of(&self.news, &branch.name, leaf, agent, is_here);
                    (leaf, is_here, mark, label)
                })
                .collect();
            let marks = || panes.iter().filter_map(|(_, _, mark, _)| mark.as_ref());
            for mark in marks() {
                summary.count(mark);
            }
            // A search looks inside a folded session too.
            let open = if finder.is_some() || !folded { panes.as_slice() } else { &[] };
            let named = finder.as_ref().and_then(|finder| finder.session(&branch.name));
            let mut shown = Vec::new();
            for (leaf, here, mark, label) in open {
                let found = finder.as_ref().and_then(|finder| finder.pane(label, leaf));
                if finder.is_some() && found.is_none() && named.is_none() {
                    continue;
                }
                let (fit, hits) = found.unzip();
                let node = PaneNode {
                    session: branch.name.clone(),
                    id: leaf.id,
                    tab: leaf.tab_id,
                    label: label.clone(),
                    mark: mark.clone(),
                    runs: leaf.runs.clone(),
                    checkout: leaf.checkout.clone(),
                    here: *here,
                    hits: hits.unwrap_or_default(),
                };
                shown.push((node, fit));
            }
            let (fit, hits) = named.unzip();
            if finder.is_some() && fit.is_none() && shown.is_empty() {
                continue;
            }
            let head = rows.len();
            rows.push(Node::Session(SessionNode {
                name: branch.name.clone(),
                current: is_current,
                folded: if finder.is_some() { shown.is_empty() } else { folded },
                waiting: marks().filter(|mark| mark.is_waiting()).count(),
                loudest: marks().min_by_key(|mark| mark.rank()).cloned(),
                hits: hits.unwrap_or_default(),
            }));
            // The pane of the session that matches best, which a session
            // found by its name hands the cursor on to. With none, its row
            // hands it on to its first pane.
            let mut lead: Option<(usize, Fit)> = None;
            for (node, matched) in shown {
                if let Some(matched) = matched {
                    weigh(rows.len(), matched);
                    if lead.is_none_or(|(_, top)| matched > top) {
                        lead = Some((rows.len(), matched));
                    }
                }
                rows.push(Node::Pane(node));
            }
            if let Some(fit) = fit {
                weigh(lead.map_or(head, |(at, _)| at), fit);
            }
        }
        let best = best.map(|(at, _)| at);
        self.summary = summary;

        let followed = if follow {
            current.zip(here).map(|(session, pane)| Place::Pane(session.to_owned(), pane))
        } else {
            None
        };
        let handed = if follow { self.handed.clone() } else { self.handed.take() };
        let held = handed.or(followed).or_else(|| self.rows.selected_row().map(Node::place));
        let row_of = |place: &Place| rows.iter().position(|node| node.is(place));
        // Forgotten once the cursor leaves the row, or the row its place.
        let held = match self.closed.take() {
            Some(closed) if held.as_ref() == Some(&closed.row) => {
                if row_of(&closed.row).is_some() {
                    self.closed = Some(closed);
                    held
                } else {
                    closed.next
                }
            },
            _ => held,
        };
        let at = match best {
            Some(best) if seek => Some(best),
            _ => held
                .and_then(|place| row_of(&place).or_else(|| row_of(&self.shown(place))))
                .or(best),
        };
        self.rows.0.replace_at(rows, at);
        // A view the wheel left for a row the cursor has left would come back
        // with the cursor.
        if self.top().is_none() {
            self.scrolled = None;
        }
    }

    /// What is typed in the search box, while it is open.
    pub fn search(&self) -> Option<&str> {
        self.search.as_ref().map(|search| match search {
            Search::Empty => "",
            Search::Typed { term, .. } => term.as_str(),
        })
    }

    /// Whether a term is typed in the search box. A box with nothing in it
    /// narrows nothing, and the tree under it works as it does without one.
    pub fn narrowed(&self) -> bool {
        matches!(self.search, Some(Search::Typed { .. }))
    }

    /// Opens the search box with `term` in it, or puts `term` in the open one.
    /// The next `rebuild` keeps the rows that match it, and puts the cursor on
    /// the one that matches best. An empty `term` narrows nothing.
    pub fn search_for(&mut self, term: String) {
        self.search =
            Some(if term.is_empty() { Search::Empty } else { Search::Typed { term, seek: true } });
    }

    /// Closes the search box, and says whether it was open. The next `rebuild`
    /// brings every row back, the cursor on the row it was on.
    pub fn close_search(&mut self) -> bool {
        self.search.take().is_some()
    }

    /// Puts the cursor on `place`, the row the picker had highlighted when it
    /// handed over the focus, from the next `rebuild` until the sidebar has
    /// the focus. A pane in a folded session unfolds it, to show the row, and
    /// a session that lists its panes has the cursor on the first.
    pub fn hand(&mut self, place: Place) {
        if let Place::Pane(session, _) = &place {
            self.folded.set(session, false);
        }
        self.handed = Some(place);
    }

    /// Where the cursor was when the sidebar lost the focus, once.
    pub fn take_left(&mut self) -> Option<Place> {
        self.left.take()
    }

    /// The row at the top of the view the wheel left, while the cursor is on
    /// the row it was on then, and that row is still listed.
    pub fn top(&self) -> Option<usize> {
        let scrolled = self.scrolled.as_ref().filter(|scrolled| self.cursor() == scrolled.cursor)?;
        self.rows.iter().position(|node| node.is(&scrolled.top))
    }

    /// Puts row `top` at the top of the view, until the cursor leaves its row
    /// or the row is gone.
    pub fn scroll_to(&mut self, top: usize) {
        let cursor = self.cursor();
        self.scrolled = self.rows.get(top).map(|node| Scrolled { top: node.place(), cursor });
    }

    /// Has the cursor go past the rows `act` closes once they are gone: to the
    /// first row after them, or else the last before them, as the rows stand
    /// then. A pane takes its row, a tab the rows of its panes, and a session
    /// every row of it.
    pub fn closing(&mut self, act: &Act) {
        let gone = |node: &Node| match (act, node) {
            (Act::Pane(seat, pane), Node::Pane(node)) => {
                node.session == seat.session() && node.id == *pane
            },
            (Act::Tab(seat, tab), Node::Pane(node)) => {
                node.session == seat.session() && node.tab == Some(*tab)
            },
            (Act::Kill(session) | Act::Leave { session, .. }, node) => node.session() == session,
            (Act::Pane(..) | Act::Tab(..), Node::Session(_)) => false,
        };
        let Some(at) = self.rows.selected() else {
            return;
        };
        let after = self.rows[at..].iter().find(|node| !gone(node));
        let next = after.or_else(|| self.rows[..at].iter().rev().find(|node| !gone(node)));
        self.closed = Some(Closed { row: self.rows[at].place(), next: next.map(Node::place) });
    }

    /// Brings the view back to the lit row, and says whether it was away.
    pub fn unscroll(&mut self) -> bool {
        self.scrolled.take().is_some()
    }

    fn cursor(&self) -> Option<Place> {
        self.rows.selected_row().map(Node::place)
    }

    /// The pane listed after `here`, or the first one when `here` is not
    /// listed: where `cycle` goes. A folded session lists none of its panes,
    /// and a search only the ones it finds.
    pub fn after(&self, here: Option<&Place>) -> Option<Place> {
        let panes: Vec<Place> =
            self.rows.iter().filter(|node| matches!(node, Node::Pane(_))).map(Node::place).collect();
        let at = here.and_then(|here| panes.iter().position(|place| place == here));
        match at {
            Some(_) if panes.len() == 1 => None,
            Some(at) => panes.get((at + 1) % panes.len()).cloned(),
            None => panes.first().cloned(),
        }
    }

    /// A pane in a folded session is shown by its session's row.
    fn shown(&self, place: Place) -> Place {
        match place {
            Place::Pane(session, _) if self.folded.contains(&session) => Place::Session(session),
            place => place,
        }
    }

    /// Folds or unfolds the session of the highlighted row, and says whether
    /// that changed anything. The rows are only redrawn by the next `rebuild`.
    pub fn fold(&mut self, folded: bool) -> bool {
        let Some(name) = self.rows.selected_row().map(|node| node.session().to_owned()) else {
            return false;
        };
        self.folded.set(&name, folded)
    }

    pub fn toggle(&mut self) -> bool {
        self.rows.selected().is_some_and(|at| self.toggle_row(at))
    }

    /// Folds or unfolds the session of row `at`, whichever it is not, as a
    /// click on its fold does, wherever the cursor is.
    pub fn toggle_row(&mut self, at: usize) -> bool {
        let Some(name) = self.rows.get(at).map(|node| node.session().to_owned()) else {
            return false;
        };
        let folded = self.folded.contains(&name);
        self.folded.set(&name, !folded)
    }
}

/// How well a row matches a search: a row that shows the term itself beats
/// any other, and then the higher score wins.
type Fit = (bool, i64);

/// A search term, and the matcher the picker uses.
struct Finder<'a> {
    matcher: &'a SkimMatcherV2,
    term: &'a str,
}

impl Finder<'_> {
    fn session(&self, name: &str) -> Option<(Fit, Vec<usize>)> {
        self.find(name, &[])
    }

    /// A pane by its label, its branch, the tab it is in, and its cwd. Only
    /// the label is lit up. A tab zellij named `Tab #N` is no name to find,
    /// and a cwd is its last two parts, as the agent list shows it: a whole
    /// path has the letters for a few typed ones to land on nearly any of it.
    fn pane(&self, label: &str, leaf: &Leaf) -> Option<(Fit, Vec<usize>)> {
        let tab = is_named_tab(&leaf.tab).then_some(leaf.tab.as_str());
        let cwd = leaf.cwd.as_deref().map(short_cwd);
        let branch = leaf.checkout.as_ref().map(|checkout| checkout.branch.as_str());
        self.find(label, &[branch, tab, cwd.as_deref()])
    }

    /// The term against what a row shows, which has the characters it lands
    /// on, and against what else the row goes by, which has a score only. A row
    /// need not have each.
    fn find(&self, shown: &str, also: &[Option<&str>]) -> Option<(Fit, Vec<usize>)> {
        let lit = self.matcher.fuzzy_indices(shown, self.term);
        let score = lit
            .iter()
            .map(|(score, _)| *score)
            .chain(
                also.iter().flatten().filter_map(|text| self.matcher.fuzzy_match(text, self.term)),
            )
            .max()?;
        let hits = lit.map(|(_, hits)| hits).unwrap_or_default();
        Some(((shown == self.term, score), hits))
    }
}

/// A name a person gave the agent, or else what the pane says it is doing.
/// Claude Code titles a pane `✳ Claude Code` until it has a topic, and some
/// never get one. A tab that someone renamed says more than that, and else the
/// directory the agent works in, which tells two such agents apart. A shell at
/// its prompt titles its pane with its cwd cut down to initials, and is shown
/// its cwd's last two parts instead, as the agent list shows a cwd:
/// `luneta/main`, where `~/P/m/l/main` hides the repository behind an `l`.
pub fn label(leaf: &Leaf, agent: Option<&AgentRow>) -> String {
    if let Some(name) = agent.and_then(AgentRow::chosen) {
        return name.to_owned();
    }
    let title = bare_title(&leaf.title);
    if let Some(cwd) = leaf.cwd.as_deref().filter(|cwd| abbreviates(shell_title(title), cwd)) {
        return short_cwd(cwd);
    }
    if !title.is_empty() && title != GENERIC_TITLE {
        return title.to_owned();
    }
    if is_named_tab(&leaf.tab) {
        return leaf.tab.clone();
    }
    if let Some(cwd) = agent.and_then(|agent| agent.cwd.as_deref()) {
        return short_cwd(cwd);
    }
    if title.is_empty() {
        return format!("pane {}", leaf.id);
    }
    title.to_owned()
}

const GENERIC_TITLE: &str = "Claude Code";

/// Whether a title is the cwd, as a shell writes it: whole, or with `~` for
/// home and any part but the last cut short, as fish's `~/P/m/l/main` is
/// `/home/you/Projects/misc/luneta/main`. Each part of the title begins the
/// part of the cwd in its place, counting from the end. A cwd the sidebar has
/// not caught up with since a `cd` is not the one the title writes, and the
/// title stays until it has.
fn abbreviates(title: &str, cwd: &str) -> bool {
    let (written, rooted) = if title.starts_with('/') {
        (title, true)
    } else if let Some(rest) =
        title.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        (rest, false)
    } else {
        return false;
    };
    let written: Vec<&str> = written.split('/').filter(|part| !part.is_empty()).collect();
    let parts: Vec<&str> = cwd.split('/').filter(|part| !part.is_empty()).collect();
    // `~` stands for a part or more.
    let fits = if rooted { written.len() == parts.len() } else { written.len() < parts.len() };
    let begins = |(written, part): (&&str, &&str)| part.starts_with(written);
    fits && written.last().is_some_and(|last| parts.last() == Some(last))
        && written.iter().rev().zip(parts.iter().rev()).all(begins)
}

/// What a shell is doing, as its title says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    /// It is back at its prompt.
    Prompt,
    /// It runs a command, as fish cut it.
    Running(String),
    /// A title fish did not write: an agent's, say, or a program's that
    /// titles the pane itself.
    Unknown,
}

/// What a shell is doing, as fish titles its pane: with its cwd at its
/// prompt, `~/P/m/l/main`, and while it runs a command, with the command cut
/// to twenty characters before the cwd, `cargo build --relea ~/P/m/l/main`.
/// The sidebar may not have caught up with a `cd` yet, so a title that is a
/// directory alone is the prompt, whichever it is.
pub fn doing(title: &str, cwd: Option<&str>) -> Doing {
    let title = title.trim();
    if title.chars().next().is_some_and(is_agent_glyph) {
        return Doing::Unknown;
    }
    let title = shell_title(title);
    if is_directory(title) || cwd.is_some_and(|cwd| abbreviates(title, cwd)) {
        return Doing::Prompt;
    }
    match title.rsplit_once(' ') {
        Some((command, cwd)) if is_directory(cwd) && !command.trim().is_empty() => {
            Doing::Running(command.trim().to_owned())
        },
        _ => Doing::Unknown,
    }
}

/// A shell's title without the host fish puts before it over SSH, as in
/// `[nixos] ~/P/m/l/main`: ten characters of its name at most, in brackets.
fn shell_title(title: &str) -> &str {
    let host = title.strip_prefix('[').and_then(|rest| rest.split_once("] "));
    let host = host.filter(|(host, _)| {
        !host.is_empty() && host.chars().count() <= 10 && !host.contains(char::is_whitespace)
    });
    host.map_or(title, |(_, rest)| rest)
}

/// Whether `text` is a directory as fish writes one in a title: from the
/// root or from `~`, and with no space in it.
fn is_directory(text: &str) -> bool {
    let rooted = text.starts_with('/') || text == "~" || text.starts_with("~/");
    rooted && !text.contains(char::is_whitespace)
}

/// The title without the glyph an agent leads it with: Claude Code's `✳` at
/// rest, and a turning frame while it works.
fn bare_title(title: &str) -> &str {
    let title = title.trim();
    let mut chars = title.chars();
    match chars.next() {
        Some(glyph) if is_agent_glyph(glyph) => chars.as_str().trim_start(),
        _ => title,
    }
}

fn is_agent_glyph(glyph: char) -> bool {
    matches!(glyph, '✳' | '✢' | '✶' | '✻' | '✽' | '·') || ('\u{2800}'..='\u{28ff}').contains(&glyph)
}

/// zellij names a tab `Tab #N` until someone renames it.
fn is_named_tab(tab: &str) -> bool {
    let unnamed = tab
        .strip_prefix("Tab #")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    !tab.trim().is_empty() && !unnamed
}

#[cfg(test)]
mod tests {
    use zellij_tile::prelude::PaneInfo;

    use super::*;
    use crate::agents::{Live, Seat};
    use crate::elapsed::Moment;
    use crate::shells::{Front, Said, Shells};
    use crate::places::Places;
    use crate::sessions::Selection;

    fn pane(id: u32, title: &str) -> PaneInfo {
        PaneInfo { id, title: title.to_owned(), is_selectable: true, ..PaneInfo::default() }
    }

    fn tab(position: usize, name: &str) -> TabInfo {
        TabInfo { position, name: name.to_owned(), ..TabInfo::default() }
    }

    fn manifest(tabs: &[(usize, Vec<PaneInfo>)]) -> PaneManifest {
        PaneManifest { panes: tabs.iter().cloned().collect() }
    }

    fn branch(name: &str, age: u64, panes: &[(u32, &str)]) -> Branch {
        Branch {
            name: name.to_owned(),
            age: Age::from_secs(age),
            panes: panes
                .iter()
                .map(|(id, title)| Leaf {
                    id: *id,
                    title: (*title).to_owned(),
                    tab: String::new(),
                    tab_id: None,
                    cwd: None,
                    runs: None,
                    checkout: None,
                    closes: Closes::Pane,
                })
                .collect(),
        }
    }

    /// claude-ps output for agents at `(session, pane, status)`, each sitting in
    /// the cwd `/w/<session>`, and the places that hold them.
    fn agents(seated: &[(&str, u32, &str)]) -> AgentSet {
        let json: Vec<String> = seated
            .iter()
            .enumerate()
            .map(|(pid, (session, pane, status))| {
                format!(
                    r#"{{"status":"{status}","status_age":4,"pid":{pid},"cwd":"/w/{session}",
                       "zellij":{{"session":"{session}","pane":"{pane}"}}}}"#
                )
            })
            .collect();
        let mut sessions: Vec<(&str, Vec<(u32, String)>)> = Vec::new();
        for (session, pane, _) in seated {
            let cwd = format!("/w/{session}");
            match sessions.iter_mut().find(|(name, _)| name == session) {
                Some((_, panes)) => panes.push((*pane, cwd)),
                None => sessions.push((session, vec![(*pane, cwd)])),
            }
        }
        let borrowed: Vec<(&str, Vec<(u32, &str)>)> = sessions
            .iter()
            .map(|(name, panes)| {
                (*name, panes.iter().map(|(id, cwd)| (*id, cwd.as_str())).collect())
            })
            .collect();
        let shaped: Vec<(&str, &[(u32, &str)])> =
            borrowed.iter().map(|(name, panes)| (*name, panes.as_slice())).collect();
        let places = Places::of(&shaped);

        let mut set = AgentSet::default();
        set.ingest(Some(0), format!("[{}]", json.join(",")).as_bytes(), b"", Moment::default());
        let live = Live::new(None, &[], &places, &[]);
        set.rebuild("", &live, None, Age::from_secs(0), Selection::SnapToTop);
        set
    }

    fn drawn(tree: &Tree) -> Vec<String> {
        tree.rows
            .iter()
            .map(|node| match node {
                Node::Session(node) => {
                    let fold = if node.folded { "▸" } else { "▾" };
                    match node.waiting {
                        0 => format!("{fold} {}", node.name),
                        n => format!("{fold} {} waiting:{n}", node.name),
                    }
                },
                Node::Pane(node) => format!(
                    "  {}{} {}",
                    if node.here { "›" } else { "" },
                    node.mark.as_ref().map_or("-", |mark| match mark {
                        Mark::Agent { status, .. } => status.word(),
                        Mark::Finished { .. } => "finished",
                        Mark::Ran { ok: None, .. } => "ran",
                        Mark::Ran { ok: Some(true), .. } => "passed",
                        Mark::Ran { ok: Some(false), .. } => "failed",
                        Mark::Running { .. } => "running",
                        Mark::Asking { .. } => "asking",
                    }),
                    node.label
                ),
            })
            .collect()
    }

    fn selected(tree: &Tree) -> Option<Place> {
        tree.rows.selected_row().map(Node::place)
    }

    #[test]
    fn sessions_keep_the_order_they_were_made_in_whatever_their_agents_do() {
        let branches = [
            branch("young", 60, &[(0, "✳ a")]),
            branch("old", 7200, &[(0, "✳ b")]),
            branch("middle", 600, &[(0, "✳ c")]),
        ];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &agents(&[("young", 0, "waiting")]), None, true);
        let sessions: Vec<&str> = tree
            .rows
            .iter()
            .filter(|node| matches!(node, Node::Session(_)))
            .map(Node::session)
            .collect();
        assert_eq!(sessions, ["old", "middle", "young"]);
    }

    #[test]
    fn each_pane_is_drawn_under_its_session_with_the_agent_in_it() {
        let branches = [branch("bipa", 100, &[(31, "✳ handoff-eng-11259"), (32, "~/w/bipa")])];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &agents(&[("bipa", 31, "waiting")]), None, true);
        assert_eq!(
            drawn(&tree),
            ["▾ bipa waiting:1", "  waiting handoff-eng-11259", "  - ~/w/bipa"]
        );
    }

    #[test]
    fn a_name_someone_chose_beats_the_title_of_the_pane() {
        let json = r#"[{"status":"busy","status_age":4,"pid":101,"cwd":"/w/s","name":"orca-copy",
            "name_source":"user","zellij":{"session":"s","pane":"0"}}]"#;
        let mut set = AgentSet::default();
        set.ingest(Some(0), json.as_bytes(), b"", Moment::default());
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let live = Live::new(None, &[], &places, &[]);
        set.rebuild("", &live, None, Age::from_secs(0), Selection::SnapToTop);

        let mut tree = Tree::default();
        tree.rebuild(&[branch("s", 1, &[(0, "⠂ Building the sidebar")])], None, &set, None, true);
        assert_eq!(drawn(&tree), ["▾ s", "  busy orca-copy"]);
    }

    #[test]
    fn agents_with_no_topic_and_no_tab_name_go_by_where_they_work() {
        let json = r#"[
            {"status":"busy","status_age":4,"pid":102,"name":"main-60","name_source":"derived",
             "cwd":"/w/luneta/main","zellij":{"session":"luneta","pane":"1"}},
            {"status":"idle","status_age":4,"pid":103,"name":"agents-tab-9a","name_source":"derived",
             "cwd":"/w/luneta/agents-tab","zellij":{"session":"luneta","pane":"4"}}]"#;
        let mut set = AgentSet::default();
        set.ingest(Some(0), json.as_bytes(), b"", Moment::default());
        let places = Places::of(&[("luneta", &[(1, "/w/luneta/main"), (4, "/w/luneta/agents-tab")])]);
        let live = Live::new(None, &[], &places, &[]);
        set.rebuild("", &live, None, Age::from_secs(0), Selection::SnapToTop);

        let branches = [branch("luneta", 1, &[(1, "✳ Claude Code"), (4, "✳ Claude Code")])];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, true);
        assert_eq!(drawn(&tree), ["▾ luneta", "  busy luneta/main", "  finished luneta/agents-tab"]);
    }

    #[test]
    fn a_title_loses_the_glyph_its_agent_leads_it_with() {
        assert_eq!(bare_title("✳ Floresta issue 1326"), "Floresta issue 1326");
        assert_eq!(bare_title("⠐ Floresta issue 1326"), "Floresta issue 1326");
        assert_eq!(bare_title("✻Thinking"), "Thinking");
        assert_eq!(bare_title("  ~/P/m/floresta "), "~/P/m/floresta");
        assert_eq!(bare_title("nvim ✳"), "nvim ✳");
        assert_eq!(bare_title("✳"), "");
    }

    #[test]
    fn a_renamed_tab_names_an_agent_that_has_no_topic_yet() {
        let leaf = |title: &str, tab: &str| Leaf {
            id: 7,
            title: title.to_owned(),
            tab: tab.to_owned(),
            tab_id: None,
            cwd: None,
            runs: None,
            checkout: None,
            closes: Closes::Pane,
        };
        assert_eq!(label(&leaf("✳ Claude Code", "liquid dual"), None), "liquid dual");
        assert_eq!(label(&leaf("✳ Claude Code", "Tab #13"), None), "Claude Code");
        assert_eq!(label(&leaf("✳ pix outflow", "liquid dual"), None), "pix outflow");
        assert_eq!(label(&leaf("", "Tab #2"), None), "pane 7");
        assert_eq!(label(&leaf("", "notes"), None), "notes");
        assert!(!is_named_tab("Tab #4"));
        assert!(is_named_tab("Tab #"));
        assert!(is_named_tab("Tab #4b"));
        assert!(!is_named_tab("  "));
    }

    #[test]
    fn a_shell_at_its_prompt_is_named_for_its_cwd() {
        let leaf = |title: &str, cwd: Option<&str>| Leaf {
            id: 7,
            title: title.to_owned(),
            tab: String::new(),
            tab_id: None,
            cwd: cwd.map(str::to_owned),
            runs: None,
            checkout: None,
            closes: Closes::Pane,
        };
        let main = Some("/home/you/Projects/misc/luneta/main");
        assert_eq!(label(&leaf("~/P/m/l/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("[nixos] ~/P/m/l/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("~/Projects/misc/luneta/main", main), None), "luneta/main");
        assert_eq!(label(&leaf("/home/you/Projects/misc/luneta/main", main), None), "luneta/main");
        // Before the sidebar is told the cwd, and after a `cd` it has not yet
        // caught up with.
        assert_eq!(label(&leaf("~/P/m/l/main", None), None), "~/P/m/l/main");
        let before = Some("/home/you/Projects/misc/luneta");
        assert_eq!(label(&leaf("~/P/m/l/main", before), None), "~/P/m/l/main");
        assert_eq!(label(&leaf("nvim ~/P/m/l/main", main), None), "nvim ~/P/m/l/main");
        assert_eq!(label(&leaf("~", Some("/home/you")), None), "~");
    }

    #[test]
    fn a_title_abbreviates_a_cwd_only_part_for_part() {
        let main = "/home/you/Projects/misc/luneta/main";
        assert!(abbreviates("~/P/m/l/main", main));
        assert!(abbreviates("~/l/main", main));
        assert!(abbreviates("~/main", main));
        assert!(abbreviates("~/.c/fish", "/home/you/.config/fish"));
        assert!(abbreviates("/etc/nixos", "/etc/nixos"));
        assert!(abbreviates("/h/y/P/m/l/main/", main));
        // A part that begins another, a last part cut short, and too many or
        // too few parts for what `~` and `/` stand for.
        assert!(!abbreviates("~/P/m/x/main", main));
        assert!(!abbreviates("~/P/m/l/ma", main));
        assert!(!abbreviates("/P/m/l/main", main));
        assert!(!abbreviates("~/home/you/Projects/misc/luneta/main", main));
        // Someone else's home, home itself, the root, and no path at all.
        assert!(!abbreviates("~luneta/main", main));
        assert!(!abbreviates("~", "/home/you"));
        assert!(!abbreviates("/", "/"));
        assert!(!abbreviates("main", main));
    }

    #[test]
    fn a_shell_s_title_says_whether_it_runs_a_command() {
        let main = Some("/home/you/Projects/misc/luneta/main");
        let running = |command: &str| Doing::Running(command.to_owned());
        for (title, cwd, said) in [
            ("~/P/m/l/main", main, Doing::Prompt),
            ("~", Some("/home/you"), Doing::Prompt),
            ("/etc/nixos", None, Doing::Prompt),
            // A `cd` the sidebar has not caught up with.
            ("~/P/m/l/main/src", main, Doing::Prompt),
            ("cargo build --relea ~/P/m/l/main", main, running("cargo build --relea")),
            ("nvim ~/notes.md ~/P/m/l/main", main, running("nvim ~/notes.md")),
            ("/usr/bin/make ~/P/m/l/main", None, running("/usr/bin/make")),
            ("git commit -m 'fix  ~/P/m/l/main", main, running("git commit -m 'fix")),
            // Over SSH, fish puts the host first.
            ("[nixos] ~/P/m/l/main", main, Doing::Prompt),
            ("[nixos] cargo test ~/P/m/l/main", main, running("cargo test")),
            // An agent, one that names a directory too, and what fish did
            // not write.
            ("✳ Claude Code", main, Doing::Unknown),
            ("⠐ Look at ~/P/m/l/main", main, Doing::Unknown),
            ("Pane #3", None, Doing::Unknown),
            ("fish", main, Doing::Unknown),
            ("", main, Doing::Unknown),
        ] {
            assert_eq!(doing(title, cwd), said, "{title:?}");
        }
    }

    #[test]
    fn panes_come_in_tab_order_and_only_the_ones_you_can_go_to() {
        let plugin = PaneInfo { is_plugin: true, ..pane(0, "zellij:tab-bar") };
        let hidden = PaneInfo { is_suppressed: true, ..pane(9, "hidden") };
        let floating = PaneInfo { is_floating: true, ..pane(1, "floating") };
        let bar = PaneInfo { is_selectable: false, ..pane(8, "bar") };
        let placeholder =
            PaneInfo { terminal_command: Some("tail -f /dev/null".to_owned()), ..pane(7, "tail") };
        let manifest = manifest(&[
            (1, vec![pane(2, "second tab")]),
            (
                0,
                vec![
                    floating,
                    plugin,
                    pane(5, "tiled, later"),
                    pane(3, "tiled"),
                    hidden,
                    bar,
                    placeholder,
                ],
            ),
        ]);
        let places = Places::of(&[("s", &[(3, "/w/s"), (2, "/w/logs")]), ("t", &[(5, "/w/t")])]);
        let mut checkouts = Checkouts::default();
        checkouts.ingest("/w/s".to_owned(), Some(0), b"# branch.head main\n", b"");
        checkouts.ingest("/w/t".to_owned(), Some(0), b"# branch.head fold\n", b"");
        let tabs = [tab(0, "code"), tab(1, "logs")];
        let leaves = leaves("s", &tabs, &manifest, &places, &checkouts, &[]);
        let seen: Vec<(u32, &str, &str, Option<&str>)> = leaves
            .iter()
            .map(|leaf| (leaf.id, leaf.title.as_str(), leaf.tab.as_str(), leaf.cwd.as_deref()))
            .collect();
        assert_eq!(
            seen,
            [
                (3, "tiled", "code", Some("/w/s")),
                (5, "tiled, later", "code", None),
                (1, "floating", "code", None),
                (2, "second tab", "logs", Some("/w/logs")),
            ]
        );
        // Pane 5's directory is session `t`'s, not this one's.
        let branches: Vec<Option<&str>> = leaves
            .iter()
            .map(|leaf| leaf.checkout.as_ref().map(|checkout| checkout.branch.as_str()))
            .collect();
        assert_eq!(branches, [Some("main"), None, None, None]);
    }

    /// A pane says what closing it closes: itself beside another, its tab
    /// alone in it, and its session alone in the session's only tab.
    #[test]
    fn a_pane_says_what_closing_it_closes() {
        let two = manifest(&[(0, vec![pane(2, "a"), pane(3, "b")]), (1, vec![pane(4, "c")])]);
        let one = manifest(&[(0, vec![pane(4, "c")])]);
        let (places, checkouts) = (Places::default(), Checkouts::default());
        let closes = |manifest: &PaneManifest, tabs: &[TabInfo]| {
            let leaves = leaves("s", tabs, manifest, &places, &checkouts, &[]);
            leaves.iter().map(|leaf| (leaf.id, leaf.closes)).collect::<Vec<_>>()
        };
        let tabs = [tab(0, "code"), tab(1, "logs")];
        let of_two = [(2, Closes::Pane), (3, Closes::Pane), (4, Closes::Tab)];
        assert_eq!(closes(&two, &tabs), of_two);
        assert_eq!(closes(&one, &tabs[..1]), [(4, Closes::Session)]);
    }

    /// A pane that moved keeps its branch until git answers for where it
    /// works now. One its session left out of an answer keeps its directory,
    /// and shows what git says is checked out there. Only git saying there is
    /// no branch takes it away.
    #[test]
    fn a_pane_keeps_its_branch_until_git_answers_for_where_it_works_now() {
        let manifest = manifest(&[(0, vec![pane(1, "fish"), pane(2, "fish"), pane(3, "fish")])]);
        let tabs = [tab(0, "code")];
        let mut checkouts = Checkouts::default();
        checkouts.ingest("/w/repo".to_owned(), Some(0), b"# branch.head main\n", b"");
        let places = Places::of(&[("s", &[(1, "/w/repo"), (2, "/w/repo"), (3, "/w/repo")])]);
        let before = leaves("s", &tabs, &manifest, &places, &checkouts, &[]);

        // Pane 1 went down into `src`, pane 2 out of the repository, and pane
        // 3's directory is missing from the session's answer.
        checkouts.ingest("/w/plain".to_owned(), Some(128), b"", b"");
        let places = Places::of(&[("s", &[(1, "/w/repo/src"), (2, "/w/plain")])]);
        let after = leaves("s", &tabs, &manifest, &places, &checkouts, &before);
        let branches: Vec<Option<&str>> = after
            .iter()
            .map(|leaf| leaf.checkout.as_ref().map(|checkout| checkout.branch.as_str()))
            .collect();
        assert_eq!(branches, [Some("main"), None, Some("main")]);

        // The repository changes branch, and pane 3, still left out, shows it.
        checkouts.ingest("/w/repo".to_owned(), Some(0), b"# branch.head fold\n", b"");
        let again = leaves("s", &tabs, &manifest, &places, &checkouts, &after);
        assert_eq!(again[2].cwd.as_deref(), Some("/w/repo"));
        assert_eq!(again[2].checkout.as_ref().map(|c| c.branch.as_str()), Some("fold"));
    }

    /// The panes of session `s` as `zellij action list-panes` lists them, each
    /// in `/w/s`, by its id and what its terminal runs in the foreground.
    fn listed(panes: &[(u32, Option<&str>)]) -> Places {
        let json: Vec<String> = panes
            .iter()
            .map(|(id, command)| {
                let command = command.map_or("null".to_owned(), |command| format!("{command:?}"));
                let pane = format!(r#""id":{id},"is_plugin":false,"pane_cwd":"/w/s""#);
                format!(r#"{{{pane},"pane_command":{command}}}"#)
            })
            .collect();
        let mut places = Places::default();
        places.ingest("s".to_owned(), Some(0), format!("[{}]", json.join(",")).as_bytes());
        places
    }

    /// A pane runs Claude Code while claude-ps finds an agent in it, whatever
    /// its terminal has in front, and else the binary its terminal has in
    /// front, as its session says. A pane its session says nothing of runs
    /// nothing luneta knows.
    #[test]
    fn a_pane_runs_its_agent_or_else_what_its_terminal_has_in_front() {
        let manifest = manifest(&[(0, (1..=4).map(|id| pane(id, "fish")).collect())]);
        let places = listed(&[
            (1, Some("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js")),
            (2, Some("/run/current-system/sw/bin/nvim src/tree.rs")),
            (3, Some("fish")),
            (4, None),
        ]);
        let leaves = leaves("s", &[tab(0, "code")], &manifest, &places, &Checkouts::default(), &[]);
        let branches = [Branch { name: "s".to_owned(), age: Age::from_secs(1), panes: leaves }];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &agents(&[("s", 1, "busy")]), None, true);
        let programs: Vec<(u32, Option<&str>)> = tree
            .rows
            .iter()
            .filter_map(|node| match node {
                Node::Pane(node) => Some((node.id, node.program())),
                Node::Session(_) => None,
            })
            .collect();
        let ran = [(1, Some("claude")), (2, Some("nvim")), (3, Some("fish")), (4, None)];
        assert_eq!(programs, ran);
    }

    /// A pane keeps what it runs while its session leaves it out of an
    /// answer, or answers with nothing for it, as zellij does for a terminal
    /// that does not say in time, and takes what its session says runs there
    /// now, even a command with no binary to go by, as a shell `sudo` starts.
    #[test]
    fn a_pane_keeps_what_it_runs_until_its_session_says_what_runs_there_now() {
        let manifest = manifest(&[(0, (1..=4).map(|id| pane(id, "fish")).collect())]);
        let tabs = [tab(0, "code")];
        let checkouts = Checkouts::default();
        let places = listed(&[
            (1, Some("nvim")),
            (2, Some("htop")),
            (3, Some("cargo build")),
            (4, Some("nvim")),
        ]);
        let before = leaves("s", &tabs, &manifest, &places, &checkouts, &[]);

        let places = listed(&[(1, Some("fish")), (2, None), (4, Some("sudo -i"))]);
        let after = leaves("s", &tabs, &manifest, &places, &checkouts, &before);
        let runs: Vec<Option<&str>> = after.iter().map(|leaf| leaf.runs.as_deref()).collect();
        assert_eq!(runs, [Some("fish"), Some("htop"), Some("cargo"), None]);
    }

    #[test]
    fn a_folded_session_hides_its_panes_and_keeps_its_count() {
        let branches = [
            branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]),
            branch("b", 1, &[(0, "✳ three")]),
        ];
        let set = agents(&[("a", 0, "waiting"), ("a", 1, "busy"), ("b", 0, "idle")]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, false);
        assert!(tree.fold(true));
        assert!(!tree.fold(true));
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(drawn(&tree), ["▸ a waiting:1", "▾ b", "  finished three"]);
        let Node::Session(a) = &tree.rows[0] else { panic!("a session comes first") };
        assert!(a.loudest.as_ref().is_some_and(Mark::is_waiting));

        assert!(tree.toggle());
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.rows.len(), 5);
    }

    /// A pane shows what finished in it since you were last there: an agent,
    /// and a command its shell ran, which names its row until you go there.
    /// The pane you are in shows none of it, as you see it. Folded, a session
    /// shows what is new in it over an agent at work, and the header counts
    /// what is new in every session.
    #[test]
    fn a_pane_shows_what_finished_in_it_since_you_were_last_there() {
        let at = |secs: u64| Moment::from_millis(secs * 1000);
        let set = agents(&[("a", 0, "idle"), ("a", 2, "busy"), ("b", 4, "idle"), ("c", 5, "idle")]);
        let titled = |shell: &str| {
            [
                branch("a", 9, &[(0, "✳ one"), (1, shell), (2, "✳ two")]),
                branch("b", 5, &[(4, "✳ three")]),
                branch("c", 1, &[(5, "✳ four")]),
            ]
        };
        let mut tree = Tree::default();
        tree.news.watch(&titled("make ~/w/a"), &set, None, |_| None, at(10));
        let branches = titled("~/w/a");
        // You stay in `b`'s pane 4, then go to `c`'s pane 5.
        for (here, now) in [(("b", 4), 20), (("b", 4), 21), (("c", 5), 22)] {
            tree.news.watch(&branches, &set, Some(here), |_| None, at(now));
        }
        tree.rebuild(&branches, Some("c"), &set, Some(5), true);
        assert_eq!(
            drawn(&tree),
            [
                "▾ a",
                "  finished one",
                "  ran make",
                "  busy two",
                "▾ b",
                "  idle three",
                "▾ c",
                "  ›idle four",
            ]
        );
        assert_eq!(tree.summary, Summary { sessions: 3, waiting: 0, unseen: 2, busy: 1 });

        assert!(tree.toggle_row(0));
        tree.rebuild(&branches, Some("c"), &set, Some(5), true);
        let Node::Session(a) = &tree.rows[0] else { panic!("a session comes first") };
        assert!(a.folded && a.loudest.as_ref().is_some_and(Mark::is_new));
    }

    /// The commands the shells' hook says run, or ended, name their rows. One
    /// that waits for you to type counts with the agents that wait for you,
    /// on its session's row too, and one at work with the agents at work.
    /// Folded, its session shows the one that waits for you over the rest.
    #[test]
    fn a_command_counts_with_the_agents_it_looks_like() {
        let at = |secs: u64| Moment::from_millis(secs * 1000);
        let branches = [branch("a", 100, &[(1, "~/w/a"), (2, "~/w/a"), (3, "~/w/a"), (4, "~/w/a")])];
        let tty = |n: u32| format!("/dev/pts/{n}");
        let going = |command: &str, n: u32| Said::Going {
            command: command.to_owned(),
            since: at(10),
            tty: tty(n),
        };
        let mut tree = Tree::default();
        tree.news.shells = Shells::of(
            vec![
                ("a", 1, at(0), going("cargo build", 1)),
                ("a", 2, at(0), going("sudo true", 2)),
                ("a", 3, at(0), going("nvim", 3)),
                (
                    "a",
                    4,
                    at(0),
                    Said::Ended {
                        command: "make".to_owned(),
                        at: at(15),
                        status: 0,
                        took: Held::from_secs(5),
                    },
                ),
            ],
            vec![(&tty(2), Front::Asking), (&tty(3), Front::Interactive)],
        );
        let set = agents(&[]);
        tree.news.watch(&branches, &set, None, |_| None, at(20));
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(
            drawn(&tree),
            ["▾ a waiting:1", "  running cargo build", "  asking sudo true", "  - ~/w/a", "  passed make"]
        );
        assert_eq!(tree.summary, Summary { sessions: 1, waiting: 1, unseen: 1, busy: 1 });

        assert!(tree.toggle_row(0));
        tree.rebuild(&branches, None, &set, None, false);
        let Node::Session(a) = &tree.rows[0] else { panic!("a session comes first") };
        assert!(a.loudest.as_ref().is_some_and(Mark::is_waiting));
    }

    /// The header sums up every agent, so neither a fold nor a search takes
    /// one out of its count.
    #[test]
    fn the_summary_counts_the_agents_a_fold_or_a_search_hides() {
        let branches = [
            branch("a", 9, &[(0, "✳ one"), (1, "✳ two"), (2, "~/w/a")]),
            branch("b", 1, &[(0, "✳ three")]),
        ];
        let set = agents(&[("a", 0, "waiting"), ("a", 1, "busy"), ("b", 0, "idle")]);
        let all = Summary { sessions: 2, waiting: 1, unseen: 1, busy: 1 };
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.summary, all);
        assert!(tree.fold(true));
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.summary, all);
        tree.search_for("three".to_owned());
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(drawn(&tree), ["▾ b", "  finished three"]);
        assert_eq!(tree.summary, all);
    }

    #[test]
    fn the_cursor_follows_you_until_the_sidebar_has_the_focus() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        let mut tree = Tree::default();

        tree.rebuild(&branches, Some("a"), &set, Some(1), true);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 1)));
        assert_eq!(drawn(&tree)[2], "  ›- two");

        tree.rows.move_selection(2);
        tree.rebuild(&branches, Some("a"), &set, Some(1), false);
        assert_eq!(selected(&tree), Some(Place::Pane("b".to_owned(), 4)));

        tree.rebuild(&branches, Some("a"), &set, Some(0), true);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));
    }

    /// The view the wheel left holds while the cursor stays on its row, and
    /// ends once you go to another pane, which the cursor follows. It does not
    /// come back when you come back.
    #[test]
    fn the_view_the_wheel_left_ends_with_the_cursor_on_another_row() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")])];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, Some("a"), &set, Some(1), true);
        tree.scroll_to(0);

        tree.rebuild(&branches, Some("a"), &set, Some(1), true);
        assert_eq!(tree.top(), Some(0));

        tree.rebuild(&branches, Some("a"), &set, Some(0), true);
        assert_eq!(tree.top(), None);
        tree.rebuild(&branches, Some("a"), &set, Some(1), true);
        assert_eq!(tree.top(), None);
    }

    /// Panes that open and close above the view the wheel left keep the row
    /// at its top there, and a view whose top row goes ends, as though the
    /// cursor had gone.
    #[test]
    fn the_view_the_wheel_left_holds_its_top_row_as_rows_come_and_go_above_it() {
        let branches = |a: &[(u32, &str)], b: &[(u32, &str)]| {
            [branch("a", 9, a), branch("b", 5, b), branch("c", 1, &[(4, "z")])]
        };
        let set = agents(&[]);
        let top = |tree: &Tree| tree.top().map(|at| tree.rows[at].place());
        let mut tree = Tree::default();
        tree.rebuild(&branches(&[(0, "x"), (1, "y")], &[(2, "w")]), Some("c"), &set, Some(4), true);
        tree.scroll_to(3);
        let b = Some(Place::Session("b".to_owned()));
        assert_eq!(top(&tree), b);

        for a in [&[(0, "x"), (1, "y"), (6, "v")][..], &[(0, "x")]] {
            tree.rebuild(&branches(a, &[(2, "w")]), Some("c"), &set, Some(4), true);
            assert_eq!(top(&tree), b, "{a:?}");
        }

        tree.scroll_to(3);
        assert_eq!(top(&tree), Some(Place::Pane("b".to_owned(), 2)));
        tree.rebuild(&branches(&[(0, "x")], &[]), Some("c"), &set, Some(4), true);
        assert_eq!(tree.top(), None);
        tree.rebuild(&branches(&[(0, "x")], &[(2, "w")]), Some("c"), &set, Some(4), true);
        assert_eq!(tree.top(), None);
    }

    #[test]
    fn the_row_the_cursor_was_on_as_the_focus_left_is_kept_once() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, Some("a"), &set, Some(0), false);
        tree.rows.move_selection(4);
        tree.rebuild(&branches, Some("a"), &set, Some(0), false);
        assert_eq!(tree.take_left(), None);

        // The cursor follows you out, and a second rebuild without the focus
        // keeps the row it left from.
        tree.rebuild(&branches, Some("a"), &set, Some(0), true);
        tree.rebuild(&branches, Some("a"), &set, Some(0), true);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));
        assert_eq!(tree.take_left(), Some(Place::Pane("b".to_owned(), 4)));
        assert_eq!(tree.take_left(), None);
    }

    /// `j` and `k` go from pane to pane, over the row of the session that
    /// heads them, and stop on a session's row only where it lists none of
    /// its panes: folded, or with none.
    #[test]
    fn the_cursor_steps_from_pane_to_pane_and_onto_a_session_that_shows_none() {
        let branches = [
            branch("a", 9, &[(0, "x"), (1, "y")]),
            branch("b", 5, &[(4, "z")]),
            branch("c", 3, &[(7, "w")]),
            branch("d", 1, &[]),
        ];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, false);
        tree.rows.move_selection(3);
        assert!(tree.fold(true));
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(drawn(&tree), ["▾ a", "  - x", "  - y", "▾ b", "  - z", "▸ c", "▾ d"]);

        let pane = |session: &str, id| Some(Place::Pane(session.to_owned(), id));
        let session = |name: &str| Some(Place::Session(name.to_owned()));
        assert!(tree.rows.select(1));
        let down = [pane("a", 1), pane("b", 4), session("c"), session("d"), session("d")];
        let up = [session("c"), pane("b", 4), pane("a", 1), pane("a", 0), pane("a", 0)];
        let steps = down.map(|place| (1, place)).into_iter().chain(up.map(|place| (-1, place)));
        for (delta, place) in steps {
            tree.rows.move_selection(delta);
            assert_eq!(selected(&tree), place, "{delta}");
        }
    }

    /// A pane in a folded session unfolds it, and a session that lists its
    /// panes is lit on the first.
    #[test]
    fn a_row_the_picker_hands_over_holds_the_cursor_until_the_sidebar_has_the_focus() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        let four = Place::Pane("b".to_owned(), 4);
        for (handed, folded) in [(four.clone(), true), (Place::Session("b".to_owned()), false)] {
            let mut tree = Tree::default();
            tree.rebuild(&branches, Some("a"), &set, Some(0), false);
            if folded {
                tree.rows.move_selection(2);
                assert!(tree.fold(true));
            }
            tree.rebuild(&branches, Some("a"), &set, Some(0), true);

            tree.hand(handed.clone());
            for focused in [false, false, true] {
                tree.rebuild(&branches, Some("a"), &set, Some(0), !focused);
                assert_eq!(selected(&tree), Some(four.clone()), "{handed:?}, focused: {focused}");
            }
            tree.rebuild(&branches, Some("a"), &set, Some(0), true);
            assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)), "{handed:?}");
        }
    }

    #[test]
    fn cycle_goes_down_the_panes_listed_and_back_to_the_first() {
        let branches = [
            branch("a", 9, &[(0, "x"), (1, "y")]),
            branch("b", 5, &[(4, "z")]),
            branch("c", 1, &[(7, "w")]),
        ];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &agents(&[]), None, false);
        tree.rows.move_selection(5);
        assert!(tree.fold(true));
        tree.rebuild(&branches, None, &agents(&[]), None, false);
        let pane = |session: &str, id| Some(Place::Pane(session.to_owned(), id));
        for (here, after) in [
            (pane("a", 0), pane("a", 1)),
            (pane("a", 1), pane("b", 4)),
            // From the last, past the folded session's.
            (pane("b", 4), pane("a", 0)),
            (pane("c", 7), pane("a", 0)),
            (None, pane("a", 0)),
        ] {
            assert_eq!(tree.after(here.as_ref()), after, "{here:?}");
        }
    }

    #[test]
    fn folding_the_session_you_are_on_leaves_the_cursor_on_the_session() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, Some("a"), &set, Some(1), true);

        assert!(tree.fold(true));
        tree.rebuild(&branches, Some("a"), &set, Some(1), false);
        assert_eq!(selected(&tree), Some(Place::Session("a".to_owned())));

        tree.rebuild(&branches, Some("a"), &set, Some(1), true);
        assert_eq!(selected(&tree), Some(Place::Session("a".to_owned())));
    }

    #[test]
    fn a_session_that_ends_takes_its_rows_and_not_the_cursor_with_it() {
        let set = agents(&[]);
        let mut tree = Tree::default();
        let both = [branch("a", 9, &[(0, "x")]), branch("b", 1, &[(0, "y")])];
        tree.rebuild(&both, None, &set, None, false);
        tree.rows.move_selection(1);
        assert_eq!(selected(&tree), Some(Place::Pane("b".to_owned(), 0)));

        tree.rebuild(&[branch("a", 9, &[(0, "x")])], None, &set, None, false);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));

        tree.rebuild(&[], None, &set, None, false);
        assert!(tree.rows.is_empty());
        assert_eq!(selected(&tree), None);
    }

    #[test]
    fn an_agent_in_a_session_that_is_not_drawn_is_not_drawn() {
        let mut tree = Tree::default();
        let gone = agents(&[("gone", 0, "waiting")]);
        tree.rebuild(&[branch("a", 9, &[(0, "x")])], None, &gone, None, true);
        assert_eq!(drawn(&tree), ["▾ a", "  - x"]);
    }

    /// Once what a delete key closed is gone, the cursor is on the pane after
    /// it, or else the one before, and not back at the top: a pane's row, a
    /// tab's panes, or a whole session. Moved away first, it stays where it
    /// was moved.
    #[test]
    fn the_cursor_goes_past_what_a_close_key_closed_once_it_is_gone() {
        let set = agents(&[]);
        let tabbed = |name: &str, age: u64, panes: &[(u32, usize)]| {
            let mut branch =
                branch(name, age, &panes.iter().map(|(id, _)| (*id, "x")).collect::<Vec<_>>());
            for (leaf, (_, tab)) in branch.panes.iter_mut().zip(panes) {
                leaf.tab_id = Some(TabId(*tab));
            }
            branch
        };
        let a = |panes: &[(u32, usize)]| tabbed("a", 9, panes);
        let b = || tabbed("b", 1, &[(4, 20)]);
        let here = |name: &str| Seat::Here(name.to_owned());
        let pane = |session: &str, id| Some(Place::Pane(session.to_owned(), id));
        for (act, left, at) in [
            (Act::Pane(here("a"), 1), vec![a(&[(0, 10), (2, 11)]), b()], pane("a", 2)),
            (Act::Tab(here("a"), TabId(10)), vec![a(&[(2, 11)]), b()], pane("a", 2)),
            (Act::Kill("a".to_owned()), vec![b()], pane("b", 4)),
        ] {
            let mut tree = Tree::default();
            let all = [a(&[(0, 10), (1, 10), (2, 11)]), b()];
            tree.rebuild(&all, None, &set, None, false);
            assert!(tree.rows.select(2));
            tree.closing(&act);
            tree.rebuild(&all, None, &set, None, false);
            assert_eq!(selected(&tree), pane("a", 1), "{act:?} before it is gone");
            tree.rebuild(&left, None, &set, None, false);
            assert_eq!(selected(&tree), at, "{act:?}");
        }
        let mut tree = Tree::default();
        let all = [a(&[(0, 10), (1, 10)]), b()];
        tree.rebuild(&all, None, &set, None, false);
        assert!(tree.rows.select(2));
        tree.closing(&Act::Pane(here("a"), 1));
        assert!(tree.rows.select(4));
        tree.rebuild(&[a(&[(0, 10)]), b()], None, &set, None, false);
        assert_eq!(selected(&tree), pane("b", 4));
    }

    /// A session found by its name keeps all its panes, the cursor on the one
    /// of them that matches best, or else on the first, though the name
    /// matches better. A pane that matches as well as its session's name is
    /// the best match.
    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        let branches = [
            branch("bipa", 300, &[(31, "✳ a-p-i-x"), (39, "✳ dda-bank-slip"), (47, "✳ pix outflow")]),
            branch("notes", 200, &[(0, "~/Documents")]),
            branch("misc", 100, &[(3, "✳ luneta")]),
            branch("zed", 50, &[(5, "fish"), (6, "z-e-d"), (7, "logs")]),
            branch("yak", 40, &[(8, "fish"), (9, "yak")]),
        ];
        let set = agents(&[]);
        for (term, rows, best) in [
            (
                "pix",
                &["▾ bipa", "  - a-p-i-x", "  - pix outflow"][..],
                Place::Pane("bipa".to_owned(), 47),
            ),
            ("note", &["▾ notes", "  - ~/Documents"][..], Place::Pane("notes".to_owned(), 0)),
            (
                "zed",
                &["▾ zed", "  - fish", "  - z-e-d", "  - logs"][..],
                Place::Pane("zed".to_owned(), 6),
            ),
            ("yak", &["▾ yak", "  - fish", "  - yak"][..], Place::Pane("yak".to_owned(), 9)),
        ] {
            let mut tree = Tree::default();
            tree.rebuild(&branches, None, &set, None, false);
            tree.search_for(term.to_owned());
            tree.rebuild(&branches, None, &set, None, false);
            assert_eq!(drawn(&tree), rows, "{term}");
            assert_eq!(selected(&tree), Some(best), "{term}");
        }
    }

    #[test]
    fn a_pane_a_search_finds_unfolds_its_session_until_the_search_closes() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        // Closed by hand, or by the sidebar losing the focus: a rebuild that
        // follows you out of it.
        for lost_focus in [false, true] {
            let mut tree = Tree::default();
            tree.rebuild(&branches, None, &set, None, false);
            assert!(tree.fold(true));

            tree.search_for("two".to_owned());
            tree.rebuild(&branches, None, &set, None, false);
            assert_eq!(drawn(&tree), ["▾ a", "  - two"]);

            if !lost_focus {
                assert!(tree.close_search());
            }
            tree.rebuild(&branches, None, &set, None, lost_focus);
            assert_eq!(tree.search(), None, "lost focus: {lost_focus}");
            assert_eq!(drawn(&tree), ["▸ a", "▾ b", "  - x"], "lost focus: {lost_focus}");
            assert_eq!(selected(&tree), Some(Place::Session("a".to_owned())));
        }
    }

    #[test]
    fn a_pane_is_found_by_its_branch_tab_and_cwd_but_lit_only_in_its_label() {
        let leaf = |id: u32, tab: &str, cwd: Option<&str>, branch: Option<&str>| Leaf {
            id,
            title: "nvim".to_owned(),
            tab: tab.to_owned(),
            tab_id: None,
            cwd: cwd.map(str::to_owned),
            runs: None,
            checkout: branch
                .map(|branch| Checkout { branch: branch.to_owned(), status: String::new() }),
            closes: Closes::Pane,
        };
        let branches = [Branch {
            name: "s".to_owned(),
            age: Age::from_secs(1),
            panes: vec![
                leaf(0, "notes", Some("/w/a"), None),
                leaf(1, "Tab #2", Some("/home/lorenzo/Projects/misc/luneta"), None),
                leaf(2, "", None, Some("feature/sidebar")),
            ],
        }];
        let found = |term: &str| {
            let mut tree = Tree::default();
            tree.search_for(term.to_owned());
            tree.rebuild(&branches, None, &agents(&[]), None, false);
            tree.rows
                .iter()
                .filter_map(|node| match node {
                    Node::Pane(pane) => Some((pane.id, pane.hits.clone())),
                    Node::Session(_) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(found("notes"), [(0, vec![])]);
        assert_eq!(found("luneta"), [(1, vec![])]);
        assert_eq!(found("sidebar"), [(2, vec![])]);
        assert_eq!(found("tab"), []);
        assert_eq!(found("loren"), []);
        assert_eq!(found("vi"), [(0, vec![1, 2]), (1, vec![1, 2]), (2, vec![1, 2])]);
    }

    #[test]
    fn the_cursor_goes_to_the_best_match_only_when_the_term_changes() {
        let all = [branch("a", 9, &[(0, "✳ pix one"), (1, "✳ two"), (2, "✳ p-i-x")])];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&all, None, &set, None, false);
        tree.rows.move_selection(1);

        tree.search_for(String::new());
        tree.rebuild(&all, None, &set, None, false);
        assert_eq!(tree.rows.len(), 4);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 1)));

        tree.search_for("pix".to_owned());
        tree.rebuild(&all, None, &set, None, false);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));

        tree.rows.move_selection(1);
        tree.rebuild(&all, None, &set, None, false);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 2)));

        let fewer = [branch("a", 9, &[(0, "✳ pix one"), (1, "✳ two")])];
        tree.rebuild(&fewer, None, &set, None, false);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));
    }

    #[test]
    fn panes_of_one_id_in_two_sessions_are_two_rows() {
        let branches = [branch("a", 9, &[(0, "✳ one")]), branch("b", 1, &[(0, "✳ two")])];
        let mut tree = Tree::default();
        let set = agents(&[("a", 0, "idle"), ("b", 0, "busy")]);
        tree.rebuild(&branches, Some("b"), &set, Some(0), true);
        assert_eq!(drawn(&tree), ["▾ a", "  finished one", "▾ b", "  ›busy two"]);
        assert_eq!(selected(&tree), Some(Place::Pane("b".to_owned(), 0)));
    }
}
