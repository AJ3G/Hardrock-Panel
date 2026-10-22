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
    tones: Vec<[u8; 79]>,
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

/// FT8's symbol length, s; its 79 symbols, and where they start in a slot.
const SYMBOL_S: f64 = 0.16;
const START_S: f64 = 0.5;

/// The demo band's FT8 stations.
pub struct DemoFt8 {
    stations: Vec<Station>,
}

impl Default for DemoFt8 {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoFt8 {
    pub fn new() -> Self {
        let stations = STATIONS
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
                        mfsk_core::ft8::wave_gen::message_to_tones(&bits)
                    })
                    .collect(),
                phase: 0.0,
            })
            .collect();
        DemoFt8 { stations }
    }

    /// The messages it sends, as the decoder prints them.
    pub fn messages(&self) -> Vec<String> {
        self.stations.iter().flat_map(|s| s.messages.iter().map(|m| m.join(" "))).collect()
    }

    /// Add the stations to `iq` (interleaved, at `rate`, around `centre`
    /// Hz, its first sample at `utc`). Stations outside the span are left
    /// out.
    pub fn add(&mut self, iq: &mut [f32], rate: f64, centre: f64, utc: f64) {
        let period = WsjtMode::Ft8.period_s();
        let n = iq.len() / 2;
        for st in &mut self.stations {
            let off = DEMO_DIAL_HZ + st.audio_hz - centre;
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
                let pos = (t - slot * period - START_S) / SYMBOL_S;
                if !(0.0..79.0).contains(&pos) {
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
                let tone = |j: usize| tones[j.min(78)] as f64;
                let ease = ((frac - 0.75) / 0.25).clamp(0.0, 1.0);
                let ease = 0.5 - 0.5 * (PI * ease).cos();
                let f = off + 6.25 * (tone(k) + (tone(k + 1) - tone(k)) * ease);
                let ramp = (pos / 0.25).min((79.0 - pos) / 0.25).min(1.0);
                let ramp = (0.5 - 0.5 * (PI * ramp).cos()) as f32;
                st.phase = (st.phase + 2.0 * PI * f / rate) % (2.0 * PI);
                iq[2 * i] += amp * ramp * st.phase.cos() as f32;
                iq[2 * i + 1] += amp * ramp * st.phase.sin() as f32;
            }
        }
    }
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
        let mut z: Vec<Complex32> =
            iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])).collect();
        for s in &mut self.stations {
            s.add_iq(&mut z, rate, centre, utc);
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
}
