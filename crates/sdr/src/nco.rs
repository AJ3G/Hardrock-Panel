// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/nco.rs, GPL-3.0-or-later. See NOTICE.md.
// The integer phase accumulator, the exact re-anchoring and the f64 lane path
// are this crate's own.

//! Numerically controlled oscillator: the complex mixer that moves a chosen
//! frequency to 0 Hz.
//!
//! # How the phase is kept
//!
//! Two things are carried, and each covers the other's weakness:
//!
//! * A **64-bit integer phase accumulator**, one turn = 2⁶⁴. Advancing it is
//!   exact — integer addition wraps exactly where a turn does — so it never
//!   drifts, however long the receiver runs, and the frequency it advances by
//!   has a resolution of `fs / 2⁶⁴` (3·10⁻¹³ Hz at 6.144 MS/s).
//! * An **f64 phasor**, advanced by complex multiplication. That is cheap and
//!   pipelines well, but every multiply rounds, so on its own its phase walks
//!   and its magnitude creeps off the unit circle.
//!
//! Every [`ANCHOR_SAMPLES`] the phasor is thrown away and recomputed from the
//! accumulator with one f64 `sin_cos`. Its error is therefore whatever 65 536
//! f64 multiplies can accumulate — measured below at 2·10⁻¹² rad — and it
//! never grows past that. (SDRoxide renormalised the magnitude only, which
//! leaves the phase error growing without limit; measured below too.)
//!
//! # How a block is mixed
//!
//! The phasor advances once per [`LANES`] samples, by `step^LANES`, and the
//! samples inside the block are reached by multiplying it with the
//! precomputed `step^0 … step^{LANES-1}`. Those products are independent, so
//! the loop pipelines and widens; a per-sample recurrence would spend the
//! full latency of a multiply on every sample.
//!
//! [`NcoPrecision`] chooses the arithmetic of that inner product. In f64 the
//! only rounding the output sees is the final one to f32; in f32 the lane
//! table and the product are rounded too, and because the lane table repeats
//! every [`LANES`] samples its rounding error is periodic, which is a spur.
//! See `the_spurs_are_where_the_arithmetic_puts_them` for both measured.

use std::f64::consts::TAU;

use num_complex::Complex64;

use crate::Complex32;

/// How many samples one advance of the f64 phasor covers.
///
/// Eight because that is where the gain flattens: measured 3.5× over a
/// per-sample recurrence at 32.4 Msps in SDRoxide, and the tail handling
/// costs more as the block grows.
const LANES: usize = 8;

/// Samples between re-anchoring the phasor on the exact accumulator phase.
/// A whole number of [`LANES`] blocks, so an anchor always falls on a block
/// boundary.
const ANCHOR_SAMPLES: u32 = 1 << 16;

/// 2⁶⁴ as a float: one turn of the accumulator.
const TURN: f64 = 18_446_744_073_709_551_616.0;

/// The arithmetic the mixer's inner product is done in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NcoPrecision {
    /// Phasor, lane table and product in f64; the output rounded to f32 once.
    /// The default: its worst spur measured at −204 dBc against −163 dBc for
    /// [`Self::F32`], for 0.56 ns a sample more (1.49 against 0.93 ns on the
    /// AVX-512 copy) — 0.3 % of a core at 6.144 MS/s.
    #[default]
    F64,
    /// Lane table and product in f32, eight to a 256-bit register — SDRoxide's
    /// original arithmetic. The phase is still kept and anchored in f64.
    F32,
}

/// Numerically controlled oscillator: multiplies a signal by e^{j2πft}.
pub struct Nco {
    precision: NcoPrecision,
    /// The exact phase, one turn = 2⁶⁴.
    acc: u64,
    /// What the accumulator advances by per sample.
    fword: u64,
    phasor: Complex64,
    step: Complex64,
    /// `step^0 … step^{LANES-1}`: where in the block each sample sits.
    lanes64: [Complex64; LANES],
    /// The same table rounded to f32, for [`NcoPrecision::F32`].
    lanes32: [Complex32; LANES],
    /// `step^LANES`: what advances the phasor from one block to the next.
    block_step: Complex64,
    /// Samples since the last anchor.
    since_anchor: u32,
}

crate::simd::kernel! {
    /// The whole-block half of [`Nco::mix`] in f64.
    ///
    /// `dst` and `src` are both a whole number of [`LANES`]-sample blocks.
    fn mix_run64 / mix_run64_portable / mix_run64_avx2 / mix_run64_avx512 (
        dst: &mut [Complex32],
        src: &[Complex32],
        phasor: &mut Complex64,
        block_step: Complex64,
        lanes: &[Complex64; LANES],
    ) {
        for (ys, xs) in dst.chunks_exact_mut(LANES).zip(src.chunks_exact(LANES)) {
            let base = *phasor;
            for ((y, &x), &lane) in ys.iter_mut().zip(xs).zip(lanes) {
                let p = base * lane;
                let x = Complex64::new(x.re as f64, x.im as f64);
                let v = x * p;
                *y = Complex32::new(v.re as f32, v.im as f32);
            }
            *phasor *= block_step;
        }
    }
}

crate::simd::kernel! {
    /// [`mix_run64`] in place.
    fn mix_in_place64 / mix_in_place64_portable / mix_in_place64_avx2 / mix_in_place64_avx512 (
        samples: &mut [Complex32],
        phasor: &mut Complex64,
        block_step: Complex64,
        lanes: &[Complex64; LANES],
    ) {
        for xs in samples.chunks_exact_mut(LANES) {
            let base = *phasor;
            for (x, &lane) in xs.iter_mut().zip(lanes) {
                let p = base * lane;
                let v = Complex64::new(x.re as f64, x.im as f64) * p;
                *x = Complex32::new(v.re as f32, v.im as f32);
            }
            *phasor *= block_step;
        }
    }
}

crate::simd::kernel! {
    /// The whole-block half of [`Nco::mix`] in f32 ([`NcoPrecision::F32`]).
    fn mix_run32 / mix_run32_portable / mix_run32_avx2 / mix_run32_avx512 (
        dst: &mut [Complex32],
        src: &[Complex32],
        phasor: &mut Complex64,
        block_step: Complex64,
        lanes: &[Complex32; LANES],
    ) {
        for (ys, xs) in dst.chunks_exact_mut(LANES).zip(src.chunks_exact(LANES)) {
            let base = Complex32::new(phasor.re as f32, phasor.im as f32);
            for ((y, &x), &lane) in ys.iter_mut().zip(xs).zip(lanes) {
                *y = x * (base * lane);
            }
            *phasor *= block_step;
        }
    }
}

crate::simd::kernel! {
    /// [`mix_run32`] in place.
    fn mix_in_place32 / mix_in_place32_portable / mix_in_place32_avx2 / mix_in_place32_avx512 (
        samples: &mut [Complex32],
        phasor: &mut Complex64,
        block_step: Complex64,
        lanes: &[Complex32; LANES],
    ) {
        for xs in samples.chunks_exact_mut(LANES) {
            let base = Complex32::new(phasor.re as f32, phasor.im as f32);
            for (x, &lane) in xs.iter_mut().zip(lanes) {
                *x *= base * lane;
            }
            *phasor *= block_step;
        }
    }
}

/// The exact phasor for an accumulator value.
fn phasor_of(acc: u64) -> Complex64 {
    // Read as signed, so phases near zero keep every bit of precision.
    let (s, c) = (TAU * (acc as i64) as f64 / TURN).sin_cos();
    Complex64::new(c, s)
}

impl Nco {
    /// `freq_hz` may be negative. Positive shifts the signal up in frequency.
    pub fn new(freq_hz: f64, sample_rate: f64) -> Self {
        Self::with_precision(freq_hz, sample_rate, NcoPrecision::default())
    }

    /// [`Nco::new`] with the inner product's arithmetic chosen.
    pub fn with_precision(freq_hz: f64, sample_rate: f64, precision: NcoPrecision) -> Self {
        let one = Complex64::new(1.0, 0.0);
        let mut nco = Nco {
            precision,
            acc: 0,
            fword: 0,
            phasor: one,
            step: one,
            lanes64: [one; LANES],
            lanes32: [Complex32::new(1.0, 0.0); LANES],
            block_step: one,
            since_anchor: 0,
        };
        nco.set_freq(freq_hz, sample_rate);
        nco
    }

    /// Phase-continuous retune.
    pub fn set_freq(&mut self, freq_hz: f64, sample_rate: f64) {
        // The frequency word, as a fraction of a turn per sample. Wrapping
        // into [0, 1) first is what makes a negative frequency the two's
        // complement of the positive one.
        let turns = (freq_hz / sample_rate).rem_euclid(1.0);
        self.fword = (turns * TURN) as u64;
        // The step is taken from the word, not from `freq_hz`, so the phasor
        // and the accumulator advance by exactly the same angle and an anchor
        // never finds them apart by more than rounding.
        let w = TAU * (self.fword as i64) as f64 / TURN;
        self.step = Complex64::new(w.cos(), w.sin());
        let mut p = Complex64::new(1.0, 0.0);
        for (l64, l32) in self.lanes64.iter_mut().zip(self.lanes32.iter_mut()) {
            *l64 = p;
            *l32 = Complex32::new(p.re as f32, p.im as f32);
            p *= self.step;
        }
        // `step^LANES` exactly, rather than the product walked above.
        let wb = TAU * (self.fword.wrapping_mul(LANES as u64) as i64) as f64 / TURN;
        self.block_step = Complex64::new(wb.cos(), wb.sin());
        // `phasor` and `acc` are untouched: that is the phase continuity.
    }

    /// The arithmetic in use.
    pub fn precision(&self) -> NcoPrecision {
        self.precision
    }

    /// samples[i] *= phasor, in place.
    pub fn mix_in_place(&mut self, samples: &mut [Complex32]) {
        let mut rest = samples;
        while !rest.is_empty() {
            let (head, tail) = rest.split_at_mut(self.run_len(rest.len()));
            let whole = head.len() - head.len() % LANES;
            match self.precision {
                NcoPrecision::F64 => mix_in_place64(
                    &mut head[..whole],
                    &mut self.phasor,
                    self.block_step,
                    &self.lanes64,
                ),
                NcoPrecision::F32 => mix_in_place32(
                    &mut head[..whole],
                    &mut self.phasor,
                    self.block_step,
                    &self.lanes32,
                ),
            }
            for x in &mut head[whole..] {
                *x = self.one(*x);
            }
            self.advanced(head.len());
            rest = tail;
        }
    }

    /// out[i] = input[i] * phasor (appends to `out`).
    pub fn mix(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        let start = out.len();
        out.resize(start + input.len(), Complex32::new(0.0, 0.0));
        let mut dst = &mut out[start..];
        let mut src = input;
        while !src.is_empty() {
            let n = self.run_len(src.len());
            let (d, dt) = dst.split_at_mut(n);
            let (s, st) = src.split_at(n);
            let whole = n - n % LANES;
            match self.precision {
                NcoPrecision::F64 => mix_run64(
                    &mut d[..whole],
                    &s[..whole],
                    &mut self.phasor,
                    self.block_step,
                    &self.lanes64,
                ),
                NcoPrecision::F32 => mix_run32(
                    &mut d[..whole],
                    &s[..whole],
                    &mut self.phasor,
                    self.block_step,
                    &self.lanes32,
                ),
            }
            for (y, &x) in d[whole..].iter_mut().zip(&s[whole..]) {
                *y = self.one(x);
            }
            self.advanced(n);
            dst = dt;
            src = st;
        }
    }

    /// How many of `avail` samples to mix before the next anchor is due —
    /// everything, unless an anchor falls inside.
    #[inline]
    fn run_len(&self, avail: usize) -> usize {
        avail.min((ANCHOR_SAMPLES - self.since_anchor) as usize)
    }

    /// One sample by the per-sample recurrence: the tail of a run that did not
    /// fill a block.
    #[inline]
    fn one(&mut self, x: Complex32) -> Complex32 {
        let v = Complex64::new(x.re as f64, x.im as f64) * self.phasor;
        self.phasor *= self.step;
        Complex32::new(v.re as f32, v.im as f32)
    }

    /// Account for `n` samples just mixed, and anchor if one is due.
    ///
    /// A run that ended mid-block left the phasor advanced sample by sample
    /// for its tail, and the next run's whole blocks start from there — the
    /// block structure is relative to the run, not to sample zero, which is
    /// why the anchor is the accumulator's and not a block count's.
    #[inline]
    fn advanced(&mut self, n: usize) {
        self.acc = self.acc.wrapping_add(self.fword.wrapping_mul(n as u64));
        self.since_anchor += n as u32;
        if self.since_anchor >= ANCHOR_SAMPLES {
            self.since_anchor = 0;
            self.phasor = phasor_of(self.acc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact phase, in turns, after `n` samples of word `fword`.
    fn exact(fword: u64, n: u64) -> Complex64 {
        phasor_of(fword.wrapping_mul(n))
    }

    /// The block-at-a-time mixer is the exact phasor sample by sample, to the
    /// precision the output is carried at — over buffers whose lengths do not
    /// divide by the lane count or the anchor interval, so every call ends on
    /// a tail and the next picks the phase up mid-block.
    #[test]
    fn every_sample_has_the_exact_phase() {
        for precision in [NcoPrecision::F64, NcoPrecision::F32] {
            let fs = 32_400_000.0;
            let mut nco = Nco::with_precision(-1_234_567.0, fs, precision);
            let input: Vec<Complex32> = (0..300_003)
                .map(|i| Complex32::new((i % 17) as f32 * 0.05, (i % 23) as f32 * 0.03))
                .collect();
            let mut got = Vec::new();
            for chunk in input.chunks(4_093) {
                nco.mix(chunk, &mut got);
            }
            let tol = if precision == NcoPrecision::F64 { 2e-7 } else { 2e-6 };
            for (i, x) in input.iter().enumerate() {
                let p = exact(nco.fword, i as u64);
                let want = Complex64::new(x.re as f64, x.im as f64) * p;
                let d = Complex64::new(got[i].re as f64, got[i].im as f64) - want;
                assert!(d.norm() < tol, "{precision:?} sample {i}: {:?} vs {want:?}", got[i]);
            }
        }
    }

    /// A tone comes out where it was asked for, and on the unit circle.
    #[test]
    fn the_shift_is_the_frequency_it_was_given() {
        let fs = 2_000_000.0;
        let shift = 137_000.0;
        let mut nco = Nco::new(shift, fs);
        let mut buf: Vec<Complex32> = (0..300_000).map(|_| Complex32::new(1.0, 0.0)).collect();
        nco.mix_in_place(&mut buf);
        let mut adv = 0.0f64;
        let tail = &buf[200_000..];
        for w in tail.windows(2) {
            adv += (w[1] * w[0].conj()).arg() as f64;
        }
        let hz = adv / (tail.len() - 1) as f64 / TAU * fs;
        assert!((hz - shift).abs() < 1.0, "read {hz:.2} Hz, asked {shift}");
        let mag = tail.iter().map(|c| c.norm()).fold(0.0f32, f32::max);
        assert!((mag - 1.0).abs() < 1e-6, "amplitude drifted to {mag}");
    }

    /// `mix` and `mix_in_place` are the same mixer.
    #[test]
    fn the_two_entry_points_agree() {
        let fs = 1_536_000.0;
        let input: Vec<Complex32> = (0..100_005)
            .map(|i| Complex32::new((i % 13) as f32 * 0.07, (i % 29) as f32 * 0.02))
            .collect();
        let mut a = Nco::new(48_000.0, fs);
        let mut out = Vec::new();
        for c in input.chunks(777) {
            a.mix(c, &mut out);
        }
        let mut b = Nco::new(48_000.0, fs);
        let mut in_place = input.clone();
        for c in in_place.chunks_mut(1001) {
            b.mix_in_place(c);
        }
        // Different chunkings put the per-sample tails in different places,
        // so the two agree to rounding rather than bit for bit.
        for (x, y) in out.iter().zip(&in_place) {
            assert!((x - y).norm() < 1e-5 * x.norm().max(1.0));
        }
    }

    /// A retune keeps the phase: the sample after it continues from the
    /// sample before, at the new rate.
    #[test]
    fn a_retune_is_phase_continuous() {
        let fs = 192_000.0;
        let mut nco = Nco::new(1_000.0, fs);
        let mut out = Vec::new();
        nco.mix(&[Complex32::new(1.0, 0.0); 1001], &mut out);
        nco.set_freq(-3_000.0, fs);
        nco.mix(&[Complex32::new(1.0, 0.0); 2], &mut out);
        let before = (out[1000] * out[999].conj()).arg() as f64;
        let across = (out[1001] * out[1000].conj()).arg() as f64;
        let after = (out[1002] * out[1001].conj()).arg() as f64;
        assert!((before - TAU * 1_000.0 / fs).abs() < 1e-6);
        // The first step after the retune is the old frequency's (that sample
        // was already on its way), the next the new one's.
        assert!((across - TAU * 1_000.0 / fs).abs() < 1e-6, "{across}");
        assert!((after + TAU * 3_000.0 / fs).abs() < 1e-6, "{after}");
    }

    /// Phase error against the exact accumulator over 10⁷ samples: the
    /// anchored phasor stays where it starts, and the magnitude-only
    /// renormalisation it replaced does not.
    ///
    /// Printed for the record with `--nocapture`.
    #[test]
    fn the_phase_does_not_drift() {
        let fs = 6_144_000.0;
        let f = 1_234_567.891;
        let n = 10_000_000u64;
        let mut nco = Nco::new(f, fs);
        let fword = nco.fword;
        let mut worst_anchored = 0.0f64;
        // A prime block length, so the checks fall all over the anchor
        // interval rather than on the anchors themselves.
        let mut buf = vec![Complex32::new(0.0, 0.0); 65_521];
        let mut done = 0u64;
        while done < n {
            nco.mix_in_place(&mut buf);
            done += buf.len() as u64;
            // The internal phasor against the exact phase, at each block end.
            let e = (nco.phasor * exact(fword, done).conj()).arg().abs();
            worst_anchored = worst_anchored.max(e);
        }

        // SDRoxide's recurrence: the same step, renormalised in magnitude
        // every 65 536 samples, never re-anchored.
        let mut p = Complex64::new(1.0, 0.0);
        let mut worst_free = 0.0f64;
        for i in 1..=n {
            p *= nco.step;
            if i % 65_536 == 0 {
                p /= p.norm();
                worst_free = worst_free.max((p * exact(fword, i).conj()).arg().abs());
            }
        }
        println!(
            "phase error over {n} samples: anchored {worst_anchored:.2e} rad, \
             renormalised only {worst_free:.2e} rad"
        );
        assert!(worst_anchored < 1e-11, "{worst_anchored:e}");
    }

    /// Spur-free dynamic range of the mixer, from a Welch spectrum of over
    /// 10⁷ mixed samples (ten 2²⁰-point FFTs, Kaiser β = 24, whose sidelobes
    /// are some 220 dB down so they do not mask what is being looked for).
    /// Returns the worst spur in dBc, outside the tone's main lobe.
    fn sfdr(precision: NcoPrecision, input_hz: Option<f64>) -> f64 {
        use rustfft::FftPlanner;
        let fs = 6_144_000.0;
        let nfft = 1 << 20;
        let segs = 10;
        let mut nco = Nco::with_precision(-1_234_567.891, fs, precision);
        let window = crate::design::kaiser(nfft, 24.0);
        let fft = FftPlanner::<f64>::new().plan_fft_forward(nfft);
        let mut psd = vec![0.0f64; nfft];
        let mut x = vec![Complex32::new(0.0, 0.0); nfft];
        let mut t = 0u64;
        for _ in 0..segs {
            for s in x.iter_mut() {
                *s = match input_hz {
                    // An exact tone, rounded to f32 once: the input a perfect
                    // mixer would have to reproduce.
                    Some(hz) => {
                        let ph = TAU * ((hz / fs * t as f64) % 1.0);
                        Complex32::new(ph.cos() as f32, ph.sin() as f32)
                    }
                    None => Complex32::new(1.0, 0.0),
                };
                t += 1;
            }
            nco.mix_in_place(&mut x);
            let mut y: Vec<Complex64> = x
                .iter()
                .zip(&window)
                .map(|(s, &w)| Complex64::new(s.re as f64 * w, s.im as f64 * w))
                .collect();
            fft.process(&mut y);
            for (p, v) in psd.iter_mut().zip(&y) {
                *p += v.norm_sqr();
            }
        }
        let (peak, &top) =
            psd.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).expect("a spectrum");
        let guard = 24;
        let spur = psd
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                let d = (*k as i64 - peak as i64).rem_euclid(nfft as i64);
                d > guard && d < nfft as i64 - guard
            })
            .map(|(_, &p)| p)
            .fold(0.0f64, f64::max);
        10.0 * (spur / top).log10()
    }

    /// The measurement behind [`NcoPrecision`]'s default.
    ///
    /// The f32 lane table repeats every eight samples, so its rounding error
    /// is periodic and lands as spurs at the tone ± multiples of fs/8; in f64
    /// the only rounding is the output's own, which is not periodic at an
    /// irrational-looking frequency and spreads as noise.
    #[test]
    fn the_spurs_are_where_the_arithmetic_puts_them() {
        let f64_dc = sfdr(NcoPrecision::F64, None);
        let f32_dc = sfdr(NcoPrecision::F32, None);
        let f64_tone = sfdr(NcoPrecision::F64, Some(250_013.7));
        let f32_tone = sfdr(NcoPrecision::F32, Some(250_013.7));
        println!(
            "worst spur, dBc: f64 {f64_dc:.1} (DC in) {f64_tone:.1} (tone in); \
             f32 {f32_dc:.1} (DC in) {f32_tone:.1} (tone in)"
        );
        assert!(f64_dc < -150.0 && f64_tone < -150.0, "{f64_dc} {f64_tone}");
        assert!(f64_dc < f32_dc && f64_tone < f32_tone);
    }

    /// Mixer cost, ns per sample, both arithmetics:
    /// `cargo test --release -p sdr -- --ignored --nocapture nco_cost`.
    #[test]
    #[ignore]
    fn nco_cost() {
        let n = 1 << 16;
        let input: Vec<Complex32> = (0..n).map(|i| Complex32::new(i as f32, 1.0)).collect();
        for precision in [NcoPrecision::F64, NcoPrecision::F32] {
            let mut nco = Nco::with_precision(123_456.7, 6_144_000.0, precision);
            let mut out = Vec::with_capacity(n);
            let reps = 2000;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                out.clear();
                nco.mix(&input, &mut out);
            }
            let ns = t.elapsed().as_nanos() as f64 / (reps * n) as f64;
            println!("{precision:?}: {ns:.3} ns/sample");
        }
    }
}
