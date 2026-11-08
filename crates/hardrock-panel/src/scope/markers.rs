//! Measurement markers: two of them, M1 and M2, with the difference between
//! them in frequency and level, and marker-to-peak.
//!
//! The DUO runs from a GPS-disciplined reference (AJ3G's Leo Bodnar GPSDO), so
//! frequencies here are good to well under a hertz: a marker on a peak reads
//! the peak interpolated between bins, to 0.1 Hz.
//!
//! The markers read a [`Bins`]: a CPU spectrum frame, or [`Exact`], the GPU
//! spectrum's trace columns with the strongest bins it found in the bands
//! asked for (the view, and each tracking marker's window), read back at
//! the spectrum's full resolution.

use sdr::{Peak, SpectrumFrame};

/// What a marker reads from.
pub trait Bins {
    /// The strongest signal within `[lo, hi]` Hz, interpolated between bins.
    fn peak_in(&self, lo: f64, hi: f64) -> Option<Peak>;
    /// The level at `hz`.
    fn level_at(&self, hz: f64) -> Option<f32>;
    /// The spectrum's resolution, Hz.
    fn bin_width(&self) -> f64;
    /// The strongest of the columns drawn within `[lo, hi]`: what the eye
    /// picks as the highest signal on the trace.
    fn drawn_peak_in(&self, lo: f64, hi: f64) -> Option<Peak> {
        self.peak_in(lo, hi)
    }
    /// The width of a drawn column, Hz.
    fn drawn_width(&self) -> f64 {
        self.bin_width()
    }
    /// The noise floor, dBm a bin.
    fn noise_dbm(&self) -> f32;
}

impl Bins for SpectrumFrame {
    fn peak_in(&self, lo: f64, hi: f64) -> Option<Peak> {
        SpectrumFrame::peak_in(self, lo, hi)
    }
    fn level_at(&self, hz: f64) -> Option<f32> {
        level_at(self, hz)
    }
    fn bin_width(&self) -> f64 {
        SpectrumFrame::bin_width(self)
    }
    fn noise_dbm(&self) -> f32 {
        self.noise_dbm
    }
}

/// The strongest bin the GPU found in `[lo, hi]` Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExactPeak {
    pub lo: f64,
    pub hi: f64,
    pub peak: Peak,
}

/// A frame of columns, and the peaks found at full resolution.
pub struct Exact<'a> {
    pub frame: &'a SpectrumFrame,
    pub peaks: &'a [ExactPeak],
    /// The real bin width, Hz.
    pub bin_hz: f64,
}

impl Bins for Exact<'_> {
    /// A peak found over (at least half of) the same band, else the
    /// strongest column.
    fn peak_in(&self, lo: f64, hi: f64) -> Option<Peak> {
        let want = (hi - lo).max(0.0);
        self.peaks
            .iter()
            .filter(|e| {
                let overlap = e.hi.min(hi) - e.lo.max(lo);
                (lo..=hi).contains(&e.peak.hz) && overlap >= 0.5 * want
            })
            .min_by(|a, b| {
                let da = ((a.hi - a.lo) - want).abs();
                let db = ((b.hi - b.lo) - want).abs();
                da.total_cmp(&db)
            })
            .map(|e| e.peak)
            .or_else(|| self.frame.peak_in(lo, hi))
    }
    fn level_at(&self, hz: f64) -> Option<f32> {
        level_at(self.frame, hz)
    }
    fn bin_width(&self) -> f64 {
        self.bin_hz
    }
    fn drawn_peak_in(&self, lo: f64, hi: f64) -> Option<Peak> {
        self.frame.peak_in(lo, hi)
    }
    fn drawn_width(&self) -> f64 {
        self.frame.bin_width()
    }
    fn noise_dbm(&self) -> f32 {
        self.frame.noise_dbm
    }
}

/// A tracking marker follows a peak only this far over the noise, dB.
const TRACK_OVER_NOISE_DB: f32 = 6.0;

/// One marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marker {
    /// Where it is, the radio's own frequency, Hz.
    pub hz: f64,
    /// It follows the peak it was put on (marker-to-peak), so a drifting
    /// signal keeps its marker.
    pub track: bool,
}

/// What a marker reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub hz: f64,
    pub dbm: f32,
}

/// The pair, and which one the next placement moves.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Markers {
    pub m: [Option<Marker>; 2],
    pub active: usize,
}

impl Markers {
    /// Put the active marker at `hz`; the next placement moves the other one
    /// while it is not yet placed.
    pub fn place(&mut self, hz: f64) {
        let i = self.active.min(1);
        self.m[i] = Some(Marker { hz, track: false });
        if self.m[1 - i].is_none() {
            self.active = 1 - i;
        }
    }

    /// Move marker `i` (a drag), which stops it tracking.
    pub fn move_to(&mut self, i: usize, hz: f64) {
        if let Some(m) = self.m.get_mut(i).and_then(|m| m.as_mut()) {
            m.hz = hz;
            m.track = false;
        }
    }

    pub fn clear(&mut self) {
        *self = Markers::default();
    }

    pub fn any(&self) -> bool {
        self.m.iter().any(|m| m.is_some())
    }

    /// Put the active marker on the strongest signal in `[lo, hi]` and have it
    /// follow it.
    ///
    /// The strongest is found on the trace as it is drawn, then read to the
    /// hertz around that column. Read on the GPU's raw spectrum across the
    /// whole view it could land on a moment's spike rather than the signal
    /// standing highest on the screen (AJ3G, 8 Nov 2026: "Marker does not
    /// snap to highest signal shown in scope").
    pub fn snap_to_peak(&mut self, frame: &impl Bins, lo: f64, hi: f64) {
        if let Some(col) = frame.drawn_peak_in(lo, hi) {
            let w = frame.drawn_width() * 1.5;
            let p = frame.peak_in(col.hz - w, col.hz + w).unwrap_or(col);
            let i = self.active.min(1);
            self.m[i] = Some(Marker { hz: p.hz, track: true });
        }
    }

    /// Let tracking markers follow their peaks, within `window` Hz either
    /// side.
    pub fn track(&mut self, frame: &impl Bins, window: f64) {
        for m in self.m.iter_mut().flatten() {
            // Only to a signal: with it gone (a CW key up, a fade) the
            // strongest thing near is the noise, and the marker wandered off.
            if m.track
                && let Some(p) = frame.peak_in(m.hz - window, m.hz + window)
                && p.dbm >= frame.noise_dbm() + TRACK_OVER_NOISE_DB
            {
                m.hz = p.hz;
            }
        }
    }

    /// What marker `i` reads on `frame`.
    pub fn read(&self, i: usize, frame: &impl Bins) -> Option<Reading> {
        let m = self.m.get(i).copied().flatten()?;
        if m.track {
            // On its peak: the interpolated level.
            let w = frame.bin_width() * 1.5;
            let p = frame.peak_in(m.hz - w, m.hz + w)?;
            return Some(Reading { hz: m.hz, dbm: p.dbm });
        }
        Some(Reading { hz: m.hz, dbm: frame.level_at(m.hz)? })
    }

    /// M2 less M1: Δf (Hz) and Δlevel (dB).
    pub fn delta(&self, frame: &impl Bins) -> Option<(f64, f32)> {
        let a = self.read(0, frame)?;
        let b = self.read(1, frame)?;
        Some((b.hz - a.hz, b.dbm - a.dbm))
    }
}

/// The level at `hz`, interpolated linearly between the two bins either side.
pub fn level_at(frame: &SpectrumFrame, hz: f64) -> Option<f32> {
    let n = frame.db.len();
    if n == 0 {
        return None;
    }
    let x = (hz - frame.start_hz()) / frame.bin_width();
    if x < 0.0 || x > (n - 1) as f64 {
        return None;
    }
    let i = x.floor() as usize;
    let j = (i + 1).min(n - 1);
    let t = (x - i as f64) as f32;
    Some(frame.db[i] + (frame.db[j] - frame.db[i]) * t)
}

/// A frequency to 0.1 Hz, in MHz: 14.0740123 MHz is "14.074 012.3".
pub fn fmt_tenth(hz: f64) -> String {
    let neg = hz < 0.0;
    let d = (hz.abs() * 10.0).round() as u64;
    let (whole, tenth) = (d / 10, d % 10);
    let (m, k, u) = (whole / 1_000_000, (whole / 1000) % 1000, whole % 1000);
    format!("{}{m}.{k:03} {u:03}.{tenth}", if neg { "-" } else { "" })
}

/// A difference in frequency, signed, to 0.1 Hz with a sensible unit.
pub fn fmt_delta(hz: f64) -> String {
    let a = hz.abs();
    if a >= 1e6 {
        format!("{:+.6} MHz", hz / 1e6)
    } else if a >= 1e3 {
        format!("{:+.4} kHz", hz / 1e3)
    } else {
        format!("{hz:+.1} Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_peak(at_bin: f64) -> SpectrumFrame {
        // A Gaussian main lobe (a parabola in dB) over a flat floor, 1 Hz
        // bins from 1000 Hz.
        let db = (0..200)
            .map(|k| {
                let d = k as f64 - at_bin;
                (-50.0 - 3.0 * d * d).max(-120.0) as f32
            })
            .collect();
        SpectrumFrame {
            center_hz: 1100.0,
            span_hz: 200.0,
            db,
            peak: None,
            seq: 1,
            enbw: 2.0,
            noise_dbm: -120.0,
        }
    }

    #[test]
    fn placing_fills_m1_then_m2() {
        let mut m = Markers::default();
        m.place(10.0);
        assert_eq!(m.m[0].unwrap().hz, 10.0);
        assert_eq!(m.active, 1);
        m.place(20.0);
        assert_eq!(m.m[1].unwrap().hz, 20.0);
        // Both placed: the active one keeps moving.
        m.place(30.0);
        assert_eq!(m.m[1].unwrap().hz, 30.0);
        m.active = 0;
        m.place(5.0);
        assert_eq!(m.m[0].unwrap().hz, 5.0);
        m.clear();
        assert!(!m.any());
    }

    #[test]
    fn marker_to_peak_reads_between_bins_and_follows() {
        let f = frame_with_peak(80.3);
        let mut m = Markers::default();
        m.snap_to_peak(&f, 1000.0, 1200.0);
        let r = m.read(0, &f).unwrap();
        // The Gaussian interpolator is exact on a Gaussian lobe.
        assert!((r.hz - 1080.3).abs() < 0.05, "{}", r.hz);
        assert!((r.dbm - -50.0).abs() < 0.1, "{}", r.dbm);
        // The signal drifts 2.4 Hz: the marker goes with it.
        let g = frame_with_peak(82.7);
        m.track(&g, 5.0);
        assert!((m.m[0].unwrap().hz - 1082.7).abs() < 0.05);
        // The signal goes (key up): the marker stays where it was.
        let mut quiet = frame_with_peak(82.7);
        quiet.db.iter_mut().for_each(|d| *d = -120.0);
        quiet.db[90] = -117.0;
        m.track(&quiet, 10.0);
        assert!((m.m[0].unwrap().hz - 1082.7).abs() < 0.05);
        // Dragged, it stops tracking.
        m.move_to(0, 1050.0);
        assert!(!m.m[0].unwrap().track);
    }

    #[test]
    fn deltas_are_m2_less_m1() {
        let f = frame_with_peak(80.0);
        let mut m = Markers::default();
        m.place(1150.0);
        m.place(1080.0);
        let (df, dl) = m.delta(&f).unwrap();
        assert_eq!(df, -70.0);
        assert!((dl - 70.0).abs() < 0.01);
        assert!(level_at(&f, 900.0).is_none());
        assert!((level_at(&f, 1150.5).unwrap() - -120.0).abs() < 0.01);
    }

    #[test]
    fn the_gpus_exact_peaks_are_read_where_they_cover_the_band() {
        // Columns 10 Hz wide, and the GPU's 1 Hz-resolution peak at 1080.3 Hz.
        let cols = SpectrumFrame {
            center_hz: 1100.0,
            span_hz: 200.0,
            db: (0..20).map(|i| if i == 8 { -51.0 } else { -120.0 }).collect(),
            peak: None,
            seq: 1,
            enbw: 0.2,
            noise_dbm: -120.0,
        };
        let p = Peak { bin: 80, hz: 1080.3, dbm: -50.0 };
        let peaks = [ExactPeak { lo: 1060.0, hi: 1100.0, peak: p }];
        let e = Exact { frame: &cols, peaks: &peaks, bin_hz: 1.0 };
        assert_eq!(e.peak_in(1062.0, 1098.0), Some(p));
        // A band it does not cover: the strongest column.
        let far = e.peak_in(1000.0, 1050.0).unwrap();
        assert!(far.dbm < -100.0);
        let mut m = Markers::default();
        m.snap_to_peak(&e, 1060.0, 1100.0);
        assert_eq!(m.read(0, &e).unwrap().hz, 1080.3);
        assert_eq!(e.bin_width(), 1.0);
    }

    #[test]
    fn marker_to_peak_takes_the_highest_signal_drawn() {
        // The drawn trace (10 Hz columns) has its highest signal at column 15
        // (1150 Hz); the GPU's raw peak across the view is a spike at 1080 Hz.
        let cols = SpectrumFrame {
            center_hz: 1100.0,
            span_hz: 200.0,
            db: (0..20)
                .map(|i| match i {
                    15 => -40.0,
                    8 => -60.0,
                    _ => -120.0,
                })
                .collect(),
            peak: None,
            seq: 1,
            enbw: 0.2,
            noise_dbm: -120.0,
        };
        let spike = Peak { bin: 80, hz: 1080.0, dbm: -35.0 };
        let sig = Peak { bin: 151, hz: 1151.2, dbm: -39.0 };
        let peaks = [
            ExactPeak { lo: 1000.0, hi: 1200.0, peak: spike },
            ExactPeak { lo: 1135.0, hi: 1165.0, peak: sig },
        ];
        let e = Exact { frame: &cols, peaks: &peaks, bin_hz: 1.0 };
        let mut m = Markers::default();
        m.snap_to_peak(&e, 1000.0, 1200.0);
        assert_eq!(m.m[0].unwrap().hz, 1151.2);
    }

    #[test]
    fn readouts_go_to_a_tenth_of_a_hertz() {
        assert_eq!(fmt_tenth(14_074_012.34), "14.074 012.3");
        assert_eq!(fmt_tenth(144_174_000.05), "144.174 000.1");
        assert_eq!(fmt_delta(-70.0), "-70.0 Hz");
        assert_eq!(fmt_delta(1234.56), "+1.2346 kHz");
        assert_eq!(fmt_delta(2_500_000.0), "+2.500000 MHz");
    }
}
