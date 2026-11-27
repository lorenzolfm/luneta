#![expect(
    clippy::panic,
    reason = "a wait that runs out fails the test that waited, with what it waited for"
)]

use std::fmt::{self, Debug, Display};
use std::future::IntoFuture;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub mod shell;

pub const PATIENCE: Duration = Duration::from_mins(1);

pub const QUIET: Duration = Duration::from_secs(1);

pub const OUTLASTING: Duration = PATIENCE.saturating_mul(2);

const LOOK: Duration = Duration::from_millis(20);

const _: () = assert!(
    LOOK.as_millis() < QUIET.as_millis(),
    "a quiet window is looked at more than once"
);

const _: () = assert!(
    QUIET.as_millis() < PATIENCE.as_millis(),
    "a quiet window is shorter than any wait for an event"
);

const _: () = assert!(
    PATIENCE.as_nanos() < OUTLASTING.as_nanos(),
    "it outlasts any wait for an event"
);

const QUIETLY: Span = Span::of(QUIET);

#[derive(Clone, Copy, Debug)]
pub struct Span(Duration);

impl Span {
    #[must_use]
    pub const fn of(span: Duration) -> Span {
        assert!(!span.is_zero(), "a window of no time sees nothing");
        let past = span.as_nanos() >= PATIENCE.as_nanos();
        assert!(
            !past,
            "a window as long as a wait for an event waits on the clock"
        );
        Span(span)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Gap(Duration);

impl Gap {
    #[must_use]
    pub const fn of(gap: Duration) -> Gap {
        assert!(
            !gap.is_zero(),
            "writes kept apart by no time are not kept apart"
        );
        let past = gap.as_nanos() > QUIET.as_nanos();
        assert!(
            !past,
            "apart spaces writes past a settle, it is no wait for an event"
        );
        Gap(gap)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Began(Instant);

impl Began {
    #[must_use]
    pub fn now() -> Began {
        Began(Instant::now())
    }

    #[must_use]
    pub fn lasted(self) -> Lasted {
        Lasted(self.0.elapsed())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lasted(Duration);

impl Lasted {
    pub fn at_least(self, span: Duration, what: &str) {
        assert!(self.0 >= span, "{what}: it lasted {self:?}, under {span:?}");
    }

    pub fn at_most_once_a_second(self, times: usize, what: &str) {
        let most = self.0.as_secs().saturating_add(1);
        let times = u64::try_from(times).expect("a count of times fits in u64");
        assert!(times <= most, "{what}: {times} times in {self:?}");
    }

    #[must_use]
    pub fn is_under(self, span: Duration) -> bool {
        self.0 < span
    }

    #[must_use]
    pub fn beyond(self, other: Lasted) -> Lasted {
        Lasted(self.0.saturating_sub(other.0))
    }
}

impl Debug for Lasted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Patience {
    deadline: Instant,
}

impl Patience {
    #[must_use]
    pub fn begun() -> Patience {
        Patience {
            deadline: Instant::now() + PATIENCE,
        }
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

    #[must_use]
    pub fn heard_first<T, U>(
        self,
        what: impl Display,
        from: &Receiver<T>,
        mut pick: impl FnMut(T) -> Option<U>,
    ) -> U {
        loop {
            if let Some(picked) = pick(self.heard(&what, from)) {
                return picked;
            }
        }
    }

    pub async fn awaited<F: IntoFuture>(self, what: impl Display, asked: F) -> F::Output {
        let deadline = tokio::time::Instant::from_std(self.deadline);
        match tokio::time::timeout_at(deadline, asked).await {
            Ok(answer) => answer,
            Err(elapsed) => panic!("not answered in {PATIENCE:?}, {elapsed}: {what}"),
        }
    }

    pub async fn awaited_first<T, U>(
        self,
        what: impl Display,
        mut ask: impl AsyncFnMut() -> T,
        mut pick: impl FnMut(T) -> Option<U>,
    ) -> U {
        loop {
            if let Some(picked) = pick(self.awaited(&what, ask()).await) {
                return picked;
            }
        }
    }
}

pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    polled(
        || check().ok_or(()),
        |()| panic!("never came to be in {PATIENCE:?}: {what}"),
    )
}

pub fn eventually_showing<T, E: Debug>(what: &str, check: impl FnMut() -> Result<T, E>) -> T {
    polled(check, |last| {
        panic!("never came to be in {PATIENCE:?}: {what}, last {last:?}")
    })
}

fn polled<T, E>(mut check: impl FnMut() -> Result<T, E>, ran_out: impl FnOnce(E) -> T) -> T {
    blocks_no_runtime();
    let deadline = Instant::now() + PATIENCE;
    loop {
        match check() {
            Ok(found) => return found,
            Err(last) if Instant::now() >= deadline => return ran_out(last),
            Err(_) => thread::sleep(LOOK),
        }
    }
}

pub async fn eventually_async<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = check().await {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "never came to be in {PATIENCE:?}: {what}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

pub fn never<T: Debug>(what: &str, check: impl FnMut() -> Option<T>) {
    through(QUIETLY, what, check);
}

pub fn through<T: Debug>(span: Span, what: &str, check: impl FnMut() -> Option<T>) {
    if let Some(found) = seen_within(span, check) {
        panic!("{what}: {found:?}");
    }
}

pub async fn through_async<T: Debug>(span: Span, what: &str, mut check: impl FnMut() -> Option<T>) {
    let end = Instant::now() + span.0;
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

pub fn seen_within<T>(span: Span, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    blocks_no_runtime();
    let end = Instant::now() + span.0;
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

pub async fn repeated_through(span: Span, what: &str, mut step: impl AsyncFnMut()) {
    let end = Instant::now() + span.0;
    let patience = Patience {
        deadline: end + PATIENCE,
    };
    while Instant::now() < end {
        patience.awaited(what, step()).await;
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
    eventually_async(what, async || {
        child.try_wait().expect("a child's status reads")
    })
    .await
}

pub fn runs_on(what: &str, child: &mut Child) {
    never(what, || child.try_wait().expect("a child's status reads"));
}

pub fn apart(gap: Gap) {
    blocks_no_runtime();
    thread::sleep(gap.0);
}

fn blocks_no_runtime() {
    let inside = tokio::runtime::Handle::try_current().is_ok();
    assert!(
        !inside,
        "a blocking wait inside a runtime starves its tasks: wait with the async form"
    );
}
