use std::sync::mpsc::Sender;
use std::thread;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
pub use lunetad::skills::{Name, Runs, Skill};
use lunetad::Directory;

use crate::agents::Harness;
use crate::away::{self, Away};
use crate::host::Event;
use crate::machines::Machine;

pub fn find(
    on: &Machine,
    cwd: &Directory,
    harness: Harness,
    events: &Sender<Event>,
    said: impl FnOnce(Result<Vec<Skill>, String>) -> Event + Send + 'static,
) {
    let (on, cwd, events) = (on.clone(), cwd.clone(), events.clone());
    thread::spawn(move || {
        let skills = match &on {
            Machine::Local => lunetad::skills::found_in(cwd.as_str(), harness),
            Machine::Remote(destination) => Away::reach(destination, away::ANSWER)
                .and_then(|mut away| away.skills(&cwd, harness)),
        };
        let _ = events.send(said(skills));
    });
}

type Fit = (bool, bool, i64);

struct Found {
    at: usize,
    hits: Vec<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Skills {
    Finding,
    Found(Vec<Skill>),
    Failed(String),
}

pub struct Search {
    term: String,
    skills: Skills,
    found: Vec<Found>,
    selected: usize,
}

impl Search {
    pub fn new() -> Self {
        Search { term: String::new(), skills: Skills::Finding, found: Vec::new(), selected: 0 }
    }

    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    pub fn found(&self) -> impl Iterator<Item = (&Skill, &[usize])> {
        let skills = match &self.skills {
            Skills::Found(skills) => skills.as_slice(),
            Skills::Finding | Skills::Failed(_) => &[],
        };
        self.found.iter().filter_map(|found| Some((skills.get(found.at)?, found.hits.as_slice())))
    }

    pub fn count(&self) -> usize {
        self.found.len()
    }

    pub fn selected(&self) -> Option<usize> {
        (self.selected < self.found.len()).then_some(self.selected)
    }

    pub fn chosen(&self) -> Option<&Skill> {
        self.found().nth(self.selected).map(|(skill, _)| skill)
    }

    pub fn refind(&mut self) {
        self.skills = Skills::Finding;
        self.narrow();
    }

    pub fn fill(&mut self, skills: Result<Vec<Skill>, String>) -> bool {
        if self.skills != Skills::Finding {
            return false;
        }
        self.skills = match skills {
            Ok(skills) => Skills::Found(skills),
            Err(why) => Skills::Failed(why),
        };
        self.narrow();
        true
    }

    pub fn push(&mut self, c: char) {
        self.term.push(c);
        self.narrow();
    }

    pub fn pop(&mut self) -> bool {
        let popped = self.term.pop().is_some();
        if popped {
            self.narrow();
        }
        popped
    }

    pub fn select(&mut self, name: &Name) -> bool {
        let at = self.found().position(|(skill, _)| skill.name == *name);
        if let Some(at) = at {
            self.selected = at;
        }
        at.is_some()
    }

    pub fn move_by(&mut self, by: isize) {
        let last = self.found.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    fn narrow(&mut self) {
        let Skills::Found(skills) = &self.skills else {
            return;
        };
        let matcher = SkimMatcherV2::default();
        let term = self.term.as_str();
        let mut found: Vec<(Fit, Found)> = skills
            .iter()
            .enumerate()
            .filter_map(|(at, skill)| {
                if term.is_empty() {
                    return Some(((false, false, 0), Found { at, hits: Vec::new() }));
                }
                let name = skill.name.as_str();
                if let Some((score, hits)) = matcher.fuzzy_indices(name, term) {
                    return Some(((name == term, true, score), Found { at, hits }));
                }
                let score = matcher.fuzzy_match(&skill.description, term)?;
                Some(((false, false, score), Found { at, hits: Vec::new() }))
            })
            .collect();
        found.sort_by(|(a, one), (b, other)| b.cmp(a).then(one.at.cmp(&other.at)));
        self.found = found.into_iter().map(|(_, found)| found).collect();
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn skill(name: &str, description: &str) -> Skill {
        let name = Name::new(name.to_owned()).expect("a name");
        Skill { name, description: description.to_owned(), runs: Runs::ForAnyone }
    }

    fn names(search: &Search) -> Vec<&str> {
        search.found().map(|(skill, _)| skill.name.as_str()).collect()
    }

    #[test]
    fn a_search_narrows_the_skills_to_the_best_match_first() {
        let mut search = Search::new();
        search.push('r');
        search.push('e');
        assert_eq!(search.count(), 0);
        assert!(search.chosen().is_none());
        assert!(search.fill(Ok(vec![
            skill("code-review", "Review the changes since a fixed point."),
            skill("prototype", "Build a throwaway prototype."),
            skill("re", "Exactly the term."),
            skill("tigerstyle", "Review code as TigerBeetle writes it."),
        ])));
        search.push('v');
        assert_eq!(names(&search), ["code-review", "tigerstyle"]);
        assert_eq!(search.chosen().map(|skill| skill.name.as_str()), Some("code-review"));
        let hits: Vec<&[usize]> = search.found().map(|(_, hits)| hits).collect();
        assert_eq!(hits, [&[5, 6, 7][..], &[]]);

        assert!(search.pop());
        assert_eq!(names(&search)[0], "re");
        search.move_by(1);
        assert_eq!(search.selected(), Some(1));
        search.move_by(9);
        assert_eq!(search.selected(), Some(3));
        assert!(search.pop());
        assert!(search.pop());
        assert!(!search.pop());
        assert_eq!(names(&search), ["code-review", "prototype", "re", "tigerstyle"]);

        assert!(!search.fill(Ok(Vec::new())));
        assert_eq!(search.count(), 4);
    }
}
