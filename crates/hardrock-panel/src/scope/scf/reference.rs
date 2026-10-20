//! Test-only: the SSCA done the plain way in f64 on the CPU, the test
//! signals, and the checks against theory. The GPU's output is compared
//! with this where a wgpu adapter is to be had (lavapipe on a build box).

use std::f64::consts::TAU;

use super::engine::{Measure, Taper};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    pub fn cis(ph: f64) -> C {
        C::new(ph.cos(), ph.sin())
    }
    pub fn conj(self) -> C {
        C::new(self.re, -self.im)
    }
    pub fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    pub fn scale(self, k: f64) -> C {
        C::new(self.re * k, self.im * k)
    }
}

impl std::ops::Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}

impl std::ops::Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}

impl std::ops::Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

/// An in-place radix-2 FFT, e^(−j2πnk/N).
pub fn fft(x: &mut [C]) {
    let n = x.len();
    assert!(n.is_power_of_two());
    let bits = n.trailing_zeros();
    if bits == 0 {
        return;
    }
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            x.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let w = C::cis(-TAU / len as f64);
        for s in (0..n).step_by(len) {
            let mut wk = C::new(1.0, 0.0);
            for t in 0..len / 2 {
                let a = x[s + t];
                let b = x[s + t + len / 2] * wk;
                x[s + t] = a + b;
                x[s + t + len / 2] = a - b;
                wk = wk * w;
            }
        }
        len <<= 1;
    }
}

/// One block's SSCA: |S(k, q)|² normalised as the GPU does (index k·N + q,
/// FFT order), and each channel's power.
pub struct Block {
    pub s2: Vec<f64>,
    pub psd: Vec<f64>,
}

/// The SSCA of `x` (N + N' samples), exactly as shaders/ssca.wgsl defines it.
pub fn ssca(x: &[C], n: usize, nc: usize, taper: Taper, conj: bool) -> Block {
    assert_eq!(x.len(), n + nc);
    let a = taper.coefficients(nc);
    let g = Taper::Hamming.coefficients(n);
    let sa: f64 = a.iter().sum();
    let sg: f64 = g.iter().sum();
    // X(m, k), channel-major.
    let mut xk = vec![C::default(); nc * n];
    let mut seg = vec![C::default(); nc];
    for m in 0..n {
        for r in 0..nc {
            seg[r] = x[m + r].scale(a[r]);
        }
        fft(&mut seg);
        for k in 0..nc {
            let ph = -TAU * ((k * m) % nc) as f64 / nc as f64;
            xk[k * n + m] = seg[k] * C::cis(ph);
        }
    }
    let mut psd = vec![0.0; nc];
    let mut s2 = vec![0.0; nc * n];
    let mut y = vec![C::default(); n];
    let norm = 1.0 / (sg * sg * sa * sa);
    for k in 0..nc {
        let row = &xk[k * n..(k + 1) * n];
        psd[k] = row.iter().map(|v| v.norm2()).sum::<f64>() / (n as f64 * sa * sa);
        for m in 0..n {
            let d = x[m + nc / 2];
            let d = if conj { d } else { d.conj() };
            y[m] = (row[m] * d).scale(g[m]);
        }
        fft(&mut y);
        for q in 0..n {
            s2[k * n + q] = y[q].norm2() * norm;
        }
    }
    Block { s2, psd }
}

/// The running average the GPU keeps: linear for the first `avg` blocks,
/// then exponential with that time constant.
pub fn beta(block: u64, avg: u32) -> f64 {
    (1.0 / (block + 1) as f64).max(1.0 / avg.max(1) as f64)
}

/// The SSCA averaged over successive blocks of `x` (a block every N
/// samples, N' carried over).
pub struct Averaged {
    pub n: usize,
    pub nc: usize,
    pub conj: bool,
    pub s2: Vec<f64>,
    pub psd: Vec<f64>,
}

pub fn averaged(x: &[C], n: usize, nc: usize, taper: Taper, conj: bool, avg: u32) -> Averaged {
    let mut s2 = vec![0.0; n * nc];
    let mut psd = vec![0.0; nc];
    let mut b = 0u64;
    let mut at = 0;
    while at + n + nc <= x.len() {
        let blk = ssca(&x[at..at + n + nc], n, nc, taper, conj);
        let w = beta(b, avg);
        for (a, v) in s2.iter_mut().zip(&blk.s2) {
            *a += (v - *a) * w;
        }
        for (a, v) in psd.iter_mut().zip(&blk.psd) {
            *a += (v - *a) * w;
        }
        b += 1;
        at += n;
    }
    Averaged { n, nc, conj, s2, psd }
}

fn wrap(s: i64, m: usize) -> usize {
    s.rem_euclid(m as i64) as usize
}

impl Averaged {
    /// The estimate at signed strip point (k, q), as the shader's `value`.
    pub fn value(&self, k: i64, q: i64, measure: Measure) -> f64 {
        let kk = wrap(k, self.nc);
        let s = self.s2[kk * self.n + wrap(q, self.n)];
        if measure == Measure::Scf {
            return s;
        }
        let mut f2 = q as f64 / self.n as f64;
        if !self.conj {
            f2 = -f2;
        }
        let x = f2 * self.nc as f64;
        let ka = x.floor();
        let t = x - ka;
        let pa = self.psd[wrap(ka as i64, self.nc)];
        let pb = self.psd[wrap(ka as i64 + 1, self.nc)];
        s / (self.psd[kk] * (pa + (pb - pa) * t)).max(1e-30)
    }

    /// Every strip point: (α, f, value), cycles per sample.
    pub fn points(&self, measure: Measure) -> Vec<(f64, f64, f64)> {
        let (n, nc) = (self.n as i64, self.nc as i64);
        let mut out = Vec::with_capacity(self.s2.len());
        for k in -nc / 2..nc / 2 {
            for q in -n / 2..n / 2 {
                let kf = k as f64 / nc as f64;
                let qf = q as f64 / n as f64;
                out.push((kf + qf, (kf - qf) / 2.0, self.value(k, q, measure)));
            }
        }
        out
    }

    /// The grid as map_main draws it (dB, rows of α).
    pub fn grid(
        &self,
        measure: Measure,
        grid_f: usize,
        grid_a: usize,
        a_range: (f64, f64),
        f_range: (f64, f64),
    ) -> Vec<f64> {
        let nc = self.nc as f64;
        let nn = self.n as f64;
        let (kmin, kmax) = (-(self.nc as i64) / 2, self.nc as i64 / 2 - 1);
        let (qmin, qmax) = (-(self.n as i64) / 2, self.n as i64 / 2 - 1);
        let df = (f_range.1 - f_range.0) / grid_f as f64;
        let da = (a_range.1 - a_range.0) / grid_a as f64;
        let mut out = vec![0.0; grid_f * grid_a];
        for ai in 0..grid_a {
            for fi in 0..grid_f {
                let f0 = f_range.0 + fi as f64 * df;
                let f1 = f0 + df;
                let a0 = a_range.0 + ai as f64 * da;
                let a1 = a0 + da;
                let k_lo = ((nc * (f0 + 0.5 * a0)).ceil() as i64).max(kmin);
                let k_hi = ((nc * (f1 + 0.5 * a1)).ceil() as i64 - 1).min(kmax);
                let mut best: f64 = 0.0;
                let mut found = false;
                let k_stride = ((k_hi - k_lo + 16) / 16).max(1);
                let mut k = k_lo;
                while k <= k_hi {
                    let kf = k as f64 / nc;
                    let q_lo = ((nn * (a0 - kf)).ceil() as i64)
                        .max((nn * (kf - 2.0 * f1)).floor() as i64 + 1)
                        .max(qmin);
                    let q_hi = ((nn * (a1 - kf)).ceil() as i64 - 1)
                        .min((nn * (kf - 2.0 * f0)).floor() as i64)
                        .min(qmax);
                    let cnt = q_hi - q_lo + 1;
                    if cnt > 0 {
                        let q_stride = ((cnt + 511) / 512).max(1);
                        let mut q = q_lo;
                        while q <= q_hi {
                            best = best.max(self.value(k, q, measure));
                            found = true;
                            q += q_stride;
                        }
                    }
                    k += k_stride;
                }
                if !found {
                    let fc = f0 + 0.5 * df;
                    let ac = a0 + 0.5 * da;
                    let (mut kl, mut kh) = (k_lo, k_hi);
                    if kl > kh {
                        kl = (nc * (fc + 0.5 * ac)).round_ties_even() as i64;
                        kh = kl;
                    }
                    let mut k = kl;
                    while k <= kh {
                        let q = (nn * (ac - k as f64 / nc)).round_ties_even() as i64;
                        if (kmin..=kmax).contains(&k) && (qmin..=qmax).contains(&q) {
                            best = best.max(self.value(k, q, measure));
                        }
                        k += k_stride;
                    }
                }
                out[ai * grid_f + fi] = 10.0 * best.max(1e-30).log10();
            }
        }
        out
    }
}

/// xorshift64*, with Gaussian pairs by Box–Muller: repeatable test noise.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    /// Complex Gaussian noise with E|w|² = `power`.
    pub fn cnoise(&mut self, power: f64) -> C {
        let r = (-self.uniform().ln() * power).sqrt();
        C::cis(TAU * self.uniform()).scale(r)
    }
}

/// Complex white Gaussian noise of total power `power`.
pub fn noise(len: usize, power: f64, rng: &mut Rng) -> Vec<C> {
    (0..len).map(|_| rng.cnoise(power)).collect()
}

/// BPSK, rectangular symbols at `rs` (cycles per sample... in Hz with `fs`),
/// amplitude `amp`, on carrier `fc`: complex (`real` false) or the real
/// passband signal amp·d(t)·cos(2πfc t).
pub fn bpsk(len: usize, fs: f64, rs: f64, fc: f64, amp: f64, real: bool, rng: &mut Rng) -> Vec<C> {
    let sps = fs / rs;
    let mut sym = 0usize;
    let mut d = 1.0;
    (0..len)
        .map(|i| {
            let s = (i as f64 / sps) as usize;
            while sym < s {
                d = if rng.next_u64() & 1 == 0 { 1.0 } else { -1.0 };
                sym += 1;
            }
            let ph = TAU * fc * i as f64 / fs;
            if real { C::new(amp * d * ph.cos(), 0.0) } else { C::cis(ph).scale(amp * d) }
        })
        .collect()
}

/// FT8's waveform without its message: 8-GFSK at 6.25 Bd, tones 6.25 Hz
/// apart (h = 1), Gaussian frequency pulse BT = 2, random symbols, constant
/// amplitude `amp`; tone 0 at `f0` Hz.
pub fn ft8_like(len: usize, fs: f64, f0: f64, amp: f64, rng: &mut Rng) -> Vec<C> {
    let rs = 6.25;
    let sps = (fs / rs).round() as usize;
    let nsym = len / sps + 3;
    let syms: Vec<f64> = (0..nsym).map(|_| (rng.next_u64() % 8) as f64).collect();
    // The Gaussian-smoothed frequency pulse over three symbols (WSJT-X's
    // gfsk_pulse, BT = 2).
    let bt = 2.0;
    let c = std::f64::consts::PI * (2.0 / 2f64.ln()).sqrt();
    let erf = |x: f64| {
        // Abramowitz–Stegun 7.1.26: plenty for a test waveform.
        let s = x.signum();
        let x = x.abs();
        let t = 1.0 / (1.0 + 0.327_591_1 * x);
        let y = 1.0
            - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736)
                * t
                + 0.254_829_592)
                * t
                * (-x * x).exp();
        s * y
    };
    let pulse = |t: f64| 0.5 * (erf(c * bt * (t + 0.5)) - erf(c * bt * (t - 0.5)));
    let mut ph = 0.0;
    (0..len)
        .map(|i| {
            let t = i as f64 / sps as f64;
            let s = t.floor() as i64;
            let mut dev = 0.0;
            for j in s - 1..=s + 1 {
                if j >= 0 && (j as usize) < nsym {
                    dev += syms[j as usize] * pulse(t - j as f64 - 0.5);
                }
            }
            let f = f0 + dev * rs;
            ph += TAU * f / fs;
            C::cis(ph).scale(amp)
        })
        .collect()
}

/// On-off keyed CW: random elements of 1.2/`wpm` s (dots and spaces at the
/// keying rate), 5 ms raised-cosine edges, carrier `fc`.
pub fn cw(len: usize, fs: f64, wpm: f64, fc: f64, amp: f64, rng: &mut Rng) -> Vec<C> {
    let unit = (1.2 / wpm * fs).round() as usize;
    let edge = (0.005 * fs).max(1.0);
    let nel = len / unit + 2;
    let keys: Vec<bool> = (0..nel).map(|_| rng.next_u64() & 1 == 1).collect();
    let mut env = 0.0f64;
    let step = 1.0 / edge;
    (0..len)
        .map(|i| {
            let on = keys[i / unit];
            env = if on { (env + step).min(1.0) } else { (env - step).max(0.0) };
            let shaped = 0.5 - 0.5 * (std::f64::consts::PI * env).cos();
            C::cis(TAU * fc * i as f64 / fs).scale(amp * shaped)
        })
        .collect()
}

/// `a + b`, sample by sample.
pub fn mix(a: &[C], b: &[C]) -> Vec<C> {
    a.iter().zip(b).map(|(x, y)| *x + *y).collect()
}

/// Interleaved f32, as the ingestion thread hands blocks to the GPU.
pub fn interleave(x: &[C]) -> Vec<f32> {
    x.iter().flat_map(|c| [c.re as f32, c.im as f32]).collect()
}

/// The signal amplitude for a given SNR in 2.5 kHz (WSJT-X's convention) on
/// complex noise of total power `noise_power` at `fs`: power amp²·`duty`.
pub fn amp_for_snr(snr_db: f64, noise_power: f64, fs: f64, duty: f64) -> f64 {
    let n_2500 = noise_power * 2500.0 / fs;
    (n_2500 * 10f64.powf(snr_db / 10.0) / duty).sqrt()
}
