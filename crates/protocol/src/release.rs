use std::fmt;
use std::sync::LazyLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const BUILT: Option<u64> = match option_env!("LUNE_BUILT") {
    Some(seconds) => match u64::from_str_radix(seconds, 10) {
        Ok(seconds) => Some(seconds),
        Err(_) => panic!("LUNE_BUILT is no number of seconds"),
    },
    None => None,
};

const _: () = assert!(env!("CARGO_PKG_VERSION_PRE").is_empty(), "a release has no pre-release");

pub const OWN: Build = Build {
    release: match (
        number(env!("CARGO_PKG_VERSION_MAJOR")),
        number(env!("CARGO_PKG_VERSION_MINOR")),
        number(env!("CARGO_PKG_VERSION_PATCH")),
    ) {
        (Some(major), Some(minor), Some(patch)) => Release(major, minor, patch),
        _ => panic!("CARGO_PKG_VERSION is no three numbers"),
    },
    built: BUILT,
};

pub static SAID: LazyLock<String> = LazyLock::new(|| spelled(VERSION, BUILT));

const fn number(said: &str) -> Option<u64> {
    match u64::from_str_radix(said, 10) {
        Ok(number) => Some(number),
        Err(_) => None,
    }
}

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

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Release(major, minor, patch) = self.release;
        write!(f, "{major}.{minor}.{patch}")?;
        match self.built {
            Some(built) => write!(f, "+{built}"),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Of {
    Own,
    Other(Other),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Other {
    spelled: String,
    build: Option<Build>,
}

impl Other {
    #[must_use]
    pub fn build(&self) -> Option<Build> {
        self.build
    }

    #[must_use]
    pub fn is_newer(&self) -> bool {
        self.build.is_some_and(|build| build > OWN)
    }
}

impl fmt::Display for Other {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spelled)
    }
}

#[must_use]
pub fn of(version: &str, built: Option<u64>) -> Of {
    let theirs = Build::of(version, built);
    if theirs == Some(OWN) {
        return Of::Own;
    }
    Of::Other(Other { spelled: spelled(version, built), build: theirs })
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
        assert_eq!(Build::parse(&SAID), Some(OWN));
        assert_eq!(OWN.to_string(), *SAID);
        assert_eq!(Build::parse("0.2.2+"), None);
        assert_eq!(Build::parse("0.2.2+x"), None);
        assert_eq!(Build::parse("0.2+1"), None);
    }

    #[test]
    fn only_this_build_is_own_and_another_says_how_it_is_spelled() {
        assert_eq!(of(VERSION, BUILT), Of::Own);
        let other = |version, built| match of(version, built) {
            Of::Other(other) => other,
            Of::Own => panic!("{} is another build than {}", spelled(version, built), *SAID),
        };
        let stamp = Some(BUILT.map_or(0, |built| built + 1));
        assert_eq!(other(VERSION, stamp).to_string(), spelled(VERSION, stamp));
        let unread = other("0.3.0-rc.1", BUILT);
        assert_eq!((unread.build(), unread.is_newer()), (None, false));
        assert!(other("999.0.0", BUILT).is_newer());
        assert!(!other("0.0.0", BUILT).is_newer());
    }

    #[test]
    fn any_two_builds_read_each_other_s_info() {
        use prost::Message as _;
        use prost::encoding::{bytes, string, uint32, uint64};

        use crate::v1::InfoResponse;

        let said = |zellij_folder: Option<&[u8]>| {
            let mut said = Vec::new();
            string::encode(1, &"0.3.0".to_owned(), &mut said);
            uint32::encode(2, &7, &mut said);
            if let Some(folder) = zellij_folder {
                bytes::encode(4, &folder.to_vec(), &mut said);
            }
            bytes::encode(5, &b"/home/you/.claude".to_vec(), &mut said);
            uint64::encode(6, &1_793_301_121, &mut said);
            said
        };
        let info = InfoResponse {
            version: "0.3.0".to_owned(),
            pid: 7,
            started_at: None,
            claude_folder: Some(b"/home/you/.claude".to_vec()),
            built: Some(1_793_301_121),
        };
        assert_eq!(info.encode_to_vec(), said(None), "an older lune reads this luned's");
        let older = InfoResponse::decode(said(Some(b"/tmp/zellij-1000")).as_slice());
        assert_eq!(older.unwrap(), info, "and this lune reads an older luned's");
    }
}
