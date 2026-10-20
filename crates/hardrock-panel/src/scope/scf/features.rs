//! The readout: the strongest cycle frequencies in the cyclic-domain
//! profile, as a CFAR detector would pick them, and what they mean.
//!
//! Each α row's strongest cell (dB) is compared with the median of all rows,
//! in units of the rows' spread (1.4826 × the median absolute deviation, a
//! σ that a few strong features do not inflate). Rows near α = 0 (the PSD
//! itself, for x·x*) and near ±fs (the same, aliased) are left out.

use super::engine::ProfileRow;

/// One detected cycle frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    /// Cycle frequency, Hz.
    pub alpha_hz: f64,
    /// Where across the lane it is strongest, Hz from the lane's centre.
    pub f_hz: f64,
    /// How far over the median row it stands, σ.
    pub sigma: f32,
    /// Its level, dB over the median row.
    pub db: f32,
    /// A known mode with this rate, if any.
    pub mode: Option<&'static str>,
}

/// The grid's geometry in Hz, for turning rows and columns into frequencies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axes {
    pub a_lo: f64,
    pub a_hi: f64,
    pub f_lo: f64,
    pub f_hi: f64,
    pub grid_f: u32,
    pub fs: f64,
    /// Δα: rows closer than a few of these to α = 0 are the PSD.
    pub d_alpha: f64,
}

impl Axes {
    pub fn alpha_of_row(&self, i: usize, rows: usize) -> f64 {
        self.a_lo + (i as f64 + 0.5) * (self.a_hi - self.a_lo) / rows as f64
    }

    pub fn f_of_col(&self, c: f64) -> f64 {
        self.f_lo + (c + 0.5) * (self.f_hi - self.f_lo) / self.grid_f as f64
    }
}

/// Symbol and keying rates of modes the SCF can be asked about, Hz.
const RATES: &[(f64, &str)] = &[
    (1.4648, "WSPR"),
    (1.7361, "JT9"),
    (2.6917, "JT65"),
    (6.25, "FT8"),
    (20.833, "FT4"),
    (31.25, "PSK31"),
    (45.45, "RTTY 45"),
    (50.0, "RTTY 50"),
    (62.5, "PSK63"),
    (125.0, "PSK125"),
];

/// The known mode at symbol rate `alpha` within `tol` Hz.
pub fn mode_at(alpha: f64, tol: f64) -> Option<&'static str> {
    RATES.iter().find(|(r, _)| (alpha.abs() - r).abs() <= tol.max(0.01 * r)).map(|&(_, n)| n)
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// The noise level and spread of the profile: (median row max, σ), dB.
pub fn floor(prof: &[ProfileRow], keep: &dyn Fn(usize) -> bool) -> Option<(f32, f32)> {
    let mut v: Vec<f32> = prof
        .iter()
        .enumerate()
        .filter(|(i, r)| keep(*i) && r[0] > -250.0)
        .map(|(_, r)| r[0])
        .collect();
    if v.len() < 8 {
        return None;
    }
    let med = median(&mut v);
    let mut dev: Vec<f32> = v.iter().map(|x| (x - med).abs()).collect();
    let mad = median(&mut dev);
    Some((med, (1.4826 * mad).max(0.05)))
}

/// The strongest `max` features standing `thresh` σ or more over the
/// median row, strongest first. `conj`: the profile is of x·x, whose
/// features sit at twice a carrier (so α = 0 is not masked).
pub fn features(
    prof: &[ProfileRow],
    ax: &Axes,
    conj: bool,
    thresh: f32,
    max: usize,
) -> Vec<Feature> {
    let rows = prof.len();
    let mask = 3.0 * ax.d_alpha;
    let keep = |i: usize| {
        let a = ax.alpha_of_row(i, rows).abs();
        conj || (a > mask && a < ax.fs - mask)
    };
    let Some((med, sigma)) = floor(prof, &keep) else { return Vec::new() };
    let mut cands: Vec<(usize, f32)> = (0..rows)
        .filter(|&i| keep(i))
        .filter(|&i| {
            let v = prof[i][0];
            (i == 0 || prof[i - 1][0] <= v) && (i + 1 == rows || prof[i + 1][0] < v)
        })
        .map(|i| (i, (prof[i][0] - med) / sigma))
        .filter(|&(_, z)| z >= thresh)
        .collect();
    cands.sort_by(|a, b| b.1.total_cmp(&a.1));
    let row_hz = (ax.a_hi - ax.a_lo) / rows as f64;
    let mut out: Vec<Feature> = Vec::new();
    for (i, z) in cands {
        let alpha = ax.alpha_of_row(i, rows);
        if out.iter().any(|f| (f.alpha_hz - alpha).abs() < 2.5 * row_hz) {
            continue;
        }
        out.push(Feature {
            alpha_hz: alpha,
            f_hz: ax.f_of_col(prof[i][1] as f64),
            sigma: z,
            db: prof[i][0] - med,
            mode: if conj { None } else { mode_at(alpha, row_hz.max(ax.d_alpha)) },
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_over_the_noise_is_found_and_named_and_alpha_zero_is_not() {
        let ax = Axes {
            a_lo: 0.0,
            a_hi: 64.0,
            f_lo: -1500.0,
            f_hi: 1500.0,
            grid_f: 64,
            fs: 3000.0,
            d_alpha: 0.73,
        };
        // 128 rows of 0.5 Hz; noise rows wobble ±0.5 dB about −18 dB.
        let mut prof: Vec<ProfileRow> = (0..128)
            .map(|i| [-18.0 + ((i * 37) % 11) as f32 / 10.0 - 0.5, 10.0, -21.0, 441.0])
            .collect();
        prof[0][0] = 0.0; // α ≈ 0: the PSD
        prof[12][0] = -8.0; // α = 6.25 Hz
        prof[12][1] = 40.0;
        prof[41][0] = -14.0; // α = 20.75 Hz, weaker
        let f = features(&prof, &ax, false, 5.0, 4);
        assert_eq!(f.len(), 2, "{f:?}");
        assert_eq!(f[0].alpha_hz, 6.25);
        assert_eq!(f[0].mode, Some("FT8"));
        assert!((f[0].f_hz - (-1500.0 + 40.5 * 3000.0 / 64.0)).abs() < 1e-9);
        assert_eq!(f[1].mode, Some("FT4"));
        assert!(f[0].sigma > f[1].sigma);
    }

    #[test]
    fn flat_noise_has_no_features() {
        let ax = Axes {
            a_lo: 0.0,
            a_hi: 100.0,
            f_lo: -1500.0,
            f_hi: 1500.0,
            grid_f: 64,
            fs: 3000.0,
            d_alpha: 0.73,
        };
        let prof: Vec<ProfileRow> =
            (0..256).map(|i| [-18.0 + ((i * 7919) % 13) as f32 / 13.0, 0.0, 0.0, 0.0]).collect();
        assert!(features(&prof, &ax, false, 5.0, 4).is_empty());
    }
}
