//! CW from the receiver's audio, for DECODE: the strongest tone in the
//! passband found and followed, mixed to zero and low-passed into a keying
//! envelope, and read by the same timing-fit decoder the CW skimmer uses
//! ([`crate::morse::CwDecoder`]). Whatever the pitch (the panel's receiver
//! puts a CW station at 600 Hz; the radio's own CW pitch is a menu setting),
//! the tone is found in [`PITCH_LO`] to [`PITCH_HI`].

use crate::morse::CwDecoder;

/// Where the tone is looked for, Hz.
pub const PITCH_LO: f64 = 200.0;
pub const PITCH_HI: f64 = 1500.0;
/// The keying envelope's rate, Hz (5 ms a sample: a 60 WPM dit is four).
const ENV_HZ: f64 = 200.0;
/// The envelope filter's corner, Hz: two one-pole stages on I and on Q.
const LP_HZ: f64 = 70.0;
/// The pitch search: a spectrum of the last second, every half second, at
/// this spacing, from audio averaged down to about this rate.
const SEARCH_STEP_HZ: f64 = 5.0;
const SEARCH_RATE: f64 = 4000.0;
const SEARCH_EVERY_S: f64 = 0.5;
/// A tone this far over the spectrum's median is a signal (power ratio).
const SIGNAL_OVER: f64 = 10.0;
/// A new tone takes over when it is this much stronger than the one being
/// read, or that one has gone.
const TAKE_OVER: f64 = 2.0;

pub struct CwRx {
    rate: f64,
    pitch: f64,
    /// Mixer phase, and the low-pass's state: two stages, I and Q.
    phase: f64,
    lp: [[f64; 2]; 2],
    alpha: f64,
    /// Samples to an envelope sample, and the count towards it.
    decim: usize,
    count: usize,
    dec: CwDecoder,
    /// The search: averaged audio, the boxcar towards it, and a smoothed
    /// spectrum (one value per step from PITCH_LO).
    search: Vec<f32>,
    box_n: usize,
    box_sum: f32,
    box_count: usize,
    since_search: usize,
    spectrum: Vec<f64>,
}

impl CwRx {
    pub fn new(rate: f64) -> Self {
        let decim = (rate / ENV_HZ).round().max(1.0) as usize;
        let box_n = (rate / SEARCH_RATE).round().max(1.0) as usize;
        let bins = ((PITCH_HI - PITCH_LO) / SEARCH_STEP_HZ) as usize + 1;
        CwRx {
            rate,
            pitch: 600.0,
            phase: 0.0,
            lp: [[0.0; 2]; 2],
            alpha: 1.0 - (-std::f64::consts::TAU * LP_HZ / rate).exp(),
            decim,
            count: 0,
            dec: CwDecoder::new((rate / decim as f64) as f32),
            search: Vec::new(),
            box_n,
            box_sum: 0.0,
            box_count: 0,
            since_search: 0,
            spectrum: vec![0.0; bins],
        }
    }

    /// The tone being read, Hz.
    pub fn pitch(&self) -> f64 {
        self.pitch
    }

    /// The speed read, WPM, while it is copying.
    pub fn wpm(&self) -> Option<f32> {
        self.dec.locked().then(|| self.dec.wpm())
    }

    pub fn process(&mut self, audio: &[f32]) -> String {
        let mut text = String::new();
        let step = std::f64::consts::TAU * self.pitch / self.rate;
        let mut env = Vec::with_capacity(audio.len() / self.decim + 1);
        for &x in audio {
            let x = x as f64;
            let (s, c) = self.phase.sin_cos();
            self.phase = (self.phase + step) % std::f64::consts::TAU;
            let mut v = [x * c, -x * s];
            for stage in &mut self.lp {
                for (st, v) in stage.iter_mut().zip(v.iter_mut()) {
                    *st += self.alpha * (*v - *st);
                    *v = *st;
                }
            }
            self.count += 1;
            if self.count == self.decim {
                self.count = 0;
                env.push(v[0].hypot(v[1]) as f32);
            }
            self.box_sum += x as f32;
            self.box_count += 1;
            if self.box_count == self.box_n {
                self.search.push(self.box_sum / self.box_n as f32);
                self.box_sum = 0.0;
                self.box_count = 0;
            }
        }
        text.push_str(&self.dec.push(&env));
        self.since_search += audio.len();
        if self.since_search as f64 >= SEARCH_EVERY_S * self.rate {
            self.since_search = 0;
            self.find_pitch();
        }
        text
    }

    /// Look for the strongest tone, and move to it when it is clearly a
    /// signal and clearly stronger than the one being read.
    fn find_pitch(&mut self) {
        let fs = self.rate / self.box_n as f64;
        let keep = fs as usize;
        if self.search.len() > keep {
            let drop = self.search.len() - keep;
            self.search.drain(..drop);
        }
        if self.search.len() < keep / 2 {
            return;
        }
        for (k, smooth) in self.spectrum.iter_mut().enumerate() {
            let f = PITCH_LO + k as f64 * SEARCH_STEP_HZ;
            let p = goertzel(&self.search, f / fs);
            // Smoothed over a few searches, so keying gaps don't matter.
            *smooth = if *smooth == 0.0 { p } else { 0.6 * *smooth + 0.4 * p };
        }
        let mut sorted = self.spectrum.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2].max(1e-20);
        let (k, &peak) = self
            .spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("the spectrum has bins");
        let f = PITCH_LO + k as f64 * SEARCH_STEP_HZ;
        let at = |hz: f64| {
            let k = ((hz - PITCH_LO) / SEARCH_STEP_HZ).round() as usize;
            self.spectrum.get(k).copied().unwrap_or(0.0)
        };
        let here = at(self.pitch);
        if peak < SIGNAL_OVER * median || (f - self.pitch).abs() <= 2.0 * SEARCH_STEP_HZ {
            return;
        }
        if here < SIGNAL_OVER * median || peak > TAKE_OVER * here {
            self.pitch = f;
            self.dec.reset();
        }
    }
}

/// The power at `f` cycles a sample in `x`.
fn goertzel(x: &[f32], f: f64) -> f64 {
    let w = std::f64::consts::TAU * f;
    let k = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for &v in x {
        let s = v as f64 + k * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - k * s1 * s2) / (x.len() as f64).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::morse::morse_encode;

    /// `text` in CW at `wpm` on `hz`, with noise `snr_db` under the tone's
    /// power in 2.5 kHz, at 48 kHz, after a second of noise.
    fn cw_audio(text: &str, hz: f64, wpm: f64, snr_db: f64) -> Vec<f32> {
        let rate = 48_000.0;
        let dit = 1.2 / wpm;
        let mut keys = Vec::new();
        let mut t = 1.0;
        for ch in text.chars() {
            if ch == ' ' {
                t += 4.0 * dit;
                continue;
            }
            for (i, e) in morse_encode(ch).unwrap().chars().enumerate() {
                if i > 0 {
                    t += dit;
                }
                let len = if e == '-' { 3.0 * dit } else { dit };
                keys.push((t, t + len));
                t += len;
            }
            t += 3.0 * dit;
        }
        let n = ((t + 1.5) * rate) as usize;
        // Noise power in 2.5 kHz relative to a tone of amplitude 0.1.
        let tone_p = 0.1f64 * 0.1 / 2.0;
        let sigma = (tone_p / 10f64.powf(snr_db / 10.0) * (rate / 2.0) / 2500.0).sqrt();
        let mut seed = 0x1234_5678_u64;
        let mut gauss = || {
            let mut u = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 11) as f64 + 0.5) / (1u64 << 53) as f64
            };
            (-2.0 * u().ln()).sqrt() * (std::f64::consts::TAU * u()).cos()
        };
        let mut k = 0;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                while k < keys.len() && t > keys[k].1 + 0.005 {
                    k += 1;
                }
                let on = keys.get(k).map_or(0.0, |&(a, b)| {
                    let edge = |d: f64| (d / 0.005).clamp(0.0, 1.0);
                    edge(t - a).min(edge(b + 0.005 - t))
                });
                (0.1 * on * (std::f64::consts::TAU * hz * t).sin() + sigma * gauss()) as f32
            })
            .collect()
    }

    fn read(audio: &[f32]) -> (String, CwRx) {
        let mut rx = CwRx::new(48_000.0);
        let mut text = String::new();
        for block in audio.chunks(4800) {
            text.push_str(&rx.process(block));
        }
        (text, rx)
    }

    #[test]
    fn it_finds_the_tone_and_reads_it() {
        let msg = "CQ CQ CQ DE AJ3G AJ3G AJ3G K";
        let (text, rx) = read(&cw_audio(msg, 720.0, 22.0, 0.0));
        assert!((rx.pitch() - 720.0).abs() <= 10.0, "pitch {}", rx.pitch());
        assert!(text.contains("AJ3G"), "{text:?}");
    }

    #[test]
    fn it_reads_fast_and_slow_and_at_another_pitch() {
        for (wpm, hz) in [(12.0, 450.0), (35.0, 1000.0)] {
            let msg = "TEST DE W1AW W1AW W1AW K";
            let (text, _) = read(&cw_audio(msg, hz, wpm, 3.0));
            assert!(text.contains("W1AW"), "{wpm} WPM at {hz} Hz: {text:?}");
        }
    }

    #[test]
    fn noise_alone_prints_next_to_nothing() {
        let audio = cw_audio(&" ".repeat(40), 700.0, 20.0, 0.0);
        let (text, _) = read(&audio[..48_000 * 9]);
        assert!(text.trim().len() <= 2, "{text:?}");
    }
}
