//! What GLOBE knows of the bands: every station heard in the last hour (and,
//! from PSK Reporter, every station that heard AJ3G), and from them the
//! propagation field, the PROP layer, as SDRoxide's 3D view draws it
//! (https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later, its
//! manual's §7.8).
//!
//! Each reception is placed at the middle of its path, the patch of
//! ionosphere that carried it, not at the far station: a map of far stations
//! shows where amateurs live, this shows where the sky worked. A path longer
//! than 3000 km gets one point in the middle of each hop. A reception counts
//! for more the further its report is above its mode's own decode floor, so
//! the most sensitive mode on a band does not paint as its worst
//! propagation, and its weight halves every half-life (45 minutes unless
//! set otherwise).

use std::collections::VecDeque;

use super::sphere::{V3, angle, dot, slerp, vec_ll};
use crate::scope::bandplan;
use crate::station::{EARTH_KM, LatLon, distance_km};

/// Where a reception came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum Source {
    /// The DECODE log.
    Decode,
    /// SKIM WIDE and SKIM CW/PSK/RTTY.
    Skim,
    /// PSK Reporter: stations that heard AJ3G.
    PskReporter,
    /// PSK Reporter: everyone's reports on GLOBE's band in the last 15
    /// minutes, between other stations. They feed PROP, PATHS, REACH, BEST
    /// and HEARD ≥, not the station list or the map's dots.
    PskBand,
    /// WSPR HEARD ME: the WSPR reports of AJ3G's call in the last hour, from
    /// wspr.live (WSPRnet's database) or, when that is down, WSPRnet.
    WsprHeard,
}

impl Source {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Source::Decode => "DECODE",
            Source::Skim => "SKIM",
            Source::PskReporter => "PSK REPORTER",
            Source::PskBand => "PSK BAND",
            Source::WsprHeard => "WSPR",
        }
    }

    /// The far station heard AJ3G (rather than AJ3G hearing it).
    pub(crate) fn reverse(self) -> bool {
        matches!(self, Source::PskReporter | Source::WsprHeard)
    }
}

/// One station heard (or hearing AJ3G), once.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reception {
    pub call: String,
    pub grid: String,
    pub pos: LatLon,
    /// True RF, Hz.
    pub rf_hz: f64,
    pub mode: String,
    pub snr: Option<f32>,
    /// Unix seconds.
    pub utc: f64,
    pub src: Source,
    /// Its DECODE line, for SHOW IN DECODE.
    pub row: Option<u64>,
    /// Where the path starts: `None` is the station itself; PSK BAND's
    /// reports, between two other stations, carry their receiver's place.
    pub from: Option<LatLon>,
}

impl Reception {
    /// Its band's name ("20"), or `None` off the amateur bands.
    pub(crate) fn band(&self) -> Option<&'static str> {
        bandplan::band_at(self.rf_hz).map(|b| b.name)
    }
}

/// The lowest report a mode decodes at, dB: what a report is measured from.
/// The WSJT modes quote SNR in 2500 Hz; the keyboard skimmer's is its own
/// signal-to-noise in the signal's bandwidth, from about 0 dB up.
pub(crate) fn decode_floor(mode: &str) -> f32 {
    match mode {
        "FT8" => -21.0,
        "FT4" => -17.0,
        "WSPR" => -28.0,
        "JT65" => -25.0,
        "JT9" => -26.0,
        "Q65" => -27.0,
        "FST4" | "FST4W" => -30.0,
        "MSK144" => -8.0,
        "JTTY" => -20.0,
        _ => 0.0,
    }
}

/// A report's weight before it ages: its margin above the floor, from 0.15
/// (just decoded) to 1 (24 dB clear). A reception with no report counts
/// as a weak one, so it adds to how busy a place is without claiming more.
pub(crate) fn margin_weight(mode: &str, snr: Option<f32>) -> f64 {
    match snr {
        Some(s) => (((s - decode_floor(mode)) / 24.0) as f64).clamp(0.15, 1.0),
        None => 0.15,
    }
}

/// The points along the path from `a` to `b` that the field is painted at:
/// the middle of each hop of at most 3000 km.
pub(crate) fn hop_points(a: LatLon, b: LatLon) -> Vec<V3> {
    let km = distance_km(a, b);
    let hops = (km / 3000.0).ceil().max(1.0) as usize;
    let (va, vb) = (vec_ll(a), vec_ll(b));
    (0..hops).map(|k| slerp(va, vb, (k as f64 + 0.5) / hops as f64)).collect()
}

/// The field's cells, degrees.
pub(crate) const CELL_DEG: f64 = 2.0;
pub(crate) const ROWS: usize = (180.0 / CELL_DEG) as usize;
pub(crate) const COLS: usize = (360.0 / CELL_DEG) as usize;
/// How far a point's evidence spreads: a Gaussian this wide, about 900 km.
const SIGMA_DEG: f64 = 8.0;
/// A cell with this much in it counts as reached (BANDS OPEN).
pub(crate) const REACHED: f32 = 0.12;

/// The field on one band: how much evidence each cell holds.
#[derive(Clone, Debug)]
pub(crate) struct BandField {
    pub band: &'static str,
    pub cells: Vec<f32>,
    /// The share of the Earth's surface reached, 0 to 1.
    pub reach: f64,
    /// The strongest cell.
    pub peak: f32,
    /// PATHS: the receptions behind it, each counted at its decayed weight.
    pub paths: f64,
    /// BEST: the strongest report's margin over its mode's decode floor, dB.
    pub best_db: Option<f32>,
}

/// The centre of cell (`row`, `col`).
pub(crate) fn cell_centre(row: usize, col: usize) -> LatLon {
    LatLon {
        lat: 90.0 - (row as f64 + 0.5) * CELL_DEG,
        lon: -180.0 + (col as f64 + 0.5) * CELL_DEG,
    }
}

/// Build the field from `recs` as it stands at `now` (Unix seconds), each
/// reception's weight halving every `half_life_s`, from `qth`. Only the
/// sources in `use_src` count.
pub(crate) fn build(
    recs: &[Reception],
    qth: LatLon,
    now: f64,
    half_life_s: f64,
    use_src: &dyn Fn(Source) -> bool,
) -> Vec<BandField> {
    let centres: Vec<V3> =
        (0..ROWS * COLS).map(|i| vec_ll(cell_centre(i / COLS, i % COLS))).collect();
    let area: Vec<f64> = (0..ROWS).map(|r| cell_centre(r, 0).lat.to_radians().cos()).collect();
    let total: f64 = area.iter().sum::<f64>() * COLS as f64;
    let sigma = SIGMA_DEG.to_radians();
    let reach_rad = 3.0 * sigma;
    let mut out: Vec<BandField> = Vec::new();
    for r in recs.iter().filter(|r| use_src(r.src)) {
        let Some(band) = r.band() else { continue };
        let age = (now - r.utc).max(0.0);
        let w = (margin_weight(&r.mode, r.snr) * 0.5f64.powf(age / half_life_s)) as f32;
        if w < 1e-4 {
            continue;
        }
        let i = match out.iter().position(|f| f.band == band) {
            Some(i) => i,
            None => {
                out.push(BandField {
                    band,
                    cells: vec![0.0; ROWS * COLS],
                    reach: 0.0,
                    peak: 0.0,
                    paths: 0.0,
                    best_db: None,
                });
                out.len() - 1
            }
        };
        out[i].paths += w as f64;
        if let Some(snr) = r.snr {
            let m = snr - decode_floor(&r.mode);
            out[i].best_db = Some(out[i].best_db.map_or(m, |b| b.max(m)));
        }
        let cells = &mut out[i].cells;
        for p in hop_points(r.from.unwrap_or(qth), r.pos) {
            let pl = super::sphere::ll_of(p);
            let dlat = reach_rad.to_degrees();
            let row_lo = (((90.0 - pl.lat - dlat) / CELL_DEG).floor().max(0.0)) as usize;
            let row_hi = ((((90.0 - pl.lat + dlat) / CELL_DEG).ceil()) as usize).min(ROWS);
            for row in row_lo..row_hi {
                for col in 0..COLS {
                    let c = centres[row * COLS + col];
                    if dot(c, p) < reach_rad.cos() {
                        continue;
                    }
                    let a = angle(c, p);
                    cells[row * COLS + col] += w * (-(a * a) / (2.0 * sigma * sigma)).exp() as f32;
                }
            }
        }
    }
    for f in &mut out {
        f.peak = f.cells.iter().copied().fold(0.0, f32::max);
        let reached: f64 = f
            .cells
            .iter()
            .enumerate()
            .filter(|(_, v)| **v >= REACHED)
            .map(|(i, _)| area[i / COLS])
            .sum();
        f.reach = reached / total;
    }
    // In frequency order, as the bands are listed.
    out.sort_by_key(|f| bandplan::HAM_BANDS.iter().position(|b| b.name == f.band));
    out
}

/// The F2 layer's height the hop geometry uses, km.
const F2_KM: f64 = 300.0;

/// How much higher a frequency gets through on a path of `km` than straight
/// up: the secant of the angle the signal meets the layer at, on a curved
/// Earth, for one hop of at most 3000 km.
fn secant(km: f64) -> f64 {
    let half = (km.min(3000.0) / 2.0) / EARTH_KM;
    // The triangle from the Earth's centre to the path's end and to the
    // reflection point.
    let (r, rh) = (EARTH_KM, EARTH_KM + F2_KM);
    let d = (r * r + rh * rh - 2.0 * r * rh * half.cos()).sqrt();
    let sin_i = r * half.sin() / d;
    1.0 / (1.0 - sin_i * sin_i).sqrt()
}

/// HEARD ≥: the highest frequency that has demonstrably got through near
/// `qth`, as a 3000 km path would carry it, MHz: a floor (the ionosphere was
/// at least this good), never an estimate. A path counts when the middle of
/// one of its hops is within 1500 km of `qth` and it is longer than 300 km
/// (shorter ones may never have touched the layer), and two different
/// stations must show it. A path between two other stations (PSK BAND)
/// counts the same way: its hop over the station's sky is what matters.
pub(crate) fn heard_floor(recs: &[Reception], qth: LatLon, since: f64) -> Option<f64> {
    let q = vec_ll(qth);
    let mut found: Vec<(f64, &str)> = recs
        .iter()
        .filter(|r| r.utc >= since && r.band().is_some())
        .filter_map(|r| {
            let a = r.from.unwrap_or(qth);
            let km = distance_km(a, r.pos);
            if km < 300.0 {
                return None;
            }
            let hops = (km / 3000.0).ceil().max(1.0);
            hop_points(a, r.pos)
                .into_iter()
                .any(|p| angle(p, q) * EARTH_KM <= 1500.0)
                .then(|| (r.rf_hz / 1e6 * secant(3000.0) / secant(km / hops), r.call.as_str()))
        })
        .collect();
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    // The highest figure two stations reach.
    let mut calls: Vec<&str> = Vec::new();
    for (f, call) in found {
        if !calls.contains(&call) {
            calls.push(call);
        }
        if calls.len() >= 2 {
            return Some(f);
        }
    }
    None
}

/// The last hour of receptions, oldest first.
#[derive(Default)]
pub(crate) struct History {
    recs: VecDeque<Reception>,
}

/// How long receptions are kept, seconds.
pub(crate) const KEEP_S: f64 = 3600.0;
/// The most kept.
const KEEP_MAX: usize = 20_000;

impl History {
    pub(crate) fn add(&mut self, r: Reception) {
        // PSK Reporter and the WSPR sites answer with the whole hour each
        // time: one of each.
        if r.src.reverse()
            && self
                .recs
                .iter()
                .any(|x| x.src == r.src && x.call == r.call && x.utc == r.utc && x.rf_hz == r.rf_hz)
        {
            return;
        }
        self.recs.push_back(r);
        if self.recs.len() > KEEP_MAX {
            self.recs.pop_front();
        }
    }

    /// Drop what is older than an hour before `now`.
    pub(crate) fn prune(&mut self, now: f64) {
        while self.recs.front().is_some_and(|r| now - r.utc > KEEP_S) {
            self.recs.pop_front();
        }
    }

    pub(crate) fn all(&self) -> Vec<Reception> {
        self.recs.iter().cloned().collect()
    }

    /// The latest reception of each station on each band from each source,
    /// newest first: the HEARD list.
    pub(crate) fn latest(&self) -> Vec<Reception> {
        let mut out: Vec<Reception> = Vec::new();
        for r in self.recs.iter().rev() {
            if !out.iter().any(|x| x.call == r.call && x.band() == r.band() && x.src == r.src) {
                out.push(r.clone());
            }
        }
        out
    }

    pub(crate) fn len(&self) -> usize {
        self.recs.len()
    }

    /// The latest reception of `call`, from any source.
    pub(crate) fn last_of(&self, call: &str) -> Option<&Reception> {
        self.recs.iter().rev().find(|r| r.call.eq_ignore_ascii_case(call))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::grid_to_latlon;

    fn rec(call: &str, grid: &str, mhz: f64, mode: &str, snr: f32, utc: f64) -> Reception {
        Reception {
            call: call.into(),
            grid: grid.into(),
            pos: grid_to_latlon(grid).unwrap(),
            rf_hz: mhz * 1e6,
            mode: mode.into(),
            snr: Some(snr),
            utc,
            src: Source::Decode,
            row: None,
            from: None,
        }
    }

    #[test]
    fn a_report_counts_by_its_margin_above_its_own_modes_floor() {
        // −15 in FT8 and −11 in FT4 are the same margin.
        assert!(
            (margin_weight("FT8", Some(-15.0)) - margin_weight("FT4", Some(-11.0))).abs() < 1e-9
        );
        assert_eq!(margin_weight("FT8", Some(-30.0)), 0.15);
        assert_eq!(margin_weight("FT8", Some(10.0)), 1.0);
        assert_eq!(margin_weight("CW", None), 0.15);
    }

    #[test]
    fn a_path_is_painted_at_the_middle_of_each_hop() {
        let qth = grid_to_latlon("FN20").unwrap();
        // Philadelphia to Berlin, about 6600 km: three hops.
        let pts = hop_points(qth, grid_to_latlon("JO62").unwrap());
        assert_eq!(pts.len(), 3);
        // A short path: one point, halfway.
        let near = hop_points(qth, grid_to_latlon("EM12").unwrap());
        assert_eq!(near.len(), 1);
        let mid = super::super::sphere::ll_of(near[0]);
        assert!(mid.lat > 32.0 && mid.lat < 41.0 && mid.lon > -97.0 && mid.lon < -75.0, "{mid:?}");
    }

    #[test]
    fn the_field_lights_the_middle_of_the_path_not_the_far_station() {
        let qth = grid_to_latlon("FN20").unwrap();
        let now = 1_000_000.0;
        // São Paulo, 7600 km: three hops, in 20 m.
        let recs = vec![rec("PY2ABC", "GG66", 14.075, "FT8", -6.0, now - 60.0)];
        let f = build(&recs, qth, now, 2700.0, &|_| true);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].band, "20");
        let at = |lat: f64, lon: f64| {
            let row = ((90.0 - lat) / CELL_DEG) as usize;
            let col = ((lon + 180.0) / CELL_DEG) as usize;
            f[0].cells[row * COLS + col]
        };
        // The middle hop is over the Atlantic off Brazil.
        let mid = super::super::sphere::ll_of(hop_points(qth, recs[0].pos)[1]);
        assert!(at(mid.lat, mid.lon) > 0.5 * f[0].peak);
        // Nothing over Asia.
        assert!(at(35.0, 100.0) < 1e-3);
        assert!(f[0].reach > 0.0 && f[0].reach < 0.2);
    }

    #[test]
    fn evidence_halves_every_half_life_and_other_sources_can_be_left_out() {
        let qth = grid_to_latlon("FN20").unwrap();
        let now = 1_000_000.0;
        let fresh =
            build(&[rec("G4XYZ", "IO91", 14.075, "FT8", -6.0, now)], qth, now, 2700.0, &|_| true);
        let old = build(
            &[rec("G4XYZ", "IO91", 14.075, "FT8", -6.0, now - 2700.0)],
            qth,
            now,
            2700.0,
            &|_| true,
        );
        assert!((old[0].peak / fresh[0].peak - 0.5).abs() < 1e-4);
        let none =
            build(&[rec("G4XYZ", "IO91", 14.075, "FT8", -6.0, now)], qth, now, 2700.0, &|s| {
                s != Source::Decode
            });
        assert!(none.is_empty());
    }

    #[test]
    fn heard_floor_needs_two_stations_and_scales_a_short_path_up() {
        let qth = grid_to_latlon("FN20").unwrap();
        let now = 1_000_000.0;
        // One 15 m station 1400 km off (EM12, Dallas): not enough alone.
        let one = vec![rec("W5ABC", "EM12", 21.074, "FT8", -2.0, now)];
        assert_eq!(heard_floor(&one, qth, now - 3600.0), None);
        // A second: the floor is above 21 MHz, a short path being harder.
        let mut two = one.clone();
        two.push(rec("K5XYZ", "EM13", 21.074, "FT8", -9.0, now));
        let f = heard_floor(&two, qth, now - 3600.0).unwrap();
        assert!(f > 21.074 && f < 40.0, "{f}");
        // A neighbour 100 km off proves nothing about the sky.
        let near = vec![
            rec("W3AAA", "FN21", 28.074, "FT8", 0.0, now),
            rec("W3BBB", "FN10", 28.074, "FT8", 0.0, now),
        ];
        assert_eq!(heard_floor(&near, qth, now - 3600.0), None);
    }

    #[test]
    fn the_history_keeps_an_hour_and_lists_each_station_once() {
        let mut h = History::default();
        h.add(rec("DL1ABC", "JO62", 14.075, "FT8", -8.0, 100.0));
        h.add(rec("DL1ABC", "JO62", 14.075, "FT8", -5.0, 200.0));
        h.add(rec("DL1ABC", "JO62", 7.075, "FT8", -9.0, 300.0));
        let l = h.latest();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].rf_hz, 7.075e6);
        assert_eq!(l[1].snr, Some(-5.0));
        h.prune(3800.0);
        assert_eq!(h.len(), 2);
    }
}
