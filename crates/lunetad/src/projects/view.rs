use crate::agent::Harness;

use super::layout::{self, Leaf, Preordered, Removed, Sides, Towards, Tree, PANES_MAX, SHARES};
use super::{Dir, Name, Size, TerminalId};

pub const LINE_BYTES_MAX: usize = 1024;

pub const SESSION_BYTES_MAX: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    id: TerminalId,
    folder: Dir,
    runs: Runs,
    name: Option<Name>,
}

impl Terminal {
    #[must_use]
    pub fn new(id: TerminalId, folder: Dir, runs: Runs) -> Terminal {
        Terminal { id, folder, runs, name: None }
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

impl Leaf for Terminal {
    fn id(&self) -> TerminalId {
        self.id
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
    tree: Tree<Terminal>,
    full: Option<TerminalId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    Alone,
    Beside { at: TerminalId, towards: Towards },
}

impl Layout {
    pub fn read(
        preorder: Vec<Preordered<Terminal>>,
        full: Option<TerminalId>,
    ) -> Result<Layout, String> {
        if preorder.is_empty() {
            return Err("a layout holds no terminal, and so is kept as no layout".to_owned());
        }
        let tree = Tree::of_preorder(preorder)?;
        if let Some(full) = full {
            fills(&tree, full)?;
        }
        Ok(Layout(Some(Laid { tree, full })))
    }

    pub fn joined(self, other: Layout) -> Result<Layout, String> {
        let (Some(one), Some(other)) = (self.tree(), other.tree()) else {
            return Ok(if self.is_empty() { other } else { self });
        };
        let owned = |node: Preordered<&Terminal>| match node {
            Preordered::Split { towards, share } => Preordered::Split { towards, share },
            Preordered::Pane(terminal) => Preordered::Pane(terminal.clone()),
        };
        let split = Preordered::Split { towards: Towards::Right, share: SHARES / 2 };
        let halves = one.preorder().into_iter().chain(other.preorder()).map(owned);
        let joined = Layout::read(std::iter::once(split).chain(halves).collect(), None)?;
        let both = one.len() + other.len();
        assert_eq!(joined.len(), both, "a joined layout holds both layouts' terminals");
        Ok(joined)
    }

    #[must_use]
    pub fn tree(&self) -> Option<&Tree<Terminal>> {
        self.0.as_ref().map(|laid| &laid.tree)
    }

    #[must_use]
    pub fn full(&self) -> Option<TerminalId> {
        self.0.as_ref().and_then(|laid| laid.full)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tree().map_or(0, Tree::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub fn terminals(&self) -> impl Iterator<Item = &Terminal> {
        self.tree().into_iter().flat_map(Tree::leaves)
    }

    #[must_use]
    pub fn holds(&self, terminal: TerminalId) -> bool {
        self.tree().is_some_and(|tree| tree.holds(terminal))
    }

    pub(super) fn terminal_mut(&mut self, terminal: TerminalId) -> Option<&mut Terminal> {
        self.0.as_mut()?.tree.get_mut(terminal)
    }

    pub(super) fn opened(
        &mut self,
        terminal: Terminal,
        placed: Placed,
        size: Size,
    ) -> Result<(), String> {
        let id = terminal.id;
        let count = self.len();
        match (&mut self.0, placed) {
            (None, Placed::Alone) => {
                self.0 = Some(Laid { tree: Tree::one(terminal), full: None });
            },
            (Some(_), Placed::Alone) => return Err("the layout holds terminals already".to_owned()),
            (None, Placed::Beside { at, .. }) => {
                return Err(format!("the layout is empty, and so holds no terminal {at}"));
            },
            (Some(laid), Placed::Beside { at, towards }) => {
                if !laid.tree.holds(at) {
                    return Err(format!("the layout holds no terminal {at}"));
                }
                if laid.tree.len() >= PANES_MAX {
                    let most = format!("the layout holds {PANES_MAX} terminals, as many as it may");
                    return Err(most);
                }
                if laid.tree.room(at, towards, id, size).is_none() {
                    return Err(format!("terminal {at} has no room for another beside it"));
                }
                laid.tree.split(at, towards, terminal, size);
                laid.full = None;
            },
        }
        assert!(self.holds(id), "terminal {id} is in the layout it opened in");
        assert_eq!(self.len(), count + 1, "a layout opens one terminal at a time");
        Ok(())
    }

    pub(super) fn closed(&mut self, terminal: TerminalId) -> Option<TerminalId> {
        let laid = self.0.as_mut().expect("a closed terminal is in the layout");
        let heir = match laid.tree.remove(terminal) {
            Removed::Last => {
                self.0 = None;
                None
            },
            Removed::Out { heir, .. } => {
                if laid.full == Some(terminal) || laid.tree.len() == 1 {
                    laid.full = None;
                }
                Some(heir)
            },
        };
        assert!(!self.holds(terminal), "terminal {terminal} left its layout");
        if let Some(heir) = heir {
            assert!(self.holds(heir), "terminal {heir} took the room of terminal {terminal}");
        }
        heir
    }

    pub(super) fn filled(&mut self, full: Option<TerminalId>) -> Result<(), String> {
        let Some(laid) = &mut self.0 else {
            return match full {
                None => Ok(()),
                Some(full) => Err(format!("the layout is empty, and so holds no terminal {full}")),
            };
        };
        if let Some(full) = full {
            fills(&laid.tree, full)?;
        }
        laid.full = full;
        Ok(())
    }

    pub(super) fn resized(
        &mut self,
        terminal: TerminalId,
        sides: Sides,
        growth: layout::Growth,
        size: Size,
    ) -> Result<(), String> {
        let laid = self.0.as_mut().expect("a resized terminal is in the layout");
        if laid.full.is_some() {
            return Err("a layout one terminal fills is resized by none".to_owned());
        }
        if !laid.tree.resize(terminal, sides, growth, size) {
            return Err(format!("no border of terminal {terminal} moves that way"));
        }
        Ok(())
    }

    pub(super) fn swapped(&mut self, one: TerminalId, other: TerminalId) -> Result<(), String> {
        let laid = self.0.as_mut().expect("swapped terminals are in the layout");
        if laid.full.is_some() {
            return Err("a layout one terminal fills moves none of them".to_owned());
        }
        laid.tree.swap(one, other);
        Ok(())
    }
}

fn fills(tree: &Tree<Terminal>, full: TerminalId) -> Result<(), String> {
    if !tree.holds(full) {
        return Err(format!("the layout holds no terminal {full}"));
    }
    if tree.len() == 1 {
        return Err("a layout of one terminal shows it whole already".to_owned());
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
