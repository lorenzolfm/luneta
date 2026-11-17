use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::keep_checks_response::{self, failed};
use lunetad::api::keep_printed_response::{self, File};
use lunetad::api::{other_tabs_response, turn_into_panes_response, turn_into_tab_response};
use lunetad::api::{
    closes_response, AskResponse, ClosesRequest, DeleteExitedRequest, DumpScreenRequest,
    IssueRepositoriesRequest, KeepChecksRequest, KeepPrintedRequest, KillRequest,
    ListContentsRequest, ListDirsRequest, ListDirsResponse, ListExitedRequest, ListExitedResponse,
    ListSkillsRequest, On, OtherTabsRequest, RenameSessionRequest, TurnIntoPanesRequest,
    TurnIntoTabRequest,
};
use lunetad::asks::{self, Asked, Step};
use lunetad::checks::{self, Failed, Failing, Log, PullPage};
use lunetad::closes::{Close, Closes};
use lunetad::mux::{Exited, TabId};
use lunetad::images::{self, Png};
use lunetad::issues::{self, Filed, Filing, Remote, Unfiled};
use lunetad::links::{Pasted, Ready, Unready};
use lunetad::opens::{Open, Opened};
use lunetad::printed::{Kept, Printed};
use lunetad::prunes::{self, Deleting, Plan, Pruned};
use lunetad::skills::{Name, Runs, Skill};
use lunetad::transcripts::{Reading, SessionId};
use lunetad::turns::{IntoPanes, IntoTab, OtherTab};
use lunetad::zoxide::Dir;
use lunetad::{log, Destination, Directory, SessionName};
use tokio::runtime::Runtime;
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Channel;
use tonic::{Code, Response, Status, Streaming};

use crate::agents::Harness;
use crate::daemon;

pub const ANSWER: Duration = Duration::from_secs(6);

const SLOW: Duration = Duration::from_secs(1);

pub struct Away {
    runtime: Runtime,
    daemon: DaemonClient<Channel>,
    on: Destination,
    patience: Duration,
}

impl Away {
    pub fn reach(destination: &Destination, patience: Duration) -> Result<Away, String> {
        let folder = lunetad::folder::find()
            .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("no runtime: {error}"))?;
        let reached = daemon::running(&folder);
        let daemon = match runtime.block_on(async { tokio::time::timeout(patience, reached).await }) {
            Ok(reached) => reached?,
            Err(_) => return Err(format!("lunetad did not answer in {}s", patience.as_secs_f32())),
        };
        Ok(Away { runtime, daemon, on: destination.clone(), patience })
    }

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

    pub fn contents(&mut self, dir: &Directory) -> Result<String, String> {
        let request = ListContentsRequest { on: Some(self.on()), dir: dir.to_string() };
        let listed = self.ask(|mut daemon| async move { daemon.list_contents(request).await })?;
        Ok(listed.listed)
    }

    pub fn screen(
        &mut self,
        session: &SessionName,
        pane: Option<u32>,
    ) -> Result<(u32, String), String> {
        let request = DumpScreenRequest { on: Some(self.on()), session: session.to_string(), pane };
        let dumped = self.ask(|mut daemon| async move { daemon.dump_screen(request).await })?;
        Ok((dumped.pane, dumped.screen))
    }

    pub fn open(&mut self, open: &Open) -> Result<Opened, String> {
        let request = open.asked(Some(self.on()));
        let opened = self.ask(|mut daemon| async move { daemon.open(request).await })?;
        let on = &self.on;
        let notes = opened.notes.into_iter().map(|note| format!("{on}: {note}")).collect();
        Ok(Opened { terminal: opened.terminal, notes })
    }

    pub fn skills(&mut self, dir: &str, harness: Harness) -> Result<Vec<Skill>, String> {
        let harness = Some(harness.to_api().into());
        let request = ListSkillsRequest { on: Some(self.on()), dir: dir.to_owned(), harness };
        let listed = self.ask(|mut daemon| async move { daemon.list_skills(request).await })?;
        let skill = |skill: lunetad::api::list_skills_response::Skill| {
            let runs = if skill.only_you { Runs::OnlyForYou } else { Runs::ForAnyone };
            Some(Skill { name: Name::new(skill.name)?, description: skill.description, runs })
        };
        Ok(listed.skills.into_iter().filter_map(skill).collect())
    }

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
            Some(keep_printed_response::Printed::Nothing(())) => Ok(Printed::Nothing),
            Some(keep_printed_response::Printed::Unsure(file)) => Ok(Printed::Unsure(kept(file))),
            Some(keep_printed_response::Printed::Top(top)) => Ok(Printed::Top(top.file.map(kept))),
            None => Err(format!(
                "{}: its lunetad says what the pane printed in words this luneta cannot read, as \
                 a newer one does",
                self.on
            )),
        }
    }

    pub fn checks(&mut self, page: &PullPage) -> Result<Failing, String> {
        let request = KeepChecksRequest { on: Some(self.on()), url: page.url().to_owned() };
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_of(checks::FETCHING, async move {
            timeout(patience, daemon.keep_checks(request)).await
        })?;
        let newer = || {
            format!(
                "{}: its lunetad says the log of a check in words this luneta cannot read, as a \
                 newer one does",
                self.on
            )
        };
        let read = |check: keep_checks_response::Failed| {
            let log = match check.log.ok_or_else(newer)? {
                failed::Log::Kept(File { path, lines, left_out }) => Log::Kept(Kept {
                    file: path,
                    lines: usize::try_from(lines).unwrap_or(usize::MAX),
                    left_out: usize::try_from(left_out).unwrap_or(usize::MAX),
                }),
                failed::Log::Unkept(why) => Log::Unkept(why),
            };
            let link = Some(check.link).filter(|link| !link.is_empty());
            Ok(Failed { name: check.name, link, log })
        };
        let failed = answer.failed.into_iter().map(read).collect::<Result<_, String>>()?;
        Ok(Failing { head: answer.head, failed, pending: answer.pending, passed: answer.passed })
    }

    pub fn closes(&mut self, session: &SessionName, pane: u32) -> Result<Closes, String> {
        let request = ClosesRequest { on: Some(self.on()), session: session.to_string(), pane };
        let answer = self.ask(|mut daemon| async move { daemon.closes(request).await })?;
        match answer.closes {
            Some(closes_response::Closes::Pane(())) => Ok(Closes::Pane),
            Some(closes_response::Closes::Tab(tab)) => {
                Ok(Closes::Tab { id: TabId(tab.id), panes: tab.panes })
            },
            Some(closes_response::Closes::Session(())) => Ok(Closes::Session),
            None => Err(format!(
                "{}: its lunetad says what closing the pane closes in words this luneta cannot \
                 read, as a newer one does",
                self.on
            )),
        }
    }

    pub fn close(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        close: &Close,
    ) -> Result<(), String> {
        let request = close.asked(Some(self.on()), session, started);
        self.ask(|mut daemon| async move { daemon.close(request).await })
    }

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
            Some(turn_into_tab_response::Came::Done(())) => Ok(IntoTab::Done),
            Some(turn_into_tab_response::Came::NoClient(())) => Ok(IntoTab::NoClient),
            None => Err(self.unread_turn()),
        }
    }

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
            Some(turn_into_panes_response::Came::Done(())) => Ok(IntoPanes::Done),
            Some(turn_into_panes_response::Came::Partly(partly)) => {
                let turn_into_panes_response::Partly { moved, of, why } = partly;
                Ok(IntoPanes::Partly { moved, of, why })
            },
            None => Err(self.unread_turn()),
        }
    }

    fn unread_turn(&self) -> String {
        format!(
            "{}: its lunetad says what came of turning the pane in words this luneta cannot read, \
             as a newer one does",
            self.on
        )
    }

    pub fn kill(&mut self, session: &SessionName, started: SystemTime) -> Result<(), String> {
        let (session, started_at) = (session.to_string(), Some(started.into()));
        let request = KillRequest { on: Some(self.on()), session, started_at };
        self.ask(|mut daemon| async move { daemon.kill(request).await })
    }

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

    pub fn delete(&mut self, session: &SessionName) -> Result<(), String> {
        let request = DeleteExitedRequest { on: Some(self.on()), session: session.to_string() };
        self.ask(|mut daemon| async move { daemon.delete_exited(request).await })
    }

    pub fn rename(&mut self, session: &SessionName, to: &SessionName) -> Result<(), String> {
        let (session, to) = (session.to_string(), to.to_string());
        let request = RenameSessionRequest { on: Some(self.on()), session, to };
        self.ask(|mut daemon| async move { daemon.rename_session(request).await })
    }

    pub fn ends(&mut self, reading: &Reading) -> Result<HashMap<SessionId, String>, String> {
        let request = reading.asked(Some(self.on()));
        let answer = self.ask(|mut daemon| async move { daemon.transcript_ends(request).await })?;
        let end = |(id, end): (String, String)| Some((SessionId::parse(&id)?, end));
        Ok(answer.ends.into_iter().filter_map(end).collect())
    }

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
            let steps = match tokio::time::timeout(patience, daemon.ask(request)).await {
                Ok(Ok(steps)) => steps.into_inner(),
                Ok(Err(status)) => return Some(Err(on.said(&status))),
                Err(_) => return Some(Err(format!("{on}: lunetad did not answer in {secs}s"))),
            };
            heard(on, steps, deadline, wanted, hear).await
        })
    }

    pub fn paste(&mut self, pasted: &Pasted) -> Result<Ready<usize>, Unready> {
        let (patience, daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        self.runtime.block_on(pasted.ready_on(daemon, on, patience))
    }

    pub fn issue_remotes(&mut self, dir: &Directory) -> Result<Vec<Remote>, String> {
        let request = IssueRepositoriesRequest { on: Some(self.on()), dir: dir.to_string() };
        let listed =
            self.ask(|mut daemon| async move { daemon.issue_repositories(request).await })?;
        let remotes = listed.repositories.into_iter().map(Remote::of);
        remotes.collect::<Result<_, _>>().map_err(|why| format!("{}: {why}", self.on))
    }

    pub fn file_issue(&mut self, filing: &Filing) -> Result<Filed, Unfiled> {
        let request = filing.asked(Some(self.on()));
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_or_code(issues::AT_MOST, async move {
            timeout(patience, daemon.file_issue(request)).await
        });
        let answer = answer.map_err(|(why, code)| Unfiled::of(why, code))?;
        Filed::of(answer).map_err(|why| Unfiled::InDoubt(format!("{}: {why}", self.on)))
    }

    pub fn prunes(&mut self, dir: &Directory) -> Result<Plan, String> {
        let request = prunes::asked(Some(self.on()), dir);
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_of(prunes::PLANNING, async move {
            timeout(patience, daemon.prunes(request)).await
        })?;
        Ok(answer.into())
    }

    pub fn prune(
        &mut self,
        dir: &Directory,
        agreed: &BTreeSet<Deleting>,
    ) -> Result<Pruned, String> {
        let request = prunes::told(Some(self.on()), dir, agreed);
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_of(prunes::PLANNING + prunes::DELETING, async move {
            timeout(patience, daemon.prune(request)).await
        })?;
        prunes::pruned_of(answer)
    }

    fn answer_of<T>(
        &mut self,
        late: Duration,
        asking: impl Future<Output = Result<Result<Response<Streaming<T>>, Status>, Elapsed>>,
    ) -> Result<T, String> {
        self.answer_or_code(late, asking).map_err(|(why, _)| why)
    }

    fn answer_or_code<T>(
        &mut self,
        late: Duration,
        asking: impl Future<Output = Result<Result<Response<Streaming<T>>, Status>, Elapsed>>,
    ) -> Result<T, (String, Option<Code>)> {
        let on = &self.on;
        let secs = self.patience.as_secs_f32();
        self.runtime.block_on(async {
            let said = |status: &Status| (on.said(status), Some(status.code()));
            let mut answer = match asking.await {
                Ok(Ok(answer)) => answer.into_inner(),
                Ok(Err(status)) => return Err(said(&status)),
                Err(_) => return Err((format!("{on}: lunetad did not answer in {secs}s"), None)),
            };
            match timeout(late, answer.message()).await {
                Ok(Ok(Some(answer))) => Ok(answer),
                Ok(Ok(None)) => {
                    Err((format!("{on}: lunetad's answer ended with nothing in it"), None))
                },
                Ok(Err(status)) => Err(said(&status)),
                Err(_) => {
                    Err((format!("{on}: lunetad said nothing for {}s", late.as_secs()), None))
                },
            }
        })
    }

    pub fn keep_image(&mut self, png: Png, slow: impl FnOnce()) -> Result<String, String> {
        let request = png.asked(Some(self.on()));
        let (patience, mut daemon, on) = (self.patience, self.daemon.clone(), &self.on);
        let secs = patience.as_secs_f32();
        let minutes = images::PATIENCE.as_secs() / 60;
        self.runtime.block_on(async move {
            let kept = async {
                let answer = tokio::time::timeout(patience, daemon.keep_image(request)).await;
                let mut answer = match answer {
                    Ok(Ok(answer)) => answer.into_inner(),
                    Ok(Err(status)) => return Err(on.said(&status)),
                    Err(_) => return Err(format!("{on}: lunetad did not answer in {secs}s")),
                };
                let deadline = images::PATIENCE + patience;
                match tokio::time::timeout(deadline, answer.message()).await {
                    Ok(Ok(Some(kept))) if kept.path.starts_with('/') => Ok(kept.path),
                    Ok(Ok(Some(kept))) => {
                        Err(format!("{on}: lunetad kept the image at \"{}\"", kept.path))
                    },
                    Ok(Ok(None)) => Err(format!("{on}: lunetad's answer ended with nothing in it")),
                    Ok(Err(status)) => Err(on.said(&status)),
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

    fn ask<T, F>(&mut self, asked: impl FnOnce(DaemonClient<Channel>) -> F) -> Result<T, String>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        self.answer(asked)?.map_err(|status| self.on.said(&status))
    }

    fn answer<T, F>(
        &mut self,
        asked: impl FnOnce(DaemonClient<Channel>) -> F,
    ) -> Result<Result<T, Status>, String>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let (patience, daemon) = (self.patience, self.daemon.clone());
        let asked = async move { tokio::time::timeout(patience, asked(daemon)).await };
        let (on, secs) = (&self.on, self.patience.as_secs_f32());
        match self.runtime.block_on(asked) {
            Ok(answer) => Ok(answer.map(Response::into_inner)),
            Err(_) => Err(format!("{on}: lunetad did not answer in {secs}s")),
        }
    }
}

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
            Ok(Some(Err(status))) => return Some(Err(on.said(&status))),
            Ok(None) => return Some(Ok(())),
        }
    }
}

fn dirs_of(answer: ListDirsResponse) -> Vec<Dir> {
    let dir = |dir: lunetad::api::list_dirs_response::Dir| {
        let name = SessionName::parse(&dir.name).ok()?;
        Some(Dir { path: Directory::parse(dir.path).ok()?, name })
    };
    answer.dirs.into_iter().filter_map(dir).collect()
}

fn exited_of(answer: ListExitedResponse) -> Vec<Exited> {
    let exited = |exited: lunetad::api::list_exited_response::Exited| {
        let name = SessionName::running(&exited.name).ok()?;
        Some(Exited { name, ago: exited.ago?.try_into().ok()? })
    };
    answer.exited.into_iter().filter_map(exited).collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::pin::Pin;

    use lunetad::api::ask_response;

    use super::*;

    const DEADLINE: Duration = Duration::from_millis(50);

    type Steps = Pin<Box<dyn Stream<Item = Result<AskResponse, Status>>>>;

    fn ending(steps: Vec<Result<AskResponse, Status>>) -> Steps {
        Box::pin(tokio_stream::iter(steps))
    }

    fn stuck(steps: Vec<Result<AskResponse, Status>>) -> Steps {
        Box::pin(tokio_stream::iter(steps).chain(tokio_stream::pending()))
    }

    fn heard_of(steps: Steps, wanted: bool) -> (Vec<Step>, Option<Result<(), String>>) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let on = "nixos".parse().unwrap();
        let told = RefCell::new(Vec::new());
        let hear = |step: Step| told.borrow_mut().push(step);
        let deadline = Instant::now() + DEADLINE;
        let ended = runtime.block_on(heard(&on, steps, deadline, &|| wanted, &hear));
        (told.into_inner(), ended)
    }

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
