//! The coding agent a key of the menu starts, in a harness luneta knows:
//! Claude Code or pi. The key says what the agent is to do, a skill or a task
//! in words, and the harness how it is told: how a skill is spelled, how the
//! agent is named, and what it may read without asking you first.
//!
//! The harness is the one you picked last, in any luneta on the host, kept
//! for the next in the file `luneta/agent` of `$XDG_STATE_HOME`, or of
//! `~/.local/state` where that is not set: `Tab` in the menu picks the other.
//! On a pane an agent runs in, the menu starts the next in that one's harness.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use lunetad::api;

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::state;

/// A harness an agent can start in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Pi,
}

/// The name of the file the harness is kept in.
const FILE: &str = "agent";

/// What the menu names `harness` by: the program it runs, which is what the
/// file it is kept in says too.
pub const fn name(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude",
        Harness::Pi => "pi",
    }
}

/// The harness `Tab` picks after `harness`.
pub const fn other(harness: Harness) -> Harness {
    match harness {
        Harness::ClaudeCode => Harness::Pi,
        Harness::Pi => Harness::ClaudeCode,
    }
}

/// The harness an agent runs in, as its daemon says, if luneta knows it.
pub fn of(agent: &api::Agent) -> Option<Harness> {
    match agent.runs_in() {
        api::Harness::ClaudeCode => Some(Harness::ClaudeCode),
        api::Harness::Pi => Some(Harness::Pi),
        api::Harness::Unspecified => None,
    }
}

/// An agent in `harness` on the skill named `skill`, as a command line,
/// given `given` after it, as what the skill is asked. It is named for the
/// skill and the branch it works on, as `/rename` would, so that the sidebar
/// and the picker say what it is doing from the start. Claude Code runs a
/// skill by `/<name>`, and pi by `/skill:<name>`.
pub fn on_skill(
    harness: Harness,
    skill: &str,
    given: Option<&str>,
    branch: Option<&str>,
) -> Vec<String> {
    let named = match branch {
        Some(branch) => format!("{skill}: {branch}"),
        None => skill.to_owned(),
    };
    let spelled = match harness {
        Harness::ClaudeCode => format!("/{skill}"),
        Harness::Pi => format!("/skill:{skill}"),
    };
    let prompt = match given {
        Some(given) => format!("{spelled} {given}"),
        None => spelled,
    };
    vec![name(harness).to_owned(), "--name".to_owned(), named, prompt]
}

/// An agent in `harness` on `words`, as a command line, let read `files`
/// with no question asked: luneta keeps what a shell printed, and the logs
/// of checks, outside the directory the agent works in, where Claude Code
/// asks first. It asks for a file no rule can name. pi asks nothing before a
/// read.
pub fn on_words(harness: Harness, words: String, files: &[&str]) -> Vec<String> {
    let mut command = vec![name(harness).to_owned()];
    if harness == Harness::ClaudeCode {
        let rules: Vec<String> = files.iter().filter_map(|file| read_rule(file)).collect();
        if !rules.is_empty() {
            command.push("--allowedTools".to_owned());
            command.extend(rules);
            // The flag takes every word after it as another tool, up to this.
            command.push("--".to_owned());
        }
    }
    command.push(words);
    command
}

/// The rule that lets Claude Code read the file by the path `file` from the
/// root, and no other, but for one whose name differs by its case alone, which
/// a rule takes no account of; or none where no rule can name it alone. A rule
/// names a path from the root with another `/` before it, as a glob: `\` makes
/// `*`, `[` or `]` itself, and it takes four to make a `\`, as Claude Code reads
/// the rule, and then the glob. Nothing makes a `?` itself. `--allowedTools`
/// cuts its word at a space or a comma outside parentheses, which any `)`
/// leaves it outside of, and a line is another glob.
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

/// The harness you picked last, as every luneta on the host keeps it.
#[derive(Debug)]
pub struct Kept {
    harness: Harness,
    /// The file it is shared through. Without one, it is this luneta's
    /// alone: a test's, or one with nowhere to keep it.
    file: Option<PathBuf>,
}

impl Default for Kept {
    fn default() -> Self {
        Kept { harness: Harness::ClaudeCode, file: None }
    }
}

impl Kept {
    /// The harness every luneta on the host shares, as it stands, and why
    /// its file could not be read, where it could not.
    pub fn shared() -> (Self, Option<String>) {
        match state::path(FILE) {
            Ok(file) => Kept::within(file),
            Err(unset) => (Kept::default(), Some(unset.to_string())),
        }
    }

    /// The harness shared through `file`, as it stands: Claude Code, where
    /// none is kept yet. Its folder is made, for it to be watched.
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

    /// The file it is shared through, while it is.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Takes up the harness as another luneta kept it since. A file that
    /// names none luneta knows leaves it as it was, and says why.
    pub fn reload(&mut self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", file.display())),
        };
        let named = text.trim();
        let known = [Harness::ClaudeCode, Harness::Pi].into_iter().find(|h| name(*h) == named);
        match known {
            Some(harness) => {
                self.harness = harness;
                Ok(())
            },
            None => Err(format!("{}: {named:?} is no harness luneta knows", file.display())),
        }
    }

    /// Keeps `harness` for every luneta, or says why it cannot. It is written
    /// whole under a name of this luneta's own before it takes the file's,
    /// so that no luneta reads it halfway written.
    pub fn keep(&mut self, harness: Harness) -> Result<(), String> {
        self.harness = harness;
        let Some(file) = &self.file else {
            return Ok(());
        };
        let folder = file.parent().expect("a file of `state::path` is in a folder");
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

/// Tells luneta's loop each time the harness is kept, by another luneta or
/// by this one, for as long as the watcher it returns is kept.
pub fn watch(file: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch_file(file, events, || Event::Agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_names_the_agent_for_it_and_its_branch_in_its_harness_s_spelling() {
        assert_eq!(
            on_skill(Harness::ClaudeCode, "tigerstyle", None, Some("feat/menu")),
            ["claude", "--name", "tigerstyle: feat/menu", "/tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::ClaudeCode, "code-review", Some("since main"), None),
            ["claude", "--name", "code-review", "/code-review since main"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "tigerstyle", None, Some("feat/menu")),
            ["pi", "--name", "tigerstyle: feat/menu", "/skill:tigerstyle"]
        );
        assert_eq!(
            on_skill(Harness::Pi, "code-review", Some("since main"), None),
            ["pi", "--name", "code-review", "/skill:code-review since main"]
        );
    }

    /// Claude Code is let read the file by a rule that names it alone, as a
    /// glob would name others, whatever the path holds, and told of it as it
    /// is. A path no rule can name gets none, and Claude Code asks first. pi
    /// needs no rule.
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
                on_words(Harness::ClaudeCode, prompt.clone(), &[file]),
                command.map(str::to_owned).to_vec(),
                "{file}"
            );
            assert_eq!(
                on_words(Harness::Pi, prompt.clone(), &[file]),
                ["pi".to_owned(), prompt],
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
                on_words(Harness::ClaudeCode, prompt.clone(), &[file]),
                ["claude".to_owned(), prompt],
                "{file:?}"
            );
        }
    }

    /// The harness picked in one luneta is the next one's, and one that
    /// reloads takes it up. None kept yet is Claude Code.
    #[test]
    fn the_harness_picked_is_kept_for_every_luneta() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join(FILE);
        let (mut one, unread) = Kept::within(file.clone());
        assert_eq!((one.harness(), unread), (Harness::ClaudeCode, None));
        let (mut two, _) = Kept::within(file.clone());

        one.keep(Harness::Pi).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "pi\n");
        assert_eq!(Kept::within(file.clone()).0.harness(), Harness::Pi);
        two.reload().unwrap();
        assert_eq!(two.harness(), Harness::Pi);
    }

    /// A file that names no harness luneta knows, as one a newer luneta
    /// wrote, leaves the harness as it was and says why, and the next pick
    /// writes over it. One with no folder to be in says why, and a pick
    /// holds in this luneta alone.
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
