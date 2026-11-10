//! When an over may go: FT8's 15 s slots, even (:00, :30) or odd (:15,
//! :45), and WSPR's even 2 min slots, a share of them picked from the call.

/// FT8's slot, s.
pub(crate) const FT8_PERIOD: i64 = 15;
/// WSPR's slot, s.
pub(crate) const WSPR_PERIOD: i64 = 120;
/// How late into its slot an FT8 over may still start, s. Started late, it
/// loses its first symbols (as WSJT-X's does); later than this the
/// receiving end can't place it in time.
pub(crate) const FT8_LATE_S: f64 = 2.0;
/// How late into its slot a WSPR beacon may still start, s: before its
/// first symbol is due.
pub(crate) const WSPR_LATE_S: f64 = 0.8;

/// The start of the slot `now` is in, UTC s.
pub(crate) fn slot_start(now: f64, period: i64) -> i64 {
    (now / period as f64).floor() as i64 * period
}

/// An even slot: :00 and :30 for FT8.
pub(crate) fn is_even(slot_utc: i64, period: i64) -> bool {
    (slot_utc / period).rem_euclid(2) == 0
}

/// The slot an FT8 over may start in at `now`, if one of parity `even` is
/// under way and not too far gone.
pub(crate) fn ft8_open(now: f64, even: bool) -> Option<i64> {
    let t0 = slot_start(now, FT8_PERIOD);
    (is_even(t0, FT8_PERIOD) == even && now - (t0 as f64) < FT8_LATE_S).then_some(t0)
}

/// Seconds until the next FT8 slot of parity `even` starts.
pub(crate) fn ft8_next_in(now: f64, even: bool) -> f64 {
    let t0 = slot_start(now, FT8_PERIOD);
    let next = if is_even(t0, FT8_PERIOD) == even { t0 + 2 * FT8_PERIOD } else { t0 + FT8_PERIOD };
    next as f64 - now
}

/// One slot in every this many, for TRANSMIT's share, %.
pub(crate) fn wspr_every(pct: u8) -> Option<i64> {
    match pct {
        0 => None,
        50.. => Some(2),
        33.. => Some(3),
        20.. => Some(5),
        _ => Some(10),
    }
}

/// Whether the 2 min slot at `slot_utc` carries our beacon: one in every
/// `wspr_every(pct)`, at an offset picked from the call so stations
/// sharing a share don't all pick the same slots. Never two in a row.
pub(crate) fn wspr_ours(call: &str, slot_utc: i64, pct: u8) -> bool {
    let Some(n) = wspr_every(pct) else { return false };
    if slot_utc.rem_euclid(WSPR_PERIOD) != 0 {
        return false;
    }
    let h = call.bytes().fold(5381u64, |h, b| h.wrapping_mul(33) ^ b.to_ascii_uppercase() as u64);
    (slot_utc / WSPR_PERIOD).rem_euclid(n) == (h % n as u64) as i64
}

/// The WSPR slot open at `now` for a beacon, if a slot is starting.
pub(crate) fn wspr_open(now: f64) -> Option<i64> {
    let t0 = slot_start(now, WSPR_PERIOD);
    (now - (t0 as f64) < WSPR_LATE_S).then_some(t0)
}

/// Seconds to the next of our WSPR slots, looking up to an hour ahead.
pub(crate) fn wspr_next_in(call: &str, now: f64, pct: u8) -> Option<f64> {
    let t0 = slot_start(now, WSPR_PERIOD);
    (1..=30)
        .map(|k| t0 + k * WSPR_PERIOD)
        .find(|&t| wspr_ours(call, t, pct))
        .map(|t| t as f64 - now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ft8_slots_open_for_two_seconds_on_their_parity() {
        let base = 1_800_000_000.0; // a multiple of 30 s
        assert_eq!(base as i64 % 30, 0);
        assert_eq!(ft8_open(base + 0.3, true), Some(base as i64));
        assert_eq!(ft8_open(base + 0.3, false), None);
        assert_eq!(ft8_open(base + 2.5, true), None);
        assert_eq!(ft8_open(base + 15.1, false), Some(base as i64 + 15));
        assert!((ft8_next_in(base + 1.0, true) - 29.0).abs() < 1e-9);
        assert!((ft8_next_in(base + 1.0, false) - 14.0).abs() < 1e-9);
    }

    #[test]
    fn a_wspr_share_is_kept_and_never_two_in_a_row() {
        let base = 1_800_000_000 / 120 * 120;
        for pct in [10u8, 20, 33, 50] {
            let n = wspr_every(pct).unwrap();
            let ours: Vec<i64> =
                (0..300).map(|k| base + k * 120).filter(|&t| wspr_ours("AJ3G", t, pct)).collect();
            assert_eq!(ours.len() as i64, 300 / n, "{pct}");
            assert!(ours.windows(2).all(|w| w[1] - w[0] >= 240), "{pct}");
        }
        assert!(!wspr_ours("AJ3G", base, 0));
        assert!(wspr_next_in("AJ3G", base as f64 + 5.0, 20).is_some_and(|s| s <= 600.0));
    }
}
