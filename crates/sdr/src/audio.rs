//! The receiver's audio on a sound card.
//!
//! The pipeline thread writes 48 kHz mono into an `rtrb` ring ([`AudioFeed`]);
//! the sound card's own callback reads it, converts to the device's rate by
//! linear interpolation, applies the volume and copies it to every channel.
//! The two clocks — the radio's ADC and the sound card's — never quite agree,
//! so the callback also keeps the ring between a floor and a ceiling: below the
//! floor it waits (silence) until there is a cushion again, above the ceiling it
//! drops the oldest audio, which bounds the latency.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::receiver::AUDIO_RATE;

/// Ring capacity: one second of audio.
const RING: usize = AUDIO_RATE as usize;
/// Audio held before playing starts, and again after an underrun.
const CUSHION: usize = (AUDIO_RATE * 0.06) as usize;
/// Above this much in the ring the oldest audio is dropped.
const CEILING: usize = (AUDIO_RATE * 0.25) as usize;
/// How long a volume change takes, so the slider does not click.
const RAMP_S: f32 = 0.01;

/// The sound outputs on this machine, by name.
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = host
        .output_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|x| x.name().to_string())).collect())
        .unwrap_or_default();
    names.dedup();
    names
}

/// The writing end of the audio ring, handed to the pipeline.
pub struct AudioFeed {
    tx: Producer<f32>,
    dropped: Arc<AtomicU64>,
}

impl AudioFeed {
    /// Queue 48 kHz mono samples. What does not fit is dropped and counted —
    /// a sound card that has stopped must not stall the receiver.
    pub fn push(&mut self, samples: &[f32]) {
        let n = self.tx.slots().min(samples.len());
        if let Ok(chunk) = self.tx.write_chunk_uninit(n) {
            chunk.fill_from_iter(samples[..n].iter().copied());
        }
        if n < samples.len() {
            self.dropped.fetch_add((samples.len() - n) as u64, Ordering::Relaxed);
        }
    }
}

/// A ring and its two ends: the feed for the pipeline, and the reading end the
/// sound card's callback owns. Exposed for tests and for callers that play the
/// audio some other way.
pub fn audio_ring() -> (AudioFeed, Consumer<f32>, Arc<AtomicU64>) {
    let (tx, rx) = RingBuffer::new(RING);
    let dropped = Arc::new(AtomicU64::new(0));
    (AudioFeed { tx, dropped: dropped.clone() }, rx, dropped)
}

/// Shared between the UI and the callback.
struct Controls {
    /// 0 to 1, as `f32` bits.
    volume: AtomicU32,
    muted: AtomicBool,
    underruns: AtomicU64,
    /// The stream reported an error: the device was turned off, unplugged
    /// or taken away. It plays nothing more; the owner opens it again.
    failed: AtomicBool,
}

/// Audio playing on one output device.
pub struct AudioOut {
    _stream: cpal::Stream,
    controls: Arc<Controls>,
    dropped: Arc<AtomicU64>,
    device: String,
    device_rate: u32,
    channels: u16,
}

impl AudioOut {
    /// Open the output named `device` (the system default if `None` or not
    /// found by that name is an error) and start it. Returns the player and
    /// the feed to give the pipeline ([`crate::Pipeline::set_audio`]).
    pub fn open(device: Option<&str>, volume: f32) -> Result<(AudioOut, AudioFeed), String> {
        let host = cpal::default_host();
        let dev = match device {
            Some(name) => host
                .output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.description().is_ok_and(|x| x.name() == name))
                .ok_or_else(|| format!("The sound output \"{name}\" isn't there."))?,
            None => host.default_output_device().ok_or("There is no sound output.")?,
        };
        let name = dev
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "default output".into());
        let supported = dev.default_output_config().map_err(|e| e.to_string())?;
        let fmt = supported.sample_format();
        let config = supported.config();
        let (feed, rx, dropped) = audio_ring();
        let controls = Arc::new(Controls {
            volume: AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()),
            muted: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        let (rate, channels) = (config.sample_rate, config.channels);
        let c = controls.clone();
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&dev, config, rx, c),
            SampleFormat::I16 => build::<i16>(&dev, config, rx, c),
            SampleFormat::I32 => build::<i32>(&dev, config, rx, c),
            SampleFormat::U16 => build::<u16>(&dev, config, rx, c),
            other => {
                return Err(format!("\"{name}\" wants {other:?} samples, which aren't supported"));
            }
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((
            AudioOut {
                _stream: stream,
                controls,
                dropped,
                device: name,
                device_rate: rate,
                channels,
            },
            feed,
        ))
    }

    /// 0 to 1.
    pub fn set_volume(&self, v: f32) {
        self.controls.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.controls.volume.load(Ordering::Relaxed))
    }

    pub fn set_muted(&self, on: bool) {
        self.controls.muted.store(on, Ordering::Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.controls.muted.load(Ordering::Relaxed)
    }

    /// The stream has failed (its device went away) and wants opening again.
    pub fn failed(&self) -> bool {
        self.controls.failed.load(Ordering::Relaxed)
    }

    /// The device's name, as [`output_devices`] lists it.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The device's own rate and channel count.
    pub fn device_format(&self) -> (u32, u16) {
        (self.device_rate, self.channels)
    }

    /// Times the card ran dry, and samples dropped because the ring was full or
    /// too far ahead.
    pub fn stats(&self) -> (u64, u64) {
        (self.controls.underruns.load(Ordering::Relaxed), self.dropped.load(Ordering::Relaxed))
    }
}

/// The system's default output's name now (it changes when, say, a
/// speaker is turned on and Windows makes it the default).
pub fn default_output() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.description().ok().map(|x| x.name().to_string()))
}

/// Linear-interpolating 48 kHz → device-rate reader over the ring, with the
/// cushion and the ceiling. Separate from the callback so it can be tested
/// without a sound card.
pub struct Resampler {
    rx: Consumer<f32>,
    /// Input samples per output sample.
    step: f64,
    /// Position between `a` and `b`, 0..1.
    frac: f64,
    a: f32,
    b: f32,
    waiting: bool,
}

impl Resampler {
    pub fn new(rx: Consumer<f32>, device_rate: u32) -> Self {
        Resampler {
            rx,
            step: AUDIO_RATE / device_rate.max(1) as f64,
            frac: 0.0,
            a: 0.0,
            b: 0.0,
            waiting: true,
        }
    }

    /// The next output sample, and whether it was real audio (not an underrun).
    pub fn next_sample(&mut self) -> (f32, bool) {
        if self.waiting {
            if self.rx.slots() < CUSHION {
                return (0.0, true);
            }
            self.waiting = false;
        }
        // Too far behind the radio: catch up by dropping the oldest.
        let queued = self.rx.slots();
        if queued > CEILING {
            let drop = queued - CUSHION;
            if let Ok(c) = self.rx.read_chunk(drop) {
                c.commit_all();
            }
        }
        self.frac += self.step;
        while self.frac >= 1.0 {
            match self.rx.pop() {
                Ok(v) => {
                    self.a = self.b;
                    self.b = v;
                    self.frac -= 1.0;
                }
                Err(_) => {
                    self.waiting = true;
                    self.frac = 0.0;
                    return (0.0, false);
                }
            }
        }
        (self.a + (self.b - self.a) * self.frac as f32, true)
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    rx: Consumer<f32>,
    controls: Arc<Controls>,
) -> Result<cpal::Stream, String> {
    let rate = config.sample_rate;
    let channels = config.channels.max(1) as usize;
    let mut rs = Resampler::new(rx, rate);
    let step = 1.0 / (RAMP_S * rate as f32);
    let mut gain = 0.0f32;
    let on_err = controls.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let want = if controls.muted.load(Ordering::Relaxed) {
                    0.0
                } else {
                    f32::from_bits(controls.volume.load(Ordering::Relaxed))
                };
                let mut ran_dry = false;
                for frame in data.chunks_mut(channels) {
                    gain += (want - gain).clamp(-step, step);
                    let (s, ok) = rs.next_sample();
                    ran_dry |= !ok;
                    let v = (s * gain).clamp(-1.0, 1.0).to_sample::<T>();
                    for x in frame.iter_mut() {
                        *x = v;
                    }
                }
                if ran_dry {
                    controls.underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            move |e| {
                tracing::warn!("receiver audio: {e}");
                on_err.failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_48k_tone_comes_out_at_the_device_rate_with_its_pitch() {
        for device_rate in [44_100u32, 48_000, 96_000] {
            let (mut feed, rx, _) = audio_ring();
            let tone: Vec<f32> = (0..24_000)
                .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / AUDIO_RATE).sin() as f32)
                .collect();
            let mut rs = Resampler::new(rx, device_rate);
            // Fed as a radio would, 10 ms at a time, and read as a card would.
            let mut out = Vec::new();
            for block in tone.chunks(480) {
                feed.push(block);
                out.extend((0..device_rate / 100).map(|_| rs.next_sample().0));
            }
            // Count rising zero crossings over the last 0.2 s.
            let tail = &out[out.len() - (device_rate as usize / 5)..];
            assert!(tail.iter().all(|s| s.abs() <= 1.0));
            let ups = tail.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            assert!((199..=201).contains(&ups), "{device_rate}: {ups} cycles in 0.2 s");
            let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.95 && peak <= 1.0);
        }
    }

    #[test]
    fn the_ring_waits_for_a_cushion_and_drops_what_is_too_far_ahead() {
        let (mut feed, rx, dropped) = audio_ring();
        let mut rs = Resampler::new(rx, 48_000);
        feed.push(&[0.5; 100]);
        assert_eq!(rs.next_sample(), (0.0, true), "waits for the cushion");
        feed.push(&vec![0.5; RING]);
        assert!(dropped.load(Ordering::Relaxed) > 0, "the overflow was counted");
        rs.next_sample();
        assert!(rs.rx.slots() <= CUSHION, "caught up to the cushion");
    }
}
