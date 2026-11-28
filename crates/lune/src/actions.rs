use crossterm::event::{KeyCode, KeyModifiers};

use crate::aloud;
use crate::dictation;

pub type Chord = (KeyCode, KeyModifiers);

pub const SWAP: Chord = (KeyCode::Char('a'), KeyModifiers::CONTROL);

pub const COLLAPSE: Chord = (KeyCode::Char('q'), KeyModifiers::CONTROL);

pub const MENU: Chord = (KeyCode::Char('a'), KeyModifiers::ALT);

pub const PALETTE: Chord = (KeyCode::Char('k'), KeyModifiers::SUPER);

pub const WORKTREES: Chord = (KeyCode::Char('j'), KeyModifiers::SUPER);

pub const LEFT: Chord = (KeyCode::Char('h'), KeyModifiers::CONTROL);

pub const DOWN: Chord = (KeyCode::Char('j'), KeyModifiers::CONTROL);

pub const UP: Chord = (KeyCode::Char('k'), KeyModifiers::CONTROL);

pub const RIGHT: Chord = (KeyCode::Char('l'), KeyModifiers::CONTROL);

pub const GROW: Chord = (KeyCode::Char('='), KeyModifiers::ALT);

pub const SHRINK: Chord = (KeyCode::Char('-'), KeyModifiers::ALT);

pub const AGENTS: Chord = (KeyCode::Char('b'), KeyModifiers::CONTROL);

pub const PROJECTS: Chord = (KeyCode::Char('o'), KeyModifiers::SUPER);

pub const REVIEW: &str = "thermo-nuclear-review";

pub const YANK: char = 'y';

pub const YANK_PULL: char = 'p';

pub const OPEN: char = 'o';

pub const SKILLS: char = 'c';

pub const QUIT: char = 'q';

pub const GIT: char = 'g';

pub const MENU_GROUPS: [(char, &str); 5] = [
    (SKILLS, "Skills"),
    ('n', "New"),
    ('d', "Close"),
    ('b', "Brief"),
    (GIT, "Git"),
];

pub const ROW_GROUPS: [(char, &str); 2] = [(YANK, "Yank"), (OPEN, "Open")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Noun {
    View,
    Worktree,
    Project,
    Machine,
}

const EVERY: &[Noun] = &[Noun::View, Noun::Worktree, Noun::Project, Noun::Machine];

const VIEW: &[Noun] = &[Noun::View];

const WORKTREE: &[Noun] = &[Noun::Worktree];

const PROJECT: &[Noun] = &[Noun::Project];

const MACHINE: &[Noun] = &[Noun::Machine];

const CHECKED_OUT: &[Noun] = &[Noun::View, Noun::Worktree];

const IN_A_PROJECT: &[Noun] = &[Noun::View, Noun::Worktree, Noun::Project];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Menu(&'static [char]),
    Row(&'static [char]),
    RowAndMenu(&'static [char]),
    Pasted(char),
    Chord(Chord),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub name: &'static str,
    pub keywords: &'static [&'static str],
    pub key: Key,
    pub nouns: &'static [Noun],
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
    NewBrowser,
    NewReview,
    ShowTabs,
    ShowPanes,
    NewWorktree,
    NewMachine,
    RenameView,
    RenameWorktree,
    RenameProject,
    CloseView,
    CloseWorktree,
    CloseProject,
    ForgetMachine,
    DeleteMerged,
    Rebase,
    BriefEvery,
    BriefThis,
    BriefHighlighted,
    Search,
    Unread,
    Reorder,
    OpenPull,
    OpenWorktree,
    YankPull,
    Quit,
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
    Palette,
    Worktrees,
    Agents,
    Projects,
    Dictate,
    ReadAloud,
    StopAloud,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Search,
    Unread,
    Reorder,
    Quit,
}

impl Command {
    #[must_use]
    pub const fn of(action: Action) -> Option<Command> {
        match action {
            Action::Search => Some(Command::Search),
            Action::Unread => Some(Command::Unread),
            Action::Reorder => Some(Command::Reorder),
            Action::Quit => Some(Command::Quit),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pasted {
    Worktree,
    Review,
    Tackle,
}

impl Pasted {
    const ALL: [Pasted; 3] = [Pasted::Worktree, Pasted::Review, Pasted::Tackle];

    #[must_use]
    pub const fn key(self) -> char {
        match self {
            Pasted::Worktree => 'w',
            Pasted::Review => 'r',
            Pasted::Tackle => 't',
        }
    }

    #[must_use]
    pub const fn action(self) -> Action {
        match self {
            Pasted::Worktree => Action::PastedWorktree,
            Pasted::Review => Action::PastedReview,
            Pasted::Tackle => Action::PastedTackle,
        }
    }
}

const fn spec(
    name: &'static str,
    keywords: &'static [&'static str],
    key: Key,
    nouns: &'static [Noun],
) -> Spec {
    Spec {
        name,
        keywords,
        key,
        nouns,
    }
}

const fn anywhere(name: &'static str, keywords: &'static [&'static str], chord: Chord) -> Spec {
    spec(name, keywords, Key::Chord(chord), EVERY)
}

impl Action {
    #[must_use]
    pub const fn spec(self) -> Spec {
        let menu = Key::Menu;
        match self {
            Action::SearchSkills => spec(
                "Search skills",
                &["slash command", "find"],
                menu(&['/']),
                VIEW,
            ),
            Action::Ask => spec("Ask", &["question", "chat"], menu(&['a']), EVERY),
            Action::FileIssue => spec(
                "File issue",
                &["bug", "report", "github"],
                menu(&['i']),
                CHECKED_OUT,
            ),
            Action::Review => spec(
                "/thermo-nuclear-review",
                &["critique", "audit"],
                menu(&['c', 't']),
                VIEW,
            ),
            Action::IllegalStates => spec(
                "/make-illegal-states-unrepresentable",
                &["types", "model"],
                menu(&['c', 'm']),
                VIEW,
            ),
            Action::TigerStyle => {
                spec("/tigerstyle", &["style", "safety"], menu(&['c', 's']), VIEW)
            }
            Action::FixLast => spec(
                "fix the last command",
                &["error", "shell"],
                menu(&['c', 'f']),
                VIEW,
            ),
            Action::ExplainLast => spec(
                "explain the last command",
                &["error", "shell", "why"],
                menu(&['c', 'e']),
                VIEW,
            ),
            Action::FixChecks => {
                let checks = menu(&['c', 'c']);
                spec(
                    "fix the failing checks",
                    &["ci", "pull request"],
                    checks,
                    VIEW,
                )
            }
            Action::NewView => {
                let new = menu(&['n', 'v']);
                spec(
                    "new view",
                    &["pane", "split", "tab", "terminal", "shell"],
                    new,
                    VIEW,
                )
            }
            Action::NewWelcome => {
                let new = menu(&['n', 'h']);
                spec(
                    "new welcome view",
                    &["home", "start", "greeting"],
                    new,
                    VIEW,
                )
            }
            Action::NewBrowser => {
                let new = menu(&['n', 'b']);
                spec(
                    "new browser view…",
                    &["url", "page", "site", "http"],
                    new,
                    CHECKED_OUT,
                )
            }
            Action::NewReview => {
                let new = menu(&['n', 'r']);
                spec(
                    "new review view",
                    &["changes", "patch", "pull request", "split"],
                    new,
                    VIEW,
                )
            }
            Action::ShowTabs => spec(
                "Show as tabs",
                &["arrange", "layout", "views"],
                menu(&['t']),
                VIEW,
            ),
            Action::ShowPanes => spec(
                "Show as panes",
                &["arrange", "layout", "split"],
                menu(&['s']),
                VIEW,
            ),
            Action::NewWorktree => {
                let keywords = &["branch", "task"];
                spec("new worktree…", keywords, menu(&['n', 'w']), IN_A_PROJECT)
            }
            Action::NewMachine => spec(
                "new machine…",
                &["ssh", "host", "remote"],
                menu(&['n', 'm']),
                EVERY,
            ),
            Action::RenameView => {
                let keywords = &["name", "pane", "terminal", "title"];
                spec("Rename the view…", keywords, menu(&['r']), VIEW)
            }
            Action::RenameWorktree => spec(
                "Rename the worktree…",
                &["name", "title"],
                menu(&['r']),
                WORKTREE,
            ),
            Action::RenameProject => spec(
                "Rename the project…",
                &["name", "title"],
                menu(&['r']),
                PROJECT,
            ),
            Action::CloseView => {
                let keywords = &["pane", "kill", "exit", "terminal"];
                spec("close the view", keywords, menu(&['d', 'd']), VIEW)
            }
            Action::CloseWorktree => spec(
                "close the worktree",
                &["kill", "end", "terminals"],
                menu(&['d', 'd']),
                WORKTREE,
            ),
            Action::CloseProject => spec(
                "close the project",
                &["remove", "forget", "delete", "worktrees"],
                menu(&['d', 'd']),
                PROJECT,
            ),
            Action::ForgetMachine => {
                let forget = menu(&['d', 'd']);
                spec(
                    "forget the machine",
                    &["remove", "ssh", "host"],
                    forget,
                    MACHINE,
                )
            }
            Action::DeleteMerged => spec(
                "delete merged worktrees",
                &["prune", "clean", "branches"],
                menu(&['d', 'm']),
                CHECKED_OUT,
            ),
            Action::Rebase => spec(
                "rebase onto upstream",
                &["update", "main", "conflicts", "branch"],
                menu(&[GIT, 'r']),
                CHECKED_OUT,
            ),
            Action::BriefEvery => spec(
                "Brief every session",
                &["summary", "all"],
                menu(&['b', 'z']),
                EVERY,
            ),
            Action::BriefThis => spec(
                "Brief this session",
                &["summary", "project"],
                menu(&['b', 's']),
                EVERY,
            ),
            Action::BriefHighlighted => {
                let keywords = &["summary", "row", "agent"];
                spec("Brief the highlighted", keywords, menu(&['b', 'h']), EVERY)
            }
            Action::Search => spec("Search", &["find", "filter"], Key::Row(&['/']), EVERY),
            Action::Unread => spec("Unread", &["mark", "read", "dot"], Key::Row(&['u']), VIEW),
            Action::Reorder => spec(
                "Reorder",
                &["move", "select"],
                Key::Row(&['v']),
                IN_A_PROJECT,
            ),
            Action::OpenPull => {
                let keywords = &["browser", "pull request"];
                spec("Open the PR", keywords, Key::Row(&[OPEN, 'p']), CHECKED_OUT)
            }
            Action::OpenWorktree => {
                let keywords = &["checkout", "branch", "unopened"];
                spec(
                    "Open a worktree",
                    keywords,
                    Key::Row(&[OPEN, 'w']),
                    IN_A_PROJECT,
                )
            }
            Action::YankPull => {
                let yank = Key::Row(&[YANK, YANK_PULL]);
                spec("Yank the PR link", &["copy", "url"], yank, CHECKED_OUT)
            }
            Action::Quit => {
                let quit = Key::RowAndMenu(&[QUIT]);
                spec("Quit lune", &["detach", "exit", "leave"], quit, EVERY)
            }
            Action::PastedWorktree => {
                let worktree = Key::Pasted(Pasted::Worktree.key());
                spec("worktree", &["checkout", "branch"], worktree, EVERY)
            }
            Action::PastedReview => {
                let review = Key::Pasted(Pasted::Review.key());
                spec(
                    "/thermo-nuclear-review",
                    &["critique", "pull request"],
                    review,
                    EVERY,
                )
            }
            Action::PastedTackle => {
                let tackle = Key::Pasted(Pasted::Tackle.key());
                spec("tackle", &["issue", "work"], tackle, EVERY)
            }
            Action::Swap => anywhere("Sidebar or views", &["focus", "switch"], SWAP),
            Action::ViewLeft => {
                let name = "View left, the worktree before, or the sidebar";
                anywhere(name, &["focus", "move"], LEFT)
            }
            Action::ViewDown => anywhere("View below", &["focus", "move"], DOWN),
            Action::ViewUp => anywhere("View above", &["focus", "move"], UP),
            Action::ViewRight => anywhere(
                "View right, or the worktree after",
                &["focus", "move"],
                RIGHT,
            ),
            Action::Grow => anywhere("Grow the view", &["resize", "bigger"], GROW),
            Action::Shrink => anywhere("Shrink the view", &["resize", "smaller"], SHRINK),
            Action::Collapse => anywhere("Collapse sidebar", &["hide", "fold"], COLLAPSE),
            Action::Menu => anywhere("Menu over views", &["which-key", "actions"], MENU),
            Action::Palette => anywhere("Pick an action", &["palette", "command", "run"], PALETTE),
            Action::Worktrees => anywhere("Pick a worktree", &["finder", "switch"], WORKTREES),
            Action::Agents => anywhere("Pick an agent", &["finder", "claude"], AGENTS),
            Action::Projects => {
                let keywords = &["finder", "folder", "directory", "repository", "new", "add"];
                anywhere("Open a project", keywords, PROJECTS)
            }
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
    pub fn fits(self, noun: Noun) -> bool {
        self.spec().nouns.contains(&noun)
    }

    #[must_use]
    pub const fn in_palette(self) -> bool {
        match (self, self.key()) {
            (_, Key::Pasted(_)) | (Action::Dictate | Action::Palette, _) => false,
            (_, Key::Menu(_) | Key::Row(_) | Key::RowAndMenu(_) | Key::Chord(_)) => true,
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
        Action::ALL
            .into_iter()
            .filter_map(|action| match action.key() {
                Key::Menu(path) | Key::RowAndMenu(path) => Some((action, path)),
                Key::Row(_) | Key::Pasted(_) | Key::Chord(_) => None,
            })
    }

    pub fn row() -> impl Iterator<Item = (Action, &'static [char])> {
        Action::ALL
            .into_iter()
            .filter_map(|action| match action.key() {
                Key::Row(path) | Key::RowAndMenu(path) => Some((action, path)),
                Key::Menu(_) | Key::Pasted(_) | Key::Chord(_) => None,
            })
    }

    pub fn pasted() -> impl Iterator<Item = (Action, char)> {
        Action::ALL
            .into_iter()
            .filter_map(|action| match action.key() {
                Key::Pasted(key) => Some((action, key)),
                Key::Menu(_) | Key::Row(_) | Key::RowAndMenu(_) | Key::Chord(_) => None,
            })
    }

    pub fn anywhere() -> impl Iterator<Item = (Action, Chord)> {
        Action::ALL
            .into_iter()
            .filter_map(|action| match action.key() {
                Key::Chord(chord) => Some((action, chord)),
                Key::Menu(_) | Key::Row(_) | Key::RowAndMenu(_) | Key::Pasted(_) => None,
            })
    }
}

#[must_use]
pub fn group_name(groups: &[(char, &'static str)], key: char) -> Option<&'static str> {
    groups
        .iter()
        .find(|(group, _)| *group == key)
        .map(|(_, name)| *name)
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
        let action = Action::ALL[at];
        let alone = matches!(action.key(), Key::Row([_]) | Key::RowAndMenu(_));
        let command = "a key of the sidebar's own is a command's key, and a command's is its own";
        assert!(alone == Command::of(action).is_some(), "{}", command);
        match action.key() {
            Key::Menu(path) => {
                let menu = "a menu path is a key, or a named group's key then one of its own";
                assert!(grouped(path, &MENU_GROUPS), "{}", menu);
            }
            Key::Row(path) => {
                let row = "a sidebar path is a key, or a named group's key then one of its own";
                assert!(grouped(path, &ROW_GROUPS), "{}", row);
            }
            Key::RowAndMenu(path) => {
                let one = "a path of the sidebar and the menu is one key";
                assert!(path.len() == 1, "{}", one);
                let row = "a key of the sidebar and the menu opens no group of the sidebar";
                assert!(!is_group(&ROW_GROUPS, path[0]), "{}", row);
                let menu = "a key of the sidebar and the menu opens no group of the menu";
                assert!(!is_group(&MENU_GROUPS, path[0]), "{}", menu);
            }
            Key::Pasted(key) => {
                let answers = "a key for a pasted link is a Pasted's key";
                assert!(pasted_key(key), "{}", answers);
            }
            Key::Chord(_) => {}
        }
        at += 1;
    }
    let mut at = 0;
    while at < Pasted::ALL.len() {
        let pasted = Pasted::ALL[at];
        let keyed = matches!(pasted.action().key(), Key::Pasted(key) if key == pasted.key());
        assert!(keyed, "{}", "a Pasted's action is keyed as it");
        at += 1;
    }
};

const fn pasted_key(key: char) -> bool {
    let mut at = 0;
    while at < Pasted::ALL.len() {
        if Pasted::ALL[at].key() == key {
            return true;
        }
        at += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(one: Action, other: Action) -> bool {
        let (one, other) = (one.spec(), other.spec());
        one.nouns.iter().any(|noun| other.nouns.contains(noun))
    }

    #[test]
    fn two_actions_on_one_key_fit_no_row_alike() {
        let menu = |action: Action| Action::menu().find(|(it, _)| *it == action).map(|it| it.1);
        let row = |action: Action| Action::row().find(|(it, _)| *it == action).map(|it| it.1);
        for (at, one) in Action::ALL.into_iter().enumerate() {
            for other in Action::ALL.into_iter().skip(at + 1) {
                let in_the_menu = menu(one).is_some_and(|path| menu(other) == Some(path));
                let in_the_sidebar = row(one).is_some_and(|path| row(other) == Some(path));
                if one.key() == other.key() || in_the_menu || in_the_sidebar {
                    assert!(
                        !overlap(one, other),
                        "{one:?} and {other:?} on {:?}",
                        one.key()
                    );
                }
            }
        }
    }

    #[test]
    fn the_skills_group_s_actions_alone_start_an_agent() {
        let starting = Action::ALL
            .into_iter()
            .filter(|action| action.starts_agent());
        let starting: Vec<Action> = starting.collect();
        assert_eq!(
            starting,
            [
                Action::Review,
                Action::IllegalStates,
                Action::TigerStyle,
                Action::FixLast,
                Action::ExplainLast,
                Action::FixChecks,
            ]
        );
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
                assert!(
                    !named.contains(&(*keyword).to_owned()),
                    "{action:?} has {keyword}"
                );
            }
        }
    }
}
