//! What the daemon reads of another process: when it started, spelled as Claude
//! Code records it and as a time, and the environment it started with. Each
//! system has its own way to say them, and a backend of its own.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("lunetad reads processes on Linux and macOS, and on nothing else yet");

pub use os::{environ, start, started, Domain};

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    /// A process is dated no earlier than it started, wherever in a clock
    /// tick or a second it starts, so that the session it started in is not
    /// ruled out for being made a moment before; and less than a second
    /// later.
    #[test]
    fn a_process_is_dated_no_earlier_than_it_started() {
        for _ in 0..10 {
            let before = SystemTime::now();
            let mut child = Command::new("sleep").arg("60").spawn().unwrap();
            let started = super::started(child.id());
            let after = SystemTime::now();
            child.kill().unwrap();
            child.wait().unwrap();
            let started = started.expect("a process that runs is dated");
            assert!(before <= started, "dated {started:?}, started after {before:?}");
            assert!(started <= after + Duration::from_secs(1), "dated {started:?}, by {after:?}");
        }
    }
}
