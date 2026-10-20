//! FIR filter design: windowed-sinc filters sized from a specification
//! rather than from a tap count picked by hand.
//!
//! A filter here is asked for by what it has to do — where its passband ends,
//! where its stopband starts and how far down the stopband must be — and the
//! window and the length follow from that:
//!
//! * [`FirWindow::Kaiser`] takes Kaiser's own formulas: the shape parameter β
//!   from the attenuation, the length from the attenuation and the transition
//!   width. It is the window to use when a number has been promised, because
//!   it trades main-lobe width against sidelobe level continuously and so
//!   spends exactly the taps the specification needs.
//! * [`FirWindow::BlackmanHarris`] is the fixed 4-term window the rest of the
//!   crate has always used. Its sidelobes stop at about −92 dB whatever the
//!   length, so it cannot be asked for more than that; its length is set from
//!   the transition width by the rule measured in this file's tests.
//!
//! Everything is designed in f64 and handed out in f64; callers round to f32
//! once, at the end.

use std::f64::consts::{PI, TAU};

use crate::Complex32;

/// The modified Bessel function of the first kind, order zero, by its power
/// series. Kaiser's window is built from it.
///
/// The series converges for every argument; it is summed until a term stops
/// changing the total in the last place, which for the β a filter ever wants
/// (under 30) is some forty terms.
pub fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term, mut k) = (1.0f64, 1.0f64, 1.0f64);
    loop {
        term *= q / (k * k);
        sum += term;
        if term < sum * 1e-17 {
            return sum;
        }
        k += 1.0;
    }
}

/// Kaiser's β for a stopband `atten_db` down (Kaiser 1974, as given in
/// Oppenheim & Schafer).
pub fn kaiser_beta(atten_db: f64) -> f64 {
    let a = atten_db;
    if a > 50.0 {
        0.1102 * (a - 8.7)
    } else if a >= 21.0 {
        0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0)
    } else {
        0.0
    }
}

/// Kaiser's estimate of the taps a lowpass needs: `atten_db` of stopband and a
/// transition `transition` wide, in cycles per sample. Rounded up to odd, so
/// the filter has a centre tap and a whole-sample delay.
///
/// The estimate is good to a tap or two at the attenuations used here, and the
/// error is on both sides, so callers that promise a figure pad it: see
/// [`lowpass`].
pub fn kaiser_len(atten_db: f64, transition: f64) -> usize {
    let n = ((atten_db - 7.95) / (14.36 * transition)).ceil() as usize + 1;
    n.max(3) | 1
}

/// A Kaiser window of `n` points with shape `beta`.
pub fn kaiser(n: usize, beta: f64) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    let denom = bessel_i0(beta);
    let m = (n - 1) as f64;
    (0..n)
        .map(|i| {
            let r = 2.0 * i as f64 / m - 1.0;
            bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / denom
        })
        .collect()
}

/// The 4-term Blackman-Harris window in f64.
pub fn blackman_harris(n: usize) -> Vec<f64> {
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    if n == 1 {
        return vec![1.0];
    }
    (0..n)
        .map(|i| {
            let x = TAU * i as f64 / (n as f64 - 1.0);
            A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos()
        })
        .collect()
}

/// Transition width a Blackman-Harris windowed sinc of `n` taps needs to reach
/// its full stopband, in cycles per sample: `BH_TRANSITION_BINS / n`.
///
/// Measured by `blackman_harris_reaches_its_floor_in_the_measured_width`
/// below, not taken from a table: at seven bins the stopband is only 80 dB
/// down, at eight it is past 109 dB at every length from 101 to 3001 taps —
/// the sinc's own decay takes the product well under the window's −92 dB
/// sidelobes.
pub const BH_TRANSITION_BINS: f64 = 8.0;

/// Extra depth a Kaiser design is cut for, over what was asked.
///
/// Kaiser's formulas are for one edge on its own. A narrow lowpass — which
/// every band-pass prototype is: a 50 Hz CW filter is a lowpass 25 Hz wide —
/// has its other edge, at minus the cutoff, close enough that the two edges'
/// sidelobes add. Measured 0.7 dB short at a 0.02 passband with a 0.01
/// transition; two edges in phase could at worst cost 6 dB, but the far edge
/// is always the further away, and 3 dB covers every case tested.
const EDGE_MARGIN_DB: f64 = 3.0;

/// The window a filter is cut with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FirWindow {
    /// Kaiser, with β and the length chosen for this stopband depth.
    Kaiser { atten_db: f64 },
    /// 4-term Blackman-Harris: about 92 dB, whatever is asked.
    BlackmanHarris,
}

impl FirWindow {
    /// Taps for a transition `transition` wide (cycles per sample).
    pub fn len_for(self, transition: f64) -> usize {
        match self {
            // Kaiser's estimate is a fit, and lands short at the ends of its
            // range: measured 4 taps short at 60 dB with a 0.15 transition, 50
            // short at 120 dB with 0.001 (7805 against 7855). A margin of 1/128
            // of the length plus eight covers every case measured.
            FirWindow::Kaiser { atten_db } => {
                let n = kaiser_len(atten_db + EDGE_MARGIN_DB, transition);
                (n + n / 128 + 8) | 1
            }
            FirWindow::BlackmanHarris => {
                ((BH_TRANSITION_BINS / transition).ceil() as usize).max(3) | 1
            }
        }
    }

    /// The window itself, `n` points.
    pub fn build(self, n: usize) -> Vec<f64> {
        match self {
            FirWindow::Kaiser { atten_db } => kaiser(n, kaiser_beta(atten_db + EDGE_MARGIN_DB)),
            FirWindow::BlackmanHarris => blackman_harris(n),
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

/// A linear-phase lowpass whose passband ends at `pass` and whose stopband
/// starts at `stop` (cycles per sample, `pass < stop ≤ 0.5`). DC gain 1.
///
/// The cut is at the middle of the transition — a windowed sinc's response
/// passes −6 dB there and is symmetric about it, so the passband and the
/// stopband edges are reached at the same distance either side.
pub fn lowpass(pass: f64, stop: f64, window: FirWindow) -> Vec<f64> {
    let n = window.len_for(stop - pass);
    lowpass_n(n, (pass + stop) / 2.0, window)
}

/// [`lowpass`] with the length given: a sinc cut at `cutoff` under `window`.
pub fn lowpass_n(n: usize, cutoff: f64, window: FirWindow) -> Vec<f64> {
    let w = window.build(n);
    let c = (n - 1) as f64 / 2.0;
    let mut h: Vec<f64> =
        (0..n).map(|i| 2.0 * cutoff * sinc(2.0 * cutoff * (i as f64 - c)) * w[i]).collect();
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|t| *t /= sum);
    h
}

/// Cosine terms in the equaliser [`shaped_lowpass`] fits: a symmetric FIR of
/// `2·EQ_ORDER + 1` taps. Three is measured: a fit of order two to an
/// inverse sinc⁶ over 0–0.104 leaves 7·10⁻⁴ dB of ripple, three 4·10⁻⁵, four
/// 2·10⁻⁵ at the price of 3 dB more boost in the stopband to cut away.
const EQ_ORDER: usize = 3;

/// A lowpass whose passband follows `shape(f)` instead of being flat: the
/// gain at each frequency up to `pass` is `shape(f)`, and the stopband from
/// `stop` is at least `atten_db` down.
///
/// It is built as two filters convolved into one set of taps:
///
/// * a Kaiser [`lowpass`] for the stopband, cut `atten_db` deep *plus* however
///   much the equaliser lifts the stopband, so the product still meets the
///   figure; and
/// * a short symmetric equaliser, `2·EQ_ORDER + 1` taps, whose cosine
///   coefficients are the least-squares fit (over a fine grid of the passband)
///   of `shape(f) / lowpass(f)` — so it flattens the lowpass's own ripple as
///   well as putting the shape in.
///
/// A windowed design of the shape directly was tried first and abandoned: the
/// window smooths the response over its main lobe, which on a filter short
/// enough to be cheap is a third of the passband, and the curvature of an
/// inverse sinc came out flattened by 0.03–0.09 dB — the size of the error the
/// compensator exists to remove. The fit is smooth on a smooth target, and
/// lands under 10⁻⁴ dB (see the tests).
pub fn shaped_lowpass(pass: f64, stop: f64, atten_db: f64, shape: impl Fn(f64) -> f64) -> Vec<f64> {
    const GRID: usize = 400;
    let grid: Vec<f64> = (0..GRID).map(|i| pass * i as f64 / (GRID - 1) as f64).collect();
    let fit = |lp: &[f64]| -> Vec<f64> {
        let target: Vec<f64> = grid.iter().map(|&f| shape(f) / amplitude(lp, f)).collect();
        let basis = |f: f64, m: usize| if m == 0 { 1.0 } else { 2.0 * (TAU * f * m as f64).cos() };
        // Normal equations, (EQ_ORDER + 1)² — small enough for elimination.
        let k = EQ_ORDER + 1;
        let mut q = vec![vec![0.0f64; k + 1]; k];
        for (&f, &t) in grid.iter().zip(&target) {
            for i in 0..k {
                let bi = basis(f, i);
                for j in 0..k {
                    q[i][j] += bi * basis(f, j);
                }
                q[i][k] += bi * t;
            }
        }
        let a = solve(q);
        // Back to taps: a[0] the centre, a[m] either side of it.
        let mut eq = vec![0.0; 2 * EQ_ORDER + 1];
        for m in 0..=EQ_ORDER {
            eq[EQ_ORDER + m] = a[m];
            eq[EQ_ORDER - m] = a[m];
        }
        eq
    };
    let lp = lowpass(pass, stop, FirWindow::Kaiser { atten_db });
    let eq = fit(&lp);
    // How far the equaliser lifts the stopband, which the lowpass has to be
    // that much deeper to absorb.
    let lift = (0..=1000)
        .map(|i| gain_db(&eq, stop + (0.5 - stop) * i as f64 / 1000.0))
        .fold(0.0f64, f64::max);
    let lp = lowpass(pass, stop, FirWindow::Kaiser { atten_db: atten_db + lift });
    let eq = fit(&lp);
    convolve(&eq, &lp)
}

/// The zero-phase amplitude of symmetric taps at `f`: the response with the
/// linear phase taken out, so it is signed and smooth.
pub fn amplitude(h: &[f64], f: f64) -> f64 {
    let c = (h.len() - 1) as f64 / 2.0;
    h.iter().enumerate().map(|(i, &t)| t * (TAU * f * (i as f64 - c)).cos()).sum()
}

/// Full linear convolution of two tap sets.
pub fn convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, &x) in a.iter().enumerate() {
        for (j, &y) in b.iter().enumerate() {
            out[i + j] += x * y;
        }
    }
    out
}

/// Gaussian elimination with partial pivoting on an augmented `k × (k+1)`
/// system.
fn solve(mut q: Vec<Vec<f64>>) -> Vec<f64> {
    let k = q.len();
    for col in 0..k {
        let piv = (col..k).max_by(|&a, &b| q[a][col].abs().total_cmp(&q[b][col].abs())).unwrap();
        q.swap(col, piv);
        for row in col + 1..k {
            let m = q[row][col] / q[col][col];
            for c in col..=k {
                q[row][c] -= m * q[col][c];
            }
        }
    }
    let mut x = vec![0.0; k];
    for row in (0..k).rev() {
        let s: f64 = (row + 1..k).map(|c| q[row][c] * x[c]).sum();
        x[row] = (q[row][k] - s) / q[row][row];
    }
    x
}

/// A complex band-pass from `lo` to `hi` (cycles per sample, either may be
/// negative, `lo < hi`), with each skirt `transition` wide: a [`lowpass`] half
/// the passband wide, moved up to the passband's centre.
///
/// Taps are in the correlation orientation [`crate::fir::ComplexFir`] applies
/// them in (`y[n] = Σ h[k]·x[n+k]`, no time reversal), which is why the shift
/// is by a negative exponent.
pub fn bandpass(lo: f64, hi: f64, transition: f64, window: FirWindow) -> Vec<Complex32> {
    let half = (hi - lo).abs() / 2.0;
    let centre = (lo + hi) / 2.0;
    let lp = lowpass(half, half + transition, window);
    shift(&lp, centre)
}

/// Real taps moved up by `centre` cycles per sample, in correlation orientation.
pub fn shift(lp: &[f64], centre: f64) -> Vec<Complex32> {
    let mid = (lp.len() - 1) as f64 / 2.0;
    lp.iter()
        .enumerate()
        .map(|(i, &t)| {
            let ph = -TAU * centre * (i as f64 - mid);
            Complex32::new((ph.cos() * t) as f32, (ph.sin() * t) as f32)
        })
        .collect()
}

/// Gain in dB of real taps at `f` cycles per sample.
pub fn gain_db(taps: &[f64], f: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, &h) in taps.iter().enumerate() {
        let p = -TAU * f * k as f64;
        re += h * p.cos();
        im += h * p.sin();
    }
    10.0 * (re * re + im * im).max(1e-300).log10()
}

/// [`gain_db`] for real taps already rounded to f32 — what the filter will
/// actually run with.
pub fn gain_db_f32(taps: &[f32], f: f64) -> f64 {
    let t: Vec<f64> = taps.iter().map(|&x| x as f64).collect();
    gain_db(&t, f)
}

/// Gain in dB of complex taps, applied in correlation orientation, to a tone
/// at `f` cycles per sample.
pub fn complex_gain_db(taps: &[Complex32], f: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, h) in taps.iter().enumerate() {
        // y = Σ h[k]·x[n+k], x = e^{j2πfn}: the response is Σ h[k]·e^{+j2πfk}.
        let p = TAU * f * k as f64;
        let (c, s) = (p.cos(), p.sin());
        re += h.re as f64 * c - h.im as f64 * s;
        im += h.re as f64 * s + h.im as f64 * c;
    }
    10.0 * (re * re + im * im).max(1e-300).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Worst stopband gain from `from` to 0.5, and passband ripple (max − min)
    /// from 0 to `pass`, scanning the response finely.
    fn measure(h: &[f64], pass: f64, from: f64) -> (f64, f64) {
        let steps = 4000;
        let mut worst = f64::NEG_INFINITY;
        for i in 0..=steps {
            let f = from + (0.5 - from) * i as f64 / steps as f64;
            worst = worst.max(gain_db(h, f));
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..=steps {
            let g = gain_db(h, pass * i as f64 / steps as f64);
            lo = lo.min(g);
            hi = hi.max(g);
        }
        (worst, hi - lo)
    }

    #[test]
    fn bessel_i0_matches_known_values() {
        // Abramowitz & Stegun, table 9.8.
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008).abs() < 1e-14);
        assert!((bessel_i0(5.0) - 27.239_871_823_604_45).abs() < 1e-10);
    }

    /// The Kaiser formulas deliver what they are asked for, across the depths
    /// and widths this crate uses them at.
    #[test]
    fn kaiser_lowpasses_meet_their_specification() {
        for atten in [60.0, 90.0, 100.0, 120.0] {
            for (pass, stop) in [
                (0.1, 0.15),
                (0.2, 0.25),
                (0.02, 0.03),
                (0.3, 0.45),
                (0.0005, 0.0015),
                (0.003, 0.004),
                (0.028, 0.0295),
            ] {
                let h = lowpass(pass, stop, FirWindow::Kaiser { atten_db: atten });
                let (stop_db, ripple) = measure(&h, pass, stop);
                assert!(
                    stop_db < -atten + 0.5,
                    "{atten} dB, {pass}–{stop}: stopband only {stop_db:.1} dB, {} taps",
                    h.len()
                );
                // Kaiser ripple is the stopband's δ in the passband too.
                let want = 20.0 * (1.0 + 10f64.powf(-atten / 20.0)).log10() * 2.0;
                assert!(ripple < want + 1e-3, "{atten} dB: ripple {ripple} dB");
            }
        }
    }

    /// Where [`BH_TRANSITION_BINS`] comes from: a Blackman-Harris sinc gets to
    /// 90 dB down within that many bins of its length.
    #[test]
    fn blackman_harris_reaches_its_floor_in_the_measured_width() {
        for n in [101usize, 331, 1001] {
            let tr = BH_TRANSITION_BINS / n as f64;
            let pass = 0.1;
            let h = lowpass_n(n, pass + tr / 2.0, FirWindow::BlackmanHarris);
            let (stop_db, ripple) = measure(&h, pass, pass + tr);
            assert!(stop_db < -90.0, "{n} taps: {stop_db:.1} dB");
            assert!(ripple < 0.01, "{n} taps: ripple {ripple} dB");
        }
    }

    /// A flat shape is an ordinary lowpass, and a shaped one follows its shape.
    #[test]
    fn a_shaped_lowpass_follows_its_shape() {
        let flat = shaped_lowpass(0.1, 0.2, 100.0, |_| 1.0);
        let (stop, ripple) = measure(&flat, 0.1, 0.2);
        assert!(stop < -99.0 && ripple < 1e-3, "flat: {stop} dB, ripple {ripple} dB");
        // A 3 dB rise across the passband, smooth and even as a real
        // filter's response has to be.
        let tilt = |f: f64| 10f64.powf(3.0 * (f / 0.1).powi(2) / 20.0);
        let h = shaped_lowpass(0.1, 0.2, 100.0, tilt);
        for i in 0..=20 {
            let f = 0.1 * i as f64 / 20.0;
            let want = 20.0 * tilt(f).log10();
            let err = (gain_db(&h, f) - want).abs();
            assert!(err < 1e-3, "{f}: {}", gain_db(&h, f));
        }
        let (stop, _) = measure(&h, 0.1, 0.2);
        assert!(stop < -99.0, "{stop}");

        // What it is for: undoing a fifth-order CIC's droop. Flat to 10⁻⁴ dB
        // once the droop is put back.
        let sinc5 = |f: f64| sinc(f).powi(5);
        let h = shaped_lowpass(0.104, 0.25, 100.0, |f| 1.0 / sinc5(f));
        let mut worst = 0.0f64;
        for i in 0..=200 {
            let f = 0.104 * i as f64 / 200.0;
            worst = worst.max((gain_db(&h, f) + 20.0 * sinc5(f).log10()).abs());
        }
        assert!(worst < 1e-4, "compensated ripple {worst:.2e} dB");
        let (stop, _) = measure(&h, 0.104, 0.25);
        assert!(stop < -100.0, "{stop}");
    }
}
