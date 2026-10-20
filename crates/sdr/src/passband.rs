//! The receiver's passband filter: a complex band-pass designed to a
//! stopband depth, run directly or by FFT whichever is cheaper for its length.
//!
//! # Design
//!
//! [`FilterShape`] names how steep the skirts are. Each is a window and a
//! transition width; the length follows from them ([`crate::design`]). The
//! sharp shapes are Kaiser windows cut for [`STOP_DB`], which the tests
//! measure from the taps' own response — the point of a brick-wall filter is
//! that a signal 60 Hz outside the passband is gone, not merely quieter.
//!
//! # Running it
//!
//! Directly, a filter of `n` complex taps costs `n` complex multiply-adds a
//! sample; a 60 Hz skirt at 48 kHz is some 5000 taps. By FFT — uniformly
//! partitioned overlap-save — it costs two FFTs of `2P` points per block of
//! `P` samples plus one spectrum product per `P` taps, so a few dozen
//! multiply-adds a sample at any length, for `P` samples of extra delay.
//! [`Passband`] picks by length at [`FFT_ABOVE`], where the two were measured
//! to cross.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner};

use crate::Complex32;
use crate::design::{self, FirWindow};
use crate::fir::ComplexFir;

/// How far down the sharp shapes' stopbands are, dB. The specification is
/// 90; Kaiser is cut 5 dB deeper so rounding the taps to f32 cannot cost it.
pub const STOP_DB: f64 = 95.0;

/// Taps above which the filter runs by FFT. Measured by `passband_cost`, on
/// the shared 2.1 GHz Xeon these numbers come from: even at 129 taps the FFT
/// is 4.5 ns a sample against 31.5 directly, at 385 taps (the soft shape)
/// 5.6 against 98, at 6145 (a brick wall) 15 against 1155. So every passband
/// the receiver builds runs by FFT, and the direct form is left for the very
/// short filters where a block's delay would be most of the filter's.
pub const FFT_ABOVE: usize = 64;

/// How steep the passband's skirts are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum FilterShape {
    /// Blackman-Harris with a 1 kHz skirt: about the filter the receiver had
    /// before (331 taps; this is 384), and the shortest delay — 9 ms with the
    /// FFT block.
    Soft,
    /// Kaiser, 95 dB down 250 Hz outside the passband: 1225 taps, 24 ms.
    #[default]
    Sharp,
    /// Kaiser, 95 dB down 60 Hz outside the passband: a true brick wall, for
    /// a weak signal beside a strong one. 5067 taps, 64 ms.
    BrickWall,
}

impl FilterShape {
    pub const ALL: [FilterShape; 3] =
        [FilterShape::Soft, FilterShape::Sharp, FilterShape::BrickWall];

    pub fn label(self) -> &'static str {
        match self {
            FilterShape::Soft => "Soft",
            FilterShape::Sharp => "Sharp",
            FilterShape::BrickWall => "Brick wall",
        }
    }

    /// From the stopband edge to the passband edge, Hz.
    pub fn transition_hz(self) -> f64 {
        match self {
            FilterShape::Soft => 1_000.0,
            FilterShape::Sharp => 250.0,
            FilterShape::BrickWall => 60.0,
        }
    }

    pub fn window(self) -> FirWindow {
        match self {
            FilterShape::Soft => FirWindow::BlackmanHarris,
            FilterShape::Sharp | FilterShape::BrickWall => FirWindow::Kaiser { atten_db: STOP_DB },
        }
    }
}

/// Complex band-pass taps for `[lo_hz, hi_hz]` relative to the dial (either
/// edge may be negative), each skirt `shape.transition_hz()` wide.
pub fn passband_taps(lo_hz: f64, hi_hz: f64, rate: f64, shape: FilterShape) -> Vec<Complex32> {
    let (lo, hi) = (lo_hz.min(hi_hz), lo_hz.max(hi_hz).max(lo_hz.min(hi_hz) + 1.0));
    design::bandpass(lo / rate, hi / rate, shape.transition_hz() / rate, shape.window())
}

/// Uniformly partitioned overlap-save convolution.
///
/// The taps are cut into `K` partitions of `P`, each transformed once at
/// `2P` points. Every block of `P` input samples is transformed once and
/// kept in a frequency-domain delay line of the last `K` blocks; the output
/// block is the inverse transform of `Σ_k X_{i−k}·H_k`, of which the second
/// half is the linear convolution (the first half is the circular wrap,
/// discarded). Latency is one block.
pub struct FftFir {
    p: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    /// Spectra of the taps' partitions, `K × 2P`.
    parts: Vec<Vec<Complex32>>,
    /// Spectra of the last `K` input blocks; `head` is the newest.
    fdl: Vec<Vec<Complex32>>,
    head: usize,
    /// The previous input block (the first half of every transform).
    prev: Vec<Complex32>,
    /// Input waiting to fill a block.
    pending: Vec<Complex32>,
    work: Vec<Complex32>,
    acc: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl FftFir {
    /// Taps in correlation orientation (as [`ComplexFir`] takes them), run in
    /// blocks of `block` samples (a power of two).
    pub fn new(taps: &[Complex32], block: usize) -> Self {
        assert!(block.is_power_of_two());
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(2 * block);
        let inv = planner.plan_fft_inverse(2 * block);
        let scratch_len = fwd.get_inplace_scratch_len().max(inv.get_inplace_scratch_len());
        let mut f = FftFir {
            p: block,
            fwd,
            inv,
            parts: Vec::new(),
            fdl: Vec::new(),
            head: 0,
            prev: vec![Complex32::default(); block],
            pending: Vec::with_capacity(2 * block),
            work: vec![Complex32::default(); 2 * block],
            acc: vec![Complex32::default(); 2 * block],
            scratch: vec![Complex32::default(); scratch_len],
        };
        f.set_taps(taps);
        f
    }

    /// New taps. The delay line is kept, so the filter changes at the next
    /// block without a gap; a change of length that alters the partition
    /// count starts the line again (one block's transient).
    pub fn set_taps(&mut self, taps: &[Complex32]) {
        let p = self.p;
        let k = taps.len().div_ceil(p).max(1);
        // Correlation orientation to convolution: reversed. And the 1/2P the
        // inverse transform does not apply, folded in here once.
        let scale = 1.0 / (2 * p) as f32;
        let rev: Vec<Complex32> = taps.iter().rev().map(|&t| t * scale).collect();
        self.parts = (0..k)
            .map(|j| {
                let mut buf = vec![Complex32::default(); 2 * p];
                let chunk = &rev[(j * p).min(rev.len())..((j + 1) * p).min(rev.len())];
                buf[..chunk.len()].copy_from_slice(chunk);
                self.fwd.process_with_scratch(&mut buf, &mut self.scratch);
                buf
            })
            .collect();
        if self.fdl.len() != k {
            self.fdl = vec![vec![Complex32::default(); 2 * p]; k];
            self.head = 0;
        }
    }

    pub fn block(&self) -> usize {
        self.p
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        let p = self.p;
        let k = self.parts.len();
        let mut rest = input;
        loop {
            let need = p - self.pending.len();
            if rest.len() < need {
                self.pending.extend_from_slice(rest);
                return;
            }
            let (take, tail) = rest.split_at(need);
            self.pending.extend_from_slice(take);
            rest = tail;

            // [previous block | this block] → the newest spectrum.
            self.head = (self.head + 1) % k;
            let x = &mut self.fdl[self.head];
            x[..p].copy_from_slice(&self.prev);
            x[p..].copy_from_slice(&self.pending);
            self.fwd.process_with_scratch(x, &mut self.scratch);
            std::mem::swap(&mut self.prev, &mut self.pending);
            self.pending.clear();

            self.acc.fill(Complex32::default());
            for (j, h) in self.parts.iter().enumerate() {
                let x = &self.fdl[(self.head + k - j) % k];
                spectrum_mac(&mut self.acc, x, h);
            }
            self.work.copy_from_slice(&self.acc);
            self.inv.process_with_scratch(&mut self.work, &mut self.scratch);
            out.extend_from_slice(&self.work[p..]);
        }
    }
}

crate::simd::kernel! {
    /// `acc += x·h`, bin by bin.
    fn spectrum_mac / spectrum_mac_portable / spectrum_mac_avx2 / spectrum_mac_avx512 (
        acc: &mut [Complex32],
        x: &[Complex32],
        h: &[Complex32],
    ) {
        for ((a, &x), &h) in acc.iter_mut().zip(x).zip(h) {
            *a += x * h;
        }
    }
}

/// The FFT block for a filter of `n` taps: a quarter of its length, kept
/// between 256 and 512 samples (5.3 to 10.7 ms of delay at 48 kHz).
/// Measured by `passband_cost`: 256 is the cheapest block up to about a
/// thousand taps; past that 512 saves a third, and 1024 only a fifth more
/// (12.9 → 10.2 ns at 4097 taps) for another 10 ms of delay.
pub fn fft_block_for(n: usize) -> usize {
    (n / 4).next_power_of_two().clamp(256, 512)
}

enum Engine {
    Direct(ComplexFir),
    Fft(FftFir),
}

/// The receiver's band-pass: the same filter whichever way it runs.
pub struct Passband {
    engine: Engine,
    taps: usize,
}

impl Passband {
    pub fn new(taps: Vec<Complex32>) -> Self {
        let n = taps.len();
        Passband { engine: Self::engine_for(taps), taps: n }
    }

    fn engine_for(taps: Vec<Complex32>) -> Engine {
        if taps.len() > FFT_ABOVE {
            Engine::Fft(FftFir::new(&taps, fft_block_for(taps.len())))
        } else {
            Engine::Direct(ComplexFir::new(taps))
        }
    }

    /// New taps, keeping the engine and its history where the length allows.
    pub fn set_taps(&mut self, taps: Vec<Complex32>) {
        let n = taps.len();
        match &mut self.engine {
            Engine::Direct(f) if n <= FFT_ABOVE => f.set_taps(taps),
            Engine::Fft(f) if n > FFT_ABOVE && fft_block_for(n) == f.block() => f.set_taps(&taps),
            _ => self.engine = Self::engine_for(taps),
        }
        self.taps = n;
    }

    /// Taps in the filter.
    pub fn len(&self) -> usize {
        self.taps
    }

    pub fn is_empty(&self) -> bool {
        self.taps == 0
    }

    /// Whether it is running by FFT.
    pub fn by_fft(&self) -> bool {
        matches!(self.engine, Engine::Fft(_))
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        match &mut self.engine {
            Engine::Direct(f) => f.process(input, out),
            Engine::Fft(f) => f.process(input, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Worst gain outside `[lo − tr, hi + tr]` and ripple inside `[lo, hi]`,
    /// from the taps as they will run (f32), scanned at 1 Hz in the
    /// passband and at a tenth of a sidelobe's width in the stopband.
    fn measure(taps: &[Complex32], lo: f64, hi: f64, tr: f64, rate: f64) -> (f64, f64) {
        let (mut pmin, mut pmax) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut f = lo;
        while f <= hi {
            let g = design::complex_gain_db(taps, f / rate);
            pmin = pmin.min(g);
            pmax = pmax.max(g);
            f += 1.0;
        }
        let step = (rate / taps.len() as f64 / 10.0).min(5.0);
        let mut worst = f64::NEG_INFINITY;
        let mut f = -rate / 2.0;
        while f < rate / 2.0 {
            if f < lo - tr || f > hi + tr {
                worst = worst.max(design::complex_gain_db(taps, f / rate));
            }
            f += step;
        }
        (worst, pmax - pmin)
    }

    /// The specification: at least 90 dB down beyond the skirts and no more
    /// than 0.1 dB of ripple across the passband, for SSB and data, AM, and
    /// CW down to 50 Hz wide, in both sharp shapes.
    #[test]
    fn the_sharp_shapes_are_brick_walls() {
        let rate = 48_000.0;
        for shape in [FilterShape::Sharp, FilterShape::BrickWall] {
            for (lo, hi) in [(300.0, 3000.0), (-2700.0, -100.0), (575.0, 625.0), (-4500.0, 4500.0)]
            {
                let taps = passband_taps(lo, hi, rate, shape);
                let (stop, ripple) = measure(&taps, lo, hi, shape.transition_hz(), rate);
                println!(
                    "{shape:?} {lo}..{hi} Hz: {} taps, stopband {stop:.1} dB, ripple {ripple:.5} dB",
                    taps.len()
                );
                assert!(stop < -90.0, "{shape:?} {lo}..{hi}: stopband {stop:.1} dB");
                assert!(ripple < 0.1, "{shape:?} {lo}..{hi}: ripple {ripple:.4} dB");
            }
        }
    }

    /// The soft shape is what the receiver had: Blackman-Harris, 90 dB down
    /// a kilohertz out.
    #[test]
    fn the_soft_shape_is_the_old_filter() {
        let taps = passband_taps(100.0, 2700.0, 48_000.0, FilterShape::Soft);
        assert!(taps.len() < 400, "{}", taps.len());
        let (stop, ripple) = measure(&taps, 100.0, 2700.0, 1000.0, 48_000.0);
        assert!(stop < -90.0 && ripple < 0.1, "{stop} {ripple}");
    }

    /// By FFT is the same filter as directly, sample for sample once both
    /// have filled, however the blocks arrive.
    #[test]
    fn the_fft_engine_is_the_direct_one() {
        let taps = passband_taps(300.0, 3000.0, 48_000.0, FilterShape::Sharp);
        let x: Vec<Complex32> = (0..20_000)
            .map(|i| {
                Complex32::new(((i * 37) % 101) as f32 / 50.0 - 1.0, ((i * 11) % 7) as f32 / 7.0)
            })
            .collect();
        let mut direct = ComplexFir::new(taps.clone());
        let mut want = Vec::new();
        direct.process(&x, &mut want);
        for block in [64usize, 256] {
            let mut fft = FftFir::new(&taps, block);
            let mut got = Vec::new();
            for c in x.chunks(777) {
                fft.process(c, &mut got);
            }
            // The FFT form starts from silence, so its output is the direct
            // form's shifted by the taps' length less one.
            let lag = taps.len() - 1;
            assert!(got.len() > lag + 1000);
            for (o, w) in want.iter().enumerate().take(got.len() - lag) {
                let g = got[o + lag];
                assert!((g - w).norm() < 2e-4, "block {block} sample {o}: {g} vs {w}");
            }
        }
    }

    /// Where [`FFT_ABOVE`] and [`fft_block_for`] come from:
    /// `cargo test --release -p sdr -- --ignored --nocapture passband_cost`.
    #[test]
    #[ignore]
    fn passband_cost() {
        let x: Vec<Complex32> = (0..96_000).map(|i| Complex32::new(i as f32, 1.0)).collect();
        let time = |f: &mut dyn FnMut(&[Complex32], &mut Vec<Complex32>)| {
            let mut out = Vec::new();
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    out.clear();
                    f(&x, &mut out);
                    t.elapsed().as_nanos() as f64 / x.len() as f64
                })
                .fold(f64::INFINITY, f64::min)
        };
        for n in [129usize, 257, 385, 513, 1025, 1537, 2049, 4097, 6145] {
            let taps = vec![Complex32::new(0.001, 0.0005); n];
            let mut d = ComplexFir::new(taps.clone());
            let direct = time(&mut |x, o| d.process(x, o));
            let mut line = format!("{n:5} taps: direct {direct:7.1} ns");
            for p in [64usize, 128, 256, 512, 1024] {
                let mut f = FftFir::new(&taps, p);
                let ns = time(&mut |x, o| f.process(x, o));
                line += &format!(", P={p} {ns:6.1}");
            }
            println!("{line}  (ns a sample)");
        }
    }
}
