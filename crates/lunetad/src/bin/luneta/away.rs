//! What luneta has zellij do on another machine, where its client runs
//! through ssh: it asks the daemon it watches, which hands each question on
//! to that machine's, through the bridge it watches the machine on. Each
//! question waits on a runtime of its own, on the thread that asks it, as
//! luneta's threads for zellij's actions are none of tokio's.

use std::future::Future;
use std::time::Duration;

use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::{Client, GoRequest, ListClientsRequest, On, RefugeRequest, RefugeResponse};
use lunetad::{Destination, SessionName};
use tokio::runtime::Runtime;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::daemon::{self, Reach};

/// How long luneta waits for an answer on a thread of its own: the daemon
/// answers in 5 seconds, or says it did not, and a connection that stalls
/// says nothing at all.
pub const ANSWER: Duration = Duration::from_secs(6);

/// How long luneta waits for an answer that its screen waits on: a machine
/// on the same network answers in milliseconds.
pub const QUICK: Duration = Duration::from_secs(1);

/// The daemon of a machine, through the daemon luneta watches.
pub struct Away {
    runtime: Runtime,
    daemon: DaemonClient<Channel>,
    on: Destination,
    /// How long it waits for each answer.
    patience: Duration,
}

impl Away {
    /// The daemon of `destination`, through the one luneta watches, waited
    /// on for `patience` for each answer; or why luneta does not reach that
    /// one.
    pub fn reach(destination: &Destination, patience: Duration) -> Result<Away, String> {
        let folder = lunetad::folder::find()
            .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("no runtime: {error}"))?;
        // A timer is made within a runtime, which it needs.
        let reached = daemon::reach(&folder, Reach::Running);
        let daemon = match runtime.block_on(async { tokio::time::timeout(patience, reached).await }) {
            Ok(reached) => reached?,
            Err(_) => return Err(format!("lunetad did not answer in {}s", patience.as_secs_f32())),
        };
        Ok(Away { runtime, daemon, on: destination.clone(), patience })
    }

    /// Has zellij there take the client that last typed in the session `at`
    /// to the pane `pane` of `to`, or to `to` alone, or says why it did not.
    pub fn go(
        &mut self,
        at: &SessionName,
        to: &SessionName,
        pane: Option<u32>,
    ) -> Result<(), String> {
        let (at, to) = (at.to_string(), to.to_string());
        let request = GoRequest { on: Some(self.on()), at, to, pane };
        self.ask(|mut daemon| async move { daemon.go(request).await }).map(|_| ())
    }

    /// The clients of `session` there, as zellij lists them now, or why it
    /// does not.
    pub fn clients(&mut self, session: &str) -> Result<Vec<Client>, String> {
        let request = ListClientsRequest { on: Some(self.on()), session: session.to_owned() };
        let listed = self.ask(|mut daemon| async move { daemon.list_clients(request).await })?;
        Ok(listed.clients)
    }

    /// What became there of the session of a client that ended in one of
    /// `seats`, and the session it goes on in, as `lunetad::moves::refuge`
    /// finds them there; or why the daemon there did not say.
    pub fn refuge(&mut self, seats: &[SessionName]) -> Result<RefugeResponse, String> {
        let seats = seats.iter().map(SessionName::to_string).collect();
        let request = RefugeRequest { on: Some(self.on()), seats };
        self.ask(|mut daemon| async move { daemon.refuge(request).await })
    }

    fn on(&self) -> On {
        On { destination: self.on.to_string() }
    }

    /// What the daemon answers `asked`, within its patience, or why it did
    /// not.
    fn ask<T, F>(&mut self, asked: impl FnOnce(DaemonClient<Channel>) -> F) -> Result<T, String>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let (patience, daemon) = (self.patience, self.daemon.clone());
        let asked = async move { tokio::time::timeout(patience, asked(daemon)).await };
        let (on, secs) = (&self.on, self.patience.as_secs_f32());
        match self.runtime.block_on(asked) {
            Ok(Ok(answer)) => Ok(answer.into_inner()),
            Ok(Err(status)) => Err(said(on, &status)),
            Err(_) => Err(format!("{on}: lunetad did not answer in {secs}s")),
        }
    }
}

/// What `status`, of a question for `on`, says: what lunetad said, or what a
/// daemon older than the question would mean.
fn said(on: &Destination, status: &Status) -> String {
    match status.code() {
        Code::Unimplemented => {
            format!("{on}: its lunetad, or this one, is older than luneta's going there")
        },
        _ if status.message().is_empty() => format!("{on}: lunetad: {}", status.code()),
        _ => format!("{on}: {}", status.message()),
    }
}
