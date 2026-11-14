use std::sync::LazyLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const BUILT: Option<u64> = match option_env!("LUNETA_BUILT") {
    Some(seconds) => match u64::from_str_radix(seconds, 10) {
        Ok(seconds) => Some(seconds),
        Err(_) => panic!("LUNETA_BUILT is no number of seconds"),
    },
    None => None,
};

pub static SAID: LazyLock<String> = LazyLock::new(|| spelled(VERSION, BUILT));

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release(u64, u64, u64);

impl Release {
    #[must_use]
    pub fn parse(version: &str) -> Option<Release> {
        let mut numbers = version.split('.').map(|number| number.parse().ok());
        match (numbers.next(), numbers.next(), numbers.next(), numbers.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => {
                Some(Release(major, minor, patch))
            },
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Build {
    release: Release,
    built: Option<u64>,
}

impl Build {
    #[must_use]
    pub fn own() -> Option<Build> {
        Build::of(VERSION, BUILT)
    }

    #[must_use]
    pub fn of(version: &str, built: Option<u64>) -> Option<Build> {
        Some(Build { release: Release::parse(version)?, built })
    }

    #[must_use]
    pub fn parse(said: &str) -> Option<Build> {
        match said.split_once('+') {
            Some((version, built)) => Build::of(version, Some(built.parse().ok()?)),
            None => Build::of(said, None),
        }
    }
}

#[must_use]
pub fn spelled(version: &str, built: Option<u64>) -> String {
    match built {
        Some(built) => format!("{version}+{built}"),
        None => version.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_reads_back_as_it_is_spelled() {
        for built in [None, Some(1793301121)] {
            assert_eq!(Build::parse(&spelled("0.2.2", built)), Build::of("0.2.2", built));
        }
        assert_eq!(Build::parse(&SAID), Build::own());
        assert_eq!(Build::parse("0.2.2+"), None);
        assert_eq!(Build::parse("0.2.2+x"), None);
        assert_eq!(Build::parse("0.2+1"), None);
    }
}
