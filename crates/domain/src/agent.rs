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
    pub const fn name(self) -> &'static str {
        match self {
            Rung::Osc7501 => "osc-7501",
            Rung::Feed => "feed",
            Rung::Extension => "extension",
            Rung::Hook => "hook",
            Rung::Transcript => "transcript",
            Rung::Screen => "screen",
        }
    }

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
    pub const fn ladder(self) -> &'static Ladder {
        match self {
            Harness::ClaudeCode => &CLAUDE_CODE,
            Harness::Pi => &PI,
            Harness::Antigravity => &ANTIGRAVITY,
        }
    }

    pub fn rungs(self) -> impl Iterator<Item = Rung> {
        Rung::ALL.into_iter().filter(move |&rung| self.ladder().declares(rung))
    }

    #[must_use]
    pub const fn osc_7501_app(self) -> Option<&'static str> {
        match &self.ladder().osc_7501 {
            Some(osc_7501) => Some(osc_7501.app),
            None => None,
        }
    }

    #[must_use]
    pub fn from_osc_7501_app(app: &str) -> Option<Declared> {
        let harness =
            Harness::ALL.into_iter().find(|harness| harness.osc_7501_app() == Some(app))?;
        Some(Declared::of(harness, Rung::Osc7501))
    }

    const fn is(self, other: Harness) -> bool {
        self as u8 == other as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    Start,
    Prompt,
    Permission,
    Reject,
    Question,
    Interrupt,
    Finish,
    Error,
    Exit,
}

impl Step {
    pub const ALL: [Step; 9] = [
        Step::Start,
        Step::Prompt,
        Step::Permission,
        Step::Reject,
        Step::Question,
        Step::Interrupt,
        Step::Finish,
        Step::Error,
        Step::Exit,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Step::Start => "start",
            Step::Prompt => "prompt",
            Step::Permission => "permission",
            Step::Reject => "reject",
            Step::Question => "question",
            Step::Interrupt => "interrupt",
            Step::Finish => "finish",
            Step::Error => "error",
            Step::Exit => "exit",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|step| step.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tells {
    Yes,
    No { why: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Steps {
    pub start: Tells,
    pub prompt: Tells,
    pub permission: Tells,
    pub reject: Tells,
    pub question: Tells,
    pub interrupt: Tells,
    pub finish: Tells,
    pub error: Tells,
    pub exit: Tells,
}

impl Steps {
    pub const EVERY: Steps = Steps {
        start: Tells::Yes,
        prompt: Tells::Yes,
        permission: Tells::Yes,
        reject: Tells::Yes,
        question: Tells::Yes,
        interrupt: Tells::Yes,
        finish: Tells::Yes,
        error: Tells::Yes,
        exit: Tells::Yes,
    };

    #[must_use]
    pub const fn tells(&self, step: Step) -> Tells {
        match step {
            Step::Start => self.start,
            Step::Prompt => self.prompt,
            Step::Permission => self.permission,
            Step::Reject => self.reject,
            Step::Question => self.question,
            Step::Interrupt => self.interrupt,
            Step::Finish => self.finish,
            Step::Error => self.error,
            Step::Exit => self.exit,
        }
    }

    const fn are_sound(&self) -> bool {
        let mut told = 0;
        let mut index = 0;
        while index < Step::ALL.len() {
            match self.tells(Step::ALL[index]) {
                Tells::Yes => told += 1,
                Tells::No { why } => {
                    if why.is_empty() {
                        return false;
                    }
                },
            }
            index += 1;
        }
        told > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Osc7501 {
    pub app: &'static str,
    pub steps: Steps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ladder {
    pub osc_7501: Option<Osc7501>,
    pub feed: Option<Steps>,
    pub extension: Option<Steps>,
    pub hook: Option<Steps>,
    pub transcript: Option<Steps>,
    pub screen: Option<Steps>,
}

impl Ladder {
    #[must_use]
    pub const fn steps(&self, rung: Rung) -> Option<&Steps> {
        match rung {
            Rung::Osc7501 => match &self.osc_7501 {
                Some(osc_7501) => Some(&osc_7501.steps),
                None => None,
            },
            Rung::Feed => self.feed.as_ref(),
            Rung::Extension => self.extension.as_ref(),
            Rung::Hook => self.hook.as_ref(),
            Rung::Transcript => self.transcript.as_ref(),
            Rung::Screen => self.screen.as_ref(),
        }
    }

    #[must_use]
    pub const fn declares(&self, rung: Rung) -> bool {
        self.steps(rung).is_some()
    }

    const fn is_sound(&self) -> bool {
        let mut declared = 0;
        let mut index = 0;
        while index < Rung::ALL.len() {
            if let Some(steps) = self.steps(Rung::ALL[index]) {
                if !steps.are_sound() {
                    return false;
                }
                declared += 1;
            }
            index += 1;
        }
        let app_is_sound = match &self.osc_7501 {
            Some(osc_7501) => !osc_7501.app.is_empty() && osc_7501.app.is_ascii(),
            None => true,
        };
        declared > 0 && app_is_sound
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Declared {
    harness: Harness,
    rung: Rung,
}

impl Declared {
    #[must_use]
    pub const fn of(harness: Harness, rung: Rung) -> Declared {
        assert!(
            harness.ladder().declares(rung),
            "a rung is read only for a harness whose ladder declares it"
        );
        Declared { harness, rung }
    }

    #[must_use]
    pub const fn harness(self) -> Harness {
        self.harness
    }

    #[must_use]
    pub const fn rung(self) -> Rung {
        self.rung
    }

    #[must_use]
    pub const fn steps(self) -> &'static Steps {
        self.harness.ladder().steps(self.rung).expect("a declared rung has its steps")
    }

    #[must_use]
    pub const fn is(self, other: Declared) -> bool {
        self.harness.is(other.harness) && self.rung as u8 == other.rung as u8
    }
}

const NO_DONE_IN_SESSIONS: Tells = Tells::No {
    why: "Claude Code's sessions file says idle when a turn ends, finished or not",
};

const NO_ERROR_IN_SESSIONS: Tells =
    Tells::No { why: "Claude Code's sessions file has no word for a turn that failed" };

const CLAUDE_CODE: Ladder = Ladder {
    osc_7501: Some(Osc7501 { app: "claude-code", steps: Steps::EVERY }),
    feed: Some(Steps { finish: NO_DONE_IN_SESSIONS, error: NO_ERROR_IN_SESSIONS, ..Steps::EVERY }),
    extension: None,
    hook: None,
    transcript: None,
    screen: None,
};

const NO_PERMISSION_IN_PI: Tells = Tells::No { why: "pi asks no permission before a tool runs" };

const NO_REJECT_IN_PI: Tells =
    Tells::No { why: "pi asks no permission before a tool runs, so there is none to reject" };

const NO_QUESTION_IN_PI: Tells = Tells::No { why: "pi has no tool that asks the user a question" };

const PI_STEPS: Steps = Steps {
    permission: NO_PERMISSION_IN_PI,
    reject: NO_REJECT_IN_PI,
    question: NO_QUESTION_IN_PI,
    ..Steps::EVERY
};

const PI: Ladder = Ladder {
    osc_7501: Some(Osc7501 { app: "pi", steps: PI_STEPS }),
    feed: None,
    extension: Some(PI_STEPS),
    hook: None,
    transcript: None,
    screen: None,
};

const ANTIGRAVITY_HOOK: Steps = Steps {
    question: Tells::No {
        why: "agy's title hook says tool_use while its ask_question tool waits for an answer",
    },
    finish: Tells::No { why: "agy's title hook says idle when a turn ends, finished or not" },
    error: Tells::No { why: "agy's title hook says idle after a turn that failed" },
    exit: Tells::No {
        why: "agy leaves its title file behind when it exits, so the process's end drops the agent",
    },
    ..Steps::EVERY
};

const ANTIGRAVITY: Ladder = Ladder {
    osc_7501: Some(Osc7501 { app: "antigravity", steps: Steps::EVERY }),
    feed: None,
    extension: None,
    hook: Some(ANTIGRAVITY_HOOK),
    transcript: None,
    screen: None,
};

const fn ladders_are_sound() -> bool {
    let mut index = 0;
    while index < Harness::ALL.len() {
        let harness = Harness::ALL[index];
        if !harness.ladder().is_sound() {
            return false;
        }
        let mut other = index + 1;
        while other < Harness::ALL.len() {
            if same_app(harness, Harness::ALL[other]) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}

const fn same_app(one: Harness, other: Harness) -> bool {
    match (one.osc_7501_app(), other.osc_7501_app()) {
        (Some(one), Some(other)) => {
            let (one, other) = (one.as_bytes(), other.as_bytes());
            if one.len() != other.len() {
                return false;
            }
            let mut index = 0;
            while index < one.len() {
                if one[index] != other[index] {
                    return false;
                }
                index += 1;
            }
            true
        },
        _ => false,
    }
}

const _: () = assert!(
    ladders_are_sound(),
    "each harness declares a rung, each declared rung tells a step and says why it tells none of the rest, and no two harnesses share an OSC 7501 app"
);

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

    #[test]
    fn every_harness_the_api_names_is_in_all() {
        let named: Vec<Harness> = (0..=i32::from(u8::MAX))
            .filter_map(|said| api::Harness::try_from(said).ok())
            .filter(|said| *said != api::Harness::Unspecified)
            .map(|said| Harness::said(Some(said.into())).unwrap())
            .collect();
        for harness in &named {
            assert!(Harness::ALL.contains(harness), "{harness:?} is missing from Harness::ALL");
        }
        assert_eq!(named.len(), Harness::ALL.len());
    }

    #[test]
    fn each_harness_climbs_its_rungs_in_rank_order_from_osc_7501() {
        for harness in Harness::ALL {
            let rungs: Vec<Rung> = harness.rungs().collect();
            assert_ne!(rungs.len(), 0, "{harness:?}");
            assert!(rungs.windows(2).all(|pair| pair[0] < pair[1]), "{harness:?}: {rungs:?}");
            if let Some(app) = harness.osc_7501_app() {
                assert_eq!(rungs[0], Rung::Osc7501, "{harness:?}");
                let declared = Harness::from_osc_7501_app(app).unwrap();
                assert_eq!((declared.harness(), declared.rung()), (harness, Rung::Osc7501));
            } else {
                assert!(!rungs.contains(&Rung::Osc7501), "{harness:?}");
            }
        }
        assert_eq!(Harness::from_osc_7501_app("no-such-agent"), None);
    }

    #[test]
    fn each_step_has_its_own_name() {
        for step in Step::ALL {
            assert_eq!(Step::named(step.name()), Some(step));
        }
        assert!(Step::ALL.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(Step::named("nap"), None);
    }
}
