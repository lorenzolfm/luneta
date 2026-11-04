//! Speech to text, on this machine, for what you dictate: NVIDIA's Parakeet,
//! an English model of 600 million weights, quantized to 8 bits in ggml's
//! GGUF, which transcribe.cpp runs, on the CPU on Linux and through Metal on
//! macOS. It writes what you say as you say it, and the whole of it once you
//! are done, punctuated and capitalized.
//!
//! The model is 731 MB, which luneta fetches the first time you dictate, as
//! `models` says.
//!
//! One thread of luneta's runs the model, from the first dictation until
//! luneta ends, as reading it in again each time would take a second where
//! the system has let it go of its cache. A dictation is a job for it: it
//! turns the microphone on, feeds the model what it hears, and says what the
//! model makes of it as it goes, until luneta has it finish, and then what
//! you said, or gives it up. One job at a time: the next waits.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use transcribe_cpp::{Model, RunOptions, Session, SessionOptions, Stream, StreamOptions};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;
use crate::mic::{self, Mic};
use crate::models::{self, Pin};

/// The model, as it is fetched. The revision, the size and the sum are those
/// hex pins.
const PINS: [Pin; 1] = [Pin {
    file: "parakeet-unified-en-0.6b-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/\
          7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf",
    bytes: 731_357_568,
    sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
}];

/// How many bytes the model is.
pub const BYTES: u64 = models::bytes(&PINS);

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

/// Whether the model is in.
pub fn fetched() -> bool {
    models::fetched(&PINS)
}

/// Fetches the model, and tells `events` how it goes, for as long as what it
/// returns is kept.
pub fn fetch(events: &Sender<Event>) -> Asking {
    models::fetch(&PINS, events, Event::Fetched)
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
    let path = PINS[0].path()?;
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
}
