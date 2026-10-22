//! The skimmer's labels (decision 54): each station DECODE heard, by its
//! callsign, at its frequency on the spectrum and the waterfall, as
//! SDRoxide's skimmers label the signals they read.
//!
//! DECODE hands the scope its newest decodes every frame
//! ([`Scope::skim`](super::Scope::skim)); here they are thinned to the
//! newest one of each station, faded by how many slots ago it was heard,
//! and laid out in lanes along the top of the waterfall so that labels of
//! nearby signals stack rather than overlap.

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
    /// The UTC start of the slot it was sent in.
    pub utc: f64,
}

impl SkimSpot {
    /// It is calling CQ.
    pub fn cq(&self) -> bool {
        self.text.starts_with("CQ ")
    }
}

/// What DECODE gives the scope each frame: its decodes and the mode's
/// period, while a slotted mode runs with SKIM on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkimFeed {
    pub period_s: f64,
    pub spots: Vec<SkimSpot>,
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
        let Some(a) = alpha(age_slots(s.utc, now, feed.period_s)) else { continue };
        let same = |o: &(SkimSpot, f32)| o.0.call == s.call && o.0.dial_hz == s.dial_hz;
        match out.iter_mut().find(|o| same(o)) {
            Some(o) if o.0.utc < s.utc => *o = (s.clone(), a),
            Some(_) => {}
            None => out.push((s.clone(), a)),
        }
    }
    out
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
        let feed = SkimFeed { period_s: 15.0, spots: vec![spot("K1ABC", 30.0, 1.0), wide] };
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
}
