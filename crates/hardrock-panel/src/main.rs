//! Hardrock Panel: a standalone control panel for a HobbyPCB Hardrock-500
//! amplifier and its ATU-500 tuner, on the amplifier's USB serial port.
//!
//! It is the AMP window from SDRoxide, running on its own. In SDRoxide the
//! radio's own readings and its dial came from SDRoxide; here they come from
//! the radio's CAT port directly, read only ([`rigcat`]), when one is set up:
//!
//! - **Readings in bypass.** In OFF the amplifier reports no FWD, RFL or SWR.
//!   With the FDM-DUO connected they are the radio's own (`FP`, `RP`, `WR`), as
//!   in SDRoxide; without it, FWD shows the amplifier's DRV, the radio's power
//!   passing straight through (`docs/design-decisions.md`, decisions 2 and 7).
//! - **Band.** With the radio connected the amplifier's band can follow its
//!   dial, between transmissions only (interlock 3). Without it the band is
//!   switched here (DN/UP) or by the radio over the amplifier's own CAT or ACC
//!   input, and the panel says so along its top.
//!
//! The safety interlocks are listed in `docs/safety-interlocks.md`.

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
mod radio;
mod settings;

use eframe::egui::{self, RichText};
use hardrock::{AmpAction, AmpConfig, AmpLink, AmpReadings, AmpStatus, Band};
use rigcat::{RigConfig, RigLink, RigStatus};
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
    rig: RigConfig,
    swr_warn: f32,
    /// The meters' needle picks (see [`AmpMeters::saved`]).
    meters: Option<[u8; 4]>,
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            amp: AmpConfig::default(),
            rig: RigConfig::default(),
            swr_warn: DEFAULT_SWR_WARN,
            meters: None,
        }
    }
}

struct App {
    saved: Saved,
    link: Option<AmpLink>,
    rig: Option<RigLink>,
    view: AmpMeters,
    show_settings: bool,
    show_radio: bool,
    /// The settings as being edited, applied with APPLY.
    draft: AmpConfig,
    draft_rig: RigConfig,
    ports: Vec<String>,
    /// The band last sent to the amplifier from the radio's dial, and whether
    /// the amplifier was connected then, so a reconnect sends it again.
    band_told: Option<Band>,
    amp_was_connected: bool,
    /// A band button pressed here, when, and the band the amplifier was on.
    band_press: Option<(std::time::Instant, Option<u8>)>,
    /// The last band press here was undone by the amplifier: its band is
    /// set by the radio's cable (decision 13).
    band_held: bool,
}

/// How long after DN or UP the amplifier is given to show the new band. The
/// HR500 reads a radio's band lines three or four times a second, so a band
/// the cable holds is back well inside this.
const BAND_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut saved: Saved =
            cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        apply_args(&mut saved.amp, &mut saved.rig, std::env::args().skip(1));
        let mut view = AmpMeters::default();
        if let Some(m) = saved.meters {
            view.restore(m);
        }
        let link = saved.amp.enabled.then(|| AmpLink::start(saved.amp.clone()));
        let rig = saved.rig.enabled.then(|| RigLink::start(saved.rig.clone()));
        // Straight to the settings on the first run, when there is nothing to
        // connect to yet.
        let show_settings = !saved.amp.enabled;
        App {
            draft: saved.amp.clone(),
            draft_rig: saved.rig.clone(),
            saved,
            link,
            rig,
            view,
            show_settings,
            show_radio: false,
            ports: serial_ports(),
            band_told: None,
            amp_was_connected: false,
            band_press: None,
            band_held: false,
        }
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
        self.saved.rig = self.draft_rig.clone();
        self.rig = None;
        if self.saved.rig.enabled {
            self.rig = Some(RigLink::start(self.saved.rig.clone()));
        }
        self.band_told = None;
    }

    /// See whether a band press here sticks. If the amplifier is back on its
    /// old band once it has had time to switch, something else sets its
    /// band: on AJ3G's station, the FDM-DUO over the cable between them, with
    /// the HR500's Transceiver menu on ELAD FDM DUO. The firmware has no
    /// command to change that menu, so the panel says where to (decision 13).
    fn watch_band(&mut self, st: &AmpStatus, actions: &[AmpAction]) {
        let r = st.readings;
        let pressed = actions
            .iter()
            .any(|a| matches!(a, AmpAction::BandUp | AmpAction::BandDown | AmpAction::SetBand(_)));
        // A press refused for RF (interlocks 1 and 2) is not the cable's doing.
        if pressed && st.connected && r.is_some_and(|r| !rf_present(&r)) {
            self.band_press = Some((std::time::Instant::now(), r.and_then(|r| r.band)));
        }
        if let Some((at, before)) = self.band_press
            && at.elapsed() >= BAND_SETTLE
        {
            self.band_press = None;
            let now = r.and_then(|r| r.band);
            if before.is_some() && now.is_some() {
                self.band_held = now == before;
            }
        }
    }

    /// Keep the amplifier's band on the radio's, when asked to. The link holds
    /// each change until the amplifier shows no RF (interlock 3); this side
    /// also waits while the radio says it is transmitting.
    fn follow_band(&mut self, amp: &AmpStatus, rig: Option<&RigStatus>) {
        let reconnected = amp.connected && !self.amp_was_connected;
        self.amp_was_connected = amp.connected;
        let (Some(link), Some(rig)) = (&self.link, rig) else { return };
        if !self.saved.rig.follow_band || !amp.connected || !rig.connected {
            return;
        }
        let Some(hz) = rig.freq_hz else { return };
        let Some(band) = band_of(hz) else { return };
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #3): not mid-over.
        // The link checks the amplifier again before sending.
        if rig.keyed() || amp.readings.is_some_and(|r| rf_present(&r)) {
            return;
        }
        if reconnected || self.band_told != Some(band) {
            link.set_frequency(hz);
            self.band_told = Some(band);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context, st: &AmpStatus, rig: Option<&RigStatus>) {
        let mut open = self.show_settings;
        let mut apply = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .frame(chrome::window_frame())
            .resizable(false)
            .collapsible(false)
            // Below the top bar, so it doesn't cover the SETTINGS chip.
            .default_pos([24.0, 48.0])
            .default_width(460.0)
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                // Both sections are taller than a small window: they scroll,
                // with APPLY kept in view below them.
                let room = (ctx.content_rect().height() - 150.0).max(200.0);
                egui::ScrollArea::vertical().max_height(room).show(ui, |ui| {
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
                    );
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    settings::settings_radio_tab(
                        ui,
                        &mut self.draft_rig,
                        &self.draft.serial.path,
                        rig,
                        &self.ports,
                    );
                });
                ui.add_space(8.0);
                apply = settings::apply_button(ui);
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
        let rig = self.rig.as_ref().map(RigLink::status);
        self.follow_band(&st, rig.as_ref());
        // In bypass the amplifier reports no FWD, RFL or SWR: the radio's own
        // readings fill them, or with no radio FWD falls back to the
        // amplifier's DRV (see `panel::from_rig`).
        if let Some(r) = st.readings {
            let (r, fill) = fill_from_radio(r, rig.as_ref());
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
                    if chrome::chip(ui, self.show_radio, "RADIO")
                        .on_hover_text(
                            "The radio's own readings and menu settings, over its CAT port",
                        )
                        .clicked()
                    {
                        self.show_radio = !self.show_radio;
                    }
                    ui.add_space(8.0);
                    let (text, col) = if self.band_held {
                        (
                            "Band held by the radio's cable. To switch it here, set the HR500's \
                             Transceiver menu to None."
                                .to_string(),
                            theme::YELLOW(),
                        )
                    } else {
                        radio_notice(rig.as_ref(), self.saved.rig.follow_band)
                    };
                    let notice = ui.label(RichText::new(text).size(11.0).color(col));
                    if self.band_held {
                        notice.on_hover_text(
                            "DN or UP was pressed and the amplifier went straight back to its \
                             band. With its Transceiver menu set to a radio (ELAD FDM DUO here), \
                             the HR500 takes its band from the radio's cable and puts back any \
                             other. Its firmware has no command to change that menu, so it is \
                             set on the amplifier: MENU, Transceiver, None. With the FDM-DUO \
                             connected under SETTINGS, the panel can follow the radio's band \
                             instead, and that can be turned off.",
                        );
                    }
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
        self.watch_band(&st, &out.actions);
        if out.settings {
            self.show_settings = true;
        }
        if self.show_settings {
            self.settings_window(ctx, &st, rig.as_ref());
        }
        if self.show_radio {
            let mut open = true;
            let mut refresh = false;
            radio::radio_window(ctx, &mut open, rig.as_ref(), &mut refresh);
            if refresh && let Some(r) = &self.rig {
                r.read_settings();
            }
            self.show_radio = open;
        }

        self.saved.meters = Some(self.view.saved());
        // The link updates on its own thread: look again a few times a second.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.saved);
    }
}

/// `--port COM5` and `--baud 115200` on the command line choose the
/// amplifier's port and turn its link on, ahead of the saved settings, for a
/// shortcut that opens straight onto one amplifier. `--radio COM3` and
/// `--radio-baud 38400` do the same for the radio.
fn apply_args(cfg: &mut AmpConfig, rig: &mut RigConfig, mut args: impl Iterator<Item = String>) {
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
            "--radio" => {
                if let Some(p) = args.next() {
                    rig.path = p;
                    rig.enabled = true;
                }
            }
            "--radio-baud" => {
                if let Some(b) = args.next().and_then(|b| b.parse().ok()) {
                    rig.baud = b;
                }
            }
            _ => {}
        }
    }
}

/// The amplifier shows RF: keyed, drive or output (as interlocks 1 and 2).
fn rf_present(r: &AmpReadings) -> bool {
    r.drive_w >= 0.1 || r.fwd_w >= 1.0
}

/// The amateur band `hz` is in, for the amplifier's band switching.
fn band_of(hz: u64) -> Option<Band> {
    const BANDS: [(u64, u64, Band); 10] = [
        (1_800_000, 2_000_000, Band::M160),
        (3_500_000, 4_000_000, Band::M80),
        (5_250_000, 5_450_000, Band::M60),
        (7_000_000, 7_300_000, Band::M40),
        (10_100_000, 10_150_000, Band::M30),
        (14_000_000, 14_350_000, Band::M20),
        (18_068_000, 18_168_000, Band::M17),
        (21_000_000, 21_450_000, Band::M15),
        (24_890_000, 24_990_000, Band::M12),
        (28_000_000, 29_700_000, Band::M10),
    ];
    BANDS.iter().find(|(lo, hi, _)| (*lo..=*hi).contains(&hz)).map(|b| b.2)
}

/// The radio's readings into the amplifier's, for bypass (`panel::from_rig`).
///
/// With the radio connected its meters are passed only while it transmits,
/// as SDRoxide passed its own: when the radio trusts its forward power, or
/// the amplifier shows the radio's drive (below 0.5 W, and in a fold-back,
/// the DUO flags its reading). A radio that measures its own watts never
/// borrows DRV. With no radio, FWD falls back to DRV.
fn fill_from_radio(r: AmpReadings, rig: Option<&RigStatus>) -> (AmpReadings, panel::RigFill) {
    match rig.filter(|s| s.connected) {
        Some(s) => {
            let keyed = s.keyed() || r.drive_w >= 0.1;
            let tx = keyed.then(|| TxMeters {
                fwd_w: s.fwd_w(),
                swr: s.swr_ratio(),
                rfl_w: s.rfl_w(),
                ..TxMeters::default()
            });
            panel::from_rig(r, tx.as_ref(), true)
        }
        None => panel::from_rig(r, Some(&TxMeters::default()), false),
    }
}

/// The line along the top: where the band comes from, and the radio's state.
fn radio_notice(rig: Option<&RigStatus>, follow: bool) -> (String, egui::Color32) {
    match rig {
        Some(s) if s.connected => {
            let band = if follow {
                "The amplifier's band follows it between transmissions."
            } else {
                "Switch bands with DN and UP, or over the amplifier's own CAT or ACC input."
            };
            (format!("{} connected. {band}", s.model.label()), theme::CYAN())
        }
        Some(s) => (
            format!("Radio: {}", s.error.clone().unwrap_or_else(|| "connecting…".to_string())),
            theme::YELLOW(),
        ),
        None => (
            "No radio connected: switch bands with DN and UP, or let the radio set them over \
             the amplifier's CAT or ACC input."
                .to_string(),
            theme::CYAN(),
        ),
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
        let (mut c, mut r) = (AmpConfig::default(), RigConfig::default());
        let args =
            ["--port", "COM5", "--baud", "38400", "--radio", "COM3", "--radio-baud", "57600"]
                .map(String::from);
        apply_args(&mut c, &mut r, args.into_iter());
        assert!(c.enabled);
        assert_eq!(c.serial.path, "COM5");
        assert_eq!(c.serial.baud, 38_400);
        assert!(r.enabled);
        assert_eq!(r.path, "COM3");
        assert_eq!(r.baud, 57_600);
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

    fn duo(fwd: &str, rfl: &str, swr: &str) -> RigStatus {
        RigStatus {
            connected: true,
            fwd: rigcat::elad::parse_watts(fwd),
            rfl: rigcat::elad::parse_watts(rfl),
            swr: rigcat::elad::parse_swr(swr),
            ..RigStatus::default()
        }
    }

    #[test]
    fn with_the_duo_bypass_readings_are_the_radios_own() {
        // In OFF, 6.09 W through the bypassed amp: FWD, RFL and SWR from the DUO.
        let r = AmpReadings { ptt: false, drive_w: 6.5, ..AmpReadings::default() };
        let (out, fill) = fill_from_radio(r, Some(&duo(" 006091", " 000120", "0 01.32")));
        assert_eq!(out.fwd_w, 6.091);
        assert_eq!(out.rfl_w, 0.12);
        assert_eq!(out.swr, 1.32);
        assert_eq!(fill.fwd, Some("rig"));
    }

    #[test]
    fn a_flagged_duo_reading_is_never_replaced_by_drv() {
        // Keyed into an open the DUO folds back and flags its power; DRV
        // over-reads into the mismatch and must not stand in for it.
        let r = AmpReadings { ptt: false, drive_w: 9.8, ..AmpReadings::default() };
        let (out, fill) = fill_from_radio(r, Some(&duo("!002940", "!000000", "1!00.00")));
        assert_eq!(out.fwd_w, 0.0);
        assert_eq!(fill.fwd, Some("flagged"));
    }

    #[test]
    fn in_receive_the_duo_fills_nothing() {
        let r = AmpReadings { ptt: false, ..AmpReadings::default() };
        let (out, _) = fill_from_radio(r, Some(&duo("!000000", "!000000", "0!00.00")));
        assert_eq!(out, r);
    }

    #[test]
    fn in_line_the_amplifiers_own_readings_stand() {
        let r = AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..AmpReadings::default() };
        let (out, _) = fill_from_radio(r, Some(&duo(" 006091", " 000001", "0 01.02")));
        assert_eq!(out.fwd_w, 400.0);
    }

    #[test]
    fn the_band_comes_from_the_dial() {
        assert_eq!(band_of(14_074_000), Some(Band::M20));
        assert_eq!(band_of(1_840_000), Some(Band::M160));
        assert_eq!(band_of(28_074_000), Some(Band::M10));
        assert_eq!(band_of(50_313_000), None);
        assert_eq!(band_of(15_000_000), None);
    }

    #[test]
    fn older_settings_get_a_radio_at_the_duos_own_rate() {
        let s: Saved = serde_json::from_str(r#"{"swr_warn": 3.0}"#).unwrap();
        assert!(!s.rig.enabled);
        assert_eq!(s.rig.baud, 38_400);
    }
}
