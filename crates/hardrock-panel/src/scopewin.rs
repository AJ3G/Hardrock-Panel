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
    /// Swap this slice with A (decision 66).
    pub swap_slice: Option<u32>,
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
    /// The sound outputs a slice can play on.
    pub sounds: &'a [String],
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
        // Painted in place: no background GPU submit meanwhile (gpu_gate).
        let _shut = crate::gpu_gate::shut();
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("scope-window"),
            crate::popout::builder(ctx, "scope-window", "Scope", [1200.0, 900.0], [640.0, 480.0]),
            |ui, _class| {
                crate::popout::track(ui.ctx(), "scope-window");
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
        // Its edges and corner can be dragged both ways (AJ3G, 21 Oct 2026:
        // "I am unable to change the size of the window when docked"). The
        // scope takes the window's width, and scrolls within its height.
        // First opened, it fills the screen as it always did; after that it
        // keeps the size it was given, from run to run, as egui keeps every
        // window's.
        let screen = ctx.content_rect();
        let w = (screen.width() - 24.0).clamp(MIN_W, 1430.0);
        let h = (screen.height() - 56.0).max(MIN_H);
        egui::Window::new("Scope")
            .id(egui::Id::new("scope-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([12.0, 44.0])
            .default_size([w, h])
            .min_size([MIN_W, MIN_H])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                fill_scroll(ui, "scope-docked", |ui| body(ui, false, scope, &mut a, &mut out));
            });
    }
    out
}

/// The smallest the docked window can be dragged to: room for the scope's
/// control rows without the trace going to nothing.
const MIN_W: f32 = 560.0;
const MIN_H: f32 = 320.0;

/// A vertical scroll area that fills the room in `ui` whatever its contents
/// measure, for the body of a window that can be resized: one that shrank to
/// its contents would leave the window's free edge where the contents end,
/// and one that measured more than the room would push the window out
/// again as fast as it was dragged in.
pub(crate) fn fill_scroll<R>(
    ui: &mut egui::Ui,
    salt: &'static str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(ui, add).inner
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
            // Wider than stand-alone the DUO is sent an image first, which
            // takes several seconds (`elad_iq::remote`).
            (_, None) if rx.opening() && rx.asked_rate() > DUO_AUDIO_MAX_RATE => (
                format!(
                    "Opening the ELAD and loading its {} kHz image, about 7 s…",
                    rx.asked_rate() / 1000
                ),
                theme::CYAN(),
            ),
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
        } else if rx.error.is_none()
            && rx.source() == IqChoice::Elad
            && !rx.opening()
            && chrome::chip(ui, false, "REOPEN")
                .on_hover_text(
                    "Close the ELAD's I/Q and open it again, as at start, without closing the \
                     panel. Waits if the radio is transmitting.",
                )
                .clicked()
        {
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
    if let Some(w) = &rx.rate_note {
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
        // A MUTE key beside the slider, lit in the selected-chip blue while
        // muted (a neutral state, so not yellow or red).
        if chrome::chip(ui, s.muted, "MUTE")
            .on_hover_text(if s.muted {
                "The receiver is muted: click for sound again"
            } else {
                "Mute the receiver's sound"
            })
            .clicked()
        {
            s.muted = !s.muted;
        }
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
        crate::slices::add_key(ui, s, rx);
    });
    let so = crate::slices::rows(ui, s, rx, a.sounds, a.rig.transmitting);
    out.swap_slice = so.swap;

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
    // The slices running, as coloured passbands on the scope.
    let running: Vec<u32> = snap.slice_levels.iter().map(|l| l.0).collect();
    scope.slices = s
        .slices
        .iter()
        .filter(|x| running.contains(&x.id))
        .map(|x| crate::scope::SliceMark {
            hz: x.rx_hz,
            passband: x.passband,
            colour: crate::slices::colour(x.id),
            name: crate::slices::name(x.id),
        })
        .collect();
    scope.tx_follows = s.tx_follows_rx;
    // With no stream (opening, or an open that failed), SPAN shows the rate
    // asked for rather than 0k.
    if snap.sample_rate <= 0.0 && rx.asked_rate() > 0 {
        scope.last_rate = rx.asked_rate() as f64;
    }
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
