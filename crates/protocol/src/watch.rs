use crate::v1::{self, watch_response::Answer};

#[derive(Clone, Debug, PartialEq)]
pub enum Watched {
    Host(v1::Version, Box<v1::Host>),
    Unchanged(v1::Version),
}

impl v1::WatchResponse {
    #[must_use]
    pub fn host(host: v1::Host) -> Self {
        assert!(host.version.is_some(), "a watched host names its version");
        v1::WatchResponse { answer: Some(Answer::Host(Box::new(host))) }
    }

    #[must_use]
    pub fn unchanged(version: v1::Version) -> Self {
        v1::WatchResponse { answer: Some(Answer::Unchanged(version)) }
    }

    pub fn read(self, after: Option<v1::Version>) -> Result<Watched, &'static str> {
        match self.answer {
            Some(Answer::Host(host)) => {
                let version = host.version.ok_or("an answer with no version")?;
                Ok(Watched::Host(version, host))
            },
            Some(Answer::Unchanged(version)) if after == Some(version) => {
                Ok(Watched::Unchanged(version))
            },
            Some(Answer::Unchanged(_)) => Err("an unchanged answer for another version"),
            None => Err("an empty watch answer"),
        }
    }

    pub fn read_host(self) -> Result<(v1::Version, v1::Host), &'static str> {
        match self.read(None)? {
            Watched::Host(version, host) => Ok((version, *host)),
            Watched::Unchanged(_) => unreachable!("read(None) refuses an unchanged answer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(pid: u32, seq: u64) -> v1::Version {
        v1::Version { pid, seq }
    }

    fn host(version: Option<v1::Version>) -> v1::Host {
        v1::Host { version, ..v1::Host::default() }
    }

    #[test]
    fn a_watch_response_reads_a_versioned_host_or_only_the_version_asked_unchanged() {
        let first = version(7, 1);
        let next = version(7, 2);

        let answered = v1::WatchResponse::host(host(Some(first)));
        assert_eq!(answered.clone().read(None), Ok(Watched::Host(first, Box::new(host(Some(first))))));
        assert_eq!(answered.clone().read(Some(next)), Ok(Watched::Host(first, Box::new(host(Some(first))))));
        assert_eq!(answered.read_host(), Ok((first, host(Some(first)))));

        let same = v1::WatchResponse::unchanged(first);
        assert_eq!(same.clone().read(Some(first)), Ok(Watched::Unchanged(first)));
        assert_eq!(same.clone().read(None), Err("an unchanged answer for another version"));
        assert_eq!(same.clone().read(Some(next)), Err("an unchanged answer for another version"));
        assert_eq!(same.read_host(), Err("an unchanged answer for another version"));

        let unversioned = v1::WatchResponse { answer: Some(Answer::Host(Box::new(host(None)))) };
        assert_eq!(unversioned.read(None), Err("an answer with no version"));

        let empty = v1::WatchResponse { answer: None };
        assert_eq!(empty.read(Some(first)), Err("an empty watch answer"));
    }
}
