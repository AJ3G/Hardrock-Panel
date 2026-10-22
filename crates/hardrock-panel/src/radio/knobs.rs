//! What the face's three knobs do, as the FDM-DUO's own do (decision 53):
//! the main knob tunes, and a push opens the tuning step, a push and hold
//! digit-by-digit tuning; E1 and E2 each change the setting they have
//! selected, a push selecting the next. Holding E1 or E2 stands in for the
//! radio's E1 and E2 pressed together, which locks the knobs.
//!
//! Only the logic is here, so it can be tested: what a wheel or a drag round
//! a knob is worth in steps, whether a press was a push or a hold, and the
//! frames a turn sends. [`super::face::knobs`] draws them.

use rigcat::{Frame, Protocol, RigStatus};

use super::logic;

/// The radio's hold time when it hasn't answered `HT` (menu 71, HOLD TIME):
/// the manual's default.
pub(crate) const HOLD_DEFAULT_S: f64 = 1.0;

/// How far round a knob one step is, when dragged: 24 to a turn.
pub(crate) const DRAG_STEP_RAD: f32 = std::f32::consts::TAU / 24.0;

/// Wheel travel in points (a trackpad) worth one step.
const POINTS_PER_STEP: f32 = 40.0;

/// Steps a page of wheel travel is worth.
const STEPS_PER_PAGE: f32 = 5.0;

/// The largest and smallest tuning steps the main knob's STEP mode offers,
/// in Hz: the dial's digits from 1 Hz to 1 MHz.
const STEP_MIN: u64 = 1;
const STEP_MAX: u64 = 1_000_000;

/// The tuning step until one is chosen.
pub(crate) const STEP_DEFAULT: u64 = 100;

/// The knobs' state from frame to frame (in [`super::RadioUi`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct KnobUi {
    pub main: MainMode,
    pub e1: E1,
    pub e2: E2,
    /// The radio's E1 and E2 lock, kept by the panel: the knobs and the
    /// dial do nothing while it is on.
    pub locked: bool,
    /// The press on each knob: main, E1, E2.
    pub press: [PressTracker; 3],
    /// Wheel travel short of a step, each knob.
    pub wheel: [f32; 3],
    /// Drag angle short of a step, and the pointer's angle last frame.
    pub drag: f32,
    pub drag_last: Option<f32>,
    /// How far each knob's mark has turned, for the drawing.
    pub turned: [f32; 3],
    /// A setting turned to and when, until the radio reads it back, so
    /// quick turns add up (by the setting's name).
    pub pending: std::collections::HashMap<&'static str, (i32, std::time::Instant)>,
}

/// What the main knob is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MainMode {
    /// Tuning by the step.
    #[default]
    Tune,
    /// The STEP menu: turning changes the step.
    Step,
    /// Digit by digit: turning changes the selected digit; E1 and E2 move
    /// the selection.
    Digit,
}

/// What E1 changes: the radio's E1 list, in its order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum E1 {
    #[default]
    Vol,
    Sql,
    Agc,
    Nr,
    Nb,
}

impl E1 {
    pub(crate) const ALL: [E1; 5] = [E1::Vol, E1::Sql, E1::Agc, E1::Nr, E1::Nb];

    pub(crate) fn name(self) -> &'static str {
        match self {
            E1::Vol => "VOL",
            E1::Sql => "SQL",
            E1::Agc => "AGC",
            E1::Nr => "NR",
            E1::Nb => "NB",
        }
    }

    /// The next on a push, round to the first after the last.
    pub(crate) fn next(self) -> E1 {
        let i = Self::ALL.iter().position(|e| *e == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// What E2 changes: the radio's E2 list, in its order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum E2 {
    #[default]
    Filter,
    Pitch,
    Rit,
}

impl E2 {
    pub(crate) const ALL: [E2; 3] = [E2::Filter, E2::Pitch, E2::Rit];

    pub(crate) fn name(self) -> &'static str {
        match self {
            E2::Filter => "FIL",
            E2::Pitch => "PITCH",
            E2::Rit => "RIT",
        }
    }

    /// Whether the radio offers it now: PITCH in CW only, RIT only while
    /// RIT is on (the radio hides its RIT value otherwise).
    pub(crate) fn offered(self, s: &RigStatus) -> bool {
        match self {
            E2::Filter => true,
            E2::Pitch => is_cw(s),
            E2::Rit => s.rit_on == Some(true),
        }
    }

    /// The next one offered on a push.
    pub(crate) fn next(self, s: &RigStatus) -> E2 {
        let i = Self::ALL.iter().position(|e| *e == self).unwrap_or(0);
        (1..=Self::ALL.len())
            .map(|k| Self::ALL[(i + k) % Self::ALL.len()])
            .find(|e| e.offered(s))
            .unwrap_or(E2::Filter)
    }

    /// Itself if offered now, else the filter.
    pub(crate) fn now(self, s: &RigStatus) -> E2 {
        if self.offered(s) { self } else { E2::Filter }
    }
}

fn is_cw(s: &RigStatus) -> bool {
    matches!(s.mode, Some("CW" | "CWR" | "CW-R"))
}

/// A push, or a push held past the hold time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    Push,
    Hold,
}

/// Tells a push from a hold, as the radio does: held past the hold time is
/// a hold, and fires then, without waiting for the release; let go sooner,
/// it is a push, on the release.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PressTracker {
    /// When the button went down, while it is down.
    since: Option<f64>,
    /// The hold has fired for this press.
    held: bool,
    /// The press turned the knob (a drag), so its release is no push.
    cancelled: bool,
}

impl PressTracker {
    /// Feed the button's state at time `now` (seconds); `hold` is the hold
    /// time. Says what happened, if anything.
    pub(crate) fn update(&mut self, down: bool, now: f64, hold: f64) -> Option<Press> {
        match (down, self.since) {
            (true, None) => {
                *self = PressTracker { since: Some(now), held: false, cancelled: false };
                None
            }
            (true, Some(t)) => {
                if !self.held && !self.cancelled && now - t >= hold {
                    self.held = true;
                    Some(Press::Hold)
                } else {
                    None
                }
            }
            (false, Some(_)) => {
                let push = !self.held && !self.cancelled;
                *self = PressTracker::default();
                push.then_some(Press::Push)
            }
            (false, None) => None,
        }
    }

    /// The press became a turn: no push or hold comes of it.
    pub(crate) fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// The button is down and a hold may still come, so keep repainting.
    pub(crate) fn waiting(&self) -> bool {
        self.since.is_some() && !self.held && !self.cancelled
    }
}

/// The hold time the radio uses, from its `HT` answer (menu 71) in ms,
/// else [`HOLD_DEFAULT_S`]. In seconds.
pub(crate) fn hold_s(s: &RigStatus) -> f64 {
    s.value("HT")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map_or(HOLD_DEFAULT_S, |ms| f64::from(ms.clamp(200, 2500)) / 1000.0)
}

/// How a wheel's travel is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelUnit {
    Point,
    Line,
    Page,
}

/// Whole steps in a wheel movement of `dy` (positive up), adding to what
/// is left over in `acc` from earlier movements. A mouse wheel's notch is a
/// line, one step, as one click of the radio's knob; a trackpad's points
/// add up to a step every [`POINTS_PER_STEP`].
pub(crate) fn wheel_to_steps(unit: WheelUnit, dy: f32, acc: &mut f32) -> i64 {
    let per = match unit {
        WheelUnit::Line => 1.0,
        WheelUnit::Point => 1.0 / POINTS_PER_STEP,
        WheelUnit::Page => STEPS_PER_PAGE,
    };
    if dy == 0.0 {
        return 0;
    }
    // A change of direction starts afresh.
    if *acc != 0.0 && acc.signum() != dy.signum() {
        *acc = 0.0;
    }
    *acc += dy * per;
    let n = whole(*acc);
    *acc -= n;
    n as i64
}

/// Whole steps in a drag round a knob by `d_rad` (clockwise positive, as
/// the screen's y runs down), adding to `acc`. Clockwise tunes up.
pub(crate) fn angle_to_steps(d_rad: f32, acc: &mut f32) -> i64 {
    use std::f32::consts::{PI, TAU};
    let mut d = d_rad % TAU;
    if d > PI {
        d -= TAU;
    } else if d <= -PI {
        d += TAU;
    }
    *acc += d / DRAG_STEP_RAD;
    let n = whole(*acc);
    *acc -= n;
    n as i64
}

/// The whole steps in `v`, a hair under a whole one counting as it (so
/// a quarter turn is six steps however the sum rounds).
fn whole(v: f32) -> f32 {
    (v + v.signum() * 1e-4).trunc()
}

/// The tuning step `dir` places on from `step`, inside 1 Hz to 1 MHz.
pub(crate) fn next_step(step: u64, dir: i64) -> u64 {
    let mut s = step.clamp(STEP_MIN, STEP_MAX);
    for _ in 0..dir.unsigned_abs() {
        s = if dir > 0 { (s * 10).min(STEP_MAX) } else { (s / 10).max(STEP_MIN) };
    }
    s
}

/// A step as the face shows it: "100 Hz", "10 kHz", "1 MHz".
pub(crate) fn step_text(step: u64) -> String {
    match step {
        s if s >= 1_000_000 => format!("{} MHz", s / 1_000_000),
        s if s >= 1000 => format!("{} kHz", s / 1000),
        s => format!("{s} Hz"),
    }
}

/// The dial digit (index, from the left of `n`) that `place` is, if any.
pub(crate) fn digit_of(place: u64, n: usize) -> Option<usize> {
    (0..n).find(|i| logic::place(*i, n) == place)
}

fn num(s: &RigStatus, p: &dyn Protocol, cmd: &str) -> Option<i32> {
    let c = logic::find(p, cmd)?;
    logic::readback(c, s)?.trim().trim_start_matches('+').parse().ok()
}

fn on(s: &RigStatus, p: &dyn Protocol, cmd: &str) -> Option<bool> {
    num(s, p, cmd).map(|v| v != 0)
}

/// Where E1's setting is now, as one number: the volume or squelch; AGC
/// 0 or manual gain 1; for NR and NB, 0 when off, else the level.
pub(crate) fn e1_value(p: &dyn Protocol, s: &RigStatus, e: E1) -> Option<i32> {
    match e {
        E1::Vol => num(s, p, "VM"),
        E1::Sql => num(s, p, "SQ"),
        E1::Agc => num(s, p, "GC"),
        E1::Nr => ladder(on(s, p, "NR")?, num(s, p, "NC")),
        E1::Nb => ladder(on(s, p, "NB")?, num(s, p, "NK")),
    }
}

fn ladder(on: bool, level: Option<i32>) -> Option<i32> {
    Some(if on { level.unwrap_or(1).max(1) } else { 0 })
}

/// The range of [`e1_value`].
pub(crate) fn e1_range(e: E1) -> (i32, i32) {
    match e {
        E1::Vol => (0, 100),
        E1::Sql | E1::Nr | E1::Nb => (0, 10),
        E1::Agc => (0, 1),
    }
}

/// E1's setting in words, as its caption shows it.
pub(crate) fn e1_text(e: E1, v: Option<i32>) -> String {
    match (e, v) {
        (_, None) => format!("{} —", e.name()),
        (E1::Agc, Some(0)) => "AGC".into(),
        (E1::Agc, Some(_)) => "MAN GAIN".into(),
        (E1::Nr | E1::Nb | E1::Sql, Some(0)) => format!("{} OFF", e.name()),
        (_, Some(v)) => format!("{} {v}", e.name()),
    }
}

/// The frames that take E1's setting from `from` to `to` (the same scale
/// as [`e1_value`]). NR and NB switch on with their level, and off at 0.
pub(crate) fn e1_frames(
    p: &dyn Protocol,
    s: &RigStatus,
    e: E1,
    from: i32,
    to: i32,
) -> Result<Vec<Frame>, String> {
    let set = |cmd: &str, v: &str| -> Result<Frame, String> {
        let c = logic::find(p, cmd).ok_or_else(|| format!("This radio has no {cmd} command."))?;
        logic::control_frame(p, c, v, s)
    };
    if from == to {
        return Ok(Vec::new());
    }
    Ok(match e {
        E1::Vol => vec![set("VM", &to.to_string())?],
        E1::Sql => vec![set("SQ", &to.to_string())?],
        E1::Agc => vec![set("GC", if to > 0 { "1" } else { "0" })?],
        E1::Nr | E1::Nb => {
            let (sw, lvl) = if e == E1::Nr { ("NR", "NC") } else { ("NB", "NK") };
            match (from, to) {
                (_, 0) => vec![set(sw, "0")?],
                (0, _) => vec![set(sw, "1")?, set(lvl, &to.to_string())?],
                _ => vec![set(lvl, &to.to_string())?],
            }
        }
    })
}

/// The filter widths of the radio's mode, narrowest first, each once.
pub(crate) fn widths(p: &dyn Protocol, s: &RigStatus) -> Vec<u32> {
    let mut w = s.mode.map(|m| p.filters(m)).unwrap_or_default();
    w.sort_unstable();
    w.dedup();
    w
}

/// The filter width `dir` widths on from `now` (the nearest listed width to
/// it), held at the ends.
pub(crate) fn next_width(widths: &[u32], now: u32, dir: i64) -> Option<u32> {
    let i = widths.iter().enumerate().min_by_key(|(_, w)| w.abs_diff(now))?.0 as i64;
    let j = (i + dir).clamp(0, widths.len() as i64 - 1) as usize;
    Some(widths[j])
}

/// The memory channel `dir` channels on from `now`, round 0 to 199.
pub(crate) fn next_memory(now: u16, dir: i64) -> u16 {
    (i64::from(now) + dir).rem_euclid(200) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigcat::RigModel;

    fn duo() -> Box<dyn Protocol> {
        RigModel::FdmDuo.protocol()
    }

    fn texts(v: &[Frame]) -> Vec<String> {
        v.iter().map(|f| String::from_utf8(f.bytes.clone()).unwrap()).collect()
    }

    fn rx(mode: &'static str) -> RigStatus {
        RigStatus {
            connected: true,
            freq_hz: Some(14_074_000),
            mode: Some(mode),
            tx_state: Some(0),
            vfo: Some(0),
            ..RigStatus::default()
        }
    }

    #[test]
    fn a_wheel_notch_is_one_step_and_a_trackpad_adds_up() {
        let mut acc = 0.0;
        assert_eq!(wheel_to_steps(WheelUnit::Line, 1.0, &mut acc), 1);
        assert_eq!(wheel_to_steps(WheelUnit::Line, -1.0, &mut acc), -1);
        assert_eq!(wheel_to_steps(WheelUnit::Line, 3.0, &mut acc), 3);
        // Points: 40 to a step, the rest kept.
        let mut acc = 0.0;
        assert_eq!(wheel_to_steps(WheelUnit::Point, 25.0, &mut acc), 0);
        assert_eq!(wheel_to_steps(WheelUnit::Point, 25.0, &mut acc), 1);
        assert_eq!(wheel_to_steps(WheelUnit::Point, 30.0, &mut acc), 1);
        // Turning back starts afresh, not by paying off what was left.
        assert_eq!(wheel_to_steps(WheelUnit::Point, -20.0, &mut acc), 0);
        assert_eq!(wheel_to_steps(WheelUnit::Point, -20.0, &mut acc), -1);
        let mut acc = 0.0;
        assert_eq!(wheel_to_steps(WheelUnit::Page, -1.0, &mut acc), -5);
    }

    #[test]
    fn dragging_round_is_24_steps_a_turn_clockwise_up() {
        let mut acc = 0.0;
        let quarter = std::f32::consts::FRAC_PI_2;
        assert_eq!(angle_to_steps(quarter, &mut acc), 6);
        assert_eq!(angle_to_steps(-quarter, &mut acc), -6);
        // Across the ±π seam the short way round counts.
        assert_eq!(angle_to_steps(std::f32::consts::TAU - DRAG_STEP_RAD, &mut acc), -1);
        let mut acc = 0.0;
        let mut n = 0;
        for _ in 0..10 {
            n += angle_to_steps(DRAG_STEP_RAD / 4.0, &mut acc);
        }
        assert_eq!(n, 2);
    }

    #[test]
    fn a_short_press_is_a_push_and_a_long_one_a_hold() {
        let hold = 1.0;
        let mut t = PressTracker::default();
        assert_eq!(t.update(true, 0.0, hold), None);
        assert!(t.waiting());
        assert_eq!(t.update(true, 0.5, hold), None);
        assert_eq!(t.update(false, 0.9, hold), Some(Press::Push));
        assert!(!t.waiting());
        // Held: fires at the hold time while still down, and the release
        // is no push.
        assert_eq!(t.update(true, 2.0, hold), None);
        assert_eq!(t.update(true, 2.99, hold), None);
        assert_eq!(t.update(true, 3.0, hold), Some(Press::Hold));
        assert_eq!(t.update(true, 4.0, hold), None);
        assert_eq!(t.update(false, 4.1, hold), None);
        // A press that turned the knob is neither.
        t.update(true, 5.0, hold);
        t.cancel();
        assert_eq!(t.update(true, 7.0, hold), None);
        assert_eq!(t.update(false, 7.1, hold), None);
        // Nothing without a press.
        assert_eq!(t.update(false, 8.0, hold), None);
    }

    #[test]
    fn the_hold_time_is_the_radios_menu_71() {
        let mut s = rx("USB");
        assert_eq!(hold_s(&s), 1.0);
        s.values.insert("HT".into(), "0600".into());
        assert_eq!(hold_s(&s), 0.6);
        s.values.insert("HT".into(), "9000".into());
        assert_eq!(hold_s(&s), 2.5);
    }

    #[test]
    fn step_mode_walks_the_dials_digits() {
        assert_eq!(next_step(100, 1), 1000);
        assert_eq!(next_step(100, -2), 1);
        assert_eq!(next_step(1, -1), 1);
        assert_eq!(next_step(1_000_000, 1), 1_000_000);
        assert_eq!(step_text(100), "100 Hz");
        assert_eq!(step_text(10_000), "10 kHz");
        assert_eq!(step_text(1_000_000), "1 MHz");
        assert_eq!(digit_of(100, 8), Some(5));
        assert_eq!(digit_of(250, 8), None);
    }

    #[test]
    fn e1_and_e2_step_through_the_radios_lists() {
        assert_eq!(E1::Vol.next(), E1::Sql);
        assert_eq!(E1::Nb.next(), E1::Vol);
        let mut s = rx("USB");
        // No pitch outside CW, no RIT while it is off.
        assert_eq!(E2::Filter.next(&s), E2::Filter);
        s.mode = Some("CW");
        assert_eq!(E2::Filter.next(&s), E2::Pitch);
        assert_eq!(E2::Pitch.next(&s), E2::Filter);
        s.rit_on = Some(true);
        assert_eq!(E2::Pitch.next(&s), E2::Rit);
        assert_eq!(E2::Rit.next(&s), E2::Filter);
        s.mode = Some("USB");
        assert_eq!(E2::Pitch.now(&s), E2::Filter);
    }

    #[test]
    fn e1_turns_send_the_radios_own_commands() {
        let d = duo();
        let mut s = rx("USB");
        s.values.insert("VM".into(), "035".into());
        s.values.insert("NR".into(), "0".into());
        s.values.insert("NC".into(), "000".into());
        s.values.insert("GC".into(), "0".into());
        assert_eq!(e1_value(&*d, &s, E1::Vol), Some(35));
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Vol, 35, 37).unwrap()), ["VM037;"]);
        assert_eq!(e1_value(&*d, &s, E1::Nr), Some(0));
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Nr, 0, 1).unwrap()), ["NR1;", "NC001;"]);
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Nr, 3, 4).unwrap()), ["NC004;"]);
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Nr, 1, 0).unwrap()), ["NR0;"]);
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Nb, 0, 2).unwrap()), ["NB1;", "NK002;"]);
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Agc, 0, 1).unwrap()), ["GC1;"]);
        assert_eq!(texts(&e1_frames(&*d, &s, E1::Sql, 0, 3).unwrap()), ["SQ003;"]);
        assert!(e1_frames(&*d, &s, E1::Vol, 5, 5).unwrap().is_empty());
        assert_eq!(e1_text(E1::Agc, Some(1)), "MAN GAIN");
        assert_eq!(e1_text(E1::Nr, Some(0)), "NR OFF");
        assert_eq!(e1_text(E1::Vol, None), "VOL —");
    }

    #[test]
    fn e2_steps_the_modes_filters_and_main_steps_memories() {
        let d = duo();
        let s = rx("CW");
        let w = widths(&*d, &s);
        assert!(w.windows(2).all(|p| p[0] < p[1]));
        assert_eq!(next_width(&w, 500, 1), Some(1000));
        assert_eq!(next_width(&w, 500, -1), Some(300));
        assert_eq!(next_width(&w, 2600, 5), Some(2600));
        assert_eq!(next_width(&[], 500, 1), None);
        assert_eq!(next_memory(0, -1), 199);
        assert_eq!(next_memory(199, 1), 0);
        assert_eq!(next_memory(10, 3), 13);
    }
}
