//! The ELAD TMate 2 USB controller, as a second face for the radio window
//! (decision 53): its main dial, E1 and E2 are the window's main knob, E1
//! and E2, pushes and holds included, so the two always agree; its six keys
//! do what the operator picks in SETTINGS; its LCD shows the DUO's dial,
//! mode and S-meter, and the amplifier's power while transmitting.
//!
//! Everything it asks for goes out the way the panel's own controls send
//! it, so every interlock still holds: nothing to the radio while it
//! transmits (#5), no amplifier relay switched with RF on it (#2), no PTT
//! ON while overdriven (#4).

use std::time::Duration;

use eframe::egui::{self, RichText};
use hardrock::{AmpAction, AmpStatus};
use rigcat::RigStatus;
use serde::{Deserialize, Serialize};
use tmate2::{
    Backlights, Display, ModeIcon, Rgb, Tmate2Control, Tmate2Event, Tmate2Handle, TxReadout,
};

use crate::panel::{AmpKey, amp_key};
use crate::radio::hardware::{self, Hardware, HardwareView, PressTracker, RadioKey};
use crate::radio::logic;
use crate::{chrome, theme};

/// What one of F1 to F6 does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum KeyAction {
    Nothing,
    BandDown,
    BandUp,
    ModeDown,
    ModeUp,
    StepDown,
    StepUp,
    SwapVfo,
    AEqualsB,
    Split,
    Rit,
    RitClear,
    Lock,
    AmpPtt,
    AmpTune,
    AmpAtu,
    AmpAntenna,
    TxReadout,
}

impl KeyAction {
    pub(crate) const ALL: [KeyAction; 18] = [
        KeyAction::Nothing,
        KeyAction::BandDown,
        KeyAction::BandUp,
        KeyAction::ModeDown,
        KeyAction::ModeUp,
        KeyAction::StepDown,
        KeyAction::StepUp,
        KeyAction::SwapVfo,
        KeyAction::AEqualsB,
        KeyAction::Split,
        KeyAction::Rit,
        KeyAction::RitClear,
        KeyAction::Lock,
        KeyAction::AmpPtt,
        KeyAction::AmpTune,
        KeyAction::AmpAtu,
        KeyAction::AmpAntenna,
        KeyAction::TxReadout,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            KeyAction::Nothing => "Nothing",
            KeyAction::BandDown => "Band down (radio)",
            KeyAction::BandUp => "Band up (radio)",
            KeyAction::ModeDown => "Mode, previous",
            KeyAction::ModeUp => "Mode, next",
            KeyAction::StepDown => "Tuning step finer",
            KeyAction::StepUp => "Tuning step coarser",
            KeyAction::SwapVfo => "VFO A/B",
            KeyAction::AEqualsB => "A=B",
            KeyAction::Split => "SPLIT",
            KeyAction::Rit => "RIT on/off",
            KeyAction::RitClear => "RIT clear",
            KeyAction::Lock => "Lock the dials",
            KeyAction::AmpPtt => "Amp PTT ON/OFF",
            KeyAction::AmpTune => "Amp tuner TUNE",
            KeyAction::AmpAtu => "Amp tuner ON/BYP",
            KeyAction::AmpAntenna => "Amp antenna",
            KeyAction::TxReadout => "TX readout: FWD, REFL, SWR",
        }
    }
}

/// The small display's figure while transmitting, in words.
pub(crate) fn readout_label(r: TxReadout) -> &'static str {
    match r {
        TxReadout::Power => "Forward power",
        TxReadout::Reflected => "Reflected power",
        TxReadout::Swr => "SWR (13 = 1.3:1)",
    }
}

const READOUTS: [TxReadout; 3] = [TxReadout::Power, TxReadout::Reflected, TxReadout::Swr];

fn next_readout(r: TxReadout) -> TxReadout {
    let i = READOUTS.iter().position(|x| *x == r).unwrap_or(0);
    READOUTS[(i + 1) % READOUTS.len()]
}

/// SETTINGS → ELAD TMate 2, kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Tm2Settings {
    pub enabled: bool,
    /// Turn the main dial, E1, E2 the other way.
    pub reverse: [bool; 3],
    /// F1 to F6.
    pub keys: [KeyAction; 6],
    pub tx_readout: TxReadout,
    pub backlights: Backlights,
}

impl Default for Tm2Settings {
    fn default() -> Self {
        Tm2Settings {
            enabled: false,
            reverse: [false; 3],
            keys: [
                KeyAction::BandDown,
                KeyAction::BandUp,
                KeyAction::ModeDown,
                KeyAction::ModeUp,
                KeyAction::AmpPtt,
                KeyAction::AmpTune,
            ],
            tx_readout: TxReadout::Power,
            backlights: Backlights::default(),
        }
    }
}

/// How long a turned E1 setting stays on the small display.
const SHOW_SETTING: Duration = Duration::from_millis(2000);

/// The controller's worker and the pushes being timed.
pub(crate) struct Tm2 {
    handle: Tmate2Handle,
    enabled: bool,
    /// Main, E1 and E2 pushes: down now, and push-or-hold timing.
    down: [bool; 3],
    press: [PressTracker; 3],
    sent: Option<Display>,
}

/// What the controller asked for this frame, for the panel to send.
#[derive(Default)]
pub(crate) struct Tm2Out {
    pub hw: Hardware,
    pub amp: Vec<AmpAction>,
    /// An amplifier key refused, and why.
    pub note: Option<String>,
}

impl Tm2 {
    pub(crate) fn start(ctx: &egui::Context, enabled: bool) -> Self {
        let c = ctx.clone();
        Tm2 {
            handle: tmate2::spawn(enabled, move || c.request_repaint()),
            enabled,
            down: [false; 3],
            press: Default::default(),
            sent: None,
        }
    }

    /// Start or stop looking for the controller. Cheap to call every frame.
    pub(crate) fn set_enabled(&mut self, on: bool) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        self.handle.set_enabled(on);
        if !on {
            self.sent = None;
        }
    }

    pub(crate) fn status(&self) -> tmate2::Tmate2Status {
        self.handle.status()
    }

    /// Read what the controller did since last frame. `amp` is the
    /// amplifier as the panel shows it; `hold` the radio's hold time, so a
    /// push held is a hold as on the radio's own knobs.
    pub(crate) fn poll(
        &mut self,
        ctx: &egui::Context,
        cfg: &mut Tm2Settings,
        amp: &AmpStatus,
        hold: f64,
    ) -> Tm2Out {
        let mut out = Tm2Out::default();
        for ev in self.handle.poll() {
            match ev {
                Tmate2Event::Dial { control, delta } => {
                    let k = match control {
                        Tmate2Control::MainDial => 0,
                        Tmate2Control::E1Dial => 1,
                        Tmate2Control::E2Dial => 2,
                        _ => continue,
                    };
                    let d = if cfg.reverse[k] { -delta } else { delta };
                    out.hw.turns[k].steps += i64::from(d);
                    // A turn while held is no push, as on the face.
                    self.press[k].cancel();
                }
                Tmate2Event::Key { control, down } => {
                    let push = match control {
                        Tmate2Control::MainPush => Some(0),
                        Tmate2Control::E1Push => Some(1),
                        Tmate2Control::E2Push => Some(2),
                        _ => None,
                    };
                    if let Some(k) = push {
                        self.down[k] = down;
                        continue;
                    }
                    let f = match control {
                        Tmate2Control::F1 => 0,
                        Tmate2Control::F2 => 1,
                        Tmate2Control::F3 => 2,
                        Tmate2Control::F4 => 3,
                        Tmate2Control::F5 => 4,
                        Tmate2Control::F6 => 5,
                        _ => continue,
                    };
                    if down {
                        key(cfg, cfg.keys[f], amp, &mut out);
                    }
                }
                Tmate2Event::Connected(_) | Tmate2Event::Disconnected => {
                    self.down = [false; 3];
                    self.press = Default::default();
                    self.sent = None;
                }
            }
        }
        let now = ctx.input(|i| i.time);
        for k in 0..3 {
            let p = self.press[k].update(self.down[k], now, hold);
            out.hw.turns[k].press = p;
            if self.press[k].waiting() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        out
    }

    /// Send the LCD what it should show, if it changed.
    pub(crate) fn show(&mut self, d: Display) {
        if self.sent.as_ref() != Some(&d) {
            self.handle.set_display(d.clone());
            self.sent = Some(d);
        }
    }

    pub(crate) fn connected(&self) -> bool {
        self.handle.status().connected
    }
}

fn key(cfg: &mut Tm2Settings, a: KeyAction, amp: &AmpStatus, out: &mut Tm2Out) {
    let radio = |k| Some(k);
    let r = match a {
        KeyAction::Nothing => None,
        KeyAction::BandDown => radio(RadioKey::Band(-1)),
        KeyAction::BandUp => radio(RadioKey::Band(1)),
        KeyAction::ModeDown => radio(RadioKey::Mode(-1)),
        KeyAction::ModeUp => radio(RadioKey::Mode(1)),
        KeyAction::StepDown => radio(RadioKey::Step(-1)),
        KeyAction::StepUp => radio(RadioKey::Step(1)),
        KeyAction::SwapVfo => radio(RadioKey::SwapVfo),
        KeyAction::AEqualsB => radio(RadioKey::AEqualsB),
        KeyAction::Split => radio(RadioKey::Split),
        KeyAction::Rit => radio(RadioKey::Rit),
        KeyAction::RitClear => radio(RadioKey::RitClear),
        KeyAction::Lock => radio(RadioKey::Lock),
        KeyAction::TxReadout => {
            cfg.tx_readout = next_readout(cfg.tx_readout);
            None
        }
        KeyAction::AmpPtt | KeyAction::AmpTune | KeyAction::AmpAtu | KeyAction::AmpAntenna => {
            let k = match a {
                KeyAction::AmpPtt => AmpKey::Ptt,
                KeyAction::AmpTune => AmpKey::Tune,
                KeyAction::AmpAtu => AmpKey::Atu,
                _ => AmpKey::Antenna,
            };
            match amp_key(amp, k) {
                Ok(act) => out.amp.push(act),
                Err(why) => out.note = Some(why),
            }
            None
        }
    };
    if let Some(r) = r {
        out.hw.keys.push(r);
    }
}

/// The TMate 2's mode icon for the DUO's mode; DIG for a data mode chosen
/// in the radio window.
fn mode_icon(mode: &str, digi: bool) -> Option<ModeIcon> {
    Some(match mode {
        _ if digi => ModeIcon::Dig,
        "LSB" => ModeIcon::Lsb,
        "USB" => ModeIcon::Usb,
        "CW" | "CW-R" | "CWR" => ModeIcon::Cw,
        "AM" => ModeIcon::Am,
        "FM" => ModeIcon::Fm,
        _ => return None,
    })
}

/// The underlined digit for a tuning step: 1 Hz is digit 1, 1 kHz digit 4.
fn step_digit(step_hz: u64) -> Option<u8> {
    (step_hz > 0).then(|| step_hz.ilog10() as u8 + 1)
}

/// What the LCD shows: the radio's dial (as on its own LCD, transverter
/// offset included), mode and S-meter in receive; while the station
/// transmits, the amplifier's power, as the AMP panel reads it (the radio's
/// own when the amplifier is in bypass).
pub(crate) fn display(
    rig: Option<&RigStatus>,
    amp: &AmpStatus,
    transmitting: bool,
    view: &HardwareView,
    cfg: &Tm2Settings,
    rx_dbm: Option<f32>,
) -> Display {
    let mut d = Display {
        tx: transmitting,
        tx_readout: cfg.tx_readout,
        lock_led: view.locked,
        ..Display::default()
    };
    if let Some(s) = rig.filter(|s| s.connected) {
        let p = s.model.protocol();
        d.freq_hz = logic::dial_hz(s).map(|h| logic::shown(h, view.xvtr_hz));
        d.mode = s.mode.and_then(|m| mode_icon(m, view.digi));
        d.vfo_b = logic::rx_vfo(s) == rigcat::Vfo::B;
        d.nb = s.value("NB") == Some("1");
        d.nr = s.value("NR") == Some("1");
        d.rit = s.rit_on == Some(true);
        d.signal_dbm = logic::s_dbm(&*p, s).or(rx_dbm).map(|v| v.round() as i32);
        d.step_digit = step_digit(view.step_hz);
    }
    if transmitting && let Some(r) = amp.readings.as_ref() {
        let full = tmate2::protocol::power_scale_w(amp.model.limits().fwd_w);
        d.tx_mw = Some((r.fwd_w.max(0.0) * 1000.0).round() as u32);
        d.tx_rfl_mw = Some((r.rfl_w.max(0.0) * 1000.0).round() as u32);
        d.tx_swr_x10 = (r.swr >= 1.0).then(|| (r.swr * 10.0).round() as u32);
        d.tx_po_pct = Some(((r.fwd_w / full).clamp(0.0, 1.0) * 100.0).round() as u8);
    }
    if let Some((v, at)) = view.e1
        && at.elapsed() < SHOW_SETTING
    {
        d.small = Some(v.max(0) as u32);
    }
    d.look = cfg.backlights.pick(transmitting, d.mode);
    d
}

/// Whether the LCD needs drawing again soon without anything else asking:
/// a turned setting is about to drop off the small display.
pub(crate) fn repaint_for(view: &HardwareView) -> Option<Duration> {
    view.e1.and_then(|(_, at)| SHOW_SETTING.checked_sub(at.elapsed()))
}

/// SETTINGS → ELAD TMate 2.
pub(crate) fn settings_ui(ui: &mut egui::Ui, cfg: &mut Tm2Settings, tm2: Option<&Tm2>) {
    ui.label(RichText::new("ELAD TMate 2").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "The TMate 2 on USB as a second face for the RADIO window: its main dial, E1 and E2 \
             are the window's knobs (push for the step or the next setting, hold for digit by \
             digit or the lock), and its LCD shows the DUO's dial, mode and S-meter, and the \
             amplifier's power while transmitting. Close any other program using it first.",
        )
        .weak(),
    );
    ui.add_space(8.0);
    chrome::checkbox(ui, &mut cfg.enabled, "Enable");
    if let Some(t) = tm2 {
        let s = t.status();
        let (text, col) = if s.connected {
            (format!("Connected: {}", s.device), theme::CYAN())
        } else if let Some(e) = s.error {
            (e, theme::YELLOW())
        } else {
            ("Not connected.".to_string(), theme::YELLOW())
        };
        if cfg.enabled {
            ui.label(RichText::new(text).color(col));
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Reverse");
        chrome::checkbox(ui, &mut cfg.reverse[0], "Main dial");
        chrome::checkbox(ui, &mut cfg.reverse[1], "E1");
        chrome::checkbox(ui, &mut cfg.reverse[2], "E2");
    });
    ui.add_space(6.0);
    egui::Grid::new("tm2-keys").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (i, k) in cfg.keys.iter_mut().enumerate() {
            ui.label(format!("F{}", i + 1));
            crate::settings::enum_combo(
                ui,
                &format!("tm2-f{i}"),
                k,
                &KeyAction::ALL,
                KeyAction::label,
            );
            ui.end_row();
        }
        ui.label("While transmitting");
        crate::settings::enum_combo(
            ui,
            "tm2-readout",
            &mut cfg.tx_readout,
            &READOUTS,
            readout_label,
        );
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Backlight").strong());
    let b = &mut cfg.backlights;
    egui::Grid::new("tm2-colours").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        colour_row(ui, "Receive", &mut b.rx);
        colour_row(ui, "Transmit", &mut b.tx);
        ui.label("");
        chrome::checkbox(ui, &mut b.by_mode, "Receive colour by mode");
        ui.end_row();
        if b.by_mode {
            colour_row(ui, "SSB", &mut b.ssb);
            colour_row(ui, "CW", &mut b.cw);
            colour_row(ui, "AM and FM", &mut b.am_fm);
            colour_row(ui, "Data modes", &mut b.data);
        }
        ui.label("Contrast");
        ui.add(egui::Slider::new(&mut b.contrast, 0..=63));
        ui.end_row();
    });
    if chrome::chip(ui, false, "DEFAULT COLOURS").clicked() {
        *b = Backlights { by_mode: b.by_mode, ..Backlights::default() };
    }
}

fn colour_row(ui: &mut egui::Ui, name: &str, c: &mut Rgb) {
    ui.label(name);
    let mut rgb = [c.0, c.1, c.2];
    if ui.color_edit_button_srgb(&mut rgb).changed() {
        *c = Rgb(rgb[0], rgb[1], rgb[2]);
    }
    ui.end_row();
}

/// The radio's hold time, for a push held on the controller.
pub(crate) fn hold_s(rig: Option<&RigStatus>) -> f64 {
    rig.map_or(hardware::HOLD_DEFAULT, hardware::hold_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_digits_match_the_lcd() {
        assert_eq!(step_digit(1), Some(1));
        assert_eq!(step_digit(100), Some(3));
        assert_eq!(step_digit(1_000), Some(4));
        assert_eq!(step_digit(1_000_000), Some(7));
    }

    #[test]
    fn readout_cycles_through_all_three() {
        let mut r = TxReadout::Power;
        for _ in 0..3 {
            r = next_readout(r);
        }
        assert_eq!(r, TxReadout::Power);
        assert_eq!(next_readout(TxReadout::Power), TxReadout::Reflected);
    }

    #[test]
    fn modes_map_to_icons() {
        assert_eq!(mode_icon("CW-R", false), Some(ModeIcon::Cw));
        assert_eq!(mode_icon("USB", true), Some(ModeIcon::Dig));
        assert_eq!(mode_icon("LSB", false), Some(ModeIcon::Lsb));
    }
}
