//! FT8 for the panel's demo band: a few made-up stations calling and
//! answering in their slots on 14.074 MHz, written into the demo I/Q so that
//! DECODE, the skimmer's labels and the offset bar have real signals to work
//! on with no radio (decision 54).
//!
//! The callsigns are the textbook examples (K1ABC, W9XYZ and the like). Each
//! station sends on its own audio frequency, in even or odd slots, starting
//! 0.5 s into the slot as WSJT-X does; its tones are plain 8-FSK at
//! 6.25 baud, phase continuous, which the decoder takes as it takes GFSK.
//!
//! Beside them, [`DemoKeys`]: made-up CW, PSK31 and RTTY stations on 20 m
//! for the keyboard skimmer (decision 56), each a [`KeySignal`] sending an
//! over and then listening. The skimmer's tests use [`KeySignal`] too.

use std::f64::consts::PI;

use sdr::Complex32;

use crate::keyskim::KeyMode;

use crate::wsjt::WsjtMode;

/// The demo's FT8 dial, Hz.
pub const DEMO_DIAL_HZ: f64 = 14_074_000.0;
/// The demo's FT4 dial, Hz: on 20 m beside FT8, for SKIM WIDE (AJ3G: a
/// spot of the other mode to click with no FT4 on the band).
pub const DEMO_FT4_DIAL_HZ: f64 = 14_080_000.0;

/// One made-up station.
struct Station {
    /// Audio frequency above the dial, Hz.
    audio_hz: f64,
    /// Level of its tone, dBFS.
    level_dbfs: f32,
    /// Sends in odd slots (WSJT-X's "2nd").
    odd: bool,
    /// What it sends, slot after slot in turn: the three words of each.
    messages: &'static [[&'static str; 3]],
    /// Its tones for each message.
    tones: Vec<Vec<u8>>,
    phase: f64,
}

const STATIONS: &[(f64, f32, bool, &[[&str; 3]])] = &[
    (620.0, -76.0, false, &[["CQ", "K1ABC", "FN42"], ["W9XYZ", "K1ABC", "-08"]]),
    (1040.0, -82.0, true, &[["K1ABC", "W9XYZ", "EN37"], ["K1ABC", "W9XYZ", "R-12"]]),
    (1310.0, -71.0, false, &[["CQ", "WA1XYZ", "FN41"], ["CQ", "WA1XYZ", "FN41"]]),
    (1585.0, -86.0, true, &[["CQ", "G4ABC", "IO91"], ["CQ", "G4ABC", "IO91"]]),
    (1820.0, -79.0, false, &[["CQ", "VE3ABC", "FN03"], ["G4ABC", "VE3ABC", "FN03"]]),
    (2210.0, -84.0, true, &[["VE3ABC", "N0XYZ", "EM48"], ["CQ", "N0XYZ", "EM48"]]),
    (2475.0, -74.0, false, &[["CQ", "JA1XYZ", "PM95"], ["CQ", "JA1XYZ", "PM95"]]),
];

/// Where a transmission starts in its slot, s, as WSJT-X starts them.
const START_S: f64 = 0.5;

const FT4_STATIONS: &[(f64, f32, bool, &[[&str; 3]])] = &[
    (850.0, -78.0, false, &[["CQ", "W1XYZ", "FN31"], ["CQ", "W1XYZ", "FN31"]]),
    (1450.0, -82.0, true, &[["CQ", "K5ABC", "EM12"], ["W1XYZ", "K5ABC", "EM12"]]),
    (2100.0, -80.0, false, &[["CQ", "DL1ABC", "JO62"], ["CQ", "DL1ABC", "JO62"]]),
];

/// The demo band's FT8 (or FT4) stations.
pub struct DemoFt8 {
    stations: Vec<Station>,
    mode: WsjtMode,
    dial_hz: f64,
}

impl Default for DemoFt8 {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoFt8 {
    pub fn new() -> Self {
        Self::of(WsjtMode::Ft8, DEMO_DIAL_HZ, STATIONS)
    }

    /// FT4 on [`DEMO_FT4_DIAL_HZ`].
    pub fn ft4() -> Self {
        Self::of(WsjtMode::Ft4, DEMO_FT4_DIAL_HZ, FT4_STATIONS)
    }

    fn of(
        mode: WsjtMode,
        dial_hz: f64,
        list: &[(f64, f32, bool, &'static [[&'static str; 3]])],
    ) -> Self {
        let stations = list
            .iter()
            .map(|&(audio_hz, level_dbfs, odd, messages)| Station {
                audio_hz,
                level_dbfs,
                odd,
                messages,
                tones: messages
                    .iter()
                    .map(|[a, b, c]| {
                        let bits = mfsk_core::msg::wsjt77::pack77(a, b, c)
                            .expect("the demo's messages pack");
                        match mode {
                            WsjtMode::Ft4 => mfsk_core::ft4::encode::message_to_tones(&bits),
                            _ => mfsk_core::ft8::wave_gen::message_to_tones(&bits).to_vec(),
                        }
                    })
                    .collect(),
                phase: 0.0,
            })
            .collect();
        DemoFt8 { stations, mode, dial_hz }
    }

    /// The messages it sends, as the decoder prints them.
    pub fn messages(&self) -> Vec<String> {
        self.stations.iter().flat_map(|s| s.messages.iter().map(|m| m.join(" "))).collect()
    }

    /// Add the stations to `iq` (interleaved, at `rate`, around `centre`
    /// Hz, its first sample at `utc`). Stations outside the span are left
    /// out.
    pub fn add(&mut self, iq: &mut [f32], rate: f64, centre: f64, utc: f64) {
        let period = self.mode.period_s();
        // FT8: 79 symbols of 0.16 s, 6.25 Hz apart; FT4: 103 of 0.048 s,
        // 20.833 Hz apart.
        let (symbols, symbol_s) = match self.mode {
            WsjtMode::Ft4 => (103usize, 0.048),
            _ => (79usize, 0.16),
        };
        let spacing = 1.0 / symbol_s;
        let n = iq.len() / 2;
        for st in &mut self.stations {
            let off = self.dial_hz + st.audio_hz - centre;
            if off.abs() > 0.48 * rate {
                continue;
            }
            let amp = 10f32.powf(st.level_dbfs / 20.0);
            for i in 0..n {
                let t = utc + i as f64 / rate;
                let slot = (t / period).floor();
                if ((slot as i64).rem_euclid(2) == 1) != st.odd {
                    continue;
                }
                let pos = (t - slot * period - START_S) / symbol_s;
                if !(0.0..symbols as f64).contains(&pos) {
                    continue;
                }
                let k = pos as usize;
                let frac = pos - k as f64;
                // The message by the slot's turn among this station's slots.
                let m = ((slot as i64).div_euclid(2) as usize) % st.tones.len();
                let tones = &st.tones[m];
                // The tone eases into the next over the symbol's last
                // quarter, and the whole signal up and down over its first
                // and last, as FT8's Gaussian shaping and ramps do: hard
                // steps splatter, and the decoder reads the splatter.
                let tone = |j: usize| tones[j.min(symbols - 1)] as f64;
                let ease = ((frac - 0.75) / 0.25).clamp(0.0, 1.0);
                let ease = 0.5 - 0.5 * (PI * ease).cos();
                let f = off + spacing * (tone(k) + (tone(k + 1) - tone(k)) * ease);
                let ramp = (pos / 0.25).min((symbols as f64 - pos) / 0.25).min(1.0);
                let ramp = (0.5 - 0.5 * (PI * ramp).cos()) as f32;
                st.phase = (st.phase + 2.0 * PI * f / rate) % (2.0 * PI);
                iq[2 * i] += amp * ramp * st.phase.cos() as f32;
                iq[2 * i + 1] += amp * ramp * st.phase.sin() as f32;
            }
        }
    }
}

/// A made-up station whose transmission is made as audio by its mode's own
/// transmitter (JT9, JTTY), put on the band as its upper sideband: sent
/// `start_s` into every `every_s`, on alternate turns when `alternate`.
pub struct DemoAudio {
    pub name: &'static str,
    /// Its dial on 20 m, Hz; the audio is above it.
    pub dial_hz: f64,
    every_s: f64,
    start_s: f64,
    /// The transmission as analytic audio at [`AUDIO_UP`] Hz.
    z: Vec<Complex32>,
    amp: f32,
}

/// The rate the demo's audio stations are held at.
const AUDIO_UP: f64 = 48_000.0;

impl DemoAudio {
    /// JT9 on 14.078 MHz: a CQ at 1500 Hz in every other minute.
    pub fn jt9() -> Self {
        let a = mfsk_core::jt9::synthesize_standard("CQ", "W3ABC", "FM19", 12_000, 1500.0, 1.0)
            .expect("the demo's JT9 message packs");
        Self::of("JT9", 14_078_000.0, 120.0, 1.0, &a, -76.0)
    }

    /// JTTY on 14.090 MHz: an over at 1500 Hz every 30 s.
    pub fn jtty() -> Self {
        use mfsk_jtty::jtty::pack::{ExchangeProfile, tones};
        let t = tones("CQ CQ DE K2XYZ K2XYZ K", ExchangeProfile::Unknown)
            .expect("the demo's JTTY text packs")
            .expect("the demo's JTTY text has tones");
        let a = mfsk_jtty::jtty::tx::synth_f32(&t, 1500.0, 1.0);
        Self::of("JTTY", 14_090_000.0, 30.0, 2.0, &a, -76.0)
    }

    /// `audio`: real audio at 12 kHz, its signal above 0 Hz as heard in USB.
    fn of(
        name: &'static str,
        dial_hz: f64,
        every_s: f64,
        start_s: f64,
        audio: &[f32],
        dbfs: f32,
    ) -> Self {
        DemoAudio {
            name,
            dial_hz,
            every_s,
            start_s,
            z: analytic_48k(audio),
            amp: 10f32.powf(dbfs / 20.0),
        }
    }

    /// Add it to `iq` (interleaved, at `rate`, around `centre` Hz, its
    /// first sample at `utc`), moved `shift` Hz from 20 m.
    pub fn add(&self, iq: &mut [f32], rate: f64, centre: f64, utc: f64, shift: f64) {
        let off = self.dial_hz + shift - centre;
        if off.abs() > 0.48 * rate - 3000.0 {
            return;
        }
        for (i, p) in iq.chunks_exact_mut(2).enumerate() {
            let t = utc + i as f64 / rate;
            let turn = (t / self.every_s).floor();
            let local = t - turn * self.every_s - self.start_s;
            let k = local * AUDIO_UP;
            if k < 0.0 || k >= (self.z.len() - 1) as f64 {
                continue;
            }
            let (j, f) = (k as usize, (k - k.floor()) as f32);
            let a = self.z[j] * (1.0 - f) + self.z[j + 1] * f;
            // The dial's offset, its phase from the transmission's start.
            let ph = 2.0 * PI * off * local;
            let r = Complex32::new(ph.cos() as f32, ph.sin() as f32);
            let v = a * r * self.amp;
            p[0] += v.re;
            p[1] += v.im;
        }
    }
}

/// Real 12 kHz audio as its analytic signal (positive frequencies only) at
/// [`AUDIO_UP`], by the FFT: what an upper sideband of it is about its dial.
fn analytic_48k(audio: &[f32]) -> Vec<Complex32> {
    let n = audio.len().next_power_of_two();
    let up = (AUDIO_UP / 12_000.0) as usize;
    let mut planner = rustfft::FftPlanner::<f32>::new();
    let mut x: Vec<Complex32> = audio.iter().map(|&v| Complex32::new(v, 0.0)).collect();
    x.resize(n, Complex32::default());
    planner.plan_fft_forward(n).process(&mut x);
    let mut y = vec![Complex32::default(); n * up];
    for k in 1..n / 2 {
        y[k] = x[k] * 2.0;
    }
    planner.plan_fft_inverse(n * up).process(&mut y);
    let s = 1.0 / n as f32;
    y.truncate(audio.len() * up);
    y.iter_mut().for_each(|v| *v *= s);
    y
}

/// One made-up keyboard-mode station: CW, PSK31 or RTTY sending `text` in
/// overs, `period_s` apart, `skew_s` into the period. Its modulation is a
/// function of the time, so it can be written into blocks of any length;
/// only its carrier's phase is carried from one to the next.
pub struct KeySignal {
    pub mode: KeyMode,
    /// Its frequency, Hz (RTTY's mark/space centre).
    pub hz: f64,
    amp: f32,
    plan: Plan,
    /// How long an over lasts, and how often one starts, s.
    on_s: f64,
    period_s: f64,
    skew_s: f64,
    phase: f64,
}

enum Plan {
    /// Key-down stretches, s from the over's start.
    Cw(Vec<(f64, f64)>),
    /// Each symbol's bit (0 is a phase reversal), and the carrier's sign
    /// going into it.
    Psk(Vec<u8>, Vec<f64>),
    /// Mark or space, half a bit at a time.
    Rtty(Vec<bool>),
}

/// CW's keying edges (raised cosine), s.
const CW_EDGE_S: f64 = 0.005;
/// PSK31's and RTTY's idle before and after the text, s.
const LEAD_S: f64 = 1.0;
const TAIL_S: f64 = 0.4;

impl KeySignal {
    /// CW at `wpm`.
    pub fn cw(hz: f64, level_dbfs: f32, wpm: f32, text: &str, period_s: f64) -> Self {
        let dit = 1.2 / wpm as f64;
        let mut keys = Vec::new();
        let mut t = 0.0;
        for ch in text.chars() {
            if ch == ' ' {
                t += 4.0 * dit; // 7 with the 3 after the last character
                continue;
            }
            let Some(code) = crate::morse::morse_encode(ch) else { continue };
            for (i, e) in code.chars().enumerate() {
                if i > 0 {
                    t += dit;
                }
                let len = if e == '-' { 3.0 * dit } else { dit };
                keys.push((t, t + len));
                t += len;
            }
            t += 3.0 * dit;
        }
        Self::new(KeyMode::Cw, hz, level_dbfs, Plan::Cw(keys), t + CW_EDGE_S, period_s)
    }

    /// PSK31 (BPSK, 31.25 baud, Varicode), after a second of idle.
    pub fn psk(hz: f64, level_dbfs: f32, text: &str, period_s: f64) -> Self {
        let mut bits = vec![0u8; (LEAD_S * 31.25) as usize];
        for b in text.bytes() {
            bits.extend(crate::modem::psk::varicode(b).bytes().map(|c| c - b'0'));
            bits.extend([0, 0]);
        }
        bits.extend(std::iter::repeat_n(0, (TAIL_S * 31.25) as usize));
        let on = bits.len() as f64 / 31.25;
        let mut sign = 1.0;
        let signs = bits
            .iter()
            .map(|&b| {
                let s = sign;
                if b == 0 {
                    sign = -sign;
                }
                s
            })
            .collect();
        Self::new(KeyMode::Psk, hz, level_dbfs, Plan::Psk(bits, signs), on, period_s)
    }

    /// RTTY (45.45 baud, 170 Hz shift, mark the upper tone), after a second
    /// of mark.
    pub fn rtty(hz: f64, level_dbfs: f32, text: &str, period_s: f64) -> Self {
        let halves = |s: f64| (s * 2.0 * 45.45) as usize;
        let mut h = vec![true; halves(LEAD_S)];
        for v in crate::modem::rtty::baudot_codes(text) {
            h.extend([false, false]);
            for i in 0..5 {
                let m = (v >> i) & 1 == 1;
                h.extend([m, m]);
            }
            h.extend([true, true, true]);
        }
        h.extend(std::iter::repeat_n(true, halves(TAIL_S)));
        let on = h.len() as f64 / (2.0 * 45.45);
        Self::new(KeyMode::Rtty, hz, level_dbfs, Plan::Rtty(h), on, period_s)
    }

    fn new(mode: KeyMode, hz: f64, level_dbfs: f32, plan: Plan, on_s: f64, period_s: f64) -> Self {
        KeySignal {
            mode,
            hz,
            amp: 10f32.powf(level_dbfs / 20.0),
            plan,
            on_s,
            period_s: period_s.max(on_s),
            skew_s: 0.0,
            phase: 0.0,
        }
    }

    /// Start its overs `s` into the period.
    pub fn skewed(mut self, s: f64) -> Self {
        self.skew_s = s;
        self
    }

    /// One over, s.
    pub fn over_s(&self) -> f64 {
        self.on_s
    }

    /// Its envelope (real, signed for PSK31) and its frequency's offset from
    /// `hz`, `tau` s into an over.
    fn at(&self, tau: f64) -> (f32, f64) {
        if !(0.0..self.on_s).contains(&tau) {
            return (0.0, 0.0);
        }
        let ease = |x: f64| 0.5 - 0.5 * (PI * x.clamp(0.0, 1.0)).cos();
        match &self.plan {
            Plan::Cw(keys) => {
                // The 50 % points of the edges are on the key's.
                let i = keys.partition_point(|&(a, _)| a - 0.5 * CW_EDGE_S <= tau);
                let env = keys[i.saturating_sub(1)..i]
                    .iter()
                    .map(|&(a, b)| {
                        ease((tau - a) / CW_EDGE_S + 0.5) * ease((b - tau) / CW_EDGE_S + 0.5)
                    })
                    .fold(0.0, f64::max);
                (env as f32, 0.0)
            }
            Plan::Psk(bits, signs) => {
                let u = tau * 31.25;
                let k = (u as usize).min(bits.len() - 1);
                let frac = u - k as f64;
                let s = signs[k];
                let mut v = if bits[k] == 0 { s * (PI * frac).cos() } else { s };
                if k == 0 {
                    v *= ease(frac);
                } else if k + 1 == bits.len() {
                    v *= ease(1.0 - frac);
                }
                (v as f32, 0.0)
            }
            Plan::Rtty(h) => {
                let i = ((tau * 2.0 * 45.45) as usize).min(h.len() - 1);
                let env = ease(tau / CW_EDGE_S) * ease((self.on_s - tau) / CW_EDGE_S);
                let f = if h[i] { 0.5 * 170.0 } else { -0.5 * 170.0 };
                (env as f32, f)
            }
        }
    }

    /// Add it to complex I/Q at `rate` around `centre`, its first sample
    /// at `t0` s (any clock: UTC for the demo).
    pub fn add_iq(&mut self, iq: &mut [Complex32], rate: f64, centre: f64, t0: f64) {
        let off = self.hz - centre;
        if off.abs() > 0.48 * rate {
            return;
        }
        for (i, z) in iq.iter_mut().enumerate() {
            let t = t0 + i as f64 / rate;
            let tau = (t - self.skew_s).rem_euclid(self.period_s);
            let (env, df) = self.at(tau);
            self.phase = (self.phase + 2.0 * PI * (off + df) / rate) % (2.0 * PI);
            if env != 0.0 {
                let a = self.amp * env;
                *z += Complex32::new(a * self.phase.cos() as f32, a * self.phase.sin() as f32);
            }
        }
    }

    /// As [`KeySignal::add_iq`], on interleaved I/Q.
    pub fn add(&mut self, iq: &mut [f32], rate: f64, centre: f64, utc: f64) {
        let mut z: Vec<Complex32> =
            iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])).collect();
        self.add_iq(&mut z, rate, centre, utc);
        for (p, z) in iq.chunks_exact_mut(2).zip(z) {
            p[0] = z.re;
            p[1] = z.im;
        }
    }
}

/// The demo band's keyboard-mode stations, on 20 m: three in CW, two in
/// PSK31 and one in RTTY, with textbook calls, sending and listening in
/// turn so that the skimmer's labels come and fade.
pub struct DemoKeys {
    pub stations: Vec<KeySignal>,
}

impl Default for DemoKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoKeys {
    pub fn new() -> Self {
        DemoKeys {
            stations: vec![
                KeySignal::cw(14_025_500.0, -78.0, 22.0, "CQ CQ DE W2XYZ W2XYZ K", 20.0),
                KeySignal::cw(14_031_200.0, -84.0, 28.0, "CQ TEST K4ABC K4ABC TEST", 14.0)
                    .skewed(5.0),
                KeySignal::cw(14_041_800.0, -80.0, 18.0, "N1ABC DE K9XYZ 5NN TU", 24.0).skewed(9.0),
                KeySignal::psk(14_071_000.0, -80.0, "CQ CQ CQ DE KB1XYZ KB1XYZ PSE K ", 18.0),
                KeySignal::psk(14_071_600.0, -84.0, "VE3XYZ DE W5ABC TNX FER QSO 73 ", 22.0)
                    .skewed(6.0),
                KeySignal::rtty(14_085_500.0, -78.0, "CQ CQ CQ DE N2ABC N2ABC K ", 16.0)
                    .skewed(3.0),
            ],
        }
    }

    /// Add them to `iq` (interleaved, at `rate`, around `centre` Hz, its
    /// first sample at `utc`).
    pub fn add(&mut self, iq: &mut [f32], rate: f64, centre: f64, utc: f64) {
        self.add_moved(iq, rate, centre, utc, |_| Some(0.0));
    }

    /// The same, each station moved by `shift(mode)` Hz from its place on
    /// 20 m (to another band's), or left out where that is `None`.
    pub fn add_moved(
        &mut self,
        iq: &mut [f32],
        rate: f64,
        centre: f64,
        utc: f64,
        shift: impl Fn(KeyMode) -> Option<f64>,
    ) {
        let mut z: Vec<Complex32> =
            iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])).collect();
        for s in &mut self.stations {
            if let Some(d) = shift(s.mode) {
                s.add_iq(&mut z, rate, centre - d, utc);
            }
        }
        for (p, z) in iq.chunks_exact_mut(2).zip(z) {
            p[0] = z.re;
            p[1] = z.im;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_stations_are_on_air_in_their_own_slots_only() {
        let mut d = DemoFt8::new();
        assert!(d.messages().contains(&"CQ K1ABC FN42".to_string()));
        let rate = 48_000.0;
        let power = |d: &mut DemoFt8, utc: f64| {
            let mut iq = vec![0.0f32; 4800 * 2];
            d.add(&mut iq, rate, DEMO_DIAL_HZ, utc);
            iq.iter().map(|v| v * v).sum::<f32>()
        };
        // An even slot, 2 s in: the even stations only. 14 s in: nothing.
        let even = 1_700_000_010.0 - 1_700_000_010.0 % 30.0;
        let p_even = power(&mut d, even + 2.0);
        let p_odd = power(&mut d, even + 15.0 + 2.0);
        assert!(p_even > 0.0 && p_odd > 0.0);
        assert!(p_even != p_odd);
        assert_eq!(power(&mut d, even + 14.0), 0.0);
        // A span that does not reach 14.074: nothing.
        let mut iq = vec![0.0f32; 4800 * 2];
        d.add(&mut iq, rate, 14_200_000.0, even + 2.0);
        assert!(iq.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn the_decoder_hears_the_demo_stations_and_nothing_else() {
        // An even slot of the demo band, with noise, through the skimmer's
        // lane and a decoder as the panel's would hear it.
        let rate = 48_000.0;
        let centre = DEMO_DIAL_HZ + 10_000.0;
        let mut d = DemoFt8::new();
        let sk = crate::skim::WideSkimmer::spawn(WsjtMode::Ft8, &[DEMO_DIAL_HZ], &[]);
        let slot = 1_700_000_040.0; // even: 1_700_000_040 / 15 is even
        assert_eq!((slot / 15.0) as i64 % 2, 0);
        let start = slot - 1.0;
        let mut rng = 7u32;
        for b in 0..(17 * 10) {
            let utc = start + b as f64 * 0.1;
            let mut iq: Vec<f32> = (0..4800 * 2)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    (rng as f32 / u32::MAX as f32 - 0.5) * 2e-3
                })
                .collect();
            d.add(&mut iq, rate, centre, utc);
            sk.push_iq(&iq, rate, centre, utc);
        }
        let t = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t.elapsed().as_secs() < 90 {
            got.extend(sk.results().into_iter().filter(|s| !s.slot.decodes.is_empty()));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut heard: Vec<String> =
            got.iter().flat_map(|s| s.slot.decodes.iter().map(|d| d.message.clone())).collect();
        heard.sort();
        // The even stations' first messages (this slot is the first of
        // their turn: 1_700_000_040 / 30 is even).
        let mut want: Vec<String> = STATIONS
            .iter()
            .filter(|s| !s.2)
            .map(|s| s.3[((slot / 15.0) as i64 / 2) as usize % s.3.len()].join(" "))
            .collect();
        want.sort();
        assert_eq!(heard, want);
    }

    #[test]
    fn the_demo_ft4_stations_on_14_080_are_decoded() {
        // An even FT4 slot (7.5 s), through SKIM WIDE's lane and decoder.
        let rate = 48_000.0;
        let centre = DEMO_DIAL_HZ + 10_000.0;
        let mut d = DemoFt8::ft4();
        let sk = crate::skim::WideSkimmer::spawn(WsjtMode::Ft4, &[DEMO_FT4_DIAL_HZ], &[]);
        let slot = 1_700_000_040.0;
        assert_eq!((slot / 7.5) as i64 % 2, 0);
        let start = slot - 1.0;
        let mut rng = 7u32;
        for b in 0..(10 * 10) {
            let utc = start + b as f64 * 0.1;
            let mut iq: Vec<f32> = (0..4800 * 2)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    (rng as f32 / u32::MAX as f32 - 0.5) * 2e-3
                })
                .collect();
            d.add(&mut iq, rate, centre, utc);
            sk.push_iq(&iq, rate, centre, utc);
        }
        let t = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t.elapsed().as_secs() < 90 {
            got.extend(sk.results().into_iter().filter(|s| !s.slot.decodes.is_empty()));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut heard: Vec<String> =
            got.iter().flat_map(|s| s.slot.decodes.iter().map(|d| d.message.clone())).collect();
        heard.sort();
        let turn = ((slot / 7.5) as i64 / 2) as usize;
        let mut want: Vec<String> =
            FT4_STATIONS.iter().filter(|s| !s.2).map(|s| s.3[turn % s.3.len()].join(" ")).collect();
        want.sort();
        assert_eq!(heard, want);
    }

    /// `st` through a USB lane on its dial and `decode`, as the panel's
    /// receiver would hear it: 48 kHz I/Q with noise, 10 kHz off the dial,
    /// `secs` from `start`.
    fn lane_audio(st: &DemoAudio, start: f64, secs: f64, mut sink: impl FnMut(&[f32], f64)) {
        let rate = 48_000.0;
        let centre = st.dial_hz - 10_000.0;
        let mut lane = crate::skim::SsbLane::new(rate, st.dial_hz - centre);
        let mut rng = 11u32;
        let mut audio = Vec::new();
        for b in 0..(secs * 10.0) as usize {
            let utc = start + b as f64 * 0.1;
            let mut iq: Vec<f32> = (0..4800 * 2)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    (rng as f32 / u32::MAX as f32 - 0.5) * 2e-3
                })
                .collect();
            st.add(&mut iq, rate, centre, utc, 0.0);
            let z: Vec<Complex32> =
                iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])).collect();
            audio.clear();
            lane.process(&z, &mut audio);
            sink(&audio, utc);
        }
    }

    #[test]
    fn the_demo_jt9_station_is_decoded() {
        let st = DemoAudio::jt9();
        let mut cfg = crate::worker::WsjtConfig::new(WsjtMode::Jt9);
        cfg.input_rate = crate::wsjt::RATE;
        let d = crate::worker::WsjtDecoder::spawn(cfg);
        // An even minute: it sends.
        let slot = 1_700_000_040.0 - 1_700_000_040.0 % 120.0;
        lane_audio(&st, slot - 1.0, 63.0, |a, utc| d.push(a, utc));
        let t = std::time::Instant::now();
        let mut heard = Vec::new();
        while heard.is_empty() && t.elapsed().as_secs() < 90 {
            for s in d.results().try_iter() {
                heard.extend(s.decodes.iter().map(|x| x.message.clone()));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(heard, ["CQ W3ABC FM19"]);
    }

    #[test]
    fn the_demo_jtty_station_is_read() {
        let st = DemoAudio::jtty();
        let d = crate::worker::JttyDecoder::spawn(crate::wsjt::RATE, 1500.0);
        let start = 1_700_000_010.0 - 1_700_000_010.0 % st.every_s;
        lane_audio(&st, start - 1.0, st.every_s + 4.0, |a, utc| d.push(a, utc));
        let t = std::time::Instant::now();
        let (mut text, mut done) = (String::new(), false);
        while !done && t.elapsed().as_secs() < 60 {
            for e in d.events().try_iter() {
                text = e.text.clone();
                done |= e.complete;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(text.contains("CQ CQ DE K2XYZ K2XYZ"), "{text:?}");
    }
}
