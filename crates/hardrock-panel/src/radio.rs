//! The RADIO window: what the radio itself reports over its CAT port, read
//! only. Live readings at the top, then every setting the radio answers a
//! READ for, with its menu number where it is one of the radio's menus.

use eframe::egui::{self, RichText};
use rigcat::RigStatus;

use crate::{chrome, theme};

pub(crate) fn radio_window(
    ctx: &egui::Context,
    open: &mut bool,
    st: Option<&RigStatus>,
    refresh: &mut bool,
) {
    egui::Window::new("Radio")
        .open(open)
        .frame(chrome::window_frame())
        .resizable(false)
        .collapsible(false)
        .default_pos([180.0, 48.0])
        .show(ctx, |ui| {
            chrome::window_body_bg(ui);
            let Some(s) = st else {
                ui.label(
                    "No radio is set up. Choose its CAT port under SETTINGS, in the Radio section.",
                );
                return;
            };
            ui.label(RichText::new(s.model.label()).size(14.0).strong().color(theme::CYAN()));
            if !s.connected {
                ui.label(
                    RichText::new(s.error.clone().unwrap_or_else(|| "Connecting…".into()))
                        .color(theme::ALERT()),
                );
            }
            ui.add_space(6.0);

            // Live: raw, never held.
            let w = |p: Option<rigcat::elad::Watts>| match p {
                Some(p) if p.valid => format!("{:.2} W", p.w),
                Some(_) => "—  (flagged)".into(),
                None => "—".into(),
            };
            let swr = match s.swr {
                Some(x) if x.valid => format!("{:.2}:1", x.ratio),
                Some(_) => "—  (flagged)".into(),
                None => "—".into(),
            };
            let tripped = s.swr.is_some_and(|x| x.tripped);
            egui::Grid::new("radio-live").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                ui.label("Transmitting");
                ui.label(if s.keyed() { "Yes" } else { "No" });
                ui.end_row();
                ui.label("FWD (FP)");
                ui.label(RichText::new(w(s.fwd)).monospace());
                ui.end_row();
                ui.label("RFL (RP)");
                ui.label(RichText::new(w(s.rfl)).monospace());
                ui.end_row();
                ui.label("SWR (WR)");
                ui.label(RichText::new(swr).monospace());
                ui.end_row();
                if tripped {
                    ui.label("");
                    ui.label(
                        RichText::new("The radio dropped to receive on a high SWR")
                            .color(theme::ALERT()),
                    );
                    ui.end_row();
                }
                ui.label("TX POWER (menu 34)");
                ui.label(s.tx_power.clone().unwrap_or_else(|| "—".into()));
                ui.end_row();
                ui.label("Dial");
                let dial = match (s.freq_hz, s.mode) {
                    (Some(hz), m) => {
                        format!("{:.6} MHz {}", hz as f64 / 1e6, m.unwrap_or(""))
                    }
                    _ => "—".into(),
                };
                ui.label(RichText::new(dial).monospace());
                ui.end_row();
            });
            ui.label(
                RichText::new(
                    "The radio flags its power and SWR below 0.5 W and in receive. In OFF \
                     (bypass) these fill the amplifier's FWD, RFL and SWR.",
                )
                .weak()
                .size(11.0),
            );

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").strong());
                if chrome::chip(ui, false, "READ AGAIN")
                    .on_hover_text("Ask the radio for every setting now. Between transmissions they are read every 10 s by themselves.")
                    .clicked()
                {
                    *refresh = true;
                }
                if let Some(t) = s.settings_read {
                    ui.label(
                        RichText::new(format!("read {} s ago", t.elapsed().as_secs()))
                            .weak()
                            .size(11.0),
                    );
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("radio-settings").num_columns(4).spacing([14.0, 3.0]).striped(true).show(
                ui,
                |ui| {
                    for h in ["Menu", "Setting", "Value", "Answer"] {
                        ui.label(RichText::new(h).weak());
                    }
                    ui.end_row();
                    if s.connected || !s.settings.is_empty() {
                        ui.label("70");
                        ui.label("CAT BAUD");
                        ui.label(format!("{} (the port is open at it)", s.baud));
                        ui.label("");
                        ui.end_row();
                    }
                    for row in &s.settings {
                        ui.label(row.item.menu.map(|m| m.to_string()).unwrap_or_default());
                        ui.label(row.item.name);
                        let (v, col) = match (&row.value, row.raw.as_deref()) {
                            (Some(v), _) => (v.clone(), theme::TEXT()),
                            (None, Some("?")) => ("Not answered: the radio refused it".into(), theme::TEXT()),
                            (None, Some(_)) => ("Answer not understood".into(), theme::YELLOW()),
                            (None, None) => ("No answer".into(), theme::TEXT()),
                        };
                        ui.label(RichText::new(v).color(col));
                        ui.label(RichText::new(row.raw.clone().unwrap_or_default()).monospace().weak());
                        ui.end_row();
                    }
                },
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Only these settings have a CAT command. The other menus, such as 37 CW IN \
                     and 45 keyer speed, can only be read on the radio.",
                )
                .weak()
                .size(11.0),
            );
        });
}
