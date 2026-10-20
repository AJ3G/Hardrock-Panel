// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/fir.rs, GPL-3.0-or-later. See NOTICE.md.

//! Streaming FIR filters at channel rate.

use std::f64::consts::TAU;

use crate::Complex32;
use crate::decim::lowpass_taps;

/// Complex band-pass taps with passband [lo_hz, hi_hz] relative to DC
/// (either or both edges may be negative — that selects the sideband).
pub fn bandpass_taps(ntaps: usize, lo_hz: f64, hi_hz: f64, sample_rate: f64) -> Vec<Complex32> {
    let bw = (hi_hz - lo_hz).abs().max(50.0);
    let center = (lo_hz + hi_hz) / 2.0;
    let lp = lowpass_taps(ntaps, (bw / 2.0) / sample_rate);
    let mid = (ntaps - 1) as f64 / 2.0;
    // Negative exponent: ComplexFir applies taps in correlation orientation
    // (no time reversal), which mirrors the shift.
    lp.iter()
        .enumerate()
        .map(|(i, &t)| {
            let ph = -TAU * center * (i as f64 - mid) / sample_rate;
            Complex32::new((ph.cos() * t as f64) as f32, (ph.sin() * t as f64) as f32)
        })
        .collect()
}

/// Independent running sums the real filter's window is walked with — see
/// [`crate::decim`], where the same reasoning and the same measurements apply.
/// A single accumulator makes the whole dot product one dependency chain: every
/// multiply-accumulate waits on the one before it.
const FIR_ACCUMULATORS: usize = 8;

/// Outputs [`complex_fir`] computes together: 256 samples, 2 KiB, small
/// enough to stay in L1 while every tap passes over it.
const FIR_TILE: usize = 256;

crate::simd::kernel! {
    /// A complete complex-tap FIR, `dst[o] = Σ_k taps[k]·buf[o+k]`.
    ///
    /// Computed tap by tap across a tile of outputs rather than output by
    /// output: each tap adds `taps[k]` times a contiguous run of the input to
    /// the whole tile. That inner loop has no reduction in it, so it
    /// vectorises at the full register width; the dot-product form it
    /// replaces is a floating-point reduction, which the compiler may not
    /// reorder and so ran two floats wide — see `complex_fir_cost`.
    fn complex_fir / complex_fir_portable / complex_fir_avx2 / complex_fir_avx512 (
        dst: &mut [Complex32],
        buf: &[Complex32],
        taps: &[Complex32],
    ) {
        for (t, tile) in dst.chunks_mut(FIR_TILE).enumerate() {
            let o0 = t * FIR_TILE;
            let len = tile.len();
            tile.fill(Complex32::default());
            for (k, &h) in taps.iter().enumerate() {
                for (a, &b) in tile.iter_mut().zip(&buf[o0 + k..o0 + k + len]) {
                    *a += b * h;
                }
            }
        }
    }
}

crate::simd::kernel! {
    /// [`complex_fir`]'s real-valued counterpart.
    fn real_fir / real_fir_portable / real_fir_avx2 / real_fir_avx512 (
        dst: &mut [f32],
        buf: &[f32],
        taps: &[f32],
    ) {
        let n = taps.len();
        for (o, d) in dst.iter_mut().enumerate() {
            let w = &buf[o..o + n];
            let mut acc = [0.0f32; FIR_ACCUMULATORS];
            let mut ws = w.chunks_exact(FIR_ACCUMULATORS);
            let mut ts = taps.chunks_exact(FIR_ACCUMULATORS);
            for (xs, tt) in (&mut ws).zip(&mut ts) {
                for i in 0..FIR_ACCUMULATORS {
                    acc[i] += xs[i] * tt[i];
                }
            }
            let mut sum = 0.0;
            for a in acc {
                sum += a;
            }
            for (x, &t) in ws.remainder().iter().zip(ts.remainder()) {
                sum += x * t;
            }
            *d = sum;
        }
    }
}

/// Streaming complex FIR (complex in, complex out).
pub struct ComplexFir {
    taps: Vec<Complex32>,
    buf: Vec<Complex32>,
}

impl ComplexFir {
    pub fn new(taps: Vec<Complex32>) -> Self {
        ComplexFir { taps, buf: Vec::new() }
    }

    pub fn set_taps(&mut self, taps: Vec<Complex32>) {
        self.taps = taps;
        // Keep history; length mismatch just causes one transient block.
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        self.buf.extend_from_slice(input);
        let n = self.taps.len();
        if self.buf.len() < n {
            return;
        }
        let count = self.buf.len() - n + 1;
        let start = out.len();
        out.resize(start + count, Complex32::default());
        complex_fir(&mut out[start..], &self.buf, &self.taps);
        self.buf.drain(..count);
    }

    pub fn reset(&mut self) {
        self.buf.clear();
    }
}

/// Streaming real FIR (f32 in/out), used for post-discriminator audio filtering.
pub struct RealFir {
    taps: Vec<f32>,
    buf: Vec<f32>,
}

impl RealFir {
    pub fn new(taps: Vec<f32>) -> Self {
        RealFir { taps, buf: Vec::new() }
    }

    pub fn lowpass(ntaps: usize, cutoff_hz: f64, sample_rate: f64) -> Self {
        RealFir { taps: lowpass_taps(ntaps, cutoff_hz / sample_rate), buf: Vec::new() }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let n = self.taps.len();
        if self.buf.len() < n {
            return;
        }
        let count = self.buf.len() - n + 1;
        let start = out.len();
        out.resize(start + count, 0.0);
        real_fir(&mut out[start..], &self.buf, &self.taps);
        self.buf.drain(..count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tiled form is the dot product it replaced, over ragged blocks.
    #[test]
    fn the_complex_fir_is_its_definition() {
        let taps = bandpass_taps(331, 300.0, 2700.0, 48_000.0);
        let x: Vec<Complex32> = (0..5_003)
            .map(|i| {
                Complex32::new(((i * 37) % 101) as f32 / 50.0 - 1.0, ((i * 11) % 7) as f32 / 7.0)
            })
            .collect();
        let mut fir = ComplexFir::new(taps.clone());
        let mut got = Vec::new();
        for c in x.chunks(613) {
            fir.process(c, &mut got);
        }
        assert_eq!(got.len(), x.len() - taps.len() + 1);
        for (o, g) in got.iter().enumerate() {
            let mut w = num_complex::Complex64::new(0.0, 0.0);
            for (k, h) in taps.iter().enumerate() {
                let p = x[o + k] * h;
                w += num_complex::Complex64::new(p.re as f64, p.im as f64);
            }
            assert!((g.re as f64 - w.re).abs() < 1e-4 && (g.im as f64 - w.im).abs() < 1e-4, "{o}");
        }
    }

    /// Cost per complex multiply-add, in release:
    /// `cargo test --release -p sdr -- --ignored --nocapture complex_fir_cost`.
    #[test]
    #[ignore]
    fn complex_fir_cost() {
        let x: Vec<Complex32> = (0..48_000).map(|i| Complex32::new(i as f32, 1.0)).collect();
        for n in [129usize, 331, 1001, 2001, 6001] {
            let mut fir = ComplexFir::new(vec![Complex32::new(0.001, 0.0005); n]);
            let mut out = Vec::new();
            let best = (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    out.clear();
                    fir.process(&x, &mut out);
                    t.elapsed().as_nanos() as f64 / x.len() as f64
                })
                .fold(f64::INFINITY, f64::min);
            println!("{n} taps: {best:.1} ns a sample, {:.3} ns a multiply-add", best / n as f64);
        }
    }
}
