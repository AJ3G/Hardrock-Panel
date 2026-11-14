//! The TFT: its top bar, the spectrum (flat, 3D or both), the band plan
//! strip, the waterfall, the grips between them and the status lines, and
//! what the mouse does on them.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Vec2, pos2,
    vec2,
};

use super::bandplan;
use super::digimodes;
use super::gpu::{self, SurfaceParams, TraceParams, WfParams};
use super::levels::{self, level_text, s_unit};
use super::markers::{fmt_delta, fmt_tenth};
use super::offset::{BAR_INK, OffsetBar, fmt_offset};
use super::pointer::{self, Drag, DragKind, Grab, drag_edge, grab_at, wheel_zoom};
use super::rows::Upload;
use super::settings::{Scale, TopView};
use super::skim;
use super::slotlines;
use super::txsig;
use super::view::{self, FreqMap, Span, View, grid_lines, grid_step};
use super::{Scope, ScopeOut, ScopeRig};
use crate::theme;

/// What every part of the TFT is drawn against this frame.
pub(super) struct Ctx {
    pub span: Span,
    pub view: View,
    pub map: FreqMap,
    pub rig: ScopeRig,
    pub now: f64,
}

/// The right-hand gutter for the level scale and the colour bar, points.
const GUTTER: f32 = 44.0;
const TBAR: f32 = 22.0;
const BAND_STRIP: f32 = 30.0;
/// The band plan strip's segment row; the data modes' lane is below it.
const SEG_H: f32 = 15.0;
const GRIP: f32 = 9.0;
const STATUS: f32 = 20.0;

// The TFT's inks, from the theme's instrument palette.
fn ground() -> Color32 {
    theme::scope().ground
}
fn grid() -> Color32 {
    theme::scope().line.gamma_multiply(0.6)
}
fn edge() -> Color32 {
    theme::scope().line
}
fn ink() -> Color32 {
    theme::scope().ink_strong
}
fn dim() -> Color32 {
    theme::scope().ink.gamma_multiply(0.7)
}
fn accent() -> Color32 {
    theme::scope().accent
}
/// The station being worked (decision 102): the slot lines' clear green,
/// already on the scope, at full strength. Green is no warning colour.
const DX_INK: Color32 = slotlines::START_INK;
/// Our own transmission, as GLOBE and the TX window draw it: on the scope,
/// the TX MONITOR caption only (decision 103).
fn tx_ink() -> Color32 {
    txsig::TX_INK
}
/// Over what was last received while the radio transmits something the
/// panel does not know (decision 74): dimmed, so it is plain nothing is
/// being heard.
const TX_VEIL: Color32 = Color32::from_black_alpha(150);
/// A receiver passband's width, as the radio's filter is named: "2.80 kHz",
/// "500 Hz".
pub(crate) fn bw_text(pb: (f32, f32)) -> String {
    let w = (pb.1 - pb.0).abs();
    if w >= 1000.0 { format!("BW {:.2} kHz", w / 1000.0) } else { format!("BW {w:.0} Hz") }
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
fn label(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Draw the whole TFT and handle the mouse on it.
pub(super) fn tft(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    cx: &Ctx,
    snap: &sdr::Snapshot,
    upload: Option<Arc<Upload>>,
    out: &mut ScopeOut,
) {
    let s = &sc.settings;
    let w = ui.available_width().max(200.0);
    let spec_h = (s.height * s.split).round();
    let wf_h = s.height - spec_h;
    let bp_h = if s.band_plan { BAND_STRIP } else { 0.0 };
    let total = TBAR + spec_h + bp_h + GRIP + wf_h + GRIP + 2.0 * STATUS;
    let (outer, _) = ui.allocate_exact_size(vec2(w, total), Sense::hover());
    let p = ui.painter_at(outer);
    p.rect_filled(outer, 2.0, ground());
    p.rect_stroke(outer, 2.0, Stroke::new(1.0, edge()), egui::StrokeKind::Inside);

    let mut y = outer.top();
    let mut take = |h: f32| {
        let r = Rect::from_min_size(pos2(outer.left(), y), vec2(w, h));
        y += h;
        r
    };
    let tbar = take(TBAR);
    let spec = take(spec_h);
    let bp = take(bp_h);
    let split_grip = take(GRIP);
    let wf = take(wf_h);
    let height_grip = take(GRIP);
    let status = take(STATUS);
    let diag = take(STATUS);
    let (spec_plot, spec_gut) = split_gutter(spec);
    let (bp_plot, bp_gut) = split_gutter(bp);
    let (wf_plot, wf_gut) = split_gutter(wf);

    // The skimmer's labels this frame (decision 54).
    sc.skim_shown = skim::labels(&sc.skim, super::utc_now());
    sc.skim_on_bar = sc.offset.and_then(|b| skim::on_bar(&sc.skim_shown, b.hz));
    sc.offset_close = None;

    // Which callback carries this frame's rows: the first one on screen.
    let mut upload = upload;
    let surface_h = match sc.settings.top {
        TopView::Spectrum | TopView::Scf => 0.0,
        TopView::Surface => spec_plot.height(),
        TopView::Both => (spec_plot.height() * 0.55).round(),
    };
    let surface = Rect::from_min_size(spec_plot.min, vec2(spec_plot.width(), surface_h));
    let trace = Rect::from_min_max(pos2(spec_plot.left(), surface.bottom()), spec_plot.max);

    if surface_h > 0.0 {
        paint_surface(sc, ui, surface, trace.height() < 1.0, cx, &mut upload);
    }
    paint_spectrum(sc, ui, spec_plot, trace, spec_gut, cx, snap, &mut upload);
    if bp_h > 0.0 {
        paint_band_plan(sc, ui, bp_plot, bp_gut, cx, out);
    }
    paint_waterfall(sc, ui, wf_plot, wf_gut, cx, upload);
    grip(sc, ui, split_grip, true);
    grip(sc, ui, height_grip, false);

    // The mouse on the spectrum and the waterfall.
    let spec_resp = ui.interact(spec_plot, ui.id().with("scope-spec"), Sense::click_and_drag());
    pointer_on(sc, ui, &spec_resp, spec_plot, cx, out);
    let wf_resp = ui.interact(wf_plot, ui.id().with("scope-wf"), Sense::click_and_drag());
    pointer_on(sc, ui, &wf_resp, wf_plot, cx, out);
    paint_hover_cursor(sc, ui, spec_plot, wf_plot, cx);

    top_bar(sc, ui, tbar, cx, snap);
    status_bars(sc, ui, status, diag, cx, snap);
}

/// The mouse cursor over the spectrum or the waterfall (decision 98): a thin
/// line through both at its frequency, and beside it a readout of that
/// frequency and the strongest level within a column of it (AJ3G, 11 Nov
/// 2026). Not over a transmission's held picture, whose levels aren't the
/// band's.
fn paint_hover_cursor(sc: &Scope, ui: &egui::Ui, spec: Rect, wf: Rect, cx: &Ctx) {
    let Some(pos) = ui.ctx().pointer_hover_pos() else { return };
    if !(spec.contains(pos) || wf.contains(pos)) || spec.width() < 1.0 {
        return;
    }
    let v = cx.view;
    let hz = v.f_at(pos.x, spec.left(), spec.width());
    let px_hz = (v.hi - v.lo) / spec.width() as f64;
    let both = spec.union(wf);
    let p = ui.painter_at(both);
    let xx = pos.x.round() + 0.5;
    let line = Stroke::new(1.0, ink().gamma_multiply(0.6));
    p.vline(xx, spec.y_range(), line);
    p.vline(xx, wf.y_range(), line);
    let mut text = format!("{} MHz", mhz_spaced(cx.map.shown(hz)));
    if let Some(af) = hover_af(hz, &cx.rig) {
        text.push_str(&format!(" · {af:.0} Hz"));
    }
    if !sc.held
        && let Some(f) = sc.shown()
        && let Some(pk) = hover_level(f, hz, px_hz)
    {
        text.push_str(&format!(" · {}", level_text(pk, sc.settings.scale)));
    }
    let g = p.layout_no_wrap(text, mono(10.5), ink());
    let pad = vec2(5.0, 3.0);
    let size = g.size() + pad * 2.0;
    // Up and to the right of the pointer, flipped at the edges.
    let mut x = pos.x + 10.0;
    if x + size.x > both.right() - 2.0 {
        x = pos.x - 10.0 - size.x;
    }
    let mut y = pos.y - 10.0 - size.y;
    if y < both.top() + 2.0 {
        y = pos.y + 14.0;
    }
    let r = Rect::from_min_size(pos2(x, y), size);
    p.rect_filled(r, 2.0, Color32::from_black_alpha(210));
    p.rect_stroke(r, 2.0, Stroke::new(1.0, ink().gamma_multiply(0.5)), egui::StrokeKind::Inside);
    p.galley(r.min + pad, g, ink());
}

/// The audio offset `hz` would be heard at, as DECODE's AF column and the
/// offset bar read it (decision 101): Hz from the receive dial, counting up
/// into the passband on either sideband. `None` outside the receiver's
/// passband, or with no dial known, where no audio is made.
fn hover_af(hz: f64, rig: &ScopeRig) -> Option<f64> {
    let dial = rig.rx_hz;
    if !(dial.is_finite() && dial > 0.0) {
        return None;
    }
    let (a, b) = (dial + rig.passband.0 as f64, dial + rig.passband.1 as f64);
    if !(a.min(b)..=a.max(b)).contains(&hz) {
        return None;
    }
    Some(OffsetBar { hz }.offset_hz(dial, rig.mode.is_lower()).round() + 0.0)
}

/// A frequency to the hertz as "14.213 720" (MHz), as the TX window's lists
/// write it.
fn mhz_spaced(hz: f64) -> String {
    let hz = hz.round().max(0.0) as u64;
    let rest = hz % 1_000_000;
    format!("{}.{:03} {:03}", hz / 1_000_000, rest / 1000, rest % 1000)
}

/// The strongest level within a drawn column either side of `hz`, so the
/// readout catches a narrow signal the pointer is a pixel off.
fn hover_level(f: &sdr::SpectrumFrame, hz: f64, px_hz: f64) -> Option<f32> {
    let half = px_hz.max(f.bin_width() * 0.5);
    f.peak_in(hz - half, hz + half).map(|p| p.dbm)
}

fn split_gutter(r: Rect) -> (Rect, Rect) {
    let g = GUTTER.min(r.width() * 0.2);
    (
        Rect::from_min_max(r.min, pos2(r.right() - g, r.bottom())),
        Rect::from_min_max(pos2(r.right() - g, r.top()), r.max),
    )
}

/// The flat spectrum (in `trace`), the frequency grid and every marker over
/// the whole spectrum area (`all`, which includes the 3D surface's part).
#[allow(clippy::too_many_arguments)]
fn paint_spectrum(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    all: Rect,
    trace: Rect,
    gut: Rect,
    cx: &Ctx,
    snap: &sdr::Snapshot,
    upload: &mut Option<Arc<Upload>>,
) {
    let p = ui.painter_at(all.union(gut));
    let v = cx.view;
    let x = |f: f64| v.x_of(f, all.left(), all.width());
    let (floor, ceil) = (sc.settings.floor_dbm, sc.settings.ceil_dbm);
    let has_trace = trace.height() >= 1.0;
    let y = |d: f32| trace.bottom() - (d - floor) / (ceil - floor) * trace.height();

    // Level grid and its labels.
    if has_trace {
        for m in levels::level_marks(floor, ceil, sc.settings.scale) {
            let yy = y(m.dbm).round() + 0.5;
            if m.line {
                p.hline(trace.x_range(), yy, Stroke::new(1.0, grid()));
            } else {
                p.hline(
                    trace.right() - 8.0..=trace.right(),
                    yy,
                    Stroke::new(1.0, accent().gamma_multiply(0.5)),
                );
            }
            if m.label.is_empty() || yy < trace.top() + 6.0 || yy > trace.bottom() - 6.0 {
                continue;
            }
            let in_gutter = match sc.settings.scale {
                Scale::Both => !m.line,
                _ => true,
            };
            let col = if m.s9 { accent() } else { dim() };
            if in_gutter {
                p.text(pos2(gut.left() + 4.0, yy), Align2::LEFT_CENTER, &m.label, label(10.5), col);
            } else {
                p.text(
                    pos2(trace.left() + 4.0, yy - 2.0),
                    Align2::LEFT_BOTTOM,
                    &m.label,
                    mono(10.0),
                    dim(),
                );
            }
        }
    }

    // The ham bands, faintly.
    for b in bandplan::HAM_BANDS {
        let (lo, hi) = (cx.map.radio(b.lo), cx.map.radio(b.hi));
        if hi > v.lo && lo < v.hi {
            let r = Rect::from_x_y_ranges(
                x(lo).max(all.left())..=x(hi).min(all.right()),
                all.y_range(),
            );
            p.rect_filled(r, 0.0, accent().gamma_multiply(0.05));
        }
    }

    // The frequency grid, on the shown frequency's round numbers.
    let step = grid_step(cx.map.width(v.width()));
    let shift = cx.map.shown(v.lo) - v.lo;
    for f in grid_lines(v, step, shift) {
        let xx = x(f).round() + 0.5;
        if has_trace {
            p.vline(xx, trace.y_range(), Stroke::new(1.0, grid()));
        }
        let lab = view::grid_label(cx.map.shown(f), step);
        p.text(pos2(xx, all.bottom() - 3.0), Align2::CENTER_BOTTOM, lab, mono(10.0), dim());
    }

    // The slices besides it (decision 66): each passband in its colour,
    // its name along the top.
    for sl in &sc.slices {
        let (a, b) = (x(sl.hz + sl.passband.0 as f64), x(sl.hz + sl.passband.1 as f64));
        let (a, b) = (a.min(b), a.max(b).max(a.min(b) + 2.0));
        if b < all.left() || a > all.right() {
            continue;
        }
        p.rect_filled(
            Rect::from_x_y_ranges(a..=b, all.y_range()),
            0.0,
            sl.colour.gamma_multiply(0.14),
        );
        for e in [a, b] {
            p.vline(e, all.y_range(), Stroke::new(1.0, sl.colour.gamma_multiply(0.7)));
        }
        p.text(
            pos2(0.5 * (a + b), all.top() + 40.0),
            Align2::CENTER_TOP,
            sl.name,
            mono(11.0),
            sl.colour,
        );
    }

    // The receiver's passband, and its edges as handles.
    let rx = cx.rig.rx_hz;
    let (pa, pb) = (x(rx + cx.rig.passband.0 as f64), x(rx + cx.rig.passband.1 as f64));
    p.rect_filled(
        Rect::from_x_y_ranges(pa.min(pb)..=pa.max(pb).max(pa.min(pb) + 2.0), all.y_range()),
        0.0,
        accent().gamma_multiply(0.18),
    );
    // Its width, always, inside it when there is room (the RX label above
    // carries it too).
    let (wa, wb) = (pa.min(pb), pa.max(pb));
    let wg = p.layout_no_wrap(bw_text(cx.rig.passband), mono(10.0), accent());
    if wb - wa >= wg.size().x + 8.0 && wb > all.left() && wa < all.right() {
        let cxp = (0.5 * (wa + wb))
            .clamp(all.left() + 0.5 * wg.size().x, all.right() - 0.5 * wg.size().x);
        p.galley(pos2(cxp - 0.5 * wg.size().x, all.top() + 22.0), wg, accent());
    }
    let edge_drag = matches!(sc.drag.map(|d| d.kind), Some(DragKind::Edge(_)));
    for ex in [pa, pb] {
        p.vline(ex, all.y_range(), Stroke::new(if edge_drag { 2.0 } else { 1.0 }, accent()));
        p.rect_filled(
            Rect::from_center_size(pos2(ex, all.center().y), vec2(6.0, 18.0)),
            1.0,
            accent(),
        );
    }

    // The trace: the zoom FFT's frame when it covers the view, else the
    // whole span's.
    let frame = sc.shown().cloned();
    let ppp = ui.ctx().pixels_per_point();
    sc.trace_px = if has_trace { (trace.width() * ppp).round() as u32 } else { 0 };
    let on_gpu = sc.gpu_trace();
    if has_trace && on_gpu {
        // The GPU's own columns, drawn where they fall in the view; the
        // persistence from what came back of them.
        if sc.settings.persist && !sc.cols.is_empty() {
            let cols = sc.cols.clone();
            paint_persistence(sc, ui, trace, &cols, cx.now);
        }
        if let (Some(g), Some(t)) = (sc.gpu, sc.trace_geom) {
            let x0 = (v.x_of(t.lo, trace.left(), trace.width()) - trace.left()) / trace.width();
            let x1 = (v.x_of(t.hi, trace.left(), trace.width()) - trace.left()) / trace.width();
            let rgb =
                |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
            let params = TraceParams {
                floor,
                ceil,
                cols: t.cols,
                x0,
                x1,
                px: [trace.width() * ppp, trace.height() * ppp],
                line_w: 1.2 * ppp,
                fill: !sc.settings.persist,
                line_a: if sc.settings.persist { 0.55 } else { 1.0 },
                peak: sc.settings.peak && t.peak,
                accent: rgb(accent()),
                ink: rgb(ink()),
            };
            let cb = gpu::TracePaint { id: sc.id, tex_w: g.tex_w, upload: upload.take(), params };
            ui.painter_at(trace).add(gpu::callback(trace, cb));
        }
    }
    if has_trace && let Some(f) = frame.as_ref().filter(|_| !on_gpu) {
        let ncols = ((trace.width() * ppp) as usize).max(16);
        let (cols, xs) = columns(f, v, trace, ncols, false);
        sc.cols.clone_from(&cols);
        if sc.settings.persist {
            paint_persistence(sc, ui, trace, &cols, cx.now);
        }
        // Fill, then the line. The TX monitor's is coloured by level through
        // the waterfall's palette, as its rows are (decision 103); only its
        // caption is in the TX orange.
        let tx_pal = sc.tx_live.then(|| TxPalette::new(sc));
        let ink_at = |dbm: f32, a: f32| match &tx_pal {
            Some(tp) => tp.at(dbm).gamma_multiply(a),
            None => accent().gamma_multiply(a),
        };
        if !cols.is_empty() {
            let top_a = 0.45;
            let mut mesh = Mesh::default();
            for (i, (&c, &xx)) in cols.iter().zip(&xs).enumerate() {
                let yy = y(c).clamp(trace.top(), trace.bottom());
                let t = 1.0 - (yy - trace.top()) / trace.height().max(1.0);
                let a = 0.03 + (top_a - 0.03) * t;
                mesh.colored_vertex(pos2(xx, yy), ink_at(c, a));
                mesh.colored_vertex(pos2(xx, trace.bottom()), ink_at(c, 0.03));
                if i > 0 {
                    let k = (i * 2) as u32;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k, k + 1);
                }
            }
            if !sc.settings.persist {
                p.add(Shape::mesh(mesh));
            }
            let line: Vec<Pos2> = cols
                .iter()
                .zip(&xs)
                .map(|(&c, &xx)| pos2(xx, y(c).clamp(trace.top(), trace.bottom())))
                .collect();
            let a = if sc.settings.persist { 0.55 } else { 1.0 };
            match &tx_pal {
                // A segment at a time, each in its level's colour.
                Some(_) => {
                    for (k, w) in line.windows(2).enumerate() {
                        let c = ink_at(cols[k].max(cols[k + 1]), a);
                        p.line_segment([w[0], w[1]], Stroke::new(1.2, c));
                    }
                }
                None => {
                    p.add(Shape::line(line, Stroke::new(1.2, accent().gamma_multiply(a))));
                }
            }
        }
        if sc.settings.peak && f.peak.is_some() {
            let (pk, pxs) = columns(f, v, trace, ncols, true);
            let line: Vec<Pos2> = pk
                .iter()
                .zip(&pxs)
                .map(|(&c, &xx)| pos2(xx, y(c).clamp(trace.top(), trace.bottom())))
                .collect();
            p.add(Shape::line(line, Stroke::new(1.0, ink().gamma_multiply(0.35))));
            sc.peak_cols = pk;
        }
    }
    if has_trace && sc.held && !sc.paused {
        paint_tx_trace(sc, &p, trace);
    }
    if has_trace && let Some(f) = &frame {
        // The noise floor estimate: a line that stands out from the trace,
        // and its reading at the right edge on a dark box, legible over
        // whatever is under it (AJ3G, 11 Nov 2026).
        if sc.settings.noise_line && f.noise_dbm > floor && f.noise_dbm < ceil {
            let yy = y(f.noise_dbm);
            p.add(Shape::dashed_line(
                &[pos2(trace.left(), yy), pos2(trace.right(), yy)],
                Stroke::new(NF_LINE_W, ink().gamma_multiply(0.9)),
                8.0,
                4.0,
            ));
            let rbw = f.bin_width() * f.enbw as f64;
            let g = p.layout_no_wrap(
                format!("NF {:.0} dBm/bin · RBW {}", f.noise_dbm, fmt_hz(rbw)),
                mono(10.5),
                ink(),
            );
            let r = nf_label_rect(trace, yy, g.size());
            p.rect_filled(r, 2.0, Color32::from_black_alpha(200));
            p.rect_stroke(
                r,
                2.0,
                Stroke::new(1.0, ink().gamma_multiply(0.5)),
                egui::StrokeKind::Inside,
            );
            p.galley(r.min + vec2(5.0, 2.0), g, ink());
        }
    }

    // Markers and their readout.
    if let Some(f) = &frame {
        let bins = sc.bins(f);
        let window = (v.width() / 200.0).max(bins.bin_hz * 3.0);
        let mut m = sc.markers;
        m.track(&bins, window);
        sc.markers = m;
        paint_markers(sc, &p, all, trace, cx, f);
    }
    if has_trace {
        paint_skim_spectrum(sc, &p, all, trace, cx);
    }
    paint_dx_mark(sc, &p, all, cx, true);
    paint_offset(sc, &p, all, cx, true);

    // The radio's centre, dashed.
    let xc = x(cx.span.center);
    if (all.left()..=all.right()).contains(&xc) {
        p.add(Shape::dashed_line(
            &[pos2(xc, all.top()), pos2(xc, all.bottom())],
            Stroke::new(1.0, ink().gamma_multiply(0.25)),
            3.0,
            4.0,
        ));
    }

    // The receiver marker, with its grab handle on top.
    let xv = x(rx).round() + 0.5;
    let rx_drag = matches!(sc.drag.map(|d| d.kind), Some(DragKind::Rx));
    p.vline(xv, all.y_range(), Stroke::new(if rx_drag { 2.0 } else { 1.0 }, ink()));
    p.add(Shape::convex_polygon(
        vec![pos2(xv - 6.0, all.top()), pos2(xv + 6.0, all.top()), pos2(xv, all.top() + 8.0)],
        ink(),
        Stroke::NONE,
    ));
    let scale = sc.settings.scale;
    let lvl = level_text(snap.level_dbm, scale);
    let snr = sc.snr(snap, &cx.rig).map_or(String::new(), |s| format!(" · SNR {s:.0} dB"));
    let bw = bw_text(cx.rig.passband);
    let lab = format!(
        "RX {} {} · {bw} · {lvl}{snr}",
        view::fmt_freq(cx.map.shown(rx)),
        cx.rig.mode.label()
    );
    let galley = p.layout_no_wrap(lab, mono(10.5), ink());
    let lx = if xv + 8.0 + galley.size().x > all.right() {
        xv - 8.0 - galley.size().x
    } else {
        xv + 8.0
    };
    p.galley(pos2(lx, all.top() + 6.0), galley, ink());

    // Which part of the span is on screen, when zoomed.
    if v.zoomed(cx.span) {
        let bw = 120.0;
        let bar =
            Rect::from_min_size(pos2(all.right() - bw - 8.0, all.top() + 22.0), vec2(bw, 6.0));
        p.rect_filled(bar, 1.0, Color32::from_black_alpha(140));
        let a = ((v.lo - cx.span.lo()) / cx.span.width) as f32;
        let b = (v.width() / cx.span.width) as f32;
        let r =
            Rect::from_min_size(pos2(bar.left() + a * bw, bar.top()), vec2((b * bw).max(2.0), 6.0));
        p.rect_filled(r, 1.0, accent());
        if sc.zoom.is_some() {
            p.text(
                pos2(bar.left(), bar.bottom() + 2.0),
                Align2::LEFT_TOP,
                "ZOOM FFT",
                label(10.0),
                accent(),
            );
        }
    }

    // FIT, twice a second.
    // Never over a transmission: the TX monitor's levels are not the band's.
    if sc.settings.fit && !sc.held && cx.now - sc.fit_at > 0.5 && !sc.cols.is_empty() {
        sc.fit_at = cx.now;
        if let Some(t) = levels::fit_target(&sc.cols) {
            let (a, b) = levels::fit_step((sc.settings.floor_dbm, sc.settings.ceil_dbm), t);
            sc.settings.floor_dbm = a;
            sc.settings.ceil_dbm = b;
        }
    }
}

/// The waterfall's palette, as it colours a level (decision 103): the TX
/// monitor's trace is drawn in it so our signal looks as a received one
/// does in the palette the operator picked. The same mapping as the
/// waterfall's shader: the waterfall's levels, then its contrast and
/// brightness.
struct TxPalette {
    lut: [u8; 256 * 4],
    floor: f32,
    ceil: f32,
    contrast: f32,
    brightness: f32,
}

/// The least of the palette a TX trace is drawn with, so the line stays
/// visible on the glass where it lies on the floor (the palettes start at
/// black).
const TX_TRACE_MIN_T: f32 = 0.4;

impl TxPalette {
    fn new(sc: &Scope) -> TxPalette {
        let s = &sc.settings;
        let (floor, ceil) = sc.wf_levels_now();
        TxPalette {
            lut: s.palette.lut(),
            floor,
            ceil,
            contrast: s.wf_contrast,
            brightness: s.wf_brightness,
        }
    }

    fn t(&self, dbm: f32) -> f32 {
        let t = (dbm - self.floor) / (self.ceil - self.floor).max(1.0);
        ((t - 0.5) * self.contrast + 0.5 + self.brightness).clamp(TX_TRACE_MIN_T, 1.0)
    }

    fn at(&self, dbm: f32) -> Color32 {
        let i = (self.t(dbm) * 255.0).round() as usize * 4;
        Color32::from_rgb(self.lut[i], self.lut[i + 1], self.lut[i + 2])
    }
}

/// The frame pooled into `n` columns across `rect` over the view (the peak
/// trace when `peak`), with each column's x. Columns only where the frame has
/// data: after a recentre the newest frame may not cover the whole view.
fn columns(
    f: &sdr::SpectrumFrame,
    v: View,
    rect: Rect,
    n: usize,
    peak: bool,
) -> (Vec<f32>, Vec<f32>) {
    let bins = if peak { f.peak.as_deref().unwrap_or(&f.db) } else { &f.db };
    let lo = v.lo.max(f.start_hz());
    let hi = v.hi.min(f.start_hz() + f.span_hz);
    if hi <= lo || bins.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let share = (hi - lo) / v.width();
    let n = ((n as f64 * share).round() as usize).max(2);
    let (a, b) = view::bin_range(f.start_hz(), f.span_hz, bins.len(), lo, hi);
    let mut cols = Vec::with_capacity(n);
    sdr::pool_max_range(bins, a, b, n, &mut cols);
    let x0 = v.x_of(lo, rect.left(), rect.width());
    let x1 = v.x_of(hi, rect.left(), rect.width());
    let xs = (0..n).map(|i| x0 + (x1 - x0) * (i as f32 + 0.5) / n as f32).collect();
    (cols, xs)
}

fn fmt_hz(hz: f64) -> String {
    if hz >= 1000.0 { format!("{:.2} kHz", hz / 1000.0) } else { format!("{hz:.1} Hz") }
}

/// The phosphor trace: fade, add this frame's trace, and draw.
fn paint_persistence(sc: &mut Scope, ui: &egui::Ui, trace: Rect, cols: &[f32], now: f64) {
    let ppp = ui.ctx().pixels_per_point();
    let w = cols.len();
    let h = ((trace.height() * ppp) as usize).clamp(16, 1024);
    sc.persist.resize(w, h);
    let dt = (now - sc.persist_at).clamp(0.0, 0.5) as f32;
    sc.persist_at = now;
    sc.persist.fade(dt, sc.settings.persist_secs / 3.0);
    sc.persist.add(cols, sc.settings.floor_dbm, sc.settings.ceil_dbm);
    let img = sc.persist.image(sc.settings.palette);
    let tex = match sc.persist_tex.as_mut() {
        Some(t) => {
            t.set(img, egui::TextureOptions::LINEAR);
            t
        }
        None => sc.persist_tex.insert(ui.ctx().load_texture(
            "scope-persistence",
            img,
            egui::TextureOptions::LINEAR,
        )),
    };
    // The columns may cover only part of the width (see `columns`); stretch
    // across the trace anyway, which is what they cover in the usual case.
    ui.painter_at(trace).image(
        tex.id(),
        trace,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// The measurement markers, and their readout across the top of the trace.
fn paint_markers(
    sc: &Scope,
    p: &egui::Painter,
    all: Rect,
    trace: Rect,
    cx: &Ctx,
    f: &sdr::SpectrumFrame,
) {
    let v = cx.view;
    let mut lines = Vec::new();
    for i in 0..2 {
        let Some(m) = sc.markers.m[i] else { continue };
        let r = sc.markers.read(i, &sc.bins(f));
        let xx = v.x_of(m.hz, all.left(), all.width());
        if (all.left()..=all.right()).contains(&xx) {
            let col = if sc.markers.active == i { ink() } else { ink().gamma_multiply(0.7) };
            p.add(Shape::dashed_line(
                &[pos2(xx, all.top() + 16.0), pos2(xx, all.bottom())],
                Stroke::new(1.0, col),
                4.0,
                3.0,
            ));
            let yy = r.map_or(trace.center().y, |r| {
                let t = (r.dbm - sc.settings.floor_dbm)
                    / (sc.settings.ceil_dbm - sc.settings.floor_dbm);
                (trace.bottom() - t * trace.height()).clamp(trace.top(), trace.bottom())
            });
            let d = 5.0;
            p.add(Shape::convex_polygon(
                vec![pos2(xx, yy - d), pos2(xx + d, yy), pos2(xx, yy + d), pos2(xx - d, yy)],
                col,
                Stroke::NONE,
            ));
            p.text(
                pos2(xx + 6.0, yy - 6.0),
                Align2::LEFT_BOTTOM,
                format!("M{}", i + 1),
                label(11.0),
                col,
            );
        }
        if let Some(r) = r {
            lines.push(format!(
                "M{} {}  {}{}",
                i + 1,
                fmt_tenth(cx.map.shown(r.hz)),
                level_text(r.dbm, sc.settings.scale),
                if m.track { " ▲" } else { "" }
            ));
        }
    }
    if let Some((df, dl)) = sc.markers.delta(&sc.bins(f)) {
        lines.push(format!("Δ {}  {:+.1} dB", fmt_delta(cx.map.width(df)), dl));
    }
    let mut yy = all.top() + 22.0;
    for l in lines {
        p.text(pos2(all.left() + 6.0, yy), Align2::LEFT_TOP, l, mono(10.5), ink());
        yy += 14.0;
    }
}

/// A skimmer label's ink: blue for a station calling CQ, the screen's ink
/// for the rest (neutral states are blue, AJ3G's rule), at its fade.
fn skim_ink(s: &skim::SkimSpot, alpha: f32) -> Color32 {
    let c = if s.cq() { accent() } else { ink() };
    c.gamma_multiply(alpha)
}

/// A skimmer label's text: its tag in the ink it is painted with. No
/// DXCC or grid badges on the scope (AJ3G, 14 Nov 2026: "Remove the DXCC
/// and BAND stuff from the scope"); DECODE, TX and GLOBE carry them.
fn skim_text(_sc: &Scope, p: &egui::Painter, s: &skim::SkimSpot, font: FontId, _alpha: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(&s.tag(), 0.0, egui::TextFormat::simple(font, Color32::PLACEHOLDER));
    p.layout_job(job)
}

/// The skimmer's labels up the spectrum's foot: each station's callsign,
/// standing on its frequency, over the grid's labels.
fn paint_skim_spectrum(sc: &Scope, p: &egui::Painter, all: Rect, trace: Rect, cx: &Ctx) {
    if trace.height() < 60.0 {
        return;
    }
    let v = cx.view;
    let font = mono(9.5 * theme::skimmer_font_scale());
    let foot = all.bottom() - 15.0;
    for (i, (s, a)) in sc.skim_shown.iter().enumerate() {
        if !v.contains(s.hz) {
            continue;
        }
        let xx = v.x_of(s.hz, all.left(), all.width()).round() + 0.5;
        // The station being worked: its tick in the worked green
        // (decision 102); else the one the offset bar is on, in its red.
        let mark = skim_mark(sc, i);
        let on = mark.is_some();
        let col = if on { ink() } else { skim_ink(s, *a) };
        let g = skim_text(sc, p, s, font.clone(), *a);
        let (w, h) = (g.size().x, g.size().y);
        // A bold tick on the station's frequency (AJ3G: easy to read).
        let tick = if let Some(c) = mark {
            Stroke::new(3.0, c)
        } else {
            Stroke::new(2.0, accent().gamma_multiply(*a))
        };
        p.vline(xx, foot - 8.0..=foot, tick);
        // Turned to read upwards, from just over the tick, on a backing so
        // the trace's noise does not run through it.
        let at = pos2(xx - 0.5 * h, foot - 5.0);
        let back =
            Rect::from_min_max(pos2(at.x - 1.0, at.y - w - 2.0), pos2(at.x + h + 1.0, at.y + 1.0));
        p.rect_filled(back, 2.0, ground().gamma_multiply(0.75 * a));
        p.add(egui::epaint::TextShape::new(at, g, col).with_angle(-std::f32::consts::FRAC_PI_2));
    }
}

/// The colour a skimmer label `i` is picked out in: the worked green for the
/// station being worked (decision 102), the bar's red for the one the
/// offset bar is on, else none. Green wins: the bar keeps its own flag.
fn skim_mark(sc: &Scope, i: usize) -> Option<Color32> {
    let s = &sc.skim_shown.get(i)?.0;
    let worked = sc
        .dx_mark
        .as_ref()
        .is_some_and(|(_, call)| !call.is_empty() && call.eq_ignore_ascii_case(&s.call));
    if worked {
        Some(DX_INK)
    } else if sc.skim_on_bar == Some(i) {
        Some(BAR_INK)
    } else {
        None
    }
}

/// Room for a label's text either side, and its box's height, points.
const SKIM_PAD: f32 = 4.0;
pub(super) const SKIM_LEAD: f32 = 5.0;

/// The skimmer's labels along the waterfall's top, where the newest rows
/// are: a box with the callsign beside each signal, a leader up from it,
/// stacked in lanes ([`skim::lanes`]). Remembers where they went, for the
/// pointer.
fn paint_skim_waterfall(sc: &mut Scope, ui: &egui::Ui, p: &egui::Painter, rect: Rect, cx: &Ctx) {
    sc.skim_boxes.clear();
    if sc.skim_shown.is_empty() {
        return;
    }
    let v = cx.view;
    let fs = theme::skimmer_font_scale();
    let font = mono(11.0 * fs);
    let box_h = 15.0 * fs;
    let x = |f: f64| v.x_of(f, rect.left(), rect.width());
    let vis: Vec<usize> =
        (0..sc.skim_shown.len()).filter(|&i| v.contains(sc.skim_shown[i].0.hz)).collect();
    let galleys: Vec<_> = vis
        .iter()
        .map(|&i| {
            let (s, a) = &sc.skim_shown[i];
            skim_text(sc, p, s, font.clone(), *a)
        })
        .collect();
    let boxes: Vec<(f32, f32)> = vis
        .iter()
        .zip(&galleys)
        .map(|(&i, g)| (x(sc.skim_shown[i].0.hz), g.size().x + 2.0 * SKIM_PAD))
        .collect();
    let lanes = skim::lanes(&boxes, SKIM_LEAD, 4.0);
    let hover = ui.ctx().pointer_hover_pos().filter(|q| rect.contains(*q));
    for (((&i, g), &(sx, w)), lane) in vis.iter().zip(galleys).zip(&boxes).zip(lanes) {
        let Some(lane) = lane else { continue };
        let a = sc.skim_shown[i].1;
        let top = rect.top() + 3.0 + lane as f32 * (box_h + 2.0);
        let r = Rect::from_min_size(pos2(sx + SKIM_LEAD, top), vec2(w, box_h));
        if r.bottom() > rect.bottom() - 2.0 {
            continue;
        }
        let hot = hover.is_some_and(|q| r.contains(q));
        // The station being worked, in the worked green (decision 102), or
        // the one the offset bar is on (AJ3G: "make it so I know which
        // station I am on") in the bar's red: its leader, pointer and box in
        // that colour, the box tinted and its call full white.
        let mark = skim_mark(sc, i);
        let on = mark.is_some();
        let mark_c = mark.unwrap_or(BAR_INK);
        let edge_c = if on {
            mark_c
        } else if hot {
            accent()
        } else {
            accent().gamma_multiply(0.55 * a)
        };
        // The leader, bold so it reads over the waterfall (AJ3G: it shows
        // where the station is, where the offset bar goes to transmit):
        // dark-edged, from a pointer at the top on the signal's frequency
        // down to the box's middle.
        let lead_c = if on {
            mark_c
        } else if hot {
            accent()
        } else {
            accent().gamma_multiply(a)
        };
        let lx = sx.round() + 0.5;
        let halo = Stroke::new(3.5, ground().gamma_multiply(0.7 * if on { 1.0 } else { a }));
        p.vline(lx, rect.top()..=r.center().y, halo);
        p.hline(lx..=r.left(), r.center().y, halo);
        p.vline(lx, rect.top()..=r.center().y, Stroke::new(1.5, lead_c));
        p.hline(lx..=r.left(), r.center().y, Stroke::new(1.5, lead_c));
        p.add(Shape::convex_polygon(
            vec![
                pos2(lx - 5.0, rect.top()),
                pos2(lx + 5.0, rect.top()),
                pos2(lx, rect.top() + 7.0),
            ],
            lead_c,
            Stroke::new(1.0, ground().gamma_multiply(0.7 * a)),
        ));
        p.rect_filled(r, 2.0, ground().gamma_multiply(0.85));
        if on {
            p.rect_filled(r, 2.0, mark_c.gamma_multiply(0.3));
        }
        p.rect_stroke(
            r,
            2.0,
            Stroke::new(
                if on {
                    2.0
                } else if hot {
                    1.5
                } else {
                    1.0
                },
                edge_c,
            ),
            egui::StrokeKind::Inside,
        );
        let col = if hot || on { ink() } else { skim_ink(&sc.skim_shown[i].0, a) };
        p.galley(pos2(r.left() + SKIM_PAD, r.center().y - 0.5 * g.size().y), g, col);
        sc.skim_boxes.push((r, i));
    }
}

/// The offset bar (decision 55): a line down the spectrum (`spectrum`) or
/// the waterfall, in red (`BAR_INK`, AJ3G's transmit marker), with its readout. On the spectrum
/// the readout is a flag at the bar's top: the offset from the dial, as
/// DECODE's AF column, and the bar's frequency, with a × that takes it away;
/// on the waterfall, the offset alone along the foot. Off the view, the
/// flag is pinned to that side with an arrow.
fn paint_offset(sc: &mut Scope, p: &egui::Painter, rect: Rect, cx: &Ctx, spectrum: bool) {
    let v = cx.view;
    // How busy TX AUTO finds the band: a faint neutral shade, darker
    // where busier, never a warning colour (decision 118).
    for &(lo, hi, busy) in &sc.tx_busy {
        let (a, b) = (v.x_of(lo, rect.left(), rect.width()), v.x_of(hi, rect.left(), rect.width()));
        let (a, b) = (a.min(b).max(rect.left()), a.max(b).min(rect.right()));
        if b > a {
            p.rect_filled(
                Rect::from_x_y_ranges(a..=b, rect.y_range()),
                0.0,
                ink().gamma_multiply(0.02 + 0.12 * busy.clamp(0.0, 1.0)),
            );
        }
    }
    let Some(bar) = sc.offset else { return };
    let xx = v.x_of(bar.hz, rect.left(), rect.width());
    let inside = (rect.left()..=rect.right()).contains(&xx);
    let lit = BAR_INK;
    if inside {
        let w = if sc.offset_drag { 1.5 } else { 1.0 };
        p.vline(xx.round() + 0.5, rect.y_range(), Stroke::new(w, lit));
    }
    let off = bar.offset_hz(cx.rig.rx_hz, cx.rig.mode.is_lower());
    let text = if spectrum {
        format!("{} · {}", fmt_offset(off), fmt_tenth(cx.map.shown(bar.hz)))
    } else {
        fmt_offset(off)
    };
    // The station it is on, by its call, so the bar reads which one even
    // where its label has no room.
    let text = match sc.skim_on_bar.and_then(|i| sc.skim_shown.get(i)) {
        Some((s, _)) => format!("{text} · {}", s.call),
        None => text,
    };
    // TX on the flag when the radio will transmit on the bar.
    let text = if sc.tx_follows && sc.tx_at(&cx.rig, cx.span.center, cx.span.width).from_bar {
        format!("TX {text}")
    } else {
        text
    };
    let text = match (inside, xx < rect.left()) {
        (true, _) => text,
        (false, true) => format!("◀ {text}"),
        (false, false) => format!("{text} ▶"),
    };
    let g = p.layout_no_wrap(text, mono(10.5), ink());
    let close_w = if spectrum { 14.0 } else { 0.0 };
    let size = g.size() + vec2(8.0 + close_w, 4.0);
    let y = if spectrum { rect.top() + 38.0 } else { rect.bottom() - size.y - 4.0 };
    let left = if !inside {
        if xx < rect.left() { rect.left() + 4.0 } else { rect.right() - size.x - 4.0 }
    } else if xx + 4.0 + size.x > rect.right() {
        xx - 4.0 - size.x
    } else {
        xx + 4.0
    };
    let r = Rect::from_min_size(pos2(left, y), size);
    p.rect_filled(r, 2.0, ground().gamma_multiply(0.9));
    p.rect_stroke(r, 2.0, Stroke::new(1.0, lit), egui::StrokeKind::Inside);
    p.galley(pos2(r.left() + 4.0, r.top() + 2.0), g, ink());
    if spectrum {
        let c = Rect::from_min_max(pos2(r.right() - close_w, r.top()), r.max);
        p.vline(c.left(), c.y_range(), Stroke::new(1.0, lit.gamma_multiply(0.6)));
        p.text(c.center(), Align2::CENTER_CENTER, "×", label(12.0), lit);
        sc.offset_close = Some(c);
    }
}

/// Over a transmission: what is shown, said at the trace's foot in the TX
/// colour. The TX monitor's frames are drawn as any (decision 83); with
/// nothing of ours known, the band as last received under a veil (decision
/// 74).
fn paint_tx_trace(sc: &Scope, p: &egui::Painter, trace: Rect) {
    let name = sc.tx_signal.as_ref().map_or("our audio", |s| s.name.as_str());
    let caption = if !sc.tx_hold.keyed() {
        "RECEIVE IN A MOMENT".to_string()
    } else if sc.tx_live {
        format!("TX MONITOR · {name} · as sent, where it lands")
    } else {
        "TRANSMITTING · band as last received · our signal not known here".to_string()
    };
    if !sc.tx_live {
        p.rect_filled(trace, 0.0, TX_VEIL);
    }
    let g = p.layout_no_wrap(caption, label(10.5), tx_ink());
    let at = pos2(trace.left() + 8.0, trace.bottom() - 6.0 - g.size().y);
    p.rect_filled(
        Rect::from_min_size(at - vec2(4.0, 2.0), g.size() + vec2(8.0, 4.0)),
        2.0,
        Color32::from_black_alpha(170),
    );
    p.galley(at, g, tx_ink());
}

/// The noise floor's reading: at the right edge of `trace`, just above the
/// line at `yy` (below it when there is no room), inside the trace.
fn nf_label_rect(trace: Rect, yy: f32, text: Vec2) -> Rect {
    let size = text + vec2(10.0, 4.0);
    let above = yy - 3.0 - size.y;
    let top = if above >= trace.top() { above } else { yy + 3.0 };
    let top = top.min(trace.bottom() - size.y).max(trace.top());
    Rect::from_min_size(pos2(trace.right() - 4.0 - size.x, top), size)
}

/// The NF line's width, points: twice the trace's, so it reads as a line of
/// its own (AJ3G, 11 Nov 2026).
const NF_LINE_W: f32 = 2.0;

/// The waterfall's rows laid down while held over a keying the panel knows
/// nothing of (decision 74): dimmed. (The TX monitor's rows are ordinary
/// rows.)
fn paint_tx_waterfall(
    sc: &Scope,
    p: &egui::Painter,
    rect: Rect,
    scroll_frac: f32,
    pts_per_row: f32,
    rows_vis: f32,
) {
    if sc.tx_rows.is_empty() {
        return;
    }
    let held: Vec<bool> = (0..rows_vis.ceil() as u32 + 1)
        .map_while(|age| sc.rows.wf_time(age))
        .map(|t| txsig::row_at(&sc.tx_rows, t).is_some())
        .collect();
    let y0 = |age: usize| slotlines::row_y(rect.top(), age as u32, scroll_frac, pts_per_row);
    let mut age = 0;
    while age < held.len() {
        if !held[age] {
            age += 1;
            continue;
        }
        // A run of held rows, veiled at once.
        let mut end = age;
        while end + 1 < held.len() && held[end + 1] {
            end += 1;
        }
        let (a, b) = (y0(age).max(rect.top()), (y0(end) + pts_per_row).min(rect.bottom()));
        if b > a {
            p.rect_filled(Rect::from_x_y_ranges(rect.x_range(), a..=b), 0.0, TX_VEIL);
        }
        age = end + 1;
    }
}

/// The station being worked (decision 102): as pronounced as the offset
/// bar, in green where the bar is red (green is the station, red is where we
/// transmit). A bold line with a dark halo through the spectrum and the
/// waterfall; on the spectrum a green-edged, green-tinted box with the call,
/// its audio offset from the dial and its frequency, a row under the bar's
/// flag so the two never overlap; on the waterfall the call along the foot,
/// a row over the bar's readout. Drawn before the bar, so a bar on the same
/// station stays visible down the middle of the green. Nothing to grab.
fn paint_dx_mark(sc: &Scope, p: &egui::Painter, rect: Rect, cx: &Ctx, spectrum: bool) {
    let Some((hz, call)) = &sc.dx_mark else { return };
    let v = cx.view;
    let xx = v.x_of(*hz, rect.left(), rect.width());
    if !(rect.left()..=rect.right()).contains(&xx) {
        return;
    }
    let lx = xx.round() + 0.5;
    p.vline(lx, rect.y_range(), Stroke::new(6.0, ground().gamma_multiply(0.75)));
    p.vline(lx, rect.y_range(), Stroke::new(3.0, DX_INK));
    if call.is_empty() {
        return;
    }
    let text = if spectrum {
        let off = OffsetBar { hz: *hz }.offset_hz(cx.rig.rx_hz, cx.rig.mode.is_lower());
        format!("{call} · {} · {}", fmt_offset(off), fmt_tenth(cx.map.shown(*hz)))
    } else {
        call.clone()
    };
    let g = p.layout_no_wrap(text, mono(10.5), ink());
    let size = g.size() + vec2(8.0, 4.0);
    // A row clear of the offset bar's flag (spectrum, at 38 points down) or
    // its readout (waterfall foot), both the same height as this box.
    let y = if spectrum {
        rect.top() + 38.0 + size.y + 4.0
    } else {
        rect.bottom() - 2.0 * size.y - 8.0
    };
    let left = if xx + 5.0 + size.x > rect.right() { xx - 5.0 - size.x } else { xx + 5.0 };
    let r = Rect::from_min_size(pos2(left, y), size);
    p.rect_filled(r, 2.0, ground().gamma_multiply(0.9));
    p.rect_filled(r, 2.0, DX_INK.gamma_multiply(0.3));
    p.rect_stroke(r, 2.0, Stroke::new(2.0, DX_INK), egui::StrokeKind::Inside);
    p.galley(pos2(r.left() + 4.0, r.top() + 2.0), g, ink());
}

/// The 3D surface: the receding floor, the GPU surface, and the receiver on
/// its front plane.
fn paint_surface(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    rect: Rect,
    alone: bool,
    cx: &Ctx,
    upload: &mut Option<Arc<Upload>>,
) {
    let p = ui.painter_at(rect);
    let h = if alone { rect.height() - 16.0 } else { rect.height() - 4.0 };
    let r = Rect::from_min_size(rect.min, vec2(rect.width(), h.max(8.0)));
    let v = cx.view;
    // The floor: the frequency lines running back to the far row.
    let step = grid_step(cx.map.width(v.width()));
    let shift = cx.map.shown(v.lo) - v.lo;
    let at = |xl: f32, d: f32| {
        let (px, py) = gpu::project(xl, d);
        pos2(r.left() + px * r.width(), r.top() + py * r.height())
    };
    for f in grid_lines(v, step, shift) {
        let xl = ((f - v.lo) / v.width()) as f32;
        p.line_segment([at(xl, 0.0), at(xl, 1.0)], Stroke::new(1.0, grid()));
    }
    p.line_segment([at(0.0, 1.0), at(1.0, 1.0)], Stroke::new(1.0, grid()));
    p.line_segment([at(0.0, 0.0), at(0.0, 1.0)], Stroke::new(1.0, grid()));
    p.line_segment([at(1.0, 0.0), at(1.0, 1.0)], Stroke::new(1.0, grid()));

    let Some(g) = sc.gpu else {
        p.text(r.center(), Align2::CENTER_CENTER, "3D needs the GPU renderer", label(12.0), dim());
        return;
    };
    if sc.rows.surface_count() == 0 {
        return;
    }
    let reference = sc.rows.ref_hz();
    let ppp = ui.ctx().pixels_per_point();
    let cols = ((r.width() * ppp) as u32).clamp(64, 4096);
    let (s3_floor, s3_ceil) = sc.s3_levels(cx.now);
    let ink3 = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
    let params = SurfaceParams {
        view_lo: (v.lo - reference) as f32,
        view_hi: (v.hi - reference) as f32,
        floor: s3_floor,
        ceil: s3_ceil,
        live: sc.rows.surface_head(),
        count: sc.rows.surface_count(),
        carry: sc.rows.surface_carry(),
        cols,
        solid: sc.settings.solid_3d,
        bg: ink3(ground()),
        ink: ink3(accent()),
        live_t: if sc.paused { 1.0 } else { sc.rows.live_t(cx.now) },
        prev: sc.rows.prev_band(),
        light: if sc.settings.lit_3d { 1.0 } else { 0.0 },
        noise: sc.noise_params(cx.now, sc.rows.surface_serial()),
    };
    let cb = gpu::SurfacePaint {
        id: sc.id,
        tex_w: g.tex_w,
        upload: upload.take(),
        palette: sc.settings.palette,
        params,
        px: [(r.width() * ppp).round() as u32, (r.height() * ppp).round() as u32],
    };
    ui.painter_at(r).add(gpu::callback(r, cb));

    // The receiver on the front plane, and the front plane's level labels.
    let p = ui.painter_at(rect);
    let fx = v.x_of(cx.rig.rx_hz, r.left(), r.width());
    let amp = gpu::front_plane(r.height());
    p.line_segment([pos2(fx, r.bottom()), pos2(fx, r.bottom() - amp)], Stroke::new(1.0, ink()));
    let (floor, ceil) = (sc.settings.floor_dbm, sc.settings.ceil_dbm);
    let mut d = (floor / 20.0).ceil() * 20.0;
    while d < ceil {
        let yy = r.bottom() - (d - floor) / (ceil - floor) * amp;
        let t = if sc.settings.scale == Scale::SUnits { s_unit(d) } else { format!("{d:.0}") };
        p.text(pos2(r.left() + 4.0, yy), Align2::LEFT_CENTER, t, mono(9.0), dim());
        d += 20.0;
    }
    if !alone {
        p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, edge()));
    }
}

/// The band plan strip: segments along its top with their names where they
/// fit, and along its bottom lane the data modes' dials (decision 39): a
/// tick at each, its passband shaded out from it, and its name where there
/// is room. Clicking a mode's marker tunes there in that mode; elsewhere the
/// strip tunes to the step.
fn paint_band_plan(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    rect: Rect,
    gut: Rect,
    cx: &Ctx,
    out: &mut ScopeOut,
) {
    let p = ui.painter_at(rect.union(gut));
    p.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, edge()));
    let v = cx.view;
    let (slo, shi) = (cx.map.shown(v.lo), cx.map.shown(v.hi));
    let x = |shown: f64| v.x_of(cx.map.radio(shown), rect.left(), rect.width());
    let seg = Rect::from_x_y_ranges(rect.x_range(), (rect.top() + 2.0)..=(rect.top() + SEG_H));
    let lane = Rect::from_x_y_ranges(rect.x_range(), (seg.bottom() + 1.0)..=(rect.bottom() - 1.0));
    for (a, b, u) in bandplan::segments_in(slo, shi) {
        let (xa, xb) = (x(a * 1000.0), x(b * 1000.0));
        let (ca, cb) = (xa.max(rect.left()), xb.min(rect.right()));
        let [r, g, bl] = u.rgb();
        let fill = theme::data_ink((r, g, bl));
        p.rect_filled(
            Rect::from_x_y_ranges(ca..=(cb - 1.0).max(ca + 1.0), seg.y_range()),
            0.0,
            fill,
        );
        let name = u.short();
        let gal =
            p.layout_no_wrap(name.to_string(), label(10.0), Color32::from_rgb(0xee, 0xf3, 0xf8));
        if cb - ca > gal.size().x + 8.0 {
            let pos =
                pos2((ca + cb) / 2.0 - gal.size().x / 2.0, seg.center().y - gal.size().y / 2.0);
            p.galley(pos, gal, Color32::WHITE);
        }
    }

    // The data modes: their passbands first, so every tick sits over them.
    let marks = digimodes::marks(slo, shi);
    let hover_hz = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|q| rect.contains(*q))
        .map(|q| cx.map.shown(v.f_at(q.x, rect.left(), rect.width())));
    let px_hz = cx.map.width(v.width()) / rect.width().max(1.0) as f64;
    let under = hover_hz.and_then(|hz| digimodes::mark_at(hz, 5.0 * px_hz));
    for (&dial, modes) in &marks {
        for d in modes {
            let (a, b) = d.extent(dial);
            let (xa, xb) = (x(a).max(rect.left()), x(b).min(rect.right()));
            let lit = under.is_some_and(|(u, f)| f == dial && std::ptr::eq(u, *d));
            let c = mode_ink(d.kind).gamma_multiply(if lit { 0.75 } else { 0.38 });
            p.rect_filled(Rect::from_x_y_ranges(xa..=xb.max(xa + 1.0), lane.y_range()), 0.0, c);
        }
    }
    // Ticks, then names where they fit: the modes with keys first, so FT8
    // is never the one crowded out by a neighbour.
    let mut taken: Vec<(f32, f32)> = Vec::new();
    for key_pass in [true, false] {
        for (&dial, modes) in &marks {
            if modes.iter().any(|m| m.key) != key_pass {
                continue;
            }
            let xx = x(dial as f64).round();
            let lit = under.is_some_and(|(_, f)| f == dial);
            let c = if lit { ink() } else { mode_ink(modes[0].kind) };
            p.vline(xx, rect.y_range(), Stroke::new(if lit { 2.0 } else { 1.0 }, c));
            let names: Vec<&str> = modes.iter().map(|m| m.name).collect();
            let gal = p.layout_no_wrap(names.join("/"), label(9.5), ink());
            let span = (xx + 2.0, xx + 3.0 + gal.size().x + 4.0);
            let free = taken.iter().all(|&(a, b)| span.1 <= a || span.0 >= b);
            if free && span.0 >= rect.left() && span.1 <= rect.right() {
                p.galley(pos2(xx + 3.0, lane.center().y - gal.size().y / 2.0), gal, ink());
                taken.push(span);
            }
        }
    }
    p.text(gut.center(), Align2::CENTER_CENTER, "R2", label(10.0), dim());

    let resp = ui.interact(rect, ui.id().with("scope-bp"), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if let Some(shown) =
        resp.hover_pos().map(|pos| cx.map.shown(v.f_at(pos.x, rect.left(), rect.width())))
    {
        sc.cursor = match under {
            Some((d, dial)) => {
                let (a, b) = d.passband(dial);
                format!(
                    "{} · {} {} · {:.0} to {:.0} Hz · click to tune there",
                    d.name,
                    d.rig_mode(dial),
                    digimodes::mhz(dial),
                    a,
                    b
                )
            }
            None => bandplan::describe(shown),
        };
        // SAFETY INTERLOCK 9: nothing tunes while transmitting.
        if resp.clicked() && !cx.rig.transmitting {
            match under {
                Some((d, dial)) => super::controls::pick_data(sc, out, cx, d, dial, None),
                None => {
                    out.tune_to = Some(cx.map.radio(view::snap(shown, sc.settings.step_hz as f64)));
                }
            }
        }
    }
}

/// A data mode marker's ink, by what the mode is: blues, teals and violets
/// only, never the warning colours.
fn mode_ink(k: digimodes::Kind) -> Color32 {
    match k {
        digimodes::Kind::Weak => accent(),
        digimodes::Kind::Keyboard => theme::data_ink((0x3f, 0xd0, 0xc8)),
        digimodes::Kind::Picture => theme::data_ink((0xa0, 0x8c, 0xf0)),
        digimodes::Kind::Link => theme::data_ink((0x8a, 0xa4, 0xc8)),
    }
}

/// The waterfall on the GPU, with the receiver, the markers, the slot lines,
/// the UTC ticks and the colour bar over it.
fn paint_waterfall(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    rect: Rect,
    gut: Rect,
    cx: &Ctx,
    upload: Option<Arc<Upload>>,
) {
    let p = ui.painter_at(rect.union(gut));
    let v = cx.view;
    let (floor, ceil) = sc.wf_range();
    let ppp = ui.ctx().pixels_per_point();
    let px_per_row = sc.settings.wf_speed.px_per_row();
    let rows_vis = (rect.height() * ppp / px_per_row).max(1.0);
    let scroll_frac = if sc.paused {
        1.0
    } else {
        ((cx.now - sc.row_at) / sc.row_interval.max(1e-3)).clamp(0.0, 1.0) as f32
    };
    match sc.gpu {
        Some(g) => {
            let reference = sc.rows.ref_hz();
            let params = WfParams {
                view_lo: (v.lo - reference) as f32,
                view_hi: (v.hi - reference) as f32,
                floor,
                ceil,
                newest: sc.rows.wf_head(),
                count: sc.rows.wf_count(),
                rows_vis,
                width_pts: rect.width(),
                smooth: sc.settings.wf_smooth,
                scroll_frac,
                contrast: sc.settings.wf_contrast,
                brightness: sc.settings.wf_brightness,
                noise: sc.noise_params(cx.now, sc.rows.wf_serial()),
            };
            let cb = gpu::WaterfallPaint {
                id: sc.id,
                tex_w: g.tex_w,
                upload,
                palette: sc.settings.palette,
                params,
            };
            ui.painter_at(rect).add(gpu::callback(rect, cb));
        }
        None => {
            p.rect_filled(rect, 0.0, ground());
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "The waterfall needs the GPU renderer",
                label(12.0),
                dim(),
            );
        }
    }

    paint_tx_waterfall(sc, &p, rect, scroll_frac, px_per_row / ppp, rows_vis);

    // The slices' passband edges (decision 66), faint in their colours.
    for sl in &sc.slices {
        for e in [sl.passband.0, sl.passband.1] {
            let ex = v.x_of(sl.hz + e as f64, rect.left(), rect.width());
            if (rect.left()..=rect.right()).contains(&ex) {
                p.vline(ex, rect.y_range(), Stroke::new(1.0, sl.colour.gamma_multiply(0.45)));
            }
        }
    }

    // The receiver, and its passband's edges as poles that can be grabbed
    // here as on the spectrum: a line down the waterfall with a handle at
    // its top, brighter under the pointer and while dragged.
    let x = |f: f64| v.x_of(f, rect.left(), rect.width());
    let rx = cx.rig.rx_hz;
    p.vline(x(rx).round() + 0.5, rect.y_range(), Stroke::new(1.0, ink().gamma_multiply(0.6)));
    let (ea, eb) = (x(rx + cx.rig.passband.0 as f64), x(rx + cx.rig.passband.1 as f64));
    let hover = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|q| rect.contains(*q) && sc.drag.is_none() && !cx.rig.transmitting)
        .and_then(|q| pointer::grab_at(q.x, x(rx), ea, eb));
    let dragging = sc.drag.map(|d| d.kind);
    for (ex, g) in [(ea, Grab::EdgeLo), (eb, Grab::EdgeHi)] {
        let hot = hover == Some(g) || dragging == Some(DragKind::Edge(g));
        let c = if hot { accent() } else { accent().gamma_multiply(0.55) };
        p.vline(ex, rect.y_range(), Stroke::new(if hot { 2.0 } else { 1.0 }, c));
        p.rect_filled(Rect::from_center_size(pos2(ex, rect.top() + 12.0), vec2(6.0, 16.0)), 1.0, c);
    }
    if hover == Some(Grab::Passband) || dragging == Some(DragKind::Centre) {
        p.rect_filled(
            Rect::from_x_y_ranges(ea.min(eb)..=ea.max(eb), rect.y_range()),
            0.0,
            accent().gamma_multiply(0.10),
        );
    }
    for m in sc.markers.m.iter().flatten() {
        p.add(Shape::dashed_line(
            &[pos2(x(m.hz), rect.top()), pos2(x(m.hz), rect.bottom())],
            Stroke::new(1.0, ink().gamma_multiply(0.5)),
            4.0,
            3.0,
        ));
    }

    // Each visible row's time, newest first, for the slot lines and the
    // UTC ticks: both are placed where the rows cross their time.
    let pts_per_row = px_per_row / ppp;
    let times: Vec<f64> =
        (0..rows_vis.ceil() as u32 + 1).map_while(|age| sc.rows.wf_time(age)).collect();
    let y_of = |t: f64| {
        let age = slotlines::age_at(&times, t)?;
        let y = slotlines::row_y(rect.top(), age, scroll_frac, pts_per_row);
        // On a whole pixel, so a line is one pixel wide.
        let y = (y * ppp).round() / ppp;
        (y > rect.top() + 6.0 && y < rect.bottom() - 4.0).then_some(y)
    };
    let label_x = rect.left() + 13.0;

    // The decoder's slot lines (decision 51): green at each slot's start,
    // with its UTC at the left, and red where its transmissions end, across
    // the whole width; nothing between (AJ3G took the tint out).
    let mut labels: Vec<f32> = Vec::new();
    if sc.settings.slot_lines
        && let (Some(&newest), Some(&oldest)) = (times.first(), times.last())
    {
        let px = 1.0 / ppp;
        for m in sc.slot_lines.marks(oldest, newest) {
            let Some(y) = y_of(m.utc) else { continue };
            // Two pixels, on the rows' edge.
            let band = Rect::from_x_y_ranges(rect.x_range(), y - px..=y + px);
            match m.line {
                slotlines::Line::End => {
                    p.rect_filled(band, 0.0, slotlines::END_INK.gamma_multiply(slotlines::DIM));
                }
                slotlines::Line::Start => {
                    p.rect_filled(band, 0.0, slotlines::START_INK.gamma_multiply(slotlines::DIM));
                    if labels.last().is_none_or(|&l| y - l >= 12.0) {
                        let ink = slotlines::START_INK.gamma_multiply(slotlines::LABEL_DIM);
                        let g = p.layout_no_wrap(fmt_utc(m.utc), mono(9.5), ink);
                        let at = pos2(label_x, y - 2.0 * px - g.size().y);
                        p.rect_filled(
                            Rect::from_min_size(at - vec2(2.0, 0.0), g.size() + vec2(4.0, 0.0)),
                            1.0,
                            ground().gamma_multiply(0.75),
                        );
                        p.galley(at, g, ink);
                        labels.push(y);
                    }
                }
            }
        }
    }

    // UTC time ticks, from each row's own time, where no slot line's label
    // is.
    if sc.settings.wf_time
        && let (Some(&newest), Some(&oldest)) = (times.first(), times.last())
    {
        let rows_per_s = (1.0 / sc.row_interval.max(1e-3)) as f32;
        let every = tick_interval(rows_per_s, pts_per_row, 70.0);
        let mut k = (newest / every).floor();
        while k * every > oldest {
            let t = k * every;
            k -= 1.0;
            let Some(yy) = y_of(t) else { continue };
            if labels.iter().any(|&l| (l - yy).abs() < 12.0) {
                continue;
            }
            p.hline(
                rect.left()..=rect.left() + 10.0,
                yy,
                Stroke::new(1.0, ink().gamma_multiply(0.7)),
            );
            p.text(
                pos2(label_x, yy),
                Align2::LEFT_CENTER,
                fmt_utc(t),
                mono(9.5),
                ink().gamma_multiply(0.8),
            );
        }
    }

    // The skimmer's labels and the offset bar over the slot lines and the
    // ticks, so the lines pass behind the labels as the rows scroll (AJ3G).
    paint_skim_waterfall(sc, ui, &p, rect, cx);
    paint_dx_mark(sc, &p, rect, cx, false);
    paint_offset(sc, &p, rect, cx, false);

    // The colour bar, with the levels it runs between.
    let bar = Rect::from_min_max(
        pos2(gut.left() + 4.0, rect.top() + 4.0),
        pos2(gut.left() + 10.0, rect.bottom() - 4.0),
    );
    let mut mesh = Mesh::default();
    let steps = 32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let [r, g, b] = sc.settings.palette.at(t);
        let c = Color32::from_rgb(r, g, b);
        let yy = bar.bottom() - t * bar.height();
        mesh.colored_vertex(pos2(bar.left(), yy), c);
        mesh.colored_vertex(pos2(bar.right(), yy), c);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    p.add(Shape::mesh(mesh));
    let lv =
        |d: f32| if sc.settings.scale == Scale::SUnits { s_unit(d) } else { format!("{d:.0}") };
    p.text(pos2(bar.right() + 3.0, bar.top()), Align2::LEFT_TOP, lv(ceil), mono(9.0), dim());
    p.text(pos2(bar.right() + 3.0, bar.bottom()), Align2::LEFT_BOTTOM, lv(floor), mono(9.0), dim());
    if sc.settings.wf_auto {
        p.text(
            pos2(bar.right() + 3.0, bar.center().y),
            Align2::LEFT_CENTER,
            "AUTO",
            label(9.0),
            accent(),
        );
    }
}

/// The UTC tick spacing, s: the shortest of 5 s to 10 min that puts ticks at
/// least `min_px` apart at `rows_per_s` rows a second, `pts_per_row` points
/// each.
pub fn tick_interval(rows_per_s: f32, pts_per_row: f32, min_px: f32) -> f64 {
    const CHOICES: [f64; 9] = [5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0];
    let pts_per_s = (rows_per_s * pts_per_row).max(1e-3) as f64;
    CHOICES.iter().copied().find(|s| s * pts_per_s >= min_px as f64).unwrap_or(1800.0)
}

/// UTC seconds as HH:MM:SS.
pub fn fmt_utc(t: f64) -> String {
    let s = (t.rem_euclid(86_400.0)).floor() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// A grip between parts of the TFT: the split (spectrum against waterfall)
/// or the whole height.
fn grip(sc: &mut Scope, ui: &mut egui::Ui, rect: Rect, split: bool) {
    let id = ui.id().with(if split { "scope-grip-split" } else { "scope-grip-height" });
    let resp = ui.interact(rect, id, Sense::drag()).on_hover_cursor(CursorIcon::ResizeVertical);
    let resp = resp.on_hover_text(if split {
        "Drag to share the height between the spectrum and the waterfall"
    } else {
        "Drag to change the scope's height"
    });
    let p = ui.painter_at(rect);
    p.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, edge()));
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, edge()));
    let lit = resp.hovered() || resp.dragged();
    let col = if lit { accent() } else { dim() };
    for dy in [-1.5, 1.5] {
        p.hline(
            rect.center().x - 22.0..=rect.center().x + 22.0,
            rect.center().y + dy,
            Stroke::new(1.0, col),
        );
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let v = if split { sc.settings.split } else { sc.settings.height };
        sc.grip = Some((split, v, pos.y));
    }
    if resp.dragged()
        && let (Some((which, v0, y0)), Some(pos)) = (sc.grip, resp.interact_pointer_pos())
        && which == split
    {
        let dy = pos.y - y0;
        if split {
            let h = sc.settings.height;
            sc.settings.set_split((h * v0 + dy) / h);
        } else {
            sc.settings.set_height(v0 + dy);
        }
    }
    if resp.drag_stopped() {
        sc.grip = None;
    }
}

/// The mouse on the spectrum or the waterfall: the same on both.
fn pointer_on(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    rect: Rect,
    cx: &Ctx,
    out: &mut ScopeOut,
) {
    let v = cx.view;
    let tx = cx.rig.transmitting;
    let step = sc.settings.step_hz as f64;
    let x = |f: f64| v.x_of(f, rect.left(), rect.width());
    let f_at = |px: f32| v.f_at(px, rect.left(), rect.width());
    let rx = cx.rig.rx_hz;
    let (lo_x, hi_x) = (x(rx + cx.rig.passband.0 as f64), x(rx + cx.rig.passband.1 as f64));
    let marker_at = |px: f32| {
        (0..2).find(|&i| sc.markers.m[i].is_some_and(|m| (x(m.hz) - px).abs() < pointer::EDGE_GRAB))
    };
    let (pressed, down, released, hover, shift) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.hover_pos(),
            i.modifiers.shift,
        )
    });
    // Only where the scope itself is under the pointer: a window over it
    // (RADIO's Tune slider, say) keeps its clicks and drags (AJ3G, 8 Nov
    // 2026: dragging the Tune slider moved the scope back and forth).
    let here = hover.filter(|p| rect.contains(*p) && resp.contains_pointer());
    // The skimmer's label, the offset bar's ×, or the bar itself under the
    // pointer: they come before anything under them.
    let label_at = |q: Pos2| sc.skim_boxes.iter().find(|(r, _)| r.contains(q)).map(|&(_, i)| i);
    let close_at = |q: Pos2| sc.offset_close.is_some_and(|r| r.contains(q));
    let bar_at = |px: f32| sc.offset.is_some_and(|b| (x(b.hz) - px).abs() < pointer::EDGE_GRAB);
    // Where the bar goes for the pointer at `px`: onto a labelled station's
    // own frequency when it is within a few pixels of one, so the bar reads
    // exactly the AF DECODE gave it.
    let bar_hz = |px: f32| {
        sc.skim_shown
            .iter()
            .map(|(s, _)| s.hz)
            .filter(|&hz| (x(hz) - px).abs() <= 4.0)
            .min_by(|a, b| (x(*a) - px).abs().total_cmp(&(x(*b) - px).abs()))
            .unwrap_or_else(|| f_at(px))
    };
    let rx_grab = |px: f32| {
        matches!(grab_at(px, x(rx), lo_x, hi_x), Some(Grab::Rx | Grab::EdgeLo | Grab::EdgeHi))
    };

    // Hover over a label, the ×, or the bar: say what it is.
    let mut over_own = false;
    if sc.drag.is_none()
        && sc.marker_drag.is_none()
        && !sc.offset_drag
        && let Some(pos) = here
    {
        if let Some(i) = label_at(pos) {
            let s = &sc.skim_shown[i].0;
            let own = s.row.is_some();
            ui.ctx().set_cursor_icon(if own || !tx {
                CursorIcon::PointingHand
            } else {
                CursorIcon::NotAllowed
            });
            let what = if own {
                "click to pick it in DECODE".to_string()
            } else if tx {
                "tuning locked while transmitting".to_string()
            } else if let Some(k) = s.key {
                format!("click to listen there in {}", k.mode.label())
            } else {
                format!("click to tune to its dial, {}", view::fmt_freq(cx.map.shown(s.dial_hz)))
            };
            sc.cursor = format!("{} · {what}", s.tag());
            let detail = match s.key {
                Some(k) => format!(
                    "{}\n{}{} · {:+.0} dB over the noise · {} · heard {} UTC\n{what}",
                    s.text,
                    k.mode.label(),
                    if k.wpm > 0 { format!(" {} WPM", k.wpm) } else { String::new() },
                    s.snr,
                    fmt_tenth(cx.map.shown(s.hz)),
                    fmt_utc(s.utc),
                ),
                None => format!(
                    "{}\n{:+.0} dB · AF {:.0} Hz on the {} dial · {} · {} UTC\n{what}",
                    s.text,
                    s.snr,
                    s.audio_hz,
                    view::fmt_freq(cx.map.shown(s.dial_hz)),
                    fmt_tenth(cx.map.shown(s.hz)),
                    fmt_utc(s.utc),
                ),
            };
            resp.clone().on_hover_text_at_pointer(detail);
            over_own = true;
        } else if close_at(pos) {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            sc.cursor = "remove the offset bar".into();
            over_own = true;
        } else if bar_at(pos.x) && (tx || !rx_grab(pos.x)) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            sc.cursor = "drag the offset bar · it tunes nothing".into();
            over_own = true;
        }
    }

    // Hover: the cursor shape and what a click would do.
    if sc.drag.is_none()
        && sc.marker_drag.is_none()
        && !sc.offset_drag
        && !over_own
        && let Some(pos) = here
    {
        let g = if tx { None } else { grab_at(pos.x, x(rx), lo_x, hi_x) };
        let m = marker_at(pos.x);
        let icon = match (g, m) {
            (Some(Grab::Rx | Grab::Passband), _) => CursorIcon::Grab,
            (Some(_), _) | (None, Some(_)) => CursorIcon::ResizeHorizontal,
            _ if tx => CursorIcon::NotAllowed,
            _ => CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(icon);
        let shown = cx.map.shown(view::snap(f_at(pos.x), step));
        sc.cursor = if tx {
            "tuning locked while transmitting".into()
        } else {
            format!(
                "{} MHz · {}",
                format_args!("{:.4}", shown / 1e6),
                match (g, m) {
                    (Some(Grab::Rx), _) => "drag to tune",
                    (Some(Grab::Passband), _) => "drag to tune with the passband · click to tune",
                    (Some(_), _) => "drag to set the filter",
                    (None, Some(_)) => "drag the marker",
                    _ =>
                        "click to tune · right-click for a marker · shift-click for the offset bar",
                }
            )
        };
    }

    // Press.
    if pressed && let Some(pos) = here {
        sc.skim_press = None;
        if close_at(pos) {
            // SAFETY INTERLOCK (docs/safety-interlocks.md, #9): the bar is
            // where the radio transmits; locked while it does.
            if !tx {
                sc.offset = None;
            }
        } else if let Some(i) = label_at(pos) {
            sc.skim_press = Some(i);
        } else if !tx && (shift || (bar_at(pos.x) && !rx_grab(pos.x))) {
            // The offset bar: shift drops it here (and drags it on), a
            // press on it drags it. It is the transmit marker (`TxAt`), so
            // not while transmitting (interlock #9).
            sc.offset = Some(OffsetBar { hz: bar_hz(pos.x) });
            sc.offset_drag = true;
            sc.offset_moved = true;
        } else if let Some(i) = marker_at(pos.x).filter(|_| {
            !matches!(
                grab_at(pos.x, x(rx), lo_x, hi_x),
                Some(Grab::Rx | Grab::EdgeLo | Grab::EdgeHi)
            ) || tx
        }) {
            sc.marker_drag = Some(i);
            sc.markers.active = i;
        } else {
            let g = grab_at(pos.x, x(rx), lo_x, hi_x);
            sc.drag = Some(Drag::press(g, pos.x, v, rx, tx));
        }
    }

    // Drag.
    if down && let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
        if sc.offset_drag && !tx {
            sc.offset = Some(OffsetBar { hz: bar_hz(pos.x) });
            sc.offset_moved = true;
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if let Some(i) = sc.marker_drag {
            sc.markers.move_to(i, f_at(pos.x));
        }
        if let Some(d) = sc.drag.as_mut() {
            d.moved_to(pos.x);
            if d.moved {
                match d.kind {
                    // SAFETY INTERLOCK 9: a press made while transmitting is
                    // always a pan (`Drag::press`); one made before keying
                    // stops tuning the moment the radio keys.
                    DragKind::Rx if !tx => {
                        let f = view::snap(cx.map.shown(f_at(pos.x)), step);
                        out.tune_to = Some(cx.map.radio(f));
                        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    }
                    DragKind::Centre if !tx => {
                        let f = view::snap(cx.map.shown(d.centre_to(pos.x, rect.width())), step);
                        out.tune_to = Some(cx.map.radio(f));
                        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    }
                    DragKind::Edge(g) if !tx => {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                        let pb = drag_edge(cx.rig.mode, g, f_at(pos.x) - rx, cx.rig.passband);
                        out.passband = Some(pb);
                        sc.digi = None;
                        sc.cursor = format!("filter {:.2} kHz", (pb.1 - pb.0) / 1000.0);
                    }
                    DragKind::Pan if v.zoomed(cx.span) => {
                        let df = ((pos.x - d.x0) / rect.width()) as f64 * d.start.width();
                        sc.want_view = Some(view::pan(cx.span, d.start, df));
                    }
                    _ => {}
                }
            }
        }
    }

    // Release.
    if released {
        if let Some(d) = sc.drag.take()
            && d.is_click_to_tune(tx)
            && let Some(pos) = here
        {
            let f = view::snap(cx.map.shown(f_at(pos.x)), step);
            out.tune_to = Some(cx.map.radio(f));
        }
        sc.marker_drag = None;
        sc.offset_drag = false;
        // A click on a skimmer label: DECODE's own picks its line there;
        // the wide skimmer's tunes the receiver to the dial it was heard on;
        // the keyboard skimmer's sets the receiver's mode and tunes onto the
        // signal, and DECODE to its mode (decision 56). (Only where the
        // press was: the spectrum and the waterfall each see the release.)
        if let Some(pos) = here
            && let Some(i) = sc.skim_press.take().filter(|&i| label_at(pos) == Some(i))
        {
            let s = &sc.skim_shown[i].0;
            match (s.row, s.key) {
                (Some(id), _) => out.skim_pick = Some(id),
                // SAFETY INTERLOCK (docs/safety-interlocks.md, #9): a label
                // tunes nothing, and changes no mode, while transmitting.
                (None, _) if tx => {}
                (None, Some(k)) => {
                    let (mode, pb, dial) = skim::key_tune(s.hz, k.mode, sc.skim.key_audio_hz);
                    out.mode = Some(mode);
                    out.passband = Some(pb);
                    out.tune_to = Some(dial);
                    out.key_pick = Some(k.mode);
                }
                (None, None) => {
                    out.tune_to = Some(s.dial_hz);
                    out.wide_pick = s.wsjt;
                }
            }
        }
    }

    if resp.double_clicked() {
        sc.want_view = None;
    }
    if resp.secondary_clicked()
        && let Some(pos) = here
    {
        sc.markers.place(f_at(pos.x));
    }

    // The wheel: a step a notch, or with ctrl a zoom about the pointer.
    if let Some(pos) = here {
        let events: Vec<(bool, f32, bool)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, modifiers, .. } => Some((
                        *unit == egui::MouseWheelUnit::Line,
                        delta.y,
                        modifiers.ctrl || modifiers.command,
                    )),
                    _ => None,
                })
                .collect()
        });
        for (lines, dy, zoom) in events {
            let n = sc.wheel.notches(lines, dy);
            if n == 0 {
                continue;
            }
            if zoom {
                sc.want_view = Some(view::zoom_at(cx.span, v, f_at(pos.x), wheel_zoom(n)));
            } else if !tx {
                // SAFETY INTERLOCK 9: no stepping while transmitting.
                let base = out.tune_to.unwrap_or(rx);
                let shown = view::snap(cx.map.shown(base), step) + n as f64 * step;
                out.tune_to = Some(cx.map.radio(shown));
            }
        }
        // The wheel belongs to the scope here, not to a scroll area round it.
        ui.input_mut(|i| {
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
    }
}

/// The bar across the top: stream state, span and centre, any transverter
/// offset, and what the pointer is over.
fn top_bar(sc: &Scope, ui: &egui::Ui, r: Rect, cx: &Ctx, snap: &sdr::Snapshot) {
    let p = ui.painter_at(r);
    p.hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, edge()));
    let state = if sc.paused {
        "PAUSED"
    } else if sc.held && sc.tx_live {
        // Decision 83: what we send, analysed as it goes out.
        "TX MONITOR"
    } else if sc.held {
        // Decision 74: the band as last received, dimmed.
        "TX · RX DIMMED"
    } else if !snap.alive {
        "NO STREAM"
    } else if sc.shown().is_none() {
        "WAITING"
    } else {
        "STREAMING"
    };
    let left = pos2(r.left() + 10.0, r.center().y);
    p.text(left, Align2::LEFT_CENTER, format!("IQ · {state}"), label(12.0), ink());
    let z = cx.view.zoom(cx.span);
    let zoom = if z > 1.01 {
        format!(" · ZOOM {}×", if z < 10.0 { format!("{z:.1}") } else { format!("{z:.0}") })
    } else {
        String::new()
    };
    let mid = format!(
        "SPAN {} kHz · CENTRE {:.3} MHz{zoom}",
        (cx.span.width / 1000.0).round(),
        cx.map.shown(cx.span.center) / 1e6
    );
    p.text(r.center(), Align2::CENTER_CENTER, mid, label(12.0), dim());
    let mut right_x = r.right() - 10.0;
    if cx.map.transverter_hz != 0.0 || cx.map.ppm != 0.0 {
        // Said loudly: every frequency on screen is not the radio's own.
        let mut t = String::new();
        if cx.map.transverter_hz != 0.0 {
            t.push_str(&format!(
                "XVTR {}{}",
                if cx.map.transverter_hz > 0.0 { "+" } else { "" },
                sc.xvtr_text
            ));
        }
        if cx.map.ppm != 0.0 {
            if !t.is_empty() {
                t.push_str(" · ");
            }
            t.push_str(&format!("CAL {:+.3} ppm", cx.map.ppm));
        }
        let g = p.layout_no_wrap(t, label(11.5), theme::INK_ON_CYAN());
        let box_r = Rect::from_min_size(
            pos2(right_x - g.size().x - 10.0, r.top() + 3.0),
            vec2(g.size().x + 10.0, r.height() - 6.0),
        );
        p.rect_filled(box_r, 2.0, accent());
        p.galley(
            pos2(box_r.left() + 5.0, r.center().y - g.size().y / 2.0),
            g,
            theme::INK_ON_CYAN(),
        );
        right_x = box_r.left() - 10.0;
    }
    let cursor = if sc.cursor.is_empty() { "click to tune" } else { &sc.cursor };
    p.text(pos2(right_x, r.center().y), Align2::RIGHT_CENTER, cursor, label(11.5), dim());
    if cx.rig.transmitting {
        p.rect_stroke(r.shrink(1.0), 2.0, Stroke::new(1.0, accent()), egui::StrokeKind::Inside);
    }
}

/// The two status lines under the TFT, in the DUO-ART's blue and its
/// diagnostic line.
fn status_bars(sc: &Scope, ui: &egui::Ui, a: Rect, b: Rect, cx: &Ctx, snap: &sdr::Snapshot) {
    let p = ui.painter_at(a.union(b));
    p.hline(a.x_range(), a.top() + 0.5, Stroke::new(1.0, edge()));
    p.hline(b.x_range(), b.top() + 0.5, Stroke::new(1.0, edge()));
    let s = &sc.settings;
    let f = sc.shown();
    let rbw = f.map_or(cx.span.width / s.fft_size as f64 * 2.0, |f| f.bin_width() * f.enbw as f64);
    let lv = |d: f32| match s.scale {
        Scale::SUnits => s_unit(d),
        Scale::Both => format!("{d:.0} dBm ({})", s_unit(d)),
        Scale::Dbm => format!("{d:.0} dBm"),
    };
    let fps = sc.frame_times.len();
    let mut parts = vec![
        format!("{} kS/s", (cx.span.width / 1000.0).round()),
        format!("FFT {} · {} · OVL {}", fft_label(s.fft_size), s.window.label(), s.overlap.label()),
        format!("RBW {}", fmt_hz(rbw)),
        format!(
            "FLOOR {} · CEIL {}{}",
            lv(s.floor_dbm.round()),
            lv(s.ceil_dbm.round()),
            if s.fit { " · FIT" } else { "" }
        ),
        if s.avg { "AVG 4".into() } else { "AVG OFF".into() },
        format!("{fps} fps"),
    ];
    if sc.gpu_fft() {
        parts.push(format!("FFT ON GPU · IQ {:.1} MB/s", sc.gpu_iq_bytes_per_sec() / 1e6));
    }
    if sc.zoom.is_some() {
        parts.push("ZOOM FFT".into());
    }
    if s.persist {
        parts.push("PERSIST".into());
    }
    p.text(
        pos2(a.left() + 10.0, a.center().y),
        Align2::LEFT_CENTER,
        parts.join("   "),
        label(11.5),
        accent(),
    );

    let notice = sc.notice.as_ref().filter(|(_, until)| cx.now < *until).map(|(n, _)| n);
    let diag = if sc.paused {
        "Display paused · stream still running".to_string()
    } else if sc.held && sc.tx_live {
        "Transmitting: the TX monitor shows the audio we send, where it lands · receive starts \
         afresh after the over"
            .to_string()
    } else if sc.held {
        "Transmitting something the panel did not send: the band as last received, dimmed · \
         receive starts afresh after the over"
            .to_string()
    } else if let Some(n) = notice {
        n.clone()
    } else if !snap.alive {
        "IQ stream stopped".to_string()
    } else {
        // FFT % is the CPU spectrum thread's share of a core. With the
        // spectrum on the GPU that thread is idle, and the figure it left
        // read as the GPU's, frozen (AJ3G, 8 Nov 2026): say where the FFT
        // runs instead. Its frame rate is not repeated here: the GPU's load
        // is the figure (AJ3G, 11 Nov 2026).
        let fft = if sc.gpu_fft() {
            "FFT on the GPU".to_string()
        } else {
            format!("FFT {:.0} % (CPU)", snap.spectrum_load * 100.0)
        };
        // The graphics card's load from this program, all of it (FFT,
        // waterfall, drawing), as Task Manager's GPU column reads it.
        let gpu = match (sc.gpu, sc.gpu_load.percent()) {
            (Some(g), Some(p)) => format!(" · GPU {p:.0} % ({})", g.adapter),
            (Some(g), None) => format!(" · GPU {}", g.adapter),
            (None, _) => String::new(),
        };
        format!(
            "IQ stream OK · {} spectrum drops · DSP {:.0} % (CPU) · {fft}{gpu}",
            snap.spectrum_drops,
            snap.load * 100.0,
        )
    };
    let ok = theme::scope().good;
    p.text(pos2(b.left() + 10.0, b.center().y), Align2::LEFT_CENTER, diag, label(11.5), ok);
    // Above 192 kHz the DUO shows REM and mutes its own audio (SDR Console's
    // ELAD notes): the panel's receiver is what is heard.
    let msg = if cx.span.width > 192_000.0 {
        "DUO on REM · receive audio from the panel"
    } else {
        "Receive audio from the panel"
    };
    p.text(pos2(b.right() - 10.0, b.center().y), Align2::RIGHT_CENTER, msg, label(11.5), ok);
}

/// An FFT size as it is said: 65 536 is "64k", 1 048 576 is "1M".
pub fn fft_label(n: usize) -> String {
    if n >= 1 << 20 { format!("{}M", n >> 20) } else { format!("{}k", n >> 10) }
}

impl Scope {
    /// The waterfall's levels: its own, following the noise, or the
    /// spectrum's.
    /// The waterfall's levels as they stand, without stepping auto levels
    /// on (that is [`Scope::wf_range`]'s, once a frame).
    pub(super) fn wf_levels_now(&self) -> (f32, f32) {
        let set = (self.settings.floor_dbm, self.settings.ceil_dbm);
        if self.settings.wf_auto { self.wf_levels.unwrap_or(set) } else { set }
    }

    /// The made-up noise where the waterfall or the surface has no history
    /// (decision 112), for rows numbered up to `serial`: the band's quiet
    /// level and spread from the trace ([`levels::noise_fill`]), eased over
    /// half a second so the fill does not flicker with each frame's median,
    /// and held as it was over a transmission (the TX monitor's trace is our
    /// signal on a flat floor, nothing to measure noise from).
    pub(super) fn noise_params(&mut self, now: f64, serial: u32) -> gpu::NoiseParams {
        let measured = if self.held { None } else { levels::noise_fill(&self.cols) };
        if let Some(m) = measured {
            let dt = if self.noise_fill_at.is_finite() {
                (now - self.noise_fill_at).clamp(0.0, 1.0) as f32
            } else {
                1.0
            };
            let k = 1.0 - (-dt / 0.5).exp();
            let cur = self.noise_fill.unwrap_or(m);
            self.noise_fill = Some(levels::NoiseFill {
                dbm: cur.dbm + (m.dbm - cur.dbm) * k,
                spread_db: cur.spread_db + (m.spread_db - cur.spread_db) * k,
            });
        }
        self.noise_fill_at = now;
        gpu::NoiseParams { fill: self.noise_fill, serial }
    }

    pub(super) fn wf_range(&mut self) -> (f32, f32) {
        if !self.settings.wf_auto {
            return (self.settings.floor_dbm, self.settings.ceil_dbm);
        }
        let Some(f) = self.shown() else {
            return (self.settings.floor_dbm, self.settings.ceil_dbm);
        };
        // Over a transmission the colours stay as the band had them: the TX
        // monitor's rows are coloured as received ones would be.
        if let Some(l) = self.wf_levels.filter(|_| self.held) {
            return l;
        }
        let strongest = self.cols.iter().copied().fold(f.noise_dbm, f32::max);
        let target = levels::wf_auto_levels(f.noise_dbm, strongest);
        let cur = self.wf_levels.unwrap_or(target);
        let next = levels::fit_step(cur, target);
        self.wf_levels = Some(next);
        next
    }

    /// The receiver's SNR, from its level and the noise floor the spectrum
    /// measures across its passband.
    pub(super) fn snr(&self, snap: &sdr::Snapshot, rig: &ScopeRig) -> Option<f32> {
        let f = self.shown()?;
        let bw = (rig.passband.1 - rig.passband.0).abs() as f64;
        Some(levels::snr_db(snap.level_dbm, f.noise_dbm, f.bin_width(), f.enbw, bw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hover_reads_the_audio_offset_inside_the_passband_only() {
        let usb = ScopeRig {
            rx_hz: 14_095_600.0,
            mode: sdr::Mode::Usb,
            passband: (200.0, 3000.0),
            transmitting: false,
            on_air: false,
        };
        assert_eq!(hover_af(14_097_150.0, &usb), Some(1550.0));
        assert_eq!(hover_af(14_095_700.0, &usb), None, "below the passband");
        assert_eq!(hover_af(14_099_000.0, &usb), None, "above it");
        // LSB: the audio rises as the signal goes down, as DECODE has it.
        let lsb = ScopeRig {
            rx_hz: 7_175_000.0,
            mode: sdr::Mode::Lsb,
            passband: (-2900.0, -100.0),
            ..usb
        };
        assert_eq!(hover_af(7_173_500.0, &lsb), Some(1500.0));
        // No dial known: no offset.
        assert_eq!(hover_af(1550.0, &ScopeRig { rx_hz: 0.0, ..usb }), None);
    }

    #[test]
    fn the_tx_trace_takes_the_waterfalls_palette_by_level() {
        let tp = TxPalette {
            lut: super::super::palette::Palette::Icom.lut(),
            floor: -120.0,
            ceil: -20.0,
            contrast: 1.0,
            brightness: 0.0,
        };
        // Full scale: the palette's top (Icom's red); the floor is lifted
        // so the line still shows.
        assert_eq!(tp.at(-20.0), Color32::from_rgb(230, 20, 20));
        assert_eq!(tp.t(-200.0), TX_TRACE_MIN_T);
        assert!((tp.t(-50.0) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn the_passband_width_reads_as_a_filter() {
        assert_eq!(bw_text((200.0, 3000.0)), "BW 2.80 kHz");
        assert_eq!(bw_text((-250.0, 250.0)), "BW 500 Hz");
        assert_eq!(bw_text((-1270.0, 4250.0)), "BW 5.52 kHz");
    }

    #[test]
    fn utc_ticks_are_spaced_to_be_read() {
        // 60 rows a second, a point each: 5 s is 300 points apart.
        assert_eq!(tick_interval(60.0, 1.0, 70.0), 5.0);
        // 7.5 rows a second, a point each: 10 s is 75 points.
        assert_eq!(tick_interval(7.5, 1.0, 70.0), 10.0);
        assert_eq!(tick_interval(0.001, 1.0, 70.0), 1800.0);
        assert_eq!(fmt_utc(3_600.0 * 25.0 + 61.9), "01:01:01");
        assert_eq!(fmt_utc(0.0), "00:00:00");
    }

    #[test]
    fn fft_sizes_are_said_short() {
        assert_eq!(fft_label(65_536), "64k");
        assert_eq!(fft_label(4096), "4k");
        assert_eq!(fft_label(1 << 20), "1M");
    }

    #[test]
    fn columns_cover_only_what_the_frame_holds() {
        let f = sdr::SpectrumFrame {
            center_hz: 1000.0,
            span_hz: 1000.0,
            db: (0..1000).map(|i| i as f32).collect(),
            peak: None,
            seq: 1,
            enbw: 1.0,
            noise_dbm: 0.0,
        };
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 10.0));
        // The view runs past the top of the frame: half the columns.
        let (c, x) = columns(&f, View { lo: 1000.0, hi: 2000.0 }, rect, 100, false);
        assert_eq!(c.len(), 50);
        assert!(x.iter().all(|&x| x <= 50.0));
        assert_eq!(*c.last().unwrap(), 999.0);
        let (c, _) = columns(&f, View { lo: 3000.0, hi: 4000.0 }, rect, 100, false);
        assert!(c.is_empty());
    }
}
