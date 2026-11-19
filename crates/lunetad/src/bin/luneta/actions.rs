use crossterm::event::{KeyCode, KeyModifiers};

use crate::aloud;
use crate::dictation;
use crate::machines::Machine;

pub type Chord = (KeyCode, KeyModifiers);

pub const SWAP: Chord = (KeyCode::Char('a'), KeyModifiers::CONTROL);

pub const COLLAPSE: Chord = (KeyCode::Char('q'), KeyModifiers::CONTROL);

pub const MENU: Chord = (KeyCode::Char('a'), KeyModifiers::ALT);

pub const WORKSTREAMS: Chord = (KeyCode::Char('j'), KeyModifiers::SUPER);

pub const LEFT: Chord = (KeyCode::Char('h'), KeyModifiers::CONTROL);

pub const DOWN: Chord = (KeyCode::Char('j'), KeyModifiers::CONTROL);

pub const UP: Chord = (KeyCode::Char('k'), KeyModifiers::CONTROL);

pub const RIGHT: Chord = (KeyCode::Char('l'), KeyModifiers::CONTROL);

pub const GROW: Chord = (KeyCode::Char('='), KeyModifiers::ALT);

pub const SHRINK: Chord = (KeyCode::Char('-'), KeyModifiers::ALT);

pub const AGENTS: Chord = (KeyCode::Char('b'), KeyModifiers::CONTROL);

pub const DIRS: Chord = (KeyCode::Char('d'), KeyModifiers::CONTROL);

pub const REVIEW: &str = "thermo-nuclear-review";

pub const YANK: char = 'y';

pub const YANK_PULL: char = 'p';

pub const SKILLS: char = 'c';

pub const MENU_GROUPS: [(char, &str); 4] =
    [(SKILLS, "Skills"), ('n', "New"), ('d', "Delete"), ('b', "Brief")];

pub const ROW_GROUPS: [(char, &str); 1] = [(YANK, "Yank")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Noun {
    View,
    Workstream,
    Project,
    Machine,
}

const EVERY: &[Noun] = &[Noun::View, Noun::Workstream, Noun::Project, Noun::Machine];

const VIEW: &[Noun] = &[Noun::View];

const WORKSTREAM: &[Noun] = &[Noun::Workstream];

const PROJECT: &[Noun] = &[Noun::Project];

const MACHINE: &[Noun] = &[Noun::Machine];

const CHECKED_OUT: &[Noun] = &[Noun::View, Noun::Workstream];

const IN_A_PROJECT: &[Noun] = &[Noun::View, Noun::Workstream, Noun::Project];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Any,
    Here,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Menu(&'static [char]),
    Row(&'static [char]),
    Pasted(char),
    Chord(Chord),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub name: &'static str,
    pub keywords: &'static [&'static str],
    pub key: Key,
    pub nouns: &'static [Noun],
    pub reach: Reach,
}

macro_rules! actions {
    ($($action:ident,)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Action {
            $($action,)+
        }

        impl Action {
            pub const ALL: [Action; [$(stringify!($action),)+].len()] = [$(Action::$action,)+];
        }
    };
}

actions! {
    SearchSkills,
    Ask,
    FileIssue,
    Review,
    IllegalStates,
    TigerStyle,
    FixLast,
    ExplainLast,
    FixChecks,
    NewView,
    NewWelcome,
    NewWorkstream,
    NewProject,
    NewMachine,
    RenameView,
    RenameWorkstream,
    RenameProject,
    CloseView,
    CloseWorkstream,
    DeleteProject,
    ForgetMachine,
    DeleteMerged,
    BriefEvery,
    BriefThis,
    BriefHighlighted,
    Search,
    Unread,
    Reorder,
    OpenPull,
    YankPull,
    PastedWorktree,
    PastedReview,
    PastedTackle,
    Swap,
    ViewLeft,
    ViewDown,
    ViewUp,
    ViewRight,
    Grow,
    Shrink,
    Collapse,
    Menu,
    Workstreams,
    Agents,
    Dirs,
    Dictate,
    ReadAloud,
    StopAloud,
}

const fn spec(
    name: &'static str,
    keywords: &'static [&'static str],
    key: Key,
    nouns: &'static [Noun],
    reach: Reach,
) -> Spec {
    Spec { name, keywords, key, nouns, reach }
}

const fn anywhere(name: &'static str, keywords: &'static [&'static str], chord: Chord) -> Spec {
    spec(name, keywords, Key::Chord(chord), EVERY, Reach::Any)
}

impl Action {
    #[must_use]
    pub const fn spec(self) -> Spec {
        use Reach::{Any, Here};
        let menu = Key::Menu;
        match self {
            Action::SearchSkills => {
                spec("Search skills", &["slash command", "find"], menu(&['/']), VIEW, Any)
            },
            Action::Ask => spec("Ask", &["question", "chat"], menu(&['a']), EVERY, Any),
            Action::FileIssue => {
                spec("File issue", &["bug", "report", "github"], menu(&['i']), CHECKED_OUT, Any)
            },
            Action::Review => spec(
                "/thermo-nuclear-review",
                &["critique", "audit"],
                menu(&['c', 't']),
                VIEW,
                Any,
            ),
            Action::IllegalStates => spec(
                "/make-illegal-states-unrepresentable",
                &["types", "model"],
                menu(&['c', 'm']),
                VIEW,
                Any,
            ),
            Action::TigerStyle => {
                spec("/tigerstyle", &["style", "safety"], menu(&['c', 's']), VIEW, Any)
            },
            Action::FixLast => {
                spec("fix the last command", &["error", "shell"], menu(&['c', 'f']), VIEW, Any)
            },
            Action::ExplainLast => spec(
                "explain the last command",
                &["error", "shell", "why"],
                menu(&['c', 'e']),
                VIEW,
                Any,
            ),
            Action::FixChecks => {
                let checks = menu(&['c', 'c']);
                spec("fix the failing checks", &["ci", "pull request"], checks, VIEW, Any)
            },
            Action::NewView => {
                let new = menu(&['n', 'v']);
                spec("new view", &["pane", "split", "terminal", "shell"], new, VIEW, Any)
            },
            Action::NewWelcome => {
                let new = menu(&['n', 'h']);
                spec("new welcome view", &["home", "start", "greeting"], new, VIEW, Any)
            },
            Action::NewWorkstream => {
                spec("new workstream", &["tab", "task"], menu(&['n', 'w']), IN_A_PROJECT, Any)
            },
            Action::NewProject => {
                spec("new project", &["folder", "directory", "add"], menu(&['n', 'p']), EVERY, Any)
            },
            Action::NewMachine => {
                spec("new machine", &["ssh", "host", "remote"], menu(&['n', 'm']), EVERY, Any)
            },
            Action::RenameView => {
                let keywords = &["name", "pane", "terminal", "title"];
                spec("Rename the view", keywords, menu(&['r']), VIEW, Any)
            },
            Action::RenameWorkstream => {
                spec("Rename the workstream", &["name", "title"], menu(&['r']), WORKSTREAM, Any)
            },
            Action::RenameProject => {
                spec("Rename the project", &["name", "title"], menu(&['r']), PROJECT, Any)
            },
            Action::CloseView => {
                let keywords = &["pane", "kill", "exit", "terminal"];
                spec("close the view", keywords, menu(&['d', 'd']), VIEW, Here)
            },
            Action::CloseWorkstream => spec(
                "close the workstream",
                &["kill", "end", "terminals"],
                menu(&['d', 'd']),
                WORKSTREAM,
                Any,
            ),
            Action::DeleteProject => spec(
                "delete the project",
                &["remove", "forget", "close", "workstreams"],
                menu(&['d', 'd']),
                PROJECT,
                Any,
            ),
            Action::ForgetMachine => {
                let forget = menu(&['d', 'd']);
                spec("forget the machine", &["remove", "ssh", "host"], forget, MACHINE, Any)
            },
            Action::DeleteMerged => spec(
                "delete merged worktrees",
                &["prune", "clean", "branches"],
                menu(&['d', 'm']),
                CHECKED_OUT,
                Any,
            ),
            Action::BriefEvery => {
                spec("Every session", &["summary", "all"], menu(&['b', 'z']), EVERY, Any)
            },
            Action::BriefThis => {
                spec("This session", &["summary", "project"], menu(&['b', 's']), EVERY, Any)
            },
            Action::BriefHighlighted => {
                spec("Highlighted", &["summary", "row", "agent"], menu(&['b', 'h']), EVERY, Any)
            },
            Action::Search => spec("Search", &["find", "filter"], Key::Row(&['/']), EVERY, Any),
            Action::Unread => spec("Unread", &["mark", "read", "dot"], Key::Row(&['u']), VIEW, Any),
            Action::Reorder => {
                spec("Reorder", &["move", "select"], Key::Row(&['v']), IN_A_PROJECT, Any)
            },
            Action::OpenPull => {
                spec("Open PR", &["browser", "pull request"], Key::Row(&['o']), CHECKED_OUT, Any)
            },
            Action::YankPull => {
                spec("PR link", &["copy", "url"], Key::Row(&[YANK, YANK_PULL]), CHECKED_OUT, Any)
            },
            Action::PastedWorktree => {
                spec("worktree", &["checkout", "branch"], Key::Pasted('w'), EVERY, Any)
            },
            Action::PastedReview => {
                let review = Key::Pasted('r');
                spec("/thermo-nuclear-review", &["critique", "pull request"], review, EVERY, Any)
            },
            Action::PastedTackle => {
                spec("tackle", &["issue", "work"], Key::Pasted('t'), EVERY, Any)
            },
            Action::Swap => anywhere("Sidebar or views", &["focus", "switch"], SWAP),
            Action::ViewLeft => {
                anywhere("View left, or the workstream before", &["focus", "move"], LEFT)
            },
            Action::ViewDown => anywhere("View below", &["focus", "move"], DOWN),
            Action::ViewUp => anywhere("View above", &["focus", "move"], UP),
            Action::ViewRight => {
                anywhere("View right, or the workstream after", &["focus", "move"], RIGHT)
            },
            Action::Grow => anywhere("Grow the view", &["resize", "bigger"], GROW),
            Action::Shrink => anywhere("Shrink the view", &["resize", "smaller"], SHRINK),
            Action::Collapse => anywhere("Collapse sidebar", &["hide", "fold"], COLLAPSE),
            Action::Menu => anywhere("Menu over views", &["which-key", "actions"], MENU),
            Action::Workstreams => {
                anywhere("Pick a workstream", &["finder", "switch"], WORKSTREAMS)
            },
            Action::Agents => anywhere("Pick an agent", &["finder", "claude"], AGENTS),
            Action::Dirs => anywhere("Pick a directory", &["finder", "folder"], DIRS),
            Action::Dictate => anywhere("Hold to dictate", &["voice", "speech"], dictation::KEY),
            Action::ReadAloud => anywhere("Read aloud, pause", &["speak", "voice"], aloud::KEY),
            Action::StopAloud => anywhere("Stop reading", &["speak", "voice"], aloud::STOPS),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.spec().name
    }

    #[must_use]
    pub const fn key(self) -> Key {
        self.spec().key
    }

    #[must_use]
    pub fn fits(self, noun: Noun, on: &Machine) -> bool {
        let Spec { nouns, reach, .. } = self.spec();
        if !nouns.contains(&noun) {
            return false;
        }
        match (on, reach) {
            (Machine::Local, Reach::Any | Reach::Here) | (Machine::Remote(_), Reach::Any) => true,
            (Machine::Remote(_), Reach::Here) => false,
        }
    }

    #[must_use]
    pub const fn starts_agent(self) -> bool {
        matches!(self.key(), Key::Menu([SKILLS, _]))
    }

    #[must_use]
    pub const fn skill(self) -> Option<&'static str> {
        match self {
            Action::Review | Action::PastedReview => Some(REVIEW),
            Action::IllegalStates => Some("make-illegal-states-unrepresentable"),
            Action::TigerStyle => Some("tigerstyle"),
            _ => None,
        }
    }

    pub fn menu() -> impl Iterator<Item = (Action, &'static [char])> {
        Action::ALL.into_iter().filter_map(|action| match action.key() {
            Key::Menu(path) => Some((action, path)),
            Key::Row(_) | Key::Pasted(_) | Key::Chord(_) => None,
        })
    }

    pub fn row() -> impl Iterator<Item = (Action, &'static [char])> {
        Action::ALL.into_iter().filter_map(|action| match action.key() {
            Key::Row(path) => Some((action, path)),
            Key::Menu(_) | Key::Pasted(_) | Key::Chord(_) => None,
        })
    }

    pub fn pasted() -> impl Iterator<Item = (Action, char)> {
        Action::ALL.into_iter().filter_map(|action| match action.key() {
            Key::Pasted(key) => Some((action, key)),
            Key::Menu(_) | Key::Row(_) | Key::Chord(_) => None,
        })
    }

    pub fn anywhere() -> impl Iterator<Item = (Action, Chord)> {
        Action::ALL.into_iter().filter_map(|action| match action.key() {
            Key::Chord(chord) => Some((action, chord)),
            Key::Menu(_) | Key::Row(_) | Key::Pasted(_) => None,
        })
    }
}

#[must_use]
pub fn group_name(groups: &[(char, &'static str)], key: char) -> Option<&'static str> {
    groups.iter().find(|(group, _)| *group == key).map(|(_, name)| *name)
}

const fn is_group(groups: &[(char, &str)], key: char) -> bool {
    let mut at = 0;
    while at < groups.len() {
        if groups[at].0 == key {
            return true;
        }
        at += 1;
    }
    false
}

const fn grouped(path: &[char], groups: &[(char, &str)]) -> bool {
    match path {
        [only] => !is_group(groups, *only),
        [group, _] => is_group(groups, *group),
        _ => false,
    }
}

const _: () = {
    let mut at = 0;
    while at < Action::ALL.len() {
        match Action::ALL[at].key() {
            Key::Menu(path) => {
                let menu = "a menu path is a key, or a named group's key then one of its own";
                assert!(grouped(path, &MENU_GROUPS), "{}", menu);
            },
            Key::Row(path) => {
                let row = "a sidebar path is a key, or a named group's key then one of its own";
                assert!(grouped(path, &ROW_GROUPS), "{}", row);
            },
            Key::Pasted(_) | Key::Chord(_) => {},
        }
        at += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(one: Action, other: Action) -> bool {
        let (one, other) = (one.spec(), other.spec());
        one.nouns.iter().any(|noun| other.nouns.contains(noun))
    }

    #[test]
    fn two_actions_on_one_key_fit_no_row_alike() {
        for (at, one) in Action::ALL.into_iter().enumerate() {
            for other in Action::ALL.into_iter().skip(at + 1) {
                if one.key() == other.key() {
                    assert!(!overlap(one, other), "{one:?} and {other:?} on {:?}", one.key());
                }
            }
        }
    }

    #[test]
    fn the_skills_group_s_actions_alone_start_an_agent() {
        let starting = Action::ALL.into_iter().filter(|action| action.starts_agent());
        let starting: Vec<Action> = starting.collect();
        assert_eq!(starting, [
            Action::Review,
            Action::IllegalStates,
            Action::TigerStyle,
            Action::FixLast,
            Action::ExplainLast,
            Action::FixChecks,
        ]);
    }

    #[test]
    fn a_skill_s_action_is_named_as_its_slash_command() {
        for action in Action::ALL {
            if let Some(skill) = action.skill() {
                assert_eq!(action.name(), format!("/{skill}"), "{action:?}");
            }
        }
    }

    #[test]
    fn keywords_add_words_the_name_has_not() {
        for action in Action::ALL {
            let Spec { name, keywords, .. } = action.spec();
            assert!(!keywords.is_empty(), "{action:?}");
            let named: Vec<String> = name.split([' ', '/', '-']).map(str::to_lowercase).collect();
            for keyword in keywords {
                assert!(!named.contains(&(*keyword).to_owned()), "{action:?} has {keyword}");
            }
        }
    }

    #[test]
    fn an_action_here_alone_is_kept_from_another_machine_s_rows() {
        let vps = Machine::Remote("me@vps".parse().unwrap());
        assert!(Action::CloseView.fits(Noun::View, &Machine::Local));
        assert!(!Action::CloseView.fits(Noun::View, &vps));
        assert!(!Action::CloseView.fits(Noun::Workstream, &Machine::Local));
        assert!(Action::CloseWorkstream.fits(Noun::Workstream, &vps));
    }
}
