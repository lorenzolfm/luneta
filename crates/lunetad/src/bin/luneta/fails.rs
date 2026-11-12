//! Where luneta's zellij goes where its client fails, as where ssh does not
//! reach its machine, or zellij there cannot attach: nowhere, as luneta ends;
//! back to the session it was in before, on another machine; or on to the
//! newest session of another machine's, where luneta took it to one by
//! itself, but never to one whose client failed since luneta's zellij was
//! last in a session. Each failure adds its machine, so the tries end; once a
//! client is in, those that failed before are tried again where it fails, as
//! what failed then may not fail now, however long it was in. A client of
//! zellij's that took the place of luneta's client of an own session goes
//! back to that session.

use lunetad::mux::own::OwnId;
use lunetad::Destination;

use crate::machines::Machine;
use crate::zellij::SessionName;

/// A session luneta's zellij was in, on `machine`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Back {
    pub machine: Machine,
    pub session: SessionName,
}

/// Where luneta's zellij goes where its client fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fails {
    /// Nowhere: luneta ends, and says what zellij said.
    End,
    /// Back to the session it was in before, on another machine.
    Back(Back),
    /// Back to the own session luneta's client was in before.
    Own(OwnId),
    /// On to the newest session of another machine's but those `tried`, as
    /// `App::newest_here` finds it: of a client of a session luneta's zellij
    /// goes on in on another machine, which luneta picked itself. Those tried
    /// are the machine it went on from, which had none left, and each whose
    /// client failed since luneta's zellij was last in a session.
    Onward(Vec<Destination>),
}

/// What luneta's zellij does as its client failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// luneta ends.
    End,
    /// It goes back to the session it was in before.
    Back(Back),
    /// It goes back to the own session it was in before.
    Own(OwnId),
    /// It goes on in this machine's newest session, or else another
    /// machine's but those it holds, as `App::newest_here` finds it.
    Onward(Vec<Destination>),
}

impl Fails {
    /// What luneta's zellij does as its client on `on` failed: one that
    /// luneta took on to another machine's session by itself goes on, and
    /// that machine is passed over until a client is in.
    pub fn failed(self, on: &Machine) -> Failed {
        match self {
            Fails::End => Failed::End,
            Fails::Back(back) => Failed::Back(back),
            Fails::Own(id) => Failed::Own(id),
            Fails::Onward(mut tried) => {
                if let Machine::Remote(destination) = on {
                    tried.push(destination.clone());
                }
                Failed::Onward(tried)
            },
        }
    }

    /// luneta's zellij's client is in its session, as zellij lists it there,
    /// or its session ended with it in: where it fails, the machines whose
    /// clients failed before it came are tried again.
    pub fn came_in(&mut self) {
        if let Fails::Onward(tried) = self {
            tried.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn away(name: &str) -> Destination {
        name.parse().unwrap()
    }

    /// Where the clients luneta takes on to other machines' sessions by
    /// itself fail one after another, each machine is passed over from then
    /// on, so that the tries end. But once one is in, as near's is, and it
    /// fails an hour later, as ssh loses the machine, those that failed
    /// before are tried again: far is up again by then.
    #[test]
    fn a_machine_whose_client_failed_is_passed_over_until_a_client_is_in() {
        let (pi, far, near) = (away("pi"), away("far"), away("near"));
        // pi had no session left once luneta's zellij's ended there.
        let refused = Fails::Onward(vec![pi.clone()]).failed(&Machine::Remote(far.clone()));
        assert_eq!(refused, Failed::Onward(vec![pi.clone(), far.clone()]));
        let tried = vec![pi.clone(), far.clone()];
        let lost = Fails::Onward(tried.clone()).failed(&Machine::Remote(near.clone()));
        assert_eq!(lost, Failed::Onward(vec![pi, far, near.clone()]));
        let mut fails = Fails::Onward(tried);
        fails.came_in();
        assert_eq!(fails.failed(&Machine::Remote(near.clone())), Failed::Onward(vec![near]));
    }
}
