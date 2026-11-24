use std::sync::mpsc::Sender;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, SupportedStreamConfig};

pub const RATE: u32 = 16_000;

pub type Heard = Result<Vec<f32>, String>;

pub struct Mic {
    _stream: Stream,
}

impl Mic {
    pub fn open(heard: Sender<Heard>) -> Result<Mic, String> {
        crate::hush::alsa_errors();
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("this machine has no microphone")?;
        let config = chosen(&device)?;
        let stream = match config.sample_format() {
            SampleFormat::F32 => stream::<f32>(&device, &config, heard),
            SampleFormat::I16 => stream::<i16>(&device, &config, heard),
            SampleFormat::I32 => stream::<i32>(&device, &config, heard),
            SampleFormat::U16 => stream::<u16>(&device, &config, heard),
            SampleFormat::U8 => stream::<u8>(&device, &config, heard),
            other => return Err(format!("the microphone gives {other} samples, lune reads none")),
        }?;
        stream.play().map_err(|error| format!("the microphone does not start: {error}"))?;
        Ok(Mic { _stream: stream })
    }
}

fn chosen(device: &Device) -> Result<SupportedStreamConfig, String> {
    let as_taken = device.supported_input_configs().ok().and_then(|mut configs| {
        configs.find_map(|range| {
            let fits = range.channels() == 1 && range.sample_format() == SampleFormat::F32;
            fits.then(|| range.try_with_sample_rate(RATE)).flatten()
        })
    });
    match as_taken {
        Some(config) => Ok(config),
        None => device
            .default_input_config()
            .map_err(|error| format!("the microphone says nothing of how it records: {error}")),
    }
}

fn stream<T>(
    device: &Device,
    config: &SupportedStreamConfig,
    heard: Sender<Heard>,
) -> Result<Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut down = Down::new(config.channels(), config.sample_rate());
    let failed = heard.clone();
    let stream = device.build_input_stream(
        config.config(),
        move |samples: &[T], _: &_| {
            let mut batch = Vec::with_capacity(samples.len());
            down.push(samples.iter().map(|sample| sample.to_sample::<f32>()), &mut batch);
            let _ = heard.send(Ok(batch));
        },
        move |error| {
            let _ = failed.send(Err(format!("the microphone stopped: {error}")));
        },
        None,
    );
    stream.map_err(|error| format!("the microphone does not open: {error}"))
}

#[derive(Debug)]
struct Down {
    channels: usize,
    step: f64,
    frames: u64,
    edge: f64,
    sum: f32,
    summed: u32,
    frame: Vec<f32>,
}

impl Down {
    fn new(channels: u16, rate: u32) -> Down {
        let channels = usize::from(channels.max(1));
        let step = f64::from(rate.max(1)) / f64::from(RATE);
        Down { channels, step, frames: 0, edge: step, sum: 0.0, summed: 0, frame: Vec::new() }
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>, taken: &mut Vec<f32>) {
        for sample in samples {
            self.frame.push(sample);
            if self.frame.len() < self.channels {
                continue;
            }
            let mixed = self.frame.drain(..).sum::<f32>() / self.channels as f32;
            self.sum += mixed;
            self.summed += 1;
            self.frames += 1;
            let mean = self.sum / self.summed as f32;
            let mut took = false;
            while self.frames as f64 >= self.edge {
                taken.push(mean);
                self.edge += self.step;
                took = true;
            }
            if took {
                (self.sum, self.summed) = (0.0, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(channels: u16, rate: u32, samples: &[f32]) -> Vec<f32> {
        let mut taken = Vec::new();
        Down::new(channels, rate).push(samples.iter().copied(), &mut taken);
        taken
    }

    #[test]
    fn what_the_model_takes_goes_through_as_it_is() {
        assert_eq!(down(1, RATE, &[0.5, -0.25, 1.0]), [0.5, -0.25, 1.0]);
    }

    #[test]
    fn channels_are_mixed_into_one() {
        assert_eq!(down(2, RATE, &[1.0, 0.0, -0.5, -0.5]), [0.5, -0.5]);
    }

    #[test]
    fn a_higher_rate_takes_the_mean_of_the_frames_each_sample_stands_for() {
        assert_eq!(down(1, 48_000, &[0.0, 0.25, 0.5, 1.0, 1.0, 1.0]), [0.25, 1.0]);
    }

    #[test]
    fn a_second_of_any_rate_is_a_second_taken() {
        for rate in [8_000, 22_050, 44_100, 48_000, 96_000] {
            let second = vec![0.25; rate as usize * 2];
            let taken = down(2, rate, &second);
            assert!(taken.len().abs_diff(RATE as usize) <= 1, "{rate}: {}", taken.len());
        }
    }

    #[test]
    fn a_frame_split_between_batches_is_one_frame() {
        let mut down = Down::new(2, RATE);
        let mut taken = Vec::new();
        down.push([1.0, 0.0, 0.5].into_iter(), &mut taken);
        down.push([0.5].into_iter(), &mut taken);
        assert_eq!(taken, [0.5, 0.5]);
    }
}
