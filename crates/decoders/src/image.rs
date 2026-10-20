//! Pictures from the SSTV and WEFAX receivers, as RGBA buffers.
//!
//! The receivers hand over one finished scan line at a time; these collect
//! the lines into an image the panel can draw, and say what changed so it can
//! upload just that.

use crate::modem::sstv::SstvEvent;
use crate::modem::wefax::{MAX_LINES, WefaxEvent};

/// An 8-bit RGBA picture, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// `width × height × 4` bytes.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// A black, opaque picture.
    pub fn new(width: u32, height: u32) -> Self {
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        RgbaImage { width, height, pixels }
    }

    /// The bytes of row `y`.
    pub fn row(&self, y: u32) -> &[u8] {
        let w = self.width as usize * 4;
        &self.pixels[y as usize * w..(y as usize + 1) * w]
    }

    /// Set row `y` from `3 × width` RGB bytes (a short row fills from the left).
    pub fn set_row_rgb(&mut self, y: u32, rgb: &[u8]) {
        if y >= self.height {
            return;
        }
        let w = self.width as usize * 4;
        let row = &mut self.pixels[y as usize * w..(y as usize + 1) * w];
        for (px, c) in row.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
            px[..3].copy_from_slice(c);
            px[3] = 255;
        }
    }

    /// Set row `y` from `width` grey levels.
    pub fn set_row_gray(&mut self, y: u32, gray: &[u8]) {
        if y >= self.height {
            return;
        }
        let w = self.width as usize * 4;
        let row = &mut self.pixels[y as usize * w..(y as usize + 1) * w];
        for (px, &g) in row.chunks_exact_mut(4).zip(gray) {
            px.copy_from_slice(&[g, g, g, 255]);
        }
    }

    /// Grow (or shrink) to `height` rows, keeping the rows there are; new rows
    /// are black.
    pub fn set_height(&mut self, height: u32) {
        let old = self.pixels.len();
        self.pixels.resize(self.width as usize * height as usize * 4, 0);
        let from = old.min(self.pixels.len());
        for px in self.pixels[from..].chunks_exact_mut(4) {
            px[3] = 255;
        }
        self.height = height;
    }
}

/// What happened to the picture.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageUpdate {
    /// A new picture began: `label` names its mode. For WEFAX the height grows
    /// as lines arrive, so `height` is only the room made so far.
    Started { label: String, width: u32, height: u32 },
    /// Row `y` is finished; read it from [`Assembler::image`].
    Row { y: u32 },
    /// The picture is finished (or was cut short): the whole of it.
    Complete(RgbaImage),
    /// Something worth showing beside the picture: an SSTV station's FSK ID,
    /// or a mode this receiver has no decoder for.
    Note(String),
}

/// Collects a receiver's lines into the picture in progress.
#[derive(Debug, Default)]
pub struct Assembler {
    image: Option<RgbaImage>,
    /// Rows written, for cutting a WEFAX picture to what arrived.
    rows: u32,
}

impl Assembler {
    pub fn new() -> Self {
        Assembler::default()
    }

    /// The picture in progress (or the last one), if any.
    pub fn image(&self) -> Option<&RgbaImage> {
        self.image.as_ref()
    }

    fn finish(&mut self, out: &mut Vec<ImageUpdate>) {
        if let Some(mut img) = self.image.take() {
            if self.rows > 0 && self.rows < img.height {
                img.set_height(self.rows);
            }
            out.push(ImageUpdate::Complete(img.clone()));
            self.image = Some(img);
        }
        self.rows = 0;
    }

    /// Take in the SSTV receiver's events.
    pub fn sstv(
        &mut self,
        events: impl IntoIterator<Item = SstvEvent>,
        out: &mut Vec<ImageUpdate>,
    ) {
        for e in events {
            match e {
                SstvEvent::ModeDetected(mode) => {
                    if self.rows > 0 {
                        self.finish(out);
                    }
                    let (w, h) = mode.dimensions();
                    self.image = Some(RgbaImage::new(w as u32, h as u32));
                    self.rows = 0;
                    out.push(ImageUpdate::Started {
                        label: mode.label().to_string(),
                        width: w as u32,
                        height: h as u32,
                    });
                }
                SstvEvent::Line { y, rgb } => {
                    if let Some(img) = &mut self.image {
                        img.set_row_rgb(y as u32, &rgb);
                        self.rows = self.rows.max(y as u32 + 1);
                        out.push(ImageUpdate::Row { y: y as u32 });
                    }
                }
                SstvEvent::ImageComplete => {
                    // An SSTV picture keeps its full size even if lines were lost.
                    self.rows = 0;
                    if let Some(img) = &self.image {
                        out.push(ImageUpdate::Complete(img.clone()));
                    }
                }
                SstvEvent::FskId(id) => out.push(ImageUpdate::Note(format!("ID {id}"))),
                SstvEvent::UnsupportedMode { code, name } => {
                    out.push(ImageUpdate::Note(match name {
                        Some(n) => format!("{n} (VIS {code}) is not supported"),
                        None => format!("unknown SSTV mode (VIS {code})"),
                    }))
                }
            }
        }
    }

    /// Take in the WEFAX receiver's events.
    pub fn wefax(
        &mut self,
        events: impl IntoIterator<Item = WefaxEvent>,
        out: &mut Vec<ImageUpdate>,
    ) {
        /// Rows added at a time as a chart grows.
        const GROW: u32 = 256;
        for e in events {
            match e {
                WefaxEvent::Start { ioc, lpm, .. } => {
                    if self.rows > 0 {
                        self.finish(out);
                    }
                    let w = ioc.width() as u32;
                    self.image = Some(RgbaImage::new(w, GROW));
                    self.rows = 0;
                    out.push(ImageUpdate::Started {
                        label: format!("WEFAX {} {}", ioc.label(), lpm.label()),
                        width: w,
                        height: GROW,
                    });
                }
                WefaxEvent::Line { y, gray } => {
                    if let Some(img) = &mut self.image {
                        let y = y as u32;
                        if y >= img.height {
                            let h = (y + GROW).min(MAX_LINES as u32);
                            img.set_height(h.max(y + 1));
                        }
                        img.set_row_gray(y, &gray);
                        self.rows = self.rows.max(y + 1);
                        out.push(ImageUpdate::Row { y });
                    }
                }
                WefaxEvent::Complete { .. } => self.finish(out),
                WefaxEvent::Phased { .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::types::SstvMode;
    use crate::modem::wefax::{Ioc, Lpm};

    #[test]
    fn sstv_lines_land_in_their_rows() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        let (w, h) = SstvMode::Robot36.dimensions();
        a.sstv(
            [
                SstvEvent::ModeDetected(SstvMode::Robot36),
                SstvEvent::Line { y: 3, rgb: [10, 20, 30].repeat(w as usize) },
                SstvEvent::ImageComplete,
            ],
            &mut out,
        );
        assert!(matches!(&out[0], ImageUpdate::Started { width, height, .. }
            if *width == w as u32 && *height == h as u32));
        assert_eq!(out[1], ImageUpdate::Row { y: 3 });
        let ImageUpdate::Complete(img) = &out[2] else { panic!("{out:?}") };
        assert_eq!(&img.row(3)[..8], &[10, 20, 30, 255, 10, 20, 30, 255]);
        assert_eq!(&img.row(2)[..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn a_wefax_chart_grows_and_is_cut_to_what_arrived() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        let w = Ioc::I576.width() as usize;
        let mut ev = vec![WefaxEvent::Start { ioc: Ioc::I576, lpm: Lpm::default(), auto: true }];
        for y in 0..300u16 {
            ev.push(WefaxEvent::Line { y, gray: vec![(y % 256) as u8; w] });
        }
        ev.push(WefaxEvent::Complete { lines: 300, by_tone: true });
        a.wefax(ev, &mut out);
        let ImageUpdate::Complete(img) = out.last().unwrap() else { panic!() };
        assert_eq!((img.width as usize, img.height), (w, 300));
        assert_eq!(&img.row(299)[..4], &[43, 43, 43, 255]);
    }
}
