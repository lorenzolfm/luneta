//! Where a bare `luneta` starts: in a session that runs, and in one it makes
//! only where none runs anywhere. You make a session from inside luneta, or
//! by naming it, as `luneta <name>` does. It starts in the first of these
//! that exists:
//!
//! 1. A session of this machine's. The one named for the directory, where it
//!    runs: that is where you mean to go, as you `cd` to a project. Or else
//!    the one you looked at last, in any luneta, as the news keeps it, or the
//!    newest of those it has no word of.
//! 2. A session of another machine's, as `App::newest_away` goes on in one: of
//!    the first machine by the sidebar's headers that lunetad reaches, its
//!    newest that answers, going on to the next where a client fails, and
//!    ending where every one does, for you to name a session, as sessions run.
//!    The news goes unread here: it dates a pane of another machine's by either
//!    machine's clock, which may not compare. luneta has to hear what runs on
//!    them first, as `Start::Hear` says.
//! 3. The session named for the directory, made, as `zellij::client` has it.
//!
//! A session that ended counts for none of these: none runs, though zellij
//! keeps it to bring back, as `attach --create` does in 3 by its name. A client
//! of 1 or 2 joins without `--create`, as `zellij::rejoin` says, so that a
//! session gone since luneta looked is not made again, empty.
//!
//! luneta never joins what may hold its own terminal, as
//! `zellij::may_hold_terminal` and `Sidebar::refuges_away` say. Where that
//! leaves nothing though sessions run, it makes none: it says so, and ends.

use std::time::{Duration, SystemTime};

use std::path::Path;

use lunetad::mux::own::OwnKey;
use lunetad::{log, Destination, Directory};

use crate::away::ANSWER;
use crate::ledger::Session;
use crate::machines;
use crate::news::News;
use crate::own_sessions;
use crate::sidebar::{NoRefuge, Sidebar};
use crate::zellij::{self, Refuge, SessionName};

/// What luneta's first client is.
pub enum First {
    /// One of this session of this machine's, which answered.
    Here(SessionName),
    /// One through ssh of this session of that machine's, which answered.
    Away(Destination, Refuge),
    /// One attached to this session, made if none runs by its name, or with
    /// none named in a session of a name zellij makes up.
    Make(Option<SessionName>),
    /// luneta's own client of this own session.
    Own(OwnKey),
}

/// How luneta comes to its first client.
pub enum Start {
    /// As it knows it already.
    Now(First),
    /// Once it has heard what runs on `machines`, which the hosts file names,
    /// as no session here is left to join: zellij has nothing to show until
    /// then, which may be as long as lunetad takes to start and reach them.
    /// It makes the session `named` only where they have none.
    Hear { machines: Vec<Destination>, named: Option<SessionName> },
}

/// A session of this machine's that runs, as a bare luneta chooses among them.
struct Running {
    name: SessionName,
    started: SystemTime,
    /// When you were last in a pane of it, if you were.
    seen: Option<SystemTime>,
}

/// What a bare luneta finds on this machine.
enum Here {
    /// The session to join, of those that answer.
    Join(SessionName),
    /// None to join, and those that run that may hold luneta's terminal.
    Nothing { holding: Vec<SessionName> },
}

/// Where a `luneta <name>` starts, or why it does not: naming a session is
/// asking for it, made if none runs.
pub fn named(session: SessionName) -> Result<Start, String> {
    make(Some(session)).map(Start::Now)
}

/// Where a `luneta --own` starts, in `dir`, or why it does not: in the own
/// session `named`, made in `dir` with your shell where none runs by the
/// name. Never one a session of zellij's has the name of, nor the one luneta
/// runs in.
pub fn own(named: &SessionName, dir: &Path) -> Result<Start, String> {
    let running = zellij::running()?;
    if running.iter().any(|(name, _)| name == named) {
        return Err(format!("{named} is a session of zellij's: name another for an own one"));
    }
    let session = if let Some(session) = own_sessions::named(named)? {
        session
    } else {
        let unread = || format!("{} is no directory an own session starts in", dir.display());
        let cwd = dir.to_str().and_then(|dir| Directory::parse(dir.to_owned()).ok());
        let cwd = cwd.ok_or_else(unread)?;
        own_sessions::made(named, &cwd).map_err(|why| format!("no own session made: {why}"))?
    };
    if let Some(refusal) = own_sessions::refusal(session, named) {
        return Err(refusal);
    }
    Ok(Start::Now(First::Own(session)))
}

/// Where a bare `luneta` starts, whose directory's session is `named`, or why
/// it does not. It goes by the rule of old, for the session `named`, where not
/// all of zellij's folder reads, which could hold a session that runs.
pub fn bare(named: Option<SessionName>, news: &News) -> Result<Start, String> {
    let holding = match here(named.as_ref(), news) {
        None => return make(named).map(Start::Now),
        Some(Here::Join(session)) => return Ok(Start::Now(First::Here(session))),
        Some(Here::Nothing { holding }) => holding,
    };
    let machines = machines::named().unwrap_or_else(|why| {
        // On the terminal as luneta left it, where it stays once luneta ends.
        eprintln!("luneta: looking on no other machine: {why}");
        Vec::new()
    });
    if !machines.is_empty() {
        return Ok(Start::Hear { machines, named });
    }
    left(named, &holding, &[]).map(Start::Now)
}

/// Where a bare `luneta` starts once it has heard all it will of the other
/// machines, as the sidebar has it, or why it does not. A session may have
/// come to this machine as it waited.
pub fn heard(named: Option<SessionName>, sidebar: &Sidebar) -> Result<First, String> {
    let holding = match here(named.as_ref(), sidebar.news()) {
        None => return make(named),
        Some(Here::Join(session)) => return Ok(First::Here(session)),
        Some(Here::Nothing { holding }) => holding,
    };
    // A machine slow to answer has a session luneta would make one for
    // otherwise, which is worth more than a moment.
    match away(sidebar, &[], ANSWER) {
        Sought::Found(destination, refuge) => Ok(First::Away(destination, refuge)),
        Sought::Nowhere { held } => left(named, &holding, &held),
    }
}

/// What luneta found of the other machines' sessions.
pub enum Sought {
    /// The first machine with a session to go to, and that session.
    Found(Destination, Refuge),
    /// No session to go to, and the machines it looked on each of whose
    /// sessions may hold its own terminal.
    Nowhere { held: Vec<Destination> },
}

/// The newest session that answers within `limit` of the first of the other
/// machines but those `tried`, by the sidebar's headers, that lunetad reaches.
/// It looks no further where it does not reach lunetad to ask; the log says why
/// it passed each machine over.
pub fn away(sidebar: &Sidebar, tried: &[Destination], limit: Duration) -> Sought {
    let mut held = Vec::new();
    for (destination, refuges) in sidebar.refuges_away(tried, zellij::within()) {
        let sessions = match refuges {
            Ok(sessions) => sessions,
            Err(why) => {
                log::say(&format!("{destination}: no session to go on in: {why}"));
                if why == NoRefuge::Held {
                    held.push(destination);
                }
                continue;
            },
        };
        match zellij::answering_away(&destination, sessions, limit) {
            Ok(Some(refuge)) => return Sought::Found(destination, refuge),
            Ok(None) => log::say(&format!("{destination}: no session to go on in answers")),
            Err(why) => {
                log::say(&format!("no session of another machine's to go on in: {why}"));
                break;
            },
        }
    }
    Sought::Nowhere { held }
}

/// What runs on this machine, as a bare luneta whose directory's session is
/// `named` goes by it, with what `news` has of each. None where not all of
/// zellij's folder reads.
fn here(named: Option<&SessionName>, news: &News) -> Option<Here> {
    let running = zellij::settled()?.into_iter();
    let (holding, free): (Vec<_>, Vec<_>) =
        running.partition(|(name, _)| zellij::may_hold_terminal(name));
    let free = free.into_iter().map(|(name, id)| Running {
        name,
        started: id.started(),
        seen: Session::of(id).and_then(|session| news.last_seen(session)),
    });
    Some(match zellij::answering(joinable(free.collect(), named)) {
        Some(Refuge { session, .. }) => Here::Join(session),
        None => Here::Nothing { holding: holding.into_iter().map(|(name, _)| name).collect() },
    })
}

/// The sessions of `running` a bare luneta can join, the likeliest first: the
/// one `named` for its directory; then those you were last in; then those you
/// have not been in, the newest first, and of two that started at once the one
/// whose name comes last, as the picker has them.
fn joinable(mut running: Vec<Running>, named: Option<&SessionName>) -> Vec<SessionName> {
    let is_named = |session: &Running| named == Some(&session.name);
    running.sort_by(|a, b| {
        (is_named(b), b.seen, b.started, &b.name).cmp(&(is_named(a), a.seen, a.started, &a.name))
    });
    running.into_iter().map(|session| session.name).collect()
}

/// luneta has no session to join, whose directory's session is `named`. It
/// makes that one, unless sessions run that it will not join, which it
/// refuses for, as `refusal_to_make` says.
fn left(
    named: Option<SessionName>,
    holding: &[SessionName],
    held: &[Destination],
) -> Result<First, String> {
    match refusal_to_make(holding, held) {
        Some(refusal) => Err(refusal),
        None => make(named),
    }
}

/// A client attached to the session `named`, made if none runs, or why it
/// must not be, as `zellij::refusal` says.
fn make(named: Option<SessionName>) -> Result<First, String> {
    match named.as_ref().and_then(zellij::refusal) {
        Some(refusal) => Err(refusal),
        None => Ok(First::Make(named)),
    }
}

/// Why a bare luneta makes no session, where it left none to join but
/// `holding`, those here its terminal may be a pane of, and every session of
/// the machines `held`, which may hold it: sessions run, and you did not ask
/// for another by naming it.
fn refusal_to_make(holding: &[SessionName], held: &[Destination]) -> Option<String> {
    let mut holders = Vec::new();
    if !holding.is_empty() {
        let sessions: Vec<String> = holding.iter().map(|name| format!("\"{name}\"")).collect();
        holders.push(format!("this terminal is a pane of {}", sessions.join(" or ")));
    }
    if !held.is_empty() {
        let machines: Vec<String> = held.iter().map(ToString::to_string).collect();
        let machines = machines.join(", ");
        holders.push(format!("each session of {machines} may hold this terminal in a pane"));
    }
    (!holders.is_empty()).then(|| {
        let holders = holders.join(", and ");
        format!(
            "{holders}, which zellij would show inside itself: name another session, or run \
             luneta outside zellij"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn running(name: &str, started: u64, seen: Option<u64>) -> Running {
        Running { name: self::name(name), started: at(started), seen: seen.map(at) }
    }

    fn name(name: &str) -> SessionName {
        SessionName::running(name).unwrap()
    }

    fn order(sessions: Vec<Running>, named: Option<&str>) -> Vec<String> {
        let named = named.map(name);
        let joinable = joinable(sessions, named.as_ref());
        joinable.iter().map(ToString::to_string).collect()
    }

    /// A bare luneta tries the session named for its directory first, then
    /// the one you were in last, however old, then those you have not been
    /// in, the newest first: being in one counts for more than its age.
    #[test]
    fn a_bare_luneta_tries_its_directory_s_then_the_last_seen_then_the_newest() {
        let sessions = || {
            vec![
                running("newest", 40, None),
                running("older", 30, None),
                running("seen-long-ago", 10, Some(100)),
                running("seen-last", 20, Some(200)),
                running("project", 5, None),
            ]
        };
        let by_news = ["seen-last", "seen-long-ago", "newest", "older", "project"];
        assert_eq!(order(sessions(), None), by_news);
        assert_eq!(order(sessions(), Some("no-such-session")), by_news);
        let in_project = ["project", "seen-last", "seen-long-ago", "newest", "older"];
        assert_eq!(order(sessions(), Some("project")), in_project);
    }

    /// Of two that started at once, the one whose name comes last goes first,
    /// as the picker has them.
    #[test]
    fn of_two_alike_the_one_whose_name_comes_last_goes_first() {
        let sessions = vec![running("a", 2, None), running("b", 2, None)];
        assert_eq!(order(sessions, None), ["b", "a"]);
    }

    /// With nothing left to join, luneta refuses where what it passed over
    /// runs, which says what it found, and otherwise has nothing to refuse:
    /// it makes one. The session's words are those of the refusal of old.
    #[test]
    fn luneta_refuses_to_make_a_session_only_where_it_passed_sessions_over() {
        let work = [name("work")];
        assert_eq!(refusal_to_make(&[], &[]), None);
        assert_eq!(
            refusal_to_make(&work, &[]).unwrap(),
            "this terminal is a pane of \"work\", which zellij would show inside itself: name \
             another session, or run luneta outside zellij"
        );
        let held: [Destination; 2] = ["pi".parse().unwrap(), "me@vps".parse().unwrap()];
        assert_eq!(
            refusal_to_make(&[], &held).unwrap(),
            "each session of pi, me@vps may hold this terminal in a pane, which zellij would show \
             inside itself: name another session, or run luneta outside zellij"
        );
        let two = [name("work"), name("play")];
        let two = refusal_to_make(&two, &[]).unwrap();
        assert!(two.starts_with("this terminal is a pane of \"work\" or \"play\", which"), "{two}");
        let both = refusal_to_make(&work, &held[..1]).unwrap();
        assert!(
            both.starts_with(
                "this terminal is a pane of \"work\", and each session of pi may hold this \
                 terminal in a pane, which"
            ),
            "{both}"
        );
    }
}
