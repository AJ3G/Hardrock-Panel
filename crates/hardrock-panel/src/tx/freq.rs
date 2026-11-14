//! TX freq AUTO: the quietest place in the audio passband for our FT8
//! over (decision 118), from the signals decoded in the slots we transmit
//! in (our own period, where we would land on top of someone) and from the
//! signal energy the receiver heard there that nothing decoded
//! ([`super::occupy`], decision 115).
//!
//! Each thing heard is a [`Mark`]: a span of the passband with a weight,
//! more for a stronger signal and for a more recent slot. Every 10 Hz spot
//! is scored by the marks our ~50 Hz over would sit on or beside, and AUTO
//! goes to the lowest score, as SDRoxide's `clearest_tx_hz` does: on a
//! crowded band with no clear gap at all it still finds the least busy
//! place, rather than staying wherever it was.

/// The audio range AUTO keeps to, Hz.
pub(crate) const LO_HZ: f32 = 200.0;
pub(crate) const HI_HZ: f32 = 2900.0;
/// An FT8 signal's width above its tone 0, Hz (8 tones of 6.25 Hz).
pub(crate) const SIGNAL_HZ: f32 = 50.0;
/// The radio's transmit filter (menu 35 TX BW, decision 125): AUTO keeps
/// the whole signal at least this far below its high edge, Hz, where the
/// filter is still flat (AJ3G, 14 Nov 2026: a tone at 2760 Hz on 100-2700
/// gave almost no power) ...
pub(crate) const FILTER_TOP_HZ: f32 = 300.0;
/// ... and tone 0 at least this far above its low edge, Hz ...
pub(crate) const FILTER_LOW_HZ: f32 = 100.0;
/// ... unless the high edge is at least this, Hz, when AUTO's own top
/// ([`HI_HZ`]) is well inside it already.
pub(crate) const FILTER_WIDE_HZ: u32 = 3500;

/// The audio AUTO searches, Hz: tone 0 from `lo`, the top of the signal up
/// to `hi`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Range {
    pub lo: f32,
    pub hi: f32,
}

/// AUTO's whole range, as before decision 125.
pub(crate) const FULL: Range = Range { lo: LO_HZ, hi: HI_HZ };

impl Range {
    /// The range a transmit filter from `bw.0` to `bw.1` Hz leaves (decision
    /// 125): the top [`FILTER_TOP_HZ`] under its high edge when that is
    /// under [`FILTER_WIDE_HZ`], tone 0 [`FILTER_LOW_HZ`] over its low edge,
    /// never wider than [`FULL`]. Not known: [`FULL`].
    pub(crate) fn for_filter(bw: Option<(u32, u32)>) -> Range {
        let Some((lo, hi)) = bw else { return FULL };
        let top = if hi < FILTER_WIDE_HZ { HI_HZ.min(hi as f32 - FILTER_TOP_HZ) } else { HI_HZ };
        Range { lo: LO_HZ.max(lo as f32 + FILTER_LOW_HZ), hi: top }
    }

    /// The highest tone 0 in the range, Hz.
    pub(crate) fn top_tone(self) -> f32 {
        self.hi - SIGNAL_HZ
    }

    /// Whether a tone 0 at `x` keeps the whole signal in the range.
    pub(crate) fn holds(self, x: f32) -> bool {
        (self.lo..=self.top_tone()).contains(&x)
    }

    /// Room for at least one spot.
    fn usable(self) -> bool {
        self.top_tone() >= self.lo
    }
}

/// The grid AUTO searches on, Hz.
const STEP_HZ: f32 = 10.0;
/// A little room either side of a heard signal, Hz: inside it counts as
/// on top of the signal.
const GUARD_HZ: f32 = 5.0;
/// A signal nearer than this, Hz, counts a little ([`NEAR_W`] at most,
/// fading with distance), so AUTO prefers the middle of a gap to its edge.
const NEAR_HZ: f32 = 100.0;
const NEAR_W: f32 = 0.2;

/// A decode at least this strong (SNR, dB) is remembered for
/// [`STRONG_KEEP_S`]; weaker ones for [`KEEP_S`] (decision 115).
pub(crate) const STRONG_DB: f32 = -5.0;
pub(crate) const STRONG_KEEP_S: i64 = 300;
pub(crate) const KEEP_S: i64 = 120;

/// How much each older slot of our period counts against the newest.
const DECAY: f32 = 0.5;
/// What a strong station counts for once past fresh activity's 2 minutes
/// (and at least, inside them): less than anything heard in the last slot.
const MEMORY_W: f32 = 0.15;
/// AUTO stays where it is while its score is within this much of the best
/// (absolute, plus a share of the best), so it does not wander.
const STAY_ABS: f32 = 0.1;
const STAY_REL: f32 = 0.2;
/// The score the scope's shading is darkest at.
const SHADE_FULL: f32 = 1.5;

/// How long a signal decoded at `snr_db` is remembered, s: a strong
/// station longer, as it is likely still there in the slots we did not
/// hear it in (we transmit in them, or it was not decoded).
pub(crate) fn keep_s(snr_db: f32) -> i64 {
    if snr_db >= STRONG_DB { STRONG_KEEP_S } else { KEEP_S }
}

/// Something heard in our period: a span of audio Hz and how much it
/// counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    pub lo: f32,
    pub hi: f32,
    pub w: f32,
}

/// How much a signal of `snr_db` heard in the slot starting `slot` counts
/// at `now`, UTC s. The newest slot of our period counts 1 (for a −10 dB
/// signal), each older one half as much; a strong station, once past
/// that, [`MEMORY_W`]. Stronger counts more: 0.3 for the weakest, 2 at
/// +10 dB.
pub(crate) fn weight(slot: i64, snr_db: f32, now: f64) -> f32 {
    let age = now - slot as f64;
    // Slots of one period come every 30 s.
    let back = (age / 30.0).floor().max(0.0) as i32;
    let fresh = if age <= KEEP_S as f64 { DECAY.powi(back) } else { 0.0 };
    let memory = if snr_db >= STRONG_DB { MEMORY_W } else { 0.0 };
    let strength = (1.0 + (snr_db + 10.0) / 20.0).clamp(0.3, 2.0);
    fresh.max(memory) * strength
}

impl Mark {
    /// A decode: tone 0 at `hz`.
    pub(crate) fn decode(slot: i64, hz: f32, snr_db: f32, now: f64) -> Self {
        Mark { lo: hz, hi: hz + SIGNAL_HZ, w: weight(slot, snr_db, now) }
    }

    /// Undecoded energy from `lo` to `hi` Hz, `over_db` above the slot's
    /// floor at its peak ([`super::occupy`]): about −10 dB SNR at its 6 dB
    /// threshold, a dB more for each dB over.
    pub(crate) fn energy(slot: i64, lo: f32, hi: f32, over_db: f32, now: f64) -> Self {
        let snr = -10.0 + (over_db - super::occupy::OVER_DB);
        Mark { lo, hi, w: weight(slot, snr, now) }
    }
}

/// The score of sending with tone 0 at `x`, Hz: each mark by its weight,
/// times how much of our signal (with its guard) it covers, plus a little
/// for one nearby.
pub(crate) fn score(marks: &[Mark], x: f32) -> f32 {
    let (a, b) = (x - GUARD_HZ, x + SIGNAL_HZ + GUARD_HZ);
    marks
        .iter()
        .map(|m| {
            let over = (b.min(m.hi) - a.max(m.lo)).max(0.0) / (b - a);
            let gap = (m.lo - b).max(a - m.hi).max(0.0);
            m.w * (over + NEAR_W * (1.0 - gap / NEAR_HZ).max(0.0))
        })
        .sum()
}

/// The tone-0 spots AUTO may choose in `r`, Hz.
fn spots(r: Range) -> impl Iterator<Item = f32> {
    let n = ((r.hi - SIGNAL_HZ - r.lo) / STEP_HZ).floor() as i32;
    (0..=n).map(move |i| r.lo + i as f32 * STEP_HZ)
}

/// Where to send (tone 0, Hz) in `range` ([`FULL`] if it has no room): the
/// spot with the lowest [`score`], the nearest `current` of equals;
/// `current` itself while it is in range and within a small margin of the
/// best. Always somewhere.
pub(crate) fn pick(marks: &[Mark], current: f32, range: Range) -> f32 {
    let range = if range.usable() { range } else { FULL };
    let scored: Vec<(f32, f32)> = spots(range).map(|x| (x, score(marks, x))).collect();
    let best = scored.iter().map(|s| s.1).fold(f32::INFINITY, f32::min);
    if range.holds(current) && score(marks, current) <= best + STAY_ABS + STAY_REL * best {
        return current;
    }
    scored
        .iter()
        .filter(|s| s.1 <= best + 1e-4)
        .min_by(|p, q| (p.0 - current).abs().total_cmp(&(q.0 - current).abs()))
        .map_or(current, |s| s.0)
}

/// The scope's shading: spans of audio Hz with how busy (0 to 1) each is,
/// in 10 Hz steps across AUTO's `range`, from the marks covering each
/// (with their guard). Neighbouring steps of the same shade are merged.
pub(crate) fn shade(marks: &[Mark], range: Range) -> Vec<(f32, f32, f32)> {
    let range = if range.usable() { range } else { FULL };
    let n = ((range.hi - range.lo) / STEP_HZ).round() as i32;
    let mut out: Vec<(f32, f32, f32)> = Vec::new();
    for i in 0..n {
        let (a, b) = (range.lo + i as f32 * STEP_HZ, range.lo + (i + 1) as f32 * STEP_HZ);
        let mid = 0.5 * (a + b);
        let s: f32 = marks
            .iter()
            .filter(|m| mid >= m.lo - GUARD_HZ && mid <= m.hi + GUARD_HZ)
            .map(|m| m.w)
            .sum();
        // In eighths, so a run of one shade is one span.
        let v = ((s / SHADE_FULL).min(1.0) * 8.0).ceil() / 8.0;
        if v <= 0.0 {
            continue;
        }
        match out.last_mut() {
            Some(l) if l.1 == a && l.2 == v => l.1 = b,
            _ => out.push((a, b, v)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 10_000.0;
    /// The newest slot of our period, 14 s ago.
    const NEW: i64 = 9_986;

    fn d(hz: f32, slot: i64, snr: f32) -> Mark {
        Mark::decode(slot, hz, snr, NOW)
    }

    fn pick_full(marks: &[Mark], current: f32) -> f32 {
        pick(marks, current, FULL)
    }

    /// Decision 125: AJ3G's DUO on TX BW 100-2700 sent almost nothing at
    /// 2760 Hz. AUTO keeps the whole signal 300 Hz under the filter's top,
    /// and tone 0 100 Hz over its bottom; at 3500 and 4000 nothing changes.
    #[test]
    fn auto_keeps_inside_the_radios_transmit_filter() {
        let r = Range::for_filter(Some((100, 2700)));
        assert_eq!((r.lo, r.top_tone()), (200.0, 2350.0));
        assert_eq!(Range::for_filter(Some((100, 3000))).top_tone(), 2650.0);
        assert_eq!(Range::for_filter(Some((300, 3000))).lo, 400.0);
        assert_eq!(Range::for_filter(Some((100, 3500))), FULL);
        assert_eq!(Range::for_filter(Some((50, 4000))), FULL);
        assert_eq!(Range::for_filter(None), FULL);
        // The band busy below 2400 Hz, quiet above: on the full range AUTO
        // goes up there, on 100-2700 it stays under 2350 Hz.
        let busy: Vec<Mark> = (0..40).map(|i| d(200.0 + 55.0 * i as f32, NEW, 0.0)).collect();
        assert!(pick(&busy, 1500.0, FULL) > 2400.0);
        let p = pick(&busy, 1500.0, r);
        assert!(r.holds(p) && p <= 2350.0, "{p}");
        // Sitting above the cap (the filter was just read): moves in.
        assert!(pick(&[], 2760.0, r) <= 2350.0);
        assert_eq!(pick(&[], 1500.0, r), 1500.0);
        // The shading covers AUTO's range only.
        let s = shade(&[d(2500.0, NEW, 0.0), d(1000.0, NEW, 0.0)], r);
        assert!(s.iter().all(|x| x.0 >= r.lo && x.1 <= r.hi), "{s:?}");
        assert!(!s.is_empty());
        // No room at all: the full range, rather than nowhere.
        let none = Range { lo: 2000.0, hi: 1900.0 };
        assert!(FULL.holds(pick(&[], 2000.0, none)));
    }

    #[test]
    fn auto_stays_where_it_is_quiet_and_moves_when_someone_lands_on_it() {
        // Quiet where it is.
        assert_eq!(pick_full(&[d(600.0, NEW, -10.0), d(1800.0, NEW, -10.0)], 1200.0), 1200.0);
        // Nothing heard: stays put.
        assert_eq!(pick_full(&[], 1500.0), 1500.0);
        // Someone landed on it: moves clear, to the nearest quiet spot.
        let p = pick_full(&[d(1000.0, NEW, -10.0)], 1000.0);
        assert!((p - 1000.0).abs() >= NEAR_HZ + SIGNAL_HZ, "{p}");
        assert!((p - 1000.0).abs() <= NEAR_HZ + SIGNAL_HZ + 2.0 * GUARD_HZ + STEP_HZ, "{p}");
        // Out of range: moves in.
        assert!((LO_HZ..=HI_HZ - SIGNAL_HZ).contains(&pick_full(&[], 2890.0)));
    }

    #[test]
    fn a_crowded_band_still_gives_the_least_busy_spot() {
        // Signals every 55 Hz across the whole range (no gap of 60 Hz
        // anywhere), all fresh, except one stretch where they were heard
        // three slots ago and weak.
        let marks: Vec<Mark> = (0..50)
            .map(|i| {
                let hz = 180.0 + 55.0 * i as f32;
                if (1400.0..1700.0).contains(&hz) {
                    d(hz, NEW - 90, -20.0)
                } else {
                    d(hz, NEW, 0.0)
                }
            })
            .collect();
        let p = pick_full(&marks, 600.0);
        assert!((1400.0..1650.0).contains(&p), "{p}");
        // Even a band full of identical signals gives a spot.
        let full: Vec<Mark> = (0..60).map(|i| d(150.0 + 50.0 * i as f32, NEW, 0.0)).collect();
        assert!((LO_HZ..=HI_HZ).contains(&pick_full(&full, 1500.0)));
    }

    #[test]
    fn recency_moves_away_from_the_newest_signal() {
        // The band busy every 55 Hz a slot ago; now someone new on us.
        let mut marks: Vec<Mark> =
            (0..50).map(|i| d(180.0 + 55.0 * i as f32, NEW - 30, -10.0)).collect();
        let ours = 180.0 + 55.0 * 20.0 + 25.0;
        marks.push(d(ours, NEW, -10.0));
        let p = pick_full(&marks, ours);
        assert!((p - ours).abs() > SIGNAL_HZ, "{p}");
        // The same spot is scored worse than when only old activity was
        // there.
        assert!(score(&marks, ours) > score(&marks[..50], ours) + 0.5);
    }

    #[test]
    fn auto_stays_put_when_near_best() {
        // A weak old signal beside us, the band otherwise empty: our score
        // is a little over the best, within the margin.
        let marks = [d(1300.0, NEW - 60, -20.0)];
        assert!(score(&marks, 1380.0) > 0.0);
        assert_eq!(pick_full(&marks, 1380.0), 1380.0);
        // A fresh strong one in the same place: we move.
        assert_ne!(pick_full(&[d(1300.0, NEW, 5.0)], 1380.0), 1380.0);
    }

    #[test]
    fn weights_favour_recent_and_strong() {
        assert!(weight(NEW, -10.0, NOW) > weight(NEW - 30, -10.0, NOW));
        assert!(weight(NEW, 0.0, NOW) > weight(NEW, -20.0, NOW));
        // Strong ones are remembered to 5 minutes, at less than fresh.
        let old = weight(NEW - 200, 0.0, NOW);
        assert!(old > 0.0 && old < weight(NEW, -20.0, NOW), "{old}");
        assert_eq!(weight(NEW - 200, -10.0, NOW), 0.0);
    }

    #[test]
    fn strong_stations_are_remembered_longer() {
        assert_eq!(keep_s(-5.0), 300);
        assert_eq!(keep_s(12.0), 300);
        assert_eq!(keep_s(-6.0), 120);
        assert_eq!(keep_s(-20.0), 120);
    }

    #[test]
    fn shading_is_darker_where_busier() {
        let marks = [d(1000.0, NEW, 10.0), d(1500.0, NEW - 60, -10.0)];
        let s = shade(&marks, FULL);
        let at = |hz: f32| s.iter().find(|x| x.0 <= hz && hz < x.1).map_or(0.0, |x| x.2);
        assert!(at(1020.0) > at(1520.0), "{s:?}");
        assert!(at(1520.0) > 0.0);
        assert_eq!(at(1250.0), 0.0);
        assert!(s.iter().all(|x| (0.0..=1.0).contains(&x.2)));
    }

    #[test]
    fn undecoded_energy_counts_too() {
        // Energy at 1200 to 1260 Hz that nothing decoded, 20 dB over the
        // floor: AUTO moves off 1220 Hz.
        let m = [Mark::energy(NEW, 1200.0, 1260.0, 20.0, NOW)];
        let p = pick_full(&m, 1220.0);
        assert!(!(1145.0..1265.0).contains(&p), "{p}");
        assert_eq!(pick_full(&m, 1600.0), 1600.0);
    }
}
