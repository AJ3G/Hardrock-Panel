//! The map picture: one equirectangular image the globe is wrapped in (and
//! the flat map shows), painted from the land, the Sun and the layers that
//! are on. The lines (coasts, borders, the terminator, the paths) are drawn
//! over it each frame; what is here changes slowly, so it is painted again
//! only when something in it has changed, on a thread of its own.

use super::fetch::{Aurora, Clouds};
use super::field::{self, BandField};
use super::geo::{TEX_H, TEX_W, geo};
use crate::station::LatLon;

/// What the picture shows.
#[derive(Clone)]
pub(crate) struct Recipe {
    pub sun: LatLon,
    pub night: bool,
    pub grey: bool,
    pub lights: bool,
    pub clouds: Option<Clouds>,
    pub aurora: Option<Aurora>,
    /// The PROP layer: one band in the ramp, or every band in its colour.
    pub prop: Option<Prop>,
}

#[derive(Clone)]
pub(crate) struct Prop {
    pub fields: Vec<BandField>,
    pub one: Option<&'static str>,
}

pub(crate) const OCEAN: [f32; 3] = [0x06 as f32, 0x10 as f32, 0x1a as f32];
pub(crate) const LAND: [f32; 3] = [0x17 as f32, 0x28 as f32, 0x3b as f32];
const DAY: [f32; 3] = [0x6f as f32, 0x9f as f32, 0xd0 as f32];
const GREY: [f32; 3] = [0xa9 as f32, 0xc4 as f32, 0xff as f32];
const CLOUD: [f32; 3] = [0xdf as f32, 0xe8 as f32, 0xf2 as f32];
const AURORA: [f32; 3] = [0x41 as f32, 0xd1 as f32, 0x7a as f32];
const LIGHT: [f32; 3] = [0xff as f32, 0xe6 as f32, 0xbf as f32];

/// ONE BAND's ramp, weak to strong: blue to white, so yellow and red stay
/// the panel's warnings.
pub(crate) const RAMP: [[u8; 3]; 6] = [
    [0x17, 0x3f, 0x78],
    [0x1f, 0x5f, 0xae],
    [0x2f, 0x86, 0xd9],
    [0x3f, 0xb2, 0xe8],
    [0x8a, 0xd8, 0xf0],
    [0xe6, 0xf7, 0xff],
];

/// Each band's colour for ALL BANDS (and its paths), none of them the
/// warnings' yellow or red.
pub(crate) fn band_colour(band: &str) -> [u8; 3] {
    match band {
        "160" => [0x8a, 0x7d, 0xff],
        "80" => [0xb0, 0x7c, 0xff],
        "60" => [0x9a, 0x8c, 0xd8],
        "40" => [0x4a, 0xa8, 0xff],
        "30" => [0x3f, 0xd0, 0xe8],
        "20" => [0x41, 0xd1, 0x7a],
        "17" => [0x7f, 0xe0, 0xc0],
        "15" => [0xe0, 0x7c, 0xff],
        "12" => [0x9f, 0xb4, 0xff],
        "10" => [0xff, 0x9a, 0xd5],
        _ => [0xc9, 0xd3, 0xde],
    }
}

/// Where `t` (0 to 1) falls on the ramp.
pub(crate) fn ramp(t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0) * (RAMP.len() - 1) as f32;
    let i = (t.floor() as usize).min(RAMP.len() - 2);
    let f = t - i as f32;
    let (a, b) = (RAMP[i], RAMP[i + 1]);
    [0, 1, 2].map(|k| a[k] as f32 + (b[k] as f32 - a[k] as f32) * f)
}

fn over(px: &mut [f32; 3], c: [f32; 3], a: f32) {
    let a = a.clamp(0.0, 1.0);
    for k in 0..3 {
        px[k] += (c[k] - px[k]) * a;
    }
}

fn smooth(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A field's value at (lat, lon), from its 2° cells, bilinear.
fn sample(cells: &[f32], lat: f64, lon: f64) -> f32 {
    let fr = ((90.0 - lat) / field::CELL_DEG - 0.5).clamp(0.0, (field::ROWS - 1) as f64);
    let fc = (lon + 180.0) / field::CELL_DEG - 0.5;
    let (r0, c0) = (fr.floor() as usize, fc.floor() as isize);
    let (dr, dc) = ((fr - r0 as f64) as f32, (fc - c0 as f64) as f32);
    let r1 = (r0 + 1).min(field::ROWS - 1);
    let col = |c: isize| c.rem_euclid(field::COLS as isize) as usize;
    let (c0u, c1u) = (col(c0), col(c0 + 1));
    let g = |r: usize, c: usize| cells[r * field::COLS + c];
    let top = g(r0, c0u) * (1.0 - dc) + g(r0, c1u) * dc;
    let bot = g(r1, c0u) * (1.0 - dc) + g(r1, c1u) * dc;
    top * (1.0 - dr) + bot * dr
}

/// Paint the picture, RGBA, `TEX_W` × `TEX_H`.
pub(crate) fn paint(r: &Recipe) -> Vec<u8> {
    let g = geo();
    let (w, h) = (TEX_W, TEX_H);
    let mut out = vec![0u8; w * h * 4];
    let (sd, cd) = (r.sun.lat.to_radians().sin(), r.sun.lat.to_radians().cos());
    let cos_dl: Vec<f64> = (0..w)
        .map(|c| ((-180.0 + (c as f64 + 0.5) * 360.0 / w as f64) - r.sun.lon).to_radians().cos())
        .collect();
    let peak = r.prop.as_ref().map(|p| {
        p.fields
            .iter()
            .filter(|f| p.one.is_none_or(|b| b == f.band))
            .map(|f| f.peak)
            .fold(0.0, f32::max)
    });
    for row in 0..h {
        let lat = 90.0 - (row as f64 + 0.5) * 180.0 / h as f64;
        let (sl, cl) = (lat.to_radians().sin(), lat.to_radians().cos());
        for col in 0..w {
            let i = row * w + col;
            let lon = -180.0 + (col as f64 + 0.5) * 360.0 / w as f64;
            let land = g.land_mask[i] as f32 / 255.0;
            let mut px = OCEAN;
            over(&mut px, LAND, land);
            // The Sun's height here, degrees.
            let elev =
                (sl * sd + cl * cd * cos_dl[col]).clamp(-1.0, 1.0).asin().to_degrees() as f32;
            let dark = if r.night { smooth(1.0, -12.0, elev) } else { 0.0 };
            if r.night {
                // Day lit up by the Sun's height; night darkened through
                // twilight to its full depth past −12°.
                over(&mut px, DAY, 0.27 * smooth(0.0, 40.0, elev));
                over(&mut px, [0.0; 3], 0.70 * dark);
            }
            if r.grey {
                // The grey line: sunset to the end of civil twilight.
                let band = smooth(-8.0, -5.0, elev) * (1.0 - smooth(1.5, 4.0, elev));
                over(&mut px, GREY, 0.28 * band);
            }
            if let Some(c) = &r.clouds {
                let cx = ((col as f64 + 0.5) / w as f64 * c.w as f64) as usize;
                let cy = ((row as f64 + 0.5) / h as f64 * c.h as f64) as usize;
                let b = c.px[cy.min(c.h - 1) * c.w + cx.min(c.w - 1)] as f32;
                // In the infrared, brighter is colder, higher cloud.
                over(&mut px, CLOUD, 0.45 * smooth(90.0, 220.0, b) * (1.0 - 0.5 * dark));
            }
            if let Some(a) = &r.aurora {
                let p = a.at(LatLon { lat, lon });
                over(&mut px, AURORA, 0.6 * smooth(4.0, 60.0, p));
            }
            if let (Some(p), Some(pk)) = (&r.prop, peak)
                && pk > 0.0
            {
                match p.one {
                    Some(b) => {
                        if let Some(f) = p.fields.iter().find(|f| f.band == b) {
                            let t = sample(&f.cells, lat, lon) / pk;
                            over(&mut px, ramp(t), 0.66 * smooth(0.06, 0.3, t));
                        }
                    }
                    None => {
                        let best = p
                            .fields
                            .iter()
                            .map(|f| (f.band, sample(&f.cells, lat, lon)))
                            .max_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((b, v)) = best {
                            let t = v / pk;
                            over(
                                &mut px,
                                band_colour(b).map(|x| x as f32),
                                0.7 * smooth(0.06, 0.35, t),
                            );
                        }
                    }
                }
            }
            if r.lights && dark > 0.0 && g.lights[i] > 0 {
                over(&mut px, LIGHT, dark * g.lights[i] as f32 / 255.0 * 0.9);
            }
            let o = i * 4;
            out[o] = px[0] as u8;
            out[o + 1] = px[1] as u8;
            out[o + 2] = px[2] as u8;
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], lat: f64, lon: f64) -> [u8; 3] {
        let r = ((90.0 - lat) / 180.0 * TEX_H as f64) as usize;
        let c = ((lon + 180.0) / 360.0 * TEX_W as f64) as usize;
        let i = (r * TEX_W + c) * 4;
        [img[i], img[i + 1], img[i + 2]]
    }

    fn lum(p: [u8; 3]) -> u32 {
        p.iter().map(|&x| x as u32).sum()
    }

    #[test]
    fn day_is_brighter_than_night_and_the_lights_come_on_in_the_dark() {
        // The Sun over the mid-Atlantic: New York by day, Tokyo by night.
        let r = Recipe {
            sun: LatLon { lat: -11.0, lon: -40.0 },
            night: true,
            grey: true,
            lights: true,
            clouds: None,
            aurora: None,
            prop: None,
        };
        let img = paint(&r);
        // Open sea by day and by night.
        assert!(lum(px(&img, 30.0, -45.0)) > lum(px(&img, 30.0, 160.0)) + 30);
        // Tokyo is lit at night; the sea beside it is not.
        assert!(lum(px(&img, 35.7, 139.7)) > lum(px(&img, 33.0, 145.0)) + 60);
        // Without the night layer New York and the sea are flat colours.
        let flat = paint(&Recipe { night: false, grey: false, lights: false, ..r });
        assert_eq!(px(&flat, 30.0, 160.0), OCEAN.map(|x| x as u8));
    }

    #[test]
    fn the_ramp_runs_blue_to_white_with_no_yellow_or_red() {
        assert_eq!(ramp(0.0).map(|x| x as u8), RAMP[0]);
        assert_eq!(ramp(1.0).map(|x| x as u8), RAMP[5]);
        for i in 0..=20 {
            let c = ramp(i as f32 / 20.0);
            // Blue always at least as strong as red: never yellow, never red.
            assert!(c[2] >= c[0], "{c:?}");
        }
    }
}
