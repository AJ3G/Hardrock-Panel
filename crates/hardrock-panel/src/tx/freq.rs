//! TX freq AUTO: the clearest place in the audio passband for our FT8 over,
//! from the signals decoded in the slots we transmit in (our own period,
//! where we would land on top of someone).

/// The audio range AUTO keeps to, Hz.
pub(crate) const LO_HZ: f32 = 200.0;
pub(crate) const HI_HZ: f32 = 2900.0;
/// An FT8 signal's width above its tone 0, Hz (8 tones of 6.25 Hz).
pub(crate) const SIGNAL_HZ: f32 = 50.0;
/// The smallest clear gap AUTO will put an over in, Hz.
pub(crate) const NEED_HZ: f32 = 60.0;
/// A little room either side of a heard signal, Hz.
const GUARD_HZ: f32 = 5.0;

/// The busy parts of the passband, merged: each signal heard (tone 0 at
/// its offset) with a guard either side.
pub(crate) fn busy(heard: &[f32]) -> Vec<(f32, f32)> {
    let mut v: Vec<(f32, f32)> =
        heard.iter().map(|&a| (a - GUARD_HZ, a + SIGNAL_HZ + GUARD_HZ)).collect();
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

/// Where to send (tone 0, Hz): `current` while it sits clear in a gap of
/// at least `NEED_HZ`, else the middle of the widest gap (the nearer, of
/// two as wide). `None` when there is no such gap.
pub(crate) fn pick(heard: &[f32], current: f32) -> Option<f32> {
    let g = gaps(&busy(heard));
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
        assert_eq!(busy(&[100.0, 140.0]), vec![(95.0, 195.0)]);
    }
}
