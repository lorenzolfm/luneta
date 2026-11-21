use std::fmt;

use crate::agent::Harness;
use crate::api;

use super::layout::{
    self, Leaf, Preordered, Removed, Sides, Towards, Tree, LAYOUT_VIEWS_MAX, SHARES,
};
use super::{Dir, Name, Size, ViewId};

pub const LINE_BYTES_MAX: usize = 1024;

pub const SESSION_BYTES_MAX: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalViewId(ViewId);

impl TerminalViewId {
    pub(super) fn new(view: ViewId) -> TerminalViewId {
        TerminalViewId(view)
    }

    #[must_use]
    pub fn view(self) -> ViewId {
        self.0
    }
}

impl fmt::Display for TerminalViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    id: TerminalViewId,
    folder: Dir,
    runs: Runs,
    name: Option<Name>,
}

impl Terminal {
    #[must_use]
    pub fn new(id: TerminalViewId, folder: Dir, runs: Runs) -> Terminal {
        Terminal { id, folder, runs, name: None }
    }

    #[must_use]
    pub fn id(&self) -> TerminalViewId {
        self.id
    }

    #[must_use]
    pub fn named(self, name: Option<Name>) -> Terminal {
        Terminal { name, ..self }
    }

    #[must_use]
    pub fn name(&self) -> Option<&Name> {
        self.name.as_ref()
    }

    #[must_use]
    pub fn folder(&self) -> &Dir {
        &self.folder
    }

    #[must_use]
    pub fn runs(&self) -> &Runs {
        &self.runs
    }

    pub(super) fn ran(&mut self, folder: Dir, runs: Runs) {
        self.folder = folder;
        self.runs = runs;
    }

    pub(super) fn renamed(&mut self, name: Option<Name>) {
        self.name = name;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Terminal(Terminal),
    Welcome(ViewId),
}

impl View {
    #[must_use]
    pub fn terminal(&self) -> Option<&Terminal> {
        match self {
            View::Terminal(terminal) => Some(terminal),
            View::Welcome(_) => None,
        }
    }
}

impl Leaf for View {
    fn id(&self) -> ViewId {
        match self {
            View::Terminal(terminal) => terminal.id.view(),
            View::Welcome(id) => *id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runs {
    Shell,
    Command(Line),
    Unrun(Line),
    Agent { harness: Harness, session: Session },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opens {
    Shell,
    Command(Argv),
}

impl Opens {
    pub fn of_api(opens: Option<api::Opens>) -> Result<Opens, String> {
        match opens.and_then(|opens| opens.runs) {
            None => Err("it says nothing to open".to_owned()),
            Some(api::opens::Runs::Shell(())) => Ok(Opens::Shell),
            Some(api::opens::Runs::Command(api::Argv { words })) => {
                Argv::parse(words).map(Opens::Command)
            },
        }
    }

    #[must_use]
    pub fn to_api(&self) -> api::Opens {
        let runs = match self {
            Opens::Shell => api::opens::Runs::Shell(()),
            Opens::Command(argv) => {
                api::opens::Runs::Command(api::Argv { words: argv.words.clone() })
            },
        };
        api::Opens { runs: Some(runs) }
    }

    #[must_use]
    pub fn runs(&self) -> Runs {
        match self {
            Opens::Shell => Runs::Shell,
            Opens::Command(argv) => Runs::Command(argv.line.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argv {
    words: Vec<String>,
    line: Line,
}

impl Argv {
    pub fn parse(words: Vec<String>) -> Result<Argv, String> {
        match words.first() {
            None => return Err("an argv names no program".to_owned()),
            Some(program) if program.is_empty() => {
                return Err("an argv names its program with an empty word".to_owned());
            },
            Some(_) => {},
        }
        let line = Line::parse(&quoted(&words))?;
        Ok(Argv { words, line })
    }

    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.words
    }

    #[must_use]
    pub fn program(&self) -> &str {
        &self.words[0]
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.words[1..]
    }
}

#[must_use]
pub fn quoted(argv: &[String]) -> String {
    let plain = |word: &str| {
        let safe = |byte: u8| byte.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&byte);
        !word.is_empty() && word.bytes().all(safe)
    };
    let words = argv.iter().map(|word| {
        if plain(word) { word.clone() } else { format!("'{}'", word.replace('\'', r"'\''")) }
    });
    words.collect::<Vec<String>>().join(" ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line(String);

impl Line {
    pub fn parse(text: &str) -> Result<Line, String> {
        if text.trim().is_empty() {
            return Err("a command line is blank".to_owned());
        }
        if text.len() > LINE_BYTES_MAX {
            return Err(format!("a command line is longer than {LINE_BYTES_MAX} bytes"));
        }
        if text.chars().any(|char| char.is_control() && !matches!(char, '\n' | '\t')) {
            return Err("a command line holds a control character".to_owned());
        }
        Ok(Line(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session(String);

impl Session {
    pub fn parse(text: &str) -> Result<Session, String> {
        if text.is_empty() || text.len() > SESSION_BYTES_MAX {
            return Err(format!("an agent's session is 1 to {SESSION_BYTES_MAX} bytes"));
        }
        if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(format!("the agent's session {text:?} holds more than letters and marks"));
        }
        if text.starts_with('-') {
            return Err(format!("the agent's session {text:?} reads as an option"));
        }
        Ok(Session(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout(Option<Laid>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Laid {
    tree: Tree<View>,
    focus: Option<Focus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Last(ViewId),
    Full(ViewId),
}

impl Focus {
    fn view(self) -> ViewId {
        match self {
            Focus::Last(view) | Focus::Full(view) => view,
        }
    }

    fn tiled(self) -> Focus {
        Focus::Last(self.view())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    Alone,
    Beside { at: ViewId, towards: Towards },
}

impl Layout {
    pub fn read(
        preorder: Vec<Preordered<View>>,
        full: Option<ViewId>,
        last_focus: Option<ViewId>,
    ) -> Result<Layout, String> {
        if preorder.is_empty() {
            return Err("a layout holds no view, and so is kept as no layout".to_owned());
        }
        let tree = Tree::of_preorder(preorder)?;
        let focus = match (full, last_focus) {
            (None, None) => None,
            (Some(full), None) => {
                fills(&tree, full)?;
                Some(Focus::Full(full))
            },
            (None, Some(last_focus)) => {
                if !tree.holds(last_focus) {
                    return Err(format!("the layout holds no view {last_focus} to focus"));
                }
                Some(Focus::Last(last_focus))
            },
            (Some(full), Some(last_focus)) => {
                let one = format!("the layout is filled by view {full}");
                return Err(format!("{one}, and so is focused on no other view {last_focus}"));
            },
        };
        Ok(Layout(Some(Laid { tree, focus })))
    }

    pub fn joined(self, other: Layout) -> Result<Layout, String> {
        let last_focus = self.last_focus().or(other.last_focus());
        let (Some(one), Some(other)) = (self.tree(), other.tree()) else {
            return Ok(if self.is_empty() { other } else { self });
        };
        let owned = |node: Preordered<&View>| match node {
            Preordered::Split { towards, share } => Preordered::Split { towards, share },
            Preordered::View(view) => Preordered::View(view.clone()),
        };
        let split = Preordered::Split { towards: Towards::Right, share: SHARES / 2 };
        let halves = one.preorder().into_iter().chain(other.preorder()).map(owned);
        let preorder = std::iter::once(split).chain(halves).collect();
        let joined = Layout::read(preorder, None, last_focus)?;
        let both = one.len() + other.len();
        assert_eq!(joined.len(), both, "a joined layout holds both layouts' views");
        assert_eq!(joined.last_focus(), last_focus, "a joined layout keeps a focus of either");
        Ok(joined)
    }

    #[must_use]
    pub fn tree(&self) -> Option<&Tree<View>> {
        self.0.as_ref().map(|laid| &laid.tree)
    }

    #[must_use]
    pub fn full(&self) -> Option<ViewId> {
        match self.0.as_ref()?.focus? {
            Focus::Full(full) => Some(full),
            Focus::Last(_) => None,
        }
    }

    #[must_use]
    pub fn last_focus(&self) -> Option<ViewId> {
        self.0.as_ref()?.focus.map(Focus::view)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tree().map_or(0, Tree::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.tree().into_iter().flat_map(Tree::leaves)
    }

    pub fn terminals(&self) -> impl Iterator<Item = &Terminal> {
        self.views().filter_map(View::terminal)
    }

    #[must_use]
    pub fn holds(&self, terminal: ViewId) -> bool {
        self.tree().is_some_and(|tree| tree.holds(terminal))
    }

    pub(super) fn terminal_mut(&mut self, terminal: TerminalViewId) -> &mut Terminal {
        let laid = self.0.as_mut().expect("a layout asked for its terminal is laid");
        match laid.tree.get_mut(terminal.view()).expect("its layout holds the terminal") {
            View::Terminal(held) => held,
            View::Welcome(_) => unreachable!("terminal {terminal} is made only as a terminal"),
        }
    }

    pub(super) fn first(view: View) -> Layout {
        let first = Layout(Some(Laid { tree: Tree::one(view), focus: None }));
        assert_eq!(first.len(), 1, "a first layout holds its one view");
        first
    }

    pub(super) fn opened(
        &mut self,
        view: View,
        placed: Placed,
        size: Size,
    ) -> Result<(), String> {
        let id = view.id();
        let count = self.len();
        match (&mut self.0, placed) {
            (None, Placed::Alone) => {
                self.0 = Some(Laid { tree: Tree::one(view), focus: None });
            },
            (Some(_), Placed::Alone) => return Err("the layout holds views already".to_owned()),
            (None, Placed::Beside { at, .. }) => {
                return Err(format!("the layout is empty, and so holds no view {at}"));
            },
            (Some(laid), Placed::Beside { at, towards }) => {
                if !laid.tree.holds(at) {
                    return Err(format!("the layout holds no view {at}"));
                }
                if laid.tree.len() >= LAYOUT_VIEWS_MAX {
                    let most = LAYOUT_VIEWS_MAX;
                    return Err(format!("the layout holds {most} views, as many as it may"));
                }
                if laid.tree.room(at, towards, id, size).is_none() {
                    return Err(format!("view {at} has no room for another beside it"));
                }
                laid.tree.split(at, towards, view, size);
                laid.focus = laid.focus.map(Focus::tiled);
            },
        }
        assert!(self.holds(id), "view {id} is in the layout it opened in");
        assert_eq!(self.len(), count + 1, "a layout opens one view at a time");
        Ok(())
    }

    pub(super) fn closed(&mut self, view: ViewId) -> Option<ViewId> {
        let laid = self.0.as_mut().expect("a closed view is in the layout");
        let heir = match laid.tree.remove(view) {
            Removed::Last => {
                self.0 = None;
                None
            },
            Removed::Out { heir, .. } => {
                laid.focus = match laid.focus {
                    Some(focus) if focus.view() == view => Some(Focus::Last(heir)),
                    Some(focus) if laid.tree.len() == 1 => Some(focus.tiled()),
                    kept => kept,
                };
                Some(heir)
            },
        };
        assert!(!self.holds(view), "view {view} left its layout");
        if let Some(heir) = heir {
            assert!(self.holds(heir), "view {heir} took the room of view {view}");
        }
        let held = self.last_focus().is_none_or(|focus| self.holds(focus));
        assert!(held, "a layout's last focus is a view of its own");
        heir
    }

    pub(super) fn focused(&mut self, view: ViewId) {
        let laid = self.0.as_mut().expect("a focused view is in the layout");
        assert!(laid.tree.holds(view), "view {view} is focused in its own layout");
        if laid.focus != Some(Focus::Full(view)) {
            laid.focus = Some(Focus::Last(view));
        }
        assert_eq!(self.last_focus(), Some(view), "view {view} is its layout's last focus");
    }

    pub(super) fn filled(&mut self, full: Option<ViewId>) -> Result<(), String> {
        let Some(laid) = &mut self.0 else {
            return match full {
                None => Ok(()),
                Some(full) => Err(format!("the layout is empty, and so holds no view {full}")),
            };
        };
        laid.focus = match full {
            Some(full) => {
                fills(&laid.tree, full)?;
                Some(Focus::Full(full))
            },
            None => laid.focus.map(Focus::tiled),
        };
        assert_eq!(self.full(), full, "a layout is filled by the view it was asked to be");
        Ok(())
    }

    pub(super) fn resized(
        &mut self,
        view: ViewId,
        sides: Sides,
        growth: layout::Growth,
        size: Size,
    ) -> Result<(), String> {
        let laid = self.0.as_mut().expect("a resized view is in the layout");
        if matches!(laid.focus, Some(Focus::Full(_))) {
            return Err("a layout one view fills is resized by none".to_owned());
        }
        if !laid.tree.resize(view, sides, growth, size) {
            return Err(format!("no border of view {view} moves that way"));
        }
        Ok(())
    }

    pub(super) fn swapped(&mut self, one: ViewId, other: ViewId) -> Result<(), String> {
        let laid = self.0.as_mut().expect("swapped views are in the layout");
        if matches!(laid.focus, Some(Focus::Full(_))) {
            return Err("a layout one view fills moves none of them".to_owned());
        }
        laid.tree.swap(one, other);
        Ok(())
    }
}

fn fills(tree: &Tree<View>, full: ViewId) -> Result<(), String> {
    if !tree.holds(full) {
        return Err(format!("the layout holds no view {full}"));
    }
    if tree.len() == 1 {
        return Err("a layout of one view shows it whole already".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_s_session_is_one_word_that_reads_as_no_option() {
        assert_eq!(Session::parse("7c22-ab").unwrap().as_str(), "7c22-ab");
        for refused in ["", "-7c22", "--debug", "7c22 ab", "7c22\n"] {
            assert!(Session::parse(refused).is_err(), "{refused:?}");
        }
    }
}
