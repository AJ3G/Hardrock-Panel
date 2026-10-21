//! What the radio window does, apart from drawing it: typed frequencies, the
//! dial's digits, bands and their memories, the data modes, the transverter
//! offset, and the frames each control asks the radio link to send. Every
//! frame comes from the radio's [`Protocol`], so another radio's window does
//! the same with its own dialect.
//!
//! Nothing here changes the radio by itself: the window hands the frames to
//! the link, which refuses them while the radio transmits (interlock #5).

use std::collections::BTreeMap;

use rigcat::{Control, Frame, Kind, Protocol, Reply, RigStatus, Vfo};
use serde::{Deserialize, Serialize};

/// The FDM-DUO's coverage, 9 kHz to 54 MHz. With a transverter this is the
/// radio's own (IF) frequency, not the one on the LCD.
pub(crate) const IF_MIN: u64 = 9_000;
pub(crate) const IF_MAX: u64 = 54_000_000;

/// The fewest digits the LCD shows: 54 MHz to the hertz.
const MIN_DIGITS: usize = 9;

/// A frequency typed in: "14.074" is MHz, "7074" or "7074.5" is kHz (100 and
/// over without a unit), "14" alone is MHz. `mhz`, `khz`, `hz` and `ghz` may
/// follow. A comma counts as the point. `None` where it isn't a number.
pub(crate) fn parse_freq(text: &str) -> Option<u64> {
    let (v, unit) = parse_parts(text)?;
    Some(scale(v, unit.unwrap_or(if v < 100.0 { Unit::Mhz } else { Unit::Khz })))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Hz,
    Khz,
    Mhz,
    Ghz,
}

fn scale(v: f64, u: Unit) -> u64 {
    let k = match u {
        Unit::Hz => 1.0,
        Unit::Khz => 1e3,
        Unit::Mhz => 1e6,
        Unit::Ghz => 1e9,
    };
    (v * k).round() as u64
}

fn parse_parts(text: &str) -> Option<(f64, Option<Unit>)> {
    let t: String = text
        .trim()
        .replacen(',', ".", 1)
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let digits_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(digits_end);
    // One point at most, with digits either side of it.
    let mut parts = num.split('.');
    let whole = parts.next()?;
    let frac = parts.next();
    if whole.is_empty() || parts.next().is_some() || frac.is_some_and(|f| f.is_empty()) {
        return None;
    }
    let unit = match unit {
        "" => None,
        "hz" => Some(Unit::Hz),
        "khz" => Some(Unit::Khz),
        "mhz" => Some(Unit::Mhz),
        "ghz" => Some(Unit::Ghz),
        _ => return None,
    };
    Some((num.parse().ok()?, unit))
}

/// The radio's own frequency for what was typed, which is the frequency on
/// the LCD: with a transverter offset `xvtr` that is the RF frequency, and the
/// radio is sent `RF - xvtr`. Without a unit a number is read as
/// [`parse_freq`] reads it, and with an offset set, if that lands outside the
/// radio's coverage, the other way (so "144.174" on a 2 m converter is MHz).
pub(crate) fn entry_to_if(text: &str, xvtr: i64) -> Result<u64, String> {
    let range = || {
        if xvtr == 0 {
            "Enter 0.009 to 54 MHz, e.g. 14.074 or 7074".to_string()
        } else {
            format!(
                "Enter {} to {} MHz: the radio covers 9 kHz to 54 MHz before the converter",
                mhz_text(IF_MIN as i64 + xvtr),
                mhz_text(IF_MAX as i64 + xvtr)
            )
        }
    };
    let (Some(first), Some((v, unit))) = (parse_freq(text), parse_parts(text)) else {
        return Err(range());
    };
    let mut tries = vec![first];
    if unit.is_none() && xvtr != 0 {
        tries.push(scale(v, if v < 100.0 { Unit::Khz } else { Unit::Mhz }));
    }
    tries.into_iter().find_map(|hz| if_of(hz, xvtr)).ok_or_else(range)
}

/// The radio's frequency for `rf` on the LCD, if it covers it.
pub(crate) fn if_of(rf: u64, xvtr: i64) -> Option<u64> {
    let f = rf as i64 - xvtr;
    (f >= IF_MIN as i64 && f <= IF_MAX as i64).then_some(f as u64)
}

/// What the LCD shows for the radio's `if_hz`: plus the converter's offset.
pub(crate) fn shown(if_hz: u64, xvtr: i64) -> u64 {
    (if_hz as i64 + xvtr).max(0) as u64
}

fn mhz_text(hz: i64) -> String {
    format!("{:.3}", hz as f64 / 1e6)
}

/// How many digits the LCD has: enough for the highest frequency it can
/// show, and never fewer than nine.
pub(crate) fn digit_count(xvtr: i64) -> usize {
    shown(IF_MAX, xvtr).to_string().len().max(MIN_DIGITS)
}

/// The place value of digit `i` of `n`, counted from the left.
pub(crate) fn place(i: usize, n: usize) -> u64 {
    10u64.pow((n - 1 - i) as u32)
}

/// Whether digit `i` of `n` can be stepped: its place is within what the LCD
/// can show.
pub(crate) fn steppable(i: usize, n: usize, xvtr: i64) -> bool {
    place(i, n) <= shown(IF_MAX, xvtr)
}

/// The radio's frequency after stepping the LCD's digit with place value
/// `place` by `dir` (+1 up, -1 down). Carries roll into the next digit, as
/// on the radio's own tuning, and the result stays inside 9 kHz to 54 MHz.
pub(crate) fn step_digit(if_hz: u64, place: u64, dir: i64) -> u64 {
    (if_hz as i64 + dir * place as i64).clamp(IF_MIN as i64, IF_MAX as i64) as u64
}

/// The LCD's digits for `hz`, leading zeros as blanks.
pub(crate) fn digits(hz: u64, n: usize) -> Vec<Option<u8>> {
    let s = format!("{hz:>n$}");
    let s = &s[s.len() - n..];
    s.bytes().map(|b| b.is_ascii_digit().then(|| b - b'0')).collect()
}

/// An amateur band: name, edges and where it starts the first time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BandDef {
    pub name: &'static str,
    pub lo: u64,
    pub hi: u64,
    pub start: u64,
}

const fn band(name: &'static str, lo: u64, hi: u64, start: u64) -> BandDef {
    BandDef { name, lo, hi, start }
}

/// The bands the radio covers by itself (IARU Region 2).
pub(crate) const HF_BANDS: &[BandDef] = &[
    band("160", 1_800_000, 2_000_000, 1_840_000),
    band("80", 3_500_000, 4_000_000, 3_573_000),
    band("60", 5_330_500, 5_406_500, 5_357_000),
    band("40", 7_000_000, 7_300_000, 7_074_000),
    band("30", 10_100_000, 10_150_000, 10_136_000),
    band("20", 14_000_000, 14_350_000, 14_074_000),
    band("17", 18_068_000, 18_168_000, 18_100_000),
    band("15", 21_000_000, 21_450_000, 21_074_000),
    band("12", 24_890_000, 24_990_000, 24_915_000),
    band("10", 28_000_000, 29_700_000, 28_074_000),
    band("6", 50_000_000, 54_000_000, 50_313_000),
];

/// The bands a converter can reach, offered when the transverter offset is
/// set.
pub(crate) const XVTR_BANDS: &[BandDef] = &[
    band("4", 70_000_000, 70_500_000, 70_154_000),
    band("2", 144_000_000, 148_000_000, 144_174_000),
    band("1.25", 222_000_000, 225_000_000, 222_100_000),
    band("70 cm", 420_000_000, 450_000_000, 432_174_000),
    band("33 cm", 902_000_000, 928_000_000, 903_100_000),
    band("23 cm", 1_240_000_000, 1_300_000_000, 1_296_174_000),
    band("13 cm", 2_300_000_000, 2_450_000_000, 2_304_100_000),
    band("9 cm", 3_300_000_000, 3_500_000_000, 3_400_100_000),
    band("6 cm", 5_650_000_000, 5_925_000_000, 5_760_100_000),
    band("3 cm", 10_000_000_000, 10_500_000_000, 10_368_100_000),
];

impl BandDef {
    /// "20 m", "70 cm".
    pub(crate) fn label(&self) -> String {
        if self.name.ends_with("cm") { self.name.to_string() } else { format!("{} m", self.name) }
    }
}

/// The bands the BAND pop-out offers: 160 to 6 m with no converter; with
/// one, every band any part of which it brings inside the radio's coverage.
pub(crate) fn bands_for(xvtr: i64) -> Vec<&'static BandDef> {
    if xvtr == 0 {
        return HF_BANDS.iter().collect();
    }
    let (lo, hi) = (shown(IF_MIN, xvtr), shown(IF_MAX, xvtr));
    HF_BANDS.iter().chain(XVTR_BANDS).filter(|b| b.hi >= lo && b.lo <= hi).collect()
}

/// The band `rf` (on the LCD) is in, among those offered.
pub(crate) fn band_of(rf: u64, xvtr: i64) -> Option<&'static BandDef> {
    bands_for(xvtr).into_iter().find(|b| rf >= b.lo && rf <= b.hi)
}

/// The last frequency (as on the LCD) and mode used on a band.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BandMem {
    pub hz: u64,
    pub mode: String,
}

/// What the radio window keeps between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RadioPrefs {
    /// By band name.
    pub bands: BTreeMap<String, BandMem>,
    /// The LCD backlight in receive and in transmit, as an index into the
    /// rig theme's backlights.
    pub rx_light: usize,
    pub tx_light: usize,
    /// The transverter's local oscillator in Hz, signed: the LCD shows the
    /// radio's frequency plus this. 0 with no converter. A panel setting; the
    /// radio is never told.
    pub xvtr_hz: i64,
    /// The ten stored CW messages as last stored from here (the radio has no
    /// way to read them back).
    pub cw_msgs: Vec<String>,
    /// A 10 MHz reference is connected to the DUO's REF IN, as the operator
    /// says (the radio reports it over neither CAT nor USB as far as is
    /// known): lights REF on the LCD.
    pub ref_10mhz: bool,
}

impl Default for RadioPrefs {
    fn default() -> Self {
        RadioPrefs {
            bands: BTreeMap::new(),
            rx_light: 0,
            tx_light: 1,
            xvtr_hz: 0,
            cw_msgs: vec![String::new(); 10],
            ref_10mhz: false,
        }
    }
}

impl RadioPrefs {
    /// Keep `rf` and `mode` as the band's last, if it is in one. Called with
    /// the radio's read-back, so what is kept is where the radio really went.
    pub(crate) fn remember(&mut self, rf: u64, mode: &str) {
        if let Some(b) = band_of(rf, self.xvtr_hz) {
            let m = BandMem { hz: rf, mode: mode.to_string() };
            if self.bands.get(b.name) != Some(&m) {
                self.bands.insert(b.name.to_string(), m);
            }
        }
    }

    /// Where BAND goes for `b`: the data mode's frequency there when one is
    /// on; else the band's last frequency and mode; else its start, in LSB
    /// below 10 MHz and USB above.
    pub(crate) fn band_target(&self, b: &BandDef, digi: Option<&Digi>) -> (u64, String) {
        if let Some(d) = digi
            && let Some(f) = digi_on(d, b)
        {
            return (f, d.rig_mode(f).into());
        }
        match self.bands.get(b.name) {
            Some(m) if m.hz >= b.lo && m.hz <= b.hi => (m.hz, m.mode.clone()),
            _ => (b.start, if b.start < 10_000_000 { "LSB" } else { "USB" }.into()),
        }
    }
}

use crate::scope::digimodes;
pub(crate) use crate::scope::digimodes::Digi;

/// The data modes with a key on the face: the one table the scope's DATA
/// keys, its band-plan markers and DECODE's snap read too (decision 39).
pub(crate) fn digis() -> impl Iterator<Item = &'static Digi> {
    digimodes::keys()
}

pub(crate) fn digi(name: &str) -> Option<&'static Digi> {
    digimodes::digi(name)
}

/// The mode's dial on band `b`, if it has one there.
pub(crate) fn digi_on(d: &Digi, b: &BandDef) -> Option<u64> {
    d.dials.iter().copied().find(|f| (b.lo..=b.hi).contains(f))
}

/// The bands offered with converter `xvtr`, as a data mode's pick wants them.
pub(crate) fn pick_bands(xvtr: i64) -> Vec<digimodes::Band> {
    bands_for(xvtr)
        .into_iter()
        .map(|b| digimodes::Band { label: b.label(), lo: b.lo, hi: b.hi })
        .collect()
}

/// The receive filter for a data mode's passband `lo..hi` among the radio's
/// `filters`: a narrow data filter (1 kHz or less) where the passband fits
/// inside it, taken as centred on 1500 Hz (to check on the bench); else the
/// narrowest ordinary filter that reaches `hi`; else the widest.
pub(crate) fn digi_filter(filters: &[u32], lo: u32, hi: u32) -> Option<u32> {
    let fits = |w: u32| {
        if w <= 1000 { 1500 - w / 2 <= lo && hi <= 1500 + w / 2 } else { w >= hi }
    };
    filters.iter().copied().filter(|w| fits(*w)).min().or_else(|| filters.iter().copied().max())
}

/// Why the panel may not change the radio now, or `None` if it may.
///
/// Every control greys while this is `Some`. While the radio transmits the
/// link refuses the change anyway (SAFETY INTERLOCK #5, in
/// `crates/rigcat/src/link.rs`); greying the control means the panel never
/// relies on that refusal.
pub(crate) fn locked(s: &RigStatus) -> Option<&'static str> {
    if !s.connected {
        Some("The radio isn't connected.")
    } else if s.transmitting() {
        Some("The radio refuses changes while it transmits (interlock #5).")
    } else {
        None
    }
}

/// The VFO the radio receives on: B when it says so, else A.
pub(crate) fn rx_vfo(s: &RigStatus) -> Vfo {
    if s.vfo == Some(1) { Vfo::B } else { Vfo::A }
}

/// The receive VFO's frequency, as the radio last answered it.
pub(crate) fn dial_hz(s: &RigStatus) -> Option<u64> {
    match rx_vfo(s) {
        Vfo::A => s.freq_hz,
        Vfo::B => s.freq_b_hz,
    }
}

/// Retune the receive VFO to `if_hz`, and set `mode` too when given.
pub(crate) fn tune_frames(
    p: &dyn Protocol,
    s: &RigStatus,
    if_hz: u64,
    mode: Option<&str>,
) -> Result<Vec<Frame>, String> {
    let mut v =
        vec![p.set_freq(rx_vfo(s), if_hz).ok_or("That frequency is outside the radio's range.")?];
    if let Some(m) = mode {
        v.push(p.set_mode(m).ok_or_else(|| format!("The radio has no {m} mode."))?);
    }
    Ok(v)
}

/// A data mode: its frequency on the band the radio is on (or the nearest,
/// with a note, where it has none there; see [`Digi::pick`]), in its mode,
/// with its passband's filter and, if `usb_input`, the radio's transmit
/// audio on USB (`TI1` on the FDM-DUO). Also says where it went, as on the
/// LCD.
pub(crate) fn digi_frames(
    p: &dyn Protocol,
    s: &RigStatus,
    d: &Digi,
    xvtr: i64,
    usb_input: bool,
) -> Result<(Vec<Frame>, Option<String>), String> {
    let rf = dial_hz(s).map(|f| shown(f, xvtr)).ok_or("The radio's dial hasn't been read yet.")?;
    let pick = d
        .pick(rf, &pick_bands(xvtr), |f| if_of(f, xvtr).is_some())
        .ok_or_else(|| format!("No {} frequency inside the radio's reach.", d.name))?;
    Ok((digi_frames_at(p, s, d, pick.dial, xvtr, usb_input)?, pick.note))
}

/// [`digi_frames`] at a dial already chosen (`dial` as on the LCD): a
/// band-plan marker clicked, or DECODE's snap.
pub(crate) fn digi_frames_at(
    p: &dyn Protocol,
    s: &RigStatus,
    d: &Digi,
    dial: u64,
    xvtr: i64,
    usb_input: bool,
) -> Result<Vec<Frame>, String> {
    let if_hz = if_of(dial, xvtr).ok_or("That frequency is outside the radio's range.")?;
    let mode = d.rig_mode(dial);
    let mut v = tune_frames(p, s, if_hz, Some(mode))?;
    if let Some(w) = digi_filter(&p.filters(mode), d.lo, d.filter_width())
        && let Some(fr) = p.set_filter(mode, w)
    {
        v.push(fr);
    }
    if usb_input {
        v.push(usb_input_frame(p)?);
    }
    Ok(v)
}

/// The transmit audio input on USB, as a setting (`TI1;` on the FDM-DUO).
pub(crate) fn usb_input_frame(p: &dyn Protocol) -> Result<Frame, String> {
    let c = find(p, "TI").ok_or("This radio has no TX input setting.")?;
    p.set_control(c, "1", None)
}

/// Whether a data mode would change where the radio takes transmit audio
/// from: it isn't already on USB audio.
pub(crate) fn input_needs_change(s: &RigStatus) -> bool {
    s.value("TI") != Some("1")
}

/// A=B: VFO B to VFO A's frequency, as read.
pub(crate) fn a_to_b(p: &dyn Protocol, s: &RigStatus) -> Option<Frame> {
    p.set_freq(Vfo::B, s.freq_hz?)
}

/// The RIT offset in Hz, as the radio last answered `RV`.
pub(crate) fn rit_hz(s: &RigStatus) -> Option<i32> {
    s.value("RV")?.trim().trim_start_matches('+').parse().ok()
}

/// The receive filter's width in Hz, from the radio's `RF` answer in words
/// ("2800 Hz (USB)").
pub(crate) fn filter_hz(p: &dyn Protocol, s: &RigStatus) -> Option<u32> {
    let body = s.value("RF")?.to_string();
    let t = p.describe(&Reply { key: "RF".into(), body })?;
    t.split_once(" Hz")?.0.trim().parse().ok()
}

/// A filter width as the LCD shows it: "FIL 2.80k", "FIL 500".
pub(crate) fn filter_text(hz: u32) -> String {
    if hz >= 1000 { format!("FIL {:.2}k", hz as f32 / 1000.0) } else { format!("FIL {hz}") }
}

/// The receive level in dBm, from the radio's own S-meter answer in words.
pub(crate) fn s_dbm(p: &dyn Protocol, s: &RigStatus) -> Option<f32> {
    let body = s.value("SM")?.to_string();
    let t = p.describe(&Reply { key: "SM".into(), body })?;
    t.strip_suffix(" dBm")?.trim().parse().ok()
}

/// S-units on HF: S9 is -73 dBm, 6 dB a unit; over S9 in dB.
pub(crate) fn s_units(dbm: f32) -> String {
    if dbm <= -73.0 {
        format!("S{}", (9.0 + (dbm + 73.0) / 6.0).round().max(0.0) as i32)
    } else {
        format!("S9+{}", (dbm + 73.0).round() as i32)
    }
}

/// How many of `n` meter segments are lit for `dbm`: S0 none, S9 three
/// fifths, 3 dB a segment.
pub(crate) fn s_segments(dbm: f32, n: usize) -> usize {
    (((dbm + 127.0) / 3.0).round().max(0.0) as usize).min(n)
}

pub(crate) fn find(p: &dyn Protocol, cmd: &str) -> Option<&'static Control> {
    p.controls().iter().find(|c| c.cmd == cmd)
}

/// The control's value as the radio last answered it, without its prefix.
pub(crate) fn readback<'a>(c: &Control, s: &'a RigStatus) -> Option<&'a str> {
    s.value(c.cmd).map(|v| c.value_of(v))
}

/// Whether the panel can set `c` at all: it isn't read only, and the radio's
/// protocol will build a frame for it (the FDM-DUO's CAT baud won't).
pub(crate) fn settable(p: &dyn Protocol, c: &Control) -> bool {
    let probe = match c.kind {
        Kind::Toggle => "0".to_string(),
        Kind::Choice(o) => match o.first() {
            Some((v, _)) => v.to_string(),
            None => return false,
        },
        Kind::Range { lo, .. } => lo.to_string(),
        Kind::Text { .. } => String::new(),
        Kind::ReadOnly => return false,
    };
    p.set_control(c, &probe, None).is_ok()
}

/// The frame that sets `c` to `value`, padded as the radio last answered.
pub(crate) fn control_frame(
    p: &dyn Protocol,
    c: &Control,
    value: &str,
    s: &RigStatus,
) -> Result<Frame, String> {
    p.set_control(c, value, s.value(c.cmd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigcat::{FrameKind, RigModel};

    fn duo() -> Box<dyn Protocol> {
        RigModel::FdmDuo.protocol()
    }

    fn text(f: &Frame) -> String {
        String::from_utf8(f.bytes.clone()).unwrap()
    }

    fn texts(v: &[Frame]) -> Vec<String> {
        v.iter().map(text).collect()
    }

    fn rx(hz: u64, mode: &'static str) -> RigStatus {
        let mut s = RigStatus {
            connected: true,
            freq_hz: Some(hz),
            mode: Some(mode),
            tx_state: Some(0),
            vfo: Some(0),
            ..RigStatus::default()
        };
        s.values.insert("TI".into(), "0".into());
        s
    }

    #[test]
    fn typed_frequencies_are_mhz_below_100_and_khz_above() {
        assert_eq!(parse_freq("14.074"), Some(14_074_000));
        assert_eq!(parse_freq("14"), Some(14_000_000));
        assert_eq!(parse_freq("7074"), Some(7_074_000));
        assert_eq!(parse_freq("7074.5"), Some(7_074_500));
        assert_eq!(parse_freq(" 3,573 "), Some(3_573_000));
        assert_eq!(parse_freq("99.9"), Some(99_900_000));
        assert_eq!(parse_freq("100"), Some(100_000));
        assert_eq!(parse_freq("14074 khz"), Some(14_074_000));
        assert_eq!(parse_freq("475hz"), Some(475));
        assert_eq!(parse_freq("1.296GHz"), Some(1_296_000_000));
        for bad in ["", "abc", "14..0", "14.", ".5", "14 m", "-7"] {
            assert_eq!(parse_freq(bad), None, "{bad}");
        }
    }

    #[test]
    fn typed_entry_stays_inside_the_radios_coverage() {
        assert_eq!(entry_to_if("14.074", 0), Ok(14_074_000));
        assert_eq!(entry_to_if("9", 0), Ok(9_000_000));
        assert_eq!(entry_to_if("9khz", 0), Ok(9_000));
        assert!(entry_to_if("8khz", 0).is_err());
        assert!(entry_to_if("55", 0).is_err());
        // Without a converter 144.174 is kHz, as the radio covers it.
        assert_eq!(entry_to_if("144.174", 0), Ok(144_174));
    }

    #[test]
    fn with_a_converter_entry_is_the_rf_frequency_and_the_radio_gets_the_if() {
        // A 2 m converter with a 116 MHz LO: 144.174 on the LCD, 28.174 on the radio.
        let lo = 116_000_000;
        assert_eq!(entry_to_if("144.174", lo), Ok(28_174_000));
        assert_eq!(entry_to_if("144174", lo), Ok(28_174_000));
        assert_eq!(entry_to_if("144.174 mhz", lo), Ok(28_174_000));
        assert!(entry_to_if("14.074", lo).is_err());
        assert!(entry_to_if("200", lo).unwrap_err().contains("116.009"));
        // A down-converter: the LO subtracts.
        assert_eq!(entry_to_if("0.475", -10_000_000), Ok(10_475_000));
        assert_eq!(shown(28_174_000, lo), 144_174_000);
        assert_eq!(if_of(116_000_000, lo), None);
        assert_eq!(digit_count(0), 9);
        assert_eq!(digit_count(10_224_000_000), 11);
    }

    #[test]
    fn digits_step_with_carry_inside_9_khz_to_54_mhz() {
        let n = digit_count(0);
        assert_eq!(place(8, n), 1);
        assert_eq!(place(1, n), 10_000_000);
        // 14.099 999 up one hertz carries to 14.100 000.
        assert_eq!(step_digit(14_099_999, 1, 1), 14_100_000);
        assert_eq!(step_digit(14_100_000, 1_000, -1), 14_099_000);
        assert_eq!(step_digit(53_500_000, 1_000_000, 1), IF_MAX);
        assert_eq!(step_digit(12_000, 10_000, -1), IF_MIN);
        assert_eq!(step_digit(9_000, 1, -1), IF_MIN);
        // The 100 MHz digit is blank and can't be stepped without a converter.
        assert!(!steppable(0, n, 0) && steppable(1, n, 0));
        assert!(steppable(0, n, 116_000_000));
        assert_eq!(
            digits(14_074_000, 9),
            [None, Some(1), Some(4), Some(0), Some(7), Some(4), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn each_band_returns_to_its_last_frequency_and_mode() {
        let mut p = RadioPrefs::default();
        let b40 = band_of(7_100_000, 0).unwrap();
        assert_eq!(p.band_target(b40, None), (7_074_000, "LSB".to_string()));
        p.remember(7_030_000, "CW");
        p.remember(14_200_000, "USB");
        // Out of band: nothing kept.
        p.remember(15_000_000, "AM");
        assert_eq!(p.band_target(b40, None), (7_030_000, "CW".to_string()));
        let b20 = band_of(14_000_000, 0).unwrap();
        assert_eq!(p.band_target(b20, None), (14_200_000, "USB".to_string()));
        // A data mode on goes to its frequency instead.
        assert_eq!(p.band_target(b40, digi("FT8")), (7_074_000, "USB".to_string()));
        assert_eq!(p.bands.len(), 2);
        // Kept between runs.
        let back: RadioPrefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        let old: RadioPrefs = serde_json::from_str("{\"xvtr_hz\":116000000}").unwrap();
        assert_eq!((old.xvtr_hz, old.tx_light, old.cw_msgs.len()), (116_000_000, 1, 10));
    }

    #[test]
    fn the_band_list_is_hf_without_a_converter_and_what_it_reaches_with_one() {
        let names = |x| bands_for(x).iter().map(|b| b.name).collect::<Vec<_>>();
        assert_eq!(names(0).len(), 11);
        assert_eq!(names(0).last(), Some(&"6"));
        assert_eq!(names(116_000_000), ["2"]);
        assert_eq!(names(404_000_000), ["70 cm"]);
        assert!(band_of(144_174_000, 0).is_none());
        assert_eq!(band_of(144_174_000, 116_000_000).unwrap().label(), "2 m");
    }

    #[test]
    fn data_modes_set_usb_their_frequency_filter_and_usb_audio() {
        let d = duo();
        let s = rx(14_200_000, "LSB");
        let (f, note) = digi_frames(&*d, &s, digi("FT8").unwrap(), 0, true).unwrap();
        assert_eq!(texts(&f), ["FA00014074000;", "MD2;", "RF214;", "TI1;"]);
        assert!(note.is_none());
        assert!(f.iter().all(|f| f.kind == FrameKind::SetSetting && d.allowed(f)));
        // WSPR's 200 Hz fits the 300 Hz data filter; RTTY's 2420 Hz needs 2500.
        let (f, _) = digi_frames(&*d, &s, digi("WSPR").unwrap(), 0, false).unwrap();
        assert_eq!(texts(&f), ["FA00014095600;", "MD2;", "RF219;"]);
        let (f, _) = digi_frames(&*d, &s, digi("RTTY").unwrap(), 0, false).unwrap();
        assert_eq!(text(&f[2]), "RF209;");
        // No FT4 on 160 m: the nearest, 80 m, with a note.
        let (f, note) =
            digi_frames(&*d, &rx(1_840_000, "LSB"), digi("FT4").unwrap(), 0, false).unwrap();
        assert_eq!(text(&f[0]), "FA00003575500;");
        assert_eq!(note.as_deref(), Some("No FT4 frequency on 160 m, so 80 m"));
        // SSTV on 40 m is LSB.
        let (f, _) =
            digi_frames(&*d, &rx(7_100_000, "USB"), digi("SSTV").unwrap(), 0, false).unwrap();
        assert_eq!(texts(&f)[..2], ["FA00007171000;", "MD1;"]);
        // Through a 2 m converter the radio is sent the IF.
        let (f, _) =
            digi_frames(&*d, &rx(28_300_000, "USB"), digi("FT8").unwrap(), 116_000_000, false)
                .unwrap();
        assert_eq!(text(&f[0]), "FA00028174000;");
        assert!(input_needs_change(&s));
    }

    #[test]
    fn the_face_controls_build_their_frames_through_the_protocol() {
        let d = duo();
        let s = rx(7_074_000, "LSB");
        assert_eq!(
            texts(&tune_frames(&*d, &s, 7_030_000, Some("CW")).unwrap()),
            ["FA00007030000;", "MD3;"]
        );
        let mut b = s.clone();
        b.vfo = Some(1);
        assert_eq!(texts(&tune_frames(&*d, &b, 7_030_000, None).unwrap()), ["FB00007030000;"]);
        assert!(tune_frames(&*d, &s, 60_000_000, None).is_err());
        assert_eq!(text(&a_to_b(&*d, &s).unwrap()), "FB00007074000;");
        assert_eq!(text(&usb_input_frame(&*d).unwrap()), "TI1;");
        let nk = find(&*d, "NK").unwrap();
        let mut s2 = s.clone();
        s2.values.insert("NK".into(), "003".into());
        assert_eq!(text(&control_frame(&*d, nk, "5", &s2).unwrap()), "NK005;");
        assert_eq!(text(&control_frame(&*d, find(&*d, "NB").unwrap(), "1", &s).unwrap()), "NB1;");
        assert_eq!(text(&control_frame(&*d, find(&*d, "MT").unwrap(), "11", &s).unwrap()), "MT11;");
        assert_eq!(
            text(&control_frame(&*d, find(&*d, "TB").unwrap(), "05", &s).unwrap()),
            "TB005;"
        );
        assert_eq!(readback(nk, &s2), Some("03"));
        // Read only, and BR, which would cut the link, can't be set.
        assert!(!settable(&*d, find(&*d, "GS").unwrap()));
        assert!(!settable(&*d, find(&*d, "BR").unwrap()));
        assert!(settable(&*d, find(&*d, "PI").unwrap()));
        assert!(settable(&*d, find(&*d, "TE").unwrap()));
        // Every pop-out control the face asks for is in the radio's table.
        for cmd in ["VM", "VA", "GC", "GS", "NB", "NK", "NR", "NC", "NO", "SQ", "RF", "MT"]
            .iter()
            .chain(&["CS", "PI", "VT", "IA", "KT", "CI", "CD", "HT", "ET", "CK", "DE", "WT"])
            .chain(&["MG", "CG", "NT", "TB", "FD", "TE", "TT"])
        {
            assert!(find(&*d, cmd).is_some(), "{cmd}");
        }
    }

    #[test]
    fn everything_greys_while_transmitting() {
        let mut s = rx(14_074_000, "USB");
        assert_eq!(locked(&s), None);
        s.tx_state = Some(1);
        assert!(locked(&s).unwrap().contains("interlock #5"));
        s.tx_state = Some(0);
        s.fwd = Some(rigcat::Watts { w: 5.0, valid: true });
        assert!(locked(&s).is_some());
        assert!(locked(&RigStatus::default()).unwrap().contains("connected"));
    }

    #[test]
    fn the_lcd_shows_the_filter_width() {
        let p = rigcat::RigModel::FdmDuo.protocol();
        let mut s = RigStatus::default();
        assert_eq!(filter_hz(&*p, &s), None);
        s.values.insert("RF".into(), "112".into());
        assert_eq!(filter_hz(&*p, &s), Some(2800));
        assert_eq!(filter_text(2800), "FIL 2.80k");
        assert_eq!(filter_text(500), "FIL 500");
    }

    #[test]
    fn the_meter_reads_the_radios_s_meter() {
        let d = duo();
        let mut s = rx(14_074_000, "USB");
        s.values.insert("SM".into(), "00011".into());
        assert_eq!(s_dbm(&*d, &s), Some(-73.0));
        assert_eq!(s_units(-73.0), "S9");
        assert_eq!(s_units(-53.0), "S9+20");
        assert_eq!(s_units(-127.0), "S0");
        assert_eq!(s_segments(-73.0, 30), 18);
        assert_eq!(s_segments(-200.0, 30), 0);
        assert_eq!(s_segments(0.0, 30), 30);
        s.values.insert("RV".into(), "-00500".into());
        assert_eq!(rit_hz(&s), Some(-500));
    }
}
