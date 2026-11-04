//! The speakers, while luneta reads aloud: the system's default output,
//! through cpal: ALSA's on Linux, which `PipeWire` and `PulseAudio` serve where
//! they run, and `CoreAudio`'s on macOS. They are open only for what is being
//! read, and what is put to them plays as it comes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{
    Device, ErrorKind, FromSample, Sample as _, SampleFormat, SizedSample, Stream,
    SupportedStreamConfig,
};

/// How often `wait` looks at how far the speakers got.
const LOOK: Duration = Duration::from_millis(10);

/// How long speakers that are not held may take none of what waits for them,
/// after which they have stopped taking it, as a stream the system has
/// stopped playing does, with no word of why.
const STALLED: Duration = Duration::from_secs(5);

/// What has the speakers keep quiet.
#[derive(Clone, Debug, Default)]
pub struct Quiet {
    /// Everything is cut off: what waits is never heard.
    pub cut: Arc<AtomicBool>,
    /// What waits waits, for as long as this is on, as you dictate.
    pub held: Arc<AtomicBool>,
}

/// What the speakers and the thread that feeds them share.
#[derive(Debug, Default)]
struct Feed {
    /// What is yet to play, as the speakers take it.
    samples: Mutex<VecDeque<f32>>,
    /// How many of them have been taken.
    taken: AtomicU64,
    /// Why the speakers are gone.
    failed: Mutex<Option<String>>,
}

/// The speakers, on for as long as they are kept.
pub struct Speaker {
    _stream: Stream,
    feed: Arc<Feed>,
    quiet: Quiet,
    up: Up,
    /// How many samples they take a second, of all their channels.
    rate: u64,
}

impl Speaker {
    /// Whether there are speakers to open, or why not: for a model that would
    /// play on them to be fetched only where there are.
    pub fn there() -> Result<(), String> {
        by_default(&default_output()?).map(drop)
    }

    /// The default output on, for what comes as `rate` samples a second of one
    /// channel, or why there is none.
    pub fn open(rate: u32, quiet: Quiet) -> Result<Speaker, String> {
        let device = default_output()?;
        let config = chosen(&device, rate)?;
        let feed = Arc::new(Feed::default());
        let stream = match config.sample_format() {
            SampleFormat::F32 => stream::<f32>(&device, &config, &feed, &quiet),
            SampleFormat::I16 => stream::<i16>(&device, &config, &feed, &quiet),
            SampleFormat::I32 => stream::<i32>(&device, &config, &feed, &quiet),
            SampleFormat::U16 => stream::<u16>(&device, &config, &feed, &quiet),
            SampleFormat::U8 => stream::<u8>(&device, &config, &feed, &quiet),
            other => return Err(format!("the speakers take {other} samples, luneta makes none")),
        }?;
        stream.play().map_err(|error| format!("the speakers do not start: {error}"))?;
        let up = Up::new(rate, config.sample_rate(), config.channels());
        let channels = u64::from(config.channels().max(1));
        let rate = u64::from(config.sample_rate()) * channels;
        Ok(Speaker { _stream: stream, feed, quiet, up, rate })
    }

    /// Puts `samples` to the speakers, to play after what is yet to.
    pub fn play(&mut self, samples: &[f32]) {
        let mut waiting = self.feed.samples.lock().unwrap_or_else(PoisonError::into_inner);
        self.up.push(samples, &mut waiting);
    }

    /// Waits until no more than `ahead` is yet to play, or why it never will
    /// be: whether what plays was not cut off.
    pub fn wait(&self, ahead: Duration) -> Result<bool, String> {
        let most = self.rate * ahead.as_millis() as u64 / 1000;
        let (mut last, mut moved) = (self.feed.taken.load(Ordering::Relaxed), Instant::now());
        loop {
            if self.quiet.cut.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let failed = self.feed.failed.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(why) = failed {
                return Err(why);
            }
            let left = self.feed.samples.lock().unwrap_or_else(PoisonError::into_inner).len();
            if left as u64 <= most {
                return Ok(true);
            }
            let taken = self.feed.taken.load(Ordering::Relaxed);
            if taken != last || self.quiet.held.load(Ordering::Relaxed) {
                (last, moved) = (taken, Instant::now());
            } else if moved.elapsed() >= STALLED {
                return Err("the speakers have stopped taking sound".to_owned());
            }
            thread::sleep(LOOK);
        }
    }
}

/// The system's default output, or why it has none.
fn default_output() -> Result<Device, String> {
    crate::hush::alsa_errors();
    let device = cpal::default_host().default_output_device();
    device.ok_or_else(|| "this machine has no speakers".to_owned())
}

/// How `device` plays by default, or why it does not say.
fn by_default(device: &Device) -> Result<SupportedStreamConfig, String> {
    device
        .default_output_config()
        .map_err(|error| format!("the speakers say nothing of how they play: {error}"))
}

/// How `device` is asked to play: as it comes, where it can, as ALSA's default
/// does where `PipeWire` or `PulseAudio` serve it, and else as it plays by
/// default, which `Up` brings what comes to.
fn chosen(device: &Device, rate: u32) -> Result<SupportedStreamConfig, String> {
    let as_it_comes = device.supported_output_configs().ok().and_then(|mut configs| {
        configs.find_map(|range| {
            let fits = range.channels() == 1 && range.sample_format() == SampleFormat::F32;
            fits.then(|| range.try_with_sample_rate(rate)).flatten()
        })
    });
    match as_it_comes {
        Some(config) => Ok(config),
        None => by_default(device),
    }
}

/// The stream that plays what `feed` holds on `device`, in samples of `T`, as
/// `config` has it play, or silence where it holds nothing, or `quiet` says.
fn stream<T>(
    device: &Device,
    config: &SupportedStreamConfig,
    feed: &Arc<Feed>,
    quiet: &Quiet,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let (playing, failing, quiet) = (Arc::clone(feed), Arc::clone(feed), quiet.clone());
    let stream = device.build_output_stream(
        config.config(),
        move |samples: &mut [T], _: &_| {
            let silent =
                quiet.cut.load(Ordering::Relaxed) || quiet.held.load(Ordering::Relaxed);
            let mut waiting = playing.samples.lock().unwrap_or_else(PoisonError::into_inner);
            let mut taken = 0;
            for sample in samples {
                let next = if silent { None } else { waiting.pop_front() };
                taken += u64::from(next.is_some());
                *sample = next.unwrap_or(0.0).to_sample::<T>();
            }
            playing.taken.fetch_add(taken, Ordering::Relaxed);
        },
        move |error| {
            // It played on, or was rerouted.
            if matches!(error.kind(), ErrorKind::Xrun | ErrorKind::DeviceChanged) {
                return;
            }
            let why = format!("the speakers stopped: {error}");
            *failing.failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
        },
        None,
    );
    stream.map_err(|error| format!("the speakers do not open: {error}"))
}

/// What brings samples of one channel, `from` a second, to what the speakers
/// take: `to` frames a second, the same in each of `channels`, each worked
/// out on the line between the two samples it falls between. Speech has
/// nothing up where this would make a sound of its own.
#[derive(Debug)]
struct Up {
    channels: usize,
    /// How many samples of what comes each frame stands for.
    step: f64,
    /// The last sample that came, and how far past it the next frame falls,
    /// as a share of the way to the next sample.
    last: f32,
    at: f64,
}

impl Up {
    fn new(from: u32, to: u32, channels: u16) -> Up {
        let channels = usize::from(channels.max(1));
        Up { channels, step: f64::from(from.max(1)) / f64::from(to.max(1)), last: 0.0, at: 0.0 }
    }

    /// Adds to `taken` what `samples` come to.
    fn push(&mut self, samples: &[f32], taken: &mut VecDeque<f32>) {
        for &sample in samples {
            while self.at < 1.0 {
                let frame = self.last + (sample - self.last) * self.at as f32;
                taken.extend(std::iter::repeat_n(frame, self.channels));
                self.at += self.step;
            }
            self.at -= 1.0;
            self.last = sample;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(from: u32, to: u32, channels: u16, samples: &[f32]) -> Vec<f32> {
        let mut taken = VecDeque::new();
        Up::new(from, to, channels).push(samples, &mut taken);
        taken.into()
    }

    #[test]
    fn what_the_speakers_take_as_it_comes_goes_through_a_sample_late() {
        assert_eq!(up(24_000, 24_000, 1, &[0.5, -0.25, 1.0]), [0.0, 0.5, -0.25]);
    }

    #[test]
    fn each_channel_has_the_same() {
        assert_eq!(up(24_000, 24_000, 2, &[0.5, 1.0]), [0.0, 0.0, 0.5, 0.5]);
    }

    #[test]
    fn a_higher_rate_falls_on_the_line_between_samples() {
        assert_eq!(up(24_000, 48_000, 1, &[1.0, 0.0]), [0.0, 0.5, 1.0, 0.5]);
    }

    #[test]
    fn a_second_of_what_comes_is_a_second_at_any_rate() {
        for to in [16_000, 22_050, 24_000, 44_100, 48_000, 96_000] {
            let second = vec![0.25; 24_000];
            let taken = up(24_000, to, 2, &second);
            assert!(taken.len().abs_diff(to as usize * 2) <= 2, "{to}: {}", taken.len());
        }
    }

    #[test]
    fn what_comes_in_pieces_plays_as_what_comes_whole() {
        let samples: Vec<f32> = (0..200).map(|at| (at as f32 * 0.1).sin()).collect();
        let mut pieces = VecDeque::new();
        let mut up_in_pieces = Up::new(24_000, 44_100, 1);
        for piece in samples.chunks(7) {
            up_in_pieces.push(piece, &mut pieces);
        }
        assert_eq!(Vec::from(pieces), up(24_000, 44_100, 1, &samples));
    }
}
