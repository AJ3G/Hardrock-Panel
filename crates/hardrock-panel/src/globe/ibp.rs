//! IBP BEACONS (decision 65): which NCDXF/IARU International Beacon Project
//! beacon is on each of its five bands now. Eighteen beacons share a
//! 180-second cycle of 10-second slots aligned to UTC midnight, one on each
//! band at a time, so a clock and a table say who to listen for: a beacon
//! heard is a path open, measured rather than forecast.
//!
//! Ported from SDRoxide's `ibp.rs` (GPL-3). Callsigns, sites and locators
//! are the NCDXF's published list
//! (<https://www.ncdxf.org/beacon/beaconlocations.html>); a position is the
//! centre of the six-character locator.

use crate::station::{LatLon, grid_to_latlon};

pub(crate) struct Beacon {
    pub call: &'static str,
    pub site: &'static str,
    pub locator: &'static str,
}

impl Beacon {
    pub(crate) fn pos(&self) -> LatLon {
        grid_to_latlon(self.locator).expect("every beacon's locator is a valid one")
    }
}

/// The eighteen beacons, in transmission order: the index is the place in
/// the cycle.
pub(crate) const BEACONS: [Beacon; 18] = [
    Beacon { call: "4U1UN", site: "United Nations, New York", locator: "FN30as" },
    Beacon { call: "VE8AT", site: "Inuvik, Canada", locator: "CP38gh" },
    Beacon { call: "W6WX", site: "Mt. Umunhum, California", locator: "CM97bd" },
    Beacon { call: "KH6RS", site: "Maui, Hawaii", locator: "BL10ts" },
    Beacon { call: "ZL6B", site: "Masterton, New Zealand", locator: "RE78tw" },
    Beacon { call: "VK6RBP", site: "Rolystone, Australia", locator: "OF87av" },
    Beacon { call: "JA2IGY", site: "Mt. Asama, Japan", locator: "PM84jk" },
    Beacon { call: "RR9O", site: "Novosibirsk, Russia", locator: "NO14kx" },
    Beacon { call: "VR2B", site: "Hong Kong", locator: "OL72bg" },
    Beacon { call: "4S7B", site: "Colombo, Sri Lanka", locator: "MJ96wv" },
    Beacon { call: "ZS6DN", site: "Pretoria, South Africa", locator: "KG33xi" },
    Beacon { call: "5Z4B", site: "Kikuyu, Kenya", locator: "KI88hr" },
    Beacon { call: "4X6TU", site: "Tel Aviv, Israel", locator: "KM72jb" },
    Beacon { call: "OH2B", site: "Lohja, Finland", locator: "KP20eh" },
    Beacon { call: "CS3B", site: "São Jorge, Madeira", locator: "IM12jt" },
    Beacon { call: "LU4AA", site: "Buenos Aires, Argentina", locator: "GF05tj" },
    Beacon { call: "OA4B", site: "Lima, Peru", locator: "FH17mw" },
    Beacon { call: "YV5B", site: "Caracas, Venezuela", locator: "FK60nd" },
];

/// The five bands, their beacon frequency (Hz), and what is added to the
/// slot to give the band's beacon: a beacon steps up a band every slot, so
/// the band `n` above 20 m has the beacon that was on 20 m `n` slots ago,
/// stored as `18 - n`.
pub(crate) const BANDS: [(&str, u64, usize); 5] = [
    ("20", 14_100_000, 0),
    ("17", 18_110_000, 17),
    ("15", 21_150_000, 16),
    ("12", 24_930_000, 15),
    ("10", 28_200_000, 14),
];

const SLOT_S: i64 = 10;
const CYCLE_S: i64 = SLOT_S * BEACONS.len() as i64;

/// The slot in the cycle, 0 to 17.
fn slot_at(unix_s: i64) -> usize {
    (unix_s.rem_euclid(86_400) % CYCLE_S / SLOT_S) as usize
}

/// Seconds left in the slot, 1 to 10.
pub(crate) fn seconds_left(unix_s: i64) -> i64 {
    SLOT_S - unix_s.rem_euclid(SLOT_S)
}

/// Each band's beacon at `unix_s`, in [`BANDS`] order.
pub(crate) fn on_air(unix_s: i64) -> [&'static Beacon; 5] {
    let slot = slot_at(unix_s);
    BANDS.map(|(_, _, off)| &BEACONS[(slot + off) % BEACONS.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::distance_km;

    /// 2026-06-01 at `h:m:s` UTC.
    fn at(h: i64, m: i64, s: i64) -> i64 {
        1_780_272_000 + h * 3600 + m * 60 + s
    }

    #[test]
    fn the_cycle_starts_at_utc_midnight_and_turns_every_three_minutes() {
        assert_eq!(at(0, 0, 0) % 86_400, 0);
        assert_eq!(slot_at(at(0, 0, 9)), 0);
        assert_eq!(slot_at(at(0, 0, 10)), 1);
        assert_eq!(slot_at(at(0, 2, 50)), 17);
        assert_eq!(slot_at(at(0, 3, 0)), 0);
        assert_eq!(slot_at(at(23, 59, 59)), 17);
        assert_eq!((seconds_left(at(0, 0, 0)), seconds_left(at(0, 0, 9))), (10, 1));
    }

    #[test]
    fn the_higher_bands_have_the_beacons_that_started_earlier() {
        let a = on_air(at(0, 0, 0));
        let calls: Vec<&str> = a.iter().map(|b| b.call).collect();
        assert_eq!(calls, ["4U1UN", "YV5B", "OA4B", "LU4AA", "CS3B"]);
        // Every beacon works every band once a cycle.
        for band in 0..BANDS.len() {
            let mut seen: Vec<&str> =
                (0..18).map(|k| on_air(at(0, 0, 0) + k * SLOT_S)[band].call).collect();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), 18);
        }
    }

    #[test]
    fn the_locators_put_the_beacons_on_their_sites() {
        let near = |call: &str, lat: f64, lon: f64| {
            let b = BEACONS.iter().find(|b| b.call == call).unwrap();
            let km = distance_km(b.pos(), LatLon { lat, lon });
            assert!(km < 25.0, "{call} is {km:.0} km from its site");
        };
        near("4U1UN", 40.749, -73.968);
        near("KH6RS", 20.8, -156.33);
        near("VE8AT", 68.36, -133.72);
        near("OA4B", -12.05, -77.04);
    }
}
