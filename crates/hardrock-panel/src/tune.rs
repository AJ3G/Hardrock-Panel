//! RIG TUNE: the radio's own carrier, keyed from the panel at the power set on
//! the slider beside it (the FDM-DUO's menu 34), to watch what the amplifier
//! does. The checks here are safety interlock #6, with the link's own time
//! limit, watchdog and SWR stop behind them.

use std::time::Instant;

use eframe::egui::{self, RichText};
use hardrock::{AmpStatus, Band};
use rigcat::{PowerStep, RigStatus};

use crate::{chrome, theme};

/// The shortest and longest TUNE the panel offers, in seconds. The link
/// refuses more than a minute whatever it is asked.
pub(crate) const TUNE_SECS: std::ops::RangeInclusive<u32> = 1..=60;
pub(crate) const DEFAULT_TUNE_SECS: u32 = 10;

/// Why TUNE may not be keyed now, or `None` if it may. `step` is the power
/// it would key at.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): the radio's TUNE carrier
/// goes through the amplifier, so it is keyed only when the amplifier's
/// state is known and safe for it: connected, not tripped and, in line
/// (PTT), on the radio's band (a carrier into the wrong low-pass filter
/// meets a mismatch at full gain) and with the carrier within its drive
/// limit (as #4: 8 W on the HR500).
pub(crate) fn tune_blocked(
    amp: &AmpStatus,
    rig: &RigStatus,
    step: Option<&PowerStep>,
) -> Option<String> {
    if !rig.connected {
        return Some("The radio isn't connected.".into());
    }
    let Some(r) = amp.readings.filter(|_| amp.connected) else {
        return Some("The amplifier isn't connected, so its state can't be checked.".into());
    };
    if amp.alerts.tripped {
        return Some("The amplifier has tripped. Clear it first.".into());
    }
    if !r.ptt {
        return None;
    }
    let Some(step) = step else {
        return Some("The radio's power setting hasn't been read yet.".into());
    };
    let limit = amp.model.limits().drive_warn_w;
    if step.watts > limit {
        return Some(format!(
            "{} is over the amplifier's {limit:.0} W drive limit while it is in line (PTT). \
             Lower the power, or put the amplifier in OFF.",
            step.label
        ));
    }
    let radio = rig.freq_hz.and_then(crate::band_of);
    let amp_band = r.band.and_then(Band::from_code);
    match (radio, amp_band) {
        (Some(a), Some(b)) if a == b => None,
        (None, _) => Some(
            "The radio isn't on a band the amplifier covers, so the amplifier's filter can't \
             be checked."
                .into(),
        ),
        (Some(a), b) => Some(format!(
            "The radio is on {a} and the amplifier on {}: the carrier would meet the wrong \
             low-pass filter. Change band first.",
            b.map_or("no band".to_string(), |b| b.to_string())
        )),
    }
}

/// Why a TUNE in progress must stop now, or `None`. Everything that would
/// have refused it, and also: drive past the limit in line, or SWR at or past
/// the warning on the amplifier (in line) or the radio.
pub(crate) fn tune_must_stop(amp: &AmpStatus, rig: &RigStatus, swr_warn: f32) -> Option<String> {
    let blocked = tune_blocked(amp, rig, rig.power());
    if blocked.is_some() {
        return blocked;
    }
    let r = amp.readings?;
    let limit = amp.model.limits().drive_warn_w;
    if r.ptt && r.drive_w > limit {
        return Some(format!("Drive {:.1} W went over the {limit:.0} W limit.", r.drive_w));
    }
    if r.ptt && r.swr >= swr_warn {
        return Some(format!("The amplifier's SWR reached {:.1}:1.", r.swr));
    }
    if let Some(s) = rig.swr_ratio().filter(|s| *s >= swr_warn) {
        return Some(format!("The radio's SWR reached {s:.1}:1."));
    }
    None
}

/// What the operator did on the TUNE strip.
#[derive(Default)]
pub(crate) struct TuneOut {
    pub start: bool,
    pub stop: bool,
    /// A new power step, by its code.
    pub power: Option<u8>,
}

/// The TUNE button, the power slider (the radio's own setting: menu 34 on
/// the FDM-DUO) and how long TUNE may run. `id` keeps two copies apart.
pub(crate) fn tune_strip(
    ui: &mut egui::Ui,
    id: &str,
    rig: &RigStatus,
    blocked: Option<&str>,
    limit_in_line: Option<f32>,
    secs: &mut u32,
) -> TuneOut {
    let mut out = TuneOut::default();
    let steps = rig.power_steps;
    ui.horizontal_wrapped(|ui| {
        let tuning = rig.tuning;
        let can = tuning.is_some() || blocked.is_none();
        let resp =
            ui.add_enabled_ui(can, |ui| chrome::chip(ui, tuning.is_some(), "RIG TUNE")).inner;
        let resp = match blocked {
            Some(why) => resp.on_disabled_hover_text(why),
            None => resp.on_hover_text(
                "Key the radio's own TUNE carrier at the power set beside it, to watch the \
                 amplifier. Click again to unkey. It also unkeys by itself at the time limit, \
                 and on a trip or a high SWR.",
            ),
        };
        if resp.clicked() {
            if tuning.is_some() {
                out.stop = true;
            } else {
                out.start = true;
            }
        }

        ui.add_space(6.0);
        ui.label(RichText::new(rig.power_name).size(11.0));
        if steps.is_empty() {
            return;
        }
        let key = egui::Id::new(("tune-power", id));
        let current = rig.power().and_then(|p| steps.iter().position(|s| s.code == p.code));
        let held: Option<usize> = ui.data(|d| d.get_temp(key));
        let mut idx = held.or(current).unwrap_or(0);
        let enabled = rig.connected && current.is_some();
        let s = ui
            .add_enabled(
                enabled,
                egui::Slider::new(&mut idx, 0..=steps.len() - 1).show_value(false).step_by(1.0),
            )
            .on_hover_text(
                "The radio's own power setting, written to the radio when you let go. It stays \
                 set after the panel closes, as on the radio's menu.",
            );
        let label = steps.get(idx).map_or("—", |p| p.label);
        ui.label(RichText::new(label).monospace());
        if s.dragged() {
            ui.data_mut(|d| d.insert_temp(key, idx));
        } else if s.drag_stopped() || s.changed() {
            ui.data_mut(|d| d.remove::<usize>(key));
            if Some(idx) != current
                && let Some(p) = steps.get(idx)
            {
                // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): not past
                // the amplifier's drive limit while it is in line.
                match limit_in_line {
                    Some(l) if p.watts > l => {}
                    _ => out.power = Some(p.code),
                }
            }
        } else {
            ui.data_mut(|d| d.remove::<usize>(key));
        }

        ui.add_space(6.0);
        ui.label(RichText::new("for").size(11.0));
        ui.add(egui::DragValue::new(secs).range(TUNE_SECS).suffix(" s"))
            .on_hover_text("TUNE unkeys by itself after this long.");

        if let Some(t) = tuning {
            let left = t.until.saturating_duration_since(Instant::now()).as_secs_f32();
            ui.label(
                RichText::new(format!("keyed · {left:.0} s left")).size(11.0).color(theme::CYAN()),
            );
        } else if let Some(n) = &rig.tune_note {
            // Yellow only for a stop that is a warning; the time limit is not.
            let col = if n.starts_with("time limit") { theme::TEXT() } else { theme::YELLOW() };
            ui.label(RichText::new(format!("Stopped: {n}")).size(11.0).color(col));
        }
    });
    if let Some(l) = limit_in_line
        && rig.power().is_some_and(|p| p.watts > l)
    {
        ui.label(
            RichText::new(format!(
                "The radio's power is over the amplifier's {l:.0} W drive limit for PTT."
            ))
            .size(11.0)
            .color(theme::YELLOW()),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardrock::{AmpAlerts, AmpModel, AmpReadings};
    use rigcat::elad;

    fn amp(ptt: bool, band: u8) -> AmpStatus {
        AmpStatus {
            configured: true,
            connected: true,
            model: AmpModel::default(),
            readings: Some(AmpReadings { ptt, band: Some(band), ..AmpReadings::default() }),
            ..AmpStatus::default()
        }
    }

    fn duo(hz: u64, code: u8) -> RigStatus {
        RigStatus {
            connected: true,
            freq_hz: Some(hz),
            power_code: Some(code),
            power_steps: &elad::POWER_STEPS,
            ..RigStatus::default()
        }
    }

    fn step(code: u8) -> Option<&'static PowerStep> {
        elad::POWER_STEPS.iter().find(|p| p.code == code)
    }

    #[test]
    fn in_bypass_tune_needs_only_both_links() {
        assert_eq!(tune_blocked(&amp(false, 7), &duo(14_074_000, 9), step(9)), None);
        let off = RigStatus { connected: false, ..duo(14_074_000, 2) };
        assert!(tune_blocked(&amp(false, 5), &off, step(2)).is_some());
        let no_amp = AmpStatus { connected: false, ..amp(false, 5) };
        assert!(tune_blocked(&no_amp, &duo(14_074_000, 2), step(2)).is_some());
    }

    #[test]
    fn in_line_the_bands_must_match() {
        // 20 m is HRBN 5, 40 m is 7.
        assert_eq!(tune_blocked(&amp(true, 5), &duo(14_074_000, 2), step(2)), None);
        let why = tune_blocked(&amp(true, 7), &duo(14_074_000, 2), step(2)).unwrap();
        assert!(why.contains("wrong low-pass filter"), "{why}");
        assert!(tune_blocked(&amp(true, 5), &duo(50_313_000, 2), step(2)).is_some());
    }

    #[test]
    fn in_line_the_carrier_stays_within_the_drive_limit() {
        // MAX is taken as 8.3 W, over the HR500's 8 W; 5 W is within it.
        assert!(tune_blocked(&amp(true, 5), &duo(14_074_000, 9), step(9)).is_some());
        assert_eq!(tune_blocked(&amp(true, 5), &duo(14_074_000, 8), step(8)), None);
    }

    #[test]
    fn a_trip_blocks_and_stops_tune() {
        let mut a = amp(false, 5);
        a.alerts = AmpAlerts { tripped: true, ..AmpAlerts::default() };
        assert!(tune_blocked(&a, &duo(14_074_000, 2), step(2)).is_some());
        assert!(tune_must_stop(&a, &duo(14_074_000, 2), 5.0).is_some());
    }

    #[test]
    fn a_high_swr_stops_tune() {
        let mut a = amp(true, 5);
        assert_eq!(tune_must_stop(&a, &duo(14_074_000, 2), 3.0), None);
        a.readings.as_mut().unwrap().swr = 3.2;
        assert!(tune_must_stop(&a, &duo(14_074_000, 2), 3.0).unwrap().contains("SWR"));
        let mut r = duo(14_074_000, 2);
        r.swr = elad::parse_swr("0 04.00");
        assert!(tune_must_stop(&amp(false, 5), &r, 3.0).unwrap().contains("radio's SWR"));
    }

    #[test]
    fn drive_past_the_limit_in_line_stops_tune() {
        let mut a = amp(true, 5);
        a.readings.as_mut().unwrap().drive_w = 9.0;
        assert!(tune_must_stop(&a, &duo(14_074_000, 2), 5.0).unwrap().contains("Drive"));
    }
}
