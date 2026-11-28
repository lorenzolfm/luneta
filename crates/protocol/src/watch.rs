use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use crate::v1::{self, watch_local_response, watch_response};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostVersion {
    pid: NonZeroU32,
    seq: u64,
}

impl HostVersion {
    #[must_use]
    pub const fn new(pid: NonZeroU32, seq: u64) -> Self {
        Self { pid, seq }
    }

    #[must_use]
    pub const fn pid(self) -> NonZeroU32 {
        self.pid
    }

    #[must_use]
    pub const fn seq(self) -> u64 {
        self.seq
    }

    pub fn of_api(version: v1::Version) -> Result<Self, &'static str> {
        let pid = NonZeroU32::new(version.pid).ok_or("a version with pid 0")?;
        Ok(Self {
            pid,
            seq: version.seq,
        })
    }

    #[must_use]
    pub const fn to_api(self) -> v1::Version {
        v1::Version {
            pid: self.pid.get(),
            seq: self.seq,
        }
    }
}

impl fmt::Display for HostVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.pid, self.seq)
    }
}

impl FromStr for HostVersion {
    type Err = String;

    fn from_str(spelled: &str) -> Result<Self, Self::Err> {
        let (pid, seq) = spelled
            .split_once(':')
            .ok_or_else(|| "not <pid>:<seq>".to_owned())?;
        let pid: NonZeroU32 = pid.parse().map_err(|error| format!("the pid: {error}"))?;
        let seq: u64 = seq.parse().map_err(|error| format!("the seq: {error}"))?;
        Ok(Self { pid, seq })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalVersion {
    pid: NonZeroU32,
    seq: u64,
}

impl LocalVersion {
    #[must_use]
    pub const fn new(pid: NonZeroU32, seq: u64) -> Self {
        Self { pid, seq }
    }

    #[must_use]
    pub const fn pid(self) -> NonZeroU32 {
        self.pid
    }

    #[must_use]
    pub const fn seq(self) -> u64 {
        self.seq
    }

    pub fn of_api(version: v1::LocalVersion) -> Result<Self, &'static str> {
        let pid = NonZeroU32::new(version.pid).ok_or("a version with pid 0")?;
        Ok(Self {
            pid,
            seq: version.seq,
        })
    }

    pub fn of_legacy_api(version: v1::Version) -> Result<Self, &'static str> {
        let pid = NonZeroU32::new(version.pid).ok_or("a version with pid 0")?;
        Ok(Self {
            pid,
            seq: version.seq,
        })
    }

    #[must_use]
    pub const fn to_api(self) -> v1::LocalVersion {
        v1::LocalVersion {
            pid: self.pid.get(),
            seq: self.seq,
        }
    }

    #[must_use]
    pub const fn to_legacy_api(self) -> v1::Version {
        v1::Version {
            pid: self.pid.get(),
            seq: self.seq,
        }
    }
}

impl fmt::Display for LocalVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.pid, self.seq)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VersionedHost {
    version: HostVersion,
    host: Box<v1::Host>,
}

impl VersionedHost {
    pub fn of_api(host: Box<v1::Host>) -> Result<Self, &'static str> {
        let raw = host.version.ok_or("an answer with no version")?;
        let version = HostVersion::of_api(raw).map_err(|_| "an answer with pid 0")?;
        Ok(Self { version, host })
    }

    #[must_use]
    pub const fn version(&self) -> HostVersion {
        self.version
    }

    #[must_use]
    pub fn host(&self) -> &v1::Host {
        &self.host
    }

    #[must_use]
    pub fn into_parts(self) -> (HostVersion, v1::Host) {
        (self.version, *self.host)
    }

    #[must_use]
    pub fn into_host(self) -> v1::Host {
        *self.host
    }

    #[must_use]
    pub fn into_response(self) -> v1::WatchResponse {
        v1::WatchResponse {
            answer: Some(watch_response::Answer::Host(self.host)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WatchedAfter {
    Host(VersionedHost),
    Unchanged,
}

impl WatchedAfter {
    #[must_use]
    pub fn into_response(self, after: HostVersion) -> v1::WatchResponse {
        match self {
            WatchedAfter::Host(host) => host.into_response(),
            WatchedAfter::Unchanged => v1::WatchResponse::unchanged(after),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VersionedLocalHost {
    version: LocalVersion,
    host: Box<v1::remote::Host>,
}

impl VersionedLocalHost {
    pub fn of_api(local: v1::LocalHost) -> Result<Self, &'static str> {
        let v1::LocalHost { version, host } = local;
        let raw = version.ok_or("an answer with no version")?;
        let version = LocalVersion::of_api(raw).map_err(|_| "an answer with pid 0")?;
        let host = host.ok_or("an answer with no host")?;
        Ok(Self {
            version,
            host: Box::new(host),
        })
    }

    #[must_use]
    pub const fn version(&self) -> LocalVersion {
        self.version
    }

    #[must_use]
    pub fn host(&self) -> &v1::remote::Host {
        &self.host
    }

    #[must_use]
    pub fn into_parts(self) -> (LocalVersion, v1::remote::Host) {
        (self.version, *self.host)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WatchedLocalAfter {
    Host(VersionedLocalHost),
    Unchanged,
}

impl v1::WatchResponse {
    #[must_use]
    pub fn host(version: HostVersion, mut host: v1::Host) -> Self {
        host.version = Some(version.to_api());
        v1::WatchResponse {
            answer: Some(watch_response::Answer::Host(Box::new(host))),
        }
    }

    #[must_use]
    pub fn unchanged(version: HostVersion) -> Self {
        v1::WatchResponse {
            answer: Some(watch_response::Answer::Unchanged(version.to_api())),
        }
    }

    pub fn read_host(self) -> Result<VersionedHost, &'static str> {
        match self.answer {
            Some(watch_response::Answer::Host(host)) => VersionedHost::of_api(host),
            Some(watch_response::Answer::Unchanged(_)) => {
                Err("an unchanged answer for another version")
            }
            None => Err("an empty watch answer"),
        }
    }

    pub fn read_after(self, after: HostVersion) -> Result<WatchedAfter, &'static str> {
        match self.answer {
            Some(watch_response::Answer::Host(host)) => {
                VersionedHost::of_api(host).map(WatchedAfter::Host)
            }
            Some(watch_response::Answer::Unchanged(raw)) => {
                let version =
                    HostVersion::of_api(raw).map_err(|_| "an unchanged answer with pid 0")?;
                if version == after {
                    Ok(WatchedAfter::Unchanged)
                } else {
                    Err("an unchanged answer for another version")
                }
            }
            None => Err("an empty watch answer"),
        }
    }
}

impl v1::WatchLocalResponse {
    #[must_use]
    pub fn host(version: LocalVersion, host: v1::remote::Host) -> Self {
        let local = v1::LocalHost {
            version: Some(version.to_api()),
            host: Some(host),
        };
        v1::WatchLocalResponse {
            answer: Some(watch_local_response::Answer::Host(Box::new(local))),
        }
    }

    #[must_use]
    pub fn unchanged(version: LocalVersion) -> Self {
        v1::WatchLocalResponse {
            answer: Some(watch_local_response::Answer::Unchanged(version.to_api())),
        }
    }

    pub fn read_host(self) -> Result<VersionedLocalHost, &'static str> {
        match self.answer {
            Some(watch_local_response::Answer::Host(local)) => VersionedLocalHost::of_api(*local),
            Some(watch_local_response::Answer::Unchanged(_)) => {
                Err("an unchanged answer for another version")
            }
            None => Err("an empty watch answer"),
        }
    }

    pub fn read_after(self, after: LocalVersion) -> Result<WatchedLocalAfter, &'static str> {
        match self.answer {
            Some(watch_local_response::Answer::Host(local)) => {
                VersionedLocalHost::of_api(*local).map(WatchedLocalAfter::Host)
            }
            Some(watch_local_response::Answer::Unchanged(raw)) => {
                let version =
                    LocalVersion::of_api(raw).map_err(|_| "an unchanged answer with pid 0")?;
                if version == after {
                    Ok(WatchedLocalAfter::Unchanged)
                } else {
                    Err("an unchanged answer for another version")
                }
            }
            None => Err("an empty watch answer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_version(pid: u32, seq: u64) -> HostVersion {
        HostVersion::new(NonZeroU32::new(pid).unwrap(), seq)
    }

    fn local_version(pid: u32, seq: u64) -> LocalVersion {
        LocalVersion::new(NonZeroU32::new(pid).unwrap(), seq)
    }

    fn host(version: Option<v1::Version>) -> v1::Host {
        v1::Host {
            version,
            ..v1::Host::default()
        }
    }

    #[test]
    fn host_version_parses_and_rejects_pid_zero() {
        let parsed: HostVersion = "7:42".parse().unwrap();
        assert_eq!(parsed, host_version(7, 42));
        assert_eq!(parsed.to_string(), "7:42");
        assert!("0:0".parse::<HostVersion>().is_err());
        assert!("7".parse::<HostVersion>().is_err());
        assert_eq!(
            HostVersion::of_api(v1::Version { pid: 0, seq: 1 }),
            Err("a version with pid 0")
        );
        assert_eq!(
            LocalVersion::of_api(v1::LocalVersion { pid: 0, seq: 1 }),
            Err("a version with pid 0")
        );
    }

    #[test]
    fn a_watch_response_reads_a_versioned_host_or_only_the_version_asked_unchanged() {
        let first = host_version(7, 1);
        let next = host_version(7, 2);
        let zero = v1::Version { pid: 0, seq: 1 };

        let answered = v1::WatchResponse::host(first, host(None));
        let parsed = answered.clone().read_host().unwrap();
        assert_eq!(parsed.version(), first);
        assert_eq!(parsed.host().version, Some(first.to_api()));
        assert_eq!(
            answered.clone().read_after(next),
            Ok(WatchedAfter::Host(
                VersionedHost::of_api(Box::new(host(Some(first.to_api())))).unwrap()
            ))
        );
        assert_eq!(parsed.into_response(), answered);

        let same = v1::WatchResponse::unchanged(first);
        assert_eq!(same.clone().read_after(first), Ok(WatchedAfter::Unchanged));
        assert_eq!(
            same.clone().read_after(next),
            Err("an unchanged answer for another version")
        );
        assert_eq!(
            same.read_host(),
            Err("an unchanged answer for another version")
        );
        assert_eq!(
            WatchedAfter::Unchanged.into_response(first),
            v1::WatchResponse::unchanged(first)
        );

        let unversioned = v1::WatchResponse {
            answer: Some(watch_response::Answer::Host(Box::new(host(None)))),
        };
        assert_eq!(
            unversioned.clone().read_host(),
            Err("an answer with no version")
        );
        assert_eq!(
            unversioned.read_after(first),
            Err("an answer with no version")
        );

        let zero_host = v1::WatchResponse {
            answer: Some(watch_response::Answer::Host(Box::new(host(Some(zero))))),
        };
        assert_eq!(zero_host.clone().read_host(), Err("an answer with pid 0"));
        assert_eq!(zero_host.read_after(first), Err("an answer with pid 0"));

        let zero_same = v1::WatchResponse {
            answer: Some(watch_response::Answer::Unchanged(zero)),
        };
        assert_eq!(
            zero_same.read_after(first),
            Err("an unchanged answer with pid 0")
        );

        let empty = v1::WatchResponse { answer: None };
        assert_eq!(empty.clone().read_host(), Err("an empty watch answer"));
        assert_eq!(empty.read_after(first), Err("an empty watch answer"));
    }

    #[test]
    fn a_watch_local_response_reads_a_versioned_local_host_or_only_the_version_asked_unchanged() {
        let first = local_version(7, 1);
        let next = local_version(7, 2);
        let zero = v1::LocalVersion { pid: 0, seq: 1 };
        let body = v1::remote::Host {
            unread: 3,
            ..v1::remote::Host::default()
        };

        let answered = v1::WatchLocalResponse::host(first, body.clone());
        let parsed = answered.clone().read_host().unwrap();
        assert_eq!(parsed.version(), first);
        assert_eq!(parsed.host(), &body);
        assert_eq!(
            answered.read_after(next),
            Ok(WatchedLocalAfter::Host(parsed))
        );

        let same = v1::WatchLocalResponse::unchanged(first);
        assert_eq!(
            same.clone().read_after(first),
            Ok(WatchedLocalAfter::Unchanged)
        );
        assert_eq!(
            same.clone().read_after(next),
            Err("an unchanged answer for another version")
        );
        assert_eq!(
            same.read_host(),
            Err("an unchanged answer for another version")
        );

        let no_host = v1::WatchLocalResponse {
            answer: Some(watch_local_response::Answer::Host(Box::new(
                v1::LocalHost {
                    version: Some(first.to_api()),
                    host: None,
                },
            ))),
        };
        assert_eq!(no_host.read_host(), Err("an answer with no host"));

        let zero_same = v1::WatchLocalResponse {
            answer: Some(watch_local_response::Answer::Unchanged(zero)),
        };
        assert_eq!(
            zero_same.read_after(first),
            Err("an unchanged answer with pid 0")
        );

        let empty = v1::WatchLocalResponse { answer: None };
        assert_eq!(empty.read_host(), Err("an empty watch answer"));
    }
}
