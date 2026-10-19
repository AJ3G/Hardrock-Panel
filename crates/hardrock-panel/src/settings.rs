//! The settings window: which amplifier, on which port, and where the SWR
//! warning starts. The live panel is the main window; this only sets the link
//! up.

use eframe::egui::{self, ComboBox, RichText};
use hardrock::{AMP_BAUD_RATES, AmpConfig, AmpModel, AmpStatus};

use crate::chrome::StyledCombo;
use crate::{chrome, theme};

pub(crate) fn settings_amp_tab(
    ui: &mut egui::Ui,
    cfg: &mut AmpConfig,
    swr_warn: &mut f32,
    status: &AmpStatus,
    serial_ports: &[String],
    apply: &mut bool,
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
    ui.add_space(6.0);
    if chrome::chip_accent(
        ui,
        false,
        RichText::new(" APPLY ").strong(),
        theme::GREEN(),
        theme::INK_ON_CYAN(),
    )
    .on_hover_text("Save and (re)open the amplifier's port")
    .clicked()
    {
        *apply = true;
    }
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
