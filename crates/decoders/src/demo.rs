//! FT8 for the panel's demo band: a few made-up stations calling and
//! answering in their slots on 14.074 MHz, written into the demo I/Q so that
//! DECODE, the skimmer's labels and the offset bar have real signals to work
//! on with no radio (decision 54).
//!
//! The callsigns are the textbook examples (K1ABC, W9XYZ and the like). Each
//! station sends on its own audio frequency, in even or odd slots, starting
//! 0.5 s into the slot as WSJT-X does; its tones are plain 8-FSK at
//! 6.25 baud, phase continuous, which the decoder takes as it takes GFSK.

use std::f64::consts::PI;

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
