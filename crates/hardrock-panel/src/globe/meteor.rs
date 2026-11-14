//! METEOR SHOWERS (decision 127): the year's major showers, which are
//! active today, and where each radiant stands in the station's sky now.
//!
//! A shower is a fixed window of dates and a fixed radiant, the same every
//! year, so this is a table and some arithmetic: nothing is fetched. The
//! dates, peak rates (ZHR), radiants and speeds are the International
//! Meteor Organization's working list of meteor showers, at each peak; the
//! idea and the table are ported from SDRoxide's `meteor.rs` (GPL-3). The
//! radiant drifts a degree or so a day through a window; the peak position
//! is near enough to say whether it is up.
//!
//! Why a radio operator cares: a meteor's ionised trail opens a path for a
//! second or two, so meteor scatter works 6 m and 2 m, and 10 m can ping
//! well past the horizon. What matters is whether the radiant is above the
//! horizon and how near the shower is to its peak.

use crate::station::LatLon;

/// One annual shower.
#[derive(Debug)]
pub(crate) struct Shower {
    pub name: &'static str,
    /// The IMO's three-letter code.
    pub code: &'static str,
    /// First and last days of activity, (month, day), inclusive. A window
    /// may run over the new year (the Quadrantids).
    pub start: (u32, u32),
    pub peak: (u32, u32),
    pub end: (u32, u32),
    /// Zenithal hourly rate at the peak: what one observer would see with
    /// the radiant overhead under a perfect sky. A strength, not a count.
    pub zhr: u32,
    /// The radiant at the peak: right ascension and declination, degrees.
    pub ra: f64,
    pub dec: f64,
    /// Speed into the atmosphere, km/s: faster leaves longer trails.
    pub kms: u32,
    pub parent: &'static str,
}

const fn sh(
    name: &'static str,
    code: &'static str,
    start: (u32, u32),
    peak: (u32, u32),
    end: (u32, u32),
    zhr: u32,
    (ra, dec): (f64, f64),
    kms: u32,
    parent: &'static str,
) -> Shower {
    Shower { name, code, start, peak, end, zhr, ra, dec, kms, parent }
}

/// The major showers through the year (IMO working list).
pub(crate) const SHOWERS: [Shower; 16] = [
    sh("Quadrantids", "QUA", (12, 28), (1, 3), (1, 12), 80, (230.0, 49.0), 41, "2003 EH1"),
    sh(
        "April Lyrids",
        "LYR",
        (4, 14),
        (4, 22),
        (4, 30),
        18,
        (271.0, 34.0),
        49,
        "C/1861 G1 Thatcher",
    ),
    sh("Eta Aquariids", "ETA", (4, 19), (5, 6), (5, 28), 50, (338.0, -1.0), 66, "1P/Halley"),
    // A daytime shower, its radiant near the Sun: heard by meteor scatter
    // rather than seen, and the year's strongest for radio.
    sh(
        "Daytime Arietids",
        "ARI",
        (5, 14),
        (6, 7),
        (6, 24),
        30,
        (44.0, 24.0),
        38,
        "96P/Machholz complex",
    ),
    sh(
        "Southern Delta Aquariids",
        "SDA",
        (7, 12),
        (7, 30),
        (8, 23),
        25,
        (340.0, -16.0),
        41,
        "96P/Machholz",
    ),
    sh("Perseids", "PER", (7, 17), (8, 12), (8, 24), 100, (48.0, 58.0), 59, "109P/Swift-Tuttle"),
    sh("Kappa Cygnids", "KCG", (8, 3), (8, 17), (8, 25), 3, (286.0, 59.0), 25, "2008 ED69"),
    sh("Aurigids", "AUR", (8, 28), (9, 1), (9, 5), 6, (91.0, 39.0), 66, "C/1911 N1 Kiess"),
    sh(
        "September Epsilon Perseids",
        "SPE",
        (9, 5),
        (9, 9),
        (9, 21),
        5,
        (48.0, 40.0),
        64,
        "unknown",
    ),
    sh(
        "Draconids",
        "DRA",
        (10, 6),
        (10, 8),
        (10, 10),
        10,
        (262.0, 54.0),
        20,
        "21P/Giacobini-Zinner",
    ),
    sh("Orionids", "ORI", (10, 2), (10, 21), (11, 7), 20, (95.0, 16.0), 66, "1P/Halley"),
    sh("Southern Taurids", "STA", (9, 10), (10, 10), (11, 20), 5, (32.0, 9.0), 27, "2P/Encke"),
    sh("Northern Taurids", "NTA", (10, 20), (11, 12), (12, 10), 5, (58.0, 22.0), 29, "2P/Encke"),
    sh("Leonids", "LEO", (11, 6), (11, 17), (11, 30), 15, (152.0, 22.0), 71, "55P/Tempel-Tuttle"),
    sh("Geminids", "GEM", (12, 4), (12, 14), (12, 20), 150, (112.0, 33.0), 35, "3200 Phaethon"),
    sh("Ursids", "URS", (12, 17), (12, 22), (12, 26), 10, (217.0, 76.0), 33, "8P/Tuttle"),
];

/// A shower active now, its radiant placed for the station.
#[derive(Debug)]
pub(crate) struct Active {
    pub shower: &'static Shower,
    /// The radiant's height above the horizon, degrees (below 0, set).
    pub alt: f64,
    /// Its bearing, degrees clockwise from north.
    pub az: f64,
    /// Days to the peak, the short way round the year: 0 on the peak day,
    /// negative after it.
    pub days_to_peak: i32,
}

/// `md` falls in the window `start..=end`, which may run over the new year.
fn in_window(md: (u32, u32), start: (u32, u32), end: (u32, u32)) -> bool {
    if start <= end { md >= start && md <= end } else { md >= start || md <= end }
}

/// The day of a (non-leap) year: no shower peaks near 29 February.
fn day_of_year((m, d): (u32, u32)) -> i32 {
    const START: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    START[(m.clamp(1, 12) - 1) as usize] + d as i32
}

/// Days from `md` to `to`, folded into ±182 so the nearer way wins.
fn days_between(md: (u32, u32), to: (u32, u32)) -> i32 {
    let d = day_of_year(to) - day_of_year(md);
    if d > 182 {
        d - 365
    } else if d < -182 {
        d + 365
    } else {
        d
    }
}

/// Greenwich mean sidereal time at `utc` (Unix seconds), degrees: the same
/// formula `sun::subsolar` uses, so the radiants and the Sun share a frame.
fn gmst(utc: f64) -> f64 {
    let n = utc / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    (280.460_618_37 + 360.985_647_366_29 * n).rem_euclid(360.0)
}

/// Where a star-fixed direction (`ra`, `dec`, degrees) stands from `p` at
/// `utc`: (altitude, azimuth) in degrees, azimuth clockwise from north. A
/// radiant is as far as a star, so no parallax.
pub(crate) fn alt_az(p: LatLon, ra: f64, dec: f64, utc: f64) -> (f64, f64) {
    let h = (gmst(utc) + p.lon - ra).to_radians();
    let (lat, dec) = (p.lat.to_radians(), dec.to_radians());
    let alt = (lat.sin() * dec.sin() + lat.cos() * dec.cos() * h.cos()).clamp(-1.0, 1.0).asin();
    let az = (-h.sin() * dec.cos()).atan2(lat.cos() * dec.sin() - lat.sin() * dec.cos() * h.cos());
    (alt.to_degrees(), az.to_degrees().rem_euclid(360.0))
}

/// The showers active at `utc` (Unix seconds) from `p`, strongest first.
/// "Active" is the IMO's window; the rate shown is the peak's.
pub(crate) fn active(p: LatLon, utc: f64) -> Vec<Active> {
    let (_, mo, d, _, _, _) = crate::decodelog::civil(utc.floor() as i64);
    let md = (mo, d);
    let mut out: Vec<Active> = SHOWERS
        .iter()
        .filter(|s| in_window(md, s.start, s.end))
        .map(|s| {
            let (alt, az) = alt_az(p, s.ra, s.dec, utc);
            Active { shower: s, alt, az, days_to_peak: days_between(md, s.peak) }
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.shower.zhr));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix seconds of a UTC date and hour (Howard Hinnant's days_from_civil).
    fn at(y: i64, mo: u32, d: u32, h: u32) -> f64 {
        let y = if mo <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (mo as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        ((era * 146_097 + doe - 719_468) * 86_400 + h as i64 * 3600) as f64
    }

    fn codes(p: LatLon, t: f64) -> Vec<&'static str> {
        active(p, t).into_iter().map(|a| a.shower.code).collect()
    }

    #[test]
    fn every_window_holds_its_peak_and_the_codes_are_unique() {
        for (i, s) in SHOWERS.iter().enumerate() {
            assert!(in_window(s.peak, s.start, s.end), "{}", s.code);
            for o in &SHOWERS[i + 1..] {
                assert_ne!(s.code, o.code);
                assert_ne!(s.name, o.name);
            }
        }
    }

    #[test]
    fn the_right_showers_are_active_by_date_and_the_quadrantids_span_the_new_year() {
        let p = LatLon { lat: 40.0, lon: -75.0 };
        assert!(codes(p, at(2026, 8, 12, 4)).contains(&"PER"));
        assert!(codes(p, at(2027, 1, 3, 9)).contains(&"QUA"));
        assert!(codes(p, at(2026, 12, 30, 9)).contains(&"QUA"));
        assert!(!codes(p, at(2026, 7, 1, 9)).contains(&"QUA"));
        assert!(codes(p, at(2026, 12, 19, 9)).contains(&"GEM"));
        assert!(!codes(p, at(2026, 10, 15, 9)).contains(&"NTA"));
        // Mid-November: the Leonids and both Taurid branches, the Leonids
        // first (the strongest), and nothing in early March.
        let nov = codes(p, at(2026, 11, 14, 9));
        assert_eq!(nov, vec!["LEO", "STA", "NTA"]);
        assert!(codes(p, at(2026, 3, 5, 9)).is_empty());
    }

    #[test]
    fn days_to_the_peak_go_the_short_way_round_the_year() {
        assert_eq!(days_between((12, 31), (1, 3)), 3);
        assert_eq!(days_between((1, 5), (1, 3)), -2);
        assert_eq!(days_between((12, 14), (12, 14)), 0);
        let a = active(LatLon { lat: 40.0, lon: -75.0 }, at(2026, 11, 14, 9));
        assert_eq!(a[0].days_to_peak, 3);
    }

    /// The pole stands at the latitude, due north, at any hour.
    #[test]
    fn the_pole_stands_at_the_latitude_due_north() {
        for h in [0, 7, 13, 22] {
            let (alt, az) =
                alt_az(LatLon { lat: 40.0, lon: -75.0 }, 123.0, 90.0, at(2026, 3, 20, h));
            assert!((alt - 40.0).abs() < 1e-6, "{alt}");
            assert!(az < 1e-6 || az > 360.0 - 1e-6, "{az}");
        }
    }

    /// The radiant frame is the Sun's: the Sun's own right ascension and
    /// declination, placed with `alt_az`, stand at the Sun's elevation.
    #[test]
    fn a_direction_at_the_suns_ra_and_dec_stands_where_the_sun_does() {
        for t in [at(2026, 1, 1, 0), at(2026, 6, 1, 6), at(2026, 9, 15, 18)] {
            let sub = super::super::sun::subsolar(t);
            // The Sun's RA is its subsolar longitude plus GMST.
            let ra = sub.lon + gmst(t);
            let p = LatLon { lat: 40.0, lon: -75.0 };
            let (alt, _) = alt_az(p, ra, sub.lat, t);
            let e = super::super::sun::elevation(p, t);
            assert!((alt - e).abs() < 1e-6, "{alt} vs {e}");
        }
    }

    /// The Perseid radiant (dec 58°) rises and sets over a day at 30° N,
    /// topping out near 90 - (58 - 30) = 62°.
    #[test]
    fn the_perseid_radiant_rises_and_sets_over_a_day() {
        let p = LatLon { lat: 30.0, lon: 0.0 };
        let base = at(2026, 8, 12, 0);
        let alts: Vec<f64> =
            (0..288).map(|k| alt_az(p, 48.0, 58.0, base + k as f64 * 300.0).0).collect();
        let hi = alts.iter().cloned().fold(f64::MIN, f64::max);
        let lo = alts.iter().cloned().fold(f64::MAX, f64::min);
        assert!((hi - 62.0).abs() < 0.5 && lo < 0.0, "{hi} {lo}");
    }
}
