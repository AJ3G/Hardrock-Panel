//! The DECODE window: the panel's own decoders (decisions 26 to 28) on the
//! receiver's audio (48 kHz, each block stamped in UTC from the system
//! clock) or on the radio's I/Q.
//!
//! * WSJT modes (FT8, FT4, WSPR, JT65, JT9, FST4, Q65, MSK144) in UTC slots,
//!   each decode with its UTC, SNR, DT, audio frequency and RF frequency, and
//!   sent on as WSJT-X UDP when that is on under SETTINGS. MSK144's meteor
//!   pings show as they are heard, with DT their time into the slot.
//! * Keyboard modes (RTTY, PSK31, NAVTEX, Olivia, THOR) as running text.
//! * Pictures (SSTV, WEFAX) as they are drawn, line by line.
//! * Data links (HFDL, ACARS, VDL2, AIS, Mode S) from the I/Q, at true RF:
//!   the converter's LO is the radio window's transverter offset.
//!
//! Every decoder runs on threads of its own behind a queue that drops rather
//! than waits; the window shows how many blocks each has dropped.
//!
//! For the WSJT modes the window also has the slot timer and decode light
//! ([`crate::slotclock`]), DIST and AZ to each station from SETTINGS →
//! Station's grid ([`crate::station`]), lines to select and copy, the running
//! `ALL.TXT` ([`crate::decodelog`]) and the SPOTS box ([`crate::spots`]).
//!
//! **SKIM** (decision 54) labels each station heard on the scope, at its
//! frequency, from this window's lines ([`Decode::skim_feed`]); a label
//! clicked there picks its line here. **SKIM WIDE** (decision 54) also
//! decodes FT8 or FT4 on every other dial of the mode in the I/Q span
//! ([`decoders::skim::WideSkimmer`]); those are labelled on the scope only.
//! **SKIM CW/PSK/RTTY** (decision 56) finds and reads every CW, PSK31 and
//! RTTY signal in the I/Q window ([`decoders::keyskim::KeySkimWorker`]),
//! whatever mode this window decodes; those are labelled on the scope only.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use datalink::{LinkConfig, LinkEvent, LinkMode, LinkWorker, Tuning};
use decoders::autoid::{AutoId, Pick, Report};
use decoders::keyskim::{KeySkimConfig, KeySkimWorker, KeySpot};
use decoders::modem::wefax::{Ioc, Lpm};
use decoders::skim::WideSkimmer;
use decoders::{
    Fst4Period, ImageCmd, ImageDecoder, ImageMode, ImageUpdate, JttyDecoder, Q65Mode, RgbaImage,
    TextDecoder, TextMode, WsjtConfig, WsjtDecoder, WsjtMode,
};
use eframe::egui::{self, RichText};
use links::WsjtxUdp;
use serde::{Deserialize, Serialize};

use crate::decodelog::{self, DecodeLog};
use crate::rx::{self, Rx};
use crate::scope::digimodes::{self, Digi};
use crate::scope::{KeyTag, SkimFeed, SkimSpot};
use crate::skimlog::{self, SkimLog};
use crate::spots::{self, SpotSettings, Spots, pskreporter, wsprnet};
use crate::station::{self, Heard, StationSettings};
use crate::{chrome, slotclock, theme};

/// Every mode the window offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Kind {
    #[default]
    Ft8,
    Ft4,
    Wspr,
    Jt65,
    Jt9,
    Fst4,
    Q65,
    Msk144,
    Jtty,
    Cw,
    Rtty,
    Psk31,
    Navtex,
    Olivia,
    Thor,
    Sstv,
    Wefax,
    Hfdl,
    Acars,
    Vdl2,
    Ais,
    ModeS,
}

/// What a mode is decoded from, and how it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Wsjt,
    Text,
    Picture,
    Link,
    /// JTTY: a line a message, growing as its frames come (decision 58).
    Chat,
}

impl Kind {
    const GROUPS: [(&'static str, &'static [Kind]); 4] = [
        (
            "WSJT",
            &[
                Kind::Ft8,
                Kind::Ft4,
                Kind::Wspr,
                Kind::Jt65,
                Kind::Jt9,
                Kind::Fst4,
                Kind::Q65,
                Kind::Msk144,
            ],
        ),
        (
            "Keyboard",
            &[
                Kind::Jtty,
                Kind::Cw,
                Kind::Rtty,
                Kind::Psk31,
                Kind::Navtex,
                Kind::Olivia,
                Kind::Thor,
            ],
        ),
        ("Pictures", &[Kind::Sstv, Kind::Wefax]),
        ("From I/Q", &[Kind::Hfdl, Kind::Acars, Kind::Vdl2, Kind::Ais, Kind::ModeS]),
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Ft8 => "FT8",
            Kind::Ft4 => "FT4",
            Kind::Wspr => "WSPR",
            Kind::Jt65 => "JT65",
            Kind::Jt9 => "JT9",
            Kind::Fst4 => "FST4-60",
            Kind::Q65 => "Q65-30A",
            Kind::Msk144 => "MSK144",
            Kind::Jtty => "JTTY",
            Kind::Cw => "CW",
            Kind::Rtty => "RTTY",
            Kind::Psk31 => "PSK31",
            Kind::Navtex => "NAVTEX",
            Kind::Olivia => "Olivia 8/500",
            Kind::Thor => "THOR16",
            Kind::Sstv => "SSTV",
            Kind::Wefax => "WEFAX",
            Kind::Hfdl => "HFDL",
            Kind::Acars => "ACARS",
            Kind::Vdl2 => "VDL2",
            Kind::Ais => "AIS",
            Kind::ModeS => "Mode S",
        }
    }

    fn family(self) -> Family {
        match self {
            Kind::Ft8
            | Kind::Ft4
            | Kind::Wspr
            | Kind::Jt65
            | Kind::Jt9
            | Kind::Fst4
            | Kind::Q65
            | Kind::Msk144 => Family::Wsjt,
            Kind::Cw | Kind::Rtty | Kind::Psk31 | Kind::Navtex | Kind::Olivia | Kind::Thor => {
                Family::Text
            }
            Kind::Sstv | Kind::Wefax => Family::Picture,
            Kind::Hfdl | Kind::Acars | Kind::Vdl2 | Kind::Ais | Kind::ModeS => Family::Link,
            Kind::Jtty => Family::Chat,
        }
    }

    fn wsjt(self) -> Option<WsjtMode> {
        Some(match self {
            Kind::Ft8 => WsjtMode::Ft8,
            Kind::Ft4 => WsjtMode::Ft4,
            Kind::Wspr => WsjtMode::Wspr,
            Kind::Jt65 => WsjtMode::Jt65,
            Kind::Jt9 => WsjtMode::Jt9,
            Kind::Fst4 => WsjtMode::Fst4(Fst4Period::P60),
            Kind::Q65 => WsjtMode::Q65(Q65Mode::A30),
            Kind::Msk144 => WsjtMode::Msk144,
            _ => return None,
        })
    }

    /// The mode's dial frequencies and receiver set-up (decision 39), where
    /// it has a calling frequency.
    pub(crate) fn digi(self) -> Option<&'static Digi> {
        digimodes::digi(match self {
            Kind::Ft8 => "FT8",
            Kind::Ft4 => "FT4",
            Kind::Wspr => "WSPR",
            Kind::Jt65 => "JT65",
            Kind::Jt9 => "JT9",
            Kind::Fst4 => "FST4",
            Kind::Q65 => "Q65",
            Kind::Msk144 => "MSK144",
            Kind::Jtty => "JTTY",
            Kind::Rtty => "RTTY",
            Kind::Psk31 => "PSK31",
            Kind::Navtex => "NAVTEX",
            Kind::Sstv => "SSTV",
            Kind::Wefax => "WEFAX",
            Kind::Acars => "ACARS",
            Kind::Vdl2 => "VDL2",
            Kind::Ais => "AIS",
            Kind::ModeS => "ADS-B",
            Kind::Cw | Kind::Olivia | Kind::Thor | Kind::Hfdl => return None,
        })
    }

    /// The decoder for data mode `d`, if the panel has one.
    pub(crate) fn of_digi(d: &Digi) -> Option<Kind> {
        Kind::GROUPS
            .iter()
            .flat_map(|(_, k)| k.iter().copied())
            .find(|k| k.digi().is_some_and(|x| std::ptr::eq(x, d)))
    }

    /// The receiver's passband for a keyboard mode with no calling
    /// frequency, about the audio its decoder listens at.
    pub(crate) fn audio_passband(self, audio_hz: f64) -> Option<(f32, f32)> {
        let half = match self {
            Kind::Olivia => 350.0,
            Kind::Thor => 250.0,
            _ => return None,
        };
        Some(((audio_hz - half) as f32, (audio_hz + half) as f32))
    }

    fn link(self) -> Option<LinkMode> {
        Some(match self {
            Kind::Hfdl => LinkMode::Hfdl,
            Kind::Acars => LinkMode::Acars,
            Kind::Vdl2 => LinkMode::Vdl2,
            Kind::Ais => LinkMode::Ais,
            Kind::ModeS => LinkMode::ModeS,
            _ => return None,
        })
    }
}

/// What is kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DecodeSettings {
    pub kind: Kind,
    /// Decoding: set each frame from whether the DECODE window is open (no
    /// RUN key, as in WSJT-X). Kept so older settings files still read.
    pub running: bool,
    /// The keyboard modes' audio frequency (RTTY's mark/space centre,
    /// PSK31's carrier, Olivia's and THOR's centre), Hz.
    pub audio_hz: f64,
    /// RTTY and NAVTEX with mark and space swapped.
    pub reverse: bool,
    /// WEFAX's lines per minute, and IOC 288 rather than 576.
    pub lpm: f64,
    pub ioc_288: bool,
    /// Picking a mode takes the radio and the receiver to it (decision 39).
    pub snap: bool,
    /// SPOTS' ticks.
    pub spots: SpotSettings,
    /// SKIM: label the stations heard on the scope (decision 54).
    pub skim: bool,
    /// SKIM WIDE: with FT8 or FT4, decode every other dial of the mode in
    /// the I/Q span too (decision 54).
    pub skim_wide: bool,
    /// SKIM CW/PSK/RTTY: find and read the keyboard modes across the I/Q
    /// window and label them on the scope (decision 56).
    pub skim_keys: bool,
    /// The SKIM LOG window is open (decision 60).
    pub skim_log: bool,
    /// SKIM CW/PSK/RTTY's CW speeds, WPM.
    pub skim_cw_wpm: (u16, u16),
}

impl Default for DecodeSettings {
    fn default() -> Self {
        DecodeSettings {
            kind: Kind::Ft8,
            running: false,
            audio_hz: 1500.0,
            reverse: false,
            lpm: 120.0,
            ioc_288: false,
            snap: true,
            spots: SpotSettings::default(),
            skim: true,
            skim_wide: false,
            skim_keys: false,
            skim_log: false,
            skim_cw_wpm: decoders::keyskim::CW_WPM,
        }
    }
}

impl DecodeSettings {
    fn text_mode(&self) -> Option<TextMode> {
        Some(match self.kind {
            Kind::Rtty => TextMode::Rtty {
                center_hz: self.audio_hz,
                baud: 45.45,
                shift_hz: 170.0,
                reverse: self.reverse,
            },
            Kind::Psk31 => TextMode::Psk31 { carrier_hz: self.audio_hz },
            Kind::Navtex => TextMode::Navtex { reverse: self.reverse },
            Kind::Olivia => TextMode::Olivia { audio_hz: self.audio_hz, tones: 8, bw_hz: 500.0 },
            Kind::Thor => TextMode::Thor { audio_hz: self.audio_hz, baud: 15.625 },
            Kind::Cw => TextMode::Cw,
            _ => return None,
        })
    }

    fn image_mode(&self) -> Option<ImageMode> {
        Some(match self.kind {
            Kind::Sstv => ImageMode::Sstv,
            Kind::Wefax => ImageMode::Wefax {
                ioc: if self.ioc_288 { Ioc::I288 } else { Ioc::I576 },
                lpm: Lpm::ALL.into_iter().find(|l| l.value() == self.lpm).unwrap_or_default(),
                auto_start: true,
            },
            _ => return None,
        })
    }
}

/// One line in the decodes list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    /// Unique this run, for the selection.
    pub id: u64,
    pub utc: f64,
    pub snr: Option<f32>,
    pub dt: Option<f32>,
    pub audio_hz: Option<f32>,
    /// True RF, Hz.
    pub rf_hz: Option<f64>,
    /// Where the signal is and the dial it was heard on, the radio's own
    /// frequencies (before the converter and the calibration), Hz: where the
    /// scope labels it.
    pub radio_hz: Option<(f64, f64)>,
    pub text: String,
    /// Who sent it, and the grid it gave (WSJT modes).
    pub call: Option<String>,
    pub grid: Option<String>,
}

enum Worker {
    Wsjt(Arc<WsjtDecoder>, WsjtMode),
    Text(Arc<TextDecoder>, TextMode),
    /// JTTY, and the audio frequency it listens at most closely.
    Jtty(Arc<JttyDecoder>, f32),
    Picture(Arc<ImageDecoder>, ImageMode),
    Link(Arc<LinkWorker>, LinkConfig),
}

impl Worker {
    fn dropped(&self) -> u64 {
        match self {
            Worker::Wsjt(w, _) => w.dropped(),
            Worker::Text(w, _) => w.dropped(),
            Worker::Jtty(w, _) => w.dropped(),
            Worker::Picture(w, _) => w.dropped(),
            Worker::Link(w, _) => w.dropped(),
        }
    }
}

/// The most rows kept.
const ROWS: usize = 500;
/// The most text kept, characters.
const TEXT: usize = 20_000;

/// The decoders as they run, and what they have heard.
#[derive(Default)]
pub(crate) struct Decode {
    worker: Option<(Kind, Worker)>,
    pub rows: VecDeque<Row>,
    pub text: String,
    image: Option<RgbaImage>,
    image_label: String,
    image_dirty: bool,
    image_at: Option<Instant>,
    tex: Option<egui::TextureHandle>,
    notes: Vec<String>,
    /// Why the mode can't run now (no I/Q, the span doesn't hold its
    /// channels).
    pub error: Option<String>,
    /// The last slot: when, how many decodes, how long it took.
    last_slot: Option<(i64, usize, u32)>,
    /// What the last snap did (or why it didn't).
    pub note: Option<(String, bool)>,
    /// When the WSJT decoder started, for the decode light.
    started_utc: Option<f64>,
    /// Every station heard with a grid.
    pub heard: Heard,
    log: DecodeLog,
    spots: Spots,
    /// The rows picked, by id, and the last one clicked.
    selected: HashSet<u64>,
    anchor: Option<u64>,
    next_id: u64,
    /// A line to bring into view (picked from a scope label).
    scroll_to: Option<u64>,
    /// SKIM WIDE's decoders, and what they heard, newest first.
    wide: Vec<Arc<WideSkimmer>>,
    wide_spots: VecDeque<SkimSpot>,
    /// JTTY's messages and their lines: (message, row id), newest first.
    jtty_rows: VecDeque<(u64, u64)>,
    /// SKIM CW/PSK/RTTY's skimmer, and the exclusions it was given.
    keys: Option<Arc<KeySkimWorker>>,
    keys_cfg: Option<KeySkimConfig>,
    /// Its signals, each kept until its label has faded: the skimmer drops
    /// a track sooner than CW's label fades.
    key_held: Vec<KeySpot>,
    /// What the scope shows, Hz, to the nearest 5 kHz.
    key_view: Option<(f64, f64)>,
    /// SKIM LOG: every mode skimmed, with its mode (decision 60).
    pub(crate) skim_log: SkimLog,
    /// Where the receiver's dial has been: (UTC it moved there, dial Hz),
    /// oldest first, so a slot is put on the dial it was heard on.
    dials: DialLog,
    /// AUTO ID (decision 61): asked for, and at work.
    pub(crate) autoid_on: bool,
    autoid: Option<AutoIdRun>,
    /// The stations placed since GLOBE last took them (decision 65): the
    /// receiver's decodes, SKIM WIDE's and the keyboard skimmer's, each
    /// with its grid or one heard from it before.
    pub(crate) globe_feed: Vec<crate::globe::field::Reception>,
    /// A line picked alone, for GLOBE to show that station and its path
    /// (AJ3G, 8 Nov 2026): its call and where it was heard, Hz.
    pub(crate) globe_pick: Option<(String, Option<f64>)>,
    /// Each FT8 slot's decodes, for TX's sequencer and decode list: taken
    /// (drained) by TX each frame, and capped when nothing takes them.
    pub(crate) tx_feed: Vec<TxHeard>,
    /// Each WSPR slot's decodes, for the TX window's WSPR list: taken as
    /// `tx_feed` is.
    pub(crate) wspr_feed: Vec<WsprHeard>,
    /// When the radio was keyed, so a slot heard while we transmitted is
    /// dropped (decision 71).
    keyed: KeyedLog,
    /// The last slot dropped for that, for the slot line.
    keyed_drop: Option<i64>,
}

/// One FT8 decode as TX reads it.
#[derive(Debug, Clone)]
pub(crate) struct TxHeard {
    pub slot_utc: i64,
    pub snr_db: f32,
    pub dt: f32,
    pub audio_hz: f32,
    pub text: String,
    /// Its true RF, Hz.
    pub rf_hz: f64,
    pub row: u64,
}

/// One WSPR decode as TX's WSPR list reads it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WsprHeard {
    pub slot_utc: i64,
    pub snr_db: f32,
    pub drift_hz: f32,
    pub audio_hz: f32,
    pub call: String,
    /// Empty for a compound call (Type 2).
    pub grid: String,
    pub dbm: i32,
    /// Its true RF, Hz.
    pub rf_hz: f64,
}

/// A station for GLOBE, if its grid is known.
#[allow(clippy::too_many_arguments)]
fn reception(
    heard: &Heard,
    call: &str,
    grid: Option<&str>,
    rf_hz: f64,
    mode: &str,
    snr: Option<f32>,
    utc: f64,
    src: crate::globe::field::Source,
    row: Option<u64>,
) -> Option<crate::globe::field::Reception> {
    let grid = grid
        .filter(|g| station::grid_to_latlon(g).is_some())
        .map(str::to_string)
        .or_else(|| heard.get(call).map(|h| h.grid.clone()))?;
    let pos = station::grid_to_latlon(&grid)?;
    Some(crate::globe::field::Reception {
        call: call.to_string(),
        grid,
        pos,
        rf_hz,
        mode: mode.to_string(),
        snr,
        utc,
        src,
        row,
        from: None,
    })
}

/// The receiver's dial over the last few minutes. A WSJT slot's decodes
/// come a second or two after it ends, by when the dial may have moved: on
/// to another band, AJ3G saw 40 m's stations labelled on 20 m (22 Oct 2026),
/// and they went to ALL.TXT and the spots at 20 m's frequency too.
#[derive(Default)]
struct DialLog(VecDeque<(f64, f64)>);

impl DialLog {
    /// The dial is at `hz` at `utc`.
    fn note(&mut self, utc: f64, hz: f64) {
        if self.0.back().is_none_or(|&(_, d)| d != hz) {
            self.0.push_back((utc, hz));
        }
        while self.0.len() > 1 && self.0[1].0 < utc - 600.0 {
            self.0.pop_front();
        }
    }

    /// The one dial from `t0` to `t1`, or `None` if it moved between.
    fn during(&self, t0: f64, t1: f64) -> Option<f64> {
        let at_start = self.0.iter().rev().find(|&&(t, _)| t <= t0).or(self.0.front())?.1;
        let moved = self.0.iter().any(|&(t, d)| t > t0 && t < t1 && d != at_start);
        (!moved).then_some(at_start)
    }
}

/// When the radio was keyed, over the last few minutes: (from, to) UTC s,
/// oldest first, the last one open (`None`) while it is keyed still.
///
/// The DUO's I/Q keeps flowing while it transmits, so the decoders hear
/// our own over at +25 dB: AJ3G saw his "UN7GBX AJ3G -18" decoded back in
/// the slot he sent it, flagged NEW with a REPLY button (11 Nov 2026). A
/// slot the radio was keyed in for more than [`KEYED_DROP_S`] is dropped
/// whole: nothing else in it was heard properly either.
#[derive(Default)]
struct KeyedLog(VecDeque<(f64, Option<f64>)>);

/// Keyed this long in a slot drops it, s: the sequencer's unkey and a
/// rig poll's lag may overlap a slot's edge by a little.
const KEYED_DROP_S: f64 = 1.0;

impl KeyedLog {
    /// The radio is keyed, or not, at `utc`.
    fn note(&mut self, utc: f64, keyed: bool) {
        match self.0.back_mut() {
            Some((_, end)) if end.is_none() => {
                if !keyed {
                    *end = Some(utc);
                }
            }
            _ if keyed => self.0.push_back((utc, None)),
            _ => {}
        }
        while self.0.front().is_some_and(|&(_, e)| e.is_some_and(|e| e < utc - 600.0)) {
            self.0.pop_front();
        }
    }

    /// How long the radio was keyed between `t0` and `t1`, s.
    fn overlap(&self, t0: f64, t1: f64) -> f64 {
        self.0.iter().map(|&(a, b)| (b.unwrap_or(f64::INFINITY).min(t1) - a.max(t0)).max(0.0)).sum()
    }

    /// Whether a slot from `t0` lasting `len` s was transmitted over.
    fn spoils(&self, t0: f64, len: f64) -> bool {
        self.overlap(t0, t0 + len) > KEYED_DROP_S
    }
}

/// AUTO ID listening to one passband on one dial.
struct AutoIdRun {
    id: Arc<AutoId>,
    /// The audio passband and the receiver's dial it was started on: moved,
    /// it starts again on what is there now.
    band: (f32, f32),
    rx_hz: f64,
    report: Report,
    at: Instant,
}

/// The audio band, Hz, a receiver passband (Hz from the dial, below it
/// negative on LSB) is heard in.
fn audio_band(pb: (f32, f32)) -> (f32, f32) {
    let (lo, hi) = (pb.0.min(pb.1), pb.0.max(pb.1));
    if lo >= 0.0 {
        (lo, hi)
    } else if hi <= 0.0 {
        (-hi, -lo)
    } else {
        (0.0, (-lo).max(hi))
    }
}

/// DECODE's mode and set-up for what AUTO ID picked: (kind, audio Hz,
/// reversed).
fn kind_of_pick(p: Pick) -> Option<(Kind, Option<f64>, bool)> {
    Some(match p {
        Pick::Wsjt(m) => (
            Kind::GROUPS
                .iter()
                .flat_map(|(_, k)| k.iter().copied())
                .find(|k| k.wsjt() == Some(m))?,
            None,
            false,
        ),
        Pick::Jtty => (Kind::Jtty, None, false),
        Pick::Text(TextMode::Rtty { center_hz, reverse, .. }) => {
            (Kind::Rtty, Some(center_hz), reverse)
        }
        Pick::Text(TextMode::Psk31 { carrier_hz }) => (Kind::Psk31, Some(carrier_hz), false),
        Pick::Text(TextMode::Navtex { reverse }) => (Kind::Navtex, None, reverse),
        Pick::Text(TextMode::Olivia { audio_hz, .. }) => (Kind::Olivia, Some(audio_hz), false),
        Pick::Text(TextMode::Thor { audio_hz, .. }) => (Kind::Thor, Some(audio_hz), false),
        Pick::Text(TextMode::Cw) => (Kind::Cw, None, false),
        Pick::Kind("SSTV") => (Kind::Sstv, None, false),
        Pick::Kind("WEFAX") => (Kind::Wefax, None, false),
        _ => return None,
    })
}

/// The most of the wide skimmer's decodes kept.
const WIDE_SPOTS: usize = 400;

/// Where the receiver is, for the RF of each decode.
#[derive(Clone, Copy)]
pub(crate) struct Where {
    /// The receiver's dial, the radio's own Hz.
    pub dial_hz: f64,
    pub lsb: bool,
    /// The transverter's LO (the radio window's offset), Hz.
    pub xvtr_hz: f64,
    pub ppm: f64,
}

impl Where {
    /// The radio's own frequency of a signal at `audio_hz`: the dial and
    /// the audio, on the receiver's sideband.
    fn radio_of_audio(&self, audio_hz: f64) -> f64 {
        if self.lsb { self.dial_hz - audio_hz } else { self.dial_hz + audio_hz }
    }

    fn rf_of_audio(&self, audio_hz: f64) -> f64 {
        decoders::FreqCal { ppm: self.ppm }.rf_of_audio(self.dial_hz, audio_hz, self.lsb)
            + self.xvtr_hz
    }
}

impl Decode {
    /// Stop decoding and drop the taps.
    pub(crate) fn stop(&mut self, taps: &rx::Taps) {
        taps.set_audio("decode", None);
        taps.set_iq("decode", None);
        taps.set_iq("skim", None);
        self.stop_autoid(taps);
        self.worker = None;
        self.wide.clear();
        self.started_utc = None;
    }

    fn stop_autoid(&mut self, taps: &rx::Taps) {
        if self.autoid.take().is_some() {
            taps.set_audio("autoid", None);
        }
    }

    /// The scope's view (the radio's own Hz), for SKIM CW/PSK/RTTY's window
    /// in a span wider than it reads.
    pub(crate) fn set_view(&mut self, v: Option<(f64, f64)>) {
        const STEP: f64 = 5_000.0;
        self.key_view = v.map(|(lo, hi)| ((lo / STEP).floor() * STEP, (hi / STEP).ceil() * STEP));
    }

    /// AUTO ID (decision 61): listen to the receiver's passband while it is
    /// asked for, and start again when the passband or the dial moves.
    fn autoid_frame(&mut self, st: &StationSettings, rx: &Rx, on: bool) {
        if !(on && self.autoid_on) {
            self.autoid_on &= on;
            self.stop_autoid(&rx.taps);
            return;
        }
        let band = audio_band(rx.passband);
        let moved = self.autoid.as_ref().is_some_and(|r| {
            (r.band.0 - band.0).abs() > 1.0
                || (r.band.1 - band.1).abs() > 1.0
                || (r.rx_hz - rx.rx_hz).abs() > 5.0
        });
        if moved {
            self.stop_autoid(&rx.taps);
        }
        if self.autoid.is_none() {
            let calls: Vec<String> = st.call().into_iter().collect();
            tracing::info!("AUTO ID started on {:.0}–{:.0} Hz", band.0, band.1);
            let id = AutoId::new(sdr::AUDIO_RATE, band, &calls);
            let f = id.clone();
            rx.taps.set_audio("autoid", Some(Box::new(move |a, utc| f.push(a, utc))));
            let report = id.report();
            self.autoid = Some(AutoIdRun { id, band, rx_hz: rx.rx_hz, report, at: Instant::now() });
        }
        if let Some(r) = self.autoid.as_mut()
            && r.at.elapsed().as_secs_f32() >= 0.5
        {
            r.report = r.id.report();
            r.at = Instant::now();
        }
    }

    /// The scope's skimmer labels (decision 54): the newest decodes, with
    /// SKIM on and a slotted mode running, and SKIM CW/PSK/RTTY's signals
    /// (decision 56); `None` with neither.
    pub(crate) fn skim_feed(&self, s: &DecodeSettings) -> Option<SkimFeed> {
        let wsjt = match self.worker.as_ref() {
            Some((kind, Worker::Wsjt(..))) if s.skim => kind.wsjt(),
            _ => None,
        };
        let now = decoders::slot::utc_now();
        let wide = s.skim && !self.wide.is_empty();
        let mut feed = match wsjt {
            Some(m) => skim_feed(&self.rows, &self.wide_spots, m, now),
            // Another mode: the skimmers' FT8 and FT4 alone.
            None if wide => skim_feed(&VecDeque::new(), &self.wide_spots, WsjtMode::Ft8, now),
            None => SkimFeed::default(),
        };
        feed.key_audio_hz = s.audio_hz;
        if self.keys.is_some() {
            feed.spots.extend(self.key_held.iter().filter_map(key_spot));
        }
        (wsjt.is_some() || wide || self.keys.is_some()).then_some(feed)
    }

    /// SKIM CW/PSK/RTTY (decision 56): run the keyboard skimmer on the I/Q
    /// while `on`, kept off the WSJT modes' sub-bands.
    fn keys_frame(&mut self, s: &DecodeSettings, rx: &Rx, at: Where, on: bool) {
        if !(on && s.skim_keys) {
            if self.keys.take().is_some() {
                rx.taps.set_iq("keyskim", None);
                self.keys_cfg = None;
            }
            return;
        }
        let cfg = KeySkimConfig {
            exclude: wsjt_bands(at.xvtr_hz),
            cw_wpm: s.skim_cw_wpm,
            // Wider than 192 kHz the skimmer reads a 192 kHz cut of the
            // span: around what the scope shows, not the span's centre.
            // At 192 kHz or less it reads it all, as before.
            view: self.key_view.filter(|_| rx.rate() > decoders::keyskim::SKIM_RATE * 1.01),
            ..KeySkimConfig::default()
        };
        match &self.keys {
            Some(k) if self.keys_cfg.as_ref() != Some(&cfg) => k.set_config(cfg.clone()),
            Some(_) => return,
            None => {
                tracing::info!("keyboard skimmer started");
                let k = Arc::new(KeySkimWorker::spawn(cfg.clone()));
                let f = k.clone();
                rx.taps.set_iq(
                    "keyskim",
                    Some(Box::new(move |b| f.push_iq(b.iq, b.rate, b.center, b.utc))),
                );
                self.keys = Some(k);
            }
        }
        self.keys_cfg = Some(cfg);
    }

    /// SKIM LOG (decision 60): SKIM CW/PSK/RTTY's signals as they are now,
    /// and what came this frame into SKIM.TXT.
    fn skim_log_frame(&mut self, at: Where) {
        if let Some(k) = &self.keys {
            let st = k.state();
            let now = decoders::slot::utc_now();
            for p in &st.spots {
                // A callsign read for the first time on this track: GLOBE
                // places it if it has been heard with a grid.
                let named = match self.key_held.iter_mut().find(|h| h.id == p.id) {
                    Some(h) => {
                        let new = h.call.is_none() && p.call.is_some();
                        *h = p.clone();
                        new
                    }
                    None => {
                        self.key_held.push(p.clone());
                        p.call.is_some()
                    }
                };
                if let Some(c) = p.call.as_deref().filter(|_| named) {
                    self.globe_feed.extend(reception(
                        &self.heard,
                        c,
                        None,
                        p.hz + at.xvtr_hz,
                        p.mode.label(),
                        Some(p.snr_db),
                        p.heard_utc,
                        crate::globe::field::Source::Skim,
                        None,
                    ));
                }
            }
            for h in &mut self.key_held {
                // Its track gone: heard no more.
                h.active &= st.spots.iter().any(|p| p.id == h.id);
            }
            self.key_held.retain(|h| now - h.heard_utc < crate::scope::skim::key_fade_s(h.mode));
            for p in &st.spots {
                // RBN (AJ3G, 7 Nov 2026): CW stations calling CQ, when the
                // SPOTS box's RBN is ticked (`spots::rbn`).
                if p.mode == decoders::keyskim::KeyMode::Cw
                    && let Some(c) = &p.call
                {
                    self.spots.cw_skim(c, &p.text, p.hz + at.xvtr_hz, p.snr_db, p.wpm, p.heard_utc);
                }
                self.skim_log.key(
                    p.id,
                    skimlog::Line {
                        utc: p.heard_utc,
                        mode: p.mode.label(),
                        source: skimlog::Source::Keys,
                        rf_hz: p.hz + at.xvtr_hz,
                        snr: p.snr_db,
                        call: p.call.clone(),
                        text: p.text.clone(),
                        track: None,
                        live: p.active,
                    },
                );
            }
            let alive: Vec<u64> = st.spots.iter().map(|p| p.id).collect();
            self.skim_log.end_tracks(&alive);
        } else {
            self.key_held.clear();
            self.skim_log.end_tracks(&[]);
        }
        self.skim_log.flush();
    }

    /// Pick line `id` alone and bring it into view: a scope label clicked.
    pub(crate) fn pick_row(&mut self, id: u64) {
        if self.rows.iter().any(|r| r.id == id) {
            self.selected.clear();
            self.selected.insert(id);
            self.anchor = Some(id);
            self.scroll_to = Some(id);
            self.note_pick();
        }
    }

    /// When one line is picked and it has a call, hand it to GLOBE.
    fn note_pick(&mut self) {
        if self.selected.len() != 1 {
            return;
        }
        let id = *self.selected.iter().next().expect("one");
        if let Some(r) = self.rows.iter().find(|r| r.id == id)
            && let Some(c) = &r.call
        {
            self.globe_pick = Some((c.clone(), r.rf_hz));
        }
    }

    /// SKIM WIDE (decision 54): run a skimmer on every other dial of the
    /// mode in the span, and on the other of FT8 and FT4's dials there
    /// (AJ3G: FT4 on the band unlabelled from FT8), started again whenever
    /// the dials change, and take what they heard.
    fn wide_frame(&mut self, s: &DecodeSettings, st: &StationSettings, rx: &Rx, at: Where) {
        let want = self.worker.as_ref().map_or_else(Vec::new, |(kind, _)| {
            wide_dials(*kind, s, at.dial_hz, at.xvtr_hz, rx.centre(), rx.rate())
        });
        let same = self.wide.len() == want.len()
            && self.wide.iter().zip(&want).all(|(w, (m, d))| w.mode() == *m && w.dials() == &d[..]);
        if !same {
            rx.taps.set_iq("skim", None);
            self.wide.clear();
            let mine: Vec<String> = st.call().into_iter().collect();
            for (mode, dials) in want {
                tracing::info!("{} skimmer started on {dials:?}", mode.label());
                self.wide.push(Arc::new(WideSkimmer::spawn(mode, &dials, &mine)));
            }
            if !self.wide.is_empty() {
                let f = self.wide.clone();
                rx.taps.set_iq(
                    "skim",
                    Some(Box::new(move |b| {
                        for w in &f {
                            w.push_iq(b.iq, b.rate, b.center, b.utc);
                        }
                    })),
                );
            }
        }
        for w in &self.wide {
            let wsjt = Some(w.mode());
            for r in w.results() {
                // Our own over, heard back (decision 71).
                if self.keyed.spoils(r.slot.slot_utc as f64, w.mode().period_s()) {
                    continue;
                }
                // Skimmed WSPR goes to WSPRnet as DECODE's own does, a slot
                // for each dial it was heard on, when the SPOTS box's WSPRnet
                // is ticked (AJ3G, 8 Nov 2026: "every two minutes or so if
                // there was a decode it should be sent"). Only slots with
                // stations in them: a dial skimmed is not one listened to.
                if w.mode() == WsjtMode::Wspr && !r.slot.decodes.is_empty() {
                    let cal = decoders::FreqCal { ppm: at.ppm };
                    let spots = r
                        .slot
                        .decodes
                        .iter()
                        .filter_map(|d| {
                            let w = d.wspr.as_ref()?;
                            Some(wsprnet::Spot {
                                call: w.callsign.clone(),
                                grid: w.grid.clone(),
                                snr_db: d.snr_db.round() as i32,
                                dt_s: d.dt_s,
                                drift_hz: w.drift_hz.round() as i32,
                                freq_hz: cal.rf_of_audio(r.dial_hz, d.audio_hz as f64, false)
                                    + at.xvtr_hz,
                                power_dbm: w.power_dbm,
                            })
                        })
                        .collect::<Vec<_>>();
                    if !spots.is_empty() {
                        self.spots.wspr_slot(wsprnet::Slot {
                            utc: r.slot.slot_utc,
                            dial_hz: cal.rf_of_audio(r.dial_hz, 0.0, false) + at.xvtr_hz,
                            spots,
                        });
                    }
                }
                for d in &r.slot.decodes {
                    let cal = decoders::FreqCal { ppm: at.ppm };
                    // WSPR's message is "CALL GRID dBm", which reads as no
                    // sender; its decode carries them.
                    let (call, grid) = match &d.wspr {
                        Some(w) => {
                            (Some(w.callsign.clone()), (!w.grid.is_empty()).then(|| w.grid.clone()))
                        }
                        None => station::sender_of(&d.message),
                    };
                    self.skim_log.add(skimlog::Line {
                        utc: d.slot_utc as f64,
                        mode: w.mode().label(),
                        source: skimlog::Source::Wide,
                        rf_hz: cal.rf_of_audio(r.dial_hz, d.audio_hz as f64, false) + at.xvtr_hz,
                        snr: d.snr_db,
                        call: call.clone(),
                        text: d.message.clone(),
                        track: None,
                        live: false,
                    });
                    let Some(call) = call else { continue };
                    if let Some(g) = &grid {
                        self.heard.note(&call, g, d.slot_utc as f64);
                    }
                    self.globe_feed.extend(reception(
                        &self.heard,
                        &call,
                        grid.as_deref(),
                        cal.rf_of_audio(r.dial_hz, d.audio_hz as f64, false) + at.xvtr_hz,
                        w.mode().label(),
                        Some(d.snr_db),
                        d.slot_utc as f64,
                        crate::globe::field::Source::Skim,
                        None,
                    ));
                    self.wide_spots.push_front(SkimSpot {
                        row: None,
                        hz: r.dial_hz + d.audio_hz as f64,
                        audio_hz: d.audio_hz,
                        dial_hz: r.dial_hz,
                        call,
                        text: d.message.clone(),
                        snr: d.snr_db,
                        utc: d.slot_utc as f64,
                        wsjt,
                        key: None,
                    });
                }
            }
        }
        self.wide_spots.truncate(WIDE_SPOTS);
    }

    /// The decoder's slots for the scope's slot lines (decision 51): the
    /// mode's period and the first slot it takes whole, while a mode with
    /// slots runs; `None` otherwise (keyboard modes, pictures, links).
    /// The mode the decoder is running, if it is.
    pub(crate) fn running_kind(&self) -> Option<Kind> {
        self.worker.as_ref().map(|(k, _)| *k)
    }

    pub(crate) fn slots(&self) -> Option<crate::scope::DecodeSlots> {
        let (kind, _) = self.worker.as_ref()?;
        slots_of(*kind, self.started_utc?)
    }

    /// Whether the radio is keyed now (from the rig's status, each frame):
    /// slots it was keyed in are dropped.
    pub(crate) fn note_keyed(&mut self, utc: f64, keyed: bool) {
        self.keyed.note(utc, keyed);
    }

    fn clear(&mut self) {
        self.keyed_drop = None;
        self.rows.clear();
        self.text.clear();
        self.image = None;
        self.tex = None;
        self.notes.clear();
        self.last_slot = None;
        self.selected.clear();
        self.anchor = None;
        self.wide_spots.clear();
        self.jtty_rows.clear();
    }

    /// Once a frame: start, retune or stop the decoder as asked, and take
    /// what it heard.
    pub(crate) fn frame(
        &mut self,
        s: &DecodeSettings,
        st: &StationSettings,
        rx: &Rx,
        at: Where,
        wsjtx: Option<&WsjtxUdp>,
    ) {
        // The uploaders follow their ticks whether or not anything decodes.
        self.spots.frame(&s.spots, me(st).as_ref());
        self.keys_frame(s, rx, at, s.running && rx.active());
        self.skim_log_frame(at);
        self.autoid_frame(st, rx, s.running && rx.active());
        self.dials.note(decoders::slot::utc_now(), at.dial_hz);
        let taps = &rx.taps;
        if !s.running || !rx.active() {
            if self.worker.is_some() {
                self.stop(taps);
            }
            self.error = (s.running && !rx.active())
                .then(|| "No receiver: choose the I/Q source under SETTINGS.".to_string());
            return;
        }
        self.error = None;
        let same = self.worker.as_ref().is_some_and(|(k, _)| *k == s.kind);
        if !same {
            self.stop(taps);
            self.clear();
            self.worker = self.spawn(s, st, rx, at, taps).map(|w| (s.kind, w));
        }
        // Retune what can be retuned in place.
        match self.worker.as_mut() {
            Some((_, Worker::Jtty(w, f0))) => {
                let want = s.audio_hz as f32;
                if want != *f0 {
                    w.retune(want);
                    *f0 = want;
                }
            }
            Some((_, Worker::Text(w, m))) => {
                if let Some(want) = s.text_mode()
                    && want != *m
                {
                    w.retune(want);
                    *m = want;
                }
            }
            Some((_, Worker::Picture(w, m))) => {
                if let Some(want) = s.image_mode()
                    && want != *m
                {
                    w.command(ImageCmd::Mode(want));
                    *m = want;
                }
            }
            Some((_, Worker::Link(w, cfg))) => {
                let want = link_config(cfg.mode, rx, at);
                if want.iq_rate > 0.0 && (want.iq_rate != cfg.iq_rate || want.tuning != cfg.tuning)
                {
                    match want.check() {
                        Ok(()) => w.reconfigure(want.clone()),
                        Err(e) => self.error = Some(e),
                    }
                    *cfg = want;
                }
            }
            _ => {}
        }
        self.take(at, wsjtx);
        self.wide_frame(s, st, rx, at);
    }

    fn spawn(
        &mut self,
        s: &DecodeSettings,
        st: &StationSettings,
        rx: &Rx,
        at: Where,
        taps: &rx::Taps,
    ) -> Option<Worker> {
        match s.kind.family() {
            Family::Wsjt => {
                let mode = s.kind.wsjt()?;
                let mut cfg = WsjtConfig::new(mode);
                // Hashed messages naming the operator resolve from the start.
                cfg.my_calls.extend(st.call());
                tracing::info!(
                    "{} decoder started, receiver at {:.0} Hz",
                    mode.label(),
                    at.dial_hz
                );
                let w = Arc::new(WsjtDecoder::spawn(cfg));
                self.started_utc = Some(decoders::slot::utc_now());
                let f = w.clone();
                taps.set_audio("decode", Some(Box::new(move |a, utc| f.push(a, utc))));
                Some(Worker::Wsjt(w, mode))
            }
            Family::Text => {
                let mode = s.text_mode()?;
                let w = Arc::new(TextDecoder::spawn(mode, sdr::AUDIO_RATE));
                let f = w.clone();
                taps.set_audio("decode", Some(Box::new(move |a, utc| f.push(a, utc))));
                Some(Worker::Text(w, mode))
            }
            Family::Chat => {
                let f0 = s.audio_hz as f32;
                tracing::info!("JTTY decoder started, listening at {f0:.0} Hz");
                let w = Arc::new(JttyDecoder::spawn(sdr::AUDIO_RATE, f0));
                let f = w.clone();
                taps.set_audio("decode", Some(Box::new(move |a, utc| f.push(a, utc))));
                Some(Worker::Jtty(w, f0))
            }
            Family::Picture => {
                let mode = s.image_mode()?;
                let w = Arc::new(ImageDecoder::spawn(mode, sdr::AUDIO_RATE));
                let f = w.clone();
                taps.set_audio("decode", Some(Box::new(move |a, utc| f.push(a, utc))));
                Some(Worker::Picture(w, mode))
            }
            Family::Link => {
                let cfg = link_config(s.kind.link()?, rx, at);
                if let Err(e) = cfg.check() {
                    self.error = Some(e);
                }
                match LinkWorker::spawn(cfg.clone()) {
                    Ok(w) => {
                        let w = Arc::new(w);
                        let f = w.clone();
                        taps.set_iq(
                            "decode",
                            Some(Box::new(move |b| f.push_interleaved(b.iq, b.utc))),
                        );
                        Some(Worker::Link(w, cfg))
                    }
                    Err(e) => {
                        self.error = Some(e);
                        None
                    }
                }
            }
        }
    }

    /// Take what the decoder heard since last frame.
    fn take(&mut self, at: Where, wsjtx: Option<&WsjtxUdp>) {
        let Some((_, w)) = &self.worker else { return };
        match w {
            Worker::Wsjt(w, mode) => {
                let mode = *mode;
                let slots: Vec<_> = w.results().try_iter().collect();
                for slot in slots {
                    // On the dial it was heard on; heard across a retune,
                    // half one place and half another, it is dropped.
                    let t0 = slot.slot_utc as f64;
                    if self.keyed.spoils(t0, mode.period_s()) {
                        if !slot.live {
                            tracing::info!(
                                "{} slot {} dropped: the radio was transmitting",
                                mode.label(),
                                hms(t0)
                            );
                            self.keyed_drop = Some(slot.slot_utc);
                        }
                        continue;
                    }
                    match self.dials.during(t0, t0 + mode.period_s()) {
                        Some(dial_hz) => {
                            self.wsjt_slot(&slot, mode, Where { dial_hz, ..at }, wsjtx)
                        }
                        None => tracing::info!(
                            "{} slot {} dropped: the dial moved during it",
                            mode.label(),
                            hms(t0)
                        ),
                    }
                }
            }
            Worker::Text(w, _) => {
                for e in w.events().try_iter() {
                    self.text.push_str(&e.text);
                }
                if self.text.len() > TEXT {
                    let cut = self.text.len() - TEXT;
                    let cut = (cut..self.text.len())
                        .find(|&i| self.text.is_char_boundary(i))
                        .unwrap_or(0);
                    self.text.drain(..cut);
                }
            }
            Worker::Jtty(w, _) => {
                let events: Vec<_> = w.events().try_iter().collect();
                for e in events {
                    let r = Row {
                        id: 0,
                        utc: e.utc,
                        snr: None,
                        dt: None,
                        audio_hz: Some(e.audio_hz),
                        rf_hz: Some(at.rf_of_audio(e.audio_hz as f64)),
                        radio_hz: Some((at.radio_of_audio(e.audio_hz as f64), at.dial_hz)),
                        text: if e.complete { e.text } else { format!("{} …", e.text) },
                        call: e.call,
                        grid: None,
                    };
                    if e.complete {
                        self.skim_log.add(skimlog::Line {
                            utc: r.utc,
                            mode: "JTTY",
                            source: skimlog::Source::Receiver,
                            rf_hz: r.rf_hz.unwrap_or(0.0),
                            snr: 0.0,
                            call: r.call.clone(),
                            text: r.text.clone(),
                            track: None,
                            live: false,
                        });
                    }
                    // One line a message, rewritten as it grows.
                    let line = self.jtty_rows.iter().find(|(m, _)| *m == e.id).map(|&(_, id)| id);
                    match line.and_then(|id| self.rows.iter_mut().find(|x| x.id == id)) {
                        Some(x) => *x = Row { id: x.id, ..r },
                        None => {
                            push_row(&mut self.rows, &mut self.next_id, r);
                            self.jtty_rows.push_front((e.id, self.next_id));
                            self.jtty_rows.truncate(ROWS);
                        }
                    }
                }
            }
            Worker::Picture(w, _) => {
                for e in w.events().try_iter() {
                    match e.update {
                        ImageUpdate::Started { label, width, height } => {
                            self.image = Some(RgbaImage::new(width, height));
                            self.image_label = format!("{label} · started {}", hms(e.utc));
                            self.image_dirty = true;
                        }
                        ImageUpdate::Row { y } => {
                            if let (Some(img), Some(row)) = (self.image.as_mut(), e.row) {
                                if y >= img.height {
                                    img.set_height(y + 1);
                                }
                                let w4 = img.width as usize * 4;
                                let at = y as usize * w4;
                                if row.len() == w4 && at + w4 <= img.pixels.len() {
                                    img.pixels[at..at + w4].copy_from_slice(&row);
                                    self.image_dirty = true;
                                }
                            }
                        }
                        ImageUpdate::Complete(img) => {
                            self.image = Some(img);
                            self.image_label.push_str(&format!(" · finished {}", hms(e.utc)));
                            self.image_dirty = true;
                        }
                        ImageUpdate::Note(n) => {
                            self.notes.push(format!("{} {n}", hms(e.utc)));
                            if self.notes.len() > 20 {
                                self.notes.remove(0);
                            }
                        }
                    }
                }
            }
            Worker::Link(w, _) => {
                for e in w.events().try_iter() {
                    match e {
                        LinkEvent::Decoded(d) => push_row(
                            &mut self.rows,
                            &mut self.next_id,
                            Row {
                                id: 0,
                                utc: d.utc,
                                snr: d.snr_db,
                                dt: None,
                                audio_hz: None,
                                rf_hz: Some(d.rf_hz as f64),
                                radio_hz: None,
                                text: d.summary.clone(),
                                call: None,
                                grid: None,
                            },
                        ),
                        LinkEvent::Refused(why) => self.error = Some(why),
                    }
                }
            }
        }
    }

    /// One WSJT slot: its rows, the stations' grids, the decode log, the
    /// outside programs and the spots.
    fn wsjt_slot(
        &mut self,
        slot: &decoders::WsjtSlot,
        mode: WsjtMode,
        at: Where,
        wsjtx: Option<&WsjtxUdp>,
    ) {
        // MSK144's live reports during a slot leave the slot's figure alone.
        if !slot.live {
            self.last_slot = Some((slot.slot_utc, slot.decodes.len(), slot.decode_ms));
        }
        let dial = at.dial_hz + at.xvtr_hz;
        let mut lines = Vec::new();
        let mut psk = Vec::new();
        let mut wspr = Vec::new();
        for d in &slot.decodes {
            let rf = at.rf_of_audio(d.audio_hz as f64);
            let (call, grid) = match &d.wspr {
                Some(w) => (
                    station::is_callsign(&w.callsign).then(|| w.callsign.clone()),
                    (!w.grid.is_empty()).then(|| w.grid.clone()),
                ),
                None => station::sender_of(&d.message),
            };
            if let (Some(c), Some(g)) = (&call, &grid) {
                self.heard.note(c, g, d.slot_utc as f64);
            }
            let row = Row {
                id: 0,
                utc: d.slot_utc as f64,
                snr: Some(d.snr_db),
                dt: Some(d.dt_s),
                audio_hz: Some(d.audio_hz),
                rf_hz: Some(rf),
                radio_hz: Some((at.radio_of_audio(d.audio_hz as f64), at.dial_hz)),
                text: d.message.clone(),
                call: call.clone(),
                grid: grid.clone(),
            };
            // MSK144 reports a message again when it is heard better: that
            // updates its line, and it is logged and sent only the first time.
            if mode == WsjtMode::Msk144 && update_row(&mut self.rows, &row) {
                continue;
            }
            lines.push(decodelog::all_txt_line(
                d.slot_utc,
                dial,
                mode.label(),
                d.snr_db,
                d.dt_s,
                d.audio_hz,
                &d.message,
            ));
            match (&d.wspr, mode) {
                (Some(w), WsjtMode::Wspr) => {
                    wspr.push(wsprnet::Spot {
                        call: w.callsign.clone(),
                        grid: w.grid.clone(),
                        snr_db: d.snr_db.round() as i32,
                        dt_s: d.dt_s,
                        drift_hz: w.drift_hz.round() as i32,
                        freq_hz: rf,
                        power_dbm: w.power_dbm,
                    });
                    if self.wspr_feed.len() >= 2_000 {
                        self.wspr_feed.drain(..1_000);
                    }
                    self.wspr_feed.push(WsprHeard {
                        slot_utc: d.slot_utc,
                        snr_db: d.snr_db,
                        drift_hz: w.drift_hz,
                        audio_hz: d.audio_hz,
                        call: w.callsign.clone(),
                        grid: w.grid.clone(),
                        dbm: w.power_dbm,
                        rf_hz: rf,
                    });
                }
                (_, WsjtMode::Ft8 | WsjtMode::Ft4) => {
                    if let Some(c) = &call {
                        let known = self.heard.get(c).map(|h| h.grid.clone());
                        psk.push(pskreporter::Report {
                            call: c.clone(),
                            grid: grid.clone().or(known).unwrap_or_default(),
                            freq_hz: rf.max(0.0).round() as u64,
                            snr_db: d.snr_db.round().clamp(-128.0, 127.0) as i8,
                            mode: mode.label().into(),
                            utc: d.slot_utc.max(0) as u32,
                        });
                    }
                }
                _ => {}
            }
            self.skim_log.add(skimlog::Line {
                utc: row.utc,
                mode: mode.label(),
                source: skimlog::Source::Receiver,
                rf_hz: rf,
                snr: d.snr_db,
                call: row.call.clone(),
                text: row.text.clone(),
                track: None,
                live: false,
            });
            if let Some(c) = &row.call {
                // The id push_row gives it.
                self.globe_feed.extend(reception(
                    &self.heard,
                    c,
                    row.grid.as_deref(),
                    rf,
                    mode.label(),
                    Some(d.snr_db),
                    row.utc,
                    crate::globe::field::Source::Decode,
                    Some(self.next_id + 1),
                ));
            }
            if mode == WsjtMode::Ft8 && !slot.live {
                if self.tx_feed.len() >= 2_000 {
                    self.tx_feed.drain(..1_000);
                }
                self.tx_feed.push(TxHeard {
                    slot_utc: d.slot_utc,
                    snr_db: d.snr_db,
                    dt: d.dt_s,
                    audio_hz: d.audio_hz,
                    text: d.message.clone(),
                    rf_hz: rf,
                    row: self.next_id + 1,
                });
            }
            push_row(&mut self.rows, &mut self.next_id, row);
            if let Some(u) = wsjtx {
                send_wsjtx(u, mode, d, rf);
            }
        }
        self.log.append(&lines);
        match mode {
            WsjtMode::Ft8 | WsjtMode::Ft4 => self.spots.ft_slot(psk),
            WsjtMode::Wspr => self.spots.wspr_slot(wsprnet::Slot {
                utc: slot.slot_utc,
                dial_hz: dial,
                spots: wspr,
            }),
            _ => {}
        }
        if let Some(u) = wsjtx {
            u.status(&links::StatusInfo {
                dial_hz: dial.max(0.0) as u64,
                mode: mode.label().into(),
                decoding: true,
                tr_period_s: mode.period_s() as u32,
                ..links::StatusInfo::default()
            });
        }
    }
}

/// The data link's set-up for where the I/Q is now: the converter's LO is the
/// radio window's transverter offset, the ppm the scope's calibration.
fn link_config(mode: LinkMode, rx: &Rx, at: Where) -> LinkConfig {
    let tuning = Tuning {
        centre_hz: rx.centre(),
        lo_hz: at.xvtr_hz,
        cal: decoders::FreqCal { ppm: at.ppm },
    };
    LinkConfig::new(mode, rx.rate(), tuning)
}

/// The skimmer's labels at `now` from DECODE's lines (newest first) and the
/// wide skimmer's decodes: those of the last few slots that name who sent
/// them, at the radio's own frequency.
fn skim_feed(
    rows: &VecDeque<Row>,
    wide: &VecDeque<SkimSpot>,
    mode: WsjtMode,
    now: f64,
) -> SkimFeed {
    let period_s = mode.period_s();
    let since = now - (crate::scope::skim::SLOTS_SHOWN as f64 + 1.0) * period_s;
    let own = rows.iter().take_while(|r| r.utc >= since).filter_map(|r| {
        let (hz, dial_hz) = r.radio_hz?;
        Some(SkimSpot {
            row: Some(r.id),
            hz,
            audio_hz: r.audio_hz?,
            dial_hz,
            call: r.call.clone()?,
            text: r.text.clone(),
            snr: r.snr.unwrap_or(0.0),
            utc: r.utc,
            wsjt: Some(mode),
            key: None,
        })
    });
    // SKIM WIDE's, each by its own mode's slots (FT8's beside FT4).
    let wide = wide.iter().filter(|w| {
        let p = w.wsjt.map_or(period_s, |m| m.period_s());
        w.utc >= now - (crate::scope::skim::SLOTS_SHOWN as f64 + 1.0) * p
    });
    let wide = wide.cloned();
    SkimFeed { period_s, spots: own.chain(wide).collect(), ..Default::default() }
}

/// A keyboard skimmer's signal as a scope label: those with a callsign read.
fn key_spot(k: &KeySpot) -> Option<SkimSpot> {
    Some(SkimSpot {
        row: None,
        hz: k.hz,
        audio_hz: 0.0,
        dial_hz: k.hz,
        call: k.call.clone()?,
        text: k.text.clone(),
        snr: k.snr_db,
        utc: k.heard_utc,
        wsjt: None,
        key: Some(KeyTag { mode: k.mode, wpm: k.wpm, active: k.active }),
    })
}

/// The WSJT modes' sub-bands, the radio's own Hz (a converter's LO off the
/// dials as shown): each dial and the 3 kHz above it, where the keyboard
/// skimmer would read FT8 and the like as PSK31 garbage.
fn wsjt_bands(xvtr_hz: f64) -> Vec<(f64, f64)> {
    digimodes::DIGIS
        .iter()
        .filter(|d| Kind::of_digi(d).is_some_and(|k| k.wsjt().is_some() || k == Kind::Jtty))
        .flat_map(|d| d.dials.iter())
        .map(|&f| {
            let lo = f as f64 - xvtr_hz;
            (lo, lo + 3000.0)
        })
        .collect()
}

/// The wide skimmers' modes and dials for DECODE's `kind`, with the
/// receiver at `dial_hz` in a span `rate` wide around `centre` (all the
/// radio's own Hz; the modes' dials are as shown, so the converter's
/// `xvtr_hz` comes off them): every other dial of FT8 or FT4 whose lane is
/// in the span, then the other mode's dials there. Empty with SKIM or SKIM
/// WIDE off, another mode, or no dial in reach.
fn wide_dials(
    kind: Kind,
    s: &DecodeSettings,
    dial_hz: f64,
    xvtr_hz: f64,
    centre: f64,
    rate: f64,
) -> Vec<(WsjtMode, Vec<f64>)> {
    // In FT8 or FT4 with SKIM WIDE, the dials besides the receiver's. In
    // any other mode with SKIM on, every FT8 and FT4 dial in the span, the
    // receiver's too, so their labels stay (AJ3G: "Even if I click on PSK,
    // you should still continue to demod and report other modes"). WSPR is
    // skimmed the same way, beside FT8 and FT4 (AJ3G, 8 Nov 2026: "Can it be
    // made so WSPR Spots are also SKIMMED?"): its dial in the span, but not
    // the receiver's while DECODE itself runs WSPR there.
    let modes: &[Kind] = match kind {
        Kind::Ft8 if s.skim_wide => &[Kind::Ft8, Kind::Ft4, Kind::Wspr],
        Kind::Ft4 if s.skim_wide => &[Kind::Ft4, Kind::Ft8, Kind::Wspr],
        Kind::Ft8 | Kind::Ft4 => return Vec::new(),
        _ => &[Kind::Ft8, Kind::Ft4, Kind::Wspr],
    };
    if !s.skim || rate <= 0.0 {
        return Vec::new();
    }
    modes
        .iter()
        .filter_map(|&k| {
            let skip = (k == kind).then_some(dial_hz);
            let all: Vec<f64> = k.digi()?.dials.iter().map(|&d| d as f64 - xvtr_hz).collect();
            let dials = decoders::skim::dials_in_span(&all, centre, rate, skip);
            (!dials.is_empty()).then_some((k.wsjt()?, dials))
        })
        .collect()
}

fn push_row(rows: &mut VecDeque<Row>, next_id: &mut u64, mut r: Row) {
    *next_id += 1;
    r.id = *next_id;
    rows.push_front(r);
    rows.truncate(ROWS);
}

/// Replace the line with `r`'s time and text, if there is one.
fn update_row(rows: &mut VecDeque<Row>, r: &Row) -> bool {
    match rows.iter_mut().find(|x| x.utc == r.utc && x.text == r.text) {
        Some(x) => {
            *x = Row { id: x.id, ..r.clone() };
            true
        }
        None => false,
    }
}

/// The operator, if both callsign and grid are set.
fn me(st: &StationSettings) -> Option<spots::Me> {
    Some(spots::Me { call: st.call()?, grid: st.grid()? })
}

fn send_wsjtx(u: &WsjtxUdp, mode: WsjtMode, d: &decoders::WsjtDecode, rf_hz: f64) {
    match (&d.wspr, mode) {
        (Some(w), WsjtMode::Wspr) => u.wspr_decode(&links::WsprDecodeInfo {
            new: true,
            slot_utc: d.slot_utc,
            snr_db: d.snr_db.round() as i32,
            dt: d.dt_s as f64,
            freq_hz: rf_hz.max(0.0).round() as u64,
            drift_hz: w.drift_hz.round() as i32,
            callsign: w.callsign.clone(),
            grid: w.grid.clone(),
            power_dbm: w.power_dbm,
        }),
        _ => u.decode(&links::DecodeInfo {
            new: true,
            slot_utc: d.slot_utc,
            snr_db: d.snr_db.round() as i32,
            dt: d.dt_s as f64,
            audio_hz: d.audio_hz.max(0.0).round() as u32,
            mode: mode.wsjtx_code().into(),
            message: d.message.clone(),
            low_confidence: false,
        }),
    }
}

/// `hh:mm:ss` UTC.
/// A mode's slots, its decoder having started hearing at `started_utc`: the
/// period, the first slot the decoder takes whole ([`WsjtConfig::first_slot`])
/// and where an on-time transmission ends in each
/// ([`WsjtMode::signal_end_s`]). `None` for a mode without slots.
fn slots_of(kind: Kind, started_utc: f64) -> Option<crate::scope::DecodeSlots> {
    let cfg = WsjtConfig::new(kind.wsjt()?);
    Some(crate::scope::DecodeSlots {
        period_s: cfg.mode.period_s(),
        first_utc: cfg.first_slot(started_utc),
        end_s: cfg.mode.signal_end_s(),
    })
}

pub(crate) fn hms(utc: f64) -> String {
    let s = utc.rem_euclid(86_400.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// AUTO ID (decision 61): its key, and while it listens what it has
/// measured and its guesses, best first, each with USE where DECODE has the
/// mode.
/// Returns the mode USE picked, if one was.
fn autoid_box(ui: &mut egui::Ui, d: &mut Decode, s: &mut DecodeSettings, rx: &Rx) -> Option<Kind> {
    ui.horizontal_wrapped(|ui| {
        let r = chrome::chip(ui, d.autoid_on, "AUTO ID").on_hover_text(
            "Put the receiver's passband over one signal and press: it listens to the audio \
             inside the passband, measures its width, tones, shift and keying speed, tries \
             every decoder that could fit, and lists what it most likely is. USE switches \
             DECODE to that mode there. Move the passband or the dial and it starts again",
        );
        if r.clicked() {
            d.autoid_on = !d.autoid_on;
        }
        if d.autoid_on && d.autoid.is_none() {
            ui.label(RichText::new("Starts with the receiver").size(11.5).color(theme::TEXT()));
        }
    });
    let Some(run) = &d.autoid else { return None };
    let rep = &run.report;
    let mut info =
        format!("Listening {:.0} s to {:.0}–{:.0} Hz", rep.heard_s, run.band.0, run.band.1);
    match &rep.features {
        Some(f) => info.push_str(&format!(" · {}", decoders::autoid::describe(f))),
        None if rep.heard_s < 6.0 => info.push_str(" · measuring after 6 s"),
        None => info.push_str(" · no signal found in the passband"),
    }
    ui.label(RichText::new(info).size(11.5).color(theme::TEXT()));
    let rx_hz = rx.rx_hz;
    let mut used = None;
    egui::Grid::new("autoid-list").num_columns(4).spacing([10.0, 3.0]).striped(true).show(
        ui,
        |ui| {
            for (i, c) in rep.candidates.iter().enumerate() {
                let name = RichText::new(c.name).size(12.5);
                ui.label(if i == 0 {
                    name.color(theme::TEXT_STRONG()).strong()
                } else {
                    name.color(theme::TEXT())
                });
                ui.label(
                    RichText::new(format!("{:.0} %", 100.0 * c.score))
                        .size(12.0)
                        .color(theme::TEXT()),
                );
                let mut why = c.why.clone();
                if let Some(t) = c.decoded.first() {
                    let t: String = t.trim().chars().take(60).collect();
                    why.push_str(&format!(" · read: {t}"));
                }
                ui.label(RichText::new(why).size(11.5).color(theme::TEXT()));
                match c.pick.and_then(kind_of_pick) {
                    Some(k) => {
                        if chrome::chip(ui, false, "USE")
                            .on_hover_text(format!("Decode {} here", k.0.label()))
                            .clicked()
                        {
                            used = Some(k);
                        }
                    }
                    None => {
                        ui.label(RichText::new("no decoder").size(11.0).weak());
                    }
                }
                ui.end_row();
            }
        },
    );
    if rep.candidates.is_empty() && rep.features.is_some() {
        ui.label(RichText::new("Nothing it knows fits yet.").weak());
    }
    if let Some((kind, hz, reverse)) = used {
        tracing::info!("AUTO ID: {} picked at {:.0} Hz", kind.label(), rx_hz);
        s.kind = kind;
        if let Some(hz) = hz {
            s.audio_hz = hz.round();
        }
        s.reverse = reverse;
        d.autoid_on = false;
        return Some(kind);
    }
    None
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

/// What the operator asked of the window this frame.
#[derive(Default)]
pub(crate) struct DecodeOut {
    /// A mode was picked: snap to it, if `snap` is on.
    pub picked: Option<Kind>,
    /// AUTO ID's USE picked a mode where the receiver already is: show it on
    /// the scope's DATA row, without retuning.
    pub identified: Option<Kind>,
    /// Pop out (`true`) or dock (`false`).
    pub pop: Option<bool>,
}

/// Draw the window, over the panel or popped out into one of its own (as
/// RADIO and SCOPE do). Clears `open` when it is closed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn window(
    ctx: &egui::Context,
    open: &mut bool,
    popped: bool,
    d: &mut Decode,
    s: &mut DecodeSettings,
    st: &StationSettings,
    rx: &Rx,
    wsjtx_on: bool,
) -> DecodeOut {
    let mut out = DecodeOut::default();
    if popped {
        // Painted in place: no background GPU submit meanwhile (gpu_gate).
        let _shut = crate::gpu_gate::shut();
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("decode-window"),
            crate::popout::builder(ctx, "decode-window", "Decode", [720.0, 640.0], [420.0, 320.0]),
            |ui, _class| {
                crate::popout::track(ui.ctx(), "decode-window");
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        body(ui, true, d, s, st, rx, wsjtx_on, &mut out);
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        egui::Window::new("Decode")
            .id(egui::Id::new("decode-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([(ctx.content_rect().width() - 660.0).max(20.0), 120.0])
            .default_size([640.0, 560.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                body(ui, false, d, s, st, rx, wsjtx_on, &mut out);
            });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut egui::Ui,
    popped: bool,
    d: &mut Decode,
    s: &mut DecodeSettings,
    st: &StationSettings,
    rx: &Rx,
    wsjtx_on: bool,
    out: &mut DecodeOut,
) {
    ui.horizontal(|ui| {
        chrome::checkbox(ui, &mut s.snap, "Snap the radio to the mode").on_hover_text(
            "Picking a mode takes the radio and the panel's receiver to its frequency on this \
             band (or the nearest), in its sideband, with the radio's filter and the receiver's \
             passband set for it. Never while transmitting.",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (text, hover) = if popped {
                ("DOCK", "Put DECODE back over the panel")
            } else {
                ("POP OUT", "Open DECODE in a window of its own")
            };
            if chrome::chip(ui, false, text).on_hover_text(hover).clicked() {
                out.pop = Some(!popped);
            }
        });
    });
    for (group, kinds) in Kind::GROUPS {
        ui.horizontal_wrapped(|ui| {
            caption(ui, group);
            for &k in kinds {
                let hover = match k.digi() {
                    Some(g) => format!(
                        "{}: {} with a {}–{} Hz passband{}",
                        k.label(),
                        if g.side == digimodes::Side::Voice { "LSB or USB" } else { g.side_name() },
                        g.lo,
                        g.hi,
                        if s.snap { ", at its frequency on this band" } else { "" }
                    ),
                    None => format!("{}: no calling frequency, tune to it by hand", k.label()),
                };
                let hover = if k == Kind::Msk144 {
                    hover
                        + ". Calling is on 50.260 MHz; a CQ with a number asks for answers \
                           there (CQ 265 …: 50.265 MHz)"
                } else {
                    hover
                };
                if chrome::chip(ui, s.kind == k, k.label()).on_hover_text(hover).clicked() {
                    s.kind = k;
                    out.picked = Some(k);
                }
            }
        });
    }
    ui.horizontal_wrapped(|ui| match s.kind.family() {
        Family::Chat => {
            caption(ui, "Rx");
            ui.add(
                egui::DragValue::new(&mut s.audio_hz)
                    .range(300.0..=2800.0)
                    .speed(5.0)
                    .suffix(" Hz"),
            )
            .on_hover_text(
                "The audio frequency listened at most closely, as WSJT-X's Rx frequency; \
                 the rest of 200 to 2800 Hz is searched too",
            );
            if chrome::chip(ui, false, "Clear").clicked() {
                d.rows.clear();
                d.selected.clear();
            }
        }
        Family::Text => {
            if s.kind == Kind::Cw {
                ui.label(
                    RichText::new("TONE: THE STRONGEST, 200 TO 1500 HZ; SPEED FOUND")
                        .size(10.5)
                        .color(theme::CYAN_DIM())
                        .strong(),
                )
                .on_hover_text(
                    "CW is read on whichever tone is strongest in the receiver's \
                         passband, at whatever speed it is sent. Put one station in the \
                         passband (CW mode, or a narrow filter) for clean copy.",
                );
            } else if s.kind != Kind::Navtex {
                caption(ui, "Audio");
                ui.add(
                    egui::DragValue::new(&mut s.audio_hz)
                        .range(300.0..=3000.0)
                        .speed(5.0)
                        .suffix(" Hz"),
                );
            }
            if matches!(s.kind, Kind::Rtty | Kind::Navtex) {
                chrome::checkbox(ui, &mut s.reverse, "Reverse");
            }
            if chrome::chip(ui, false, "Clear").clicked() {
                d.text.clear();
            }
        }
        Family::Picture if s.kind == Kind::Wefax => {
            for l in Lpm::ALL {
                if chrome::chip(ui, s.lpm == l.value(), l.label()).clicked() {
                    s.lpm = l.value();
                }
            }
            chrome::checkbox(ui, &mut s.ioc_288, "IOC 288");
            if let Some((_, Worker::Picture(w, _))) = &d.worker {
                if chrome::chip(ui, false, "Start now").clicked() {
                    w.command(ImageCmd::Start);
                }
                if chrome::chip(ui, false, "Stop").clicked() {
                    w.command(ImageCmd::Stop);
                }
            }
        }
        Family::Wsjt | Family::Link => {
            if chrome::chip(ui, false, "Clear").clicked() {
                d.rows.clear();
                d.selected.clear();
            }
            let n = d.selected.len();
            let copy = ui
                .add_enabled_ui(n > 0, |ui| chrome::chip(ui, false, format!("COPY {n}")))
                .inner
                .on_hover_text("Copy the lines picked (click, Ctrl+click, Shift+click); Ctrl+C too")
                .clicked();
            if copy {
                ui.ctx().copy_text(copy_text(d, s.kind.family(), st, true));
            }
            let all = chrome::chip(ui, false, "COPY ALL")
                .on_hover_text("Copy every line shown, newest first")
                .clicked();
            if all {
                ui.ctx().copy_text(copy_text(d, s.kind.family(), st, false));
            }
            if s.kind.family() == Family::Wsjt {
                let r = chrome::chip(ui, s.skim, "SKIM").on_hover_text(
                    "Label each station heard on the scope, by its callsign at its frequency, \
                     on the spectrum and the waterfall; the labels fade over three slots. \
                     Click a label to pick its line here",
                );
                if r.clicked() {
                    s.skim = !s.skim;
                }
                let can = s.skim && matches!(s.kind, Kind::Ft8 | Kind::Ft4);
                let r = ui
                    .add_enabled_ui(can, |ui| chrome::chip(ui, s.skim_wide && can, "SKIM WIDE"))
                    .inner
                    .on_hover_text(
                        "Also decode the mode on every other one of its dials in the I/Q span \
                         (FT8 or FT4, up to 8 dials: a wide SPAN takes in other bands), and \
                         label those stations on the scope too. Click one of those labels to \
                         tune there. Their lines are not added here",
                    )
                    .on_disabled_hover_text("With SKIM on, for FT8 and FT4");
                if r.clicked() {
                    s.skim_wide = !s.skim_wide;
                }
            }
        }
        _ => {}
    });
    ui.horizontal_wrapped(|ui| {
        let r = chrome::chip(ui, s.skim_keys, "SKIM CW/PSK/RTTY").on_hover_text(
            "Find every CW, PSK31 and RTTY signal in the I/Q window, read it, and label it \
             on the scope by the callsign it sends, with its mode; the labels fade a few \
             seconds after the signal stops. Hover a label for its text, click it to listen \
             there in its mode. Whatever this window decodes; nothing is sent anywhere",
        );
        if r.clicked() {
            s.skim_keys = !s.skim_keys;
        }
        if s.skim_keys {
            caption(ui, "CW WPM");
            let (lo, hi) = &mut s.skim_cw_wpm;
            // Two numbers with "to" between them: a dash inside the first box
            // read as a minus sign (AJ3G, 31 Oct 2026: "6-35").
            ui.add(egui::DragValue::new(lo).range(6..=*hi))
                .on_hover_text("The slowest CW the skimmer reads and labels, WPM");
            ui.label("to");
            ui.add(egui::DragValue::new(hi).range(*lo..=70))
                .on_hover_text("The fastest CW the skimmer reads and labels, WPM");
        }
        let r = chrome::chip(ui, s.skim_log, "SKIM LOG").on_hover_text(
            "Open a window listing everything heard, of every mode, with the mode and where \
             it was heard: the receiver's own decodes, SKIM WIDE's FT8 and FT4 on the other \
             dials, and SKIM CW/PSK/RTTY's signals. Also kept in SKIM.TXT beside ALL.TXT",
        );
        if r.clicked() {
            s.skim_log = !s.skim_log;
        }
        if let Some(k) = &d.keys {
            let st = k.state();
            ui.label(
                RichText::new(format!(
                    "{} signals tracked · {} read · {:.0} % of a core",
                    st.tracks,
                    st.spots.iter().filter(|p| p.call.is_some()).count(),
                    100.0 * st.load
                ))
                .size(11.5)
                .color(theme::TEXT()),
            );
        }
    });
    // The scope's DATA row lights the mode USE picked; AUTO ID found it where
    // the receiver is, so the radio isn't taken to its calling frequency
    // (AJ3G, 31 Oct 2026: the DATA row didn't follow USE).
    if let Some(k) = autoid_box(ui, d, s, rx) {
        out.identified = Some(k);
    }
    if s.kind.family() == Family::Wsjt
        && let Some(mode) = s.kind.wsjt()
    {
        let started = d.worker.is_some().then_some(d.started_utc).flatten();
        let last = d.last_slot.map(|(slot, n, _)| (slot, n));
        let light =
            slotclock::light(decoders::slot::utc_now(), mode.period_s(), started, 1.0, last);
        slotclock::row(ui, decoders::slot::utc_now(), mode.period_s(), light);
    }
    // In every mode: RBN takes the CW skimmer's stations and WSPRnet the
    // skimmed WSPR whatever DECODE is on, and with the box shown only for
    // the WSJT modes its ticks could not be found from CW (AJ3G, 8 Nov
    // 2026: "There is no checkbox").
    spots::spots_box(ui, &mut s.spots, &d.spots, me(st).as_ref());

    // Where it listens, and what it has dropped.
    let src = match s.kind.family() {
        Family::Link => {
            format!("I/Q at {:.0} kHz, centre {:.3} MHz", rx.rate() / 1e3, rx.centre() / 1e6)
        }
        _ => format!("receiver audio, 48 kHz · {} at {:.3} kHz", rx.mode.label(), rx.rx_hz / 1e3),
    };
    let dropped = d.worker.as_ref().map_or(0, |(_, w)| w.dropped())
        + d.wide.iter().map(|w| w.dropped()).sum::<u64>()
        + d.keys.as_ref().map_or(0, |k| k.dropped());
    let mut line = format!("From the {src} · {dropped} blocks dropped");
    for w in &d.wide {
        let dials: Vec<String> = w.dials().iter().map(|&h| format!("{:.3}", h / 1e6)).collect();
        line.push_str(&format!(" · skimming {} {} MHz too", w.mode().label(), dials.join(", ")));
    }
    match (d.last_slot, d.keyed_drop) {
        (last, Some(k)) if last.is_none_or(|(slot, ..)| k > slot) => {
            line.push_str(&format!(" · slot {} : transmitting · decodes dropped", hms(k as f64)));
        }
        (Some((slot, n, ms)), _) => {
            line.push_str(&format!(" · slot {} : {n} decoded in {ms} ms", hms(slot as f64)));
        }
        _ => {}
    }
    if wsjtx_on && s.kind.family() == Family::Wsjt {
        line.push_str(" · sent as WSJT-X UDP");
    }
    let log_path = d.log.path().map(|p| p.display().to_string());
    let col = if dropped > 0 { theme::YELLOW() } else { theme::TEXT() };
    let r = ui.label(RichText::new(line).size(11.5).color(col));
    if s.kind.family() == Family::Wsjt
        && let Some(p) = log_path
    {
        r.on_hover_text(format!("Every decode is also added to {p}, as WSJT-X's ALL.TXT"));
    }
    if let Some(e) = &d.log.error {
        ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
    }
    if let Some(e) = &d.error {
        ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
    }
    if let Some((n, refused)) = &d.note {
        let col = if *refused { theme::YELLOW() } else { theme::CYAN() };
        ui.label(RichText::new(n).size(11.5).color(col));
    }
    ui.separator();

    match s.kind.family() {
        Family::Wsjt | Family::Link | Family::Chat => rows(ui, d, s.kind.family(), st),
        Family::Text => {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                ui.label(RichText::new(&d.text).monospace().size(13.0));
            });
        }
        Family::Picture => picture(ui, d),
    }
}

/// A row's distance and bearing from the operator: from the grid it gave,
/// else the one its sender gave before.
fn dist_az(r: &Row, heard: &Heard, me: Option<station::LatLon>) -> Option<(f64, f64)> {
    let me = me?;
    let there = match &r.grid {
        Some(g) => station::grid_to_latlon(g),
        None => r.call.as_ref().and_then(|c| heard.get(c)).map(|h| h.pos),
    }?;
    Some((station::distance_km(me, there), station::bearing_deg(me, there)))
}

/// One row as text, in the window's columns.
fn row_text(r: &Row, fam: Family, heard: &Heard, st: &StationSettings) -> String {
    let snr = r.snr.map_or("—".into(), |v| format!("{v:+.0}"));
    let rf = r.rf_hz.map_or("—".into(), |v| format!("{:.6}", v / 1e6));
    if fam == Family::Chat {
        let af = r.audio_hz.map_or("—".into(), |v| format!("{v:.0}"));
        return format!("{}  {af:>5}  {rf:>11}  {}", hms(r.utc), r.text);
    }
    if fam != Family::Wsjt {
        return format!("{}  {snr:>4}  {rf:>11}  {}", hms(r.utc), r.text);
    }
    let dt = r.dt.map_or("—".into(), |v| format!("{v:+.1}"));
    let af = r.audio_hz.map_or("—".into(), |v| format!("{v:.0}"));
    let (dist, az) = match dist_az(r, heard, st.pos()) {
        Some((km, az)) => {
            (format!("{:.0}", st.dist_unit.of_km(km)), format!("{:03.0}", az.round() % 360.0))
        }
        None => ("—".into(), "—".into()),
    };
    format!("{}  {snr:>4}  {dt:>5}  {af:>5}  {rf:>11}  {dist:>7}  {az:>3}  {}", hms(r.utc), r.text)
}

/// The header, in the same columns.
fn header_text(fam: Family, st: &StationSettings) -> String {
    if fam == Family::Chat {
        return format!("{:8}  {:>5}  {:>11}  Message", "UTC", "AF Hz", "RF MHz");
    }
    if fam != Family::Wsjt {
        return format!("{:8}  {:>4}  {:>11}  Message", "UTC", "SNR", "RF MHz");
    }
    format!(
        "{:8}  {:>4}  {:>5}  {:>5}  {:>11}  {:>7}  {:>3}  Message",
        "UTC",
        "SNR",
        "DT",
        "AF Hz",
        "RF MHz",
        format!("DIST {}", st.dist_unit.label()),
        "AZ"
    )
}

/// The lines to copy: the picked ones, or all, newest first, under the header.
fn copy_text(d: &Decode, fam: Family, st: &StationSettings, picked: bool) -> String {
    let mut out = header_text(fam, st);
    out.push('\n');
    for r in d.rows.iter().filter(|r| !picked || d.selected.contains(&r.id)) {
        out.push_str(&row_text(r, fam, &d.heard, st));
        out.push('\n');
    }
    out
}

fn rows(ui: &mut egui::Ui, d: &mut Decode, fam: Family, st: &StationSettings) {
    let font = egui::FontId::monospace(12.5);
    // Ctrl+C copies the picked lines, when no text field has the keys.
    let copy = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
        && ui.memory(|m| m.focused().is_none());
    if copy && !d.selected.is_empty() {
        ui.ctx().copy_text(copy_text(d, fam, st, true));
    }
    ui.label(
        RichText::new(header_text(fam, st)).font(font.clone()).color(theme::CYAN_DIM()).strong(),
    );
    let line_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    let mut click: Option<(u64, egui::Modifiers)> = None;
    let mut area = egui::ScrollArea::vertical().auto_shrink(false);
    // A line picked from a scope label: scrolled to, a line above it showing.
    if let Some(i) = d.scroll_to.take().and_then(|id| d.rows.iter().position(|r| r.id == id)) {
        area = area.vertical_scroll_offset((i.saturating_sub(1)) as f32 * line_h);
    }
    area.show_rows(ui, line_h, d.rows.len(), |ui, range| {
        for (i, r) in d.rows.iter().enumerate().skip(range.start).take(range.len()) {
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, line_h), egui::Sense::click());
            let picked = d.selected.contains(&r.id);
            let bg = if picked {
                Some(theme::FILL_ACTIVE())
            } else if resp.hovered() {
                Some(theme::ROW_HOVER())
            } else if i % 2 == 1 {
                Some(theme::ROW_BG())
            } else {
                None
            };
            if let Some(bg) = bg {
                ui.painter().rect_filled(rect, 0.0, bg);
            }
            let col = if picked { theme::TEXT_STRONG() } else { theme::TEXT() };
            ui.painter().text(
                rect.left_center() + egui::vec2(2.0, 0.0),
                egui::Align2::LEFT_CENTER,
                row_text(r, fam, &d.heard, st),
                font.clone(),
                col,
            );
            if resp.clicked() {
                click = Some((r.id, ui.input(|i| i.modifiers)));
            }
        }
        if d.rows.is_empty() {
            ui.label(RichText::new("Nothing decoded yet.").weak());
        }
    });
    if let Some((id, m)) = click {
        let order: Vec<u64> = d.rows.iter().map(|r| r.id).collect();
        pick(&mut d.selected, &mut d.anchor, &order, id, m.command, m.shift);
        d.note_pick();
    }
}

/// A click on row `id`: alone picks only it (or, if it was the only one,
/// none); with Ctrl toggles it; with Shift picks the run from the last click.
fn pick(
    sel: &mut HashSet<u64>,
    anchor: &mut Option<u64>,
    order: &[u64],
    id: u64,
    ctrl: bool,
    shift: bool,
) {
    let at = |x: u64| order.iter().position(|&o| o == x);
    if shift && let (Some(a), Some(b)) = (anchor.and_then(at), at(id)) {
        if !ctrl {
            sel.clear();
        }
        sel.extend(&order[a.min(b)..=a.max(b)]);
        return;
    }
    if ctrl {
        if !sel.remove(&id) {
            sel.insert(id);
        }
    } else if sel.len() == 1 && sel.contains(&id) {
        sel.clear();
    } else {
        sel.clear();
        sel.insert(id);
    }
    *anchor = Some(id);
}

fn picture(ui: &mut egui::Ui, d: &mut Decode) {
    let due = d.image_at.is_none_or(|t| t.elapsed().as_millis() >= 250);
    if d.image_dirty
        && due
        && let Some(img) = &d.image
    {
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [img.width as usize, img.height as usize],
            &img.pixels,
        );
        match &mut d.tex {
            Some(t) if t.size() == ci.size => t.set(ci, egui::TextureOptions::LINEAR),
            _ => d.tex = Some(ui.ctx().load_texture("decode-picture", ci, Default::default())),
        }
        d.image_dirty = false;
        d.image_at = Some(Instant::now());
    }
    ui.label(RichText::new(&d.image_label).size(11.5));
    for n in &d.notes {
        ui.label(RichText::new(n).size(11.5).weak());
    }
    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| match &d.tex {
        Some(t) => {
            let [w, h] = t.size();
            let scale = (ui.available_width() / w as f32).min(1.0);
            ui.image((t.id(), egui::vec2(w as f32 * scale, h as f32 * scale)));
        }
        None => {
            ui.label(RichText::new("Waiting for a picture.").weak());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AJ3G: a mode picked in the scope didn't show in DECODE. Every DATA key
    /// with a decoder finds it, and back.
    #[test]
    fn every_scope_mode_with_a_decoder_finds_it_and_back() {
        for d in crate::scope::digimodes::keys() {
            if let Some(k) = Kind::of_digi(d) {
                assert!(std::ptr::eq(k.digi().unwrap(), d), "{}", d.name);
            }
        }
        for name in ["FT8", "FT4", "WSPR"] {
            let d = crate::scope::digimodes::digi(name).unwrap();
            assert!(Kind::of_digi(d).is_some(), "{name}");
        }
    }

    #[test]
    fn every_mode_has_its_decoder() {
        for (_, kinds) in Kind::GROUPS {
            for &k in kinds {
                let s = DecodeSettings { kind: k, ..DecodeSettings::default() };
                let ok = match k.family() {
                    Family::Wsjt => k.wsjt().is_some(),
                    Family::Text => s.text_mode().is_some(),
                    Family::Picture => s.image_mode().is_some(),
                    Family::Link => k.link().is_some(),
                    Family::Chat => true,
                };
                assert!(ok, "{k:?}");
            }
        }
    }

    #[test]
    fn rf_is_the_dial_plus_the_audio_and_the_converter() {
        let usb = Where { dial_hz: 14_074_000.0, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        assert_eq!(usb.rf_of_audio(1500.0), 14_075_500.0);
        let xv = Where { dial_hz: 28_074_000.0, lsb: false, xvtr_hz: 116_000_000.0, ppm: 0.0 };
        assert_eq!(xv.rf_of_audio(1000.0), 144_075_000.0);
        let lsb = Where { lsb: true, ..usb };
        assert_eq!(lsb.rf_of_audio(1500.0), 14_072_500.0);
    }

    #[test]
    fn lines_are_picked_as_in_a_list() {
        let order = [5, 4, 3, 2, 1];
        let (mut sel, mut anchor) = (HashSet::new(), None);
        pick(&mut sel, &mut anchor, &order, 4, false, false);
        assert_eq!(sel, HashSet::from([4]));
        pick(&mut sel, &mut anchor, &order, 2, false, true);
        assert_eq!(sel, HashSet::from([4, 3, 2]));
        pick(&mut sel, &mut anchor, &order, 3, true, false);
        assert_eq!(sel, HashSet::from([4, 2]));
        pick(&mut sel, &mut anchor, &order, 1, false, false);
        assert_eq!(sel, HashSet::from([1]));
        pick(&mut sel, &mut anchor, &order, 1, false, false);
        assert!(sel.is_empty());
    }

    #[test]
    fn a_line_shows_its_distance_from_the_grid_heard_before() {
        let st = StationSettings { grid: "FN31pr".into(), ..Default::default() };
        let mut heard = Heard::default();
        heard.note("DL1ABC", "JO65", 0.0);
        let row = |text: &str| {
            let (call, grid) = station::sender_of(text);
            Row {
                id: 1,
                utc: 3600.0,
                snr: Some(-12.0),
                dt: Some(0.2),
                audio_hz: Some(1234.0),
                rf_hz: Some(14_075_234.0),
                radio_hz: Some((14_075_234.0, 14_074_000.0)),
                text: text.into(),
                call,
                grid,
            }
        };
        let t = row_text(&row("K1ABC DL1ABC RR73"), Family::Wsjt, &heard, &st);
        assert_eq!(t, "01:00:00   -12   +0.2   1234    14.075234     6064  044  K1ABC DL1ABC RR73");
        let mi = StationSettings { dist_unit: station::DistUnit::Mi, ..st.clone() };
        let t = row_text(&row("CQ DL1ABC JO65"), Family::Wsjt, &Heard::default(), &mi);
        assert!(t.contains("  3768  044  "), "{t}");
        // Nowhere to measure from, or to.
        let t = row_text(&row("K1ABC W9XYZ -05"), Family::Wsjt, &heard, &st);
        assert!(t.contains("        —    —  K1ABC"), "{t}");
        // The header's columns line up with the rows'.
        let h = header_text(Family::Wsjt, &mi);
        assert_eq!(h.find("Message"), t.find("K1ABC").map(|b| t[..b].chars().count()));
    }

    #[test]
    fn slotted_modes_give_the_scope_their_decoders_slot_timing() {
        // Started 0.4 s into the slot at 22:34:00 (an even minute) + 90 s.
        let b = 1_700_000_040.0 + 90.0;
        let t = b + 0.4;
        let at = |k| {
            slots_of(k, t).map(|s| {
                let end = s.end_s.map(|e| (e * 1000.0).round() / 1000.0);
                (s.period_s, s.first_utc - b, end)
            })
        };
        // Each transmission's end: its start (DT 0) plus its symbols.
        assert_eq!(at(Kind::Ft8), Some((15.0, 0.0, Some(13.14))));
        assert_eq!(at(Kind::Ft4), Some((7.5, 0.0, Some(5.54))));
        // MSK144's pings come any time: no end.
        assert_eq!(at(Kind::Msk144), Some((15.0, 0.0, None)));
        // Q65-30A joined 0.4 s in; JT65, JT9 and FST4-60 joined 30.4 s in, too
        // late: their next minute.
        assert_eq!(at(Kind::Q65), Some((30.0, 0.0, Some(26.0))));
        assert_eq!(at(Kind::Jt65), Some((60.0, 30.0, Some(47.83))));
        assert_eq!(at(Kind::Jt9), Some((60.0, 30.0, Some(49.96))));
        assert_eq!(at(Kind::Fst4), Some((60.0, 30.0, Some(52.84))));
        // WSPR: 90.4 s into its two minutes, so the next even minute.
        assert_eq!(at(Kind::Wspr), Some((120.0, 30.0, Some(111.592))));
        // No slots, no lines.
        for k in [Kind::Psk31, Kind::Rtty, Kind::Navtex, Kind::Olivia, Kind::Thor, Kind::Sstv] {
            assert_eq!(slots_of(k, t), None, "{k:?}");
        }
        assert_eq!(slots_of(Kind::Hfdl, t), None);
    }

    #[test]
    fn a_slot_the_radio_was_keyed_in_is_spoiled_and_one_it_only_touched_is_not() {
        let mut k = KeyedLog::default();
        let t0 = 1_800_000_015.0;
        // Our over: keyed from 0.4 s into the slot to 13.3 s.
        k.note(t0 - 5.0, false);
        k.note(t0 + 0.4, true);
        k.note(t0 + 6.0, true);
        k.note(t0 + 13.3, false);
        assert!((k.overlap(t0, t0 + 15.0) - 12.9).abs() < 1e-3);
        assert!(k.spoils(t0, 15.0));
        // The slots either side are clean.
        assert!(!k.spoils(t0 - 15.0, 15.0));
        assert!(!k.spoils(t0 + 15.0, 15.0));
        // A WSPR slot holding it is spoiled too.
        assert!(k.spoils(t0 - 15.0, 120.0));
        // Unkeyed late, 0.8 s into the next slot: that slot is kept.
        k.note(t0 + 30.0, true);
        k.note(t0 + 45.8, false);
        assert!(!k.spoils(t0 + 45.0, 15.0));
        assert!(k.spoils(t0 + 30.0, 15.0));
        // Keyed still: the open span counts up to the slot's end.
        k.note(t0 + 60.5, true);
        assert!(k.spoils(t0 + 60.0, 15.0));
        // Old spans are forgotten.
        k.note(t0 + 61.0, false);
        k.note(t0 + 1000.0, false);
        assert!(k.0.is_empty());
    }

    #[test]
    fn utc_reads_as_hours_minutes_seconds() {
        assert_eq!(hms(86_400.0 * 3.0 + 3600.0 * 13.0 + 62.0), "13:01:02");
    }

    #[test]
    fn a_mode_with_a_calling_frequency_snaps_and_its_decoder_is_found_again() {
        for (_, kinds) in Kind::GROUPS {
            for &k in kinds {
                match k.digi() {
                    Some(d) => assert_eq!(Kind::of_digi(d), Some(k), "{k:?}"),
                    None => assert!(
                        matches!(k, Kind::Cw | Kind::Olivia | Kind::Thor | Kind::Hfdl),
                        "{k:?}"
                    ),
                }
            }
        }
        // JS8 has frequencies but no decoder here.
        assert_eq!(Kind::of_digi(digimodes::digi("JS8").unwrap()), None);
        assert_eq!(Kind::Olivia.audio_passband(1500.0), Some((1150.0, 1850.0)));
        assert_eq!(Kind::Ft8.audio_passband(1500.0), None);
    }

    #[test]
    fn a_slot_goes_on_the_dial_it_was_heard_on_and_one_heard_across_a_band_change_is_dropped() {
        let mut d = DialLog::default();
        d.note(1000.0, 7_074_000.0);
        d.note(1010.0, 7_074_000.0);
        // To 20 m at 1050, early in the 1050 slot.
        d.note(1050.4, 14_074_000.0);
        d.note(1060.0, 14_074_000.0);
        // The slot before, all on 40 m, decoded once on 20 m: 40 m's.
        assert_eq!(d.during(1035.0, 1050.0), Some(7_074_000.0));
        // The slot the change fell in: half each.
        assert_eq!(d.during(1050.0, 1065.0), None);
        assert_eq!(d.during(1065.0, 1080.0), Some(14_074_000.0));
        // Before anything was noted: the first dial.
        assert_eq!(d.during(900.0, 915.0), Some(7_074_000.0));
        assert_eq!(DialLog::default().during(0.0, 15.0), None);
    }

    #[test]
    fn auto_id_hears_the_passband_on_either_sideband_and_its_picks_map_to_decode() {
        assert_eq!(audio_band((300.0, 2700.0)), (300.0, 2700.0));
        assert_eq!(audio_band((-2700.0, -300.0)), (300.0, 2700.0));
        assert_eq!(audio_band((-3000.0, 3000.0)), (0.0, 3000.0));
        assert_eq!(kind_of_pick(Pick::Wsjt(WsjtMode::Ft4)), Some((Kind::Ft4, None, false)));
        assert_eq!(
            kind_of_pick(Pick::Text(TextMode::Rtty {
                center_hz: 2125.0,
                baud: 45.45,
                shift_hz: 170.0,
                reverse: true
            })),
            Some((Kind::Rtty, Some(2125.0), true))
        );
        assert_eq!(
            kind_of_pick(Pick::Text(TextMode::Psk31 { carrier_hz: 1000.0 })),
            Some((Kind::Psk31, Some(1000.0), false))
        );
        assert_eq!(kind_of_pick(Pick::Kind("SSTV")), Some((Kind::Sstv, None, false)));
        assert_eq!(kind_of_pick(Pick::Kind("Packet")), None);
        // An older settings file snaps.
        let s: DecodeSettings = serde_json::from_str("{\"running\": true}").unwrap();
        assert!(s.snap && s.running);
    }

    #[test]
    fn the_scope_labels_the_last_few_slots_stations_where_they_were_heard() {
        let at = Where { dial_hz: 14_074_000.0, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        assert_eq!(at.radio_of_audio(1234.0), 14_075_234.0);
        let lsb = Where { lsb: true, dial_hz: 7_175_000.0, ..at };
        assert_eq!(lsb.radio_of_audio(1500.0), 7_173_500.0);
        let row = |id: u64, utc: f64, text: &str| {
            let (call, grid) = station::sender_of(text);
            Row {
                id,
                utc,
                snr: Some(-7.0),
                dt: Some(0.1),
                audio_hz: Some(1234.0),
                rf_hz: Some(at.rf_of_audio(1234.0)),
                radio_hz: Some((at.radio_of_audio(1234.0), at.dial_hz)),
                text: text.into(),
                call,
                grid,
            }
        };
        // Newest first, as the list keeps them.
        let rows: VecDeque<Row> = [
            row(3, 1000.0, "CQ K1ABC FN42"),
            row(2, 985.0, "W9XYZ G4ABC -12"),
            // A message naming no sender: no label.
            row(9, 985.0, "TNX 73 GL"),
            row(1, 900.0, "CQ VE3ABC FN03"),
        ]
        .into();
        let wide: VecDeque<SkimSpot> = [SkimSpot {
            row: None,
            hz: 18_101_000.0,
            audio_hz: 1000.0,
            dial_hz: 18_100_000.0,
            call: "JA1XYZ".into(),
            text: "CQ JA1XYZ PM95".into(),
            snr: -15.0,
            utc: 1000.0,
            wsjt: Some(WsjtMode::Ft8),
            key: None,
        }]
        .into();
        let f = skim_feed(&rows, &wide, WsjtMode::Ft8, 1016.0);
        assert_eq!(f.period_s, 15.0);
        let calls: Vec<&str> = f.spots.iter().map(|s| s.call.as_str()).collect();
        // VE3ABC's slot is too long ago.
        assert_eq!(calls, ["K1ABC", "G4ABC", "JA1XYZ"]);
        let k = &f.spots[0];
        assert_eq!(
            (k.row, k.hz, k.dial_hz, k.audio_hz),
            (Some(3), 14_075_234.0, 14_074_000.0, 1234.0)
        );
        assert_eq!(f.spots[2].row, None);
        // A label clicked on the scope picks its line alone; a line since
        // dropped from the list picks nothing.
        let mut d = Decode { rows, ..Default::default() };
        d.selected.insert(1);
        d.pick_row(2);
        assert_eq!(d.selected, HashSet::from([2]));
        assert_eq!(d.scroll_to, Some(2));
        d.pick_row(42);
        assert_eq!(d.selected, HashSet::from([2]));
    }

    #[test]
    fn skim_wide_takes_the_modes_other_dials_and_the_other_modes_in_the_span() {
        let mut s = DecodeSettings { skim_wide: true, ..Default::default() };
        // 6.144 MHz around 16 MHz with the receiver on 14.074: FT8's 18.100,
        // and FT4's 14.080 and 18.104.
        let w = wide_dials(Kind::Ft8, &s, 14_074_000.0, 0.0, 16e6, 6_144_000.0);
        assert_eq!(
            w,
            vec![
                (WsjtMode::Ft8, vec![18_100_000.0]),
                (WsjtMode::Ft4, vec![14_080_000.0, 18_104_000.0]),
                (WsjtMode::Wspr, vec![14_095_600.0, 18_104_600.0]),
            ]
        );
        let w = wide_dials(Kind::Ft4, &s, 14_080_000.0, 0.0, 16e6, 6_144_000.0);
        assert_eq!(w[0], (WsjtMode::Ft4, vec![18_104_000.0]));
        assert_eq!(w[1].0, WsjtMode::Ft8);
        assert!(w[1].1.contains(&14_074_000.0));
        // 192 kHz on 20 m (AJ3G's DUO): FT8 hears FT4's 14.080 beside it.
        let w = wide_dials(Kind::Ft8, &s, 14_074_000.0, 0.0, 14.1e6, 192_000.0);
        assert_eq!(
            w,
            vec![(WsjtMode::Ft4, vec![14_080_000.0]), (WsjtMode::Wspr, vec![14_095_600.0])]
        );
        // Behind a converter the dials are where the radio hears them.
        let w = wide_dials(Kind::Ft8, &s, 28_000_000.0, 116e6, 28.1e6, 192_000.0);
        assert_eq!(w[0], (WsjtMode::Ft8, vec![28_174_000.0]));
        // Another mode (a PSK31 label clicked): FT8 and FT4 go on being
        // skimmed on every dial in the span, SKIM WIDE or not.
        let psk = DecodeSettings { skim_wide: false, ..s.clone() };
        let w = wide_dials(Kind::Psk31, &psk, 14_070_000.0, 0.0, 14.07e6, 192_000.0);
        assert_eq!(
            w,
            vec![
                (WsjtMode::Ft8, vec![14_074_000.0]),
                (WsjtMode::Ft4, vec![14_080_000.0]),
                (WsjtMode::Wspr, vec![14_095_600.0]),
            ]
        );
        // DECODE on WSPR: the other WSPR dials, not its own, and FT8 and FT4.
        let w = wide_dials(Kind::Wspr, &psk, 14_095_600.0, 0.0, 14.07e6, 192_000.0);
        assert_eq!(
            w,
            vec![(WsjtMode::Ft8, vec![14_074_000.0]), (WsjtMode::Ft4, vec![14_080_000.0])]
        );
        // FT8 with SKIM WIDE off: none beside its own.
        assert!(wide_dials(Kind::Ft8, &psk, 14_074_000.0, 0.0, 14.07e6, 192_000.0).is_empty());
        // SKIM off: none.
        s.skim = false;
        assert!(wide_dials(Kind::Ft8, &s, 14_074_000.0, 0.0, 16e6, 6_144_000.0).is_empty());
        let d = DecodeSettings::default();
        assert!(d.skim && !d.skim_wide);
    }

    #[test]
    fn ft8_heard_by_the_panel_reaches_the_list() {
        // The panel's own path: DECODE started on a running receiver, a slot
        // of FT8 audio at the tap, the slot's decodes taken frame by frame.
        let scope = crate::scope::Scope::new(None, crate::scope::ScopeSettings::default());
        let rs = rx::RxSettings { source: rx::IqChoice::Demo, ..Default::default() };
        let mut rx = Rx::new(&rs);
        rx.start(&rs, &scope, rx::RadioNow { link: None, rig: None, transmitting: false });
        assert!(rx.active());
        let s = DecodeSettings { kind: Kind::Ft8, running: true, ..Default::default() };
        let st =
            StationSettings { callsign: "AJ3G".into(), grid: "FN20".into(), ..Default::default() };
        let at = Where { dial_hz: 14_074_000.0, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        let mut d = Decode::default();
        d.frame(&s, &st, &rx, at, None);
        let Some((_, Worker::Wsjt(w, _))) = &d.worker else { panic!("no FT8 decoder") };
        let w = w.clone();
        rx.stop();

        let slot_utc = 1_700_000_010i64;
        let bits = mfsk_core::msg::wsjt77::pack77("CQ", "K1ABC", "FN42").unwrap();
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits);
        let burst = mfsk_core::ft8::wave_gen::tones_to_f32(&tones, 1500.0, 0.3);
        let mut a12 = vec![0.0f32; 12_000 * 18];
        let k = 12_000 / 2 + 2 * 12_000;
        a12[k..k + burst.len()].copy_from_slice(&burst);
        // 12 kHz to 48 kHz, each sample four times (the decoder filters it).
        let a48: Vec<f32> = a12.iter().flat_map(|&x| [x; 4]).collect();
        let start = slot_utc as f64 - 2.0;
        for (i, c) in a48.chunks(4800).enumerate() {
            w.push(c, start + (i * 4800) as f64 / 48_000.0);
        }
        let t = Instant::now();
        while d.rows.is_empty() && t.elapsed().as_secs() < 60 {
            d.take(at, None);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(d.rows.iter().any(|r| r.text == "CQ K1ABC FN42"), "{:?} {:?}", d.rows, d.last_slot);
    }
}
