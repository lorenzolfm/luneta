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
//!    newest that answers, going on to the next where a client fails. The news
//!    goes unread here: it dates a pane of another machine's by either
//!    machine's clock, which may not compare. luneta has to hear what runs on
//!    them first, as `Start::Hear` says.
//! 3. The session named for the directory, made, as `zellij::client` has it.
//!
//! A session that ended counts for none of these: none runs, though zellij
//! keeps it to bring back, as `attach --create` does in 3 by its name. A client
//! of 1 or 2 joins without `--create`, as `zellij::rejoin` says, so that a
//! session gone since luneta looked is not made again, empty.
//!
//! luneta never joins what may hold its own terminal, as `zellij::refusal` and
//! `Sidebar::refuges_away` say. Where that leaves nothing though sessions run,
//! it makes none: it says so, and ends.

use std::time::{Duration, SystemTime};

use lunetad::log;
use lunetad::Destination;

use crate::away::ANSWER;
use crate::machines;
use crate::news::News;
use crate::sidebar::{NoRefuge, Sidebar};
use crate::zellij::{self, Refuge, SessionName};

/// What luneta's first client is.
pub enum First {
    /// One of this session of this machine's, which answered.
    Here(Refuge),
    /// One through ssh of this session of that machine's, which answered.
    Away(Destination, Refuge),
    /// One attached to this session, made if none runs by its name, or with
    /// none named in a session of a name zellij makes up.
    Make(Option<SessionName>),
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
    name: String,
    started: SystemTime,
    /// When you were last in a pane of it, if you were.
    seen: Option<SystemTime>,
}

/// What a bare luneta finds on this machine.
struct Here {
    /// The session to join, of those that answer.
    refuge: Option<Refuge>,
    /// The session luneta's terminal is a pane of, where it runs.
    holding: Option<String>,
}

/// Where a `luneta <name>` starts, or why it does not: naming a session is
/// asking for it, made if none runs.
pub fn named(session: SessionName) -> Result<Start, String> {
    make(Some(session)).map(Start::Now)
}

/// Where a bare `luneta` starts, whose directory's session is `named`, or why
/// it does not. It goes by the rule of old, for the session `named`, where not
/// all of zellij's folder reads, which could hold a session that runs.
pub fn bare(named: Option<SessionName>, news: &News) -> Result<Start, String> {
    let Some(here) = here(named.as_ref(), news) else {
        return make(named).map(Start::Now);
    };
    if let Some(refuge) = here.refuge {
        return Ok(Start::Now(First::Here(refuge)));
    }
    let machines = machines::named();
    if !machines.is_empty() {
        return Ok(Start::Hear { machines, named });
    }
    left(named, here.holding.as_deref(), &[]).map(Start::Now)
}

/// Where a bare `luneta` starts once it has heard all it will of the other
/// machines, as the sidebar has it, or why it does not. A session may have
/// come to this machine as it waited.
pub fn heard(named: Option<SessionName>, sidebar: &Sidebar) -> Result<First, String> {
    let Some(here) = here(named.as_ref(), sidebar.news()) else {
        return make(named);
    };
    if let Some(refuge) = here.refuge {
        return Ok(First::Here(refuge));
    }
    // A machine slow to answer has a session luneta would make one for
    // otherwise, which is worth more than a moment.
    let Sought { found, held } = away(sidebar, &[], ANSWER);
    if let Some((destination, refuge)) = found {
        return Ok(First::Away(destination, refuge));
    }
    left(named, here.holding.as_deref(), &held)
}

/// What luneta found of the other machines' sessions.
pub struct Sought {
    /// The first machine with a session to go to, and that session.
    pub found: Option<(Destination, Refuge)>,
    /// Those it passed over before it, each of whose sessions may hold its
    /// own terminal.
    pub held: Vec<Destination>,
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
            Ok(Some(refuge)) => return Sought { found: Some((destination, refuge)), held },
            Ok(None) => log::say(&format!("{destination}: no session to go on in answers")),
            Err(why) => {
                log::say(&format!("no session of another machine's to go on in: {why}"));
                break;
            },
        }
    }
    Sought { found: None, held }
}

/// What runs on this machine, as a bare luneta whose directory's session is
/// `named` goes by it, with what `news` has of each. None where not all of
/// zellij's folder reads.
fn here(named: Option<&SessionName>, news: &News) -> Option<Here> {
    let running = zellij::settled()?;
    let current = zellij::current();
    let holding = current.filter(|current| running.iter().any(|(name, _)| name == current));
    let running = running.into_iter().map(|(name, id)| Running {
        name,
        started: id.started(),
        seen: news.last_seen(id.started(), id.socket()),
    });
    let sessions = joinable(running.collect(), named, holding.as_deref());
    Some(Here { refuge: zellij::answering(sessions), holding })
}

/// The sessions of `running` a bare luneta can join, the likeliest first: the
/// one `named` for its directory; then those you were last in; then those you
/// have not been in, the newest first, and of two that started at once the one
/// whose name comes last, as the picker has them. Never `holding`, nor one of
/// a name luneta cannot hand zellij.
fn joinable(
    mut running: Vec<Running>,
    named: Option<&SessionName>,
    holding: Option<&str>,
) -> Vec<SessionName> {
    running.retain(|session| holding != Some(session.name.as_str()));
    let is_named = |session: &Running| named.is_some_and(|named| named.as_str() == session.name);
    running.sort_by(|a, b| {
        (is_named(b), b.seen, b.started, &b.name).cmp(&(is_named(a), a.seen, a.started, &a.name))
    });
    let sessions = running.into_iter();
    sessions.filter_map(|session| SessionName::running(&session.name).ok()).collect()
}

/// luneta has no session to join, whose directory's session is `named`. It
/// makes that one, unless sessions run that it will not join, which it
/// refuses for, as `refusal_to_make` says.
fn left(
    named: Option<SessionName>,
    holding: Option<&str>,
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
/// `holding`, the session its terminal is a pane of, and every session of the
/// machines `held`, which may hold it: sessions run, and you did not ask for
/// another by naming it.
fn refusal_to_make(holding: Option<&str>, held: &[Destination]) -> Option<String> {
    let mut holders = Vec::new();
    if let Some(session) = holding {
        holders.push(format!("this terminal is a pane of \"{session}\""));
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
        Running { name: name.to_owned(), started: at(started), seen: seen.map(at) }
    }

    fn name(name: &str) -> SessionName {
        SessionName::running(name).unwrap()
    }

    fn order(
        sessions: Vec<Running>,
        named: Option<&str>,
        holding: Option<&str>,
    ) -> Vec<String> {
        let named = named.map(name);
        let joinable = joinable(sessions, named.as_ref(), holding);
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
        assert_eq!(order(sessions(), None, None), by_news);
        assert_eq!(order(sessions(), Some("no-such-session"), None), by_news);
        let in_project = ["project", "seen-last", "seen-long-ago", "newest", "older"];
        assert_eq!(order(sessions(), Some("project"), None), in_project);
    }

    /// Of two that started at once, the one whose name comes last goes first,
    /// as the picker has them.
    #[test]
    fn of_two_alike_the_one_whose_name_comes_last_goes_first() {
        let sessions = vec![running("a", 2, None), running("b", 2, None)];
        assert_eq!(order(sessions, None, None), ["b", "a"]);
    }

    /// A session luneta's terminal is a pane of is never joined, though it is
    /// the one named for the directory, and the last you were in; and one of
    /// a name luneta cannot hand zellij is passed over.
    #[test]
    fn a_session_that_holds_luneta_is_never_joined() {
        let sessions = vec![
            running("here", 30, Some(300)),
            running("other", 20, None),
            running("no/name", 40, Some(400)),
        ];
        assert_eq!(order(sessions, Some("here"), Some("here")), ["other"]);
        assert!(order(vec![running("here", 30, None)], None, Some("here")).is_empty());
    }

    /// With nothing left to join, luneta refuses where what it passed over
    /// runs, which says what it found, and otherwise has nothing to refuse:
    /// it makes one. The session's words are those of the refusal of old.
    #[test]
    fn luneta_refuses_to_make_a_session_only_where_it_passed_sessions_over() {
        assert_eq!(refusal_to_make(None, &[]), None);
        assert_eq!(
            refusal_to_make(Some("work"), &[]).unwrap(),
            "this terminal is a pane of \"work\", which zellij would show inside itself: name \
             another session, or run luneta outside zellij"
        );
        let held: [Destination; 2] = ["pi".parse().unwrap(), "me@vps".parse().unwrap()];
        assert_eq!(
            refusal_to_make(None, &held).unwrap(),
            "each session of pi, me@vps may hold this terminal in a pane, which zellij would show \
             inside itself: name another session, or run luneta outside zellij"
        );
        let both = refusal_to_make(Some("work"), &held[..1]).unwrap();
        assert!(
            both.starts_with(
                "this terminal is a pane of \"work\", and each session of pi may hold this \
                 terminal in a pane, which"
            ),
            "{both}"
        );
    }
}
