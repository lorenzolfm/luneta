use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use luned::api;
use luned::projects::Argv;
pub use luned::Harness;

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::state;

const FILE: &str = "agent";

pub const fn name(harness: Harness) -> &'static str {
    harness.program()
}

pub fn named(program: &str) -> Option<Harness> {
    match program {
        "claude" => Some(Harness::ClaudeCode),
        "pi" => Some(Harness::Pi),
        _ => None,
    }
}

pub const fn next(harness: Harness) -> Harness {
    match harness {
        Harness::ClaudeCode => Harness::Pi,
        Harness::Pi => Harness::ClaudeCode,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    ByView(Harness),
    ByYou(Harness),
}

impl Picked {
    pub const fn harness(self) -> Harness {
        match self {
            Picked::ByView(harness) | Picked::ByYou(harness) => harness,
        }
    }

    pub const fn next(self) -> Picked {
        Picked::ByYou(next(self.harness()))
    }
}

pub fn spelled(harness: Harness, skill: &str) -> String {
    match harness {
        Harness::ClaudeCode => format!("/{skill}"),
        Harness::Pi => format!("/skill:{skill}"),
    }
}

pub fn of(agent: &api::Agent) -> Option<Harness> {
    match agent.runs_in() {
        api::Harness::ClaudeCode => Some(Harness::ClaudeCode),
        api::Harness::Pi => Some(Harness::Pi),
        api::Harness::Unspecified => None,
    }
}

pub fn on_skill(harness: Harness, skill: &str, given: Option<&str>) -> Result<Argv, String> {
    let spelled = spelled(harness, skill);
    let prompt = match given {
        Some(given) => format!("{spelled} {given}"),
        None => spelled,
    };
    Argv::parse(vec![name(harness).to_owned(), prompt])
}

#[derive(Debug)]
pub struct Kept {
    harness: Harness,
    file: Option<PathBuf>,
}

impl Default for Kept {
    fn default() -> Self {
        Kept { harness: Harness::ClaudeCode, file: None }
    }
}

impl Kept {
    pub fn shared(state_folder: &state::Folder) -> (Self, Option<String>) {
        match state_folder.path(FILE) {
            Ok(file) => Kept::within(file),
            Err(unset) => (Kept::default(), Some(unset.to_string())),
        }
    }

    pub fn within(file: PathBuf) -> (Self, Option<String>) {
        if let Err(error) = file.parent().map_or(Ok(()), fs::create_dir_all) {
            return (Kept::default(), Some(format!("{}: {error}", file.display())));
        }
        let mut kept = Kept { file: Some(file), ..Kept::default() };
        let unread = kept.reload().err();
        (kept, unread)
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub fn reload(&mut self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", file.display())),
        };
        let program = text.trim();
        match named(program) {
            Some(harness) => {
                self.harness = harness;
                Ok(())
            },
            None => Err(format!("{}: {program:?} is no harness lune knows", file.display())),
        }
    }

    pub fn keep(&mut self, harness: Harness) -> Result<(), String> {
        self.harness = harness;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let folder = file.parent().expect("a file of the state folder is in it");
        let unfinished = folder.join(format!("{FILE}.{}", state::unfinished()));
        let written = fs::create_dir_all(folder)
            .and_then(|()| fs::write(&unfinished, format!("{}\n", name(harness))))
            .and_then(|()| fs::rename(&unfinished, file));
        if written.is_err() {
            let _ = fs::remove_file(&unfinished);
        }
        written.map_err(|error| format!("{}: {error}", file.display()))
    }
}

pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Agent)
}

#[cfg(test)]
mod tests {
    use luned::projects::{Line, LINE_BYTES_MAX};

    use super::*;

    #[test]
    fn a_skill_starts_an_unnamed_agent_in_its_harness_s_spelling() {
        assert_eq!(
            on_skill(Harness::ClaudeCode, "tigerstyle", None).unwrap().words(),
            ["claude", "/tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::ClaudeCode, "code-review", Some("since main")).unwrap().words(),
            ["claude", "/code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "tigerstyle", None).unwrap().words(),
            ["pi", "/skill:tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "code-review", Some("since main")).unwrap().words(),
            ["pi", "/skill:code-review since main"]
        );
    }

    #[test]
    fn an_agent_is_refused_words_that_no_command_line_holds() {
        let refused = Err(Line::parse(&"x".repeat(LINE_BYTES_MAX + 1)).unwrap_err());
        let fits = "x".repeat(LINE_BYTES_MAX - "pi -- ".len());
        let argv = Argv::on_words(Harness::Pi, fits.clone(), &[]);
        let argv = argv.expect("pi and the words fill a command line");
        assert_eq!(argv.words(), ["pi".to_owned(), "--".to_owned(), fits.clone()]);
        assert_eq!(Argv::on_words(Harness::Pi, format!("{fits}x"), &[]), refused);
        assert_eq!(Argv::on_words(Harness::Pi, "x".repeat(LINE_BYTES_MAX), &[]), refused);
        assert_eq!(on_skill(Harness::Pi, "tdd", Some(&"x".repeat(LINE_BYTES_MAX))), refused);
    }

    #[test]
    fn a_harness_is_known_by_its_program_s_name() {
        for harness in [Harness::ClaudeCode, Harness::Pi] {
            assert_eq!(named(name(harness)), Some(harness));
        }
        assert_eq!(named("codex"), None);
    }

    #[test]
    fn the_harness_picked_is_kept_for_every_lune() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("lune").join(FILE);
        let (mut one, unread) = Kept::within(file.clone());
        assert_eq!((one.harness(), unread), (Harness::ClaudeCode, None));
        let (mut two, _) = Kept::within(file.clone());

        one.keep(Harness::Pi).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "pi\n");
        assert_eq!(Kept::within(file.clone()).0.harness(), Harness::Pi);
        two.reload().unwrap();
        assert_eq!(two.harness(), Harness::Pi);
    }

    #[test]
    fn a_file_that_names_no_harness_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(FILE);
        fs::write(&file, "codex\n").unwrap();
        let (mut kept, unread) = Kept::within(file.clone());
        assert_eq!(kept.harness(), Harness::ClaudeCode);
        assert!(unread.is_some_and(|why| why.contains("\"codex\" is no harness")));
        kept.keep(Harness::Pi).unwrap();
        assert_eq!(Kept::within(file).0.harness(), Harness::Pi);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let (mut kept, unread) = Kept::within(blocker.join(FILE));
        assert!(unread.is_some_and(|why| why.contains("not a folder")));
        assert_eq!(kept.file(), None);
        assert_eq!(kept.keep(Harness::Pi), Ok(()));
        assert_eq!(kept.harness(), Harness::Pi);
    }
}
