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
mod logic;
mod rigtheme;

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, RichText};
use rigcat::{Frame, Protocol, RigStatus};

use crate::tune::{TuneArgs, TuneOut, tune_strip};
use crate::{chrome, theme};
use logic::RadioPrefs;
use rigtheme::{RigTheme, rig_theme};

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
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("radio-window"),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([600.0, 820.0])
                .with_min_inner_size([480.0, 420.0]),
            |ui, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let w = ui.available_width().clamp(460.0, 760.0);
                            radio_body(ui, w, true, st, &mut tune, &mut out, &mut prefs, &mut rui);
                        });
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        egui::Window::new(title)
            .id(egui::Id::new("radio-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(false)
            .collapsible(false)
            .default_pos([180.0, 48.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                let room = (ctx.content_rect().height() - 120.0).max(240.0);
                egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).show(
                    ui,
                    |ui| {
                        radio_body(ui, 560.0, false, st, &mut tune, &mut out, &mut prefs, &mut rui);
                    },
                );
            });
    }
    ctx.data_mut(|d| {
        d.insert_persisted(prefs_id(), prefs);
        d.insert_temp(ui_id(), rui);
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn radio_body(
    ui: &mut egui::Ui,
    width: f32,
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
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;
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
                let lcd_w = ui.available_width() - 104.0 - 14.0;
                face::lcd(ui, &mut f, prefs, lcd_w);
                ui.add_space(6.0);
                face::knobs(ui, &mut f, prefs);
            });

            ui.separator();
            controls::keys(ui, &mut f);
            controls::popout(ui, &mut f, prefs);
            controls::vfo_row(ui, &mut f);
            ui.add_space(2.0);
            f.out.tune = tune_strip(ui, s, tune);
            controls::readouts(ui, &f);
            for n in [f.st.digi_note.clone(), f.st.note.clone(), s.set_note.clone()]
                .into_iter()
                .flatten()
            {
                ui.label(RichText::new(n).size(11.5).color(th.dim));
            }
        });

    let ctx = ui.ctx().clone();
    if f.st.menu {
        controls::menu_window(&ctx, &mut f, prefs);
    }
    controls::confirm(&ctx, &mut f);
}
