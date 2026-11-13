//! What the CLI has luneta's own multiplexer do, for an agent that drives
//! luneta from an own pane, or from luneta's ask box while you are in one: it
//! knows the pane by its ids, which a rename keeps, opens a tab with no
//! client there, refuses a pane beside another and a turn, which an own
//! session's tabs have none of yet, takes a client among own sessions and
//! not across to zellij's, closes as it closes in zellij's, and takes no
//! session by a name both multiplexers run one by.

use std::path::Path;
use std::process::Output;

use lunetad::api::{self, RenameSessionRequest};
use lunetad::opens;

use crate::host::{answered, eventually, LUNETA};
use crate::own::{own, Owned};
use crate::sessions::session;

/// What a close exits with where it closes nothing, as it was not told yes to
/// the plan it says.
const PLANNED: i32 = 3;

/// The variables luneta's own multiplexer gives what runs in the pane `pane`
/// of the own session `session`.
fn in_pane(session: api::OwnSession, pane: u32) -> Vec<(&'static str, String)> {
    let made = session.made_at.unwrap();
    let seconds = u128::try_from(made.seconds).unwrap();
    let nanos = seconds * 1_000_000_000 + u128::try_from(made.nanos).unwrap();
    vec![
        ("LUNETA_MUX_SESSION", session.id.to_string()),
        ("LUNETA_MUX_MADE", nanos.to_string()),
        ("LUNETA_MUX_PANE", pane.to_string()),
    ]
}

/// The CLI, run with `args` and `vars`, as an agent runs it.
fn luneta(owned: &Owned, vars: &[(&str, String)], args: &[&str]) -> Output {
    let mut command = owned.host.vars().command(LUNETA);
    let vars = vars.iter().map(|(name, value)| (*name, value.as_str()));
    answered(command.envs(vars).args(args))
}

/// What it said, once it did what it was asked.
fn said(ran: &Output) -> String {
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    String::from_utf8(ran.stdout.clone()).unwrap()
}

/// Why it did not, once it failed.
fn why(ran: &Output) -> String {
    assert!(!ran.status.success(), "{}", String::from_utf8_lossy(&ran.stdout));
    String::from_utf8(ran.stderr.clone()).unwrap()
}

/// The panes of the own session `name`, by their ids, as the host has them,
/// once it has `count` of them.
fn panes(owned: &Owned, name: &str, count: usize) -> Vec<u32> {
    eventually(&format!("{name} has {count} panes"), || {
        let panes = session(&owned.host.host(), name)?.panes.clone()?.panes;
        (panes.len() == count).then(|| panes.iter().map(|pane| pane.id).collect())
    })
}

/// The pane each client of the server is on, by the client's id.
fn clients_on(owned: &Owned) -> Vec<(u64, u32)> {
    let listing = owned.listing();
    let tabs = listing.sessions.iter().flat_map(|session| &session.tabs);
    let panes = tabs.flat_map(|tab| &tab.panes);
    panes.flat_map(|pane| pane.clients.iter().map(|client| (client.id, pane.id))).collect()
}

/// A command in an own pane is in it by the ids its server gave the pane and
/// its session: `here` names it by the session's name now, through a rename,
/// and says so once the session is gone. The ask box's agent goes by the own
/// pane the box names in the same way.
#[test]
fn an_own_pane_is_here_by_its_ids_through_a_rename() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let work = own(&owned.listed("work")).unwrap();
    let here = in_pane(work, made.pane);
    assert_eq!(said(&luneta(&owned, &here, &["here"])), format!("work:{}\n", made.pane));
    let turn = ("LUNETA_TURN", "00000000000000aa".to_owned());
    let made_nanos = &here[1].1;
    let asked = format!("{}:{made_nanos}:{}:named-then", work.id, made.pane);
    let from_the_box = [("LUNETA_OWN_PANE", asked), turn];
    let box_says = said(&luneta(&owned, &from_the_box, &["here"]));
    assert_eq!(box_says, format!("work:{}\n", made.pane));

    let request =
        RenameSessionRequest { on: None, session: "work".to_owned(), to: "play".to_owned() };
    owned.ask(|mut daemon| async move { daemon.rename_session(request).await }).unwrap();
    owned.listed("play");
    assert_eq!(said(&luneta(&owned, &here, &["here"])), format!("play:{}\n", made.pane));
    let box_says = said(&luneta(&owned, &from_the_box, &["here"]));
    assert_eq!(box_says, format!("play:{}\n", made.pane));

    let other = own(&owned.listed("play")).map(|play| api::OwnSession { id: play.id + 1, ..play });
    let gone = why(&luneta(&owned, &in_pane(other.unwrap(), made.pane), &["here"]));
    assert!(gone.contains("which no longer runs"), "{gone}");
}

/// A tab opens in an own session with no client in it, and says its pane. A
/// pane does not open beside another there, nor does one turn, as an own
/// session's tabs hold a pane each: both say so, and change nothing.
#[test]
fn a_tab_opens_in_an_own_session_and_no_pane_beside_another_nor_a_turn() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let here = in_pane(own(&owned.listed("work")).unwrap(), made.pane);

    let opened = said(&luneta(&owned, &here, &["open", "tab", "--", "sleep", "600"]));
    let tabs = panes(&owned, "work", 2);
    assert_eq!(opened, format!("work:{}\n", tabs[1]));
    let named = ["open", "tab", "--session", "work", "--name", "second", "--", "sleep", "600"];
    let opened = said(&luneta(&owned, &[], &named));
    let tabs = panes(&owned, "work", 3);
    assert_eq!(opened, format!("work:{}\n", tabs[2]));

    let beside = why(&luneta(&owned, &here, &["open", "pane", "--", "sleep", "600"]));
    assert!(beside.contains("whose tabs hold a pane each"), "{beside}");
    assert!(beside.contains("luneta open tab --session work"), "{beside}");
    let pane = format!("work:{}", made.pane);
    let turned = why(&luneta(&owned, &[], &["turn", "pane", &pane]));
    assert!(turned.contains("none turns"), "{turned}");
    let into = format!("work:{}", tabs[1]);
    let turned = why(&luneta(&owned, &[], &["turn", "tab", &pane, "--into", &into]));
    assert!(turned.contains("none turns"), "{turned}");
    panes(&owned, "work", 3);
}

/// A close of an own pane closes its tab, and of an own session, the session,
/// once told yes to the plan it says: by the session's ids, and what holds
/// the pane it runs in last.
#[test]
fn a_close_in_an_own_session_closes_the_tab_of_a_pane_and_the_session() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let here = in_pane(own(&owned.listed("work")).unwrap(), made.pane);
    said(&luneta(&owned, &here, &["open", "tab", "--", "sleep", "600"]));
    let second = panes(&owned, "work", 2)[1];

    let pane = format!("work:{second}");
    let asked = luneta(&owned, &here, &["close", &pane]);
    assert_eq!(asked.status.code(), Some(PLANNED), "{}", String::from_utf8_lossy(&asked.stderr));
    let plan = String::from_utf8(asked.stdout).unwrap();
    assert!(plan.starts_with(&format!("close the tab of {pane}: ")), "{plan}");
    let stderr = String::from_utf8(asked.stderr).unwrap();
    let agreed = stderr.split_once("--yes ").unwrap().1.split_whitespace().next().unwrap();
    said(&luneta(&owned, &here, &["close", &pane, "--yes", agreed]));
    panes(&owned, "work", 1);

    let asked = luneta(&owned, &here, &["close", "work"]);
    assert_eq!(asked.status.code(), Some(PLANNED), "{}", String::from_utf8_lossy(&asked.stderr));
    assert!(String::from_utf8_lossy(&asked.stdout).starts_with("delete session work: "));
    let stderr = String::from_utf8(asked.stderr).unwrap();
    let agreed = stderr.split_once("--yes ").unwrap().1.split_whitespace().next().unwrap();
    said(&luneta(&owned, &here, &["close", "work", "--yes", agreed]));
    eventually("work is gone", || owned.host.host().sessions.is_empty().then_some(()));
}

/// `go` takes the client that last came to the own session it runs in to
/// another own session, or to a pane of one, and back. It takes none across
/// to zellij's, and says why, as luneta's own client and zellij's are two.
#[test]
fn go_takes_a_client_among_own_sessions_and_not_across_to_zellij_s() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let play = owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    let in_play = in_pane(own(&owned.listed("play")).unwrap(), play.pane);
    let _client = owned.attach(work.session);
    let [(client, on)] = clients_on(&owned)[..] else {
        panic!("one client: {:?}", clients_on(&owned));
    };
    assert_eq!(on, work.pane);

    said(&luneta(&owned, &in_work, &["go", "play"]));
    assert_eq!(clients_on(&owned), [(client, play.pane)]);
    said(&luneta(&owned, &in_play, &["go", &format!("work:{}", work.pane)]));
    assert_eq!(clients_on(&owned), [(client, work.pane)]);
    let unmoved = why(&luneta(&owned, &in_play, &["go", "work"]));
    assert!(unmoved.contains("no client is on session"), "{unmoved}");

    let _zellij = owned.host.serve("zellij-s");
    eventually("zellij-s runs", || session(&owned.host.host(), "zellij-s").map(drop));
    let across = why(&luneta(&owned, &in_work, &["go", "zellij-s"]));
    assert!(across.contains("luneta's own client and zellij's are two"), "{across}");
    assert_eq!(clients_on(&owned), [(client, work.pane)]);
}

/// A name a zellij session and an own one share, as each multiplexer checks
/// only its own, names neither to the CLI: a close of it closes nothing, nor
/// does a tab open in it, and the own session runs on.
#[test]
fn a_name_both_multiplexers_run_a_session_by_is_neither_s() {
    let owned = Owned::new();
    owned.make("work").unwrap();
    owned.listed("work");
    let _zellij = owned.host.serve("work");
    eventually("both are on the host", || {
        let host = owned.host.host();
        let named = host.sessions.iter().filter(|session| session.name == "work").count();
        (named == 2).then_some(())
    });
    for args in [&["close", "work"][..], &["open", "tab", "--session", "work"]] {
        let refused = why(&luneta(&owned, &[], args));
        assert!(refused.contains("two sessions are named work"), "{args:?}: {refused}");
    }
    assert_eq!(owned.listing().sessions.len(), 1);
}

/// A command in an own pane goes by its session's ids, whatever zellij runs
/// by the session's name: `here` names its pane, a tab opens in it, a pane
/// beside another does not, as there alone, and `go` takes its client to
/// another own session. The name, typed, is still neither's.
#[test]
fn an_own_pane_s_session_is_its_own_where_zellij_runs_one_by_its_name() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let play = owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    owned.listed("play");
    let _client = owned.attach(work.session);
    let _zellij = owned.host.serve("work");
    eventually("both are on the host", || {
        let host = owned.host.host();
        let named = host.sessions.iter().filter(|session| session.name == "work").count();
        (named == 2).then_some(())
    });
    let tabs = |owned: &Owned| {
        let listing = owned.listing();
        listing.sessions.iter().find(|session| session.name == "work").unwrap().tabs.len()
    };

    assert_eq!(said(&luneta(&owned, &in_work, &["here"])), format!("work:{}\n", work.pane));
    let opened = said(&luneta(&owned, &in_work, &["open", "tab", "--", "sleep", "600"]));
    assert!(opened.starts_with("work:"), "{opened}");
    assert_eq!(tabs(&owned), 2);
    let beside = why(&luneta(&owned, &in_work, &["open", "pane", "--", "sleep", "600"]));
    assert!(beside.contains("whose tabs hold a pane each"), "{beside}");
    said(&luneta(&owned, &in_work, &["go", "play"]));
    let [(_, on)] = clients_on(&owned)[..] else {
        panic!("one client: {:?}", clients_on(&owned));
    };
    assert_eq!(on, play.pane);

    let typed = why(&luneta(&owned, &in_work, &["open", "tab", "--session", "work"]));
    assert!(typed.contains("two sessions are named work"), "{typed}");
    assert_eq!(tabs(&owned), 2);
}

/// `open session` in an own pane names its session as none of either
/// multiplexer has it: a name either has is taken.
#[test]
fn a_session_opened_from_an_own_pane_takes_no_name_either_multiplexer_has() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    let _zellij = owned.host.serve("zellij-s");
    eventually("zellij-s runs", || session(&owned.host.host(), "zellij-s").map(drop));
    for taken in ["play", "zellij-s"] {
        let refused = why(&luneta(&owned, &in_work, &["open", "session", "--name", taken]));
        assert!(refused.contains(&format!("{taken} runs already")), "{refused}");
    }
}

/// `open session` in an own pane makes an own session, with your shell, and
/// takes the client that came last to the session it runs in there. In the
/// build's sandbox, the user database names a shell that is not there, and
/// the server makes no session of it.
#[test]
fn a_session_opened_from_an_own_pane_is_an_own_one_the_client_goes_to() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    let _client = owned.attach(work.session);
    let [(client, _)] = clients_on(&owned)[..] else {
        panic!("one client: {:?}", clients_on(&owned));
    };

    let home = owned.home();
    let ran = luneta(&owned, &in_work, &["open", "session", "--name", "new", "--cwd", &home]);
    if !Path::new(&opens::your_shell()).is_file() {
        why(&ran);
        return;
    }
    assert_eq!(said(&ran), "new\n");
    let new = owned.listed("new");
    assert!(own(&new).is_some(), "{new:?}");
    let pane = new.panes.unwrap().panes[0].id;
    assert_eq!(clients_on(&owned), [(client, pane)]);
}
