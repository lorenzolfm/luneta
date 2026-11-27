use std::fs;
use std::path::Path;
use std::process::Output;
use std::sync::mpsc;

use lune_domain::projects::{Size, WorktreeId};
use lune_waits::eventually;
use luned::api::Project;
use luned::api::projects::Held;
use luned::{Beside, FocusAsked, Heard, Where};

use crate::host::{Host, LUNE, folder_in, script};
use crate::own::Owned;
use crate::projects::{command, project, worktree};
use crate::worktree::{cloned_as_wt, git};

fn started(host: &Host, from: &Path, what: &str, env: &[(&str, String)]) -> Output {
    let mut lune = host.vars().command(&LUNE);
    luned::local_git(&mut lune);
    lune.current_dir(from).args(["start", what]);
    lune.envs(env.iter().map(|(name, value)| (*name, value)));
    lune.output().unwrap()
}

fn said(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "lune start failed: {stderr}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn refused(output: &Output) -> String {
    assert!(!output.status.success(), "lune start went ahead");
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().last().unwrap_or_default().to_owned()
}

fn projects(host: &Host) -> Vec<Project> {
    let Some(Held::Kept(kept)) = host.host().projects.and_then(|projects| projects.held) else {
        panic!("luned keeps no projects");
    };
    kept.projects
}

fn folders(host: &Host) -> Vec<(String, String, Vec<String>)> {
    let folders = |project: Project| {
        let worktrees = project
            .worktrees()
            .unwrap()
            .iter()
            .map(|it| it.folder.clone())
            .collect();
        (project.name, project.dir, worktrees)
    };
    projects(host).into_iter().map(folders).collect()
}

fn shown(dir: &Path) -> String {
    format!("{}\n", dir.display())
}

#[test]
fn a_folder_starts_the_worktree_of_its_project_or_of_one_made_of_it() {
    let owned = Owned::new();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let (work, docs) = (home.join("work"), home.join("work/docs"));
    fs::create_dir_all(&docs).unwrap();

    let made = started(&owned.host, &home, "work", &[]);
    assert_eq!(said(&made), shown(&work));
    assert!(
        made.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let work_s = work.display().to_string();
    let made = vec![("work".to_owned(), work_s.clone(), vec![work_s.clone()])];
    assert_eq!(
        folders(&owned.host),
        made,
        "a folder's project holds one worktree there"
    );
    assert_eq!(
        said(&started(&owned.host, &work, "docs", &[])),
        shown(&docs)
    );
    assert_eq!(folders(&owned.host), made, "a folder in it starts that one");
    assert_eq!(said(&started(&owned.host, &docs, "..", &[])), shown(&work));
    assert_eq!(folders(&owned.host), made);

    assert_eq!(
        refused(&started(&owned.host, &home, "./gone", &[])),
        format!("lune: {} is no folder", home.join("./gone").display())
    );
}

#[test]
fn a_branch_starts_in_a_worktree_of_the_clone_wt_made_and_only_there() {
    let owned = Owned::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let (main, feat) = (clone.join("main"), clone.join("feat"));

    assert_eq!(
        said(&started(&owned.host, &main, "feat", &[])),
        shown(&feat)
    );
    assert!(feat.join(".git").is_file(), "wt made the worktree");
    let opened = vec![feat.display().to_string()];
    let made = vec![("n".to_owned(), clone.display().to_string(), opened)];
    assert_eq!(
        folders(&owned.host),
        made,
        "only the worktree started opens"
    );
    assert_eq!(
        said(&started(&owned.host, &main, "feat", &[])),
        shown(&feat)
    );
    assert_eq!(folders(&owned.host), made, "the worktree is started again");

    worktree(&owned.host, projects(&owned.host)[0].id, &main);
    let projects = projects(&owned.host);
    let worktrees = projects[0].worktrees().unwrap();
    let [there, here] = [0, 1].map(|at| WorktreeId::of(worktrees[at].id).unwrap());
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    let (heard, hearing) = mpsc::channel();
    let size = Size::of(80, 24).unwrap();
    let at = Where::Here { runs_in: None };
    let mut show = luned::show(folder, at, here, size, false, move |it| {
        drop(heard.send(it));
    });
    let _ = show.ask_focus(FocusAsked::Open {
        beside: Beside::Alone,
        opens: command(&["sleep", "600"]),
    });
    let laid = eventually("the layout lays out the terminal", || {
        match hearing.try_recv() {
            Ok(Heard::Laid { laid, .. }) if !laid.is_empty() => Some(laid),
            Ok(_) | Err(_) => None,
        }
    });
    let key = laid
        .focused()
        .and_then(luned::View::key)
        .expect("a terminal of the mux's");
    let inside = [
        ("LUNE_MUX_TERMINAL", key.id.to_string()),
        ("LUNE_MUX_TERMINAL_MADE", key.made.nanos().to_string()),
    ];
    assert_eq!(
        said(&started(&owned.host, &main, "feat", &inside)),
        shown(&feat)
    );
    let sent = eventually("lune is sent to the worktree started", || {
        match hearing.try_recv() {
            Ok(Heard::Sent(to)) => Some(to),
            Ok(_) | Err(_) => None,
        }
    });
    assert_eq!(sent, there);
    let unseen = [
        ("LUNE_MUX_TERMINAL", (key.id.get() + 100).to_string()),
        ("LUNE_MUX_TERMINAL_MADE", key.made.nanos().to_string()),
    ];
    let stayed = started(&owned.host, &main, "feat", &unseen);
    assert_eq!(
        said(&stayed),
        shown(&feat),
        "no lune on the terminal is no failure"
    );
    let stderr = String::from_utf8_lossy(&stayed.stderr);
    assert!(stderr.contains("no lune is on this terminal"), "{stderr}");

    git(
        &scratch,
        &["clone", "-q", &up.display().to_string(), "plain"],
    );
    let plain = refused(&started(&owned.host, &scratch.join("plain"), "feat", &[]));
    assert!(plain.contains("is in a clone git made"), "{plain}");
    git(
        &scratch,
        &[
            "clone",
            "-q",
            "--bare",
            &up.display().to_string(),
            "infra.git",
        ],
    );
    let bare = refused(&started(
        &owned.host,
        &scratch.join("infra.git"),
        "feat",
        &[],
    ));
    assert!(
        bare.contains("is in a bare repository, not in a worktree of it"),
        "{bare}"
    );
    let nowhere = started(&owned.host, &scratch, "feat", &[]);
    refused(&nowhere);
    let stderr = String::from_utf8_lossy(&nowhere.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("lune: wt add: ")),
        "{stderr}"
    );
}

#[test]
fn a_project_asked_for_at_a_worktree_is_its_clone_s_and_a_branch_joins_it() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let (main, feat) = (clone.join("main"), clone.join("feat"));
    let made = project(host, &main, None);
    worktree(host, made, &main);
    let rooted = vec![(
        "n".to_owned(),
        clone.display().to_string(),
        vec![main.display().to_string()],
    )];
    assert_eq!(
        folders(host),
        rooted,
        "a project is made at its clone's root"
    );
    let docs = clone.join("docs");
    git(
        &clone.join(".bare"),
        &["worktree", "add", "-q", "-b", "docs", "../docs", "main"],
    );

    assert_eq!(said(&started(host, &clone, "docs", &[])), shown(&docs));
    let joined = [&main, &docs].map(|it| it.display().to_string()).to_vec();
    let joined = vec![("n".to_owned(), clone.display().to_string(), joined)];
    assert_eq!(folders(host), joined, "a worktree's folder joins it too");
    assert_eq!(said(&started(host, &main, "feat", &[])), shown(&feat));
    let all = [&main, &docs, &feat]
        .map(|it| it.display().to_string())
        .to_vec();
    assert_eq!(
        folders(host),
        vec![("n".to_owned(), clone.display().to_string(), all)]
    );
    assert_eq!(
        projects(host)[0].id,
        made,
        "the worktree joins the project there was"
    );
}

#[test]
fn a_branch_in_a_clone_a_folder_s_project_holds_makes_nothing_and_its_folder_goes_there() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, notes) = (scratch.join("up"), scratch.join("notes"));
    fs::create_dir(&notes).unwrap();
    let clone = notes.join("n");
    cloned_as_wt(&scratch, &up, &clone);
    project(host, &notes, None);
    let notes_s = notes.display().to_string();
    let kept = vec![("notes".to_owned(), notes_s.clone(), vec![notes_s.clone()])];
    assert_eq!(folders(host), kept);
    let main = clone.join("main");

    let side = scratch.join("side");
    git(
        &clone.join(".bare"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &side.display().to_string(),
        ],
    );

    let one = "notes is a folder, and its own is its one worktree";
    let held = format!(
        "{} is in {notes_s}, project notes's folder; {one}",
        clone.display()
    );
    let refusal = format!("lune: nothing made: {held}");
    assert_eq!(refused(&started(host, &main, "feat", &[])), refusal);
    assert_eq!(
        refused(&started(host, &side, "feat", &[])),
        refusal,
        "wt makes it in the clone"
    );
    assert!(!clone.join("feat").exists(), "wt makes no worktree");
    assert_eq!(folders(host), kept);

    let went = started(host, &clone, "main", &[]);
    assert_eq!(
        said(&went),
        shown(&main),
        "a worktree's folder goes to the folder's project"
    );
    assert!(
        went.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&went.stderr)
    );
    assert_eq!(folders(host), kept, "and makes it no row");
}

#[test]
fn an_issue_starts_in_a_worktree_named_after_it_with_an_agent_on_it() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let bare = clone.join(".bare");
    let github = "git@github.com:o/n.git";
    git(&bare, &["remote", "add", "upstream", github]);
    git(
        &bare,
        &["config", &format!("url.{}.insteadOf", up.display()), github],
    );
    host.gh.issue(7, "https://github.com/o/n/issues/7");
    let asked = host.home.path().join("claude-asked");
    let claude = format!(
        "printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n",
        asked.display()
    );
    script(&host.bin().join("claude"), &claude);

    let issue = clone.join("issue-7");
    assert_eq!(
        said(&started(host, &clone.join("main"), "#7", &[])),
        shown(&issue)
    );
    assert!(issue.join(".git").is_file(), "the issue's worktree is made");
    let agent = eventually("an agent starts on the issue", || {
        fs::read_to_string(&asked).ok()
    });
    assert_eq!(agent, "--\nTackle https://github.com/o/n/issues/7\n");
    let opened = vec![issue.display().to_string()];
    let made = vec![("n".to_owned(), clone.display().to_string(), opened)];
    assert_eq!(folders(host), made, "only the issue's worktree opens");
    let terminals = || projects(host)[0].worktrees().unwrap()[0].terminals.len();
    assert_eq!(terminals(), 1);

    let link = "https://github.com/o/n/issues/7";
    assert_eq!(
        said(&started(host, &clone.join("main"), link, &[])),
        shown(&issue)
    );
    assert_eq!(
        (folders(host), terminals()),
        (made, 1),
        "no second agent starts on it"
    );
    let unknown = refused(&started(host, &clone.join("main"), "#8", &[]));
    assert!(unknown.starts_with("lune: gh issue view: "), "{unknown}");

    host.gh.issue(9, "https://github.com/o/n/issues/9");
    let nine = clone.join("issue-9");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "issue-9",
            "../issue-9",
            "main",
        ],
    );
    fs::remove_file(&asked).unwrap();
    assert_eq!(
        said(&started(host, &clone.join("main"), "#9", &[])),
        shown(&nine)
    );
    let agent = eventually("an agent starts in one not open", || {
        fs::read_to_string(&asked).ok()
    });
    assert_eq!(agent, "--\nTackle https://github.com/o/n/issues/9\n");
    let projects = projects(host);
    let mut held = projects.iter().flat_map(|it| it.worktrees().unwrap());
    let nine = nine.display().to_string();
    let there = held
        .find(|it| it.folder == nine)
        .expect("the worktree started opens");
    assert_eq!(
        there.terminals.len(),
        1,
        "with its agent as its one terminal"
    );
}
