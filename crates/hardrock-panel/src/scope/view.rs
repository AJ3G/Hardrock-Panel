//! The scope's frequency axis: the span the radio sends, the part of it on
//! screen, zooming and panning, and SDR Console's fixed centre.
//!
//! Everything here is in the radio's own frequency (its IF when a transverter
//! is in use); the transverter offset is added only where a frequency is
//! written on screen or looked up in the band plan.

/// The IQ window the radio is sending: its centre (the DUO's VFO A) and its
/// width (the sample rate), Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub center: f64,
    pub width: f64,
}

impl Span {
    pub fn lo(&self) -> f64 {
        self.center - self.width / 2.0
    }
    pub fn hi(&self) -> f64 {
        self.center + self.width / 2.0
    }
    pub fn contains(&self, f: f64) -> bool {
        f >= self.lo() && f <= self.hi()
    }
}

/// The part of the span on screen, Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub lo: f64,
    pub hi: f64,
}

impl View {
    pub fn width(&self) -> f64 {
        self.hi - self.lo
    }

    pub fn center(&self) -> f64 {
        (self.lo + self.hi) / 2.0
    }

    pub fn contains(&self, f: f64) -> bool {
        f >= self.lo && f <= self.hi
    }

    /// Where `f` lands across a plot `w` wide starting at `left`.
    pub fn x_of(&self, f: f64, left: f32, w: f32) -> f32 {
        left + ((f - self.lo) / self.width()) as f32 * w
    }

    /// The frequency under `x` on a plot `w` wide starting at `left`.
    pub fn f_at(&self, x: f32, left: f32, w: f32) -> f64 {
        self.lo + ((x - left) / w.max(1.0)) as f64 * self.width()
    }

    /// How far in the view is zoomed on `span`: 1 for the whole span.
    pub fn zoom(&self, span: Span) -> f64 {
        span.width / self.width()
    }

    pub fn zoomed(&self, span: Span) -> bool {
        self.zoom(span) > 1.01
    }
}

/// The narrowest the view can be zoomed to: 1/4096 of the span (a few bins at
/// the FFT sizes offered), but never under 1.5 kHz, which still shows a whole
/// SSB passband.
pub fn min_width(span: Span) -> f64 {
    (span.width / 4096.0).max(1500.0).min(span.width)
}

/// The view: what was asked for (`None` is the whole span), kept inside the
/// span and no narrower than [`min_width`].
pub fn view(span: Span, want: Option<View>) -> View {
    let Some(w) = want else {
        return View { lo: span.lo(), hi: span.hi() };
    };
    let width = w.width().clamp(min_width(span), span.width);
    let lo = w.lo.clamp(span.lo(), span.hi() - width);
    View { lo, hi: lo + width }
}

/// Zoom in by `k` (out when under 1) about `f`, which stays under the pointer.
pub fn zoom_at(span: Span, cur: View, f: f64, k: f64) -> View {
    let width = (cur.width() / k).clamp(min_width(span), span.width);
    let t = (f - cur.lo) / cur.width();
    view(span, Some(View { lo: f - t * width, hi: f - t * width + width }))
}

/// The view a pan drag gives: `start` moved by `df` Hz (positive moves the
/// band to the right, i.e. the view to lower frequencies, as dragging paper).
pub fn pan(span: Span, start: View, df: f64) -> View {
    view(span, Some(View { lo: start.lo - df, hi: start.hi - df }))
}

/// How far from the centre, as a share of the span, the receiver may go
/// before the centre follows it (SDR Console's "automatically adjust the
/// tuning range").
pub const RECENTRE_AT: f64 = 0.45;

/// SDR Console's fixed centre: the centre stays put while the receiver moves
/// inside the span, and is moved (to the receiver, on a whole kHz) only once
/// the receiver is past [`RECENTRE_AT`] of the span from it.
pub fn needs_recentre(span: Span, rx: f64) -> Option<f64> {
    if span.width <= 0.0 {
        return None;
    }
    ((rx - span.center).abs() > span.width * RECENTRE_AT).then(|| (rx / 1000.0).round() * 1000.0)
}

/// A jump of the receiver to `f` (a DECODE mode's calling frequency) can
/// leave the centre where it is: `f` is already well inside the span, by the
/// same [`RECENTRE_AT`] rule as [`needs_recentre`]. Moving the centre would
/// leave a strip of the waterfall that the old rows never covered. False
/// when the span is not known yet (nothing running), so the centre is set.
pub fn keeps_centre(span: Span, f: f64) -> bool {
    span.width > 0.0 && needs_recentre(span, f).is_none()
}

/// A zoomed view that has lost the receiver pans to put it back in the
/// middle; `None` when the view can stay as it is.
pub fn keep_in_view(span: Span, cur: View, rx: f64) -> Option<View> {
    if !cur.zoomed(span) || cur.contains(rx) {
        return None;
    }
    let w = cur.width();
    Some(view(span, Some(View { lo: rx - w / 2.0, hi: rx + w / 2.0 })))
}

/// How far (Hz) the receiver must move to count as moved, so rounding in the
/// radio's reports never reads as a retune.
pub const RX_MOVED_HZ: f64 = 0.5;

/// [`keep_in_view`], but only when the receiver itself has moved since
/// `last_rx` (tuning, a mode snap, a click to tune): a view the operator
/// panned or zoomed away from the receiver stays where it was put.
/// `last_rx` is `None` before the first frame, which counts as a move.
pub fn follow_rx(span: Span, cur: View, rx: f64, last_rx: Option<f64>) -> Option<View> {
    let moved = last_rx.is_none_or(|l| (rx - l).abs() > RX_MOVED_HZ);
    if moved { keep_in_view(span, cur, rx) } else { None }
}

/// The frequency grid's spacing for a view `w` Hz wide: at most nine lines.
pub fn grid_step(w: f64) -> f64 {
    const STEPS: [f64; 16] = [
        100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 10e3, 25e3, 50e3, 100e3, 250e3, 500e3, 1e6, 2e6, 5e6,
        10e6,
    ];
    STEPS.iter().copied().find(|s| w / s <= 9.0).unwrap_or(10e6)
}

/// The grid lines inside `v`, every `step`, offset by `shift` (a transverter),
/// returned in the view's own frequency.
pub fn grid_lines(v: View, step: f64, shift: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut f = ((v.lo + shift) / step).ceil() * step;
    while f <= v.hi + shift && out.len() < 64 {
        out.push(f - shift);
        f += step;
    }
    out
}

/// A grid label in MHz, with as many places as the step needs.
pub fn grid_label(shown_hz: f64, step: f64) -> String {
    let mhz = shown_hz / 1e6;
    if step < 1000.0 {
        format!("{mhz:.5}")
    } else if step < 10_000.0 {
        format!("{mhz:.4}")
    } else {
        format!("{mhz:.3}")
    }
}

/// A frequency in MHz to the hertz, grouped like the radio's display:
/// 14.074.000.
pub fn fmt_freq(hz: f64) -> String {
    let neg = hz < 0.0;
    let h = hz.abs().round() as u64;
    let (m, k, u) = (h / 1_000_000, (h / 1000) % 1000, h % 1000);
    format!("{}{m}.{k:03}.{u:03}", if neg { "-" } else { "" })
}

/// `f` rounded to the tuning step (SDR Console's snap).
pub fn snap(f: f64, step: f64) -> f64 {
    if step <= 0.0 { f } else { (f / step).round() * step }
}

/// The fractional bin range `[first, last)` of a frame (`n` bins over
/// `[frame_lo, frame_lo + frame_span)`) that covers `[lo, hi]`, for
/// [`sdr::pool_max_range`].
pub fn bin_range(frame_lo: f64, frame_span: f64, n: usize, lo: f64, hi: f64) -> (f64, f64) {
    let per = n as f64 / frame_span;
    ((lo - frame_lo) * per, (hi - frame_lo) * per)
}

/// How much wider than the view a waterfall or 3D row's band is: half the
/// view's width either side, as SDRoxide's slack viewport.
pub const COVER_SLACK: f64 = 2.0;
/// A held band is kept while the view stays this fraction of the band's width
/// inside either edge (or the band is against the frame's edge there).
const HOLD_MARGIN: f64 = 0.05;
/// A held band is kept while it is this many times the view's width: past
/// either end a zoom has changed enough that the rows should follow it.
const HOLD_RES: std::ops::RangeInclusive<f64> = 1.15..=3.5;

/// The band a waterfall or 3D row would keep for a fresh view: twice the
/// view's width, centred on it and slid (not clipped) inside what the frame
/// actually holds, so a pan either way shows history rather than an empty
/// edge. At most the whole frame.
pub fn coverage(frame: Span, v: View) -> View {
    let w = (v.width() * COVER_SLACK).min(frame.width);
    if w <= 0.0 || v.width() <= 0.0 {
        return View { lo: frame.lo(), hi: frame.hi() };
    }
    let lo = (v.center() - w / 2.0).clamp(frame.lo(), (frame.hi() - w).max(frame.lo()));
    View { lo, hi: lo + w }
}

/// The band a row keeps, given the band the rows were last laid down over:
/// that one again while it is still good for the view (SDRoxide's
/// `cfg_still_good`), so pans inside it never move it and every row taken
/// meanwhile covers the same band; otherwise a fresh [`coverage`].
pub fn held_coverage(frame: Span, v: View, held: Option<View>) -> View {
    let fresh = coverage(frame, v);
    let Some(h) = held else { return fresh };
    // The frame moved off it (a recentre, another lane), or it is the whole
    // frame anyway: nothing to hold.
    let tol = 1.0;
    if h.lo < frame.lo() - tol || h.hi > frame.hi() + tol || fresh.width() >= frame.width - tol {
        return fresh;
    }
    let margin = h.width() * HOLD_MARGIN;
    let lo_ok = v.lo >= h.lo + margin || h.lo <= frame.lo() + tol;
    let hi_ok = v.hi <= h.hi - margin || h.hi >= frame.hi() - tol;
    let res = h.width() / v.width().max(1.0);
    if lo_ok && hi_ok && v.lo >= h.lo && v.hi <= h.hi && HOLD_RES.contains(&res) {
        h
    } else {
        fresh
    }
}

/// The radio's own frequency to what is shown, and back: a frequency
/// calibration in parts per million (what the radio's reference is off by;
/// 0 with a GPS-disciplined one) and a transverter's local oscillator.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FreqMap {
    pub ppm: f64,
    pub transverter_hz: f64,
}

impl FreqMap {
    /// What the scope shows for the radio's `hz`.
    pub fn shown(&self, hz: f64) -> f64 {
        hz * (1.0 + self.ppm * 1e-6) + self.transverter_hz
    }

    /// The radio's own frequency for a `shown` one.
    pub fn radio(&self, shown: f64) -> f64 {
        (shown - self.transverter_hz) / (1.0 + self.ppm * 1e-6)
    }

    /// The shown width of a band `hz` wide at the radio.
    pub fn width(&self, hz: f64) -> f64 {
        hz * (1.0 + self.ppm * 1e-6)
    }

    pub fn is_identity(&self) -> bool {
        self.ppm == 0.0 && self.transverter_hz == 0.0
    }
}

/// The zoom FFT's size for a view `view_w` Hz across `px` pixels when the
/// zoomed band runs at `rate`: about two bins a pixel, but never finer than
/// `min_bin` Hz and never more than a second of samples (or the display
/// would lag the band), between 2048 (the analyser's smallest) and 262 144 points.
///
/// The second is a hard limit. It used to give way to a 4096-point floor,
/// so a view zoomed in to a few hundred Hz (WSPR's 200 Hz) ran at a zoomed
/// rate of a few hundred samples a second and each FFT took 6 to 15 s of
/// them: the trace crawled and smeared like persistence (AJ3G, 11 Nov
/// 2026).
pub fn zoom_fft_size(rate: f64, view_w: f64, px: f32, min_bin: f64) -> usize {
    let want_bin = (view_w / (2.0 * px.max(1.0) as f64)).max(min_bin);
    let n = (rate / want_bin).max(1.0) as usize;
    // The largest power of two within a second of samples.
    let second = rate.max(2048.0) as usize;
    let cap = if second.is_power_of_two() { second } else { second.next_power_of_two() / 2 };
    n.next_power_of_two().min(cap).clamp(2048, 262_144)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: Span = Span { center: 14_074_000.0, width: 192_000.0 };

    #[test]
    fn the_whole_span_unless_zoomed() {
        let v = view(SPAN, None);
        assert_eq!((v.lo, v.hi), (13_978_000.0, 14_170_000.0));
        assert!(!v.zoomed(SPAN));
        assert_eq!(v.zoom(SPAN), 1.0);
    }

    #[test]
    fn a_view_never_leaves_the_span_or_gets_too_narrow() {
        let v = view(SPAN, Some(View { lo: 13_000_000.0, hi: 13_010_000.0 }));
        assert_eq!(v.lo, SPAN.lo());
        assert_eq!(v.width(), 10_000.0);
        let v = view(SPAN, Some(View { lo: 14_169_000.0, hi: 14_179_000.0 }));
        assert_eq!(v.hi, SPAN.hi());
        let v = view(SPAN, Some(View { lo: 14_074_000.0, hi: 14_074_010.0 }));
        assert_eq!(v.width(), 1500.0);
        // Wider than the span: the span.
        let v = view(SPAN, Some(View { lo: 13e6, hi: 15e6 }));
        assert_eq!(v, view(SPAN, None));
        // A 6 MHz span zooms to 1/4096 of itself.
        let wide = Span { center: 10e6, width: 6_144_000.0 };
        assert_eq!(min_width(wide), 1500.0);
        assert_eq!(min_width(Span { center: 10e6, width: 1000.0 }), 1000.0);
    }

    #[test]
    fn zoom_keeps_the_frequency_under_the_pointer() {
        let v = view(SPAN, None);
        let f = 14_100_000.0;
        let x0 = v.x_of(f, 0.0, 1000.0);
        let z = zoom_at(SPAN, v, f, 4.0);
        assert!((z.width() - 48_000.0).abs() < 1e-6);
        assert!((z.x_of(f, 0.0, 1000.0) - x0).abs() < 1e-3);
        assert!((z.zoom(SPAN) - 4.0).abs() < 1e-9);
        // Out again past the whole span: the whole span.
        let back = zoom_at(SPAN, z, f, 1.0 / 10.0);
        assert_eq!(back, view(SPAN, None));
        // Zooming out near an edge slides the view back inside rather than
        // past it.
        let edge = View { lo: SPAN.lo(), hi: SPAN.lo() + 20_000.0 };
        let e = zoom_at(SPAN, edge, SPAN.lo() + 1000.0, 0.5);
        assert_eq!(e.lo, SPAN.lo());
        assert!((e.width() - 40_000.0).abs() < 1e-6);
    }

    #[test]
    fn x_and_frequency_are_inverse() {
        let v = View { lo: 14e6, hi: 14.1e6 };
        for f in [14e6, 14.025e6, 14.1e6] {
            let x = v.x_of(f, 20.0, 800.0);
            assert!((v.f_at(x, 20.0, 800.0) - f).abs() < 1.0);
        }
        assert_eq!(v.x_of(14.05e6, 20.0, 800.0), 420.0);
    }

    #[test]
    fn pan_moves_the_view_with_the_drag_and_stops_at_the_edges() {
        let z = zoom_at(SPAN, view(SPAN, None), SPAN.center, 4.0);
        let p = pan(SPAN, z, 10_000.0);
        assert!((p.lo - (z.lo - 10_000.0)).abs() < 1e-6);
        let p = pan(SPAN, z, 1e6);
        assert_eq!(p.lo, SPAN.lo());
        assert_eq!(p.width(), z.width());
    }

    #[test]
    fn the_centre_follows_only_past_045_of_the_span() {
        assert_eq!(needs_recentre(SPAN, SPAN.center + 86_000.0), None);
        assert_eq!(needs_recentre(SPAN, SPAN.center - 86_400.0), None);
        assert_eq!(needs_recentre(SPAN, SPAN.center + 86_500.0), Some(14_160_000.0 + 1000.0));
        assert_eq!(needs_recentre(SPAN, 7_074_321.0), Some(7_074_000.0));
        assert_eq!(needs_recentre(Span { center: 0.0, width: 0.0 }, 5.0), None);
    }

    #[test]
    fn a_mode_snap_keeps_the_centre_when_its_frequency_is_in_the_span() {
        // FT8 to FT4 on 20 m: 14.080 is 6 kHz from 14.074, well inside.
        assert!(keeps_centre(SPAN, 14_080_000.0));
        assert!(keeps_centre(SPAN, SPAN.center + 86_000.0));
        // Past 0.45 of the span, or another band: the centre moves.
        assert!(!keeps_centre(SPAN, SPAN.center + 87_000.0));
        assert!(!keeps_centre(SPAN, 7_074_000.0));
        // Nothing running yet: set it.
        assert!(!keeps_centre(Span { center: 14_074_000.0, width: 0.0 }, 14_074_000.0));
    }

    #[test]
    fn a_zoomed_view_follows_a_receiver_that_leaves_it() {
        let z = zoom_at(SPAN, view(SPAN, None), SPAN.center, 8.0);
        assert_eq!(keep_in_view(SPAN, z, SPAN.center), None);
        let k = keep_in_view(SPAN, z, 14_150_000.0).unwrap();
        assert!(k.contains(14_150_000.0));
        assert!((k.width() - z.width()).abs() < 1e-6);
        // The whole span never pans.
        assert_eq!(keep_in_view(SPAN, view(SPAN, None), 20e6), None);
    }

    #[test]
    fn a_zoomed_view_follows_only_a_receiver_that_moves() {
        let rx = SPAN.center;
        let z = zoom_at(SPAN, view(SPAN, None), rx, 8.0);
        // Panned away from a receiver that has not moved: the view stays.
        let panned = pan(SPAN, z, -60_000.0);
        assert!(!panned.contains(rx));
        assert_eq!(follow_rx(SPAN, panned, rx, Some(rx)), None);
        // Ctrl+wheel zoom at a pointer far from the receiver stays too.
        let at = zoom_at(SPAN, z, z.lo + 10.0, 4.0);
        let at = zoom_at(SPAN, pan(SPAN, at, -50_000.0), 14_130_000.0, 2.0);
        assert!(!at.contains(rx));
        assert_eq!(follow_rx(SPAN, at, rx, Some(rx)), None);
        // A report a fraction of a hertz off is not a move.
        assert_eq!(follow_rx(SPAN, panned, rx + 0.3, Some(rx)), None);
        // The receiver tuned (or snapped) out of the view: it follows.
        let k = follow_rx(SPAN, panned, rx + 1000.0, Some(rx)).unwrap();
        assert!(k.contains(rx + 1000.0));
        assert!((k.width() - panned.width()).abs() < 1e-6);
        // The receiver moved but still on screen: nothing to do.
        assert_eq!(follow_rx(SPAN, z, rx + 500.0, Some(rx)), None);
        // The first frame counts as a move.
        assert!(follow_rx(SPAN, panned, rx, None).is_some());
    }

    #[test]
    fn grid_and_labels() {
        assert_eq!(grid_step(192_000.0), 25_000.0);
        assert_eq!(grid_step(6_144_000.0), 1e6);
        assert_eq!(grid_step(3000.0), 500.0);
        let v = View { lo: 14_010_000.0, hi: 14_060_000.0 };
        assert_eq!(grid_lines(v, 25_000.0, 0.0), [14_025_000.0, 14_050_000.0]);
        // Through a +116 MHz converter the lines fall on the shown frequency's
        // round numbers.
        let v2 = View { lo: 28_010_000.0 + 0.3, hi: 28_060_000.0 };
        let l = grid_lines(v2, 25_000.0, 116e6);
        assert_eq!(l, [28_025_000.0, 28_050_000.0]);
        assert_eq!(grid_label(14_025_000.0, 25_000.0), "14.025");
        assert_eq!(grid_label(14_025_500.0, 5000.0), "14.0255");
        assert_eq!(grid_label(14_025_500.0, 500.0), "14.02550");
        assert_eq!(fmt_freq(14_074_000.0), "14.074.000");
        assert_eq!(fmt_freq(144_174_050.0), "144.174.050");
    }

    #[test]
    fn snap_rounds_to_the_step() {
        assert_eq!(snap(14_074_049.0, 100.0), 14_074_000.0);
        assert_eq!(snap(14_074_050.0, 100.0), 14_074_100.0);
        assert_eq!(snap(14_074_049.0, 0.0), 14_074_049.0);
    }

    #[test]
    fn bins_map_to_frequency() {
        // 1024 bins over 192 kHz starting at 13.978 MHz: 187.5 Hz each.
        let (a, b) = bin_range(13_978_000.0, 192_000.0, 1024, 14_074_000.0, 14_074_187.5);
        assert!((a - 512.0).abs() < 1e-9 && (b - 513.0).abs() < 1e-9);
    }

    #[test]
    fn rows_keep_half_the_view_either_side_inside_the_frame() {
        let cov = coverage(SPAN, View { lo: 14_070_000.0, hi: 14_078_000.0 });
        assert_eq!((cov.lo, cov.hi), (14_066_000.0, 14_082_000.0));
        let cov = coverage(SPAN, view(SPAN, None));
        assert_eq!(cov, view(SPAN, None));
        // Against an edge the band slides inside the frame and keeps its
        // width rather than losing the half that would fall off.
        let cov = coverage(SPAN, View { lo: SPAN.lo(), hi: SPAN.lo() + 10_000.0 });
        assert_eq!((cov.lo, cov.hi), (SPAN.lo(), SPAN.lo() + 20_000.0));
        // Half the span or wider: the whole span.
        let cov = coverage(SPAN, View { lo: 14_000_000.0, hi: 14_100_000.0 });
        assert_eq!(cov, view(SPAN, None));
    }

    #[test]
    fn the_band_is_held_while_the_view_stays_inside_it() {
        let v0 = View { lo: 14_070_000.0, hi: 14_078_000.0 };
        let h = held_coverage(SPAN, v0, None);
        assert_eq!((h.lo, h.hi), (14_066_000.0, 14_082_000.0));
        // Pans of up to 3.2 kHz either way (to 5 % of the band from its
        // edge) keep the very same band.
        for d in [-3200.0, -1000.0, 0.0, 2000.0, 3200.0] {
            let v = View { lo: v0.lo + d, hi: v0.hi + d };
            assert_eq!(held_coverage(SPAN, v, Some(h)), h, "pan {d}");
        }
        // Further, a fresh band centred on the new view.
        let v = View { lo: v0.lo + 3300.0, hi: v0.hi + 3300.0 };
        let n = held_coverage(SPAN, v, Some(h));
        assert_eq!((n.lo, n.hi), (14_069_300.0, 14_085_300.0));
        // A small zoom in keeps it; one that makes it over 3.5 views wide,
        // or a zoom out that leaves under 1.15, does not.
        let z = View { lo: 14_071_000.0, hi: 14_077_000.0 };
        assert_eq!(held_coverage(SPAN, z, Some(h)), h);
        let z = View { lo: 14_073_000.0, hi: 14_075_000.0 };
        assert_ne!(held_coverage(SPAN, z, Some(h)), h);
        let z = View { lo: 14_067_000.0, hi: 14_081_000.0 };
        assert_ne!(held_coverage(SPAN, z, Some(h)), h);
        // A recentre that puts the band outside the frame drops it.
        let moved = Span { center: SPAN.center + 100_000.0, ..SPAN };
        let v = View { lo: moved.lo(), hi: moved.lo() + 8000.0 };
        let n = held_coverage(moved, v, Some(h));
        assert_eq!((n.lo, n.hi), (moved.lo(), moved.lo() + 16_000.0));
        // Against the frame's edge, the view may reach the band's edge there.
        let e =
            held_coverage(SPAN, View { lo: SPAN.lo() + 2000.0, hi: SPAN.lo() + 10_000.0 }, None);
        assert_eq!(e.lo, SPAN.lo());
        let v = View { lo: SPAN.lo(), hi: SPAN.lo() + 8000.0 };
        assert_eq!(held_coverage(SPAN, v, Some(e)), e);
        // Not zoomed, the whole span, held or not.
        assert_eq!(held_coverage(SPAN, view(SPAN, None), Some(h)), view(SPAN, None));
    }

    #[test]
    fn a_transverter_and_a_calibration_map_both_ways() {
        let m = FreqMap { ppm: 0.0, transverter_hz: 116e6 };
        assert_eq!(m.shown(28_174_000.0), 144_174_000.0);
        assert_eq!(m.radio(144_174_000.0), 28_174_000.0);
        let d = FreqMap { ppm: 0.0, transverter_hz: -116e6 };
        assert_eq!(d.shown(144_174_000.0), 28_174_000.0);
        let c = FreqMap { ppm: 1.0, transverter_hz: 0.0 };
        assert!((c.shown(14e6) - 14_000_014.0).abs() < 1e-6);
        assert!((c.radio(c.shown(14_074_123.4)) - 14_074_123.4).abs() < 1e-6);
        assert!(FreqMap::default().is_identity());
        assert!(!c.is_identity());
    }

    #[test]
    fn zoom_fft_size_is_two_bins_a_pixel_within_limits() {
        // 6 kHz over 1000 px at 12 kHz: 3 Hz bins wanted, 4096 points.
        assert_eq!(zoom_fft_size(12_000.0, 6000.0, 1000.0, 0.1), 4096);
        // 1.5 kHz across 2000 px of a 3 kHz band wants 0.375 Hz bins, but a
        // second of samples holds 2048 points.
        assert_eq!(zoom_fft_size(3000.0, 1500.0, 2000.0, 0.1), 2048);
        // WSPR's 200 Hz zoomed in, at the lowest zoomed rate: no FFT spans
        // more than a second of samples.
        let n = zoom_fft_size(4096.0, 300.0, 1600.0, 0.05);
        assert!(n as f64 / 4096.0 <= 1.0, "{n} points at 4096 Hz");
        assert_eq!(zoom_fft_size(96_000.0, 40_000.0, 2000.0, 0.1), 16_384);
        // Never finer than asked.
        assert_eq!(zoom_fft_size(96_000.0, 40_000.0, 2000.0, 20.0), 8192);
        assert_eq!(zoom_fft_size(6_144_000.0, 1.0, 2000.0, 0.0001), 262_144);
    }
}
