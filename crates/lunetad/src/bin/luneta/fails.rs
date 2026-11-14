use lunetad::mux::own::OwnKey;
use lunetad::Destination;

use crate::machines::Machine;
use crate::zellij::SessionName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Back {
    pub machine: Machine,
    pub session: SessionName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fails {
    End,
    Back(Back),
    Own(OwnKey),
    Onward(Vec<Destination>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    End,
    Back(Back),
    Own(OwnKey),
    Onward(Vec<Destination>),
}

impl Fails {
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

    #[test]
    fn a_machine_whose_client_failed_is_passed_over_until_a_client_is_in() {
        let (pi, far, near) = (away("pi"), away("far"), away("near"));
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
