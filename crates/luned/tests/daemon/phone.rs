use std::fs;
use std::sync::{mpsc, Arc, Mutex};

use lune_phone::{
    Came, Core, Held, Landed, Marked, Model, Observer, Order, Plan, Pruned, Rebase, Step,
    Unanswered, View, Worktree,
};
use lune_waits::{eventually, Patience, PATIENCE};

use crate::host::{folder_in, script, start, Host};
use crate::own::Owned;
use crate::projects::project;
use crate::prune::{self, merged, serving};
use crate::rebase::{self, commit};
use crate::remote::{claude_that_takes_turns, claude_that_writes_issues, hosts, reached, told, FakeSsh};
use crate::worktree::{cloned_as_wt, git};

enum Heard {
    Changed(Model),
    Lost(String),
}

struct Told(Mutex<mpsc::Sender<Heard>>);

impl Observer for Told {
    fn changed(&self, model: Model) {
        let _ = self.0.lock().unwrap().send(Heard::Changed(model));
    }

    fn lost(&self, why: String) {
        let _ = self.0.lock().unwrap().send(Heard::Lost(why));
    }
}

struct Phone {
    core: Arc<Core>,
    heard: mpsc::Receiver<Heard>,
    runtime: tokio::runtime::Runtime,
}

impl Phone {
    fn of(host: &Host) -> Phone {
        let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
        let core = Core::over_socket(folder.socket(), PATIENCE).unwrap();
        let (tell, heard) = mpsc::channel();
        core.watch(Arc::new(Told(Mutex::new(tell))));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Phone { core, heard, runtime }
    }

    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    fn loses(&self, what: &str) -> String {
        let patience = Patience::begun();
        loop {
            if let Heard::Lost(why) = patience.heard(what, &self.heard) {
                return why;
            }
        }
    }

    fn sees(&self, what: &str, wanted: impl Fn(&Model) -> bool) -> Model {
        let patience = Patience::begun();
        loop {
            match patience.heard(what, &self.heard) {
                Heard::Changed(model) if wanted(&model) => return model,
                Heard::Changed(_) | Heard::Lost(_) => {},
            }
        }
    }

    fn steps(&self, asking: &lune_phone::Asking) -> Result<Vec<Step>, Unanswered> {
        let patience = Patience::begun();
        let mut steps = Vec::new();
        while let Some(step) = self.block_on(patience.awaited("the next step", asking.next()))? {
            steps.push(step);
        }
        Ok(steps)
    }
}

fn projects_here(model: &Model) -> &[lune_phone::Project] {
    match &model.machines[0].held {
        Held::Kept { projects } => projects,
        held => panic!("this machine keeps its projects: {held:?}"),
    }
}

fn worktree_of(model: &Model, worktree: u64) -> Option<&Worktree> {
    projects_here(model).iter().flat_map(|it| &it.worktrees).find(|it| it.id == worktree)
}

fn started_view(model: &Model, worktree: u64) -> Option<View> {
    let view = worktree_of(model, worktree)?.views.first()?;
    view.terminal.is_some().then(|| view.clone())
}

fn names(model: &Model) -> Vec<&str> {
    projects_here(model).iter().map(|it| it.name.as_str()).collect()
}

fn refused<T: std::fmt::Debug>(answered: Result<T, Unanswered>) -> String {
    match answered {
        Err(Unanswered::Refused { why }) => why,
        answered => panic!("luned refuses it: {answered:?}"),
    }
}

#[test]
fn the_phone_hears_this_machine_s_projects_and_renames_and_reorders_them() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    for dir in ["a", "b"] {
        fs::create_dir(scratch.join(dir)).unwrap();
    }
    let a = project(&host, &scratch.join("a"), Some("alpha"));
    let b = project(&host, &scratch.join("b"), Some("beta"));
    let phone = Phone::of(&host);
    phone.sees("both projects, in the order made", |model| names(model) == ["alpha", "beta"]);

    phone.block_on(phone.core.rename_project(None, a, "gamma".to_owned())).unwrap();
    phone.sees("the project renamed", |model| names(model) == ["gamma", "beta"]);
    let taken = refused(phone.block_on(phone.core.rename_project(None, b, "gamma".to_owned())));
    assert!(taken.contains("gamma"), "{taken}");

    let reorder = Order { seen: vec![a, b], wanted: vec![b, a] };
    phone.block_on(phone.core.reorder_projects(None, reorder)).unwrap();
    phone.sees("the projects reordered", |model| names(model) == ["beta", "gamma"]);
    let stale = phone.core.reorder_projects(None, Order { seen: vec![a, b], wanted: vec![a, b] });
    let stale = refused(phone.block_on(stale));
    assert!(stale.contains("changed since"), "{stale}");

    drop(daemon);
    phone.loses("luned ended");
    let _again = start(host.vars()).unwrap();
    phone.sees("the projects, once luned is back", |model| names(model) == ["beta", "gamma"]);
}

#[test]
fn the_phone_renames_and_reorders_a_clone_s_worktrees_and_closes_a_view_and_a_worktree() {
    let owned = Owned::new();
    let (made, main, two) = crate::layouts::two_in_a_clone(&owned, "clone");
    let phone = Phone::of(&owned.host);
    let model = phone.sees("both worktrees' views run", |model| {
        started_view(model, main).is_some() && started_view(model, two).is_some()
    });
    let order = |model: &Model| -> Vec<u64> {
        let project = projects_here(model).iter().find(|it| it.id == made);
        project.map_or_default(|it| it.worktrees.iter().map(|it| it.id).collect())
    };
    assert_eq!(order(&model), [main, two]);
    let (view, last) = (started_view(&model, main).unwrap(), started_view(&model, two).unwrap());

    phone.block_on(phone.core.rename_worktree(None, main, Some("desk".to_owned()))).unwrap();
    phone.block_on(phone.core.rename_view(None, view.id, Some("server".to_owned()))).unwrap();
    phone.sees("the worktree and its view renamed", |model| {
        let renamed = worktree_of(model, main).is_some_and(|it| it.renamed && it.name == "desk");
        renamed && started_view(model, main).is_some_and(|it| it.name.as_deref() == Some("server"))
    });
    let reorder = Order { seen: vec![main, two], wanted: vec![two, main] };
    let reorder = phone.core.reorder_worktrees(None, made, reorder);
    phone.block_on(reorder).unwrap();
    phone.sees("the worktrees reordered", |model| order(model) == [two, main]);

    phone.block_on(phone.core.close_view(None, last.terminal.unwrap())).unwrap();
    phone.sees("the last view closed takes its worktree", |model| order(model) == [main]);
    let stale = refused(phone.block_on(phone.core.close_worktree(None, main, Vec::new())));
    assert!(stale.contains("changed since"), "{stale}");
    phone.block_on(phone.core.close_worktree(None, main, vec![view.id])).unwrap();
    phone.sees("the worktree closed", |model| order(model).is_empty());
}

#[test]
fn a_new_agent_starts_alone_in_its_branch_s_worktree_on_the_words_the_phone_gave() {
    let owned = Owned::new();
    let host = &owned.host;
    let asked = host.home.path().join("claude-asked");
    let claude = format!("printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n", asked.display());
    script(&host.bin().join("claude"), &claude);
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let clone = scratch.join("lune");
    cloned_as_wt(&scratch, &scratch.join("up"), &clone);
    git(&clone.join(".bare"), &["update-ref", "refs/remotes/origin/main", "main"]);
    let lune = project(host, &clone, None);
    let phone = Phone::of(host);

    let words = "Make the button blue\nand say so".to_owned();
    let landed = phone.core.new_agent(None, lune, "feat".to_owned(), words.clone());
    let Landed::Made { worktree } = phone.block_on(landed).unwrap() else {
        panic!("a branch with no worktree gets one made");
    };
    let told = eventually("claude starts in the worktree", || fs::read_to_string(&asked).ok());
    assert!(told.ends_with(&format!("{words}\n")), "{told}");
    let model = phone.sees("the agent's view runs", |model| started_view(model, worktree).is_some());
    let feat = worktree_of(&model, worktree).unwrap();
    assert_eq!((feat.folder.as_str(), feat.views.len()), (clone.join("feat").to_str().unwrap(), 1));

    fs::remove_file(&asked).unwrap();
    let again = phone.core.new_agent(None, lune, "feat".to_owned(), "Again".to_owned());
    assert_eq!(phone.block_on(again).unwrap(), Landed::Found { worktree });
    let blank = phone.core.new_agent(None, lune, "other".to_owned(), "  ".to_owned());
    let blank = refused(phone.block_on(blank));
    assert!(blank.contains("no words"), "{blank}");
    assert!(!clone.join("other").exists(), "words refused make no worktree");
    assert!(!asked.exists(), "a worktree found starts no second agent");
}

#[test]
fn the_phone_rebases_a_branch_onto_upstream_and_hears_why_not() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = rebase::Repository::new(&scratch);
    let fix = repository.fix.to_str().unwrap().to_owned();
    let phone = Phone::of(&host);

    commit(&repository.fix, "own", "o\n");
    commit(&repository.up, "theirs", "t\n");
    fs::write(repository.fix.join("own"), "not committed\n").unwrap();
    let dirty = refused(phone.block_on(phone.core.rebase(None, fix.clone(), None)));
    assert!(dirty.ends_with("has changes not committed: commit or stash them first"), "{dirty}");
    git(&repository.fix, &["checkout", "-q", "own"]);
    let rebased = phone.block_on(phone.core.rebase(None, fix.clone(), None)).unwrap();
    assert_eq!((rebased.branch.as_str(), rebased.onto.as_str()), ("fix", "origin/main"));
    assert_eq!(rebased.came, Rebase::Clean { ahead: 1 });

    commit(&repository.fix, "theirs", "mine\n");
    commit(&repository.up, "theirs", "yours\n");
    let stopped = phone.block_on(phone.core.rebase(None, fix, Some("fix".to_owned()))).unwrap();
    let Rebase::Stopped { conflicted, conflicted_count, .. } = stopped.came else {
        panic!("a conflict stops the rebase: {stopped:?}");
    };
    assert_eq!((conflicted, conflicted_count), (vec!["theirs".to_owned()], 1));
}

#[test]
fn the_phone_plans_a_prune_and_deletes_what_it_agreed_to_once_the_plan_stands() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = prune::Repository::new(&scratch);
    host.gh.api("me\n");
    let mine = repository.pushed("mine");
    host.gh.open("mine", &merged(1, &mine, "me"));
    let _serving = serving(&host, &repository.at("main"));
    let main = repository.at("main").to_str().unwrap().to_owned();
    let phone = Phone::of(&host);

    let plan = phone.block_on(phone.core.prunes(None, main.clone())).unwrap();
    let Plan { goes, .. } = &plan;
    let mine_at = repository.at("mine").to_str().unwrap().to_owned();
    assert_eq!(goes.iter().map(|it| (it.folder.as_str(), it.number)).collect::<Vec<_>>(), [(mine_at.as_str(), 1)]);

    let mut stale = goes[0].clone();
    stale.head = "0".repeat(40);
    let changed = phone.block_on(phone.core.prune(None, main.clone(), vec![stale])).unwrap();
    let Pruned::Changed { plan: now } = changed else {
        panic!("a plan agreed to that is not luned's deletes nothing: {changed:?}");
    };
    let heads = |plan: &Plan| plan.goes.iter().map(|it| (it.folder.clone(), it.head.clone())).collect::<Vec<_>>();
    assert_eq!(heads(&now), heads(&plan), "and says luned's own");
    assert!(repository.at("mine").exists());

    let pruned = phone.block_on(phone.core.prune(None, main, plan.goes)).unwrap();
    let Pruned::Went { outcomes } = pruned else {
        panic!("the plan agreed to is deleted: {pruned:?}");
    };
    let came: Vec<(&str, &Came)> = outcomes.iter().map(|it| (it.folder.as_str(), &it.came)).collect();
    assert_eq!(came, [(mine_at.as_str(), &Came::Gone)]);
    assert!(!repository.at("mine").exists());
}

#[test]
fn the_phone_files_an_issue_on_the_machine_it_names() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    claude_that_writes_issues(&far);
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let dir = far.home.path().join("w");
    fs::create_dir(&dir).unwrap();
    for args in [&["init", "--quiet"][..], &["remote", "add", "origin", "git@github.com:o/n.git"]] {
        git(&dir, args);
    }
    let dir = dir.to_str().unwrap().to_owned();
    let phone = Phone::of(&here);
    let far_on = || Some("far".to_owned());

    let listed = phone.block_on(phone.core.issue_repositories(far_on(), dir.clone())).unwrap();
    let listed: Vec<(&str, &str)> = listed.iter().map(|it| (it.remote.as_str(), it.repository.as_str())).collect();
    assert_eq!(listed, [("origin", "o/n")]);
    let filed = phone.core.file_issue(far_on(), dir.clone(), "o/n".to_owned(), "a red button".to_owned());
    let filed = phone.block_on(filed).unwrap();
    assert_eq!((filed.url.as_str(), filed.title.as_str()), ("https://github.com/o/n/issues/360", "A red button"));
    assert!(far.gh.created().is_some() && here.gh.created().is_none(), "filed on far alone");
    let relative = phone.core.issue_repositories(far_on(), "w".to_owned());
    assert!(refused(phone.block_on(relative)).contains("absolute"));
}

#[test]
fn the_phone_asks_in_turns_and_tackles_what_it_marked() {
    let host = Host::new();
    claude_that_takes_turns(&host);
    let _daemon = start(host.vars()).unwrap();
    let dir = fs::canonicalize(host.home.path()).unwrap().join("w");
    fs::create_dir(&dir).unwrap();
    let phone = Phone::of(&host);
    let marked = || Marked { issues: Vec::new(), pulls: Vec::new() };
    let on = |told: &[String], key: &str| -> String {
        let line = told.iter().find_map(|line| line.strip_prefix(&format!("{key}=")));
        line.unwrap_or_else(|| panic!("claude was told its {key}: {told:?}")).to_owned()
    };

    let cwd = Some(dir.to_str().unwrap().to_owned());
    let first = phone.core.ask(None, cwd, "which agents run".to_owned(), None, marked());
    let session = Step::Session { id: "7c22".to_owned() };
    let ran = Step::Ran { id: "toolu_1".to_owned(), command: "lune agents".to_owned() };
    let said = Step::Said { text: "No agent runs.".to_owned() };
    assert_eq!(phone.steps(&first).unwrap(), [session.clone(), ran.clone(), said.clone()]);
    let (told_first, words) = told(&host);
    assert_eq!((on(&told_first, "cwd"), on(&told_first, "before")), (dir.display().to_string(), "unset".to_owned()));
    assert_eq!(words, "which agents run");

    let again = phone.core.ask(None, None, "yes".to_owned(), Some("7c22".to_owned()), marked());
    assert_eq!(phone.steps(&again).unwrap(), [session.clone(), ran, said]);
    let (told_again, _) = told(&host);
    assert_eq!(on(&told_again, "resumed"), "7c22");
    assert_eq!(on(&told_again, "before"), on(&told_first, "turn"), "the turn before is the session's last");
    assert_ne!(on(&told_again, "turn"), on(&told_first, "turn"));

    let these = Marked { issues: vec![7], pulls: vec![12] };
    let tackled = phone.core.tackle(None, dir.to_str().unwrap().to_owned(), these);
    phone.steps(&tackled).unwrap();
    let (_, words) = told(&host);
    assert!(words.ends_with(&format!("\n\n{}", lune_phone::TACKLE)), "{words}");
    assert!(words.contains("the user marked issue #7, and pull request #12"), "{words}");

    let failing = phone.core.ask(None, None, "please-fail".to_owned(), None, marked());
    assert_eq!(refused(phone.steps(&failing)), "Not logged in");
    let blank = phone.core.ask(None, None, " ".to_owned(), None, marked());
    assert!(refused(phone.steps(&blank)).contains("nothing asked"));
}

#[test]
fn the_phone_waits_for_each_verb_as_long_as_luned_may_work_at_it() {
    use lune_phone::waits;
    assert!(waits::ANSWER > luned::WATCH, "a watch parks for less than the phone waits");
    assert!(waits::TURN >= luned::asks::TURN);
    assert!(waits::MAKING >= luned::links::WORKTREE_PATIENCE);
    assert!(waits::LISTING >= luned::work::ANSWERED_MAX);
    assert!(waits::PLANNING >= luned::prunes::PLANNING);
    assert!(waits::DELETING >= luned::prunes::PLANNING + luned::prunes::DELETING);
    assert!(waits::REBASING >= luned::rebases::REBASING);
    assert!(waits::FILING >= luned::issues::AT_MOST);
}
