use std::fs;
use std::path::Path;
use std::process::Output;
use std::sync::mpsc;

use lunetad::api::projects::Held;
use lunetad::api::Project;
use lunetad::projects::{Size, WorkstreamId};
use lunetad::{Asked, Beside, Heard, Where};

use crate::host::{eventually, folder_in, script, Host, LUNETA};
use crate::own::Owned;
use crate::projects::{project, workstream};
use crate::worktree::{cloned_as_wt, git};

fn started(host: &Host, from: &Path, what: &str, env: &[(&str, String)]) -> Output {
    let mut luneta = host.vars().command(LUNETA);
    lunetad::local_git(&mut luneta);
    luneta.current_dir(from).args(["start", what]);
    luneta.envs(env.iter().map(|(name, value)| (*name, value)));
    luneta.output().unwrap()
}

fn said(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "luneta start failed: {stderr}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn refused(output: &Output) -> String {
    assert!(!output.status.success(), "luneta start went ahead");
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().last().unwrap_or_default().to_owned()
}

fn projects(host: &Host) -> Vec<Project> {
    let Some(Held::Kept(kept)) = host.host().projects.and_then(|projects| projects.held) else {
        panic!("lunetad keeps no projects");
    };
    kept.projects
}

fn folders(host: &Host) -> Vec<(String, String, Vec<String>)> {
    let folders = |project: Project| {
        let workstreams = project.workstreams.into_iter().map(|it| it.folder).collect();
        (project.name, project.dir, workstreams)
    };
    projects(host).into_iter().map(folders).collect()
}

fn shown(dir: &Path) -> String {
    format!("{}\n", dir.display())
}

#[test]
fn a_folder_starts_in_the_project_it_is_in_or_one_made_of_it_and_luneta_goes_there() {
    let owned = Owned::new();
    let home = fs::canonicalize(owned.host.home.path()).unwrap();
    let (work, docs, notes) = (home.join("work"), home.join("work/docs"), home.join("work/notes"));
    fs::create_dir_all(&docs).unwrap();
    fs::create_dir_all(&notes).unwrap();

    let made = started(&owned.host, &home, "work", &[]);
    assert_eq!(said(&made), shown(&work));
    let stderr = String::from_utf8_lossy(&made.stderr);
    assert!(stderr.contains("is a project with no workstream there"), "{stderr}");
    let (work_s, docs_s) = (work.display().to_string(), docs.display().to_string());
    assert_eq!(folders(&owned.host), vec![("work".to_owned(), work_s.clone(), vec![])]);
    assert_eq!(said(&started(&owned.host, &work, "docs", &[])), shown(&docs));
    let made = vec![("work".to_owned(), work_s.clone(), vec![docs_s.clone()])];
    assert_eq!(folders(&owned.host), made);
    assert_eq!(said(&started(&owned.host, &docs, ".", &[])), shown(&docs));
    assert_eq!(folders(&owned.host), made, "a workstream there already is the one started");
    assert_eq!(said(&started(&owned.host, &docs, "..", &[])), shown(&work));
    assert_eq!(folders(&owned.host), made, "a project's own folder starts its last workstream");

    let here = WorkstreamId::of(projects(&owned.host)[0].workstreams[0].id).unwrap();
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    let (heard, hearing) = mpsc::channel();
    let size = Size::of(80, 24).unwrap();
    let at = Where::Here { runs_in: None };
    let show = lunetad::show(folder, at, here, size, false, move |it| drop(heard.send(it)));
    let argv = ["sleep", "600"].map(str::to_owned).to_vec();
    show.ask(Asked::Open { beside: Beside::Alone, argv });
    let laid = eventually("the view lays out the terminal", || match hearing.try_recv() {
        Ok(Heard::Laid(laid)) if !laid.is_empty() => Some(laid),
        Ok(_) | Err(_) => None,
    });
    let key = laid.focused().and_then(lunetad::Pane::key).expect("a terminal of the mux's");
    let inside = [
        ("LUNETA_MUX_TERMINAL", key.id.to_string()),
        ("LUNETA_MUX_TERMINAL_MADE", key.made.nanos().to_string()),
    ];
    assert_eq!(said(&started(&owned.host, &home, "work/notes", &inside)), shown(&notes));
    let notes_s = notes.display().to_string();
    let both = vec![("work".to_owned(), work_s, vec![docs_s, notes_s])];
    assert_eq!(folders(&owned.host), both);
    let there = WorkstreamId::of(projects(&owned.host)[0].workstreams[1].id).unwrap();
    let sent = eventually("luneta is sent to the workstream started", || {
        match hearing.try_recv() {
            Ok(Heard::Sent(to)) => Some(to),
            Ok(_) | Err(_) => None,
        }
    });
    assert_eq!(sent, there);

    assert_eq!(refused(&started(&owned.host, &home, "./gone", &[])), format!(
        "luneta: {} is no folder",
        home.join("./gone").display()
    ));
}

#[test]
fn a_branch_starts_in_a_worktree_of_the_clone_wt_made_and_only_there() {
    let owned = Owned::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let (main, feat) = (clone.join("main"), clone.join("feat"));

    assert_eq!(said(&started(&owned.host, &main, "feat", &[])), shown(&feat));
    assert!(feat.join(".git").is_file(), "wt made the worktree");
    let feat_s = feat.display().to_string();
    let made = vec![("n".to_owned(), clone.display().to_string(), vec![feat_s])];
    assert_eq!(folders(&owned.host), made);
    assert_eq!(said(&started(&owned.host, &main, "feat", &[])), shown(&feat));
    assert_eq!(folders(&owned.host), made, "the worktree's workstream is started again");

    git(&scratch, &["clone", "-q", &up.display().to_string(), "plain"]);
    let plain = refused(&started(&owned.host, &scratch.join("plain"), "feat", &[]));
    assert!(plain.contains("is in a clone git made"), "{plain}");
    let nowhere = refused(&started(&owned.host, &scratch, "feat", &[]));
    assert!(nowhere.starts_with("luneta: wt add: "), "{nowhere}");
}

#[test]
fn a_branch_joins_the_project_of_its_clone_made_at_a_worktree_which_widens_to_the_clone() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("n"));
    cloned_as_wt(&scratch, &up, &clone);
    let (main, feat) = (clone.join("main"), clone.join("feat"));
    let made = project(host, &main, None);
    workstream(host, made, &main);

    assert_eq!(said(&started(host, &main, "feat", &[])), shown(&feat));
    let both = [&main, &feat].map(|it| it.display().to_string()).to_vec();
    let widened = vec![("main".to_owned(), clone.display().to_string(), both)];
    assert_eq!(folders(host), widened);
    assert_eq!(projects(host)[0].id, made, "the workstream joins the project there was");


    let two = scratch.join("m");
    cloned_as_wt(&scratch, &scratch.join("up-m"), &two);
    let bare = two.join(".bare");
    git(&bare, &["worktree", "add", "-q", "../docs", "feat"]);
    git(&bare, &["branch", "next", "main"]);
    project(host, &two.join("main"), None);
    project(host, &two.join("docs"), None);
    let many = refused(&started(host, &two.join("main"), "next", &[]));
    assert!(many.contains("projects main, docs are all of the clone in"), "{many}");
    assert_eq!(projects(host).len(), 3, "no project is made for it");
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
    git(&bare, &["config", &format!("url.{}.insteadOf", up.display()), github]);
    host.gh.issue(7, "https://github.com/o/n/issues/7");
    let asked = host.home.path().join("claude-asked");
    let claude = format!("printf '%s\\n' \"$@\" > {}\nexec sleep 600\n", asked.display());
    script(&host.zellij.bin().join("claude"), &claude);

    let issue = clone.join("issue-7");
    assert_eq!(said(&started(host, &clone.join("main"), "#7", &[])), shown(&issue));
    assert!(issue.join(".git").is_file(), "the issue's worktree is made");
    let agent = eventually("an agent starts on the issue", || fs::read_to_string(&asked).ok());
    assert_eq!(agent, "Tackle https://github.com/o/n/issues/7\n");
    let issue_s = issue.display().to_string();
    let made = vec![("n".to_owned(), clone.display().to_string(), vec![issue_s])];
    assert_eq!(folders(host), made);
    let terminals = || projects(host)[0].workstreams[0].terminals.len();
    assert_eq!(terminals(), 1);

    let link = "https://github.com/o/n/issues/7";
    assert_eq!(said(&started(host, &clone.join("main"), link, &[])), shown(&issue));
    assert_eq!((folders(host), terminals()), (made, 1), "no second agent starts on it");
    let unknown = refused(&started(host, &clone.join("main"), "#8", &[]));
    assert!(unknown.starts_with("luneta: gh issue view: "), "{unknown}");
}
