use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::process;

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

pub fn abort_on_panic() {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        default(panic);
        if !CONTAINED.get() {
            eprintln!("luneta-mux: a thread panicked, so it aborts");
            process::abort();
        }
    }));
}

pub(crate) struct Panicked;

pub(crate) fn contained<T>(run: impl FnOnce() -> T) -> Result<T, Panicked> {
    let _contained = Containing::to(true);
    panic::catch_unwind(AssertUnwindSafe(run)).map_err(|_| Panicked)
}

pub(crate) fn uncontained() -> Uncontained {
    Uncontained { _was: Containing::to(false) }
}

pub(crate) struct Uncontained {
    _was: Containing,
}

struct Containing {
    was: bool,
    _this_thread: PhantomData<*const ()>,
}

impl Containing {
    fn to(contained: bool) -> Containing {
        Containing { was: CONTAINED.replace(contained), _this_thread: PhantomData }
    }
}

impl Drop for Containing {
    fn drop(&mut self) {
        CONTAINED.set(self.was);
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus, Stdio};

    use super::{abort_on_panic, contained, uncontained};

    const PANICS: &str = "LUNETA_MUX_TEST_PANICS";

    fn panicking(panics: &str) -> ExitStatus {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["panics::tests::panics", "--exact", "--ignored"]);
        command.env(PANICS, panics).stdout(Stdio::null()).stderr(Stdio::null());
        command.status().unwrap()
    }

    fn aborted(status: ExitStatus) -> bool {
        status.signal() == Some(rustix::process::Signal::ABORT.as_raw())
    }

    #[test]
    #[ignore = "the process that panics as one test asks, which that test runs"]
    fn panics() {
        let Ok(panics) = env::var(PANICS) else {
            return;
        };
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
            unknown => unreachable!("{unknown} is no way to panic"),
        }
    }

    #[test]
    fn a_panic_in_a_spawned_task_aborts_the_process() {
        assert!(aborted(panicking("in a task")));
    }

    #[test]
    fn a_panic_caught_as_tonic_catches_a_handler_s_aborts_the_process() {
        assert!(aborted(panicking("caught")));
    }

    #[test]
    fn a_contained_panic_runs_on() {
        assert!(panicking("contained").success());
    }

    #[test]
    fn an_uncontained_panic_aborts_even_within_a_contained_run() {
        assert!(aborted(panicking("uncontained in contained")));
    }

    #[test]
    fn a_contained_run_stays_contained_once_an_uncontained_one_inside_it_ends() {
        assert!(panicking("contained after uncontained").success());
    }
}
