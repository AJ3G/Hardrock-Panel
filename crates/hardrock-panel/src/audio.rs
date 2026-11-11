//! The tone TUNE plays into the radio's USB audio, as SDRoxide's TUNE did: a
//! steady 1 kHz sine, which on USB or LSB is a single carrier 1 kHz from the
//! dial. Its level is the Drive slider, scaled into the radio's audio
//! ceiling (`rigcat::Protocol::audio_ceiling`), so the slider spans the
//! radio's output instead of the bottom quarter of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};

const TONE_HZ: f32 = 1000.0;
/// How long the tone takes to rise or fall to a new level: short enough to
/// follow the slider, long enough not to click.
const RAMP_S: f32 = 0.005;

/// The tone's amplitude, 0 to 1 of full scale, for Drive `frac` (0 to 1).
/// Drive is a share of power, so the amplitude is its square root, as in
/// SDRoxide (`audio_gain_for_power` there).
pub(crate) fn gain_for(frac: f32, ceiling: f32) -> f32 {
    (frac.clamp(0.0, 1.0) * ceiling.clamp(0.0, 1.0)).sqrt()
}

/// The sound outputs on this machine, by name.
pub(crate) fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = host
        .output_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|x| x.name().to_string())).collect())
        .unwrap_or_default();
    names.dedup();
    names
}

/// The radio's own USB audio, among `names`: the FDM-DUO shows as
/// "FDM-DUO Audio" on Windows.
pub(crate) fn radio_device(names: &[String]) -> Option<String> {
    names
        .iter()
        .find(|n| {
            let n = n.to_ascii_uppercase();
            n.contains("FDM-DUO") || n.contains("FDM DUO") || n.contains("ELAD")
        })
        .cloned()
}

/// Audio sent to the radio, every sample of it in order: what the scope's
/// TX monitor analyses while the radio is keyed (decision 83).
pub(crate) use crate::scope::txsig::TxAudio;

/// How much a [`Tap`] holds between two reads, seconds: a repaint that comes
/// later than this loses the oldest samples, never the newest.
const TAP_SECS: f64 = 1.0;

/// Every sample an output stream sends, as it sends it: the output callback
/// pushes each one (never waiting; a full ring drops it), and the scope's
/// repaint takes what came since its last read ([`Tap::take`]).
struct Tap {
    rate: u32,
    rx: std::sync::Mutex<rtrb::Consumer<f32>>,
}

impl Tap {
    /// A tap for a stream at `rate`, and the end its callback pushes to.
    fn new(rate: u32) -> (Tap, rtrb::Producer<f32>) {
        let (tx, rx) = rtrb::RingBuffer::new((rate as f64 * TAP_SECS).max(1024.0) as usize);
        (Tap { rate, rx: std::sync::Mutex::new(rx) }, tx)
    }

    /// The samples sent since the last read, oldest first: all of them, or
    /// the newest `max` when more came.
    fn take(&self, max: usize) -> TxAudio {
        let mut rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let n = rx.slots();
        let mut samples = Vec::with_capacity(n.min(max));
        if let Ok(chunk) = rx.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            let skip = n.saturating_sub(max);
            samples.extend(a.iter().chain(b).skip(skip));
            chunk.commit_all();
        }
        TxAudio { rate: self.rate, samples }
    }
}

/// A tone playing, or ready to.
#[derive(Default)]
pub(crate) struct Tone {
    stream: Option<cpal::Stream>,
    /// The wanted amplitude, as `f32` bits, read by the audio thread.
    level: Arc<AtomicU32>,
    /// The device it plays on.
    device: Option<String>,
    /// What it has sent, for the scope.
    tap: Option<Tap>,
}

impl Tone {
    pub(crate) fn playing(&self) -> bool {
        self.stream.is_some()
    }

    pub(crate) fn set_gain(&self, g: f32) {
        self.level.store(g.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The samples of the tone sent to the radio since the last call (at
    /// most the newest `max`), exactly as the output callback made them, for
    /// the scope's TX monitor (decision 83). `None` while it is not playing.
    pub(crate) fn recent(&self, max: usize) -> Option<TxAudio> {
        self.stream.as_ref()?;
        Some(self.tap.as_ref()?.take(max))
    }

    /// Start the tone on the output named `name`, at amplitude `gain`.
    pub(crate) fn start(&mut self, name: &str, gain: f32) -> Result<(), String> {
        self.set_gain(gain);
        if self.stream.is_some() && self.device.as_deref() == Some(name) {
            return Ok(());
        }
        self.stop();
        let host = cpal::default_host();
        let device = host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.description().is_ok_and(|x| x.name() == name))
            .ok_or_else(|| {
                format!("The sound output \"{name}\" isn't there. Is the radio's USB lead in?")
            })?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let fmt = supported.sample_format();
        let config = supported.config();
        let (tap, push) = Tap::new(config.sample_rate);
        let lv = self.level.clone();
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&device, config, lv, push),
            SampleFormat::I16 => build::<i16>(&device, config, lv, push),
            SampleFormat::I32 => build::<i32>(&device, config, lv, push),
            SampleFormat::U16 => build::<u16>(&device, config, lv, push),
            other => {
                return Err(format!("\"{name}\" wants {other:?} samples, which aren't supported"));
            }
        }?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        self.device = Some(name.to_string());
        self.tap = Some(tap);
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.set_gain(0.0);
        self.stream = None;
        self.device = None;
        self.tap = None;
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: Arc<AtomicU32>,
    mut tap: rtrb::Producer<f32>,
) -> Result<cpal::Stream, String> {
    let rate = config.sample_rate as f32;
    let channels = config.channels.max(1) as usize;
    let inc = std::f32::consts::TAU * TONE_HZ / rate;
    let step = 1.0 / (RAMP_S * rate);
    let (mut phase, mut now) = (0.0f32, 0.0f32);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let want = f32::from_bits(level.load(Ordering::Relaxed));
                for frame in data.chunks_mut(channels) {
                    now += (want - now).clamp(-step, step);
                    let s = phase.cos() * now;
                    let _ = tap.push(s);
                    let v = s.to_sample::<T>();
                    phase += inc;
                    if phase > std::f32::consts::TAU {
                        phase -= std::f32::consts::TAU;
                    }
                    for x in frame.iter_mut() {
                        *x = v;
                    }
                }
            },
            |e| tracing::warn!("tune tone: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// A rendered message played once (an FT8 over, a WSPR beacon): silence
/// until its start time, then its samples at the output's own rate, then
/// silence. [`Player::finished`] says when the last sample has gone out.
#[derive(Default)]
pub(crate) struct Player {
    stream: Option<cpal::Stream>,
    level: Arc<AtomicU32>,
    /// Samples played so far, counted from the start of the stream.
    pos: Arc<std::sync::atomic::AtomicUsize>,
    /// Silence before the message, then its length, in samples.
    lead: usize,
    len: usize,
    /// What it has sent, silence and message alike, for the scope.
    tap: Option<Tap>,
}

impl Player {
    pub(crate) fn playing(&self) -> bool {
        self.stream.is_some()
    }

    /// The last of the message has gone out.
    pub(crate) fn finished(&self) -> bool {
        self.stream.is_some() && self.pos.load(Ordering::Relaxed) >= self.lead + self.len
    }

    /// Its level, as the LEVEL slider moves it while it plays (decision 87):
    /// the output ramps to it over a few milliseconds, as TUNE's tone does,
    /// so a move doesn't click.
    pub(crate) fn set_gain(&self, g: f32) {
        self.level.store(g.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The level asked for.
    #[cfg(test)]
    pub(crate) fn gain(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// The samples that have gone to the output since the last call (at
    /// most the newest `max`), at their level, the silence before the
    /// message included, for the scope's TX monitor (decision 83): `None`
    /// while nothing plays.
    pub(crate) fn recent(&self, max: usize) -> Option<TxAudio> {
        self.stream.as_ref()?;
        Some(self.tap.as_ref()?.take(max))
    }

    /// Play on the output `name` at amplitude `gain`: `render` makes the
    /// samples at the output's rate, and they start at `start_at` (silence
    /// until then; a start already past cuts that much off the front).
    pub(crate) fn start(
        &mut self,
        name: &str,
        gain: f32,
        start_at: std::time::SystemTime,
        render: impl FnOnce(u32) -> Vec<f32>,
    ) -> Result<(), String> {
        self.stop();
        self.set_gain(gain);
        let host = cpal::default_host();
        let device = host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.description().is_ok_and(|x| x.name() == name))
            .ok_or_else(|| {
                format!("The sound output \"{name}\" isn't there. Is the radio's USB lead in?")
            })?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let fmt = supported.sample_format();
        let config = supported.config();
        let rate = config.sample_rate;
        let mut samples = render(rate);
        let now = std::time::SystemTime::now();
        let lead = match start_at.duration_since(now) {
            Ok(d) => (d.as_secs_f64() * rate as f64) as usize,
            Err(e) => {
                let late = ((e.duration().as_secs_f64() * rate as f64) as usize).min(samples.len());
                samples.drain(..late);
                0
            }
        };
        self.lead = lead;
        self.len = samples.len();
        self.pos.store(0, Ordering::Relaxed);
        let buf = Arc::new(samples);
        let (tap, push) = Tap::new(rate);
        self.tap = Some(tap);
        let (lv, pos) = (self.level.clone(), self.pos.clone());
        let at = (buf, lead, push);
        let stream = match fmt {
            SampleFormat::F32 => build_buf::<f32>(&device, config, lv, pos, at),
            SampleFormat::I16 => build_buf::<i16>(&device, config, lv, pos, at),
            SampleFormat::I32 => build_buf::<i32>(&device, config, lv, pos, at),
            SampleFormat::U16 => build_buf::<u16>(&device, config, lv, pos, at),
            other => {
                return Err(format!("\"{name}\" wants {other:?} samples, which aren't supported"));
            }
        }?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.set_gain(0.0);
        self.stream = None;
        self.tap = None;
    }
}

fn build_buf<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: Arc<AtomicU32>,
    pos: Arc<std::sync::atomic::AtomicUsize>,
    (buf, lead, mut tap): (Arc<Vec<f32>>, usize, rtrb::Producer<f32>),
) -> Result<cpal::Stream, String> {
    let channels = config.channels.max(1) as usize;
    let step = 1.0 / (RAMP_S * config.sample_rate as f32);
    // The level the output is at, ramping to the one asked for.
    let mut now = 0.0f32;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let want = f32::from_bits(level.load(Ordering::Relaxed));
                let mut p = pos.load(Ordering::Relaxed);
                for frame in data.chunks_mut(channels) {
                    now += (want - now).clamp(-step, step);
                    let v = p.checked_sub(lead).and_then(|i| buf.get(i)).copied().unwrap_or(0.0);
                    let s = v * now;
                    let _ = tap.push(s);
                    let v = s.to_sample::<T>();
                    for x in frame.iter_mut() {
                        *x = v;
                    }
                    p += 1;
                }
                pos.store(p, Ordering::Relaxed);
            },
            |e| tracing::warn!("transmit audio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_is_power_so_the_tone_is_its_square_root_within_the_ceiling() {
        assert_eq!(gain_for(1.0, 0.24), 0.24f32.sqrt());
        assert_eq!(gain_for(0.25, 1.0), 0.5);
        assert_eq!(gain_for(0.0, 0.24), 0.0);
        assert_eq!(gain_for(3.0, 0.24), gain_for(1.0, 0.24));
    }

    #[test]
    fn the_tap_gives_every_sample_once_in_order_and_keeps_the_newest() {
        let (tap, mut push) = Tap::new(48_000);
        for i in 0..10 {
            push.push(i as f32).unwrap();
        }
        let a = tap.take(100);
        assert_eq!((a.rate, a.samples), (48_000, (0..10).map(|i| i as f32).collect::<Vec<_>>()));
        assert!(tap.take(100).samples.is_empty(), "nothing twice");
        for i in 10..20 {
            push.push(i as f32).unwrap();
        }
        assert_eq!(tap.take(4).samples, vec![16.0, 17.0, 18.0, 19.0]);
        // A full ring drops what comes, never blocks the audio thread.
        let n = (48_000.0 * TAP_SECS) as usize;
        for _ in 0..n {
            push.push(1.0).unwrap();
        }
        assert!(push.push(2.0).is_err());
        assert_eq!(tap.take(usize::MAX).samples.len(), n);
    }

    #[test]
    fn the_duos_audio_is_found_by_name() {
        let names = ["Speakers (Realtek)", "Speakers (FDM-DUO Audio v1.04)"].map(String::from);
        assert_eq!(radio_device(&names).as_deref(), Some("Speakers (FDM-DUO Audio v1.04)"));
        assert_eq!(radio_device(&names[..1]), None);
    }
}
