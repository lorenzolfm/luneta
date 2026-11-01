//! The sessions the daemon lists, of each machine, by which luneta tells
//! which of the sessions it keeps something of have ended, for what it kept
//! of them to go: the news of their panes, and their colours.
//!
//! A session has ended once the machine it ran on has listed its sessions
//! whole without it for `GONE`. One whose machine's list may leave it out, as
//! that of a machine lunetad does not reach, may run still. What luneta keeps
//! of a session does not say its machine, so luneta goes by the machine it
//! last heard the session listed on: one it never heard listed may be of any
//! machine's, and so has not ended while any machine's list may leave one
//! out.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::ledger::Session;
use crate::machines::Machine;

/// How long the daemon goes without listing a session before what was kept
/// of it goes: the news of its panes, and its colour in the order. A rename
/// made while the daemon reads zellij's folder leaves the session out of one
/// list, and a luneta can hear of a session after another kept something of
/// it. The daemon lists it again within its sweep, of two seconds.
pub const GONE: Duration = Duration::from_secs(10);

/// The sessions the daemon lists now, of every machine.
#[derive(Debug, Default)]
pub struct Lists {
    /// The sessions that run, and the machine each runs on.
    running: HashMap<Session, Machine>,
    /// The machines whose list may leave out one that runs.
    unsure: HashSet<Machine>,
}

impl Lists {
    /// `machine` runs `sessions`, as the daemon lists them, which may leave
    /// out one that runs unless the list is `whole`.
    pub fn lists(
        &mut self,
        machine: &Machine,
        sessions: impl IntoIterator<Item = Session>,
        whole: bool,
    ) {
        self.running.extend(sessions.into_iter().map(|session| (session, machine.clone())));
        if !whole {
            self.unsure.insert(machine.clone());
        }
    }
}

/// Since when the daemon has not listed each session something is kept of,
/// while the machine it ran on listed its sessions whole, and the machine
/// each ran on, as the daemon last listed it.
#[derive(Debug, Default)]
pub struct Missing {
    since: BTreeMap<Session, Instant>,
    ran_on: HashMap<Session, Machine>,
}

impl Missing {
    /// The sessions of `kept` that have ended, by the lists `live` at `at`,
    /// by luneta's own clock. With no lists, as while the daemon does not
    /// answer, each waits from the start again, and so does each that a
    /// list may leave out.
    pub fn gone(
        &mut self,
        kept: &BTreeSet<Session>,
        live: Option<&Lists>,
        at: Instant,
    ) -> BTreeSet<Session> {
        self.since.retain(|session, _| kept.contains(session));
        self.ran_on.retain(|session, _| kept.contains(session));
        let Some(live) = live else {
            self.since.clear();
            return BTreeSet::new();
        };
        let mut gone = BTreeSet::new();
        for session in kept {
            if let Some(machine) = live.running.get(session) {
                self.ran_on.insert(*session, machine.clone());
                self.since.remove(session);
                continue;
            }
            let unsure = match self.ran_on.get(session) {
                Some(machine) => live.unsure.contains(machine),
                None => !live.unsure.is_empty(),
            };
            if unsure {
                self.since.remove(session);
                continue;
            }
            let since = *self.since.entry(*session).or_insert(at);
            if at.duration_since(since) >= GONE {
                gone.insert(*session);
            }
        }
        for session in &gone {
            self.since.remove(session);
            self.ran_on.remove(session);
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::time::SystemTime;

    use super::*;

    fn session(n: u64) -> Session {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(n);
        Session::new(started, NonZeroU64::new(n).unwrap()).unwrap()
    }

    /// The lists of `machines`, each its sessions and whether it is whole.
    fn lists<const N: usize>(machines: [(&Machine, &[Session], bool); N]) -> Lists {
        let mut live = Lists::default();
        for (machine, sessions, whole) in machines {
            live.lists(machine, sessions.iter().copied(), whole);
        }
        live
    }

    /// A session ends once the machine it ran on lists its sessions whole
    /// without it for `GONE`, while another machine's list may leave out
    /// one of its own: that machine's session stays, and so does one luneta
    /// never heard listed, which may be of that machine's. Once no list may
    /// leave one out, as that machine is forgotten, each of them ends.
    #[test]
    fn a_session_ends_by_the_list_of_the_machine_it_ran_on() {
        let (here, vps) = (Machine::Local, Machine::Remote("me@vps".parse().unwrap()));
        let [mine, its, unheard] = [1, 2, 3].map(session);
        let kept = BTreeSet::from([mine, its, unheard]);
        let mut missing = Missing::default();
        let start = Instant::now();
        let both = lists([(&here, &[mine], true), (&vps, &[its], true)]);
        assert!(missing.gone(&kept, Some(&both), start).is_empty());

        let unreached = lists([(&here, &[], true), (&vps, &[], false)]);
        assert!(missing.gone(&kept, Some(&unreached), start).is_empty());
        let gone = missing.gone(&kept, Some(&unreached), start + GONE);
        assert_eq!(gone, BTreeSet::from([mine]));

        let kept = BTreeSet::from([its, unheard]);
        let forgotten = lists([(&here, &[], true)]);
        assert!(missing.gone(&kept, Some(&forgotten), start + GONE * 2).is_empty());
        assert_eq!(missing.gone(&kept, Some(&forgotten), start + GONE * 3), kept);
    }
}
