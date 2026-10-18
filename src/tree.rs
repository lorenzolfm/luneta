//! The sidebar's model: every live session, the panes in it, and the agent in
//! a pane where there is one.
//!
//! Sessions keep their place — oldest first, the order `zellij list-sessions`
//! prints — so a row never moves under the eye or the mouse. Saying which agent
//! wants you is the job of the glyph and of the `next` key, not of the order.
//! A search hides the rows that do not match and leaves the rest in that
//! order; the cursor, not the order, says which matches best.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use zellij_tile::prelude::{PaneInfo, PaneManifest, TabInfo};

use crate::agents::{self, AgentRow, AgentSet, Status};
use crate::cursor::Cursor;
use crate::dirs::short_cwd;
use crate::elapsed::{Age, Held};
use crate::places::Places;

/// What a placeholder runs: the column each tab keeps for the sidebar, which
/// the dock opens it into. Nothing anyone goes to.
pub const PLACEHOLDER: [&str; 3] = ["tail", "-f", "/dev/null"];

pub fn is_placeholder(pane: &PaneInfo) -> bool {
    !pane.is_plugin
        && pane.terminal_command.as_deref().is_some_and(|c| c.split(' ').eq(PLACEHOLDER))
}

/// A live session and its panes, as the sidebar is handed it.
pub struct Branch {
    pub name: String,
    pub age: Age,
    pub panes: Vec<Leaf>,
}

/// A terminal pane, the name of the tab that holds it, and the directory it
/// works in, once its session has said.
pub struct Leaf {
    pub id: u32,
    pub title: String,
    pub tab: String,
    pub cwd: Option<String>,
}

impl Branch {
    pub fn new(
        name: String,
        age: Age,
        tabs: &[TabInfo],
        manifest: &PaneManifest,
        places: &Places,
    ) -> Self {
        let panes = leaves(&name, tabs, manifest, places);
        Branch { name, age, panes }
    }
}

/// The panes a person can go to, in tab order, the tiled ones of a tab before
/// its floating ones, and in the order they were opened within each. The
/// sidebar's placeholders are not among them.
pub fn leaves(
    session: &str,
    tabs: &[TabInfo],
    manifest: &PaneManifest,
    places: &Places,
) -> Vec<Leaf> {
    let mut keyed: Vec<((usize, bool, u32), Leaf)> = Vec::new();
    for (position, panes) in &manifest.panes {
        let tab = tabs
            .iter()
            .find(|tab| tab.position == *position)
            .map_or("", |tab| tab.name.as_str());
        for pane in panes {
            if pane.is_plugin || !pane.is_selectable || pane.is_suppressed || is_placeholder(pane) {
                continue;
            }
            keyed.push((
                (*position, pane.is_floating, pane.id),
                Leaf {
                    id: pane.id,
                    title: pane.title.clone(),
                    tab: tab.to_owned(),
                    cwd: places.cwd(session, pane.id).map(str::to_owned),
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
    /// The status of the agent that wants you most, for a folded session to
    /// show in place of the rows it hides.
    pub loudest: Option<Status>,
    /// The characters of the name a search lands on.
    pub hits: Vec<usize>,
}

pub struct PaneNode {
    pub session: String,
    pub id: u32,
    pub label: String,
    pub agent: Option<Mark>,
    pub here: bool,
    /// The characters of the label a search lands on.
    pub hits: Vec<usize>,
}

pub struct Mark {
    pub status: Status,
    pub age: Held,
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

/// The live sessions, and how many of the agents in them are in each state
/// worth counting, folded or not: what the sidebar's header says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub sessions: usize,
    pub waiting: usize,
    pub idle: usize,
    pub busy: usize,
}

impl Summary {
    fn count(&mut self, status: &Status) {
        match status {
            Status::Waiting => self.waiting += 1,
            Status::Idle => self.idle += 1,
            Status::Busy => self.busy += 1,
            Status::Shell | Status::Unknown(_) | Status::Unreported => {},
        }
    }

    /// The counts the header shows, each with the status it counts.
    pub fn counts(&self) -> [(Status, usize); 3] {
        [(Status::Waiting, self.waiting), (Status::Idle, self.idle), (Status::Busy, self.busy)]
    }
}

#[derive(Default)]
pub struct Tree {
    pub rows: Cursor<Node>,
    /// Counts every session and agent, whatever a search hides.
    pub summary: Summary,
    folded: BTreeSet<String>,
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
}

/// The row at the top of the view the wheel moved, and the row lit as it did,
/// or since by a click. The view stays there until another row is lit, or a
/// key moves the cursor, and then shows the lit row again.
struct Scrolled {
    top: usize,
    lit: Option<Place>,
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
    /// `follow` puts the cursor on the pane you are in, which is where it
    /// belongs while the sidebar does not have the focus. Otherwise the cursor
    /// stays on the row it was on, or on its session once that is folded.
    ///
    /// While the search box holds a term, only the rows that match it are
    /// kept, with the session of each pane that does, unfolded. A session
    /// found by its name alone shows as folded. The cursor goes to the best
    /// match when the term changes, and stays on its row otherwise.
    ///
    /// A search lasts while the sidebar has the focus, so `follow` closes it.
    /// One left behind would keep the rows it hides out of sight.
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
        }
        let seated: HashMap<(&str, u32), &AgentRow> =
            agents.rows.iter().map(|row| ((row.seat.session(), row.pane), row)).collect();

        let mut order: Vec<&Branch> = branches.iter().collect();
        order.sort_by(|a, b| b.age.cmp(&a.age).then_with(|| a.name.cmp(&b.name)));

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
            let panes: Vec<(&Leaf, Option<&AgentRow>)> = branch
                .panes
                .iter()
                .map(|leaf| (leaf, seated.get(&(branch.name.as_str(), leaf.id)).copied()))
                .collect();
            let statuses =
                || panes.iter().filter_map(|(_, agent)| agent.map(|agent| &agent.status));
            for status in statuses() {
                summary.count(status);
            }
            // A search looks inside a folded session too.
            let open = if finder.is_some() || !folded { panes.as_slice() } else { &[] };
            let mut shown = Vec::new();
            for (leaf, agent) in open {
                let label = label(leaf, *agent);
                let (fit, hits) = match &finder {
                    Some(finder) => match finder.pane(&label, leaf) {
                        Some((fit, hits)) => (Some(fit), hits),
                        None => continue,
                    },
                    None => (None, Vec::new()),
                };
                let node = PaneNode {
                    session: branch.name.clone(),
                    id: leaf.id,
                    label,
                    agent: agent.map(|agent| Mark { status: agent.status.clone(), age: agent.age }),
                    here: is_current && here == Some(leaf.id),
                    hits,
                };
                shown.push((node, fit));
            }
            let named = finder.as_ref().and_then(|finder| finder.session(&branch.name));
            let (fit, hits) = named.unzip();
            if finder.is_some() && fit.is_none() && shown.is_empty() {
                continue;
            }
            if let Some(fit) = fit {
                weigh(rows.len(), fit);
            }
            rows.push(Node::Session(SessionNode {
                name: branch.name.clone(),
                current: is_current,
                folded: if finder.is_some() { shown.is_empty() } else { folded },
                waiting: statuses().filter(|status| status.is_waiting()).count(),
                loudest: statuses().min_by_key(|status| agents::status_rank(status)).cloned(),
                hits: hits.unwrap_or_default(),
            }));
            for (node, fit) in shown {
                if let Some(fit) = fit {
                    weigh(rows.len(), fit);
                }
                rows.push(Node::Pane(node));
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
        let at = match best {
            Some(best) if seek => Some(best),
            _ => held
                .and_then(|place| row_of(&place).or_else(|| row_of(&self.shown(place))))
                .or(best),
        };
        self.rows.replace_at(rows, at);
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
    /// the focus. A pane in a folded session unfolds it, to show the row.
    pub fn hand(&mut self, place: Place) {
        if let Place::Pane(session, _) = &place {
            self.folded.remove(session);
        }
        self.handed = Some(place);
    }

    /// Where the cursor was when the sidebar lost the focus, once.
    pub fn take_left(&mut self) -> Option<Place> {
        self.left.take()
    }

    /// The row at the top of the view the wheel left, while the row it lit
    /// then, `lit`, is lit still.
    pub fn top(&self, lit: Option<usize>) -> Option<usize> {
        let scrolled = self.scrolled.as_ref()?;
        (self.place_of(lit) == scrolled.lit).then_some(scrolled.top)
    }

    /// Puts row `top` at the top of the view, until another row than `lit`
    /// is lit.
    pub fn scroll_to(&mut self, top: usize, lit: Option<usize>) {
        self.scrolled = Some(Scrolled { top, lit: self.place_of(lit) });
    }

    /// Brings the view back to the lit row, and says whether it was away.
    pub fn unscroll(&mut self) -> bool {
        self.scrolled.take().is_some()
    }

    fn place_of(&self, row: Option<usize>) -> Option<Place> {
        row.and_then(|row| self.rows.get(row)).map(Node::place)
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
        if folded {
            self.folded.insert(name)
        } else {
            self.folded.remove(&name)
        }
    }

    pub fn toggle(&mut self) -> bool {
        let Some(name) = self.rows.selected_row().map(Node::session) else {
            return false;
        };
        let folded = self.folded.contains(name);
        self.fold(!folded)
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

    /// A pane by its label, the tab it is in, and its cwd. Only the label is
    /// on screen to light up. A tab zellij named `Tab #N` is no name to find,
    /// and a cwd is its last two parts, as the agent list shows it: a whole
    /// path has the letters for a few typed ones to land on nearly any of it.
    fn pane(&self, label: &str, leaf: &Leaf) -> Option<(Fit, Vec<usize>)> {
        let tab = is_named_tab(&leaf.tab).then_some(leaf.tab.as_str());
        let cwd = leaf.cwd.as_deref().map(short_cwd);
        self.find(label, &[tab, cwd.as_deref()])
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
fn label(leaf: &Leaf, agent: Option<&AgentRow>) -> String {
    if let Some(agent) = agent.filter(|agent| agent.chosen) {
        return agent.display.clone();
    }
    let title = bare_title(&leaf.title);
    if let Some(cwd) = leaf.cwd.as_deref().filter(|cwd| abbreviates(title, cwd)) {
        return short_cwd(cwd);
    }
    if !title.is_empty() && title != GENERIC_TITLE {
        return title.to_owned();
    }
    if is_named_tab(&leaf.tab) {
        return leaf.tab.clone();
    }
    if let Some(agent) = agent.filter(|agent| !agent.cwd.is_empty()) {
        return short_cwd(&agent.cwd);
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
    use crate::agents::Live;
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
                    cwd: None,
                })
                .collect(),
        }
    }

    /// claude-ps output for agents at `(session, pane, status)`, each sitting in
    /// the cwd `/w/<session>`, and the places that hold them.
    fn agents(seated: &[(&str, u32, &str)]) -> AgentSet {
        let json: Vec<String> = seated
            .iter()
            .map(|(session, pane, status)| {
                format!(
                    r#"{{"status":"{status}","status_age":4,"cwd":"/w/{session}",
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
        set.ingest(Some(0), format!("[{}]", json.join(",")).as_bytes(), b"");
        let live = Live::new(None, &[], &places);
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
                    node.agent.as_ref().map_or("-", |mark| mark.status.word()),
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
        let json = r#"[{"status":"busy","status_age":4,"cwd":"/w/s","name":"orca-copy",
            "name_source":"user","zellij":{"session":"s","pane":"0"}}]"#;
        let mut set = AgentSet::default();
        set.ingest(Some(0), json.as_bytes(), b"");
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let live = Live::new(None, &[], &places);
        set.rebuild("", &live, None, Age::from_secs(0), Selection::SnapToTop);

        let mut tree = Tree::default();
        tree.rebuild(&[branch("s", 1, &[(0, "⠂ Building the sidebar")])], None, &set, None, true);
        assert_eq!(drawn(&tree), ["▾ s", "  busy orca-copy"]);
    }

    #[test]
    fn agents_with_no_topic_and_no_tab_name_go_by_where_they_work() {
        let json = r#"[
            {"status":"busy","status_age":4,"name":"main-60","name_source":"derived",
             "cwd":"/w/luneta/main","zellij":{"session":"luneta","pane":"1"}},
            {"status":"idle","status_age":4,"name":"agents-tab-9a","name_source":"derived",
             "cwd":"/w/luneta/agents-tab","zellij":{"session":"luneta","pane":"4"}}]"#;
        let mut set = AgentSet::default();
        set.ingest(Some(0), json.as_bytes(), b"");
        let places = Places::of(&[("luneta", &[(1, "/w/luneta/main"), (4, "/w/luneta/agents-tab")])]);
        set.rebuild("", &Live::new(None, &[], &places), None, Age::from_secs(0), Selection::SnapToTop);

        let branches = [branch("luneta", 1, &[(1, "✳ Claude Code"), (4, "✳ Claude Code")])];
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, true);
        assert_eq!(drawn(&tree), ["▾ luneta", "  busy luneta/main", "  idle luneta/agents-tab"]);
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
            cwd: None,
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
            cwd: cwd.map(str::to_owned),
        };
        let main = Some("/home/you/Projects/misc/luneta/main");
        assert_eq!(label(&leaf("~/P/m/l/main", main), None), "luneta/main");
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
        let leaves = leaves("s", &[tab(0, "code"), tab(1, "logs")], &manifest, &places);
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
        assert_eq!(drawn(&tree), ["▸ a waiting:1", "▾ b", "  idle three"]);
        let Node::Session(a) = &tree.rows[0] else { panic!("a session comes first") };
        assert_eq!(a.loudest, Some(Status::Waiting));

        assert!(tree.toggle());
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.rows.len(), 5);
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
        let all = Summary { sessions: 2, waiting: 1, idle: 1, busy: 1 };
        let mut tree = Tree::default();
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.summary, all);
        assert!(tree.fold(true));
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(tree.summary, all);
        tree.search_for("three".to_owned());
        tree.rebuild(&branches, None, &set, None, false);
        assert_eq!(drawn(&tree), ["▾ b", "  idle three"]);
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

    #[test]
    fn a_row_the_picker_hands_over_holds_the_cursor_until_the_sidebar_has_the_focus() {
        let branches = [branch("a", 9, &[(0, "✳ one"), (1, "✳ two")]), branch("b", 1, &[(4, "x")])];
        let set = agents(&[]);
        let mut tree = Tree::default();
        tree.rebuild(&branches, Some("a"), &set, Some(0), false);
        tree.rows.move_selection(3);
        assert!(tree.fold(true));
        tree.rebuild(&branches, Some("a"), &set, Some(0), true);

        let handed = Place::Pane("b".to_owned(), 4);
        tree.hand(handed.clone());
        for focused in [false, false, true] {
            tree.rebuild(&branches, Some("a"), &set, Some(0), !focused);
            assert_eq!(selected(&tree), Some(handed.clone()), "focused: {focused}");
        }
        tree.rebuild(&branches, Some("a"), &set, Some(0), true);
        assert_eq!(selected(&tree), Some(Place::Pane("a".to_owned(), 0)));
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
        tree.rows.move_selection(3);
        assert_eq!(selected(&tree), Some(Place::Pane("b".to_owned(), 0)));

        tree.rebuild(&[branch("a", 9, &[(0, "x")])], None, &set, None, false);
        assert_eq!(selected(&tree), Some(Place::Session("a".to_owned())));

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

    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        let branches = [
            branch("bipa", 300, &[(31, "✳ a-p-i-x"), (39, "✳ dda-bank-slip"), (47, "✳ pix outflow")]),
            branch("notes", 200, &[(0, "~/Documents")]),
            branch("misc", 100, &[(3, "✳ luneta")]),
        ];
        let set = agents(&[]);
        for (term, rows, best) in [
            (
                "pix",
                &["▾ bipa", "  - a-p-i-x", "  - pix outflow"][..],
                Place::Pane("bipa".to_owned(), 47),
            ),
            ("note", &["▸ notes"][..], Place::Session("notes".to_owned())),
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
    fn a_pane_is_found_by_its_tab_and_its_cwd_but_lit_only_where_it_shows() {
        let leaf = |id: u32, tab: &str, cwd: Option<&str>| Leaf {
            id,
            title: "nvim".to_owned(),
            tab: tab.to_owned(),
            cwd: cwd.map(str::to_owned),
        };
        let branches = [Branch {
            name: "s".to_owned(),
            age: Age::from_secs(1),
            panes: vec![
                leaf(0, "notes", Some("/w/a")),
                leaf(1, "Tab #2", Some("/home/lorenzo/Projects/misc/luneta")),
                leaf(2, "", None),
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
        tree.rows.move_selection(2);

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
        assert_eq!(drawn(&tree), ["▾ a", "  idle one", "▾ b", "  ›busy two"]);
        assert_eq!(selected(&tree), Some(Place::Pane("b".to_owned(), 0)));
    }
}
