use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lunetad::api::say::Does;
use lunetad::api::{self, OwnSpeaker, RenameSessionRequest, UnreadFile};
use lunetad::mux::own::api as mux_api;
use lunetad::opens;

use crate::host::{answered, eventually, Running, LUNETA};
use crate::own::{own, Owned};
use crate::sessions::session;

const PLANNED: i32 = 3;

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

fn luneta(owned: &Owned, vars: &[(&str, String)], args: &[&str]) -> Output {
    let mut command = owned.host.vars().command(LUNETA);
    let vars = vars.iter().map(|(name, value)| (*name, value.as_str()));
    answered(command.envs(vars).args(args))
}

fn said(ran: &Output) -> String {
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    String::from_utf8(ran.stdout.clone()).unwrap()
}

fn why(ran: &Output) -> String {
    assert!(!ran.status.success(), "{}", String::from_utf8_lossy(&ran.stdout));
    String::from_utf8(ran.stderr.clone()).unwrap()
}

fn panes(owned: &Owned, name: &str, count: usize) -> Vec<u32> {
    eventually(&format!("{name} has {count} panes"), || {
        let panes = session(&owned.host.host(), name)?.panes.clone()?.panes;
        (panes.len() == count).then(|| panes.iter().map(|pane| pane.id).collect())
    })
}

fn clients_on(owned: &Owned) -> Vec<(u64, u32)> {
    let listing = owned.listing();
    let tabs = listing.sessions.iter().flat_map(|session| &session.tabs);
    let panes = tabs.flat_map(|tab| &tab.panes);
    panes.flat_map(|pane| pane.clients.iter().map(|client| (client.id, pane.id))).collect()
}

#[test]
fn an_own_pane_is_here_by_its_ids_through_a_rename() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let work = own(&owned.listed("work")).unwrap();
    let here = in_pane(work, made.pane);
    assert_eq!(said(&luneta(&owned, &here, &["here"])), format!("work:{}\n", made.pane));
    let turn = ("LUNETA_TURN", "00000000000000aa".to_owned());
    let made_nanos = &here[1].1;
    let asked = format!("{}:{made_nanos}:{}:1:named-then", work.id, made.pane);
    let older_box = [("LUNETA_OWN_PANE", asked.clone()), turn.clone()];
    let older = why(&luneta(&owned, &older_box, &["here"]));
    assert!(older.contains("relaunch luneta"), "{older}");
    let from_the_box = [("LUNETA_OWN_SEAT", asked), turn];
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

fn session_file(owned: &Owned, pid: u32) -> PathBuf {
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    sessions.join(format!("{pid}.json"))
}

#[test]
fn a_close_in_an_own_session_ends_the_agent_of_a_file_that_does_not_read() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let work = own(&owned.listed("work")).unwrap();
    let here = in_pane(work, made.pane);
    said(&luneta(&owned, &here, &["open", "tab", "--", "sleep", "600"]));
    let second = panes(&owned, "work", 2)[1];
    let mut agent = Command::new("sleep");
    agent.arg("600").env_remove("ZELLIJ_SESSION_NAME").env_remove("ZELLIJ_PANE_ID");
    let agent = Running::spawn(agent.envs(in_pane(work, second)));
    fs::write(session_file(&owned, agent.pid()), "{\"pid\":").unwrap();
    let in_own = api::Own { session: Some(work), pane: second };
    let file = UnreadFile { pid: agent.pid(), zellij: None, own: Some(in_own), terminal: None };
    eventually("the file does not read, in its own pane", || {
        (owned.host.host().unread_files == [file.clone()]).then_some(())
    });

    let unread = format!("work:{second}");
    let first = format!("work:{}", made.pane);
    for (asked, ends) in [(unread.as_str(), true), ("work", true), (first.as_str(), false)] {
        let plan = luneta(&owned, &here, &["close", asked]);
        let stderr = String::from_utf8_lossy(&plan.stderr);
        assert_eq!(plan.status.code(), Some(PLANNED), "{stderr}");
        let plan = String::from_utf8(plan.stdout).unwrap();
        let line = |line: &str| {
            line.trim_start().starts_with(&format!("{unread} "))
                && line.ends_with("its session file does not read")
        };
        assert_eq!(plan.lines().any(line), ends, "{asked}: {plan}");
        assert_eq!(plan.contains("1 unknown"), ends, "{asked}: {plan}");
    }
    assert_eq!(panes(&owned, "work", 2)[1], second);
}

#[test]
fn what_is_said_from_an_own_pane_names_it_and_its_session_now() {
    let owned = Owned::new();
    let made = owned.make("work").unwrap();
    let work = own(&owned.listed("work")).unwrap();
    let here = in_pane(work, made.pane);
    let made_nanos = &here[1].1;
    let asked = format!("{}:{made_nanos}:{}:1:work", work.id, made.pane);
    let from_the_box = [("LUNETA_OWN_SEAT", asked), ("LUNETA_TURN", "00000000000000aa".to_owned())];
    let speakers = || {
        let sayings = owned.host.host().sayings;
        let does = sayings.into_iter().filter_map(|saying| saying.say?.does);
        let spoken = does.filter_map(|does| match does {
            Does::InTurn(utterance) => Some((utterance.text, utterance.own_speaker)),
            Does::Now(_) | Does::Stop(()) => None,
        });
        spoken.collect::<Vec<_>>()
    };
    let from = |session: &str| {
        let pane = Some(api::Own { session: Some(work), pane: made.pane });
        Some(OwnSpeaker { pane, session: Some(session.to_owned()) })
    };

    said(&luneta(&owned, &here, &["say", "from the pane"]));
    said(&luneta(&owned, &from_the_box, &["say", "from the box"]));
    let request =
        RenameSessionRequest { on: None, session: "work".to_owned(), to: "play".to_owned() };
    owned.ask(|mut daemon| async move { daemon.rename_session(request).await }).unwrap();
    owned.listed("play");
    said(&luneta(&owned, &here, &["say", "renamed"]));
    assert_eq!(
        speakers(),
        [
            ("from the pane".to_owned(), from("work")),
            ("from the box".to_owned(), from("work")),
            ("renamed".to_owned(), from("play")),
        ],
    );
}

#[test]
fn go_takes_a_client_among_own_sessions_and_not_across_to_zellij_s() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let play = owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    let in_play = in_pane(own(&owned.listed("play")).unwrap(), play.pane);
    let _client = owned.attach(&work);
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

#[test]
fn go_from_the_ask_box_takes_luneta_s_client_whoever_typed_last() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let play = owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    owned.listed("play");
    let attached = owned.attach(&work);
    let paste = mux_api::to_server::Event::Paste(mux_api::Paste { text: "typed".to_owned() });
    let typist = owned.attach_saying(&work, vec![mux_api::ToServer { event: Some(paste) }]);
    let (luneta_s, other) = (attached.client, typist.client);
    let made = &in_work[1].1;
    let asked = format!("{}:{made}:{}:{luneta_s}:work", in_work[0].1, work.pane);
    let from_the_box = [("LUNETA_OWN_SEAT", asked), ("LUNETA_TURN", "00000000000000aa".to_owned())];

    said(&luneta(&owned, &from_the_box, &["go", "play"]));
    let mut on = clients_on(&owned);
    on.sort_unstable();
    assert_eq!(on, [(luneta_s, play.pane), (other, work.pane)]);

    drop(attached);
    eventually("luneta's client is gone", || (clients_on(&owned).len() == 1).then_some(()));
    let gone = why(&luneta(&owned, &from_the_box, &["go", "play"]));
    assert!(gone.contains(&format!("no client {luneta_s}")), "{gone}");
    assert_eq!(clients_on(&owned), [(other, work.pane)]);
}

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

#[test]
fn an_own_pane_s_session_is_its_own_where_zellij_runs_one_by_its_name() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let play = owned.make("play").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    owned.listed("play");
    let _client = owned.attach(&work);
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

#[test]
fn a_session_opened_from_an_own_pane_is_an_own_one_the_client_goes_to() {
    let owned = Owned::new();
    let work = owned.make("work").unwrap();
    let in_work = in_pane(own(&owned.listed("work")).unwrap(), work.pane);
    let _client = owned.attach(&work);
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
