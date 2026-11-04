//! Text to speech, on this machine, for what luneta reads aloud: Phonon 31m,
//! an English model of Gradium's, which `ptts` runs on the CPU, quantized to 8
//! bits, and which makes its sound as it goes, so that long text does not wait
//! for its end.
//!
//! The model is 65 MB, which luneta fetches the first time it has something to
//! read, where it has speakers to read it on, as `models` says. One thread of
//! luneta's runs it, from then until luneta ends. What it reads is a job for
//! it: it opens the speakers, plays what the model makes as the model makes it,
//! and says how it ended. The first job reads the model in, in tens of
//! milliseconds. One job at a time: the next waits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ptts::checkpoint::read_config;
use ptts::preprocess::Lang;
use ptts::synth::{DeviceKind, Quant, Synth};
use xn::threadpool::{start_pool_with, PoolConfig};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;
use crate::models::{self, Pin};
use crate::speaker::{Quiet, Speaker};

/// Where the model's files come from: the revision of them `ptts` loads by
/// default, of the version `Cargo.toml` pins.
macro_rules! of_revision {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/gradium/phonon-31m-en/resolve/",
            "8c64b55ec301728e2d1695bfea87a08a267d0c2d/",
            $file
        )
    };
}

const CONFIG: Pin = Pin {
    file: "config.json",
    url: of_revision!("config.json"),
    bytes: 2_851,
    sha256: "7062ea376e6b158543c8abec8e5af67df171c49ad70eb7e2a0fb25c16f265e06",
};

const TOKENIZER: Pin = Pin {
    file: "tokenizer.json",
    url: of_revision!("tokenizer.json"),
    bytes: 277_967,
    sha256: "691a00068fa7b989f2f3e5ae5daa2d1a5bb319f2d5360deafcfb19a8bb6e7936",
};

/// The one voice luneta reads in.
const VOICE: Pin = Pin {
    file: "default-voice.safetensors",
    url: of_revision!("default-voice.safetensors"),
    bytes: 256_136,
    sha256: "f739e8c4c76ee7f92e7c94a82002748c8cd22e99582ac49f7b9b94cd63786e18",
};

const WEIGHTS: Pin = Pin {
    file: "model.q8.gguf",
    url: of_revision!("model.q8.gguf"),
    bytes: 64_512_352,
    sha256: "a7d9aad2cd6dc66473f24a08bc6d2939030ef4d751ef82349361fbef3272dbae",
};

/// What is fetched of the model, the smallest first.
const PINS: [Pin; 4] = [CONFIG, VOICE, TOKENIZER, WEIGHTS];

/// How many bytes the model is.
pub const BYTES: u64 = models::bytes(&PINS);

/// What the model's work is shared out among, besides the threads of its own
/// that `ptts` runs it on. On a 16-core Ryzen, two make speech 11 times faster
/// than it plays, at a quarter of a core while they do. One for each core, as
/// it would have by itself, makes it 9 times faster, at nine times the CPU,
/// spinning as they wait on each other.
const THREADS: usize = 2;

/// How much the model makes ahead of what the speakers play, once it has
/// room to run on: what plays on, and what is thrown away where it is cut off.
const AHEAD: Duration = Duration::from_secs(2);

/// How long the speakers stay on once all has played: the end of the last word
/// is on its way through the system.
const TAIL: Duration = Duration::from_millis(200);

/// Whether the model is in.
pub fn fetched() -> bool {
    models::fetched(&PINS)
}

/// Fetches the model, where there are speakers for it to play on, and tells
/// `events` how it goes, for as long as what it returns is kept.
pub fn fetch(events: &Sender<Event>) -> Asking {
    models::fetch(&PINS, events, Event::VoiceFetched, Speaker::there)
}

/// The thread that runs the model, for as long as it is kept.
pub struct Voice {
    jobs: Sender<Job>,
    /// Has what plays wait.
    held: Arc<AtomicBool>,
}

/// Something to read, for the thread: `quiet` has it stop, and how it ended
/// goes as `Due` does.
struct Job {
    text: String,
    quiet: Quiet,
    ended: Due<()>,
}

impl Voice {
    /// The thread, with no model read in until the first job, whose speakers
    /// keep quiet while `held` is on.
    pub fn start(held: Arc<AtomicBool>) -> Voice {
        let (jobs, taken) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let mut synth = None;
            for job in taken {
                let Job { text, quiet, ended } = job;
                ended.pay(read(&mut synth, &text, &quiet));
            }
        });
        Voice { jobs, held }
    }

    /// Reads `text` numbered `number`, after what it is at, and tells `events`
    /// how that ended, unless it is dropped first.
    pub fn read(&self, number: u64, text: String, events: &Sender<Event>) -> Reading {
        let (reading, quiet) = Reading::new(Arc::clone(&self.held));
        let ended = Due::new(events, move |ended| Event::Voiced { number, ended });
        // A thread that panicked drops the job, which says so as it drops.
        let _ = self.jobs.send(Job { text, quiet, ended });
        reading
    }
}

/// What the thread reads, until it is done: dropped, it stops at once.
#[derive(Debug)]
pub struct Reading {
    cut: Arc<AtomicBool>,
}

impl Reading {
    /// Something to read, and what has it keep quiet, which it stops, once it
    /// is dropped, and which `held` holds.
    pub fn new(held: Arc<AtomicBool>) -> (Reading, Quiet) {
        let quiet = Quiet { cut: Arc::new(AtomicBool::new(false)), held };
        (Reading { cut: Arc::clone(&quiet.cut) }, quiet)
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.cut.store(true, Ordering::Relaxed);
    }
}

/// Reads `text` on the speakers with the model of `synth`, read in first if it
/// is not, unless `quiet` cuts it off. Or why not.
fn read(synth: &mut Option<Synth>, text: &str, quiet: &Quiet) -> Result<(), String> {
    if quiet.cut.load(Ordering::Relaxed) {
        return Ok(());
    }
    let synth = match synth {
        Some(synth) => synth,
        None => synth.insert(load()?),
    };
    let failed = |error: ptts::Error| format!("the voice failed: {error}");
    let mut speaker = Speaker::open(synth.sample_rate(), quiet.clone())?;
    for samples in synth.stream(text).map_err(failed)? {
        speaker.play(&samples.map_err(failed)?);
        if !speaker.wait(AHEAD)? {
            return Ok(());
        }
    }
    if speaker.wait(Duration::ZERO)? {
        thread::sleep(TAIL);
    }
    Ok(())
}

/// The model read in, on `THREADS`. A person can take away one of the model's
/// size that is not it.
fn load() -> Result<Synth, String> {
    start_pool_with(PoolConfig { workers: THREADS - 1, spin_budget: None }, |worker| {
        thread::spawn(worker);
    });
    let unread = |error: ptts::Error| format!("the voice cannot be read in: {error}");
    let config = read_config(CONFIG.path()?).map_err(unread)?;
    let synth = Synth::builder(config, WEIGHTS.path()?, Lang::En)
        .device(DeviceKind::Cpu)
        .quant(Quant::Q80)
        .tokenizer_file(TOKENIZER.path()?)
        .add_voice("default", VOICE.path()?)
        .build();
    synth.map_err(unread)
}
