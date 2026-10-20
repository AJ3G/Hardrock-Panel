// The calls into mfsk-core follow SDRoxide's crates/sdroxide-digi/src/modem.rs
// (https://github.com/dividebysandwich/sdroxide), GPL-3.0-or-later.

//! The WSJT modes: FT8, FT4, WSPR, JT65, JT9, FST4 and Q65, decoded by the
//! `mfsk-core` crate from one whole UTC slot of 12 kHz audio.
//!
//! [`decode_slot`] is one slot, synchronously; [`WsjtSession`] keeps what a
//! run of slots learns (the callsigns heard, which resolve the hashed ones in
//! later messages). [`crate::worker::WsjtDecoder`] runs it on a thread of its
//! own, fed time-stamped audio.
//!
//! The audio must start exactly on the slot boundary: each mode's DT is
//! reported from where WSJT-X puts it (FT8 0.5 s into the slot, the others
//! 1 s, FST4-15 and Q65-15/30 0.5 s), so an on-time station reads about 0.

use mfsk_core::msg::decode_request::DecodeRequest;
use mfsk_core::msg::hash_table::CallsignHashTable;
use mfsk_core::msg::wsjt77;

/// The rate every WSJT mode is decoded at, Hz.
pub const RATE: f64 = 12_000.0;
const RATE_U32: u32 = 12_000;

/// FST4's transmit/receive period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fst4Period {
    P15,
    P30,
    P60,
    P120,
    P300,
}

/// Q65's sub-mode: its period and tone spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Q65Mode {
    A15,
    A30,
    A60,
    B60,
    C60,
    D60,
    E60,
    D120,
    E120,
    A300,
}

/// A WSJT mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WsjtMode {
    Ft8,
    Ft4,
    Wspr,
    Jt65,
    Jt9,
    Fst4(Fst4Period),
    Q65(Q65Mode),
}

impl WsjtMode {
    /// The slot length in seconds.
    pub fn period_s(self) -> f64 {
        match self {
            WsjtMode::Ft8 => 15.0,
            WsjtMode::Ft4 => 7.5,
            WsjtMode::Wspr => 120.0,
            WsjtMode::Jt65 | WsjtMode::Jt9 => 60.0,
            WsjtMode::Fst4(p) => match p {
                Fst4Period::P15 => 15.0,
                Fst4Period::P30 => 30.0,
                Fst4Period::P60 => 60.0,
                Fst4Period::P120 => 120.0,
                Fst4Period::P300 => 300.0,
            },
            WsjtMode::Q65(m) => match m {
                Q65Mode::A15 => 15.0,
                Q65Mode::A30 => 30.0,
                Q65Mode::A60 | Q65Mode::B60 | Q65Mode::C60 | Q65Mode::D60 | Q65Mode::E60 => 60.0,
                Q65Mode::D120 | Q65Mode::E120 => 120.0,
                Q65Mode::A300 => 300.0,
            },
        }
    }

    /// Where a station on time starts sending, seconds into the slot.
    pub fn start_delay_s(self) -> f64 {
        match self {
            WsjtMode::Ft8 | WsjtMode::Ft4 => 0.5,
            WsjtMode::Fst4(Fst4Period::P15) => 0.5,
            WsjtMode::Q65(Q65Mode::A15 | Q65Mode::A30) => 0.5,
            _ => 1.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WsjtMode::Ft8 => "FT8",
            WsjtMode::Ft4 => "FT4",
            WsjtMode::Wspr => "WSPR",
            WsjtMode::Jt65 => "JT65",
            WsjtMode::Jt9 => "JT9",
            WsjtMode::Fst4(_) => "FST4",
            WsjtMode::Q65(_) => "Q65",
        }
    }

    /// The mode code WSJT-X puts in its UDP Decode message.
    pub fn wsjtx_code(self) -> &'static str {
        match self {
            WsjtMode::Ft8 => "~",
            WsjtMode::Ft4 => "+",
            WsjtMode::Jt65 => "#",
            WsjtMode::Jt9 => "@",
            WsjtMode::Q65(_) => ":",
            WsjtMode::Fst4(_) => "`",
            WsjtMode::Wspr => "WSPR",
        }
    }
}

/// A WSPR spot's fields.
#[derive(Debug, Clone, PartialEq)]
pub struct WsprSpot {
    pub callsign: String,
    /// Empty for a compound callsign (WSPR Type 2), which carries no grid.
    pub grid: String,
    pub power_dbm: i32,
    pub drift_hz: f32,
}

/// One decoded message.
#[derive(Debug, Clone, PartialEq)]
pub struct WsjtDecode {
    pub mode: WsjtMode,
    /// UTC of the slot's start, Unix seconds.
    pub slot_utc: i64,
    pub snr_db: f32,
    /// Time offset from where an on-time station starts, seconds.
    pub dt_s: f32,
    /// Audio frequency of the signal (tone 0 for the JT modes and WSPR), Hz.
    pub audio_hz: f32,
    pub message: String,
    /// WSPR's fields, for WSPR only.
    pub wspr: Option<WsprSpot>,
}

/// What a run of slots learns: the callsigns heard, for the hashed forms
/// later messages use.
#[derive(Default)]
pub struct WsjtSession {
    hashes: CallsignHashTable,
    wspr_calls: mfsk_core::wspr::decode::WsprCallsignTable,
}

impl WsjtSession {
    pub fn new() -> Self {
        WsjtSession::default()
    }

    /// Register callsigns already known (the operator's own), so the first
    /// hashed message naming them resolves.
    pub fn seed(&mut self, calls: &[&str]) {
        for c in calls.iter().filter(|c| !c.is_empty()) {
            self.hashes.insert(c);
        }
    }
}

/// The audio band searched, Hz.
const AUDIO_MIN_HZ: f32 = 100.0;
const AUDIO_MAX_HZ: f32 = 3_000.0;
/// FT8/FT4 coarse-sync floor and candidate cap, as SDRoxide measured them.
const SYNC_MIN: f32 = 1.5;
const MAX_CAND: usize = 600;

/// 12 kHz audio as the i16 the FT8-family decoders take. The panel's audio
/// is around ±1; the scale leaves headroom for a strong signal.
fn to_i16(audio: &[f32]) -> Vec<i16> {
    audio.iter().map(|&x| (x * 20_000.0).clamp(-32_767.0, 32_767.0) as i16).collect()
}

/// Decode one slot of 12 kHz mono audio starting on the slot boundary at
/// `slot_utc` (Unix seconds). Results are de-duplicated and sorted by audio
/// frequency.
pub fn decode_slot(
    mode: WsjtMode,
    audio: &[f32],
    slot_utc: i64,
    session: &mut WsjtSession,
) -> Vec<WsjtDecode> {
    let mk = |snr_db: f32, dt_s: f32, audio_hz: f32, message: String| WsjtDecode {
        mode,
        slot_utc,
        snr_db,
        dt_s,
        audio_hz,
        message,
        wspr: None,
    };
    let mut out: Vec<WsjtDecode> = match mode {
        WsjtMode::Ft8 => {
            let pcm = to_i16(audio);
            let ht = &session.hashes;
            DecodeRequest::<mfsk_core::Ft8>::new(&pcm, AUDIO_MIN_HZ, AUDIO_MAX_HZ, SYNC_MIN, MAX_CAND)
                .osd(true)
                .sic_rounds(2)
                .decode()
                .results
                .into_iter()
                .filter_map(|r| {
                    let text = wsjt77::unpack77_with_hash(r.message77(), ht)?;
                    Some(mk(r.snr_db, r.dt_sec, r.freq_hz, text))
                })
                .collect()
        }
        WsjtMode::Ft4 => {
            let pcm = to_i16(audio);
            let ht = &session.hashes;
            DecodeRequest::<mfsk_core::Ft4>::new(&pcm, AUDIO_MIN_HZ, AUDIO_MAX_HZ, SYNC_MIN, MAX_CAND)
                .osd(true)
                .sic_rounds(2)
                .decode()
                .results
                .into_iter()
                .filter_map(|r| {
                    let text = wsjt77::unpack77_with_hash(r.message77(), ht)?;
                    Some(mk(r.snr_db, r.dt_sec, r.freq_hz, text))
                })
                .collect()
        }
        WsjtMode::Fst4(p) => {
            let pcm = to_i16(audio);
            let ht = &session.hashes;
            // A deep, sparse mode: a lower sync floor and few candidates.
            macro_rules! run {
                ($p:ty) => {
                    DecodeRequest::<$p>::new(&pcm, AUDIO_MIN_HZ, AUDIO_MAX_HZ, 0.8, 30)
                        .decode()
                        .results
                        .into_iter()
                        .filter_map(|r| {
                            let text = wsjt77::unpack77_with_hash(r.message77(), ht)?;
                            Some(mk(r.snr_db, r.dt_sec, r.freq_hz, text))
                        })
                        .collect()
                };
            }
            match p {
                Fst4Period::P15 => run!(mfsk_core::fst4::Fst4s15),
                Fst4Period::P30 => run!(mfsk_core::fst4::Fst4s30),
                Fst4Period::P60 => run!(mfsk_core::fst4::Fst4s60),
                Fst4Period::P120 => run!(mfsk_core::fst4::Fst4s120),
                Fst4Period::P300 => run!(mfsk_core::fst4::Fst4s300),
            }
        }
        WsjtMode::Q65(m) => {
            use mfsk_core::q65::DecodeRequest as Q65Request;
            use mfsk_core::q65::search::SearchParams;
            let nominal_s = mode.start_delay_s() as f32;
            let nominal = (mode.start_delay_s() * RATE).round() as usize;
            macro_rules! run {
                ($p:ty) => {
                    Q65Request::<$p>::new(audio, RATE_U32, nominal, SearchParams::default())
                        .decode()
                        .into_iter()
                        .map(|r| mk(r.snr_db, r.dt_sec - nominal_s, r.freq_hz, r.message))
                        .collect()
                };
            }
            use mfsk_core::q65 as q;
            match m {
                Q65Mode::A15 => run!(q::Q65a15),
                Q65Mode::A30 => run!(q::Q65a30),
                Q65Mode::A60 => run!(q::Q65a60),
                Q65Mode::B60 => run!(q::Q65b60),
                Q65Mode::C60 => run!(q::Q65c60),
                Q65Mode::D60 => run!(q::Q65d60),
                Q65Mode::E60 => run!(q::Q65e60),
                Q65Mode::D120 => run!(q::Q65d120),
                Q65Mode::E120 => run!(q::Q65e120),
                Q65Mode::A300 => run!(q::Q65a300),
            }
        }
        WsjtMode::Jt65 => {
            let params = mfsk_core::jt65::search::SearchParams {
                freq_min_hz: 200.0,
                // 65 tones run about 180 Hz up from the sync tone.
                freq_max_hz: AUDIO_MAX_HZ - 180.0,
                max_candidates: 20,
                ..Default::default()
            };
            let nominal = RATE_U32 as usize;
            mfsk_core::jt65::decode_scan(audio, RATE_U32, nominal, &params)
                .into_iter()
                .filter_map(|r| {
                    let text = jt72_text(r.message)?;
                    Some(mk(r.snr_db, r.dt_sec - 1.0, r.freq_hz, text))
                })
                .collect()
        }
        WsjtMode::Jt9 => {
            let nominal = RATE_U32 as usize;
            mfsk_core::jt9::decode_scan(
                audio,
                RATE_U32,
                nominal,
                &mfsk_core::jt9::search::SearchParams::default(),
            )
            .into_iter()
            .filter_map(|r| {
                let text = jt72_text(r.message)?;
                let dt = r.start_sample as f32 / RATE_U32 as f32 - 1.0;
                Some(mk(r.snr_db, dt, r.freq_hz, text))
            })
            .collect()
        }
        WsjtMode::Wspr => {
            let params = mfsk_core::wspr::SearchParams::default();
            mfsk_core::wspr::decode::decode_scan_with_table(
                audio,
                RATE_U32,
                0,
                &params,
                &mut session.wspr_calls,
            )
            .into_iter()
            .map(|r| {
                use mfsk_core::msg::WsprMessage as W;
                let spot = match &r.message {
                    W::Type1 { callsign, grid, power_dbm } => Some(WsprSpot {
                        callsign: callsign.clone(),
                        grid: grid.clone(),
                        power_dbm: *power_dbm,
                        drift_hz: r.drift_hz,
                    }),
                    W::Type2 { callsign, power_dbm } => Some(WsprSpot {
                        callsign: callsign.clone(),
                        grid: String::new(),
                        power_dbm: *power_dbm,
                        drift_hz: r.drift_hz,
                    }),
                    _ => None,
                };
                // WSPR's frequency is the middle of its four tones.
                let centre = r.freq_hz + 1.5 * mfsk_core::wspr::demod::TONE_SPACING_HZ;
                let mut d = mk(r.snr_db, r.dt_sec, centre, r.message.to_string());
                d.wspr = spot;
                d
            })
            .collect()
        }
    };
    // Different candidates can reach the same signal.
    out.sort_by(|a, b| a.audio_hz.total_cmp(&b.audio_hz));
    out.dedup_by(|a, b| a.message == b.message && (a.audio_hz - b.audio_hz).abs() < 10.0);
    // Remember who was heard, for hashed callsigns in later slots.
    for d in &out {
        for tok in d.message.split_whitespace() {
            let t = tok.trim_matches(|c| c == '<' || c == '>');
            if wsjt77::is_standard_callsign(t) {
                session.hashes.insert(t);
            }
        }
    }
    out
}

/// A JT 72-bit message as text; the layouts this build does not unpack
/// (compound calls, free text) are skipped rather than shown as hex.
fn jt72_text(m: mfsk_core::msg::Jt72Message) -> Option<String> {
    match m {
        mfsk_core::msg::Jt72Message::Standard { call1, call2, grid_or_report } => {
            Some(format!("{call1} {call2} {grid_or_report}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot of low-level noise with `signal` added at `at_s` seconds.
    fn slot(mode: WsjtMode, signal: &[f32], at_s: f64) -> Vec<f32> {
        let n = (mode.period_s() * RATE) as usize;
        // A cheap deterministic noise, about 0.01 RMS.
        let mut x: u32 = 12345;
        let mut out: Vec<f32> = (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.035
            })
            .collect();
        let at = (at_s * RATE) as usize;
        for (o, s) in out[at..].iter_mut().zip(signal) {
            *o += s;
        }
        out
    }

    fn ft8_burst(msg: &str, hz: f32) -> Vec<f32> {
        let mut parts = msg.split_whitespace();
        let (a, b, c) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let bits = wsjt77::pack77(a, b, c).expect("pack");
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits);
        mfsk_core::ft8::wave_gen::tones_to_f32(&tones, hz, 0.3)
    }

    #[test]
    fn ft8_round_trips_with_dt_from_the_nominal_start() {
        let mut s = WsjtSession::new();
        let mut audio = slot(WsjtMode::Ft8, &ft8_burst("CQ AJ3G FN20", 1200.0), 0.5);
        let other = ft8_burst("K1ABC W9XYZ -12", 1900.0);
        for (o, v) in audio[(0.8 * RATE) as usize..].iter_mut().zip(&other) {
            *o += v;
        }
        let d = decode_slot(WsjtMode::Ft8, &audio, 1_700_000_010, &mut s);
        let a = d.iter().find(|d| d.message == "CQ AJ3G FN20").expect("first");
        assert!((a.audio_hz - 1200.0).abs() < 3.0, "{a:?}");
        assert!(a.dt_s.abs() < 0.1, "{a:?}");
        assert_eq!(a.slot_utc, 1_700_000_010);
        let b = d.iter().find(|d| d.message == "K1ABC W9XYZ -12").expect("second");
        assert!((b.dt_s - 0.3).abs() < 0.1, "{b:?}");
    }

    #[test]
    fn ft4_round_trips() {
        let bits = wsjt77::pack77("CQ", "AJ3G", "FN20").unwrap();
        let tones = mfsk_core::ft4::encode::message_to_tones(&bits);
        let burst = mfsk_core::ft4::encode::tones_to_f32(&tones, 1500.0, 0.3);
        let audio = slot(WsjtMode::Ft4, &burst, 0.5);
        let d = decode_slot(WsjtMode::Ft4, &audio, 0, &mut WsjtSession::new());
        let a = d.iter().find(|d| d.message == "CQ AJ3G FN20").expect("{d:?}");
        assert!((a.audio_hz - 1500.0).abs() < 5.0, "{a:?}");
    }

    #[test]
    fn wspr_round_trips_with_its_fields() {
        let burst =
            mfsk_core::wspr::synthesize_type1("AJ3G", "FN20", 37, RATE_U32, 1500.0, 0.2).unwrap();
        let audio = slot(WsjtMode::Wspr, &burst, 1.0);
        let d = decode_slot(WsjtMode::Wspr, &audio, 120, &mut WsjtSession::new());
        let a = d.first().unwrap_or_else(|| panic!("no decode"));
        let spot = a.wspr.as_ref().expect("spot");
        assert_eq!(spot.callsign, "AJ3G");
        assert_eq!(spot.grid, "FN20");
        assert_eq!(spot.power_dbm, 37);
        assert!(a.dt_s.abs() < 0.5, "{a:?}");
        assert!((a.audio_hz - 1502.2).abs() < 2.0, "{a:?}");
    }

    #[test]
    fn jt65_and_jt9_round_trip() {
        for (mode, burst) in [
            (
                WsjtMode::Jt65,
                mfsk_core::jt65::synthesize_standard("CQ", "AJ3G", "FN20", RATE_U32, 1270.0, 0.3),
            ),
            (
                WsjtMode::Jt9,
                mfsk_core::jt9::synthesize_standard("CQ", "AJ3G", "FN20", RATE_U32, 1500.0, 0.3),
            ),
        ] {
            let audio = slot(mode, &burst.unwrap(), 1.0);
            let d = decode_slot(mode, &audio, 0, &mut WsjtSession::new());
            let a = d.iter().find(|d| d.message == "CQ AJ3G FN20");
            let a = a.unwrap_or_else(|| panic!("{mode:?}: {d:?}"));
            assert!(a.dt_s.abs() < 0.3, "{a:?}");
        }
    }

    #[test]
    fn q65_round_trips() {
        let mode = WsjtMode::Q65(Q65Mode::A30);
        let burst =
            mfsk_core::q65::synthesize_standard("CQ", "AJ3G", "FN20", RATE_U32, 1500.0, 0.3);
        let audio = slot(mode, &burst.unwrap(), 0.5);
        let d = decode_slot(mode, &audio, 0, &mut WsjtSession::new());
        let a = d.iter().find(|d| d.message == "CQ AJ3G FN20").expect("{d:?}");
        assert!(a.dt_s.abs() < 0.3, "{a:?}");
    }

    #[test]
    fn noise_alone_decodes_nothing() {
        for mode in [WsjtMode::Ft8, WsjtMode::Ft4] {
            let audio = slot(mode, &[], 0.0);
            assert!(decode_slot(mode, &audio, 0, &mut WsjtSession::new()).is_empty());
        }
    }
}
