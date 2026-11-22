use std::cell::RefCell;

use frizbee::{Config, Matcher, Scoring};

pub type Score = u16;

const TYPED_PER_TYPO: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typos {
    None,
    Forgiven,
}

impl Typos {
    pub fn found<T>(mut find: impl FnMut(Typos) -> Vec<T>) -> Vec<T> {
        let found = find(Typos::None);
        if found.is_empty() { find(Typos::Forgiven) } else { found }
    }

    fn of(self, term: &str) -> Score {
        match self {
            Typos::None => 0,
            Typos::Forgiven => {
                Score::try_from(term.chars().count() / TYPED_PER_TYPO).unwrap_or(Score::MAX)
            },
        }
    }
}

pub struct Fuzzy(RefCell<Matcher>);

impl Fuzzy {
    pub fn new(term: &str, typos: Typos) -> Self {
        Self::scored(term, typos, Scoring::default())
    }

    pub fn anywhere(term: &str, typos: Typos) -> Self {
        let scoring = Scoring::default();
        let at_start =
            Scoring { prefix_bonus: scoring.delimiter_bonus, exact_match_bonus: 0, ..scoring };
        Self::scored(term, typos, at_start)
    }

    fn scored(term: &str, typos: Typos, scoring: Scoring) -> Self {
        let config = Config::default().max_typos(Some(typos.of(term))).scoring(scoring);
        Fuzzy(RefCell::new(Matcher::new(term, &config)))
    }

    pub fn score(&self, text: &str) -> Option<Score> {
        Some(self.0.borrow_mut().match_one(text, 0)?.score)
    }

    pub fn hits(&self, text: &str) -> Option<(Score, Vec<usize>)> {
        let found = self.0.borrow_mut().match_one_indices(text, 0)?;
        let byte = |at: &u32| usize::try_from(*at).expect("a text's byte is a usize");
        let lit: Vec<usize> = found.indices.iter().map(byte).collect();
        let hit = |(at, c): &(usize, char)| {
            let bytes = *at..at + c.len_utf8();
            lit.iter().any(|byte| bytes.contains(byte))
        };
        let hits = text.char_indices().enumerate().filter(|(_, at)| hit(at));
        Some((found.score, hits.map(|(at, _)| at).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typo_in_every_four_typed_is_forgiven_and_only_then() {
        assert!(Fuzzy::new("lnueta", Typos::None).score("luneta").is_none());
        assert!(Fuzzy::new("lnueta", Typos::Forgiven).score("luneta").is_some());
        assert!(Fuzzy::new("lnu", Typos::Forgiven).score("luneta").is_none());
    }

    #[test]
    fn typos_are_forgiven_only_where_nothing_is_found_without() {
        let texts = ["luneta", "luneta-main"];
        let find = |typed: &str| {
            Typos::found(|typos| {
                let fuzzy = Fuzzy::new(typed, typos);
                texts.into_iter().filter(|text| fuzzy.score(text).is_some()).collect::<Vec<_>>()
            })
        };
        assert_eq!(find("luneta-m"), ["luneta-main"]);
        assert_eq!(find("lnueta"), ["luneta", "luneta-main"]);
        assert_eq!(find("zzz"), Vec::<&str>::new());
    }

    #[test]
    fn the_hits_are_characters_not_bytes() {
        let hits = |typed: &str, text: &str| {
            Fuzzy::new(typed, Typos::None).hits(text).map(|(_, hits)| hits)
        };
        assert_eq!(hits("fe", "café-fe"), Some(vec![5, 6]));
        assert_eq!(hits("é", "café"), Some(vec![3]));
    }

    #[test]
    fn anywhere_weighs_a_text_s_start_as_a_word_s() {
        let (start, word) = ("review-code", "code-review");
        let plain = Fuzzy::new("rev", Typos::None);
        assert!(plain.score(start) > plain.score(word));
        let anywhere = Fuzzy::anywhere("rev", Typos::None);
        assert_eq!(anywhere.score(start), anywhere.score(word));
        let anywhere = Fuzzy::anywhere("review", Typos::None);
        assert_eq!(anywhere.score("review"), anywhere.score("docs (review:4)"));
    }
}
