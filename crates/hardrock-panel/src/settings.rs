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

/// Read the sound outputs again when the list `id` has just opened (it did
/// not show last pass), so a speaker or cable plugged in after start-up is
/// in it. Called from inside the open list.
fn reread_sounds_on_open(ui: &egui::Ui, id: &str, sounds: &mut Vec<String>) {
    let key = egui::Id::new(("sound-list-shown", id));
    let now = ui.ctx().cumulative_pass_nr();
    let last = ui.ctx().data_mut(|d| {
        let last = d.get_temp::<u64>(key);
        d.insert_temp(key, now);
        last
    });
    if just_opened(last, now) {
        *sounds = crate::audio::output_devices();
    }
}

/// Whether a list last shown on pass `last` and showing on pass `now` has
/// just opened.
fn just_opened(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|l| now > l + 1)
}

pub(crate) fn settings_radio_tab(
    ui: &mut egui::Ui,
    cfg: &mut RigConfig,
    amp_port: &str,
    status: Option<&RigStatus>,
    serial_ports: &[String],
    sounds: &mut Vec<String>,
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
                reread_sounds_on_open(ui, "rig-sound", sounds);
                if ui.selectable_label(audio_out.is_none(), "The radio's own USB audio").clicked() {
                    *audio_out = None;
                }
                for d in sounds.iter() {
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
    sounds: &mut Vec<String>,
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
                reread_sounds_on_open(ui, "rx-sound", sounds);
                if ui.selectable_label(audio_out.is_none(), "The system's default output").clicked()
                {
                    *audio_out = None;
                }
                for d in sounds.iter() {
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
        (None, true) if rx.opening() => ("Opening…".to_string(), theme::CYAN()),
        (None, true) => (rx.label.clone(), theme::GREEN()),
        (None, false) => ("Off".to_string(), theme::TEXT()),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label("Status");
        ui.label(RichText::new(text).color(col));
    });
    look
}

/// SETTINGS → Receiver's DSP (decisions 38, 39): the decimator, the passband's
/// shape with the delay each costs, the listening AGC and the decoders' AGC
/// target. Taken at once, and kept.
pub(crate) fn settings_dsp(ui: &mut egui::Ui, d: &mut crate::rx::DspSettings) {
    use crate::rx::{Decimator, DspSettings, Shape};
    let caption = |ui: &mut egui::Ui, t: &str| {
        ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong())
    };
    ui.add_space(6.0);
    ui.label(RichText::new("Receiver DSP").size(13.0).strong().color(theme::CYAN()));
    ui.horizontal_wrapped(|ui| {
        caption(ui, "Decimator");
        for k in Decimator::ALL {
            let hover = match k {
                Decimator::CicFir => {
                    "CIC, compensating FIR and a Kaiser FIR: what folds in from far away is \
                     11 to 15 dB further down. The default."
                }
                Decimator::HalfBand => "A ladder of half-band filters and a Kaiser FIR",
            };
            if chrome::chip(ui, d.decimator == k, k.kind().label()).on_hover_text(hover).clicked() {
                d.decimator = k;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "Passband");
        for k in Shape::ALL {
            let text = format!("{} · {} ms", k.kind().label(), k.delay_ms());
            let hover = format!(
                "95 dB down {:.0} Hz outside the passband; {} ms of audio delay",
                k.kind().transition_hz(),
                k.delay_ms()
            );
            if chrome::chip(ui, d.shape == k, text).on_hover_text(hover).clicked() {
                d.shape = k;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "AGC");
        let field = |ui: &mut egui::Ui, v: &mut f32, r: std::ops::RangeInclusive<f32>, unit| {
            chrome::field(ui, egui::DragValue::new(v).range(r).speed(1.0).suffix(unit))
        };
        ui.label("attack");
        field(ui, &mut d.agc_attack_ms, 0.1..=500.0, " ms").on_hover_text(
            "Lookahead, and how long the gain takes to come down before a loud signal arrives",
        );
        ui.label("decay");
        field(ui, &mut d.agc_decay_ms, 1.0..=60_000.0, " ms")
            .on_hover_text("How fast the gain comes back up after the hang");
        ui.label("hang");
        field(ui, &mut d.agc_hang_ms, 0.0..=60_000.0, " ms")
            .on_hover_text("How long the gain is held after it last fell");
        ui.label("target");
        field(ui, &mut d.agc_target_dbfs, -60.0..=0.0, " dBFS")
            .on_hover_text("The peak level the receiver's audio is brought to");
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "Decoder AGC");
        ui.label("target");
        chrome::field(
            ui,
            egui::DragValue::new(&mut d.decoder_target_dbfs)
                .range(-60.0..=0.0)
                .speed(0.5)
                .suffix(" dBFS"),
        )
        .on_hover_text(
            "The decoders' own feed, whatever the volume and the AGC above: slow (5 ms attack, \
             2 s hang and decay) so a strong station does not make the weak ones pump.",
        );
        if chrome::chip(ui, false, "DEFAULTS").on_hover_text("Decision 38's settings").clicked() {
            *d = DspSettings::default();
        }
    });
}

#[cfg(test)]
mod sound_list_tests {
    use super::just_opened;

    #[test]
    fn the_sound_list_is_read_again_each_time_it_opens() {
        // First shown: read.
        assert!(just_opened(None, 10));
        // Still open on the next pass: not read again.
        assert!(!just_opened(Some(10), 11));
        assert!(!just_opened(Some(11), 11));
        // Closed for a while and opened again: read.
        assert!(just_opened(Some(11), 40));
    }
}

/// SETTINGS → Station: the operator's callsign and grid, for DECODE's DIST and
/// AZ, the hashed callsigns and SPOTS, and how distances are shown. Taken at
/// once, and kept.
pub(crate) fn settings_station(ui: &mut egui::Ui, st: &mut crate::station::StationSettings) {
    use crate::station::{DistUnit, GridCheck, grid_check};
    ui.label(RichText::new("Station").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Your callsign and Maidenhead grid (4 or 6 characters, FN31 or FN31pr). DECODE \
             measures each station's distance and bearing from your grid, and SPOTS reports \
             what you hear under your callsign. Taken at once. Nothing is sent anywhere \
             unless you tick it under DECODE's SPOTS.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("station-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("My Callsign");
        ui.horizontal(|ui| {
            let r = chrome::field(
                ui,
                egui::TextEdit::singleline(&mut st.callsign).desired_width(120.0).char_limit(13),
            );
            if r.changed() {
                st.callsign = st.callsign.to_ascii_uppercase();
            }
            if !st.callsign.trim().is_empty() && st.call().is_none() {
                ui.label(RichText::new("Not a callsign").color(theme::YELLOW()));
            }
        });
        ui.end_row();

        ui.label("My Grid");
        ui.horizontal(|ui| {
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut st.grid).desired_width(120.0).char_limit(6),
            );
            let (text, col) = match grid_check(&st.grid) {
                GridCheck::Empty => (String::new(), theme::TEXT()),
                GridCheck::Partial => ("4 or 6 characters".to_string(), theme::TEXT()),
                GridCheck::Valid => {
                    let p = st.pos().expect("a valid grid has a place");
                    (
                        format!(
                            "{:.3}° {}, {:.3}° {}",
                            p.lat.abs(),
                            if p.lat < 0.0 { "S" } else { "N" },
                            p.lon.abs(),
                            if p.lon < 0.0 { "W" } else { "E" }
                        ),
                        theme::CYAN(),
                    )
                }
                GridCheck::Invalid => (
                    "Not a grid: two letters A–R, two digits, then two letters A–X".to_string(),
                    theme::YELLOW(),
                ),
            };
            if !text.is_empty() {
                ui.label(RichText::new(text).color(col));
            }
        });
        ui.end_row();

        ui.label("Distances in");
        ui.horizontal(|ui| {
            for u in DistUnit::ALL {
                if chrome::chip(ui, st.dist_unit == u, u.label()).clicked() {
                    st.dist_unit = u;
                }
            }
        });
        ui.end_row();
    });
}
