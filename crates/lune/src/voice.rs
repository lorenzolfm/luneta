use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use ptts::checkpoint::read_config;
use ptts::preprocess::Lang;
use ptts::synth::{DeviceKind, Quant, Synth};
use xn::threadpool::{PoolConfig, start_pool_with};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;
use crate::models::{self, Pin};
use crate::speaker::{Quiet, Speaker};

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

const PINS: [Pin; 4] = [CONFIG, VOICE, TOKENIZER, WEIGHTS];

pub const BYTES: u64 = models::bytes(&PINS);

const THREADS: usize = 2;

const AHEAD: Duration = Duration::from_secs(2);

const TAIL: Duration = Duration::from_millis(200);

pub fn fetched() -> bool {
    models::fetched(&PINS)
}

pub fn fetch(events: &Sender<Event>) -> Asking {
    models::fetch(&PINS, events, Event::VoiceFetched, Speaker::there)
}

pub struct Voice {
    jobs: Sender<Job>,
    held: Arc<AtomicBool>,
}

struct Job {
    text: String,
    quiet: Quiet,
    ended: Due<()>,
}

impl Voice {
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

    pub fn read(&self, number: u64, text: String, events: &Sender<Event>) -> Reading {
        let (reading, quiet) = Reading::new(Arc::clone(&self.held));
        let ended = Due::new(events, move |ended| Event::Voiced { number, ended });
        let _ = self.jobs.send(Job { text, quiet, ended });
        reading
    }
}

#[derive(Debug)]
pub struct Reading {
    cut: Arc<AtomicBool>,
}

impl Reading {
    pub fn new(held: Arc<AtomicBool>) -> (Reading, Quiet) {
        let quiet = Quiet {
            cut: Arc::new(AtomicBool::new(false)),
            held,
        };
        (
            Reading {
                cut: Arc::clone(&quiet.cut),
            },
            quiet,
        )
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.cut.store(true, Ordering::Relaxed);
    }
}

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

fn load() -> Result<Synth, String> {
    start_pool_with(
        PoolConfig {
            workers: THREADS - 1,
            spin_budget: None,
        },
        |worker| {
            thread::spawn(worker);
        },
    );
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
