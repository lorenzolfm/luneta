use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;

use luneta_mux::api;
use rustix::process::{Pid, test_kill_process};

use crate::host::{
    LUNETA_MUX, Server, listed, new_terminal, run, script, shell, size, watched_end,
};

fn programs(listing: &api::Listing) -> Vec<Pid> {
    let pid = |terminal: &api::Terminal| match &terminal.program {
        Some(api::terminal::Program::Runs(runs)) => {
            Pid::from_raw(i32::try_from(runs.pid).unwrap()).unwrap()
        },
        program => panic!("terminal {:?} runs nothing: {program:?}", terminal.key),
    };
    listing.terminals.iter().map(pid).collect()
}

async fn ended(pid: Pid) {
    let what = format!("program {pid:?} ends");
    luneta_waits::eventually_async(&what, async || test_kill_process(pid).is_err().then_some(()))
        .await;
}

#[test]
fn it_listens_only_in_the_one_absolute_folder_it_is_given() {
    let refused: [(&[&str], &str); 3] = [
        (&[], "give it the one folder to listen in"),
        (&["/tmp/a", "/tmp/b"], "give it the one folder to listen in"),
        (&["luneta"], "luneta is no folder to listen in, as it is not absolute"),
    ];
    for (arguments, why) in refused {
        let output = Command::new(LUNETA_MUX).args(arguments).output().unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains(why), "{arguments:?}: {said}");
    }
}

#[test]
fn a_server_whose_folder_goes_ends_and_so_do_its_terminals_programs() {
    run(async {
        let mut server = Server::start().await;
        let mut mux = server.client().await;
        let held = script("exec sleep 600", api::Ends::Holds);
        new_terminal(&mut mux, held, b"held", size(20, 4)).await;
        new_terminal(&mut mux, shell(), b"shell", size(20, 4)).await;
        let programs = programs(&listed(&mut mux).await);
        assert_eq!(programs.len(), 2);
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        fs::remove_dir_all(server.socket().parent().unwrap()).unwrap();
        let socket = server.socket();
        let why = format!("{} is gone, so nothing reaches it again", socket.display());
        let unreached = api::watched::ended::Why::Unreached(api::watched::Unreached { why });
        assert_eq!(watched_end(&mut watch).await, unreached);
        drop((watch, mux));
        assert!(server.ended().await.success());
        for program in programs {
            ended(program).await;
        }
    });
}

#[test]
fn a_server_whose_socket_another_takes_ends_and_leaves_that_socket_be() {
    run(async {
        let mut server = Server::start().await;
        let mut mux = server.client().await;
        new_terminal(&mut mux, shell(), b"shell", size(20, 4)).await;
        drop(mux);
        let socket = server.socket();
        let taking = socket.with_file_name("taking.sock");
        let theirs = UnixListener::bind(&taking).unwrap();
        fs::rename(&taking, &socket).unwrap();
        assert!(server.ended().await.success());
        UnixStream::connect(&socket).expect("the socket that took the server's place stays");
        drop(theirs);
    });
}
