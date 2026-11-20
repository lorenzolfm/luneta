use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use lunetad::links::{self, Branch, Kind, Link, Of, Pasted, Unready, Work, Worked};
use lunetad::mux::own::terminal_here;
use lunetad::program;
use lunetad::projects::{Dir, WorktreeId};
use lunetad::Directory;

use crate::agents::{self, Kept};
use crate::machines::Machine;
use crate::worktrees::{self, Landed, Landing, Opening, Went};

pub const UNFINISHED: u8 = 4;

pub const ELSEWHERE: &str = "no luneta is on this terminal";

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
        assert!(cwd.is_absolute(), "a folder is read against an absolute cwd, not {relative}");
        let text = text.trim();
        if text.is_empty() {
            return Err("nothing to start: name a folder, a branch, an issue or a pull request"
                .to_owned());
        }
        if let Some(link) = Link::parse(text) {
            return Ok(What::Link(link));
        }
        let github = ["github.com/", "www.github.com/"].iter().any(|at| text.starts_with(at));
        if text.contains("://") || github {
            return Err(format!("{text} is no link of an issue or a pull request on github.com"));
        }
        if let Some(number) = text.strip_prefix('#') {
            let digits = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
            let number = number.parse().ok().filter(|_| digits);
            return number.map(What::Issue).ok_or_else(|| format!("{text} is no issue's number"));
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
        let pathlike = ["/", "./", "../", "~"].iter().any(|start| text.starts_with(start));
        if pathlike || text == "." || text == ".." {
            return Err(format!("{} is no folder", dir.display()));
        }
        Branch::parse(text).map(What::Branch)
    }
}

enum Started {
    Project { folder: PathBuf },
    Worktree { worktree: WorktreeId, folder: PathBuf, left: Left },
}

enum Left {
    Done,
    Noted(String),
    Unfinished(String),
}

pub fn run(what: &str, path: &Path) -> ExitCode {
    let started = match started(what, path) {
        Ok(started) => started,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    let (folder, finished) = match &started {
        Started::Project { folder } => {
            let at = folder.display();
            eprintln!("luneta: {at} is a clone with no worktree: start a branch in it");
            (folder, true)
        },
        Started::Worktree { worktree, folder, left } => {
            let done = match left {
                Left::Done => true,
                Left::Noted(note) => {
                    eprintln!("luneta: {note}");
                    true
                },
                Left::Unfinished(why) => {
                    eprintln!("luneta: {why}");
                    false
                },
            };
            let went = went(path, *worktree);
            (folder, done && went)
        },
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
            eprintln!("luneta: started, and went nowhere: {why}");
            return false;
        },
    };
    match worktrees::bring(path, worktree, from) {
        Ok(Went::Taken) => true,
        Ok(Went::Elsewhere) => {
            eprintln!("luneta: {ELSEWHERE}, so none went there");
            true
        },
        Err(unchanged) => {
            let said = unchanged.said("went nowhere", "luneta may have gone to the worktree");
            eprintln!("luneta: started, and {said}");
            false
        },
    }
}

fn started(what: &str, path: &Path) -> Result<Started, String> {
    let cwd = env::current_dir().map_err(|error| format!("no directory to start from: {error}"))?;
    let cwd = worktrees::real(&cwd)?;
    let home = env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    match What::read(what, &cwd, home.as_deref(), Path::is_dir)? {
        What::Folder(dir) => {
            let folder = worktrees::real(&dir)?;
            let at = Directory::parse(utf8(&folder)?.to_owned())?;
            Ok(match worktrees::made_at(path, &Machine::Local, &at, None)? {
                Landing::Worktree(landed) => {
                    let worktree = landed.worktree();
                    let there = worktrees::folder_of(path, worktree);
                    let there = there.unwrap_or_else(|_| folder.clone());
                    let folder = if folder.starts_with(&there) { folder } else { there };
                    Started::Worktree { worktree, folder, left: Left::Done }
                },
                Landing::Unopened(_) => Started::Project { folder },
            })
        },
        What::Branch(branch) => {
            let repo = utf8(&cwd)?;
            if let Some(clone) = links::holder(repo) {
                links::lands(&clone, &worktrees::holders(path)?)?;
            }
            let folder = worktrees::real(&links::made(repo, &Of::Branch(branch.clone()))?)?;
            let at = Directory::parse(utf8(&folder)?.to_owned())?;
            let landed = worktrees::joined_as(path, &at, branch.as_str(), Vec::new())?;
            Ok(Started::Worktree { worktree: landed.worktree(), folder, left: Left::Done })
        },
        What::Issue(number) => linked(path, &cwd, &numbered(number, &cwd)?),
        What::Link(link) => linked(path, &cwd, &link),
    }
}

fn linked(path: &Path, cwd: &Path, link: &Link) -> Result<Started, String> {
    let dir = Directory::parse(utf8(cwd)?.to_owned())?;
    let dirs = worktrees::dirs(path)?.into_iter().filter(|other| *other != dir);
    let others = dirs.map(|dir| Worked { dir, pull: None, here: false }).collect::<Vec<_>>();
    let here = Worked { dir, pull: None, here: true };
    let worked = std::iter::once(here).chain(others).collect();
    let holders = worktrees::holders(path)?;
    let ready = Pasted::new(Work::Link(link.clone()), worked).ready(&holders).map_err(said)?;
    let folder = PathBuf::from(ready.folder.as_str());
    let off = Work::Link(link.clone()).off_branch(ready.branch.as_deref());
    let off = off.filter(|_| ready.moved_off);
    let agent = match (link.kind(), &off) {
        (Kind::Issue, None) => {
            let harness = Kept::shared().0.harness();
            agents::on_words(harness, format!("{TACKLE} {}", link.url()), &[])
        },
        (Kind::Issue, Some(_)) | (Kind::Pull, _) => Vec::new(),
    };
    let landed = worktrees::joined_as(path, &ready.folder, &link.tab(), agent.clone())?;
    let worktree = landed.worktree();
    let left = match (link.kind(), landed, off) {
        (Kind::Pull, _, _)
        | (Kind::Issue, Landed::Found(_), Some(_))
        | (Kind::Issue, Landed::Made(_), None) => Left::Done,
        (Kind::Issue, Landed::Made(_), Some(off)) => {
            Left::Noted(format!("{} is {off}, so no agent starts there", folder.display()))
        },
        (Kind::Issue, Landed::Found(_), None) => {
            match worktrees::opened(path, worktree, agent, Opening::IfEmpty) {
                Ok(()) => Left::Done,
                Err(why) => Left::Unfinished(format!("no agent started on {}: {why}", link.tab())),
            }
        },
    };
    Ok(Started::Worktree { worktree, folder, left })
}

fn numbered(number: NonZeroU32, cwd: &Path) -> Result<Link, String> {
    let mut gh = Command::new("gh");
    let number = number.to_string();
    gh.current_dir(cwd).args(["issue", "view", &number, "--json", "url", "--jq", ".url"]);
    let ran = program::run(&mut gh, ASKING)
        .map_err(|error| format!("gh issue view could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh issue view: {why}"))?;
    let said = String::from_utf8_lossy(&said);
    let said = said.trim();
    Link::parse(said).ok_or_else(|| format!("gh issue view said {said}, which is no link"))
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))
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
        What::read(text, Path::new("/w/luneta"), Some(Path::new("/w")), |dir| {
            [Path::new("/w/luneta/main"), Path::new("/w/luneta"), Path::new("/w")].contains(&dir)
        })
    }

    #[test]
    fn a_folder_that_is_there_is_started_before_a_branch_of_its_name() {
        assert_eq!(read("main"), Ok(What::Folder(PathBuf::from("/w/luneta/main"))));
        assert_eq!(read("/w"), Ok(What::Folder(PathBuf::from("/w"))));
        assert_eq!(read(" . "), Ok(What::Folder(PathBuf::from("/w/luneta/."))));
        let branch = |text: &str| Ok(What::Branch(Branch::parse(text).unwrap()));
        assert_eq!(read("feat/sidebar"), branch("feat/sidebar"));
        assert_eq!(read("issue-448"), branch("issue-448"));
        assert_eq!(read("~"), Ok(What::Folder(PathBuf::from("/w"))), "~ is home");
        assert_eq!(read("~/luneta"), Ok(What::Folder(PathBuf::from("/w/luneta"))));
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
        let pull = "github.com/lorenzolfm/luneta/pull/441";
        assert_eq!(read(pull), Ok(What::Link(Link::parse(pull).unwrap())));
        let issue = "https://github.com/lorenzolfm/luneta/issues/448";
        assert_eq!(read(issue), Ok(What::Link(Link::parse(issue).unwrap())));
        let wrong = "https://github.com/lorenzolfm/luneta/commits";
        let said = format!("{wrong} is no link of an issue or a pull request on github.com");
        assert_eq!(read(wrong), Err(said));
        let named = "lorenzolfm/luneta#448";
        let said = format!("{named} names an issue or a pull request: paste its link");
        assert_eq!(read(named), Err(said), "the name luneta gives a link is no branch");
        assert!(read("  ").unwrap_err().starts_with("nothing to start"));
    }
}
