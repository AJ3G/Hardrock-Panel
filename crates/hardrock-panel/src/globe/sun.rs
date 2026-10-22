//! Where the Sun is overhead (the subsolar point), from the Astronomical
//! Almanac's low-precision solar coordinates: good to about 0.01° in
//! declination and well under a minute of time, which is far finer than a
//! grey line needs.

use crate::station::LatLon;

/// The subsolar point at `utc` (Unix seconds).
pub(crate) fn subsolar(utc: f64) -> LatLon {
    // Days from J2000.0 (2000-01-01 12:00 TT, near enough UTC here).
    let n = utc / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let l = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let g = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_4 * n).to_radians();
    let decl = (eps.sin() * lambda.sin()).asin().to_degrees();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos()).to_degrees();
    let gmst = (280.460_618_37 + 360.985_647_366_29 * n).rem_euclid(360.0);
    let lon = (ra - gmst + 540.0).rem_euclid(360.0) - 180.0;
    LatLon { lat: decl, lon }
}

/// How high the Sun stands over `p` at `utc`, degrees (below 0 at night),
/// ignoring refraction.
pub(crate) fn elevation(p: LatLon, utc: f64) -> f64 {
    let s = super::sphere::vec_ll(subsolar(utc));
    let v = super::sphere::vec_ll(p);
    super::sphere::dot(s, v).clamp(-1.0, 1.0).asin().to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix seconds of a UTC date and time.
    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> f64 {
        // Howard Hinnant's days_from_civil.
        let y = if mo <= 2 { y - 1 } else { y } as i64;
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (mo as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        (days * 86_400 + (h * 3600 + mi * 60) as i64) as f64
    }

    #[test]
    fn the_sun_crosses_the_equator_at_the_equinox_and_tops_out_at_the_solstice() {
        // March equinox 2026: 20 March 14:46 UTC.
        assert!(subsolar(at(2026, 3, 20, 14, 46)).lat.abs() < 0.05);
        // June solstice 2026: 21 June 08:24 UTC, 23.44° N.
        assert!((subsolar(at(2026, 6, 21, 8, 24)).lat - 23.44).abs() < 0.02);
        // December solstice: 23.44° S.
        assert!((subsolar(at(2026, 12, 21, 20, 50)).lat + 23.44).abs() < 0.02);
    }

    #[test]
    fn noon_follows_the_clock_and_the_equation_of_time() {
        // Early November the Sun runs about 16 minutes fast: noon at
        // Greenwich is 11:44 UTC, so at 12:00 it is overhead 4° west.
        let p = subsolar(at(2026, 11, 3, 12, 0));
        assert!((p.lon + 4.1).abs() < 0.3, "{p:?}");
        // Mid-February it runs about 14 minutes slow: still 3.5° east.
        let p = subsolar(at(2026, 2, 11, 12, 0));
        assert!((p.lon - 3.55).abs() < 0.3, "{p:?}");
        // Six hours later it is a quarter of the way round, westward.
        let q = subsolar(at(2026, 2, 11, 18, 0));
        assert!(((p.lon - q.lon) - 90.0).abs() < 0.2);
    }

    #[test]
    fn it_is_day_under_the_sun_and_night_opposite() {
        let t = at(2026, 10, 22, 15, 30);
        let s = subsolar(t);
        assert!((elevation(s, t) - 90.0).abs() < 1e-6);
        assert!((elevation(LatLon { lat: -s.lat, lon: s.lon + 180.0 }, t) + 90.0).abs() < 1e-6);
    }
}
