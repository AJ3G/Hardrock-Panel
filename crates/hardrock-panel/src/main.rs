//! Hardrock Panel: a standalone control panel for a HobbyPCB Hardrock-500
//! amplifier and its ATU-500 tuner, on the amplifier's USB serial port.
//!
//! It is the AMP window from SDRoxide, running on its own. Without a radio
//! program beside it, two things change:
//!
//! - **Band.** Nothing tells the amplifier the radio's frequency, so its band is
//!   switched here (BAND DN/UP) or by the radio over the amplifier's own CAT or
//!   ACC input. The panel says so along its top.
//! - **Readings in bypass.** In OFF the amplifier reports no FWD, RFL or SWR,
//!   and there is no radio to ask, so FWD shows the amplifier's DRV, which is
//!   the radio's power passing straight through (`docs/design-decisions.md`,
//!   decision 2).
//!
//! The safety interlocks that live in the panel and the link are unchanged
//! (`docs/safety-interlocks.md`). Interlock 3, the band following the radio
//! only between transmissions, has no radio to follow here.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// SDRoxide's look, carried over whole rather than trimmed, so that a change to
// it there can be copied across as it stands. The panel uses only part of it.
#[allow(dead_code, unused_imports)]
mod chrome;
#[allow(dead_code, unused_imports)]
mod layout;
#[allow(dead_code, unused_imports)]
mod repaint;
#[allow(dead_code, unused_imports)]
mod style;
#[allow(dead_code, unused_imports)]
mod theme;

mod panel;
mod settings;

use eframe::egui::{self, RichText};
use hardrock::{AmpConfig, AmpLink, AmpStatus};
use serde::{Deserialize, Serialize};

use panel::{AmpMeters, TxMeters};

/// Where eframe keeps what's saved between runs.
const STORAGE_KEY: &str = "hardrock-panel";

/// The SWR warning SDRoxide gives with its default settings: its transmitter's
/// tune limit, 5:1.
const DEFAULT_SWR_WARN: f32 = 5.0;

/// Everything kept from one run to the next. The window's size and place are
/// kept by eframe itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    amp: AmpConfig,
    swr_warn: f32,
    /// The meters' needle picks (see [`AmpMeters::saved`]).
    meters: Option<[u8; 4]>,
}

impl Default for Saved {
    fn default() -> Self {
        Saved { amp: AmpConfig::default(), swr_warn: DEFAULT_SWR_WARN, meters: None }
    }
}

struct App {
    saved: Saved,
    link: Option<AmpLink>,
    view: AmpMeters,
    show_settings: bool,
    /// The settings as being edited, applied with APPLY.
    draft: AmpConfig,
    ports: Vec<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut saved: Saved =
            cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        apply_args(&mut saved.amp, std::env::args().skip(1));
        let mut view = AmpMeters::default();
        if let Some(m) = saved.meters {
            view.restore(m);
        }
        let link = saved.amp.enabled.then(|| AmpLink::start(saved.amp.clone()));
        // Straight to the settings on the first run, when there is nothing to
        // connect to yet.
        let show_settings = !saved.amp.enabled;
        App { draft: saved.amp.clone(), saved, link, view, show_settings, ports: serial_ports() }
    }

    fn status(&self) -> AmpStatus {
        match &self.link {
            Some(l) => l.status(),
            None => AmpStatus { model: self.saved.amp.model, ..AmpStatus::default() },
        }
    }

    /// Save the settings and (re)open the port. The old link is dropped first,
    /// which closes the port, so the new one can open it.
    fn apply(&mut self) {
        self.saved.amp = self.draft.clone();
        self.link = None;
        if self.saved.amp.enabled {
            self.link = Some(AmpLink::start(self.saved.amp.clone()));
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context, st: &AmpStatus) {
        let mut open = self.show_settings;
        let mut apply = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .frame(chrome::window_frame())
            .resizable(false)
            .collapsible(false)
            // Below the top bar, so it doesn't cover the SETTINGS chip.
            .default_pos([24.0, 48.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                ui.horizontal(|ui| {
                    if chrome::chip(ui, false, "REFRESH PORTS")
                        .on_hover_text("Look again for serial ports, after plugging the amp in")
                        .clicked()
                    {
                        self.ports = serial_ports();
                    }
                });
                ui.add_space(6.0);
                settings::settings_amp_tab(
                    ui,
                    &mut self.draft,
                    &mut self.saved.swr_warn,
                    st,
                    &self.ports,
                    &mut apply,
                );
            });
        if apply {
            self.apply();
        }
        self.show_settings = open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let mut st = self.status();
        // In bypass the amplifier reports no FWD; with no radio to ask, FWD
        // falls back to its DRV (see `panel::from_rig`).
        if let Some(r) = st.readings {
            let (r, fill) = panel::from_rig(r, Some(&TxMeters::default()), false);
            st.readings = Some(r);
            self.view.set_rig(fill);
        }
        self.view.set_swr_warn(self.saved.swr_warn);

        egui::Panel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_DEEP())
                    .inner_margin(egui::Margin::symmetric(11, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if chrome::chip(ui, self.show_settings, "SETTINGS").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "No radio connected: switch bands with DN and UP, or let the radio \
                             set them over the amplifier's CAT or ACC input.",
                        )
                        .size(11.0)
                        .color(theme::CYAN()),
                    );
                });
            });

        let out = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
            .show(ui, |ui| {
                chrome::window_body_bg(ui);
                panel::fit_panel(ui, |ui, scale| {
                    panel::amp_panel(ui, &st, &mut self.view, None, Some(scale))
                })
            })
            .inner;
        if let Some(link) = &self.link {
            for &a in &out.actions {
                link.action(a);
            }
        }
        if out.settings {
            self.show_settings = true;
        }
        if self.show_settings {
            self.settings_window(ctx, &st);
        }

        self.saved.meters = Some(self.view.saved());
        // The link updates on its own thread: look again a few times a second.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.saved);
    }
}

/// `--port COM5` and `--baud 115200` on the command line choose the port
/// and turn the link on, ahead of the saved settings, for a shortcut that
/// opens straight onto one amplifier.
fn apply_args(cfg: &mut AmpConfig, mut args: impl Iterator<Item = String>) {
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                if let Some(p) = args.next() {
                    cfg.serial.path = p;
                    cfg.enabled = true;
                }
            }
            "--baud" => {
                if let Some(b) = args.next().and_then(|b| b.parse().ok()) {
                    cfg.serial.baud = b;
                }
            }
            _ => {}
        }
    }
}

/// The serial ports on this machine, by name.
fn serial_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    ports.sort();
    ports
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Hardrock Panel")
            .with_inner_size([760.0, 820.0])
            .with_min_inner_size([520.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("Hardrock Panel", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardrock::AmpReadings;

    #[test]
    fn a_first_run_warns_at_sdroxides_tune_limit() {
        let s = Saved::default();
        assert_eq!(s.swr_warn, 5.0);
        assert!(!s.amp.enabled);
        assert_eq!(s.amp.serial.baud, 115_200);
    }

    #[test]
    fn settings_from_an_older_file_fill_in_what_is_missing() {
        let s: Saved = serde_json::from_str(r#"{"swr_warn": 3.0}"#).unwrap();
        assert_eq!(s.swr_warn, 3.0);
        assert_eq!(s.amp, AmpConfig::default());
    }

    #[test]
    fn the_command_line_chooses_the_port_and_turns_the_link_on() {
        let mut c = AmpConfig::default();
        let args = ["--port", "COM5", "--baud", "38400"].map(String::from);
        apply_args(&mut c, args.into_iter());
        assert!(c.enabled);
        assert_eq!(c.serial.path, "COM5");
        assert_eq!(c.serial.baud, 38_400);
    }

    #[test]
    fn with_no_radio_bypass_fwd_comes_from_drv() {
        // In OFF the amplifier reports no FWD; its DRV is the radio's power
        // passing straight through.
        let r = AmpReadings { ptt: false, drive_w: 6.0, ..AmpReadings::default() };
        let (out, _) = panel::from_rig(r, Some(&TxMeters::default()), false);
        assert_eq!(out.fwd_w, 6.0);
        // In line, the amplifier's own FWD is left alone.
        let keyed = AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..AmpReadings::default() };
        assert_eq!(panel::from_rig(keyed, Some(&TxMeters::default()), false).0.fwd_w, 400.0);
    }
}
