//! The phosphor (persistence) trace: every trace drawn leaves a glow that
//! fades over a second or two, so a signal that comes and goes, or one that
//! wanders, shows where it has been and how often, as on an analogue
//! spectrum analyser's long-persistence screen.
//!
//! A grid of hit densities, one cell a pixel or so; each frame fades it and
//! adds the new trace (joined column to column, so a steep skirt is a line,
//! not dots). The scope colours it through the waterfall's palette.

use eframe::egui::{Color32, ColorImage};

use super::palette::Palette;

/// The density grid.
#[derive(Debug, Clone, Default)]
pub struct Persistence {
    w: usize,
    h: usize,
    cells: Vec<f32>,
}

impl Persistence {
    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Make it `w` by `h`, starting dark if that is a change.
    pub fn resize(&mut self, w: usize, h: usize) {
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.cells = vec![0.0; w * h];
        }
    }

    pub fn clear(&mut self) {
        self.cells.fill(0.0);
    }

    /// Fade for `dt` seconds with time constant `tau`.
    pub fn fade(&mut self, dt: f32, tau: f32) {
        let k = (-dt.max(0.0) / tau.max(0.05)).exp();
        for c in &mut self.cells {
            *c *= k;
        }
    }

    /// Add one trace: `cols` levels (one a column, `w` of them) on a scale
    /// from `floor` (bottom row) to `ceil` (top row).
    pub fn add(&mut self, cols: &[f32], floor: f32, ceil: f32) {
        if self.w == 0 || self.h == 0 || cols.len() != self.w {
            return;
        }
        let h = self.h as f32;
        let row = |v: f32| -> usize {
            let t = ((v - floor) / (ceil - floor).max(1.0)).clamp(0.0, 1.0);
            ((1.0 - t) * (h - 1.0)).round() as usize
        };
        let mut prev = row(cols[0]);
        for (x, &v) in cols.iter().enumerate() {
            let r = row(v);
            let (a, b) = if r < prev { (r, prev) } else { (prev, r) };
            // From the previous column's level to this one's, so the trace is
            // joined; the far end belongs to the previous column.
            let (a, b) = if x == 0 { (r, r) } else { (a, b) };
            for y in a..=b {
                let c = &mut self.cells[y * self.w + x];
                *c = (*c + 1.0).min(64.0);
            }
            prev = r;
        }
    }

    /// The hits at (`x`, `y`).
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.cells.get(y * self.w + x).copied().unwrap_or(0.0)
    }

    /// As an image in `palette`'s colours: brighter where the trace has been
    /// more often, transparent where it has not been at all.
    pub fn image(&self, palette: Palette) -> ColorImage {
        let lut = palette.lut();
        let pixels = self
            .cells
            .iter()
            .map(|&c| {
                if c < 0.02 {
                    return Color32::TRANSPARENT;
                }
                // A frame or two of hits is already visible; a steady trace
                // saturates.
                let t = (c / 8.0).sqrt().min(1.0);
                let i = (40.0 + t * 215.0) as usize * 4;
                let a = (t * 255.0).max(60.0) as u8;
                Color32::from_rgba_unmultiplied(lut[i], lut[i + 1], lut[i + 2], a)
            })
            .collect();
        ColorImage::new([self.w, self.h], pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_is_joined_and_fades() {
        let mut p = Persistence::default();
        p.resize(4, 11);
        // floor -100 at row 10, ceil 0 at row 0.
        p.add(&[-100.0, -100.0, 0.0, -100.0], -100.0, 0.0);
        assert_eq!(p.at(0, 10), 1.0);
        assert_eq!(p.at(1, 10), 1.0);
        assert_eq!(p.at(1, 5), 0.0);
        // The jump to the top is drawn as a line in column 2, and back down
        // in column 3.
        assert!((0..=10).all(|y| p.at(2, y) == 1.0));
        assert!((0..=10).all(|y| p.at(3, y) == 1.0));
        p.fade(1.0, 1.0);
        assert!((p.at(0, 10) - (-1.0f32).exp()).abs() < 1e-6);
        let img = p.image(Palette::Elad);
        assert_eq!(img.size, [4, 11]);
        assert_eq!(img.pixels[5 * 4 + 1], Color32::TRANSPARENT);
        assert_ne!(img.pixels[10 * 4], Color32::TRANSPARENT);
    }

    #[test]
    fn a_resize_starts_dark_and_a_wrong_width_is_ignored() {
        let mut p = Persistence::default();
        p.resize(3, 3);
        p.add(&[0.0, 0.0, 0.0], -10.0, 0.0);
        p.resize(3, 3);
        assert_eq!(p.at(0, 0), 1.0);
        p.resize(4, 3);
        assert_eq!(p.at(0, 0), 0.0);
        p.add(&[0.0; 3], -10.0, 0.0);
        assert_eq!(p.at(0, 0), 0.0);
    }
}
