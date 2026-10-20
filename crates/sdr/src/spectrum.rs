// Adapted from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/spectrum.rs, GPL-3.0-or-later: the windowed rustfft
// transform normalised by the window's coherent gain, the DC-centred bin order
// and the max-pooling into display columns follow SDRoxide's SpectrumAnalyzer.
// The frame-rate limiter, averaging steps, peak hold and calibration are this
// crate's. See NOTICE.md.

//! The panadapter's spectrum: calibrated dB frames from interleaved I/Q.
//!
//! # Levels
//!
//! 0 dBFS is a complex sample of magnitude 1, and the transform is normalised by
//! the window's coherent gain, so a carrier of magnitude `A` reads
//! `20·log10(A)` dBFS in its bin whatever the FFT size or window — the same
//! scale [`crate::Receiver::level_dbfs`] reads, which is what lets one
//! calibration offset turn both into dBm. The noise floor, as on any FFT
//! display, falls 3 dB with each doubling of the FFT size.
//!
//! # Cost
//!
//! One transform per frame, not one per `N` samples: the limiter keeps the last
//! `N` samples and transforms them when a frame is due, so at 6144 kHz and 30
//! frames a second only thirty transforms run however small `N` is. Averaging
//! is therefore across frames.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner};

use crate::types::Complex32;
use crate::window::{blackman_harris, enbw_bins, flat_top, hann, rectangular};

/// The smallest FFT a [`Spectrum`] accepts.
pub const MIN_FFT_SIZE: usize = 2048;
/// The largest: 2^20, about a million points, 5.9 Hz bins at 6144 kHz and
/// 0.18 Hz at 192 kHz.
pub const MAX_FFT_SIZE: usize = 1 << 20;
/// The fastest frame rate.
pub const MAX_FPS: f32 = 60.0;

/// The analysis window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    /// Narrow main lobe, -31 dB sidelobes: the sharper trace.
    Hann,
    /// Wide main lobe, -92 dB sidelobes: weak signals next to strong ones.
    #[default]
    BlackmanHarris,
    /// No window: the narrowest main lobe, -13 dB sidelobes.
    Rectangular,
    /// Flat-top: wide, but a carrier reads its true level between bins.
    FlatTop,
}

impl Window {
    pub const ALL: [Window; 4] =
        [Window::Hann, Window::BlackmanHarris, Window::Rectangular, Window::FlatTop];

    pub fn label(self) -> &'static str {
        match self {
            Window::Hann => "Hann",
            Window::BlackmanHarris => "Blackman-Harris",
            Window::Rectangular => "Rectangular",
            Window::FlatTop => "Flat-top",
        }
    }

    pub fn coefficients(self, n: usize) -> Vec<f32> {
        match self {
            Window::Hann => hann(n),
            Window::BlackmanHarris => blackman_harris(n),
            Window::Rectangular => rectangular(n),
            Window::FlatTop => flat_top(n),
        }
    }
}

/// Which samples a frame is made from.
///
/// [`Overlap::Latest`] transforms the newest `N` samples once per frame, which
/// at a wide span and a small FFT leaves most samples unseen. The others are
/// Welch's method: a transform every `N/2` or `N/4` samples, all of them
/// averaged into the next frame, so every sample counts, the noise is
/// smoother and a short burst between frames still shows. When frames come
/// faster than segments (a big FFT at a narrow span) a frame is the newest
/// `N` samples as with `Latest`, so the display never stalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlap {
    #[default]
    Latest,
    Half,
    ThreeQuarters,
}

impl Overlap {
    pub const ALL: [Overlap; 3] = [Overlap::Latest, Overlap::Half, Overlap::ThreeQuarters];

    pub fn label(self) -> &'static str {
        match self {
            Overlap::Latest => "Off",
            Overlap::Half => "50%",
            Overlap::ThreeQuarters => "75%",
        }
    }

    /// Samples between segments for an `n`-point FFT, `None` for `Latest`.
    pub fn step(self, n: usize) -> Option<usize> {
        match self {
            Overlap::Latest => None,
            Overlap::Half => Some((n / 2).max(1)),
            Overlap::ThreeQuarters => Some((n / 4).max(1)),
        }
    }
}

/// Exponential averaging of the displayed power, across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Averaging {
    #[default]
    Off,
    X2,
    X4,
    X8,
}

impl Averaging {
    pub const ALL: [Averaging; 4] = [Averaging::Off, Averaging::X2, Averaging::X4, Averaging::X8];

    /// The equivalent number of frames averaged.
    pub fn frames(self) -> u32 {
        match self {
            Averaging::Off => 1,
            Averaging::X2 => 2,
            Averaging::X4 => 4,
            Averaging::X8 => 8,
        }
    }
}

/// How a [`Spectrum`] is set up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumConfig {
    /// A power of two in [`MIN_FFT_SIZE`]`..=`[`MAX_FFT_SIZE`]; anything else
    /// is rounded to the nearest one inside it.
    pub fft_size: usize,
    pub window: Window,
    pub overlap: Overlap,
    pub averaging: Averaging,
    /// Keep a peak-hold trace in each frame.
    pub peak_hold: bool,
    /// How fast the held peaks fall back, in dB per second. 0 holds for ever.
    pub peak_decay_db_per_s: f32,
    /// Frames per second, up to [`MAX_FPS`].
    pub fps: f32,
    /// Added to every dBFS value to make it dBm at the antenna.
    pub calibration_db: f32,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        SpectrumConfig {
            fft_size: 8192,
            window: Window::BlackmanHarris,
            overlap: Overlap::Latest,
            averaging: Averaging::Off,
            peak_hold: false,
            peak_decay_db_per_s: 10.0,
            fps: 30.0,
            calibration_db: 0.0,
        }
    }
}

/// One spectrum, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumFrame {
    /// The radio's centre (DDC) frequency these bins were taken at, in Hz.
    pub center_hz: f64,
    /// The sample rate, and so the width of the frame, in Hz.
    pub span_hz: f64,
    /// `fft_size` values in dBm (dBFS plus the calibration), ascending in
    /// frequency: bin `k` is at `center_hz - span_hz/2 + k·span_hz/N`.
    pub db: Vec<f32>,
    /// The peak-hold trace, same layout, when peak hold is on.
    pub peak: Option<Vec<f32>>,
    /// Counts frames from this spectrum since it was built.
    pub seq: u64,
    /// The window's equivalent noise bandwidth, in bins: one bin's noise is
    /// the noise density times `enbw · span / N`.
    pub enbw: f32,
    /// The noise floor, dBm per bin: the median bin, which sits within about
    /// 1.6 dB under the mean noise in a band that is mostly noise and is not
    /// pulled up by the signals in it.
    pub noise_dbm: f32,
}

impl SpectrumFrame {
    /// The frequency of bin `k`, in Hz.
    pub fn bin_hz(&self, k: usize) -> f64 {
        self.center_hz - self.span_hz / 2.0 + k as f64 * self.span_hz / self.db.len() as f64
    }

    /// The bin nearest `hz`, if it is inside the frame.
    pub fn bin_of(&self, hz: f64) -> Option<usize> {
        let n = self.db.len() as f64;
        let k = ((hz - (self.center_hz - self.span_hz / 2.0)) / self.span_hz * n).round();
        (k >= 0.0 && k < n).then_some(k as usize)
    }

    /// The lowest frequency in the frame.
    pub fn start_hz(&self) -> f64 {
        self.center_hz - self.span_hz / 2.0
    }

    /// The width of one bin, Hz.
    pub fn bin_width(&self) -> f64 {
        self.span_hz / self.db.len().max(1) as f64
    }

    /// The strongest bin within `[lo, hi]` Hz, and the peak's frequency and
    /// level interpolated between bins (see [`interpolate_peak`]).
    pub fn peak_in(&self, lo: f64, hi: f64) -> Option<Peak> {
        let n = self.db.len();
        if n == 0 || hi < lo {
            return None;
        }
        let per = n as f64 / self.span_hz;
        let a = ((lo - self.start_hz()) * per).floor().max(0.0) as usize;
        let b = (((hi - self.start_hz()) * per).ceil() as usize).min(n - 1);
        if a > b || a >= n {
            return None;
        }
        let k = (a..=b).max_by(|&i, &j| self.db[i].total_cmp(&self.db[j]))?;
        Some(interpolate_peak(&self.db, k, self.start_hz(), self.bin_width()))
    }
}

/// A spectral peak found between bins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    /// The bin with the most power.
    pub bin: usize,
    /// The peak's frequency, Hz, interpolated.
    pub hz: f64,
    /// Its level, dBm, interpolated.
    pub dbm: f32,
}

/// Interpolate the peak at bin `k` of `db` (bin `i` at `start + i·width`).
///
/// A parabola through the bin and its two neighbours, on the dB values: the
/// Gaussian interpolator, exact for a Gaussian main lobe and within a few
/// hundredths of a bin for Hann and Blackman-Harris, so with a large FFT a
/// carrier's frequency reads to well under a hertz. At an edge, or on a
/// plateau, the bin itself.
pub fn interpolate_peak(db: &[f32], k: usize, start: f64, width: f64) -> Peak {
    let at = |d: f64, v: f32| Peak { bin: k, hz: start + (k as f64 + d) * width, dbm: v };
    if k == 0 || k + 1 >= db.len() {
        return at(0.0, db[k]);
    }
    let (a, b, c) = (db[k - 1] as f64, db[k] as f64, db[k + 1] as f64);
    let den = a - 2.0 * b + c;
    if !den.is_finite() || den >= -1e-9 {
        return at(0.0, db[k]);
    }
    let d = (0.5 * (a - c) / den).clamp(-0.5, 0.5);
    let v = b - 0.25 * (a - c) * d;
    at(d, v as f32)
}

/// Clamp an FFT size to a supported power of two.
pub fn valid_fft_size(n: usize) -> usize {
    let n = n.clamp(MIN_FFT_SIZE, MAX_FFT_SIZE);
    if n.is_power_of_two() {
        return n;
    }
    let up = n.next_power_of_two();
    let down = up / 2;
    if n - down <= up - n { down.max(MIN_FFT_SIZE) } else { up.min(MAX_FFT_SIZE) }
}

/// The spectrum analyser.
pub struct Spectrum {
    cfg: SpectrumConfig,
    rate: f64,
    center_hz: f64,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// `1 / (Σw)²`: what normalises a carrier to its own magnitude.
    norm: f32,
    enbw: f32,
    /// Welch: summed segment powers since the last frame, how many, and the
    /// samples until the next segment.
    acc: Vec<f32>,
    acc_n: u32,
    seg_countdown: f64,
    /// One transform's power, in display order.
    power: Vec<f32>,
    /// Interleaved copy of complex input, for [`Self::process_complex`].
    interleaved: Vec<f32>,
    noise_scratch: Vec<f32>,
    /// The last `N` samples, as a ring.
    ring: Vec<Complex32>,
    ring_at: usize,
    /// Samples in the ring that are real, up to `N`.
    filled: usize,
    /// Samples until the next frame is due.
    countdown: f64,
    work: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Averaged linear power, in display order.
    avg: Vec<f32>,
    primed: bool,
    /// Held peaks in dBFS, in display order.
    peak: Vec<f32>,
    seq: u64,
}

impl Spectrum {
    pub fn new(cfg: SpectrumConfig, sample_rate: f64) -> Self {
        let mut s = Spectrum {
            cfg,
            rate: sample_rate.max(1.0),
            center_hz: 0.0,
            fft: FftPlanner::new().plan_fft_forward(MIN_FFT_SIZE),
            window: Vec::new(),
            norm: 1.0,
            enbw: 1.0,
            acc: Vec::new(),
            acc_n: 0,
            seg_countdown: 0.0,
            power: Vec::new(),
            interleaved: Vec::new(),
            noise_scratch: Vec::new(),
            ring: Vec::new(),
            ring_at: 0,
            filled: 0,
            countdown: 0.0,
            work: Vec::new(),
            scratch: Vec::new(),
            avg: Vec::new(),
            primed: false,
            peak: Vec::new(),
            seq: 0,
        };
        s.rebuild();
        s
    }

    pub fn config(&self) -> SpectrumConfig {
        self.cfg
    }

    pub fn fft_size(&self) -> usize {
        self.ring.len()
    }

    pub fn sample_rate(&self) -> f64 {
        self.rate
    }

    /// Change any of the settings. A new size or window starts the averages
    /// over; averaging, peak decay, rate and calibration change in place.
    pub fn set_config(&mut self, cfg: SpectrumConfig) {
        let rebuild =
            valid_fft_size(cfg.fft_size) != self.ring.len() || cfg.window != self.cfg.window;
        if cfg.overlap != self.cfg.overlap {
            self.acc_n = 0;
            self.seg_countdown = 0.0;
        }
        if !cfg.peak_hold {
            self.peak.clear();
        }
        self.cfg = cfg;
        if rebuild {
            self.rebuild();
        }
    }

    pub fn set_sample_rate(&mut self, rate: f64) {
        if rate != self.rate {
            self.rate = rate.max(1.0);
            self.reset();
        }
    }

    /// Forget the averages, the held peaks and the samples in hand.
    pub fn reset(&mut self) {
        self.filled = 0;
        self.ring_at = 0;
        self.countdown = 0.0;
        self.seg_countdown = 0.0;
        self.acc_n = 0;
        self.primed = false;
        self.peak.clear();
    }

    fn rebuild(&mut self) {
        let n = valid_fft_size(self.cfg.fft_size);
        self.cfg.fft_size = n;
        self.fft = FftPlanner::new().plan_fft_forward(n);
        self.window = self.cfg.window.coefficients(n);
        let gain: f64 = self.window.iter().map(|&w| w as f64).sum();
        self.norm = (1.0 / (gain * gain)) as f32;
        self.enbw = enbw_bins(&self.window);
        self.acc = vec![0.0; n];
        self.power = vec![0.0; n];
        self.ring = vec![Complex32::default(); n];
        self.work = vec![Complex32::default(); n];
        self.scratch = vec![Complex32::default(); self.fft.get_inplace_scratch_len()];
        self.avg = vec![0.0; n];
        self.reset();
    }

    /// Samples between frames at the configured rate.
    fn hop(&self) -> f64 {
        self.rate / self.cfg.fps.clamp(0.1, MAX_FPS) as f64
    }

    /// Consume interleaved I/Q taken at `center_hz` and append any frames that
    /// fell due to `out`. A change of centre starts the averages and the peaks
    /// over, since every bin now means a different frequency.
    pub fn process(&mut self, iq: &[f32], center_hz: f64, out: &mut Vec<SpectrumFrame>) {
        if center_hz != self.center_hz {
            self.center_hz = center_hz;
            self.primed = false;
            self.peak.clear();
            self.acc_n = 0;
        }
        let n = self.ring.len();
        let seg_step = self.cfg.overlap.step(n);
        let mut rest = iq;
        while rest.len() >= 2 {
            let have = rest.len() / 2;
            // Samples to take before the next frame or segment is due.
            let mut next = self.countdown;
            if seg_step.is_some() {
                next = next.min(self.seg_countdown);
            }
            let due = next.ceil().max(1.0) as usize;
            let take = have.min(due);
            let (now, later) = rest.split_at(take * 2);
            self.push(now);
            rest = later;
            self.countdown -= take as f64;
            if let Some(step) = seg_step {
                self.seg_countdown -= take as f64;
                if self.seg_countdown <= 0.0 {
                    self.seg_countdown += step as f64;
                    if self.filled >= n {
                        self.transform();
                        for (a, p) in self.acc.iter_mut().zip(&self.power) {
                            *a += p;
                        }
                        self.acc_n += 1;
                    }
                }
            }
            if self.countdown <= 0.0 {
                self.countdown += self.hop();
                if self.filled >= n {
                    out.push(self.frame());
                } else {
                    // Not enough yet for a whole transform: try again as soon
                    // as there is.
                    self.countdown = self.countdown.min((n - self.filled) as f64);
                }
            }
        }
    }

    /// [`Self::process`] for samples already complex.
    pub fn process_complex(
        &mut self,
        iq: &[Complex32],
        center_hz: f64,
        out: &mut Vec<SpectrumFrame>,
    ) {
        let mut buf = std::mem::take(&mut self.interleaved);
        buf.clear();
        buf.extend(iq.iter().flat_map(|c| [c.re, c.im]));
        self.process(&buf, center_hz, out);
        self.interleaved = buf;
    }

    /// Put interleaved samples into the ring, keeping only the newest `N`.
    fn push(&mut self, iq: &[f32]) {
        let n = self.ring.len();
        let count = iq.len() / 2;
        let skip = count.saturating_sub(n);
        for p in iq[skip * 2..].chunks_exact(2) {
            self.ring[self.ring_at] = Complex32::new(p[0], p[1]);
            self.ring_at += 1;
            if self.ring_at == n {
                self.ring_at = 0;
            }
        }
        self.filled = (self.filled + count).min(n);
    }

    /// One windowed transform of the newest `N` samples into `power`, in
    /// display order (natural bin `N/2..N`, the negative frequencies, first).
    fn transform(&mut self) {
        let n = self.ring.len();
        let half = n / 2;
        // Oldest sample first: the ring from the write position on.
        let (newer, older) = self.ring.split_at(self.ring_at);
        for ((w, &x), &c) in self.work.iter_mut().zip(older.iter().chain(newer)).zip(&self.window) {
            *w = x * c;
        }
        self.fft.process_with_scratch(&mut self.work, &mut self.scratch);
        for (k, p) in self.power.iter_mut().enumerate() {
            *p = self.work[(k + half) % n].norm_sqr() * self.norm;
        }
    }

    fn frame(&mut self) -> SpectrumFrame {
        let n = self.ring.len();
        if self.acc_n > 0 {
            // Welch: the mean of the segments since the last frame.
            let k = 1.0 / self.acc_n as f32;
            for (p, a) in self.power.iter_mut().zip(self.acc.iter_mut()) {
                *p = *a * k;
                *a = 0.0;
            }
            self.acc_n = 0;
        } else {
            self.transform();
        }

        let alpha = 1.0 / self.cfg.averaging.frames() as f32;
        let primed = self.primed;
        self.primed = true;
        for (a, &p) in self.avg.iter_mut().zip(&self.power) {
            // Unprimed (or holding something that is not a number), the
            // average starts from this frame: `a + 1·(p − a)` would keep a
            // NaN for good.
            if primed && a.is_finite() {
                *a += alpha * (p - *a);
            } else {
                *a = p;
            }
        }
        let cal = self.cfg.calibration_db;
        let db: Vec<f32> = self.avg.iter().map(|&p| 10.0 * (p + 1e-20).log10() + cal).collect();
        let noise_dbm = self.noise_floor() + cal;

        let peak = if self.cfg.peak_hold {
            let fall = self.cfg.peak_decay_db_per_s.max(0.0) * (self.hop() / self.rate) as f32;
            if self.peak.len() != n {
                self.peak = db.clone();
            } else {
                for (h, &d) in self.peak.iter_mut().zip(&db) {
                    *h = (*h - fall).max(d);
                }
            }
            Some(self.peak.clone())
        } else {
            None
        };
        self.seq += 1;
        SpectrumFrame {
            center_hz: self.center_hz,
            span_hz: self.rate,
            db,
            peak,
            seq: self.seq,
            enbw: self.enbw,
            noise_dbm,
        }
    }

    /// The median of the averaged power, dBFS, from at most 32768 bins spread
    /// evenly across the span (a median does not need them all).
    fn noise_floor(&mut self) -> f32 {
        let n = self.avg.len();
        if n == 0 {
            return -200.0;
        }
        let stride = n.div_ceil(32_768).max(1);
        self.noise_scratch.clear();
        self.noise_scratch.extend(self.avg.iter().step_by(stride).copied());
        let mid = self.noise_scratch.len() / 2;
        let (_, m, _) = self.noise_scratch.select_nth_unstable_by(mid, f32::total_cmp);
        10.0 * (*m + 1e-20).log10()
    }
}

/// Pool `bins` into `columns` display columns by taking the maximum of the bins
/// each column covers, so a large FFT sharpens a narrow display rather than
/// aliasing it. With more columns than bins each column takes the bin it falls
/// in.
pub fn pool_max(bins: &[f32], columns: usize) -> Vec<f32> {
    let mut out = Vec::new();
    pool_max_range(bins, 0.0, bins.len() as f64, columns, &mut out);
    out
}

/// [`pool_max`] over the fractional bin range `first..last` only — a zoomed
/// view — into `out`, which is cleared first.
pub fn pool_max_range(bins: &[f32], first: f64, last: f64, columns: usize, out: &mut Vec<f32>) {
    out.clear();
    if bins.is_empty() || columns == 0 {
        return;
    }
    let n = bins.len();
    let first = first.clamp(0.0, n as f64);
    let last = last.clamp(first, n as f64);
    let per = (last - first) / columns as f64;
    out.reserve(columns);
    for c in 0..columns {
        let a = first + c as f64 * per;
        let b = a + per;
        let lo = (a.floor() as usize).min(n - 1);
        // Every bin the column touches, and at least the one it starts in.
        let hi = ((b.ceil() as usize).min(n)).max(lo + 1);
        let m = bins[lo..hi].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        out.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{IqSource, SynthSignal, SyntheticSource};

    fn carrier(rate: f64, hz: f64, amp: f32, n: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let ph = std::f64::consts::TAU * hz * i as f64 / rate;
            v.push(amp * ph.cos() as f32);
            v.push(amp * ph.sin() as f32);
        }
        v
    }

    #[test]
    fn a_carrier_at_12k5_lands_in_its_bin_at_its_level() {
        let rate = 192_000.0;
        for window in [Window::Hann, Window::BlackmanHarris] {
            let cfg = SpectrumConfig {
                fft_size: 8192,
                window,
                calibration_db: -10.0,
                ..Default::default()
            };
            let mut s = Spectrum::new(cfg, rate);
            // 12.5 kHz is a third of a bin off the grid at 192 kHz / 8192
            // (23.4375 Hz bins), so allow the window's scalloping loss there.
            let iq = carrier(rate, 12_500.0, 0.1, 20_000);
            let mut frames = Vec::new();
            s.process(&iq, 7_100_000.0, &mut frames);
            let f = frames.last().expect("a frame");
            let (k, &peak) = f.db.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            assert_eq!(f.bin_of(7_112_500.0), Some(k), "{window:?}: peak in bin {k}");
            assert!((f.bin_hz(k) - 7_112_500.0).abs() <= rate / 8192.0 / 2.0);
            // -20 dBFS carrier, -10 dB calibration.
            assert!(peak < -29.95 && peak > -30.8, "{window:?}: read {peak} dBm");
            assert_eq!(f.span_hz, rate);
            assert_eq!(f.center_hz, 7_100_000.0);
        }
    }

    #[test]
    fn bin_zero_is_the_bottom_of_the_span_and_the_middle_is_the_centre() {
        let rate = 384_000.0;
        let mut s = Spectrum::new(SpectrumConfig { fft_size: 4096, ..Default::default() }, rate);
        let mut frames = Vec::new();
        s.process(&carrier(rate, 0.0, 0.5, 8192), 14e6, &mut frames);
        let f = &frames[0];
        assert_eq!(f.bin_hz(0), 14e6 - rate / 2.0);
        let k = f.db.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(k, 2048);
    }

    #[test]
    fn the_frame_rate_is_limited_by_the_sample_count() {
        let rate = 6_144_000.0;
        let mut s =
            Spectrum::new(SpectrumConfig { fft_size: 2048, fps: 25.0, ..Default::default() }, rate);
        let mut frames = Vec::new();
        let block = vec![0.0f32; 2 * 61_440]; // 10 ms
        for _ in 0..100 {
            s.process(&block, 0.0, &mut frames);
        }
        // One second of samples at 25 frames a second.
        assert!((24..=26).contains(&frames.len()), "{} frames", frames.len());
    }

    #[test]
    fn a_big_fft_at_a_low_rate_overlaps_rather_than_stalling() {
        let rate = 192_000.0;
        let mut s = Spectrum::new(
            SpectrumConfig { fft_size: 131_072, fps: 30.0, ..Default::default() },
            rate,
        );
        let mut frames = Vec::new();
        let block = vec![0.0f32; 2 * 1920];
        for _ in 0..200 {
            s.process(&block, 0.0, &mut frames);
        }
        // 2 s of samples, the first 0.68 s spent filling the ring.
        assert!(frames.len() >= 38 && frames.len() <= 42, "{} frames", frames.len());
        assert_eq!(frames[0].db.len(), 131_072);
    }

    #[test]
    fn averaging_smooths_the_noise_and_keeps_the_level() {
        let rate = 192_000.0;
        let spread = |avg: Averaging| {
            let cfg =
                SpectrumConfig { fft_size: 4096, averaging: avg, fps: 60.0, ..Default::default() };
            let mut s = Spectrum::new(cfg, rate);
            let mut src = SyntheticSource::new(rate, 0.0, -60.0, vec![]);
            let mut buf = vec![0.0f32; 2 * 3200];
            let mut frames = Vec::new();
            for _ in 0..200 {
                let n = src.read(&mut buf);
                s.process(&buf[..n], 0.0, &mut frames);
            }
            let f = frames.last().unwrap();
            let mean = f.db.iter().sum::<f32>() / f.db.len() as f32;
            let var = f.db.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / f.db.len() as f32;
            (mean, var.sqrt())
        };
        let (_, off) = spread(Averaging::Off);
        let (_, x8) = spread(Averaging::X8);
        assert!(x8 < off * 0.6, "spread {off} dB off, {x8} dB at x8");
    }

    #[test]
    fn peak_hold_holds_then_decays() {
        let rate = 192_000.0;
        let cfg = SpectrumConfig {
            fft_size: 2048,
            peak_hold: true,
            peak_decay_db_per_s: 20.0,
            fps: 10.0,
            ..Default::default()
        };
        let mut s = Spectrum::new(cfg, rate);
        let mut frames = Vec::new();
        s.process(&carrier(rate, 12_000.0, 1.0, 19_200), 0.0, &mut frames);
        let k = frames[0].bin_of(12_000.0).unwrap();
        let held = frames.last().unwrap().peak.as_ref().unwrap()[k];
        assert!(held > -1.0);
        frames.clear();
        // One second of silence: the held peak falls 20 dB, not to the floor.
        s.process(&vec![0.0; 2 * 192_000], 0.0, &mut frames);
        let after = frames.last().unwrap().peak.as_ref().unwrap()[k];
        assert!((held - after - 20.0).abs() < 2.5, "{held} -> {after}");
        assert!(frames.last().unwrap().db[k] < -150.0);
    }

    #[test]
    fn pooling_by_max_keeps_a_one_bin_signal() {
        let mut bins = vec![-120.0f32; 131_072];
        bins[70_001] = -20.0;
        let cols = pool_max(&bins, 1000);
        assert_eq!(cols.len(), 1000);
        assert_eq!(cols.iter().copied().fold(f32::MIN, f32::max), -20.0);
        assert_eq!(cols[(70_001.0 / 131.072) as usize], -20.0);
        // More columns than bins: each column takes the bin under it.
        let wide = pool_max(&[1.0, 2.0], 4);
        assert_eq!(wide, vec![1.0, 1.0, 2.0, 2.0]);
        let mut zoom = Vec::new();
        pool_max_range(&bins, 69_000.0, 71_000.0, 100, &mut zoom);
        assert_eq!(zoom[50], -20.0);
    }

    #[test]
    fn fft_sizes_are_kept_to_supported_powers_of_two() {
        assert_eq!(valid_fft_size(1000), 2048);
        assert_eq!(valid_fft_size(5000), 4096);
        assert_eq!(valid_fft_size(7000), 8192);
        assert_eq!(valid_fft_size(1 << 21), 1 << 20);
        assert_eq!(valid_fft_size(1 << 20), 1 << 20);
        let s = Spectrum::new(SpectrumConfig { fft_size: 65_536, ..Default::default() }, 1e6);
        assert_eq!(s.fft_size(), 65_536);
    }

    #[test]
    fn synthetic_carriers_show_where_they_were_put() {
        let rate = 768_000.0;
        let sigs =
            vec![SynthSignal::carrier(-150_000.0, -40.0), SynthSignal::carrier(212_345.0, -70.0)];
        let mut src = SyntheticSource::new(rate, 21e6, -110.0, sigs);
        let mut s = Spectrum::new(SpectrumConfig { fft_size: 16_384, ..Default::default() }, rate);
        let mut buf = vec![0.0f32; 2 * 16_384];
        let mut frames = Vec::new();
        while frames.is_empty() {
            let n = src.read(&mut buf);
            s.process(&buf[..n], src.center_hz(), &mut frames);
        }
        let f = &frames[0];
        for (hz, want) in [(21e6 - 150_000.0, -40.0), (21e6 + 212_345.0, -70.0)] {
            let k = f.bin_of(hz).unwrap();
            let got = f.db[k.saturating_sub(2)..k + 3].iter().copied().fold(f32::MIN, f32::max);
            // Off-bin carriers lose up to ~1.4 dB of scalloping in BH.
            assert!(got <= want + 0.2 && got > want - 1.6, "{hz}: {got} dB, want {want}");
        }
    }

    #[test]
    fn welch_uses_every_sample_and_smooths_the_noise() {
        // A small FFT at a wide span: Latest sees 4 % of the samples, Welch
        // all of them, so its noise is far steadier for the same frame rate.
        let rate = 1_536_000.0;
        let spread = |overlap: Overlap| {
            let cfg = SpectrumConfig { fft_size: 2048, overlap, fps: 30.0, ..Default::default() };
            let mut s = Spectrum::new(cfg, rate);
            let mut src = SyntheticSource::new(rate, 0.0, -60.0, vec![]);
            let mut buf = vec![0.0f32; 2 * 16_384];
            let mut frames = Vec::new();
            for _ in 0..100 {
                let n = src.read(&mut buf);
                s.process(&buf[..n], 0.0, &mut frames);
            }
            assert!((30..=33).contains(&frames.len()), "{overlap:?}: {}", frames.len());
            let f = frames.last().unwrap();
            let mean = f.db.iter().sum::<f32>() / f.db.len() as f32;
            (f.db.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / f.db.len() as f32).sqrt()
        };
        let latest = spread(Overlap::Latest);
        let half = spread(Overlap::Half);
        assert!(half < latest * 0.2, "spread {latest} dB latest, {half} dB Welch");
        assert_eq!(Overlap::ThreeQuarters.step(4096), Some(1024));
        assert_eq!(Overlap::Latest.step(4096), None);
    }

    #[test]
    fn the_noise_floor_is_the_median_and_ignores_signals() {
        let rate = 192_000.0;
        let sigs =
            vec![SynthSignal::carrier(10_000.0, -30.0), SynthSignal::carrier(-40_000.0, -40.0)];
        let mut src = SyntheticSource::new(rate, 0.0, -100.0, sigs);
        let cfg = SpectrumConfig {
            fft_size: 8192,
            averaging: Averaging::X8,
            overlap: Overlap::Half,
            ..Default::default()
        };
        let mut s = Spectrum::new(cfg, rate);
        let mut buf = vec![0.0f32; 2 * 8192];
        let mut frames = Vec::new();
        for _ in 0..60 {
            let n = src.read(&mut buf);
            s.process(&buf[..n], 0.0, &mut frames);
        }
        let f = frames.last().unwrap();
        // -100 dBFS of noise over the whole span is -100 - 10·log10(N/ENBW)
        // per bin.
        let want = -100.0 - 10.0 * (8192.0f32 / f.enbw).log10();
        assert!((f.enbw - 2.0).abs() < 0.01);
        assert!(f.noise_dbm < want + 0.5 && f.noise_dbm > want - 2.5, "{} vs {want}", f.noise_dbm);
    }

    #[test]
    fn peaks_are_interpolated_between_bins() {
        let rate = 192_000.0;
        for window in [Window::Hann, Window::BlackmanHarris, Window::FlatTop] {
            let cfg = SpectrumConfig { fft_size: 65_536, window, ..Default::default() };
            let mut s = Spectrum::new(cfg, rate);
            let mut frames = Vec::new();
            // 2.93 Hz bins; the carrier 0.37 of a bin off the grid.
            let hz = 12_345.678;
            s.process(&carrier(rate, hz, 0.1, 70_000), 0.0, &mut frames);
            let f = frames.last().unwrap();
            let p = f.peak_in(hz - 50.0, hz + 50.0).unwrap();
            assert!((p.hz - hz).abs() < 0.1, "{window:?}: {} vs {hz}", p.hz);
            assert!((p.dbm - -20.0).abs() < 0.3, "{window:?}: {} dBm", p.dbm);
        }
        // At an edge, the bin itself.
        let p = interpolate_peak(&[0.0, -10.0], 0, 100.0, 10.0);
        assert_eq!((p.hz, p.dbm), (100.0, 0.0));
    }

    #[test]
    fn a_million_points_resolve_a_few_hertz_at_6_mhz() {
        let rate = 6_144_000.0;
        let cfg = SpectrumConfig { fft_size: 1 << 20, fps: 10.0, ..Default::default() };
        let mut s = Spectrum::new(cfg, rate);
        assert_eq!(s.fft_size(), 1 << 20);
        let mut frames = Vec::new();
        s.process(&carrier(rate, 100_003.0, 0.1, 1 << 20), 0.0, &mut frames);
        let f = frames.last().unwrap();
        assert!(f.bin_width() < 6.0);
        let p = f.peak_in(99_900.0, 100_100.0).unwrap();
        assert!((p.hz - 100_003.0).abs() < 0.3, "{}", p.hz);
    }
}
