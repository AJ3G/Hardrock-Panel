//! TUNE, to watch what the amplifier does, laid out as SDRoxide's TX box: the
//! FDM-DUO's own power buttons (1W, 3W, 5W and MAX: menu 34), and the Tune
//! slider, which sets the power within the button's setting.
//!
//! **TUNE** keys an ordinary transmission (`TX;`) of a 1 kHz tone from the
//! panel through the radio's USB audio, at the slider's level. It is what
//! SDRoxide's TUNE did, and it keys the radio's PTT output to the amplifier
//! as its PTT does. **RIG TUNE** keys the radio's own carrier (`TX2;`) at the
//! full menu 34 setting, with no tone and no slider.
//!
//! The checks here are safety interlock #6, with the link's own time limit,
//! watchdog and SWR stop behind them, and #7 for TUNE's tone.

use std::time::Instant;

use eframe::egui::{self, RichText};
use hardrock::{AmpStatus, Band};
use rigcat::{Keying, RigStatus};

use crate::radio::z;
use crate::{chrome, theme};

/// The shortest and longest TUNE the panel offers, in seconds. The link
/// refuses more than a minute whatever it is asked.
pub(crate) const TUNE_SECS: std::ops::RangeInclusive<u32> = 1..=60;
pub(crate) const DEFAULT_TUNE_SECS: u32 = 10;
/// Where the Tune slider starts on a first run: a quarter of the setting.
pub(crate) const DEFAULT_DRIVE: f32 = 0.25;

/// The power buttons, as in SDRoxide (`duo_power::PRESETS` there): label and
/// menu 34 setting.
pub(crate) const PRESETS: [(&str, u8); 4] = [("1W", 2), ("3W", 6), ("5W", 8), ("MAX", 9)];
const PRESETS_CAPTION: &str = "ELAD FDM-DUO TX PWR";
/// How long a power key shows as clicked while the radio's read-back is
/// awaited, s.
const POWER_WAIT_S: f64 = 3.0;

/// The hover for the button that sets `code` (from SDRoxide's).
fn preset_hover(code: u8) -> String {
    let base = "Sets the FDM-DUO's own power setting (menu 34 TX POWER). The Tune slider then \
                sets TUNE's power within it. It stays set after the panel closes, as on the \
                radio's menu, and is the ceiling for every program that transmits.";
    match code {
        9 => format!(
            "MAX: the FDM-DUO's full output, about 8 W. The setting to use with the HR500 in \
             line, with the Tune slider to bring it down.\n\n{base}"
        ),
        8 => format!(
            "5 W. With the HR500 in line it will not reach full output on most bands at this \
             setting: use MAX, and the slider to bring it down.\n\n{base}"
        ),
        c => format!(
            "{}, for running barefoot or QRP.\n\n{base}",
            rigcat::elad::POWER_STEPS.get(c as usize).map_or("?", |p| p.label)
        ),
    }
}

/// The power TUNE would key at, as near as is known: the menu 34 setting for
/// the carrier; that times the Tune slider for the tone, which the slider
/// spans in a straight line (`rigcat::elad::AUDIO_CEILING`).
pub(crate) fn planned_watts(rig: &RigStatus, keying: Keying, drive: f32) -> Option<f32> {
    let step = rig.power()?;
    Some(match keying {
        Keying::Carrier => step.watts,
        Keying::Audio => step.watts * drive.clamp(0.0, 1.0),
    })
}

/// Why TUNE may not be keyed now, or `None` if it may. `watts` is the power
/// it would key at ([`planned_watts`]).
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): TUNE goes through the
/// amplifier, so it is keyed only when the amplifier's state is known and
/// safe for it: connected and, in line (PTT), on the radio's band (a carrier
/// into the wrong low-pass filter meets a mismatch at full gain) and within
/// its drive limit (as #4: 8 W on the HR500).
///
/// A trip the amplifier still shows does not refuse TUNE: the HR500 clears
/// it only the next time it is keyed (its manual: trip conditions "do not
/// need to be manually reset"), so refusing left no way to clear it (AJ3G,
/// 25 Oct 2026). To clear one, TUNE runs at no more than the drive limit
/// whether the amplifier is in line or not, and a new trip stops it
/// ([`tune_must_stop`]). Outside programs and stored CW messages are still
/// refused while it shows ([`key_blocked`]).
pub(crate) fn tune_blocked(amp: &AmpStatus, rig: &RigStatus, watts: Option<f32>) -> Option<String> {
    if !rig.connected {
        return Some("The radio isn't connected.".into());
    }
    let Some(r) = amp.readings.filter(|_| amp.connected) else {
        return Some("The amplifier isn't connected, so its state can't be checked.".into());
    };
    if amp.alerts.tripped {
        let limit = amp.model.limits().drive_warn_w;
        match watts {
            None => return Some("The radio's power setting hasn't been read yet.".into()),
            Some(w) if w > limit => {
                return Some(format!(
                    "The amplifier has tripped. TUNE clears it, at {limit:.0} W or less: lower \
                     the power or the Tune slider (about {w:.1} W now)."
                ));
            }
            Some(_) => {}
        }
    }
    if !r.ptt {
        return None;
    }
    let Some(watts) = watts else {
        return Some("The radio's power setting hasn't been read yet.".into());
    };
    let limit = amp.model.limits().drive_warn_w;
    if watts > limit {
        return Some(format!(
            "About {watts:.1} W is over the amplifier's {limit:.0} W drive limit while it is in \
             line (PTT). Lower the power, or put the amplifier in OFF."
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

/// Why an outside program (TCI, rigctld) may not key the radio now, or
/// `None` if it may.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #8 with #6): an outside
/// program's transmission goes through the amplifier as TUNE's does, so it
/// is judged by [`tune_blocked`] at the radio's full power setting (the
/// program's audio level can bring the power up to it, as for a stored CW
/// message), and refused while anything else keys the radio. For an ordinary
/// transmission the link also reads the radio's input and mode again just
/// before it keys (#7).
pub(crate) fn outside_ptt_blocked(amp: &AmpStatus, rig: &RigStatus) -> Option<String> {
    if amp.alerts.tripped {
        return Some("The amplifier has tripped. TUNE clears it.".into());
    }
    if rig.tuning.is_some() {
        return Some("TUNE is running.".into());
    }
    if rig.transmitting() {
        return Some("The radio is already transmitting.".into());
    }
    key_blocked(amp, rig, planned_watts(rig, Keying::Carrier, 1.0))
}

/// [`tune_blocked`] for anything but TUNE: a stored CW message or an outside
/// program. A trip the amplifier shows refuses it; TUNE is what clears one.
pub(crate) fn key_blocked(amp: &AmpStatus, rig: &RigStatus, watts: Option<f32>) -> Option<String> {
    if amp.alerts.tripped {
        return Some("The amplifier has tripped. TUNE clears it.".into());
    }
    tune_blocked(amp, rig, watts)
}

/// Why TUNE's tone may not be keyed, beyond [`tune_blocked`]: no sound
/// output for it, or the radio not set to send it.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #7): an ordinary transmission
/// sends whatever audio is on the radio's input, so it is keyed only with the
/// input on USB audio (not the microphone) and the radio on USB or LSB. The
/// link reads both again just before it keys.
pub(crate) fn tone_blocked(rig: &RigStatus, device: Option<&str>) -> Option<String> {
    if device.is_none() {
        return Some(
            "No sound output for the radio. Choose its USB audio under SETTINGS, Radio.".into(),
        );
    }
    match rig.tx_input.as_deref() {
        Some("USB audio") => {}
        Some(other) => {
            return Some(format!(
                "The radio's TX IN (menu 32) is on {other}, so TUNE would send that. USB AUDIO \
                 sets it to the radio's USB audio."
            ));
        }
        None => return Some("The radio's TX IN (menu 32) hasn't been read yet.".into()),
    }
    match rig.mode {
        Some("USB" | "LSB") => None,
        Some(m) => Some(format!("The radio is in {m}. TUNE's tone needs USB or LSB.")),
        None => Some("The radio's mode hasn't been read yet.".into()),
    }
}

/// Why a TUNE in progress must stop now, or `None`. Everything that would
/// have refused it, and also: drive past the limit in line, or SWR at or past
/// the warning on the amplifier (in line) or the radio.
///
/// `carried_trip` is a trip the amplifier already showed when this TUNE
/// started and hasn't cleared since: that one doesn't stop it, since keying
/// is what clears it. Any other trip does.
pub(crate) fn tune_must_stop(
    amp: &AmpStatus,
    rig: &RigStatus,
    swr_warn: f32,
    watts: Option<f32>,
    carried_trip: bool,
) -> Option<String> {
    if amp.alerts.tripped && !carried_trip {
        return Some("The amplifier tripped.".into());
    }
    let blocked = tune_blocked(amp, rig, watts);
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

/// What the strip needs that the radio's status doesn't carry.
pub(crate) struct TuneArgs<'a> {
    /// Why TUNE (the tone) may not be keyed.
    pub tone_blocked: Option<String>,
    /// Why RIG TUNE (the carrier) may not be keyed.
    pub carrier_blocked: Option<String>,
    pub limit_in_line: Option<f32>,
    pub secs: &'a mut u32,
    /// The Tune slider, 0 to 1 of the menu 34 setting.
    pub drive: &'a mut f32,
    /// The last tone that wouldn't play, and why.
    pub tone_error: Option<String>,
    /// The panel's Sound, 0 to 1, when it plays on a speaker other than the
    /// DUO's (the DUO's own is muted then): RADIO's E1 VOL turns this.
    pub panel_vol: Option<f32>,
}

/// What the operator did on the TUNE strip.
#[derive(Default)]
pub(crate) struct TuneOut {
    pub start: Option<Keying>,
    pub stop: bool,
    /// A new menu 34 setting, by its code.
    pub power: Option<u8>,
    /// Put menu 32 on USB audio.
    pub usb_audio: bool,
}

/// The TUNE and RIG TUNE buttons, the radio's power buttons and the Tune
/// slider, at the radio window's scale where it is drawn there.
pub(crate) fn tune_strip(ui: &mut egui::Ui, rig: &RigStatus, args: &mut TuneArgs<'_>) -> TuneOut {
    let mut out = TuneOut::default();
    let tuning = rig.tuning;
    ui.horizontal_wrapped(|ui| {
        let keyed = |k: Keying| tuning.is_some_and(|t| t.keying == k);
        for (k, label, blocked, hover) in [
            (
                Keying::Audio,
                "TX TUNE",
                args.tone_blocked.as_deref(),
                "Key the radio as its PTT does, with a 1 kHz tone from the panel at the Tune \
                 slider's level, through the radio's USB audio: SDRoxide's TUNE. It keys the \
                 amplifier's PTT line too. Click again to unkey. It also unkeys by itself at \
                 the time limit, and on a trip or a high SWR.",
            ),
            (
                Keying::Carrier,
                "RIG TUNE",
                args.carrier_blocked.as_deref(),
                "Key the radio's own TUNE carrier at the full power button setting, with no \
                 tone. The FDM-DUO may not key its PTT output for this: if the amplifier stays \
                 in receive, use TUNE. Click again to unkey.",
            ),
        ] {
            let on = keyed(k);
            let can = on || (tuning.is_none() && blocked.is_none());
            let resp = ui.add_enabled_ui(can, |ui| chrome::chip(ui, on, label)).inner;
            let resp = match blocked.filter(|_| tuning.is_none()) {
                Some(why) => resp.on_disabled_hover_text(why),
                None => resp.on_hover_text(hover),
            };
            if resp.clicked() {
                if tuning.is_some() {
                    out.stop = true;
                } else {
                    out.start = Some(k);
                }
            }
        }

        ui.add_space(z(6.0));
        ui.label(RichText::new("for").size(z(11.0)));
        ui.add(egui::DragValue::new(args.secs).range(TUNE_SECS).suffix(" s"))
            .on_hover_text("TUNE unkeys by itself after this long.");

        if let Some(t) = tuning {
            let left = t.until.saturating_duration_since(Instant::now()).as_secs_f32();
            ui.label(
                RichText::new(format!("keyed · {left:.0} s left"))
                    .size(z(11.0))
                    .color(theme::CYAN()),
            );
        } else if let Some(n) = &rig.tune_note {
            // Yellow only for a stop that is a warning; the time limit is not.
            let col = if n.starts_with("time limit") { theme::TEXT() } else { theme::YELLOW() };
            ui.label(RichText::new(format!("Stopped: {n}")).size(z(11.0)).color(col));
        }
    });

    let mut refused = None;
    ui.horizontal(|ui| {
        // The power buttons, captioned underneath so they read as the
        // radio's own and not the amplifier's.
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = z(2.0);
            // A click shows at once, as asked, until the radio's read-back
            // agrees or POWER_WAIT_S passes (AJ3G, 24 Oct 2026: the keys
            // seemed not to respond while the read-back was slow).
            let key = egui::Id::new("duo-power-asked");
            let t = ui.ctx().input(|i| i.time);
            let mut asked = ui.ctx().data(|d| d.get_temp::<(u8, f64)>(key));
            // Taken, or long enough ago that the note has been read.
            if asked.is_some_and(|(c, at)| rig.power_code == Some(c) || t - at > 15.0) {
                asked = None;
            }
            let pending = asked.filter(|(_, at)| t - at < POWER_WAIT_S).map(|(c, _)| c);
            let row = ui.horizontal(|ui| {
                for (label, code) in PRESETS {
                    let lit = pending.map_or(rig.power_code == Some(code), |c| c == code);
                    let resp = ui
                        .add_enabled_ui(rig.connected, |ui| chrome::chip(ui, lit, label))
                        .inner
                        .on_hover_text(preset_hover(code));
                    if resp.clicked() && rig.power_code != Some(code) {
                        out.power = Some(code);
                        asked = Some((code, t));
                    }
                }
            });
            if pending.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            }
            ui.ctx().data_mut(|d| match asked {
                Some(a) => {
                    d.insert_temp(key, a);
                }
                None => d.remove::<(u8, f64)>(key),
            });
            refused = asked.filter(|(_, at)| t - at >= POWER_WAIT_S).map(|(c, _)| c);
            let ink = ui.visuals().widgets.inactive.fg_stroke.color;
            let w = row.response.rect.width();
            ui.allocate_ui_with_layout(
                egui::vec2(w, 0.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.set_min_width(w);
                    let caption = match rig.power() {
                        Some(p) if !PRESETS.iter().any(|x| x.1 == p.code) => {
                            format!("{PRESETS_CAPTION} · {}", p.label)
                        }
                        _ => PRESETS_CAPTION.to_string(),
                    };
                    ui.label(RichText::new(caption).size(z(9.5)).strong().color(ink));
                },
            );
        });
        ui.add_space(z(10.0));
        ui.label("Tune");
        let est = planned_watts(rig, Keying::Audio, *args.drive);
        let over = matches!((est, args.limit_in_line), (Some(w), Some(l)) if w > l);
        let text = est.map_or("≈ — W".to_string(), |w| format!("≈ {w:.1} W"));
        ui.label(RichText::new(text).monospace().size(z(11.0)).color(if over {
            theme::YELLOW()
        } else {
            theme::TEXT()
        }))
        .on_hover_text(
            "What TUNE should put out at this level, from AJ3G's measurements: the slider \
             spans the power button's setting in a straight line. The radio's and the \
             amplifier's own meters show what it really does.",
        );
        // A fixed rail: `slider_vertical` is the skin without `fit_rail`'s
        // measuring, which the chips before it on this row throw off.
        ui.spacing_mut().slider_width = z(180.0);
        chrome::slider_vertical(
            ui,
            egui::Slider::new(args.drive, 0.0..=1.0)
                .show_value(true)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        )
        .on_hover_text(
            "TUNE's level within the power button's setting: the tone's volume into the \
             radio's USB audio. It can be moved while TUNE is keyed.",
        );
    });

    if rig.tx_input.as_deref().is_some_and(|i| i != "USB audio") && rig.connected {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "The radio's TX IN (menu 32) is on {}: TUNE's tone can't reach it.",
                    rig.tx_input.as_deref().unwrap_or("?")
                ))
                .size(z(11.0))
                .color(theme::YELLOW()),
            );
            if chrome::chip(ui, false, "USB AUDIO")
                .on_hover_text(
                    "Set menu 32 TX IN to USB audio, so the radio transmits the computer's \
                     audio (the panel's tone, or a digital-mode program's) and not the \
                     microphone. It stays set after the panel closes.",
                )
                .clicked()
            {
                out.usb_audio = true;
            }
        });
    }
    if let Some(c) = refused {
        let name = |c: Option<u8>| {
            c.and_then(|c| rigcat::elad::POWER_STEPS.get(c as usize)).map_or("?", |p| p.label)
        };
        ui.label(
            RichText::new(format!(
                "The radio still reads {}: it didn't take {}. Another program setting its power \
                 over CAT (SDR Console, SDRoxide) puts it back; close it, or click again.",
                name(rig.power_code),
                name(Some(c))
            ))
            .size(z(11.0))
            .color(theme::YELLOW()),
        );
    }
    if let Some(e) = &args.tone_error {
        ui.label(RichText::new(format!("Tone: {e}")).size(z(11.0)).color(theme::YELLOW()));
    }
    if let Some(l) = args.limit_in_line
        && rig.power().is_some_and(|p| p.watts > l)
    {
        ui.label(
            RichText::new(format!(
                "Menu 34 is over the amplifier's {l:.0} W drive limit for PTT: keep the Tune \
                 slider, and other programs' audio, below it."
            ))
            .size(z(11.0))
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
            tx_input: Some("USB audio".into()),
            mode: Some("USB"),
            ..RigStatus::default()
        }
    }

    fn w(r: &RigStatus) -> Option<f32> {
        planned_watts(r, Keying::Carrier, 1.0)
    }

    #[test]
    fn an_outside_program_keys_only_when_tune_could_at_the_full_setting() {
        // In bypass with both links: allowed.
        let r = duo(14_074_000, 9);
        assert_eq!(outside_ptt_blocked(&amp(false, 7), &r), None);
        // In line at MAX (8.3 W): over the HR500's drive limit (#6).
        assert!(outside_ptt_blocked(&amp(true, 5), &r).unwrap().contains("drive limit"));
        // At 5 W on the amplifier's band: allowed; on the wrong band: not.
        let five = duo(14_074_000, 8);
        assert_eq!(outside_ptt_blocked(&amp(true, 5), &five), None);
        assert!(outside_ptt_blocked(&amp(true, 7), &five).is_some());
        // Not while TUNE runs or the radio transmits on its own.
        let now = Instant::now();
        let tuning = RigStatus {
            tuning: Some(rigcat::Tuning { since: now, until: now, keying: Keying::Audio }),
            ..duo(14_074_000, 8)
        };
        assert!(outside_ptt_blocked(&amp(false, 5), &tuning).unwrap().contains("TUNE"));
        let keyed = RigStatus { tx_state: Some(1), ..duo(14_074_000, 8) };
        assert!(outside_ptt_blocked(&amp(false, 5), &keyed).is_some());
        // A tripped or absent amplifier refuses it, as it refuses TUNE.
        let mut tripped = amp(false, 5);
        tripped.alerts.tripped = true;
        assert!(outside_ptt_blocked(&tripped, &five).is_some());
        let gone = AmpStatus { connected: false, ..amp(false, 5) };
        assert!(outside_ptt_blocked(&gone, &five).is_some());
    }

    #[test]
    fn in_bypass_tune_needs_only_both_links() {
        let r = duo(14_074_000, 9);
        assert_eq!(tune_blocked(&amp(false, 7), &r, w(&r)), None);
        let off = RigStatus { connected: false, ..duo(14_074_000, 2) };
        assert!(tune_blocked(&amp(false, 5), &off, w(&off)).is_some());
        let no_amp = AmpStatus { connected: false, ..amp(false, 5) };
        assert!(tune_blocked(&no_amp, &r, w(&r)).is_some());
    }

    #[test]
    fn in_line_the_bands_must_match() {
        // 20 m is HRBN 5, 40 m is 7.
        let r = duo(14_074_000, 2);
        assert_eq!(tune_blocked(&amp(true, 5), &r, w(&r)), None);
        let why = tune_blocked(&amp(true, 7), &r, w(&r)).unwrap();
        assert!(why.contains("wrong low-pass filter"), "{why}");
        let six = duo(50_313_000, 2);
        assert!(tune_blocked(&amp(true, 5), &six, w(&six)).is_some());
    }

    #[test]
    fn in_line_tune_stays_within_the_drive_limit() {
        // MAX is taken as 8.3 W, over the HR500's 8 W; 5 W is within it.
        let max = duo(14_074_000, 9);
        assert!(tune_blocked(&amp(true, 5), &max, w(&max)).is_some());
        let five = duo(14_074_000, 8);
        assert_eq!(tune_blocked(&amp(true, 5), &five, w(&five)), None);
        // The tone at 90% of MAX is about 7.5 W: within it. At 100%, not.
        let tone = |d| planned_watts(&max, Keying::Audio, d);
        assert_eq!(tune_blocked(&amp(true, 5), &max, tone(0.9)), None);
        assert!(tune_blocked(&amp(true, 5), &max, tone(1.0)).is_some());
    }

    #[test]
    fn the_tune_slider_is_a_share_of_the_power_setting() {
        let r = duo(14_074_000, 8);
        assert_eq!(planned_watts(&r, Keying::Audio, 0.5), Some(2.5));
        assert_eq!(planned_watts(&r, Keying::Carrier, 0.5), Some(5.0));
        assert_eq!(planned_watts(&RigStatus::default(), Keying::Audio, 0.5), None);
    }

    #[test]
    fn the_tone_needs_its_sound_output_usb_audio_and_a_sideband() {
        let r = duo(14_074_000, 9);
        assert_eq!(tone_blocked(&r, Some("FDM-DUO Audio")), None);
        assert!(tone_blocked(&r, None).unwrap().contains("sound output"));
        let mic = RigStatus { tx_input: Some("Microphone".into()), ..r.clone() };
        assert!(tone_blocked(&mic, Some("FDM-DUO Audio")).unwrap().contains("menu 32"));
        let cw = RigStatus { mode: Some("CW"), ..r };
        assert!(tone_blocked(&cw, Some("FDM-DUO Audio")).unwrap().contains("CW"));
    }

    #[test]
    fn a_trip_lets_tune_clear_it_but_a_new_one_stops_it() {
        let mut a = amp(false, 5);
        a.alerts = AmpAlerts { tripped: true, ..AmpAlerts::default() };
        let r = duo(14_074_000, 2);
        // The HR500 clears a trip only when keyed: TUNE may, at low power.
        assert_eq!(tune_blocked(&a, &r, w(&r)), None);
        assert!(tune_blocked(&a, &r, Some(20.0)).unwrap().contains("tripped"));
        // Carried in from before the TUNE: it runs on. A new one stops it.
        assert_eq!(tune_must_stop(&a, &r, 5.0, w(&r), true), None);
        assert!(tune_must_stop(&a, &r, 5.0, w(&r), false).is_some());
        // Outside programs and CW messages are still refused.
        assert!(key_blocked(&a, &r, w(&r)).is_some());
        assert!(outside_ptt_blocked(&a, &r).is_some());
    }

    #[test]
    fn a_high_swr_stops_tune() {
        let mut a = amp(true, 5);
        let r = duo(14_074_000, 2);
        assert_eq!(tune_must_stop(&a, &r, 3.0, w(&r), false), None);
        a.readings.as_mut().unwrap().swr = 3.2;
        assert!(tune_must_stop(&a, &r, 3.0, w(&r), false).unwrap().contains("SWR"));
        let mut r = duo(14_074_000, 2);
        r.swr = elad::parse_swr("0 04.00");
        assert!(
            tune_must_stop(&amp(false, 5), &r, 3.0, w(&r), false).unwrap().contains("radio's SWR")
        );
    }

    #[test]
    fn drive_past_the_limit_in_line_stops_tune() {
        let mut a = amp(true, 5);
        a.readings.as_mut().unwrap().drive_w = 9.0;
        let r = duo(14_074_000, 2);
        assert!(tune_must_stop(&a, &r, 5.0, w(&r), false).unwrap().contains("Drive"));
    }
}
