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
    pub const fn is(self, other: Rung) -> bool {
        self as u8 == other as u8
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

    #[must_use]
    pub const fn topics(self) -> TopicSource {
        match self {
            Harness::ClaudeCode => CLAUDE_CODE_TOPICS,
            Harness::Pi => PI_TOPICS,
            Harness::Antigravity => ANTIGRAVITY_TOPICS,
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
    pub fn from_osc_7501_app(app: &str) -> Option<OscReads> {
        let harness =
            Harness::ALL.into_iter().find(|harness| harness.osc_7501_app() == Some(app))?;
        Some(OscReads::of(harness))
    }

    #[must_use]
    pub const fn is(self, other: Harness) -> bool {
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
pub enum TopicSource {
    Transcript { terminal_title: bool },
    TerminalTitle,
    No { why: &'static str },
}

impl TopicSource {
    #[must_use]
    pub const fn has_transcript(self) -> bool {
        matches!(self, TopicSource::Transcript { .. })
    }

    #[must_use]
    pub const fn has_terminal_title(self) -> bool {
        matches!(self, TopicSource::Transcript { terminal_title: true } | TopicSource::TerminalTitle)
    }

    const fn gap_explained(self) -> bool {
        match self {
            TopicSource::Transcript { .. } | TopicSource::TerminalTitle => true,
            TopicSource::No { why } => !why.is_empty(),
        }
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

    const fn tell_a_step(&self) -> bool {
        let mut index = 0;
        while index < Step::ALL.len() {
            if matches!(self.tells(Step::ALL[index]), Tells::Yes) {
                return true;
            }
            index += 1;
        }
        false
    }

    const fn say_why_of_each_gap(&self) -> bool {
        let mut index = 0;
        while index < Step::ALL.len() {
            if let Tells::No { why } = self.tells(Step::ALL[index])
                && why.is_empty()
            {
                return false;
            }
            index += 1;
        }
        true
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

    const fn holds(&self, sound: Sound) -> bool {
        match sound {
            Sound::RungDeclared => self.declares_a_rung(),
            Sound::StepTold => self.each_rung_tells_a_step(),
            Sound::GapExplained => self.each_rung_says_why_of_each_gap(),
            Sound::AppNamed => match &self.osc_7501 {
                Some(osc_7501) => !osc_7501.app.is_empty(),
                None => true,
            },
            Sound::AppAscii => match &self.osc_7501 {
                Some(osc_7501) => osc_7501.app.is_ascii(),
                None => true,
            },
        }
    }

    const fn declares_a_rung(&self) -> bool {
        let mut index = 0;
        while index < Rung::ALL.len() {
            if self.declares(Rung::ALL[index]) {
                return true;
            }
            index += 1;
        }
        false
    }

    const fn each_rung_tells_a_step(&self) -> bool {
        let mut index = 0;
        while index < Rung::ALL.len() {
            if let Some(steps) = self.steps(Rung::ALL[index])
                && !steps.tell_a_step()
            {
                return false;
            }
            index += 1;
        }
        true
    }

    const fn each_rung_says_why_of_each_gap(&self) -> bool {
        let mut index = 0;
        while index < Rung::ALL.len() {
            if let Some(steps) = self.steps(Rung::ALL[index])
                && !steps.say_why_of_each_gap()
            {
                return false;
            }
            index += 1;
        }
        true
    }
}

#[derive(Clone, Copy)]
enum Sound {
    RungDeclared,
    StepTold,
    GapExplained,
    AppNamed,
    AppAscii,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OscReads(Declared);

impl OscReads {
    #[must_use]
    pub const fn of(harness: Harness) -> OscReads {
        OscReads(Declared::of(harness, Rung::Osc7501))
    }

    #[must_use]
    pub const fn declared(self) -> Declared {
        self.0
    }

    #[must_use]
    pub const fn harness(self) -> Harness {
        self.0.harness
    }
}

const NO_FINISH_IN_SESSIONS: Tells = Tells::No {
    why: "Claude Code's sessions file says idle when a turn ends, finished or not",
};

const NO_ERROR_IN_SESSIONS: Tells =
    Tells::No { why: "Claude Code's sessions file has no word for a turn that failed" };

const CLAUDE_CODE: Ladder = Ladder {
    osc_7501: Some(Osc7501 { app: "claude-code", steps: Steps::EVERY }),
    feed: Some(Steps {
        finish: NO_FINISH_IN_SESSIONS,
        error: NO_ERROR_IN_SESSIONS,
        ..Steps::EVERY
    }),
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

const CLAUDE_CODE_TOPICS: TopicSource = TopicSource::Transcript { terminal_title: true };

const PI_TOPICS: TopicSource =
    TopicSource::No { why: "pi's session transcripts are not read by luned yet" };

const ANTIGRAVITY_TOPICS: TopicSource = TopicSource::Transcript { terminal_title: false };

const fn every_topic_gap_explained() -> bool {
    let mut index = 0;
    while index < Harness::ALL.len() {
        if !Harness::ALL[index].topics().gap_explained() {
            return false;
        }
        index += 1;
    }
    true
}

const fn every_ladder(sound: Sound) -> bool {
    let mut index = 0;
    while index < Harness::ALL.len() {
        if !Harness::ALL[index].ladder().holds(sound) {
            return false;
        }
        index += 1;
    }
    true
}

const fn apps_are_distinct() -> bool {
    let mut index = 0;
    while index < Harness::ALL.len() {
        let harness = Harness::ALL[index];
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

const _: () = assert!(every_ladder(Sound::RungDeclared), "each harness's ladder declares a rung");

const _: () = assert!(every_ladder(Sound::StepTold), "each declared rung tells at least one step");

const _: () = assert!(
    every_ladder(Sound::GapExplained),
    "each step a declared rung does not tell says why in Tells::No"
);

const _: () = assert!(every_ladder(Sound::AppNamed), "each OSC 7501 app has a name");

const _: () = assert!(every_ladder(Sound::AppAscii), "each OSC 7501 app is ASCII");

const _: () = assert!(apps_are_distinct(), "no two harnesses share an OSC 7501 app");

const _: () = assert!(
    every_topic_gap_explained(),
    "each harness with no topic source says why in TopicSource::No"
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
                let declared = Harness::from_osc_7501_app(app).unwrap().declared();
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

    #[test]
    fn each_harness_declares_its_topic_source_or_explains_its_gap() {
        for harness in Harness::ALL {
            match harness.topics() {
                TopicSource::Transcript { .. } | TopicSource::TerminalTitle => {},
                TopicSource::No { why } => {
                    assert!(!why.trim().is_empty(), "{harness:?}: gap has no explanation");
                },
            }
        }
        assert!(Harness::ClaudeCode.topics().has_transcript());
        assert!(Harness::ClaudeCode.topics().has_terminal_title());
        assert!(Harness::Antigravity.topics().has_transcript());
        assert!(!Harness::Antigravity.topics().has_terminal_title());
        assert!(!Harness::Pi.topics().has_transcript());
        assert!(!Harness::Pi.topics().has_terminal_title());
    }
}
