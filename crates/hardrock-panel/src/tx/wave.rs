//! The audio of one over, at the sound output's own sample rate: FT8's 79
//! GFSK symbols (WSJT-X's `gen_ft8wave`: BT 2, 0.16 s symbols, 6.25 Hz
//! apart, a raised-cosine ramp at each end) or WSPR's 162 four-tone
//! symbols, from mfsk-core's encoders.

use mfsk_core::engine::dsp::gfsk::{self, GfskCfg};

/// FT8's symbol time, s.
const FT8_SYMBOL_S: f64 = 0.16;
/// How far into its 15 s slot an FT8 signal starts, as WSJT-X sends it.
pub(crate) const FT8_START_S: f64 = 0.5;
/// How far into its 2 min slot a WSPR beacon starts.
pub(crate) const WSPR_START_S: f64 = 1.0;
/// An FT8 over on the air: 79 symbols.
pub(crate) const FT8_LEN_S: f64 = 79.0 * FT8_SYMBOL_S;
/// A WSPR beacon on the air: 162 symbols of 8192/12000 s.
pub(crate) const WSPR_LEN_S: f64 = 162.0 * 8192.0 / 12000.0;

/// What an over sends, ready to be rendered at the output's rate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Message {
    /// FT8 tones (79, 0..=7) at an audio offset, Hz (tone 0).
    Ft8 { tones: Vec<u8>, audio_hz: f32 },
    /// WSPR symbols (162, 0..=3) at an audio offset, Hz (tone 0).
    Wspr { symbols: Vec<u8>, audio_hz: f32 },
}

impl Message {
    /// An FT8 message from its text, or `None` if it can't be packed.
    pub(crate) fn ft8(text: &str, audio_hz: f32) -> Option<Message> {
        let bits = super::msg::pack(text)?;
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits).to_vec();
        Some(Message::Ft8 { tones, audio_hz })
    }

    /// A WSPR Type 1 beacon: call, 4-character grid and power, dBm.
    pub(crate) fn wspr(call: &str, grid: &str, dbm: i32, audio_hz: f32) -> Option<Message> {
        let grid: String = grid.chars().take(4).collect::<String>().to_ascii_uppercase();
        let info = mfsk_core::msg::wspr::pack_type1(&call.to_ascii_uppercase(), &grid, dbm)?;
        let symbols = mfsk_core::wspr::encode_channel_symbols(&info).to_vec();
        Some(Message::Wspr { symbols, audio_hz })
    }

    /// How long it is on the air, s.
    #[cfg(test)]
    pub(crate) fn len_s(&self) -> f64 {
        match self {
            Message::Ft8 { .. } => FT8_LEN_S,
            Message::Wspr { .. } => WSPR_LEN_S,
        }
    }

    /// The samples at `rate`, full scale 1.
    pub(crate) fn render(&self, rate: u32) -> Vec<f32> {
        match self {
            Message::Ft8 { tones, audio_hz } => {
                let nsps = (rate as f64 * FT8_SYMBOL_S).round() as usize;
                let cfg = GfskCfg {
                    sample_rate: rate as f32,
                    samples_per_symbol: nsps,
                    bt: 2.0,
                    hmod: 1.0,
                    ramp_samples: nsps / 8,
                };
                gfsk::synth_f32(tones, *audio_hz, 1.0, &cfg)
            }
            Message::Wspr { symbols, audio_hz } => {
                let mut s = [0u8; 162];
                for (o, i) in s.iter_mut().zip(symbols) {
                    *o = *i;
                }
                mfsk_core::wspr::synthesize_audio(&s, rate, *audio_hz, 1.0)
            }
        }
    }
}

/// The WSPR power levels the message can carry, dBm.
pub(crate) const WSPR_DBM: [i32; 19] =
    [0, 3, 7, 10, 13, 17, 20, 23, 27, 30, 33, 37, 40, 43, 47, 50, 53, 57, 60];

/// A WSPR level as watts for the operator: "5 W", "200 mW".
pub(crate) fn dbm_label(dbm: i32) -> String {
    let w = 10f64.powf(dbm as f64 / 10.0) / 1000.0;
    if w >= 1.0 { format!("{} W", round_125(w)) } else { format!("{} mW", round_125(w * 1000.0)) }
}

fn round_125(v: f64) -> String {
    let r = if v >= 10.0 { v.round() } else { (v * 10.0).round() / 10.0 };
    let s = format!("{r}");
    s.trim_end_matches(".0").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ft8_over_is_79_symbols_at_the_outputs_rate() {
        let m = Message::ft8("CQ AJ3G FN20", 1500.0).expect("packs");
        let a = m.render(48_000);
        // 79 symbols of 7 680 samples, plus GFSK's ramp symbols either side.
        assert!(a.len() >= 79 * 7_680, "{}", a.len());
        assert!(a.iter().all(|v| v.abs() <= 1.0001));
        // Its strongest frequency is in the 50 Hz above 1500 Hz.
        let (mut best, mut bf) = (0.0, 0.0);
        for f in (1400..1650).step_by(5) {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, v) in a.iter().enumerate().step_by(3) {
                let ph = std::f64::consts::TAU * f as f64 * i as f64 / 48_000.0;
                re += *v as f64 * ph.cos();
                im += *v as f64 * ph.sin();
            }
            let p = re * re + im * im;
            if p > best {
                best = p;
                bf = f as f64;
            }
        }
        assert!((1500.0..=1550.0).contains(&bf), "{bf}");
    }

    #[test]
    fn a_wspr_beacon_is_110_6_s() {
        let m = Message::wspr("AJ3G", "FN20xx", 37, 1500.0).expect("packs");
        let a = m.render(12_000);
        assert_eq!(a.len(), 162 * 8192);
        assert!((m.len_s() - 110.592).abs() < 0.01);
        assert!(Message::wspr("PJ4/K1ABC", "FK52", 37, 1500.0).is_none());
        assert!(Message::wspr("AJ3G", "FN20", 36, 1500.0).is_none());
        assert_eq!(dbm_label(37), "5 W");
        assert_eq!(dbm_label(23), "200 mW");
        assert_eq!(dbm_label(60), "1000 W");
    }
}
