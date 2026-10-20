//! The narrow lane's front end, on the ingestion thread: a digital down-
//! converter from the whole span to a few kHz of complex baseband around the
//! receiver, and the assembler that cuts any stream into the engine's
//! overlapping blocks.
//!
//! The DDC is an NCO (in f64, so it does not drift) and a cascade of
//! decimate-by-2 FIR stages: 23-tap half-bands while the band of interest is
//! a small part of the rate, then a 63-tap low-pass for the last octave,
//! which keeps ±0.44 of the lane rate flat and puts what would alias into it
//! 60 dB down. From 6.144 MS/s to 3 kHz that is eleven stages and about 30
//! multiplies a input sample, all told.

use std::f64::consts::{PI, TAU};

/// A windowed-sinc low-pass, cutoff `fc` (cycles per sample), Kaiser β = 6,
/// unity gain at DC.
fn lowpass(taps: usize, fc: f64) -> Vec<f32> {
    let w = super::engine::Taper::Kaiser.coefficients(taps);
    let m = (taps - 1) as f64 / 2.0;
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            let t = i as f64 - m;
            let s = if t == 0.0 { 2.0 * fc } else { (TAU * fc * t).sin() / (PI * t) };
            s * w[i]
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for v in &mut h {
        *v /= sum;
    }
    h.into_iter().map(|v| v as f32).collect()
}

/// One decimate-by-2 FIR stage. Unconsumed input stays in `hist`, so any
/// chunking gives the same output.
struct Half {
    h: Vec<f32>,
    hist: Vec<[f32; 2]>,
}

impl Half {
    fn new(h: Vec<f32>) -> Half {
        Half { h, hist: Vec::new() }
    }

    fn run(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        self.hist.extend_from_slice(input);
        let taps = self.h.len();
        let mut i = 0;
        while i + taps <= self.hist.len() {
            let w = &self.hist[i..i + taps];
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (c, x) in self.h.iter().zip(w) {
                re += c * x[0];
                im += c * x[1];
            }
            out.push([re, im]);
            i += 2;
        }
        self.hist.drain(..i);
    }
}

/// The down-converter: `in_rate` to `rate()` (a power-of-two division at or
/// above the target), centred `shift_hz` from the span's centre.
pub struct Ddc {
    in_rate: f64,
    rate: f64,
    shift_hz: f64,
    ph: (f64, f64),
    rot: (f64, f64),
    since_norm: u32,
    stages: Vec<Half>,
    mixed: Vec<[f32; 2]>,
    ping: Vec<[f32; 2]>,
}

impl Ddc {
    pub fn new(in_rate: f64, target: f64, shift_hz: f64) -> Ddc {
        let mut d = 1u32;
        while in_rate / (2 * d) as f64 >= target && d < 1 << 16 {
            d *= 2;
        }
        let n = d.trailing_zeros();
        let stages = (0..n)
            .map(|i| {
                if i + 1 == n { Half::new(lowpass(63, 0.25)) } else { Half::new(lowpass(23, 0.25)) }
            })
            .collect();
        let mut ddc = Ddc {
            in_rate,
            rate: in_rate / d as f64,
            shift_hz: 0.0,
            ph: (1.0, 0.0),
            rot: (1.0, 0.0),
            since_norm: 0,
            stages,
            mixed: Vec::new(),
            ping: Vec::new(),
        };
        ddc.set_shift(shift_hz);
        ddc
    }

    /// The lane's complex rate, Hz.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn in_rate(&self) -> f64 {
        self.in_rate
    }

    pub fn shift_hz(&self) -> f64 {
        self.shift_hz
    }

    /// Move the lane (the phase carries on, so there is no click).
    pub fn set_shift(&mut self, hz: f64) {
        self.shift_hz = hz;
        let w = -TAU * hz / self.in_rate;
        self.rot = (w.cos(), w.sin());
    }

    /// Down-convert interleaved I/Q, appending the lane's interleaved I/Q.
    pub fn process(&mut self, iq: &[f32], out: &mut Vec<f32>) {
        self.mixed.clear();
        let (mut pr, mut pi) = self.ph;
        let (rr, ri) = self.rot;
        for s in iq.chunks_exact(2) {
            let (x, y) = (s[0] as f64, s[1] as f64);
            self.mixed.push([(x * pr - y * pi) as f32, (x * pi + y * pr) as f32]);
            (pr, pi) = (pr * rr - pi * ri, pr * ri + pi * rr);
        }
        self.since_norm += iq.len() as u32;
        if self.since_norm > 1 << 16 {
            let m = (pr * pr + pi * pi).sqrt();
            (pr, pi) = (pr / m, pi / m);
            self.since_norm = 0;
        }
        self.ph = (pr, pi);
        let mut cur = std::mem::take(&mut self.mixed);
        for st in &mut self.stages {
            self.ping.clear();
            st.run(&cur, &mut self.ping);
            std::mem::swap(&mut cur, &mut self.ping);
        }
        out.extend(cur.iter().flatten());
        self.mixed = cur;
    }
}

/// Cuts a stream of interleaved I/Q into blocks of `len` complex samples
/// that overlap by `overlap` (the next block starts `len − overlap` on).
pub struct Blocks {
    len: usize,
    overlap: usize,
    buf: Vec<f32>,
}

impl Blocks {
    pub fn new(len: usize, overlap: usize) -> Blocks {
        Blocks { len, overlap, buf: Vec::with_capacity(4 * len) }
    }

    pub fn push(&mut self, iq: &[f32]) {
        self.buf.extend_from_slice(iq);
    }

    /// The next whole block, if there is one: a fresh `Vec` to hand to the
    /// GPU thread (one bulk copy, no per-sample work).
    pub fn next(&mut self) -> Option<Vec<f32>> {
        if self.buf.len() < 2 * self.len {
            return None;
        }
        let out = self.buf[..2 * self.len].to_vec();
        self.buf.drain(..2 * (self.len - self.overlap));
        Some(out)
    }

    /// Drop all but the newest block's worth: when the GPU is behind, the
    /// estimate wants fresh data, not a queue.
    pub fn keep_latest(&mut self) {
        let keep = 2 * self.len;
        if self.buf.len() > 2 * keep {
            let cut = self.buf.len() - keep;
            self.buf.drain(..cut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, f: f64, fs: f64) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let p = TAU * f * i as f64 / fs;
                [p.cos() as f32, p.sin() as f32]
            })
            .collect()
    }

    /// The strongest frequency in interleaved I/Q, by a plain DFT scan.
    fn peak(iq: &[f32], fs: f64, lo: f64, hi: f64, step: f64) -> (f64, f64) {
        let mut best = (0.0, 0.0);
        let mut f = lo;
        while f <= hi {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, s) in iq.chunks_exact(2).enumerate() {
                let p = -TAU * f * i as f64 / fs;
                re += s[0] as f64 * p.cos() - s[1] as f64 * p.sin();
                im += s[0] as f64 * p.sin() + s[1] as f64 * p.cos();
            }
            let m = (re * re + im * im).sqrt() / (iq.len() / 2) as f64;
            if m > best.1 {
                best = (f, m);
            }
            f += step;
        }
        best
    }

    #[test]
    fn the_lane_brings_the_receiver_to_baseband_at_its_rate() {
        let fs = 192_000.0;
        let mut d = Ddc::new(fs, 3000.0, 10_000.0);
        assert_eq!(d.rate(), 3000.0);
        let x = tone(192_000, 10_350.0, fs);
        let mut out = Vec::new();
        // In uneven chunks: the stages keep their own history.
        for c in x.chunks(2 * 777) {
            d.process(c, &mut out);
        }
        assert!((out.len() / 2) as i64 - 3000 < 4);
        let tail = &out[out.len() - 2 * 1500..];
        let (f, m) = peak(tail, 3000.0, -1000.0, 1000.0, 5.0);
        assert!((f - 350.0).abs() < 1e-9, "at {f}");
        assert!((m - 1.0).abs() < 0.02, "passband gain {m}");
    }

    #[test]
    fn what_is_outside_the_lane_does_not_alias_into_it() {
        let fs = 192_000.0;
        let mut d = Ddc::new(fs, 3000.0, 0.0);
        // 2 kHz out: it would alias to −1 kHz at 3 kS/s.
        let x = tone(192_000, 2000.0, fs);
        let mut out = Vec::new();
        d.process(&x, &mut out);
        let tail = &out[out.len() - 2 * 1500..];
        let (_, m) = peak(tail, 3000.0, -1500.0, 1500.0, 5.0);
        assert!(m < 1e-3, "leak {m}");
    }

    #[test]
    fn blocks_overlap_by_the_channelizer_length() {
        let mut b = Blocks::new(10, 4);
        let x: Vec<f32> = (0..40).map(|i| i as f32).collect();
        b.push(&x);
        let a = b.next().unwrap();
        let c = b.next().unwrap();
        assert!(b.next().is_none());
        assert_eq!(a.len(), 20);
        // The second starts six samples (twelve floats) on.
        assert_eq!(c[0], 12.0);
        assert_eq!(&a[12..20], &c[..8]);
    }
}
