//! Speech to text, on this machine, for what you dictate: NVIDIA's Parakeet,
//! an English model of 600 million weights, quantized to 8 bits in ggml's
//! GGUF, which transcribe.cpp runs, on the CPU on Linux and through Metal on
//! macOS. It writes what you say as you say it, and the whole of it once you
//! are done, punctuated and capitalized.
//!
//! The model is 731 MB, too much to ship with luneta, which fetches it the
//! first time you dictate, from Hugging Face, of a revision pinned here, and
//! keeps it in a folder named by its SHA-256 in `luneta/models` of
//! `$XDG_DATA_HOME`, or of `~/.local/share` where that is not set, on macOS
//! too: a luneta that pins another model reads none of this one's. curl
//! fetches it, and `sha256sum`, or `shasum` on macOS, checks it against the
//! size and the SHA-256 it is pinned by: both are on every machine luneta
//! runs on. It takes its name in the folder only once it passes, so that no
//! luneta reads one halfway fetched. One luneta fetches it at a time, under a
//! lock beside it, into the one part of it there is, which a luneta that
//! ended mid-fetch, however it ended, leaves for the next to fetch over.
//!
//! One thread of luneta's runs the model, from the first dictation until
//! luneta ends, as reading it in again each time would take a second where
//! the system has let it go of its cache. A dictation is a job for it: it
//! turns the microphone on, feeds the model what it hears, and says what the
//! model makes of it as it goes, until luneta has it finish, and then what
//! you said, or gives it up. One job at a time: the next waits.

use std::cell::Cell;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::program;
use transcribe_cpp::{Model, RunOptions, Session, SessionOptions, Stream, StreamOptions};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;
use crate::mic::{self, Mic};

/// The model, as it is fetched: the name it is kept by, where it is fetched
/// from, of a revision of its own, and how many bytes it is, and the SHA-256
/// of them. The revision, the size and the sum are those hex pins.
const FILE: &str = "parakeet-unified-en-0.6b-Q8_0.gguf";
const URL: &str = "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/\
    7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf";
pub const BYTES: u64 = 731_357_568;
const SHA256: &str = "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795";

/// What sums a file's SHA-256, and says it first on its line.
#[cfg(target_os = "macos")]
const SUM: &[&str] = &["shasum", "-a", "256"];
#[cfg(not(target_os = "macos"))]
const SUM: &[&str] = &["sha256sum"];

/// How long the model has to come, over a line of a megabyte a second. One
/// that takes longer is stuck, and is killed.
const FETCH: Duration = Duration::from_hours(1);

/// How long the model's sum has to be worked out, which takes `shasum`, of
/// perl, a few seconds.
const CHECK: Duration = Duration::from_mins(2);

/// How often luneta is told how much of the model is in.
const PROGRESS: Duration = Duration::from_millis(250);

/// How long the microphone stays on once you are done: the end of a last
/// word comes as the key is let go.
const TAIL: Duration = Duration::from_millis(200);

/// The longest a dictation goes on, after which it ends as if you were done.
const LONGEST: Duration = Duration::from_mins(5);

/// How long the job waits on the microphone before it looks at whether it is
/// to finish.
const LOOK: Duration = Duration::from_millis(20);

/// How many samples the model is fed at once: a tenth of a second.
const BATCH: usize = mic::RATE as usize / 10;

/// How often luneta is told how loud the microphone is.
const LEVEL: Duration = Duration::from_millis(50);

/// The shortest the model hears: a dictation shorter than this has silence
/// after it, as Parakeet makes out a word or two alone less well.
const SHORTEST: usize = mic::RATE as usize * 3 / 2;

/// How many samples a stretch is of those that tell whether you said
/// anything: 30 ms of them.
const STRETCH: usize = mic::RATE as usize * 3 / 100;

/// The power of the loudest stretch under which the microphone heard nothing:
/// -50 dB under the loudest it can hear. A model fed only noise makes words
/// of it.
const QUIET: f32 = 1e-5;

/// The most threads the model takes on the CPU. The model runs fastest on one
/// a core, and gains nothing past 16, as measured on a 16-core Ryzen.
const THREADS: usize = 16;

/// Why a dictation has nothing to write, though the microphone was on.
pub const UNHEARD: &str = "the microphone heard nothing: is it muted?";

/// What the model of a dictation says, on its way.
#[derive(Debug, PartialEq)]
pub enum Spoken {
    /// The model is being read in, which the first dictation waits on: what
    /// you say meanwhile it hears once it is in.
    Loading,
    /// The model is in, and hears what you say.
    Listening,
    /// How loud the microphone heard it last, from 0 for silence to 1.
    Level(f32),
    /// What the model makes of what you said so far.
    Words(String),
    /// The dictation is over: what you said, or why there is nothing.
    Ended(Result<String, String>),
}

/// How the fetch of the model goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// How many of its bytes are in.
    Got(u64),
    /// It is in, or why not.
    Ended(Result<(), String>),
}

/// Where the model is kept, or why it cannot be.
fn path() -> Result<PathBuf, String> {
    kept_in(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))
}

/// Where the model is kept, by `$XDG_DATA_HOME` and `$HOME` as they are set,
/// if they are: one set to a relative path is not, as the spec has it.
fn kept_in(data: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    let set = |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
    let data = set(data)
        .or_else(|| set(home).map(|home| home.join(".local").join("share")))
        .ok_or("neither XDG_DATA_HOME nor HOME is set to an absolute path")?;
    Ok(data.join("luneta").join("models").join(SHA256).join(FILE))
}

/// Whether the model is in.
pub fn fetched() -> bool {
    path().is_ok_and(|path| whole(&path))
}

/// Whether the model at `path` is whole: it takes its name only once it
/// passed its check, and one of another size, as of a copy cut short, or a
/// disk that filled, is not it, and is fetched again, over it.
fn whole(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|kept| kept.is_file() && kept.len() == BYTES)
}

/// Fetches the model, and tells `events` how it goes, on a thread of its
/// own, for as long as what it returns is kept: dropped, as luneta ends,
/// curl is killed, and what it fetched thrown away, by the time the drop
/// returns, so that no luneta leaves a fetch going, or half a model, behind.
pub fn fetch(events: &Sender<Event>) -> Asking {
    let due = Due::new(events, |ended| Event::Fetched(Fetched::Ended(ended)));
    let events = events.clone();
    Asking::on_thread(move |wanted| due.pay(fetching(wanted, &events)))
}

/// Fetches the model, for as long as it is `wanted`, as its part beside where
/// it is kept, and gives it its name once it passed its check. It waits for
/// the lock beside it first, which the system lets go of however the luneta
/// that holds it ends, and tells `events` how far the luneta that holds it
/// got, and takes what that one kept.
fn fetching(wanted: &dyn Fn() -> bool, events: &Sender<Event>) -> Result<(), String> {
    let path = path()?;
    let folder = path.parent().ok_or("the model is kept in no folder")?;
    fs::create_dir_all(folder)
        .map_err(|error| format!("no folder for the model, {}: {error}", folder.display()))?;
    let part = path.with_added_extension("part");
    let told = Cell::new(Instant::now());
    let still = || {
        if told.get().elapsed() >= PROGRESS {
            told.set(Instant::now());
            if let Ok(got) = fs::metadata(&part) {
                // luneta is on its way out.
                let _ = events.send(Event::Fetched(Fetched::Got(got.len())));
            }
        }
        wanted()
    };
    // Let go of as `lock` closes.
    let lock = File::create(path.with_added_extension("lock"))
        .map_err(|error| format!("the speech model takes no lock: {error}"))?;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if still() => thread::sleep(PROGRESS),
            Err(TryLockError::WouldBlock) => return Err("given up".to_owned()),
            Err(TryLockError::Error(error)) => {
                return Err(format!("the speech model takes no lock: {error}"));
            },
        }
    }
    // Another luneta kept it, as this one waited.
    if whole(&path) {
        return Ok(());
    }
    let mut curl = Command::new("curl");
    curl.args(["--fail", "--location", "--silent", "--show-error", "--retry", "3", "--output"]);
    curl.arg(&part).arg(URL);
    let fetched = match program::run_while(&mut curl, FETCH, still) {
        Ok(Some(ran)) => ran.answer().map(drop).map_err(|why| format!("curl: {why}")),
        Ok(None) => Err("given up".to_owned()),
        Err(error) => Err(format!("luneta fetches the speech model with curl: {error}")),
    };
    let checked = fetched.and_then(|()| check(&part, wanted));
    let kept = checked.and_then(|()| {
        fs::rename(&part, &path).map_err(|error| format!("the model is not kept: {error}"))
    });
    if kept.is_err() {
        // Nothing of it was fetched, as curl could not start.
        let _ = fs::remove_file(&part);
    }
    kept
}

/// Whether `file` is the model as it is pinned: of its size, and its sum,
/// worked out for as long as it is `wanted`.
fn check(file: &Path, wanted: &dyn Fn() -> bool) -> Result<(), String> {
    let bytes = fs::metadata(file).map_err(|error| format!("the model is gone: {error}"))?.len();
    if bytes != BYTES {
        return Err(format!("the model came {bytes} bytes long, and not {BYTES}"));
    }
    let mut sum = Command::new(SUM[0]);
    sum.args(&SUM[1..]).arg(file);
    let said = program::run_while(&mut sum, CHECK, wanted)
        .map_err(|error| format!("{}: {error}", SUM[0]))?
        .ok_or("given up")?
        .answer()
        .map_err(|why| format!("{}: {why}", SUM[0]))?;
    match String::from_utf8_lossy(&said).split_whitespace().next() {
        Some(sum) if sum == SHA256 => Ok(()),
        _ => Err("the model that came is not the one luneta pins: its SHA-256 differs".to_owned()),
    }
}

/// The thread that runs the model, for as long as it is kept.
pub struct Speech {
    jobs: Sender<Job>,
}

/// A dictation, for the thread to hear: what it hears goes in an event of
/// its number, on `events`, and how it ended, as `Due` does. `stop` has it
/// finish, or, once it is dropped, give up.
struct Job {
    number: u64,
    events: Sender<Event>,
    stop: Receiver<()>,
    ended: Due<String>,
}

impl Speech {
    /// The thread, with no model read in until the first dictation.
    pub fn start() -> Speech {
        let (jobs, taken) = mpsc::channel::<Job>();
        thread::spawn(move || {
            // What the model says of itself would go to luneta's terminal,
            // over its screen.
            transcribe_cpp::disable_logging();
            let mut session = None;
            for job in taken {
                listen(&mut session, job);
            }
        });
        Speech { jobs }
    }

    /// A dictation numbered `number`, which tells `events` what it hears,
    /// until it is told to finish, or dropped.
    pub fn listen(&self, number: u64, events: &Sender<Event>) -> Listening {
        let (listening, stop) = Listening::new();
        let ended = Due::new(events, move |ended| {
            Event::Spoken { number, spoken: Spoken::Ended(ended) }
        });
        // A thread that panicked drops the job, which says so as it drops.
        let _ = self.jobs.send(Job { number, events: events.clone(), stop, ended });
        listening
    }
}

/// A dictation the thread hears, until it is told to finish, and while it
/// writes what you said: dropped, it gives up.
#[derive(Debug)]
pub struct Listening {
    finish: Sender<()>,
}

impl Listening {
    /// A dictation, and what tells the thread that hears it to finish it, or,
    /// once the dictation is dropped, to give it up.
    pub fn new() -> (Listening, Receiver<()>) {
        let (finish, stop) = mpsc::channel();
        (Listening { finish }, stop)
    }

    /// Has the dictation end, and write what you said.
    pub fn finish(&self) {
        // It ended already.
        let _ = self.finish.send(());
    }
}

/// Hears `job` with the model's `session`, read in first if it is not.
fn listen(session: &mut Option<Session>, job: Job) {
    let Job { number, events, stop, ended } = job;
    let said = |spoken| {
        // luneta is on its way out.
        let _ = events.send(Event::Spoken { number, spoken });
    };
    ended.pay(heard(session, &stop, &said));
}

/// What you said, until `stop` has the dictation finish, with what the model
/// makes of it on its way told to `said`, or why there is nothing. The
/// microphone is on first, so that what you say as the model is read in
/// waits for it.
fn heard(
    session: &mut Option<Session>,
    stop: &Receiver<()>,
    said: &dyn Fn(Spoken),
) -> Result<String, String> {
    let (sender, heard) = mpsc::channel();
    let mic = Mic::open(sender)?;
    let session = if let Some(session) = session {
        session
    } else {
        said(Spoken::Loading);
        session.insert(load()?)
    };
    let mut stream = session
        .stream(&RunOptions::default(), &StreamOptions::default())
        .map_err(|error| format!("the speech model does not start: {error}"))?;
    said(Spoken::Listening);
    let started = Instant::now();
    let (mut loudest, mut batch, mut fed) = (Loudest::default(), Vec::new(), 0);
    let mut leveled: Option<Instant> = None;
    // When the microphone goes off, once you are done.
    let mut off: Option<Instant> = None;
    loop {
        match stop.try_recv() {
            Ok(()) => {
                off.get_or_insert_with(|| Instant::now() + TAIL);
            },
            Err(TryRecvError::Disconnected) => return Err("given up".to_owned()),
            Err(TryRecvError::Empty) => {},
        }
        let now = Instant::now();
        if off.is_some_and(|off| now >= off) || now - started >= LONGEST {
            break;
        }
        let samples = match heard.recv_timeout(LOOK) {
            Ok(samples) => samples?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("the microphone went".to_owned()),
        };
        loudest.hear(&samples);
        if leveled.is_none_or(|leveled| now - leveled >= LEVEL) {
            said(Spoken::Level(level(&samples)));
            leveled = Some(now);
        }
        batch.extend(samples);
        if batch.len() >= BATCH {
            fed += batch.len();
            feed(&mut stream, &batch, said)?;
            batch.clear();
        }
    }
    drop(mic);
    // What it heard before it went off.
    for samples in heard.try_iter().flatten() {
        loudest.hear(&samples);
        batch.extend(samples);
    }
    if !loudest.heard() {
        return Err(UNHEARD.to_owned());
    }
    batch.resize(batch.len() + SHORTEST.saturating_sub(fed + batch.len()), 0.0);
    // The model takes no batch of none.
    if !batch.is_empty() {
        feed(&mut stream, &batch, said)?;
    }
    stream.finalize().map_err(|error| format!("the speech model failed: {error}"))?;
    let text = stream.text().full.trim().to_owned();
    if text.is_empty() {
        return Err("the speech model made out no words".to_owned());
    }
    Ok(text)
}

/// The model read in, with a session of its own to run on, one thread a
/// core, as many as `THREADS` at the most.
fn load() -> Result<Session, String> {
    let path = path()?;
    // A person can take away one of the model's size that is not it.
    let model = Model::load(&path).map_err(|error| {
        format!("the speech model cannot be read in, {}: {error}", path.display())
    })?;
    let threads = thread::available_parallelism().map_or(1, usize::from).min(THREADS);
    let options = SessionOptions { n_threads: threads as i32, ..SessionOptions::default() };
    let session = model.session_with(&options);
    session.map_err(|error| format!("the speech model does not start: {error}"))
}

/// Feeds the model `samples`, and tells `said` what it makes of all it was fed
/// where that changed.
fn feed(stream: &mut Stream<'_>, samples: &[f32], said: &dyn Fn(Spoken)) -> Result<(), String> {
    let update = stream.feed(samples).map_err(|error| format!("the speech model failed: {error}"))?;
    if update.committed_changed || update.tentative_changed {
        said(Spoken::Words(stream.text().display()));
    }
    Ok(())
}

/// How loud `samples` are, from 0 at 60 dB under the loudest the microphone
/// hears, or quieter, to 1 at the loudest.
fn level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let power = samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    let decibels = 10.0 * power.max(1e-12).log10();
    ((decibels + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The loudest stretch of what the microphone heard, of `STRETCH` samples
/// each, by its power.
#[derive(Debug, Default)]
struct Loudest {
    /// The sum of the squares of the stretch heard so far, and how many.
    sum: f32,
    summed: usize,
    most: f32,
}

impl Loudest {
    fn hear(&mut self, samples: &[f32]) {
        for sample in samples {
            self.sum += sample * sample;
            self.summed += 1;
            if self.summed == STRETCH {
                self.most = self.most.max(self.sum / STRETCH as f32);
                (self.sum, self.summed) = (0.0, 0);
            }
        }
    }

    /// Whether any stretch was loud enough to be more than noise.
    fn heard(&self) -> bool {
        self.most >= QUIET
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of a tone as loud as `amplitude`.
    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        let count = (mic::RATE as f32 * seconds) as usize;
        (0..count).map(|at| amplitude * (at as f32 * 0.05).sin()).collect()
    }

    #[test]
    fn silence_and_hiss_are_nothing_heard() {
        for heard in [vec![0.0; mic::RATE as usize], tone(0.002, 1.0)] {
            let mut loudest = Loudest::default();
            loudest.hear(&heard);
            assert!(!loudest.heard());
        }
    }

    #[test]
    fn a_word_in_silence_is_heard() {
        let mut loudest = Loudest::default();
        loudest.hear(&vec![0.0; mic::RATE as usize]);
        loudest.hear(&tone(0.1, 0.05));
        loudest.hear(&vec![0.0; mic::RATE as usize]);
        assert!(loudest.heard());
    }

    #[test]
    fn a_stretch_split_between_batches_counts_whole() {
        let mut loudest = Loudest::default();
        let word = tone(0.1, 0.03);
        let (first, last) = word.split_at(STRETCH / 2);
        loudest.hear(first);
        assert!(!loudest.heard());
        loudest.hear(last);
        assert!(loudest.heard());
    }

    #[test]
    fn the_level_goes_from_silence_to_the_loudest() {
        assert!(level(&[]).abs() < f32::EPSILON);
        assert!(level(&[0.0; 160]).abs() < f32::EPSILON);
        assert!((level(&[1.0; 160]) - 1.0).abs() < f32::EPSILON);
        let (quiet, loud) = (level(&tone(0.01, 0.01)), level(&tone(0.3, 0.01)));
        assert!(0.0 < quiet && quiet < loud && loud < 1.0, "{quiet} {loud}");
    }

    #[test]
    fn the_model_is_kept_where_xdg_data_home_says_or_under_home() {
        let set = |value: &str| Some(OsString::from(value));
        let kept = kept_in(set("/data"), set("/home/you"));
        assert_eq!(kept, Ok(PathBuf::from(format!("/data/luneta/models/{SHA256}/{FILE}"))));
        let kept = kept_in(set("relative"), set("/home/you"));
        let under_home = format!("/home/you/.local/share/luneta/models/{SHA256}/{FILE}");
        assert_eq!(kept, Ok(PathBuf::from(under_home)));
        assert!(kept_in(None, set("relative")).is_err());
    }

    /// A model cut short, as of a copy, is not in: it is fetched over.
    #[test]
    fn only_a_model_of_its_size_is_in() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join(FILE);
        assert!(!whole(&path));
        let file = File::create(&path).unwrap();
        file.set_len(BYTES / 2).unwrap();
        assert!(!whole(&path));
        file.set_len(BYTES).unwrap();
        assert!(whole(&path));
    }
}
