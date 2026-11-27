use std::borrow::Borrow;
use std::fmt;

pub const NAME_BYTES_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        let name = SessionName::listed(name)?;
        if name.0.len() > NAME_BYTES_MAX {
            return Err("name must be shorter than 61 bytes");
        }
        if name.0.starts_with('-') {
            return Err("name cannot start with '-'");
        }
        Ok(name)
    }

    pub fn listed(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        Ok(SessionName(name.to_owned()))
    }

    #[must_use]
    pub fn with_postfix(&self, n: usize) -> Self {
        let postfix = format!("-{n}");
        let mut end = NAME_BYTES_MAX
            .saturating_sub(postfix.len())
            .min(self.0.len());
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        SessionName(format!("{}{postfix}", &self.0[..end]))
    }

    #[must_use]
    pub fn free(&self, names: &[&str]) -> Self {
        let taken = |name: &SessionName| names.contains(&name.as_str());
        std::iter::once(self.clone())
            .chain((2..names.len() + 3).map(|n| self.with_postfix(n)))
            .find(|name| !taken(name))
            .expect("one of more names than there are sessions is free")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SessionName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_postfix_makes_room_for_itself_between_characters() {
        let Ok(name) = SessionName::parse("lune-main") else {
            panic!("a name lune takes");
        };
        assert_eq!(name.with_postfix(2).as_str(), "lune-main-2");
        let Ok(long) = SessionName::parse(&format!("{}é", "x".repeat(NAME_BYTES_MAX - 3))) else {
            panic!("a name lune takes");
        };
        let postfixed = long.with_postfix(12);
        assert_eq!(
            postfixed.as_str(),
            format!("{}-12", "x".repeat(NAME_BYTES_MAX - 3))
        );
        assert!(SessionName::parse(postfixed.as_str()).is_ok());
    }

    #[test]
    fn a_name_that_names_no_file_is_no_row_s() {
        for name in ["../out", "..", ".", ""] {
            assert!(SessionName::parse(name).is_err(), "{name:?}");
            assert!(SessionName::listed(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_name_that_starts_with_a_dash_is_listed_but_made_by_no_one() {
        assert!(SessionName::parse("-c").is_err());
        assert!(SessionName::listed("-c").is_ok());
    }
}
