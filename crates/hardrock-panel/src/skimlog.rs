//! SKIM LOG (decision 60): every station the skimmers heard, of every mode,
//! in one list of its own with the mode it was heard in. AJ3G: "add another
//! decode window that covers all modes and reports what mode each decode was
//! collected from… I am on FT4 and I am seeing SKIMs from FT8, but only the
//! FT4 SKIM gets put into the decode log."
//!
//! Three sources: the receiver's own decodes (also DECODE's list), SKIM
//! WIDE's FT8 and FT4 on the other dials, and SKIM CW/PSK/RTTY's signals,
//! one line a signal, its text growing while it is heard. Each line is also
//! added to `SKIM.TXT` beside `ALL.TXT`: the WSJT ones as they come, a
//! keyboard signal's once it stops, with all it sent. Nothing here goes to
//! PSK Reporter, WSPRnet or WSJT-X UDP.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::{chrome, decodelog, theme};

/// Lines kept on screen.
pub const LINES: usize = 2000;
const FILE: &str = "SKIM.TXT";

/// Where a line was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// DECODE's own decoder, on the receiver's dial.
    Receiver,
    /// SKIM WIDE, on another dial in the span.
    Wide,
    /// SKIM CW/PSK/RTTY.
    Keys,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Receiver => "RX",
            Source::Wide => "WIDE",
            Source::Keys => "SKIM",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The slot's UTC start (WSJT), or when a keyboard signal was last heard.
    pub utc: f64,
    /// "FT8", "FT4", "CW", "PSK", "RTTY"…
    pub mode: &'static str,
    pub source: Source,
    /// True RF of the signal, Hz.
    pub rf_hz: f64,
    pub snr: f32,
    pub call: Option<String>,
    pub text: String,
    /// The keyboard skimmer's track, while its line still grows.
    pub track: Option<u64>,
    /// Still being heard (keyboard signals only).
    pub live: bool,
}

impl Line {
    /// As the list and `SKIM.TXT` write it.
    pub fn text_line(&self) -> String {
        let (y, mo, d, h, mi, s) = decodelog::civil(self.utc as i64);
        format!(
            "{:02}{mo:02}{d:02}_{h:02}{mi:02}{s:02}  {:<5} {:<4} {:>12.6} {:>4}  {}",
            y.rem_euclid(100),
            self.mode,
            self.source.label(),
            self.rf_hz / 1e6,
            self.snr.round() as i32,
            self.text.trim(),
        )
    }
}

pub const HEADER: &str = "UTC            Mode  From       RF MHz  SNR  Message";

/// The list, newest first, and its file.
#[derive(Default)]
pub struct SkimLog {
    lines: VecDeque<Line>,
    /// Show only this mode.
    pub only: Option<&'static str>,
    path: Option<PathBuf>,
    pub error: Option<String>,
    pending: Vec<String>,
}

impl SkimLog {
    pub fn lines(&self) -> &VecDeque<Line> {
        &self.lines
    }

    /// A WSJT decode, from the receiver or SKIM WIDE.
    pub fn add(&mut self, l: Line) {
        self.pending.push(l.text_line());
        self.lines.push_front(l);
        self.lines.truncate(LINES);
    }

    /// A keyboard skimmer's signal as it is now: its line updated while it
    /// is heard, written to the file once it stops. Signals with no
    /// callsign read are left out, as on the scope.
    pub fn key(&mut self, track: u64, l: Line) {
        if l.call.is_none() {
            return;
        }
        match self.lines.iter_mut().find(|x| x.track == Some(track)) {
            Some(x) => {
                let was = x.live;
                *x = Line { track: Some(track), ..l };
                if was && !x.live {
                    self.pending.push(x.text_line());
                    x.track = None;
                }
            }
            None if l.live => {
                self.lines.push_front(Line { track: Some(track), ..l });
                self.lines.truncate(LINES);
            }
            None => {}
        }
    }

    /// A keyboard track the skimmer no longer has: its line is final.
    pub fn end_tracks(&mut self, alive: &[u64]) {
        for x in self.lines.iter_mut() {
            if let Some(t) = x.track
                && !alive.contains(&t)
            {
                x.track = None;
                if x.live {
                    x.live = false;
                    self.pending.push(x.text_line());
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Add what came since last time to `SKIM.TXT`.
    pub fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if self.path.is_none() {
            self.path = decodelog::settings_dir().map(|d| d.join(FILE));
        }
        let Some(path) = self.path.clone() else {
            self.pending.clear();
            self.error = Some("No folder for SKIM.TXT on this system".into());
            return;
        };
        let lines = std::mem::take(&mut self.pending);
        let r = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            let mut text = String::new();
            for l in &lines {
                text.push_str(l);
                text.push('\n');
            }
            f.write_all(text.as_bytes())
        })();
        self.error = r.err().map(|e| format!("SKIM.TXT {}: {e}", path.display()));
    }

    fn modes(&self) -> Vec<&'static str> {
        let mut m: Vec<&'static str> = Vec::new();
        for l in &self.lines {
            if !m.contains(&l.mode) {
                m.push(l.mode);
            }
        }
        m.sort_unstable();
        m
    }
}

/// The SKIM LOG window, a window of its own on the desktop.
pub fn window(ctx: &egui::Context, open: &mut bool, log: &mut SkimLog) {
    // Painted in place: no background GPU submit meanwhile (gpu_gate).
    let _shut = crate::gpu_gate::shut();
    let closed = ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("skim-log-window"),
        egui::ViewportBuilder::default()
            .with_title("Skim log")
            .with_inner_size([780.0, 520.0])
            .with_min_inner_size([420.0, 240.0]),
        |ui, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                .show(ui, |ui| {
                    chrome::window_body_bg(ui);
                    body(ui, log);
                });
            ui.ctx().input(|i| i.viewport().close_requested())
        },
    );
    if closed {
        *open = false;
    }
}

fn shown(log: &SkimLog) -> Vec<&Line> {
    log.lines.iter().filter(|l| log.only.is_none_or(|m| m == l.mode)).collect()
}

fn body(ui: &mut egui::Ui, log: &mut SkimLog) {
    ui.horizontal_wrapped(|ui| {
        if chrome::chip(ui, log.only.is_none(), "ALL").clicked() {
            log.only = None;
        }
        for m in log.modes() {
            if chrome::chip(ui, log.only == Some(m), m).clicked() {
                log.only = Some(m);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if chrome::chip(ui, false, "CLEAR")
                .on_hover_text("Empty the list (not SKIM.TXT)")
                .clicked()
            {
                log.clear();
            }
            if chrome::chip(ui, false, "COPY ALL")
                .on_hover_text("Copy every line shown, newest first")
                .clicked()
            {
                let mut t = String::from(HEADER);
                for l in shown(log) {
                    t.push('\n');
                    t.push_str(&l.text_line());
                }
                ui.ctx().copy_text(t);
            }
        });
    });
    let n = shown(log).len();
    let mut info = format!(
        "{n} lines · RX: the receiver's own decodes · WIDE: SKIM WIDE's other dials · SKIM: SKIM CW/PSK/RTTY"
    );
    if let Some(p) = &log.path {
        info.push_str(&format!(" · also in {}", p.display()));
    }
    ui.label(RichText::new(info).size(11.5).color(theme::TEXT()));
    if let Some(e) = &log.error {
        ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
    }
    ui.separator();
    let font = egui::FontId::monospace(12.5);
    ui.label(RichText::new(HEADER).font(font.clone()).color(theme::CYAN_DIM()).strong());
    let line_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    let lines = shown(log);
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        line_h,
        lines.len(),
        |ui, range| {
            for (i, l) in lines.iter().enumerate().skip(range.start).take(range.len()) {
                let w = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, line_h), egui::Sense::hover());
                if i % 2 == 1 {
                    ui.painter().rect_filled(rect, 0.0, theme::ROW_BG());
                }
                let col = if l.live { theme::TEXT_STRONG() } else { theme::TEXT() };
                ui.painter().text(
                    rect.left_center() + egui::vec2(2.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    l.text_line(),
                    font.clone(),
                    col,
                );
            }
            if lines.is_empty() {
                ui.label(RichText::new("Nothing skimmed yet.").weak());
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(mode: &'static str, source: Source, text: &str, live: bool) -> Line {
        Line {
            utc: 1_792_670_850.0,
            mode,
            source,
            rf_hz: 7_074_950.0,
            snr: -12.0,
            call: Some("K1ABC".into()),
            text: text.into(),
            track: None,
            live,
        }
    }

    #[test]
    fn every_mode_goes_in_with_its_mode_and_where_it_was_heard() {
        let mut log = SkimLog::default();
        log.add(line("FT4", Source::Receiver, "CQ W1AW FN31", false));
        log.add(line("FT8", Source::Wide, "CQ K1ABC FN42", false));
        assert_eq!(log.lines()[0].mode, "FT8");
        assert_eq!(
            log.lines()[0].text_line(),
            "261022_120730  FT8   WIDE     7.074950  -12  CQ K1ABC FN42"
        );
        assert!(log.lines()[1].text_line().contains("FT4   RX "));
        assert_eq!(log.pending.len(), 2);
        assert_eq!(log.modes(), vec!["FT4", "FT8"]);
        log.only = Some("FT8");
        assert_eq!(shown(&log).len(), 1);
    }

    #[test]
    fn a_keyboard_signal_is_one_line_that_grows_and_is_written_when_it_stops() {
        let mut log = SkimLog::default();
        log.key(7, line("CW", Source::Keys, "CQ CQ DE K1ABC", true));
        log.key(7, line("CW", Source::Keys, "CQ CQ DE K1ABC K1ABC K", true));
        assert_eq!(log.lines().len(), 1);
        assert!(log.pending.is_empty());
        log.key(7, line("CW", Source::Keys, "CQ CQ DE K1ABC K1ABC K", false));
        assert_eq!(log.pending.len(), 1);
        assert!(log.pending[0].ends_with("CQ CQ DE K1ABC K1ABC K"));
        // Its track gone, a new signal on the same number is a new line.
        log.key(7, line("CW", Source::Keys, "TEST DE W9XYZ", true));
        assert_eq!(log.lines().len(), 2);
        log.end_tracks(&[]);
        assert_eq!(log.pending.len(), 2);
        // No callsign read: not logged.
        let mut none = line("RTTY", Source::Keys, "RYRYRY", true);
        none.call = None;
        log.key(9, none);
        assert_eq!(log.lines().len(), 2);
    }
}
