//! The TFT: its top bar, the spectrum (flat, 3D or both), the band plan
//! strip, the waterfall, the grips between them and the status lines, and
//! what the mouse does on them.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2,
};

use super::bandplan;
use super::gpu::{self, SurfaceParams, WfParams};
use super::levels::{self, level_text, s_unit};
use super::markers::{fmt_delta, fmt_tenth};
use super::pointer::{self, Drag, DragKind, Grab, drag_edge, grab_at, wheel_zoom};
use super::rows::Upload;
use super::settings::{Scale, TopView};
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
const BAND_STRIP: f32 = 18.0;
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

    // Which callback carries this frame's rows: the first one on screen.
    let mut upload = upload;
    let surface_h = match sc.settings.top {
        TopView::Spectrum => 0.0,
        TopView::Surface => spec_plot.height(),
        TopView::Both => (spec_plot.height() * 0.55).round(),
    };
    let surface = Rect::from_min_size(spec_plot.min, vec2(spec_plot.width(), surface_h));
    let trace = Rect::from_min_max(pos2(spec_plot.left(), surface.bottom()), spec_plot.max);

    if surface_h > 0.0 {
        paint_surface(sc, ui, surface, trace.height() < 1.0, cx, &mut upload);
    }
    paint_spectrum(sc, ui, spec_plot, trace, spec_gut, cx, snap);
    if bp_h > 0.0 {
        paint_band_plan(sc, ui, bp_plot, bp_gut, cx, out);
    }
    paint_waterfall(sc, ui, wf_plot, wf_gut, cx, upload);
    grip(sc, ui, split_grip, true);
    grip(sc, ui, height_grip, false);

    // The mouse on the spectrum and the waterfall.
    let spec_resp = ui.interact(spec_plot, ui.id().with("scope-spec"), Sense::click_and_drag());
    pointer_on(sc, ui, &spec_resp, spec_plot, true, cx, out);
    let wf_resp = ui.interact(wf_plot, ui.id().with("scope-wf"), Sense::click_and_drag());
    pointer_on(sc, ui, &wf_resp, wf_plot, false, cx, out);

    top_bar(sc, ui, tbar, cx, snap);
    status_bars(sc, ui, status, diag, cx, snap);
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
fn paint_spectrum(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    all: Rect,
    trace: Rect,
    gut: Rect,
    cx: &Ctx,
    snap: &sdr::Snapshot,
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

    // The receiver's passband, and its edges as handles.
    let rx = cx.rig.rx_hz;
    let (pa, pb) = (x(rx + cx.rig.passband.0 as f64), x(rx + cx.rig.passband.1 as f64));
    p.rect_filled(
        Rect::from_x_y_ranges(pa.min(pb)..=pa.max(pb).max(pa.min(pb) + 2.0), all.y_range()),
        0.0,
        accent().gamma_multiply(0.18),
    );
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
    if has_trace && let Some(f) = &frame {
        let ncols = ((trace.width() * ppp) as usize).max(16);
        let (cols, xs) = columns(f, v, trace, ncols, false);
        sc.cols.clone_from(&cols);
        if sc.settings.persist {
            paint_persistence(sc, ui, trace, &cols, cx.now);
        }
        // Fill, then the line.
        if !cols.is_empty() {
            let top_a = 0.45;
            let mut mesh = Mesh::default();
            for (i, (&c, &xx)) in cols.iter().zip(&xs).enumerate() {
                let yy = y(c).clamp(trace.top(), trace.bottom());
                let t = 1.0 - (yy - trace.top()) / trace.height().max(1.0);
                let a = 0.03 + (top_a - 0.03) * t;
                mesh.colored_vertex(pos2(xx, yy), accent().gamma_multiply(a));
                mesh.colored_vertex(pos2(xx, trace.bottom()), accent().gamma_multiply(0.03));
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
            p.add(Shape::line(line, Stroke::new(1.2, accent().gamma_multiply(a))));
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
        // The noise floor estimate.
        if sc.settings.noise_line && f.noise_dbm > floor && f.noise_dbm < ceil {
            let yy = y(f.noise_dbm);
            p.add(Shape::dashed_line(
                &[pos2(trace.left(), yy), pos2(trace.right(), yy)],
                Stroke::new(1.0, ink().gamma_multiply(0.45)),
                6.0,
                5.0,
            ));
            let rbw = f.bin_width() * f.enbw as f64;
            p.text(
                pos2(trace.right() - 4.0, (yy - 2.0).min(all.bottom() - 16.0)),
                Align2::RIGHT_BOTTOM,
                format!("NF {:.0} dBm/bin · RBW {}", f.noise_dbm, fmt_hz(rbw)),
                mono(9.5),
                dim(),
            );
        }
    }

    // Markers and their readout.
    if let Some(f) = &frame {
        let window = (v.width() / 200.0).max(f.bin_width() * 3.0);
        sc.markers.track(f, window);
        paint_markers(sc, &p, all, trace, cx, f);
    }

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
    let lab =
        format!("RX {} {} · {lvl}{snr}", view::fmt_freq(cx.map.shown(rx)), cx.rig.mode.label());
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
    if sc.settings.fit && cx.now - sc.fit_at > 0.5 && !sc.cols.is_empty() {
        sc.fit_at = cx.now;
        if let Some(t) = levels::fit_target(&sc.cols) {
            let (a, b) = levels::fit_step((sc.settings.floor_dbm, sc.settings.ceil_dbm), t);
            sc.settings.floor_dbm = a;
            sc.settings.ceil_dbm = b;
        }
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
        let r = sc.markers.read(i, f);
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
    if let Some((df, dl)) = sc.markers.delta(f) {
        lines.push(format!("Δ {}  {:+.1} dB", fmt_delta(cx.map.width(df)), dl));
    }
    let mut yy = all.top() + 22.0;
    for l in lines {
        p.text(pos2(all.left() + 6.0, yy), Align2::LEFT_TOP, l, mono(10.5), ink());
        yy += 14.0;
    }
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
    let ink3 = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
    let params = SurfaceParams {
        view_lo: (v.lo - reference) as f32,
        view_hi: (v.hi - reference) as f32,
        floor: sc.settings.floor_dbm,
        ceil: sc.settings.ceil_dbm,
        live: sc.rows.surface_head(),
        count: sc.rows.surface_count(),
        carry: sc.rows.surface_carry(),
        cols,
        solid: sc.settings.solid_3d,
        bg: ink3(ground()),
        ink: ink3(accent()),
    };
    let cb = gpu::SurfacePaint {
        id: sc.id,
        tex_w: g.tex_w,
        upload: upload.take(),
        palette: sc.settings.palette,
        params,
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

/// The band plan strip: segments, their names where they fit, and the
/// data-mode calling frequencies as ticks.
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
    let inner = rect.shrink2(vec2(0.0, 2.0));
    for (a, b, u) in bandplan::segments_in(slo, shi) {
        let (xa, xb) = (x(a * 1000.0), x(b * 1000.0));
        let (ca, cb) = (xa.max(rect.left()), xb.min(rect.right()));
        let [r, g, bl] = u.rgb();
        let fill = theme::data_ink((r, g, bl));
        p.rect_filled(
            Rect::from_x_y_ranges(ca..=(cb - 1.0).max(ca + 1.0), inner.y_range()),
            0.0,
            fill,
        );
        let name = u.short();
        let gal =
            p.layout_no_wrap(name.to_string(), label(10.0), Color32::from_rgb(0xee, 0xf3, 0xf8));
        if cb - ca > gal.size().x + 8.0 {
            let pos =
                pos2((ca + cb) / 2.0 - gal.size().x / 2.0, rect.center().y - gal.size().y / 2.0);
            p.galley(pos, gal, Color32::WHITE);
        }
    }
    let narrow = cx.map.width(v.width()) < 60_000.0;
    for (hz, modes) in bandplan::data_marks(slo, shi) {
        let xx = x(hz as f64).round();
        p.vline(xx, rect.y_range(), Stroke::new(1.0, ink()));
        if narrow {
            let names: Vec<&str> = modes.iter().map(|m| m.label()).collect();
            p.text(
                pos2(xx + 3.0, rect.center().y),
                Align2::LEFT_CENTER,
                names.join("/"),
                label(9.5),
                ink(),
            );
        }
    }
    p.text(gut.center(), Align2::CENTER_CENTER, "R2", label(10.0), dim());

    let resp = ui.interact(rect, ui.id().with("scope-bp"), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if let Some(pos) = resp.hover_pos() {
        let f = v.f_at(pos.x, rect.left(), rect.width());
        let tol = cx.map.width(v.width()) / 150.0;
        let shown = cx.map.shown(f);
        let dm: Vec<&str> = bandplan::data_marks(shown - tol, shown + tol)
            .values()
            .flatten()
            .map(|m| m.label())
            .collect();
        sc.cursor = format!(
            "{}{}",
            bandplan::describe(shown),
            if dm.is_empty() { String::new() } else { format!(" · {}", dm.join(", ")) }
        );
        if resp.clicked() && !cx.rig.transmitting {
            // SAFETY INTERLOCK 9: nothing tunes while transmitting.
            out.tune_to = Some(cx.map.radio(view::snap(shown, sc.settings.step_hz as f64)));
        }
    }
}

/// The waterfall on the GPU, with the receiver, the markers, the UTC ticks
/// and the colour bar over it.
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

    // The receiver and its passband, faintly.
    let x = |f: f64| v.x_of(f, rect.left(), rect.width());
    let rx = cx.rig.rx_hz;
    p.vline(x(rx).round() + 0.5, rect.y_range(), Stroke::new(1.0, ink().gamma_multiply(0.6)));
    for e in [cx.rig.passband.0, cx.rig.passband.1] {
        p.vline(x(rx + e as f64), rect.y_range(), Stroke::new(1.0, accent().gamma_multiply(0.45)));
    }
    for m in sc.markers.m.iter().flatten() {
        p.add(Shape::dashed_line(
            &[pos2(x(m.hz), rect.top()), pos2(x(m.hz), rect.bottom())],
            Stroke::new(1.0, ink().gamma_multiply(0.5)),
            4.0,
            3.0,
        ));
    }

    // UTC time ticks, from each row's own time.
    if sc.settings.wf_time {
        let rows_per_s = (1.0 / sc.row_interval.max(1e-3)) as f32;
        let every = tick_interval(rows_per_s, px_per_row / ppp, 70.0);
        let mut last: Option<i64> = None;
        let n = rows_vis.ceil() as u32;
        for age in 0..n {
            let Some(t) = sc.rows.wf_time(age) else { break };
            let slot = (t / every).floor() as i64;
            if let Some(l) = last
                && l != slot
            {
                let yy = rect.top() + (age as f32 - (1.0 - scroll_frac)) * px_per_row / ppp;
                if yy > rect.top() + 6.0 && yy < rect.bottom() - 4.0 {
                    p.hline(
                        rect.left()..=rect.left() + 10.0,
                        yy,
                        Stroke::new(1.0, ink().gamma_multiply(0.7)),
                    );
                    p.text(
                        pos2(rect.left() + 13.0, yy),
                        Align2::LEFT_CENTER,
                        fmt_utc(l as f64 * every + every),
                        mono(9.5),
                        ink().gamma_multiply(0.8),
                    );
                }
            }
            last = Some(slot);
        }
    }

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

/// The mouse on the spectrum (`edges`: the filter edges can be grabbed there)
/// or the waterfall.
fn pointer_on(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    rect: Rect,
    edges: bool,
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
    let (pressed, down, released, hover) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.hover_pos(),
        )
    });
    let here = hover.filter(|p| rect.contains(*p));

    // Hover: the cursor shape and what a click would do.
    if sc.drag.is_none()
        && sc.marker_drag.is_none()
        && let Some(pos) = here
    {
        let g = if tx { None } else { grab_at(pos.x, x(rx), lo_x, hi_x, edges) };
        let m = marker_at(pos.x);
        let icon = match (g, m) {
            (Some(Grab::Rx), _) => CursorIcon::Grab,
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
                    (Some(_), _) => "drag to set the filter",
                    (None, Some(_)) => "drag the marker",
                    _ => "click to tune · right-click for a marker",
                }
            )
        };
    }

    // Press.
    if pressed && let Some(pos) = here {
        if let Some(i) = marker_at(pos.x).filter(|_| {
            !matches!(
                grab_at(pos.x, x(rx), lo_x, hi_x, edges),
                Some(Grab::Rx) | Some(Grab::EdgeLo) | Some(Grab::EdgeHi)
            ) || tx
        }) {
            sc.marker_drag = Some(i);
            sc.markers.active = i;
        } else {
            let g = grab_at(pos.x, x(rx), lo_x, hi_x, edges);
            sc.drag = Some(Drag::press(g, pos.x, v, tx));
        }
    }

    // Drag.
    if down && let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
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
                    DragKind::Edge(g) if !tx => {
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
    } else if !snap.alive {
        "NO STREAM"
    } else if snap.latest.is_none() {
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

    let diag = if sc.paused {
        "Display paused · stream still running".to_string()
    } else if !snap.alive {
        "IQ stream stopped".to_string()
    } else {
        format!(
            "IQ stream OK · {} spectrum drops · DSP {:.0} % · FFT {:.0} %{}",
            snap.spectrum_drops,
            snap.load * 100.0,
            snap.spectrum_load * 100.0,
            sc.gpu.map_or(String::new(), |g| format!(" · GPU {}", g.adapter)),
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
    pub(super) fn wf_range(&mut self) -> (f32, f32) {
        if !self.settings.wf_auto {
            return (self.settings.floor_dbm, self.settings.ceil_dbm);
        }
        let Some(f) = self.shown() else {
            return (self.settings.floor_dbm, self.settings.ceil_dbm);
        };
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
