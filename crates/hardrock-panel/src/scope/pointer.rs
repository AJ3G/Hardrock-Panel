//! What the mouse does on the scope, as SDR Console does it.
//!
//! * Press on the receiver marker and drag: tune, snapped to the step.
//! * Press on a filter edge, on the spectrum or the waterfall, and drag: set
//!   that edge (decision 39: the edges are "poles" in the waterfall too).
//! * Press inside the passband and drag: tune, the passband carried along
//!   with the pointer. Just click: tune there.
//! * Press anywhere else and drag: pan a zoomed view. Just click: tune there.
//! * Wheel: tune by one step. Ctrl+wheel: zoom about the pointer.
//! * Double-click: the whole span again.
//!
//! While the radio transmits every press is a pan and nothing tunes
//! (safety interlock 9). The decisions are here, apart from egui, so they can
//! be tested.

use super::view::View;

/// How near (points) the pointer must be to grab a filter edge.
pub const EDGE_GRAB: f32 = 6.0;
/// How near to grab the receiver marker.
pub const RX_GRAB: f32 = 7.0;
/// How far a press may wander and still be a click.
pub const CLICK_SLOP: f32 = 3.0;

/// Something under the pointer that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    Rx,
    EdgeLo,
    EdgeHi,
    /// Inside the passband, between its edges.
    Passband,
}

/// What is under `x`: a filter edge, else the receiver marker, else the
/// passband between the edges, else nothing. The edges come first because on
/// a narrow filter they sit right beside the marker. The spectrum and the
/// waterfall both have all three.
pub fn grab_at(x: f32, rx_x: f32, lo_x: f32, hi_x: f32) -> Option<Grab> {
    let (dl, dh) = ((x - lo_x).abs(), (x - hi_x).abs());
    if dl < EDGE_GRAB || dh < EDGE_GRAB {
        return Some(if dl <= dh { Grab::EdgeLo } else { Grab::EdgeHi });
    }
    if (x - rx_x).abs() < RX_GRAB {
        return Some(Grab::Rx);
    }
    (x > lo_x.min(hi_x) && x < lo_x.max(hi_x)).then_some(Grab::Passband)
}

/// What a press turned into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    Rx,
    Edge(Grab),
    /// The passband, carried along: tunes by how far the pointer has moved.
    Centre,
    Pan,
}

/// A press in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    pub kind: DragKind,
    /// Where it started, points.
    pub x0: f32,
    /// The view when it started, which a pan moves from.
    pub start: View,
    /// The receiver when it started, which a passband drag moves from (the
    /// radio's own Hz).
    pub rx0: f64,
    /// It has gone past [`CLICK_SLOP`], so it is a drag, not a click.
    pub moved: bool,
}

impl Drag {
    /// Begin a press on `grab`. While transmitting, nothing that tunes can be
    /// grabbed: the press can only pan.
    pub fn press(grab: Option<Grab>, x0: f32, start: View, rx0: f64, transmitting: bool) -> Drag {
        // SAFETY INTERLOCK 9: no tuning or filter change from the scope while
        // the radio transmits, so a stray drag never moves a keyed signal.
        let kind = match grab {
            _ if transmitting => DragKind::Pan,
            Some(Grab::Rx) => DragKind::Rx,
            Some(Grab::Passband) => DragKind::Centre,
            Some(g) => DragKind::Edge(g),
            None => DragKind::Pan,
        };
        Drag { kind, x0, start, rx0, moved: false }
    }

    /// The pointer is at `x`: note whether this is now a drag.
    pub fn moved_to(&mut self, x: f32) {
        if (x - self.x0).abs() > CLICK_SLOP {
            self.moved = true;
        }
    }

    /// Released: was it a click that tunes?
    pub fn is_click_to_tune(&self, transmitting: bool) -> bool {
        // SAFETY INTERLOCK 9, as above.
        !self.moved && matches!(self.kind, DragKind::Pan | DragKind::Centre) && !transmitting
    }

    /// The receiver for a passband drag with the pointer at `x` on a plot
    /// `width_px` wide: where it started, moved by as many Hz as the pointer
    /// has, in the view the drag started in.
    pub fn centre_to(&self, x: f32, width_px: f32) -> f64 {
        self.rx0 + ((x - self.x0) / width_px.max(1.0)) as f64 * self.start.width()
    }
}

/// The widest filter the scope will set, either side of the dial.
pub const MAX_EDGE_HZ: f32 = 12_000.0;
/// The narrowest.
pub const MIN_WIDTH_HZ: f32 = 50.0;

/// The passband after dragging `edge` to `offset` Hz from the dial.
///
/// AM and FM are symmetric about the dial, so either edge sets both. The
/// sideband modes move only the edge being dragged, as SDR Console does, and
/// keep it at least [`MIN_WIDTH_HZ`] from the other. Edges land on 10 Hz.
pub fn drag_edge(mode: sdr::Mode, edge: Grab, offset: f64, pb: (f32, f32)) -> (f32, f32) {
    let off = ((offset as f32) / 10.0).round() * 10.0;
    match mode {
        sdr::Mode::Am | sdr::Mode::Fm => {
            let w = off.abs().clamp(MIN_WIDTH_HZ / 2.0, MAX_EDGE_HZ);
            (-w, w)
        }
        _ => match edge {
            Grab::EdgeLo => (off.clamp(-MAX_EDGE_HZ, pb.1 - MIN_WIDTH_HZ), pb.1),
            _ => (pb.0, off.clamp(pb.0 + MIN_WIDTH_HZ, MAX_EDGE_HZ)),
        },
    }
}

/// Mouse-wheel notches, from wheel events that come as lines (a mouse) or as
/// points (a touchpad, a smooth wheel).
#[derive(Debug, Clone, Copy, Default)]
pub struct Wheel {
    acc: f32,
}

/// Points of smooth scrolling that make one notch.
const POINTS_PER_NOTCH: f32 = 40.0;

impl Wheel {
    /// Whole notches in this event, up positive. Lines are notches; points
    /// are gathered until they make one.
    pub fn notches(&mut self, lines: bool, dy: f32) -> i32 {
        if lines {
            self.acc = 0.0;
            return dy.round() as i32;
        }
        self.acc += dy / POINTS_PER_NOTCH;
        let n = self.acc.trunc();
        self.acc -= n;
        n as i32
    }
}

/// The zoom factor for `notches` of ctrl+wheel: 1.25 a notch, as the mockup.
pub fn wheel_zoom(notches: i32) -> f64 {
    1.25f64.powi(notches)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: View = View { lo: 14e6, hi: 14.192e6 };

    #[test]
    fn edges_win_over_the_marker_then_the_passband() {
        // Marker at 100, filter 104..130, on the spectrum or the waterfall.
        assert_eq!(grab_at(103.0, 100.0, 104.0, 130.0), Some(Grab::EdgeLo));
        assert_eq!(grab_at(128.0, 100.0, 104.0, 130.0), Some(Grab::EdgeHi));
        assert_eq!(grab_at(135.5, 100.0, 104.0, 130.0), Some(Grab::EdgeHi));
        assert_eq!(grab_at(97.0, 100.0, 104.0, 130.0), Some(Grab::Rx));
        assert_eq!(grab_at(118.0, 100.0, 104.0, 130.0), Some(Grab::Passband));
        assert_eq!(grab_at(300.0, 100.0, 104.0, 130.0), None);
        // LSB: the passband is left of the marker.
        assert_eq!(grab_at(80.0, 100.0, 70.0, 96.0), Some(Grab::Passband));
        assert_eq!(grab_at(70.5, 100.0, 70.0, 96.0), Some(Grab::EdgeLo));
    }

    #[test]
    fn dragging_the_passband_tunes_by_the_pointers_travel() {
        // 192 kHz across 960 points: 200 Hz a point.
        let d = Drag::press(Some(Grab::Passband), 500.0, V, 14_074_000.0, false);
        assert_eq!(d.kind, DragKind::Centre);
        assert!((d.centre_to(510.0, 960.0) - 14_076_000.0).abs() < 0.5);
        assert!((d.centre_to(495.0, 960.0) - 14_073_000.0).abs() < 0.5);
        // Not moved, it is a click that tunes, as on empty spectrum.
        assert!(d.is_click_to_tune(false));
        assert!(!d.is_click_to_tune(true));
        // While transmitting it can only pan.
        assert_eq!(Drag::press(Some(Grab::Passband), 0.0, V, 0.0, true).kind, DragKind::Pan);
    }

    #[test]
    fn a_press_drags_what_it_grabbed_unless_transmitting() {
        assert_eq!(Drag::press(Some(Grab::Rx), 0.0, V, 0.0, false).kind, DragKind::Rx);
        assert_eq!(
            Drag::press(Some(Grab::EdgeHi), 0.0, V, 0.0, false).kind,
            DragKind::Edge(Grab::EdgeHi)
        );
        assert_eq!(Drag::press(None, 0.0, V, 0.0, false).kind, DragKind::Pan);
        for g in [Some(Grab::Rx), Some(Grab::EdgeLo), None] {
            assert_eq!(Drag::press(g, 0.0, V, 0.0, true).kind, DragKind::Pan);
        }
    }

    #[test]
    fn a_click_tunes_and_a_drag_does_not() {
        let mut d = Drag::press(None, 50.0, V, 0.0, false);
        d.moved_to(52.5);
        assert!(d.is_click_to_tune(false));
        assert!(!d.is_click_to_tune(true), "never while transmitting");
        d.moved_to(54.0);
        assert!(!d.is_click_to_tune(false));
        // Once moved, coming back does not make it a click again.
        d.moved_to(50.0);
        assert!(d.moved);
        // A press on the marker is never a click-to-tune; it tunes by dragging.
        let r = Drag::press(Some(Grab::Rx), 50.0, V, 0.0, false);
        assert!(!r.is_click_to_tune(false));
    }

    #[test]
    fn sideband_edges_move_alone_and_keep_a_minimum_width() {
        let pb = (100.0, 2700.0);
        assert_eq!(drag_edge(sdr::Mode::Usb, Grab::EdgeHi, 3004.0, pb), (100.0, 3000.0));
        assert_eq!(drag_edge(sdr::Mode::Usb, Grab::EdgeLo, 296.0, pb), (300.0, 2700.0));
        assert_eq!(drag_edge(sdr::Mode::Usb, Grab::EdgeLo, 2690.0, pb), (2650.0, 2700.0));
        assert_eq!(drag_edge(sdr::Mode::Usb, Grab::EdgeHi, 99_000.0, pb), (100.0, 12_000.0));
        let lsb = (-2700.0, -100.0);
        assert_eq!(drag_edge(sdr::Mode::Lsb, Grab::EdgeLo, -2000.0, lsb), (-2000.0, -100.0));
        let cw = (350.0, 850.0);
        assert_eq!(drag_edge(sdr::Mode::Cw, Grab::EdgeHi, 700.0, cw), (350.0, 700.0));
    }

    #[test]
    fn am_and_fm_stay_symmetric() {
        let pb = (-4500.0, 4500.0);
        assert_eq!(drag_edge(sdr::Mode::Am, Grab::EdgeHi, 3000.0, pb), (-3000.0, 3000.0));
        assert_eq!(drag_edge(sdr::Mode::Am, Grab::EdgeLo, -2000.0, pb), (-2000.0, 2000.0));
        assert_eq!(drag_edge(sdr::Mode::Fm, Grab::EdgeLo, 0.0, pb), (-25.0, 25.0));
    }

    #[test]
    fn wheel_lines_are_notches_and_points_add_up() {
        let mut w = Wheel::default();
        assert_eq!(w.notches(true, 1.0), 1);
        assert_eq!(w.notches(true, -2.0), -2);
        assert_eq!(w.notches(false, 15.0), 0);
        assert_eq!(w.notches(false, 15.0), 0);
        assert_eq!(w.notches(false, 15.0), 1);
        assert_eq!(w.notches(false, -100.0), -2);
        assert!((wheel_zoom(1) - 1.25).abs() < 1e-12);
        assert!((wheel_zoom(-1) - 0.8).abs() < 1e-12);
    }
}
