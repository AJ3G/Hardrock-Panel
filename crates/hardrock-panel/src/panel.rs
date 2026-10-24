//! The AMP window: the station's Hardrock amplifier, laid out after HobbyPCB's
//! own control panel but wider: two cross-needle meters side by side, then
//! the readings, the controls, the amplifier's performance and the tuner's
//! network running full width underneath, all in this program's own chrome.
//! The window can be dragged wider or narrower and everything in it scales.
//!
//! Everything here is drawn from [`RadioEvent::AmpStatus`] and every button is
//! a [`Command::Amp`], so the window is the same whether the amplifier is on
//! this machine's port or on a station across the internet.
//!
//! [`RadioEvent::AmpStatus`]: hardrock::RadioEvent::AmpStatus

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke, Vec2};
use hardrock::{
    AmpAction, AmpGauge, AmpLevel, AmpLimits, AmpModel, AmpReadings, AmpStatus, AtuNetwork,
    AtuReadout, amp_band_label,
};

use crate::{chrome, theme};

mod amptheme;
mod tft;

pub(crate) use amptheme::{AmpTheme, amp_theme};

/// What the two meters show, and what the window remembers from one frame to
/// the next to show it: the bottom scales' ranges and the performance figures'
/// peak holds. The needles and readings themselves are always live.
#[derive(Clone, Debug)]
pub(crate) struct AmpMeters {
    pairs: [Pair; 2],
    /// Each meter's bottom scale as a multiple of its full scale: other than 1
    /// while it is rescaled to keep its needle clear of the top one.
    zoom: [f32; 2],
    /// The range FWD and RFL are on, and what decides it (see [`PowerRange`]).
    range: RangeWatch,
    perf: Perf,
    /// Which bypass readings came from the radio this frame (see [`from_rig`]).
    rig: RigFill,
    /// The SWR at which the panel warns: the transmitter's tune limit (see
    /// [`swr_past`]).
    swr_warn: f32,
    /// The look the window is drawn in: `None` for the panel's own, or a
    /// radio maker's (see [`amp_theme`]).
    look: Option<&'static AmpTheme>,
    /// The radio chosen under SETTINGS and whether the amplifier's band
    /// follows it, for the status line of a radio maker's look.
    radio: Option<(&'static str, bool)>,
}

impl Default for AmpMeters {
    fn default() -> Self {
        AmpMeters {
            // DRV under FWD on the left (AJ3G, 18 Oct 2026), so the power
            // meter shows the radio's power going in beside the amplifier's
            // coming out; RFL moves under IDD.
            pairs: DEFAULT_PAIRS,
            zoom: [1.0; 2],
            range: RangeWatch::default(),
            perf: Perf::default(),
            rig: RigFill::default(),
            swr_warn: f32::INFINITY,
            look: None,
            radio: None,
        }
    }
}

/// The built-in picks: FWD over DRV on the left, IDD over RFL on the right.
const DEFAULT_PAIRS: [Pair; 2] = [
    Pair { top: AmpMeter::Fwd, bottom: Some(AmpMeter::Drv) },
    Pair { top: AmpMeter::Idd, bottom: Some(AmpMeter::Rfl) },
];

/// The built-in picks before DRV moved to the left meter, as
/// [`AmpMeters::saved`] wrote them: FWD over RFL, IDD over DRV. A file holding
/// exactly these never chose them, so it takes the new built-in picks.
const OLD_DEFAULT_SAVED: [u8; 4] = [0, 1, 3, 4];

/// Where FWD, RFL and SWR came from when they weren't the amplifier's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RigFill {
    /// FWD: `Some("rig")` measured by the radio, `Some("flagged")` a radio
    /// that measures watts but has flagged this reading, `Some("DRV")` the
    /// amplifier's drive reading.
    pub(crate) fwd: Option<&'static str>,
    /// RFL: `Some("rig")` measured by the radio, `Some("flagged")` as for
    /// FWD, `Some("SWR")` worked out from FWD and the radio's SWR.
    rfl: Option<&'static str>,
    /// SWR measured by the radio.
    swr: bool,
}

impl RigFill {
    fn any(self) -> bool {
        self.fwd.is_some() || self.rfl.is_some() || self.swr
    }
}

/// What a radio measures of its own transmission. The standalone panel has no
/// radio connected, so it passes these all empty, which still lets FWD fall
/// back to the amplifier's DRV in bypass (decision 2 in
/// `docs/design-decisions.md`). Kept so a radio link can fill them in later.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TxMeters {
    pub fwd_w: Option<f32>,
    pub swr: Option<f32>,
    pub alc: f32,
    pub po: Option<f32>,
    pub rfl_w: Option<f32>,
}

/// In OFF (bypass) only: FWD, RFL and SWR from the radio, since the
/// amplifier reports none of them then (AJ3G, 18 Oct: ANT 1 at 3:1 showed
/// nothing). In PTT the amplifier's own bridge is used, unchanged.
///
/// - SWR: what the radio measures over CAT, where it does.
/// - FWD: the watts the radio measures, where it reports watts. Otherwise
///   the amplifier's DRV, which measures the radio's power at the amplifier's
///   input; in bypass that power goes straight through. A needle position
///   (`po`) is not turned into watts: that would invent a calibration.
/// - RFL: the reflected watts the radio measures, alongside its forward watts
///   (the FDM-DUO's `RP`). Otherwise worked out as FWD × |Γ|², with
///   |Γ| = (SWR − 1) / (SWR + 1).
///
/// A radio that measures its own watts (`rig_watts`, the FDM-DUO with its Rig
/// control port) never borrows DRV. When it flags a reading as not to be
/// trusted, FWD and RFL are left empty and marked so: AJ3G, 18 Oct 2026,
/// keyed into an open, the DUO folded back to 2.94 W on the LP-100A while
/// DRV, which over-reads into a mismatch, said almost 10 W.
///
/// Any reading the amplifier does give in OFF is kept: only a zero is filled.
/// `tx` is the radio's meters, present only while it transmits.
pub(crate) fn from_rig(
    r: AmpReadings,
    tx: Option<&TxMeters>,
    rig_watts: bool,
) -> (AmpReadings, RigFill) {
    let mut fill = RigFill::default();
    if r.ptt {
        return (r, fill);
    }
    let Some(tx) = tx else { return (r, fill) };
    let mut out = r;
    if r.swr < 1.0
        && let Some(swr) = tx.swr.filter(|&v| v >= 1.0 && v.is_finite())
    {
        out.swr = swr;
        fill.swr = true;
    }
    if r.fwd_w < 1.0 {
        if let Some(w) = tx.fwd_w.filter(|&w| w > 0.0 && w.is_finite()) {
            out.fwd_w = w;
            fill.fwd = Some("rig");
        } else if rig_watts {
            fill.fwd = Some("flagged");
            fill.rfl = Some("flagged");
            return (out, fill);
        } else if r.drive_w > 0.0 {
            out.fwd_w = r.drive_w;
            fill.fwd = Some("DRV");
        }
    }
    if r.rfl_w < 1.0
        && fill.fwd == Some("rig")
        && let Some(w) = tx.rfl_w.filter(|&w| w >= 0.0 && w.is_finite())
    {
        // Measured by the radio, alongside the forward power it came with.
        out.rfl_w = w;
        fill.rfl = Some("rig");
    } else if r.rfl_w < 1.0 && out.swr >= 1.0 && out.fwd_w > 0.0 && fill.any() {
        let g = (out.swr - 1.0) / (out.swr + 1.0);
        out.rfl_w = out.fwd_w * g * g;
        fill.rfl = Some("SWR");
    } else if r.rfl_w < 1.0 && rig_watts && fill.fwd == Some("rig") {
        // Forward power given but the reflected flagged, and no SWR to work
        // it out from: said, rather than left blank with no reason.
        fill.rfl = Some("flagged");
    }
    (out, fill)
}

/// The range FWD and RFL are shown on (AJ3G, 18 Oct). Every other reading
/// keeps its full scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PowerRange {
    /// The amplifier's own full scales. Where the meters start, and where they
    /// stay until power is seen: the amplifier is taken to be in line and
    /// ready to make full power.
    #[default]
    Full,
    /// 0 to 100 W: a 100 W radio running through the amplifier in OFF
    /// (bypass).
    Hundred,
    /// 0 to 10 W: low power, in line or a QRP radio in bypass.
    Ten,
}

/// Power at or under this, held for [`LOW_HOLD`] while keyed, puts FWD and RFL
/// on the 10 W range.
const LOW_W: f32 = 10.0;
// 2.5 s, from 5 s (AJ3G, 18 Oct 2026): quicker to settle on the low range.
const LOW_HOLD: f64 = 2.5;
/// In bypass, power over [`LOW_W`] for this long in all, or in this many
/// separate swings, puts them on the 100 W range. Counts start again after
/// [`SWING_FORGET`] with none.
const OVER_HOLD: f64 = 2.0;
const OVER_SWINGS: u8 = 2;
const SWING_FORGET: f64 = 10.0;
/// Over this, the full scales at once, in line or not.
const HUNDRED_W: f32 = 100.0;

/// Watches the power to choose FWD's and RFL's range. Going up to the full
/// scales is immediate, so a needle is never pinned at real power for long;
/// coming down waits for the power to settle. Unkeyed, the range stays as it
/// was, so the meters don't change between overs.
#[derive(Clone, Copy, Debug, Default)]
struct RangeWatch {
    range: PowerRange,
    /// Since when the power has been at or under [`LOW_W`] while keyed.
    low_since: Option<f64>,
    /// In bypass: seconds over [`LOW_W`] so far, swings over it, whether the
    /// last reading was over, and when one last was.
    over_s: f64,
    swings: u8,
    was_over: bool,
    over_at: Option<f64>,
    last_t: Option<f64>,
}

impl RangeWatch {
    fn update(&mut self, now: f64, live: bool, r: &AmpReadings) {
        if !live {
            return;
        }
        let dt = self.last_t.map_or(0.0, |t| (now - t).clamp(0.0, 0.5));
        self.last_t = Some(now);
        // In OFF (bypass) the radio's own power goes straight through; the
        // drive reading is that power if the amp's FWD doesn't see it.
        let bypass = !r.ptt;
        let power = if bypass { r.fwd_w.max(r.drive_w) } else { r.fwd_w };
        if power > HUNDRED_W || (power > LOW_W && !bypass) {
            self.set(PowerRange::Full);
        } else if power > LOW_W {
            // A 100 W radio in bypass: up to 100 W once it has been over 10 W
            // for a couple of seconds or swung over twice.
            self.low_since = None;
            if self.range != PowerRange::Hundred {
                if !self.was_over {
                    self.swings += 1;
                }
                self.over_s += dt;
                self.over_at = Some(now);
                if self.over_s >= OVER_HOLD || self.swings >= OVER_SWINGS {
                    self.set(PowerRange::Hundred);
                }
            }
            self.was_over = true;
            return;
        } else if rf_present(r) {
            let since = *self.low_since.get_or_insert(now);
            if now - since >= LOW_HOLD {
                self.set(PowerRange::Ten);
            }
        } else {
            self.low_since = None;
        }
        self.was_over = false;
        if self.over_at.is_some_and(|t| now - t > SWING_FORGET) {
            self.forget_swings();
        }
    }

    fn set(&mut self, range: PowerRange) {
        self.range = range;
        self.low_since = None;
        self.forget_swings();
    }

    fn forget_swings(&mut self) {
        self.over_s = 0.0;
        self.swings = 0;
        self.over_at = None;
    }
}

impl AmpMeters {
    /// Which bypass readings were filled in this frame (see [`from_rig`]).
    pub(crate) fn set_rig(&mut self, fill: RigFill) {
        self.rig = fill;
    }

    /// The SWR at which the panel warns (see [`swr_past`]).
    pub(crate) fn set_swr_warn(&mut self, swr: f32) {
        self.swr_warn = swr;
    }

    /// The radio chosen under SETTINGS: it picks the window's look (see
    /// [`amp_theme`]) and names where the band comes from.
    pub(crate) fn set_radio(&mut self, rig: &rigcat::RigConfig, look: crate::theme::StationLook) {
        self.look = amp_theme(look);
        self.radio = rig.enabled.then(|| (rig.model.label(), rig.follow_band));
    }

    /// The meter picks as the settings keep them (see
    /// [`hardrock::UiSettings::amp_meters`]).
    pub(crate) fn saved(&self) -> [u8; 4] {
        let code = |m: AmpMeter| AmpMeter::ALL.iter().position(|&a| a == m).unwrap_or(0) as u8;
        let [l, r] = self.pairs;
        let bottom = |b: Option<AmpMeter>| b.map_or(NO_BOTTOM, code);
        [code(l.top), bottom(l.bottom), code(r.top), bottom(r.bottom)]
    }

    /// Back to the picks [`AmpMeters::saved`] gave. Anything not understood
    /// (a file from a later version) leaves that meter as it is.
    pub(crate) fn restore(&mut self, saved: [u8; 4]) {
        if saved == OLD_DEFAULT_SAVED {
            self.pairs = DEFAULT_PAIRS;
            return;
        }
        let meter = |c: u8| AmpMeter::ALL.get(usize::from(c)).copied();
        for (pair, half) in self.pairs.iter_mut().zip(saved.chunks(2)) {
            let Some(top) = meter(half[0]) else { continue };
            let bottom = match half[1] {
                NO_BOTTOM => None,
                c => match meter(c) {
                    Some(m) if m != top => Some(m),
                    _ => continue,
                },
            };
            *pair = Pair { top, bottom };
        }
    }
}

/// A meter with no bottom needle, in [`AmpMeters::saved`].
const NO_BOTTOM: u8 = 255;

/// One meter's two needles: the top one on the outer scale and an optional
/// bottom one on its own inner scale, as on a cross-needle meter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pair {
    top: AmpMeter,
    bottom: Option<AmpMeter>,
}

impl Pair {
    /// Puts a reading on the top needle; if the bottom one had it, they swap.
    fn set_top(&mut self, m: AmpMeter) {
        if self.bottom == Some(m) {
            self.bottom = Some(self.top);
        }
        self.top = m;
    }

    /// Puts a reading on the bottom needle, or takes it off; picking the top
    /// needle's reading swaps the two.
    fn set_bottom(&mut self, m: Option<AmpMeter>) {
        match (m, self.bottom) {
            (Some(m), Some(b)) if m == self.top => {
                self.top = b;
                self.bottom = Some(m);
            }
            (Some(m), None) if m == self.top => {}
            _ => self.bottom = m,
        }
    }
}

/// One reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AmpMeter {
    #[default]
    Fwd,
    Rfl,
    Vdd,
    Idd,
    Drv,
}

impl AmpMeter {
    const ALL: [AmpMeter; 5] =
        [AmpMeter::Fwd, AmpMeter::Rfl, AmpMeter::Vdd, AmpMeter::Idd, AmpMeter::Drv];

    /// The full scale on a power range other than the full one, for the
    /// readings that have one. RFL's is a fifth of FWD's, which covers up to
    /// about 2.4:1. DRV's is FWD's, so barefoot it reads on the same 10 W or
    /// 100 W scale (AJ3G, 18 Oct 2026); its yellow and red, past the end of a
    /// 10 W scale, are marked beyond the arc.
    fn range_full(self, range: PowerRange) -> Option<f32> {
        let fwd = match range {
            PowerRange::Full => return None,
            PowerRange::Hundred => HUNDRED_W,
            PowerRange::Ten => LOW_W,
        };
        match self {
            AmpMeter::Fwd | AmpMeter::Drv => Some(fwd),
            AmpMeter::Rfl => Some(fwd / 5.0),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            AmpMeter::Fwd => "FWD",
            AmpMeter::Rfl => "RFL",
            AmpMeter::Vdd => "VDD",
            AmpMeter::Idd => "IDD",
            AmpMeter::Drv => "DRV",
        }
    }

    /// The reading, the meter's full scale, and where the scale turns yellow
    /// and red, as fractions of full scale.
    fn read(self, r: &AmpReadings, l: &AmpLimits) -> (f32, f32, Zones) {
        let up = |warn: f32, trip: f32, full: f32| Zones::Above(warn / full, trip / full);
        match self {
            AmpMeter::Fwd => (r.fwd_w, l.fwd_w, up(l.fwd_warn_w, l.fwd_trip_w, l.fwd_w)),
            AmpMeter::Rfl => (r.rfl_w, l.rfl_w, up(l.rfl_warn_w, l.rfl_trip_w, l.rfl_w)),
            AmpMeter::Vdd => (r.vdd_v, l.vdd_v, Zones::Below(l.vdd_low_v / l.vdd_v)),
            AmpMeter::Idd => (r.idd_a, l.idd_a, up(l.idd_warn_a, l.idd_trip_a, l.idd_a)),
            AmpMeter::Drv => (r.drive_w, l.drive_w, up(l.drive_warn_w, l.drive_trip_w, l.drive_w)),
        }
    }

    fn text(self, r: &AmpReadings) -> String {
        match self {
            AmpMeter::Fwd => watts(r.fwd_w),
            AmpMeter::Rfl => watts(r.rfl_w),
            AmpMeter::Vdd => format!("{:.1} V", r.vdd_v),
            AmpMeter::Idd => format!("{:.1} A", r.idd_a),
            AmpMeter::Drv => format!("{:.1} W", r.drive_w),
        }
    }
}

/// The legend's word on a meter's scale. RFL is on a fifth of FWD's range, so
/// it names its own full scale, not the range's.
fn scale_note(m: AmpMeter, ranged: Option<PowerRange>, rescaled: bool) -> &'static str {
    let rfl = m == AmpMeter::Rfl;
    match (ranged, rfl, rescaled) {
        (Some(PowerRange::Ten), false, true) => ", 10 W scale, rescaled",
        (Some(PowerRange::Ten), false, false) => ", 10 W scale",
        (Some(PowerRange::Ten), true, true) => ", 2 W scale, rescaled",
        (Some(PowerRange::Ten), true, false) => ", 2 W scale",
        (Some(PowerRange::Hundred), false, true) => ", 100 W scale, rescaled",
        (Some(PowerRange::Hundred), false, false) => ", 100 W scale",
        (Some(PowerRange::Hundred), true, true) => ", 20 W scale, rescaled",
        (Some(PowerRange::Hundred), true, false) => ", 20 W scale",
        (_, _, true) => ", rescaled",
        (_, _, false) => "",
    }
}

/// Whether the SWR is at or past the tune limit. There the SWR tile is red,
/// and a radio's own watts are marked unreliable: into an open the FDM-DUO
/// reported 7.75 W forward and 6–7 W reflected while the LP-100A read 2.62 W
/// forward (AJ3G, 18 Oct 2026). Its SWR, 22.5:1, was the honest figure.
fn swr_past(r: &AmpReadings, warn: f32) -> bool {
    r.swr >= 1.0 && r.swr >= warn
}

/// The legend's font size: 12 points (scaled) when a legend `width` wide at
/// that size fits the `room` left of the needles, smaller in proportion when it
/// doesn't, down to 8.
fn legend_size(width: f32, room: f32, s: f32) -> f32 {
    let full = 12.0 * s;
    if width <= room || width <= 0.0 { full } else { (full * room / width).max(8.0 * s) }
}

/// The warning across the top of a meter whose radio watts can't be trusted
/// at this SWR, naming them: "FWD UNRELIABLE AT THIS SWR".
fn unsure_warning(top: &Dial, bottom: Option<&Dial>) -> Option<String> {
    let names: Vec<&str> =
        std::iter::once(top).chain(bottom).filter(|d| d.unsure).map(|d| d.name).collect();
    (!names.is_empty()).then(|| format!("{} UNRELIABLE AT THIS SWR", names.join(" AND ")))
}

/// Power in whole watts, or tenths below 10 W: barefoot, a whole watt is a
/// fifth of the reading, and half a watt reflected would show as "1 W".
fn watts(w: f32) -> String {
    if w < 9.95 { format!("{w:.1} W") } else { format!("{w:.0} W") }
}

/// Where a meter's scale is painted yellow and red.
#[derive(Clone, Copy)]
enum Zones {
    /// Yellow from the first fraction up, red (trip) from the second.
    Above(f32, f32),
    /// Yellow below the fraction: the supply, which never trips.
    Below(f32),
}

impl Zones {
    /// The same zones on a scale `zoom` times the usual one.
    fn zoomed(self, zoom: f32) -> Zones {
        match self {
            Zones::Above(w, t) => Zones::Above(w / zoom, t / zoom),
            Zones::Below(l) => Zones::Below(l / zoom),
        }
    }
}

/// A peak-reading hold, as on an SSB wattmeter: the highest figure stays put
/// for a moment, then falls away slowly, so a voice peak can be read.
#[derive(Clone, Copy, Debug, Default)]
struct Peak {
    value: f32,
    at: f64,
}

impl Peak {
    const HOLD_S: f64 = 1.0;
    /// How fast it falls once the hold is over, as a fraction of the peak a
    /// second: every figure, whatever its scale and wherever it sits on it,
    /// reaches nothing in the same time.
    const FALL: f32 = 0.33;

    fn get(&self, now: f64) -> f32 {
        let t = (now - self.at - Self::HOLD_S).max(0.0) as f32;
        (self.value * (1.0 - t * Self::FALL)).max(0.0)
    }

    /// Takes a new reading and returns what the hold now shows.
    fn feed(&mut self, v: f32, now: f64) -> f32 {
        let held = self.get(now);
        if v >= held {
            *self = Peak { value: v, at: now };
            v
        } else {
            held
        }
    }
}

/// Input power, efficiency and gain: the live figures, and each one's peak
/// hold, which is what the window shows as the number.
#[derive(Clone, Copy, Debug, Default)]
struct Perf {
    live: Option<[f32; 3]>,
    peaks: [Peak; 3],
}

impl Perf {
    fn feed(&mut self, now: f64, x: Option<[f32; 3]>) {
        self.live = x;
        for (i, p) in self.peaks.iter_mut().enumerate() {
            p.feed(x.map_or(0.0, |a| a[i]), now);
        }
    }

    /// Whether any hold is still showing something, so still to be redrawn.
    fn holding(&self, now: f64) -> bool {
        self.peaks.iter().any(|p| p.get(now) > 0.0)
    }
}

/// Input power (W), efficiency (%) and gain (dB) from one set of readings, or
/// `None` unless the amplifier is transmitting with enough going in and out to
/// measure: a figure worked out from nothing would only mislead.
fn performance(r: &AmpReadings) -> Option<[f32; 3]> {
    if !(r.ptt && r.fwd_w >= 5.0 && r.drive_w >= 0.1 && r.vdd_v >= 1.0 && r.idd_a >= 0.2) {
        return None;
    }
    let input = r.vdd_v * r.idd_a;
    Some([input, (r.fwd_w / input * 100.0).min(100.0), 10.0 * (r.fwd_w / r.drive_w).log10()])
}

/// Why a relay button is greyed: shown to the operator on hover. See
/// docs/hardrock/safety-interlocks.md.
const NOT_WHILE_TX: &str =
    "Not while transmitting: switching a relay with RF on it can damage it. Unkey first.";

/// Whether there is RF in the amplifier now, going in or coming out: while
/// there is, no relay in its path may be switched (safety interlock #2).
/// Drive or output rather than the PTT state, because RF is what does the
/// damage, and a reading of either is enough.
fn rf_present(r: &AmpReadings) -> bool {
    r.fwd_w >= 1.0 || r.drive_w >= 0.1
}

/// The performance bars' full scales: the most the supply can deliver, 100 %
/// and 30 dB.
fn perf_full(l: &AmpLimits) -> [f32; 3] {
    [l.vdd_v * l.idd_a, 100.0, 30.0]
}

/// The bottom scale's ranges, as multiples of its usual full scale, in order
/// of preference.
const ZOOMS: [f32; 4] = [1.0, 0.5, 0.2, 2.0];
/// Needles closer than this, as a fraction of the sweep, read as one.
const CLOSE: f32 = 0.04;

/// The bottom scale's range, so its needle stays clear of the top one: the
/// usual range unless the two would lie on top of each other, then whichever
/// range parts them, back to the usual one once they are well apart. The
/// margin between "overlapping" and "well apart" stops it flicking to and fro.
fn rescale(zoom: f32, top_frac: f32, bottom: f32, bottom_full: f32) -> f32 {
    if bottom <= 0.0 || top_frac <= 0.0 {
        return 1.0;
    }
    let frac = |z: f32| bottom / (bottom_full * z);
    let clear = |z: f32, gap: f32| frac(z) <= 1.0 && (frac(z) - top_frac).abs() >= gap;
    if clear(1.0, 2.0 * CLOSE) {
        1.0
    } else if clear(zoom, CLOSE) {
        zoom
    } else if let Some(z) = ZOOMS.into_iter().find(|&z| clear(z, 2.0 * CLOSE)) {
        z
    } else if frac(zoom) <= 1.0 {
        zoom
    } else {
        1.0
    }
}

/// The amplifier's own colours for a reading.
fn level_col(l: AmpLevel) -> Color32 {
    match l {
        AmpLevel::Green => theme::GREEN(),
        AmpLevel::Yellow => theme::YELLOW(),
        AmpLevel::Red => theme::ALERT(),
    }
}

/// The top needle's colour and the bottom one's: every name and figure
/// belonging to a needle is written in it.
fn top_ink() -> Color32 {
    theme::TEXT_STRONG()
}
fn bottom_ink() -> Color32 {
    theme::CYAN()
}

/// A button sized for the window's scale `s`, filled with `fill` while
/// `selected` (or the ordinary selected look when there is no fill).
fn button(
    ui: &mut egui::Ui,
    s: f32,
    selected: bool,
    text: &str,
    fill: Option<Color32>,
) -> egui::Response {
    let pt = 13.0 * s;
    let size = Vec2::new(chrome::chip_width(ui, text, Some(pt)), chrome::chip_height(ui, Some(pt)));
    let rt = RichText::new(text).size(pt);
    let resp = match fill {
        Some(f) => chrome::chip_accent_sized(ui, selected, rt, f, theme::INK_ON_BRIGHT(), size),
        None => chrome::chip_sized(ui, selected, rt, size),
    };
    clickable(ui, &resp, s)
}

/// Anything that can be clicked must look it (AJ3G): a hand pointer and a
/// bright outline under the mouse, on top of whatever the widget draws.
fn clickable(ui: &egui::Ui, resp: &egui::Response, s: f32) -> egui::Response {
    if resp.enabled() && resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect.expand(1.0),
            3.0,
            egui::Stroke::new(1.5 * s, theme::TEXT_STRONG()),
            egui::StrokeKind::Outside,
        );
    }
    resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A folded corner on a control that changes something but would otherwise
/// read as a plain light or reading, so it is plain at a glance that it can
/// be clicked.
fn corner_mark(p: &egui::Painter, rect: egui::Rect, s: f32, col: Color32) {
    let k = 9.0 * s;
    let tr = rect.right_top();
    p.add(egui::Shape::convex_polygon(
        vec![tr, tr + Vec2::new(0.0, k), tr + Vec2::new(-k, 0.0)],
        col,
        egui::Stroke::NONE,
    ));
}

/// The panel in a window of its own: scaled to fit the window's width and
/// height both. The docked window has no height to fit; its height follows
/// its width.
///
/// In a window wider than the panel's proportions the boxes and rows span
/// up to [`MAX_STRETCH`] times the panel's own width, the text, dials and
/// drawing at the scale the height allows centred in them (AJ3G, 18 Oct);
/// wider than that, the panel keeps that width and is centred, so that it
/// still reads as the panel grown rather than as a strip of small text
/// across the screen (AJ3G, 21 Oct 2026: "Amp Panel do[es] not scale when
/// window is expanded").
pub(crate) fn fit_panel(
    ui: &mut egui::Ui,
    panel: impl FnOnce(&mut egui::Ui, f32) -> PanelOut,
) -> PanelOut {
    fit_scaled(ui, "sdroxide-amp", 720.0, (MIN_SCALE, MAX_SCALE), |ui, s| {
        let w = ui.available_width().min(720.0 * s * MAX_STRETCH);
        let left = ui.max_rect().left() + (ui.available_width() - w) / 2.0;
        let rect = Rect::from_min_size(
            Pos2::new(left, ui.cursor().top()),
            Vec2::new(w, ui.available_height()),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| panel(ui, s),
        )
        .inner
    })
}

/// How much wider than its own proportions the panel's boxes may stretch in
/// a wide window.
const MAX_STRETCH: f32 = 1.3;

/// Draw `contents` at the scale that fits the room there is: `base_w` wide
/// at scale 1, and as tall as it measured last time it was drawn, within
/// `range`. Scrolls only once the room is too small for the smallest scale.
/// `key` keeps each window's measurement apart.
pub(crate) fn fit_scaled<R>(
    ui: &mut egui::Ui,
    key: &'static str,
    base_w: f32,
    range: (f32, f32),
    contents: impl FnOnce(&mut egui::Ui, f32) -> R,
) -> R {
    // The contents' height at scale 1, measured as they were last drawn.
    // They aren't exactly proportional to the scale (some spacing is fixed),
    // so the scale is worked out from it only while the window's size is
    // changing and for a few frames after, then held. Re-fitting every frame
    // chased its own measurement and made the panel shake (AJ3G, 18 Oct).
    let nat_id = egui::Id::new((key, "natural-height"));
    let fit_id = egui::Id::new((key, "fit"));
    let natural: f32 = ui.data(|d| d.get_temp(nat_id)).unwrap_or(600.0);
    let avail = ui.available_size();
    let prev: Option<Fit> = ui.data(|d| d.get_temp(fit_id));
    let same = prev.filter(|f| (f.avail - avail).length() < 0.5);
    let s = match same {
        Some(f) if f.settle == 0 => f.s,
        // A little short of a full fit, so rounding never tips it into a
        // scroll bar.
        _ => fit_scale(avail, base_w, natural, range),
    };
    let settle = same.map_or(FIT_SETTLE, |f| f.settle.saturating_sub(1));
    ui.data_mut(|d| d.insert_temp(fit_id, Fit { avail, s, settle }));
    let out = egui::ScrollArea::vertical().id_salt(key).show(ui, |ui| contents(ui, s));
    let h = out.content_size.y;
    if h > 1.0 && settle > 0 {
        ui.data_mut(|d| d.insert_temp(nat_id, h / s));
    }
    out.inner
}

/// The scale at which contents `base_w` wide and `natural` tall at scale 1
/// fit `avail`, a little short so rounding never tips it into a scroll bar.
fn fit_scale(avail: Vec2, base_w: f32, natural: f32, (lo, hi): (f32, f32)) -> f32 {
    (avail.x / base_w).min(avail.y * 0.98 / natural.max(1.0)).clamp(lo, hi)
}

/// The popped-out panel's fit, kept from frame to frame: the size it was
/// fitted to, its scale, and how many more frames may refine it.
#[derive(Clone, Copy)]
struct Fit {
    avail: Vec2,
    s: f32,
    settle: u8,
}

/// Frames after a resize in which the fit is refined before it is held.
const FIT_SETTLE: u8 = 3;

/// A section caption, as [`chrome::menu_caption`] draws it but scaled with
/// the panel: unscaled, it stayed tiny in a large pop-out (AJ3G, 18 Oct).
fn caption(ui: &mut egui::Ui, s: f32, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).color(theme::CYAN_DIM()).size(9.5 * s).strong());
}

/// Smallest and largest scale the panel is drawn at.
const MIN_SCALE: f32 = 0.7;
const MAX_SCALE: f32 = 3.0;

/// What was asked for inside the window.
#[derive(Default)]
pub(crate) struct PanelOut {
    /// The amplifier's own buttons.
    pub actions: Vec<AmpAction>,
    /// The settings tab.
    pub settings: bool,
    /// Into a window of its own, or back into the main one.
    pub pop: bool,
}

/// The window's contents. `pop` is whether it is in a window of its own, or
/// `None` where it cannot be (the browser), which hides that button.
pub(crate) fn amp_panel(
    ui: &mut egui::Ui,
    st: &AmpStatus,
    view: &mut AmpMeters,
    pop: Option<bool>,
    scale: Option<f32>,
) -> PanelOut {
    let mut out = PanelOut::default();
    out.settings = amp_panel_inner(ui, st, view, pop, scale, &mut out);
    out
}

fn amp_panel_inner(
    ui: &mut egui::Ui,
    st: &AmpStatus,
    view: &mut AmpMeters,
    pop: Option<bool>,
    scale: Option<f32>,
    out: &mut PanelOut,
) -> bool {
    if !st.configured {
        ui.label(
            RichText::new("No amplifier is set up yet. Choose its port under SETTINGS.").weak(),
        );
        return chrome::chip(ui, false, "SETTINGS").clicked();
    }

    let limits = st.model.limits();
    let r = st.readings.unwrap_or_default();
    let live = st.connected && st.readings.is_some();
    let now = ui.input(|i| i.time);
    // Everything is drawn for a window 720 points wide and scaled from there:
    // by the width when docked, or as the pop-out fits it. Popped out in a
    // wide window, the boxes span `width` while the dials and the tuner's
    // drawing keep the height `nominal` gives them.
    let width = ui.available_width();
    let s = scale.unwrap_or(width / 720.0).clamp(MIN_SCALE, MAX_SCALE);
    let nominal = (720.0 * s).min(width);
    // The spacing between rows and boxes scales too, so the whole panel grows
    // in proportion and the pop-out can fit it to the window's height.
    ui.spacing_mut().item_spacing *= s;

    // ── what changed since the last frame ──────────────────────────────────
    let perf_now = performance(&r).filter(|_| live);
    view.perf.feed(now, perf_now);
    // Redrawn while a held peak is falling, between the amplifier's readings.
    if view.perf.holding(now) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
    }
    view.range.update(now, live, &r);
    // Blinking while the tuner tunes.
    if st.atu.as_ref().is_some_and(|a| a.tuning) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    let sh = Shown {
        st,
        r,
        limits,
        rig: view.rig,
        live,
        past: live && swr_past(&r, view.swr_warn),
        range: view.range.range,
        now,
        s,
        width,
        nominal,
        transmitting: perf_now.is_some(),
    };
    match view.look {
        Some(th) => tft::screen(ui, &sh, view, th, pop, out),
        None => panel_look(ui, &sh, view, pop, out),
    }
    false
}

/// What every look draws from, worked out once a frame.
pub(super) struct Shown<'a> {
    st: &'a AmpStatus,
    r: AmpReadings,
    limits: AmpLimits,
    /// Which bypass readings came from the radio (see [`from_rig`]).
    rig: RigFill,
    /// The amplifier is answering, with readings.
    live: bool,
    /// The SWR is at or past the warning (see [`swr_past`]).
    past: bool,
    range: PowerRange,
    now: f64,
    /// The window's scale, its width, and the width the dials and the
    /// tuner's drawing keep.
    s: f32,
    width: f32,
    nominal: f32,
    /// Transmitting with enough going in and out for the performance figures.
    transmitting: bool,
}

impl Shown<'_> {
    /// A reading's colour: its live colour or, if worse, the colour the
    /// amplifier is holding from the last transmission.
    fn level(&self, g: AmpGauge) -> Option<AmpLevel> {
        self.live.then(|| self.st.alerts.shown(&self.limits, g, &self.r))
    }

    /// The band to show, and whether it is one just asked for that the
    /// amplifier hasn't been read back on yet (drawn in blue).
    fn band(&self) -> (Option<u8>, bool) {
        let asked = self.st.pending.iter().find_map(|a| match *a {
            AmpAction::SetBand(c) => Some(c),
            // Not yet taken by the link: where the step will go.
            AmpAction::BandUp => Some(self.r.band.map_or(5, |c| c.saturating_sub(1).max(1))),
            AmpAction::BandDown => Some(self.r.band.map_or(5, |c| (c + 1).min(10))),
            _ => None,
        });
        match asked {
            Some(c) => (Some(c), true),
            None => (self.r.band, false),
        }
    }

    /// A reading's value and full scale, on the power range when it has one.
    fn read(&self, m: AmpMeter) -> (f32, f32, Zones, Option<PowerRange>) {
        let (value, full, zones) = m.read(&self.r, &self.limits);
        match m.range_full(self.range) {
            Some(rf) => (value, rf, zones.zoomed(rf / full), Some(self.range)),
            None => (value, full, zones, None),
        }
    }

    /// One needle, its scale `zoom` times the usual one.
    fn dial(&self, m: AmpMeter, zoom: f32) -> Dial {
        let (value, full, zones, ranged) = self.read(m);
        let rig = self.rig;
        Dial {
            name: m.label(),
            readout: self.live.then(|| m.text(&self.r)),
            value,
            full: full * zoom,
            zones: zones.zoomed(zoom),
            live: self.live,
            note: scale_note(m, ranged, zoom != 1.0),
            unsure: self.past
                && match m {
                    AmpMeter::Fwd => rig.fwd == Some("rig"),
                    AmpMeter::Rfl => rig.rfl == Some("rig"),
                    _ => false,
                },
            source: match m {
                AmpMeter::Fwd if rig.fwd == Some("rig") => ", from the rig",
                AmpMeter::Fwd if rig.fwd == Some("flagged") => ", the rig flagged its reading",
                AmpMeter::Rfl if rig.rfl == Some("flagged") => ", the rig flagged its reading",
                AmpMeter::Fwd if rig.fwd.is_some() => ", from DRV",
                AmpMeter::Rfl if rig.rfl == Some("rig") => ", from the rig",
                AmpMeter::Rfl if rig.rfl.is_some() => ", from the rig's SWR",
                _ => "",
            },
        }
    }

    /// Meter `i`'s two needles, its bottom scale rescaled first to keep its
    /// needle clear of the top one.
    fn dials(&self, view: &mut AmpMeters, i: usize) -> (Dial, Option<Dial>) {
        let pair = view.pairs[i];
        if let Some(b) = pair.bottom {
            let (tv, tf, _, _) = self.read(pair.top);
            let (bv, bf, _, _) = self.read(b);
            view.zoom[i] = if self.live { rescale(view.zoom[i], tv / tf, bv, bf) } else { 1.0 };
        } else {
            view.zoom[i] = 1.0;
        }
        (self.dial(pair.top, 1.0), pair.bottom.map(|b| self.dial(b, view.zoom[i])))
    }
}

/// The panel's own look: SDRoxide's AMP window.
fn panel_look(
    ui: &mut egui::Ui,
    sh: &Shown,
    view: &mut AmpMeters,
    pop: Option<bool>,
    out: &mut PanelOut,
) {
    let Shown { st, r, live, s, width, nominal, .. } = *sh;
    let gap = ui.spacing().item_spacing.x;
    let actions = &mut out.actions;

    ui.horizontal(|ui| {
        link_line(ui, st, s, sh.transmitting);
        if let Some(popped) = pop {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, hover) = pop_words(popped);
                if button(ui, s, false, text, None).on_hover_text(hover).clicked() {
                    out.pop = true;
                }
            });
        }
    });
    ui.add_space(4.0 * s);

    // ── the two meters ─────────────────────────────────────────────────────
    let meter_w = (width - 2.0 * gap) / 2.0;
    let meter_h = meter_w.min((nominal - 2.0 * gap) / 2.0) * 0.52;
    ui.horizontal(|ui| {
        for i in 0..2 {
            ui.vertical(|ui| {
                ui.set_width(meter_w);
                pickers(ui, i, &mut view.pairs[i], s, &PickInk::panel());
                ui.add_space(2.0 * s);
                let (top, bottom) = sh.dials(view, i);
                meter(ui, Vec2::new(meter_w, meter_h), s, &top, bottom);
            });
        }
    });
    ui.add_space(6.0 * s);

    // ── every reading, full width ──────────────────────────────────────────
    if readings_row(ui, sh) {
        actions.push(AmpAction::SetCelsius(r.celsius == Some(false)));
    }
    ui.add_space(6.0 * s);

    let keys = keys(sh);
    // A key in the panel's chips: lit keys filled in their tone.
    let chip = |ui: &mut egui::Ui, k: &Key| {
        let fill = match k.tone {
            Tone::Plain => None,
            Tone::Blue => Some(theme::CYAN()),
            Tone::Green => Some(theme::GREEN()),
            Tone::Yellow => Some(theme::YELLOW()),
            Tone::Red => Some(theme::ALERT()),
        };
        ui.add_enabled_ui(k.enabled, |ui| button(ui, s, k.lit, &k.text, fill))
            .inner
            .on_hover_text(k.hover)
            .on_disabled_hover_text(&k.why_not)
    };

    // ── mode, antenna, band ────────────────────────────────────────────────
    ui.horizontal(|ui| {
        let ptt = chip(ui, &keys.ptt);
        corner_mark(ui.painter(), ptt.rect, s, theme::INK_ON_BRIGHT());
        if ptt.clicked() {
            actions.push(keys.ptt.action);
        }
        if let Some(k) = &keys.ant
            && chip(ui, k).clicked()
        {
            actions.push(k.action);
        }
        ui.add_space(6.0 * s);
        if chip(ui, &keys.dn).clicked() {
            actions.push(keys.dn.action);
        }
        let (band, asked) = sh.band();
        ui.add_enabled_ui(live, |ui| {
            ui.label(
                RichText::new(band.map_or("UNK", amp_band_label))
                    .size(15.0 * s)
                    .strong()
                    .color(if asked { theme::CYAN() } else { theme::YELLOW() }),
            )
        });
        if chip(ui, &keys.up).clicked() {
            actions.push(keys.up.action);
        }
    });
    ui.add_space(8.0 * s);

    // ── performance ────────────────────────────────────────────────────────
    performance_row(ui, s, &view.perf, &sh.limits, sh.now);

    // ── the tuner ──────────────────────────────────────────────────────────
    if let Some(atu) = st.atu.as_ref() {
        ui.add_space(8.0 * s);
        caption(ui, s, "Tuner");
        let net = atu.network;
        let bypassed = atu_bypassed(atu);
        let h = (nominal * 0.21).max(110.0 * s);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, theme::BG_DEEP());
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::LINE()), egui::StrokeKind::Inside);
        // The drawing keeps its proportions: in a wide pop-out it is drawn at
        // the panel's scale, centred, rather than stretched across the box
        // (AJ3G, 18 Oct).
        let art = Rect::from_center_size(rect.center(), Vec2::new(nominal, rect.height()));
        let ink = NetInk::panel();
        if bypassed {
            bypass_drawing(&p, art, s, &ink);
        } else if let Some(n) = net {
            lc_network(&p, art, s, n, st.model, &ink);
        }

        // TUNE and ON/BYP sit in the drawing's bottom-left corner.
        let btn_h = chrome::chip_height(ui, Some(13.0 * s));
        let corner = Rect::from_min_size(
            Pos2::new(rect.left() + 12.0 * s, rect.bottom() - btn_h - 10.0 * s),
            Vec2::new(rect.width() * 0.4, btn_h),
        );
        let mut inside = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(corner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        for k in [&keys.tune, &keys.atu].into_iter().flatten() {
            if chip(&mut inside, k).clicked() {
                actions.push(k.action);
            }
        }

        if let Some((state, tone, swr)) = atu_words(atu) {
            ui.horizontal(|ui| {
                let col = match tone {
                    Tone::Red => theme::ALERT(),
                    Tone::Blue => theme::CYAN(),
                    _ => theme::GREEN(),
                };
                ui.label(RichText::new(state).size(14.0 * s).strong().color(col));
                if let Some(swr) = swr {
                    ui.label(RichText::new(swr).size(14.0 * s).color(theme::TEXT()));
                }
            });
        }
    }
}

/// POP OUT or DOCK, and what it does.
fn pop_words(popped: bool) -> (&'static str, &'static str) {
    if popped {
        ("DOCK", "Put the AMP window back inside the main window")
    } else {
        ("POP OUT", "Give the AMP window a window of its own, to move to another screen")
    }
}

/// How a key or a line is lit. Each look has its own colours for these;
/// yellow and red only ever mean a warning (AJ3G).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Plain,
    /// A neutral state: OFF, BYP, tuning.
    Blue,
    /// In line, tuned, connected.
    Green,
    Yellow,
    Red,
}

/// One of the amplifier's controls, as every look draws it. Whether it can
/// be pressed, and why not, is decided here once, so no look can loosen an
/// interlock.
#[derive(Clone, Debug)]
struct Key {
    text: String,
    /// Lit in its tone, rather than plain.
    lit: bool,
    tone: Tone,
    enabled: bool,
    hover: &'static str,
    /// Why it is greyed, shown on hover while it is.
    why_not: String,
    action: AmpAction,
}

/// The amplifier's six controls. ANT only where there are two antennas, and
/// TUNE and ON/BYP only with a tuner.
struct Keys {
    ptt: Key,
    ant: Option<Key>,
    dn: Key,
    up: Key,
    tune: Option<Key>,
    atu: Option<Key>,
}

/// Why a key is greyed while the amplifier isn't answering.
const NOT_CONNECTED: &str = "The amplifier isn't answering.";

/// The amplifier's controls as the readings allow them now.
fn keys(sh: &Shown) -> Keys {
    let Shown { st, r, limits, live, now, .. } = *sh;
    let off_line = || NOT_CONNECTED.to_string();
    // SAFETY INTERLOCK (docs/safety-interlocks.md, #4): no OFF to PTT while
    // the radio drives harder than the amplifier's drive limit, as a 100 W
    // radio running barefoot through the bypassed amp does. The link checks
    // again before sending.
    // A click not yet read back shows as asked, in the neutral blue, so the
    // button answers at once (AJ3G, 21 Oct 2026); the next click toggles
    // what it shows. The interlocks still go by the readings: shown OFF
    // while the amplifier reads PTT, the key is greyed overdriven as if it
    // read OFF.
    let asked = |c: u8| st.pending.iter().copied().find(|a| a.control() == Some(c));
    let shown_ptt = match asked(0) {
        Some(AmpAction::SetPtt(on)) => on,
        _ => r.ptt,
    };
    let overdriven = ptt_overdriven(&r, &limits)
        || ptt_overdriven(&AmpReadings { ptt: shown_ptt, ..r }, &limits);
    // Green when keyed with the radio, the program's neutral blue in standby:
    // lit either way so the state can be told at a glance, but never yellow
    // or red, which only ever mean a warning (AJ3G).
    let (mode, tone) = match (shown_ptt, asked(0).is_some()) {
        (true, false) => ("PTT", Tone::Green),
        (true, true) => ("PTT", Tone::Blue),
        (false, _) => ("OFF", Tone::Blue),
    };
    let ptt = Key {
        text: mode.into(),
        lit: true,
        tone,
        enabled: live && !overdriven,
        hover: "PTT keys the amplifier with the radio; OFF leaves it in bypass. Click to switch.",
        why_not: if overdriven {
            format!(
                "Lower the drive first: {:.0} W would overdrive the amplifier in line (its limit is {:.0} W).",
                r.drive_w, limits.drive_warn_w
            )
        } else {
            off_line()
        },
        action: AmpAction::SetPtt(!shown_ptt),
    };
    // SAFETY INTERLOCK (docs/safety-interlocks.md, #2): antenna, band and
    // tuner switch relays in the RF path, which must not be switched with RF
    // on them; see NOT_WHILE_TX and `rf_present`. Doing so made AJ3G's HR500
    // raise its RFL flag (17 Oct 2026); it stresses the output devices and
    // pits the contacts. The link checks again before sending (#1).
    let rf = rf_present(&r);
    let relay = |text: String, hover: &'static str, action: AmpAction| Key {
        text,
        lit: false,
        tone: Tone::Plain,
        enabled: live && !rf,
        hover,
        why_not: if rf { NOT_WHILE_TX.to_string() } else { off_line() },
        action,
    };
    let ant = (st.model.antennas() > 1).then(|| {
        let (n, pending) = match asked(1) {
            Some(AmpAction::SetAntenna(n)) => (n, true),
            _ => (r.antenna, false),
        };
        Key {
            lit: pending,
            tone: if pending { Tone::Blue } else { Tone::Plain },
            ..relay(
                format!("ANT {}", n.max(1)),
                "Switch antenna jack",
                AmpAction::SetAntenna(if n == 2 { 1 } else { 2 }),
            )
        }
    });
    let dn = relay("DN".into(), "Band down", AmpAction::BandDown);
    let up = relay("UP".into(), "Band up", AmpAction::BandUp);
    let tune = st.atu.as_ref().map(|atu| {
        // Blinking blue while tuning (busy, not a warning), green once a
        // tune has succeeded and the tuner is in line, red when the last one
        // failed (AJ3G). Not a relay switch: TUNE works on RF.
        let (failed, tuned) = atu_result(atu);
        let (lit, tone) = if atu.tuning {
            (now.rem_euclid(1.0) < 0.5, Tone::Blue)
        } else if tuned {
            (true, Tone::Green)
        } else {
            (failed, Tone::Red)
        };
        Key {
            text: "TUNE".into(),
            lit,
            tone,
            enabled: live,
            hover: "Start a tune — then key a steady carrier (CW, FM or RTTY). Press again to stop.",
            why_not: off_line(),
            action: AmpAction::Tune,
        }
    });
    let atu = st.atu.as_ref().map(|atu| {
        let (active, pending) = match asked(3) {
            Some(AmpAction::SetAtu(on)) => (on, true),
            _ => (atu.active, false),
        };
        let (text, tone) = match (active, pending) {
            (true, false) => ("ON", Tone::Green),
            (true, true) => ("ON", Tone::Blue),
            (false, _) => ("BYP", Tone::Blue),
        };
        Key {
            lit: true,
            tone,
            ..relay(text.into(), "Put the tuner in line, or bypass it", AmpAction::SetAtu(!active))
        }
    });
    Keys { ptt, ant, dn, up, tune, atu }
}

/// Whether the drive is over the amplifier's limit in bypass, so that going
/// in line now would overdrive it (safety interlock #4).
fn ptt_overdriven(r: &AmpReadings, limits: &AmpLimits) -> bool {
    !r.ptt && r.drive_w > limits.drive_warn_w
}

/// The tuner is out of the line, or has no network to show.
fn atu_bypassed(atu: &AtuReadout) -> bool {
    !atu.active || atu.network.is_none_or(|n| n.bypassed)
}

/// Whether the last tune failed, and whether the tuner is in line on a
/// successful one.
fn atu_result(atu: &AtuReadout) -> (bool, bool) {
    let failed = atu.last_result.as_deref().is_some_and(|r| {
        ["Failed", "SWR above", "Lo power", "Power too high"].iter().any(|f| r.starts_with(f))
    });
    let tuned = atu.active && atu.last_result.as_deref() == Some("Tuned");
    (failed, tuned)
}

/// The tuner's state in words, its tone, and the SWR it tuned to, while it
/// tunes or is in line.
fn atu_words(atu: &AtuReadout) -> Option<(String, Tone, Option<String>)> {
    let bypassed = atu_bypassed(atu);
    if !atu.tuning && bypassed {
        return None;
    }
    let (failed, _) = atu_result(atu);
    let state = if atu.tuning {
        "Tuning…".to_string()
    } else if let Some(res) = &atu.last_result {
        res.clone()
    } else {
        "ON".into()
    };
    let tone = if atu.tuning {
        Tone::Blue
    } else if failed {
        Tone::Red
    } else {
        Tone::Green
    };
    let swr = atu.network.and_then(|n| n.swr).filter(|_| !bypassed).map(|swr| {
        if swr >= 10.0 {
            // A refused tune into an open wire reads near total reflection; a
            // three-figure ratio means nothing.
            "> 10:1".to_string()
        } else {
            format!("{swr:.2}:1")
        }
    });
    Some((state, tone, swr))
}

/// A meter's TOP and BOTTOM pickers, each showing its reading's name in its
/// needle's colour.
fn pickers(ui: &mut egui::Ui, i: usize, pair: &mut Pair, s: f32, ink: &PickInk) {
    let pt = ink.size * s;
    let word =
        |t: &str| RichText::new(t).size(pt).color(ink.label).extra_letter_spacing(ink.spacing * s);
    // A row of fixed height, each item centred in it. In `ui.horizontal`
    // the row grew at the first picker and everything after was centred
    // lower, BOTTOM a few points below TOP (AJ3G, 24 Oct 2026): a picker
    // takes more room than its button shows. The height is the row's, as
    // measured last frame.
    let row_id = egui::Id::new(("amp-pickers-height", i));
    let row_h = ui.data(|d| d.get_temp::<f32>(row_id)).unwrap_or(ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(word("TOP"));
            let mut top = pair.top;
            dot(ui, ink.top, s);
            egui::ComboBox::from_id_salt(("amp-top", i))
                .width(64.0 * s)
                .selected_text(RichText::new(top.label()).size(pt).color(ink.top_text))
                .show_ui(ui, |ui| {
                    for m in AmpMeter::ALL {
                        ui.selectable_value(&mut top, m, m.label());
                    }
                });
            if top != pair.top {
                pair.set_top(top);
            }
            ui.add_space(8.0 * s);
            ui.label(word("BOTTOM"));
            let mut bottom = pair.bottom;
            let shown = bottom.map_or("none", |b| b.label());
            dot(ui, if bottom.is_some() { ink.bottom } else { ink.none }, s);
            egui::ComboBox::from_id_salt(("amp-bottom", i))
                .width(64.0 * s)
                .selected_text(RichText::new(shown).size(pt).color(ink.bottom_text))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut bottom, None, "none");
                    for m in AmpMeter::ALL {
                        ui.selectable_value(&mut bottom, Some(m), m.label());
                    }
                });
            if bottom != pair.bottom {
                pair.set_bottom(bottom);
            }
            let h = ui.min_rect().height();
            ui.data_mut(|d| d.insert_temp(row_id, h));
        },
    )
    .response
    .on_hover_text(ink.hover);
}

/// The pickers' colours: their words, the needles' dots, and the picked
/// readings' names.
struct PickInk {
    label: Color32,
    top: Color32,
    bottom: Color32,
    none: Color32,
    top_text: Color32,
    bottom_text: Color32,
    size: f32,
    spacing: f32,
    hover: &'static str,
}

impl PickInk {
    fn panel() -> PickInk {
        PickInk {
            label: theme::TEXT(),
            top: top_ink(),
            bottom: bottom_ink(),
            none: theme::LINE(),
            top_text: top_ink(),
            bottom_text: bottom_ink(),
            size: 12.0,
            spacing: 0.0,
            hover: "Pick any reading for either needle. The top needle is white, the bottom blue.",
        }
    }
}

/// A needle's colour as a dot beside its picker. Drawn rather than typed:
/// the interface font has no "●" and showed a hollow box in its place.
fn dot(ui: &mut egui::Ui, col: Color32, s: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0 * s), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0 * s, col);
}

/// One of the seven readings under the meters.
struct Tile {
    name: &'static str,
    value: String,
    gauge: Option<AmpGauge>,
    hover: &'static str,
    /// How far along its full scale it is, 0 to 1.
    frac: f32,
    /// Its colour: the amplifier's own for it, or red for an SWR at or past
    /// the warning.
    level: Option<AmpLevel>,
}

/// The seven readings, each in the amplifier's own colour for it.
fn tiles(sh: &Shown) -> [Tile; 7] {
    let Shown { r, limits, rig, past, .. } = *sh;
    let r = &r;
    let unit = if r.celsius == Some(false) { "F" } else { "C" };
    let swr = if r.swr >= 1.0 { format!("{:.1}:1", r.swr) } else { "---".into() };
    // In bypass, where a reading came from the radio (see `from_rig`).
    const FLAGGED: &str = "Nothing shown: the radio has flagged its own reading as not to be \
                           trusted (under 0.5 W, ramping up or down, or folding its power back \
                           into a bad match). DRV is not used instead, because it over-reads \
                           into a mismatch";
    const UNRELIABLE: &str = "Measured by the radio, but not to be trusted at this SWR: into an \
                              open the FDM-DUO reported about three times the power a wattmeter \
                              read. The SWR is the figure to go by: check the antenna";
    let fwd_hover = match rig.fwd {
        Some("rig") if past => UNRELIABLE,
        Some("rig") => "Forward power, measured by the radio: the amplifier reports none in OFF",
        Some("flagged") => FLAGGED,
        Some(_) => {
            "Forward power, taken from DRV: the amplifier reports none in OFF, and \
                    in bypass the radio's power goes straight through"
        }
        None => "Forward power",
    };
    let rfl_hover = match rig.rfl {
        Some("rig") if past => UNRELIABLE,
        Some("rig") => "Reflected power, measured by the radio: the amplifier reports none in OFF",
        Some("flagged") => FLAGGED,
        Some(_) => {
            "Reflected power, worked out from FWD and the SWR: the amplifier reports none in OFF"
        }
        None => "Reflected power",
    };
    let swr_hover = if past {
        "Standing wave ratio: red at or past the tune limit (Settings → General). Check the \
         antenna"
    } else if rig.swr {
        "Standing wave ratio, measured by the radio: the amplifier reports none in OFF"
    } else {
        "Standing wave ratio"
    };
    let raw: [(&str, String, Option<AmpGauge>, &str); 7] = [
        ("FWD", AmpMeter::Fwd.text(r), Some(AmpGauge::Fwd), fwd_hover),
        ("RFL", AmpMeter::Rfl.text(r), Some(AmpGauge::Rfl), rfl_hover),
        ("DRV", AmpMeter::Drv.text(r), Some(AmpGauge::Drive), "Drive from the radio"),
        ("VDD", AmpMeter::Vdd.text(r), Some(AmpGauge::Vdd), "Drain supply"),
        ("IDD", AmpMeter::Idd.text(r), Some(AmpGauge::Idd), "Drain current"),
        (
            "TEMP",
            format!("{:.0}°{unit}", r.temperature),
            Some(AmpGauge::Temp),
            "Heat spreader temperature: yellow from 49 °C (120 °F), red above 65 °C (149 °F), \
             off line above 70 °C (158 °F). Click for °C/°F.",
        ),
        // No colour of its own, as on the amplifier, where reflected power is
        // the one that warns and trips; but red at or past the tune limit,
        // where a radio's watts can't be trusted and the SWR is the figure
        // that tells the operator something is wrong (AJ3G, 18 Oct 2026).
        ("SWR", swr, None, swr_hover),
    ];
    let frac = |m: AmpMeter| {
        let (v, full, _) = m.read(r, &limits);
        v / full
    };
    let fracs = [
        frac(AmpMeter::Fwd),
        frac(AmpMeter::Rfl),
        frac(AmpMeter::Drv),
        frac(AmpMeter::Vdd),
        frac(AmpMeter::Idd),
        r.temp_c() / limits.temp_trip_c,
        // 1:1 to 5:1.
        if r.swr >= 1.0 { (r.swr - 1.0) / 4.0 } else { 0.0 },
    ];
    let mut i = 0;
    raw.map(|(name, value, gauge, hover)| {
        let level = if name == "SWR" && past {
            Some(AmpLevel::Red)
        } else {
            gauge.and_then(|g| sh.level(g))
        };
        let frac = fracs[i].clamp(0.0, 1.0);
        i += 1;
        Tile { name, value, gauge, hover, frac, level }
    })
}

/// The seven readings as tiles across the window, each in the amplifier's own
/// colour for it. Returns whether the temperature was clicked (°C/°F).
fn readings_row(ui: &mut egui::Ui, sh: &Shown) -> bool {
    let Shown { s, live, r, .. } = *sh;
    let unit = if r.celsius == Some(false) { "F" } else { "C" };
    let gap = ui.spacing().item_spacing.x;
    let w = (ui.available_width() - gap * 6.0) / 7.0;
    let h = 44.0 * s;
    let mut temp_clicked = false;
    ui.horizontal(|ui| {
        for Tile { name, value, gauge, hover, level, .. } in tiles(sh) {
            let temp = gauge == Some(AmpGauge::Temp);
            let sense = if temp && live { egui::Sense::click() } else { egui::Sense::hover() };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), sense);
            let (fill, ink) = match level {
                Some(l) => (level_col(l), theme::INK_ON_BRIGHT()),
                None => (theme::FILL(), theme::TEXT_STRONG()),
            };
            let fill =
                if resp.hovered() && temp && live { fill.gamma_multiply(0.85) } else { fill };
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 3.0, fill);
            // The one tile that does something when clicked gets an outline
            // and a folded corner, and the pointer on hover (AJ3G).
            if temp && live {
                p.rect_stroke(
                    rect.shrink(1.0),
                    3.0,
                    egui::Stroke::new(1.0 * s, ink.gamma_multiply(0.7)),
                    egui::StrokeKind::Inside,
                );
                corner_mark(&p, rect, s, ink);
                p.text(
                    Pos2::new(rect.right() - 6.0 * s, rect.bottom() - 4.0 * s),
                    egui::Align2::RIGHT_BOTTOM,
                    if unit == "F" { "°C" } else { "°F" },
                    egui::FontId::proportional(10.0 * s),
                    ink.gamma_multiply(0.8),
                );
            }
            let pad = Vec2::new(8.0 * s, 4.0 * s);
            p.text(
                rect.left_top() + pad,
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(11.0 * s),
                ink,
            );
            if live {
                p.text(
                    Pos2::new(rect.left() + pad.x, rect.bottom() - pad.y),
                    egui::Align2::LEFT_BOTTOM,
                    value,
                    egui::FontId::monospace(14.0 * s),
                    ink,
                );
            }
            let resp = if temp && live { clickable(ui, &resp, s) } else { resp };
            if resp.on_hover_text(hover).clicked() {
                temp_clicked = true;
            }
        }
    });
    temp_clicked
}

/// Input power, efficiency and gain, worked out from the readings while
/// transmitting. The number is the peak hold, as on an SSB wattmeter; the bar
/// under it is live, with the held peak as a tick on it.
fn performance_row(ui: &mut egui::Ui, s: f32, perf: &Perf, limits: &AmpLimits, now: f64) {
    let row = ui.horizontal(|ui| caption(ui, s, "Performance")).response.rect;
    // Centred over the middle box, EFFICIENCY.
    ui.painter().text(
        Pos2::new(ui.max_rect().center().x, row.center().y),
        egui::Align2::CENTER_CENTER,
        "numbers hold peaks · bars are live",
        egui::FontId::proportional(11.0 * s),
        theme::TEXT(),
    );
    let full = perf_full(limits);
    let boxes = [
        ("INPUT POWER", "W", 0, theme::CYAN(), "Supply volts × drain amps"),
        ("EFFICIENCY", "%", 0, theme::GREEN(), "Output power ÷ input power"),
        ("GAIN", "dB", 1, theme::GREEN(), "Output power over drive, in decibels"),
    ];
    let gap = ui.spacing().item_spacing.x;
    let w = (ui.available_width() - gap * 2.0) / 3.0;
    let h = 72.0 * s;
    ui.horizontal(|ui| {
        for (i, (name, unit, places, col, hover)) in boxes.into_iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), egui::Sense::hover());
            resp.on_hover_text(hover);
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 4.0, theme::BG_DEEP());
            p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::LINE()), egui::StrokeKind::Inside);
            let x = rect.left() + 12.0 * s;
            p.text(
                Pos2::new(x, rect.top() + 6.0 * s),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(11.0 * s),
                theme::TEXT(),
            );
            let fmt = |v: f32| format!("{v:.places$}");
            let held = perf.peaks[i].get(now);
            let value = if held > 0.0 { fmt(held) } else { "—".to_string() };
            let vr = p.text(
                Pos2::new(x, rect.top() + 20.0 * s),
                egui::Align2::LEFT_TOP,
                value,
                egui::FontId::monospace(20.0 * s),
                theme::TEXT_STRONG(),
            );
            p.text(
                Pos2::new(vr.right() + 5.0 * s, vr.bottom() - 2.0 * s),
                egui::Align2::LEFT_BOTTOM,
                unit,
                egui::FontId::monospace(11.0 * s),
                theme::TEXT(),
            );
            // The bar, and the held peak as a tick on it.
            let track = Rect::from_min_size(
                Pos2::new(x, rect.top() + 47.0 * s),
                Vec2::new(rect.width() - 24.0 * s, 4.0 * s),
            );
            p.rect_filled(track, 2.0, theme::FILL());
            let frac = |v: f32| (v / full[i]).clamp(0.0, 1.0);
            if let Some(a) = perf.live {
                let mut bar = track;
                bar.set_width(track.width() * frac(a[i]));
                p.rect_filled(bar, 2.0, col);
            }
            if held > 0.0 {
                let px = track.left() + track.width() * frac(held);
                p.line_segment(
                    [Pos2::new(px, track.top() - 3.0 * s), Pos2::new(px, track.bottom() + 3.0 * s)],
                    Stroke::new(2.5 * s, theme::YELLOW()),
                );
            }
            if let Some(a) = perf.live {
                p.text(
                    Pos2::new(x, rect.bottom() - 5.0 * s),
                    egui::Align2::LEFT_BOTTOM,
                    format!("live {} {unit}", fmt(a[i])),
                    egui::FontId::proportional(11.0 * s),
                    theme::TEXT(),
                );
            }
        }
    });
}

/// One line saying whether the amplifier is answering, and why not; or, when
/// it has taken itself off line, why.
fn link_line(ui: &mut egui::Ui, st: &AmpStatus, s: f32, transmitting: bool) {
    let (text, tone) = link_state(st, transmitting);
    let col = match tone {
        Tone::Red => theme::ALERT(),
        Tone::Yellow => theme::YELLOW(),
        _ => theme::GREEN(),
    };
    ui.label(RichText::new(text).size(12.0 * s).color(col));
}

/// Whether the amplifier is answering, and why not; or, when it has taken
/// itself off line, why. Green, or yellow or red for a warning.
fn link_state(st: &AmpStatus, transmitting: bool) -> (String, Tone) {
    let trips = st.alerts.trips();
    let hot = st.readings.is_some_and(|r| st.model.limits().over_temp(&r));
    if st.connected && hot {
        ("OFF LINE · over temperature".to_string(), Tone::Red)
    } else if st.connected && st.alerts.tripped {
        let why =
            if trips.is_empty() { String::new() } else { format!(" · {}", trips.join(", ")) };
        (format!("TRIPPED{why}"), Tone::Red)
    } else if st.connected && transmitting {
        ("CONNECTED · TRANSMITTING".to_string(), Tone::Green)
    } else if st.connected {
        ("CONNECTED".to_string(), Tone::Green)
    } else if let Some(e) = &st.error {
        (e.clone(), Tone::Red)
    } else {
        ("Connecting…".to_string(), Tone::Yellow)
    }
}

/// What one needle shows.
struct Dial {
    name: &'static str,
    readout: Option<String>,
    value: f32,
    full: f32,
    zones: Zones,
    live: bool,
    /// Added to the name when the scale isn't the usual one.
    note: &'static str,
    /// Added after that when the reading isn't the amplifier's own.
    source: &'static str,
    /// The radio's own watts at an SWR where they can't be trusted (see
    /// [`swr_past`]): warned of across the top of the meter.
    unsure: bool,
}

/// A cross-needle meter of `size`: the top reading on the outer scale in
/// white, the bottom one on its own inner scale in blue. Both are live.
fn meter(ui: &mut egui::Ui, size: Vec2, s: f32, top: &Dial, bottom: Option<Dial>) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::BG_DEEP());
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::LINE()), egui::StrokeKind::Inside);

    let h = rect.height();
    let pivot = Pos2::new(rect.center().x, rect.bottom() - h * 0.06);
    let outer = h * 0.70;
    let inner = outer * 0.62;
    // The scale sweeps 55° either side of vertical, as a panel meter does.
    let sweep = 55f32.to_radians();
    let at = |frac: f32, r: f32| {
        let a = -std::f32::consts::FRAC_PI_2 - sweep + frac.clamp(0.0, 1.2) * 2.0 * sweep;
        pivot + Vec2::new(a.cos(), a.sin()) * r
    };

    // One scale: the arc, its yellow and red stretches just inside it, and
    // ticks with labels outside (the top scale) or inside (the bottom one).
    let scale = |d: &Dial, radius: f32, ink: Color32, labels_out: bool| {
        let arc: Vec<Pos2> = (0..=40).map(|i| at(i as f32 / 40.0, radius)).collect();
        p.add(egui::Shape::line(arc, Stroke::new(1.5 * s, ink)));
        let band = |from: f32, to: f32, col: Color32| {
            let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
            if to > from {
                let n = ((to - from) * 40.0).ceil().max(1.0) as usize;
                let pts =
                    (0..=n).map(|i| at(from + (to - from) * i as f32 / n as f32, radius - 3.0 * s));
                p.add(egui::Shape::line(pts.collect(), Stroke::new(4.0 * s, col)));
            }
        };
        match d.zones {
            Zones::Above(warn, alarm) => {
                band(warn, alarm, theme::YELLOW());
                band(alarm, 1.0, theme::ALERT());
                // As on an airspeed indicator, the colours are always on
                // show: a level beyond the end of a rescaled scale is marked
                // just past it, in its colour and with its value.
                let beyond = [(warn, theme::YELLOW()), (alarm, theme::ALERT())]
                    .into_iter()
                    .filter(|&(f, _)| f > 1.0);
                for (k, (f, col)) in beyond.enumerate() {
                    let pos = 1.08 + 0.08 * k as f32;
                    p.line_segment(
                        [at(pos, radius + 2.0 * s), at(pos, radius - 7.0 * s)],
                        Stroke::new(4.0 * s, col),
                    );
                    let v = d.full * f;
                    // Outside the arc, clear of the scale's own end label.
                    let r = radius + 10.0 * s;
                    p.text(
                        at(pos, r),
                        egui::Align2::CENTER_CENTER,
                        if v < 20.0 { format!("{v:.1}") } else { format!("{v:.0}") },
                        egui::FontId::monospace(if labels_out { 10.5 } else { 9.5 } * s),
                        col,
                    );
                }
            }
            Zones::Below(low) => band(0.0, low, theme::YELLOW()),
        }
        for i in 0..=10 {
            let f = i as f32 / 10.0;
            let major = i % 2 == 0;
            let len = if major { 9.0 } else { 5.0 } * s;
            p.line_segment([at(f, radius), at(f, radius - len)], Stroke::new(1.0 * s, ink));
            if major {
                let v = d.full * f;
                let label = if d.full < 20.0 { format!("{v:.1}") } else { format!("{v:.0}") };
                let r = if labels_out { radius + 10.0 * s } else { radius - 18.0 * s };
                p.text(
                    at(f, r),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(if labels_out { 10.5 } else { 9.5 } * s),
                    ink,
                );
            }
        }
    };
    scale(top, outer, top_ink(), true);
    if let Some(d) = &bottom {
        scale(d, inner, bottom_ink(), false);
    }

    // Red, large and centred at the top, above the scale, while it lasts
    // (AJ3G, 18 Oct 2026): the legend was too easy to miss for a reading
    // that is three times high.
    if let Some(text) = unsure_warning(top, bottom.as_ref()) {
        p.text(
            Pos2::new(rect.center().x, rect.top() + 6.0 * s),
            egui::Align2::CENTER_TOP,
            text,
            egui::FontId::proportional(15.0 * s),
            theme::ALERT(),
        );
    }

    // Names on the left and readings on the right, in the bottom corners and
    // clear of the needles, each in its needle's colour.
    let corner = |d: &Dial, y: f32, ring: &str, ink: Color32, big: bool| {
        let more = format!("{}{}", d.note, d.source);
        // The dot is drawn, not typed: the interface font has no "●" and
        // showed a hollow box in its place.
        // Full size while it fits left of the needles; a longer one (a scale
        // note, "rescaled", where the reading came from) is set smaller
        // rather than run into them (AJ3G, 18 Oct 2026). The needles are
        // furthest left at zero, where they rise from the pivot at 90° − 55°
        // = 35°, so the room ends where that line crosses the legend's top.
        let text = format!("{} · {ring}{more}", d.name);
        let left = rect.left() + 22.0 * s;
        let full = p.layout_no_wrap(text.clone(), egui::FontId::proportional(12.0 * s), ink);
        let rise = (pivot.y - (y - full.size().y)).max(0.0);
        let needle_x = pivot.x - rise / (std::f32::consts::FRAC_PI_2 - sweep).tan();
        let room = needle_x - 8.0 * s - left;
        let size = legend_size(full.size().x, room, s);
        let galley = if size < 12.0 * s {
            p.layout_no_wrap(text, egui::FontId::proportional(size), ink)
        } else {
            full
        };
        let name = Rect::from_min_size(Pos2::new(left, y - galley.size().y), galley.size());
        p.galley(name.min, galley, ink);
        p.circle_filled(Pos2::new(rect.left() + 14.0 * s, name.center().y), 4.0 * s, ink);
        if let Some(text) = &d.readout {
            p.text(
                Pos2::new(rect.right() - 10.0 * s, y),
                egui::Align2::RIGHT_BOTTOM,
                text,
                egui::FontId::monospace(if big { 16.0 } else { 13.0 } * s),
                ink,
            );
        }
    };
    // Outer above inner, as the scales are (AJ3G, 18 Oct).
    let (outer_y, inner_y) = (rect.bottom() - 26.0 * s, rect.bottom() - 6.0 * s);
    if let Some(d) = &bottom {
        corner(top, outer_y, "outer", top_ink(), true);
        corner(d, inner_y, "inner", bottom_ink(), false);
    } else {
        corner(top, inner_y, "outer", top_ink(), true);
    }

    let needle = |d: &Dial, radius: f32, ink: Color32, width: f32| {
        if d.live {
            let tip = at((d.value / d.full.max(1e-6)).min(1.03), radius - 4.0 * s);
            p.line_segment([pivot, tip], Stroke::new(width * s, ink));
        }
    };
    if let Some(d) = &bottom {
        needle(d, inner, bottom_ink(), 2.0);
    }
    needle(top, outer, top_ink(), 2.5);
    p.circle_filled(pivot, 6.0 * s, theme::PANEL());
}

/// An arrowhead at `tip`, pointing away from `from`.
fn arrow(p: &egui::Painter, from: Pos2, tip: Pos2, s: f32, col: Color32) {
    let dir = (tip - from).normalized();
    let head = tip - dir * 9.0 * s;
    let perp = Vec2::new(-dir.y, dir.x) * 5.0 * s;
    p.add(egui::Shape::convex_polygon(vec![tip, head + perp, head - perp], col, Stroke::NONE));
}

/// A wire's end: a small open circle on the ground `bg`.
fn node(p: &egui::Painter, c: Pos2, s: f32, ink: Stroke, bg: Color32) {
    p.circle_filled(c, 5.0 * s, bg);
    p.circle_stroke(c, 5.0 * s, ink);
}

/// The tuner drawing's colours: wires and parts, values, small labels, the
/// wire the capacitor is switched to, the ground, and the caption in the
/// corner, if the drawing carries its own.
struct NetInk {
    wire: Color32,
    value: Color32,
    small: Color32,
    lit: Color32,
    bg: Color32,
    caption: Option<Color32>,
}

impl NetInk {
    fn panel() -> NetInk {
        NetInk {
            wire: theme::TEXT_STRONG(),
            value: theme::TEXT_STRONG(),
            small: theme::TEXT(),
            lit: theme::CYAN(),
            bg: theme::BG_DEEP(),
            caption: Some(theme::CYAN()),
        }
    }
}

/// The tuner drawing's caption: which side the shunt capacitor is on, or
/// that the tuner is bypassed.
fn net_caption(atu: &AtuReadout) -> &'static str {
    match atu.network.filter(|_| !atu_bypassed(atu)) {
        None => "ATU BYPASSED",
        Some(n) if n.cap_output => "C OUT · antenna side",
        Some(_) => "C IN · transmitter side",
    }
}

/// The tuner bypassed, drawn as HobbyPCB draws it: a relay at each end
/// switched up to the straight-through wire, past the ATU. Only the wires
/// stretch with the window; the parts keep their size.
fn bypass_drawing(p: &egui::Painter, rect: Rect, s: f32, c: &NetInk) {
    let ink = Stroke::new(2.0 * s, c.wire);
    let x = |f: f32| rect.left() + rect.width() * f;
    let y = |f: f32| rect.top() + rect.height() * f;
    let (line, through, below) = (y(0.47), y(0.30), y(0.63));
    let (sw_l, sw_r) = (Pos2::new(x(0.21), line), Pos2::new(x(0.79), line));
    let (up_l, up_r) = (Pos2::new(x(0.30), through), Pos2::new(x(0.70), through));
    let (dn_l, dn_r) = (Pos2::new(x(0.30), below), Pos2::new(x(0.70), below));

    p.line_segment([Pos2::new(x(0.04), line), sw_l], ink);
    p.line_segment([sw_r, Pos2::new(x(0.96), line)], ink);
    p.line_segment([up_l, up_r], ink);
    // The relays' arms, each pointing at the wire it is switched to.
    for (from, to) in [(sw_l, up_l), (sw_r, up_r)] {
        let tip = to + (from - to).normalized() * 7.0 * s;
        p.line_segment([from, tip], ink);
        arrow(p, from, tip, s, c.wire);
    }
    // The ATU, out of the path.
    let box_w = 100.0 * s;
    let box_h = (rect.height() * 0.24).max(24.0 * s);
    let atu = Rect::from_center_size(Pos2::new(rect.center().x, below), Vec2::new(box_w, box_h));
    p.line_segment([dn_l, Pos2::new(atu.left(), below)], ink);
    p.line_segment([Pos2::new(atu.right(), below), dn_r], ink);
    p.rect_stroke(atu, 2.0, ink, egui::StrokeKind::Middle);
    p.text(
        atu.center(),
        egui::Align2::CENTER_CENTER,
        "ATU",
        egui::FontId::proportional(18.0 * s),
        c.value,
    );
    for at in [sw_l, sw_r, up_l, up_r, dn_l, dn_r] {
        node(p, at, s, ink, c.bg);
    }

    let small = egui::FontId::proportional(11.0 * s);
    p.text(
        Pos2::new(x(0.05), line - 8.0 * s),
        egui::Align2::LEFT_BOTTOM,
        "TX",
        small.clone(),
        c.small,
    );
    p.text(Pos2::new(x(0.95), line - 8.0 * s), egui::Align2::RIGHT_BOTTOM, "ANT", small, c.small);
    if let Some(col) = c.caption {
        p.text(
            Pos2::new(rect.right() - 14.0 * s, rect.bottom() - 12.0 * s),
            egui::Align2::RIGHT_BOTTOM,
            "ATU BYPASSED",
            egui::FontId::proportional(13.0 * s),
            col,
        );
    }
}

/// The tuner's L network, drawn the way HobbyPCB's panel draws it: the
/// inductor in the line and the capacitor switched to whichever side of it
/// the tuner chose.
fn lc_network(p: &egui::Painter, rect: Rect, s: f32, n: AtuNetwork, model: AmpModel, c: &NetInk) {
    let ink = Stroke::new(2.0 * s, c.wire);
    let lit = Stroke::new(2.5 * s, c.lit);
    let x = |f: f32| rect.left() + rect.width() * f;
    let y = |f: f32| rect.top() + rect.height() * f;
    let top = y(0.34);

    // The line through, with the inductor as four turns in the middle. The
    // turns keep their size; only the wires stretch.
    let turn = 30.0 * s;
    let (coil_l, coil_r) = (rect.center().x - 2.0 * turn, rect.center().x + 2.0 * turn);
    p.line_segment([Pos2::new(x(0.04), top), Pos2::new(coil_l, top)], ink);
    p.line_segment([Pos2::new(coil_r, top), Pos2::new(x(0.96), top)], ink);
    for i in 0..4 {
        let c = Pos2::new(coil_l + turn * (i as f32 + 0.5), top);
        let pts: Vec<Pos2> = (0..=16)
            .map(|k| {
                let a = std::f32::consts::PI * (1.0 - k as f32 / 16.0);
                c + Vec2::new(a.cos() * turn / 2.0, -a.sin() * turn / 2.0)
            })
            .collect();
        p.add(egui::Shape::line(pts, ink));
    }

    // The two taps down to the switch, and the capacitor below the middle.
    let mid = y(0.61);
    let (tap_l, tap_r, centre) =
        (Pos2::new(x(0.31), mid), Pos2::new(x(0.69), mid), Pos2::new(rect.center().x, mid));
    p.line_segment([Pos2::new(x(0.31), top), tap_l], ink);
    p.line_segment([Pos2::new(x(0.69), top), tap_r], ink);
    let side = if n.cap_output { tap_r } else { tap_l };
    p.line_segment([centre, side], lit);
    node(p, tap_l, s, ink, c.bg);
    node(p, tap_r, s, ink, c.bg);
    node(p, centre, s, ink, c.bg);

    let plate_w = 26.0 * s;
    let (p1, p2) = (mid + 14.0 * s, mid + 21.0 * s);
    let ground = mid + 30.0 * s;
    p.line_segment([centre + Vec2::new(0.0, 5.0 * s), Pos2::new(centre.x, p1)], ink);
    for py in [p1, p2] {
        p.line_segment([Pos2::new(centre.x - plate_w, py), Pos2::new(centre.x + plate_w, py)], ink);
    }
    p.line_segment([Pos2::new(centre.x, p2), Pos2::new(centre.x, ground)], ink);
    for (k, wf) in [(0.0, 1.0), (1.0, 0.65), (2.0, 0.3)] {
        let gy = ground + k * 4.0 * s;
        p.line_segment(
            [Pos2::new(centre.x - plate_w * wf, gy), Pos2::new(centre.x + plate_w * wf, gy)],
            ink,
        );
    }

    // A step of 0 isn't a missing value: the tuner needed none.
    // The step goes alongside, for comparing with the amplifier's own
    // figures; the value includes the board's strays (see `atu_scale`).
    let (l_text, c_text) = match model.atu_scale() {
        Some(sc) => (
            if n.l_step == 0 {
                "no inductance needed".to_string()
            } else {
                format!("~{:.2} µH · step {}", sc.inductance_uh(n.l_step), n.l_step)
            },
            if n.c_step == 0 {
                "no capacitance needed".to_string()
            } else {
                format!("~{:.0} pF · step {}", sc.capacitance_pf(n.c_step), n.c_step)
            },
        ),
        None => (format!("L {}", n.l_step), format!("C {}", n.c_step)),
    };
    let font = egui::FontId::monospace(14.0 * s);
    p.text(
        Pos2::new(rect.center().x, top - turn / 2.0 - 6.0 * s),
        egui::Align2::CENTER_BOTTOM,
        l_text,
        font.clone(),
        c.value,
    );
    p.text(
        Pos2::new(centre.x + plate_w + 12.0 * s, (p1 + p2) / 2.0),
        egui::Align2::LEFT_CENTER,
        c_text,
        font,
        c.value,
    );
    // Which end is which, and which side the shunt capacitor is on: HobbyPCB
    // calls these CAP IN (transmitter side) and CAP OUT (antenna side).
    let small = egui::FontId::proportional(11.0 * s);
    p.text(
        Pos2::new(x(0.05), top - 8.0 * s),
        egui::Align2::LEFT_BOTTOM,
        "TX",
        small.clone(),
        c.small,
    );
    p.text(Pos2::new(x(0.95), top - 8.0 * s), egui::Align2::RIGHT_BOTTOM, "ANT", small, c.small);
    if let Some(col) = c.caption {
        let shunt = if n.cap_output { "C OUT · antenna side" } else { "C IN · transmitter side" };
        p.text(
            Pos2::new(rect.right() - 14.0 * s, rect.bottom() - 12.0 * s),
            egui::Align2::RIGHT_BOTTOM,
            shunt,
            egui::FontId::proportional(13.0 * s),
            col,
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn in_bypass_fwd_rfl_and_swr_come_from_the_rig() {
        // AJ3G's ELAD in OFF: the amp reads 5 W of drive and nothing else; the
        // DUO reports SWR but not watts.
        let off = AmpReadings { drive_w: 5.0, ptt: false, ..Default::default() };
        let duo = TxMeters { fwd_w: None, swr: Some(3.0), alc: 0.0, po: None, rfl_w: None };
        let (r, fill) = from_rig(off, Some(&duo), false);
        assert_eq!((r.fwd_w, r.swr), (5.0, 3.0));
        // |Γ| = 0.5 at 3:1, so a quarter of the power comes back.
        assert!((r.rfl_w - 1.25).abs() < 1e-4);
        assert_eq!(fill, RigFill { fwd: Some("DRV"), rfl: Some("SWR"), swr: true });

        // A rig that measures its own watts is believed over DRV.
        let watts = TxMeters { fwd_w: Some(4.6), ..duo };
        assert_eq!(from_rig(off, Some(&watts), false).0.fwd_w, 4.6);
        // And its reflected watts, where it measures them (the DUO's `RP`).
        let both = TxMeters { fwd_w: Some(6.091), rfl_w: Some(0.012), swr: Some(1.09), ..duo };
        let (r, fill) = from_rig(off, Some(&both), true);
        assert_eq!((r.fwd_w, r.rfl_w, r.swr), (6.091, 0.012, 1.09));
        assert_eq!(fill, RigFill { fwd: Some("rig"), rfl: Some("rig"), swr: true });

        // In PTT nothing changes: the amplifier's bridge is the one to show.
        let ptt = AmpReadings { ptt: true, ..off };
        assert_eq!(from_rig(ptt, Some(&duo), true), (ptt, RigFill::default()));
        // Receiving: the rig has no meters to give, and nothing is invented.
        assert_eq!(from_rig(off, None, true), (off, RigFill::default()));
    }

    #[test]
    fn drv_is_on_the_power_meter_and_rescales_with_it() {
        use super::{AmpMeter, AmpMeters, DEFAULT_PAIRS, OLD_DEFAULT_SAVED, PowerRange};
        assert_eq!(DEFAULT_PAIRS[0].bottom, Some(AmpMeter::Drv));
        // The old built-in picks, as saved, come back as the new ones; a
        // layout someone chose is kept.
        let mut m = AmpMeters::default();
        m.restore(OLD_DEFAULT_SAVED);
        assert_eq!(m.pairs, DEFAULT_PAIRS);
        m.restore([2, 255, 0, 1]);
        assert_eq!(m.pairs[0].top, AmpMeter::Vdd);
        // Barefoot, DRV is on FWD's 10 W or 100 W scale.
        assert_eq!(AmpMeter::Drv.range_full(PowerRange::Ten), Some(10.0));
        assert_eq!(AmpMeter::Drv.range_full(PowerRange::Hundred), Some(100.0));
        assert_eq!(AmpMeter::Drv.range_full(PowerRange::Full), None);
    }

    #[test]
    fn a_duo_that_flags_its_reading_never_borrows_drv() {
        // AJ3G, 18 Oct, into an open: the DUO folded back and flagged FP and
        // RP, while DRV read almost 10 W against 2.94 W on the LP-100A.
        let off = AmpReadings { drive_w: 9.8, ptt: false, ..Default::default() };
        let flagged = TxMeters { fwd_w: None, swr: None, alc: 0.0, po: None, rfl_w: None };
        let (r, fill) = from_rig(off, Some(&flagged), true);
        assert_eq!((r.fwd_w, r.rfl_w), (0.0, 0.0));
        assert_eq!(fill, RigFill { fwd: Some("flagged"), rfl: Some("flagged"), swr: false });
        // Forward given, reflected and SWR flagged: RFL says so.
        let half = TxMeters { fwd_w: Some(8.7), ..flagged };
        assert_eq!(from_rig(off, Some(&half), true).1.rfl, Some("flagged"));
        // A radio that never gives watts still gets DRV.
        assert_eq!(from_rig(off, Some(&flagged), false).1.fwd, Some("DRV"));
    }

    #[test]
    fn fwd_and_rfl_change_range_as_aj3g_set_out() {
        use super::{AmpReadings, PowerRange::*, RangeWatch};
        let inline =
            |fwd: f32| AmpReadings { fwd_w: fwd, drive_w: 0.2, ptt: true, ..Default::default() };
        let through = |w: f32| AmpReadings { drive_w: w, ..Default::default() };
        let run = |w: &mut RangeWatch, from: f64, to: f64, r: AmpReadings| {
            let mut t = from;
            while t <= to {
                w.update(t, true, &r);
                t += 0.25;
            }
        };

        // Never keyed: the full scales stay.
        let mut w = RangeWatch::default();
        run(&mut w, 0.0, 9.0, AmpReadings::default());
        assert_eq!(w.range, Full);

        // In line at 5 W: the 10 W range, but only after 2.5 s of it.
        run(&mut w, 10.0, 12.0, inline(5.0));
        assert_eq!(w.range, Full);
        run(&mut w, 12.25, 15.5, inline(5.0));
        assert_eq!(w.range, Ten);
        // Unkeyed, the range stays; 10 W is still low; over it, full at once.
        run(&mut w, 16.0, 17.0, AmpReadings::default());
        w.update(17.5, true, &inline(10.0));
        assert_eq!(w.range, Ten);
        w.update(17.75, true, &inline(10.5));
        assert_eq!(w.range, Full);

        // A QRP radio in bypass: the 10 W range.
        let mut b = RangeWatch::default();
        run(&mut b, 0.0, 6.0, through(5.0));
        assert_eq!(b.range, Ten);
        // A 100 W radio in bypass: one short swing over 10 W isn't enough...
        run(&mut b, 7.0, 7.5, through(80.0));
        assert_eq!(b.range, Ten);
        // ...a second swing is.
        run(&mut b, 8.0, 8.5, through(0.0));
        b.update(9.0, true, &through(80.0));
        assert_eq!(b.range, Hundred);
        // Two seconds over 10 W in one go is too.
        let mut c = RangeWatch::default();
        run(&mut c, 0.0, 6.0, through(5.0));
        run(&mut c, 7.0, 9.25, through(60.0));
        assert_eq!(c.range, Hundred);
        // Over 100 W: the full scales at once.
        c.update(9.5, true, &through(150.0));
        assert_eq!(c.range, Full);
    }

    #[test]
    fn the_meter_picks_come_back_as_they_were_saved() {
        let mut m = super::AmpMeters::default();
        m.pairs[0] = super::Pair { top: super::AmpMeter::Vdd, bottom: None };
        m.pairs[1].set_bottom(Some(super::AmpMeter::Fwd));
        let mut back = super::AmpMeters::default();
        back.restore(m.saved());
        assert_eq!(back.pairs, m.pairs);
        // Nonsense leaves the built-in picks alone.
        let mut bad = super::AmpMeters::default();
        bad.restore([9, 9, 0, 0]);
        assert_eq!(bad.pairs, super::AmpMeters::default().pairs);
    }

    use super::*;

    #[test]
    fn either_needle_takes_any_reading_and_they_swap_cleanly() {
        let mut m = Pair { top: AmpMeter::Fwd, bottom: Some(AmpMeter::Rfl) };
        // Putting the bottom's reading on top swaps them rather than doubling up.
        m.set_top(AmpMeter::Rfl);
        assert_eq!(m, Pair { top: AmpMeter::Rfl, bottom: Some(AmpMeter::Fwd) });
        m.set_bottom(Some(AmpMeter::Rfl));
        assert_eq!(m, Pair { top: AmpMeter::Fwd, bottom: Some(AmpMeter::Rfl) });
        m.set_bottom(None);
        assert_eq!(m.bottom, None);
        // With no bottom needle, the top's reading can't be put there too.
        m.set_bottom(Some(AmpMeter::Fwd));
        assert_eq!(m.bottom, None);
        m.set_bottom(Some(AmpMeter::Idd));
        assert_eq!(m, Pair { top: AmpMeter::Fwd, bottom: Some(AmpMeter::Idd) });
    }

    #[test]
    fn a_peak_holds_then_falls_away() {
        let mut p = Peak::default();
        assert_eq!(p.feed(500.0, 0.0), 500.0);
        // Held through a pause in speech…
        assert_eq!(p.feed(100.0, 1.0), 500.0);
        // …then falling a third of the peak a second…
        assert!((p.get(1.0) - 500.0).abs() < 1e-3);
        assert!((p.get(2.0) - 335.0).abs() < 1e-3);
        // …until a new peak takes over, or it reaches nothing.
        assert_eq!(p.feed(400.0, 2.0), 400.0);
        assert_eq!(p.get(20.0), 0.0);
    }

    #[test]
    fn every_figure_falls_away_in_the_same_time() {
        // 850 W, 49 % and 20.7 dB, held together, are gone together, though
        // they sit at different heights on different scales.
        let held = [850.0, 49.0, 20.7].map(|v| {
            let mut p = Peak::default();
            p.feed(v, 0.0);
            p
        });
        assert!(held.iter().all(|p| p.get(3.9) > 0.0));
        assert!(held.iter().all(|p| p.get(4.1) == 0.0));
    }

    #[test]
    fn performance_is_worked_out_only_while_transmitting() {
        let r = AmpReadings {
            fwd_w: 420.0,
            drive_w: 3.6,
            vdd_v: 56.4,
            idd_a: 15.1,
            ptt: true,
            ..Default::default()
        };
        let [input, eff, gain] = performance(&r).unwrap();
        assert!((input - 851.6).abs() < 0.1);
        assert!((eff - 49.3).abs() < 0.1);
        assert!((gain - 20.67).abs() < 0.01);
        assert_eq!(performance(&AmpReadings { fwd_w: 0.0, ..r }), None);
        assert_eq!(performance(&AmpReadings { ptt: false, ..r }), None);
        assert_eq!(performance(&AmpReadings::default()), None);
    }

    fn status(r: AmpReadings) -> AmpStatus {
        AmpStatus {
            configured: true,
            connected: true,
            readings: Some(r),
            atu: Some(AtuReadout { active: true, ..Default::default() }),
            ..Default::default()
        }
    }

    fn shown(st: &AmpStatus) -> Shown<'_> {
        Shown {
            st,
            r: st.readings.unwrap_or_default(),
            limits: st.model.limits(),
            rig: RigFill::default(),
            live: st.connected && st.readings.is_some(),
            past: false,
            range: PowerRange::Full,
            now: 0.0,
            s: 1.0,
            width: 720.0,
            nominal: 720.0,
            transmitting: false,
        }
    }

    #[test]
    fn every_look_greys_the_relay_keys_while_there_is_rf() {
        // Interlock #2: the keys both looks draw come from `keys`.
        let quiet = status(AmpReadings { ptt: true, vdd_v: 53.0, ..Default::default() });
        let k = keys(&shown(&quiet));
        let relays = |k: &Keys| {
            [k.ant.as_ref(), Some(&k.dn), Some(&k.up), k.atu.as_ref()]
                .into_iter()
                .map(|k| k.unwrap().enabled)
                .collect::<Vec<_>>()
        };
        assert_eq!(relays(&k), [true; 4]);
        let keyed =
            status(AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..Default::default() });
        let k = keys(&shown(&keyed));
        assert_eq!(relays(&k), [false; 4]);
        assert_eq!(k.dn.why_not, NOT_WHILE_TX);
        // TUNE is not a relay switch: it works on RF.
        assert!(k.tune.unwrap().enabled);
        // Not answering: nothing can be pressed.
        let gone = AmpStatus { connected: false, ..quiet };
        let k = keys(&shown(&gone));
        assert!(!k.ptt.enabled && !k.dn.enabled && !k.tune.unwrap().enabled);
    }

    #[test]
    fn every_look_refuses_ptt_while_overdriven_in_bypass() {
        // Interlock #4: 9 W through the bypassed HR500, whose limit is 8 W.
        let st = status(AmpReadings { ptt: false, drive_w: 9.0, ..Default::default() });
        let k = keys(&shown(&st));
        assert!(!k.ptt.enabled);
        assert!(k.ptt.why_not.starts_with("Lower the drive first"));
        let st = status(AmpReadings { ptt: false, drive_w: 5.0, ..Default::default() });
        let k = keys(&shown(&st));
        assert!(k.ptt.enabled);
        assert_eq!(k.ptt.action, AmpAction::SetPtt(true));
        // In line already, PTT to OFF is always allowed.
        let st = status(AmpReadings { ptt: true, drive_w: 9.0, ..Default::default() });
        assert!(keys(&shown(&st)).ptt.enabled);
    }

    #[test]
    fn a_window_fits_by_its_width_or_its_height_whichever_is_tighter() {
        let r = (MIN_SCALE, MAX_SCALE);
        // 720 wide and 600 tall at scale 1.
        assert!((fit_scale(Vec2::new(1440.0, 2000.0), 720.0, 600.0, r) - 2.0).abs() < 1e-3);
        // A wide window: the height decides, a little short of a full fit.
        assert!((fit_scale(Vec2::new(1900.0, 600.0), 720.0, 600.0, r) - 0.98).abs() < 1e-3);
        // Never below the smallest scale (it scrolls) or past the largest.
        assert_eq!(fit_scale(Vec2::new(200.0, 200.0), 720.0, 600.0, r), MIN_SCALE);
        assert_eq!(fit_scale(Vec2::new(9000.0, 9000.0), 720.0, 600.0, r), MAX_SCALE);
    }

    #[test]
    fn a_click_shows_as_asked_in_blue_until_read_back() {
        let mut st =
            status(AmpReadings { ptt: false, antenna: 1, band: Some(7), ..Default::default() });
        st.pending = vec![
            AmpAction::SetPtt(true),
            AmpAction::SetAntenna(2),
            AmpAction::BandUp,
            AmpAction::SetAtu(false),
        ];
        let sh = shown(&st);
        let k = keys(&sh);
        // Shown as asked, in the neutral blue, never green before it is read.
        assert_eq!((k.ptt.text.as_str(), k.ptt.tone), ("PTT", Tone::Blue));
        assert_eq!(k.ptt.action, AmpAction::SetPtt(false));
        let ant = k.ant.unwrap();
        assert_eq!((ant.text.as_str(), ant.tone, ant.lit), ("ANT 2", Tone::Blue, true));
        assert_eq!(ant.action, AmpAction::SetAntenna(1));
        assert_eq!(sh.band(), (Some(6), true));
        let atu = k.atu.unwrap();
        assert_eq!((atu.text.as_str(), atu.action), ("BYP", AmpAction::SetAtu(true)));
        // Read back: shown as read, PTT green again.
        let st = status(AmpReadings { ptt: true, antenna: 2, band: Some(6), ..Default::default() });
        let k = keys(&shown(&st));
        assert_eq!(k.ptt.tone, Tone::Green);
        assert_eq!(k.ant.unwrap().tone, Tone::Plain);
        assert_eq!(shown(&st).band(), (Some(6), false));
    }

    #[test]
    fn a_pending_off_never_lets_ptt_through_overdriven() {
        // SAFETY INTERLOCK #4: in line with 9 W of drive, OFF clicked and not
        // yet read back. The key now offers PTT again, and is greyed as if
        // the amplifier read OFF.
        let mut st = status(AmpReadings { ptt: true, drive_w: 9.0, ..Default::default() });
        st.pending = vec![AmpAction::SetPtt(false)];
        let k = keys(&shown(&st));
        assert_eq!(k.ptt.action, AmpAction::SetPtt(true));
        assert!(!k.ptt.enabled);
        // And a pending relay click is still greyed with RF on it (#2).
        let mut st =
            status(AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..Default::default() });
        st.pending = vec![AmpAction::SetAntenna(2)];
        assert!(!keys(&shown(&st)).ant.unwrap().enabled);
    }

    #[test]
    fn states_are_blue_or_green_and_only_a_failed_tune_is_red() {
        let st = status(AmpReadings { ptt: false, ..Default::default() });
        let k = keys(&shown(&st));
        assert_eq!(k.ptt.tone, Tone::Blue);
        assert_eq!(k.atu.as_ref().unwrap().tone, Tone::Green);
        let mut atu = AtuReadout { active: false, ..Default::default() };
        assert_eq!(atu_words(&atu), None);
        atu.tuning = true;
        assert_eq!(atu_words(&atu).unwrap().1, Tone::Blue);
        atu.tuning = false;
        atu.active = true;
        atu.last_result = Some("Failed".into());
        atu.network = Some(AtuNetwork { swr: Some(12.0), ..Default::default() });
        let (_, tone, swr) = atu_words(&atu).unwrap();
        assert_eq!((tone, swr.as_deref()), (Tone::Red, Some("> 10:1")));
    }

    #[test]
    fn the_tuner_cannot_be_switched_with_rf_on_it() {
        assert!(!rf_present(&AmpReadings::default()));
        assert!(rf_present(&AmpReadings { drive_w: 0.5, ..Default::default() }));
        // Output with no drive reading, as when the drive sample was missed.
        assert!(rf_present(&AmpReadings { fwd_w: 50.0, ..Default::default() }));
    }

    #[test]
    fn overlapping_needles_get_a_scale_that_parts_them() {
        // FWD 500 of 750 and DRV 10 of 15 lie on top of each other; DRV's
        // scale doubles to read 10 of 30, a third of the way round.
        assert_eq!(rescale(1.0, 500.0 / 750.0, 10.0, 15.0), 2.0);
        // DRV 3 of 15 under FWD 150 of 750: halving it parts them.
        assert_eq!(rescale(1.0, 0.2, 3.0, 15.0), 0.5);
        // Once rescaled it stays put while they are merely close…
        assert_eq!(rescale(0.5, 0.2, 2.2, 15.0), 0.5);
        // …and goes back once the usual scale is well clear.
        assert_eq!(rescale(0.5, 0.2, 5.0, 15.0), 1.0);
        // Apart already, or nothing to read: the usual scale.
        assert_eq!(rescale(1.0, 0.56, 9.0, 60.0), 1.0);
        assert_eq!(rescale(2.0, 0.0, 0.0, 15.0), 1.0);
    }

    #[test]
    fn barefoot_power_reads_in_tenths() {
        assert_eq!(watts(0.55), "0.6 W");
        assert_eq!(watts(5.04), "5.0 W");
        assert_eq!(watts(9.96), "10 W");
        assert_eq!(watts(480.4), "480 W");
    }

    #[test]
    fn rfl_names_its_own_scale() {
        // The 10 W range puts RFL on 2 W; the legend said "10 W" (AJ3G's
        // screenshot, 18 Oct 2026).
        let rfl = scale_note(AmpMeter::Rfl, Some(PowerRange::Ten), false);
        assert_eq!(rfl, ", 2 W scale");
        assert_eq!(
            rfl,
            format!(", {} W scale", AmpMeter::Rfl.range_full(PowerRange::Ten).unwrap())
        );
        let rfl = scale_note(AmpMeter::Rfl, Some(PowerRange::Hundred), false);
        assert_eq!(
            rfl,
            format!(", {} W scale", AmpMeter::Rfl.range_full(PowerRange::Hundred).unwrap())
        );
        assert_eq!(scale_note(AmpMeter::Drv, Some(PowerRange::Ten), false), ", 10 W scale");
    }

    #[test]
    fn swr_warns_at_the_tune_limit() {
        let at = |swr| AmpReadings { swr, ..AmpReadings::default() };
        assert!(swr_past(&at(22.5), 5.0));
        assert!(swr_past(&at(5.0), 5.0));
        assert!(!swr_past(&at(1.9), 5.0));
        // No SWR, no warning.
        assert!(!swr_past(&at(0.0), 0.5));
    }

    #[test]
    fn a_long_legend_is_set_smaller_to_clear_the_needles() {
        assert_eq!(legend_size(200.0, 260.0, 1.0), 12.0);
        assert_eq!(legend_size(300.0, 250.0, 1.0), 10.0);
        // Never below 8 points.
        assert_eq!(legend_size(900.0, 250.0, 1.0), 8.0);
        assert_eq!(legend_size(300.0, 250.0, 2.0), 20.0);
    }

    #[test]
    fn the_swr_warning_names_the_readings_it_covers() {
        let dial = |name, unsure| Dial {
            name,
            readout: None,
            value: 0.0,
            full: 10.0,
            zones: Zones::Below(0.0),
            live: true,
            note: "",
            source: "",
            unsure,
        };
        assert_eq!(unsure_warning(&dial("FWD", false), Some(&dial("DRV", false))), None);
        assert_eq!(
            unsure_warning(&dial("FWD", true), Some(&dial("DRV", false))).as_deref(),
            Some("FWD UNRELIABLE AT THIS SWR")
        );
        assert_eq!(
            unsure_warning(&dial("FWD", true), Some(&dial("RFL", true))).as_deref(),
            Some("FWD AND RFL UNRELIABLE AT THIS SWR")
        );
    }
}
