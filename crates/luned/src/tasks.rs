use std::sync::OnceLock;

use tokio::runtime::{self, Handle, Runtime, RuntimeFlavor};

const WORKERS: usize = 2;

const WORKER: &str = "lune tasks";

static SHARED: OnceLock<Runtime> = OnceLock::new();

pub fn start() -> Result<(), String> {
    if SHARED.get().is_some() {
        return Ok(());
    }
    let built = built().map_err(|error| format!("no runtime for lune's tasks: {error}"))?;
    if let Err(second) = SHARED.set(built) {
        second.shutdown_background();
    }
    assert!(SHARED.get().is_some(), "the runtime just set is held");
    Ok(())
}

#[must_use]
pub fn shared() -> Handle {
    let building = || built().expect("lune's tasks build as they are first handed over");
    let runtime = SHARED.get_or_init(building);
    let handle = runtime.handle().clone();
    assert_eq!(
        handle.runtime_flavor(),
        RuntimeFlavor::MultiThread,
        "a task may block in place"
    );
    handle
}

fn built() -> std::io::Result<Runtime> {
    runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .thread_name(WORKER)
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::{WORKER, shared};

    #[test]
    fn a_task_goes_on_after_the_thread_that_handed_it_over_ends() {
        let (started, starting) = tokio::sync::oneshot::channel::<()>();
        let (ran, heard) = mpsc::channel();
        let handing = thread::spawn(move || {
            shared().spawn(async move {
                let _ = starting.await;
                drop(ran.send(thread::current().name().map(str::to_owned)));
            });
        });
        handing.join().unwrap();
        started.send(()).unwrap();
        let name = heard.recv().unwrap();
        assert_eq!(name.as_deref(), Some(WORKER));
    }
}
