use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::panic::{self, AssertUnwindSafe};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Containment {
    Unset,
    Contained,
    Uncontained,
}

thread_local! {
    static CONTAINMENT: Cell<Containment> = const { Cell::new(Containment::Unset) };
}

static HOOKED: AtomicBool = AtomicBool::new(false);

pub fn abort_on_panic() {
    assert!(
        !HOOKED.swap(true, Ordering::Relaxed),
        "the hook that aborts is set once"
    );
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        default(panic);
        match CONTAINMENT.get() {
            Containment::Contained => {}
            Containment::Unset | Containment::Uncontained => {
                eprintln!("lune-mux: a thread panicked, so it aborts");
                process::abort();
            }
        }
    }));
}

pub(crate) struct Panicked;

pub(crate) fn contained<T>(run: impl FnOnce() -> T) -> Result<T, Panicked> {
    let _contained = Containing::to(Containment::Contained, &[Containment::Unset]);
    panic::catch_unwind(AssertUnwindSafe(run)).map_err(|_| Panicked)
}

pub(crate) struct Shared<T>(Mutex<T>);

impl<T> Shared<T> {
    pub(crate) fn new(value: T) -> Shared<T> {
        Shared(Mutex::new(value))
    }

    pub(crate) fn lock(&self) -> Held<'_, T> {
        let uncontained = uncontained();
        let held = self
            .0
            .lock()
            .expect("a thread panicked while it held shared state");
        Held {
            held,
            _uncontained: uncontained,
        }
    }
}

pub(crate) struct Held<'a, T> {
    held: MutexGuard<'a, T>,
    _uncontained: Uncontained,
}

impl<T> Deref for Held<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.held
    }
}

impl<T> DerefMut for Held<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.held
    }
}

fn uncontained() -> Uncontained {
    let after = [Containment::Unset, Containment::Contained];
    Uncontained {
        _containing: Containing::to(Containment::Uncontained, &after),
    }
}

struct Uncontained {
    _containing: Containing,
}

struct Containing {
    was: Containment,
    is: Containment,
    _this_thread: PhantomData<*const ()>,
}

impl Containing {
    fn to(is: Containment, after: &[Containment]) -> Containing {
        let was = CONTAINMENT.replace(Containment::Uncontained);
        assert!(
            after.contains(&was),
            "a thread goes {is:?} only after {after:?}, not after {was:?}"
        );
        CONTAINMENT.set(is);
        Containing {
            was,
            is,
            _this_thread: PhantomData,
        }
    }
}

impl Drop for Containing {
    fn drop(&mut self) {
        let is = CONTAINMENT.replace(Containment::Uncontained);
        assert_eq!(
            is, self.is,
            "a thread's containment ends in the order it began"
        );
        CONTAINMENT.set(self.was);
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    use super::{abort_on_panic, contained, uncontained};
    use crate::mux;

    const PANICS: &str = "LUNE_MUX_TEST_PANICS";

    #[derive(Clone, Copy)]
    enum Way {
        InATask,
        Caught,
        Contained,
        UncontainedInContained,
        ContainedAfterUncontained,
        TheModelInContained,
        ContainedInUncontained,
        UncontainedInUncontained,
        EndedOutOfOrder,
        HookedTwice,
    }

    impl Way {
        const ALL: [Way; 10] = [
            Way::InATask,
            Way::Caught,
            Way::Contained,
            Way::UncontainedInContained,
            Way::ContainedAfterUncontained,
            Way::TheModelInContained,
            Way::ContainedInUncontained,
            Way::UncontainedInUncontained,
            Way::EndedOutOfOrder,
            Way::HookedTwice,
        ];

        fn name(self) -> &'static str {
            match self {
                Way::InATask => "in a task",
                Way::Caught => "caught",
                Way::Contained => "contained",
                Way::UncontainedInContained => "uncontained in contained",
                Way::ContainedAfterUncontained => "contained after uncontained",
                Way::TheModelInContained => "the model in contained",
                Way::ContainedInUncontained => "contained in uncontained",
                Way::UncontainedInUncontained => "uncontained in uncontained",
                Way::EndedOutOfOrder => "ended out of order",
                Way::HookedTwice => "hooked twice",
            }
        }
    }

    fn panicking(way: Way) -> (std::process::ExitStatus, String) {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args([
            "panics::tests::panics",
            "--exact",
            "--ignored",
            "--nocapture",
        ]);
        command
            .env(PANICS, way.name())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let output = command.output().unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn assert_aborts(way: Way) {
        let (status, said) = panicking(way);
        let abort = rustix::process::Signal::ABORT.as_raw();
        assert_eq!(
            status.signal(),
            Some(abort),
            "{}: {status}\n{said}",
            way.name()
        );
    }

    fn assert_runs_on(way: Way) {
        let (status, said) = panicking(way);
        assert_eq!(status.code(), Some(0), "{}: {status}\n{said}", way.name());
        let panicked = said.contains("a terminal's assertion fails");
        assert!(
            panicked,
            "{}: the child ran no panic, so nothing proves it ran on\n{said}",
            way.name()
        );
    }

    #[test]
    #[ignore = "the process that panics as one test asks, which that test runs"]
    fn panics() {
        let Ok(name) = env::var(PANICS) else {
            return;
        };
        let way = Way::ALL.into_iter().find(|way| way.name() == name);
        let way = way.unwrap_or_else(|| panic!("{name} is no way to panic"));
        let undumped = rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        };
        rustix::process::setrlimit(rustix::process::Resource::Core, undumped).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        abort_on_panic();
        match way {
            Way::InATask => {
                let task = runtime.spawn(async { panic!("a task's assertion fails") });
                assert!(runtime.block_on(task).is_err(), "the task panicked");
            }
            Way::Caught => {
                let caught = std::panic::catch_unwind(|| panic!("a handler's assertion fails"));
                assert!(caught.is_err(), "the panic was caught");
            }
            Way::Contained => {
                let ran =
                    std::thread::spawn(|| contained(|| panic!("a terminal's assertion fails")));
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            }
            Way::UncontainedInContained => {
                let ran = std::thread::spawn(|| {
                    contained(|| {
                        let _uncontained = uncontained();
                        panic!("the model's assertion fails");
                    })
                });
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            }
            Way::ContainedAfterUncontained => {
                let ran = std::thread::spawn(|| {
                    contained(|| {
                        drop(uncontained());
                        panic!("a terminal's assertion fails");
                    })
                });
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            }
            Way::TheModelInContained => {
                let ran = std::thread::spawn(|| contained(mux::tests::panic_holding_the_model));
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            }
            Way::ContainedInUncontained => {
                let _uncontained = uncontained();
                assert!(contained(|| ()).is_ok(), "the run was contained");
            }
            Way::UncontainedInUncontained => {
                let _uncontained = uncontained();
                drop(uncontained());
            }
            Way::EndedOutOfOrder => {
                let ran = std::thread::spawn(|| contained(|| std::mem::forget(uncontained())));
                assert!(ran.join().unwrap().is_ok(), "the run was contained");
            }
            Way::HookedTwice => abort_on_panic(),
        }
    }

    #[test]
    fn a_panic_in_a_spawned_task_aborts_the_process() {
        assert_aborts(Way::InATask);
    }

    #[test]
    fn a_panic_caught_as_tonic_catches_a_handler_s_aborts_the_process() {
        assert_aborts(Way::Caught);
    }

    #[test]
    fn a_contained_panic_runs_on() {
        assert_runs_on(Way::Contained);
    }

    #[test]
    fn an_uncontained_panic_aborts_even_within_a_contained_run() {
        assert_aborts(Way::UncontainedInContained);
    }

    #[test]
    fn a_contained_run_stays_contained_once_an_uncontained_one_inside_it_ends() {
        assert_runs_on(Way::ContainedAfterUncontained);
    }

    #[test]
    fn a_panic_while_a_contained_run_holds_the_model_aborts() {
        assert_aborts(Way::TheModelInContained);
    }

    #[test]
    fn a_run_contained_while_the_model_is_held_aborts() {
        assert_aborts(Way::ContainedInUncontained);
    }

    #[test]
    fn holding_the_model_twice_aborts() {
        assert_aborts(Way::UncontainedInUncontained);
    }

    #[test]
    fn containment_ended_out_of_order_aborts() {
        assert_aborts(Way::EndedOutOfOrder);
    }

    #[test]
    fn setting_the_hook_twice_aborts() {
        assert_aborts(Way::HookedTwice);
    }
}
