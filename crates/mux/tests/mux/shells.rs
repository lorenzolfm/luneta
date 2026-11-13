//! What runs in front of a pane's terminal, and what its shell says it ran
//! by its marks (OSC 133), as the server lists them. `/bin/sh` writes the
//! marks here as fish writes them of itself.

use luneta_mux::api;
use luneta_mux::api::ran::Is;

use crate::host::{Attached, Server, new_session, run, script, session, size, tabs, watched_until};

/// What writes the mark `$1`, as a shell does.
const MARK: &str = "m() { printf '\\033]133;%s\\007' \"$1\"; }";

/// The first pane of the first session of `listing`.
fn pane(listing: &api::Listing) -> Option<&api::Pane> {
    tabs(listing).first().and_then(|tab| tab.panes.first())
}

/// What the shell of the first pane said.
fn said(listing: &api::Listing) -> Option<&Is> {
    pane(listing)?.ran.as_ref()?.is.as_ref()
}

/// Whether the first pane has `program` in front, by its first argument.
fn in_front(listing: &api::Listing, program: &str) -> bool {
    pane(listing)
        .and_then(|pane| pane.foreground.as_ref())
        .is_some_and(|front| front.argv.first().is_some_and(|first| first == program))
}

/// What runs `script` in the `bash` on the `PATH`, in `/`, its tab held as
/// it ends: macOS's `/bin/sh`, a bash 3.2, never comes back from the `fg` of
/// a job that stops.
fn bash(script: &str) -> api::Opens {
    let argv = ["bash", "-c", script].map(str::to_owned).to_vec();
    api::Opens {
        cwd: "/".to_owned(),
        runs: Some(api::opens::Runs::Program(api::Program { argv })),
        ends: api::Ends::Holds.into(),
    }
}

/// Sends `signal` to the job in front of the first pane, which leads its
/// group.
fn signal_front(listing: &api::Listing, signal: rustix::process::Signal) {
    let front = pane(listing).and_then(|pane| pane.foreground.as_ref()).unwrap().pid;
    let front = rustix::process::Pid::from_raw(front.cast_signed()).unwrap();
    rustix::process::kill_process_group(front, signal).unwrap();
}

/// A shell's marks say it is ready, then what it runs and since when, and
/// how that ended and how long it took. The shell in front, reading a line
/// of the terminal with the echo off, as a password prompt does, reads as
/// asking; and what runs in front is the pane's program.
#[test]
fn a_shell_s_marks_say_what_it_runs_and_how_that_ended() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let body = "m A; m 'C;cmdline_url=make%20test'; stty -echo; read x; stty echo; m 'D;3'\n\
            m A; read y";
        let opens = script(&format!("{MARK}\n{body}"), api::Ends::Holds);
        let made = new_session(&mut mux, "marks", opens).await;
        let attached = Attached::new(&mut mux, session(made.session), size(40, 10)).await;
        let listing = watched_until(&mut watch, "make test going, asking", |listing| {
            matches!(said(listing), Some(Is::Going(going)) if going.front() == api::Front::Asking)
        })
        .await;
        let Some(Is::Going(going)) = said(&listing) else { unreachable!() };
        assert_eq!(going.command.as_deref(), Some("make test"));
        let pane = pane(&listing).unwrap();
        let Some(api::pane::Program::Pid(pid)) = pane.program else { panic!("{pane:?}") };
        let front = pane.foreground.as_ref().expect("something runs in front");
        assert_eq!(front.pid, pid);
        assert_eq!(front.argv[..2], ["/bin/sh", "-c"]);
        attached.type_line("").await;
        let listing = watched_until(&mut watch, "make test ended", |listing| {
            matches!(said(listing), Some(Is::Ended(_)))
        })
        .await;
        let Some(Is::Ended(ended)) = said(&listing) else { unreachable!() };
        assert_eq!(ended.command.as_deref(), Some("make test"));
        assert_eq!(ended.ending, Some(api::ran::ended::Ending::Status(3)));
        let since = going.since.unwrap();
        let at = ended.at.unwrap();
        assert!((at.seconds, at.nanos) >= (since.seconds, since.nanos), "{since:?} after {at:?}");
        assert!(ended.took.is_some());
    });
}

/// A job in front is what runs in front. Stopped, as `Ctrl z` stops it, as
/// its shell says it ended, it reads as stopped; `fg` brings it back as what runs, and
/// its end says it was resumed, whatever status `fg` gave. Once the program
/// ends, nothing runs in front and its shell says nothing.
#[test]
fn a_job_stopped_and_brought_back_reads_as_its_command() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let body = "set -m\n\
            m A; m 'C;cmdline_url=sleep%2030'; sleep 30 & fg >/dev/null; m 'D;0'\n\
            m A; read x; m 'C;cmdline_url=fg'; fg >/dev/null; m 'D;0'\n\
            m A; read x";
        let made = new_session(&mut mux, "jobs", bash(&format!("{MARK}\n{body}"))).await;
        let attached = Attached::new(&mut mux, session(made.session), size(40, 10)).await;
        let listing = watched_until(&mut watch, "sleep in front", |listing| {
            in_front(listing, "sleep") && matches!(said(listing), Some(Is::Going(_)))
        })
        .await;
        signal_front(&listing, rustix::process::Signal::STOP);
        let listing = watched_until(&mut watch, "sleep stopped", |listing| {
            matches!(said(listing), Some(Is::Stopped(_)))
        })
        .await;
        let Some(Is::Stopped(stopped)) = said(&listing) else { unreachable!() };
        assert_eq!(stopped.command.as_deref(), Some("sleep 30"));
        attached.type_line("").await;
        let listing = watched_until(&mut watch, "sleep brought back", |listing| {
            in_front(listing, "sleep") && matches!(said(listing), Some(Is::Going(_)))
        })
        .await;
        let Some(Is::Going(going)) = said(&listing) else { unreachable!() };
        assert_eq!(going.command.as_deref(), Some("sleep 30"));
        signal_front(&listing, rustix::process::Signal::TERM);
        let listing = watched_until(&mut watch, "sleep ended", |listing| {
            matches!(said(listing), Some(Is::Ended(_))) && in_front(listing, "bash")
        })
        .await;
        let Some(Is::Ended(ended)) = said(&listing) else { unreachable!() };
        assert_eq!(ended.command.as_deref(), Some("sleep 30"));
        assert_eq!(ended.ending, Some(api::ran::ended::Ending::Resumed(api::ran::Resumed {})));
        attached.type_line("").await;
        watched_until(&mut watch, "nothing said once the program ended", |listing| {
            pane(listing).is_some_and(|pane| {
                matches!(pane.program, Some(api::pane::Program::Ended(_)))
                    && pane.ran.is_none()
                    && pane.foreground.is_none()
            })
        })
        .await;
    });
}

/// A command of the pane's shell is what runs until that shell says it ended:
/// the marks of a shell it runs, as over `ssh`, are none of its own, as that
/// one is in front of the terminal as it writes them.
#[test]
fn the_marks_of_a_shell_the_command_runs_are_none_of_the_pane_s() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let inner = "m A; m 'C;cmdline_url=ls'; m 'D;0'; stty -echo; read x; stty echo";
        let body = format!(
            "set -m; export -f m\n\
             m A; m 'C;cmdline_url=inner'; bash -c \"{inner}\"; m 'D;7'\n\
             m A; read y"
        );
        let made = new_session(&mut mux, "nested", bash(&format!("{MARK}\n{body}"))).await;
        let attached = Attached::new(&mut mux, session(made.session), size(40, 10)).await;
        let listing = watched_until(&mut watch, "inner going, asking", |listing| {
            matches!(said(listing), Some(Is::Going(going)) if going.front() == api::Front::Asking)
        })
        .await;
        let Some(Is::Going(going)) = said(&listing) else { unreachable!() };
        assert_eq!(going.command.as_deref(), Some("inner"));
        attached.type_line("").await;
        let listing = watched_until(&mut watch, "inner ended", |listing| {
            matches!(said(listing), Some(Is::Ended(_)))
        })
        .await;
        let Some(Is::Ended(ended)) = said(&listing) else { unreachable!() };
        assert_eq!(ended.command.as_deref(), Some("inner"));
        assert_eq!(ended.ending, Some(api::ran::ended::Ending::Status(7)));
    });
}

/// A job in front under a process that is not, as a shell that `nix develop`
/// started runs its jobs, is what the command does: one that reads a line
/// of the terminal asks. macOS says where no process sleeps.
#[cfg(target_os = "linux")]
#[test]
fn a_job_in_front_under_one_that_is_not_reads_as_asking() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let body = "m A; m 'C;cmdline_url=nested'; bash -c 'set -m; head -n1; true'; m 'D;0'\n\
            m A; read y";
        let opens = script(&format!("{MARK}\n{body}"), api::Ends::Holds);
        let made = new_session(&mut mux, "under", opens).await;
        let attached = Attached::new(&mut mux, session(made.session), size(40, 10)).await;
        watched_until(&mut watch, "nested going, asking", |listing| {
            matches!(said(listing), Some(Is::Going(going)) if going.front() == api::Front::Asking)
        })
        .await;
        attached.type_line("").await;
        watched_until(&mut watch, "nested ended", |listing| {
            matches!(said(listing), Some(Is::Ended(_)))
        })
        .await;
    });
}
