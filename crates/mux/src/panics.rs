use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

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
    assert!(!HOOKED.swap(true, Ordering::Relaxed), "the hook that aborts is set once");
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        default(panic);
        match CONTAINMENT.get() {
            Containment::Contained => {},
            Containment::Unset | Containment::Uncontained => {
                eprintln!("luneta-mux: a thread panicked, so it aborts");
                process::abort();
            },
        }
    }));
}

pub(crate) struct Panicked;

pub(crate) fn contained<T>(run: impl FnOnce() -> T) -> Result<T, Panicked> {
    let _contained = Containing::to(Containment::Contained, &[Containment::Unset]);
    panic::catch_unwind(AssertUnwindSafe(run)).map_err(|_| Panicked)
}

pub(crate) fn uncontained() -> Uncontained {
    let after = [Containment::Unset, Containment::Contained];
    Uncontained { _containing: Containing::to(Containment::Uncontained, &after) }
}

pub(crate) struct Uncontained {
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
        assert!(after.contains(&was), "a thread goes {is:?} only after {after:?}, not after {was:?}");
        CONTAINMENT.set(is);
        Containing { was, is, _this_thread: PhantomData }
    }
}

impl Drop for Containing {
    fn drop(&mut self) {
        let is = CONTAINMENT.replace(Containment::Uncontained);
        assert_eq!(is, self.is, "a thread's containment ends in the order it began");
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

    const PANICS: &str = "LUNETA_MUX_TEST_PANICS";

    fn panicking(panics: &str) -> (std::process::ExitStatus, String) {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["panics::tests::panics", "--exact", "--ignored"]);
        command.env(PANICS, panics).stdin(Stdio::null()).stdout(Stdio::null());
        let output = command.output().unwrap();
        (output.status, String::from_utf8_lossy(&output.stderr).into_owned())
    }

    fn assert_aborts(panics: &str) {
        let (status, said) = panicking(panics);
        let abort = rustix::process::Signal::ABORT.as_raw();
        assert_eq!(status.signal(), Some(abort), "{panics}: {status}\n{said}");
    }

    fn assert_runs_on(panics: &str) {
        let (status, said) = panicking(panics);
        assert_eq!(status.code(), Some(0), "{panics}: {status}\n{said}");
    }

    #[test]
    #[ignore = "the process that panics as one test asks, which that test runs"]
    fn panics() {
        let Ok(panics) = env::var(PANICS) else {
            return;
        };
        let undumped = rustix::process::Rlimit { current: Some(0), maximum: Some(0) };
        rustix::process::setrlimit(rustix::process::Resource::Core, undumped).unwrap();
        abort_on_panic();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        match panics.as_str() {
            "in a task" => {
                let task = runtime.spawn(async { panic!("a task's assertion fails") });
                assert!(runtime.block_on(task).is_err(), "the task panicked");
            },
            "caught" => {
                let caught = std::panic::catch_unwind(|| panic!("a handler's assertion fails"));
                assert!(caught.is_err(), "the panic was caught");
            },
            "contained" => {
                let ran = std::thread::spawn(|| contained(|| panic!("a terminal's assertion fails")));
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            },
            "uncontained in contained" => {
                let ran = std::thread::spawn(|| {
                    contained(|| {
                        let _uncontained = uncontained();
                        panic!("the model's assertion fails");
                    })
                });
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            },
            "contained after uncontained" => {
                let ran = std::thread::spawn(|| {
                    contained(|| {
                        drop(uncontained());
                        panic!("a terminal's assertion fails");
                    })
                });
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            },
            "the model in contained" => {
                let ran = std::thread::spawn(|| contained(mux::tests::panic_holding_the_model));
                assert!(ran.join().unwrap().is_err(), "the panic was contained");
            },
            "contained in uncontained" => {
                let _uncontained = uncontained();
                assert!(contained(|| ()).is_ok(), "the run was contained");
            },
            "uncontained in uncontained" => {
                let _uncontained = uncontained();
                drop(uncontained());
            },
            "ended out of order" => {
                let ran = std::thread::spawn(|| contained(|| std::mem::forget(uncontained())));
                assert!(ran.join().unwrap().is_ok(), "the run was contained");
            },
            "hooked twice" => abort_on_panic(),
            unknown => unreachable!("{unknown} is no way to panic"),
        }
    }

    #[test]
    fn a_panic_in_a_spawned_task_aborts_the_process() {
        assert_aborts("in a task");
    }

    #[test]
    fn a_panic_caught_as_tonic_catches_a_handler_s_aborts_the_process() {
        assert_aborts("caught");
    }

    #[test]
    fn a_contained_panic_runs_on() {
        assert_runs_on("contained");
    }

    #[test]
    fn an_uncontained_panic_aborts_even_within_a_contained_run() {
        assert_aborts("uncontained in contained");
    }

    #[test]
    fn a_contained_run_stays_contained_once_an_uncontained_one_inside_it_ends() {
        assert_runs_on("contained after uncontained");
    }

    #[test]
    fn a_panic_while_a_contained_run_holds_the_model_aborts() {
        assert_aborts("the model in contained");
    }

    #[test]
    fn a_run_contained_while_the_model_is_held_aborts() {
        assert_aborts("contained in uncontained");
    }

    #[test]
    fn holding_the_model_twice_aborts() {
        assert_aborts("uncontained in uncontained");
    }

    #[test]
    fn containment_ended_out_of_order_aborts() {
        assert_aborts("ended out of order");
    }

    #[test]
    fn setting_the_hook_twice_aborts() {
        assert_aborts("hooked twice");
    }
}
