//! A review an agent runs, as the daemon keeps it: by what its session file
//! and its transcript say, and what zellij is asked to write in its pane.

use std::fs;
use std::os::unix::net::UnixListener;
use std::process::Command;

use lunetad::api::agent::Status as Reported;
use lunetad::api::{self, MoveReviewRequest, ReviewMove, ReviewWaits};
use serde_json::json;
use tonic::Code;

use crate::agents::file_for;
use crate::host::{eventually, folder_in, start, Daemon, Host, Running};
use crate::sessions::listed;

const LINK: &str = "https://github.com/o/app/pull/7";

/// Claude Code's id for the agent's session.
const SESSION: &str = "aa-1";

/// What zellij dumps of an empty prompt box, as Claude Code 2.1.317 draws it,
/// and of one with a draft in it.
const EMPTY: &str = "\u{1b}[mold output\n\u{1b}[m\u{276f}\u{a0}\n\u{1b}[38;2;102;92;84m────\n";
const DRAFT: &str = "\u{1b}[mold output\n\u{1b}[m\u{276f}\u{a0}hello\n\u{1b}[38;2;102;92;84m────\n";

/// A prompt you typed, and what wakes an agent with no word from you.
const YOURS: &str = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"fix it"}}"#;
const WOKEN: &str =
    r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"done"}}"#;

/// What zellij is asked for each skill that goes, in the pane of the agent.
fn sent(skill: &str) -> [String; 2] {
    [
        format!("--session work action paste --pane-id terminal_2 {skill} {LINK}"),
        "--session work action send-keys --pane-id terminal_2 Enter".to_owned(),
    ]
}

/// A daemon with an agent on the first skill of a review, in the pane 2 of
/// the session `work`.
struct Reviewed {
    host: Host,
    _work: UnixListener,
    _daemon: Daemon,
    agent: Running,
    file: serde_json::Value,
}

impl Reviewed {
    /// One that has just begun, its prompt empty.
    fn new() -> Self {
        let host = Host::new();
        fs::create_dir_all(host.home.path().join(".claude/sessions")).unwrap();
        host.zellij.list("work", &json!([listed(2, "claude", (0, "Tab #1"), Some("/w"))]));
        host.zellij.dumps("work", EMPTY);
        let work = host.serve("work");
        let daemon = start(host.vars()).unwrap();
        let mut sleeps = Command::new("sleep");
        sleeps
            .arg("600")
            .env("ZELLIJ_SESSION_NAME", "work")
            .env("ZELLIJ_PANE_ID", "2")
            .env("LUNETA_REVIEW", LINK);
        let agent = Running::spawn(&mut sleeps);
        let file = file_for(SESSION, &agent, "idle");
        let reviewed = Reviewed { host, _work: work, _daemon: daemon, agent, file };
        reviewed.goes("idle");
        reviewed
    }

    /// Writes its session file, with `status` for what it is doing.
    fn goes(&self, status: &str) {
        let mut file = self.file.clone();
        file["status"] = json!(status);
        let path = self.host.home.path().join(".claude/sessions");
        let writing = path.join(format!("{}.writing", self.agent.pid()));
        fs::write(&writing, file.to_string()).unwrap();
        fs::rename(writing, path.join(format!("{}.json", self.agent.pid()))).unwrap();
    }

    /// Writes its transcript, as `records` say.
    fn says(&self, records: &[&str]) {
        let dir = self.host.home.path().join(".claude/projects/-w");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{SESSION}.jsonl")), records.join("\n")).unwrap();
    }

    /// The review its agent runs, as the daemon says, if the daemon lists it.
    fn review(&self) -> Option<api::Review> {
        let agents = self.host.host().agents.into_iter();
        agents.filter(|agent| agent.pid == self.agent.pid()).find_map(|agent| agent.review)
    }

    /// Waits for what the next skill waits for to be `waits`.
    fn comes_to(&self, waits: ReviewWaits) -> api::Review {
        eventually(&format!("the next skill waits for {waits:?}"), || {
            self.review().filter(|review| review.waits == i32::from(waits))
        })
    }

    /// Goes to work, and waits for the daemon to have read that: it reads a
    /// file as it changes, and two writes at once can read as the last.
    fn works(&self) {
        self.goes("busy");
        let busy = Some(Reported::KnownStatus(api::Status::Busy.into()));
        eventually("the agent is at work", || {
            let agents = self.host.host().agents;
            agents.iter().any(|agent| agent.status == busy).then_some(())
        });
    }

    /// The skill it is on begins and stops, which leaves the next to you.
    fn skill_done(&self) {
        self.comes_to(ReviewWaits::Skill);
        self.works();
        self.goes("idle");
        self.comes_to(ReviewWaits::You);
    }

    /// Has the daemon move the review of the agent `pid` as `does` says.
    fn asked(&self, pid: u32, does: ReviewMove) -> Result<(), tonic::Status> {
        let folder = folder_in(self.host.runtime.path()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let request = MoveReviewRequest { on: None, pid, does: does.into() };
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&folder).await.unwrap();
            daemon.move_review(request).await.map(drop)
        })
    }

    /// Has the daemon move its review as `does` says, which it does.
    fn moved(&self, does: ReviewMove) {
        self.asked(self.agent.pid(), does).unwrap();
    }

    /// What zellij was asked to write, once `count` asks came.
    fn written(&self, count: usize) -> Vec<String> {
        let written = || self.host.zellij.written();
        eventually("zellij is asked to write", || (written().len() == count).then(written))
    }
}

/// An agent carries its review, on the first of the skills. A skill's stop is
/// for you, and sends nothing. A turn that a background task woke it for, once
/// the transcript says who began it, leaves the next skill waiting for you, as
/// it did. A turn you began sends it, in the agent's pane, as you would type
/// it, and the review goes on to that one, and from there the last, whose stop
/// ends the review.
#[test]
fn a_review_goes_on_after_a_turn_you_began() {
    let reviewed = Reviewed::new();
    let first = reviewed.comes_to(ReviewWaits::Skill);
    assert_eq!((first.step, first.steps, first.link.as_str()), (1, 3, LINK));
    assert_eq!(first.skill, "/thermo-nuclear-review");
    assert_eq!(first.next.as_deref(), Some("/make-illegal-states-unrepresentable"));
    reviewed.skill_done();
    assert_eq!(reviewed.host.zellij.written(), Vec::<String>::new());

    reviewed.says(&[WOKEN]);
    reviewed.works();
    reviewed.comes_to(ReviewWaits::Turn);
    reviewed.goes("idle");
    reviewed.comes_to(ReviewWaits::You);
    assert_eq!(reviewed.host.zellij.written(), Vec::<String>::new());

    reviewed.says(&[WOKEN, YOURS]);
    reviewed.works();
    reviewed.comes_to(ReviewWaits::Turn);
    reviewed.goes("idle");
    assert_eq!(reviewed.written(2), sent("/make-illegal-states-unrepresentable"));
    let second = reviewed.comes_to(ReviewWaits::Skill);
    assert_eq!((second.step, second.skill.as_str()), (2, "/make-illegal-states-unrepresentable"));
    assert_eq!(second.next.as_deref(), Some("/tigerstyle"));

    reviewed.skill_done();
    reviewed.moved(ReviewMove::Next);
    assert_eq!(reviewed.written(4)[2..], sent("/tigerstyle"));
    let last = reviewed.comes_to(ReviewWaits::Skill);
    assert_eq!((last.step, last.next), (3, None));
    reviewed.works();
    reviewed.goes("idle");
    eventually("the review ends", || reviewed.review().is_none().then_some(()));
}

/// A key asks for the next skill with no turn of yours. What you have typed in
/// the prompt holds it back, and the row is told, until you have cleared it;
/// and a hold holds it back, until you go on.
#[test]
fn a_draft_and_a_hold_hold_the_next_skill_back() {
    let reviewed = Reviewed::new();
    reviewed.skill_done();

    reviewed.host.zellij.dumps("work", DRAFT);
    reviewed.moved(ReviewMove::Next);
    reviewed.comes_to(ReviewWaits::Draft);
    assert_eq!(reviewed.host.zellij.written(), Vec::<String>::new());

    reviewed.moved(ReviewMove::Hold);
    assert!(reviewed.review().unwrap().held);
    reviewed.host.zellij.dumps("work", EMPTY);
    reviewed.moved(ReviewMove::Resume);
    assert!(!reviewed.review().unwrap().held);
    assert_eq!(reviewed.written(2), sent("/make-illegal-states-unrepresentable"));
}

/// A pane that shows no prompt, or that zellij will not write in, holds the
/// next skill back, and the row is told.
#[test]
fn a_pane_it_cannot_write_in_says_so() {
    let reviewed = Reviewed::new();
    reviewed.skill_done();
    reviewed.host.zellij.dumps("work", "no box here\n");
    reviewed.moved(ReviewMove::Next);
    reviewed.comes_to(ReviewWaits::Prompt);

    reviewed.host.zellij.dumps("work", EMPTY);
    reviewed.host.zellij.refuse("work");
    reviewed.moved(ReviewMove::Next);
    reviewed.comes_to(ReviewWaits::Prompt);
    assert_eq!(reviewed.host.zellij.written(), Vec::<String>::new());
}

/// A key skips the next skill, and the last ends the review. A daemon refuses
/// one for an agent that runs no review, and one that says no move.
#[test]
fn a_key_skips_the_next_skill() {
    let reviewed = Reviewed::new();
    reviewed.skill_done();
    reviewed.moved(ReviewMove::Skip);
    assert_eq!(reviewed.review().unwrap().next.as_deref(), Some("/tigerstyle"));
    reviewed.moved(ReviewMove::Skip);
    assert_eq!(reviewed.review(), None);

    let pid = reviewed.agent.pid();
    let refused = reviewed.asked(pid, ReviewMove::Hold).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    let refused = reviewed.asked(pid + 1, ReviewMove::Hold).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    let refused = reviewed.asked(pid, ReviewMove::Unspecified).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument, "{refused:?}");
}
