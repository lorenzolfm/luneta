use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant};

use luned::api::keep_checks_response::{self, failed};
use luned::api::{
    AskResponse, IssueRepositoriesRequest, KeepChecksRequest, ListContentsRequest,
    ListRepositoriesRequest, ListRepositoriesResponse, ListSkillsRequest, On, PrintedFile,
};
use luned::asks::{self, Asked, Step};
use luned::checks::{self, Failed, Failing, Log, PullPage};
use luned::images::{self, Png};
use luned::issues::{self, Filed, Filing, Remote, Unfiled};
use luned::links::{Pasted, Ready, Unready};
use luned::printed::Kept;
use luned::prunes::{self, Deleting, Plan, Pruned};
use luned::rebases::{self, Rebased, Unrebased};
use luned::repositories::Repository;
use luned::skills::{Name, Runs, Skill};
use luned::transcripts::{Reading, SessionId};
use luned::{log, Daemon, Destination, Directory, SessionName};
use tokio::runtime::Runtime;
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use tokio_stream::{Stream, StreamExt as _};
use tonic::{Code, Response, Status, Streaming};

use crate::agents::Harness;
use crate::daemon;

pub const ANSWER: Duration = Duration::from_secs(6);

const SLOW: Duration = Duration::from_secs(1);

pub struct Away {
    runtime: Runtime,
    daemon: Daemon,
    on: Destination,
    patience: Duration,
}

impl Away {
    pub fn reach(destination: &Destination, patience: Duration) -> Result<Away, String> {
        let folder = luned::folder::find().map_err(|unfound| format!("{unfound}, to find luned in"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("no runtime: {error}"))?;
        let reached = daemon::running(&folder);
        let daemon = match runtime.block_on(async { tokio::time::timeout(patience, reached).await }) {
            Ok(reached) => reached?,
            Err(_) => return Err(format!("luned did not answer in {}s", patience.as_secs_f32())),
        };
        Ok(Away { runtime, daemon, on: destination.clone(), patience })
    }

    pub fn repositories(&mut self) -> Result<Vec<Repository>, String> {
        let request = ListRepositoriesRequest { on: Some(self.on()) };
        let listed =
            self.ask(|mut daemon| async move { daemon.list_repositories(request).await })?;
        let sent = listed.repositories.len();
        let repositories = repositories_of(listed);
        let left_out = sent - repositories.len();
        if left_out > 0 {
            let on = &self.on;
            log::say(&format!("{on}: left out {left_out} repositories no project is named for"));
        }
        Ok(repositories)
    }

    pub fn contents(&mut self, dir: &Directory) -> Result<String, String> {
        let request = ListContentsRequest { on: Some(self.on()), dir: dir.to_string() };
        let listed = self.ask(|mut daemon| async move { daemon.list_contents(request).await })?;
        Ok(listed.listed)
    }

    pub fn skills(&mut self, dir: &Directory, harness: Harness) -> Result<Vec<Skill>, String> {
        let harness = Some(harness.to_api().into());
        let dir = dir.as_str().to_owned();
        let request = ListSkillsRequest { on: Some(self.on()), dir, harness };
        let listed = self.ask(|mut daemon| async move { daemon.list_skills(request).await })?;
        let skill = |skill: luned::api::list_skills_response::Skill| {
            let runs = if skill.only_you { Runs::OnlyForYou } else { Runs::ForAnyone };
            Some(Skill { name: Name::new(skill.name)?, description: skill.description, runs })
        };
        Ok(listed.skills.into_iter().filter_map(skill).collect())
    }

    pub fn checks(&mut self, page: &PullPage) -> Result<Failing, String> {
        let request = KeepChecksRequest { on: Some(self.on()), url: page.url().to_owned() };
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_of(checks::FETCHING, async move {
            timeout(patience, daemon.keep_checks(request)).await
        })?;
        let newer = || {
            format!(
                "{}: its luned says the log of a check in words this lune cannot read, as a \
                 newer one does",
                self.on
            )
        };
        let read = |check: keep_checks_response::Failed| {
            let log = match check.log.ok_or_else(newer)? {
                failed::Log::Kept(PrintedFile { path, lines, left_out }) => Log::Kept(Kept {
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
                Err(_) => return Some(Err(format!("{on}: luned did not answer in {secs}s"))),
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

    pub fn rebase(&mut self, dir: &Directory, branch: &str) -> Result<Rebased, Unrebased> {
        let request = rebases::asked(Some(self.on()), dir, Some(branch));
        let (patience, mut daemon) = (self.patience, self.daemon.clone());
        let answer = self.answer_or_code(rebases::REBASING, async move {
            timeout(patience, daemon.rebase(request)).await
        });
        let answer = answer.map_err(|(why, code)| Unrebased::of(why, code))?;
        Rebased::of_api(answer).map_err(|why| Unrebased::InDoubt(format!("{}: {why}", self.on)))
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
                Err(_) => return Err((format!("{on}: luned did not answer in {secs}s"), None)),
            };
            match timeout(late, answer.message()).await {
                Ok(Ok(Some(answer))) => Ok(answer),
                Ok(Ok(None)) => {
                    Err((format!("{on}: luned's answer ended with nothing in it"), None))
                },
                Ok(Err(status)) => Err(said(&status)),
                Err(_) => {
                    Err((format!("{on}: luned said nothing for {}s", late.as_secs()), None))
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
                    Err(_) => return Err(format!("{on}: luned did not answer in {secs}s")),
                };
                let deadline = images::PATIENCE + patience;
                match tokio::time::timeout(deadline, answer.message()).await {
                    Ok(Ok(Some(kept))) if kept.path.starts_with('/') => Ok(kept.path),
                    Ok(Ok(Some(kept))) => {
                        Err(format!("{on}: luned kept the image at \"{}\"", kept.path))
                    },
                    Ok(Ok(None)) => Err(format!("{on}: luned's answer ended with nothing in it")),
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

    fn ask<T, F>(&mut self, asked: impl FnOnce(Daemon) -> F) -> Result<T, String>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        self.answer(asked)?.map_err(|status| self.on.said(&status))
    }

    fn answer<T, F>(
        &mut self,
        asked: impl FnOnce(Daemon) -> F,
    ) -> Result<Result<T, Status>, String>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let (patience, daemon) = (self.patience, self.daemon.clone());
        let asked = async move { tokio::time::timeout(patience, asked(daemon)).await };
        let (on, secs) = (&self.on, self.patience.as_secs_f32());
        match self.runtime.block_on(asked) {
            Ok(answer) => Ok(answer.map(Response::into_inner)),
            Err(_) => Err(format!("{on}: luned did not answer in {secs}s")),
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

fn repositories_of(answer: ListRepositoriesResponse) -> Vec<Repository> {
    let repository = |said: luned::api::list_repositories_response::Repository| {
        let name = SessionName::parse(&said.name).ok()?;
        Some(Repository { path: Directory::parse(said.path).ok()?, name })
    };
    answer.repositories.into_iter().filter_map(repository).collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::pin::Pin;

    use lune_waits::Began;
    use luned::api::ask_response;

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

        let started = Began::now();
        let late = heard_of(stuck(vec![Ok(said.clone().answer())]), true);
        let why = "nixos: the turn had not ended after 10 minutes".to_owned();
        assert_eq!(late, (vec![said.clone()], Some(Err(why))));
        started.lasted().at_least(DEADLINE, "a turn there is waited for to its deadline");

        let unnamed = AskResponse { step: Some(ask_response::Step::Session("../x".to_owned())) };
        let none = heard_of(stuck(vec![Ok(unnamed), Ok(said.answer())]), true);
        let why = "nixos: session: \"../x\" names no session".to_owned();
        assert_eq!(none, (Vec::new(), Some(Err(why))));

        assert_eq!(heard_of(stuck(Vec::new()), false), (Vec::new(), None));
    }

    #[test]
    fn another_machine_s_repositories_are_those_a_project_can_be_named_for() {
        let dir = |path: &str, name: &str| luned::api::list_repositories_response::Repository {
            path: path.to_owned(),
            name: name.to_owned(),
        };
        let answer = ListRepositoriesResponse {
            repositories: vec![
                dir("/home/me/api", "api-2"),
                dir("home/me/relative", "relative"),
                dir("/home/me/odd", "../odd"),
                dir("/home/me/long", &"x".repeat(luned::SESSION_NAME_BYTES_MAX + 1)),
                dir("/home/me/blog", "blog"),
            ],
        };
        let made = |path: &str, name: &str| Repository {
            path: Directory::parse(path.to_owned()).unwrap(),
            name: SessionName::parse(name).unwrap(),
        };
        let repositories = [made("/home/me/api", "api-2"), made("/home/me/blog", "blog")];
        assert_eq!(repositories_of(answer), repositories);
    }
}
