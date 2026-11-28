use std::fmt;

use crate::skills::{Name, Runs, Skill};

const SLASH: &str = "slash";

const SKILL: &str = "skill";

const HEARD_AS_SKILL: [&str; 3] = ["skills", "scale", "queue"];

const SAID_IN: usize = 8;

const LEADING_UP: [&str; 16] = [
    "a", "an", "and", "apply", "by", "my", "now", "our", "please", "run", "then", "the", "through",
    "use", "using", "with",
];

const ARTICLES: [&str; 3] = ["a", "an", "the"];

const LEAD_INS: [&str; 5] = ["apply", "run", "use", "using", "with"];

const BETWEEN: [&str; 5] = ["a", "an", "my", "our", "the"];

const MARGIN: u32 = 100;

const PLUGINS_COST: u32 = MARGIN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Said,
    Heard,
}

struct Word<'a> {
    text: &'a str,
    letters: String,
}

impl<'a> Word<'a> {
    fn new(text: &'a str) -> Self {
        Word {
            text,
            letters: letters(text),
        }
    }

    fn mark(&self) -> Option<Mark> {
        let letters = self.letters.as_str();
        if letters == SKILL {
            return Some(Mark::Said);
        }
        HEARD_AS_SKILL.contains(&letters).then_some(Mark::Heard)
    }

    fn says_slash(&self) -> Option<usize> {
        if self.letters == SLASH {
            return Some(1);
        }
        let bare = self
            .text
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '/');
        let name = bare.strip_prefix('/').filter(|name| !name.contains('/'))?;
        Some(usize::from(name.is_empty()))
    }

    fn ending(&self) -> &'a str {
        let bare = self.text.trim_end_matches(|c: char| !c.is_alphanumeric());
        &self.text[bare.len()..]
    }
}

struct Named<'a> {
    from: usize,
    to: usize,
    skill: &'a Skill,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Written {
    pub command: Option<Name>,
    pub text: String,
    pub only_first: Vec<Name>,
}

impl Written {
    pub fn said(said: &str) -> Self {
        Written {
            command: None,
            text: said.to_owned(),
            only_first: Vec::new(),
        }
    }
}

impl fmt::Display for Written {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.command {
            Some(command) if self.text.is_empty() => write!(f, "/{command}"),
            Some(command) => write!(f, "/{command} {}", self.text),
            None => f.write_str(&self.text),
        }
    }
}

pub fn marked(said: &str) -> bool {
    let says = |word: Word<'_>| word.mark().is_some() || word.says_slash().is_some();
    said.split_whitespace().map(Word::new).any(says)
}

pub fn written(said: &str, skills: &[Skill]) -> Written {
    let words: Vec<Word<'_>> = said.split_whitespace().map(Word::new).collect();
    let named = named_in(&words, skills);
    let Some((first, others)) = named.split_first() else {
        return Written::said(said);
    };
    let leading_up = words[..first.from].iter().rev();
    let leading_up = leading_up.take_while(|word| LEADING_UP.contains(&word.letters.as_str()));
    let before = &words[..first.from - leading_up.count()];
    let ended = words[first.to - 1].ending();
    let mut parts = before_it(before, ended, first.to < words.len());
    let mut at = first.to;
    let mut only_first = Vec::new();
    for other in others {
        if other.skill.runs == Runs::OnlyForYou {
            only_first.push(other.skill.name.clone());
            continue;
        }
        let the = other.from > at && words[other.from - 1].letters == "the";
        parts.extend(
            words[at..other.from - usize::from(the)]
                .iter()
                .map(|w| w.text.to_owned()),
        );
        parts.push(format!(
            "the `{}` skill{}",
            other.skill.name,
            words[other.to - 1].ending()
        ));
        at = other.to;
    }
    parts.extend(words[at..].iter().map(|word| word.text.to_owned()));
    Written {
        command: Some(first.skill.name.clone()),
        text: parts.join(" "),
        only_first,
    }
}

fn before_it(before: &[Word<'_>], ended: &str, more: bool) -> Vec<String> {
    let mut words: Vec<String> = before.iter().map(|word| word.text.to_owned()).collect();
    if let Some(last) = words.last_mut() {
        last.truncate(last.trim_end_matches([',', ';', ':', '-', '–', '—']).len());
    }
    words.pop_if(|last| last.is_empty());
    if let Some(last) = words.last_mut()
        && more
        && !last.ends_with(['.', '?', '!'])
        && ended.starts_with(['.', '?', '!'])
    {
        last.push_str(ended);
    }
    words
}

fn named_in<'s>(words: &[Word<'_>], skills: &'s [Skill]) -> Vec<Named<'s>> {
    let mut named = Vec::new();
    let mut taken = 0;
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        if let Some(after) = word.says_slash() {
            let name = at + after;
            let longest = name..(name + SAID_IN).min(words.len());
            let spans = longest.map(|end| &words[name..=end]);
            if let Some((span, skill)) = closest(spans, skills) {
                let end = name + span;
                let said = words.get(end).and_then(Word::mark) == Some(Mark::Said);
                let to = end + usize::from(said);
                named.push(Named {
                    from: at,
                    to,
                    skill,
                });
                (taken, at) = (to, to);
                continue;
            }
        } else if let Some(mark) = word.mark() {
            let room = at - taken;
            let spans = (1..=SAID_IN.min(room)).map(|span| &words[at - span..at]);
            if let Some((span, skill)) = closest(spans, skills)
                && (mark == Mark::Said || led_in(&words[taken..at - span]))
            {
                named.push(Named {
                    from: at - span,
                    to: at + 1,
                    skill,
                });
                taken = at + 1;
            }
        }
        at += 1;
    }
    named
}

fn led_in(before: &[Word<'_>]) -> bool {
    let mut words = before.iter().rev();
    let lead = words.find(|word| !BETWEEN.contains(&word.letters.as_str()));
    lead.is_some_and(|word| LEAD_INS.contains(&word.letters.as_str()))
}

fn closest<'w, 's>(
    spans: impl Iterator<Item = &'w [Word<'w>]>,
    skills: &'s [Skill],
) -> Option<(usize, &'s Skill)> {
    let mut best: Option<(u32, usize, &Skill)> = None;
    for span in spans {
        if span
            .first()
            .is_some_and(|word| ARTICLES.contains(&word.letters.as_str()))
        {
            continue;
        }
        let said: String = span.iter().map(|word| word.letters.as_str()).collect();
        let Some((fit, skill)) = closest_skill(&said, skills) else {
            continue;
        };
        if best.is_none_or(|(most, ..)| fit > most) {
            best = Some((fit, span.len(), skill));
        }
    }
    best.map(|(_, words, skill)| (words, skill))
}

fn closest_skill<'s>(said: &str, skills: &'s [Skill]) -> Option<(u32, &'s Skill)> {
    let mut fits = skills
        .iter()
        .filter_map(|skill| Some((fit(said, skill.name.as_str())?, skill)));
    let first = fits.next()?;
    let (best, next) = fits.fold((first, 0), |(best, next), other| {
        if other.0 > best.0 {
            (other, best.0)
        } else {
            (best, next.max(other.0))
        }
    });
    (best.0 >= next + MARGIN).then_some(best)
}

fn fit(said: &str, name: &str) -> Option<u32> {
    let whole = letters_fit(said, name);
    let bare = name
        .rsplit_once(':')
        .and_then(|(_, bare)| letters_fit(said, bare));
    whole.max(bare.map(|fit| fit.saturating_sub(PLUGINS_COST)))
}

fn letters_fit(said: &str, name: &str) -> Option<u32> {
    let name: Vec<char> = letters(name).chars().collect();
    let said: Vec<char> = said.chars().collect();
    if name.is_empty() || said.is_empty() {
        return None;
    }
    let edits = edits(&said, &name);
    let allowed = name.len().saturating_sub(3) / 3;
    let longest = said.len().max(name.len());
    (edits <= allowed).then(|| u32::try_from(1000 - 1000 * edits / longest).unwrap_or(0))
}

fn edits(one: &[char], other: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=other.len()).collect();
    for (at, a) in one.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = at + 1;
        for (column, b) in other.iter().enumerate() {
            let changed = diagonal + usize::from(a != b);
            diagonal = row[column + 1];
            row[column + 1] = changed.min(diagonal + 1).min(row[column] + 1);
        }
    }
    row[other.len()]
}

fn letters(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skills(names: &[&str]) -> Vec<Skill> {
        let skill = |name: &&str| Skill {
            name: Name::new((*name).to_owned()).expect("a name"),
            description: String::new(),
            runs: Runs::ForAnyone,
        };
        names.iter().map(skill).collect()
    }

    fn written_for(said: &str) -> String {
        let names = [
            "code-review",
            "grill-me",
            "grilling",
            "handoff",
            "make-illegal-states-unrepresentable",
            "research",
            "review:deep",
            "teach",
            "thermo-nuclear-code-quality-review",
            "thermo-nuclear-review",
            "tigerstyle",
            "triage",
        ];
        written(said, &skills(&names)).to_string()
    }

    #[test]
    fn a_skill_named_goes_first() {
        for (said, command) in [
            (
                "review this pull request with the thermonuclear review skill",
                "/thermo-nuclear-review review this pull request",
            ),
            (
                "Review this pull request with the thermonuclear review skill.",
                "/thermo-nuclear-review Review this pull request",
            ),
            ("slash tiger style", "/tigerstyle"),
            ("Slash, Tiger Style.", "/tigerstyle"),
            ("/tigerstyle review this", "/tigerstyle review this"),
            ("/ tiger style review this", "/tigerstyle review this"),
            (
                "Slash tiger style skill on this branch.",
                "/tigerstyle on this branch.",
            ),
            (
                "Review this with slash tiger style.",
                "/tigerstyle Review this",
            ),
            (
                "Make sure this passes the make illegal states unrepresentable skill.",
                "/make-illegal-states-unrepresentable Make sure this passes",
            ),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
    }

    #[test]
    fn handoff_runs_only_where_a_mark_says_so() {
        for (said, command) in [
            ("Use the handoff skill.", "/handoff"),
            ("Now use the hand off skill.", "/handoff"),
            (
                "Hand-off skill. Next session fixes CI.",
                "/handoff Next session fixes CI.",
            ),
            (
                "slash hand off next session fixes CI",
                "/handoff next session fixes CI",
            ),
            ("Use the hand of skill.", "/handoff"),
            ("Use the handoff scale", "/handoff"),
            (
                "The next session fixes CI, so use the handoff skill.",
                "/handoff The next session fixes CI, so",
            ),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
        for said in [
            "Hand off the work to the next agent.",
            "Write a handoff for this.",
            "Hand this off with a note.",
            "The handout skill.",
            "A hands-on skill.",
        ] {
            assert_eq!(written_for(said), said);
        }
    }

    #[test]
    fn a_name_is_matched_loosely() {
        for (said, command) in [
            (
                "make unavailable states unrepresentable skill",
                "/make-illegal-states-unrepresentable",
            ),
            (
                "make legal statements unrepresentable skill",
                "/make-illegal-states-unrepresentable",
            ),
            (
                "Make sure this passes the make illegal state's unrepresentable skill",
                "/make-illegal-states-unrepresentable Make sure this passes",
            ),
            (
                "Run the thermonuclear review scale",
                "/thermo-nuclear-review",
            ),
            ("slash thermal nuclear review", "/thermo-nuclear-review"),
            ("the tigers style skill", "/tigerstyle"),
            ("Use the Tiger-Style queue", "/tigerstyle"),
            ("the deep skill", "/review:deep"),
            ("the review deep skill", "/review:deep"),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
    }

    #[test]
    fn what_names_no_skill_stays_as_it_was_said() {
        for said in [
            "research the login flow",
            "triage these and teach me how",
            "make sure this passes make illegal states unrepresentable",
            "Write a new skill for it.",
            "For each skill, read this skill.",
            "Look in slash etc slash nginx.",
            "Look in /etc/nginx first.",
            "It is either and/or.",
            "the grill skill",
            "skill",
            "slash",
        ] {
            assert_eq!(written_for(said), said);
        }
        let close = skills(&[
            "thermo-nuclear-review",
            "thermo-nuclear-code-quality-review",
        ]);
        let said = "the thermonuclear code review skill";
        assert_eq!(written(said, &close), Written::said(said));
        let spaced = "slash  tiger style";
        assert_eq!(written(spaced, &[]), Written::said(spaced));
    }

    #[test]
    fn a_word_heard_as_skill_takes_a_lead_in() {
        for (said, command) in [
            ("Use the handoff scale", "/handoff"),
            ("Now run my tiger style queue.", "/tigerstyle"),
            (
                "Review this with the thermonuclear review skills.",
                "/thermo-nuclear-review Review this",
            ),
            ("Look at the triage skill.", "/triage Look at"),
            ("slash triage queue", "/triage queue"),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
        for said in [
            "Look at the triage queue.",
            "Clear the code review queue.",
            "What is in the research queue?",
            "Improve my research skills.",
            "Rate it on the code review scale.",
            "Work through the triage queue.",
        ] {
            assert_eq!(written_for(said), said);
        }
    }

    #[test]
    fn the_closest_of_two_names_wins() {
        assert_eq!(
            written_for("the thermo nuclear code quality review skill"),
            "/thermo-nuclear-code-quality-review"
        );
        assert_eq!(
            written_for("the thermo nuclear review skill"),
            "/thermo-nuclear-review"
        );
        assert_eq!(written_for("the grilling skill"), "/grilling");
        assert_eq!(written_for("the grill me skill"), "/grill-me");
    }

    #[test]
    fn a_skill_of_yours_wins_over_a_plugin_s() {
        let both = skills(&["handoff", "tools:handoff"]);
        assert_eq!(written("the handoff skill", &both).to_string(), "/handoff");
        let two = skills(&["other:handoff", "tools:handoff"]);
        assert_eq!(
            written("the handoff skill", &two).to_string(),
            "the handoff skill"
        );
        assert_eq!(
            written("the tools handoff skill", &two).to_string(),
            "/tools:handoff"
        );
    }

    #[test]
    fn a_skill_after_the_first_is_written_by_name() {
        for (said, command) in [
            (
                "Run the tiger style skill and then the grilling skill.",
                "/tigerstyle and then the `grilling` skill.",
            ),
            (
                "slash thermonuclear review and slash tiger style on this",
                "/thermo-nuclear-review and the `tigerstyle` skill on this",
            ),
            (
                "slash tiger style, then slash grilling, and stop",
                "/tigerstyle then the `grilling` skill, and stop",
            ),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
    }

    #[test]
    fn a_later_skill_only_you_can_run_stays_as_it_was_said() {
        let mut list = skills(&["handoff", "tigerstyle"]);
        list[0].runs = Runs::OnlyForYou;
        let later = written(
            "Run the tiger style skill and then the handoff skill.",
            &list,
        );
        assert_eq!(later.to_string(), "/tigerstyle and then the handoff skill.");
        assert_eq!(later.only_first, [list[0].name.clone()]);
        let first = written("Use the handoff skill, then the tiger style skill.", &list);
        assert_eq!(first.to_string(), "/handoff then the `tigerstyle` skill.");
        assert_eq!(first.only_first, []);
    }

    #[test]
    fn what_was_said_around_a_name_still_reads() {
        for (said, command) in [
            (
                "Fix the tests. Then use the tiger style skill. Then open a PR.",
                "/tigerstyle Fix the tests. Then open a PR.",
            ),
            (
                "Review this with the tiger style skill. Open a PR after.",
                "/tigerstyle Review this. Open a PR after.",
            ),
            (
                "Review this, with the tiger style skill.",
                "/tigerstyle Review this",
            ),
            (
                "Review this — tiger style skill, and fix it.",
                "/tigerstyle Review this and fix it.",
            ),
            (
                "Look in /etc/nginx with slash tiger style.",
                "/tigerstyle Look in /etc/nginx",
            ),
        ] {
            assert_eq!(written_for(said), command, "{said:?}");
        }
    }

    #[test]
    fn a_mark_is_what_can_change_what_is_written() {
        for said in [
            "the tiger style skill",
            "Slash tiger style",
            "/tigerstyle",
            "/ tigerstyle",
        ] {
            assert!(marked(said), "{said:?}");
        }
        for said in ["Run the tests.", "Look in /etc/nginx first.", "and/or", ""] {
            assert!(!marked(said), "{said:?}");
        }
    }
}
