//! The station: the operator's callsign and Maidenhead grid (SETTINGS →
//! Station), where every station heard is, and the great-circle distance and
//! bearing between them.
//!
//! Positions are kept here and only here ([`Heard`]), so DECODE's DIST and AZ
//! columns and anything that plots stations later (a globe) read the same
//! figures. A callsign heard once with a grid keeps it: a later message from
//! it without one (a report, RR73) is placed from what was heard before.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The Earth's mean radius, km (IUGG), as the great-circle figures use.
pub(crate) const EARTH_KM: f64 = 6371.0;
/// Kilometres in a statute mile.
const KM_PER_MI: f64 = 1.609_344;

/// How distances are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DistUnit {
    #[default]
    Km,
    Mi,
}

impl DistUnit {
    pub(crate) const ALL: [DistUnit; 2] = [DistUnit::Km, DistUnit::Mi];

    pub(crate) fn label(self) -> &'static str {
        match self {
            DistUnit::Km => "km",
            DistUnit::Mi => "mi",
        }
    }

    /// `km` in this unit.
    pub(crate) fn of_km(self, km: f64) -> f64 {
        match self {
            DistUnit::Km => km,
            DistUnit::Mi => km / KM_PER_MI,
        }
    }
}

/// SETTINGS → Station, kept between runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct StationSettings {
    /// As typed; [`StationSettings::call`] gives it cleaned up.
    pub callsign: String,
    /// As typed, 4 or 6 characters; [`StationSettings::grid`] gives it only
    /// when it is a whole, valid locator.
    pub grid: String,
    pub dist_unit: DistUnit,
}

impl StationSettings {
    /// The operator's callsign, upper case, if it looks like one.
    pub(crate) fn call(&self) -> Option<String> {
        let c = self.callsign.trim().to_ascii_uppercase();
        is_callsign(&c).then_some(c)
    }

    /// The operator's grid, normalised (`FN31pr`), if it is a whole valid
    /// 4- or 6-character locator.
    pub(crate) fn grid(&self) -> Option<String> {
        let g = self.grid.trim();
        (grid_check(g) == GridCheck::Valid).then(|| normalise_grid(g))
    }

    /// Where the operator is: the centre of their grid.
    pub(crate) fn pos(&self) -> Option<LatLon> {
        self.grid().and_then(|g| grid_to_latlon(&g))
    }
}

/// A place on the Earth, degrees: latitude north, longitude east.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// What a grid typed so far is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GridCheck {
    Empty,
    /// The start of a valid locator (`F`, `FN3`, `FN31p`).
    Partial,
    /// A whole 4- or 6-character locator.
    Valid,
    /// Not a locator, and no more typing will make it one.
    Invalid,
}

/// Check a Maidenhead locator as it is typed: field letters A–R, square
/// digits, subsquare letters A–X (either case).
pub(crate) fn grid_check(g: &str) -> GridCheck {
    let b = g.trim().as_bytes();
    if b.is_empty() {
        return GridCheck::Empty;
    }
    if b.len() > 6 {
        return GridCheck::Invalid;
    }
    for (i, &c) in b.iter().enumerate() {
        let c = c.to_ascii_uppercase();
        let ok = match i {
            0 | 1 => (b'A'..=b'R').contains(&c),
            2 | 3 => c.is_ascii_digit(),
            _ => (b'A'..=b'X').contains(&c),
        };
        if !ok {
            return GridCheck::Invalid;
        }
    }
    if b.len() == 4 || b.len() == 6 { GridCheck::Valid } else { GridCheck::Partial }
}

/// `fn31PR` → `FN31pr`, as locators are written.
fn normalise_grid(g: &str) -> String {
    g.trim()
        .chars()
        .enumerate()
        .map(|(i, c)| if i < 4 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
        .collect()
}

/// The centre of a 4- or 6-character Maidenhead locator.
///
/// A field is 20° of longitude by 10° of latitude from 180° W, 90° S; a
/// square 2° by 1°; a subsquare 5′ by 2.5′.
pub(crate) fn grid_to_latlon(g: &str) -> Option<LatLon> {
    if grid_check(g) != GridCheck::Valid {
        return None;
    }
    let b: Vec<u8> = g.trim().bytes().map(|c| c.to_ascii_uppercase()).collect();
    let mut lon = -180.0 + (b[0] - b'A') as f64 * 20.0 + (b[2] - b'0') as f64 * 2.0;
    let mut lat = -90.0 + (b[1] - b'A') as f64 * 10.0 + (b[3] - b'0') as f64;
    if b.len() == 6 {
        lon += (b[4] - b'A') as f64 * (2.0 / 24.0) + 1.0 / 24.0;
        lat += (b[5] - b'A') as f64 * (1.0 / 24.0) + 1.0 / 48.0;
    } else {
        lon += 1.0;
        lat += 0.5;
    }
    Some(LatLon { lat, lon })
}

/// The great-circle distance from `a` to `b`, km (haversine, on a sphere of
/// [`EARTH_KM`]).
pub(crate) fn distance_km(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_KM * h.sqrt().min(1.0).asin()
}

/// The initial great-circle bearing from `a` towards `b`, degrees from true
/// north, 0 to 360.
pub(crate) fn bearing_deg(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (b.lon - a.lon).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// Whether `s` looks like a callsign: 3 to 13 letters, digits and slashes,
/// with at least one of each of a letter and a digit, not a grid.
pub(crate) fn is_callsign(s: &str) -> bool {
    let n = s.len();
    (3..=13).contains(&n)
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'/')
        && s.bytes().any(|c| c.is_ascii_digit())
        && s.bytes().any(|c| c.is_ascii_alphabetic())
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !(n == 4 && grid_check(s) == GridCheck::Valid)
}

/// A grid as a WSJT message carries it: four characters, upper case, not
/// `RR73` (which is a sign-off, though it reads as a square in the Arctic).
fn is_message_grid(t: &str) -> bool {
    t.len() == 4
        && t != "RR73"
        && t.bytes().all(|c| !c.is_ascii_lowercase())
        && grid_check(t) == GridCheck::Valid
}

/// The station that sent a WSJT message, and the grid it gave, if any.
///
/// `CQ [DX|NA|260|…] CALL [GRID]` is sent by CALL; every other standard
/// message (`TO FROM [R] [GRID|report|RR73|73]`) by its second callsign.
/// Hashed callsigns the decoder resolved (`<K1ABC>`) count; unresolved
/// (`<...>`) and free text do not.
pub(crate) fn sender_of(msg: &str) -> (Option<String>, Option<String>) {
    let mut t: Vec<&str> = msg.split_whitespace().collect();
    let grid = match t.last() {
        Some(g) if t.len() >= 2 && is_message_grid(g) => {
            let g = g.to_string();
            t.pop();
            Some(g)
        }
        _ => None,
    };
    let clean = |s: &str| {
        let s = s.trim_start_matches('<').trim_end_matches('>');
        is_callsign(s).then(|| s.to_string())
    };
    let call = match t.first() {
        Some(&"CQ" | &"QRZ" | &"DE") => t.last().and_then(|s| clean(s)),
        Some(_) => t.get(1).and_then(|s| clean(s)),
        None => None,
    };
    // A grid with no sender to go with it is no use.
    match call {
        Some(c) => (Some(c), grid),
        None => (None, None),
    }
}

/// A station heard, and where it said it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Station {
    pub grid: String,
    pub pos: LatLon,
    /// When it was last heard with this grid, Unix seconds.
    pub utc: f64,
}

/// Every station heard with a grid this run, by callsign: the one place the
/// panel keeps stations' positions.
#[derive(Default)]
pub(crate) struct Heard {
    by_call: HashMap<String, Station>,
}

/// The most stations kept; the longest unheard go first.
const HEARD_MAX: usize = 20_000;

impl Heard {
    /// `call` was heard at `utc` giving `grid`.
    pub(crate) fn note(&mut self, call: &str, grid: &str, utc: f64) {
        let Some(pos) = grid_to_latlon(grid) else { return };
        if self.by_call.len() >= HEARD_MAX && !self.by_call.contains_key(call) {
            let oldest = self
                .by_call
                .iter()
                .min_by(|a, b| a.1.utc.total_cmp(&b.1.utc))
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.by_call.remove(&k);
            }
        }
        self.by_call.insert(call.to_string(), Station { grid: grid.to_string(), pos, utc });
    }

    /// Where `call` was last heard to be.
    pub(crate) fn get(&self, call: &str) -> Option<&Station> {
        self.by_call.get(call)
    }

    /// Every station placed so far, for a map.
    #[allow(dead_code)] // For the globe.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Station)> {
        self.by_call.iter().map(|(k, v)| (k.as_str(), v))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_call.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn grids_are_checked_as_they_are_typed() {
        assert_eq!(grid_check(""), GridCheck::Empty);
        for g in ["F", "FN", "FN3", "fn31p"] {
            assert_eq!(grid_check(g), GridCheck::Partial, "{g}");
        }
        for g in ["FN31", "fn31pr", "JO65", "AA00aa", "RR99xx"] {
            assert_eq!(grid_check(g), GridCheck::Valid, "{g}");
        }
        for g in ["S", "FS", "FNA", "FN3a", "FN31y", "FN31pr0", "FN31p9", "1N"] {
            assert_eq!(grid_check(g), GridCheck::Invalid, "{g}");
        }
        let s = StationSettings { grid: " fn31PR ".into(), ..Default::default() };
        assert_eq!(s.grid().as_deref(), Some("FN31pr"));
        let s = StationSettings { grid: "FN3".into(), ..Default::default() };
        assert_eq!(s.grid(), None);
    }

    #[test]
    fn a_grid_is_placed_at_its_centre() {
        // JO65: 0–20° E field J, 50–60° N field O; square 6, 5.
        let p = grid_to_latlon("JO65").unwrap();
        assert_eq!((p.lat, p.lon), (55.5, 13.0));
        // FN31pr, the subsquare holding W1AW (41.7148 N, 72.7272 W).
        let p = grid_to_latlon("FN31pr").unwrap();
        assert!(close(p.lat, 41.729_167, 1e-6) && close(p.lon, -72.708_333, 1e-6), "{p:?}");
        assert!(close(p.lat, 41.7148, 2.5 / 60.0 / 2.0) && close(p.lon, -72.7272, 5.0 / 120.0));
        // The corners of the world.
        let p = grid_to_latlon("AA00aa").unwrap();
        assert!(close(p.lat, -90.0 + 1.0 / 48.0, 1e-9) && close(p.lon, -180.0 + 1.0 / 24.0, 1e-9));
        let p = grid_to_latlon("RR99xx").unwrap();
        assert!(close(p.lat, 90.0 - 1.0 / 48.0, 1e-9) && close(p.lon, 180.0 - 1.0 / 24.0, 1e-9));
        assert_eq!(grid_to_latlon("FN3"), None);
    }

    #[test]
    fn great_circles_match_known_figures() {
        let london = LatLon { lat: 51.5074, lon: -0.1278 };
        let new_york = LatLon { lat: 40.7128, lon: -74.0060 };
        // 5570 km, leaving London at 288.3°.
        assert!(close(distance_km(london, new_york), 5570.2, 0.5));
        assert!(close(bearing_deg(london, new_york), 288.33, 0.01));
        // A quarter of the equator, due east; to the pole, due north.
        let o = LatLon { lat: 0.0, lon: 0.0 };
        let east = LatLon { lat: 0.0, lon: 90.0 };
        assert!(close(distance_km(o, east), EARTH_KM * std::f64::consts::FRAC_PI_2, 1e-6));
        assert!(close(bearing_deg(o, east), 90.0, 1e-9));
        assert!(close(bearing_deg(o, LatLon { lat: 90.0, lon: 0.0 }), 0.0, 1e-9));
        assert!(close(bearing_deg(o, LatLon { lat: 0.0, lon: -90.0 }), 270.0, 1e-9));
        // Antipodes: half the circumference.
        let anti = LatLon { lat: 0.0, lon: 180.0 };
        assert!(close(distance_km(o, anti), EARTH_KM * std::f64::consts::PI, 1e-6));
        // Grid to grid: FN31pr to JO65, 6064 km at 43.9°.
        let a = grid_to_latlon("FN31pr").unwrap();
        let b = grid_to_latlon("JO65").unwrap();
        assert!(close(distance_km(a, b), 6064.2, 0.5));
        assert!(close(bearing_deg(a, b), 43.90, 0.01));
        assert!(close(DistUnit::Mi.of_km(1609.344), 1000.0, 1e-9));
    }

    #[test]
    fn the_sender_and_its_grid_are_read_from_a_message() {
        let s = |m: &str| sender_of(m);
        let some = |c: &str, g: Option<&str>| (Some(c.to_string()), g.map(String::from));
        assert_eq!(s("CQ K1ABC FN42"), some("K1ABC", Some("FN42")));
        assert_eq!(s("CQ DX K1ABC FN42"), some("K1ABC", Some("FN42")));
        assert_eq!(s("CQ 260 K1ABC"), some("K1ABC", None));
        assert_eq!(s("K1ABC W9XYZ EN37"), some("W9XYZ", Some("EN37")));
        assert_eq!(s("K1ABC W9XYZ R EN37"), some("W9XYZ", Some("EN37")));
        assert_eq!(s("K1ABC W9XYZ -12"), some("W9XYZ", None));
        assert_eq!(s("K1ABC W9XYZ RR73"), some("W9XYZ", None));
        assert_eq!(s("<K1ABC> W9XYZ 73"), some("W9XYZ", None));
        assert_eq!(s("W9XYZ <PJ4/K1ABC> -05"), some("PJ4/K1ABC", None));
        assert_eq!(s("W9XYZ <...> RR73"), (None, None));
        assert_eq!(s("TNX BOB 73 GL"), (None, None));
        assert_eq!(s(""), (None, None));
    }

    #[test]
    fn a_station_heard_with_a_grid_keeps_it() {
        let mut h = Heard::default();
        h.note("W9XYZ", "EN37", 100.0);
        h.note("K1ABC", "nonsense", 100.0);
        assert_eq!(h.len(), 1);
        assert_eq!(h.get("W9XYZ").unwrap().grid, "EN37");
        h.note("W9XYZ", "EN52", 200.0);
        assert_eq!(h.get("W9XYZ").unwrap().grid, "EN52");
        assert!(h.get("K1ABC").is_none());
    }

    #[test]
    fn callsigns_are_told_from_grids_and_words() {
        for c in ["K1ABC", "W9XYZ", "PJ4/K1ABC", "K1ABC/P", "2E0ABC", "VK2ABC"] {
            assert!(is_callsign(c), "{c}");
        }
        for c in ["FN42", "CQ", "RR73", "-12", "R+05", "73", "/K1", "K1/"] {
            assert!(!is_callsign(c), "{c}");
        }
    }
}
