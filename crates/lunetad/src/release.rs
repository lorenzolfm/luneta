//! Which build of luneta and lunetad this is: the release Cargo.toml names,
//! and when the commit it was built from was made, where the flake stamped
//! it. Every commit is of one release until the next, so it is the stamp that
//! tells an install from the one before it.

use std::sync::LazyLock;

/// The release this is of, as Cargo.toml names it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// When the commit this was built from was made, in seconds since the epoch,
/// as the flake stamps each build of a commit in `LUNETA_BUILT`. None for a
/// cargo build of a checkout, and for the flake's build of a tree with
/// changes not yet committed. A stamp that is no number fails the build: the
/// const panics as it is evaluated, which is as rustc compiles it.
pub const BUILT: Option<u64> = match option_env!("LUNETA_BUILT") {
    Some(seconds) => match u64::from_str_radix(seconds, 10) {
        Ok(seconds) => Some(seconds),
        Err(_) => panic!("LUNETA_BUILT is no number of seconds"),
    },
    None => None,
};

/// This build, as `lunetad --version` and `luneta --version` say it after the
/// name: luneta asks it of the lunetad it would start, to tell whether that
/// one is newer than the daemon that runs.
pub static SAID: LazyLock<String> = LazyLock::new(|| spelled(VERSION, BUILT));

/// A release of luneta and lunetad, by its version, as `0.2.2`. Releases are
/// ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release(u64, u64, u64);

impl Release {
    /// The release `version` names, or none: a version of luneta's is three
    /// numbers and no more.
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

/// A build of luneta and lunetad: its release, and when the commit it was
/// built from was made, where the flake stamped it. Builds are ordered by
/// their release first, as the fields are, and then by their stamp, where one
/// with none is older than any with one: a cargo build of a checkout is older
/// than an install of its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Build {
    release: Release,
    built: Option<u64>,
}

impl Build {
    /// This build, where Cargo.toml names a release.
    #[must_use]
    pub fn own() -> Option<Build> {
        Build::of(VERSION, BUILT)
    }

    /// The build of the release `version` names, stamped `built`, as a daemon
    /// says the two.
    #[must_use]
    pub fn of(version: &str, built: Option<u64>) -> Option<Build> {
        Some(Build { release: Release::parse(version)?, built })
    }

    /// The build `said` names, as `spelled` spells one.
    #[must_use]
    pub fn parse(said: &str) -> Option<Build> {
        match said.split_once('+') {
            Some((version, built)) => Build::of(version, Some(built.parse().ok()?)),
            None => Build::of(said, None),
        }
    }
}

/// The build of the release `version` names, stamped `built`, as `lunetad
/// --version` and `luneta info` say it: the version, and after a `+` the
/// stamp where there is one, as `0.2.2+1793301121`. `Build::parse` reads it
/// back.
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

    /// A build reads back as `lunetad --version` spells it, stamped or not,
    /// and this one too, however it was built. What has no number for its
    /// stamp names no build.
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
