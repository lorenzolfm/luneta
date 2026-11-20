#![expect(clippy::panic, reason = "a wait that runs out fails the test that waited, with what it waited for")]

use std::fmt::Debug;
use std::future::{Future, IntoFuture};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub const PATIENCE: Duration = Duration::from_mins(1);

pub const QUIET: Duration = Duration::from_secs(1);

const LOOK: Duration = Duration::from_millis(20);

pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "never came to be in {PATIENCE:?}: {what}");
        thread::sleep(LOOK);
    }
}

pub async fn eventually_async<T, F: Future<Output = Option<T>>>(what: &str, mut check: impl FnMut() -> F) -> T {
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

pub fn seen_within<T>(span: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
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

#[must_use]
pub fn heard<T>(what: &str, from: &Receiver<T>) -> T {
    match from.recv_timeout(PATIENCE) {
        Ok(heard) => heard,
        Err(RecvTimeoutError::Timeout) => panic!("not heard in {PATIENCE:?}: {what}"),
        Err(RecvTimeoutError::Disconnected) => panic!("gone before it was heard: {what}"),
    }
}

pub async fn awaited<F: IntoFuture>(what: &str, asked: F) -> F::Output {
    match tokio::time::timeout(PATIENCE, asked).await {
        Ok(answer) => answer,
        Err(elapsed) => panic!("not answered in {PATIENCE:?}, {elapsed}: {what}"),
    }
}

pub fn ended(what: &str, child: &mut Child) -> ExitStatus {
    eventually(what, || child.try_wait().expect("a child's status reads"))
}

pub fn runs_on(what: &str, child: &mut Child) {
    never(what, || child.try_wait().expect("a child's status reads"));
}

pub fn apart(span: Duration) {
    thread::sleep(span);
}
