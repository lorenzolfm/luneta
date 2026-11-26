use crate::v1::{self, watch_response::Answer};

#[derive(Clone, Debug, PartialEq)]
pub enum Watched {
    Host(v1::Version, Box<v1::Host>),
    Unchanged(v1::Version),
}

impl v1::WatchResponse {
    #[must_use]
    pub fn host(host: v1::Host) -> Self {
        let version = host.version.expect("a watched host names its version");
        assert_ne!(version.pid, 0, "a watched host names a running pid");
        v1::WatchResponse { answer: Some(Answer::Host(Box::new(host))) }
    }

    #[must_use]
    pub fn unchanged(version: v1::Version) -> Self {
        assert_ne!(version.pid, 0, "an unchanged watch names a running pid");
        v1::WatchResponse { answer: Some(Answer::Unchanged(version)) }
    }

    pub fn validate(&self, after: Option<v1::Version>) -> Result<(), &'static str> {
        match &self.answer {
            Some(Answer::Host(host)) => {
                let version = host.version.ok_or("an answer with no version")?;
                if version.pid == 0 {
                    return Err("an answer with pid 0");
                }
                Ok(())
            },
            Some(Answer::Unchanged(version)) => {
                if version.pid == 0 {
                    return Err("an unchanged answer with pid 0");
                }
                if after == Some(*version) {
                    Ok(())
                } else {
                    Err("an unchanged answer for another version")
                }
            },
            None => Err("an empty watch answer"),
        }
    }

    pub fn read(self, after: Option<v1::Version>) -> Result<Watched, &'static str> {
        self.validate(after)?;
        match self.answer {
            Some(Answer::Host(host)) => {
                let version = host.version.expect("validated watch host carries a version");
                assert_ne!(version.pid, 0);
                assert_eq!(host.version, Some(version));
                Ok(Watched::Host(version, host))
            },
            Some(Answer::Unchanged(version)) => {
                assert_ne!(version.pid, 0);
                assert_eq!(after, Some(version));
                Ok(Watched::Unchanged(version))
            },
            None => unreachable!("validate refuses an empty watch answer"),
        }
    }

    pub fn read_host(self) -> Result<(v1::Version, v1::Host), &'static str> {
        match self.read(None)? {
            Watched::Host(version, host) => {
                assert_eq!(host.version, Some(version));
                Ok((version, *host))
            },
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
        let zero = version(0, 1);

        let answered = v1::WatchResponse::host(host(Some(first)));
        assert_eq!(answered.validate(None), Ok(()));
        assert_eq!(answered.validate(Some(next)), Ok(()));
        assert_eq!(answered.clone().read(None), Ok(Watched::Host(first, Box::new(host(Some(first))))));
        assert_eq!(answered.clone().read(Some(next)), Ok(Watched::Host(first, Box::new(host(Some(first))))));
        assert_eq!(answered.read_host(), Ok((first, host(Some(first)))));

        let same = v1::WatchResponse::unchanged(first);
        assert_eq!(same.validate(Some(first)), Ok(()));
        assert_eq!(same.validate(None), Err("an unchanged answer for another version"));
        assert_eq!(same.clone().read(Some(first)), Ok(Watched::Unchanged(first)));
        assert_eq!(same.clone().read(None), Err("an unchanged answer for another version"));
        assert_eq!(same.clone().read(Some(next)), Err("an unchanged answer for another version"));
        assert_eq!(same.read_host(), Err("an unchanged answer for another version"));

        let unversioned = v1::WatchResponse { answer: Some(Answer::Host(Box::new(host(None)))) };
        assert_eq!(unversioned.validate(None), Err("an answer with no version"));
        assert_eq!(unversioned.read(None), Err("an answer with no version"));

        let zero_host = v1::WatchResponse { answer: Some(Answer::Host(Box::new(host(Some(zero))))) };
        assert_eq!(zero_host.validate(None), Err("an answer with pid 0"));
        assert_eq!(zero_host.read(None), Err("an answer with pid 0"));

        let zero_same = v1::WatchResponse { answer: Some(Answer::Unchanged(zero)) };
        assert_eq!(zero_same.validate(Some(zero)), Err("an unchanged answer with pid 0"));
        assert_eq!(zero_same.read(Some(zero)), Err("an unchanged answer with pid 0"));

        let empty = v1::WatchResponse { answer: None };
        assert_eq!(empty.validate(Some(first)), Err("an empty watch answer"));
        assert_eq!(empty.read(Some(first)), Err("an empty watch answer"));
    }
}
