//! The zoom FFT: a second spectrum of only the part of the span on screen.
//!
//! Zooming the main spectrum only stretches its bins: at 6144 kHz a 65 536
//! point FFT has 94 Hz bins however far in the display goes. The zoom FFT
//! mixes the zoomed region down to DC, decimates it with half-band stages to
//! the narrowest power-of-two rate that still holds it, and transforms that,
//! so the same number of points spread over a few kilohertz instead of
//! megahertz: bins of a hertz or less, without the cost (or the time smear)
//! of a million-point transform of the whole span.

use crate::decim::Decimator;
use crate::nco::Nco;
use crate::spectrum::{Spectrum, SpectrumConfig, SpectrumFrame};
use crate::types::Complex32;

/// What the UI asks to see at full resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomRequest {
    /// The middle of the region, absolute Hz (the radio's own frequency).
    pub center_hz: f64,
    /// How wide the region must be, Hz.
    pub width_hz: f64,
    /// The zoom FFT's size.
    pub fft_size: usize,
}

/// The share of a decimated band clean enough to show: the half-band
/// stages' transition bands fold into the outer edges.
pub const USABLE: f64 = 0.8;

/// The decimation for a `width_hz` region of a `rate` span: the largest power
/// of two that leaves the region inside the usable part of the decimated
/// band. 1 means a zoom would gain nothing.
pub fn zoom_factor(rate: f64, width_hz: f64) -> u32 {
    if width_hz <= 0.0 || rate <= 0.0 {
        return 1;
    }
    let max = (rate * USABLE / width_hz).floor().max(1.0) as u64;
    // The largest power of two not over `max`, at most 2^16.
    1u32 << (63 - max.leading_zeros()).min(16)
}

/// A spectrum of one region of the span, at the decimated rate.
pub struct ZoomSpectrum {
    req: ZoomRequest,
    rate: f64,
    factor: u32,
    nco: Nco,
    nco_offset: f64,
    decim: Decimator,
    spectrum: Spectrum,
    iq: Vec<Complex32>,
    mixed: Vec<Complex32>,
    low: Vec<Complex32>,
}

impl ZoomSpectrum {
    /// A zoom on `req` within a span sampled at `rate`; `cfg` sets the
    /// window, averaging, peak hold, frame rate and calibration, and the
    /// request the FFT size.
    pub fn new(req: ZoomRequest, rate: f64, cfg: SpectrumConfig) -> ZoomSpectrum {
        let factor = zoom_factor(rate, req.width_hz);
        let out_rate = rate / factor as f64;
        let cfg = SpectrumConfig { fft_size: req.fft_size, ..cfg };
        ZoomSpectrum {
            req,
            rate,
            factor,
            nco: Nco::new(0.0, rate),
            nco_offset: f64::NAN,
            decim: Decimator::new(factor),
            spectrum: Spectrum::new(cfg, out_rate),
            iq: Vec::new(),
            mixed: Vec::new(),
            low: Vec::new(),
        }
    }

    pub fn request(&self) -> ZoomRequest {
        self.req
    }

    pub fn factor(&self) -> u32 {
        self.factor
    }

    /// The rate, and so the span, of the zoomed frames.
    pub fn out_rate(&self) -> f64 {
        self.rate / self.factor as f64
    }

    /// Whether this zoom still fits `req` at `rate` (else build a new one).
    pub fn fits(&self, req: ZoomRequest, rate: f64) -> bool {
        rate == self.rate
            && req.fft_size == self.req.fft_size
            && zoom_factor(rate, req.width_hz) == self.factor
    }

    /// Move the region without rebuilding (a pan).
    pub fn set_center(&mut self, center_hz: f64) {
        self.req.center_hz = center_hz;
    }

    /// Change the shared settings (window, averaging and so on).
    pub fn set_config(&mut self, cfg: SpectrumConfig) {
        self.spectrum.set_config(SpectrumConfig { fft_size: self.req.fft_size, ..cfg });
    }

    /// Consume interleaved I/Q taken at `center_hz` and append any zoomed
    /// frames due. Nothing comes out while the region is outside the span.
    pub fn process(&mut self, iq: &[f32], center_hz: f64, out: &mut Vec<SpectrumFrame>) {
        let offset = self.req.center_hz - center_hz;
        let half = self.out_rate() / 2.0;
        if offset.abs() + half > self.rate / 2.0 {
            return;
        }
        if offset != self.nco_offset {
            // Mix the region's centre down to DC.
            self.nco.set_freq(-offset, self.rate);
            self.nco_offset = offset;
        }
        self.iq.clear();
        self.iq.extend(iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])));
        self.mixed.clear();
        self.nco.mix(&self.iq, &mut self.mixed);
        self.low.clear();
        self.decim.process(&self.mixed, &mut self.low);
        self.spectrum.process_complex(&self.low, center_hz + offset, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_factor_leaves_the_region_in_the_clean_band() {
        assert_eq!(zoom_factor(6_144_000.0, 6_000.0), 512);
        assert_eq!(zoom_factor(192_000.0, 150_000.0), 1);
        assert_eq!(zoom_factor(192_000.0, 76_800.0), 2);
        assert_eq!(zoom_factor(192_000.0, 76_801.0), 1);
        assert_eq!(zoom_factor(192_000.0, 0.0), 1);
        assert_eq!(zoom_factor(6_144_000.0, 1.0), 1 << 16);
        for (rate, w) in [(6_144_000.0, 3000.0), (192_000.0, 20_000.0), (1_536_000.0, 9000.0)] {
            let f = zoom_factor(rate, w) as f64;
            assert!(rate / f * USABLE >= w, "{rate} {w}");
            assert!(rate / (2.0 * f) * USABLE < w, "{rate} {w} could go further");
        }
    }

    fn carrier(rate: f64, hz: f64, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let ph = std::f64::consts::TAU * hz * i as f64 / rate;
                [amp * ph.cos() as f32, amp * ph.sin() as f32]
            })
            .collect()
    }

    #[test]
    fn a_zoom_resolves_two_close_carriers_the_main_spectrum_cannot() {
        // Two carriers 30 Hz apart at 1536 kHz: 8192-point bins are 187.5 Hz,
        // so the main spectrum sees one lump. Zoomed to 6 kHz, the same 8192
        // points make bins under a hertz wide.
        let rate = 1_536_000.0;
        let center = 14_000_000.0;
        let (f1, f2) = (center + 200_010.0, center + 200_040.0);
        let mut iq = carrier(rate, f1 - center, 0.01, 1_536_000);
        for (s, t) in iq.iter_mut().zip(carrier(rate, f2 - center, 0.01, 1_536_000)) {
            *s += t;
        }
        let cfg = SpectrumConfig { fps: 4.0, ..Default::default() };
        let req = ZoomRequest { center_hz: center + 200_000.0, width_hz: 6000.0, fft_size: 8192 };
        let mut z = ZoomSpectrum::new(req, rate, cfg);
        assert_eq!(z.factor(), 128);
        let mut frames = Vec::new();
        for block in iq.chunks(2 * 32_768) {
            z.process(block, center, &mut frames);
        }
        let f = frames.last().expect("a zoomed frame");
        assert_eq!(f.span_hz, rate / 128.0);
        assert_eq!(f.center_hz, center + 200_000.0);
        assert!(f.bin_width() < 1.5);
        let p1 = f.peak_in(f1 - 10.0, f1 + 10.0).unwrap();
        let p2 = f.peak_in(f2 - 10.0, f2 + 10.0).unwrap();
        assert!((p1.hz - f1).abs() < 0.2, "{} vs {f1}", p1.hz);
        assert!((p2.hz - f2).abs() < 0.2, "{} vs {f2}", p2.hz);
        // -40 dBFS each, through the decimators' passband.
        assert!((p1.dbm - -40.0).abs() < 1.0, "{}", p1.dbm);
        let mid = f.peak_in(f1 + 12.0, f2 - 12.0).unwrap();
        assert!(mid.dbm < -80.0, "a deep gap between them: {}", mid.dbm);
    }

    #[test]
    fn a_region_outside_the_span_makes_no_frames() {
        let rate = 192_000.0;
        let req = ZoomRequest { center_hz: 7e6 + 95_000.0, width_hz: 6000.0, fft_size: 4096 };
        let mut z = ZoomSpectrum::new(req, rate, SpectrumConfig::default());
        let mut frames = Vec::new();
        z.process(&vec![0.0; 2 * 192_000], 7e6, &mut frames);
        assert!(frames.is_empty());
    }
}
