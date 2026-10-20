//! The SCOPE window: the panadapter scope ([`crate::scope`]) with the panel's
//! receiver along its top (where the I/Q comes from, the receiver's sound,
//! and TX FOLLOWS RX). It sits over the panel, or pops out into a window of
//! its own, as the radio's window does.

use eframe::egui::{self, RichText};

use crate::rx::{DUO_AUDIO_MAX_RATE, IqBlock, IqChoice, IqSink, Rx, RxSettings};
use crate::scope::{Scope, ScopeOut, ScopeRig};
use crate::{chrome, theme};

/// What the operator asked of the window itself this frame.
#[derive(Default)]
pub(crate) struct WinOut {
    pub scope: Option<ScopeOut>,
    /// Pop out (`true`) or dock (`false`).
    pub pop: Option<bool>,
    /// Open the I/Q source again (after an error).
    pub reopen: bool,
    /// Open SETTINGS.
    pub settings: bool,
}

/// What the window shows besides the scope.
pub(crate) struct Args<'a> {
    pub rx: &'a Rx,
    pub settings: &'a mut RxSettings,
    pub rig: ScopeRig,
    /// The radio's CAT link is connected (split can be set).
    pub cat: bool,
    /// What the outside-program links are doing, one line each.
    pub links: Vec<String>,
    /// Why the panel thinks the radio is transmitting, when it does.
    pub tx_why: Option<String>,
}

/// Draw the window, docked or popped out. Clears `open` when it is closed.
pub(crate) fn scope_window(
    ctx: &egui::Context,
    open: &mut bool,
    popped: bool,
    scope: &mut Scope,
    mut a: Args<'_>,
) -> WinOut {
    let mut out = WinOut::default();
    if popped {
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("scope-window"),
            egui::ViewportBuilder::default()
                .with_title("Scope")
                .with_inner_size([1200.0, 900.0])
                .with_min_inner_size([640.0, 480.0]),
            |ui, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            body(ui, true, scope, &mut a, &mut out);
                        });
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        let w = (ctx.content_rect().width() - 40.0).clamp(560.0, 1400.0);
        egui::Window::new("Scope")
            .id(egui::Id::new("scope-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(false)
            .collapsible(false)
            .default_pos([12.0, 44.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                ui.set_width(w);
                let room = (ctx.content_rect().height() - 110.0).max(300.0);
                egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).show(
                    ui,
                    |ui| {
                        body(ui, false, scope, &mut a, &mut out);
                    },
                );
            });
    }
    out
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

fn body(ui: &mut egui::Ui, popped: bool, scope: &mut Scope, a: &mut Args<'_>, out: &mut WinOut) {
    let rx = a.rx;
    let s = &mut *a.settings;
    ui.horizontal_wrapped(|ui| {
        caption(ui, "I/Q");
        let (text, col) = match (rx.source(), &rx.error) {
            (_, Some(e)) => (e.clone(), theme::ALERT()),
            (IqChoice::Off, None) => {
                ("Off: choose ELAD or Demo signals under SETTINGS".into(), theme::TEXT())
            }
            (_, None) if rx.opening() => ("Opening the ELAD…".into(), theme::CYAN()),
            (_, None) => (rx.label.clone(), theme::GREEN()),
        };
        ui.label(RichText::new(text).size(12.0).color(col));
        // Why the panel thinks the radio is transmitting, so a reading that
        // is wrong can be seen for what it is.
        if let Some(why) = a.tx_why.as_deref() {
            ui.label(
                RichText::new(format!("Transmitting: {why}")).size(12.0).color(theme::YELLOW()),
            );
        }
        if rx.error.is_some() && chrome::chip(ui, false, "TRY AGAIN").clicked() {
            out.reopen = true;
        }
        if let Some(log) = elad_iq::diagnostics()
            && chrome::chip(ui, false, "COPY ELAD LOG")
                .on_hover_text("Copy the ELAD driver's session log, for a bug report")
                .clicked()
        {
            ui.ctx().copy_text(log);
        }
        if chrome::chip(ui, false, "SETTINGS").on_hover_text("Where the I/Q comes from").clicked() {
            out.settings = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (text, hover) = if popped {
                ("DOCK", "Put the scope back over the panel")
            } else {
                ("POP OUT", "Open the scope in a window of its own")
            };
            if chrome::chip(ui, false, text).on_hover_text(hover).clicked() {
                out.pop = Some(!popped);
            }
        });
    });
    for n in &rx.notes {
        ui.label(RichText::new(n).size(11.5).color(theme::YELLOW()));
    }
    if let Some(w) = &rx.watchdog {
        ui.label(RichText::new(w).size(11.5).color(theme::YELLOW()));
    }

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Receiver");
        ui.label(
            RichText::new(format!(
                "{:.3} kHz {} {:.0}–{:.0} Hz",
                a.rig.rx_hz / 1e3,
                a.rig.mode.label(),
                a.rig.passband.0,
                a.rig.passband.1
            ))
            .monospace()
            .size(13.0)
            .color(theme::CYAN()),
        );
        caption(ui, "· Sound");
        ui.add(egui::Slider::new(&mut s.volume, 0.0..=1.0).show_value(false))
            .on_hover_text("The receiver's volume");
        chrome::checkbox(ui, &mut s.muted, "Mute");
        match (rx.audio_state(), &rx.audio_error) {
            (_, Some(e)) => {
                ui.label(RichText::new(e).size(11.5).color(theme::ALERT()));
            }
            (Some((dev, under, dropped)), None) => {
                let r = ui.label(RichText::new(dev).size(11.5).weak());
                r.on_hover_text(format!(
                    "Chosen under SETTINGS. {under} underruns, {dropped} samples dropped."
                ));
            }
            _ => {}
        }
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Transmit");
        let r = ui.add_enabled_ui(a.cat && rx.source() == IqChoice::Elad, |ui| {
            chrome::checkbox(ui, &mut s.tx_follows_rx, "TX follows RX")
        });
        r.inner
            .on_hover_text(
                "While the receiver is off VFO A, the radio is put in split with VFO B on the \
                 receiver, so it transmits where you listen. Split is taken off again when the \
                 receiver is back on VFO A. Not changed while the radio transmits.",
            )
            .on_disabled_hover_text("Needs the FDM-DUO's CAT link and its I/Q");
        let snap_rate = rx.rate();
        if rx.source() == IqChoice::Elad && snap_rate > DUO_AUDIO_MAX_RATE as f64 {
            ui.label(
                RichText::new(
                    "Above 192 kHz the DUO mutes its own audio: what you hear is the panel's \
                     receiver.",
                )
                .size(11.5)
                .weak(),
            );
        }
        if a.rig.transmitting {
            ui.label(
                RichText::new("Transmitting: tuning is locked.").size(11.5).color(theme::YELLOW()),
            );
        }
    });
    for l in &a.links {
        ui.label(RichText::new(l).size(11.0).weak());
    }
    ui.add_space(4.0);

    if !rx.active() {
        ui.add_space(20.0);
        ui.label(RichText::new("No I/Q: the scope starts when a source is chosen.").weak());
        return;
    }
    let snap = rx.snapshot();
    out.scope = Some(scope.ui(ui, &snap, a.rig));
    scope.scf_tap(&mut |sink| {
        let sink = sink.map(|s| Box::new(move |b: &IqBlock| s(b.iq, b.rate)) as IqSink);
        rx.taps.set_iq("scf", sink);
    });
    if snap.spectrum_drops > 0 {
        ui.label(
            RichText::new(format!(
                "Spectrum: {} I/Q blocks dropped while the FFT was busy (the receiver's audio \
                 was not).",
                snap.spectrum_drops
            ))
            .size(11.0)
            .weak(),
        );
    }
}
