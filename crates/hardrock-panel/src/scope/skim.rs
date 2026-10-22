//! The skimmer's labels (decision 54): each station DECODE heard, by its
//! callsign, at its frequency on the spectrum and the waterfall, as
//! SDRoxide's skimmers label the signals they read.
//!
//! DECODE hands the scope its newest decodes every frame
//! ([`Scope::skim`](super::Scope::skim)); here they are thinned to the
//! newest one of each station, faded by how many slots ago it was heard,
//! and laid out in lanes along the top of the waterfall so that labels of
//! nearby signals stack rather than overlap.
//!
//! The keyboard skimmer's signals (decision 56) come in the same feed, each
//! with a [`KeyTag`]: their labels carry the mode, and fade over
//! [`key_fade_s`] once the signal stops rather than by slots.

pub use decoders::WsjtMode;
pub use decoders::keyskim::KeyMode;

/// A keyboard skimmer's signal: its mode, CW's speed, and whether it is
/// being heard now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyTag {
    pub mode: KeyMode,
    pub wpm: u16,
    pub active: bool,
}

/// One decode to label.
#[derive(Debug, Clone, PartialEq)]
pub struct SkimSpot {
    /// The DECODE line it came from (`None` for the wide skimmer's, which
    /// are not in DECODE's list).
    pub row: Option<u64>,
    /// Where the signal is, the radio's own frequency, Hz.
    pub hz: f64,
    /// Its audio frequency from its dial, Hz (DECODE's AF column).
    pub audio_hz: f32,
    /// The dial it was heard on, the radio's own Hz: the receiver's for
    /// DECODE's own, another segment's for the wide skimmer's.
    pub dial_hz: f64,
    pub call: String,
    pub text: String,
    pub snr: f32,
    /// The UTC start of the slot it was sent in; for the keyboard
    /// skimmer's, when it was last heard.
    pub utc: f64,
    /// The WSJT mode it was heard in (SKIM WIDE hears FT4 beside FT8 and
    /// FT8 beside FT4); `None` for the keyboard skimmer's.
    pub wsjt: Option<WsjtMode>,
    /// The keyboard skimmer's signals (decision 56): `None` for FT8's.
    pub key: Option<KeyTag>,
}

impl SkimSpot {
    /// It is calling CQ: a WSJT message that starts so, or "CQ" among a
    /// keyboard signal's words.
    pub fn cq(&self) -> bool {
        match self.key {
            Some(_) => self.text.split_whitespace().any(|w| w == "CQ"),
            None => self.text.starts_with("CQ "),
        }
    }

    /// The label's text: the callsign, and a keyboard signal's mode.
    /// Its label: the call and the mode it was heard in (AJ3G: label the
    /// FT4 station "as FT4"), for WSJT's as for the keyboard skimmer's.
    pub fn tag(&self) -> String {
        match (self.key, self.wsjt) {
            (Some(k), _) => format!("{} {}", self.call, k.mode.label()),
            (None, Some(m)) => format!("{} {}", self.call, m.label()),
            (None, None) => self.call.clone(),
        }
    }
}

/// What DECODE gives the scope each frame: its decodes and the mode's
/// period, while a slotted mode runs with SKIM on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkimFeed {
    pub period_s: f64,
    pub spots: Vec<SkimSpot>,
    /// DECODE's audio frequency for PSK31 and RTTY: a keyboard label
    /// clicked tunes so its signal lands there.
    pub key_audio_hz: f64,
}

/// How long a keyboard signal's label fades once it stops, s: CW's twice
/// PSK31's and RTTY's (AJ3G: "Increase persistence on the spot by double…
/// Just double on CW").
pub const KEY_FADE_S: f64 = 5.0;
pub const CW_FADE_S: f64 = 2.0 * KEY_FADE_S;

pub fn key_fade_s(mode: KeyMode) -> f64 {
    if mode == KeyMode::Cw { CW_FADE_S } else { KEY_FADE_S }
}

/// A keyboard signal's label's opacity: full while it is heard, then fading
/// over [`key_fade_s`] from when it was last heard, then gone.
pub fn key_alpha(mode: KeyMode, heard_utc: f64, active: bool, now: f64) -> Option<f32> {
    if active {
        return Some(1.0);
    }
    let (t, fade) = ((now - heard_utc).max(0.0), key_fade_s(mode));
    (t < fade).then(|| (1.0 - 0.8 * t / fade) as f32)
}

/// Slots a label stays for: the newest slot's, and two before it, fading.
pub const SLOTS_SHOWN: u32 = 3;

/// A label's opacity, `slots` slots after the newest: full, then fading, then
/// gone.
pub fn alpha(slots: u32) -> Option<f32> {
    match slots {
        0 => Some(1.0),
        1 => Some(0.6),
        2 => Some(0.35),
        _ => None,
    }
}

/// How many slots old a decode of the slot starting at `utc` is at `now`:
/// a slot's decodes come at its end, so they are 0 through the next slot.
pub fn age_slots(utc: f64, now: f64, period: f64) -> u32 {
    if period <= 0.0 {
        return u32::MAX;
    }
    (((now - utc) / period).floor() - 1.0).max(0.0) as u32
}

/// The labels to show at `now`: the newest decode of each station (on each
/// dial), with its opacity; stations last heard more than
/// [`SLOTS_SHOWN`] slots ago are left out.
pub fn labels(feed: &SkimFeed, now: f64) -> Vec<(SkimSpot, f32)> {
    let mut out: Vec<(SkimSpot, f32)> = Vec::new();
    for s in &feed.spots {
        let a = match s.key {
            Some(k) => key_alpha(k.mode, s.utc, k.active, now),
            // By its own mode's slots: SKIM WIDE's FT8 beside FT4 fades
            // as FT8 does.
            None => alpha(age_slots(s.utc, now, s.wsjt.map_or(feed.period_s, |m| m.period_s()))),
        };
        let Some(a) = a else { continue };
        let same = |o: &(SkimSpot, f32)| o.0.call == s.call && o.0.dial_hz == s.dial_hz;
        match out.iter_mut().find(|o| same(o)) {
            Some(o) if o.0.utc < s.utc => *o = (s.clone(), a),
            Some(_) => {}
            None => out.push((s.clone(), a)),
        }
    }
    out
}

/// Where a keyboard signal's label clicked puts the receiver: its mode,
/// passband and dial (the radio's own Hz). CW on its carrier at the CW
/// passband's middle (the pitch); PSK31 and RTTY on USB with the signal at
/// DECODE's audio frequency `audio_hz`, so DECODE reads it there.
pub fn key_tune(hz: f64, mode: KeyMode, audio_hz: f64) -> (sdr::Mode, (f32, f32), f64) {
    match mode {
        KeyMode::Cw => {
            let pb = sdr::Mode::Cw.default_passband();
            let pitch = 0.5 * (pb.0 + pb.1) as f64;
            (sdr::Mode::Cw, pb, hz - pitch)
        }
        KeyMode::Psk | KeyMode::Rtty => {
            let half = if mode == KeyMode::Psk { 250.0 } else { 350.0 };
            let pb = ((audio_hz - half) as f32, (audio_hz + half) as f32);
            (sdr::Mode::Usb, pb, hz - audio_hz)
        }
    }
}

/// How far either side of a signal's middle the offset bar counts as on
/// it, Hz, and where that middle is from the label's frequency. A WSJT
/// label sits on the signal's lowest tone, as DECODE's AF column does, so
/// its middle is half its width above. A keyboard signal's frequency is its
/// middle: CW's carrier, PSK31's 31 Hz, RTTY's 170 Hz shift.
fn reach(s: &SkimSpot) -> (f64, f64) {
    match s.key.map(|k| k.mode) {
        None => {
            let half = 0.5 * width_hz(s.wsjt);
            (half, half + 5.0)
        }
        Some(KeyMode::Cw) | Some(KeyMode::Psk) => (0.0, 25.0),
        Some(KeyMode::Rtty) => (0.0, 100.0),
    }
}

/// A WSJT signal's width above its lowest tone, Hz: FT8's eight tones at
/// 6.25 Hz, FT4's four at 20.8 Hz; others counted narrow.
fn width_hz(m: Option<WsjtMode>) -> f64 {
    match m {
        Some(WsjtMode::Ft4) => 83.0,
        Some(WsjtMode::Ft8) => 50.0,
        _ => 10.0,
    }
}

/// The label the offset bar at `bar_hz` is on (AJ3G: "make it so I know
/// which station I am on"): of those whose signal the bar is over, the one
/// whose middle is nearest it.
pub fn on_bar(shown: &[(SkimSpot, f32)], bar_hz: f64) -> Option<usize> {
    shown
        .iter()
        .enumerate()
        .filter_map(|(i, (s, _))| {
            let (mid, half) = reach(s);
            let d = (bar_hz - (s.hz + mid)).abs();
            (d <= half).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Lanes a label may take, down from the waterfall's top.
pub const MAX_LANES: usize = 5;

/// Lay labels out in lanes: `boxes` are each label's signal x and its
/// box's width (the box starts `lead` to the right of the signal), in any
/// order. Each goes in the first lane whose last box ends left of its
/// signal, so a label never covers another's signal or box; one that finds
/// no lane in [`MAX_LANES`] is left out (`None`).
pub fn lanes(boxes: &[(f32, f32)], lead: f32, gap: f32) -> Vec<Option<usize>> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| boxes[a].0.total_cmp(&boxes[b].0));
    let mut right: Vec<f32> = Vec::new();
    let mut out = vec![None; boxes.len()];
    for i in order {
        let (x, w) = boxes[i];
        let lane = right.iter().position(|&r| x >= r).or_else(|| {
            (right.len() < MAX_LANES).then(|| {
                right.push(f32::NEG_INFINITY);
                right.len() - 1
            })
        });
        if let Some(l) = lane {
            right[l] = x + lead + w + gap;
            out[i] = Some(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(call: &str, utc: f64, hz: f64) -> SkimSpot {
        SkimSpot {
            row: Some(utc as u64),
            hz,
            audio_hz: (hz - 14_074_000.0) as f32,
            dial_hz: 14_074_000.0,
            call: call.into(),
            text: format!("CQ {call} FN42"),
            snr: -10.0,
            utc,
            wsjt: Some(WsjtMode::Ft8),
            key: None,
        }
    }

    #[test]
    fn a_slots_decodes_are_newest_through_the_next_slot_then_fade_and_go() {
        let p = 15.0;
        // Slot 0 to 15 decoded at about 16 s: the newest until 30 s.
        assert_eq!(age_slots(0.0, 16.0, p), 0);
        assert_eq!(age_slots(0.0, 29.9, p), 0);
        assert_eq!(age_slots(0.0, 30.0, p), 1);
        assert_eq!(age_slots(0.0, 50.0, p), 2);
        assert_eq!(alpha(0), Some(1.0));
        assert!(alpha(1).unwrap() > alpha(2).unwrap());
        assert_eq!(alpha(SLOTS_SHOWN), None);
        // A clock behind the slot does not make it older.
        assert_eq!(age_slots(30.0, 20.0, p), 0);
    }

    #[test]
    fn each_station_is_labelled_once_at_its_newest_decode() {
        let feed = SkimFeed {
            period_s: 15.0,
            spots: vec![
                spot("K1ABC", 30.0, 14_075_300.0),
                spot("W9XYZ", 30.0, 14_075_000.0),
                spot("K1ABC", 15.0, 14_075_200.0),
                spot("G4ABC", 0.0, 14_074_900.0),
            ],
            ..Default::default()
        };
        let l = labels(&feed, 46.0);
        assert_eq!(l.len(), 3);
        let k = l.iter().find(|(s, _)| s.call == "K1ABC").unwrap();
        assert_eq!((k.0.hz, k.1), (14_075_300.0, 1.0));
        let g = l.iter().find(|(s, _)| s.call == "G4ABC").unwrap();
        assert_eq!(g.1, alpha(2).unwrap());
        // Three slots on, all gone.
        assert!(labels(&feed, 46.0 + 45.0).is_empty());
        // The same call on another dial is another label.
        let mut wide = spot("K1ABC", 30.0, 18_101_000.0);
        wide.dial_hz = 18_100_000.0;
        wide.row = None;
        let feed = SkimFeed {
            period_s: 15.0,
            spots: vec![spot("K1ABC", 30.0, 1.0), wide],
            ..Default::default()
        };
        assert_eq!(labels(&feed, 46.0).len(), 2);
        assert!(feed.spots[0].cq());
    }

    #[test]
    fn labels_of_nearby_signals_stack_and_the_crowded_are_left_out() {
        // Three close together, one well clear.
        let b = [(100.0, 50.0), (120.0, 50.0), (300.0, 50.0), (110.0, 50.0)];
        let l = lanes(&b, 6.0, 4.0);
        assert_eq!(l, vec![Some(0), Some(2), Some(0), Some(1)]);
        // More in one place than there are lanes: the rest are left out.
        let crowd: Vec<(f32, f32)> = (0..8).map(|i| (100.0 + i as f32, 40.0)).collect();
        let l = lanes(&crowd, 6.0, 4.0);
        assert_eq!(l.iter().filter(|x| x.is_some()).count(), MAX_LANES);
        assert_eq!(l[MAX_LANES], None);
    }

    #[test]
    fn a_keyboard_signals_label_carries_its_mode_and_fades_once_it_stops() {
        let mut k = spot("W2XYZ", 100.0, 14_025_500.0);
        k.row = None;
        k.text = "CQ CQ DE W2XYZ W2XYZ K".into();
        k.key = Some(KeyTag { mode: KeyMode::Cw, wpm: 22, active: true });
        assert_eq!(k.tag(), "W2XYZ CW");
        let mut f = k.clone();
        f.key = None;
        f.wsjt = Some(WsjtMode::Ft4);
        assert_eq!(f.tag(), "W2XYZ FT4");
        assert!(k.cq());
        let feed = SkimFeed { period_s: 15.0, spots: vec![k.clone()], ..Default::default() };
        // Heard now: full, however long ago the slots went by.
        assert_eq!(labels(&feed, 500.0)[0].1, 1.0);
        // Stopped: fading, then gone after CW_FADE_S, twice PSK31's.
        k.key = Some(KeyTag { active: false, ..k.key.unwrap() });
        let mut psk = k.clone();
        psk.key = Some(KeyTag { mode: KeyMode::Psk, ..k.key.unwrap() });
        let feed = SkimFeed { spots: vec![k], ..feed };
        let a = labels(&feed, 102.0)[0].1;
        assert!(a < 1.0 && a > 0.2, "{a}");
        assert_eq!(labels(&feed, 100.0 + KEY_FADE_S + 1.0).len(), 1);
        assert!(labels(&feed, 100.0 + CW_FADE_S).is_empty());
        let p = SkimFeed { spots: vec![psk], ..feed.clone() };
        assert!(labels(&p, 100.0 + KEY_FADE_S).is_empty());
        // "CQ" must be a word of its own in a keyboard signal's text.
        let mut q = feed.spots[0].clone();
        q.text = "TNX QSO CQX".into();
        assert!(!q.cq());
    }

    #[test]
    fn the_offset_bar_is_on_the_station_whose_signal_it_crosses() {
        // FT8: labels on the lowest tone, the signal 50 Hz up from it.
        let shown =
            vec![(spot("K1ABC", 0.0, 14_075_000.0), 1.0), (spot("W9XYZ", 0.0, 14_075_060.0), 1.0)];
        assert_eq!(on_bar(&shown, 14_075_025.0), Some(0));
        assert_eq!(on_bar(&shown, 14_074_996.0), Some(0));
        assert_eq!(on_bar(&shown, 14_075_090.0), Some(1));
        // Between them, nearer W9XYZ's middle.
        assert_eq!(on_bar(&shown, 14_075_057.0), Some(1));
        assert_eq!(on_bar(&shown, 14_075_200.0), None);
        assert_eq!(on_bar(&shown, 14_074_900.0), None);
        // A CW signal is on its carrier.
        let mut cw = spot("DL1AA", 0.0, 14_030_000.0);
        cw.key = Some(KeyTag { mode: KeyMode::Cw, wpm: 25, active: true });
        let shown = vec![(cw, 1.0)];
        assert_eq!(on_bar(&shown, 14_029_980.0), Some(0));
        assert_eq!(on_bar(&shown, 14_030_040.0), None);
    }
}
