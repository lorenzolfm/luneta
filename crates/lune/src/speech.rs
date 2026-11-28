use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use transcribe_cpp::{Model, RunOptions, Session, SessionOptions, Stream, StreamOptions};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;
use crate::mic::{self, Mic};
use crate::models::{self, Pin};

const PINS: [Pin; 1] = [Pin {
    file: "parakeet-unified-en-0.6b-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/\
          7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf",
    bytes: 731_357_568,
    sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
}];

pub const BYTES: u64 = models::bytes(&PINS);

const TAIL: Duration = Duration::from_millis(200);

const LONGEST: Duration = Duration::from_mins(5);

const LOOK: Duration = Duration::from_millis(20);

const BATCH: usize = mic::RATE as usize / 10;

const LEVEL: Duration = Duration::from_millis(50);

const SHORTEST: usize = mic::RATE as usize * 3 / 2;

const STRETCH: usize = mic::RATE as usize * 3 / 100;

const QUIET: f32 = 1e-5;

const THREADS: usize = 16;

pub const UNHEARD: &str = "the microphone heard nothing: is it muted?";

#[derive(Debug, PartialEq)]
pub enum Spoken {
    Loading,
    Listening,
    Level(f32),
    Words(String),
    Ended(Result<String, String>),
}

pub fn fetched() -> bool {
    models::fetched(&PINS)
}

pub fn fetch(events: &Sender<Event>) -> Asking {
    models::fetch(&PINS, events, Event::Fetched, || Ok(()))
}

pub struct Speech {
    jobs: Sender<Job>,
}

struct Job {
    number: u64,
    events: Sender<Event>,
    stop: Receiver<()>,
    ended: Due<String>,
}

impl Speech {
    pub fn start() -> Speech {
        let (jobs, taken) = mpsc::channel::<Job>();
        thread::spawn(move || {
            transcribe_cpp::disable_logging();
            let mut session = None;
            for job in taken {
                listen(&mut session, job);
            }
        });
        Speech { jobs }
    }

    pub fn listen(&self, number: u64, events: &Sender<Event>) -> Listening {
        let (listening, stop) = Listening::new();
        let ended = Due::new(events, move |ended| Event::Spoken {
            number,
            spoken: Spoken::Ended(ended),
        });
        let _ = self.jobs.send(Job {
            number,
            events: events.clone(),
            stop,
            ended,
        });
        listening
    }
}

#[derive(Debug)]
pub struct Listening {
    finish: Sender<()>,
}

impl Listening {
    pub fn new() -> (Listening, Receiver<()>) {
        let (finish, stop) = mpsc::channel();
        (Listening { finish }, stop)
    }

    pub fn finish(&self) {
        let _ = self.finish.send(());
    }
}

fn listen(session: &mut Option<Session>, job: Job) {
    let Job {
        number,
        events,
        stop,
        ended,
    } = job;
    let said = |spoken| {
        let _ = events.send(Event::Spoken { number, spoken });
    };
    ended.pay(heard(session, &stop, &said));
}

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
    let mut off: Option<Instant> = None;
    loop {
        match stop.try_recv() {
            Ok(()) => {
                off.get_or_insert_with(|| Instant::now() + TAIL);
            }
            Err(TryRecvError::Disconnected) => return Err("given up".to_owned()),
            Err(TryRecvError::Empty) => {}
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
    for samples in heard.try_iter().flatten() {
        loudest.hear(&samples);
        batch.extend(samples);
    }
    if !loudest.heard() {
        return Err(UNHEARD.to_owned());
    }
    batch.resize(
        batch.len() + SHORTEST.saturating_sub(fed + batch.len()),
        0.0,
    );
    if !batch.is_empty() {
        feed(&mut stream, &batch, said)?;
    }
    stream
        .finalize()
        .map_err(|error| format!("the speech model failed: {error}"))?;
    let text = stream.text().full.trim().to_owned();
    if text.is_empty() {
        return Err("the speech model made out no words".to_owned());
    }
    Ok(text)
}

fn load() -> Result<Session, String> {
    let path = PINS[0].path()?;
    let model = Model::load(&path).map_err(|error| {
        format!(
            "the speech model cannot be read in, {}: {error}",
            path.display()
        )
    })?;
    let threads = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(THREADS);
    let options = SessionOptions {
        n_threads: threads as i32,
        ..SessionOptions::default()
    };
    let session = model.session_with(&options);
    session.map_err(|error| format!("the speech model does not start: {error}"))
}

fn feed(stream: &mut Stream<'_>, samples: &[f32], said: &dyn Fn(Spoken)) -> Result<(), String> {
    let update = stream
        .feed(samples)
        .map_err(|error| format!("the speech model failed: {error}"))?;
    if update.committed_changed || update.tentative_changed {
        said(Spoken::Words(stream.text().display()));
    }
    Ok(())
}

fn level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let power = samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    let decibels = 10.0 * power.max(1e-12).log10();
    ((decibels + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[derive(Debug, Default)]
struct Loudest {
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

    fn heard(&self) -> bool {
        self.most >= QUIET
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        let count = (mic::RATE as f32 * seconds) as usize;
        (0..count)
            .map(|at| amplitude * (at as f32 * 0.05).sin())
            .collect()
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
