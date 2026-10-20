//! The settings window: which amplifier and which radio, on which ports, and
//! where the SWR warning starts. The live panel is the main window; this only sets the link
//! up.

use eframe::egui::{self, ComboBox, RichText};
use hardrock::{AMP_BAUD_RATES, AmpConfig, AmpModel, AmpStatus};
use rigcat::{RigConfig, RigModel, RigStatus};

use crate::chrome::StyledCombo;
use crate::{chrome, theme};

pub(crate) fn settings_amp_tab(
    ui: &mut egui::Ui,
    cfg: &mut AmpConfig,
    swr_warn: &mut f32,
    status: &AmpStatus,
    serial_ports: &[String],
) {
    ui.label(RichText::new("Amplifier").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "A HobbyPCB Hardrock-500 or Hardrock-50 on its USB serial port. The panel reads its \
             meters, switches its band, mode and antenna, and runs its tuner. The amplifier is \
             still keyed by the radio's PTT line as before. Only one program can have the port \
             open at a time, so close SDRoxide's AMP link or HobbyPCB's own panel first.",
        )
        .weak(),
    );
    ui.add_space(8.0);

    chrome::checkbox(ui, &mut cfg.enabled, "Enable");
    ui.add_space(4.0);
    egui::Grid::new("amp-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Model");
        enum_combo(ui, "amp-model", &mut cfg.model, &AmpModel::ALL, AmpModel::label);
        ui.end_row();
        if !cfg.model.tested() {
            ui.label("");
            ui.label(
                RichText::new(
                    "Untested: this panel has only been tested with a Hardrock-500. \
                     Meter limits for this model are scaled from the Hardrock-500's \
                     and may be wrong. Watch the amplifier's own display.",
                )
                .color(theme::YELLOW()),
            );
            ui.end_row();
        }

        ui.label("Port");
        let shown = if cfg.serial.path.is_empty() {
            "— choose —".to_string()
        } else {
            cfg.serial.path.clone()
        };
        ComboBox::from_id_salt("amp-port").width(200.0).selected_text(shown).show_styled(
            ui,
            |ui| {
                for p in serial_ports {
                    if ui.selectable_label(&cfg.serial.path == p, p).clicked() {
                        cfg.serial.path = p.clone();
                    }
                }
            },
        );
        ui.end_row();

        ui.label("Baud");
        ComboBox::from_id_salt("amp-baud")
            .width(200.0)
            .selected_text(cfg.serial.baud.to_string())
            .show_styled(ui, |ui| {
                for b in AMP_BAUD_RATES {
                    if ui.selectable_label(cfg.serial.baud == b, b.to_string()).clicked() {
                        cfg.serial.baud = b;
                    }
                }
            })
            .response
            .on_hover_text(
                "Must match the amplifier's menu. HobbyPCB's own control panel needs 115200.",
            );
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("SWR warning at");
        ui.add(
            egui::DragValue::new(swr_warn)
                .range(1.5..=10.0)
                .speed(0.05)
                .fixed_decimals(1)
                .suffix(":1"),
        )
        .on_hover_text(
            "At or past this SWR the SWR reading turns red and the meter warns that FWD and RFL \
             can't be trusted. SDRoxide uses its transmitter's tune limit, 5:1 by default.",
        );
    });
    ui.add_space(8.0);

    let (text, col) = if !status.configured {
        ("Off".to_string(), theme::TEXT())
    } else if status.connected {
        (format!("Connected to the {}", status.model.label()), theme::GREEN())
    } else if let Some(e) = &status.error {
        (e.clone(), theme::ALERT())
    } else {
        ("Connecting…".to_string(), theme::YELLOW())
    };
    ui.horizontal(|ui| {
        ui.label("Status");
        ui.label(RichText::new(text).color(col));
    });
}

/// The APPLY button under both sections: saves both and reopens both ports.
pub(crate) fn apply_button(ui: &mut egui::Ui) -> bool {
    chrome::chip_accent(
        ui,
        false,
        RichText::new(" APPLY ").strong(),
        theme::GREEN(),
        theme::INK_ON_CYAN(),
    )
    .on_hover_text("Save, and (re)open the amplifier's and the radio's ports")
    .clicked()
}

pub(crate) fn settings_radio_tab(
    ui: &mut egui::Ui,
    cfg: &mut RigConfig,
    amp_port: &str,
    status: Option<&RigStatus>,
    serial_ports: &[String],
    sounds: &[String],
    audio_out: &mut Option<String>,
) {
    ui.label(RichText::new("Radio").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "The radio's CAT port. The panel reads the radio's own forward and reflected power \
             and SWR, which fill FWD, RFL and SWR while the amplifier is in OFF (bypass), its \
             dial, which can set the amplifier's band, and its menu settings, under RADIO. It \
             keys the radio only for TX TUNE and RIG TUNE, and changes only menu 34 (the power \
             buttons) and menu 32 (USB AUDIO), when you press them. Only one program can have \
             the port open, so close SDRoxide, FDM-SW2 or a logger using it first.",
        )
        .weak(),
    );
    ui.add_space(8.0);
    chrome::checkbox(ui, &mut cfg.enabled, "Enable");
    ui.add_space(4.0);
    egui::Grid::new("rig-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Model");
        enum_combo(ui, "rig-model", &mut cfg.model, &RigModel::ALL, RigModel::label);
        ui.end_row();

        ui.label("Port");
        let shown =
            if cfg.path.is_empty() { "— choose —".to_string() } else { cfg.path.clone() };
        ComboBox::from_id_salt("rig-port").width(200.0).selected_text(shown).show_styled(
            ui,
            |ui| {
                for p in serial_ports {
                    if ui.selectable_label(&cfg.path == p, p).clicked() {
                        cfg.path = p.clone();
                    }
                }
            },
        );
        ui.end_row();
        if !cfg.path.is_empty() && cfg.path.eq_ignore_ascii_case(amp_port) {
            ui.label("");
            ui.label(
                RichText::new("This is the amplifier's port. Choose the radio's own CAT port.")
                    .color(theme::ALERT()),
            );
            ui.end_row();
        }

        ui.label("Baud");
        ComboBox::from_id_salt("rig-baud")
            .width(200.0)
            .selected_text(format!("{} 8N1", cfg.baud))
            .show_styled(ui, |ui| {
                for &b in cfg.model.bauds() {
                    if ui.selectable_label(cfg.baud == b, format!("{b} 8N1")).clicked() {
                        cfg.baud = b;
                    }
                }
            })
            .response
            .on_hover_text("Must match menu 70 CAT BAUD on the radio. The FDM-DUO ships at 38400.");
        ui.end_row();

        ui.label("TUNE sound");
        let auto = crate::audio::radio_device(sounds);
        let shown = match (audio_out.as_ref(), &auto) {
            (Some(d), _) => d.clone(),
            (None, Some(a)) => format!("The radio's: {a}"),
            (None, None) => "— none found —".to_string(),
        };
        ComboBox::from_id_salt("rig-sound")
            .width(260.0)
            .selected_text(shown)
            .show_styled(ui, |ui| {
                if ui.selectable_label(audio_out.is_none(), "The radio's own USB audio").clicked() {
                    *audio_out = None;
                }
                for d in sounds {
                    if ui.selectable_label(audio_out.as_ref() == Some(d), d).clicked() {
                        *audio_out = Some(d.clone());
                    }
                }
            })
            .response
            .on_hover_text(
                "Where TUNE plays its tone: the radio's USB audio (FDM-DUO Audio), as for a \
                 digital-mode program. Taken at once, without APPLY.",
            );
        ui.end_row();
    });
    ui.add_space(4.0);
    chrome::checkbox(ui, &mut cfg.follow_band, "Amplifier follows the radio's band").on_hover_text(
        "Sends the radio's frequency to the amplifier when it moves to another band, \
             as SDRoxide does. Held until the amplifier shows no RF, so a band relay never \
             switches mid-transmission. Leave it off if the amplifier already gets the band \
             over its own CAT or ACC cable.",
    );
    ui.add_space(8.0);
    let (text, col) = match status {
        None => ("Off".to_string(), theme::TEXT()),
        Some(s) if s.connected => (format!("Connected to the {}", s.model.label()), theme::GREEN()),
        Some(s) => match &s.error {
            Some(e) => (e.clone(), theme::ALERT()),
            None => ("Connecting…".to_string(), theme::YELLOW()),
        },
    };
    ui.horizontal(|ui| {
        ui.label("Status");
        ui.label(RichText::new(text).color(col));
    });
}

fn enum_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    cur: &mut T,
    all: &[T],
    label: impl Fn(T) -> &'static str,
) {
    ComboBox::from_id_salt(id).selected_text(label(*cur)).show_styled(ui, |ui| {
        for &opt in all {
            if ui.selectable_label(*cur == opt, label(opt)).clicked() {
                *cur = opt;
            }
        }
    });
}

/// Where the panel's receiver gets its I/Q, and where it plays.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settings_receiver_tab(
    ui: &mut egui::Ui,
    source: &mut crate::rx::IqChoice,
    audio_out: &mut Option<String>,
    sounds: &[String],
    elad: &[elad_iq::EladDevice],
    rx: &crate::rx::Rx,
) -> bool {
    use crate::rx::IqChoice;
    let mut look = false;
    ui.label(RichText::new("Scope and receiver").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "The FDM-DUO's I/Q over its second USB lead, for SCOPE and the panel's own \
             receiver, which DECODE listens to. Demo signals shows a made-up band around \
             14.2 MHz, with no radio. The I/Q rate is the scope's SPAN key. The source is taken \
             with APPLY; the sound output at once.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("rx-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("I/Q from");
        enum_combo(ui, "rx-source", source, &IqChoice::ALL, IqChoice::label);
        ui.end_row();
        if *source == IqChoice::Elad {
            ui.label("Found");
            ui.horizontal(|ui| {
                let t = if elad.is_empty() {
                    "No ELAD on USB yet".to_string()
                } else {
                    elad.iter().map(|d| d.label()).collect::<Vec<_>>().join(", ")
                };
                ui.label(t);
                if chrome::chip(ui, false, "LOOK AGAIN").clicked() {
                    look = true;
                }
            });
            ui.end_row();
        }
        ui.label("Receiver sound");
        let shown = audio_out.clone().unwrap_or_else(|| "The system's default output".into());
        ComboBox::from_id_salt("rx-sound")
            .width(260.0)
            .selected_text(shown)
            .show_styled(ui, |ui| {
                if ui.selectable_label(audio_out.is_none(), "The system's default output").clicked()
                {
                    *audio_out = None;
                }
                for d in sounds {
                    if ui.selectable_label(audio_out.as_ref() == Some(d), d).clicked() {
                        *audio_out = Some(d.clone());
                    }
                }
            })
            .response
            .on_hover_text(
                "Where the panel's receiver plays. Choose VB-Cable's input (CABLE Input) to feed \
             an outside decoder.",
            );
        ui.end_row();
    });
    if *source == IqChoice::Elad && cfg!(windows) {
        ui.label(
            RichText::new(
                "On Windows the ELAD's I/Q device must be on the WinUSB driver (Zadig), which \
                 stops ELAD's own FDM-SW2 from seeing it until its driver is put back.",
            )
            .size(11.5)
            .weak(),
        );
    }
    let (text, col) = match (&rx.error, rx.active()) {
        (Some(e), _) => (e.clone(), theme::ALERT()),
        (None, true) if rx.opening() => ("Opening…".to_string(), theme::YELLOW()),
        (None, true) => (rx.label.clone(), theme::GREEN()),
        (None, false) => ("Off".to_string(), theme::TEXT()),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label("Status");
        ui.label(RichText::new(text).color(col));
    });
    look
}
