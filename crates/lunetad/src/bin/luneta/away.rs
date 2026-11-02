//! What luneta has zellij do on another machine, where its client runs
//! through ssh: it asks the daemon it watches, which hands each question on
//! to that machine's, through the bridge it watches the machine on. Each
//! question waits on a runtime of its own, on the thread that asks it, as
//! luneta's threads for zellij's actions are none of tokio's.

use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::keep_printed_response::{self, File};
use lunetad::api::{
    closes_response, Client, ClosesRequest, GoRequest, KeepPrintedRequest, KillRequest,
    ListClientsRequest, ListSkillsRequest, NameSessionRequest, On, RefugeRequest, RefugeResponse,
    StartRequest,
};
use lunetad::asks::{self, Asked, Step};
use lunetad::closes::{Close, Closes};
use lunetad::opens::Open;
use lunetad::printed::{Kept, Printed};
use lunetad::skills::{Name, Skill};
use lunetad::transcripts::{Reading, SessionId};
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

    /// Has zellij there take the client that last typed in the session `at`
    /// to a session it makes, `to`, that starts in `cwd`, as
    /// `lunetad::moves::start` says, or says why it did not.
    pub fn start(
        &mut self,
        at: &SessionName,
        to: &SessionName,
        cwd: Option<&str>,
    ) -> Result<(), String> {
        let (at, to, cwd) = (at.to_string(), to.to_string(), cwd.map(str::to_owned));
        let request = StartRequest { on: Some(self.on()), at, to, cwd };
        self.ask(|mut daemon| async move { daemon.start(request).await }).map(|_| ())
    }

    /// The name a session luneta makes for `dir` there takes, as
    /// `lunetad::names::free_for` names it there, or why it takes none.
    pub fn name(&mut self, dir: &str) -> Result<SessionName, String> {
        let request = NameSessionRequest { on: Some(self.on()), dir: dir.to_owned() };
        let named = self.ask(|mut daemon| async move { daemon.name_session(request).await })?;
        let on = &self.on;
        SessionName::running(&named.name)
            .map_err(|why| format!("{on}: lunetad named a session \"{}\": {why}", named.name))
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

    /// Has zellij there open `open`, as `lunetad::opens::opening` does, and
    /// says which terminal it opened, or why it did not.
    pub fn open(&mut self, open: &Open) -> Result<u32, String> {
        let request = open.asked(Some(self.on()));
        let opened = self.ask(|mut daemon| async move { daemon.open(request).await })?;
        Ok(opened.terminal)
    }

    /// The skills `dir` there can run, as `lunetad::skills::found_in` finds
    /// them, or why it found none. One of a name no prompt runs, as a newer
    /// daemon's could be, is left out.
    pub fn skills(&mut self, dir: &str) -> Result<Vec<Skill>, String> {
        let request = ListSkillsRequest { on: Some(self.on()), dir: dir.to_owned() };
        let listed = self.ask(|mut daemon| async move { daemon.list_skills(request).await })?;
        let skill = |skill: lunetad::api::list_skills_response::Skill| {
            Some(Skill { name: Name::new(skill.name)?, description: skill.description })
        };
        Ok(listed.skills.into_iter().filter_map(skill).collect())
    }

    /// Has zellij there dump the terminal `pane` of `session`, and keeps
    /// what it printed after the line `typed` was last typed on, there, as
    /// `lunetad::printed::kept_after` does, or says why it kept nothing.
    pub fn printed(
        &mut self,
        session: &SessionName,
        pane: u32,
        typed: &Said,
    ) -> Result<Printed, String> {
        let (session, typed) = (session.to_string(), typed.as_str().to_owned());
        let request = KeepPrintedRequest { on: Some(self.on()), session, pane, typed };
        let answer = self.ask(|mut daemon| async move { daemon.keep_printed(request).await })?;
        let kept = |File { path, lines, left_out }| Kept {
            file: path,
            lines: usize::try_from(lines).unwrap_or(usize::MAX),
            left_out: usize::try_from(left_out).unwrap_or(usize::MAX),
        };
        match answer.printed {
            Some(keep_printed_response::Printed::After(file)) => Ok(Printed::After(kept(file))),
            Some(keep_printed_response::Printed::Nothing(_)) => Ok(Printed::Nothing),
            Some(keep_printed_response::Printed::Unsure(file)) => Ok(Printed::Unsure(kept(file))),
            Some(keep_printed_response::Printed::Top(top)) => Ok(Printed::Top(top.file.map(kept))),
            None => Err(format!(
                "{}: its lunetad says what the pane printed in words this luneta cannot read, as \
                 a newer one does",
                self.on
            )),
        }
    }

    /// What closing the terminal `pane` of `session` there closes, as
    /// `lunetad::closes::closes_now` says there, or why it closes nothing.
    pub fn closes(&mut self, session: &SessionName, pane: u32) -> Result<Closes, String> {
        let request = ClosesRequest { on: Some(self.on()), session: session.to_string(), pane };
        let answer = self.ask(|mut daemon| async move { daemon.closes(request).await })?;
        match answer.closes {
            Some(closes_response::Closes::Pane(_)) => Ok(Closes::Pane),
            Some(closes_response::Closes::Tab(tab)) => {
                Ok(Closes::Tab { id: tab.id, panes: tab.panes })
            },
            Some(closes_response::Closes::Session(_)) => Ok(Closes::Session),
            None => Err(format!(
                "{}: its lunetad says what closing the pane closes in words this luneta cannot \
                 read, as a newer one does",
                self.on
            )),
        }
    }

    /// Has zellij there close `close` of `session`, as
    /// `lunetad::closes::closed` does there, or says why it did not.
    pub fn close(&mut self, session: &SessionName, close: &Close) -> Result<(), String> {
        let request = close.asked(Some(self.on()), session);
        self.ask(|mut daemon| async move { daemon.close(request).await }).map(|_| ())
    }

    /// Has zellij there kill `session`, the one that started at `started`,
    /// and delete it, as `lunetad::closes::killed` does there, or says why
    /// it did not.
    pub fn kill(&mut self, session: &SessionName, started: SystemTime) -> Result<(), String> {
        let (session, started_at) = (session.to_string(), Some(started.into()));
        let request = KillRequest { on: Some(self.on()), session, started_at };
        self.ask(|mut daemon| async move { daemon.kill(request).await }).map(|_| ())
    }

    /// The end of each transcript `reading` reads there, by its session's
    /// id, as `lunetad::transcripts` reads them there, or why it read none.
    /// One by an id that names no session, as a newer daemon's could be, is
    /// left out.
    pub fn ends(&mut self, reading: &Reading) -> Result<HashMap<SessionId, String>, String> {
        let request = reading.asked(Some(self.on()));
        let answer = self.ask(|mut daemon| async move { daemon.transcript_ends(request).await })?;
        let end = |(id, end): (String, String)| Some((SessionId::parse(&id)?, end));
        Ok(answer.ends.into_iter().filter_map(end).collect())
    }

    /// Has Claude Code take the turn `asked` there, as `lunetad::asks` runs
    /// it there, and hands each step it hears to `hear` as it comes, for as
    /// long as the turn is `wanted`: none once it is not, as the question
    /// hangs up then, which gives up the turn there. Or how the turn ended:
    /// well, or why not, as it did there, or as luneta heard nothing of its
    /// end by `asks::TURN` and its patience after, by which that daemon ends
    /// it.
    pub fn talk(
        &mut self,
        asked: &Asked,
        wanted: &dyn Fn() -> bool,
        hear: &dyn Fn(Step),
    ) -> Option<Result<(), String>> {
        let request = asked.asked(Some(self.on()));
        let (patience, mut daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        let deadline = Instant::now() + asks::TURN + patience;
        let (secs, minutes) = (patience.as_secs_f32(), asks::TURN.as_secs() / 60);
        self.runtime.block_on(async move {
            // The daemon answers with the stream as the turn starts.
            let mut steps = match tokio::time::timeout(patience, daemon.ask(request)).await {
                Ok(Ok(steps)) => steps.into_inner(),
                Ok(Err(status)) => return Some(Err(said(on, &status))),
                Err(_) => return Some(Err(format!("{on}: lunetad did not answer in {secs}s"))),
            };
            loop {
                if !wanted() {
                    return None;
                }
                if Instant::now() >= deadline {
                    let late = format!("{on}: the turn had not ended after {minutes} minutes");
                    return Some(Err(late));
                }
                match tokio::time::timeout(asks::LOOK, steps.message()).await {
                    Err(_) => {},
                    Ok(Ok(Some(step))) => Step::of(step).into_iter().for_each(hear),
                    Ok(Ok(None)) => return Some(Ok(())),
                    Ok(Err(status)) => return Some(Err(said(on, &status))),
                }
            }
        })
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
            format!("{on}: its lunetad, or this one, is older than what luneta asked of it")
        },
        _ if status.message().is_empty() => format!("{on}: lunetad: {}", status.code()),
        _ => format!("{on}: {}", status.message()),
    }
}
