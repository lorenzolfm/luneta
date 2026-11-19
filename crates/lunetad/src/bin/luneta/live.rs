use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::ledger::Session;
use crate::machines::Machine;

pub const GONE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct Lists {
    running: HashMap<Session, Machine>,
    unsure: HashSet<Machine>,
}

impl Lists {
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

#[derive(Debug, Default)]
pub struct Missing {
    since: BTreeMap<Session, Instant>,
    ran_on: HashMap<Session, Machine>,
}

impl Missing {
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
    use super::*;

    fn session(n: u64) -> Session {
        Session::Project(lunetad::projects::ProjectId::of(n).unwrap())
    }

    fn lists<const N: usize>(machines: [(&Machine, &[Session], bool); N]) -> Lists {
        let mut live = Lists::default();
        for (machine, sessions, whole) in machines {
            live.lists(machine, sessions.iter().copied(), whole);
        }
        live
    }

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
