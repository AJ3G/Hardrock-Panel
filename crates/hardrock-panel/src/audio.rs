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

/// The last stretch of audio sent to the radio: what the scope draws of our
/// own transmission while the radio is keyed (decision 74).
pub(crate) use crate::scope::txsig::TxAudio;

/// The rate [`Tone::recent`] renders the tone at: its spectrum is one line
/// whatever the output's rate.
const TONE_VIEW_RATE: u32 = 48_000;

/// A tone playing, or ready to.
#[derive(Default)]
pub(crate) struct Tone {
    stream: Option<cpal::Stream>,
    /// The wanted amplitude, as `f32` bits, read by the audio thread.
    level: Arc<AtomicU32>,
    /// The device it plays on.
    device: Option<String>,
}

impl Tone {
    pub(crate) fn playing(&self) -> bool {
        self.stream.is_some()
    }

    pub(crate) fn set_gain(&self, g: f32) {
        self.level.store(g.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The last `n` samples of the tone as it plays, at its level: the same
    /// sine the output callback makes, for the scope (decision 74). `None`
    /// while it is not playing.
    pub(crate) fn recent(&self, n: usize) -> Option<TxAudio> {
        self.stream.as_ref()?;
        let g = f32::from_bits(self.level.load(Ordering::Relaxed));
        let inc = std::f32::consts::TAU * TONE_HZ / TONE_VIEW_RATE as f32;
        let samples = (0..n).map(|i| (i as f32 * inc).cos() * g).collect();
        Some(TxAudio { rate: TONE_VIEW_RATE, samples })
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
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&device, config, self.level.clone()),
            SampleFormat::I16 => build::<i16>(&device, config, self.level.clone()),
            SampleFormat::I32 => build::<i32>(&device, config, self.level.clone()),
            SampleFormat::U16 => build::<u16>(&device, config, self.level.clone()),
            other => {
                return Err(format!("\"{name}\" wants {other:?} samples, which aren't supported"));
            }
        }?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        self.device = Some(name.to_string());
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.set_gain(0.0);
        self.stream = None;
        self.device = None;
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: Arc<AtomicU32>,
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
                    let v = (phase.cos() * now).to_sample::<T>();
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
    /// The message's samples and the output's rate, for [`Player::recent`].
    buf: Option<Arc<Vec<f32>>>,
    rate: u32,
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

    /// The last `n` samples of the message that have gone to the output, at
    /// their level, for the scope (decision 74): `None` before the message
    /// starts and once it has ended.
    pub(crate) fn recent(&self, n: usize) -> Option<TxAudio> {
        let buf = self.buf.as_ref().filter(|_| self.stream.is_some())?;
        let g = f32::from_bits(self.level.load(Ordering::Relaxed));
        let at = played(self.pos.load(Ordering::Relaxed), self.lead, buf.len())?;
        let samples = buf[at.saturating_sub(n)..at].iter().map(|v| v * g).collect();
        Some(TxAudio { rate: self.rate, samples })
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
        self.buf = Some(buf.clone());
        self.rate = rate;
        let (lv, pos) = (self.level.clone(), self.pos.clone());
        let stream = match fmt {
            SampleFormat::F32 => build_buf::<f32>(&device, config, lv, pos, buf, lead),
            SampleFormat::I16 => build_buf::<i16>(&device, config, lv, pos, buf, lead),
            SampleFormat::I32 => build_buf::<i32>(&device, config, lv, pos, buf, lead),
            SampleFormat::U16 => build_buf::<u16>(&device, config, lv, pos, buf, lead),
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
        self.buf = None;
    }
}

/// How much of a message `len` samples long has played at output position
/// `pos` after `lead` samples of silence: `None` before it starts and after
/// it ends.
fn played(pos: usize, lead: usize, len: usize) -> Option<usize> {
    pos.checked_sub(lead).filter(|&at| at > 0 && at <= len)
}

fn build_buf<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    level: Arc<AtomicU32>,
    pos: Arc<std::sync::atomic::AtomicUsize>,
    buf: Arc<Vec<f32>>,
    lead: usize,
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
                    let v = (v * now).to_sample::<T>();
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
    fn what_has_played_is_counted_from_the_message_start() {
        assert_eq!(played(100, 480, 1000), None);
        assert_eq!(played(480, 480, 1000), None);
        assert_eq!(played(500, 480, 1000), Some(20));
        assert_eq!(played(1480, 480, 1000), Some(1000));
        assert_eq!(played(1481, 480, 1000), None);
    }

    #[test]
    fn the_duos_audio_is_found_by_name() {
        let names = ["Speakers (Realtek)", "Speakers (FDM-DUO Audio v1.04)"].map(String::from);
        assert_eq!(radio_device(&names).as_deref(), Some("Speakers (FDM-DUO Audio v1.04)"));
        assert_eq!(radio_device(&names[..1]), None);
    }
}
