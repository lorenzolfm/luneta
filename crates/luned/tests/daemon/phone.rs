use std::fs;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};

use lune_phone::{
    Appearance, Came, Core, Found, Framed, Grid, Held, KeyName, Landed, Listed, Marked, Model,
    NonZeroDuration, Observer, Order, Plan, Pruned, Rebase, Refusals, Step, TerminalView,
    Unanswered, View, Worktree, waits,
};
use lune_waits::{OUTLASTING, PATIENCE, Patience, eventually, never, shell};

use crate::host::{Host, folder_in, script, start};
use crate::own::Owned;
use crate::projects::project;
use crate::prune::{self, merged, serving};
use crate::rebase::{self, commit};
use crate::remote::{
    FakeSsh, claude_that_takes_turns, claude_that_writes_issues, hosts, reached, told,
};
use crate::worktree::{cloned_as_wt, git};

#[derive(Debug)]
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

const TEST_PATIENCE: NonZeroDuration = NonZeroDuration::new(PATIENCE).unwrap();
const TEST_OUTLASTING: NonZeroDuration = NonZeroDuration::new(OUTLASTING).unwrap();

impl Phone {
    fn of(host: &Host) -> Phone {
        let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
        Phone::watching(Core::over_socket(folder.socket(), TEST_PATIENCE, waits::again).unwrap())
    }

    fn backing_off(host: &Host) -> Phone {
        let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
        Phone::watching(
            Core::over_socket(folder.socket(), TEST_PATIENCE, |_, _| TEST_OUTLASTING).unwrap(),
        )
    }

    fn watching(core: Arc<Core>) -> Phone {
        let (tell, heard) = mpsc::channel();
        core.watch(Arc::new(Told(Mutex::new(tell))));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Phone {
            core,
            heard,
            runtime,
        }
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
                Heard::Changed(_) | Heard::Lost(_) => {}
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
    match &model.here {
        Held::Kept { projects } => projects,
        held => panic!("this machine keeps its projects: {held:?}"),
    }
}

fn worktree_of(model: &Model, worktree: u64) -> Option<&Worktree> {
    projects_here(model)
        .iter()
        .flat_map(|it| it.holds.worktrees())
        .find(|it| it.id == worktree)
}

fn started_view(model: &Model, worktree: u64) -> Option<View> {
    let view = worktree_of(model, worktree)?.views.first()?;
    matches!(view.found, Found::Key { .. }).then(|| view.clone())
}

fn texts(grid: &Grid) -> Vec<String> {
    let line = |row: &lune_phone::Row| {
        let mut line = String::new();
        for run in &row.runs {
            let at = usize::from(run.column);
            let drawn = line.chars().count();
            line.extend(std::iter::repeat_n(' ', at.saturating_sub(drawn)));
            line.push_str(&run.text);
        }
        line.trim_end().to_owned()
    };
    grid.rows.iter().map(line).collect()
}

fn shows(phone: &Phone, view: &TerminalView, what: &str, shown: impl Fn(&Grid) -> bool) -> Grid {
    found(phone, view, what, |grid| shown(grid).then(|| grid.clone()))
}

fn found<T>(
    phone: &Phone,
    view: &TerminalView,
    what: &str,
    find: impl Fn(&Grid) -> Option<T>,
) -> T {
    let patience = Patience::begun();
    let mut last = None;
    loop {
        let waited = format!("{what}, not only {:?}", last.as_ref().map(texts));
        match phone.block_on(patience.awaited(waited, view.next())) {
            Framed::Shown { grid } => match find(&grid) {
                Some(found) => return found,
                None => last = Some(grid),
            },
            ended @ (Framed::Closed | Framed::Lost { .. } | Framed::Refused { .. }) => {
                panic!("the terminal is not shown: {ended:?}")
            }
        }
    }
}

fn screen_of(grid: &Grid) -> shell::Screen {
    let cursor = grid.cursor.as_ref().map(|cursor| shell::Cursor {
        row: usize::from(cursor.row),
        column: usize::from(cursor.column),
    });
    shell::Screen::new(texts(grid), cursor)
        .unwrap_or_else(|why| panic!("the terminal's screen is broken: {why}"))
}

fn prompted(phone: &Phone, view: &TerminalView) -> (Grid, shell::Prompted) {
    found(phone, view, "the shell's prompt", |grid| {
        shell::prompted(&screen_of(grid)).map(|it| (grid.clone(), it))
    })
}

fn said(phone: &Phone, view: &TerminalView, typed: &shell::Typed) -> Vec<String> {
    let what = format!("what {:?} says, then the shell's next prompt", typed.line());
    found(phone, view, &what, |grid| {
        typed.said(&screen_of(grid)).map(<[String]>::to_vec)
    })
}

fn ends(phone: &Phone, view: &TerminalView, what: &str) -> Framed {
    let patience = Patience::begun();
    loop {
        match phone.block_on(patience.awaited(what, view.next())) {
            Framed::Shown { .. } => {}
            ended @ (Framed::Closed | Framed::Lost { .. } | Framed::Refused { .. }) => {
                return ended;
            }
        }
    }
}

fn names(model: &Model) -> Vec<&str> {
    projects_here(model)
        .iter()
        .map(|it| it.name.as_str())
        .collect()
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
    phone.sees("both projects, in the order made", |model| {
        names(model) == ["alpha", "beta"]
    });

    phone
        .block_on(phone.core.rename_project(None, a, "gamma".to_owned()))
        .unwrap();
    phone.sees("the project renamed", |model| {
        names(model) == ["gamma", "beta"]
    });
    let taken = refused(phone.block_on(phone.core.rename_project(None, b, "gamma".to_owned())));
    assert!(taken.contains("gamma"), "{taken}");

    let reorder = Order {
        seen: vec![a, b],
        wanted: vec![b, a],
    };
    phone
        .block_on(phone.core.reorder_projects(None, reorder))
        .unwrap();
    phone.sees("the projects reordered", |model| {
        names(model) == ["beta", "gamma"]
    });
    let stale = phone.core.reorder_projects(
        None,
        Order {
            seen: vec![a, b],
            wanted: vec![a, b],
        },
    );
    let stale = refused(phone.block_on(stale));
    assert!(stale.contains("changed since"), "{stale}");

    drop(daemon);
    phone.loses("luned ended");
    let _again = start(host.vars()).unwrap();
    phone.sees("the projects, once luned is back", |model| {
        names(model) == ["beta", "gamma"]
    });
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
        project.map_or_default(|it| it.holds.worktrees().iter().map(|it| it.id).collect())
    };
    assert_eq!(order(&model), [main, two]);
    let (view, last) = (
        started_view(&model, main).unwrap(),
        started_view(&model, two).unwrap(),
    );

    phone
        .block_on(
            phone
                .core
                .rename_worktree(None, main, Some("desk".to_owned())),
        )
        .unwrap();
    phone
        .block_on(
            phone
                .core
                .rename_view(None, view.id, Some("server".to_owned())),
        )
        .unwrap();
    phone.sees("the worktree and its view renamed", |model| {
        let renamed = worktree_of(model, main).is_some_and(|it| it.renamed && it.name == "desk");
        renamed && started_view(model, main).is_some_and(|it| it.name.as_deref() == Some("server"))
    });
    let reorder = Order {
        seen: vec![main, two],
        wanted: vec![two, main],
    };
    let reorder = phone.core.reorder_worktrees(None, made, reorder);
    phone.block_on(reorder).unwrap();
    phone.sees("the worktrees reordered", |model| {
        order(model) == [two, main]
    });

    let Found::Key { key } = last.found else {
        panic!("the last view has its terminal: {last:?}");
    };
    phone.block_on(phone.core.close_view(key)).unwrap();
    phone.sees("the last view closed takes its worktree", |model| {
        order(model) == [main]
    });
    let stale = refused(phone.block_on(phone.core.close_worktree(None, main, Vec::new())));
    assert!(stale.contains("changed since"), "{stale}");
    phone
        .block_on(phone.core.close_worktree(None, main, vec![view.id]))
        .unwrap();
    phone.sees("the worktree closed", |model| order(model).is_empty());
}

#[test]
fn the_phone_shows_a_terminal_types_into_it_scrolls_back_lets_it_go_and_hears_it_close() {
    let owned = Owned::new();
    let worktree = crate::layouts::worktree_in_home(&owned);
    let mut shown = crate::layouts::Shown::new(&owned, worktree, 80, 24);
    shown.open_running(&shell::AT_A_PROMPT);
    shown.open_running(&["sleep", "600"]);
    let phone = Phone::of(&owned.host);
    let keyed = |model: &Model| {
        let views = worktree_of(model, worktree).map_or(&[][..], |it| it.views.as_slice());
        views
            .iter()
            .filter(|view| matches!(view.found, Found::Key { .. }))
            .count()
    };
    let model = phone.sees("both views run", |model| keyed(model) == 2);
    let Some(View {
        found: Found::Key { key: shell },
        ..
    }) = started_view(&model, worktree)
    else {
        panic!("the shell's view has its terminal");
    };
    let view = phone.core.terminal(&shell, Appearance::Dark);
    let (first, at_the_prompt) = prompted(&phone, &view);
    assert_eq!(first.scrolled_back, 0);

    view.typed("echo wr''ong", false).unwrap();
    view.typed("u", true).unwrap();
    let right = at_the_prompt.typed("echo right-$((40 + 2))");
    view.typed(right.line(), false).unwrap();
    view.pressed(KeyName::Enter, false).unwrap();
    assert_eq!(
        said(&phone, &view, &right),
        ["right-42"],
        "ctrl-u let the first line go"
    );

    view.typed("seq 1 200", false).unwrap();
    view.pressed(KeyName::Enter, false).unwrap();
    shows(
        &phone,
        &view,
        "the shell counts to 200, then prompts",
        |grid| {
            let screen = screen_of(grid);
            let above = screen.cursor().and_then(|cursor| cursor.row.checked_sub(1));
            shell::prompted(&screen).is_some()
                && above.map(|row| screen.rows()[row].as_str()) == Some("200")
        },
    );
    view.scroll(5).unwrap();
    let back = shows(&phone, &view, "the screen five rows back", |grid| {
        grid.scrolled_back == 5
    });
    assert!(back.scrollback >= 5, "{back:?}");
    view.pressed(KeyName::Left, false).unwrap();
    shows(
        &phone,
        &view,
        "a key brings the screen back to now",
        |grid| grid.scrolled_back == 0,
    );

    let again = phone.core.terminal(&shell, Appearance::Light);
    shows(&phone, &again, "the terminal's screen shown twice", |_| {
        true
    });
    again.close();
    assert!(matches!(phone.block_on(again.next()), Framed::Lost { .. }));
    let gone = again.typed("x", false);
    assert!(matches!(gone, Err(Unanswered::Refused { .. })), "{gone:?}");

    phone
        .block_on(phone.core.close_view(shell.clone()))
        .unwrap();
    assert_eq!(
        ends(&phone, &view, "the shown terminal closes"),
        Framed::Closed
    );
    let closed = view.typed("x", false);
    assert!(
        matches!(closed, Err(Unanswered::Refused { .. })),
        "{closed:?}"
    );
    phone.sees("the shell's view gone", |model| keyed(model) == 1);
    let unmade = phone.core.terminal(&shell, Appearance::Light);
    let refused = phone.block_on(Patience::begun().awaited("luned refuses it", unmade.next()));
    assert!(
        matches!(refused, Framed::Refused { .. }),
        "a closed terminal is no one's to show: {refused:?}"
    );
}

#[test]
fn a_new_agent_starts_alone_in_its_branch_s_worktree_on_the_words_the_phone_gave() {
    let owned = Owned::new();
    let host = &owned.host;
    let asked = host.home.path().join("claude-asked");
    let claude = format!(
        "printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n",
        asked.display()
    );
    script(&host.bin().join("claude"), &claude);
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let clone = scratch.join("lune");
    cloned_as_wt(&scratch, &scratch.join("up"), &clone);
    git(
        &clone.join(".bare"),
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    let lune = project(host, &clone, None);
    let phone = Phone::of(host);

    let words = "Make the button blue\nand say so".to_owned();
    let landed = phone
        .core
        .new_agent(None, lune, "feat".to_owned(), words.clone());
    let Landed::Started { worktree } = phone.block_on(landed).unwrap() else {
        panic!("a branch with no worktree gets one made");
    };
    let told = eventually("claude starts in the worktree", || {
        fs::read_to_string(&asked).ok()
    });
    assert!(told.ends_with(&format!("{words}\n")), "{told}");
    let model = phone.sees("the agent's view runs", |model| {
        started_view(model, worktree).is_some()
    });
    let feat = worktree_of(&model, worktree).unwrap();
    assert_eq!(
        (feat.folder.as_str(), feat.views.len()),
        (clone.join("feat").to_str().unwrap(), 1)
    );

    fs::remove_file(&asked).unwrap();
    let again = phone
        .core
        .new_agent(None, lune, "feat".to_owned(), "Again".to_owned());
    assert_eq!(phone.block_on(again).unwrap(), Landed::Found { worktree });
    let blank = phone
        .core
        .new_agent(None, lune, "other".to_owned(), "  ".to_owned());
    let blank = refused(phone.block_on(blank));
    assert!(blank.contains("no words"), "{blank}");
    assert!(
        !clone.join("other").exists(),
        "words refused make no worktree"
    );
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
    assert!(
        dirty.ends_with("has changes not committed: commit or stash them first"),
        "{dirty}"
    );
    git(&repository.fix, &["checkout", "-q", "own"]);
    let rebased = phone
        .block_on(phone.core.rebase(None, fix.clone(), None))
        .unwrap();
    assert_eq!(
        (rebased.branch.as_str(), rebased.onto.as_str()),
        ("fix", "origin/main")
    );
    assert_eq!(rebased.came, Rebase::Clean { ahead: 1 });

    commit(&repository.fix, "theirs", "mine\n");
    commit(&repository.up, "theirs", "yours\n");
    let stopped = phone
        .block_on(phone.core.rebase(None, fix, Some("fix".to_owned())))
        .unwrap();
    let Rebase::Stopped { named, unnamed, .. } = stopped.came else {
        panic!("a conflict stops the rebase: {stopped:?}");
    };
    assert_eq!((named, unnamed), (vec!["theirs".to_owned()], 0));
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

    let plan = phone
        .block_on(phone.core.prunes(None, main.clone()))
        .unwrap();
    let Plan { goes, .. } = &plan;
    let mine_at = repository.at("mine").to_str().unwrap().to_owned();
    assert_eq!(
        goes.iter()
            .map(|it| (it.folder.as_str(), it.number))
            .collect::<Vec<_>>(),
        [(mine_at.as_str(), 1)]
    );

    let mut stale = goes[0].clone();
    stale.head = "0".repeat(40);
    let changed = phone
        .block_on(phone.core.prune(None, main.clone(), vec![stale]))
        .unwrap();
    let Pruned::Changed { plan: now } = changed else {
        panic!("a plan agreed to that is not luned's deletes nothing: {changed:?}");
    };
    let heads = |plan: &Plan| {
        plan.goes
            .iter()
            .map(|it| (it.folder.clone(), it.head.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(heads(&now), heads(&plan), "and says luned's own");
    assert!(repository.at("mine").exists());

    let pruned = phone
        .block_on(phone.core.prune(None, main, plan.goes))
        .unwrap();
    let Pruned::Went { outcomes } = pruned else {
        panic!("the plan agreed to is deleted: {pruned:?}");
    };
    let came: Vec<(&str, &Came)> = outcomes
        .iter()
        .map(|it| (it.folder.as_str(), &it.came))
        .collect();
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
    for args in [
        &["init", "--quiet"][..],
        &["remote", "add", "origin", "git@github.com:o/n.git"],
    ] {
        git(&dir, args);
    }
    let dir = dir.to_str().unwrap().to_owned();
    let phone = Phone::of(&here);
    let far_on = || Some("far".to_owned());

    let listed = phone
        .block_on(phone.core.issue_repositories(far_on(), dir.clone()))
        .unwrap();
    let listed: Vec<(&str, &str)> = listed
        .iter()
        .map(|it| (it.remote.as_str(), it.repository.as_str()))
        .collect();
    assert_eq!(listed, [("origin", "o/n")]);
    let filed = phone.core.file_issue(
        far_on(),
        dir.clone(),
        "o/n".to_owned(),
        "a red button".to_owned(),
    );
    let filed = phone.block_on(filed).unwrap();
    assert_eq!(
        (filed.url.as_str(), filed.title.as_str()),
        ("https://github.com/o/n/issues/360", "A red button")
    );
    assert!(
        far.gh.created().is_some() && here.gh.created().is_none(),
        "filed on far alone"
    );
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
    let on = |told: &[String], key: &str| -> String {
        let line = told
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{key}=")));
        line.unwrap_or_else(|| panic!("claude was told its {key}: {told:?}"))
            .to_owned()
    };

    let cwd = || Some(dir.to_str().unwrap().to_owned());
    let talk = phone.core.clone().converse(None, cwd());
    assert!(!talk.resumes());
    let first = talk.clone().ask("which agents run".to_owned()).unwrap();
    let ran = Step::Ran {
        id: "toolu_1".to_owned(),
        command: "lune agents".to_owned(),
    };
    let said = Step::Said {
        text: "No agent runs.".to_owned(),
    };
    assert_eq!(phone.steps(&first).unwrap(), [ran.clone(), said.clone()]);
    assert!(
        talk.resumes(),
        "the session luned named is the conversation's"
    );
    let (told_first, words) = told(&host);
    assert_eq!(
        (on(&told_first, "cwd"), on(&told_first, "before")),
        (dir.display().to_string(), "unset".to_owned())
    );
    assert_eq!(words, "which agents run");

    let again = talk.clone().ask("yes".to_owned()).unwrap();
    assert_eq!(phone.steps(&again).unwrap(), [ran.clone(), said]);
    let (told_again, _) = told(&host);
    assert_eq!(
        (on(&told_again, "resumed"), on(&told_again, "cwd")),
        ("7c22".to_owned(), dir.display().to_string())
    );
    assert_eq!(
        on(&told_again, "before"),
        on(&told_first, "turn"),
        "the turn before is the session's last"
    );
    assert_ne!(on(&told_again, "turn"), on(&told_first, "turn"));

    let unread = talk.clone().ask("and then".to_owned()).unwrap();
    let heard = phone.block_on(Patience::begun().awaited("the first step", unread.next()));
    assert_eq!(heard.unwrap(), Some(ran));
    let twice = talk.clone().ask("and then".to_owned());
    assert!(
        matches!(twice, Err(Unanswered::Refused { .. })),
        "a conversation takes one turn at a time"
    );
    drop(unread);
    let after = talk.clone().ask("yes".to_owned()).unwrap();
    phone.steps(&after).unwrap();
    let (told_after, _) = told(&host);
    let unread = "a turn left unread is none before, so no answer agrees to its plan";
    assert_eq!(
        (on(&told_after, "resumed"), on(&told_after, "before")),
        ("7c22".to_owned(), "unset".to_owned()),
        "{unread}"
    );

    let these = Marked {
        issues: vec![7],
        pulls: vec![12],
    };
    let tackled = phone
        .core
        .clone()
        .converse(None, cwd())
        .tackle(these)
        .unwrap();
    phone.steps(&tackled).unwrap();
    let (told_tackled, words) = told(&host);
    assert_eq!(
        on(&told_tackled, "resumed"),
        "none",
        "a new conversation resumes no session"
    );
    assert!(
        words.ends_with(&format!("\n\n{}", lune_phone::TACKLE)),
        "{words}"
    );
    assert!(
        words.contains("the user marked issue #7, and pull request #12"),
        "{words}"
    );

    let failing = phone
        .core
        .clone()
        .converse(None, None)
        .ask("please-fail".to_owned())
        .unwrap();
    assert_eq!(refused(phone.steps(&failing)), "Not logged in");
    let blank = phone
        .core
        .clone()
        .converse(None, None)
        .ask(" ".to_owned())
        .unwrap();
    assert!(refused(phone.steps(&blank)).contains("nothing asked"));
}

#[test]
fn a_turn_the_phone_lets_go_of_stops_its_claude_before_claude_says_anything() {
    let host = Host::new();
    let ran = host.home.path().join("claude-pid");
    let claude = format!(
        "echo $$ > {0}.part && mv {0}.part {0}\nexec sleep 600\n",
        ran.display()
    );
    script(&host.bin().join("claude"), &claude);
    let _daemon = start(host.vars()).unwrap();
    let phone = Phone::of(&host);

    let asking = phone
        .core
        .clone()
        .converse(None, None)
        .ask("take your time".to_owned())
        .unwrap();
    let pid = eventually("claude starts", || fs::read_to_string(&ran).ok());
    let alive = || {
        Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(), "claude {pid} runs");
    drop(asking);
    eventually("claude is stopped", || (!alive()).then_some(()));
}

#[test]
fn the_phone_takes_a_code_for_a_refusal_where_luned_s_own_reader_of_that_verb_does() {
    use luned::changes::Unchanged;
    use luned::issues::Unfiled;
    use luned::rebases::Unrebased;
    use tonic::{Code, Status};
    let refused = |code, refusals| {
        let read = Unanswered::of(&Status::new(code, "w"), refusals);
        matches!(read, Unanswered::Refused { .. })
    };
    for code in (0..=16).map(Code::from) {
        let changed = matches!(
            Unchanged::of(&Status::new(code, "w")),
            Unchanged::Refused(_)
        );
        assert_eq!(
            refused(code, Refusals::Changes),
            changed,
            "a change: {code:?}"
        );
        let filed = matches!(Unfiled::of("w".to_owned(), Some(code)), Unfiled::Refused(_));
        assert_eq!(
            refused(code, Refusals::Filings),
            filed,
            "an issue filed: {code:?}"
        );
        let rebased = matches!(
            Unrebased::of("w".to_owned(), Some(code)),
            Unrebased::Refused(_)
        );
        assert_eq!(
            refused(code, Refusals::Others),
            rebased,
            "a rebase: {code:?}"
        );
    }
}

#[test]
fn the_phone_waits_for_each_verb_as_long_as_luned_may_work_at_it() {
    assert!(
        waits::ANSWER > luned::WATCH,
        "a watch parks for less than the phone waits"
    );
    assert!(waits::TURN >= luned::asks::TURN);
    assert!(waits::MAKING >= luned::links::WORKTREE_PATIENCE);
    assert!(waits::LISTING >= luned::work::ANSWERED_MAX);
    assert!(waits::PLANNING >= luned::prunes::PLANNING);
    assert!(waits::DELETING >= luned::prunes::PLANNING + luned::prunes::DELETING);
    assert!(waits::REBASING >= luned::rebases::REBASING);
    assert!(waits::FILING >= luned::issues::AT_MOST);
}

#[test]
fn a_call_that_gets_through_wakes_a_backed_off_watch() {
    const DRAIN_MAX: usize = 64;
    let mut owned = Owned::new();
    claude_that_takes_turns(&owned.host);
    let worktree = crate::layouts::worktree_in_home(&owned);
    let mut shown = crate::layouts::Shown::new(&owned, worktree, 80, 24);
    shown.open_running(&["sleep", "600"]);
    let phone = Phone::backing_off(&owned.host);
    let model = phone.sees("the view runs", |model| {
        started_view(model, worktree).is_some()
    });
    let project = projects_here(&model)[0].id;
    let Some(View {
        found: Found::Key { key },
        ..
    }) = started_view(&model, worktree)
    else {
        panic!("the view has its terminal");
    };
    let back = |phone: &Phone| {
        phone.sees(
            "the watch brings the model back without waiting for its next retry",
            |model| started_view(model, worktree).is_some(),
        )
    };

    phone.block_on(phone.core.waiting(key.clone())).unwrap();
    owned.stop_daemon();
    phone.loses("luned ended");
    let down = phone.block_on(phone.core.waiting(key.clone()));
    assert!(
        matches!(down, Err(Unanswered::InDoubt { .. })),
        "down: {down:?}"
    );
    owned.start_daemon();
    never(
        "a healthy call before the loss and a failed call while down do not wake the watch",
        || match phone.heard.try_recv() {
            Ok(heard) => Some(heard),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => panic!("watch ended while backed off"),
        },
    );

    phone.block_on(phone.core.waiting(key.clone())).unwrap();
    back(&phone);

    owned.stop_daemon();
    phone.loses("luned ended before work");
    owned.start_daemon();
    assert_eq!(
        phone.block_on(phone.core.work(None, project)).unwrap(),
        Listed::Unhosted
    );
    back(&phone);

    owned.stop_daemon();
    phone.loses("luned ended before refusal");
    owned.start_daemon();
    refused(phone.block_on(phone.core.issue_repositories(None, "relative".to_owned())));
    back(&phone);

    owned.stop_daemon();
    phone.loses("luned ended before ask");
    owned.start_daemon();
    let asking = phone
        .core
        .clone()
        .converse(None, None)
        .ask("hi".to_owned())
        .unwrap();
    phone.steps(&asking).unwrap();
    back(&phone);

    owned.stop_daemon();
    phone.loses("luned ended before refused ask");
    owned.start_daemon();
    let blank = phone
        .core
        .clone()
        .converse(None, None)
        .ask(" ".to_owned())
        .unwrap();
    assert!(refused(phone.steps(&blank)).contains("nothing asked"));
    back(&phone);

    owned.stop_daemon();
    phone.loses("luned ended before terminal");
    owned.start_daemon();
    let view = phone.core.terminal(&key, Appearance::Dark);
    shows(&phone, &view, "the terminal opens", |_| true);
    back(&phone);
    view.close();

    phone.core.stop();
    eventually("the stopped watch drops its observer", || {
        for _ in 0..DRAIN_MAX {
            match phone.heard.try_recv() {
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => return Some(()),
            }
        }
        panic!("stopped watch kept sending past {DRAIN_MAX} messages");
    });
    phone.block_on(phone.core.waiting(key)).unwrap();
    never("a call while stopped starts no watch", || {
        phone.heard.try_recv().ok()
    });
}

#[test]
fn the_phone_falls_back_to_watch_against_an_older_luned_and_upgrades_on_reconnect() {
    use std::num::NonZeroU32;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    use lune_protocol::watch::HostVersion;
    use luned::api::daemon_server::{Daemon, DaemonServer};
    use luned::api::{
        self, AnswerRequest, AskRequest, AskResponse, ChangeProjectsRequest,
        ChangeProjectsResponse, FileIssueRequest, FileIssueResponse, FramesRequest, InfoResponse,
        IssueRepositoriesRequest, IssueRepositoriesResponse, KeepChecksRequest, KeepChecksResponse,
        KeepImageRequest, KeepImageResponse, KeepPrintedRequest, KeepPrintedResponse,
        ListAgentsResponse, ListContentsRequest, ListContentsResponse, ListRepositoriesRequest,
        ListRepositoriesResponse, ListResumeRequest, ListResumeResponse, ListSkillsRequest,
        ListSkillsResponse, ListWorkRequest, ListWorkResponse, MakeWorktreeRequest,
        MakeWorktreeResponse, PasteRequest, PasteResponse, PruneRequest, PruneResponse,
        PrunesRequest, PrunesResponse, ReadDiffRequest, ReadDiffResponse, RebaseRequest,
        RebaseResponse, ReloadResumeRequest, ReloadResumeResponse, SayRequest, ShowRequest,
        ShowResponse, TranscriptEndsRequest, TranscriptEndsResponse, Version, WaitingRequest,
        WaitingResponse, WatchChangesRequest, WatchLocalChangesRequest, WatchRequest,
    };
    use tokio::net::UnixListener;
    use tokio::sync::{oneshot, watch};
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::Server;
    use tonic::{Request, Response, Status, Streaming};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Stage {
        LegacyFirst,
        LegacySecond,
        Restarting,
        Upgraded,
    }

    struct Upgrading {
        stage: watch::Sender<Stage>,
        watch_changes_calls: Arc<AtomicUsize>,
        watch_calls: Arc<AtomicUsize>,
        watch_calls_after_upgrade: Arc<AtomicUsize>,
        unchanged_legacy_sent: Arc<AtomicBool>,
        restart_sent: Arc<AtomicBool>,
        upgraded_long_poll: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    }

    fn host_named(pid: u32, seq: u64, name: &str) -> api::Host {
        let project = api::Project {
            id: 1,
            name: name.to_owned(),
            dir: "/w".to_owned(),
            holds: Some(api::project::Holds::Folder(api::Worktree {
                id: 1,
                name: "main".to_owned(),
                folder: "/w".to_owned(),
                seen: Some(api::worktree::Seen::Unread(())),
                ..api::Worktree::default()
            })),
            ..api::Project::default()
        };
        let kept = api::projects::Held::Kept(api::projects::Kept {
            projects: vec![project],
            version: None,
        });
        api::Host {
            version: Some(Version { pid, seq }),
            projects: Some(api::Projects { held: Some(kept) }),
            ..api::Host::default()
        }
    }

    #[tonic::async_trait]
    impl Daemon for Upgrading {
        async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
            Err(Status::permission_denied("not served"))
        }

        async fn list_agents(
            &self,
            _: Request<()>,
        ) -> Result<Response<ListAgentsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn watch(
            &self,
            request: Request<WatchRequest>,
        ) -> Result<Response<api::Host>, Status> {
            let request = request.into_inner();
            assert!(
                request.kept.is_none() && request.shown,
                "phone watches as a window: {request:?}"
            );
            self.watch_calls.fetch_add(1, Ordering::SeqCst);
            if *self.stage.borrow() == Stage::Upgraded {
                self.watch_calls_after_upgrade
                    .fetch_add(1, Ordering::SeqCst);
                return Err(Status::failed_precondition(
                    "legacy watch called after upgrade",
                ));
            }
            match request.after {
                None => Ok(Response::new(host_named(1, 1, "alpha"))),
                Some(Version { pid: 1, seq: 1 }) => {
                    if !self.unchanged_legacy_sent.swap(true, Ordering::SeqCst) {
                        return Ok(Response::new(host_named(1, 1, "alpha")));
                    }
                    let mut stage = self.stage.subscribe();
                    let _ = stage
                        .wait_for(|current| *current != Stage::LegacyFirst)
                        .await;
                    Ok(Response::new(host_named(1, 2, "beta")))
                }
                Some(Version { pid: 1, seq: 2 }) => {
                    let mut stage = self.stage.subscribe();
                    let _ = stage
                        .wait_for(|current| matches!(current, Stage::Restarting | Stage::Upgraded))
                        .await;
                    if !self.restart_sent.swap(true, Ordering::SeqCst) {
                        self.stage.send_replace(Stage::Upgraded);
                        return Err(Status::unavailable("restarting"));
                    }
                    self.watch_calls_after_upgrade
                        .fetch_add(1, Ordering::SeqCst);
                    Err(Status::unavailable("restarting"))
                }
                Some(other) => Err(Status::invalid_argument(format!(
                    "unexpected after {other:?}"
                ))),
            }
        }

        async fn watch_changes(
            &self,
            request: Request<WatchChangesRequest>,
        ) -> Result<Response<api::WatchResponse>, Status> {
            self.watch_changes_calls.fetch_add(1, Ordering::SeqCst);
            if *self.stage.borrow() != Stage::Upgraded {
                return Err(Status::unimplemented(""));
            }
            let version = HostVersion::new(NonZeroU32::new(2).unwrap(), 1);
            match request.into_inner().after {
                None => Ok(Response::new(api::WatchResponse::host(
                    version,
                    host_named(2, 1, "gamma"),
                ))),
                Some(after) if after == version.to_api() => {
                    if let Some(done) = self.upgraded_long_poll.lock().unwrap().take() {
                        let _ = done.send(());
                    }
                    Ok(Response::new(api::WatchResponse::unchanged(version)))
                }
                Some(other) => Err(Status::invalid_argument(format!(
                    "unexpected after {other:?}"
                ))),
            }
        }

        async fn watch_local_changes(
            &self,
            _: Request<WatchLocalChangesRequest>,
        ) -> Result<Response<api::WatchLocalResponse>, Status> {
            Err(Status::permission_denied("not served"))
        }

        type KeepChecksStream = tokio_stream::Empty<Result<KeepChecksResponse, Status>>;

        async fn keep_checks(
            &self,
            _: Request<KeepChecksRequest>,
        ) -> Result<Response<Self::KeepChecksStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type FileIssueStream = tokio_stream::Empty<Result<FileIssueResponse, Status>>;

        async fn file_issue(
            &self,
            _: Request<FileIssueRequest>,
        ) -> Result<Response<Self::FileIssueStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn issue_repositories(
            &self,
            _: Request<IssueRepositoriesRequest>,
        ) -> Result<Response<IssueRepositoriesResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn change_projects(
            &self,
            _: Request<ChangeProjectsRequest>,
        ) -> Result<Response<ChangeProjectsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type MakeWorktreeStream = tokio_stream::Empty<Result<MakeWorktreeResponse, Status>>;

        async fn make_worktree(
            &self,
            _: Request<MakeWorktreeRequest>,
        ) -> Result<Response<Self::MakeWorktreeStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type ListWorkStream = tokio_stream::Empty<Result<ListWorkResponse, Status>>;

        async fn list_work(
            &self,
            _: Request<ListWorkRequest>,
        ) -> Result<Response<Self::ListWorkStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type ReadDiffStream = tokio_stream::Empty<Result<ReadDiffResponse, Status>>;

        async fn read_diff(
            &self,
            _: Request<ReadDiffRequest>,
        ) -> Result<Response<Self::ReadDiffStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn list_skills(
            &self,
            _: Request<ListSkillsRequest>,
        ) -> Result<Response<ListSkillsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn list_repositories(
            &self,
            _: Request<ListRepositoriesRequest>,
        ) -> Result<Response<ListRepositoriesResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn keep_printed(
            &self,
            _: Request<KeepPrintedRequest>,
        ) -> Result<Response<KeepPrintedResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn list_contents(
            &self,
            _: Request<ListContentsRequest>,
        ) -> Result<Response<ListContentsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn transcript_ends(
            &self,
            _: Request<TranscriptEndsRequest>,
        ) -> Result<Response<TranscriptEndsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn waiting(
            &self,
            _: Request<WaitingRequest>,
        ) -> Result<Response<WaitingResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn answer(&self, _: Request<AnswerRequest>) -> Result<Response<()>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn start_flow(
            &self,
            _: Request<api::StartFlowRequest>,
        ) -> Result<Response<api::FlowStateResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn control_flow(
            &self,
            _: Request<api::ControlFlowRequest>,
        ) -> Result<Response<api::FlowStateResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn list_flows(
            &self,
            _: Request<api::ListFlowsRequest>,
        ) -> Result<Response<api::ListFlowsResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type AskStream = tokio_stream::Empty<Result<AskResponse, Status>>;

        async fn ask(&self, _: Request<AskRequest>) -> Result<Response<Self::AskStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type PasteStream = tokio_stream::Empty<Result<PasteResponse, Status>>;

        async fn paste(
            &self,
            _: Request<PasteRequest>,
        ) -> Result<Response<Self::PasteStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type PrunesStream = tokio_stream::Empty<Result<PrunesResponse, Status>>;

        async fn prunes(
            &self,
            _: Request<PrunesRequest>,
        ) -> Result<Response<Self::PrunesStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type PruneStream = tokio_stream::Empty<Result<PruneResponse, Status>>;

        async fn prune(
            &self,
            _: Request<PruneRequest>,
        ) -> Result<Response<Self::PruneStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type RebaseStream = tokio_stream::Empty<Result<RebaseResponse, Status>>;

        async fn rebase(
            &self,
            _: Request<RebaseRequest>,
        ) -> Result<Response<Self::RebaseStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type KeepImageStream = tokio_stream::Empty<Result<KeepImageResponse, Status>>;

        async fn keep_image(
            &self,
            _: Request<Streaming<KeepImageRequest>>,
        ) -> Result<Response<Self::KeepImageStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn say(&self, _: Request<SayRequest>) -> Result<Response<()>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type ShowStream = tokio_stream::Empty<Result<ShowResponse, Status>>;

        async fn show(
            &self,
            _: Request<Streaming<ShowRequest>>,
        ) -> Result<Response<Self::ShowStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        type FramesStream = tokio_stream::Empty<Result<luned::mux::own::api::ToViewer, Status>>;

        async fn frames(
            &self,
            _: Request<Streaming<FramesRequest>>,
        ) -> Result<Response<Self::FramesStream>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn list_resume(
            &self,
            _: Request<ListResumeRequest>,
        ) -> Result<Response<ListResumeResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }

        async fn reload_resume(
            &self,
            _: Request<ReloadResumeRequest>,
        ) -> Result<Response<ReloadResumeResponse>, Status> {
            Err(Status::unimplemented("unused"))
        }
    }

    let scratch = tempfile::tempdir().unwrap();
    let socket = scratch.path().join("legacy.sock");
    let listener = StdUnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();

    let (stage, _) = watch::channel(Stage::LegacyFirst);
    let watch_changes_calls = Arc::new(AtomicUsize::new(0));
    let watch_calls = Arc::new(AtomicUsize::new(0));
    let watch_calls_after_upgrade = Arc::new(AtomicUsize::new(0));
    let (upgraded_done_tx, upgraded_done_rx) = oneshot::channel();
    let service = Upgrading {
        stage: stage.clone(),
        watch_changes_calls: Arc::clone(&watch_changes_calls),
        watch_calls: Arc::clone(&watch_calls),
        watch_calls_after_upgrade: Arc::clone(&watch_calls_after_upgrade),
        unchanged_legacy_sent: Arc::new(AtomicBool::new(false)),
        restart_sent: Arc::new(AtomicBool::new(false)),
        upgraded_long_poll: Arc::new(Mutex::new(Some(upgraded_done_tx))),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let incoming = UnixListenerStream::new(UnixListener::from_std(listener).unwrap());
            let _ = Server::builder()
                .add_service(DaemonServer::new(service))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
    });

    let phone = Phone::watching(Core::over_socket(socket, TEST_PATIENCE, waits::again).unwrap());
    let patience = Patience::begun();

    let first = patience.heard(
        "the phone falls back to legacy Watch without reporting a loss",
        &phone.heard,
    );
    let Heard::Changed(first) = first else {
        panic!("expected initial model via Watch fallback, got {first:?}");
    };
    assert_eq!(names(&first), ["alpha"]);

    stage.send_replace(Stage::LegacySecond);
    let second = patience.heard(
        "the phone receives the next update via legacy Watch",
        &phone.heard,
    );
    let Heard::Changed(second) = second else {
        panic!("expected updated model via Watch without spurious loss, got {second:?}");
    };
    assert_eq!(names(&second), ["beta"]);
    assert_eq!(
        watch_changes_calls.load(Ordering::SeqCst),
        1,
        "legacy long-polling does not re-probe WatchChanges on every poll"
    );
    assert!(watch_calls.load(Ordering::SeqCst) >= 3);

    stage.send_replace(Stage::Restarting);
    let lost = patience.heard(
        "the phone reports loss when the server restarts",
        &phone.heard,
    );
    let Heard::Lost(why) = lost else {
        panic!("expected loss during restart, got {lost:?}");
    };
    assert_eq!(why, "restarting");

    let upgraded = patience.heard(
        "the phone re-probes WatchChanges after reconnect",
        &phone.heard,
    );
    let Heard::Changed(upgraded) = upgraded else {
        panic!("expected model via WatchChanges after reconnect, got {upgraded:?}");
    };
    assert_eq!(names(&upgraded), ["gamma"]);

    phone
        .block_on(patience.awaited(
            "the phone long-polls WatchChanges after upgrading",
            upgraded_done_rx,
        ))
        .unwrap();
    assert!(watch_changes_calls.load(Ordering::SeqCst) >= 3);
    assert_eq!(watch_calls_after_upgrade.load(Ordering::SeqCst), 0);

    drop(phone);
    let _ = shutdown_tx.send(());
    server.join().unwrap();
}
