//! Points on the Earth as unit vectors, great circles between them, and the
//! two ways GLOBE draws the Earth: an orthographic globe seen from above a
//! chosen point, and a flat (equirectangular) map centred on a longitude.

use crate::station::LatLon;

/// A point on the unit sphere: x towards 0° N 0° E, y towards 0° N 90° E,
/// z towards the North Pole.
pub(crate) type V3 = [f64; 3];

pub(crate) fn vec_of(lat: f64, lon: f64) -> V3 {
    let (p, l) = (lat.to_radians(), lon.to_radians());
    [p.cos() * l.cos(), p.cos() * l.sin(), p.sin()]
}

pub(crate) fn vec_ll(p: LatLon) -> V3 {
    vec_of(p.lat, p.lon)
}

/// Latitude and longitude of `v`, degrees.
pub(crate) fn ll_of(v: V3) -> LatLon {
    LatLon { lat: v[2].clamp(-1.0, 1.0).asin().to_degrees(), lon: v[1].atan2(v[0]).to_degrees() }
}

pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn unit(a: V3) -> V3 {
    let n = dot(a, a).sqrt();
    if n == 0.0 { a } else { [a[0] / n, a[1] / n, a[2] / n] }
}

/// The angle between two unit vectors, radians.
pub(crate) fn angle(a: V3, b: V3) -> f64 {
    dot(a, b).clamp(-1.0, 1.0).acos()
}

/// The point a fraction `t` of the way from `a` to `b` along their great
/// circle.
pub(crate) fn slerp(a: V3, b: V3, t: f64) -> V3 {
    let w = angle(a, b);
    let s = w.sin();
    if s < 1e-9 {
        return a;
    }
    let (k1, k2) = (((1.0 - t) * w).sin() / s, (t * w).sin() / s);
    [a[0] * k1 + b[0] * k2, a[1] * k1 + b[1] * k2, a[2] * k1 + b[2] * k2]
}

/// The great circle from `a` to `b` in `n` steps (`n + 1` points).
pub(crate) fn great_circle(a: V3, b: V3, n: usize) -> Vec<V3> {
    (0..=n).map(|i| slerp(a, b, i as f64 / n as f64)).collect()
}

/// The circle of points `deg` above the horizon as seen from `pole`'s
/// direction (the sun's subsolar point, say): 0° is the terminator, −6° the
/// edge of civil twilight. `n` steps, closed.
pub(crate) fn small_circle(pole: V3, deg: f64, n: usize) -> Vec<V3> {
    let pole = unit(pole);
    let side = if pole[2].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
    let u = unit(cross(pole, side));
    let w = cross(pole, u);
    let (s, c) = (deg.to_radians().sin(), deg.to_radians().cos());
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64 * std::f64::consts::TAU;
            let (st, ct) = t.sin_cos();
            [
                s * pole[0] + c * (ct * u[0] + st * w[0]),
                s * pole[1] + c * (ct * u[1] + st * w[1]),
                s * pole[2] + c * (ct * u[2] + st * w[2]),
            ]
        })
        .collect()
}

/// How the Earth is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum Proj {
    #[default]
    Globe,
    Flat,
}

/// Where the camera is: the point under the middle of the view, the zoom,
/// the projection and the size of the picture.
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    pub proj: Proj,
    pub centre: LatLon,
    /// 1 shows the whole Earth.
    pub zoom: f64,
    /// The middle of the picture and its size, points.
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
}

/// A point as drawn: where on the screen, and whether it can be seen (on the
/// near side of the globe; always on the flat map).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Screen {
    pub x: f64,
    pub y: f64,
    /// On the globe, how far towards the viewer (1 in the middle, 0 at the
    /// edge, below 0 behind); 1 on the flat map.
    pub z: f64,
}

impl Screen {
    pub(crate) fn seen(&self) -> bool {
        self.z > 0.0
    }
}

/// The view's projection, worked out once a frame.
pub(crate) struct Projector {
    pub view: View,
    /// The globe's radius, points; on the flat map the map's width.
    pub r: f64,
    e: V3,
    n: V3,
    o: V3,
}

impl Projector {
    pub(crate) fn new(view: View) -> Self {
        let r = match view.proj {
            Proj::Globe => 0.47 * view.w.min(view.h) * view.zoom,
            Proj::Flat => view.w.min(view.h * 2.0) * 0.98 * view.zoom,
        };
        let (p, l) = (view.centre.lat.to_radians(), view.centre.lon.to_radians());
        let e = [-l.sin(), l.cos(), 0.0];
        let n = [-p.sin() * l.cos(), -p.sin() * l.sin(), p.cos()];
        let o = [p.cos() * l.cos(), p.cos() * l.sin(), p.sin()];
        Projector { view, r, e, n, o }
    }

    /// The flat map's longitude across the middle of the picture, and the
    /// latitude there: the latitude stays put until the map is taller than
    /// the picture.
    fn flat_centre(&self) -> (f64, f64) {
        let half_deg = self.view.h / 2.0 * 360.0 / self.r;
        let span = (90.0 - half_deg).max(0.0);
        (self.view.centre.lon, self.view.centre.lat.clamp(-span, span))
    }

    /// Longitude `lon` east of the flat map's middle, −180 to 180.
    pub(crate) fn rel_lon(&self, lon: f64) -> f64 {
        (lon - self.flat_centre().0 + 540.0).rem_euclid(360.0) - 180.0
    }

    /// The flat map's x of a longitude already taken relative to its middle,
    /// and the y of a latitude.
    pub(crate) fn flat_x(&self, rel: f64) -> f64 {
        self.view.cx + rel / 360.0 * self.r
    }

    pub(crate) fn flat_y(&self, lat: f64) -> f64 {
        self.view.cy - (lat - self.flat_centre().1) / 360.0 * self.r
    }

    pub(crate) fn project(&self, v: V3) -> Screen {
        match self.view.proj {
            Proj::Globe => Screen {
                x: self.view.cx + dot(v, self.e) * self.r,
                y: self.view.cy - dot(v, self.n) * self.r,
                z: dot(v, self.o),
            },
            Proj::Flat => {
                let p = ll_of(v);
                Screen { x: self.flat_x(self.rel_lon(p.lon)), y: self.flat_y(p.lat), z: 1.0 }
            }
        }
    }

    /// A point lifted `k` times the Earth's radius from its centre (an arc
    /// drawn above the surface): on the globe it can be seen past the limb.
    pub(crate) fn project_lifted(&self, v: V3, k: f64) -> Screen {
        match self.view.proj {
            Proj::Flat => self.project(v),
            Proj::Globe => {
                let (x, y, z) = (dot(v, self.e) * k, dot(v, self.n) * k, dot(v, self.o));
                // Hidden only when it is behind the globe and inside its disk.
                let seen = z > 0.0 || x * x + y * y > 1.0;
                Screen {
                    x: self.view.cx + x * self.r,
                    y: self.view.cy - y * self.r,
                    z: if seen { z.max(1e-6) } else { -1.0 },
                }
            }
        }
    }

    /// A point behind the globe pushed out to the limb, so a filled shape
    /// that runs behind the globe stays closed.
    pub(crate) fn project_clamped(&self, v: V3) -> (f64, f64) {
        let s = self.project(v);
        if s.z >= 0.0 || self.view.proj == Proj::Flat {
            return (s.x, s.y);
        }
        let (dx, dy) = (s.x - self.view.cx, s.y - self.view.cy);
        let n = (dx * dx + dy * dy).sqrt().max(1e-9);
        (self.view.cx + dx / n * self.r, self.view.cy + dy / n * self.r)
    }

    /// The pieces of a line that can be seen, each a run of screen points.
    /// On the flat map a line is cut where it crosses the date line.
    pub(crate) fn runs(
        &self,
        pts: &[V3],
        lift: Option<&dyn Fn(usize) -> f64>,
    ) -> Vec<Vec<(f64, f64)>> {
        let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut cur: Vec<(f64, f64)> = Vec::new();
        let flat = self.view.proj == Proj::Flat;
        for (i, &v) in pts.iter().enumerate() {
            let s = match lift {
                Some(f) => self.project_lifted(v, f(i)),
                None => self.project(v),
            };
            if !s.seen() {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            }
            if flat && cur.last().is_some_and(|p| (p.0 - s.x).abs() > self.r / 2.0) {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
            cur.push((s.x, s.y));
        }
        if cur.len() > 1 {
            out.push(cur);
        }
        out
    }

    /// The view turned by a drag of `dx`, `dy` points.
    pub(crate) fn dragged(&self, dx: f64, dy: f64) -> LatLon {
        let deg_per_pt = match self.view.proj {
            Proj::Globe => (1.0 / self.r).to_degrees(),
            Proj::Flat => 360.0 / self.r,
        };
        let c = self.view.centre;
        LatLon {
            lat: (c.lat + dy * deg_per_pt).clamp(-85.0, 85.0),
            lon: (c.lon - dx * deg_per_pt + 540.0).rem_euclid(360.0) - 180.0,
        }
    }

    /// The point on the Earth under a screen point, if it is on the Earth.
    #[cfg(test)]
    pub(crate) fn unproject(&self, x: f64, y: f64) -> Option<LatLon> {
        match self.view.proj {
            Proj::Globe => {
                let (a, b) = ((x - self.view.cx) / self.r, (self.view.cy - y) / self.r);
                let rr = a * a + b * b;
                if rr > 1.0 {
                    return None;
                }
                let c = (1.0 - rr).sqrt();
                let v = [
                    a * self.e[0] + b * self.n[0] + c * self.o[0],
                    a * self.e[1] + b * self.n[1] + c * self.o[1],
                    a * self.e[2] + b * self.n[2] + c * self.o[2],
                ];
                Some(ll_of(v))
            }
            Proj::Flat => {
                let (lon0, lat0) = self.flat_centre();
                let lat = lat0 + (self.view.cy - y) / self.r * 360.0;
                let lon = lon0 + (x - self.view.cx) / self.r * 360.0;
                (lat.abs() <= 90.0)
                    .then(|| LatLon { lat, lon: (lon + 540.0).rem_euclid(360.0) - 180.0 })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn view(proj: Proj, lat: f64, lon: f64) -> Projector {
        Projector::new(View {
            proj,
            centre: LatLon { lat, lon },
            zoom: 1.0,
            cx: 400.0,
            cy: 300.0,
            w: 800.0,
            h: 600.0,
        })
    }

    #[test]
    fn points_go_to_vectors_and_back() {
        for (lat, lon) in [(40.5, -75.0), (-33.9, 151.2), (0.0, 180.0), (89.0, 10.0)] {
            let p = ll_of(vec_of(lat, lon));
            assert!(
                close(p.lat, lat, 1e-9)
                    && close(((p.lon - lon + 540.0) % 360.0) - 180.0, 0.0, 1e-9),
                "{lat} {lon} → {p:?}"
            );
        }
    }

    #[test]
    fn halfway_along_a_great_circle_is_the_midpoint() {
        // Along the equator from 0° to 90° E the middle is 45° E.
        let m = ll_of(slerp(vec_of(0.0, 0.0), vec_of(0.0, 90.0), 0.5));
        assert!(close(m.lat, 0.0, 1e-9) && close(m.lon, 45.0, 1e-9));
        // The path stays a quarter circle long, whatever the steps.
        let pts = great_circle(vec_of(10.0, 20.0), vec_of(-30.0, 100.0), 64);
        let total: f64 = pts.windows(2).map(|w| angle(w[0], w[1])).sum();
        assert!(close(total, angle(vec_of(10.0, 20.0), vec_of(-30.0, 100.0)), 1e-9));
    }

    #[test]
    fn the_terminator_is_ninety_degrees_from_the_sun() {
        let sun = vec_of(-11.0, -52.0);
        for v in small_circle(sun, 0.0, 90) {
            assert!(close(angle(v, sun).to_degrees(), 90.0, 1e-9));
        }
        for v in small_circle(sun, -6.0, 90) {
            assert!(close(angle(v, sun).to_degrees(), 96.0, 1e-9));
        }
    }

    #[test]
    fn the_globe_shows_the_centre_in_the_middle_and_hides_the_far_side() {
        let p = view(Proj::Globe, 40.5, -75.0);
        let s = p.project(vec_of(40.5, -75.0));
        assert!(close(s.x, 400.0, 1e-9) && close(s.y, 300.0, 1e-9) && close(s.z, 1.0, 1e-12));
        // North is up, east is right.
        assert!(p.project(vec_of(50.0, -75.0)).y < 300.0);
        assert!(p.project(vec_of(40.5, -65.0)).x > 400.0);
        // The antipode is behind.
        assert!(!p.project(vec_of(-40.5, 105.0)).seen());
        // A point on the screen goes back to where it came from.
        let back = p.unproject(s.x + 50.0, s.y - 20.0).unwrap();
        let again = p.project(vec_ll(back));
        assert!(close(again.x, 450.0, 1e-6) && close(again.y, 280.0, 1e-6));
    }

    #[test]
    fn an_arc_lifted_off_the_globe_is_seen_past_the_limb() {
        let p = view(Proj::Globe, 0.0, 0.0);
        // Just behind the limb, lifted: seen beside the disk.
        let v = vec_of(0.0, 95.0);
        assert!(!p.project(v).seen());
        assert!(p.project_lifted(v, 1.2).seen());
        // Far behind, lifted a little: still hidden by the globe.
        assert!(!p.project_lifted(vec_of(0.0, 150.0), 1.05).seen());
    }

    #[test]
    fn the_flat_map_is_centred_on_its_longitude_and_cut_at_the_date_line() {
        let p = view(Proj::Flat, 0.0, -75.0);
        let s = p.project(vec_of(0.0, -75.0));
        assert!(close(s.x, 400.0, 1e-9) && close(s.y, 300.0, 1e-9));
        // 105° E is the map's edge, 180° from its middle.
        let east = p.project(vec_of(0.0, 104.0));
        let west = p.project(vec_of(0.0, 106.0));
        assert!(east.x > 700.0 && west.x < 100.0);
        // A line across that edge comes out as two pieces.
        let line =
            vec![vec_of(0.0, 100.0), vec_of(0.0, 104.0), vec_of(0.0, 106.0), vec_of(0.0, 110.0)];
        assert_eq!(p.runs(&line, None).len(), 2);
        // Unzoomed, the latitude stays at the equator whatever is asked.
        let tilted = view(Proj::Flat, 50.0, -75.0);
        assert!(close(tilted.project(vec_of(0.0, -75.0)).y, 300.0, 1e-9));
    }

    #[test]
    fn a_drag_turns_the_view_the_way_the_hand_moves() {
        let p = view(Proj::Globe, 0.0, 0.0);
        // Dragging right brings the west into view: the centre moves west.
        assert!(p.dragged(50.0, 0.0).lon < 0.0);
        // Dragging down brings the north into view.
        assert!(p.dragged(0.0, 50.0).lat > 0.0);
        let f = view(Proj::Flat, 0.0, 170.0);
        assert!(
            close(f.dragged(-f.r / 36.0, 0.0).lon, -180.0, 1e-9)
                || close(f.dragged(-f.r / 36.0, 0.0).lon, 180.0, 1e-9)
        );
    }
}
