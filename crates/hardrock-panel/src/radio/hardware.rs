//! A hardware controller's dials and keys, as the radio window's own: the
//! ELAD TMate 2's main dial, E1 and E2 are the face's main knob, E1 and E2
//! (decision 53), and its keys do what the face's keys do. They work with
//! the window closed: the window's state (the tuning step, E1's and E2's
//! settings, the lock) is kept where the window keeps it, so the two always
//! agree.
//!
//! Like the window, this sends nothing itself: it hands back a
//! [`RadioOut`] for the panel to send as it sends the window's.

use std::time::Instant;

use eframe::egui;
use rigcat::RigStatus;

use super::face;
pub(crate) use super::face::Turn;
use super::knobs::{E1, MainMode};
pub(crate) use super::knobs::{HOLD_DEFAULT_S as HOLD_DEFAULT, PressTracker, hold_s};
use super::logic::{self, BandDef, RadioPrefs};
use super::rigtheme::rig_theme;
use super::{Face, RadioOut, RadioUi, prefs_id, ui_id};

/// A key's job on the radio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadioKey {
    /// The next band up (`1`) or down (`-1`), at its last frequency and
    /// mode. From between bands, the nearest band that way.
    Band(i64),
    /// The next mode in the radio's list, or the one before.
    Mode(i64),
    /// Receive on the other VFO.
    SwapVfo,
    /// VFO B to VFO A's frequency.
    AEqualsB,
    Split,
    /// RIT on or off.
    Rit,
    RitClear,
    /// The knobs' lock, as E1 and E2 pressed together on the radio.
    Lock,
    /// The main dial's tuning step ten times coarser (`1`) or finer (`-1`).
    Step(i64),
}

/// What the controller did this frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hardware {
    /// Main, E1, E2.
    pub turns: [Turn; 3],
    pub keys: Vec<RadioKey>,
}

impl Hardware {
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.turns.iter().all(|t| t.steps == 0 && t.press.is_none())
    }
}

/// What the controller's display shows of the window's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HardwareView {
    /// The main dial's tuning step, in Hz.
    pub step_hz: u64,
    pub locked: bool,
    /// Turning the main dial changes the step, not the frequency.
    pub step_mode: bool,
    /// E1's setting as last turned to, shown for a moment after a turn.
    pub e1: Option<(i32, Instant)>,
    /// The dial as on the LCD: with the transverter's offset.
    pub xvtr_hz: i64,
    /// A data mode was chosen in the window, and the radio is still in USB.
    pub digi: bool,
}

fn state(ctx: &egui::Context) -> (RadioPrefs, RadioUi) {
    ctx.data_mut(|d| {
        (d.get_persisted(prefs_id()).unwrap_or_default(), d.get_temp(ui_id()).unwrap_or_default())
    })
}

/// The window's state as the controller's display shows it.
pub(crate) fn view(ctx: &egui::Context) -> HardwareView {
    let (prefs, rui) = state(ctx);
    let n = logic::digit_count(prefs.xvtr_hz);
    let step_hz = rui.sel.map_or(super::knobs::STEP_DEFAULT, |i| logic::place(i, n));
    HardwareView {
        step_hz,
        locked: rui.knobs.locked,
        step_mode: rui.knobs.main == MainMode::Step,
        e1: rui.knobs.pending.get(rui.knobs.e1.name()).copied(),
        xvtr_hz: prefs.xvtr_hz,
        digi: rui.digi.is_some(),
    }
}

/// Apply what the controller did. `panel_vol` is the panel's Sound when it
/// plays on an outside speaker (E1 VOL turns it then, as on the face).
pub(crate) fn apply(
    ctx: &egui::Context,
    st: Option<&RigStatus>,
    panel_vol: Option<f32>,
    hw: &Hardware,
) -> RadioOut {
    let mut out = RadioOut::default();
    let Some(s) = st else { return out };
    if hw.is_empty() {
        return out;
    }
    let (prefs, mut rui) = state(ctx);
    let proto = s.model.protocol();
    let mut f = Face {
        p: &*proto,
        s,
        th: rig_theme(s.model),
        lock: logic::locked(s),
        carrier_blocked: None,
        xvtr: prefs.xvtr_hz,
        panel_vol,
        rx_dbm: None,
        st: &mut rui,
        out: &mut out,
    };
    let n = logic::digit_count(prefs.xvtr_hz);

    // The face's knobs turn with the dials, for the eye.
    for (k, t) in hw.turns.iter().enumerate() {
        f.st.knobs.turned[k] += t.steps as f32 * super::knobs::DRAG_STEP_RAD;
    }
    face::turn_knobs(&mut f, n, hw.turns);
    // VOL turned on an outside speaker: shown as the face's caption is.
    if hw.turns[1].steps != 0
        && f.st.knobs.e1 == E1::Vol
        && let Some(v) = f.out.panel_volume
    {
        f.st.knobs.pending.insert(E1::Vol.name(), ((v * 100.0).round() as i32, Instant::now()));
    }

    for &k in &hw.keys {
        key(&mut f, &prefs, n, k);
    }
    ctx.data_mut(|d| d.insert_temp(ui_id(), rui));
    out
}

fn key(f: &mut Face<'_>, prefs: &RadioPrefs, n: usize, k: RadioKey) {
    // The lock is the panel's own, so it works whatever the radio is doing.
    if k == RadioKey::Lock {
        f.st.knobs.locked = !f.st.knobs.locked;
        return;
    }
    // SAFETY INTERLOCK (#5, panel side): nothing changes on the radio while
    // it transmits or isn't connected, nor while the knobs are locked.
    if f.lock.is_some() || f.st.knobs.locked {
        return;
    }
    match k {
        RadioKey::Band(dir) => band(f, prefs, dir),
        RadioKey::Mode(dir) => mode(f, dir),
        RadioKey::SwapVfo => {
            let b = logic::rx_vfo(f.s) == rigcat::Vfo::B;
            f.push(f.p.select_vfo(if b { rigcat::Vfo::A } else { rigcat::Vfo::B }));
        }
        RadioKey::AEqualsB => f.push(logic::a_to_b(f.p, f.s)),
        RadioKey::Split => {
            let split = f.s.split == Some(true);
            f.push(f.p.set_split(!split));
            f.out.tx_follows_rx = Some(!split);
        }
        RadioKey::Rit => {
            let on = f.s.rit_on == Some(true);
            f.push(f.p.set_rit_on(!on));
        }
        RadioKey::RitClear => {
            let v = f.p.set_rit(0);
            f.out.frames.extend(v);
        }
        RadioKey::Step(dir) => {
            let now = face::tuning_step(f, n);
            let to = super::knobs::next_step(now, dir);
            f.st.sel = super::knobs::digit_of(to, n).or(f.st.sel);
        }
        RadioKey::Lock => {}
    }
}

/// The band beside the radio's, `dir` up or down, at its last frequency and
/// mode (the HR500 follows the radio).
fn band(f: &mut Face<'_>, prefs: &RadioPrefs, dir: i64) {
    let xvtr = prefs.xvtr_hz;
    let Some(rf) = logic::dial_hz(f.s).map(|h| logic::shown(h, xvtr)) else { return };
    let Some(b) = band_beside(&logic::bands_for(xvtr), rf, dir > 0) else { return };
    let digi = f.st.digi.and_then(logic::digi).filter(|d| logic::digi_on(d, b).is_some());
    if digi.is_none() {
        f.st.digi = None;
        f.st.digi_note = None;
    }
    let (to, mode) = prefs.band_target(b, digi);
    match logic::if_of(to, xvtr) {
        Some(hz) => face::tune_to(f, hz, Some(&mode)),
        None => f.st.note = Some(format!("{} is outside the radio's range.", b.label())),
    }
}

/// The band after (`up`) or before the one `rf` is in; from between bands,
/// the nearest that way. None past the last.
fn band_beside(bands: &[&'static BandDef], rf: u64, up: bool) -> Option<&'static BandDef> {
    let mut sorted: Vec<&'static BandDef> = bands.to_vec();
    sorted.sort_by_key(|b| b.lo);
    if up {
        sorted.into_iter().find(|b| b.lo > rf)
    } else {
        sorted.into_iter().rev().find(|b| b.hi < rf)
    }
}

/// The next mode in the radio's list `dir` along, round from the end, as
/// the MODE pop-out lists them (FM only where the radio's menu offers it).
fn mode(f: &mut Face<'_>, dir: i64) {
    let fm_off = logic::find(f.p, "FM").is_some() && f.s.value("FM") == Some("0");
    let modes: Vec<&str> =
        f.p.modes().iter().copied().filter(|m| !(*m == "FM" && fm_off)).collect();
    if modes.is_empty() {
        return;
    }
    let len = modes.len() as i64;
    let now = f.s.mode.and_then(|m| modes.iter().position(|x| *x == m)).map_or(0, |i| i as i64);
    let to = modes[(now + dir).rem_euclid(len) as usize];
    f.st.digi = None;
    f.st.digi_note = None;
    f.push(f.p.set_mode(to));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_beside_steps_and_finds_the_nearest_from_between() {
        let bands = logic::bands_for(0);
        let name = |b: Option<&BandDef>| b.map(|b| b.name);
        // In 40 m: 30 up, 60 down.
        assert_eq!(name(band_beside(&bands, 7_074_000, true)), Some("30"));
        assert_eq!(name(band_beside(&bands, 7_074_000, false)), Some("60"));
        // General coverage at 6 MHz, between 60 and 40.
        assert_eq!(name(band_beside(&bands, 6_000_000, true)), Some("40"));
        assert_eq!(name(band_beside(&bands, 6_000_000, false)), Some("60"));
        // Past the ends: nowhere.
        assert_eq!(name(band_beside(&bands, 1_840_000, false)), None);
        assert_eq!(name(band_beside(&bands, 52_000_000, true)), None);
    }
}
