use std::collections::BTreeSet;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::Duration;

use lune_domain::projects::{Dir, Spot, TerminalViewId, VIEWS_MAX};

use crate::projects::spot;

pub const LOOKS_MAX: usize = 4;

const _: () = assert!(LOOKS_MAX > 0);
const _: () = assert!(LOOKS_MAX <= VIEWS_MAX);

struct Look {
    terminal: TerminalViewId,
    folder: Dir,
}

pub struct Spotted {
    pub terminal: TerminalViewId,
    pub folder: Dir,
    pub spot: Result<Spot, String>,
}

pub struct Homes {
    sent: SyncSender<Look>,
    spotted: Receiver<Spotted>,
    in_flight: BTreeSet<TerminalViewId>,
}

impl Homes {
    pub fn start(timeout: Duration) -> io::Result<Homes> {
        assert!(!timeout.is_zero(), "a look has time to run");
        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let thread = thread::Builder::new().name("homes".to_owned());
        thread.spawn(move || run(&looks, &found, timeout))?;
        Ok(Homes { sent, spotted, in_flight: BTreeSet::new() })
    }

    #[must_use]
    pub fn room(&self) -> usize {
        assert!(self.in_flight.len() <= LOOKS_MAX, "no more looks than the homes take");
        LOOKS_MAX - self.in_flight.len()
    }

    #[must_use]
    pub fn looking(&self, terminal: TerminalViewId) -> bool {
        assert!(self.in_flight.len() <= LOOKS_MAX, "no more looks than the homes take");
        self.in_flight.contains(&terminal)
    }

    pub fn look(&mut self, terminal: TerminalViewId, folder: Dir) {
        assert!(!self.looking(terminal), "terminal {terminal} is looked for once at a time");
        assert!(self.in_flight.len() < LOOKS_MAX, "a look is asked only while the homes have room");
        let sent = self.sent.try_send(Look { terminal, folder });
        assert!(
            !matches!(sent, Err(TrySendError::Disconnected(_))),
            "the homes run as long as layouts ask"
        );
        assert!(sent.is_ok(), "the homes take every look there is room for");
        self.in_flight.insert(terminal);
        assert!(self.in_flight.len() <= LOOKS_MAX, "no more looks than the homes take");
    }

    #[must_use]
    pub fn spotted(&mut self) -> Option<Spotted> {
        let spotted = match self.spotted.try_recv() {
            Ok(spotted) => spotted,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                unreachable!("the homes run as long as layouts listen")
            },
        };
        let was = self.in_flight.remove(&spotted.terminal);
        assert!(was, "terminal {} is spotted only once it is looked for", spotted.terminal);
        assert!(self.in_flight.len() < LOOKS_MAX, "a look ended leaves room for another");
        Some(spotted)
    }
}

fn run(looks: &Receiver<Look>, found: &SyncSender<Spotted>, timeout: Duration) {
    assert!(!timeout.is_zero(), "a look has time to run");
    while let Ok(Look { terminal, folder }) = looks.recv() {
        let spot = spot::of(&folder, timeout);
        let sent = found.try_send(Spotted { terminal, folder, spot });
        assert!(!matches!(sent, Err(TrySendError::Full(_))), "spotted holds every look in flight");
        if sent.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use lune_domain::projects::{Dir, Id, Projects, Spot, TerminalViewId};
    use lune_waits::eventually;

    use super::{run, Homes, Look, Spotted, LOOKS_MAX};
    use crate::panics::contained;

    fn terminal(id: u64) -> TerminalViewId {
        Projects::default().fresh_terminal(&mut || Ok(Id::of(id).unwrap())).unwrap()
    }

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    #[test]
    fn homes_holds_at_most_its_limit_one_per_terminal_and_reuses_slots() {
        assert!(contained(|| Homes::start(Duration::ZERO)).is_err());

        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let mut homes = Homes { sent, spotted, in_flight: BTreeSet::new() };
        assert_eq!(homes.room(), LOOKS_MAX);
        assert!(homes.spotted().is_none());

        for step in 1..=LOOKS_MAX {
            let id = terminal(u64::try_from(step).unwrap());
            assert!(!homes.looking(id));
            homes.look(id, dir(&format!("/work/{step}")));
            assert!(homes.looking(id));
            assert_eq!(homes.room(), LOOKS_MAX - step);
        }
        assert_eq!(homes.room(), 0);
        assert!(contained(|| homes.look(terminal(1), dir("/work/1"))).is_err());
        assert!(contained(|| homes.look(terminal(99), dir("/work/99"))).is_err());

        let received: Vec<Look> = (0..LOOKS_MAX).map(|_| looks.recv().unwrap()).collect();
        assert_eq!(received.len(), LOOKS_MAX);

        let first = terminal(1);
        let second = terminal(2);
        let first_dir = dir("/work/1");
        let first_spot = Spot::in_folder(first_dir.clone(), None);
        found
            .send(Spotted { terminal: first, folder: first_dir.clone(), spot: Ok(first_spot.clone()) })
            .unwrap();
        found
            .send(Spotted { terminal: second, folder: dir("/work/2"), spot: Err("stuck".to_owned()) })
            .unwrap();

        let one = homes.spotted().unwrap();
        assert_eq!((one.terminal, one.folder, one.spot), (first, first_dir, Ok(first_spot)));
        assert!(!homes.looking(first));
        assert_eq!(homes.room(), 1);

        let two = homes.spotted().unwrap();
        assert_eq!((two.terminal, two.spot), (second, Err("stuck".to_owned())));
        assert!(!homes.looking(second));
        assert_eq!(homes.room(), 2);
        assert!(homes.spotted().is_none());

        homes.look(first, dir("/work/again"));
        assert!(homes.looking(first));
        assert_eq!(homes.room(), 1);

        found
            .send(Spotted { terminal: terminal(77), folder: dir("/work/77"), spot: Err("x".to_owned()) })
            .unwrap();
        assert!(contained(|| homes.spotted()).is_err());
    }

    #[test]
    fn homes_spots_folders_and_its_thread_ends_when_dropped() {
        let scratch = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(scratch.path()).unwrap();
        let here_path = real.join("here");
        fs::create_dir(&here_path).unwrap();
        let here = dir(here_path.to_str().unwrap());
        let gone = dir(real.join("gone").to_str().unwrap());

        let mut homes = Homes::start(Duration::from_secs(5)).unwrap();
        let (first, second) = (terminal(1), terminal(2));
        homes.look(first, here.clone());
        homes.look(second, gone.clone());

        let one = eventually("first folder spotted", || homes.spotted());
        let two = eventually("second folder spotted", || homes.spotted());
        assert_eq!(homes.room(), LOOKS_MAX);
        assert_eq!(
            (one.terminal, one.folder, one.spot),
            (first, here.clone(), Ok(Spot::in_folder(here.clone(), None)))
        );
        assert_eq!(two.terminal, second);
        assert_eq!(two.folder, gone);
        assert!(two.spot.unwrap_err().contains("is gone"));
        drop(homes);

        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        let worker = thread::spawn(move || run(&looks, &found, Duration::from_secs(5)));
        drop(sent);
        assert!(worker.join().is_ok(), "dropping sent ends the homes thread");
        drop(spotted);

        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let (found, spotted) = mpsc::sync_channel(LOOKS_MAX);
        drop(spotted);
        let worker = thread::spawn(move || run(&looks, &found, Duration::from_secs(5)));
        sent.send(Look { terminal: first, folder: here }).unwrap();
        assert!(worker.join().is_ok(), "dropping spotted ends the homes thread");
    }
}
