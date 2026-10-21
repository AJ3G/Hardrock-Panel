// The gradient builder and every palette but ELAD are ported from SDRoxide
// (https://github.com/dividebysandwich/sdroxide), crates/sdroxide-ui/src/colormap.rs,
// GPL-3.0-or-later. ELAD is this panel's own, in the DUO-ART's blues.

//! The waterfall's (and the solid 3D surface's) palettes: 256-entry colour
//! tables built from a few anchors.
//!
//! ELAD, the default, stays in blues like the rest of the panel. Icom and
//! Classic run up to yellow and red, the agreed exception to "yellow and red
//! for warnings only" (AJ3G): a waterfall's ramp is a level, not a warning.
//! The slot lines' green and red over the waterfall are the other named
//! exception ([`super::slotlines`], decision 51).

use serde::{Deserialize, Serialize};

/// A waterfall palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Palette {
    /// Near-black through navy and the DUO-ART's status blue to white.
    #[default]
    Elad,
    /// Icom's SDR waterfall: black, blue, cyan, green, yellow, orange, red.
    Icom,
    /// PowerSDR's: black, blue, cyan, green, yellow, red, white.
    Classic,
    /// SDR#'s traditional blue, navy floor and white, yellow and red peaks.
    Blue,
    Viridis,
    Matrix,
    Amber,
    Gray,
}

type Anchors = &'static [(f32, [u8; 3])];

impl Palette {
    pub const ALL: [Palette; 8] = [
        Palette::Elad,
        Palette::Icom,
        Palette::Classic,
        Palette::Blue,
        Palette::Viridis,
        Palette::Matrix,
        Palette::Amber,
        Palette::Gray,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Palette::Elad => "ELAD",
            Palette::Icom => "Icom",
            Palette::Classic => "Classic",
            Palette::Blue => "Blue",
            Palette::Viridis => "Viridis",
            Palette::Matrix => "Matrix",
            Palette::Amber => "Amber",
            Palette::Gray => "Gray",
        }
    }

    /// The anchors: (position 0..1, RGB), starting at 0 and ending at 1.
    pub fn anchors(self) -> Anchors {
        match self {
            Palette::Elad => &[
                (0.00, [5, 7, 11]),
                (0.30, [11, 42, 92]),
                (0.60, [31, 111, 214]),
                (0.85, [111, 211, 255]),
                (1.00, [242, 251, 255]),
            ],
            Palette::Icom => &[
                (0.00, [0, 0, 0]),
                (0.12, [0, 0, 92]),
                (0.30, [0, 40, 210]),
                (0.46, [0, 170, 220]),
                (0.58, [0, 210, 180]),
                (0.70, [40, 210, 40]),
                (0.83, [235, 235, 30]),
                (0.93, [242, 130, 20]),
                (1.00, [230, 20, 20]),
            ],
            Palette::Classic => &[
                (0.00, [0, 0, 0]),
                (0.25, [0, 0, 160]),
                (0.45, [0, 180, 200]),
                (0.60, [40, 200, 60]),
                (0.75, [230, 230, 40]),
                (0.90, [240, 60, 30]),
                (1.00, [255, 255, 255]),
            ],
            Palette::Blue => &[
                (0.00, [0, 0, 32]),
                (0.15, [0, 0, 80]),
                (0.28, [0, 0, 145]),
                (0.38, [30, 144, 255]),
                (0.46, [255, 255, 255]),
                (0.54, [255, 255, 0]),
                (0.66, [254, 109, 22]),
                (0.78, [255, 0, 0]),
                (0.89, [198, 0, 0]),
                (1.00, [117, 0, 0]),
            ],
            Palette::Viridis => &[
                (0.00, [68, 1, 84]),
                (0.25, [59, 82, 139]),
                (0.50, [33, 145, 140]),
                (0.75, [94, 201, 98]),
                (1.00, [253, 231, 37]),
            ],
            Palette::Matrix => &[
                (0.00, [0, 0, 0]),
                (0.30, [0, 36, 8]),
                (0.55, [0, 150, 40]),
                (0.78, [46, 240, 88]),
                (1.00, [200, 255, 205]),
            ],
            Palette::Amber => &[
                (0.00, [0, 0, 0]),
                (0.18, [26, 12, 0]),
                (0.38, [92, 42, 0]),
                (0.56, [176, 86, 0]),
                (0.72, [255, 176, 0]),
                (0.88, [255, 210, 96]),
                (1.00, [255, 234, 204]),
            ],
            Palette::Gray => &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
        }
    }

    /// The 256-entry table, RGBA8, as the GPU takes it.
    pub fn lut(self) -> [u8; 256 * 4] {
        gradient(self.anchors())
    }

    /// The colour at `t` in 0..1.
    pub fn at(self, t: f32) -> [u8; 3] {
        let lut = self.lut();
        let i = (t.clamp(0.0, 1.0) * 255.0).round() as usize * 4;
        [lut[i], lut[i + 1], lut[i + 2]]
    }
}

/// Piecewise-linear gradient through the anchors, as SDRoxide builds it.
fn gradient(anchors: Anchors) -> [u8; 256 * 4] {
    let mut out = [0u8; 256 * 4];
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let seg = anchors.windows(2).find(|w| t <= w[1].0).unwrap_or(&anchors[anchors.len() - 2..]);
        let (t0, c0) = seg[0];
        let (t1, c1) = seg[1];
        let f = if t1 > t0 { ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
        for ch in 0..3 {
            out[i * 4 + ch] = (c0[ch] as f32 + f * (c1[ch] as f32 - c0[ch] as f32)) as u8;
        }
        out[i * 4 + 3] = 255;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_runs_from_its_first_anchor_to_its_last() {
        for p in Palette::ALL {
            let a = p.anchors();
            assert_eq!(a[0].0, 0.0, "{p:?}");
            assert_eq!(a[a.len() - 1].0, 1.0, "{p:?}");
            assert!(a.windows(2).all(|w| w[0].0 < w[1].0), "{p:?} anchors in order");
            assert_eq!(p.at(0.0), a[0].1, "{p:?}");
            assert_eq!(p.at(1.0), a[a.len() - 1].1, "{p:?}");
            assert!(p.lut().chunks(4).all(|c| c[3] == 255));
        }
    }

    #[test]
    fn elad_stays_in_blues() {
        // Blue never below red or green anywhere along it: no yellow or red,
        // which are kept for warnings.
        let lut = Palette::Elad.lut();
        for c in lut.chunks(4) {
            assert!(c[2] >= c[0] && c[2] >= c[1], "{c:?}");
        }
    }

    #[test]
    fn a_ramp_is_interpolated_between_anchors() {
        // Halfway between Gray's black and white.
        let [r, g, b] = Palette::Gray.at(0.5);
        assert!((126..=129).contains(&r) && r == g && g == b);
        // Icom passes its green anchor exactly at 0.70.
        let lut = Palette::Icom.lut();
        let i = (0.70f32 * 255.0) as usize;
        assert!(lut[i * 4 + 1] > 190, "green near 0.70");
    }

    #[test]
    fn the_default_is_elad() {
        assert_eq!(Palette::default(), Palette::Elad);
    }
}
