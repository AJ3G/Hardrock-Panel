//! FORECAST (decision 65): how open each HF band is likely to be from the
//! station, hour by hour for the next day. A rough model, not VOACAP: the
//! F2 layer's MUF(3000) rises with the Sun's height and the solar flux, the
//! D layer's absorption (which sets the lowest usable frequency) with the
//! Sun's height too, and a geomagnetic storm lowers the one and raises the
//! other. When KC2G has a sounder near the station, the model is scaled to
//! its MUF now, and the scaling fades over the hours ahead.

use super::sun;
use crate::station::LatLon;

/// What the forecast is worked from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs {
    pub qth: LatLon,
    /// 10.7 cm solar flux, sfu.
    pub sfi: f64,
    pub kp: f64,
    /// A sounder's MUF(3000) near the station now, MHz.
    pub muf_now: Option<f64>,
}

/// The bands, and the frequency each is judged at, MHz.
pub(crate) const BANDS: [(&str, f64); 10] = [
    ("160", 1.9),
    ("80", 3.6),
    ("60", 5.35),
    ("40", 7.1),
    ("30", 10.12),
    ("20", 14.1),
    ("17", 18.1),
    ("15", 21.2),
    ("12", 24.9),
    ("10", 28.5),
];

/// One hour of the forecast.
#[derive(Clone, Debug)]
pub(crate) struct Hour {
    /// Its start, Unix seconds (on the hour).
    pub utc: f64,
    pub muf: f64,
    pub luf: f64,
    /// Each band in [`BANDS`] order: 0 closed, 1 open.
    pub open: [f64; 10],
}

/// How much of the Sun's light falls on the ionosphere over `p`: 0 at
/// night, rising through twilight (the F layer is lit before the ground).
fn daylight(p: LatLon, utc: f64) -> f64 {
    let s = sun::elevation(p, utc).to_radians().sin();
    ((s + 0.15) / 1.15).clamp(0.0, 1.0)
}

/// The model's MUF(3000), MHz, before it is scaled to a sounder.
fn model_muf(i: &Inputs, utc: f64) -> f64 {
    let day = 8.0 + 0.14 * i.sfi;
    let night = 5.0 + 0.035 * i.sfi;
    let storm = 1.0 - 0.04 * (i.kp - 3.0).max(0.0);
    (night + (day - night) * daylight(i.qth, utc).powf(0.6)) * storm
}

/// The lowest usable frequency, MHz: the D layer's absorption, which only
/// the Sun overhead makes.
fn luf(i: &Inputs, utc: f64) -> f64 {
    let sun_up = sun::elevation(i.qth, utc).to_radians().sin().max(0.0);
    1.5 + 6.5 * sun_up.powf(0.75) * (i.sfi / 150.0).sqrt() + 0.3 * (i.kp - 4.0).max(0.0)
}

/// How open a band at `f` MHz is between `luf` and `muf`: closed under the
/// LUF and over the MUF, best from just over the LUF to the FOT (85 % of
/// the MUF).
fn openness(f: f64, luf: f64, muf: f64) -> f64 {
    let up = ((f - 0.7 * luf) / (0.4 * luf)).clamp(0.0, 1.0);
    let fot = 0.85 * muf;
    let down = if f <= fot { 1.0 } else { ((1.05 * muf - f) / (0.2 * muf)).clamp(0.0, 1.0) };
    up * down
}

/// The next 24 hours from the hour `now` is in.
pub(crate) fn build(i: &Inputs, now: f64) -> Vec<Hour> {
    let start = (now / 3600.0).floor() * 3600.0;
    // The sounder over the model now, fading back to the model with a
    // six-hour time constant.
    let k = i.muf_now.map_or(1.0, |m| (m / model_muf(i, now)).clamp(0.6, 1.6));
    (0..24)
        .map(|h| {
            let utc = start + h as f64 * 3600.0;
            let mid = utc + 1800.0;
            let ahead = ((mid - now) / 3600.0).max(0.0);
            let scale = 1.0 + (k - 1.0) * (-ahead / 6.0).exp();
            let muf = model_muf(i, mid) * scale;
            let luf = luf(i, mid);
            let open = BANDS.map(|(_, f)| openness(f, luf, muf));
            Hour { utc, muf, luf, open }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QTH: LatLon = LatLon { lat: 38.73, lon: -77.04 };

    /// 2026-06-21 at `h` UTC.
    fn june(h: f64) -> f64 {
        1_782_000_000.0 - 1_782_000_000.0 % 86_400.0 + h * 3600.0
    }

    fn band(hour: &Hour, b: &str) -> f64 {
        hour.open[BANDS.iter().position(|(n, _)| *n == b).unwrap()]
    }

    #[test]
    fn the_high_bands_open_by_day_and_the_low_ones_by_night() {
        let i = Inputs { qth: QTH, sfi: 150.0, kp: 2.0, muf_now: None };
        // 17:00 UTC is local noon at 77° W; 05:00 UTC is midnight.
        let f = build(&i, june(17.0));
        let (noon, night) = (&f[0], &f[12]);
        assert!(band(noon, "15") > 0.9 && band(noon, "20") > 0.9);
        assert!(band(noon, "160") < 0.1 && band(noon, "80") < 0.1);
        assert!(band(night, "80") > 0.9 && band(night, "160") > 0.9);
        assert!(band(night, "10") < 0.1 && band(night, "15") < 0.1);
        assert!(noon.muf > night.muf + 10.0);
    }

    #[test]
    fn more_flux_opens_higher_and_a_storm_closes_them() {
        let at = june(17.0);
        let low = build(&Inputs { qth: QTH, sfi: 70.0, kp: 2.0, muf_now: None }, at);
        let high = build(&Inputs { qth: QTH, sfi: 200.0, kp: 2.0, muf_now: None }, at);
        let storm = build(&Inputs { qth: QTH, sfi: 200.0, kp: 7.0, muf_now: None }, at);
        assert!(band(&low[0], "10") < band(&high[0], "10"));
        assert!(storm[0].muf < high[0].muf && storm[0].luf > high[0].luf);
    }

    #[test]
    fn a_sounder_now_scales_the_next_hours_and_fades() {
        let i = Inputs { qth: QTH, sfi: 150.0, kp: 2.0, muf_now: None };
        let at = june(17.0);
        let plain = build(&i, at);
        let sounded = build(&Inputs { muf_now: Some(plain[0].muf * 0.7), ..i }, at);
        assert!((sounded[0].muf / plain[0].muf - 0.7).abs() < 0.05);
        let late = sounded[23].muf / plain[23].muf;
        assert!(late > 0.95, "{late}");
    }
}
