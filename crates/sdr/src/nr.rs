//! NR: the receiver's noise reduction (AJ3G, 31 Oct 2026: "Add Noise
//! Reduction or NR for CW ... to include SSB").
//!
//! An adaptive line enhancer, as the LMS noise reduction in WDSP (the NR of
//! PowerSDR, Thetis and SDR Console's "ANR") and in most radios' NR1 works:
//! a normalised-LMS filter predicts each sample from the ones a short delay
//! before it. What can be predicted, a CW note or the harmonics of a voice,
//! comes through; white noise, which can't be, is left behind. The output
//! is the prediction.
//!
//! Four levels: more taps (a sharper picture of the spectrum) and a smaller
//! step (a slower, quieter adaptation) as the level rises. Level 1 is light
//! and suits SSB; 3 and 4 are for CW, where a note is easy to predict and
//! the hiss is what goes.

/// Taps, delay (samples) and step for levels 1 to 4, at 48 kHz.
const LEVELS: [(usize, usize, f32); 4] =
    [(48, 12, 0.020), (64, 16, 0.012), (96, 16, 0.007), (128, 24, 0.004)];
/// Leakage: the weights decay towards nothing at this rate times the step,
/// so a signal that goes leaves no ringing behind.
const LEAK: f32 = 0.01;

/// The noise reduction on one audio stream.
pub struct Nr {
    level: u8,
    taps: usize,
    delay: usize,
    mu: f32,
    w: Vec<f32>,
    /// The last `taps + delay` samples, twice over so a window is one slice.
    hist: Vec<f32>,
    pos: usize,
}

impl Default for Nr {
    fn default() -> Self {
        Nr::new(0)
    }
}

impl Nr {
    /// Off (0) or level 1 to 4.
    pub fn new(level: u8) -> Self {
        let mut n =
            Nr { level: 0, taps: 0, delay: 0, mu: 0.0, w: Vec::new(), hist: Vec::new(), pos: 0 };
        n.set_level(level);
        n
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    /// Off (0) or 1 to 4; anything above 4 is 4. The filter starts again.
    pub fn set_level(&mut self, level: u8) {
        let level = level.min(LEVELS.len() as u8);
        self.level = level;
        if level == 0 {
            self.w.clear();
            self.hist.clear();
            return;
        }
        let (taps, delay, mu) = LEVELS[level as usize - 1];
        self.taps = taps;
        self.delay = delay;
        self.mu = mu;
        self.w = vec![0.0; taps];
        self.hist = vec![0.0; 2 * (taps + delay)];
        self.pos = 0;
    }

    /// Reduce the noise in `x` in place.
    pub fn process(&mut self, x: &mut [f32]) {
        if self.level == 0 {
            return;
        }
        let len = self.taps + self.delay;
        for s in x.iter_mut() {
            // The window: the `taps` samples that end `delay` before this
            // one, oldest first.
            let start = self.pos;
            let r = &self.hist[start..start + self.taps];
            let mut y = 0.0f32;
            let mut power = 1e-9f32;
            for (w, v) in self.w.iter().zip(r) {
                y += w * v;
                power += v * v;
            }
            let e = *s - y;
            let g = self.mu * e / power;
            let keep = 1.0 - self.mu * LEAK;
            for (w, v) in self.w.iter_mut().zip(r) {
                *w = *w * keep + g * v;
            }
            // Push this sample: the history holds the newest last.
            self.hist[self.pos] = *s;
            self.hist[self.pos + len] = *s;
            self.pos = (self.pos + 1) % len;
            *s = if y.is_finite() { y } else { 0.0 };
            if !y.is_finite() {
                self.w.iter_mut().for_each(|w| *w = 0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn gauss(&mut self) -> f32 {
            let mut u = || {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 >> 11) as f64 / (1u64 << 53) as f64
            };
            let (a, b) = (u().max(1e-12), u());
            ((-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()) as f32
        }
    }

    /// The tone's power and everything else's in `x`, by projecting onto
    /// the tone at `f` Hz.
    fn split(x: &[f32], f: f64) -> (f64, f64) {
        let n = x.len() as f64;
        let (mut c, mut s) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let p = std::f64::consts::TAU * f * i as f64 / 48_000.0;
            c += *v as f64 * p.cos();
            s += *v as f64 * p.sin();
        }
        let tone = 2.0 * (c * c + s * s) / (n * n);
        let all: f64 = x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / n;
        (tone, (all - tone).max(1e-12))
    }

    fn snr_db(level: u8, f: f64) -> (f64, f64) {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let n = 48_000 * 4;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                0.3 * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin() as f32
                    + 0.3 * rng.gauss()
            })
            .collect();
        let mut y = x.clone();
        Nr::new(level).process(&mut y);
        // After two seconds to settle.
        let (t0, n0) = split(&x[n / 2..], f);
        let (t1, n1) = split(&y[n / 2..], f);
        (10.0 * (t0 / n0).log10(), 10.0 * (t1 / n1).log10())
    }

    #[test]
    fn off_changes_nothing() {
        let mut x = vec![0.5, -0.25, 0.125];
        Nr::new(0).process(&mut x);
        assert_eq!(x, [0.5, -0.25, 0.125]);
    }

    #[test]
    fn each_level_lifts_a_cw_note_out_of_the_hiss() {
        let mut last = f64::MIN;
        for level in 1..=4 {
            let (before, after) = snr_db(level, 700.0);
            eprintln!("NR{level}: {before:.1} dB to {after:.1} dB");
            assert!(after > before + 6.0, "NR{level}: {before:.1} to {after:.1} dB");
            assert!(after > last - 1.0, "NR{level} is no stronger than the one below it");
            last = after;
        }
    }

    #[test]
    fn the_note_keeps_its_level() {
        let mut rng = Rng(7);
        let x: Vec<f32> = (0..96_000)
            .map(|i| {
                0.3 * (std::f64::consts::TAU * 1500.0 * i as f64 / 48_000.0).sin() as f32
                    + 0.1 * rng.gauss()
            })
            .collect();
        let mut y = x.clone();
        Nr::new(2).process(&mut y);
        let (t0, _) = split(&x[48_000..], 1500.0);
        let (t1, _) = split(&y[48_000..], 1500.0);
        let db = 10.0 * (t1 / t0).log10();
        assert!(db.abs() < 3.0, "the note moved {db:.1} dB");
    }
}
