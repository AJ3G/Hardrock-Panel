// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/demod.rs, GPL-3.0-or-later. See NOTICE.md.
// Cut down to the sideband, AM and narrow-FM demodulators the panel's receiver
// offers; the trait loses SDRoxide's stereo, RDS, DRM and HD Radio hooks and
// FM loses its CTCSS decoder.

//! Demodulators operating on channel-rate (48 kHz) complex baseband.
//!
//! The wanted signal's dial sits at DC; the passband filter edges are in Hz
//! relative to it (negative = below the dial, which is what selects the lower
//! sideband).

use crate::fir::RealFir;
use crate::passband::{FilterShape, Passband, passband_taps};
use crate::types::Complex32;

pub trait Demodulator: Send {
    /// Consume channel-rate IQ, append audio samples at the same rate to
    /// `out`, and the audio's envelope — what the AGC levels by — to `env`,
    /// one value per audio sample.
    fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>, env: &mut Vec<f32>);
    /// The passband, `[lo, hi]` Hz from the dial, and how steep its skirts
    /// are.
    fn set_filter(&mut self, lo_hz: f32, hi_hz: f32, shape: FilterShape);
    /// Post-filter, pre-AGC signal power (dBFS, 0 dB being a complex sample of
    /// magnitude 1) for the S-meter.
    fn power_dbfs(&self) -> f32;
}

/// Smoothed power tracker shared by all demods.
///
/// Smoothed by time rather than by block, so the meter moves at the same speed
/// whatever size of block the caller feeds.
struct PowerMeter {
    mean_sq: f32,
    /// One-pole coefficient per sample.
    k: f32,
}

/// The S-meter's time constant.
const METER_TC_S: f64 = 0.05;

impl PowerMeter {
    fn new(rate: f64) -> Self {
        PowerMeter { mean_sq: 0.0, k: (1.0 / (rate * METER_TC_S)) as f32 }
    }

    fn update(&mut self, filtered: &[Complex32]) {
        if filtered.is_empty() {
            return;
        }
        let n = filtered.len() as f32;
        let p: f32 = filtered.iter().map(|z| z.norm_sqr()).sum::<f32>() / n;
        let a = 1.0 - (1.0 - self.k).powf(n);
        self.mean_sq += a * (p - self.mean_sq);
    }

    fn dbfs(&self) -> f32 {
        10.0 * (self.mean_sq + 1e-20).log10()
    }
}

/// SSB/CW: complex band-pass, take the real part.
///
/// A carrier of complex magnitude `A` inside the passband comes out as a tone
/// of amplitude `A`.
pub struct SsbDemod {
    rate: f64,
    fir: Passband,
    filtered: Vec<Complex32>,
    power: PowerMeter,
}

impl SsbDemod {
    pub fn new(rate: f64, lo: f32, hi: f32, shape: FilterShape) -> Self {
        SsbDemod {
            rate,
            fir: Passband::new(passband_taps(lo as f64, hi as f64, rate, shape)),
            filtered: Vec::new(),
            power: PowerMeter::new(rate),
        }
    }
}

impl Demodulator for SsbDemod {
    /// The envelope is `|z|`, the magnitude of the filtered signal whose real
    /// part is the audio. With the passband on one side of the dial that is
    /// the audio's true envelope — constant on a steady tone of any pitch —
    /// so the AGC holds a tone exactly at its target rather than at its
    /// sampled peaks.
    fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>, env: &mut Vec<f32>) {
        self.filtered.clear();
        self.fir.process(iq, &mut self.filtered);
        self.power.update(&self.filtered);
        out.extend(self.filtered.iter().map(|z| z.re));
        env.extend(self.filtered.iter().map(|z| z.norm()));
    }

    fn set_filter(&mut self, lo: f32, hi: f32, shape: FilterShape) {
        self.fir.set_taps(passband_taps(lo as f64, hi as f64, self.rate, shape));
    }

    fn power_dbfs(&self) -> f32 {
        self.power.dbfs()
    }
}

/// Single-pole DC blocker with a rate-aware corner frequency.
pub struct DcBlock {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlock {
    pub fn new(cutoff_hz: f64, sample_rate: f64) -> Self {
        let r = (1.0 - std::f64::consts::TAU * cutoff_hz / sample_rate).clamp(0.9, 0.999_999);
        DcBlock { r: r as f32, x1: 0.0, y1: 0.0 }
    }

    #[inline]
    pub fn run(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

/// AM: envelope detector after the band-pass, DC blocked.
pub struct AmDemod {
    rate: f64,
    fir: Passband,
    dc: DcBlock,
    filtered: Vec<Complex32>,
    power: PowerMeter,
}

impl AmDemod {
    pub fn new(rate: f64, lo: f32, hi: f32, shape: FilterShape) -> Self {
        AmDemod {
            rate,
            fir: Passband::new(passband_taps(lo as f64, hi as f64, rate, shape)),
            dc: DcBlock::new(20.0, rate),
            filtered: Vec::new(),
            power: PowerMeter::new(rate),
        }
    }
}

impl Demodulator for AmDemod {
    fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>, env: &mut Vec<f32>) {
        self.filtered.clear();
        self.fir.process(iq, &mut self.filtered);
        self.power.update(&self.filtered);
        let start = out.len();
        out.extend(self.filtered.iter().map(|z| self.dc.run(z.norm())));
        env.extend(out[start..].iter().map(|s| s.abs()));
    }

    fn set_filter(&mut self, lo: f32, hi: f32, shape: FilterShape) {
        self.fir.set_taps(passband_taps(lo as f64, hi as f64, self.rate, shape));
    }

    fn power_dbfs(&self) -> f32 {
        self.power.dbfs()
    }
}

/// Corner of the NFM listener high-pass, and how many one-pole sections it is
/// built from. Below this live the CTCSS tones and the DCS data, which arrive
/// at the speaker as a rumble without it.
const NFM_HPF_HZ: f64 = 250.0;
const NFM_HPF_POLES: usize = 4;

/// NFM: quadrature discriminator scaled for ±5 kHz deviation, then a high-pass
/// for the sub-audible signalling and an audio low-pass.
pub struct FmDemod {
    rate: f64,
    fir: Passband,
    lpf: RealFir,
    /// One-pole high-pass sections, which also block the DC an off-tune
    /// carrier leaves on the discriminator.
    hpf: Vec<DcBlock>,
    prev: Complex32,
    scale: f32,
    filtered: Vec<Complex32>,
    listen: Vec<f32>,
    power: PowerMeter,
}

impl FmDemod {
    pub fn new(rate: f64, lo: f32, hi: f32, shape: FilterShape) -> Self {
        FmDemod {
            rate,
            fir: Passband::new(passband_taps(lo as f64, hi as f64, rate, shape)),
            lpf: RealFir::lowpass(63, 3600.0, rate),
            hpf: (0..NFM_HPF_POLES).map(|_| DcBlock::new(NFM_HPF_HZ, rate)).collect(),
            prev: Complex32::new(1.0, 0.0),
            scale: (rate / (std::f64::consts::TAU * 5_000.0)) as f32,
            filtered: Vec::new(),
            listen: Vec::new(),
            power: PowerMeter::new(rate),
        }
    }
}

impl Demodulator for FmDemod {
    fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>, env: &mut Vec<f32>) {
        let start = out.len();
        self.filtered.clear();
        self.fir.process(iq, &mut self.filtered);
        self.power.update(&self.filtered);

        self.listen.clear();
        for &z in &self.filtered {
            let d = z * self.prev.conj();
            self.prev = z;
            let s = d.arg() * self.scale;
            self.listen.push(self.hpf.iter_mut().fold(s, |v, hp| hp.run(v)));
        }
        self.lpf.process(&self.listen, out);
        env.extend(out[start..].iter().map(|s| s.abs()));
    }

    fn set_filter(&mut self, lo: f32, hi: f32, shape: FilterShape) {
        self.fir.set_taps(passband_taps(lo as f64, hi as f64, self.rate, shape));
    }

    fn power_dbfs(&self) -> f32 {
        self.power.dbfs()
    }
}
