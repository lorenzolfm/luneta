mod tests {
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::num::NonZeroU32;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use lune_domain::agent::{Declared, Harness, OscReads, TopicSource};
    use lune_domain::mux::TerminalKey;
    use lune_protocol::said::Said;
    use serde::Deserialize;
    use serde_json::Value;

    use crate::agent::{Agent, Sighting};
    use crate::core::{self, Command, Wakes};
    use crate::mux::OwnMade;
    use crate::process::{self, AgentId, AgentPid};
    use crate::statuses::tests::decided;
    use crate::statuses::{ActiveStatus, Proof, ReportedAgent, RungUpdate};
    use crate::topics::{Due, TopicKey, Topics, Wrote};
    use crate::transcripts::{Folders, SessionId, TranscriptHarness};
    use crate::{Directory, agy, claude, pi, titles};

    const SESSION_ID: &str = "9c696c75-660f-4097-934f-e1cc290e4bd1";

    const FIXTURES: &str = "src/topics/fixtures";

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        first_prompt: Vec<Value>,
        follow_up: Vec<Value>,
        expected_first_asked: String,
        expected_step: String,
        expected_follow_up_asked: String,
    }

    struct TranscriptScenario {
        harness: Harness,
        reads: Declared,
        write_records: fn(&Path, &SessionId, &[Value]),
        folders: fn(&Path) -> Folders,
    }

    const TRANSCRIPT_SCENARIOS: [TranscriptScenario; 2] = [
        TranscriptScenario {
            harness: Harness::ClaudeCode,
            reads: claude::READS,
            write_records: write_claude_records,
            folders: |root| Folders {
                claude: Some(root.to_owned()),
                agy: None,
            },
        },
        TranscriptScenario {
            harness: Harness::Antigravity,
            reads: agy::READS,
            write_records: write_agy_records,
            folders: |root| Folders {
                claude: None,
                agy: Some(root.to_owned()),
            },
        },
    ];

    const fn covers_each_transcript_harness_once(scenarios: &[TranscriptScenario]) -> bool {
        let mut harness = 0;
        while harness < Harness::ALL.len() {
            let of = Harness::ALL[harness];
            let mut count = 0;
            let mut index = 0;
            while index < scenarios.len() {
                if scenarios[index].harness.is(of) {
                    count += 1;
                }
                index += 1;
            }
            let wanted = if of.topics().has_transcript() { 1 } else { 0 };
            if count != wanted {
                return false;
            }
            harness += 1;
        }
        true
    }

    const _: () = assert!(
        covers_each_transcript_harness_once(&TRANSCRIPT_SCENARIOS),
        "each harness declaring transcript topics has one TranscriptScenario in TRANSCRIPT_SCENARIOS"
    );

    struct TitleScenario {
        harness: Harness,
        titled: &'static str,
        expected_topic: &'static str,
        generic: &'static str,
    }

    const TITLE_SCENARIOS: [TitleScenario; 1] = [TitleScenario {
        harness: Harness::ClaudeCode,
        titled: "✳ Fixing the tests",
        expected_topic: "Fixing the tests",
        generic: "✳ Claude Code",
    }];

    const fn covers_each_terminal_title_harness_once(scenarios: &[TitleScenario]) -> bool {
        let mut harness = 0;
        while harness < Harness::ALL.len() {
            let of = Harness::ALL[harness];
            let mut count = 0;
            let mut index = 0;
            while index < scenarios.len() {
                if scenarios[index].harness.is(of) {
                    count += 1;
                }
                index += 1;
            }
            let wanted = if of.topics().has_terminal_title() {
                1
            } else {
                0
            };
            if count != wanted {
                return false;
            }
            harness += 1;
        }
        true
    }

    const _: () = assert!(
        covers_each_terminal_title_harness_once(&TITLE_SCENARIOS),
        "each harness declaring terminal title topics has one TitleScenario in TITLE_SCENARIOS"
    );

    fn fixture_path(harness: Harness) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(FIXTURES)
            .join(format!("{}.json", harness.program()))
    }

    fn load_fixture(harness: Harness) -> Fixture {
        let path = fixture_path(harness);
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn append_jsonl(path: &Path, records: &[Value]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
    }

    fn write_claude_records(root: &Path, id: &SessionId, records: &[Value]) {
        let path = root
            .join("projects/-w")
            .join(format!("{}.jsonl", id.as_str()));
        append_jsonl(&path, records);
    }

    fn write_agy_records(root: &Path, id: &SessionId, records: &[Value]) {
        let path = root
            .join("brain")
            .join(id.as_str())
            .join(".system_generated/logs/transcript.jsonl");
        append_jsonl(&path, records);
    }

    fn began() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_mins(1)
    }

    fn seated_agent(harness: Harness, reads: Declared, id: &str) -> Agent {
        let entrypoint = harness
            .to_api()
            .prompt_word()
            .and_then(|word| Said::new(word.to_owned()));
        Agent {
            pid: AgentPid::new(2).unwrap(),
            harness,
            session_id: Said::new(id.to_owned()),
            cwd: Directory::parse("/w".to_owned()).unwrap(),
            status: decided(
                reads,
                ActiveStatus::Working,
                Some(began() + Duration::from_secs(2)),
            ),
            started_at: Some(began() + Duration::from_secs(1)),
            name: None,
            entrypoint,
            terminal: Some(TerminalKey {
                id: NonZeroU32::MIN,
                made: OwnMade::from_nanos(60),
            }),
        }
    }

    fn verify_transcript_scenario(scenario: &TranscriptScenario) {
        let harness = scenario.harness;
        let reader =
            TranscriptHarness::of(harness).expect("declared transcript harness has a reader");
        assert_eq!(reader.harness(), harness);
        let fixture = load_fixture(harness);
        let scratch = tempfile::tempdir().unwrap();
        let id = SessionId::parse(SESSION_ID).unwrap();
        let folders = (scenario.folders)(scratch.path());
        assert!(folders.any(), "{harness:?}: folders reports present");
        (scenario.write_records)(scratch.path(), &id, &fixture.first_prompt);
        assert!(
            folders.find(reader, &id).is_some(),
            "{harness:?}: transcript is found on disk"
        );

        let mut topics = Topics::new(folders, began());
        let agent = [seated_agent(harness, scenario.reads, SESSION_ID)];
        let due = topics.look(&agent, true);
        let [Due { key, prompt }] = due.as_slice() else {
            panic!("{harness:?}: expected one due prompt, got {due:?}");
        };
        let expected_key = TopicKey::new(reader, id.clone());
        assert_eq!(*key, expected_key);
        assert!(
            prompt.contains(&format!(
                "The developer last asked it: {}",
                fixture.expected_first_asked
            )),
            "{harness:?}: {prompt}"
        );
        assert!(
            prompt.contains(&fixture.expected_step),
            "{harness:?}: {prompt}"
        );

        let first_topic = Said::new("Sidebar fold".to_owned()).unwrap();
        topics.wrote(Wrote {
            key: expected_key.clone(),
            said: Ok(first_topic.clone()),
        });
        assert_eq!(
            topics.topics(),
            HashMap::from([(expected_key.clone(), first_topic)])
        );

        (scenario.write_records)(scratch.path(), &id, &fixture.follow_up);
        let next = topics.look(&agent, true);
        let [
            Due {
                key: next_key,
                prompt: next_prompt,
            },
        ] = next.as_slice()
        else {
            panic!("{harness:?}: expected follow-up due prompt, got {next:?}");
        };
        assert_eq!(*next_key, expected_key);
        assert!(
            next_prompt.contains("The topic the row goes by now: Sidebar fold"),
            "{harness:?}: {next_prompt}"
        );
        assert!(
            next_prompt.contains(&format!(
                "The developer last asked it: {}",
                fixture.expected_follow_up_asked
            )),
            "{harness:?}: {next_prompt}"
        );
    }

    fn verify_title_scenario(scenario: &TitleScenario) {
        let parsed = titles::titled(scenario.titled).expect("sample title parses");
        assert_eq!(
            parsed.topic.as_ref().map(Said::as_str),
            Some(scenario.expected_topic),
            "{:?}",
            scenario.harness
        );
        let generic =
            titles::titled(scenario.generic).expect("generic title is recognized as agent");
        assert_eq!(generic.topic, None, "{:?}", scenario.harness);
    }

    #[test]
    fn every_harness_declares_its_topic_source_and_declared_sources_produce_topics() {
        for harness in Harness::ALL {
            match harness.topics() {
                TopicSource::Transcript { terminal_title } => {
                    let scenario = TRANSCRIPT_SCENARIOS
                        .iter()
                        .find(|scenario| scenario.harness == harness)
                        .expect("declared transcript harness has a scenario");
                    verify_transcript_scenario(scenario);
                    if terminal_title {
                        let title = TITLE_SCENARIOS
                            .iter()
                            .find(|scenario| scenario.harness == harness)
                            .expect("declared terminal title harness has a scenario");
                        verify_title_scenario(title);
                    }
                }
                TopicSource::TerminalTitle => {
                    assert_eq!(TranscriptHarness::of(harness), None);
                    let title = TITLE_SCENARIOS
                        .iter()
                        .find(|scenario| scenario.harness == harness)
                        .expect("declared terminal title harness has a scenario");
                    verify_title_scenario(title);
                }
                TopicSource::No { why } => {
                    assert!(!why.trim().is_empty(), "{harness:?}: gap has no reason");
                    assert_eq!(TranscriptHarness::of(harness), None);
                    let reads = match harness {
                        Harness::ClaudeCode => claude::READS,
                        Harness::Pi => pi::READS,
                        Harness::Antigravity => agy::READS,
                        Harness::Codex => OscReads::of(Harness::Codex).declared(),
                    };
                    let mut topics = Topics::new(Folders::default(), began());
                    let agent = [seated_agent(harness, reads, SESSION_ID)];
                    assert_eq!(
                        topics.look(&agent, true),
                        [],
                        "{harness:?}: ignored by topics"
                    );
                }
            }
        }
    }

    #[test]
    fn core_attaches_topics_for_each_transcript_harness_without_cross_harness_collision() {
        let wake = || mpsc::sync_channel(1).0;
        let core = core::start(Wakes {
            git: wake(),
            github: wake(),
            projects: wake(),
        })
        .unwrap();
        let start_id = process::start(std::process::id()).unwrap();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);

        let report = |pid: u32, reads: Declared, terminal_id: u32| {
            let id = AgentId::new(AgentPid::new(pid).unwrap(), start_id.clone());
            let entrypoint = reads
                .harness()
                .to_api()
                .prompt_word()
                .and_then(|word| Said::new(word.to_owned()));
            let sighting = Sighting {
                session_id: Said::new(SESSION_ID.to_owned()),
                cwd: Directory::parse("/w".to_owned()).unwrap(),
                started_at: Some(at),
                name: None,
                entrypoint,
                terminal: Some(TerminalKey {
                    id: NonZeroU32::new(terminal_id).unwrap(),
                    made: OwnMade::from_nanos(60),
                }),
            };
            let working = RungUpdate::Active {
                status: ActiveStatus::Working,
                at: Some(at),
            };
            ReportedAgent::new(reads, id, sighting, working, Proof::Started { written: at })
        };

        core.send(Command::Agents(claude::Scan {
            agents: vec![report(101, claude::READS, 1)],
            unread: Vec::new(),
        }))
        .unwrap();
        core.send(Command::AgyAgents(vec![report(102, agy::READS, 2)]))
            .unwrap();
        core.send(Command::PiAgents(vec![report(103, pi::READS, 3)]))
            .unwrap();

        let session = SessionId::parse(SESSION_ID).unwrap();
        let claude_topic = Said::new("Claude work".to_owned()).unwrap();
        let agy_topic = Said::new("Antigravity work".to_owned()).unwrap();
        let sent = HashMap::from([
            (
                TopicKey::new(TranscriptHarness::Claude, session.clone()),
                claude_topic,
            ),
            (
                TopicKey::new(TranscriptHarness::Antigravity, session),
                agy_topic,
            ),
        ]);
        core.send(Command::Topics(sent)).unwrap();

        let listed = core::ask(&core, Command::List).unwrap().agents;
        let topic_of = |pid: u32| {
            listed
                .iter()
                .find(|agent| agent.pid == pid)
                .and_then(|agent| agent.topic.as_deref())
        };
        assert_eq!(topic_of(101), Some("Claude work"));
        assert_eq!(topic_of(102), Some("Antigravity work"));
        assert_eq!(topic_of(103), None);
    }
}
