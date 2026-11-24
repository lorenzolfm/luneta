use std::sync::OnceLock;

use tokio::runtime::{self, Handle, Runtime};

const WORKERS: usize = 2;

static SHARED: OnceLock<Runtime> = OnceLock::new();

pub fn shared() -> Result<Handle, String> {
    if let Some(runtime) = SHARED.get() {
        return Ok(runtime.handle().clone());
    }
    let built = runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .thread_name("lune tasks")
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))?;
    if let Err(second) = SHARED.set(built) {
        second.shutdown_background();
    }
    let runtime = SHARED.get().expect("the runtime just set is held");
    Ok(runtime.handle().clone())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::shared;

    #[test]
    fn a_task_goes_on_after_the_thread_that_handed_it_over_ends() {
        let (started, starting) = tokio::sync::oneshot::channel::<()>();
        let (ran, heard) = mpsc::channel();
        let handing = thread::spawn(move || {
            shared().unwrap().spawn(async move {
                let _ = starting.await;
                drop(ran.send(thread::current().name().map(str::to_owned)));
            });
        });
        handing.join().unwrap();
        started.send(()).unwrap();
        let name = heard.recv().unwrap();
        assert_eq!(name.as_deref(), Some("lune tasks"));
    }
}
