//! Many narrow channels out of the wide I/Q for the price of one FFT
//! (decision 110).
//!
//! The wide skimmer's lanes each used to mix and filter every sample of the
//! I/Q on their own: at 6144 kHz, a dozen lanes were a dozen passes over six
//! million samples a second, on one thread that fell behind. Here the I/Q
//! is transformed once, by overlap-save fast convolution: each block of
//! `N/2` new samples, with the `N/2` before it, goes through one `N`-point
//! FFT shared by every lane. A lane then takes only the `K` bins around its
//! own frequency, weighted by the channel filter's response, and turns them
//! back into `K/2` samples at `rate/D` with a `K`-point inverse FFT: the
//! mix, the filter and the decimation by `D = N/K` in one step, at a few
//! hundred multiplies per block per lane.
//!
//! The lane comes out centred on the bin nearest the frequency asked for,
//! within half a bin of it; [`Channelizer::bin_of`] says how far off, for
//! the lane's own mixer to finish the job at the low rate.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner};
use sdr::Complex32;
use sdr::design::{FirWindow, lowpass_n};

/// The lowest output rate taken, Hz: the decimation is the largest power of
/// two that keeps the rate at least this. The filter is flat to a quarter of
/// the output rate (2 kHz here) and gone by half of it, the edge of the bins
/// a lane takes, so a lane's ±1.5 kHz is clean.
const MIN_OUT_RATE: f64 = 8000.0;
/// Bins a lane takes, and so the inverse FFT's size.
const BINS: usize = 64;
/// Larger than this between where a block was expected to start and its
/// time stamp, and the times are taken afresh (seconds).
const RESYNC_S: f64 = 0.05;

/// The shared stage: the I/Q in, one spectrum a block out.
pub struct Channelizer {
    rate: f64,
    n: usize,
    d: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    /// The channel filter's response, over `N` (it folds in the transform's
    /// 1/N, so a tone comes out at the level it went in).
    h: Vec<Complex32>,
    /// The filter's delay, input samples.
    delay: f64,
    /// The last `N/2` samples and the new ones coming in.
    buf: Vec<Complex32>,
    spec: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Frames made so far.
    frames: u64,
    /// Samples taken so far, and the UTC of sample `anchor.1`.
    taken: u64,
    anchor: Option<(f64, u64)>,
}

/// One block's spectrum, for each lane to take its bins from.
pub struct Frame<'a> {
    ch: &'a Channelizer,
    /// UTC of the frame's first output sample.
    pub utc: f64,
}

impl Channelizer {
    /// For I/Q at `rate`.
    pub fn new(rate: f64) -> Self {
        let mut d = 1usize;
        while rate / (2 * d) as f64 >= MIN_OUT_RATE {
            d *= 2;
        }
        let n = BINS * d;
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(BINS);
        // The longest filter overlap-save can take with half the window new:
        // N/2 + 1 taps, odd. A Blackman-Harris sinc that long is down 109 dB
        // within 8/len, a quarter of the output rate, so cut at 3/8 of it.
        let taps = lowpass_n(n / 2 - 1, 3.0 / (8.0 * d as f64), FirWindow::BlackmanHarris);
        let mut h = vec![Complex32::new(0.0, 0.0); n];
        for (i, &t) in taps.iter().enumerate() {
            h[i] = Complex32::new((t / n as f64) as f32, 0.0);
        }
        fwd.process(&mut h);
        Channelizer {
            rate,
            n,
            d,
            scratch: vec![Complex32::new(0.0, 0.0); fwd.get_inplace_scratch_len()],
            fwd,
            inv,
            h,
            delay: (taps.len() - 1) as f64 / 2.0,
            buf: vec![Complex32::new(0.0, 0.0); n / 2],
            spec: vec![Complex32::new(0.0, 0.0); n],
            frames: 0,
            taken: 0,
            anchor: None,
        }
    }

    /// The I/Q rate it takes.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// A lane's rate.
    pub fn out_rate(&self) -> f64 {
        self.rate / self.d as f64
    }

    /// The bin nearest `offset_hz` from the I/Q centre, and how far the
    /// offset is above that bin's centre, Hz (at most half a bin).
    pub fn bin_of(&self, offset_hz: f64) -> (i64, f64) {
        let bin_hz = self.rate / self.n as f64;
        let k = (offset_hz / bin_hz).round();
        (k as i64, offset_hz - k * bin_hz)
    }

    /// Complex I/Q whose first sample was heard at `utc`; `each` is called
    /// with every frame the samples finish.
    pub fn process(&mut self, iq: &[Complex32], utc: f64, mut each: impl FnMut(&Frame)) {
        let expect = self.anchor.map(|(t, at)| t + (self.taken - at) as f64 / self.rate);
        if expect.is_none_or(|e| (utc - e).abs() >= RESYNC_S) {
            self.anchor = Some((utc, self.taken));
        }
        let half = self.n / 2;
        let mut rest = iq;
        while !rest.is_empty() {
            let take = (self.n - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            self.taken += take as u64;
            if self.buf.len() < self.n {
                break;
            }
            self.spec.copy_from_slice(&self.buf);
            self.fwd.process_with_scratch(&mut self.spec, &mut self.scratch);
            // The window's newer half: the first sample a frame's output
            // stands for, less the filter's delay.
            let first = self.taken - half as u64;
            let (t0, at) = self.anchor.expect("anchored above");
            let utc = t0 + (first as f64 - at as f64 - self.delay) / self.rate;
            each(&Frame { ch: self, utc });
            self.frames += 1;
            self.buf.drain(..half);
        }
    }
}

/// One lane's cut of each [`Frame`].
pub struct Tap {
    bin: i64,
    buf: Vec<Complex32>,
}

impl Tap {
    /// Centred on `bin` ([`Channelizer::bin_of`]).
    pub fn new(bin: i64) -> Self {
        Tap { bin, buf: vec![Complex32::new(0.0, 0.0); BINS] }
    }

    pub fn set_bin(&mut self, bin: i64) {
        self.bin = bin;
    }

    /// The frame's `BINS/2` samples of this lane, at the lane rate,
    /// appended to `out`.
    pub fn take(&mut self, f: &Frame, out: &mut Vec<Complex32>) {
        let ch = f.ch;
        let n = ch.n as i64;
        let k = BINS as i64;
        for j in -k / 2..k / 2 {
            let x = ch.spec[(self.bin + j).rem_euclid(n) as usize];
            let h = ch.h[j.rem_euclid(n) as usize];
            self.buf[j.rem_euclid(k) as usize] = x * h;
        }
        ch.inv.process(&mut self.buf);
        // Each frame starts N/2 samples after the last, so its bins came out
        // turned by half a turn per bin: back by (-1)^(bin·frame).
        let flip = (self.bin.rem_euclid(2) as u64 * ch.frames) % 2 == 1;
        let s = if flip { -1.0 } else { 1.0 };
        out.extend(self.buf[BINS / 2..].iter().map(|z| z * s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone(rate: f64, hz: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = 2.0 * PI * hz * i as f64 / rate;
                Complex32::new(p.cos() as f32, p.sin() as f32)
            })
            .collect()
    }

    /// The level (dB on a unit tone) and the frequency of a complex lane.
    fn measure(x: &[Complex32], rate: f64) -> (f64, f64) {
        let p = x.iter().map(|z| z.norm_sqr() as f64).sum::<f64>() / x.len() as f64;
        // Frequency from the average turn between samples.
        let turn: Complex32 = x.windows(2).map(|w| w[1] * w[0].conj()).sum();
        (10.0 * p.log10(), (turn.im as f64).atan2(turn.re as f64) * rate / (2.0 * PI))
    }

    fn lane(rate: f64, offset: f64, signal: &[Complex32], blocks: usize) -> (Vec<Complex32>, f64) {
        let mut ch = Channelizer::new(rate);
        let (bin, resid) = ch.bin_of(offset);
        let mut tap = Tap::new(bin);
        let mut out = Vec::new();
        for c in signal.chunks(blocks) {
            ch.process(c, 0.0, |f| tap.take(f, &mut out));
        }
        (out, resid)
    }

    #[test]
    fn a_tone_in_the_lane_comes_out_whole_and_steady() {
        for rate in [48_000.0, 192_000.0, 6_144_000.0] {
            let ch = Channelizer::new(rate);
            assert!(ch.out_rate() >= MIN_OUT_RATE && ch.out_rate() < 2.0 * MIN_OUT_RATE);
            // A lane 1.2 MHz (or 10 kHz) off the centre, a tone 900 Hz above
            // the lane's middle; in odd-sized blocks, as the tap gives them.
            let offset = if rate > 1e6 { -1_234_567.0 } else { 10_123.0 };
            let n = (rate * 0.5) as usize;
            let (out, resid) = lane(rate, offset, &tone(rate, offset + 900.0, n), 7919);
            let tail = &out[out.len() / 4..];
            let (db, hz) = measure(tail, ch.out_rate());
            assert!(db.abs() < 0.5, "{rate}: {db:.2} dB");
            assert!((hz - (900.0 + resid)).abs() < 1.0, "{rate}: {hz:.1} Hz");
            // A steady tone has a steady level: no step at the frames' joins.
            let worst = tail.iter().map(|z| (z.norm() - 1.0).abs()).fold(0.0, f32::max);
            assert!(worst < 0.02, "{rate}: ripple {worst}");
        }
    }

    #[test]
    fn a_tone_outside_the_lane_is_not_heard() {
        let rate = 192_000.0;
        // Past half the output rate off the lane's middle: gone, rather than
        // folded back into the lane. (Between a quarter and a half it comes
        // through where it is, outside the lane, for the lane's own filter.)
        for off in [6_500.0, 8_000.0, 20_000.0, -7_000.0] {
            let (out, _) = lane(rate, 30_000.0, &tone(rate, 30_000.0 + off, 96_000), 1000);
            let (db, _) = measure(&out[out.len() / 4..], 12_000.0);
            assert!(db < -90.0, "{off}: {db:.1} dB");
        }
    }

    #[test]
    fn frames_are_stamped_with_the_time_they_stand_for() {
        let rate = 192_000.0;
        let mut ch = Channelizer::new(rate);
        let mut utcs = Vec::new();
        let block = 4000;
        for i in 0..20 {
            let t = 1000.0 + (i * block) as f64 / rate;
            ch.process(&vec![Complex32::new(0.0, 0.0); block], t, |f| utcs.push(f.utc));
        }
        // A frame's output samples are rate/D apart, BINS/2 of them.
        let step = (BINS / 2) as f64 / ch.out_rate();
        for w in utcs.windows(2) {
            assert!((w[1] - w[0] - step).abs() < 1e-9);
        }
        // The first is the start of the stream, less the filter's delay.
        assert!((utcs[0] - 1000.0).abs() < 0.01, "{}", utcs[0]);
    }
}
