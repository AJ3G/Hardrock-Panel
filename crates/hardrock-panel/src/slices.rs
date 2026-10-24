//! RX slices (decision 66): up to three more receivers in the span beside
//! the panel's own (slice A), as FDM-SW2's and SDR Console's. Each has its
//! own dial, mode, passband, volume and sound output, so one can sit on
//! WSPR and another on FT4 and feed a WSJT-X each through a virtual audio
//! cable while A is listened to and transmitted from. All of them are
//! software, in the one I/Q stream, so they must be inside the span.
//!
//! A is the only slice that moves the radio: the DUO's filter, VFO B and
//! split follow it as before (decision 22). The others never send the radio
//! anything. SWAP A swaps a slice with A, which retunes A through the same path
//! as a click on the scope, so not while transmitting (interlock #9).

use eframe::egui::{self, Color32, RichText};
use sdr::{Mode, SliceSpec};
use serde::{Deserialize, Serialize};

use crate::rx::{Rx, RxSettings, mode_of};
use crate::scope::digimodes;
use crate::{chrome, theme};

/// Slices besides A.
pub(crate) const MAX: usize = 3;

/// A slice as kept between runs. `id` 1, 2 and 3 are B, C and D.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SliceSettings {
    pub id: u32,
    /// Its dial, the radio's own Hz.
    pub rx_hz: f64,
    /// As `sdr::Mode::label` names it.
    pub mode: String,
    pub passband: (f32, f32),
    /// The sound output it plays on; `None` is the system's default.
    pub audio_out: Option<String>,
    pub volume: f32,
    pub muted: bool,
}

impl Default for SliceSettings {
    fn default() -> Self {
        SliceSettings {
            id: 1,
            rx_hz: 14_074_000.0,
            mode: "USB".into(),
            passband: Mode::Usb.default_passband(),
            audio_out: None,
            volume: 0.5,
            muted: false,
        }
    }
}

impl SliceSettings {
    pub(crate) fn mode(&self) -> Mode {
        mode_of(&self.mode)
    }
}

/// B, C or D.
pub(crate) fn name(id: u32) -> &'static str {
    match id {
        1 => "B",
        2 => "C",
        _ => "D",
    }
}

/// Each slice's colour on the scope and in its row: none of them the
/// warnings' yellow or red, nor A's blue.
pub(crate) fn colour(id: u32) -> Color32 {
    match id {
        1 => Color32::from_rgb(0xb0, 0x7c, 0xff),
        2 => Color32::from_rgb(0x41, 0xd1, 0x7a),
        _ => Color32::from_rgb(0xff, 0x9a, 0xd5),
    }
}

/// Whether a slice's passband is inside a span of `rate` around `centre`,
/// with a little room at the edges where the radio's own filter rolls off.
pub(crate) fn in_span(s: &SliceSettings, centre: f64, rate: f64) -> bool {
    let half = 0.48 * rate;
    let (lo, hi) = (s.rx_hz + s.passband.0 as f64, s.rx_hz + s.passband.1 as f64);
    rate > 0.0 && lo.min(hi) >= centre - half && lo.max(hi) <= centre + half
}

/// The slices the pipeline is to run: those inside the span.
pub(crate) fn specs(slices: &[SliceSettings], centre: f64, rate: f64) -> Vec<SliceSpec> {
    slices
        .iter()
        .filter(|s| in_span(s, centre, rate))
        .map(|s| SliceSpec {
            id: s.id,
            offset_hz: s.rx_hz - centre,
            mode: s.mode(),
            passband: s.passband,
        })
        .collect()
}

/// A new slice at `hz` in `mode`, with the first free name, playing where A
/// does. `None` when all three are in use.
pub(crate) fn add(
    slices: &mut Vec<SliceSettings>,
    hz: f64,
    mode: Mode,
    passband: (f32, f32),
    audio_out: Option<String>,
) -> Option<u32> {
    let id = (1..=MAX as u32).find(|i| slices.iter().all(|s| s.id != *i))?;
    slices.push(SliceSettings {
        id,
        rx_hz: hz,
        mode: mode.label().into(),
        passband,
        audio_out,
        ..SliceSettings::default()
    });
    slices.sort_by_key(|s| s.id);
    Some(id)
}

/// Put a WSJT or keyboard mode's dial on a slice: its frequency on the band
/// `near` is in, upper sideband, the mode's passband.
pub(crate) fn park(s: &mut SliceSettings, d: &digimodes::Digi, near: f64) -> bool {
    let band = crate::scope::bandplan::nearest_band(near);
    let Some(&dial) = d.dials.iter().find(|&&f| (f as f64) >= band.lo && (f as f64) <= band.hi)
    else {
        return false;
    };
    s.rx_hz = dial as f64;
    s.mode = Mode::Usb.label().into();
    s.passband = d.passband(dial);
    true
}

/// What the slice rows asked for this frame.
#[derive(Default)]
pub(crate) struct SliceOut {
    /// Swap this slice with A.
    pub swap: Option<u32>,
}

/// The modes a slice can be parked on with one pick: those with a calling
/// frequency.
const PARK: [&str; 7] = ["FT8", "FT4", "WSPR", "JT65", "JT9", "PSK31", "RTTY"];

/// The SLICES rows under SCOPE's receiver line: + SLICE, then a row for
/// each slice.
pub(crate) fn rows(
    ui: &mut egui::Ui,
    s: &mut RxSettings,
    rx: &Rx,
    sounds: &[String],
    transmitting: bool,
) -> SliceOut {
    let mut out = SliceOut::default();
    let centre = rx.centre();
    let rate = rx.rate();
    let levels = rx.slice_levels();
    let mut remove = None;
    for sl in &mut s.slices {
        let c = colour(sl.id);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("SLICE {}", name(sl.id))).size(10.5).strong().color(c));
            let mut khz = sl.rx_hz / 1e3;
            let r = ui.add(
                egui::DragValue::new(&mut khz)
                    .speed(0.1)
                    .fixed_decimals(3)
                    .suffix(" kHz")
                    .range(0.0..=6.0e6),
            );
            if r.changed() {
                sl.rx_hz = (khz * 1e3).round();
            }
            r.on_hover_text("Its dial: drag, or click and type");
            let mut m = sl.mode();
            egui::ComboBox::from_id_salt(("slice-mode", sl.id))
                .width(64.0)
                .selected_text(m.label())
                .show_ui(ui, |ui| {
                    for x in Mode::ALL {
                        ui.selectable_value(&mut m, x, x.label());
                    }
                });
            if m != sl.mode() {
                sl.mode = m.label().into();
                sl.passband = m.default_passband();
            }
            egui::ComboBox::from_id_salt(("slice-park", sl.id))
                .width(64.0)
                .selected_text("PARK")
                .show_ui(ui, |ui| {
                    for n in PARK {
                        if let Some(d) = digimodes::digi(n)
                            && ui.selectable_label(false, n).clicked()
                        {
                            park(sl, d, centre);
                        }
                    }
                })
                .response
                .on_hover_text("Put it on a mode's calling frequency on this band");
            ui.label(
                RichText::new(crate::scope::bw_text(sl.passband)).monospace().size(11.5).color(c),
            );
            let inside = in_span(sl, centre, rate);
            match levels.iter().find(|l| l.0 == sl.id) {
                Some((_, dbm)) if inside => {
                    ui.label(
                        RichText::new(format!("{dbm:.0} dBm"))
                            .monospace()
                            .size(11.5)
                            .color(theme::TEXT()),
                    );
                }
                _ if !inside && rate > 0.0 => {
                    ui.label(
                        RichText::new("outside the span: not heard")
                            .size(11.5)
                            .color(theme::TEXT()),
                    );
                }
                _ => {}
            }
            ui.add(egui::Slider::new(&mut sl.volume, 0.0..=1.0).show_value(false))
                .on_hover_text("Its volume");
            if chrome::chip(ui, sl.muted, "MUTE").clicked() {
                sl.muted = !sl.muted;
            }
            let shown = sl.audio_out.clone().unwrap_or_else(|| "Default output".into());
            egui::ComboBox::from_id_salt(("slice-out", sl.id))
                .width(170.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    let sounds = fresh_sounds(ui, sounds);
                    ui.selectable_value(&mut sl.audio_out, None, "Default output");
                    for d in &sounds {
                        ui.selectable_value(&mut sl.audio_out, Some(d.clone()), d);
                    }
                })
                .response
                .on_hover_text(
                    "Where it plays: a speaker, or a virtual audio cable to feed WSJT-X or \
                     another program",
                );
            if let Some(e) = rx.slice_audio_error(sl.id) {
                ui.label(RichText::new(e).size(11.5).color(theme::ALERT()));
            }
            let r = ui.add_enabled(!transmitting && inside, egui::Button::new("SWAP A"));
            if r.on_hover_text(format!(
                "Swap slice {} with A: A, the one the radio follows and transmits from, goes \
                 here",
                name(sl.id)
            ))
            .on_disabled_hover_text("Not while transmitting, and only inside the span")
            .clicked()
            {
                out.swap = Some(sl.id);
            }
            if chrome::chip(ui, false, "×").on_hover_text("Remove this slice").clicked() {
                remove = Some(sl.id);
            }
        });
    }
    if let Some(id) = remove {
        s.slices.retain(|x| x.id != id);
    }
    out
}

/// The + SLICE key, on the receiver's line: a new slice where A is, or
/// beside it when the span has room.
pub(crate) fn add_key(ui: &mut egui::Ui, s: &mut RxSettings, rx: &Rx) {
    let full = s.slices.len() >= MAX;
    let r = chrome::chip_enabled(ui, !full && rx.active(), false, "+ SLICE")
        .on_hover_text("Another receiver in the span, with its own mode and sound output")
        .on_disabled_hover_text(if full {
            "Four slices is the most: A, B, C and D"
        } else {
            "Needs the I/Q running"
        });
    if r.clicked() {
        // 5 kHz up from A for B, 10 for C, 15 for D.
        let at = rx.rx_hz + 5_000.0 * (s.slices.len() + 1) as f64;
        let at = if (at - rx.centre()).abs() < 0.45 * rx.rate() { at } else { rx.rx_hz };
        add(&mut s.slices, at, rx.mode, rx.passband, s.audio_out.clone());
    }
}

/// The sound outputs for a slice's list, read again each time a list
/// opens, so a speaker turned on after start-up is in it (AJ3G, 24 Oct
/// 2026); `known` until one has.
fn fresh_sounds(ui: &egui::Ui, known: &[String]) -> Vec<String> {
    let (shown, list) = (egui::Id::new("slice-sounds-shown"), egui::Id::new("slice-sounds"));
    let now = ui.ctx().cumulative_pass_nr();
    ui.ctx().data_mut(|d| {
        let last = d.get_temp::<u64>(shown);
        d.insert_temp(shown, now);
        if crate::settings::just_opened(last, now) {
            d.insert_temp(list, crate::audio::output_devices());
        }
        d.get_temp::<Vec<String>>(list).unwrap_or_else(|| known.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_take_the_first_free_name_up_to_d() {
        let mut v = Vec::new();
        let pb = (100.0, 2700.0);
        assert_eq!(add(&mut v, 14.2e6, Mode::Usb, pb, None), Some(1));
        assert_eq!(add(&mut v, 14.2e6, Mode::Usb, pb, None), Some(2));
        v.retain(|s| s.id != 1);
        assert_eq!(add(&mut v, 14.2e6, Mode::Usb, pb, None), Some(1));
        assert_eq!(add(&mut v, 14.2e6, Mode::Usb, pb, None), Some(3));
        assert_eq!(add(&mut v, 14.2e6, Mode::Usb, pb, None), None);
        assert_eq!(v.iter().map(|s| name(s.id)).collect::<Vec<_>>(), ["B", "C", "D"]);
    }

    #[test]
    fn only_slices_inside_the_span_are_run() {
        let mut v = Vec::new();
        add(&mut v, 14_074_000.0, Mode::Usb, (200.0, 3000.0), None);
        add(&mut v, 14_300_000.0, Mode::Usb, (200.0, 3000.0), None);
        // 192 kHz around 14.1 MHz: B is in, C is not.
        let sp = specs(&v, 14_100_000.0, 192_000.0);
        assert_eq!(sp.len(), 1);
        assert_eq!(sp[0].id, 1);
        assert_eq!(sp[0].offset_hz, -26_000.0);
        assert_eq!(sp[0].passband, (200.0, 3000.0));
        // Its passband past the edge is out too.
        v[0].rx_hz = 14_100_000.0 + 0.48 * 192_000.0 - 1000.0;
        assert!(specs(&v, 14_100_000.0, 192_000.0).is_empty());
    }

    #[test]
    fn park_puts_a_slice_on_the_modes_dial_on_this_band() {
        let mut s = SliceSettings::default();
        assert!(park(&mut s, digimodes::digi("WSPR").unwrap(), 14_100_000.0));
        assert_eq!(s.rx_hz, 14_095_600.0);
        assert_eq!(s.passband, (1400.0, 1600.0));
        assert!(park(&mut s, digimodes::digi("FT4").unwrap(), 7_050_000.0));
        assert_eq!(s.rx_hz, 7_047_500.0);
        assert_eq!(s.mode(), Mode::Usb);
    }
}
