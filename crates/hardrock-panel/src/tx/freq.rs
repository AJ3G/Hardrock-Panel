//! TX freq AUTO: the clearest place in the audio passband for our FT8 over,
//! from the signals decoded in the slots we transmit in (our own period,
//! where we would land on top of someone), and from the signal energy the
//! receiver heard there that nothing decoded ([`super::occupy`], decision
//! 115).

/// The audio range AUTO keeps to, Hz.
pub(crate) const LO_HZ: f32 = 200.0;
pub(crate) const HI_HZ: f32 = 2900.0;
/// An FT8 signal's width above its tone 0, Hz (8 tones of 6.25 Hz).
pub(crate) const SIGNAL_HZ: f32 = 50.0;
/// The smallest clear gap AUTO will put an over in, Hz.
pub(crate) const NEED_HZ: f32 = 60.0;
/// A little room either side of a heard signal, Hz.
const GUARD_HZ: f32 = 5.0;

/// A decode at least this strong (SNR, dB) keeps its place busy for
/// [`STRONG_KEEP_S`]; weaker ones for [`KEEP_S`] (decision 115).
pub(crate) const STRONG_DB: f32 = -5.0;
pub(crate) const STRONG_KEEP_S: i64 = 300;
pub(crate) const KEEP_S: i64 = 120;

/// How long a signal decoded at `snr_db` counts as busy, s: a strong
/// station is remembered longer, as it is likely still there in the slots
/// we did not hear it in (we transmit in them, or it was not decoded).
pub(crate) fn keep_s(snr_db: f32) -> i64 {
    if snr_db >= STRONG_DB { STRONG_KEEP_S } else { KEEP_S }
}

/// The busy parts of the passband, merged: each signal heard (tone 0 at
/// its offset) with a guard either side, and `energy`, the spans where the
/// receiver saw signal that nothing decoded (each with the same guard).
pub(crate) fn busy(heard: &[f32], energy: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut v: Vec<(f32, f32)> = heard
        .iter()
        .map(|&a| (a - GUARD_HZ, a + SIGNAL_HZ + GUARD_HZ))
        .chain(energy.iter().map(|&(a, b)| (a - GUARD_HZ, b + GUARD_HZ)))
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f32, f32)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The clear gaps between `LO_HZ` and `HI_HZ`.
fn gaps(busy: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut from = LO_HZ;
    for &(a, b) in busy {
        if a > from {
            out.push((from, a.min(HI_HZ)));
        }
        from = from.max(b);
    }
    if from < HI_HZ {
        out.push((from, HI_HZ));
    }
    out.retain(|g| g.1 - g.0 >= NEED_HZ);
    out
}

/// Where to send (tone 0, Hz), around the `busy` spans ([`busy`]):
/// `current` while it sits clear in a gap of at least `NEED_HZ`, else the
/// middle of the widest gap (the nearer, of two as wide). `None` when there
/// is no such gap.
pub(crate) fn pick(busy: &[(f32, f32)], current: f32) -> Option<f32> {
    let g = gaps(busy);
    if g.iter().any(|&(a, b)| current >= a && current + SIGNAL_HZ <= b) {
        return Some(current);
    }
    let mid = |g: &(f32, f32)| (0.5 * (g.0 + g.1) - 0.5 * SIGNAL_HZ).round();
    g.iter()
        .max_by(|x, y| {
            let (wx, wy) = (x.1 - x.0, y.1 - y.0);
            wx.total_cmp(&wy).then_with(|| (mid(y) - current).abs().total_cmp(&(mid(x) - current).abs()))
        })
        .map(mid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_stays_where_it_is_clear_and_moves_to_the_widest_gap_when_not() {
        // Clear where it is.
        let pick = |heard: &[f32], cur| pick(&busy(heard, &[]), cur);
        assert_eq!(pick(&[600.0, 1800.0], 1200.0), Some(1200.0));
        // Someone landed on it: the widest gap is above 1855 Hz.
        let heard: Vec<f32> = (0..20).map(|i| 300.0 + 75.0 * i as f32).collect();
        let p = pick(&heard, 1000.0).expect("a gap");
        assert!(p > 1730.0 + 5.0, "{p}");
        // Nothing heard: stays put.
        assert_eq!(pick(&[], 1500.0), Some(1500.0));
        // A full band: nowhere.
        let full: Vec<f32> = (0..60).map(|i| 150.0 + 50.0 * i as f32).collect();
        assert_eq!(pick(&full, 1500.0), None);
        assert_eq!(busy(&[100.0, 140.0], &[]), vec![(95.0, 195.0)]);
    }

    #[test]
    fn strong_stations_are_remembered_longer() {
        assert_eq!(keep_s(-5.0), 300);
        assert_eq!(keep_s(12.0), 300);
        assert_eq!(keep_s(-6.0), 120);
        assert_eq!(keep_s(-20.0), 120);
    }

    #[test]
    fn undecoded_energy_is_busy_too() {
        // Energy at 1200 to 1260 Hz that nothing decoded, beside a decode
        // at 1300 Hz: both busy (the 30 Hz between them is too narrow for
        // an over), and AUTO moves off 1220 Hz.
        let b = busy(&[1300.0], &[(1200.0, 1260.0)]);
        assert_eq!(b, vec![(1195.0, 1265.0), (1295.0, 1355.0)]);
        let p = pick(&b, 1220.0).expect("a gap");
        assert!(!(1145.0..1355.0).contains(&p), "{p}");
        // Clear of it, AUTO stays.
        assert_eq!(pick(&b, 1600.0), Some(1600.0));
    }
}
