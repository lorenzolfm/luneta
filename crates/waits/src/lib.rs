#![expect(clippy::panic, reason = "a wait that runs out fails the test that waited, with what it waited for")]

use std::fmt::{Debug, Display};
use std::future::IntoFuture;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub const PATIENCE: Duration = Duration::from_mins(1);

pub const QUIET: Duration = Duration::from_secs(1);

const LOOK: Duration = Duration::from_millis(20);

const _: () = assert!(LOOK.as_millis() < QUIET.as_millis(), "a quiet window is looked at more than once");

const _: () = assert!(QUIET.as_millis() < PATIENCE.as_millis(), "a quiet window is shorter than any wait for an event");

#[derive(Clone, Copy, Debug)]
pub struct Patience {
    deadline: Instant,
}

impl Patience {
    #[must_use]
    pub fn begun() -> Patience {
        Patience { deadline: Instant::now() + PATIENCE }
    }

    #[must_use]
    pub fn heard<T>(self, what: impl Display, from: &Receiver<T>) -> T {
        blocks_no_runtime();
        let left = self.deadline.saturating_duration_since(Instant::now());
        match from.recv_timeout(left) {
            Ok(heard) => heard,
            Err(RecvTimeoutError::Timeout) => panic!("not heard in {PATIENCE:?}: {what}"),
            Err(RecvTimeoutError::Disconnected) => panic!("gone before it was heard: {what}"),
        }
    }

    pub async fn awaited<F: IntoFuture>(self, what: impl Display, asked: F) -> F::Output {
        let deadline = tokio::time::Instant::from_std(self.deadline);
        match tokio::time::timeout_at(deadline, asked).await {
            Ok(answer) => answer,
            Err(elapsed) => panic!("not answered in {PATIENCE:?}, {elapsed}: {what}"),
        }
    }
}

pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    blocks_no_runtime();
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "never came to be in {PATIENCE:?}: {what}");
        thread::sleep(LOOK);
    }
}

pub async fn eventually_async<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = check().await {
            return found;
        }
        assert!(Instant::now() < deadline, "never came to be in {PATIENCE:?}: {what}");
        tokio::time::sleep(LOOK).await;
    }
}

pub fn never<T: Debug>(what: &str, check: impl FnMut() -> Option<T>) {
    through(QUIET, what, check);
}

pub fn through<T: Debug>(span: Duration, what: &str, check: impl FnMut() -> Option<T>) {
    if let Some(found) = seen_within(span, check) {
        panic!("{what}: {found:?}");
    }
}

pub async fn through_async<T: Debug>(span: Duration, what: &str, mut check: impl FnMut() -> Option<T>) {
    a_window(span);
    let end = Instant::now() + span;
    loop {
        if let Some(found) = check() {
            panic!("{what}: {found:?}");
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        tokio::time::sleep(LOOK.min(left)).await;
    }
}

pub fn seen_within<T>(span: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    blocks_no_runtime();
    a_window(span);
    let end = Instant::now() + span;
    loop {
        if let Some(found) = check() {
            return Some(found);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        thread::sleep(LOOK.min(left));
    }
}

pub async fn repeated_through(span: Duration, mut step: impl AsyncFnMut()) {
    a_window(span);
    let end = Instant::now() + span;
    while Instant::now() < end {
        step().await;
    }
}

#[must_use]
pub fn heard<T>(what: &str, from: &Receiver<T>) -> T {
    Patience::begun().heard(what, from)
}

pub async fn awaited<F: IntoFuture>(what: &str, asked: F) -> F::Output {
    Patience::begun().awaited(what, asked).await
}

pub fn ended(what: &str, child: &mut Child) -> ExitStatus {
    eventually(what, || child.try_wait().expect("a child's status reads"))
}

pub async fn ended_async(what: &str, child: &mut Child) -> ExitStatus {
    eventually_async(what, async || child.try_wait().expect("a child's status reads")).await
}

pub fn runs_on(what: &str, child: &mut Child) {
    never(what, || child.try_wait().expect("a child's status reads"));
}

pub fn apart(span: Duration) {
    blocks_no_runtime();
    assert!(!span.is_zero(), "writes kept apart by no time are not kept apart");
    assert!(span <= QUIET, "apart spaces writes past a settle, it is no wait for an event: {span:?}");
    thread::sleep(span);
}

fn a_window(span: Duration) {
    assert!(!span.is_zero(), "a window of no time sees nothing");
    assert!(span <= PATIENCE, "a window longer than any wait for an event waits on the clock: {span:?}");
}

fn blocks_no_runtime() {
    let inside = tokio::runtime::Handle::try_current().is_ok();
    assert!(!inside, "a blocking wait inside a runtime starves its tasks: wait with the async form");
}
