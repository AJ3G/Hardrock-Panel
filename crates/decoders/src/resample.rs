//! Getting audio and I/Q to the rates the decoders want.
//!
//! The `sdr` crate's [`sdr::Receiver`] gives 48 kHz mono audio; the WSJT
//! modes want exactly 12 kHz, and the keyboard and picture modems take any
//! rate they are built for. The radio's I/Q is interleaved `f32` at one of the
//! ELAD rates (192 kHz to 6.144 MHz), and a channel decoder wants a narrow
//! complex lane at its own rate around one frequency in it.
//!
//! * [`Resampler`]: any rate to any rate, real or complex, with its own
//!   anti-alias filter.
//! * [`IqChannel`]: one narrow channel out of the wide I/Q, mixed to zero and
//!   brought down to an exact rate.
//! * [`deinterleave`]: interleaved `I, Q, I, Q …` to complex samples.

use std::f64::consts::PI;
use std::ops::{Add, Mul};

use sdr::Complex32;
use sdr::decim::HalfbandDecim;
use sdr::nco::Nco;

/// Interleaved `I0, Q0, I1, Q1, …` (an odd trailing value is ignored) as
/// complex samples, appended to `out`.
pub fn deinterleave(iq: &[f32], out: &mut Vec<Complex32>) {
    out.extend(iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])));
}

/// Phases in the polyphase table; the delay between two is interpolated.
const PHASES: usize = 128;
/// Taps per phase, at the lower of the two rates. 32 keeps aliasing more than
/// 70 dB down for these filters, at 32 multiply-adds an output sample.
const TAPS: usize = 32;

/// A sample type the resampler can filter: `f32` audio or complex I/Q.
pub trait Sample: Copy + Default + Add<Output = Self> + Mul<f32, Output = Self> + Send {}
impl Sample for f32 {}
impl Sample for Complex32 {}

/// Arbitrary-ratio resampling by a windowed-sinc polyphase filter. The
/// low-pass sits at 90 % of the lower rate's Nyquist, so going down it is the
/// anti-alias filter as well.
pub struct Resampler<T: Sample> {
    in_rate: f64,
    out_rate: f64,
    /// Input samples per output sample.
    step: f64,
    /// Taps in each phase (more than [`TAPS`] when going down, because the
    /// filter is as long in time at the lower rate).
    width: usize,
    /// `PHASES + 1` rows of `width` taps.
    table: Vec<f32>,
    hist: Vec<T>,
    /// Where the next output falls, in input samples from `hist[0]`.
    pos: f64,
}

impl<T: Sample> Resampler<T> {
    pub fn new(in_rate: f64, out_rate: f64) -> Self {
        assert!(in_rate > 0.0 && out_rate > 0.0);
        let step = in_rate / out_rate;
        let down = step.max(1.0);
        let width = ((TAPS as f64 * down).ceil() as usize).max(TAPS);
        let width = width + width % 2;
        // Cutoff in cycles per input sample.
        let fc = 0.45 / down;
        let mut table = vec![0.0f32; (PHASES + 1) * width];
        let half = width as f64 / 2.0;
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            let row = &mut table[p * width..(p + 1) * width];
            for (k, t) in row.iter_mut().enumerate() {
                // Tap k weighs the input `k - half + 1 - frac` samples from
                // the output instant.
                let x = k as f64 - half + 1.0 - frac;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (2.0 * PI * fc * x).sin() / (PI * x) };
                let sinc = if x.abs() < 1e-9 { 2.0 * fc } else { sinc };
                // Blackman window over the filter's span.
                let w = 0.42 + 0.5 * (PI * x / half).cos() + 0.08 * (2.0 * PI * x / half).cos();
                let w = if x.abs() >= half { 0.0 } else { w };
                *t = (sinc * w) as f32;
                sum += sinc * w;
            }
            for t in row.iter_mut() {
                *t = (*t as f64 / sum) as f32;
            }
        }
        Resampler {
            in_rate,
            out_rate,
            step,
            width,
            table,
            hist: vec![T::default(); width],
            pos: (width / 2) as f64 - 1.0,
        }
    }

    pub fn in_rate(&self) -> f64 {
        self.in_rate
    }

    pub fn out_rate(&self) -> f64 {
        self.out_rate
    }

    /// The filter's delay, in output samples.
    pub fn delay(&self) -> f64 {
        (self.width as f64 / 2.0) / self.step
    }

    /// Resample `input`, appending to `out`.
    pub fn process(&mut self, input: &[T], out: &mut Vec<T>) {
        self.hist.extend_from_slice(input);
        let half = self.width / 2;
        loop {
            let centre = self.pos.floor();
            let i = centre as usize;
            // Taps cover hist[i + 1 - half ..= i + half].
            if i + half >= self.hist.len() {
                break;
            }
            let frac = self.pos - centre;
            let fp = frac * PHASES as f64;
            let p = fp.floor() as usize;
            let a = (fp - p as f64) as f32;
            let r0 = &self.table[p * self.width..(p + 1) * self.width];
            let r1 = &self.table[(p + 1) * self.width..(p + 2) * self.width];
            let base = i + 1 - half;
            let mut acc = T::default();
            for k in 0..self.width {
                let t = r0[k] + (r1[k] - r0[k]) * a;
                acc = acc + self.hist[base + k] * t;
            }
            out.push(acc);
            self.pos += self.step;
        }
        // Keep what the next output still needs.
        let keep_from = (self.pos.floor() as usize + 1).saturating_sub(half);
        if keep_from > 0 {
            self.hist.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}

/// One narrow complex channel out of the radio's wide I/Q: the wanted
/// frequency mixed to zero, half-band stages down to near the target rate,
/// and a [`Resampler`] onto it exactly.
///
/// Frequencies are offsets from the I/Q centre in Hz, positive above.
pub struct IqChannel {
    in_rate: f64,
    out_rate: f64,
    nco: Nco,
    halfbands: Vec<HalfbandDecim>,
    resampler: Resampler<Complex32>,
    a: Vec<Complex32>,
    b: Vec<Complex32>,
}

impl IqChannel {
    /// `in_rate` is the I/Q rate, `out_rate` the lane rate the decoder wants,
    /// `offset_hz` the channel's offset from the I/Q centre.
    pub fn new(in_rate: f64, out_rate: f64, offset_hz: f64) -> Self {
        let mut rate = in_rate;
        let mut halfbands = Vec::new();
        // A half-band stage keeps its output's middle 80 % clean, so stop
        // halving while the next stage would cut into the lane.
        while rate / 2.0 >= out_rate * 2.0 {
            halfbands.push(HalfbandDecim::new());
            rate /= 2.0;
        }
        IqChannel {
            in_rate,
            out_rate,
            nco: Nco::new(-offset_hz, in_rate),
            halfbands,
            resampler: Resampler::new(rate, out_rate),
            a: Vec::new(),
            b: Vec::new(),
        }
    }

    pub fn out_rate(&self) -> f64 {
        self.out_rate
    }

    /// Retune to another offset from the I/Q centre.
    pub fn set_offset_hz(&mut self, offset_hz: f64) {
        self.nco.set_freq(-offset_hz, self.in_rate);
    }

    /// Channel `input` (complex, at the I/Q rate), appending lane-rate samples
    /// to `out`.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        self.a.clear();
        self.nco.mix(input, &mut self.a);
        for hb in &mut self.halfbands {
            self.b.clear();
            hb.process(&self.a, &mut self.b);
            std::mem::swap(&mut self.a, &mut self.b);
        }
        self.resampler.process(&self.a, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, hz: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * hz * i as f64 / rate).sin() as f32).collect()
    }

    /// Power of `x` at `hz` relative to a unit sine, in dB, by a DFT bin.
    fn level_db(x: &[f32], rate: f64, hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let p = 2.0 * PI * hz * i as f64 / rate;
            re += v as f64 * p.cos();
            im += v as f64 * p.sin();
        }
        let amp = 2.0 * (re * re + im * im).sqrt() / x.len() as f64;
        20.0 * amp.max(1e-12).log10()
    }

    #[test]
    fn audio_48k_to_12k_keeps_the_passband_and_rejects_what_would_alias() {
        let mut r = Resampler::<f32>::new(48_000.0, 12_000.0);
        let mut out = Vec::new();
        r.process(&tone(48_000.0, 1500.0, 48_000), &mut out);
        assert!((out.len() as i64 - 12_000).abs() < 40, "{}", out.len());
        let skip = 200;
        assert!(level_db(&out[skip..], 12_000.0, 1500.0).abs() < 0.2);

        // 10.5 kHz would land on 1.5 kHz after decimation.
        let mut r = Resampler::<f32>::new(48_000.0, 12_000.0);
        let mut out = Vec::new();
        r.process(&tone(48_000.0, 10_500.0, 48_000), &mut out);
        assert!(level_db(&out[skip..], 12_000.0, 1500.0) < -60.0);
    }

    #[test]
    fn an_odd_ratio_comes_out_at_the_right_rate_block_by_block() {
        let mut r = Resampler::<f32>::new(48_000.0, 11_025.0);
        let input = tone(48_000.0, 1000.0, 96_000);
        let mut out = Vec::new();
        for chunk in input.chunks(777) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 22_050).abs() < 40, "{}", out.len());
        assert!(level_db(&out[200..], 11_025.0, 1000.0).abs() < 0.2);
    }

    #[test]
    fn an_iq_channel_brings_one_signal_to_zero_at_the_lane_rate() {
        let rate = 768_000.0;
        let off = 123_456.0;
        let n = 768_000;
        let mut iq = Vec::with_capacity(n);
        for i in 0..n {
            let p = 2.0 * PI * (off + 300.0) * i as f64 / rate;
            // The wanted signal 300 Hz above the channel, and another 20 kHz away.
            let q = 2.0 * PI * (off + 20_000.0) * i as f64 / rate;
            iq.push(Complex32::new(
                (p.cos() + q.cos()) as f32 * 0.5,
                (p.sin() + q.sin()) as f32 * 0.5,
            ));
        }
        let mut ch = IqChannel::new(rate, 24_000.0, off);
        let mut out = Vec::new();
        for c in iq.chunks(8192) {
            ch.process(c, &mut out);
        }
        assert!((out.len() as i64 - 24_000).abs() < 60, "{}", out.len());
        let tail = &out[2000..];
        let bin = |hz: f64| {
            let mut acc = Complex32::new(0.0, 0.0);
            for (i, s) in tail.iter().enumerate() {
                let p = -2.0 * PI * hz * i as f64 / 24_000.0;
                acc += s * Complex32::new(p.cos() as f32, p.sin() as f32);
            }
            acc.norm() / tail.len() as f32
        };
        assert!((bin(300.0) - 0.5).abs() < 0.02, "{}", bin(300.0));
        // 20 kHz off is outside a 24 kS/s lane; whatever folds in is far down.
        assert!(bin(20_000.0 - 24_000.0) < 0.005);
    }
}
