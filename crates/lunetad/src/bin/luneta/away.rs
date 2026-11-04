//! What luneta has zellij do on another machine, where its client runs
//! through ssh: it asks the daemon it watches, which hands each question on
//! to that machine's, through the bridge it watches the machine on. Each
//! question waits on a runtime of its own, on the thread that asks it, as
//! luneta's threads for zellij's actions are none of tokio's.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::keep_printed_response::{self, File};
use lunetad::api::{other_tabs_response, turn_into_panes_response, turn_into_tab_response};
use lunetad::api::{
    closes_response, AskResponse, CheckNameRequest, Client, ClosesRequest, DeleteExitedRequest,
    DumpScreenRequest, GoRequest, KeepPrintedRequest, KillRequest, ListClientsRequest,
    ListContentsRequest, ListDirsRequest, ListDirsResponse, ListExitedRequest, ListExitedResponse,
    ListSkillsRequest, NameSessionRequest, NameSessionResponse, On, OtherTabsRequest,
    RefugeRequest, RefugeResponse, RenameSessionRequest, TurnIntoPanesRequest, TurnIntoTabRequest,
};
use lunetad::asks::{self, Asked, Step};
use lunetad::closes::{Close, Closes};
use lunetad::ended::Exited;
use lunetad::images::{self, Png};
use lunetad::links::{self, Pasted, Ready, Unready};
use lunetad::opens::{Open, Opened};
use lunetad::printed::{Kept, Printed};
use lunetad::skills::{Name, Runs, Skill};
use lunetad::transcripts::{Reading, SessionId};
use lunetad::turns::{IntoPanes, IntoTab, OtherTab};
use lunetad::zoxide::Dir;
use lunetad::{log, Destination, Directory, SessionName};
use tokio::runtime::Runtime;
use tokio_stream::{Stream, StreamExt as _};
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

/// How long an image takes before luneta says it is on its way: a machine
/// on the same network takes even a large one in less.
const SLOW: Duration = Duration::from_secs(1);

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

    /// The name a session luneta makes for `dir` there takes, as
    /// `lunetad::names::free_for` names it there, with the time just before
    /// it read which names zellij there has, by that machine's clock, as
    /// `named_by` reads them; or why it takes none.
    pub fn name(&mut self, dir: &str) -> Result<(SessionName, Option<SystemTime>), String> {
        let request = NameSessionRequest { on: Some(self.on()), dir: dir.to_owned() };
        let named = self.ask(|mut daemon| async move { daemon.name_session(request).await })?;
        named_by(&self.on, named)
    }

    /// The time just before zellij there read which names it has, by that
    /// machine's clock, where it would make a session called `to` now, as
    /// `lunetad::names::unused` says there; or why it would make none. Every
    /// daemon that answers this says when: an answer without the time is of
    /// one that did not do what its answer says, and is refused.
    pub fn check(&mut self, to: &SessionName) -> Result<SystemTime, String> {
        let request = CheckNameRequest { on: Some(self.on()), name: to.to_string() };
        let checked = self.ask(|mut daemon| async move { daemon.check_name(request).await })?;
        let on = &self.on;
        let at = checked.named_at.ok_or_else(|| format!("{on}: lunetad checked {to} at no time"))?;
        SystemTime::try_from(at).map_err(|why| format!("{on}: lunetad checked {to} at {why}"))
    }

    /// The directories zoxide knows there, best first, each with the name a
    /// session luneta makes there takes now, as `lunetad::zoxide::visited_free`
    /// names them there, as `dirs_of` reads them; or why it did not list them.
    /// How many `dirs_of` left out goes in the log.
    pub fn dirs(&mut self) -> Result<Vec<Dir>, String> {
        let request = ListDirsRequest { on: Some(self.on()) };
        let listed = self.ask(|mut daemon| async move { daemon.list_dirs(request).await })?;
        let sent = listed.dirs.len();
        let dirs = dirs_of(listed);
        let left_out = sent - dirs.len();
        if left_out > 0 {
            let on = &self.on;
            log::say(&format!("{on}: left out {left_out} directories no session is made in"));
        }
        Ok(dirs)
    }

    /// What is in the directory `dir` there, as `lunetad::eza::listed` lists
    /// it there, or why eza did not.
    pub fn contents(&mut self, dir: &Directory) -> Result<String, String> {
        let request = ListContentsRequest { on: Some(self.on()), dir: dir.to_string() };
        let listed = self.ask(|mut daemon| async move { daemon.list_contents(request).await })?;
        Ok(listed.listed)
    }

    /// The screen of the terminal `pane` of `session` there, or of the one a
    /// client coming there would be on, where it asks of none, as
    /// `lunetad::screens::dumped` dumps it there: the terminal, and its
    /// lines; or why zellij did not dump it.
    pub fn screen(
        &mut self,
        session: &SessionName,
        pane: Option<u32>,
    ) -> Result<(u32, String), String> {
        let request = DumpScreenRequest { on: Some(self.on()), session: session.to_string(), pane };
        let dumped = self.ask(|mut daemon| async move { daemon.dump_screen(request).await })?;
        Ok((dumped.pane, dumped.screen))
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
    /// says which terminal it opened, and what it left as it was, each of
    /// the machine's, or why it opened nothing.
    pub fn open(&mut self, open: &Open) -> Result<Opened, String> {
        let request = open.asked(Some(self.on()));
        let opened = self.ask(|mut daemon| async move { daemon.open(request).await })?;
        let on = &self.on;
        let notes = opened.notes.into_iter().map(|note| format!("{on}: {note}")).collect();
        Ok(Opened { terminal: opened.terminal, notes })
    }

    /// The skills `dir` there can run, as `lunetad::skills::found_in` finds
    /// them, or why it found none. One of a name no prompt runs, as a newer
    /// daemon's could be, is left out.
    pub fn skills(&mut self, dir: &str) -> Result<Vec<Skill>, String> {
        let request = ListSkillsRequest { on: Some(self.on()), dir: dir.to_owned() };
        let listed = self.ask(|mut daemon| async move { daemon.list_skills(request).await })?;
        let skill = |skill: lunetad::api::list_skills_response::Skill| {
            let runs = if skill.only_you { Runs::OnlyForYou } else { Runs::ForAnyone };
            Some(Skill { name: Name::new(skill.name)?, description: skill.description, runs })
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
    pub fn close(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        close: &Close,
    ) -> Result<(), String> {
        let request = close.asked(Some(self.on()), session, started);
        self.ask(|mut daemon| async move { daemon.close(request).await }).map(|_| ())
    }

    /// The tabs of `session` there other than that of its terminal `pane`, as
    /// `lunetad::turns::other_tabs` says there, or why it does not say.
    pub fn other_tabs(
        &mut self,
        session: &SessionName,
        pane: u32,
    ) -> Result<Vec<OtherTab>, String> {
        let request = OtherTabsRequest { on: Some(self.on()), session: session.to_string(), pane };
        let listed = self.ask(|mut daemon| async move { daemon.other_tabs(request).await })?;
        let tab = |tab: other_tabs_response::Tab| OtherTab { pane: tab.pane, name: tab.name };
        Ok(listed.tabs.into_iter().map(tab).collect())
    }

    /// Has zellij there turn the terminal `pane` of `session` into a tab, as
    /// `lunetad::turns::into_tab` does there, and says what came of it, or why
    /// it did not.
    pub fn turn_into_tab(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        pane: u32,
    ) -> Result<IntoTab, String> {
        let (session, started_at) = (session.to_string(), Some(started.into()));
        let request = TurnIntoTabRequest { on: Some(self.on()), session, pane, started_at };
        let answer = self.ask(|mut daemon| async move { daemon.turn_into_tab(request).await })?;
        match answer.came {
            Some(turn_into_tab_response::Came::Done(_)) => Ok(IntoTab::Done),
            Some(turn_into_tab_response::Came::NoClient(_)) => Ok(IntoTab::NoClient),
            None => Err(self.unread_turn()),
        }
    }

    /// Has zellij there turn the panes of the tab of the terminal `pane` of
    /// `session` into panes of the tab of the terminal `beside`, as
    /// `lunetad::turns::into_panes` does there, and says what came of it where
    /// any moved, or why none did.
    pub fn turn_into_panes(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        pane: u32,
        beside: u32,
    ) -> Result<IntoPanes, String> {
        let (session, started_at) = (session.to_string(), Some(started.into()));
        let request =
            TurnIntoPanesRequest { on: Some(self.on()), session, pane, beside, started_at };
        let answer = self.ask(|mut daemon| async move { daemon.turn_into_panes(request).await })?;
        match answer.came {
            Some(turn_into_panes_response::Came::Done(_)) => Ok(IntoPanes::Done),
            Some(turn_into_panes_response::Came::Partly(partly)) => {
                let turn_into_panes_response::Partly { moved, of, why } = partly;
                Ok(IntoPanes::Partly { moved, of, why })
            },
            None => Err(self.unread_turn()),
        }
    }

    /// Why a turn's answer says nothing this luneta knows: whatever came of it
    /// there, it cannot say.
    fn unread_turn(&self) -> String {
        format!(
            "{}: its lunetad says what came of turning the pane in words this luneta cannot read, \
             as a newer one does",
            self.on
        )
    }

    /// Has zellij there kill `session`, the one that started at `started`,
    /// and delete it, as `lunetad::closes::killed` does there, or says why
    /// it did not.
    pub fn kill(&mut self, session: &SessionName, started: SystemTime) -> Result<(), String> {
        let (session, started_at) = (session.to_string(), Some(started.into()));
        let request = KillRequest { on: Some(self.on()), session, started_at };
        self.ask(|mut daemon| async move { daemon.kill(request).await }).map(|_| ())
    }

    /// The sessions zellij keeps to bring back there, which ended, as
    /// `lunetad::ended::exited` lists them there, as `exited_of` reads them;
    /// or why it did not list them. How many `exited_of` left out goes in the
    /// log.
    pub fn exited(&mut self) -> Result<Vec<Exited>, String> {
        let request = ListExitedRequest { on: Some(self.on()) };
        let listed = self.ask(|mut daemon| async move { daemon.list_exited(request).await })?;
        let sent = listed.exited.len();
        let exited = exited_of(listed);
        let left_out = sent - exited.len();
        if left_out > 0 {
            let on = &self.on;
            log::say(&format!("{on}: left out {left_out} sessions zellij keeps that do not read"));
        }
        Ok(exited)
    }

    /// Has zellij there let go of `session`, one that ended, as
    /// `lunetad::closes::deleted` does there, or says why it did not.
    pub fn delete(&mut self, session: &SessionName) -> Result<(), String> {
        let request = DeleteExitedRequest { on: Some(self.on()), session: session.to_string() };
        self.ask(|mut daemon| async move { daemon.delete_exited(request).await }).map(|_| ())
    }

    /// Has zellij there call `session` `to`, as `lunetad::renames::renamed`
    /// does there, or says why it did not.
    pub fn rename(&mut self, session: &SessionName, to: &SessionName) -> Result<(), String> {
        let (session, to) = (session.to_string(), to.to_string());
        let request = RenameSessionRequest { on: Some(self.on()), session, to };
        self.ask(|mut daemon| async move { daemon.rename_session(request).await }).map(|_| ())
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
    /// it, or as it heard a step that is none, which it hangs up on too.
    pub fn talk(
        &mut self,
        asked: &Asked,
        wanted: &dyn Fn() -> bool,
        hear: &dyn Fn(Step),
    ) -> Option<Result<(), String>> {
        let request = asked.asked(Some(self.on()));
        let (patience, mut daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        let deadline = Instant::now() + asks::TURN + patience;
        let secs = patience.as_secs_f32();
        self.runtime.block_on(async move {
            // The daemon answers with the stream as the turn starts.
            let steps = match tokio::time::timeout(patience, daemon.ask(request)).await {
                Ok(Ok(steps)) => steps.into_inner(),
                Ok(Err(status)) => return Some(Err(said(on, &status))),
                Err(_) => return Some(Err(format!("{on}: lunetad did not answer in {secs}s"))),
            };
            heard(on, steps, deadline, wanted, hear).await
        })
    }

    /// Where a key of luneta's menu goes for what was `pasted` there, as
    /// `lunetad::links` finds it there, by the place of its pane among those
    /// `pasted` says, and the worktree it has made there; or why it found
    /// nothing, as it said there, or as luneta heard no answer by
    /// `links::PATIENCE` and its patience after, by which the daemon there
    /// has done with it.
    pub fn paste(&mut self, pasted: &Pasted) -> Result<Ready<usize>, Unready> {
        let request = pasted.asked(Some(self.on()));
        let (patience, mut daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        let secs = patience.as_secs_f32();
        let minutes = links::PATIENCE.as_secs() / 60;
        self.runtime.block_on(async move {
            // The daemon answers with the stream at once, and the answer in
            // it once it has found it.
            let mut answer = match tokio::time::timeout(patience, daemon.paste(request)).await {
                Ok(Ok(answer)) => answer.into_inner(),
                Ok(Err(status)) => return Err(unready(on, &status)),
                Err(_) => return Err(format!("{on}: lunetad did not answer in {secs}s").into()),
            };
            match tokio::time::timeout(links::PATIENCE + patience, answer.message()).await {
                Ok(Ok(Some(ready))) => {
                    pasted.answered(ready).map_err(|why| format!("{on}: {why}").into())
                },
                Ok(Ok(None)) => {
                    Err(format!("{on}: lunetad's answer ended with nothing in it").into())
                },
                Ok(Err(status)) => Err(unready(on, &status)),
                Err(_) => {
                    Err(format!("{on}: lunetad had found nothing after {minutes} minutes").into())
                },
            }
        })
    }

    /// Has the daemon there keep `png`, as `lunetad::images` does there, and
    /// says the file's path there, or why it kept none: as it said there, or
    /// as luneta heard no answer by `images::PATIENCE` and its patience
    /// after, by which that daemon has given up on it. `slow` hears of an
    /// image that takes longer than `SLOW`. Short of an answer, the question
    /// hangs up as the runtime goes, which leaves no file there.
    pub fn keep_image(&mut self, png: Png, slow: impl FnOnce()) -> Result<String, String> {
        let request = png.asked(Some(self.on()));
        let (patience, mut daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        let secs = patience.as_secs_f32();
        let minutes = images::PATIENCE.as_secs() / 60;
        self.runtime.block_on(async move {
            let kept = async {
                // The daemon answers with the stream at once, and the answer
                // in it once all of the image has come.
                let answer = tokio::time::timeout(patience, daemon.keep_image(request)).await;
                let mut answer = match answer {
                    Ok(Ok(answer)) => answer.into_inner(),
                    Ok(Err(status)) => return Err(said(on, &status)),
                    Err(_) => return Err(format!("{on}: lunetad did not answer in {secs}s")),
                };
                let deadline = images::PATIENCE + patience;
                match tokio::time::timeout(deadline, answer.message()).await {
                    Ok(Ok(Some(kept))) if kept.path.starts_with('/') => Ok(kept.path),
                    Ok(Ok(Some(kept))) => {
                        Err(format!("{on}: lunetad kept the image at \"{}\"", kept.path))
                    },
                    Ok(Ok(None)) => Err(format!("{on}: lunetad's answer ended with nothing in it")),
                    Ok(Err(status)) => Err(said(on, &status)),
                    Err(_) => Err(format!("{on}: the image had not gone after {minutes} minutes")),
                }
            };
            let mut kept = pin!(kept);
            if let Ok(done) = tokio::time::timeout(SLOW, &mut kept).await {
                return done;
            }
            slow();
            kept.await
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

/// How a turn of the ask box's agent on `on` ended, as the daemon hands on
/// its `steps`, each of which goes to `hear` as it comes, for as long as the
/// turn is `wanted`: none once it is not. Or why not: as it ended there, or
/// as its end had not come by `deadline`, or as a step is none. Short of its
/// end there, the question hangs up as `steps` goes, which gives up the turn
/// there.
async fn heard(
    on: &Destination,
    mut steps: impl Stream<Item = Result<AskResponse, Status>> + Unpin,
    deadline: Instant,
    wanted: &dyn Fn() -> bool,
    hear: &dyn Fn(Step),
) -> Option<Result<(), String>> {
    let minutes = asks::TURN.as_secs() / 60;
    loop {
        if !wanted() {
            return None;
        }
        if Instant::now() >= deadline {
            return Some(Err(format!("{on}: the turn had not ended after {minutes} minutes")));
        }
        match tokio::time::timeout(asks::LOOK, steps.next()).await {
            Err(_) => {},
            Ok(Some(Ok(step))) => match Step::of(step) {
                Ok(step) => step.into_iter().for_each(hear),
                Err(why) => return Some(Err(format!("{on}: {why}"))),
            },
            Ok(Some(Err(status))) => return Some(Err(said(on, &status))),
            Ok(None) => return Some(Ok(())),
        }
    }
}

/// The name the daemon of `on` gives a session in `answer`, and the time it
/// read zellij's names at there: none from a daemon older than that time,
/// which names the session all the same, as for one to start. Or why it
/// names none zellij would make.
fn named_by(
    on: &Destination,
    answer: NameSessionResponse,
) -> Result<(SessionName, Option<SystemTime>), String> {
    let NameSessionResponse { name, named_at } = answer;
    let name = SessionName::parse(&name)
        .map_err(|why| format!("{on}: lunetad named a session \"{name}\": {why}"))?;
    Ok((name, named_at.and_then(|at| SystemTime::try_from(at).ok())))
}

/// The directories in `answer`, in its order: those by an absolute path,
/// each named for a session zellij would make. Another is none a session
/// can be made in, as a daemon that did not do what its answer says could
/// send.
fn dirs_of(answer: ListDirsResponse) -> Vec<Dir> {
    let dir = |dir: lunetad::api::list_dirs_response::Dir| {
        let name = SessionName::parse(&dir.name).ok()?;
        Some(Dir { path: Directory::parse(dir.path).ok()?, name })
    };
    answer.dirs.into_iter().filter_map(dir).collect()
}

/// The sessions that ended in `answer`, in its order: those by a name zellij
/// takes, each with how long before zellij listed it it was made. Another
/// is none zellij would bring back, as a daemon that did not do what its
/// answer says could send.
fn exited_of(answer: ListExitedResponse) -> Vec<Exited> {
    let exited = |exited: lunetad::api::list_exited_response::Exited| {
        let name = SessionName::running(&exited.name).ok()?;
        Some(Exited { name, ago: exited.ago?.try_into().ok()? })
    };
    answer.exited.into_iter().filter_map(exited).collect()
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

/// Why the daemon of `on` found nowhere for a key of the menu to go, as
/// `status` says: `Code::NotFound` is its machine having none of the
/// repository, and a daemon older than that says no such thing.
fn unready(on: &Destination, status: &Status) -> Unready {
    let why = said(on, status);
    if status.code() == Code::NotFound { Unready::Nowhere(why) } else { Unready::Failed(why) }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::pin::Pin;

    use lunetad::api::ask_response;

    use super::*;

    /// How long a turn there has, in these tests.
    const DEADLINE: Duration = Duration::from_millis(50);

    /// What the daemon hands on of a turn there.
    type Steps = Pin<Box<dyn Stream<Item = Result<AskResponse, Status>>>>;

    /// `steps`, and then the stream's end.
    fn ending(steps: Vec<Result<AskResponse, Status>>) -> Steps {
        Box::pin(tokio_stream::iter(steps))
    }

    /// `steps`, and then nothing more, ever.
    fn stuck(steps: Vec<Result<AskResponse, Status>>) -> Steps {
        Box::pin(tokio_stream::iter(steps).chain(tokio_stream::pending()))
    }

    /// The steps luneta hears of a turn on nixos as the daemon hands on
    /// `steps`, by a deadline `DEADLINE` off, while the turn is `wanted`, and
    /// how it ended.
    fn heard_of(steps: Steps, wanted: bool) -> (Vec<Step>, Option<Result<(), String>>) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let on = "nixos".parse().unwrap();
        let told = RefCell::new(Vec::new());
        let hear = |step: Step| told.borrow_mut().push(step);
        let deadline = Instant::now() + DEADLINE;
        let ended = runtime.block_on(heard(&on, steps, deadline, &|| wanted, &hear));
        (told.into_inner(), ended)
    }

    /// A turn there ends for luneta as the daemon's stream of it does, its
    /// steps heard as they come: well, or with why not. One whose end has
    /// not come by its deadline ends with luneta saying so, one with a step
    /// that is none ends at that step, and one given up ends with nothing
    /// to say.
    #[test]
    fn a_turn_there_ends_as_its_stream_does_or_by_its_deadline() {
        let said = Step::Said("Looking.".to_owned());
        let well = heard_of(ending(vec![Ok(said.clone().answer())]), true);
        assert_eq!(well, (vec![said.clone()], Some(Ok(()))));
        let failed = Err(Status::failed_precondition("Not logged in"));
        let not_well = heard_of(ending(vec![Ok(said.clone().answer()), failed]), true);
        let why = "nixos: Not logged in".to_owned();
        assert_eq!(not_well, (vec![said.clone()], Some(Err(why))));

        let started = Instant::now();
        let late = heard_of(stuck(vec![Ok(said.clone().answer())]), true);
        let why = "nixos: the turn had not ended after 10 minutes".to_owned();
        assert_eq!(late, (vec![said.clone()], Some(Err(why))));
        assert!(started.elapsed() >= DEADLINE, "{:?}", started.elapsed());

        let unnamed = AskResponse { step: Some(ask_response::Step::Session("../x".to_owned())) };
        let none = heard_of(stuck(vec![Ok(unnamed), Ok(said.answer())]), true);
        let why = "nixos: session: \"../x\" names no session".to_owned();
        assert_eq!(none, (Vec::new(), Some(Err(why))));

        assert_eq!(heard_of(stuck(Vec::new()), false), (Vec::new(), None));
    }

    /// The daemon there names a session to make, and says when it read
    /// zellij's names there. One older than that time says none, and its
    /// name stands all the same, as for a session to start. A name zellij
    /// would not take is none.
    #[test]
    fn a_session_is_named_there_by_a_daemon_old_or_new() {
        let on = "nixos".parse().unwrap();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let answer = |name: &str, at: Option<SystemTime>| NameSessionResponse {
            name: name.to_owned(),
            named_at: at.map(Into::into),
        };
        let cdk = SessionName::running("cdk-2").unwrap();
        assert_eq!(named_by(&on, answer("cdk-2", Some(at))), Ok((cdk.clone(), Some(at))));
        assert_eq!(named_by(&on, answer("cdk-2", None)), Ok((cdk, None)));
        let why = "nixos: lunetad named a session \"../x\": name cannot contain '/'";
        assert_eq!(named_by(&on, answer("../x", None)), Err(why.to_owned()));
        let long = "x".repeat(lunetad::SESSION_NAME_BYTES_MAX + 1);
        let why = "name must be shorter than 61 bytes";
        let why = format!("nixos: lunetad named a session \"{long}\": {why}");
        assert_eq!(named_by(&on, answer(&long, None)), Err(why));
    }

    /// The directories another machine's zoxide knows are those of the
    /// answer, in its order, but for one by no absolute path, or named for a
    /// session zellij would not make, as one too long for it on any host.
    #[test]
    fn another_machine_s_directories_are_those_a_session_can_be_made_in() {
        let dir = |path: &str, name: &str| lunetad::api::list_dirs_response::Dir {
            path: path.to_owned(),
            name: name.to_owned(),
        };
        let answer = ListDirsResponse {
            dirs: vec![
                dir("/home/me/api", "api-2"),
                dir("home/me/relative", "relative"),
                dir("/home/me/odd", "../odd"),
                dir("/home/me/long", &"x".repeat(lunetad::SESSION_NAME_BYTES_MAX + 1)),
                dir("/home/me/blog", "blog"),
            ],
        };
        let made = |path: &str, name: &str| Dir {
            path: Directory::parse(path.to_owned()).unwrap(),
            name: SessionName::parse(name).unwrap(),
        };
        let dirs = [made("/home/me/api", "api-2"), made("/home/me/blog", "blog")];
        assert_eq!(dirs_of(answer), dirs);
    }
}
