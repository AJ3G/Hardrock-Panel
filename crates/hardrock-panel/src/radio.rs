//! The radio's window, drawn as the radio's own front panel: an LCD with the
//! dial as clickable seven-segment digits, its annunciators and meter; the
//! MODE, BAND and F INPUT keys and the RX, CW, TX audio and MEM pop-outs;
//! TUNE, the power buttons and the Tune slider; the live FWD, RFL and SWR;
//! and MENU, every setting the radio has a CAT command for. It sits over the
//! panel, or pops out into a window of its own.
//!
//! Every control comes from the radio's [`Protocol`] and its [`Control`]
//! table, so another radio gets the same window with its own settings, and a
//! look of its own from [`rigtheme`]. The window sends nothing itself: it
//! hands the frames to send back in [`RadioOut`], and shows only what the
//! radio reads back. Everything greys while the radio transmits; the link
//! refuses changes then anyway (safety interlock #5).
//!
//! [`Control`]: rigcat::Control

mod controls;
mod face;
mod knobs;
pub(crate) mod logic;
mod rigtheme;
mod zoom;

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, RichText};
use rigcat::{Frame, Protocol, RigStatus};

use crate::tune::{TuneArgs, TuneOut, tune_strip};
use crate::{chrome, theme};
use logic::RadioPrefs;
use rigtheme::{RigTheme, rig_theme};
pub(crate) use zoom::z;

/// What the operator asked of the radio this frame.
#[derive(Default)]
pub(crate) struct RadioOut {
    pub refresh: bool,
    /// Pop out (`true`) or dock (`false`).
    pub pop: Option<bool>,
    pub tune: TuneOut,
    /// Settings to change, in order: for `RigLink::set`, which sends each
    /// and reads it back, and refuses them while the radio transmits
    /// (interlock #5).
    pub frames: Vec<Frame>,
    /// A stored CW message to start, which transmits. Only offered when RIG
    /// TUNE could be keyed (interlock #6); check that again before
    /// `RigLink::set`.
    pub cw_send: Option<Frame>,
    /// SPLIT was pressed: TX follows RX goes on or off with it, so the scope
    /// doesn't put back a split the operator just took off.
    pub tx_follows_rx: Option<bool>,
}

/// Which pop-out is open under the face. One at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pop {
    Mode,
    Band,
    Freq,
    Rx,
    Cw,
    Tx,
    Mem,
}

/// What MENU lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MenuFilter {
    #[default]
    All,
    Settable,
    Transmit,
}

impl MenuFilter {
    const ALL: [MenuFilter; 3] = [MenuFilter::All, MenuFilter::Settable, MenuFilter::Transmit];

    fn label(self) -> &'static str {
        match self {
            MenuFilter::All => "All menus",
            MenuFilter::Settable => "Changeable here",
            MenuFilter::Transmit => "Transmit and PTT",
        }
    }
}

/// A question waiting for an answer before anything is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirm {
    /// A data mode, while TX IN isn't on USB audio.
    Digi(&'static str),
    /// Where TX IN goes, from its annunciator.
    Input,
}

/// The window's state from frame to frame, not kept between runs.
#[derive(Clone, Default)]
pub(crate) struct RadioUi {
    pub pop: Option<Pop>,
    /// The dial digit last stepped, for the arrow keys and the knob.
    pub sel: Option<usize>,
    pub entry: String,
    pub entry_msg: Option<String>,
    pub focus_entry: bool,
    /// The frequency last asked for and when, until the radio reads it back.
    pub pending: Option<(u64, Instant)>,
    pub menu: bool,
    pub menu_filter: MenuFilter,
    pub mem: u16,
    /// The data mode last chosen, while the radio stays in USB.
    pub digi: Option<&'static str>,
    pub digi_note: Option<String>,
    pub confirm: Option<Confirm>,
    /// Sliders being dragged: the value under the pointer, sent on release.
    pub drags: HashMap<&'static str, f64>,
    pub texts: HashMap<&'static str, String>,
    /// Why the last thing asked here couldn't be sent.
    pub note: Option<String>,
    /// The main knob, E1 and E2.
    pub knobs: knobs::KnobUi,
}

impl RadioUi {
    fn toggle(&mut self, p: Pop) {
        self.pop = if self.pop == Some(p) { None } else { Some(p) };
        if self.pop == Some(Pop::Freq) {
            self.entry.clear();
            self.entry_msg = None;
            self.focus_entry = true;
        }
    }
}

/// Everything a part of the window needs to draw itself and say what it
/// wants sent.
pub(crate) struct Face<'a> {
    pub p: &'a dyn Protocol,
    pub s: &'a RigStatus,
    pub th: &'static RigTheme,
    /// Why nothing may be changed now ([`logic::locked`]).
    pub lock: Option<&'static str>,
    /// Why RIG TUNE couldn't be keyed now, for the CW messages.
    pub carrier_blocked: Option<String>,
    pub xvtr: i64,
    pub st: &'a mut RadioUi,
    pub out: &'a mut RadioOut,
}

impl Face<'_> {
    fn push(&mut self, f: Option<Frame>) {
        if let Some(f) = f {
            self.out.frames.push(f);
        }
    }

    fn send_control(&mut self, c: &rigcat::Control, value: &str) {
        match logic::control_frame(self.p, c, value, self.s) {
            Ok(f) => self.out.frames.push(f),
            Err(e) => self.st.note = Some(e),
        }
    }
}

fn prefs_id() -> egui::Id {
    egui::Id::new("radio-window-prefs")
}

/// The transverter LO set in the radio window, in Hz; 0 when none is set.
pub(crate) fn xvtr_hz(ctx: &egui::Context) -> i64 {
    ctx.data_mut(|d| d.get_persisted::<RadioPrefs>(prefs_id())).map_or(0, |p| p.xvtr_hz)
}

/// Set the transverter LO, as if typed in the radio window (the scope's own
/// setting is kept the same as this one).
pub(crate) fn set_xvtr_hz(ctx: &egui::Context, hz: i64) {
    ctx.data_mut(|d| {
        let mut p: RadioPrefs = d.get_persisted(prefs_id()).unwrap_or_default();
        p.xvtr_hz = hz;
        d.insert_persisted(prefs_id(), p);
    });
}

/// Whether the operator marked a 10 MHz reference as connected.
pub(crate) fn ref_10mhz(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.get_persisted::<RadioPrefs>(prefs_id())).is_some_and(|p| p.ref_10mhz)
}

pub(crate) fn set_ref_10mhz(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| {
        let mut p: RadioPrefs = d.get_persisted(prefs_id()).unwrap_or_default();
        p.ref_10mhz = on;
        d.insert_persisted(prefs_id(), p);
    });
}

fn ui_id() -> egui::Id {
    egui::Id::new("radio-window-ui")
}

/// Draw the radio's window, docked over the panel or popped out into its own.
/// Clears `open` when it is closed.
pub(crate) fn radio_window(
    ctx: &egui::Context,
    open: &mut bool,
    popped: bool,
    st: Option<&RigStatus>,
    mut tune: TuneArgs<'_>,
) -> RadioOut {
    let mut out = RadioOut::default();
    // Kept between runs with the window's other state (egui's memory, which
    // eframe saves): band memories, backlights, the transverter offset.
    let mut prefs: RadioPrefs = ctx.data_mut(|d| d.get_persisted(prefs_id())).unwrap_or_default();
    let mut rui: RadioUi = ctx.data_mut(|d| d.get_temp(ui_id())).unwrap_or_default();
    let title = st.map_or("Radio", |s| s.model.label());
    if popped {
        // Painted in place: no background GPU submit meanwhile (gpu_gate).
        let _shut = crate::gpu_gate::shut();
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("radio-window"),
            crate::popout::builder(ctx, "radio-window", title, [600.0, 820.0], [480.0, 420.0]),
            |ui, _class| {
                crate::popout::track(ui.ctx(), "radio-window");
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        fitted(ui, "radio-popped", |ui| {
                            radio_body(ui, true, st, &mut tune, &mut out, &mut prefs, &mut rui)
                        });
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        // Its edges and corner can be dragged both ways (AJ3G, 21 Oct 2026),
        // and its face scales to fit the window's width and height both,
        // keeping its proportions, as it does popped out.
        let w = BODY_W + CASE_W + DOCK_MARGIN;
        egui::Window::new(title)
            .id(egui::Id::new("radio-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([180.0, 48.0])
            .default_size([w, DOCK_H])
            .min_size([MIN_SCALE * (BODY_W + CASE_W) + DOCK_MARGIN, 240.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                // A window is as big as its contents measure: the face is
                // centred in all the room it was dragged to, not shrunk round.
                ui.set_min_size(ui.available_size());
                fitted(ui, "radio-docked", |ui| {
                    radio_body(ui, false, st, &mut tune, &mut out, &mut prefs, &mut rui)
                });
            });
    }
    ctx.data_mut(|d| {
        d.insert_persisted(prefs_id(), prefs);
        d.insert_temp(ui_id(), rui);
    });
    out
}

/// The radio's face is drawn [`BODY_W`] wide at scale 1, inside a case
/// [`CASE_W`] wider.
const BODY_W: f32 = 560.0;
const CASE_W: f32 = 32.0;
/// Smallest and largest scale the radio window is drawn at. Below the
/// smallest it scrolls.
const MIN_SCALE: f32 = 0.75;
const MAX_SCALE: f32 = 3.0;
/// What the docked window adds to its contents' width: the frame's margin
/// either side, and its edge.
const DOCK_MARGIN: f32 = 26.0;
/// The docked window's height when it is first opened: the face at scale 1
/// and the window's title bar.
const DOCK_H: f32 = 570.0;

/// The radio's face scaled to fit the room there is, width and height both,
/// and centred across it: it keeps its proportions, and its text is laid
/// out at the size it is shown (see [`zoom`]).
fn fitted(ui: &mut egui::Ui, key: &'static str, body: impl FnOnce(&mut egui::Ui)) {
    crate::panel::fit_scaled(ui, key, BODY_W + CASE_W, (MIN_SCALE, MAX_SCALE), |ui, k| {
        let w = (BODY_W + CASE_W) * k;
        let left = ui.max_rect().left() + ((ui.available_width() - w) / 2.0).max(0.0);
        let rect = egui::Rect::from_min_size(
            egui::pos2(left, ui.cursor().top()),
            egui::vec2(w, ui.available_height()),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| zoom::scaled(ui, k, body),
        );
    });
}

/// The window's contents at the scale in force ([`zoom::scaled`]).
fn radio_body(
    ui: &mut egui::Ui,
    popped: bool,
    st: Option<&RigStatus>,
    tune: &mut TuneArgs<'_>,
    out: &mut RadioOut,
    prefs: &mut RadioPrefs,
    rui: &mut RadioUi,
) {
    let Some(s) = st else {
        ui.label("No radio is set up. Choose its CAT port under SETTINGS, in the Radio section.");
        return;
    };
    let th = rig_theme(s.model);
    let proto = s.model.protocol();
    let lock = logic::locked(s);

    // The band memories follow where the radio really is: its read-back.
    if s.connected
        && !s.transmitting()
        && let (Some(hz), Some(m)) = (logic::dial_hz(s), s.mode)
    {
        prefs.remember(logic::shown(hz, prefs.xvtr_hz), m);
        if rui.pending.is_some_and(|(p, t)| p == hz || t.elapsed().as_secs() >= 2) {
            rui.pending = None;
        }
    }
    if s.mode.is_some_and(|m| m != "USB") {
        rui.digi = None;
        rui.digi_note = None;
    }

    let mut f = Face {
        p: &*proto,
        s,
        th,
        lock,
        carrier_blocked: tune.carrier_blocked.clone(),
        xvtr: prefs.xvtr_hz,
        st: rui,
        out,
    };

    egui::Frame::new()
        .fill(th.case)
        .stroke(egui::Stroke::new(1.0, th.case_line))
        .corner_radius(egui::CornerRadius::same(zoom::zi(10.0) as u8))
        .inner_margin(egui::Margin::symmetric(zoom::zi(14.0), zoom::zi(12.0)))
        .show(ui, |ui| {
            ui.set_width(z(BODY_W));
            ui.spacing_mut().item_spacing.y = z(8.0);
            // Silk screen.
            ui.horizontal(|ui| {
                ui.label(RichText::new(th.brand).font(face::label_font(17.0)).color(th.label));
                let line = format!("{} · CAT {} 8N1", th.model, s.baud);
                ui.label(RichText::new(line).font(face::label_font(12.0)).color(th.dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, hover) = if popped {
                        ("DOCK", "Put this window back over the panel")
                    } else {
                        ("POP OUT", "Open this in a window of its own")
                    };
                    if chrome::chip(ui, false, text).on_hover_text(hover).clicked() {
                        f.out.pop = Some(!popped);
                    }
                    // The end of the panel's log, which has each setting
                    // sent and what the radio read back (`rigcat: set …`).
                    if chrome::chip(ui, false, "COPY LOG")
                        .on_hover_text(
                            "Copy the end of the panel's log: each setting sent to the radio \
                             and what it read back, for a bug report",
                        )
                        .clicked()
                        && let Some(t) = crate::logfile::tail()
                    {
                        ui.ctx().copy_text(t);
                    }
                });
            });
            if !s.connected {
                ui.label(
                    RichText::new(s.error.clone().unwrap_or_else(|| "Connecting…".into()))
                        .color(theme::ALERT()),
                );
            }

            // The face: LCD and knobs.
            ui.horizontal(|ui| {
                let lcd_w = ui.available_width() - z(face::KNOBS_W + 14.0);
                let h = face::lcd(ui, &mut f, prefs, lcd_w);
                ui.add_space(z(6.0));
                face::knobs(ui, &mut f, prefs, h);
            });

            ui.separator();
            controls::keys(ui, &mut f);
            controls::popout(ui, &mut f, prefs);
            controls::vfo_row(ui, &mut f);
            ui.add_space(z(2.0));
            f.out.tune = tune_strip(ui, s, tune);
            controls::readouts(ui, &f);
            for n in [f.st.digi_note.clone(), f.st.note.clone(), s.set_note.clone()]
                .into_iter()
                .flatten()
            {
                ui.label(RichText::new(n).size(z(11.5)).color(th.dim));
            }
        });

    // MENU and the questions are windows of their own, at their own size.
    let ctx = ui.ctx().clone();
    zoom::unscaled(|| {
        if f.st.menu {
            controls::menu_window(&ctx, &mut f, prefs);
        }
        controls::confirm(&ctx, &mut f);
    });
}
