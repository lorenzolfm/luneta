use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub use lune_domain::agent::Harness;
use lune_domain::projects::Argv;
use luned::api;

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::state;

const FILE: &str = "agent";

const FILE_BYTES_MAX: u64 = 64;

const _: () = assert!(FILE_BYTES_MAX >= 16);

pub const fn name(harness: Harness) -> &'static str {
    harness.program()
}

pub const fn next(harness: Harness) -> Harness {
    match harness {
        Harness::ClaudeCode => Harness::Pi,
        Harness::Pi => Harness::Antigravity,
        Harness::Antigravity => Harness::Codex,
        Harness::Codex => Harness::ClaudeCode,
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
        Harness::ClaudeCode | Harness::Antigravity => format!("/{skill}"),
        Harness::Pi => format!("/skill:{skill}"),
        Harness::Codex => format!("${skill}"),
    }
}

pub fn of(agent: &api::Agent) -> Option<Harness> {
    match agent.runs_in() {
        api::Harness::ClaudeCode => Some(Harness::ClaudeCode),
        api::Harness::Pi => Some(Harness::Pi),
        api::Harness::Antigravity => Some(Harness::Antigravity),
        api::Harness::Codex => Some(Harness::Codex),
        api::Harness::Unspecified => None,
    }
}

pub fn on_skill(harness: Harness, skill: &str, given: Option<&str>) -> Result<Argv, String> {
    let spelled = spelled(harness, skill);
    let prompt = match given {
        Some(given) => format!("{spelled} {given}"),
        None => spelled,
    };
    let words = match harness {
        Harness::ClaudeCode | Harness::Pi | Harness::Codex => {
            vec![name(harness).to_owned(), prompt]
        }
        Harness::Antigravity => vec![name(harness).to_owned(), "-i".to_owned(), prompt],
    };
    Argv::parse(words)
}

#[derive(Debug)]
struct StoreFile {
    dir: PathBuf,
    path: PathBuf,
}

impl StoreFile {
    fn new(path: PathBuf) -> Result<Self, String> {
        let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
            return Err(format!("{}: no parent folder", path.display()));
        };
        Ok(StoreFile {
            dir: dir.to_owned(),
            path,
        })
    }
}

#[derive(Debug)]
enum Backing {
    Memory,
    Unready(StoreFile),
    File(StoreFile),
}

#[derive(Debug)]
pub struct Kept {
    harness: Harness,
    backing: Backing,
}

impl Default for Kept {
    fn default() -> Self {
        Kept {
            harness: Harness::ClaudeCode,
            backing: Backing::Memory,
        }
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
        let store = match StoreFile::new(file) {
            Ok(store) => store,
            Err(why) => return (Kept::default(), Some(why)),
        };
        if let Err(error) = fs::create_dir_all(&store.dir) {
            let why = format!("{}: {error}", store.path.display());
            let kept = Kept {
                harness: Harness::ClaudeCode,
                backing: Backing::Unready(store),
            };
            return (kept, Some(why));
        }
        let mut kept = Kept {
            harness: Harness::ClaudeCode,
            backing: Backing::File(store),
        };
        let unread = kept.reload().err();
        (kept, unread)
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    pub fn file(&self) -> Option<&Path> {
        match &self.backing {
            Backing::File(store) => Some(&store.path),
            Backing::Memory | Backing::Unready(_) => None,
        }
    }

    pub fn reload(&mut self) -> Result<(), String> {
        let Backing::File(store) = &self.backing else {
            return Ok(());
        };
        let text = match read_bounded(&store.path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.harness = Harness::ClaudeCode;
                return Ok(());
            }
            Err(error) => return Err(format!("{}: {error}", store.path.display())),
        };
        let program = text.trim();
        match Harness::from_program(program) {
            Some(harness) => {
                self.harness = harness;
                Ok(())
            }
            None => Err(format!(
                "{}: {program:?} is no harness lune knows",
                store.path.display()
            )),
        }
    }

    pub fn keep(&mut self, harness: Harness) -> Result<(), String> {
        let store = match &self.backing {
            Backing::Memory => {
                self.harness = harness;
                return Ok(());
            }
            Backing::File(store) | Backing::Unready(store) => store,
        };
        let unfinished = store.dir.join(format!("{FILE}.{}", state::unfinished()));
        let written = fs::create_dir_all(&store.dir)
            .and_then(|()| fs::write(&unfinished, format!("{}\n", name(harness))))
            .and_then(|()| fs::rename(&unfinished, &store.path));
        if let Err(error) = written {
            let _ = fs::remove_file(&unfinished);
            return Err(format!("{}: {error}", store.path.display()));
        }
        self.backing = match std::mem::replace(&mut self.backing, Backing::Memory) {
            Backing::File(store) | Backing::Unready(store) => Backing::File(store),
            Backing::Memory => Backing::Memory,
        };
        self.harness = harness;
        Ok(())
    }
}

fn read_bounded(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(FILE_BYTES_MAX + 1).read_to_end(&mut bytes)?;
    let len = u64::try_from(bytes.len()).expect("a small bounded file length fits a u64");
    if len > FILE_BYTES_MAX {
        return Err(std::io::Error::new(
            ErrorKind::FileTooLarge,
            format!("it holds more than {FILE_BYTES_MAX} bytes"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))
}

pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Agent)
}

#[cfg(test)]
mod tests {
    use lune_domain::projects::{LINE_BYTES_MAX, Line};

    use super::*;

    #[test]
    fn a_skill_starts_an_unnamed_agent_in_its_harness_s_spelling() {
        assert_eq!(
            on_skill(Harness::ClaudeCode, "tigerstyle", None)
                .unwrap()
                .words(),
            ["claude", "/tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::ClaudeCode, "code-review", Some("since main"))
                .unwrap()
                .words(),
            ["claude", "/code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "tigerstyle", None).unwrap().words(),
            ["pi", "/skill:tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "code-review", Some("since main"))
                .unwrap()
                .words(),
            ["pi", "/skill:code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Antigravity, "tigerstyle", None)
                .unwrap()
                .words(),
            ["agy", "-i", "/tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Antigravity, "code-review", Some("since main"))
                .unwrap()
                .words(),
            ["agy", "-i", "/code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Codex, "tigerstyle", None)
                .unwrap()
                .words(),
            ["codex", "$tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Codex, "code-review", Some("since main"))
                .unwrap()
                .words(),
            ["codex", "$code-review since main"]
        );
    }

    #[test]
    fn an_agent_is_refused_words_that_no_command_line_holds() {
        let refused = Err(Line::parse(&"x".repeat(LINE_BYTES_MAX + 1)).unwrap_err());
        let fits = "x".repeat(LINE_BYTES_MAX - "pi -- ".len());
        let argv = Argv::on_words(Harness::Pi, fits.clone(), &[]);
        let argv = argv.expect("pi and the words fill a command line");
        assert_eq!(
            argv.words(),
            ["pi".to_owned(), "--".to_owned(), fits.clone()]
        );
        assert_eq!(
            Argv::on_words(Harness::Pi, format!("{fits}x"), &[]),
            refused
        );
        assert_eq!(
            Argv::on_words(Harness::Pi, "x".repeat(LINE_BYTES_MAX), &[]),
            refused
        );
        assert_eq!(
            on_skill(Harness::Pi, "tdd", Some(&"x".repeat(LINE_BYTES_MAX))),
            refused
        );
    }

    #[test]
    fn a_harness_is_known_by_its_program_s_name() {
        let mut picked = Harness::ClaudeCode;
        for harness in Harness::ALL {
            assert_eq!(Harness::from_program(name(harness)), Some(harness));
            assert_eq!(picked, harness, "the pick steps through every harness");
            picked = next(picked);
        }
        assert_eq!(picked, Harness::ClaudeCode, "and comes back round");
        assert_eq!(Harness::from_program("opencode"), None);
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
        fs::write(&file, "opencode\n").unwrap();
        let (mut kept, unread) = Kept::within(file.clone());
        assert_eq!(kept.harness(), Harness::ClaudeCode);
        assert!(unread.is_some_and(|why| why.contains("\"opencode\" is no harness")));
        kept.keep(Harness::Pi).unwrap();
        assert_eq!(Kept::within(file).0.harness(), Harness::Pi);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let (mut kept, unread) = Kept::within(blocker.join(FILE));
        assert!(unread.is_some_and(|why| why.contains("not a folder")));
        assert_eq!(kept.file(), None);
        assert!(kept.keep(Harness::Pi).is_err());
        assert_eq!(
            kept.harness(),
            Harness::ClaudeCode,
            "a failed write does not diverge memory from disk"
        );

        let file = scratch.path().join("reset").join(FILE);
        let (mut kept, unread) = Kept::within(file.clone());
        assert_eq!(unread, None);
        kept.keep(Harness::Pi).unwrap();
        assert_eq!(kept.harness(), Harness::Pi);
        fs::remove_file(&file).unwrap();
        kept.reload().unwrap();
        assert_eq!(
            kept.harness(),
            Harness::ClaudeCode,
            "removing the agent file returns to the default harness"
        );

        fs::write(
            &file,
            "x".repeat(usize::try_from(FILE_BYTES_MAX + 1).unwrap()),
        )
        .unwrap();
        assert!(kept.reload().is_err());
    }
}
