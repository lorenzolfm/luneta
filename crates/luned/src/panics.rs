use std::cell::Cell;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle, Scope, ScopedJoinHandle};

use crate::log;

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

static HOOKED: AtomicBool = AtomicBool::new(false);

pub const ABORTS: &str = "a thread panicked, so luned aborts";

pub struct Hooked(());

pub fn abort_on_panic() -> Hooked {
    assert!(!HOOKED.swap(true, Ordering::Relaxed), "the hook that aborts is set once");
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        log::panicked(panic);
        default(panic);
        if !CONTAINED.get() {
            log::say(ABORTS);
            process::abort();
        }
    }));
    Hooked(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Panicked;

pub fn contained<T>(run: impl FnOnce() -> T) -> Result<T, Panicked> {
    let was = CONTAINED.replace(true);
    let ran = panic::catch_unwind(AssertUnwindSafe(run));
    let held = CONTAINED.replace(was);
    assert!(held, "a contained run leaves its thread contained as it found it");
    ran.map_err(|_| Panicked)
}

pub struct Contained<T>(JoinHandle<Result<T, Panicked>>);

impl<T> Contained<T> {
    pub fn join(self) -> Result<T, Panicked> {
        self.0.join().unwrap_or(Err(Panicked))
    }
}

pub fn spawn<T: Send + 'static>(
    thread: thread::Builder,
    run: impl FnOnce() -> T + Send + 'static,
) -> io::Result<Contained<T>> {
    thread.spawn(move || contained(run)).map(Contained)
}

pub struct ContainedIn<'scope, T>(ScopedJoinHandle<'scope, Result<T, Panicked>>);

impl<T> ContainedIn<'_, T> {
    pub fn join(self) -> Result<T, Panicked> {
        self.0.join().unwrap_or(Err(Panicked))
    }
}

pub fn spawn_in<'scope, T: Send + 'scope>(
    scope: &'scope Scope<'scope, '_>,
    run: impl FnOnce() -> T + Send + 'scope,
) -> ContainedIn<'scope, T> {
    ContainedIn(scope.spawn(move || contained(run)))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus, Stdio};
    use std::thread;

    use super::{abort_on_panic, contained, spawn, Panicked};

    const PANICS: &str = "LUNED_TEST_PANICS";

    const RAN_ON: &str = "luned ran on past the panic";

    #[derive(Clone, Copy)]
    enum Way {
        InAThread,
        InATask,
        Caught,
        Contained,
        ContainedInContained,
        AfterContained,
        SpawnedInContained,
        HookedTwice,
    }

    impl Way {
        const ALL: [Way; 8] = [
            Way::InAThread,
            Way::InATask,
            Way::Caught,
            Way::Contained,
            Way::ContainedInContained,
            Way::AfterContained,
            Way::SpawnedInContained,
            Way::HookedTwice,
        ];

        fn name(self) -> &'static str {
            match self {
                Way::InAThread => "in a thread",
                Way::InATask => "in a task",
                Way::Caught => "caught",
                Way::Contained => "contained",
                Way::ContainedInContained => "contained in contained",
                Way::AfterContained => "after contained",
                Way::SpawnedInContained => "spawned in contained",
                Way::HookedTwice => "hooked twice",
            }
        }
    }

    fn panicking(way: Way) -> (ExitStatus, String) {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["panics::tests::panics", "--exact", "--ignored", "--nocapture"]);
        command.env(PANICS, way.name()).stdin(Stdio::null()).stdout(Stdio::null());
        let output = command.output().unwrap();
        (output.status, String::from_utf8_lossy(&output.stderr).into_owned())
    }

    fn assert_aborts(way: Way) {
        let (status, said) = panicking(way);
        let abort = rustix::process::Signal::ABORT.as_raw();
        assert_eq!(status.signal(), Some(abort), "{}: {status}\n{said}", way.name());
        assert!(!said.contains(RAN_ON), "{}: {said}", way.name());
    }

    fn assert_runs_on(way: Way) {
        let (status, said) = panicking(way);
        assert_eq!(status.code(), Some(0), "{}: {status}\n{said}", way.name());
        let panicked = said.contains("a gh answer's assertion fails");
        assert!(panicked, "{}: the child ran no panic, so nothing proves it ran on\n{said}", way.name());
        assert!(said.contains(RAN_ON), "{}: {said}", way.name());
    }

    #[test]
    #[ignore = "the process that panics as one test asks, which that test runs"]
    fn panics() {
        let Ok(name) = env::var(PANICS) else {
            return;
        };
        let way = Way::ALL.into_iter().find(|way| way.name() == name);
        let way = way.unwrap_or_else(|| panic!("{name} is no way to panic"));
        let undumped = rustix::process::Rlimit { current: Some(0), maximum: Some(0) };
        rustix::process::setrlimit(rustix::process::Resource::Core, undumped).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _hooked = abort_on_panic();
        match way {
            Way::InAThread => {
                let core = thread::Builder::new().name("core".to_owned());
                let ran = core.spawn(|| panic!("the core's assertion fails")).unwrap();
                assert!(ran.join().is_err(), "the thread panicked");
            },
            Way::InATask => {
                let task = runtime.spawn(async { panic!("a task's assertion fails") });
                assert!(runtime.block_on(task).is_err(), "the task panicked");
            },
            Way::Caught => {
                let caught = std::panic::catch_unwind(|| panic!("a handler's assertion fails"));
                assert!(caught.is_err(), "the panic was caught");
            },
            Way::Contained => {
                let asks = thread::Builder::new().name("gh ask".to_owned());
                let ran = spawn(asks, || panic!("a gh answer's assertion fails")).unwrap();
                assert_eq!(ran.join(), Err(Panicked));
            },
            Way::ContainedInContained => {
                let ran = contained(|| {
                    assert_eq!(contained(|| ()), Ok(()));
                    panic!("a gh answer's assertion fails");
                });
                assert_eq!(ran, Err(Panicked));
            },
            Way::AfterContained => {
                assert_eq!(contained(|| ()), Ok(()));
                let caught = std::panic::catch_unwind(|| panic!("the core's assertion fails"));
                assert!(caught.is_err(), "the panic was caught");
            },
            Way::SpawnedInContained => {
                let ran = contained(|| {
                    let reader = thread::Builder::new().name("program".to_owned());
                    reader.spawn(|| panic!("a reader's assertion fails")).unwrap().join()
                });
                assert!(ran.is_ok_and(|joined| joined.is_err()), "the reader panicked");
            },
            Way::HookedTwice => _ = abort_on_panic(),
        }
        eprintln!("{RAN_ON}");
    }

    #[test]
    fn a_panic_in_a_long_lived_thread_aborts_luned() {
        assert_aborts(Way::InAThread);
    }

    #[test]
    fn a_panic_in_a_spawned_task_aborts_luned() {
        assert_aborts(Way::InATask);
    }

    #[test]
    fn a_panic_caught_as_tonic_or_tokio_catch_one_aborts_luned() {
        assert_aborts(Way::Caught);
    }

    #[test]
    fn a_contained_panic_runs_on() {
        assert_runs_on(Way::Contained);
    }

    #[test]
    fn a_contained_run_stays_contained_once_one_inside_it_ends() {
        assert_runs_on(Way::ContainedInContained);
    }

    #[test]
    fn a_panic_once_a_contained_run_has_ended_aborts_luned() {
        assert_aborts(Way::AfterContained);
    }

    #[test]
    fn a_panic_in_a_thread_a_contained_run_spawns_aborts_luned() {
        assert_aborts(Way::SpawnedInContained);
    }

    #[test]
    fn setting_the_hook_twice_aborts_luned() {
        assert_aborts(Way::HookedTwice);
    }
}
