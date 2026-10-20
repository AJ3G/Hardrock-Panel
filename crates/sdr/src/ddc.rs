// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/ddc.rs, GPL-3.0-or-later. See NOTICE.md.
// The CIC chain, the Kaiser-designed final stages and the response analysis
// are this crate's own.

//! Digital down-converter: NCO mix to baseband, then multistage decimation
//! from the device rate to a ~48 kHz channel rate.
//!
//! Two chains, chosen by [`DecimatorKind`], both specified the same way at the
//! channel: flat to [`PASS_HZ`] and at least [`STOP_DB`] down on everything
//! that would alias into the ±24 kHz the channel holds.
//!
//! * [`DecimatorKind::HalfBand`]: a ladder of 23-tap half-band /2 stages down
//!   to 384 or 192 kHz, then one Kaiser-designed FIR decimator by 8 or 4.
//! * [`DecimatorKind::CicFir`]: a CIC decimator (order [`CIC_ORDER`]) down to
//!   384 kHz, a short FIR /2 that undoes the CIC's passband droop, then the
//!   same kind of Kaiser FIR /4. At 192 and 384 kHz there is nothing for a CIC
//!   to do and the chain is the half-band one.
//!
//! Every stage computes only the outputs it keeps (the polyphase saving), so
//! an FIR stage of `n` taps decimating by `m` costs `n/m` multiply-adds per
//! output. Which is the default, and why, is decision 36 in
//! docs/design-decisions.md; the numbers are from this file's tests.

use crate::Complex32;
use crate::cic::{CicDecim, cic_gain};
use crate::decim::{FirDecim, HalfbandDecim, halfband_prototype};
use crate::design::{self, FirWindow};
use crate::nco::Nco;

/// The channel is flat to here, Hz (where the output rate allows it).
pub const PASS_HZ: f64 = 20_000.0;

/// And this far down, dB, on everything that would alias into it.
pub const STOP_DB: f64 = 100.0;

/// The CIC's order. What folds into the channel through a CIC landing at
/// 384 kHz comes from within ±24 kHz of its nulls, and the worst of it is a
/// tone 21 kHz from one (at 363 kHz), which the final filter passes. Measured
/// there: order 4 is 99 dB down at 6.144 MS/s but only 86 at 768 kS/s, where
/// R = 2 makes the nulls shallow; order 5 is 124 and 107; order 6 is past
/// 125 at every rate, so the CIC is never what sets the figure.
pub const CIC_ORDER: usize = 6;

/// How the DDC decimates. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum DecimatorKind {
    /// Half-band ladder and a Kaiser FIR.
    HalfBand,
    /// CIC, compensating FIR, Kaiser FIR. The default: the two measure the
    /// same at the channel's edge, where the shared final filter sets the
    /// figure, but the CIC's nulls take what folds in from beyond ±168 kHz
    /// 11–15 dB further down (125 dB against 114 at 3.072 and 6.144 MS/s),
    /// for 0.8–1.5 ns a sample more.
    #[default]
    CicFir,
}

impl DecimatorKind {
    pub const ALL: [DecimatorKind; 2] = [DecimatorKind::HalfBand, DecimatorKind::CicFir];

    pub fn label(self) -> &'static str {
        match self {
            DecimatorKind::HalfBand => "Half-band + FIR",
            DecimatorKind::CicFir => "CIC + FIR",
        }
    }
}

/// One decimating stage.
enum Stage {
    Halfband(HalfbandDecim),
    Cic(CicDecim),
    Fir(FirDecim),
}

impl Stage {
    fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        match self {
            Stage::Halfband(s) => s.process(input, out),
            Stage::Cic(s) => s.process(input, out),
            Stage::Fir(s) => s.process(input, out),
        }
    }

    fn factor(&self) -> usize {
        match self {
            Stage::Halfband(_) => 2,
            Stage::Cic(s) => s.factor(),
            Stage::Fir(s) => s.factor(),
        }
    }

    /// Gain in dB at `f` cycles per sample of this stage's input.
    fn gain_db(&self, f: f64) -> f64 {
        match self {
            Stage::Halfband(_) => design::gain_db_f32(&halfband_prototype(), f),
            Stage::Cic(s) => 20.0 * s.gain_at(f).max(1e-300).log10(),
            Stage::Fir(s) => design::gain_db_f32(s.taps(), f),
        }
    }
}

/// The stages a chain is built from, before any filter is cut: what each
/// decimates by, and the rate that comes out.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Plan {
    Halfband,
    Cic(usize),
    /// The CIC compensator, by 2, for a CIC of this factor.
    Comp(usize),
    Final(usize),
}

fn plan(in_rate: f64, target: f64, kind: DecimatorKind) -> (Vec<Plan>, f64) {
    let mut rate = in_rate;
    let mut stages = Vec::new();
    let cic = (in_rate / (target * 8.0)).floor() as usize;
    if kind == DecimatorKind::CicFir && cic >= 2 {
        stages.push(Plan::Cic(cic));
        rate /= cic as f64;
        stages.push(Plan::Comp(cic));
        rate /= 2.0;
    } else {
        while rate > target * 8.0 {
            stages.push(Plan::Halfband);
            rate /= 2.0;
        }
    }
    let m = (rate / target).round().max(1.0) as usize;
    if m > 1 {
        stages.push(Plan::Final(m));
    }
    (stages, rate / m as f64)
}

pub struct Ddc {
    nco: Nco,
    in_rate: f64,
    kind: DecimatorKind,
    /// Each stage with the rate it is fed at.
    stages: Vec<(Stage, f64)>,
    out_rate: f64,
    tmp_a: Vec<Complex32>,
    tmp_b: Vec<Complex32>,
}

impl Ddc {
    /// Build a chain from `in_rate` down to as close to `target_rate` as an
    /// integer decimation allows, of the default kind. The exact rate is
    /// [`Self::out_rate`] — callers resample audio afterwards if they need the
    /// target exactly.
    pub fn new(in_rate: f64, target_rate: f64) -> Self {
        Self::with_kind(in_rate, target_rate, DecimatorKind::default())
    }

    /// [`Ddc::new`] with the chain chosen.
    pub fn with_kind(in_rate: f64, target_rate: f64, kind: DecimatorKind) -> Self {
        let (plan, out_rate) = plan(in_rate, target_rate, kind);
        // The channel's edges: flat to PASS_HZ where the output rate has room
        // for it (five-twelfths of the rate otherwise), and nothing from
        // beyond its Nyquist frequency left to fold in.
        let edge = out_rate / 2.0;
        let pass = PASS_HZ.min(out_rate * 5.0 / 12.0);
        let mut rate = in_rate;
        let mut stages = Vec::new();
        for p in plan {
            let stage = match p {
                Plan::Halfband => Stage::Halfband(HalfbandDecim::new()),
                Plan::Cic(r) => Stage::Cic(CicDecim::new(CIC_ORDER, r)),
                Plan::Comp(r) => {
                    // What folds onto the channel at this stage's output comes
                    // from the output rate, ± the channel's edge.
                    let stop = (rate / 2.0 - edge) / rate;
                    let h = design::shaped_lowpass(pass / rate, stop, STOP_DB, |f| {
                        1.0 / cic_gain(f / r as f64, r, CIC_ORDER)
                    });
                    Stage::Fir(FirDecim::with_taps(to_f32(&h), 2))
                }
                Plan::Final(m) => {
                    let h = design::lowpass(
                        pass / rate,
                        edge / rate,
                        FirWindow::Kaiser { atten_db: STOP_DB },
                    );
                    Stage::Fir(FirDecim::with_taps(to_f32(&h), m))
                }
            };
            let next = rate / stage.factor() as f64;
            stages.push((stage, rate));
            rate = next;
        }
        Ddc {
            nco: Nco::new(0.0, in_rate),
            in_rate,
            kind,
            stages,
            out_rate,
            tmp_a: Vec::new(),
            tmp_b: Vec::new(),
        }
    }

    /// The rate [`Ddc::new`] would settle on, without building one. Lets a
    /// caller advertise or compare the achievable rate on a hot path (the TCI
    /// server publishes it on every state tick) instead of constructing filters.
    pub fn rate_for(in_rate: f64, target_rate: f64) -> f64 {
        plan(in_rate, target_rate, DecimatorKind::default()).1
    }

    /// [`Ddc::rate_for`] for a chosen kind.
    pub fn rate_for_kind(in_rate: f64, target_rate: f64, kind: DecimatorKind) -> f64 {
        plan(in_rate, target_rate, kind).1
    }

    pub fn out_rate(&self) -> f64 {
        self.out_rate
    }

    pub fn kind(&self) -> DecimatorKind {
        self.kind
    }

    /// Tune: `offset_hz` is the wanted signal's offset from the hardware
    /// center frequency; it gets mixed down to DC.
    pub fn set_offset_hz(&mut self, offset_hz: f64) {
        self.nco.set_freq(-offset_hz, self.in_rate);
    }

    /// The chain's gain, dB, for a tone at `f_hz` from the dial, wherever in
    /// the channel it lands after the decimations fold it.
    ///
    /// Exact for a single tone: a decimation changes where a tone sits, not
    /// how strong it is, and each stage's response is periodic in its own
    /// input rate, so the gain through the chain is the sum of each stage's
    /// gain at `f_hz` over that stage's rate.
    pub fn response_db(&self, f_hz: f64) -> f64 {
        self.stages.iter().map(|(s, rate)| s.gain_db(f_hz / rate)).sum()
    }

    /// Short descriptions of the stages, for a report.
    pub fn describe(&self) -> String {
        self.stages
            .iter()
            .map(|(s, rate)| match s {
                Stage::Halfband(_) => format!("HB/2@{:.0}k", rate / 1e3),
                Stage::Cic(c) => format!("CIC{}/{}@{:.0}k", c.order(), c.factor(), rate / 1e3),
                Stage::Fir(f) => {
                    format!("FIR{}/{}@{:.0}k", f.taps().len(), f.factor(), rate / 1e3)
                }
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }

    /// Appends channel-rate samples to `out`.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        self.tmp_a.clear();
        let Some(((last, _), rest)) = self.stages.split_last_mut() else {
            self.nco.mix(input, out);
            return;
        };
        self.nco.mix(input, &mut self.tmp_a);
        for (stage, _) in rest {
            self.tmp_b.clear();
            stage.process(&self.tmp_a, &mut self.tmp_b);
            std::mem::swap(&mut self.tmp_a, &mut self.tmp_b);
        }
        last.process(&self.tmp_a, out);
    }
}

fn to_f32(h: &[f64]) -> Vec<f32> {
    h.iter().map(|&t| t as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::SUPPORTED_RATES;

    /// `rate_for` must agree with the constructed chain — it exists so callers
    /// can skip building one, which is only safe while the two stay in step.
    #[test]
    fn rate_for_matches_constructed() {
        for kind in DecimatorKind::ALL {
            for &in_rate in &[1_536_000.0, 2_000_000.0, 768_000.0, 48_000.0, 122_880.0] {
                for &target in &[48_000.0, 96_000.0, 192_000.0, 256_000.0] {
                    assert_eq!(
                        Ddc::rate_for_kind(in_rate, target, kind),
                        Ddc::with_kind(in_rate, target, kind).out_rate(),
                        "{kind:?} in={in_rate} target={target}"
                    );
                }
            }
        }
    }

    /// Passband ripple over 0–[`PASS_HZ`], and the worst gain of any tone
    /// from `from_hz` outwards (all of which fold into the channel), with the
    /// frequency it is at. Scanned on a 25 Hz grid, then refined to 1 Hz
    /// around the worst point.
    pub(crate) fn measure_from(ddc: &Ddc, from_hz: f64) -> (f64, f64, f64) {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut f = 0.0;
        while f <= PASS_HZ {
            let g = ddc.response_db(f);
            lo = lo.min(g);
            hi = hi.max(g);
            f += 25.0;
        }
        let (mut worst, mut at) = (f64::NEG_INFINITY, 0.0);
        let mut f = from_hz;
        while f <= ddc.in_rate / 2.0 {
            let g = ddc.response_db(f);
            if g > worst {
                (worst, at) = (g, f);
            }
            f += 25.0;
        }
        for d in -25..=25 {
            let f = (at + d as f64).max(from_hz);
            let g = ddc.response_db(f);
            if g > worst {
                (worst, at) = (g, f);
            }
        }
        (hi - lo, -worst, at)
    }

    /// [`measure_from`] the channel's edge: everything that can alias.
    pub(crate) fn measure(ddc: &Ddc) -> (f64, f64, f64) {
        measure_from(ddc, ddc.out_rate() / 2.0)
    }

    /// Both chains meet the channel's specification at every DUO rate,
    /// measured from the taps they run with.
    #[test]
    fn every_duo_rate_is_flat_and_clean() {
        for kind in DecimatorKind::ALL {
            for rate in SUPPORTED_RATES {
                let ddc = Ddc::with_kind(rate as f64, 48_000.0, kind);
                assert_eq!(ddc.out_rate(), 48_000.0);
                let (ripple, rejection, at) = measure(&ddc);
                assert!(ripple < 0.01, "{kind:?} {rate}: ripple {ripple:.4} dB");
                assert!(
                    rejection > STOP_DB - 0.5,
                    "{kind:?} {rate}: alias only {rejection:.1} dB down at {at:.0} Hz ({})",
                    ddc.describe()
                );
            }
        }
    }

    /// The analysis is what the chain does: a tone at the worst point found
    /// comes out at the level the analysis gave, through the real stages.
    #[test]
    fn the_worst_alias_is_where_the_analysis_says() {
        for kind in DecimatorKind::ALL {
            let rate = 6_144_000.0;
            let mut ddc = Ddc::with_kind(rate, 48_000.0, kind);
            let (_, rejection, at) = measure(&ddc);
            // Strong enough to be read well clear of the f32 floor.
            let n = 1 << 21;
            let x: Vec<Complex32> = (0..n)
                .map(|i| {
                    let p = std::f64::consts::TAU * ((at / rate * i as f64) % 1.0);
                    Complex32::new(p.cos() as f32, p.sin() as f32)
                })
                .collect();
            let mut y = Vec::new();
            for c in x.chunks(65_536) {
                ddc.process(c, &mut y);
            }
            let y = &y[2000..];
            let alias = (at / 48_000.0).fract() * 48_000.0;
            let mut acc = num_complex::Complex64::new(0.0, 0.0);
            for (i, s) in y.iter().enumerate() {
                let p = -std::f64::consts::TAU * ((alias / 48_000.0 * i as f64) % 1.0);
                acc += num_complex::Complex64::new(s.re as f64, s.im as f64)
                    * num_complex::Complex64::new(p.cos(), p.sin());
            }
            let got = -20.0 * (acc.norm() / y.len() as f64).log10();
            assert!(
                (got - rejection).abs() < 1.5,
                "{kind:?}: analysis {rejection:.1} dB at {at:.0} Hz, measured {got:.1} dB"
            );
        }
    }

    /// Nanoseconds per input sample through `ddc`: the best of seven runs of
    /// 2²² samples each, so another process on the machine shows up as a slow
    /// run that is discarded rather than as a slower chain.
    pub(crate) fn time_ns_per_sample(ddc: &mut Ddc) -> f64 {
        let block: Vec<Complex32> = (0..65_536)
            .map(|i| Complex32::new((i % 7) as f32 * 0.1, (i % 5) as f32 * 0.1))
            .collect();
        let mut out = Vec::new();
        for _ in 0..8 {
            out.clear();
            ddc.process(&block, &mut out);
        }
        let reps = 64;
        (0..7)
            .map(|_| {
                let t = std::time::Instant::now();
                for _ in 0..reps {
                    out.clear();
                    ddc.process(&block, &mut out);
                }
                t.elapsed().as_nanos() as f64 / (reps * block.len()) as f64
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// The table behind decision 36:
    /// `cargo test --release -p sdr -- --ignored --nocapture decimator_report`.
    #[test]
    #[ignore]
    fn decimator_report() {
        println!(
            "| rate kS/s | kind | stages | ripple 0–20 kHz dB | alias rejection dB (at Hz) | ns/sample |"
        );
        println!("|---|---|---|---|---|---|");
        for rate in SUPPORTED_RATES {
            for kind in DecimatorKind::ALL {
                let mut ddc = Ddc::with_kind(rate as f64, 48_000.0, kind);
                ddc.set_offset_hz(12_345.0);
                let (ripple, rejection, at) = measure(&ddc);
                let (_, far, far_at) = measure_from(&ddc, 168_000.0);
                let block: Vec<Complex32> = (0..65_536)
                    .map(|i| Complex32::new((i % 7) as f32 * 0.1, (i % 5) as f32 * 0.1))
                    .collect();
                let ns = time_ns_per_sample(&mut ddc);
                println!(
                    "| {} | {} | {} | {ripple:.5} | {rejection:.1} ({at:.0}) | \
                     {far:.1} ({far_at:.0}) | {ns:.2} |",
                    rate / 1000,
                    kind.label(),
                    ddc.describe(),
                );
            }
        }
    }
}
