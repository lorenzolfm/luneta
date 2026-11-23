use std::fmt;

use crate::agent::Harness;
use crate::api;

use super::layout::{self, Leaf, Preordered, Rect, Removed, Sides, Towards, Tree};
use super::layout::{FRAME_CELLS, LAYOUT_VIEWS_MAX, SHARES};
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
    #[must_use]
    pub fn first(agent: Option<Argv>) -> Opens {
        agent.map_or(Opens::Shell, Opens::Command)
    }

    pub fn of_api(opens: Option<api::Opens>) -> Result<Opens, String> {
        match opens.and_then(|opens| opens.runs) {
            None => Err("it says nothing to open".to_owned()),
            Some(api::opens::Runs::Shell(())) => Ok(Opens::Shell),
            Some(api::opens::Runs::Command(argv)) => Argv::of_api(argv).map(Opens::Command),
            Some(api::opens::Runs::Agent(api::Prompted { harness, words })) => {
                let harness = Harness::said(Some(harness))?;
                if words.trim().is_empty() {
                    return Err("an agent is asked for with no words".to_owned());
                }
                Argv::on_words(harness, words, &[]).map(Opens::Command)
            },
        }
    }

    #[must_use]
    pub fn to_api(&self) -> api::Opens {
        let runs = match self {
            Opens::Shell => api::opens::Runs::Shell(()),
            Opens::Command(argv) => api::opens::Runs::Command(argv.to_api()),
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

    pub fn on_words(harness: Harness, words: String, files: &[&str]) -> Result<Argv, String> {
        let mut command = vec![harness.program().to_owned()];
        if harness == Harness::ClaudeCode {
            let rules: Vec<String> = files.iter().filter_map(|file| read_rule(file)).collect();
            if !rules.is_empty() {
                command.push("--allowedTools".to_owned());
                command.extend(rules);
            }
        }
        command.push("--".to_owned());
        command.push(words);
        Argv::parse(command)
    }

    pub fn of_api(argv: api::Argv) -> Result<Argv, String> {
        Argv::parse(argv.words)
    }

    #[must_use]
    pub fn to_api(&self) -> api::Argv {
        api::Argv { words: self.words.clone() }
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
    shape: Shape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arranged {
    Tabs,
    Panes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Tabs { last: Option<ViewId> },
    Panes { focus: Option<Focus> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Last(ViewId),
    Full(ViewId),
}

impl Focus {
    fn view(self) -> ViewId {
        match self {
            Focus::Last(view) | Focus::Full(view) => view,
        }
    }
}

impl Shape {
    fn arranged(self) -> Arranged {
        match self {
            Shape::Tabs { .. } => Arranged::Tabs,
            Shape::Panes { .. } => Arranged::Panes,
        }
    }

    fn last_focus(self) -> Option<ViewId> {
        match self {
            Shape::Tabs { last } => last,
            Shape::Panes { focus } => focus.map(Focus::view),
        }
    }

    fn full(self) -> Option<ViewId> {
        match self {
            Shape::Panes { focus: Some(Focus::Full(full)) } => Some(full),
            Shape::Panes { focus: Some(Focus::Last(_)) | None } | Shape::Tabs { .. } => None,
        }
    }

    fn unfocused(arranged: Arranged) -> Shape {
        match arranged {
            Arranged::Tabs => Shape::Tabs { last: None },
            Arranged::Panes => Shape::Panes { focus: None },
        }
    }

    fn focused_on(self, view: ViewId) -> Shape {
        match self {
            Shape::Tabs { .. } => Shape::Tabs { last: Some(view) },
            Shape::Panes { .. } => Shape::Panes { focus: Some(Focus::Last(view)) },
        }
    }

    fn unfilled(self) -> Shape {
        match self.last_focus() {
            Some(view) => self.focused_on(view),
            None => Shape::unfocused(self.arranged()),
        }
    }

    fn arranged_as(self, arranged: Arranged) -> Shape {
        let unfocused = Shape::unfocused(arranged);
        self.last_focus().map_or(unfocused, |view| unfocused.focused_on(view))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    Alone,
    Beside { at: ViewId, towards: Towards },
}

impl Layout {
    pub fn read(preorder: Vec<Preordered<View>>, shape: Shape) -> Result<Layout, String> {
        if preorder.is_empty() {
            return Err("a layout holds no view, and so is kept as no layout".to_owned());
        }
        let tree = Tree::of_preorder(preorder)?;
        match shape {
            Shape::Tabs { last: Some(last) } | Shape::Panes { focus: Some(Focus::Last(last)) } => {
                if !tree.holds(last) {
                    return Err(format!("the layout holds no view {last} to focus"));
                }
            },
            Shape::Panes { focus: Some(Focus::Full(full)) } => fills(&tree, full)?,
            Shape::Tabs { last: None } | Shape::Panes { focus: None } => {},
        }
        Ok(Layout(Some(Laid { tree, shape })))
    }

    pub fn joined(self, other: Layout) -> Result<Layout, String> {
        let last_focus = self.last_focus().or(other.last_focus());
        let (Some(one), Some(other)) = (self.tree(), other.tree()) else {
            return Ok(if self.is_empty() { other } else { self });
        };
        let arranged = self.arranged().expect("a layout that holds views is arranged");
        let owned = |node: Preordered<&View>| match node {
            Preordered::Split { towards, share } => Preordered::Split { towards, share },
            Preordered::View(view) => Preordered::View(view.clone()),
        };
        let split = Preordered::Split { towards: Towards::Right, share: SHARES / 2 };
        let halves = one.preorder().into_iter().chain(other.preorder()).map(owned);
        let preorder = std::iter::once(split).chain(halves).collect();
        let unfocused = Shape::unfocused(arranged);
        let shape = last_focus.map_or(unfocused, |view| unfocused.focused_on(view));
        let joined = Layout::read(preorder, shape)?;
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
    pub fn shape(&self) -> Option<Shape> {
        self.0.as_ref().map(|laid| laid.shape)
    }

    #[must_use]
    pub fn arranged(&self) -> Option<Arranged> {
        self.0.as_ref().map(|laid| laid.shape.arranged())
    }

    #[must_use]
    pub fn tabbed(&self) -> bool {
        self.arranged() == Some(Arranged::Tabs)
    }

    #[must_use]
    fn room_in(&self, size: Size) -> Size {
        if self.tabbed() { layout::SIZE_MAX } else { size }
    }

    #[must_use]
    pub fn full(&self) -> Option<ViewId> {
        self.0.as_ref()?.shape.full()
    }

    #[must_use]
    pub fn last_focus(&self) -> Option<ViewId> {
        self.0.as_ref()?.shape.last_focus()
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

    #[must_use]
    pub fn beside(&self, view: ViewId, direction: layout::Direction, size: Size) -> Option<ViewId> {
        use layout::Direction;
        let laid = self.0.as_ref()?;
        let views = laid.tree.views();
        let at = views.iter().position(|it| *it == view).expect("a view stepped from is laid");
        match (laid.shape, direction) {
            (Shape::Panes { .. }, _) => laid.tree.neighbour(view, direction, size),
            (Shape::Tabs { .. }, Direction::Up | Direction::Down) => None,
            (Shape::Tabs { .. }, Direction::Left) => at.checked_sub(1).map(|before| views[before]),
            (Shape::Tabs { .. }, Direction::Right) => views.get(at + 1).copied(),
        }
    }

    #[must_use]
    pub fn room_for(&self, new: ViewId, beside: Option<ViewId>, size: Size) -> Option<Placed> {
        let tree = self.tree()?;
        assert!(!tree.holds(new), "view {new} is placed in a layout that holds it");
        let at = beside.unwrap_or_else(|| tree.largest(size).0);
        let rects = tree.rects(size);
        let (_, rect) = rects.iter().find(|(view, _)| *view == at)?;
        let towards = rect.longer();
        let mut ways = [towards, towards.across()].into_iter();
        let towards = ways.find(|towards| self.room_towards(at, *towards, new, size))?;
        Some(Placed::Beside { at, towards })
    }

    #[must_use]
    pub fn room_towards(&self, at: ViewId, towards: Towards, new: ViewId, size: Size) -> bool {
        let room_in = self.room_in(size);
        self.tree().is_some_and(|tree| tree.room(at, towards, new, room_in).is_some())
    }

    #[must_use]
    pub fn rects(&self, size: Size) -> (Size, Vec<(&View, Rect)>) {
        let Some(laid) = &self.0 else {
            return (size, Vec::new());
        };
        let tiles = laid.tree.laid(size);
        let (fitted, laid_out) = match laid.shape {
            Shape::Tabs { .. } => {
                let fitted = layout::lone_whole(size);
                let tab = Rect::whole(fitted);
                (fitted, tiles.into_iter().map(|(view, _)| (view, tab)).collect::<Vec<_>>())
            },
            Shape::Panes { .. } => (laid.tree.fitted(size), tiles),
        };
        assert_eq!(laid_out.len(), laid.tree.len(), "a layout lays out each of its views");
        (fitted, laid_out)
    }

    #[must_use]
    pub fn sized(&self, size: Size) -> (Size, Vec<(&View, Size)>) {
        let full = self.full();
        let (fitted, laid_out) = self.rects(size);
        let sized = laid_out.into_iter().map(|(view, rect)| {
            let shown = if full == Some(view.id()) { Rect::whole(fitted) } else { rect };
            (view, shown.inner())
        });
        let sized: Vec<(&View, Size)> = sized.collect();
        for (view, inner) in &sized {
            let ((columns, rows), (columns_fitted, rows_fitted)) = (inner.get(), fitted.get());
            let id = view.id();
            assert!(columns + 2 * FRAME_CELLS <= columns_fitted, "view {id} fits its width");
            assert!(rows + 2 * FRAME_CELLS <= rows_fitted, "view {id} fits its height");
        }
        (fitted, sized)
    }

    pub(super) fn terminal_mut(&mut self, terminal: TerminalViewId) -> &mut Terminal {
        let laid = self.0.as_mut().expect("a layout asked for its terminal is laid");
        match laid.tree.get_mut(terminal.view()).expect("its layout holds the terminal") {
            View::Terminal(held) => held,
            View::Welcome(_) => unreachable!("terminal {terminal} is made only as a terminal"),
        }
    }

    pub(super) fn first(view: View) -> Layout {
        let shape = Shape::unfocused(Arranged::Tabs);
        let first = Layout(Some(Laid { tree: Tree::one(view), shape }));
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
        let size = self.room_in(size);
        match (&mut self.0, placed) {
            (None, Placed::Alone) => {
                let shape = Shape::unfocused(Arranged::Tabs);
                self.0 = Some(Laid { tree: Tree::one(view), shape });
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
                laid.shape = laid.shape.unfilled();
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
                laid.shape = match laid.shape.last_focus() {
                    Some(focus) if focus == view => laid.shape.focused_on(heir),
                    Some(_) if laid.tree.len() == 1 => laid.shape.unfilled(),
                    Some(_) | None => laid.shape,
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
        if laid.shape.full() != Some(view) {
            laid.shape = laid.shape.focused_on(view);
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
        laid.shape = match (laid.shape, full) {
            (Shape::Tabs { .. }, Some(_)) => {
                return Err("a layout shown as tabs shows each view whole already".to_owned());
            },
            (Shape::Tabs { .. }, None) => laid.shape,
            (Shape::Panes { .. }, Some(full)) => {
                fills(&laid.tree, full)?;
                Shape::Panes { focus: Some(Focus::Full(full)) }
            },
            (Shape::Panes { .. }, None) => laid.shape.unfilled(),
        };
        assert_eq!(self.full(), full, "a layout is filled by the view it was asked to be");
        Ok(())
    }

    pub(super) fn arranged_as(&mut self, arranged: Arranged) -> Result<(), String> {
        let Some(laid) = &mut self.0 else {
            return Err("an empty layout is shown neither as tabs nor as panes".to_owned());
        };
        let last_focus = laid.shape.last_focus();
        laid.shape = laid.shape.arranged_as(arranged);
        assert_eq!(self.arranged(), Some(arranged), "a layout is arranged as it was asked");
        assert_eq!(self.last_focus(), last_focus, "a layout arranged anew keeps its last focus");
        assert!(self.full().is_none(), "a layout arranged anew is filled by no view");
        Ok(())
    }

    #[must_use]
    pub fn resizes(&self, view: ViewId, sides: Sides, growth: layout::Growth, size: Size) -> bool {
        if self.is_empty() {
            return false;
        }
        self.clone().resized(view, sides, growth, size).is_ok()
    }

    pub(super) fn resized(
        &mut self,
        view: ViewId,
        sides: Sides,
        growth: layout::Growth,
        size: Size,
    ) -> Result<(), String> {
        let laid = self.0.as_mut().expect("a resized view is in the layout");
        match laid.shape {
            Shape::Tabs { .. } => {
                return Err("a layout shown as tabs is resized by none".to_owned());
            },
            Shape::Panes { focus: Some(Focus::Full(_)) } => {
                return Err("a layout one view fills is resized by none".to_owned());
            },
            Shape::Panes { focus: Some(Focus::Last(_)) | None } => {},
        }
        if !laid.tree.resize(view, sides, growth, size) {
            return Err(format!("no border of view {view} moves that way"));
        }
        Ok(())
    }

    pub(super) fn swapped(&mut self, one: ViewId, other: ViewId) -> Result<(), String> {
        let laid = self.0.as_mut().expect("swapped views are in the layout");
        if laid.shape.full().is_some() {
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

impl Arranged {
    pub fn said(said: i32) -> Result<Arranged, String> {
        match api::Arrangement::try_from(said) {
            Ok(api::Arrangement::Tabs) => Ok(Arranged::Tabs),
            Ok(api::Arrangement::Panes) => Ok(Arranged::Panes),
            Ok(api::Arrangement::Unspecified) | Err(_) => {
                Err("it says neither tabs nor panes".to_owned())
            },
        }
    }

    #[must_use]
    pub fn to_api(self) -> api::Arrangement {
        match self {
            Arranged::Tabs => api::Arrangement::Tabs,
            Arranged::Panes => api::Arrangement::Panes,
        }
    }
}

fn read_rule(file: &str) -> Option<String> {
    let mut rule = String::from("Read(/");
    let mut inside = true;
    for c in file.chars() {
        match c {
            '?' => return None,
            ' ' | ',' if !inside => return None,
            c if c.is_control() => return None,
            '(' => inside = true,
            ')' => inside = false,
            '*' | '[' | ']' => rule.push('\\'),
            '\\' => rule.push_str(r"\\\"),
            _ => {},
        }
        rule.push(c);
    }
    rule.push(')');
    Some(rule)
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

    #[test]
    fn an_agent_is_let_read_the_file_by_a_rule_that_names_it_alone() {
        for (file, rule) in [
            ("/w/plan.md", "Read(//w/plan.md)"),
            ("/w/My Plan (v2).md", "Read(//w/My Plan (v2).md)"),
            ("/w/a, b.md", "Read(//w/a, b.md)"),
            ("/w/b{a,c}.md", "Read(//w/b{a,c}.md)"),
            ("/w/[draft] a*.md", r"Read(//w/\[draft\] a\*.md)"),
            (r"/w/back\slash.md", r"Read(//w/back\\\\slash.md)"),
        ] {
            let prompt = format!("Tackle {file}");
            let command = ["claude", "--allowedTools", rule, "--", prompt.as_str()];
            assert_eq!(
                Argv::on_words(Harness::ClaudeCode, prompt.clone(), &[file]).unwrap().words(),
                command.map(str::to_owned).to_vec(),
                "{file}"
            );
            assert_eq!(
                Argv::on_words(Harness::Pi, prompt.clone(), &[file]).unwrap().words(),
                ["pi".to_owned(), "--".to_owned(), prompt],
                "{file}"
            );
        }
        for file in [
            "/w/what?.md",
            "/w/Plan (v2) final.md",
            "/w/Plan (v2), final.md",
            "/w/two\nlines.md",
        ] {
            let prompt = format!("Tackle {file}");
            assert_eq!(
                Argv::on_words(Harness::ClaudeCode, prompt.clone(), &[file]).unwrap().words(),
                ["claude".to_owned(), "--".to_owned(), prompt],
                "{file:?}"
            );
        }
    }

    #[test]
    fn an_agent_asked_for_by_its_words_opens_as_its_command_line() {
        let asked = |harness: api::Harness, words: &str| api::Opens {
            runs: Some(api::opens::Runs::Agent(api::Prompted {
                harness: harness.into(),
                words: words.to_owned(),
            })),
        };
        let opens = Opens::of_api(Some(asked(api::Harness::ClaudeCode, "Tackle #444"))).unwrap();
        let Opens::Command(argv) = opens else { panic!("an agent opens as a command") };
        assert_eq!(argv.words(), ["claude", "--", "Tackle #444"]);
        let opens = Opens::of_api(Some(asked(api::Harness::Pi, "Tackle #444"))).unwrap();
        let Opens::Command(argv) = opens else { panic!("an agent opens as a command") };
        assert_eq!(argv.words(), ["pi", "--", "Tackle #444"]);
        let unnamed = Opens::of_api(Some(asked(api::Harness::Unspecified, "Tackle #444")));
        assert_eq!(unnamed, Err("a harness this luned does not know".to_owned()));
        let blank = Opens::of_api(Some(asked(api::Harness::ClaudeCode, " \n")));
        assert_eq!(blank, Err("an agent is asked for with no words".to_owned()));
    }

    #[test]
    fn an_agent_s_words_are_never_read_as_its_flags() {
        let words = "--dangerously-skip-permissions".to_owned();
        for harness in [Harness::ClaudeCode, Harness::Pi] {
            let argv = Argv::on_words(harness, words.clone(), &[]).unwrap();
            assert_eq!(argv.words(), [harness.program(), "--", words.as_str()]);
        }
    }

}
