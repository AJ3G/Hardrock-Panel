//! Levels: S-units, the labels on the level scale, and FIT.
//!
//! S-units are the HF convention: S9 is −73 dBm at the antenna and each unit
//! is 6 dB, so S1 is −121 dBm; above S9 a level is given in dB over it.

use super::settings::{LEVEL_MAX, LEVEL_MIN, Scale};

/// S9 on HF, dBm.
pub const S9_DBM: f32 = -73.0;
/// dB per S-unit.
pub const DB_PER_S: f32 = 6.0;

/// `dbm` as S-units: "S0" to "S9", then "+10", "+20" and so on over S9.
pub fn s_unit(dbm: f32) -> String {
    if dbm <= S9_DBM {
        let s = (9.0 + (dbm - S9_DBM) / DB_PER_S).round().max(0.0);
        format!("S{s:.0}")
    } else {
        format!("+{:.0}", dbm - S9_DBM)
    }
}

/// A level as the chosen scale writes it in readouts.
pub fn level_text(dbm: f32, scale: Scale) -> String {
    match scale {
        Scale::Dbm => format!("{dbm:.0} dBm"),
        Scale::SUnits => s_unit(dbm),
        Scale::Both => format!("{} · {dbm:.0} dBm", s_unit(dbm)),
    }
}

/// The S-unit marks on the scale: S1, S3, S5, S7, S9 and 10 to 40 dB over.
pub fn s_marks() -> Vec<(f32, &'static str)> {
    vec![
        (S9_DBM - 8.0 * DB_PER_S, "S1"),
        (S9_DBM - 6.0 * DB_PER_S, "S3"),
        (S9_DBM - 4.0 * DB_PER_S, "S5"),
        (S9_DBM - 2.0 * DB_PER_S, "S7"),
        (S9_DBM, "S9"),
        (S9_DBM + 10.0, "+10"),
        (S9_DBM + 20.0, "+20"),
        (S9_DBM + 30.0, "+30"),
        (S9_DBM + 40.0, "+40"),
    ]
}

/// One labelled line on the level scale.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelMark {
    pub dbm: f32,
    /// What the gutter writes beside it; empty for a line with no label.
    pub label: String,
    /// A full-width grid line (true), or only a tick at the gutter.
    pub line: bool,
    /// S9, which is drawn in the accent colour.
    pub s9: bool,
}

/// The lines and labels the level scale carries between `floor` and `ceil`.
///
/// dBm: a line every 10 dB, labelled every 20. S-units: a line and label at
/// each S mark. Both: the dBm lines, with the S marks as ticks in the gutter.
pub fn level_marks(floor: f32, ceil: f32, scale: Scale) -> Vec<LevelMark> {
    let mut out = Vec::new();
    let dbm = scale != Scale::SUnits;
    let s = scale != Scale::Dbm;
    if dbm {
        let mut v = (floor / 10.0).ceil() * 10.0;
        while v <= ceil {
            let labelled = (v as i32) % 20 == 0;
            out.push(LevelMark {
                dbm: v,
                // In Both the gutter has the S-units; dBm goes inside the plot.
                label: if labelled { format!("{v:.0}") } else { String::new() },
                line: true,
                s9: false,
            });
            v += 10.0;
        }
    }
    if s {
        for (v, l) in s_marks() {
            if v < floor || v > ceil {
                continue;
            }
            out.push(LevelMark { dbm: v, label: l.to_string(), line: !dbm, s9: v == S9_DBM });
        }
    }
    out
}

/// FIT's target: the floor 8 dB under the quietest fifth of the columns and
/// the ceiling 8 dB over the strongest, at least 40 dB above the floor.
pub fn fit_target(columns: &[f32]) -> Option<(f32, f32)> {
    let mut v: Vec<f32> = columns.iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < 8 {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let floor = (v[v.len() / 5] - 8.0).round();
    let ceil = (v[v.len() - 1] + 8.0).max(floor + 40.0).round();
    let floor = floor.clamp(LEVEL_MIN, LEVEL_MAX - 40.0);
    Some((floor, ceil.clamp(floor + 40.0, LEVEL_MAX)))
}

/// Move `(floor, ceil)` part of the way towards FIT's target, so the scale
/// glides rather than jumps.
pub fn fit_step(cur: (f32, f32), target: (f32, f32)) -> (f32, f32) {
    const K: f32 = 0.15;
    (cur.0 + (target.0 - cur.0) * K, cur.1 + (target.1 - cur.1) * K)
}

/// The receiver's signal-to-noise ratio, dB: its passband level against the
/// noise the same passband collects.
///
/// `noise_bin_dbm` is one bin's noise from the spectrum, which with the
/// window's noise bandwidth `enbw` (in bins) of `bin_hz` each is a noise
/// density of `noise_bin / (enbw · bin_hz)`; the passband `bw_hz` wide
/// collects `bw_hz` of it. The level includes that noise, so it is taken out:
/// a passband of noise alone reads 0 (or less), not 0 dB plus the noise.
pub fn snr_db(level_dbm: f32, noise_bin_dbm: f32, bin_hz: f64, enbw: f32, bw_hz: f64) -> f32 {
    if bin_hz <= 0.0 || bw_hz <= 0.0 {
        return 0.0;
    }
    let noise = noise_bin_dbm as f64 + 10.0 * (bw_hz / (enbw.max(0.1) as f64 * bin_hz)).log10();
    let s = 10f64.powf(level_dbm as f64 / 10.0) - 10f64.powf(noise / 10.0);
    if s <= 0.0 {
        return 0.0;
    }
    (10.0 * s.log10() - noise) as f32
}

/// The waterfall's own levels from the noise floor: the floor a little under
/// the noise, so it is a dark texture rather than black or a wash, and 45 dB
/// of range over it (wider if the strongest signal needs it, up to 70).
pub fn wf_auto_levels(noise_dbm: f32, strongest_dbm: f32) -> (f32, f32) {
    let floor = noise_dbm - 4.0;
    let range = (strongest_dbm - floor + 3.0).clamp(45.0, 70.0);
    (floor, floor + range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s_units_follow_the_hf_convention() {
        assert_eq!(s_unit(-73.0), "S9");
        assert_eq!(s_unit(-79.0), "S8");
        assert_eq!(s_unit(-121.0), "S1");
        assert_eq!(s_unit(-127.0), "S0");
        assert_eq!(s_unit(-160.0), "S0");
        assert_eq!(s_unit(-63.0), "+10");
        assert_eq!(s_unit(-33.0), "+40");
        // Rounded to the nearest unit.
        assert_eq!(s_unit(-75.9), "S9");
        assert_eq!(s_unit(-76.1), "S8");
    }

    #[test]
    fn readouts_follow_the_scale() {
        assert_eq!(level_text(-73.0, Scale::Dbm), "-73 dBm");
        assert_eq!(level_text(-73.0, Scale::SUnits), "S9");
        assert_eq!(level_text(-53.0, Scale::Both), "+20 · -53 dBm");
    }

    #[test]
    fn the_scale_carries_dbm_s_units_or_both() {
        let d = level_marks(-130.0, -40.0, Scale::Dbm);
        assert!(d.iter().all(|m| m.line));
        assert_eq!(d.first().unwrap().dbm, -130.0);
        assert_eq!(d.last().unwrap().dbm, -40.0);
        assert_eq!(d.iter().filter(|m| !m.label.is_empty()).count(), 5); // -120..-40 by 20

        let s = level_marks(-130.0, -40.0, Scale::SUnits);
        let labels: Vec<_> = s.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["S1", "S3", "S5", "S7", "S9", "+10", "+20", "+30"]);
        assert!(s.iter().all(|m| m.line));
        assert!(s.iter().find(|m| m.label == "S9").unwrap().s9);

        let b = level_marks(-130.0, -40.0, Scale::Both);
        // dBm lines plus S ticks that are not full lines.
        assert!(b.iter().any(|m| m.line && m.label == "-100"));
        assert!(b.iter().any(|m| !m.line && m.label == "S9"));
    }

    #[test]
    fn fit_sits_under_the_noise_and_over_the_strongest() {
        let mut cols = vec![-120.0f32; 100];
        cols[50] = -60.0;
        let (f, c) = fit_target(&cols).unwrap();
        assert_eq!(f, -128.0);
        assert_eq!(c, -52.0);
        // A flat band still gets 40 dB of range.
        let (f, c) = fit_target(&[-110.0; 50]).unwrap();
        assert_eq!(c - f, 40.0);
        assert!(fit_target(&[]).is_none());
        // Glides a sixth or so of the way each step.
        let (a, b) = fit_step((-130.0, -40.0), (-120.0, -60.0));
        assert!((a - -128.5).abs() < 1e-4 && (b - -43.0).abs() < 1e-4);
    }

    #[test]
    fn snr_takes_the_noise_out_of_the_passband() {
        // -130 dBm a bin, Blackman-Harris (2 bins), 10 Hz bins: -143 dBm/Hz,
        // so a 2.4 kHz passband collects -109.2 dBm of noise.
        let noise = -130.0 - 10.0 * 20f32.log10() + 10.0 * 2400f32.log10();
        // Noise alone: no SNR.
        assert_eq!(snr_db(noise, -130.0, 10.0, 2.0, 2400.0), 0.0);
        // A signal 20 dB over that noise, plus the noise.
        let p = 10f64.powf((noise as f64 + 20.0) / 10.0) + 10f64.powf(noise as f64 / 10.0);
        let level = (10.0 * p.log10()) as f32;
        assert!((snr_db(level, -130.0, 10.0, 2.0, 2400.0) - 20.0).abs() < 0.01);
        assert_eq!(snr_db(-50.0, -130.0, 0.0, 2.0, 2400.0), 0.0);
    }

    #[test]
    fn waterfall_auto_levels_sit_on_the_noise() {
        assert_eq!(wf_auto_levels(-120.0, -100.0), (-124.0, -79.0));
        assert_eq!(wf_auto_levels(-120.0, -60.0), (-124.0, -124.0 + 67.0));
        assert_eq!(wf_auto_levels(-120.0, 0.0), (-124.0, -54.0));
    }
}
