use crossterm::event::{KeyCode, KeyModifiers};

use crate::aloud;
use crate::dictation;

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

pub const MENU_GROUPS: [(char, &str); 4] =
    [('c', "Skills"), ('n', "New"), ('d', "Delete"), ('b', "Brief")];

pub const ROW_GROUPS: [(char, &str); 1] = [(YANK, "Yank")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Noun {
    Pane,
    Workstream,
    Project,
    Machine,
}

const EVERY: &[Noun] = &[Noun::Pane, Noun::Workstream, Noun::Project, Noun::Machine];

const PANE: &[Noun] = &[Noun::Pane];

const WORKSTREAM: &[Noun] = &[Noun::Workstream];

const PROJECT: &[Noun] = &[Noun::Project];

const MACHINE: &[Noun] = &[Noun::Machine];

const CHECKED_OUT: &[Noun] = &[Noun::Pane, Noun::Workstream];

const IN_A_PROJECT: &[Noun] = &[Noun::Pane, Noun::Workstream, Noun::Project];

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    SearchSkills,
    Ask,
    FileIssue,
    Review,
    IllegalStates,
    TigerStyle,
    FixLast,
    ExplainLast,
    FixChecks,
    NewPane,
    NewWorkstream,
    NewProject,
    NewMachine,
    RenameTerminal,
    RenameWorkstream,
    RenameProject,
    ClosePane,
    CloseWorkstream,
    RemoveProject,
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
    PaneLeft,
    PaneDown,
    PaneUp,
    PaneRight,
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
    pub const ALL: [Action; 47] = [
        Action::SearchSkills,
        Action::Ask,
        Action::FileIssue,
        Action::Review,
        Action::IllegalStates,
        Action::TigerStyle,
        Action::FixLast,
        Action::ExplainLast,
        Action::FixChecks,
        Action::NewPane,
        Action::NewWorkstream,
        Action::NewProject,
        Action::NewMachine,
        Action::RenameTerminal,
        Action::RenameWorkstream,
        Action::RenameProject,
        Action::ClosePane,
        Action::CloseWorkstream,
        Action::RemoveProject,
        Action::ForgetMachine,
        Action::DeleteMerged,
        Action::BriefEvery,
        Action::BriefThis,
        Action::BriefHighlighted,
        Action::Search,
        Action::Unread,
        Action::Reorder,
        Action::OpenPull,
        Action::YankPull,
        Action::PastedWorktree,
        Action::PastedReview,
        Action::PastedTackle,
        Action::Swap,
        Action::PaneLeft,
        Action::PaneDown,
        Action::PaneUp,
        Action::PaneRight,
        Action::Grow,
        Action::Shrink,
        Action::Collapse,
        Action::Menu,
        Action::Workstreams,
        Action::Agents,
        Action::Dirs,
        Action::Dictate,
        Action::ReadAloud,
        Action::StopAloud,
    ];

    #[must_use]
    pub const fn spec(self) -> Spec {
        use Reach::{Any, Here};
        let menu = Key::Menu;
        match self {
            Action::SearchSkills => {
                spec("Search skills", &["slash command", "find"], menu(&['/']), PANE, Here)
            },
            Action::Ask => spec("Ask", &["question", "chat"], menu(&['a']), EVERY, Any),
            Action::FileIssue => {
                spec("File issue", &["bug", "report", "github"], menu(&['i']), CHECKED_OUT, Any)
            },
            Action::Review => spec(
                "/thermo-nuclear-review",
                &["critique", "audit"],
                menu(&['c', 't']),
                PANE,
                Here,
            ),
            Action::IllegalStates => spec(
                "/make-illegal-states-unrepresentable",
                &["types", "model"],
                menu(&['c', 'm']),
                PANE,
                Here,
            ),
            Action::TigerStyle => {
                spec("/tigerstyle", &["style", "safety"], menu(&['c', 's']), PANE, Here)
            },
            Action::FixLast => {
                spec("fix the last command", &["error", "shell"], menu(&['c', 'f']), PANE, Here)
            },
            Action::ExplainLast => spec(
                "explain the last command",
                &["error", "shell", "why"],
                menu(&['c', 'e']),
                PANE,
                Here,
            ),
            Action::FixChecks => {
                let checks = menu(&['c', 'c']);
                spec("fix the failing checks", &["ci", "pull request"], checks, PANE, Here)
            },
            Action::NewPane => {
                spec("new pane", &["split", "terminal", "shell"], menu(&['n', 'p']), PANE, Any)
            },
            Action::NewWorkstream => {
                spec("new workstream", &["tab", "task"], menu(&['n', 'w']), IN_A_PROJECT, Here)
            },
            Action::NewProject => {
                spec("new project", &["folder", "directory", "add"], menu(&['n', 'P']), EVERY, Any)
            },
            Action::NewMachine => {
                spec("new machine", &["ssh", "host", "remote"], menu(&['n', 'm']), EVERY, Any)
            },
            Action::RenameTerminal => {
                spec("Rename the terminal", &["name", "pane", "title"], menu(&['r']), PANE, Any)
            },
            Action::RenameWorkstream => {
                spec("Rename the workstream", &["name", "title"], menu(&['r']), WORKSTREAM, Any)
            },
            Action::RenameProject => {
                spec("Rename the project", &["name", "title"], menu(&['r']), PROJECT, Any)
            },
            Action::ClosePane => {
                spec("close the pane", &["kill", "exit", "terminal"], menu(&['d', 'd']), PANE, Here)
            },
            Action::CloseWorkstream => spec(
                "close the workstream",
                &["kill", "end", "terminals"],
                menu(&['d', 'd']),
                WORKSTREAM,
                Any,
            ),
            Action::RemoveProject => {
                spec("remove the project", &["forget", "delete"], menu(&['d', 'd']), PROJECT, Any)
            },
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
            Action::Unread => spec("Unread", &["mark", "read", "dot"], Key::Row(&['u']), PANE, Any),
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
            Action::Swap => anywhere("Sidebar or panes", &["focus", "switch"], SWAP),
            Action::PaneLeft => {
                anywhere("Pane left, or the workstream before", &["focus", "move"], LEFT)
            },
            Action::PaneDown => anywhere("Pane below", &["focus", "move"], DOWN),
            Action::PaneUp => anywhere("Pane above", &["focus", "move"], UP),
            Action::PaneRight => {
                anywhere("Pane right, or the workstream after", &["focus", "move"], RIGHT)
            },
            Action::Grow => anywhere("Grow the pane", &["resize", "bigger"], GROW),
            Action::Shrink => anywhere("Shrink the pane", &["resize", "smaller"], SHRINK),
            Action::Collapse => anywhere("Collapse sidebar", &["hide", "fold"], COLLAPSE),
            Action::Menu => anywhere("Menu over panes", &["which-key", "actions"], MENU),
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
    pub fn fits(self, noun: Noun, here: bool) -> bool {
        let Spec { nouns, reach, .. } = self.spec();
        nouns.contains(&noun) && (here || reach == Reach::Any)
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn overlap(one: Action, other: Action) -> bool {
        let (one, other) = (one.spec(), other.spec());
        one.nouns.iter().any(|noun| other.nouns.contains(noun))
    }

    #[test]
    fn every_action_is_listed_once() {
        let listed: HashSet<Action> = Action::ALL.into_iter().collect();
        assert_eq!(listed.len(), Action::ALL.len());
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
    fn a_key_two_deep_is_in_a_group_and_a_group_holds_no_action_of_its_own() {
        let deep = |path: &[char], groups: &[(char, &'static str)]| match path {
            [first, _] => group_name(groups, *first).is_some(),
            [only] => group_name(groups, *only).is_none(),
            _ => false,
        };
        for (action, path) in Action::menu() {
            assert!(deep(path, &MENU_GROUPS), "{action:?} on {path:?}");
        }
        for (action, path) in Action::row() {
            assert!(deep(path, &ROW_GROUPS), "{action:?} on {path:?}");
        }
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
        assert!(Action::ClosePane.fits(Noun::Pane, true));
        assert!(!Action::ClosePane.fits(Noun::Pane, false));
        assert!(!Action::ClosePane.fits(Noun::Workstream, true));
        assert!(Action::CloseWorkstream.fits(Noun::Workstream, false));
    }
}
