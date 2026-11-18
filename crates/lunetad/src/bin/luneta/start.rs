use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use lunetad::links::{self, Kind, Link, Of, Pasted, Unready, Work, Worked};
use lunetad::mux::own::terminal_here;
use lunetad::program;
use lunetad::projects::WorkstreamId;
use lunetad::Directory;

use crate::agents::{self, Kept};
use crate::machines::Machine;
use crate::workstreams;

const ASKING: Duration = Duration::from_secs(30);

const TACKLE: &str = "Tackle";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    Folder(PathBuf),
    Branch(String),
    Issue(NonZeroU32),
    Link(Link),
}

impl What {
    pub fn read(text: &str, cwd: &Path, is_dir: impl Fn(&Path) -> bool) -> Result<What, String> {
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
        let dir = cwd.join(text);
        if is_dir(&dir) {
            return Ok(What::Folder(dir));
        }
        let pathlike = ["/", "./", "../", "~"].iter().any(|start| text.starts_with(start));
        if pathlike || text == "." || text == ".." {
            return Err(format!("{} is no folder", dir.display()));
        }
        Ok(What::Branch(text.to_owned()))
    }
}

struct Started {
    workstream: Option<WorkstreamId>,
    folder: PathBuf,
    note: Option<String>,
}

pub fn run(what: &str, path: &Path) -> ExitCode {
    let started = match started(what, path) {
        Ok(started) => started,
        Err(why) => {
            eprintln!("luneta: {why}");
            return ExitCode::FAILURE;
        },
    };
    if let Some(note) = &started.note {
        eprintln!("luneta: {note}");
    }
    if let Some(workstream) = started.workstream
        && let Some(from) = terminal_here()
        && let Err(why) = workstreams::bring(path, workstream, from)
    {
        eprintln!("luneta: started, and went nowhere: {why}");
    }
    println!("{}", started.folder.display());
    ExitCode::SUCCESS
}

fn started(what: &str, path: &Path) -> Result<Started, String> {
    let cwd = env::current_dir().map_err(|error| format!("no directory to start from: {error}"))?;
    let cwd = workstreams::real(&cwd)?;
    match What::read(what, &cwd, Path::is_dir)? {
        What::Folder(dir) => {
            let folder = workstreams::real(&dir)?;
            let at = Directory::parse(utf8(&folder)?.to_owned())?;
            let workstream = workstreams::made_at(path, &Machine::Local, &at, None)?;
            let note = workstream.is_none().then(|| {
                format!("{at} is a project with no workstream there: start a folder in it")
            });
            Ok(Started { workstream, folder, note })
        },
        What::Branch(branch) => {
            let repo = utf8(&cwd)?;
            let folder = workstreams::real(&links::made(repo, &Of::Branch(branch))?)?;
            let (workstream, _) = in_clone(path, &folder)?;
            Ok(Started { workstream: Some(workstream), folder, note: None })
        },
        What::Issue(number) => linked(path, &cwd, &numbered(number, &cwd)?),
        What::Link(link) => linked(path, &cwd, &link),
    }
}

fn linked(path: &Path, cwd: &Path, link: &Link) -> Result<Started, String> {
    let dir = utf8(cwd)?.to_owned();
    let dirs = workstreams::dirs(path)?.into_iter().filter(|other| *other != dir);
    let others = dirs.map(|dir| Worked { dir, pull: None, here: false }).collect::<Vec<_>>();
    let here = Worked { dir, pull: None, here: true };
    let worked = std::iter::once(here).chain(others).collect();
    let ready = Pasted::new(Work::Link(link.clone()), worked).ready().map_err(said)?;
    let folder = PathBuf::from(&ready.worktree);
    let (workstream, made) = in_clone(path, &folder)?;
    let off = Work::Link(link.clone()).off_branch(ready.branch.as_deref());
    let note = match (link.kind(), made, off.filter(|_| ready.moved_off)) {
        (Kind::Pull, _, _) | (Kind::Issue, false, _) => None,
        (Kind::Issue, true, Some(off)) => {
            Some(format!("{} is {off}, so no agent starts there", folder.display()))
        },
        (Kind::Issue, true, None) => {
            let harness = Kept::shared().0.harness();
            let argv = agents::on_words(harness, format!("{TACKLE} {}", link.url()), &[]);
            workstreams::opened(path, workstream, argv)
                .err()
                .map(|why| format!("no agent started on {}: {why}", link.tab()))
        },
    };
    Ok(Started { workstream: Some(workstream), folder, note })
}

fn in_clone(path: &Path, folder: &Path) -> Result<(WorkstreamId, bool), String> {
    let worktree = utf8(folder)?;
    let clone = links::holder(worktree)
        .ok_or_else(|| format!("git does not say what repository {worktree} is"))?;
    workstreams::worked_in(path, folder, &clone)
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
        What::read(text, Path::new("/w/luneta"), |dir| {
            [Path::new("/w/luneta/main"), Path::new("/w/luneta"), Path::new("/w")].contains(&dir)
        })
    }

    #[test]
    fn a_folder_that_is_there_is_started_before_a_branch_of_its_name() {
        assert_eq!(read("main"), Ok(What::Folder(PathBuf::from("/w/luneta/main"))));
        assert_eq!(read("/w"), Ok(What::Folder(PathBuf::from("/w"))));
        assert_eq!(read(" . "), Ok(What::Folder(PathBuf::from("/w/luneta/."))));
        assert_eq!(read("feat/sidebar"), Ok(What::Branch("feat/sidebar".to_owned())));
        assert_eq!(read("issue-448"), Ok(What::Branch("issue-448".to_owned())));
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
        assert!(read("  ").unwrap_err().starts_with("nothing to start"));
    }
}
