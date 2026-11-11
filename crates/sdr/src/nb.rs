// Adapted from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/nb.rs, GPL-3.0-or-later. See NOTICE.md. Here it
// works on interleaved I/Q and its threshold can be set.

//! NB: the panel receiver's impulse noise blanker (AJ3G, 11 Nov 2026: "Add
//! NB and slide to scope").
//!
//! A sample whose magnitude jumps far above the running average, ignition
//! noise or a power-line crackle, is set to zero. It runs on the I/Q at the
//! radio's rate, ahead of the spectrum and the receivers, as SDRoxide's does:
//! an impulse is only an impulse before a decimating filter smears it over a
//! filter length.
//!
//! The threshold is how many times the average a sample must reach to be
//! blanked: lower blanks more. SDRoxide uses 5.

/// SDRoxide's threshold, and the panel's default.
pub const DEFAULT_THRESHOLD: f32 = 5.0;
/// The threshold's range on the scope's slider.
pub const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 2.0..=20.0;
/// Samples of running-average settling before blanking starts.
pub const WARMUP: u32 = 8_192;
/// How fast the average follows ordinary samples, per sample.
const ALPHA: f32 = 5e-4;

/// The blanker on one I/Q stream.
pub struct NoiseBlanker {
    threshold: f32,
    mean: f32,
    seen: u32,
}

impl Default for NoiseBlanker {
    fn default() -> Self {
        NoiseBlanker::new(DEFAULT_THRESHOLD)
    }
}

impl NoiseBlanker {
    pub fn new(threshold: f32) -> Self {
        let mut nb = NoiseBlanker { threshold: DEFAULT_THRESHOLD, mean: 0.0, seen: 0 };
        nb.set_threshold(threshold);
        nb
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Times the average a sample must reach to be blanked, kept within
    /// [`THRESHOLD_RANGE`]. The average already learnt is kept.
    pub fn set_threshold(&mut self, threshold: f32) {
        let t = if threshold.is_finite() { threshold } else { DEFAULT_THRESHOLD };
        self.threshold = t.clamp(*THRESHOLD_RANGE.start(), *THRESHOLD_RANGE.end());
    }

    /// Forget the average and settle again for [`WARMUP`] samples.
    pub fn reset(&mut self) {
        self.mean = 0.0;
        self.seen = 0;
    }

    /// Blank the impulses in `iq`, interleaved I and Q, in place.
    pub fn process(&mut self, iq: &mut [f32]) {
        for s in iq.chunks_exact_mut(2) {
            let mag = s[0].hypot(s[1]);
            // Track the mean of ordinary samples only, so impulses don't
            // raise their own threshold.
            if self.seen < WARMUP || mag < self.threshold * self.mean {
                self.mean += ALPHA * (mag - self.mean);
                self.seen = self.seen.saturating_add(1);
            } else {
                s[0] = 0.0;
                s[1] = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A steady tone of magnitude 0.1, `n` samples, with impulses of 100
    /// times that wherever `impulse(i)` says.
    fn tone(n: usize, impulse: impl Fn(usize) -> bool) -> Vec<f32> {
        let mut v = Vec::with_capacity(2 * n);
        for i in 0..n {
            let ph = std::f32::consts::TAU * 0.01 * i as f32;
            let k = if impulse(i) { 100.0 } else { 1.0 };
            v.push(0.1 * k * ph.cos());
            v.push(0.1 * k * ph.sin());
        }
        v
    }

    fn mag(iq: &[f32], i: usize) -> f32 {
        iq[2 * i].hypot(iq[2 * i + 1])
    }

    #[test]
    fn an_impulse_is_blanked() {
        let w = WARMUP as usize;
        let mut nb = NoiseBlanker::default();
        let mut iq = tone(60_000, |i| i > w && i % 10_000 == 0);
        nb.process(&mut iq);
        let peak = (w..60_000).map(|i| mag(&iq, i)).fold(0.0, f32::max);
        assert!(peak < 0.2, "impulse survived: {peak}");
        assert_eq!(mag(&iq, 20_000), 0.0);
    }

    #[test]
    fn a_steady_signal_is_untouched() {
        let mut nb = NoiseBlanker::default();
        let mut iq = tone(60_000, |_| false);
        let before = iq.clone();
        nb.process(&mut iq);
        assert_eq!(iq, before);
        // Also in blocks of odd sizes, as the pipeline hands them over.
        let mut nb = NoiseBlanker::new(2.0);
        let mut iq = tone(30_000, |_| false);
        for block in iq.chunks_mut(2 * 777) {
            nb.process(block);
        }
        assert_eq!(iq, tone(30_000, |_| false));
    }

    #[test]
    fn nothing_is_blanked_while_warming_up() {
        let mut nb = NoiseBlanker::default();
        // An impulse inside the warm-up passes, and an impulse just after it
        // is blanked.
        let w = WARMUP as usize;
        let mut iq = tone(w + 1000, |i| i == 4000 || i == w + 500);
        nb.process(&mut iq);
        assert!((mag(&iq, 4000) - 10.0).abs() < 1e-3);
        assert_eq!(mag(&iq, w + 500), 0.0);
        // After a reset it settles again before blanking.
        nb.reset();
        let mut iq = tone(1000, |i| i == 500);
        nb.process(&mut iq);
        assert!((mag(&iq, 500) - 10.0).abs() < 1e-3);
    }

    #[test]
    fn a_lower_threshold_blanks_more() {
        let w = WARMUP as usize;
        // A burst at 4 times the tone: under SDRoxide's 5, over 3.
        let burst = |iq: &mut Vec<f32>| {
            iq[2 * (w + 100)] *= 4.0;
            iq[2 * (w + 100) + 1] *= 4.0;
        };
        let mut iq = tone(w + 200, |_| false);
        burst(&mut iq);
        NoiseBlanker::new(5.0).process(&mut iq);
        assert!(mag(&iq, w + 100) > 0.3);
        let mut iq = tone(w + 200, |_| false);
        burst(&mut iq);
        NoiseBlanker::new(3.0).process(&mut iq);
        assert_eq!(mag(&iq, w + 100), 0.0);
    }

    #[test]
    fn the_threshold_stays_in_range() {
        assert_eq!(NoiseBlanker::new(0.5).threshold(), 2.0);
        assert_eq!(NoiseBlanker::new(99.0).threshold(), 20.0);
        assert_eq!(NoiseBlanker::new(f32::NAN).threshold(), DEFAULT_THRESHOLD);
    }
}
