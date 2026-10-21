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

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use datalink::{LinkConfig, LinkEvent, LinkMode, LinkWorker, Tuning};
use decoders::modem::wefax::{Ioc, Lpm};
use decoders::{
    Fst4Period, ImageCmd, ImageDecoder, ImageMode, ImageUpdate, Q65Mode, RgbaImage, TextDecoder,
    TextMode, WsjtConfig, WsjtDecoder, WsjtMode,
};
use eframe::egui::{self, RichText};
use links::WsjtxUdp;
use serde::{Deserialize, Serialize};

use crate::decodelog::{self, DecodeLog};
use crate::rx::{self, Rx};
use crate::scope::digimodes::{self, Digi};
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
        ("Keyboard", &[Kind::Rtty, Kind::Psk31, Kind::Navtex, Kind::Olivia, Kind::Thor]),
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
            Kind::Rtty | Kind::Psk31 | Kind::Navtex | Kind::Olivia | Kind::Thor => Family::Text,
            Kind::Sstv | Kind::Wefax => Family::Picture,
            Kind::Hfdl | Kind::Acars | Kind::Vdl2 | Kind::Ais | Kind::ModeS => Family::Link,
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
            Kind::Rtty => "RTTY",
            Kind::Psk31 => "PSK31",
            Kind::Navtex => "NAVTEX",
            Kind::Sstv => "SSTV",
            Kind::Wefax => "WEFAX",
            Kind::Acars => "ACARS",
            Kind::Vdl2 => "VDL2",
            Kind::Ais => "AIS",
            Kind::ModeS => "ADS-B",
            Kind::Olivia | Kind::Thor | Kind::Hfdl => return None,
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
    pub text: String,
    /// Who sent it, and the grid it gave (WSJT modes).
    pub call: Option<String>,
    pub grid: Option<String>,
}

enum Worker {
    Wsjt(Arc<WsjtDecoder>, WsjtMode),
    Text(Arc<TextDecoder>, TextMode),
    Picture(Arc<ImageDecoder>, ImageMode),
    Link(Arc<LinkWorker>, LinkConfig),
}

impl Worker {
    fn dropped(&self) -> u64 {
        match self {
            Worker::Wsjt(w, _) => w.dropped(),
            Worker::Text(w, _) => w.dropped(),
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
}

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
        self.worker = None;
        self.started_utc = None;
    }

    /// The decoder's slots for the scope's slot lines (decision 51): the
    /// mode's period and the first slot it takes whole, while a mode with
    /// slots runs; `None` otherwise (keyboard modes, pictures, links).
    pub(crate) fn slots(&self) -> Option<crate::scope::DecodeSlots> {
        let (kind, _) = self.worker.as_ref()?;
        slots_of(*kind, self.started_utc?)
    }

    fn clear(&mut self) {
        self.rows.clear();
        self.text.clear();
        self.image = None;
        self.tex = None;
        self.notes.clear();
        self.last_slot = None;
        self.selected.clear();
        self.anchor = None;
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
                    self.wsjt_slot(&slot, mode, at, wsjtx);
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
                (Some(w), WsjtMode::Wspr) => wspr.push(wsprnet::Spot {
                    call: w.callsign.clone(),
                    grid: w.grid.clone(),
                    snr_db: d.snr_db.round() as i32,
                    dt_s: d.dt_s,
                    drift_hz: w.drift_hz.round() as i32,
                    freq_hz: rf,
                    power_dbm: w.power_dbm,
                }),
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
/// period, and the first slot the decoder takes whole, from the decoder's
/// own timing ([`WsjtConfig::first_slot`]). `None` for a mode without slots.
fn slots_of(kind: Kind, started_utc: f64) -> Option<crate::scope::DecodeSlots> {
    let cfg = WsjtConfig::new(kind.wsjt()?);
    Some(crate::scope::DecodeSlots {
        period_s: cfg.mode.period_s(),
        first_utc: cfg.first_slot(started_utc),
    })
}

pub(crate) fn hms(utc: f64) -> String {
    let s = utc.rem_euclid(86_400.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

/// What the operator asked of the window this frame.
#[derive(Default)]
pub(crate) struct DecodeOut {
    /// A mode was picked: snap to it, if `snap` is on.
    pub picked: Option<Kind>,
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
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("decode-window"),
            egui::ViewportBuilder::default()
                .with_title("Decode")
                .with_inner_size([720.0, 640.0])
                .with_min_inner_size([420.0, 320.0]),
            |ui, _class| {
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
        Family::Text => {
            if s.kind != Kind::Navtex {
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
        }
        _ => {}
    });
    if s.kind.family() == Family::Wsjt
        && let Some(mode) = s.kind.wsjt()
    {
        let started = d.worker.is_some().then_some(d.started_utc).flatten();
        let last = d.last_slot.map(|(slot, n, _)| (slot, n));
        let light =
            slotclock::light(decoders::slot::utc_now(), mode.period_s(), started, 1.0, last);
        slotclock::row(ui, decoders::slot::utc_now(), mode.period_s(), light);
        spots::spots_box(ui, &mut s.spots, &d.spots, me(st).as_ref());
    }

    // Where it listens, and what it has dropped.
    let src = match s.kind.family() {
        Family::Link => {
            format!("I/Q at {:.0} kHz, centre {:.3} MHz", rx.rate() / 1e3, rx.centre() / 1e6)
        }
        _ => format!("receiver audio, 48 kHz · {} at {:.3} kHz", rx.mode.label(), rx.rx_hz / 1e3),
    };
    let dropped = d.worker.as_ref().map_or(0, |(_, w)| w.dropped());
    let mut line = format!("From the {src} · {dropped} blocks dropped");
    if let Some((slot, n, ms)) = d.last_slot {
        line.push_str(&format!(" · slot {} : {n} decoded in {ms} ms", hms(slot as f64)));
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
        Family::Wsjt | Family::Link => rows(ui, d, s.kind.family(), st),
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
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        line_h,
        d.rows.len(),
        |ui, range| {
            for (i, r) in d.rows.iter().enumerate().skip(range.start).take(range.len()) {
                let w = ui.available_width();
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(w, line_h), egui::Sense::click());
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
        },
    );
    if let Some((id, m)) = click {
        let order: Vec<u64> = d.rows.iter().map(|r| r.id).collect();
        pick(&mut d.selected, &mut d.anchor, &order, id, m.command, m.shift);
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
        let at = |k| slots_of(k, t).map(|s| (s.period_s, s.first_utc - b));
        assert_eq!(at(Kind::Ft8), Some((15.0, 0.0)));
        assert_eq!(at(Kind::Ft4), Some((7.5, 0.0)));
        assert_eq!(at(Kind::Msk144), Some((15.0, 0.0)));
        // Q65-30A joined 0.4 s in; JT65, JT9 and FST4-60 joined 30.4 s in, too
        // late: their next minute.
        assert_eq!(at(Kind::Q65), Some((30.0, 0.0)));
        assert_eq!(at(Kind::Jt65), Some((60.0, 30.0)));
        assert_eq!(at(Kind::Jt9), Some((60.0, 30.0)));
        assert_eq!(at(Kind::Fst4), Some((60.0, 30.0)));
        // WSPR: 90.4 s into its two minutes, so the next even minute.
        assert_eq!(at(Kind::Wspr), Some((120.0, 30.0)));
        // No slots, no lines.
        for k in [Kind::Psk31, Kind::Rtty, Kind::Navtex, Kind::Olivia, Kind::Thor, Kind::Sstv] {
            assert_eq!(slots_of(k, t), None, "{k:?}");
        }
        assert_eq!(slots_of(Kind::Hfdl, t), None);
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
                    None => assert!(matches!(k, Kind::Olivia | Kind::Thor | Kind::Hfdl), "{k:?}"),
                }
            }
        }
        // JS8 has frequencies but no decoder here.
        assert_eq!(Kind::of_digi(digimodes::digi("JS8").unwrap()), None);
        assert_eq!(Kind::Olivia.audio_passband(1500.0), Some((1150.0, 1850.0)));
        assert_eq!(Kind::Ft8.audio_passband(1500.0), None);
        // An older settings file snaps.
        let s: DecodeSettings = serde_json::from_str("{\"running\": true}").unwrap();
        assert!(s.snap && s.running);
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
