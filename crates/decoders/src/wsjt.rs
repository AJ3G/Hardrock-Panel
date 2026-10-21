// The calls into mfsk-core follow SDRoxide's crates/sdroxide-digi/src/modem.rs
// (https://github.com/dividebysandwich/sdroxide), GPL-3.0-or-later.

//! The WSJT modes: FT8, FT4, WSPR, JT65, JT9, FST4, Q65 and MSK144, decoded
//! by the `mfsk-core` crate from one whole UTC slot of 12 kHz audio.
//!
//! MSK144 (meteor scatter) is also decoded as the slot runs, by
//! [`Msk144Live`]: a meteor ping lasts 50 to 200 ms and can come at any time
//! in the slot, so each ping is shown a second or so after it was heard
//! rather than at the slot's end.
//!
//! [`decode_slot`] is one slot, synchronously; [`WsjtSession`] keeps what a
//! run of slots learns (the callsigns heard, which resolve the hashed ones in
//! later messages). [`crate::worker::WsjtDecoder`] runs it on a thread of its
//! own, fed time-stamped audio.
//!
//! The audio must start exactly on the slot boundary: each mode's DT is
//! reported from where WSJT-X puts it (FT8 0.5 s into the slot, the others
//! 1 s, FST4-15 and Q65-15/30 0.5 s), so an on-time station reads about 0.

use std::sync::Arc;

use mfsk_core::msg::decode_request::DecodeRequest;
use mfsk_core::msg::hash_table::CallsignHashTable;
use mfsk_core::msg::wsjt77;
use mfsk_core::msk144::decode::Depth;

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
    /// Meteor scatter, 15 s T/R. Its "DT" is the time into the slot the ping
    /// was heard, as WSJT-X shows it.
    Msk144,
}

impl WsjtMode {
    /// The slot length in seconds.
    pub fn period_s(self) -> f64 {
        match self {
            WsjtMode::Ft8 | WsjtMode::Msk144 => 15.0,
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
            // Pings come any time: DT is measured from the slot's start.
            WsjtMode::Msk144 => 0.0,
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
            WsjtMode::Msk144 => "MSK144",
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
            WsjtMode::Msk144 => "&",
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
    /// Shared so MSK144, which takes the table by `Arc`, need not copy it.
    hashes: Arc<CallsignHashTable>,
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
            Arc::make_mut(&mut self.hashes).insert(c);
        }
    }

    /// Remember the standard callsigns in `message`, for the hashed forms
    /// later messages use.
    fn learn(&mut self, message: &str) {
        for tok in message.split_whitespace() {
            let t = tok.trim_matches(|c| c == '<' || c == '>');
            if wsjt77::is_standard_callsign(t) {
                Arc::make_mut(&mut self.hashes).insert(t);
            }
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
            let ht = &*session.hashes;
            DecodeRequest::<mfsk_core::Ft8>::new(
                &pcm,
                AUDIO_MIN_HZ,
                AUDIO_MAX_HZ,
                SYNC_MIN,
                MAX_CAND,
            )
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
            let ht = &*session.hashes;
            DecodeRequest::<mfsk_core::Ft4>::new(
                &pcm,
                AUDIO_MIN_HZ,
                AUDIO_MAX_HZ,
                SYNC_MIN,
                MAX_CAND,
            )
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
            let ht = &*session.hashes;
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
        WsjtMode::Msk144 => {
            let mut live = Msk144Live::new(slot_utc);
            live.push(audio);
            live.finish(session);
            return live.decodes().to_vec();
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
        session.learn(&d.message);
    }
    out
}

// ───────────────────────────── MSK144 ─────────────────────────────

/// mfsk-core's MSK144 analysis block (7168 samples, 0.597 s) and its step
/// (half a block), samples.
const MSK_BLOCK: usize = 7168;
const MSK_STEP: usize = MSK_BLOCK / 2;
/// How far before the new audio each pass starts, past the block it must
/// overlap, samples (1 s). The decoder learns the noise floor, which each
/// ping's SNR is measured against, from the blocks with no ping in them;
/// this gives it a few before the new audio.
const MSK_LOOKBACK: usize = 12_000;
/// Where MSK144 is listened for: WSJT-X's default 1500 Hz, ±100 Hz (its F Tol).
const MSK_FC_HZ: f32 = 1500.0;
const MSK_TOL_HZ: f32 = 100.0;

/// MSK144 decoded as its slot runs.
///
/// Push the slot's audio as it comes and call [`Msk144Live::pass`] every
/// second or so: each pass decodes only the audio not yet looked at (with a
/// block's overlap, so a ping across the join is not missed, and a second
/// before that for the noise floor), so a pass costs the same at the end of
/// the slot as at its start. The passes run only mfsk-core's short-ping
/// decoder, which is what a meteor ping needs and costs about a millisecond
/// a block. At the slot's end [`Msk144Live::finish`] goes over the whole
/// slot once more with the frame-averaging search as well (WSJT-X's
/// "Normal" depth, some 15 ms a block, under a second a slot), for signals
/// too weak for one frame but lasting several (a long trail, sporadic E).
///
/// A message heard more than once in the slot (a station sends the same one
/// over and over, and a long ping is decoded in several blocks) is kept
/// once, at its best SNR, with the time and frequency it was heard at then.
///
/// From mfsk-core: no Sh (short) messages or MSK40, and no adaptive
/// equaliser trained on the session's decodes (WSJT-X's "Rx equalization"),
/// so a station with a lopsided passband decodes a little worse than in
/// WSJT-X. A ping in the slot's first 0.6 s is measured before any noise
/// floor is known, and its SNR reads 0.
pub struct Msk144Live {
    slot_utc: i64,
    pcm: Vec<i16>,
    /// Where the last pass's last block ended, samples.
    done_to: usize,
    best: Vec<WsjtDecode>,
}

impl Msk144Live {
    /// For the slot starting at `slot_utc` (Unix seconds).
    pub fn new(slot_utc: i64) -> Self {
        Msk144Live { slot_utc, pcm: Vec::new(), done_to: 0, best: Vec::new() }
    }

    pub fn slot_utc(&self) -> i64 {
        self.slot_utc
    }

    /// How much of the slot's audio has been pushed, samples at 12 kHz.
    pub fn heard(&self) -> usize {
        self.pcm.len()
    }

    /// Add the next 12 kHz audio of the slot.
    pub fn push(&mut self, audio: &[f32]) {
        self.pcm.extend(to_i16(audio));
    }

    /// Decode the audio no pass has looked at yet, if there is a block's step
    /// of it. Returns the messages new to the slot, and those heard again at
    /// a better SNR (with their new SNR, time and frequency).
    pub fn pass(&mut self, session: &mut WsjtSession) -> Vec<WsjtDecode> {
        let len = self.pcm.len();
        let fresh = if self.done_to == 0 { MSK_BLOCK } else { self.done_to + MSK_STEP };
        if len < fresh {
            return Vec::new();
        }
        let start = if self.done_to == 0 {
            0
        } else {
            self.done_to.saturating_sub(MSK_BLOCK + MSK_LOOKBACK)
        };
        let blocks = (len - start - MSK_BLOCK) / MSK_STEP + 1;
        let end = start + (blocks - 1) * MSK_STEP + MSK_BLOCK;
        self.done_to = end;
        // The window's first blocks were in the last pass too, which knew
        // the noise floor there; this pass did not, so their SNR is wrong.
        let skip_s = if start > 0 { MSK_BLOCK as f32 / RATE as f32 } else { 0.0 };
        self.decode(start, end, Depth::Fast, skip_s, session)
    }

    /// At the slot's end: decode it all again, more deeply (see the type's
    /// doc). Returns what [`Msk144Live::pass`] does.
    pub fn finish(&mut self, session: &mut WsjtSession) -> Vec<WsjtDecode> {
        self.done_to = self.pcm.len();
        self.decode(0, self.pcm.len(), Depth::Normal, 0.0, session)
    }

    /// Decode samples `start..end`, ignoring what was heard in the first
    /// `skip_s` seconds of them, and keep each message's best.
    fn decode(
        &mut self,
        start: usize,
        end: usize,
        depth: Depth,
        skip_s: f32,
        session: &mut WsjtSession,
    ) -> Vec<WsjtDecode> {
        let found = mfsk_core::msk144::decode::decode_slot_with_hash_table(
            &self.pcm[start..end],
            MSK_FC_HZ,
            MSK_TOL_HZ,
            depth,
            Some(session.hashes.clone()),
        );
        let start_s = start as f32 / RATE as f32;
        let mut out = Vec::new();
        for r in found.into_iter().filter(|r| r.tsec >= skip_s) {
            let d = WsjtDecode {
                mode: WsjtMode::Msk144,
                slot_utc: self.slot_utc,
                snr_db: r.snr_db as f32,
                dt_s: start_s + r.tsec,
                audio_hz: r.freq_hz,
                message: r.message,
                wspr: None,
            };
            if keep_best(&mut self.best, &d) {
                session.learn(&d.message);
                out.push(d);
            }
        }
        out
    }

    /// Every message of the slot so far, each at its best SNR, in the order
    /// first heard.
    pub fn decodes(&self) -> &[WsjtDecode] {
        &self.best
    }
}

/// Keep `d` in `best` if its message is new there or it was heard better
/// (a higher SNR) this time. Returns whether it was kept.
fn keep_best(best: &mut Vec<WsjtDecode>, d: &WsjtDecode) -> bool {
    match best.iter_mut().find(|b| b.message == d.message) {
        None => {
            best.push(d.clone());
            true
        }
        Some(b) if d.snr_db > b.snr_db => {
            *b = d.clone();
            true
        }
        Some(_) => false,
    }
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
pub(crate) mod tests {
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

    /// Gaussian noise of RMS `rms`, deterministic (xorshift and Box-Muller).
    fn gauss(n: usize, rms: f32, seed: u32) -> Vec<f32> {
        let mut x = seed | 1;
        let mut u = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 + 1.0) / (u32::MAX as f32 + 2.0)
        };
        (0..n)
            .map(|_| {
                let (a, b) = (u(), u());
                rms * (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
            })
            .collect()
    }

    /// An MSK144 transmission of `msg` at `hz`, `frames` frames long (72 ms
    /// each), peak amplitude `amp`: packed, given its CRC-13, LDPC-encoded and
    /// framed by mfsk-core, then carried up from baseband to audio.
    pub(crate) fn msk144_burst(msg: &str, hz: f32, frames: usize, amp: f32) -> Vec<f32> {
        use mfsk_core::fec::FecCodec;
        let mut parts = msg.split_whitespace();
        let (a, b, c) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let bits = wsjt77::pack77(a, b, c).expect("pack");
        let mut info = [0u8; 90];
        info[..77].copy_from_slice(&bits);
        let mut bytes = [0u8; 12];
        for (i, &b) in bits.iter().enumerate() {
            bytes[i / 8] |= (b & 1) << (7 - i % 8);
        }
        let crc = mfsk_core::fec::ldpc_128_90::crc13(&bytes);
        for i in 0..13 {
            info[77 + i] = ((crc >> (12 - i)) & 1) as u8;
        }
        let mut cw = [0u8; 128];
        mfsk_core::fec::Ldpc128_90.encode(&info, &mut cw);
        let frame = mfsk_core::msk144::tx::synth_codeword_frame(&cw);
        let w = std::f32::consts::TAU * hz / RATE as f32;
        (0..frames * frame.len())
            .map(|k| {
                let c = frame[k % frame.len()];
                let ph = w * k as f32;
                amp * (c.re * ph.cos() - c.im * ph.sin())
            })
            .collect()
    }

    /// A 15 s slot of noise (0.03 RMS, about the panel's level) with a ping
    /// of `msg` added at `at_s`.
    fn msk144_slot(msg: &str, at_s: f64, hz: f32, frames: usize, amp: f32) -> Vec<f32> {
        let mut audio = gauss(15 * RATE_U32 as usize, 0.03, 99);
        let at = (at_s * RATE) as usize;
        for (o, s) in audio[at..].iter_mut().zip(msk144_burst(msg, hz, frames, amp)) {
            *o += s;
        }
        audio
    }

    #[test]
    fn an_msk144_ping_is_reported_live_with_its_time_snr_and_df() {
        // A 144 ms ping (two frames) at a time the decoder can't know,
        // 30 Hz off the centre, at a modest SNR.
        let at_s = 7.37;
        let audio = msk144_slot("CQ AJ3G FN20", at_s, 1530.0, 2, 0.05);
        let mut s = WsjtSession::new();
        let mut live = Msk144Live::new(1_700_000_010);
        let mut first = None;
        let mut worst_ms = 0.0f64;
        let mut total_ms = 0.0f64;
        let mut passes = 0;
        for chunk in audio.chunks(RATE_U32 as usize) {
            live.push(chunk);
            let t = std::time::Instant::now();
            let new = live.pass(&mut s);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            worst_ms = worst_ms.max(ms);
            total_ms += ms;
            passes += 1;
            if first.is_none() && new.iter().any(|d| d.message == "CQ AJ3G FN20") {
                first = Some(live.heard() as f64 / RATE);
            }
        }
        let t = std::time::Instant::now();
        live.finish(&mut s);
        let end_ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "MSK144 live: {passes} passes, {total_ms:.0} ms in all, worst {worst_ms:.0} ms; \
             slot end {end_ms:.0} ms"
        );
        let heard_at = first.expect("decoded");
        // Reported a second or two after the ping, well before the slot ends.
        assert!(heard_at < at_s + 2.5 && heard_at < 15.0, "{heard_at}");
        let d = live.decodes();
        assert_eq!(d.len(), 1, "{d:?}");
        let d = &d[0];
        assert_eq!(d.mode, WsjtMode::Msk144);
        assert_eq!(d.slot_utc, 1_700_000_010);
        assert!((d.dt_s - at_s as f32).abs() < 0.3, "{d:?}");
        assert!((d.audio_hz - 1530.0).abs() < 10.0, "{d:?}");
        assert!((-8.0..=24.0).contains(&d.snr_db), "{d:?}");
        assert!(d.snr_db > 0.0, "{d:?}");
    }

    #[test]
    fn msk144_whole_slot_keeps_each_message_once_at_its_best() {
        // The same message in a weak ping and later a strong one, and another
        // station's message.
        let mut audio = msk144_slot("K1ABC AJ3G -03", 2.2, 1500.0, 3, 0.03);
        for (o, s) in audio[(9.1 * RATE) as usize..].iter_mut().zip(msk144_burst(
            "K1ABC AJ3G -03",
            1490.0,
            2,
            0.2,
        )) {
            *o += s;
        }
        for (o, s) in audio[(12.0 * RATE) as usize..].iter_mut().zip(msk144_burst(
            "CQ W9XYZ EN37",
            1560.0,
            2,
            0.1,
        )) {
            *o += s;
        }
        let d = decode_slot(WsjtMode::Msk144, &audio, 0, &mut WsjtSession::new());
        let a: Vec<_> = d.iter().filter(|d| d.message == "K1ABC AJ3G -03").collect();
        assert_eq!(a.len(), 1, "{d:?}");
        // The strong ping's SNR, time and frequency.
        assert!((a[0].dt_s - 9.1).abs() < 0.3, "{d:?}");
        assert!((a[0].audio_hz - 1490.0).abs() < 10.0, "{d:?}");
        assert!(d.iter().any(|d| d.message == "CQ W9XYZ EN37"), "{d:?}");
    }

    #[test]
    fn a_long_weak_msk144_signal_is_found_at_the_slot_end() {
        // 2.9 s of a signal too weak for any one frame: the live passes miss
        // it, the slot end's frame averaging finds it.
        let audio = msk144_slot("CQ AJ3G FN20", 4.0, 1500.0, 40, 0.012);
        let mut s = WsjtSession::new();
        let mut live = Msk144Live::new(0);
        for c in audio.chunks(RATE_U32 as usize) {
            live.push(c);
            assert!(live.pass(&mut s).is_empty());
        }
        let d = live.finish(&mut s);
        assert!(d.iter().any(|d| d.message == "CQ AJ3G FN20"), "{d:?}");
    }

    #[test]
    fn keep_best_keeps_the_higher_snr() {
        let mk = |snr_db: f32, dt_s: f32, m: &str| WsjtDecode {
            mode: WsjtMode::Msk144,
            slot_utc: 0,
            snr_db,
            dt_s,
            audio_hz: 1500.0,
            message: m.into(),
            wspr: None,
        };
        let mut best = Vec::new();
        assert!(keep_best(&mut best, &mk(3.0, 1.0, "A B C")));
        assert!(!keep_best(&mut best, &mk(2.0, 4.0, "A B C")));
        assert!(!keep_best(&mut best, &mk(3.0, 5.0, "A B C")));
        assert!(keep_best(&mut best, &mk(0.0, 6.0, "D E F")));
        assert!(keep_best(&mut best, &mk(9.0, 7.0, "A B C")));
        assert_eq!(best.len(), 2);
        assert_eq!((best[0].snr_db, best[0].dt_s), (9.0, 7.0));
    }

    #[test]
    fn noise_alone_decodes_nothing() {
        let audio = gauss(15 * RATE_U32 as usize, 0.03, 5);
        assert!(decode_slot(WsjtMode::Msk144, &audio, 0, &mut WsjtSession::new()).is_empty());
        for mode in [WsjtMode::Ft8, WsjtMode::Ft4] {
            let audio = slot(mode, &[], 0.0);
            assert!(decode_slot(mode, &audio, 0, &mut WsjtSession::new()).is_empty());
        }
    }
}
