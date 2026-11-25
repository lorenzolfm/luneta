use lune_protocol::v1 as api;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    Osc7501 = 1,
    Feed = 2,
    Extension = 3,
    Hook = 4,
    Transcript = 5,
    Screen = 6,
}

impl Rung {
    pub const ALL: [Rung; 6] = [
        Rung::Osc7501,
        Rung::Feed,
        Rung::Extension,
        Rung::Hook,
        Rung::Transcript,
        Rung::Screen,
    ];

    #[must_use]
    pub const fn to_api(self) -> api::StatusRung {
        match self {
            Rung::Osc7501 => api::StatusRung::Osc7501,
            Rung::Feed => api::StatusRung::Feed,
            Rung::Extension => api::StatusRung::Extension,
            Rung::Hook => api::StatusRung::Hook,
            Rung::Transcript => api::StatusRung::Transcript,
            Rung::Screen => api::StatusRung::Screen,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    ClaudeCode,
    Pi,
    Antigravity,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::ClaudeCode, Harness::Pi, Harness::Antigravity];

    pub fn said(said: Option<i32>) -> Result<Harness, String> {
        match said.map(api::Harness::try_from) {
            None | Some(Ok(api::Harness::ClaudeCode)) => Ok(Harness::ClaudeCode),
            Some(Ok(api::Harness::Pi)) => Ok(Harness::Pi),
            Some(Ok(api::Harness::Antigravity)) => Ok(Harness::Antigravity),
            Some(Ok(api::Harness::Unspecified) | Err(_)) => {
                Err("a harness this luned does not know".to_owned())
            },
        }
    }

    #[must_use]
    pub fn to_api(self) -> api::Harness {
        match self {
            Harness::ClaudeCode => api::Harness::ClaudeCode,
            Harness::Pi => api::Harness::Pi,
            Harness::Antigravity => api::Harness::Antigravity,
        }
    }

    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude",
            Harness::Pi => "pi",
            Harness::Antigravity => "agy",
        }
    }

    #[must_use]
    pub const fn resumes(self) -> &'static str {
        match self {
            Harness::ClaudeCode | Harness::Antigravity => "--resume",
            Harness::Pi => "--session",
        }
    }

    #[must_use]
    pub const fn osc_7501_app(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => Some("claude-code"),
            Harness::Pi => Some("pi"),
            Harness::Antigravity => Some("antigravity"),
        }
    }

    #[must_use]
    pub fn from_osc_7501_app(app: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|harness| harness.osc_7501_app() == Some(app))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_names_a_harness_this_luned_knows() {
        assert_eq!(Harness::said(None), Ok(Harness::ClaudeCode));
        for harness in [Harness::ClaudeCode, Harness::Pi, Harness::Antigravity] {
            assert_eq!(Harness::said(Some(harness.to_api().into())), Ok(harness));
        }
        for unknown in [api::Harness::Unspecified.into(), 99] {
            assert!(Harness::said(Some(unknown)).is_err(), "{unknown}");
        }
    }
}
