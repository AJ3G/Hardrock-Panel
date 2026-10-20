//! The DECODE window: the panel's own decoders (decisions 26 to 28) on the
//! receiver's audio (48 kHz, each block stamped in UTC from the system
//! clock) or on the radio's I/Q.
//!
//! * WSJT modes (FT8, FT4, WSPR, JT65, JT9, FST4, Q65) in UTC slots, each
//!   decode with its UTC, SNR, DT, audio frequency and RF frequency, and sent
//!   on as WSJT-X UDP when that is on under SETTINGS.
//! * Keyboard modes (RTTY, PSK31, NAVTEX, Olivia, THOR) as running text.
//! * Pictures (SSTV, WEFAX) as they are drawn, line by line.
//! * Data links (HFDL, ACARS, VDL2, AIS, Mode S) from the I/Q, at true RF:
//!   the converter's LO is the radio window's transverter offset.
//!
//! Every decoder runs on threads of its own behind a queue that drops rather
//! than waits; the window shows how many blocks each has dropped.

use std::collections::VecDeque;
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

use crate::rx::{self, Rx};
use crate::{chrome, theme};

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
        ("WSJT", &[Kind::Ft8, Kind::Ft4, Kind::Wspr, Kind::Jt65, Kind::Jt9, Kind::Fst4, Kind::Q65]),
        ("Keyboard", &[Kind::Rtty, Kind::Psk31, Kind::Navtex, Kind::Olivia, Kind::Thor]),
        ("Pictures", &[Kind::Sstv, Kind::Wefax]),
        ("From I/Q", &[Kind::Hfdl, Kind::Acars, Kind::Vdl2, Kind::Ais, Kind::ModeS]),
    ];

    fn label(self) -> &'static str {
        match self {
            Kind::Ft8 => "FT8",
            Kind::Ft4 => "FT4",
            Kind::Wspr => "WSPR",
            Kind::Jt65 => "JT65",
            Kind::Jt9 => "JT9",
            Kind::Fst4 => "FST4-60",
            Kind::Q65 => "Q65-30A",
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
            | Kind::Q65 => Family::Wsjt,
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
            _ => return None,
        })
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
    /// Decoding (the window's RUN), whether or not the window is open.
    pub running: bool,
    /// The keyboard modes' audio frequency (RTTY's mark/space centre,
    /// PSK31's carrier, Olivia's and THOR's centre), Hz.
    pub audio_hz: f64,
    /// RTTY and NAVTEX with mark and space swapped.
    pub reverse: bool,
    /// WEFAX's lines per minute, and IOC 288 rather than 576.
    pub lpm: f64,
    pub ioc_288: bool,
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
    pub utc: f64,
    pub snr: Option<f32>,
    pub dt: Option<f32>,
    pub audio_hz: Option<f32>,
    /// True RF, Hz.
    pub rf_hz: Option<f64>,
    pub text: String,
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
    }

    fn clear(&mut self) {
        self.rows.clear();
        self.text.clear();
        self.image = None;
        self.tex = None;
        self.notes.clear();
        self.last_slot = None;
    }

    /// Once a frame: start, retune or stop the decoder as asked, and take
    /// what it heard.
    pub(crate) fn frame(
        &mut self,
        s: &DecodeSettings,
        rx: &Rx,
        at: Where,
        wsjtx: Option<&WsjtxUdp>,
    ) {
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
            self.worker = self.spawn(s, rx, at, taps).map(|w| (s.kind, w));
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

    fn spawn(&mut self, s: &DecodeSettings, rx: &Rx, at: Where, taps: &rx::Taps) -> Option<Worker> {
        match s.kind.family() {
            Family::Wsjt => {
                let mode = s.kind.wsjt()?;
                let w = Arc::new(WsjtDecoder::spawn(WsjtConfig::new(mode)));
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
                for slot in w.results().try_iter() {
                    self.last_slot = Some((slot.slot_utc, slot.decodes.len(), slot.decode_ms));
                    for d in &slot.decodes {
                        let rf = at.rf_of_audio(d.audio_hz as f64);
                        let row = Row {
                            utc: d.slot_utc as f64,
                            snr: Some(d.snr_db),
                            dt: Some(d.dt_s),
                            audio_hz: Some(d.audio_hz),
                            rf_hz: Some(rf),
                            text: d.message.clone(),
                        };
                        push_row(&mut self.rows, row);
                        if let Some(u) = wsjtx {
                            send_wsjtx(u, *mode, d, rf);
                        }
                    }
                    if let Some(u) = wsjtx {
                        u.status(&links::StatusInfo {
                            dial_hz: (at.dial_hz + at.xvtr_hz).max(0.0) as u64,
                            mode: mode.label().into(),
                            decoding: true,
                            tr_period_s: mode.period_s() as u32,
                            ..links::StatusInfo::default()
                        });
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
                            Row {
                                utc: d.utc,
                                snr: d.snr_db,
                                dt: None,
                                audio_hz: None,
                                rf_hz: Some(d.rf_hz as f64),
                                text: d.summary.clone(),
                            },
                        ),
                        LinkEvent::Refused(why) => self.error = Some(why),
                    }
                }
            }
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

fn push_row(rows: &mut VecDeque<Row>, r: Row) {
    rows.push_front(r);
    rows.truncate(ROWS);
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
pub(crate) fn hms(utc: f64) -> String {
    let s = utc.rem_euclid(86_400.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

/// Draw the window. Clears `open` when it is closed.
pub(crate) fn window(
    ctx: &egui::Context,
    open: &mut bool,
    d: &mut Decode,
    s: &mut DecodeSettings,
    rx: &Rx,
    wsjtx_on: bool,
) {
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
            body(ui, d, s, rx, wsjtx_on);
        });
}

fn body(ui: &mut egui::Ui, d: &mut Decode, s: &mut DecodeSettings, rx: &Rx, wsjtx_on: bool) {
    for (group, kinds) in Kind::GROUPS {
        ui.horizontal_wrapped(|ui| {
            caption(ui, group);
            for &k in kinds {
                if chrome::chip(ui, s.kind == k, k.label()).clicked() {
                    s.kind = k;
                }
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        let (t, hover) = if s.running {
            (" STOP ", "Stop decoding")
        } else {
            (" RUN ", "Decode the receiver's audio (or the I/Q, for the data links)")
        };
        if chrome::chip(ui, s.running, t).on_hover_text(hover).clicked() {
            s.running = !s.running;
        }
        match s.kind.family() {
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
            Family::Wsjt | Family::Link if chrome::chip(ui, false, "Clear").clicked() => {
                d.rows.clear();
            }
            _ => {}
        }
    });

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
    let col = if dropped > 0 { theme::YELLOW() } else { theme::TEXT() };
    ui.label(RichText::new(line).size(11.5).color(col));
    if let Some(e) = &d.error {
        ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
    }
    ui.separator();

    match s.kind.family() {
        Family::Wsjt | Family::Link => rows(ui, d, s.kind.family()),
        Family::Text => {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                ui.label(RichText::new(&d.text).monospace().size(13.0));
            });
        }
        Family::Picture => picture(ui, d),
    }
}

fn rows(ui: &mut egui::Ui, d: &Decode, fam: Family) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        egui::Grid::new("decode-rows").striped(true).spacing([10.0, 2.0]).show(ui, |ui| {
            let heads: &[&str] = if fam == Family::Wsjt {
                &["UTC", "SNR", "DT", "AF Hz", "RF MHz", "Message"]
            } else {
                &["UTC", "SNR", "RF MHz", "Message"]
            };
            for h in heads {
                caption(ui, h);
            }
            ui.end_row();
            for r in &d.rows {
                let mono = |t: String| RichText::new(t).monospace().size(12.5);
                ui.label(mono(hms(r.utc)));
                ui.label(mono(r.snr.map_or("—".into(), |v| format!("{v:+.0}"))));
                if fam == Family::Wsjt {
                    ui.label(mono(r.dt.map_or("—".into(), |v| format!("{v:+.1}"))));
                    ui.label(mono(r.audio_hz.map_or("—".into(), |v| format!("{v:.0}"))));
                }
                ui.label(mono(r.rf_hz.map_or("—".into(), |v| format!("{:.6}", v / 1e6))));
                ui.label(mono(r.text.clone()));
                ui.end_row();
            }
        });
        if d.rows.is_empty() {
            ui.label(RichText::new("Nothing decoded yet.").weak());
        }
    });
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
    fn utc_reads_as_hours_minutes_seconds() {
        assert_eq!(hms(86_400.0 * 3.0 + 3600.0 * 13.0 + 62.0), "13:01:02");
    }
}
