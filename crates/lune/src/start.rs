use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use lune_domain::agent::FlowHarness;
use lune_domain::projects::{Argv, Dir, Opens, WorktreeId};
use luned::Directory;
use luned::links::{self, Branch, Kind, Link, Of, Pasted, Unready, Work, Worked};
use luned::mux::own::terminal_here;
use luned::program;

use crate::agents::Kept;
use crate::flow;
use crate::machines::Machine;
use crate::reach::{self, Luned};
use crate::state;
use crate::worktrees::{self, Landed, Landing, Opening, Went};

pub struct ShipOptions {
    flow: Option<String>,
    interactive: bool,
    harness: FlowHarness,
}

impl ShipOptions {
    pub fn on_the_agent_picked(
        flow: Option<String>,
        interactive: bool,
    ) -> Result<ShipOptions, String> {
        let harness = flow::picked()?;
        Ok(ShipOptions {
            flow,
            interactive,
            harness,
        })
    }

    fn mode(&self) -> lune_domain::flows::FlowMode {
        if self.interactive {
            lune_domain::flows::FlowMode::Interactive
        } else {
            lune_domain::flows::FlowMode::Unattended
        }
    }
}

pub const UNFINISHED: u8 = 4;

pub const ELSEWHERE: &str = "no lune is on this terminal";

const ASKING: Duration = Duration::from_secs(30);

const TACKLE: &str = "Tackle";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    Folder(PathBuf),
    Branch(Branch),
    Issue(NonZeroU32),
    Link(Link),
}

impl What {
    pub fn read(
        text: &str,
        cwd: &Path,
        home: Option<&Path>,
        is_dir: impl Fn(&Path) -> bool,
    ) -> Result<What, String> {
        let relative = cwd.display();
        assert!(
            cwd.is_absolute(),
            "a folder is read against an absolute cwd, not {relative}"
        );
        let text = text.trim();
        if text.is_empty() {
            return Err(
                "nothing to start: name a folder, a branch, an issue or a pull request".to_owned(),
            );
        }
        if let Some(link) = Link::parse(text) {
            return Ok(What::Link(link));
        }
        let github = ["github.com/", "www.github.com/"]
            .iter()
            .any(|at| text.starts_with(at));
        if text.contains("://") || github {
            return Err(format!(
                "{text} is no link of an issue or a pull request on github.com"
            ));
        }
        if let Some(number) = text.strip_prefix('#') {
            let digits = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
            let number = number.parse().ok().filter(|_| digits);
            return number
                .map(What::Issue)
                .ok_or_else(|| format!("{text} is no issue's number"));
        }
        let below_home = Dir::below_home(text);
        let dir = match (below_home, home) {
            (Some(""), Some(home)) => home.to_path_buf(),
            (Some(below), Some(home)) => home.join(below.trim_start_matches('/')),
            _ => cwd.join(text),
        };
        if is_dir(&dir) {
            return Ok(What::Folder(dir));
        }
        let pathlike = ["/", "./", "../", "~"]
            .iter()
            .any(|start| text.starts_with(start));
        if pathlike || text == "." || text == ".." {
            return Err(format!("{} is no folder", dir.display()));
        }
        Branch::parse(text).map(What::Branch)
    }
}

enum Started {
    Project {
        folder: PathBuf,
    },
    Worktree {
        worktree: WorktreeId,
        folder: PathBuf,
        left: Left,
    },
}

enum Left {
    Done,
    Noted(String),
    Unfinished(String),
}

pub fn run(what: &str, ship: Option<&ShipOptions>, path: &Path) -> ExitCode {
    let started = match started(what, ship, path) {
        Ok(started) => started,
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        }
    };
    let (folder, finished) = match &started {
        Started::Project { folder } => {
            let at = folder.display();
            eprintln!("lune: {at} is a clone with no worktree: start a branch in it");
            (folder, true)
        }
        Started::Worktree {
            worktree,
            folder,
            left,
        } => {
            let done = match left {
                Left::Done => true,
                Left::Noted(note) => {
                    eprintln!("lune: {note}");
                    true
                }
                Left::Unfinished(why) => {
                    eprintln!("lune: {why}");
                    false
                }
            };
            let went = went(path, *worktree);
            (folder, done && went)
        }
    };
    println!("{}", folder.display());
    if finished {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(UNFINISHED)
    }
}

fn went(path: &Path, worktree: WorktreeId) -> bool {
    let from = match terminal_here() {
        Ok(Some(from)) => from,
        Ok(None) => return true,
        Err(why) => {
            eprintln!("lune: started, and went nowhere: {why}");
            return false;
        }
    };
    match worktrees::bring(path, worktree, from) {
        Ok(Went::Taken) => true,
        Ok(Went::Elsewhere) => {
            eprintln!("lune: {ELSEWHERE}, so none went there");
            true
        }
        Err(unchanged) => {
            let said = unchanged.said("went nowhere", "lune may have gone to the worktree");
            eprintln!("lune: started, and {said}");
            false
        }
    }
}

fn started(what: &str, ship: Option<&ShipOptions>, path: &Path) -> Result<Started, String> {
    let cwd = env::current_dir().map_err(|error| format!("no directory to start from: {error}"))?;
    let cwd = worktrees::real(&cwd)?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    match What::read(what, &cwd, home.as_deref(), Path::is_dir)? {
        What::Folder(dir) => {
            let folder = worktrees::real(&dir)?;
            let at = Dir::parse(utf8(&folder)?)?;
            Ok(
                match worktrees::made_at(path, &Machine::Local, &(&at).into())? {
                    Landing::Worktree(landed) => {
                        let worktree = landed.worktree();
                        let there = worktrees::folder_of(path, worktree);
                        let there = there.unwrap_or_else(|_| folder.clone());
                        let folder = if folder.starts_with(&there) {
                            folder
                        } else {
                            there
                        };
                        let left = ship.map_or(Left::Done, |opts| {
                            start_ship_flow(path, worktree, opts, None, &folder.display())
                        });
                        Started::Worktree {
                            worktree,
                            folder,
                            left,
                        }
                    }
                    Landing::Unopened(_) => Started::Project { folder },
                },
            )
        }
        What::Branch(branch) => {
            let repo = utf8(&cwd)?;
            let holders = worktrees::holders(path)?;
            let made = links::made_landing(repo, &Of::Branch(branch.clone()), &holders)?;
            let folder = worktrees::real(&made)?;
            let at = Directory::parse(utf8(&folder)?.to_owned())?;
            let landed = worktrees::joined_as(path, &at, branch.as_str(), &Opens::Shell)?;
            let worktree = landed.worktree();
            let left = ship.map_or(Left::Done, |opts| {
                start_ship_flow(path, worktree, opts, None, &branch.as_str())
            });
            Ok(Started::Worktree {
                worktree,
                folder,
                left,
            })
        }
        What::Issue(number) => linked(path, &cwd, &numbered(number, &cwd)?, ship),
        What::Link(link) => linked(path, &cwd, &link, ship),
    }
}

fn start_ship_flow(
    path: &Path,
    worktree: WorktreeId,
    options: &ShipOptions,
    prompt: Option<String>,
    label: &dyn std::fmt::Display,
) -> Left {
    match flow::start_on(
        path,
        worktree,
        options.mode(),
        options.harness,
        options.flow.clone(),
        prompt,
    ) {
        Ok(_) => Left::Done,
        Err(why) => Left::Unfinished(format!("no flow started on {label}: {why}")),
    }
}

fn linked(
    path: &Path,
    cwd: &Path,
    link: &Link,
    ship: Option<&ShipOptions>,
) -> Result<Started, String> {
    let dir = Directory::parse(utf8(cwd)?.to_owned())?;
    let dirs = worktrees::dirs(path)?
        .into_iter()
        .filter(|other| *other != dir);
    let others = dirs
        .map(|dir| Worked {
            dir,
            pull: None,
            here: false,
        })
        .collect::<Vec<_>>();
    let here = Worked {
        dir,
        pull: None,
        here: true,
    };
    let worked = std::iter::once(here).chain(others).collect();
    let pasted = Pasted::new(Work::Link(link.clone()), worked);
    let ready = Luned::at(path, &Machine::Local, reach::ANSWER)?
        .paste(&pasted)
        .map_err(said)?;
    let folder = PathBuf::from(ready.folder.as_str());
    let off = ready
        .moved_off
        .then(|| Work::Link(link.clone()).off_branch(ready.branch.as_deref()));
    let agent = match (link.kind(), &off, ship) {
        (Kind::Issue, None, None) => {
            let harness = Kept::shared(&state::Folder::of_user()).0.harness();
            let agent = Argv::on_words(harness, format!("{TACKLE} {}", link.url()), &[]);
            let unstarted = |why| format!("no agent starts on {}: {why}", link.tab());
            Opens::Command(agent.map_err(unstarted)?)
        }
        (Kind::Issue, Some(_) | None, Some(_))
        | (Kind::Issue, Some(_), None)
        | (Kind::Pull, ..) => Opens::Shell,
    };
    let landed = worktrees::joined_as(path, &ready.folder, &link.tab(), &agent)?;
    let worktree = landed.worktree();
    let left = if let Some(opts) = ship {
        if let Some(off) = off {
            Left::Noted(format!(
                "{} is {off}, so no flow starts there",
                folder.display()
            ))
        } else {
            let prompt = match link.kind() {
                Kind::Issue => Some(format!("{TACKLE} {}", link.url())),
                Kind::Pull => None,
            };
            start_ship_flow(path, worktree, opts, prompt, &link.tab())
        }
    } else {
        match (link.kind(), landed, off) {
            (Kind::Pull, _, None)
            | (Kind::Pull | Kind::Issue, Landed::Found(_), Some(_))
            | (Kind::Issue, Landed::Made(_), None) => Left::Done,
            (Kind::Pull, Landed::Made(_), Some(off)) => {
                Left::Noted(format!("{} is {off}", folder.display()))
            }
            (Kind::Issue, Landed::Made(_), Some(off)) => Left::Noted(format!(
                "{} is {off}, so no agent starts there",
                folder.display()
            )),
            (Kind::Issue, Landed::Found(_), None) => {
                match worktrees::opened(path, worktree, agent, Opening::IfEmpty) {
                    Ok(()) => Left::Done,
                    Err(why) => {
                        Left::Unfinished(format!("no agent started on {}: {why}", link.tab()))
                    }
                }
            }
        }
    };
    Ok(Started::Worktree {
        worktree,
        folder,
        left,
    })
}

fn numbered(number: NonZeroU32, cwd: &Path) -> Result<Link, String> {
    let mut gh = Command::new("gh");
    let number = number.to_string();
    gh.current_dir(cwd)
        .args(["issue", "view", &number, "--json", "url", "--jq", ".url"]);
    let ran = program::run(&mut gh, ASKING)
        .map_err(|error| format!("gh issue view could not be run: {error}"))?;
    let said = ran
        .answer()
        .map_err(|why| format!("gh issue view: {why}"))?;
    let said = String::from_utf8_lossy(&said);
    let said = said.trim();
    Link::parse(said).ok_or_else(|| format!("gh issue view said {said}, which is no link"))
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

fn said(unready: Unready) -> String {
    match unready {
        Unready::Nowhere(why) | Unready::Failed(why) | Unready::InDoubt(why) => why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<What, String> {
        What::read(text, Path::new("/w/lune"), Some(Path::new("/w")), |dir| {
            [
                Path::new("/w/lune/main"),
                Path::new("/w/lune"),
                Path::new("/w"),
            ]
            .contains(&dir)
        })
    }

    #[test]
    fn a_folder_that_is_there_is_started_before_a_branch_of_its_name() {
        assert_eq!(
            read("main"),
            Ok(What::Folder(PathBuf::from("/w/lune/main")))
        );
        assert_eq!(read("/w"), Ok(What::Folder(PathBuf::from("/w"))));
        assert_eq!(read(" . "), Ok(What::Folder(PathBuf::from("/w/lune/."))));
        let branch = |text: &str| Ok(What::Branch(Branch::parse(text).unwrap()));
        assert_eq!(read("feat/sidebar"), branch("feat/sidebar"));
        assert_eq!(read("issue-448"), branch("issue-448"));
        assert_eq!(
            read("~"),
            Ok(What::Folder(PathBuf::from("/w"))),
            "~ is home"
        );
        assert_eq!(read("~/lune"), Ok(What::Folder(PathBuf::from("/w/lune"))));
    }

    #[test]
    fn a_path_to_no_folder_is_no_branch() {
        for path in ["./gone", "../gone", "/gone", "~/gone"] {
            let said = read(path).unwrap_err();
            assert!(said.ends_with("is no folder"), "{path}: {said}");
        }
    }

    #[test]
    fn an_issue_is_a_number_or_a_link_and_a_pull_request_a_link() {
        assert_eq!(read("#448"), Ok(What::Issue(NonZeroU32::new(448).unwrap())));
        for wrong in ["#", "#0", "#4a", "#-1"] {
            assert_eq!(read(wrong), Err(format!("{wrong} is no issue's number")));
        }
        let pull = "github.com/lorenzolfm/lune/pull/441";
        assert_eq!(read(pull), Ok(What::Link(Link::parse(pull).unwrap())));
        let issue = "https://github.com/lorenzolfm/lune/issues/448";
        assert_eq!(read(issue), Ok(What::Link(Link::parse(issue).unwrap())));
        let wrong = "https://github.com/lorenzolfm/lune/commits";
        let said = format!("{wrong} is no link of an issue or a pull request on github.com");
        assert_eq!(read(wrong), Err(said));
        let named = "lorenzolfm/lune#448";
        let said = format!("{named} names an issue or a pull request: paste its link");
        assert_eq!(
            read(named),
            Err(said),
            "the name lune gives a link is no branch"
        );
        assert!(read("  ").unwrap_err().starts_with("nothing to start"));
    }
}
