//! Cascaded integrator-comb decimator (Hogenauer, 1981): decimation by any
//! integer with no multiplies at all.
//!
//! `N` integrators run at the input rate, the rate drops by `R`, and `N`
//! combs (differential delay 1) run at the output rate. The response is
//!
//! ```text
//! H(f) = [ sin(πfR) / (R·sin(πf)) ]^N        f in cycles per input sample
//! ```
//!
//! which has a null of order `N` at every multiple of the output rate — just
//! where the signals that would alias onto DC come from — and droops across
//! the passband, which a compensating FIR afterwards puts right (see
//! [`crate::ddc`]).
//!
//! # Integer arithmetic
//!
//! The integrators have a pole at DC, so they grow without bound on any input
//! with a DC component. In two's-complement integers that does not matter: the
//! arithmetic wraps, the combs subtract the wrapped values, and the result is
//! exact as long as the *output* fits — Hogenauer's own observation. In
//! floating point it does matter: an f64 integrator's rounding error is
//! integrated too, and the output drifts. So the input is converted to fixed
//! point once, everything runs in wrapping `i64`, and the output is converted
//! back.
//!
//! The scale is chosen per filter: the output grows by `R^N` over the input,
//! so `N·log₂R` bits are set aside for that, three more for inputs up to
//! ±8.0 (eight times full scale; anything larger is clamped) and one for the
//! sign. Below full scale the resolution is 2⁻²⁷ — the most an `i32` holds
//! with that headroom, chosen so the conversion is one vector instruction per
//! eight or sixteen floats (see [`to_fixed`]). Scaling an f32 by a power of
//! two is exact, so nothing an f32 carries above 2⁻²⁷ (−162 dBFS) is lost.

use crate::Complex32;

/// Orders this implementation is built for.
pub const ORDERS: std::ops::RangeInclusive<usize> = 2..=7;

/// Largest sample magnitude carried without clamping.
const HEADROOM: f64 = 8.0;

/// A CIC decimator on complex samples.
pub struct CicDecim {
    order: usize,
    factor: usize,
    /// Input to fixed point.
    scale: f64,
    /// Fixed point back to unit gain: `1 / (scale · R^N)`.
    gain: f64,
    /// Integrator and comb state, `[re, im]` per stage.
    integ: [[i64; 2]; 8],
    comb: [[i64; 2]; 8],
    /// Input samples since the last output.
    phase: usize,
    /// The block in fixed point, I and Q interleaved.
    fixed: Vec<i64>,
}

/// Bits of resolution below full scale. See the module documentation.
const FRACTION_BITS: f64 = 27.0;

crate::simd::kernel! {
    /// Floats to fixed point, clamped to the headroom. `as` saturates and
    /// sends NaN to zero, so nothing that is not a number reaches the
    /// integrators, where it would stay.
    fn to_fixed / to_fixed_portable / to_fixed_avx2 / to_fixed_avx512 (
        dst: &mut [i64],
        src: &[f32],
        scale: f32,
    ) {
        for (d, &x) in dst.iter_mut().zip(src) {
            // Through i32, which every vector unit converts to directly; the
            // headroom keeps it inside one.
            *d = (x.clamp(-HEADROOM as f32, HEADROOM as f32) * scale) as i32 as i64;
        }
    }
}

impl CicDecim {
    /// Order `order` (2 to 7), decimation `factor` (≥ 2).
    pub fn new(order: usize, factor: usize) -> Self {
        assert!(ORDERS.contains(&order), "CIC order {order} outside {ORDERS:?}");
        assert!(factor >= 2, "CIC factor {factor}");
        let growth = order as f64 * (factor as f64).log2();
        // The output has to fit an i64 with the headroom and the sign.
        let bits = (62.0 - growth.ceil() - HEADROOM.log2()).min(FRACTION_BITS);
        assert!(bits >= 20.0, "CIC N={order} R={factor} leaves only {bits} bits");
        let scale = 2f64.powf(bits);
        CicDecim {
            order,
            factor,
            scale,
            gain: 1.0 / (scale * (factor as f64).powi(order as i32)),
            integ: [[0; 2]; 8],
            comb: [[0; 2]; 8],
            phase: 0,
            fixed: Vec::new(),
        }
    }

    pub fn order(&self) -> usize {
        self.order
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Magnitude response at `f` cycles per *input* sample, linear.
    pub fn gain_at(&self, f: f64) -> f64 {
        cic_gain(f, self.factor, self.order)
    }

    /// Appends one output per `factor` inputs to `out`; a partial group is
    /// carried to the next call.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        match self.order {
            2 => self.run::<2>(input, out),
            3 => self.run::<3>(input, out),
            4 => self.run::<4>(input, out),
            5 => self.run::<5>(input, out),
            6 => self.run::<6>(input, out),
            _ => self.run::<7>(input, out),
        }
    }

    /// The filter with its order known at compile time, so both stage loops
    /// unroll and the state lives in registers.
    fn run<const N: usize>(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        self.fixed.resize(2 * input.len(), 0);
        to_fixed(&mut self.fixed, bytemuck::cast_slice(input), self.scale as f32);
        let pairs: &[[i64; 2]] = bytemuck::cast_slice(&self.fixed);
        let start = out.len();
        out.resize(start + (self.phase + input.len()) / self.factor, Complex32::default());
        let state = State {
            integ: &mut self.integ,
            comb: &mut self.comb,
            phase: &mut self.phase,
            factor: self.factor,
            gain: self.gain,
        };
        #[cfg(target_arch = "x86_64")]
        integrate_sse2::<N>(state, pairs, &mut out[start..]);
        #[cfg(not(target_arch = "x86_64"))]
        integrate_scalar::<N>(state, pairs, &mut out[start..]);
    }
}

/// What the integrate-and-comb loops work on.
struct State<'a> {
    integ: &'a mut [[i64; 2]; 8],
    comb: &'a mut [[i64; 2]; 8],
    phase: &'a mut usize,
    factor: usize,
    gain: f64,
}

/// The integrators and combs in plain integers, for every target.
///
/// I and Q are run as two passes over the block rather than together: six
/// integrators and six combs for both would be 24 live 64-bit values, more
/// than x86-64 has general registers, and run together the compiler kept them
/// on the stack — a load and a store per stage per sample.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn integrate_scalar<const N: usize>(st: State<'_>, pairs: &[[i64; 2]], dst: &mut [Complex32]) {
    let mut phase_after = *st.phase;
    for c in 0..2 {
        let mut integ = [0i64; N];
        let mut comb = [0i64; N];
        for k in 0..N {
            integ[k] = st.integ[k][c];
            comb[k] = st.comb[k][c];
        }
        let mut phase = *st.phase;
        let mut o = 0;
        for p in pairs {
            let mut v = p[c];
            for s in integ.iter_mut() {
                *s = s.wrapping_add(v);
                v = *s;
            }
            phase += 1;
            if phase == st.factor {
                phase = 0;
                for d in comb.iter_mut() {
                    let y = v.wrapping_sub(*d);
                    *d = v;
                    v = y;
                }
                let y = (v as f64 * st.gain) as f32;
                if c == 0 {
                    dst[o].re = y;
                } else {
                    dst[o].im = y;
                }
                o += 1;
            }
        }
        for k in 0..N {
            st.integ[k][c] = integ[k];
            st.comb[k][c] = comb[k];
        }
        phase_after = phase;
    }
    *st.phase = phase_after;
}

/// The same loop with I and Q side by side in one SSE2 register, which every
/// x86-64 has: one 64-bit pair add per stage per sample instead of two scalar
/// ones, and the whole state in vector registers.
///
/// Written with intrinsics because LLVM will not pair the two components of
/// [`integrate_scalar`] by itself. The two are held to each other by
/// `the_vector_and_scalar_loops_agree`.
#[cfg(target_arch = "x86_64")]
fn integrate_sse2<const N: usize>(st: State<'_>, pairs: &[[i64; 2]], dst: &mut [Complex32]) {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is part of the x86-64 baseline, so these intrinsics are
    // available on every CPU this code is compiled for; the loads read
    // exactly the 16 bytes of one `[i64; 2]` and the stores write exactly the
    // 16 bytes of one state entry.
    unsafe {
        let mut integ = [_mm_setzero_si128(); N];
        let mut comb = [_mm_setzero_si128(); N];
        for k in 0..N {
            integ[k] = _mm_loadu_si128(st.integ[k].as_ptr().cast());
            comb[k] = _mm_loadu_si128(st.comb[k].as_ptr().cast());
        }
        let mut phase = *st.phase;
        let mut o = 0;
        for p in pairs {
            let mut v = _mm_loadu_si128(p.as_ptr().cast());
            for s in integ.iter_mut() {
                *s = _mm_add_epi64(*s, v);
                v = *s;
            }
            phase += 1;
            if phase == st.factor {
                phase = 0;
                for d in comb.iter_mut() {
                    let y = _mm_sub_epi64(v, *d);
                    *d = v;
                    v = y;
                }
                let mut pair = [0i64; 2];
                _mm_storeu_si128(pair.as_mut_ptr().cast(), v);
                dst[o] = Complex32::new(
                    (pair[0] as f64 * st.gain) as f32,
                    (pair[1] as f64 * st.gain) as f32,
                );
                o += 1;
            }
        }
        for k in 0..N {
            _mm_storeu_si128(st.integ[k].as_mut_ptr().cast(), integ[k]);
            _mm_storeu_si128(st.comb[k].as_mut_ptr().cast(), comb[k]);
        }
        *st.phase = phase;
    }
}

/// `|sin(πfR) / (R·sin(πf))|^N`: a CIC's magnitude at `f` cycles per input
/// sample.
pub fn cic_gain(f: f64, factor: usize, order: usize) -> f64 {
    let r = factor as f64;
    let s = (std::f64::consts::PI * f).sin();
    let g =
        if s.abs() < 1e-15 { 1.0 } else { ((std::f64::consts::PI * f * r).sin() / (r * s)).abs() };
    g.powi(order as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, f: f64, amp: f64) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = std::f64::consts::TAU * f * i as f64;
                Complex32::new((amp * p.cos()) as f32, (amp * p.sin()) as f32)
            })
            .collect()
    }

    fn level(x: &[Complex32], f: f64) -> f64 {
        let mut acc = num_complex::Complex64::new(0.0, 0.0);
        for (i, s) in x.iter().enumerate() {
            let p = -std::f64::consts::TAU * f * i as f64;
            acc += num_complex::Complex64::new(s.re as f64, s.im as f64)
                * num_complex::Complex64::new(p.cos(), p.sin());
        }
        acc.norm() / x.len() as f64
    }

    /// Against the direct form written from the definition: an R-sample
    /// boxcar applied N times, then every R-th sample kept — in f64, so it
    /// shares nothing with the integer implementation but the maths.
    #[test]
    fn it_is_n_boxcars_and_a_decimation() {
        for (order, factor) in [(4usize, 8usize), (5, 16), (6, 4), (3, 7)] {
            let x = tone(5000, 0.0123, 0.9);
            let mut y: Vec<num_complex::Complex64> =
                x.iter().map(|s| num_complex::Complex64::new(s.re as f64, s.im as f64)).collect();
            for _ in 0..order {
                let mut z = vec![num_complex::Complex64::new(0.0, 0.0); y.len()];
                for n in 0..y.len() {
                    for k in 0..factor.min(n + 1) {
                        z[n] += y[n - k];
                    }
                    z[n] /= factor as f64;
                }
                y = z;
            }
            let mut cic = CicDecim::new(order, factor);
            let mut got = Vec::new();
            // Ragged blocks, so groups straddle calls.
            for c in x.chunks(37) {
                cic.process(c, &mut got);
            }
            assert_eq!(got.len(), x.len() / factor);
            for (k, g) in got.iter().enumerate() {
                let w = y[k * factor + factor - 1];
                let d = num_complex::Complex64::new(g.re as f64, g.im as f64) - w;
                assert!(d.norm() < 1e-6, "N={order} R={factor} out {k}: {g} vs {w}");
            }
        }
    }

    /// The formula is what the filter does, at a passband frequency and near a
    /// null where the depth is the whole point.
    #[test]
    fn the_response_is_the_sinc_power() {
        let (order, factor) = (5, 16);
        for f in [0.003, 0.02, 1.0 / 16.0 - 0.002] {
            let x = tone(1 << 17, f, 1.0);
            let mut cic = CicDecim::new(order, factor);
            let mut y = Vec::new();
            cic.process(&x, &mut y);
            let y = &y[16..];
            let got = 20.0 * level(y, (f * factor as f64) % 1.0).log10();
            let want = 20.0 * cic_gain(f, factor, order).log10();
            assert!((got - want).abs() < 0.05, "{f}: {got:.2} vs {want:.2} dB");
        }
    }

    /// Hours of DC do not walk the output away: the integrators wrap, and the
    /// combs take the wrap back out.
    #[test]
    fn dc_for_ever_is_still_dc() {
        let mut cic = CicDecim::new(6, 16);
        let block = vec![Complex32::new(0.999, -0.999); 1 << 16];
        let mut out = Vec::new();
        // 2²⁶ samples: the last integrator passes 2⁶³ thousands of times over.
        for _ in 0..1024 {
            out.clear();
            cic.process(&block, &mut out);
        }
        for s in &out {
            assert!((s.re - 0.999).abs() < 1e-6 && (s.im + 0.999).abs() < 1e-6, "{s}");
        }
    }

    /// The SSE2 loop the x86-64 build runs is the plain-integer one every
    /// other target runs, bit for bit, across ragged blocks.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_vector_and_scalar_loops_agree() {
        let x = tone(10_007, 0.0371, 0.7);
        let mut fixed = vec![0i64; 2 * x.len()];
        to_fixed(&mut fixed, bytemuck::cast_slice(&x), 2f32.powi(27));
        let pairs: &[[i64; 2]] = bytemuck::cast_slice(&fixed);
        let run = |vector: bool| {
            let (mut integ, mut comb, mut phase) = ([[0i64; 2]; 8], [[0i64; 2]; 8], 0usize);
            let mut out = Vec::new();
            let mut at = 0;
            for len in [1usize, 5, 999, 13, 4000].iter().cycle() {
                if at >= pairs.len() {
                    break;
                }
                let block = &pairs[at..(at + len).min(pairs.len())];
                at += block.len();
                let start = out.len();
                out.resize(start + (phase + block.len()) / 6, Complex32::default());
                let st = State {
                    integ: &mut integ,
                    comb: &mut comb,
                    phase: &mut phase,
                    factor: 6,
                    gain: 1e-9,
                };
                if vector {
                    integrate_sse2::<5>(st, block, &mut out[start..]);
                } else {
                    integrate_scalar::<5>(st, block, &mut out[start..]);
                }
            }
            out
        };
        assert_eq!(run(true), run(false));
    }

    /// Nothing that is not a number gets into the integrators.
    #[test]
    fn a_nan_does_not_stay() {
        let mut cic = CicDecim::new(4, 8);
        let mut out = Vec::new();
        cic.process(&[Complex32::new(f32::NAN, f32::INFINITY); 64], &mut out);
        out.clear();
        cic.process(&vec![Complex32::new(0.5, 0.0); 1024], &mut out);
        let last = out.last().unwrap();
        assert!((last.re - 0.5).abs() < 1e-6 && last.im.abs() < 1e-6, "{last}");
    }
}
