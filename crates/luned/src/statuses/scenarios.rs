use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::time::SystemTime;

use lune_domain::agent::{Declared, Harness, Rung, Step, Tells};
use lune_mux::screen::{Screen, Size};
use serde::Deserialize;
use serde_json::Value;

use super::{ActiveStatus, BlockedReason, Report, RungUpdate, Statuses};
use crate::process::{AgentId, AgentPid, Start};
use crate::{agy, claude, osc_7501, pi};

const SCROLLBACK_BYTES: usize = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Heard {
    Gone,
    Said(RungUpdate),
}

struct Reading {
    reads: Declared,
    heard: fn(Declared, Step, &Value) -> Result<Heard, String>,
}

const fn osc_7501(harness: Harness) -> Reading {
    Reading { reads: Declared::of(harness, Rung::Osc7501), heard: heard_osc_7501 }
}

const READINGS: [Reading; 6] = [
    osc_7501(Harness::ClaudeCode),
    Reading { reads: claude::READS, heard: heard_claude_sessions },
    osc_7501(Harness::Pi),
    Reading { reads: pi::READS, heard: heard_pi_extension },
    osc_7501(Harness::Antigravity),
    Reading { reads: agy::READS, heard: heard_agy_hook },
];

const fn reads_each_declared_rung_once(readings: &[Reading]) -> bool {
    let mut harness = 0;
    while harness < Harness::ALL.len() {
        let mut rung = 0;
        while rung < Rung::ALL.len() {
            let (of, at) = (Harness::ALL[harness], Rung::ALL[rung]);
            let mut reads = 0;
            let mut reading = 0;
            while reading < readings.len() {
                let read = readings[reading].reads;
                if read.harness() as u8 == of as u8 && read.rung() as u8 == at as u8 {
                    reads += 1;
                }
                reading += 1;
            }
            let wanted = if of.ladder().declares(at) { 1 } else { 0 };
            if reads != wanted {
                return false;
            }
            rung += 1;
        }
        harness += 1;
    }
    true
}

const _: () = assert!(
    reads_each_declared_rung_once(&READINGS),
    "each rung a harness's ladder declares has one Reading in READINGS, which feeds its recorded steps through the reader luned runs"
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    recorded: Recorded,
    steps: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    by: String,
    agent: String,
    on: String,
}

const FIXTURES: &str = "src/statuses/fixtures";

fn fixture_path(reads: Declared) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURES);
    folder.join(reads.harness().program()).join(format!("{}.json", reads.rung().name()))
}

fn shown(reads: Declared) -> String {
    format!("crates/luned/{FIXTURES}/{}/{}.json", reads.harness().program(), reads.rung().name())
}

fn fixture(reads: Declared) -> Result<Fixture, String> {
    let path = shown(reads);
    let record = format!("record it with `dev record-status {}`", reads.harness().program());
    let text =
        fs::read(fixture_path(reads)).map_err(|error| format!("{path}: {error}; {record}"))?;
    let fixture: Fixture =
        serde_json::from_slice(&text).map_err(|error| format!("{path}: {error}"))?;
    let Recorded { by, agent, on } = &fixture.recorded;
    for (field, said) in [("by", by), ("agent", agent), ("on", on)] {
        if said.trim().is_empty() {
            return Err(format!("{path}: recorded.{field} is empty"));
        }
    }
    if let Some(unknown) = fixture.steps.keys().find(|name| Step::named(name).is_none()) {
        return Err(format!("{path}: {unknown:?} is no step"));
    }
    Ok(fixture)
}

fn recorded_step(reads: Declared, fixture: &Fixture, step: Step) -> Result<Option<Heard>, String> {
    let Some(value) = fixture.steps.get(step.name()) else {
        return Ok(None);
    };
    let reading = READINGS
        .iter()
        .find(|reading| reading.reads.is(reads))
        .expect("READINGS reads each declared rung");
    let heard = (reading.heard)(reads, step, value)
        .map_err(|why| format!("{}: {}: {why}", shown(reads), step.name()))?;
    Ok(Some(heard))
}

fn tells(step: Step, heard: &Heard) -> bool {
    let status = match heard {
        Heard::Said(RungUpdate::Active { status, .. }) => status,
        Heard::Gone | Heard::Said(RungUpdate::Clear) => return step == Step::Exit,
    };
    match step {
        Step::Start | Step::Reject | Step::Interrupt => {
            matches!(status, ActiveStatus::Idle | ActiveStatus::Stopped)
        },
        Step::Prompt => *status == ActiveStatus::Working,
        Step::Permission => {
            matches!(status, ActiveStatus::Blocked { reason: BlockedReason::Permission, .. })
        },
        Step::Question => {
            matches!(status, ActiveStatus::Blocked { reason: BlockedReason::Question, .. })
        },
        Step::Finish => matches!(status, ActiveStatus::Done { .. }),
        Step::Error => matches!(status, ActiveStatus::Error { .. }),
        Step::Exit => false,
    }
}

fn wrong_steps(reads: Declared) -> Vec<String> {
    let fixture = match fixture(reads) {
        Ok(fixture) => fixture,
        Err(why) => return vec![why],
    };
    let mut wrong = Vec::new();
    let path = shown(reads);
    for step in Step::ALL {
        let heard = match recorded_step(reads, &fixture, step) {
            Ok(heard) => heard,
            Err(why) => {
                wrong.push(why);
                continue;
            },
        };
        let name = step.name();
        match (reads.steps().tells(step), heard) {
            (Tells::Yes, None) => wrong.push(format!(
                "{path}: {name} has no recording; record it with `dev record-status {}`, or declare in {:?}'s ladder that this rung tells no {name}, and why",
                reads.harness().program(),
                reads.harness(),
            )),
            (Tells::Yes, Some(heard)) if !tells(step, &heard) => wrong.push(format!(
                "{path}: {name} reads as {heard:?}, which does not tell {name}"
            )),
            (Tells::No { why }, Some(heard)) if tells(step, &heard) => wrong.push(format!(
                "{path}: {name} reads as {heard:?}, so the gap {why:?} in {:?}'s ladder is closed: drop it",
                reads.harness(),
            )),
            (Tells::Yes, Some(_)) | (Tells::No { .. }, _) => {},
        }
    }
    wrong
}

#[test]
fn every_declared_rung_tells_each_step_from_its_recording_or_says_why_not() {
    let wrong: Vec<String> =
        READINGS.iter().flat_map(|reading| wrong_steps(reading.reads)).collect();
    assert_eq!(wrong.join("\n"), "", "each line above is a rung that does not tell a step");
}

fn agent() -> AgentId {
    AgentId::new(AgentPid::new(42).unwrap(), Start::spelled("100".to_owned()).unwrap())
}

fn told(reads: Declared, step: Step) -> Option<(RungUpdate, ActiveStatus)> {
    if reads.steps().tells(step) != Tells::Yes {
        return None;
    }
    let fixture = fixture(reads).ok()?;
    match recorded_step(reads, &fixture, step).ok()?? {
        Heard::Said(RungUpdate::Active { status, at, subagents }) => {
            Some((RungUpdate::Active { status: status.clone(), at, subagents }, status))
        },
        Heard::Said(RungUpdate::Clear) | Heard::Gone => None,
    }
}

fn gone(reads: Declared, statuses: &mut Statuses) {
    let fixture = fixture(reads).expect("every declared rung has its fixture");
    match recorded_step(reads, &fixture, Step::Exit).expect("an exit reads") {
        Some(Heard::Said(RungUpdate::Clear)) => {
            statuses.record(Report::new(reads, agent(), RungUpdate::Clear));
        },
        Some(Heard::Said(RungUpdate::Active { .. }) | Heard::Gone) | None => {
            statuses.replace_rung(reads.rung(), Vec::new());
        },
    }
}

#[test]
fn the_highest_rung_that_tells_a_step_wins_and_the_next_takes_over_when_it_goes() {
    let mut pairs = 0;
    for harness in Harness::ALL {
        let rungs: Vec<Declared> =
            harness.rungs().map(|rung| Declared::of(harness, rung)).collect();
        for (place, &higher) in rungs.iter().enumerate() {
            for &lower in &rungs[place + 1..] {
                for step in Step::ALL {
                    let (Some((high, high_status)), Some((low, low_status))) =
                        (told(higher, step), told(lower, step))
                    else {
                        continue;
                    };
                    let mut statuses = Statuses::default();
                    statuses.record(Report::new(lower, agent(), low));
                    statuses.record(Report::new(higher, agent(), high));
                    let decided = statuses.decide(&agent());
                    assert_eq!(
                        (decided.rung(), decided.status()),
                        (Some(higher.rung()), Some(&high_status)),
                        "{harness:?} at {}: {higher:?} over {lower:?}",
                        step.name()
                    );
                    gone(higher, &mut statuses);
                    let fallen = statuses.decide(&agent());
                    assert_eq!(
                        (fallen.rung(), fallen.status()),
                        (Some(lower.rung()), Some(&low_status)),
                        "{harness:?} at {}: {lower:?} once {higher:?} goes",
                        step.name()
                    );
                    pairs += 1;
                }
            }
        }
    }
    assert!(pairs > 0, "some harness has two rungs that tell a step");
}

fn bytes_of(value: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn file_of(step: Step, value: &Value) -> Result<Option<Vec<u8>>, String> {
    match value {
        Value::Null if step == Step::Exit => Ok(None),
        Value::Null => Err("only an exit has no file".to_owned()),
        Value::Object(_) => bytes_of(value).map(Some),
        _ => Err(format!("a file's recording is an object or null, not {value}")),
    }
}

fn heard_claude_sessions(_: Declared, step: Step, value: &Value) -> Result<Heard, String> {
    let Some(bytes) = file_of(step, value)? else {
        return Ok(Heard::Gone);
    };
    claude::update_of_file(&bytes).map(Heard::Said).map_err(|error| error.to_string())
}

fn heard_pi_extension(_: Declared, step: Step, value: &Value) -> Result<Heard, String> {
    let Some(bytes) = file_of(step, value)? else {
        return Ok(Heard::Gone);
    };
    pi::update_of_file(&bytes).map(Heard::Said).map_err(|error| error.to_string())
}

fn heard_agy_hook(_: Declared, step: Step, value: &Value) -> Result<Heard, String> {
    let Some(bytes) = file_of(step, value)? else {
        return Ok(Heard::Gone);
    };
    agy::update_of_file(&bytes, SystemTime::UNIX_EPOCH)
        .map(Heard::Said)
        .map_err(|error| error.to_string())
}

fn heard_osc_7501(reads: Declared, step: Step, value: &Value) -> Result<Heard, String> {
    let Value::String(bytes) = value else {
        return Err(format!("an OSC 7501 recording is the bytes the agent wrote, not {value}"));
    };
    let side = NonZeroU16::new(80).expect("80 is not 0");
    let mut screen = Screen::new(Size { columns: side, rows: side }, SCROLLBACK_BYTES)
        .map_err(|error| format!("a screen: {error:?}"))?;
    screen.write(bytes.as_bytes()).map_err(|error| format!("writing: {error:?}"))?;
    if step == Step::Exit {
        let _ = screen.program_exited();
    }
    let Some(root) = screen.program_statuses().root else {
        return Ok(Heard::Gone);
    };
    let app = reads.harness().osc_7501_app().expect("a harness on rung 1 has its app");
    if root.app.as_ref().map(lune_protocol::program_status::AppName::as_str) != Some(app) {
        return Err(format!(
            "the record names app {:?}, and {:?}'s ladder says {app:?}",
            root.app,
            reads.harness()
        ));
    }
    Ok(Heard::Said(osc_7501::report(reads, agent(), &root, Vec::new()).update().clone()))
}
