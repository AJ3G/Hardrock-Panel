//! The map: Natural Earth's 1:50m land, country borders and state and
//! province lines, its built-up urban areas (the city lights) and its
//! populated places (the city names), all public domain
//! (naturalearthdata.com), simplified and kept in `assets/globe/`.
//!
//! Read once, the first time GLOBE opens. The land is also drawn into a
//! mask, and the urban areas into a lights mask, at the map picture's size.

use std::sync::OnceLock;

use super::sphere::{V3, vec_of};

/// The map picture's size: about 20 km a pixel at the equator.
pub(crate) const TEX_W: usize = 2048;
pub(crate) const TEX_H: usize = 1024;

pub(crate) struct Place {
    pub name: String,
    pub v: V3,
    /// 3 over 3 million people, 2 over a million, 1 over 300 000, 0 the rest.
    pub tier: u8,
}

pub(crate) struct Geo {
    /// The coastlines, borders and state lines as points on the sphere,
    /// ready to project.
    pub coast: Vec<Vec<V3>>,
    pub borders: Vec<Vec<V3>>,
    pub states: Vec<Vec<V3>>,
    pub places: Vec<Place>,
    /// 0 sea, 255 land, row 0 at 90° N, column 0 at 180° W.
    pub land_mask: Vec<u8>,
    /// How bright a city's lights are there, 0 to 255.
    pub lights: Vec<u8>,
    /// The Earth by day in natural colour, RGB, at the picture's size: NASA's
    /// Blue Marble Next Generation (public domain), its land's relief drawn
    /// out with Natural Earth's shaded relief.
    pub earth: Vec<u8>,
}

fn flat_pairs(v: &[f64]) -> Vec<(f64, f64)> {
    v.chunks_exact(2).map(|p| (p[0], p[1])).collect()
}

/// A line's points, with any step longer than 2° filled in so it follows
/// the sphere when drawn.
fn densify(pts: &[(f64, f64)], closed: bool) -> Vec<V3> {
    let mut out = Vec::with_capacity(pts.len() + 8);
    let n = pts.len();
    let last = if closed { n } else { n.saturating_sub(1) };
    for i in 0..last {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) / 2.0).ceil().max(1.0) as usize;
        for s in 0..steps {
            let t = s as f64 / steps as f64;
            out.push(vec_of(a.1 + (b.1 - a.1) * t, a.0 + (b.0 - a.0) * t));
        }
    }
    if let Some(&p) = if closed { pts.first() } else { pts.last() } {
        out.push(vec_of(p.1, p.0));
    }
    out
}

/// Fill `rings` (lon, lat) into a `w` × `h` mask by the even-odd rule, a
/// scanline at each row's middle: land is inside an odd number of rings
/// (lakes in the 1:50m land are holes, given as rings of their own).
pub(crate) fn rasterize(rings: &[Vec<(f64, f64)>], w: usize, h: usize) -> Vec<u8> {
    let mut mask = vec![0u8; w * h];
    let mut xs: Vec<f64> = Vec::new();
    for row in 0..h {
        let lat = 90.0 - (row as f64 + 0.5) * 180.0 / h as f64;
        xs.clear();
        for r in rings {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                if (a.1 <= lat) != (b.1 <= lat) {
                    let t = (lat - a.1) / (b.1 - a.1);
                    xs.push(a.0 + t * (b.0 - a.0));
                }
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks_exact(2) {
            let c0 = (((pair[0] + 180.0) / 360.0 * w as f64) - 0.5).ceil().max(0.0) as usize;
            let c1 = ((((pair[1] + 180.0) / 360.0 * w as f64) - 0.5).floor() as isize)
                .min(w as isize - 1);
            if c1 < c0 as isize {
                continue;
            }
            for c in c0..=c1 as usize {
                mask[row * w + c] = 255;
            }
        }
    }
    mask
}

/// The urban areas' glow: each a soft spot as wide as the area, brighter
/// for a bigger one.
fn lights_mask(urban: &[f64], w: usize, h: usize) -> Vec<u8> {
    let mut acc = vec![0f32; w * h];
    let km_per_px = 40_075.0 / w as f64;
    for u in urban.chunks_exact(3) {
        let (lon, lat, r_km) = (u[0], u[1], u[2]);
        let cx = (lon + 180.0) / 360.0 * w as f64;
        let cy = (90.0 - lat) / 180.0 * h as f64;
        let sy = (r_km / km_per_px).max(0.7);
        let sx = sy / lat.to_radians().cos().max(0.2);
        let peak = (0.45 + r_km / 40.0).min(1.0) as f32;
        let (rx, ry) = ((sx * 2.5).ceil() as isize, (sy * 2.5).ceil() as isize);
        for dy in -ry..=ry {
            let y = cy.floor() as isize + dy;
            if y < 0 || y >= h as isize {
                continue;
            }
            for dx in -rx..=rx {
                let x = (cx.floor() as isize + dx).rem_euclid(w as isize);
                let (ex, ey) = ((x as f64 + 0.5 - cx) / sx, (y as f64 + 0.5 - cy) / sy);
                let ex = if ex.abs() > w as f64 / 2.0 / sx {
                    ex - (w as f64 / sx) * ex.signum()
                } else {
                    ex
                };
                let g = (-(ex * ex + ey * ey) / 2.0).exp() as f32;
                let i = y as usize * w + x as usize;
                acc[i] = (acc[i] + peak * g).min(1.0);
            }
        }
    }
    acc.iter().map(|a| (a * 255.0) as u8).collect()
}

/// The bundled natural-colour picture, RGB, `TEX_W` × `TEX_H`.
fn decode_earth(jpg: &[u8]) -> Vec<u8> {
    let mut d = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(jpg));
    let px = d.decode().expect("the bundled picture is a valid JPEG");
    assert_eq!(px.len(), TEX_W * TEX_H * 3, "the bundled picture is the map's size, RGB");
    px
}

fn parse_lists(s: &str) -> Vec<Vec<f64>> {
    serde_json::from_str(s).expect("the bundled map data is valid")
}

fn load() -> Geo {
    let land: Vec<Vec<(f64, f64)>> = parse_lists(include_str!("../../assets/globe/land.json"))
        .iter()
        .map(|r| flat_pairs(r))
        .collect();
    let coast = land.iter().map(|r| densify(r, true)).collect();
    let lines = |s: &str| -> Vec<Vec<V3>> {
        parse_lists(s).iter().map(|r| densify(&flat_pairs(r), false)).collect()
    };
    let borders = lines(include_str!("../../assets/globe/borders.json"));
    let states = lines(include_str!("../../assets/globe/states.json"));
    let urban: Vec<f64> = serde_json::from_str(include_str!("../../assets/globe/urban.json"))
        .expect("the bundled map data is valid");
    let raw: Vec<(String, f64, f64, u8)> =
        serde_json::from_str(include_str!("../../assets/globe/places.json"))
            .expect("the bundled map data is valid");
    let places = raw
        .into_iter()
        .map(|(name, lon, lat, tier)| Place { name, v: vec_of(lat, lon), tier })
        .collect();
    let land_mask = rasterize(&land, TEX_W, TEX_H);
    let lights = lights_mask(&urban, TEX_W, TEX_H);
    let earth = decode_earth(include_bytes!("../../assets/globe/earth.jpg"));
    Geo { earth, coast, borders, states, places, land_mask, lights }
}

/// The map, read the first time it is asked for.
pub(crate) fn geo() -> &'static Geo {
    static GEO: OnceLock<Geo> = OnceLock::new();
    GEO.get_or_init(load)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_ring_fills_its_inside_and_a_hole_stays_open() {
        let outer = vec![(-90.0, -45.0), (90.0, -45.0), (90.0, 45.0), (-90.0, 45.0)];
        let hole = vec![(-10.0, -10.0), (10.0, -10.0), (10.0, 10.0), (-10.0, 10.0)];
        let m = rasterize(&[outer, hole], 36, 18);
        let at = |lon: f64, lat: f64| {
            m[((90.0 - lat) / 10.0) as usize * 36 + ((lon + 180.0) / 10.0) as usize]
        };
        assert_eq!(at(-50.0, 20.0), 255);
        assert_eq!(at(150.0, 20.0), 0);
        assert_eq!(at(0.0, 0.0), 0);
        assert_eq!(at(0.0, 70.0), 0);
    }

    #[test]
    fn the_bundled_map_has_land_where_land_is() {
        let g = geo();
        let at = |lat: f64, lon: f64| {
            let r = ((90.0 - lat) / 180.0 * TEX_H as f64) as usize;
            let c = ((lon + 180.0) / 360.0 * TEX_W as f64) as usize;
            g.land_mask[r * TEX_W + c]
        };
        // Philadelphia, Kansas, the Sahara, Siberia, Sydney: land.
        for (lat, lon) in
            [(40.0, -75.2), (38.5, -98.0), (23.0, 10.0), (62.0, 100.0), (-33.8, 150.9)]
        {
            assert_eq!(at(lat, lon), 255, "{lat} {lon}");
        }
        // The Atlantic, the Pacific, the Indian Ocean, the Caspian: water.
        for (lat, lon) in [(35.0, -40.0), (0.0, -150.0), (-20.0, 80.0), (42.0, 50.5)] {
            assert_eq!(at(lat, lon), 0, "{lat} {lon}");
        }
        assert!(g.borders.len() > 100 && g.states.len() > 100 && g.places.len() > 1000);
        // City lights over New York, none over the open sea.
        let lr = |lat: f64, lon: f64| {
            let r = ((90.0 - lat) / 180.0 * TEX_H as f64) as usize;
            let c = ((lon + 180.0) / 360.0 * TEX_W as f64) as usize;
            g.lights[r * TEX_W + c]
        };
        assert!(lr(40.75, -73.95) > 100);
        assert_eq!(lr(35.0, -40.0), 0);
    }
}
