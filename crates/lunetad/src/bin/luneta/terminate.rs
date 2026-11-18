use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher as _};
use std::io::{self, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::CommandFactory as _;
use lunetad::api::agent;
use lunetad::asks::Turn;
use lunetad::program::{self, Ran};
use lunetad::projects::{self, Forgotten, Projects};
use lunetad::terminate::{self, Running};
use lunetad::Destination;
use signal_hook::consts::SIGHUP;

use crate::close::Doing;
use crate::commands;
use crate::drive::{self, Agreed, PLANNED};
use crate::machines;

const ROUNDS: usize = 3;

const CONNECT_SECS: u32 = 10;

const ASKED: Duration = Duration::from_secs(30);

const ENDING: Duration = Duration::from_secs(120);

const SSH_FAILED: i32 = 255;

pub fn run(yes: Option<Agreed>, here: bool, ending: bool, folder: &Path) -> ExitCode {
    if here {
        return if ending { end_alone(folder) } else { say_alone(folder) };
    }
    let turn = match drive::turn_here() {
        Ok(turn) => turn,
        Err(why) => return failed(&why),
    };
    let destinations = match machines::named() {
        Ok(destinations) => destinations,
        Err(why) => {
            return failed(&format!("ended nothing, as the hosts file does not read: {why}"));
        },
    };
    let planned = plan(&destinations, folder);
    let agreed = Agreed::of_terminate(&planned.keys(), turn.map(Turn::key));
    let takes = match turn {
        None => Some(agreed),
        Some(turn) => {
            turn.before().map(|before| Agreed::of_terminate(&planned.keys(), Some(before)))
        },
    };
    if yes.is_some() && yes == takes {
        return terminated(&planned, folder);
    }
    for line in planned.lines() {
        say(&line);
    }
    let _ = writeln!(io::stderr(), "luneta: {}", unagreed(yes, agreed));
    ExitCode::from(PLANNED)
}

fn unagreed(yes: Option<Agreed>, agreed: Agreed) -> String {
    match yes {
        Some(yes) if yes == agreed => format!(
            "ended nothing, as luneta's ask box takes a yes to a plan only in the turn after the \
             one that said it, with the user's answer between them: end your turn asking them, and \
             once they say yes, run it again with --yes {agreed}"
        ),
        Some(_) => format!(
            "ended nothing, as it was told yes to another plan than this; run it again with --yes \
             {agreed} to end what it says now"
        ),
        None => format!("ended nothing; run it again with --yes {agreed} to end what it says"),
    }
}

fn failed(why: &str) -> ExitCode {
    let _ = writeln!(io::stderr(), "luneta: {why}");
    ExitCode::FAILURE
}

fn say(line: &str) {
    let _ = writeln!(io::stdout(), "{line}");
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Agents {
    count: usize,
    busy: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Here {
    running: Running,
    agents: Option<Agents>,
    projects: Result<Kept, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Kept {
    projects: usize,
    workstreams: usize,
}

impl Kept {
    fn of(projects: &Projects) -> Kept {
        Kept { projects: projects.all().len(), workstreams: projects.workstreams().count() }
    }
}

impl Here {
    fn now(folder: &Path) -> Here {
        let running = terminate::running(folder);
        let agents = (!running.lunetads.is_empty()).then(|| agents(folder)).flatten();
        let projects = projects::kept().map(|projects| Kept::of(&projects));
        Here { running, agents, projects }
    }

    fn key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        let fronts = self.running.lunetas.iter().filter(|it| front(&it.argv)).count();
        let counts = (fronts, self.running.lunetads.len(), self.running.muxes.len());
        let agents = self.agents.map(|agents| agents.count);
        (counts, agents, self.projects.as_ref().ok()).hash(&mut hasher);
        hasher.finish()
    }

    fn says(&self) -> String {
        let runs = runs(&self.running);
        let runs = if runs.is_empty() {
            "nothing of luneta runs".to_owned()
        } else {
            let agents = self.agents.filter(|agents| agents.count > 0).map(|agents| {
                let busy = (agents.busy > 0).then(|| format!(", {} busy", agents.busy));
                let busy = busy.unwrap_or_default();
                format!(", and {} with them{busy}", counted(agents.count, "agent", "agents"))
            });
            format!("ends {}{}", listed(&runs), agents.unwrap_or_default())
        };
        let kept = match &self.projects {
            Ok(Kept { projects: 0, .. }) => "keeps no project to forget".to_owned(),
            Ok(kept) => format!("forgets {}", kept_said(*kept)),
            Err(why) => format!("forgets its projects file, which does not read: {why}"),
        };
        format!("{runs}; {kept}")
    }
}

fn agents(folder: &Path) -> Option<Agents> {
    let listed = commands::listed_agents(folder).ok()?;
    let mut agents = Agents::default();
    for agent in &listed.agents {
        let in_mux = matches!(agent.pane, Some(agent::Pane::Terminal(_) | agent::Pane::Own(_)));
        match Doing::from(agent) {
            Doing::Shell => {},
            _ if !in_mux => {},
            doing => {
                agents.count += 1;
                agents.busy += usize::from(doing == Doing::Busy);
            },
        }
    }
    Some(agents)
}

fn front(argv: &[String]) -> bool {
    match argv.get(1) {
        None => true,
        Some(first) => {
            !first.starts_with('-') && crate::Cli::command().find_subcommand(first).is_none()
        },
    }
}

fn runs(running: &Running) -> Vec<String> {
    let fronts = running.lunetas.iter().filter(|it| front(&it.argv)).count();
    let commands = running.lunetas.len() - fronts;
    runs_of([fronts, commands, running.lunetads.len(), running.muxes.len()])
}

fn runs_of([fronts, commands, lunetads, muxes]: [usize; 4]) -> Vec<String> {
    let mut runs = Vec::new();
    if fronts > 0 {
        runs.push(counted(fronts, "luneta", "lunetas"));
    }
    if commands > 0 {
        runs.push(counted(commands, "luneta command", "luneta commands"));
    }
    match lunetads {
        0 => {},
        1 => runs.push("lunetad".to_owned()),
        n => runs.push(format!("{n} lunetads")),
    }
    match muxes {
        0 => {},
        1 => runs.push("luneta-mux".to_owned()),
        n => runs.push(format!("{n} luneta-muxes")),
    }
    runs
}

fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [before @ .., last] => format!("{} and {last}", before.join(", ")),
    }
}

fn kept_said(kept: Kept) -> String {
    let projects = counted(kept.projects, "project", "projects");
    match kept.workstreams {
        0 => projects,
        n => format!("{projects} and {}", counted(n, "workstream", "workstreams")),
    }
}

fn say_alone(folder: &Path) -> ExitCode {
    let here = Here::now(folder);
    say(&format!("{:016x}", here.key()));
    say(&here.says());
    ExitCode::SUCCESS
}

fn end_alone(folder: &Path) -> ExitCode {
    hold_on_through_hangups();
    let mut ended = Ended::before(folder);
    match ended.all(folder) {
        Ok(()) => {
            say(&ended.says());
            ExitCode::SUCCESS
        },
        Err(why) => failed(&why),
    }
}

fn hold_on_through_hangups() {
    let _ = signal_hook::flag::register(SIGHUP, Arc::new(AtomicBool::new(false)));
}

#[derive(Debug, PartialEq, Eq)]
enum Far {
    Ends { key: u64, says: String },
    Skipped(Skip),
}

#[derive(Debug, PartialEq, Eq)]
enum Skip {
    Unreached(String),
    Unsaid(String),
}

struct Planned {
    local: String,
    here: Here,
    remotes: Vec<(Destination, Far)>,
}

impl Planned {
    fn keys(&self) -> Vec<(String, Option<u64>)> {
        let here = (String::new(), Some(self.here.key()));
        let remotes = self.remotes.iter().map(|(destination, far)| {
            let key = match far {
                Far::Ends { key, .. } => Some(*key),
                Far::Skipped(_) => None,
            };
            (destination.to_string(), key)
        });
        std::iter::once(here).chain(remotes).collect()
    }

    fn lines(&self) -> Vec<String> {
        let here = format!("{} (this machine): {}", self.local, self.here.says());
        let remotes = self.remotes.iter().map(|(destination, far)| match far {
            Far::Ends { says, .. } => format!("{destination}: {says}"),
            Far::Skipped(skip) => format!("{destination}: {}", skipped_said(skip)),
        });
        std::iter::once(here).chain(remotes).collect()
    }
}

fn skipped_said(skip: &Skip) -> String {
    match skip {
        Skip::Unreached(why) => format!(
            "skipped, as ssh does not reach it, and so luneta runs on there and keeps its \
             projects: {why}"
        ),
        Skip::Unsaid(why) => format!(
            "skipped, as its luneta does not say what would end there, and so luneta runs on \
             there and keeps its projects: {why}"
        ),
    }
}

fn plan(destinations: &[Destination], folder: &Path) -> Planned {
    let (here, remotes) = thread::scope(|scope| {
        let asked: Vec<_> = destinations
            .iter()
            .map(|destination| scope.spawn(move || asked(destination)))
            .collect();
        let here = Here::now(folder);
        let remotes = destinations.iter().zip(asked).map(|(destination, asked)| {
            let panicked = || Far::Skipped(Skip::Unsaid("asking it panicked".to_owned()));
            let far = asked.join().unwrap_or_else(|_| panicked());
            (destination.clone(), far)
        });
        (here, remotes.collect())
    });
    Planned { local: machines::local_name(), here, remotes }
}

fn ssh(destination: &Destination, ending: bool) -> Command {
    let mut ssh = Command::new("ssh");
    let connect = format!("ConnectTimeout={CONNECT_SECS}");
    ssh.args(["-T", "-o", "BatchMode=yes", "-o", &connect]);
    ssh.arg(destination.to_string()).args(["luneta", "terminate", "--here"]);
    if ending {
        ssh.arg("--ending");
    }
    ssh
}

fn asked(destination: &Destination) -> Far {
    let ran = match program::run(&mut ssh(destination, false), ASKED) {
        Ok(ran) => ran,
        Err(error) => return Far::Skipped(Skip::Unreached(format!("ssh: {error}"))),
    };
    let said = match answer(ran, ASKED) {
        Ok(said) => said,
        Err(skip) => return Far::Skipped(skip),
    };
    planned_far(&said).unwrap_or_else(|| {
        let unread = "it said what would end in words this luneta does not read";
        Far::Skipped(Skip::Unsaid(unread.to_owned()))
    })
}

fn planned_far(said: &str) -> Option<Far> {
    let mut lines = said.lines().map(str::trim);
    let key = u64::from_str_radix(lines.next()?, 16).ok()?;
    let says = lines.next().filter(|says| !says.is_empty())?;
    Some(Far::Ends { key, says: says.to_owned() })
}

fn answer(ran: Ran, limit: Duration) -> Result<String, Skip> {
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        },
        Ran::Ended { status, stderr, .. } => {
            let stderr = String::from_utf8_lossy(&stderr);
            let last = stderr.lines().map(str::trim).rfind(|line| !line.is_empty());
            let why = last.map_or_else(|| status.to_string(), str::to_owned);
            let unreached = status.code() == Some(SSH_FAILED);
            Err(if unreached { Skip::Unreached(why) } else { Skip::Unsaid(why) })
        },
        Ran::Stuck => Err(Skip::Unreached(format!("it did not answer in {}s", limit.as_secs()))),
    }
}

fn terminated(planned: &Planned, folder: &Path) -> ExitCode {
    hold_on_through_hangups();
    let mut ended = Ended::before(folder);
    let first = ended.daemons(folder);
    let remotes: Vec<(&Destination, Result<String, String>)> = thread::scope(|scope| {
        let ending: Vec<_> = planned
            .remotes
            .iter()
            .map(|(destination, far)| {
                let ending = match far {
                    Far::Ends { .. } => Ok(scope.spawn(move || ended_far(destination))),
                    Far::Skipped(skip) => Err(skip),
                };
                (destination, ending)
            })
            .collect();
        ending
            .into_iter()
            .map(|(destination, ending)| {
                let came = match ending {
                    Err(skip) => Ok(format!("{destination}: {}", skipped_said(skip))),
                    Ok(ending) => ending
                        .join()
                        .unwrap_or_else(|_| Err("ending it panicked".to_owned()))
                        .map(|said| format!("{destination}: {said}")),
                };
                (destination, came)
            })
            .collect()
    });
    let here = first.and_then(|()| ended.all(folder));
    let mut well = true;
    let local = &planned.local;
    match here {
        Ok(()) => say(&format!("{local} (this machine): {}", ended.says())),
        Err(why) => {
            well = false;
            let _ = writeln!(io::stderr(), "luneta: {local} (this machine): {why}");
        },
    }
    for (destination, came) in remotes {
        match came {
            Ok(line) => say(&line),
            Err(why) => {
                well = false;
                let _ = writeln!(io::stderr(), "luneta: {destination}: {why}");
            },
        }
    }
    if well { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn ended_far(destination: &Destination) -> Result<String, String> {
    let ran = program::run(&mut ssh(destination, true), ENDING);
    let ran = ran.map_err(|error| format!("ssh: {error}"))?;
    match answer(ran, ENDING) {
        Ok(said) => {
            let said = said.lines().map(str::trim).find(|line| !line.is_empty());
            Ok(said.unwrap_or("ended, and said nothing of what").to_owned())
        },
        Err(Skip::Unreached(why)) => {
            Err(format!("ssh reaches it no more, and it may run on: {why}"))
        },
        Err(Skip::Unsaid(why)) => Err(why),
    }
}

struct Ended {
    lunetas: BTreeSet<u32>,
    fronts: BTreeSet<u32>,
    lunetads: BTreeSet<u32>,
    muxes: BTreeSet<u32>,
    agents: Option<Agents>,
    forgot: Option<Forgotten>,
}

impl Ended {
    fn before(folder: &Path) -> Ended {
        let lunetad_runs = !terminate::running(folder).lunetads.is_empty();
        let agents = lunetad_runs.then(|| agents(folder)).flatten();
        Ended {
            lunetas: BTreeSet::new(),
            fronts: BTreeSet::new(),
            lunetads: BTreeSet::new(),
            muxes: BTreeSet::new(),
            agents,
            forgot: None,
        }
    }

    fn daemons(&mut self, folder: &Path) -> Result<(), String> {
        let running = terminate::running(folder);
        let fronts = running.lunetas.iter().filter(|it| front(&it.argv)).map(|it| it.pid);
        self.fronts.extend(fronts);
        let pids = running.luneta_pids();
        ended_or(&pids, terminate::LUNETA)?;
        self.lunetas.extend(pids);
        let lunetads = terminate::running(folder).lunetads;
        ended_or(&lunetads, terminate::LUNETAD)?;
        self.lunetads.extend(lunetads);
        Ok(())
    }

    fn all(&mut self, folder: &Path) -> Result<(), String> {
        let mut unforgotten = None;
        for _ in 0..ROUNDS {
            if terminate::running(folder).is_empty() && self.forgot.is_some() {
                return Ok(());
            }
            self.daemons(folder)?;
            if self.forgot.is_none() {
                match projects::forget() {
                    Ok(forgot) => self.forgot = Some(forgot),
                    Err(why) => unforgotten = Some(why),
                }
            }
            let muxes = terminate::running(folder).muxes;
            ended_or(&muxes, terminate::MUX)?;
            self.muxes.extend(muxes);
        }
        let running = terminate::running(folder);
        if !running.is_empty() {
            let pids = running.luneta_pids().into_iter().map(|pid| (terminate::LUNETA, pid));
            let lunetads = running.lunetads.iter().map(|pid| (terminate::LUNETAD, *pid));
            let muxes = running.muxes.iter().map(|pid| (terminate::MUX, *pid));
            let left: Vec<String> = pids
                .chain(lunetads)
                .chain(muxes)
                .map(|(name, pid)| format!("{name} (pid {pid})"))
                .collect();
            return Err(format!(
                "{} started again as often as they were ended, {ROUNDS} times",
                listed(&left)
            ));
        }
        match (&self.forgot, unforgotten) {
            (Some(_), _) => Ok(()),
            (None, Some(why)) => Err(format!("ended luneta, and forgot no project: {why}")),
            (None, None) => unreachable!("each round forgets the projects or says why not"),
        }
    }

    fn says(&self) -> String {
        let commands = self.lunetas.len() - self.fronts.len();
        let runs = runs_of([self.fronts.len(), commands, self.lunetads.len(), self.muxes.len()]);
        let runs = if runs.is_empty() {
            "nothing of luneta ran".to_owned()
        } else {
            let agents = self.agents.filter(|agents| agents.count > 0 && !self.muxes.is_empty());
            let agents = agents.map(|agents| {
                format!(", and {} with them", counted(agents.count, "agent", "agents"))
            });
            format!("ended {}{}", listed(&runs), agents.unwrap_or_default())
        };
        let forgot = match &self.forgot {
            Some(Forgotten::Read(projects)) if projects.all().is_empty() => {
                "kept no project to forget".to_owned()
            },
            Some(Forgotten::Read(projects)) => format!("forgot {}", kept_said(Kept::of(projects))),
            Some(Forgotten::Unread(why)) => {
                format!("forgot its projects file, which did not read: {why}")
            },
            None => unreachable!("only what has ended and forgotten says so"),
        };
        format!("{runs}; {forgot}")
    }
}

fn ended_or(pids: &[u32], name: &str) -> Result<(), String> {
    let left = terminate::ended(pids);
    if left.is_empty() {
        return Ok(());
    }
    let left: Vec<String> = left.iter().map(|pid| format!("{name} (pid {pid})")).collect();
    Err(format!("{} still ran after a SIGKILL", listed(&left)))
}

#[cfg(test)]
mod tests {
    use lunetad::terminate::Luneta;

    use super::*;

    fn luneta(pid: u32, after: &[&str]) -> Luneta {
        let argv = std::iter::once("luneta").chain(after.iter().copied()).map(str::to_owned);
        Luneta { pid, argv: argv.collect() }
    }

    #[test]
    fn a_machine_says_what_ends_on_it_with_the_agents_in_its_terminals() {
        let running = Running {
            lunetas: vec![luneta(1, &[]), luneta(2, &["api"]), luneta(3, &["bridge"])],
            lunetads: vec![4],
            muxes: vec![5],
        };
        let agents = Some(Agents { count: 3, busy: 1 });
        let projects = Ok(Kept { projects: 4, workstreams: 9 });
        let here = Here { running, agents, projects };
        assert_eq!(
            here.says(),
            "ends 2 lunetas, 1 luneta command, lunetad and luneta-mux, and 3 agents with them, 1 \
             busy; forgets 4 projects and 9 workstreams"
        );

        let running = Running { muxes: vec![5, 6], ..Running::default() };
        let projects = Err("it is over 32 bytes".to_owned());
        let here = Here { running, agents: None, projects };
        assert_eq!(
            here.says(),
            "ends 2 luneta-muxes; forgets its projects file, which does not read: it is over 32 \
             bytes"
        );
    }

    #[test]
    fn a_plan_s_key_holds_whether_an_agent_is_busy_or_a_command_runs() {
        let lunetas = vec![luneta(1, &[])];
        let running = Running { lunetas, lunetads: vec![2], ..Running::default() };
        let projects = Ok(Kept { projects: 1, workstreams: 0 });
        let agents = Some(Agents { count: 1, busy: 0 });
        let idle = Here { running, agents, projects };
        let busy = Here { agents: Some(Agents { count: 1, busy: 1 }), ..idle.clone() };
        assert_eq!(idle.key(), busy.key());
        let mut commanded = idle.clone();
        commanded.running.lunetas.push(luneta(7, &["watch"]));
        assert_eq!(idle.key(), commanded.key());

        let mut fronted = idle.clone();
        fronted.running.lunetas.push(luneta(8, &[]));
        let more = Here { agents: Some(Agents { count: 2, busy: 0 }), ..idle.clone() };
        let kept = Here { projects: Ok(Kept { projects: 1, workstreams: 1 }), ..idle.clone() };
        for changed in [fronted, more, kept] {
            assert_ne!(idle.key(), changed.key(), "{}", changed.says());
        }
    }

    #[test]
    fn another_machine_s_plan_is_its_key_and_then_its_words() {
        let said = "00000000000000ff\nends lunetad; forgets 1 project\n";
        let far = Far::Ends { key: 255, says: "ends lunetad; forgets 1 project".to_owned() };
        assert_eq!(planned_far(said), Some(far));
        for unread in ["", "ends lunetad\n", "00ff\n\n", "zz\nends lunetad\n"] {
            assert_eq!(planned_far(unread), None, "{unread:?}");
        }
    }

    #[test]
    fn only_a_luneta_with_no_command_is_a_front_end() {
        assert!(front(&luneta(1, &[]).argv));
        assert!(front(&luneta(1, &["floresta"]).argv));
        for command in [&["bridge"][..], &["terminate", "--here"], &["--version"], &["worktree"]] {
            assert!(!front(&luneta(1, command).argv), "{command:?}");
        }
    }
}
