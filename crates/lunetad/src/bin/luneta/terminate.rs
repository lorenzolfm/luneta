use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher as _};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
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
use lunetad::terminate::{self, Folders, Id, Running, Unended};
use lunetad::{Destination, HostsParsed};
use signal_hook::consts::SIGHUP;

use crate::close::Doing;
use crate::commands;
use crate::agreed::{self, Agreed, PLANNED};
use crate::machines::{self, Machine};

const ROUNDS: usize = 3;

const CONNECT_SECS: u32 = 10;

const ASK_LIMIT: Duration = Duration::from_secs(30);

const ENDING_LIMIT: Duration = Duration::from_secs(120);

const SSH_FAILED: i32 = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(u64);

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

pub fn run(yes: Option<Agreed>, here: bool, ending: Option<Key>, folder: &Path) -> ExitCode {
    let folders = Folders::of(folder);
    if here {
        return match ending {
            Some(key) => end_alone(key, &folders),
            None => say_alone(&folders),
        };
    }
    let turn = match agreed::turn_here() {
        Ok(turn) => turn,
        Err(why) => return failed(&why),
    };
    let hosts = match machines::parsed() {
        Ok(hosts) => hosts,
        Err(why) => {
            return failed(&format!("ended nothing, as the hosts file does not read: {why}"));
        },
    };
    let planned = plan(hosts, &folders);
    let agreed_in = |turn| Agreed::of_terminate(&planned.keys(), &planned.unread, turn);
    let agreed = agreed_in(turn.map(Turn::key));
    let takes = match turn {
        None => Some(agreed),
        Some(turn) => turn.before().map(|before| agreed_in(Some(before))),
    };
    if yes.is_some() && yes == takes {
        return terminated(&planned, &folders);
    }
    for line in planned.lines() {
        say(&line);
    }
    let _ = writeln!(io::stderr(), "luneta: {}", unagreed(yes, agreed));
    ExitCode::from(PLANNED)
}

pub fn key(spelled: &str) -> Result<Key, String> {
    let read = u64::from_str_radix(spelled, 16);
    read.map(Key).map_err(|_| format!("{spelled} is no key luneta said"))
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
enum Counted {
    Unasked,
    Agents(Agents),
    Uncounted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Here {
    running: Running,
    agents: Counted,
    projects: Result<Kept, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Kept {
    projects: usize,
    worktrees: usize,
}

impl Kept {
    fn of(projects: &Projects) -> Kept {
        Kept { projects: projects.all().len(), worktrees: projects.worktrees().count() }
    }
}

impl Here {
    fn now(folders: &Folders) -> Here {
        let running = terminate::running(folders);
        let agents =
            if running.served.is_empty() { Counted::Unasked } else { agents_in(&running.served) };
        let projects = projects::kept().map(|projects| Kept::of(&projects));
        Here { running, agents, projects }
    }

    fn key(&self) -> Key {
        let mut hasher = DefaultHasher::new();
        let roles = Roles::of(self.running.lunetas.iter().map(|it| role(&it.argv)));
        let counts = (roles.fronts, self.running.lunetads.len(), self.running.muxes.len());
        let agents = match &self.agents {
            Counted::Unasked => None,
            Counted::Agents(agents) => Some(Some(agents.count)),
            Counted::Uncounted(_) => Some(None),
        };
        (counts, agents, self.projects.as_ref().ok()).hash(&mut hasher);
        Key(hasher.finish())
    }

    fn says(&self) -> String {
        let runs = runs(&self.running);
        let runs = if runs.is_empty() {
            "nothing of luneta runs".to_owned()
        } else {
            let agents = match &self.agents {
                Counted::Agents(agents) if agents.count > 0 => {
                    let busy = (agents.busy > 0).then(|| format!(", {} busy", agents.busy));
                    let busy = busy.unwrap_or_default();
                    format!(", and {} with them{busy}", counted(agents.count, "agent", "agents"))
                },
                Counted::Unasked if !self.running.muxes.is_empty() => {
                    ", and the agents in its terminals with them, which no lunetad runs to count"
                        .to_owned()
                },
                Counted::Unasked | Counted::Agents(_) => String::new(),
                Counted::Uncounted(why) => uncounted_said(why),
            };
            format!("ends {}{agents}", listed(&runs))
        };
        let kept = match &self.projects {
            Ok(Kept { projects: 0, .. }) => "keeps no project to forget".to_owned(),
            Ok(kept) => format!("forgets {}", kept_said(*kept)),
            Err(why) => format!("forgets its projects file, which does not read: {why}"),
        };
        format!("{runs}; {kept}")
    }
}

fn uncounted_said(why: &str) -> String {
    format!(", and the agents in its terminals with them, which lunetad did not count: {why}")
}

fn agents_in(served: &[PathBuf]) -> Counted {
    let mut agents = Agents::default();
    for folder in served {
        let listed = match commands::listed_agents(folder) {
            Ok(listed) => listed,
            Err(why) => return Counted::Uncounted(why),
        };
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
    }
    Counted::Agents(agents)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Front,
    Command,
}

fn role(argv: &[String]) -> Role {
    let Some(first) = argv.get(1) else {
        return Role::Front;
    };
    if first.starts_with('-') || crate::Cli::command().find_subcommand(first).is_some() {
        Role::Command
    } else {
        Role::Front
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Roles {
    fronts: usize,
    commands: usize,
}

impl Roles {
    fn of(roles: impl IntoIterator<Item = Role>) -> Roles {
        let mut counted = Roles::default();
        for role in roles {
            match role {
                Role::Front => counted.fronts += 1,
                Role::Command => counted.commands += 1,
            }
        }
        counted
    }
}

fn runs(running: &Running) -> Vec<String> {
    let roles = Roles::of(running.lunetas.iter().map(|it| role(&it.argv)));
    runs_of(roles, running.lunetads.len(), running.muxes.len())
}

fn runs_of(roles: Roles, lunetads: usize, muxes: usize) -> Vec<String> {
    let mut runs = Vec::new();
    if roles.fronts > 0 {
        runs.push(counted(roles.fronts, "luneta", "lunetas"));
    }
    if roles.commands > 0 {
        runs.push(counted(roles.commands, "luneta command", "luneta commands"));
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
    match kept.worktrees {
        0 => projects,
        n => format!("{projects} and {}", counted(n, "worktree", "worktrees")),
    }
}

fn say_alone(folders: &Folders) -> ExitCode {
    let here = Here::now(folders);
    say(&here.key().to_string());
    say(&here.says());
    ExitCode::SUCCESS
}

fn end_alone(key: Key, folders: &Folders) -> ExitCode {
    let in_luneta = match agreed::turn_here() {
        Ok(turn) => turn.is_some() || lunetad::mux::own::terminal_here().is_some(),
        Err(why) => return failed(&format!("ended nothing, as {why}")),
    };
    if in_luneta {
        return failed(
            "ended nothing, as `--here --ending` is for the luneta that asks it through ssh, and \
             this runs in a terminal of luneta's: run `luneta terminate` to say the plan",
        );
    }
    let here = Here::now(folders);
    if here.key() != key {
        let says = here.says();
        return failed(&format!("ended nothing, as what would end here changed since: {says}"));
    }
    hold_on_through_hangups();
    match Ending::default().all(here.agents, folders) {
        Ok(ended) => {
            say(&ended.says());
            ExitCode::SUCCESS
        },
        Err(why) => failed(&why),
    }
}

fn hold_on_through_hangups() {
    if let Err(error) = signal_hook::flag::register(SIGHUP, Arc::new(AtomicBool::new(false))) {
        let _ = writeln!(
            io::stderr(),
            "luneta: a hangup may cut this short, as it could not be held off: {error}"
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Far {
    Ends { key: Key, says: String },
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
    unread: Vec<String>,
}

impl Planned {
    fn keys(&self) -> Vec<(Machine, Option<Key>)> {
        let here = (Machine::Local, Some(self.here.key()));
        let remotes = self.remotes.iter().map(|(destination, far)| {
            let key = match far {
                Far::Ends { key, .. } => Some(*key),
                Far::Skipped(_) => None,
            };
            (Machine::Remote(destination.clone()), key)
        });
        std::iter::once(here).chain(remotes).collect()
    }

    fn lines(&self) -> Vec<String> {
        let here = format!("{} (this machine): {}", self.local, self.here.says());
        let remotes = self.remotes.iter().map(|(destination, far)| match far {
            Far::Ends { says, .. } => format!("{destination}: {says}"),
            Far::Skipped(skip) => format!("{destination}: {}", skipped_said(skip)),
        });
        let unread = self.unread.iter().map(|why| unread_said(why));
        std::iter::once(here).chain(remotes).chain(unread).collect()
    }
}

fn unread_said(why: &str) -> String {
    format!(
        "hosts file: skipped a line, as it names no machine ssh could be asked of, and so any \
         luneta it meant runs on there and keeps its projects: {why}"
    )
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

fn plan(hosts: HostsParsed, folders: &Folders) -> Planned {
    let HostsParsed { machines: destinations, unread } = hosts;
    let (here, remotes) = thread::scope(|scope| {
        let asked: Vec<_> = destinations
            .iter()
            .map(|destination| scope.spawn(move || asked(destination)))
            .collect();
        let here = Here::now(folders);
        let remotes = destinations.iter().zip(asked).map(|(destination, asked)| {
            let panicked = || Far::Skipped(Skip::Unsaid("asking it panicked".to_owned()));
            let far = asked.join().unwrap_or_else(|_| panicked());
            (destination.clone(), far)
        });
        (here, remotes.collect())
    });
    Planned { local: machines::local_name(), here, remotes, unread }
}

fn ssh(destination: &Destination, ending: Option<Key>) -> Command {
    let mut ssh = Command::new("ssh");
    let connect = format!("ConnectTimeout={CONNECT_SECS}");
    ssh.args(["-T", "-o", "BatchMode=yes", "-o", &connect]);
    ssh.arg(destination.to_string()).args(["luneta", "terminate", "--here"]);
    if let Some(key) = ending {
        ssh.args(["--ending", &key.to_string()]);
    }
    ssh
}

fn asked(destination: &Destination) -> Far {
    let ran = match program::run(&mut ssh(destination, None), ASK_LIMIT) {
        Ok(ran) => ran,
        Err(error) => return Far::Skipped(Skip::Unreached(format!("ssh: {error}"))),
    };
    let said = match answer(ran, ASK_LIMIT) {
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
    let key = key(lines.next()?).ok()?;
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

fn terminated(planned: &Planned, folders: &Folders) -> ExitCode {
    hold_on_through_hangups();
    let mut ending = Ending::default();
    let first = ending.daemons(folders);
    let remotes: Vec<(&Destination, Result<String, String>)> = thread::scope(|scope| {
        let endings: Vec<_> = planned
            .remotes
            .iter()
            .map(|(destination, far)| {
                let ending = match far {
                    Far::Ends { key, .. } => {
                        let key = *key;
                        Ok(scope.spawn(move || ended_far(destination, key)))
                    },
                    Far::Skipped(skip) => Err(skip),
                };
                (destination, ending)
            })
            .collect();
        endings
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
    let here = first.and_then(|()| ending.all(planned.here.agents.clone(), folders));
    let mut well = true;
    let local = &planned.local;
    match here {
        Ok(ended) => say(&format!("{local} (this machine): {}", ended.says())),
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
    for why in &planned.unread {
        say(&unread_said(why));
    }
    if well { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn ended_far(destination: &Destination, key: Key) -> Result<String, String> {
    let ran = program::run(&mut ssh(destination, Some(key)), ENDING_LIMIT);
    let ran = ran.map_err(|error| format!("ssh: {error}"))?;
    match answer(ran, ENDING_LIMIT) {
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

#[derive(Default)]
struct Ending {
    lunetas: BTreeMap<Id, Role>,
    lunetads: BTreeSet<Id>,
    muxes: BTreeSet<Id>,
}

struct Ended {
    ending: Ending,
    agents: Counted,
    forgot: Forgotten,
}

impl Ending {
    fn daemons(&mut self, folders: &Folders) -> Result<(), String> {
        let running = terminate::running(folders);
        end_or(&running.luneta_ids(), terminate::LUNETA)?;
        self.lunetas.extend(running.lunetas.into_iter().map(|it| (it.id, role(&it.argv))));
        let lunetads = terminate::running(folders).lunetads;
        end_or(&lunetads, terminate::LUNETAD)?;
        self.lunetads.extend(lunetads);
        Ok(())
    }

    fn round(&mut self, folders: &Folders) -> Result<Result<Forgotten, String>, String> {
        self.daemons(folders)?;
        let forgot = projects::forget();
        let muxes = terminate::running(folders).muxes;
        end_or(&muxes, terminate::MUX)?;
        self.muxes.extend(muxes);
        Ok(forgot)
    }

    fn all(mut self, agents: Counted, folders: &Folders) -> Result<Ended, String> {
        let mut forgot = self.round(folders)?;
        for _ in 1..ROUNDS {
            if terminate::running(folders).is_empty() {
                break;
            }
            let later = self.round(folders)?;
            forgot = match forgot {
                Ok(first) => later.map(|_| first),
                Err(_) => later,
            };
        }
        let running = terminate::running(folders);
        if !running.is_empty() {
            let of_luneta = running.luneta_ids().into_iter().map(|id| (terminate::LUNETA, id));
            let of_lunetad = running.lunetads.into_iter().map(|id| (terminate::LUNETAD, id));
            let of_mux = running.muxes.into_iter().map(|id| (terminate::MUX, id));
            let left: Vec<String> = of_luneta
                .chain(of_lunetad)
                .chain(of_mux)
                .map(|(name, id)| format!("{name} (pid {})", id.pid))
                .collect();
            return Err(format!(
                "{} started again as often as they were ended, {ROUNDS} times",
                listed(&left)
            ));
        }
        let forgot = forgot.map_err(|why| format!("ended luneta, and forgot no project: {why}"))?;
        Ok(Ended { ending: self, agents, forgot })
    }
}

impl Ended {
    fn says(&self) -> String {
        let Ending { lunetas, lunetads, muxes } = &self.ending;
        let roles = Roles::of(lunetas.values().copied());
        let runs = runs_of(roles, lunetads.len(), muxes.len());
        let runs = if runs.is_empty() {
            "nothing of luneta ran".to_owned()
        } else {
            let agents = match &self.agents {
                _ if muxes.is_empty() => String::new(),
                Counted::Agents(agents) if agents.count > 0 => {
                    format!(", and {} with them", counted(agents.count, "agent", "agents"))
                },
                Counted::Unasked => {
                    ", and the agents in its terminals with them, which no lunetad ran to count"
                        .to_owned()
                },
                Counted::Agents(_) => String::new(),
                Counted::Uncounted(why) => uncounted_said(why),
            };
            format!("ended {}{agents}", listed(&runs))
        };
        let forgot = match &self.forgot {
            Forgotten::Read(projects) if projects.all().is_empty() => {
                "kept no project to forget".to_owned()
            },
            Forgotten::Read(projects) => format!("forgot {}", kept_said(Kept::of(projects))),
            Forgotten::Unread(why) => {
                format!("forgot its projects file, which did not read: {why}")
            },
        };
        format!("{runs}; {forgot}")
    }
}

fn end_or(ids: &[Id], name: &str) -> Result<(), String> {
    let left = terminate::end(ids);
    if left.is_empty() {
        return Ok(());
    }
    let left: Vec<String> = left.iter().map(|(id, why)| left_said(name, id, *why)).collect();
    Err(listed(&left))
}

fn left_said(name: &str, id: &Id, why: Unended) -> String {
    let pid = id.pid;
    match why {
        Unended::Outlived => format!("{name} (pid {pid}) still ran after a SIGKILL"),
        Unended::Unsignalled(error) => {
            format!("{name} (pid {pid}) took no signal to end: {}", io::Error::from(error))
        },
        Unended::Unwatched(error) => format!(
            "{name} (pid {pid}) was told to end, and its end could not be watched for: {}",
            io::Error::from(error)
        ),
    }
}

#[cfg(test)]
mod tests {
    use lunetad::terminate::Luneta;

    use super::*;

    fn luneta(pid: u32, after: &[&str]) -> Luneta {
        let argv = std::iter::once("luneta").chain(after.iter().copied()).map(str::to_owned);
        Luneta { id: id(pid), argv: argv.collect() }
    }

    fn id(pid: u32) -> Id {
        Id { pid, start: "1".to_owned() }
    }

    #[test]
    fn a_machine_says_what_ends_on_it_with_the_agents_in_its_terminals() {
        let running = Running {
            lunetas: vec![luneta(1, &[]), luneta(2, &["api"]), luneta(3, &["bridge"])],
            lunetads: vec![id(4)],
            muxes: vec![id(5)],
            served: vec![PathBuf::from("/run/user/1000/luneta")],
        };
        let agents = Counted::Agents(Agents { count: 3, busy: 1 });
        let projects = Ok(Kept { projects: 4, worktrees: 9 });
        let here = Here { running: running.clone(), agents, projects };
        assert_eq!(
            here.says(),
            "ends 2 lunetas, 1 luneta command, lunetad and luneta-mux, and 3 agents with them, 1 \
             busy; forgets 4 projects and 9 worktrees"
        );

        let agents = Counted::Uncounted("lunetad did not answer".to_owned());
        let projects = Ok(Kept { projects: 1, worktrees: 0 });
        let here = Here { running, agents, projects };
        assert_eq!(
            here.says(),
            "ends 2 lunetas, 1 luneta command, lunetad and luneta-mux, and the agents in its \
             terminals with them, which lunetad did not count: lunetad did not answer; forgets 1 \
             project"
        );

        let running = Running { muxes: vec![id(5), id(6)], ..Running::default() };
        let projects = Err("it is over 32 bytes".to_owned());
        let here = Here { running, agents: Counted::Unasked, projects };
        assert_eq!(
            here.says(),
            "ends 2 luneta-muxes, and the agents in its terminals with them, which no lunetad runs \
             to count; forgets its projects file, which does not read: it is over 32 bytes"
        );

        let running = Running { lunetas: vec![luneta(1, &[])], ..Running::default() };
        let projects = Ok(Kept { projects: 0, worktrees: 0 });
        let here = Here { running, agents: Counted::Unasked, projects };
        assert_eq!(here.says(), "ends 1 luneta; keeps no project to forget");
    }

    #[test]
    fn a_plan_s_key_holds_whether_an_agent_is_busy_or_a_command_runs() {
        let lunetas = vec![luneta(1, &[])];
        let running = Running { lunetas, lunetads: vec![id(2)], ..Running::default() };
        let projects = Ok(Kept { projects: 1, worktrees: 0 });
        let agents = Counted::Agents(Agents { count: 1, busy: 0 });
        let idle = Here { running, agents, projects };
        let busy = Here { agents: Counted::Agents(Agents { count: 1, busy: 1 }), ..idle.clone() };
        assert_eq!(idle.key(), busy.key());
        let mut commanded = idle.clone();
        commanded.running.lunetas.push(luneta(7, &["watch"]));
        assert_eq!(idle.key(), commanded.key());

        let mut fronted = idle.clone();
        fronted.running.lunetas.push(luneta(8, &[]));
        let more = Here { agents: Counted::Agents(Agents { count: 2, busy: 0 }), ..idle.clone() };
        let kept = Here { projects: Ok(Kept { projects: 1, worktrees: 1 }), ..idle.clone() };
        let uncounted = Here { agents: Counted::Uncounted("no".to_owned()), ..idle.clone() };
        let unasked = Here { agents: Counted::Unasked, ..idle.clone() };
        for changed in [fronted, more, kept, uncounted, unasked] {
            assert_ne!(idle.key(), changed.key(), "{}", changed.says());
        }
    }

    #[test]
    fn another_machine_s_plan_is_its_key_and_then_its_words() {
        let said = "00000000000000ff\nends lunetad; forgets 1 project\n";
        let far = Far::Ends { key: Key(255), says: "ends lunetad; forgets 1 project".to_owned() };
        assert_eq!(planned_far(said), Some(far));
        for unread in ["", "ends lunetad\n", "00ff\n\n", "zz\nends lunetad\n"] {
            assert_eq!(planned_far(unread), None, "{unread:?}");
        }
    }

    #[test]
    fn only_a_luneta_with_no_command_is_a_front_end() {
        assert_eq!(role(&luneta(1, &[]).argv), Role::Front);
        assert_eq!(role(&luneta(1, &["floresta"]).argv), Role::Front);
        for command in [&["bridge"][..], &["terminate", "--here"], &["--version"], &["worktree"]] {
            assert_eq!(role(&luneta(1, command).argv), Role::Command, "{command:?}");
        }
    }

    #[test]
    fn what_ended_says_each_luneta_once_whether_a_front_end_or_a_command() {
        let mut ending = Ending::default();
        ending.lunetas.extend([(id(1), Role::Front), (id(2), Role::Command)]);
        ending.lunetas.insert(id(1), Role::Front);
        ending.lunetads.insert(id(3));
        ending.muxes.insert(id(4));
        let agents = Counted::Agents(Agents { count: 2, busy: 0 });
        let ended = Ended { ending, agents, forgot: Forgotten::Read(Projects::default()) };
        assert_eq!(
            ended.says(),
            "ended 1 luneta, 1 luneta command, lunetad and luneta-mux, and 2 agents with them; \
             kept no project to forget"
        );
    }

    #[test]
    fn what_ended_says_the_agents_of_a_mux_no_lunetad_counted() {
        let mut ending = Ending::default();
        ending.muxes.insert(id(4));
        let forgot = Forgotten::Read(Projects::default());
        let ended = Ended { ending, agents: Counted::Unasked, forgot };
        assert_eq!(
            ended.says(),
            "ended luneta-mux, and the agents in its terminals with them, which no lunetad ran to \
             count; kept no project to forget"
        );
    }
}
