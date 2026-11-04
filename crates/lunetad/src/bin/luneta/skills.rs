//! The skills a pane's directory can run, as the finder of `lunetad::skills`
//! finds them there, on the machine the pane is on, and the search box over
//! them in the menu, narrowed to what you type.

use std::sync::mpsc::Sender;
use std::thread;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
pub use lunetad::skills::{Name, Runs, Skill};

use crate::away::{self, Away};
use crate::host::Event;
use crate::machines::Machine;

/// Asks the finder for the skills `cwd`, on `on`, can run, on a thread of
/// its own, and hands back what it found, or why it found nothing, as the
/// event `said` makes of it.
pub fn find(
    on: &Machine,
    cwd: &str,
    events: &Sender<Event>,
    said: impl FnOnce(Result<Vec<Skill>, String>) -> Event + Send + 'static,
) {
    let (on, cwd, events) = (on.clone(), cwd.to_owned(), events.clone());
    thread::spawn(move || {
        let skills = match &on {
            Machine::Local => lunetad::skills::found_in(&cwd),
            Machine::Remote(destination) => {
                Away::reach(destination, away::ANSWER).and_then(|mut away| away.skills(&cwd))
            },
        };
        // luneta is on its way out.
        let _ = events.send(said(skills));
    });
}

/// How well a skill matches a search: one named the term itself beats any
/// other, then one whose name matches beats one only its description does,
/// and then the higher score wins. A few letters land somewhere in nearly
/// any description, so those come last.
type Fit = (bool, bool, i64);

/// A skill that matches the term: where it stands among the skills, and the
/// characters of its name the term lit.
struct Found {
    at: usize,
    hits: Vec<usize>,
}

/// The skills of a directory, as the finder said them.
#[derive(Debug, PartialEq, Eq)]
pub enum Skills {
    Finding,
    Found(Vec<Skill>),
    Failed(String),
}

/// The search box over the skills of a directory, while it is open: what is
/// typed, the skills, once the finder has said, and those that match what is
/// typed, best first, the cursor on one. With nothing typed, every skill
/// matches, by name.
pub struct Search {
    term: String,
    skills: Skills,
    found: Vec<Found>,
    selected: usize,
}

impl Search {
    /// A box waiting on the finder.
    pub fn new() -> Self {
        Search { term: String::new(), skills: Skills::Finding, found: Vec::new(), selected: 0 }
    }

    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    /// The skills that match, best first, each with the characters of its
    /// name the term lit.
    pub fn found(&self) -> impl Iterator<Item = (&Skill, &[usize])> {
        let skills = match &self.skills {
            Skills::Found(skills) => skills.as_slice(),
            Skills::Finding | Skills::Failed(_) => &[],
        };
        self.found.iter().filter_map(|found| Some((skills.get(found.at)?, found.hits.as_slice())))
    }

    /// How many skills match.
    pub fn count(&self) -> usize {
        self.found.len()
    }

    /// Where the cursor is among the skills that match, while one does.
    pub fn selected(&self) -> Option<usize> {
        (self.selected < self.found.len()).then_some(self.selected)
    }

    /// The skill the cursor is on.
    pub fn chosen(&self) -> Option<&Skill> {
        self.found().nth(self.selected).map(|(skill, _)| skill)
    }

    /// Takes what the finder said, while the box waits for it, and says
    /// whether it did.
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

    /// Takes back the last character typed, and says whether there was one.
    pub fn pop(&mut self) -> bool {
        let popped = self.term.pop().is_some();
        if popped {
            self.narrow();
        }
        popped
    }

    /// Puts the cursor on the skill called `name`, where it is among those
    /// that match, and says whether it is.
    pub fn select(&mut self, name: &Name) -> bool {
        let at = self.found().position(|(skill, _)| skill.name == *name);
        if let Some(at) = at {
            self.selected = at;
        }
        at.is_some()
    }

    /// Moves the cursor `by` skills down, or up, and no further than the ends.
    pub fn move_by(&mut self, by: isize) {
        let last = self.found.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Keeps the skills that match the term, best first, and puts the cursor
    /// on the best.
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

    /// The skills that match what is typed come best first: a skill named
    /// the term, then the ones whose names match, then the ones only their
    /// descriptions match. Typing goes on while the finder looks, and the
    /// skills it finds are narrowed by what was typed by then.
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

        // A second answer is for a box closed before the first came.
        assert!(!search.fill(Ok(Vec::new())));
        assert_eq!(search.count(), 4);
    }
}
