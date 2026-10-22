//! AUTO ID: what mode the signal in the receiver's passband is (AJ3G: "When
//! I place my filter width over a signal of interest, go through all modes
//! and attempt to identify the mode, i.e. RTTY, Packet, PSK, FT8, MSK…").
//!
//! Two kinds of evidence, as a person at the waterfall would use:
//!
//! * **What it looks like** ([`measure`]): over the audio heard, its
//!   occupied bandwidth and centre, the tones standing out of it and their
//!   spacing, its keying rate (baud: the strongest spectral line of the
//!   envelope, or of the size of its frequency steps, which any keyed
//!   signal has at its symbol rate and noise does not), and whether its
//!   envelope is constant, keyed on and off, or varying. [`rank`] scores
//!   that against each mode's signature ([`SIGNATURES`]).
//! * **What decodes** ([`AutoId`]): every decoder the panel has, run on the
//!   same audio: the WSJT modes (a message with a good CRC settles it),
//!   JTTY, and the keyboard modes tuned to what was measured, whose text is
//!   scored for looking like a QSO ([`readable`]).
//!
//! It is advice: a signal under the noise, two signals in the passband, or a
//! mode with no signature here can fool it, and it says how sure it is.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use num_complex::Complex32;
use rustfft::FftPlanner;

use crate::resample::Resampler;
use crate::worker::{JttyDecoder, TextDecoder, TextMode, WsjtConfig, WsjtDecoder};
use crate::wsjt::WsjtMode;

/// The rate it measures at, Hz: 3 kHz of audio, and every mode's tones.
pub const RATE: f64 = 12_000.0;
/// The most audio kept, s: a whole FT8 cycle and then some.
pub const KEEP_S: f64 = 32.0;
/// Audio heard before the first measurement, s.
pub const FIRST_S: f64 = 6.0;

/// How the envelope behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    /// Constant while on (FSK, MSK, FM).
    Constant,
    /// Keyed on and off in short elements (CW, Hellschreiber).
    OnOff,
    /// Varying (PSK's shaped phase reversals, SSB voice, AM).
    Varying,
}

/// What the signal looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    /// Audio heard, s.
    pub heard_s: f64,
    /// Where most of its power is, audio Hz.
    pub centre_hz: f32,
    /// Its occupied bandwidth (90 % of the power over the noise), Hz.
    pub bw_hz: f32,
    /// Its level over the noise in the passband, dB (per bin, at its peak).
    pub snr_db: f32,
    /// The tones standing out, audio Hz, low to high.
    pub tones: Vec<f32>,
    /// Their usual spacing, Hz.
    pub spacing_hz: Option<f32>,
    /// The keying rate, Hz, and how far its line stands out, dB.
    pub baud: Option<(f32, f32)>,
    pub envelope: Envelope,
    /// For an on-off signal, how long it is on at a time on average, s.
    pub on_s: f32,
}

/// Hann-windowed Welch PSD of `x` (`n`-point, half overlap), bins 0..n/2.
fn psd(x: &[f32], n: usize) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let win: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect();
    let mut acc = vec![0.0f32; n / 2];
    let mut buf = vec![Complex32::new(0.0, 0.0); n];
    let mut count = 0;
    let mut start = 0;
    while start + n <= x.len() {
        for i in 0..n {
            buf[i] = Complex32::new(x[start + i] * win[i], 0.0);
        }
        fft.process(&mut buf);
        for (a, b) in acc.iter_mut().zip(&buf) {
            *a += b.norm_sqr();
        }
        count += 1;
        start += n / 2;
    }
    if count > 0 {
        for a in &mut acc {
            *a /= count as f32;
        }
    }
    acc
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    v.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    v[m]
}

/// The strongest line in `s` (a power spectrum, `hz_per_bin` apart) between
/// `lo` and `hi` Hz, over the median of its neighbours: (Hz, dB over them).
fn strongest_line(s: &[f32], hz_per_bin: f64, lo: f64, hi: f64) -> Option<(f64, f32)> {
    let k0 = ((lo / hz_per_bin).ceil() as usize).max(3);
    let k1 = ((hi / hz_per_bin).floor() as usize).min(s.len().saturating_sub(4));
    if k1 <= k0 {
        return None;
    }
    let ratio_at = |k: usize| -> f32 {
        let w = 40usize;
        let a = k.saturating_sub(w).max(1);
        let b = (k + w).min(s.len() - 1);
        let mut nb: Vec<f32> = (a..=b).filter(|&j| j + 2 < k || j > k + 2).map(|j| s[j]).collect();
        let m = median(&mut nb).max(1e-30);
        // A line may fall between two bins.
        (s[k] + s[k - 1].max(s[k + 1])) / m
    };
    let mut best: Option<(usize, f32)> = None;
    for k in k0..=k1 {
        if s[k] < s[k - 1] || s[k] < s[k + 1] {
            continue;
        }
        let r = ratio_at(k);
        if best.is_none_or(|(_, b)| r > b) {
            best = Some((k, r));
        }
    }
    let (k, r) = best?;
    // Parabolic refinement of where between bins the line is.
    let (a, b, c) = (s[k - 1].ln(), s[k].ln(), s[k + 1].ln());
    let d = a - 2.0 * b + c;
    let off = if d.abs() > 1e-12 { 0.5 * (a - c) / d } else { 0.0 };
    let off = off.clamp(-0.5, 0.5) as f64;
    Some(((k as f64 + off) * hz_per_bin, 10.0 * r.max(1e-30).log10()))
}

/// The power spectrum of `seq` (at `rate`), its mean taken out, and its
/// bins' spacing, Hz.
fn line_spectrum(seq: &[f32], rate: f64) -> Option<(Vec<f32>, f64)> {
    let n = seq.len().next_power_of_two() / 2;
    if n < 4096 {
        return None;
    }
    let tail = &seq[seq.len() - n..];
    let mean = tail.iter().sum::<f32>() / n as f32;
    let mut buf: Vec<Complex32> = tail.iter().map(|&v| Complex32::new(v - mean, 0.0)).collect();
    FftPlanner::<f32>::new().plan_fft_forward(n).process(&mut buf);
    Some((buf[..n / 2].iter().map(|c| c.norm_sqr()).collect(), rate / n as f64))
}

/// The lines standing `min_db` or more over their neighbours in `s`
/// between `lo` and `hi` Hz: (Hz, dB).
fn lines(s: &[f32], hz: f64, lo: f64, hi: f64, min_db: f32) -> Vec<(f64, f32)> {
    let mut out: Vec<(f64, f32)> = Vec::new();
    let k0 = ((lo / hz).ceil() as usize).max(3);
    let k1 = ((hi / hz).floor() as usize).min(s.len().saturating_sub(4));
    let mut k = k0;
    while k < k1 {
        if s[k] >= s[k - 1] && s[k] >= s[k + 1] {
            let w = (k as f64 * 0.04).max(2.0) as usize;
            if let Some(l) =
                strongest_line(s, hz, (k - w.min(k - 1)) as f64 * hz, (k + w) as f64 * hz)
                && l.1 >= min_db
                && (l.0 / hz - k as f64).abs() < 1.5
            {
                out.push(l);
                k += w;
                continue;
            }
        }
        k += 1;
    }
    out
}

/// The keying rate in any of `spectra`: the lowest clear line of which the
/// strongest is a whole multiple (a keyed signal's spectrum has lines at
/// every multiple of its rate, and the fundamental is not always the
/// strongest).
fn keying(spectra: &[(Vec<f32>, f64)], lo: f64, hi: f64) -> Option<(f32, f32)> {
    let mut all: Vec<(f64, f32)> =
        spectra.iter().flat_map(|(s, hz)| lines(s, *hz, lo, hi, 10.0)).collect();
    let best = all.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (f, db) = all
        .into_iter()
        .find(|&(f, db)| {
            let n = best.0 / f;
            if db < (best.1 - 12.0).max(12.0) {
                return false;
            }
            let r = n.round();
            (1.0..=8.0).contains(&r) && (n - r).abs() < 0.02 * r
        })
        .unwrap_or(best);
    Some((f as f32, db))
}

/// Measure the signal in `audio` (at [`RATE`]) between `lo` and `hi` audio
/// Hz: the receiver's passband. `None` with nothing over the noise.
pub fn measure(audio: &[f32], lo: f32, hi: f32) -> Option<Features> {
    let n = 8192;
    if audio.len() < 2 * n {
        return None;
    }
    let hz = RATE as f32 / n as f32;
    let p = psd(audio, n);
    let (k_lo, k_hi) =
        (((lo / hz).floor() as usize).max(1), ((hi / hz).ceil() as usize).min(p.len() - 2));
    if k_hi <= k_lo + 8 {
        return None;
    }
    let band = &p[k_lo..=k_hi];
    let floor = median(&mut band.to_vec()).max(1e-30);
    let peak = band.iter().copied().fold(0.0f32, f32::max);
    let snr_db = 10.0 * (peak / floor).log10();
    if snr_db < 8.0 {
        return None;
    }
    // Occupied band: 5 % to 95 % of the power over the noise.
    let excess: Vec<f32> = band.iter().map(|&v| (v - 2.0 * floor).max(0.0)).collect();
    let total: f32 = excess.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let (mut c, mut a, mut b, mut wsum) = (0.0f32, None, None, 0.0f32);
    for (i, &e) in excess.iter().enumerate() {
        c += e;
        wsum += e * i as f32;
        if a.is_none() && c >= 0.05 * total {
            a = Some(i);
        }
        if b.is_none() && c >= 0.95 * total {
            b = Some(i);
        }
    }
    let (a, b) = (a.unwrap_or(0), b.unwrap_or(excess.len() - 1));
    let bw_hz = ((b - a + 1) as f32 * hz).max(hz);
    let centre_hz = (k_lo as f32 + wsum / total) * hz;

    // Tones: local maxima 10 dB over the noise, at least 3 bins apart, the
    // strongest 64, inside the occupied band and a little either side.
    let mut peaks: Vec<(f32, f32)> = Vec::new();
    for i in a.saturating_sub(4).max(1)..(b + 4).min(band.len() - 1) {
        let v = band[i];
        if v > 10.0 * floor && v >= band[i - 1] && v >= band[i + 1] {
            let (x, y, z) = (band[i - 1].ln(), v.ln(), band[i + 1].ln());
            let d = x - 2.0 * y + z;
            let off = if d.abs() > 1e-12 { (0.5 * (x - z) / d).clamp(-0.5, 0.5) } else { 0.0 };
            peaks.push(((k_lo as f32 + i as f32 + off) * hz, v));
        }
    }
    peaks.sort_by(|p, q| q.1.total_cmp(&p.1));
    // Within 8 dB of the strongest: a keyed tone's sidelobes are 13 dB down.
    let strongest = peaks.first().map_or(0.0, |p| p.1);
    peaks.retain(|p| p.1 >= strongest / 6.3);

    // The band alone, as an analytic signal: its envelope and frequency.
    let len = audio.len().next_power_of_two() / 2;
    let len = len.min(1 << 19);
    let x = &audio[audio.len() - len..];
    let mut spec: Vec<Complex32> = x.iter().map(|&v| Complex32::new(v, 0.0)).collect();
    let mut planner = FftPlanner::<f32>::new();
    planner.plan_fft_forward(len).process(&mut spec);
    let bin = RATE as f32 / len as f32;
    let margin = (0.25 * bw_hz).max(15.0);
    let (f1, f2) = (
        (k_lo as f32 * hz + a as f32 * hz - margin).max(lo),
        (k_lo as f32 * hz + b as f32 * hz + margin).min(hi),
    );
    for (k, v) in spec.iter_mut().enumerate() {
        let f = k as f32 * bin;
        if k >= len / 2 || f < f1 || f > f2 {
            *v = Complex32::new(0.0, 0.0);
        }
    }
    planner.plan_fft_inverse(len).process(&mut spec);
    // To baseband about its centre, so a step's phase over the lag stays
    // inside ±π.
    let w = -2.0 * std::f64::consts::PI * centre_hz as f64 / RATE;
    for (i, v) in spec.iter_mut().enumerate() {
        let ph = (w * i as f64).rem_euclid(2.0 * std::f64::consts::PI) as f32;
        *v *= Complex32::from_polar(1.0, ph);
    }
    let env: Vec<f32> = spec.iter().map(|c| c.norm_sqr()).collect();

    // Keying, from the envelope and from the frequency's steps: the second
    // over the lag a symbol can change in, at most the bandwidth's inverse.
    let lag = ((RATE as f32 / (16.0 * bw_hz.max(20.0))).round() as usize).max(1);
    let mut ifreq = vec![0.0f32; len];
    for i in lag..len {
        let p = spec[i] * spec[i - lag].conj();
        ifreq[i] = p.arg();
    }
    let steps: Vec<f32> = (2 * lag..len).map(|i| (ifreq[i] - ifreq[i - lag]).abs()).collect();
    let hi_rate = (2.2 * bw_hz as f64).clamp(10.0, 2500.0);
    let spectra: Vec<_> =
        [line_spectrum(&env, RATE), line_spectrum(&steps, RATE)].into_iter().flatten().collect();
    let baud = keying(&spectra, 0.8, hi_rate);

    // Tones: the peaks, those closer than a keyed tone's own width (about
    // 0.4 of its rate) taken as one.
    let apart = baud.map_or(0.0, |(b, _)| 0.4 * b).max(3.0 * hz);
    let mut tones: Vec<f32> = Vec::new();
    for (f, _) in peaks {
        if tones.iter().all(|&t| (t - f).abs() >= apart) {
            tones.push(f);
        }
        if tones.len() >= 64 {
            break;
        }
    }
    tones.sort_by(f32::total_cmp);
    let spacing_hz = (tones.len() >= 2).then(|| {
        let mut d: Vec<f32> = tones.windows(2).map(|w| w[1] - w[0]).collect();
        median(&mut d)
    });

    // The envelope: keyed on and off (with short elements), constant while
    // on, or varying.
    let mut sorted = env.clone();
    sorted.sort_by(f32::total_cmp);
    let top = sorted[(sorted.len() as f32 * 0.95) as usize].max(1e-30);
    let on_level = 0.15 * top;
    let off_frac = sorted.partition_point(|&v| v < on_level) as f32 / sorted.len() as f32;
    let dip_frac = sorted.partition_point(|&v| v < 0.3 * top) as f32 / sorted.len() as f32;
    // Smoothed over 5 ms so a carrier's noise does not count as keying.
    let sm = (RATE * 0.005) as usize;
    let mut runs = (0usize, 0usize);
    let mut offs = (0usize, 0usize);
    let mut on = false;
    let mut acc = 0.0f32;
    for i in 0..len {
        acc += env[i];
        if i >= sm {
            acc -= env[i - sm];
        }
        let now = acc / sm as f32 > on_level;
        if now && !on {
            runs.0 += 1;
        }
        if now {
            runs.1 += 1;
        } else if i >= sm {
            if on {
                offs.0 += 1;
            }
            offs.1 += 1;
        }
        on = now;
    }
    let on_s = if runs.0 > 0 { runs.1 as f32 / runs.0 as f32 / RATE as f32 } else { 0.0 };
    let on_vals: Vec<f32> = env.iter().copied().filter(|&v| v > on_level).collect();
    let mean = on_vals.iter().sum::<f32>() / on_vals.len().max(1) as f32;
    let var =
        on_vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / on_vals.len().max(1) as f32;
    let cv = var.sqrt() / mean.max(1e-30);
    // Off for whole elements (CW's dots and gaps, 25 ms and more), not the
    // few milliseconds of a PSK reversal.
    let off_s = if offs.0 > 0 { offs.1 as f32 / offs.0 as f32 / RATE as f32 } else { 0.0 };
    let envelope = if off_frac > 0.15 && off_s >= 0.025 && on_s < 1.5 {
        Envelope::OnOff
    } else if dip_frac < 0.06 && cv < 0.35 {
        Envelope::Constant
    } else {
        Envelope::Varying
    };

    Some(Features {
        heard_s: audio.len() as f64 / RATE,
        centre_hz,
        bw_hz,
        snr_db,
        tones,
        spacing_hz,
        baud,
        envelope,
        on_s,
    })
}

/// What DECODE should run for a mode, where it has a decoder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    Wsjt(WsjtMode),
    Jtty,
    Text(TextMode),
    /// DECODE's mode of this label (pictures).
    Kind(&'static str),
}

/// A mode's look.
pub struct Signature {
    pub name: &'static str,
    /// Occupied bandwidth, Hz.
    pub bw: (f32, f32),
    /// Tone spacing, Hz, where it has distinct tones.
    pub spacing: Option<f32>,
    /// How many tones show.
    pub tones: Option<(usize, usize)>,
    pub baud: Option<f32>,
    pub envelope: Option<Envelope>,
    /// What DECODE runs for it (`None`: no decoder in the panel).
    pub decoder: bool,
}

const fn sig(
    name: &'static str,
    bw: (f32, f32),
    spacing: Option<f32>,
    tones: Option<(usize, usize)>,
    baud: Option<f32>,
    envelope: Option<Envelope>,
    decoder: bool,
) -> Signature {
    Signature { name, bw, spacing, tones, baud, envelope, decoder }
}

use Envelope::{Constant, OnOff, Varying};

/// The modes it knows by their look.
pub const SIGNATURES: &[Signature] = &[
    sig("CW", (3.0, 120.0), None, Some((1, 2)), None, Some(OnOff), false),
    sig("PSK31", (30.0, 90.0), None, Some((1, 3)), Some(31.25), Some(Varying), true),
    sig("PSK63", (60.0, 170.0), None, Some((1, 3)), Some(62.5), Some(Varying), false),
    sig("PSK125", (120.0, 320.0), None, Some((1, 3)), Some(125.0), Some(Varying), false),
    sig("RTTY 45", (180.0, 360.0), Some(170.0), Some((2, 2)), Some(45.45), Some(Constant), true),
    sig("RTTY 50", (180.0, 380.0), Some(170.0), Some((2, 2)), Some(50.0), Some(Constant), true),
    sig("RTTY 75", (200.0, 420.0), Some(170.0), Some((2, 2)), Some(75.0), Some(Constant), true),
    sig("RTTY 50 425", (430.0, 650.0), Some(425.0), Some((2, 2)), Some(50.0), Some(Constant), true),
    sig(
        "RTTY 50 850",
        (850.0, 1100.0),
        Some(850.0),
        Some((2, 2)),
        Some(50.0),
        Some(Constant),
        true,
    ),
    sig("NAVTEX", (200.0, 420.0), Some(170.0), Some((2, 2)), Some(100.0), Some(Constant), true),
    sig(
        "Packet 300",
        (300.0, 650.0),
        Some(200.0),
        Some((2, 2)),
        Some(300.0),
        Some(Constant),
        false,
    ),
    sig("Packet 1200", (1000.0, 2400.0), None, None, Some(1200.0), Some(Constant), false),
    sig("FT8", (35.0, 65.0), Some(6.25), Some((4, 8)), Some(6.25), Some(Constant), true),
    sig("FT4", (60.0, 100.0), Some(20.833), Some((3, 4)), Some(20.833), Some(Constant), true),
    sig("JTTY", (95.0, 150.0), Some(31.25), Some((3, 4)), Some(31.25), Some(Constant), true),
    sig("JT65", (150.0, 200.0), Some(2.69), Some((6, 64)), Some(2.69), Some(Constant), true),
    sig("JT9", (10.0, 22.0), Some(1.736), None, Some(1.736), Some(Constant), true),
    sig("WSPR", (3.0, 9.0), Some(1.4648), None, Some(1.4648), Some(Constant), true),
    sig("MSK144", (1500.0, 2600.0), Some(1000.0), Some((2, 2)), Some(2000.0), Some(Constant), true),
    sig(
        "Olivia 8/250",
        (200.0, 300.0),
        Some(31.25),
        Some((5, 8)),
        Some(31.25),
        Some(Constant),
        true,
    ),
    sig("Olivia 8/500", (400.0, 580.0), Some(62.5), Some((5, 8)), Some(62.5), Some(Constant), true),
    sig(
        "Olivia 16/500",
        (400.0, 580.0),
        Some(31.25),
        Some((9, 16)),
        Some(31.25),
        Some(Constant),
        true,
    ),
    sig(
        "Olivia 32/1000",
        (850.0, 1150.0),
        Some(31.25),
        Some((17, 32)),
        Some(31.25),
        Some(Constant),
        true,
    ),
    sig("THOR 16", (230.0, 360.0), Some(15.625), Some((6, 18)), Some(15.625), Some(Constant), true),
    sig("MFSK16", (230.0, 360.0), Some(15.625), Some((6, 16)), Some(15.625), Some(Constant), false),
    sig("Hellschreiber", (150.0, 450.0), None, Some((1, 3)), Some(122.5), Some(OnOff), false),
    sig("SSTV", (500.0, 1400.0), None, None, None, Some(Constant), true),
    sig("WEFAX", (500.0, 1000.0), None, None, None, Some(Constant), true),
    sig("Voice (SSB)", (1200.0, 3200.0), None, None, None, Some(Varying), false),
    sig("Carrier", (0.5, 6.0), None, Some((1, 1)), None, Some(Constant), false),
];

/// How well `got` matches `want`, 0 to 1, by the ratio between them.
fn closeness(got: f32, want: f32, tol: f32) -> f32 {
    let r = (got / want).ln().abs();
    (-(r / tol).powi(2)).exp()
}

/// How well the features match `s`, 0 to 1.
pub fn fit(f: &Features, s: &Signature) -> f32 {
    let mut score = 1.0f32;
    // Bandwidth: inside the range is full marks, falling off outside.
    let (a, b) = s.bw;
    score *= if f.bw_hz < a {
        closeness(f.bw_hz, a, 0.35)
    } else if f.bw_hz > b {
        closeness(f.bw_hz, b, 0.35)
    } else {
        1.0
    };
    if let Some(want) = s.baud {
        score *= match f.baud {
            Some((got, _)) => closeness(got, want, 0.06).max(0.08),
            None => 0.35,
        };
    } else if f.baud.is_some_and(|(_, db)| db > 15.0) && s.envelope != Some(OnOff) {
        // A clear keying rate is not voice or a carrier.
        score *= 0.4;
    }
    if let Some(want) = s.spacing {
        score *= match f.spacing_hz {
            Some(got) => closeness(got, want, 0.12).max(0.15),
            None => 0.5,
        };
    }
    if let Some((lo, hi)) = s.tones {
        let n = f.tones.len();
        score *= if n >= lo && n <= hi {
            1.0
        } else if n < lo {
            0.55
        } else {
            (hi as f32 / n as f32).clamp(0.2, 0.7)
        };
    }
    if let Some(e) = s.envelope {
        score *= if e == f.envelope { 1.0 } else { 0.35 };
    }
    score
}

/// One mode it might be.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub name: &'static str,
    /// How sure, 0 to 1.
    pub score: f32,
    /// Why: what matched, or what decoded.
    pub why: String,
    /// Messages or text it decoded, newest first, a few.
    pub decoded: Vec<String>,
    /// What DECODE runs for it.
    pub pick: Option<Pick>,
}

/// The features' best matches, best first.
pub fn rank(f: &Features) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = SIGNATURES
        .iter()
        .map(|s| Candidate {
            name: s.name,
            score: 0.85 * fit(f, s),
            why: describe(f),
            decoded: Vec::new(),
            pick: s.decoder.then(|| pick_for(s.name, f)).flatten(),
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// The measurement in a line.
pub fn describe(f: &Features) -> String {
    let mut s = format!("{:.0} Hz wide at {:.0} Hz", f.bw_hz, f.centre_hz);
    match f.tones.len() {
        0 => {}
        1 => s.push_str(", 1 tone"),
        n => s.push_str(&format!(", {n} tones")),
    }
    if let Some(sp) = f.spacing_hz.filter(|_| f.tones.len() >= 2) {
        s.push_str(&format!(" {sp:.1} Hz apart"));
    }
    if let Some((b, _)) = f.baud {
        s.push_str(&format!(", keyed at {b:.2} Bd"));
    }
    s.push_str(match f.envelope {
        Constant => ", constant envelope",
        OnOff => ", keyed on and off",
        Varying => ", varying envelope",
    });
    s
}

/// The decoder's set-up for mode `name`, tuned to what was measured.
pub fn pick_for(name: &str, f: &Features) -> Option<Pick> {
    let c = f.centre_hz as f64;
    let shift = f.spacing_hz.filter(|_| f.tones.len() == 2).map_or(170.0, |s| s as f64);
    Some(match name {
        "PSK31" => Pick::Text(TextMode::Psk31 { carrier_hz: c }),
        "RTTY 45" => Pick::Text(TextMode::Rtty {
            center_hz: c,
            baud: 45.45,
            shift_hz: shift,
            reverse: false,
        }),
        "RTTY 50" | "RTTY 50 425" | "RTTY 50 850" => {
            Pick::Text(TextMode::Rtty { center_hz: c, baud: 50.0, shift_hz: shift, reverse: false })
        }
        "RTTY 75" => {
            Pick::Text(TextMode::Rtty { center_hz: c, baud: 75.0, shift_hz: shift, reverse: false })
        }
        "NAVTEX" => Pick::Text(TextMode::Navtex { reverse: false }),
        "FT8" => Pick::Wsjt(WsjtMode::Ft8),
        "FT4" => Pick::Wsjt(WsjtMode::Ft4),
        "JT65" => Pick::Wsjt(WsjtMode::Jt65),
        "JT9" => Pick::Wsjt(WsjtMode::Jt9),
        "WSPR" => Pick::Wsjt(WsjtMode::Wspr),
        "MSK144" => Pick::Wsjt(WsjtMode::Msk144),
        "JTTY" => Pick::Jtty,
        "Olivia 8/250" => Pick::Text(TextMode::Olivia { audio_hz: c, tones: 8, bw_hz: 250.0 }),
        "Olivia 8/500" => Pick::Text(TextMode::Olivia { audio_hz: c, tones: 8, bw_hz: 500.0 }),
        "Olivia 16/500" => Pick::Text(TextMode::Olivia { audio_hz: c, tones: 16, bw_hz: 500.0 }),
        "Olivia 32/1000" => Pick::Text(TextMode::Olivia { audio_hz: c, tones: 32, bw_hz: 1000.0 }),
        "THOR 16" => Pick::Text(TextMode::Thor { audio_hz: c, baud: 15.625 }),
        "SSTV" => Pick::Kind("SSTV"),
        "WEFAX" => Pick::Kind("WEFAX"),
        _ => return None,
    })
}

/// How much `text` reads like a QSO or a bulletin, 0 to 1: callsigns and
/// the usual words, in letters and figures rather than a decoder's noise.
pub fn readable(text: &str) -> f32 {
    let t = text.to_uppercase();
    let chars = t.chars().filter(|c| !c.is_whitespace()).count();
    if chars < 12 {
        return 0.0;
    }
    let plain = t.chars().filter(|c| c.is_ascii_alphanumeric() || " .,/?-=+".contains(*c)).count();
    let plain_frac = plain as f32 / t.chars().count().max(1) as f32;
    const WORDS: &[&str] = &[
        "CQ", "DE", "TU", "73", "RST", "UR", "ES", "NAME", "QTH", "TNX", "FB", "OP", "HW", "BK",
        "K", "KN", "SK", "TEST", "QSL", "RIG", "ANT", "WX", "THE", "AND", "PSE", "GM", "GA", "GE",
        "599", "5NN", "AGN", "QRZ", "HR", "FER", "TO", "IS", "OM", "YL",
    ];
    let tokens: Vec<&str> = t.split_whitespace().collect();
    let words = tokens.iter().filter(|w| WORDS.contains(w)).count();
    let calls = tokens
        .iter()
        .filter(|w| crate::callsign::find_callsign(w).is_some_and(|c| c.len() == w.len()))
        .count();
    let per = (2.0 * calls as f32 + words as f32) / (tokens.len().max(1) as f32);
    let sane_len = tokens.iter().filter(|w| (1..=10).contains(&w.len())).count() as f32
        / tokens.len().max(1) as f32;
    (plain_frac * sane_len * (0.15 + per.min(1.0)) * 1.2).clamp(0.0, 0.97)
}

/// What it has found so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub heard_s: f64,
    pub features: Option<Features>,
    /// Best first.
    pub candidates: Vec<Candidate>,
}

impl Report {
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }
}

/// The text decoders tried, by the mode they stand for.
const TEXT_TRIES: &[&str] = &[
    "RTTY 45",
    "RTTY 50",
    "RTTY 75",
    "NAVTEX",
    "PSK31",
    "Olivia 8/250",
    "Olivia 8/500",
    "Olivia 16/500",
    "Olivia 32/1000",
    "THOR 16",
];

/// WSJT modes tried (their CRC settles it).
const WSJT_TRIES: &[WsjtMode] = &[
    WsjtMode::Ft8,
    WsjtMode::Ft4,
    WsjtMode::Jt65,
    WsjtMode::Jt9,
    WsjtMode::Wspr,
    WsjtMode::Msk144,
];

struct Text {
    name: &'static str,
    mode: TextMode,
    dec: TextDecoder,
    /// Its reverse (RTTY), tried alongside.
    rev: Option<(TextMode, TextDecoder)>,
    text: String,
    rev_text: String,
}

struct State {
    audio: VecDeque<f32>,
    rs: Resampler<f32>,
    tmp: Vec<f32>,
    passband: (f32, f32),
    rate: f64,
    wsjt: Vec<(WsjtMode, WsjtDecoder, Vec<String>)>,
    jtty: (JttyDecoder, Vec<String>),
    texts: Vec<Text>,
    features: Option<Features>,
    measured_at: f64,
    /// Centre the text decoders were tuned to.
    tuned_hz: Option<f32>,
}

/// AUTO ID at work: audio in, a [`Report`] out. The measurement runs on a
/// thread of its own every couple of seconds, the decoders on theirs; the
/// caller's [`AutoId::push`] and [`AutoId::report`] only copy.
pub struct AutoId {
    state: Mutex<State>,
}

/// Seconds between measurements.
const EVERY_S: f64 = 2.0;

impl AutoId {
    /// For audio at `rate` (the receiver's), its passband `passband` audio Hz.
    pub fn new(rate: f64, passband: (f32, f32), my_calls: &[String]) -> Arc<AutoId> {
        let wsjt = WSJT_TRIES
            .iter()
            .map(|&m| {
                let mut cfg = WsjtConfig::new(m);
                cfg.input_rate = rate;
                cfg.my_calls.extend(my_calls.iter().cloned());
                // Joined mid-slot it still has a go at the first one.
                cfg.max_late_s = cfg.max_late_s.max(0.6 * m.period_s().min(15.0));
                (m, WsjtDecoder::spawn(cfg), Vec::new())
            })
            .collect();
        let mid = 0.5 * (passband.0 + passband.1);
        let id = Arc::new(AutoId {
            state: Mutex::new(State {
                audio: VecDeque::new(),
                rs: Resampler::new(rate, RATE),
                tmp: Vec::new(),
                passband,
                rate,
                wsjt,
                jtty: (JttyDecoder::spawn(rate, mid), Vec::new()),
                texts: Vec::new(),
                features: None,
                measured_at: 0.0,
                tuned_hz: None,
            }),
        });
        let weak = Arc::downgrade(&id);
        let _ = std::thread::Builder::new().name("autoid-measure".into()).spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(400));
                let Some(id) = weak.upgrade() else { return };
                let (audio, (lo, hi)) = {
                    let s = id.lock();
                    let heard = s.audio.len() as f64 / RATE;
                    if heard < FIRST_S || heard - s.measured_at < EVERY_S {
                        continue;
                    }
                    (s.audio.iter().copied().collect::<Vec<f32>>(), s.passband)
                };
                let f = measure(&audio, lo.max(50.0), hi.min(5500.0));
                let mut s = id.lock();
                s.features = f;
                s.measured_at = audio.len() as f64 / RATE;
                s.retune();
            }
        });
        id
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The receiver's audio, its first sample heard at `utc`.
    pub fn push(&self, audio: &[f32], utc: f64) {
        let mut s = self.lock();
        for (_, d, _) in &s.wsjt {
            d.push(audio, utc);
        }
        s.jtty.0.push(audio, utc);
        for t in &s.texts {
            t.dec.push(audio, utc);
            if let Some((_, d)) = &t.rev {
                d.push(audio, utc);
            }
        }
        let st = &mut *s;
        st.tmp.clear();
        st.rs.process(audio, &mut st.tmp);
        st.audio.extend(st.tmp.iter().copied());
        let keep = (KEEP_S * RATE) as usize;
        while st.audio.len() > keep {
            st.audio.pop_front();
        }
    }

    /// What the decoders found, with the latest measurement, ranked.
    pub fn report(&self) -> Report {
        let mut s = self.lock();
        let heard_s = s.audio.len() as f64 / RATE;
        s.take();
        let mut candidates = match &s.features {
            Some(f) => rank(f),
            None => Vec::new(),
        };
        s.decode_evidence(&mut candidates);
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        candidates.retain(|c| c.score >= 0.05);
        candidates.truncate(8);
        Report { heard_s, features: s.features.clone(), candidates }
    }
}

impl State {
    /// Tune the keyboard decoders to what was measured, once it settles.
    fn retune(&mut self) {
        let Some(f) = self.features.clone() else { return };
        if self.tuned_hz.is_some_and(|c| (c - f.centre_hz).abs() < 0.25 * f.bw_hz.max(40.0)) {
            return;
        }
        self.tuned_hz = Some(f.centre_hz);
        self.jtty.0.retune(f.centre_hz);
        self.texts.clear();
        for &name in TEXT_TRIES {
            let Some(Pick::Text(mode)) = pick_for(name, &f) else { continue };
            let rev = match mode {
                TextMode::Rtty { center_hz, baud, shift_hz, .. } => {
                    let m = TextMode::Rtty { center_hz, baud, shift_hz, reverse: true };
                    Some((m, TextDecoder::spawn(m, self.rate)))
                }
                TextMode::Navtex { .. } => {
                    let m = TextMode::Navtex { reverse: true };
                    Some((m, TextDecoder::spawn(m, self.rate)))
                }
                _ => None,
            };
            self.texts.push(Text {
                name,
                mode,
                dec: TextDecoder::spawn(mode, self.rate),
                rev,
                text: String::new(),
                rev_text: String::new(),
            });
        }
    }

    fn take(&mut self) {
        let (lo, hi) = self.passband;
        for (_, d, got) in &mut self.wsjt {
            for slot in d.results().try_iter() {
                for m in slot.decodes {
                    // Only what is in the passband: another station on the
                    // dial is not this signal.
                    if m.audio_hz >= lo - 30.0 && m.audio_hz <= hi + 30.0 {
                        got.insert(0, m.message);
                    }
                }
            }
            got.truncate(6);
        }
        for e in self.jtty.0.events().try_iter() {
            if e.complete {
                self.jtty.1.insert(0, e.text);
                self.jtty.1.truncate(6);
            }
        }
        for t in &mut self.texts {
            for e in t.dec.events().try_iter() {
                t.text.push_str(&e.text);
            }
            if let Some((_, d)) = &t.rev {
                for e in d.events().try_iter() {
                    t.rev_text.push_str(&e.text);
                }
            }
            for s in [&mut t.text, &mut t.rev_text] {
                if s.len() > 600 {
                    let cut = s.len() - 600;
                    let cut = (cut..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
                    s.drain(..cut);
                }
            }
        }
    }

    /// Raise the modes whose decoders made sense of it.
    fn decode_evidence(&self, cands: &mut Vec<Candidate>) {
        let mut set =
            |name: &'static str, score: f32, why: String, decoded: Vec<String>, pick| match cands
                .iter_mut()
                .find(|c| c.name == name)
            {
                Some(c) => {
                    if score > c.score {
                        c.score = score;
                        c.why = why;
                    }
                    if !decoded.is_empty() {
                        c.decoded = decoded;
                    }
                    if c.pick.is_none() {
                        c.pick = pick;
                    }
                }
                None => cands.push(Candidate { name, score, why, decoded, pick }),
            };
        for (m, _, got) in &self.wsjt {
            if !got.is_empty() {
                let name = match m {
                    WsjtMode::Ft8 => "FT8",
                    WsjtMode::Ft4 => "FT4",
                    WsjtMode::Jt65 => "JT65",
                    WsjtMode::Jt9 => "JT9",
                    WsjtMode::Wspr => "WSPR",
                    WsjtMode::Msk144 => "MSK144",
                    _ => continue,
                };
                let why = format!(
                    "decoded {} message{}",
                    got.len(),
                    if got.len() == 1 { "" } else { "s" }
                );
                set(name, 0.99, why, got.clone(), Some(Pick::Wsjt(*m)));
            }
        }
        if !self.jtty.1.is_empty() {
            set("JTTY", 0.99, "decoded a message".into(), self.jtty.1.clone(), Some(Pick::Jtty));
        }
        for t in &self.texts {
            for (text, mode) in
                [(&t.text, Some(t.mode)), (&t.rev_text, t.rev.as_ref().map(|r| r.0))]
            {
                let Some(mode) = mode else { continue };
                let r = readable(text);
                if r < 0.25 {
                    continue;
                }
                let rev = matches!(
                    mode,
                    TextMode::Rtty { reverse: true, .. } | TextMode::Navtex { reverse: true }
                );
                let why = format!(
                    "its decoder reads text{}",
                    if rev { ", mark and space reversed" } else { "" }
                );
                let tail: String =
                    text.chars().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect();
                set(
                    t.name,
                    0.5 + 0.48 * r,
                    why,
                    vec![tail.trim().to_string()],
                    Some(Pick::Text(mode)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// A little noise, the same every run.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f32 {
            // Sum of uniforms: near enough to Gaussian.
            let mut s = 0.0;
            for _ in 0..4 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                s += (self.0 >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
            }
            s
        }
    }

    fn bits(n: usize, seed: u64) -> Vec<u32> {
        // splitmix64.
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                ((z ^ (z >> 31)) >> 32) as u32
            })
            .collect()
    }

    /// `secs` of an FSK signal with `m` tones `spacing` apart from `f0`, at
    /// `baud`, in noise `noise` (rms).
    fn fsk(f0: f32, spacing: f32, m: u32, baud: f32, secs: f32, noise: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let sps = RATE as f32 / baud;
        let syms = bits((n as f32 / sps) as usize + 2, 7);
        let mut ph = 0.0f32;
        let mut nz = Noise(99);
        (0..n)
            .map(|i| {
                let k = syms[(i as f32 / sps) as usize] % m;
                ph = (ph + 2.0 * PI * (f0 + k as f32 * spacing) / RATE as f32) % (2.0 * PI);
                0.5 * ph.sin() + noise * nz.next()
            })
            .collect()
    }

    fn bpsk(fc: f32, baud: f32, secs: f32, noise: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let sps = RATE as f32 / baud;
        let b = bits((n as f32 / sps) as usize + 2, 3);
        let mut nz = Noise(5);
        (0..n)
            .map(|i| {
                let k = (i as f32 / sps) as usize;
                let t = (i as f32 / sps).fract();
                let (a, c) = (
                    if b[k] & 1 == 0 { 1.0 } else { -1.0 },
                    if b[k + 1] & 1 == 0 { 1.0 } else { -1.0 },
                );
                // Raised-cosine through each reversal, as PSK31 sends.
                let amp = if a == c { a } else { a * (PI * t).cos() };
                0.5 * amp * (2.0 * PI * fc * i as f32 / RATE as f32).sin() + noise * nz.next()
            })
            .collect()
    }

    fn cw(f: f32, wpm: f32, secs: f32, noise: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let dot = 1.2 / wpm;
        // "CQ DE " over and over, by its elements.
        let pattern = "-.-. --.- -.. .   ";
        let mut on = Vec::new();
        for ch in pattern.chars().cycle() {
            match ch {
                '.' => on.extend([true, false]),
                '-' => on.extend([true, true, true, false]),
                _ => on.extend([false, false]),
            }
            if on.len() as f32 * dot > secs {
                break;
            }
        }
        let mut nz = Noise(11);
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let k = (t / dot) as usize;
                let a = if on.get(k).copied().unwrap_or(false) { 0.5 } else { 0.0 };
                a * (2.0 * PI * f * t).sin() + noise * nz.next()
            })
            .collect()
    }

    fn top(audio: &[f32], lo: f32, hi: f32) -> (Features, Vec<Candidate>) {
        let f = measure(audio, lo, hi).expect("a signal");
        let r = rank(&f);
        (f, r)
    }

    #[test]
    fn rtty_is_two_tones_170_apart_keyed_at_45_baud() {
        let a = fsk(2125.0, 170.0, 2, 45.45, 12.0, 0.05);
        let (f, r) = top(&a, 1800.0, 2600.0);
        assert_eq!(f.tones.len(), 2, "{f:?}");
        assert!((f.spacing_hz.unwrap() - 170.0).abs() < 10.0, "{f:?}");
        let (b, _) = f.baud.expect("a keying rate");
        assert!((b - 45.45).abs() < 1.5, "{f:?}");
        assert_eq!(r[0].name, "RTTY 45", "{:?}", &r[..3]);
    }

    #[test]
    fn psk31_is_one_narrow_signal_keyed_at_31_baud() {
        let a = bpsk(1000.0, 31.25, 12.0, 0.03);
        let (f, r) = top(&a, 700.0, 1300.0);
        let (b, _) = f.baud.expect("a keying rate");
        assert!((b - 31.25).abs() < 1.0, "{f:?}");
        assert!(f.bw_hz < 100.0, "{f:?}");
        assert_eq!(r[0].name, "PSK31", "{r:?} {f:?}");
    }

    #[test]
    fn ft8_is_eight_tones_six_hertz_apart() {
        let a = fsk(1200.0, 6.25, 8, 6.25, 20.0, 0.05);
        let (f, r) = top(&a, 900.0, 1600.0);
        assert!((f.spacing_hz.unwrap() - 6.25).abs() < 0.8, "{f:?}");
        assert!(f.bw_hz > 35.0 && f.bw_hz < 70.0, "{f:?}");
        assert_eq!(r[0].name, "FT8", "{r:?} {f:?}");
    }

    #[test]
    fn cw_is_one_tone_keyed_on_and_off() {
        let a = cw(700.0, 20.0, 12.0, 0.02);
        let (f, r) = top(&a, 400.0, 1000.0);
        assert_eq!(f.envelope, Envelope::OnOff, "{f:?}");
        assert_eq!(r[0].name, "CW", "{r:?} {f:?}");
    }

    #[test]
    fn packet_1200_is_two_tones_a_kilohertz_apart() {
        let a = fsk(1200.0, 1000.0, 2, 1200.0, 8.0, 0.05);
        let (f, r) = top(&a, 300.0, 3000.0);
        assert!(r.iter().take(2).any(|c| c.name == "Packet 1200"), "{r:?} {f:?}");
    }

    #[test]
    fn noise_alone_is_nothing() {
        let mut nz = Noise(1);
        let a: Vec<f32> = (0..(10.0 * RATE) as usize).map(|_| 0.1 * nz.next()).collect();
        assert!(measure(&a, 300.0, 2700.0).is_none());
    }

    #[test]
    fn text_that_reads_scores_high_and_noise_low() {
        assert!(readable("CQ CQ DE K1ABC K1ABC PSE K") > 0.6);
        assert!(readable("UR RST 599 NAME BOB QTH BOSTON") > 0.4);
        assert!(readable("XQJ ZZPT EWQ T TTTE ZXCVQ EIUR ETE") < 0.3);
        assert!(readable("#$%^&*() @! ~~ ``") < 0.1);
    }

    /// `secs` of `sig` as the receiver's 48 kHz audio, in noise, pushed
    /// into an AUTO ID with its passband `pb`, then its report once the
    /// decoders have had a moment.
    fn run(mut sig: crate::demo::KeySignal, secs: f64, pb: (f32, f32)) -> Report {
        let rate = 48_000.0;
        let id = AutoId::new(rate, pb, &[]);
        let block = 4800;
        let mut nz = Noise(77);
        let t0 = 1_792_670_850.0;
        let mut t = 0.0;
        while t < secs {
            let mut z = vec![Complex32::new(0.0, 0.0); block];
            sig.add_iq(&mut z, rate, 0.0, t);
            let a: Vec<f32> = z.iter().map(|z| z.re + 0.003 * nz.next()).collect();
            id.push(&a, t0 + t);
            t += block as f64 / rate;
            // Heard as fast as it is made; give the measuring thread its turn.
            if (t * 10.0).round() as i64 % 20 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(450));
                id.report();
            }
        }
        let mut r = id.report();
        for _ in 0..40 {
            if r.best().is_some_and(|b| !b.decoded.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            r = id.report();
        }
        r
    }

    #[test]
    fn auto_id_reads_psk31_text_and_names_it() {
        let text = "CQ CQ CQ DE K1ABC K1ABC K1ABC PSE K ";
        let sig = crate::demo::KeySignal::psk(1000.0, -20.0, &text.repeat(3), 60.0);
        let r = run(sig, 24.0, (600.0, 1400.0));
        let b = r.best().expect("a candidate");
        assert_eq!(b.name, "PSK31", "{:?}", r.candidates);
        assert!(b.decoded.iter().any(|t| t.contains("K1ABC")), "{b:?}");
        assert!(b.score > 0.8, "{b:?}");
    }

    #[test]
    fn auto_id_reads_rtty_text_and_names_it() {
        let text = "CQ CQ DE W1AW W1AW K ";
        let sig = crate::demo::KeySignal::rtty(2210.0, -20.0, &text.repeat(4), 60.0);
        let r = run(sig, 24.0, (1900.0, 2500.0));
        let b = r.best().expect("a candidate");
        assert_eq!(b.name, "RTTY 45", "{:?}", r.candidates);
        assert!(b.decoded.iter().any(|t| t.contains("W1AW")), "{b:?}");
    }
}
