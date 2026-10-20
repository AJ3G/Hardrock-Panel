//! The AMP window as the screen of ELAD's DUO-ART 120 amplifier, beside an
//! ELAD radio: a TFT behind a black bezel in a black case, banded as the
//! DUO-ART's screen is. Top to bottom: a title bar; the meters, as white
//! analog-style faces under their TOP and BOTTOM pickers; the readings;
//! PERFORMANCE; the tuner's network; a status line in blue; a diagnostic
//! line in green; and a message line, warnings on the left and the tuner on
//! the right. The amplifier's controls are the six soft keys under the
//! screen.
//!
//! Everything shown and every control comes from the same place as in the
//! panel's own look ([`Shown`], [`keys`], [`tiles`]), interlocks included;
//! only the drawing is this look's own.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Galley, Mesh, Rect, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use hardrock::{AmpAction, AmpGauge, AmpLevel, amp_band_label};

use super::amptheme::AmpTheme;
use super::{
    AmpMeters, Dial, Key, Keys, NetInk, PanelOut, PickInk, PowerRange, Shown, Tile, Tone, Zones,
    atu_bypassed, atu_words, bypass_drawing, keys, lc_network, legend_size, link_state,
    net_caption, pickers, pop_words, tiles, unsure_warning,
};

/// The case's and the screen's lettering.
fn label_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("chakra-bold".into()))
}

/// Text laid out on one line, with letters spaced as on a silk screen.
fn text(ui: &Ui, t: &str, font: FontId, col: Color32, spacing: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        t,
        0.0,
        egui::TextFormat {
            font_id: font,
            color: col,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
}

/// A tone in this look's colours. Plain is the screen's own ink.
fn tone_col(th: &AmpTheme, t: Tone) -> Color32 {
    match t {
        Tone::Plain => th.ink,
        Tone::Blue => th.status,
        Tone::Green => th.diag,
        Tone::Yellow => th.warn,
        Tone::Red => th.alarm,
    }
}

/// A level of the amplifier's in this look's colours: green is the screen's
/// ordinary ink, so only a warning stands out.
fn level_ink(th: &AmpTheme, l: Option<AmpLevel>) -> Color32 {
    match l {
        Some(AmpLevel::Yellow) => th.warn,
        Some(AmpLevel::Red) => th.alarm,
        _ => th.ink,
    }
}

/// A vertical gradient from `top` to `bottom` over `r`.
fn gradient(p: &egui::Painter, r: Rect, top: Color32, bottom: Color32) {
    let mut m = Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.left_bottom(), bottom);
    m.colored_vertex(r.right_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 2, 3);
    p.add(Shape::mesh(m));
}

/// The pickers' combo boxes, their lists and the hover texts in the
/// screen's colours.
fn screen_visuals(ui: &mut Ui, th: &AmpTheme) {
    let v = ui.visuals_mut();
    v.window_fill = th.screen;
    v.panel_fill = th.screen;
    v.extreme_bg_color = th.screen;
    v.window_stroke = Stroke::new(1.0, th.screen_line);
    for (w, edge) in [
        (&mut v.widgets.inactive, th.screen_line),
        (&mut v.widgets.hovered, th.status),
        (&mut v.widgets.active, th.status),
        (&mut v.widgets.open, th.status),
    ] {
        w.weak_bg_fill = th.screen;
        w.bg_fill = th.screen;
        w.bg_stroke = Stroke::new(1.0, edge);
        w.fg_stroke = Stroke::new(1.0, th.ink);
    }
    v.selection.bg_fill = th.status.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, th.ink);
}

/// One band of the screen, `h` high and the screen's width, with the line
/// under it that divides it from the next.
fn band(ui: &mut Ui, th: &AmpTheme, h: f32) -> Rect {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, th.screen_line));
    rect
}

/// The window's contents in this look.
pub(super) fn screen(
    ui: &mut Ui,
    sh: &Shown,
    view: &mut AmpMeters,
    th: &AmpTheme,
    pop: Option<bool>,
    out: &mut PanelOut,
) {
    let s = sh.s;
    let keys = keys(sh);
    let m = |v: f32| (v * s).round().clamp(0.0, 127.0) as i8;
    let case = egui::Frame::new()
        .fill(th.case)
        .stroke(Stroke::new(1.0, th.case_line))
        .corner_radius(CornerRadius::same(m(10.0).max(0) as u8))
        .inner_margin(egui::Margin::symmetric(m(16.0), m(14.0)));
    case.show(ui, |ui| {
        // The case's top catches the light.
        let mut sheen = ui.max_rect().expand2(vec2(16.0 * s, 14.0 * s));
        sheen.set_height(28.0 * s);
        gradient(&ui.painter().clone(), sheen.shrink(1.0), th.case_hi, th.case);

        silk(ui, sh, th, pop, out);
        ui.add_space(8.0 * s);
        egui::Frame::new()
            .fill(th.bezel)
            .corner_radius(CornerRadius::same(m(6.0).max(0) as u8))
            .inner_margin(m(10.0))
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(th.screen)
                    .stroke(Stroke::new(1.0, th.screen_line))
                    .corner_radius(CornerRadius::same(2))
                    .show(ui, |ui| {
                        screen_visuals(ui, th);
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        title_bar(ui, sh, th);
                        pick_row(ui, view, th, s);
                        meters(ui, sh, view, th);
                        if tile_row(ui, sh, th) {
                            out.actions.push(AmpAction::SetCelsius(sh.r.celsius == Some(false)));
                        }
                        performance(ui, sh, view, th);
                        tuner(ui, sh, th);
                        status_bar(ui, sh, view, th);
                        diag_bar(ui, sh, th);
                        message_line(ui, sh, th);
                    });
            });
        ui.add_space(10.0 * s);
        soft_keys(ui, th, &keys, s, &mut out.actions);
    });
}

/// The lettering on the case above the screen: the amplifier's name, what
/// it is, and POP OUT or DOCK where the window can be popped out.
fn silk(ui: &mut Ui, sh: &Shown, th: &AmpTheme, pop: Option<bool>, out: &mut PanelOut) {
    let s = sh.s;
    ui.horizontal(|ui| {
        let brand = sh.st.model.label().split(' ').next().unwrap_or("").to_uppercase();
        let g = text(ui, &brand, label_font(15.0 * s), th.label, 3.3 * s);
        let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(rect.min, g, th.label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(popped) = pop {
                let (t, hover) = pop_words(popped);
                if super::button(ui, s, false, t, None).on_hover_text(hover).clicked() {
                    out.pop = true;
                }
                ui.add_space(8.0 * s);
            }
            let what = if sh.st.atu.is_some() { "AMPLIFIER · ATU-500" } else { "AMPLIFIER" };
            let g = text(ui, what, label_font(11.0 * s), th.ink_dim, 1.5 * s);
            let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(rect.min, g, th.ink_dim);
        });
    });
}

/// The title bar: the amplifier and its link, its mode, and the range the
/// power meters are on.
fn title_bar(ui: &mut Ui, sh: &Shown, th: &AmpTheme) {
    let s = sh.s;
    let rect = band(ui, th, 24.0 * s);
    let p = ui.painter_at(rect);
    let font = label_font(11.5 * s);
    let sp = 1.1 * s;
    let x = rect.left() + 10.0 * s;
    let cy = rect.center().y;
    let name = text(ui, sh.st.model.short(), font.clone(), th.ink, sp);
    let w = name.size().x;
    p.galley(pos2(x, cy - name.size().y / 2.0), name, th.ink);
    let (state, tone) = link_state(sh.st, sh.transmitting);
    // Connected is the ordinary state, in the secondary ink; a fault stands
    // out in its warning colour.
    let col = match tone {
        Tone::Red | Tone::Yellow => tone_col(th, tone),
        _ => th.ink_dim,
    };
    let state = text(ui, &format!(" · {}", state.to_uppercase()), font.clone(), col, sp);
    p.galley(pos2(x + w, cy - state.size().y / 2.0), state, col);

    let r = &sh.r;
    let mode = match (sh.live, r.ptt, super::rf_present(r)) {
        (false, _, _) => "",
        (true, true, true) => "TRANSMIT · PTT",
        (true, true, false) => "STAND-BY · PTT",
        (true, false, true) => "RF THROUGH · OFF",
        (true, false, false) => "BYPASS · OFF",
    };
    let g = text(ui, mode, font.clone(), th.ink_dim, sp);
    p.galley(pos2(rect.center().x - g.size().x / 2.0, cy - g.size().y / 2.0), g, th.ink_dim);
    let range = match sh.range {
        PowerRange::Full => "FULL SCALE",
        PowerRange::Hundred => "100 W SCALE",
        PowerRange::Ten => "10 W SCALE",
    };
    let g = text(ui, range, font, th.ink_dim, sp);
    p.galley(pos2(rect.right() - 10.0 * s - g.size().x, cy - g.size().y / 2.0), g, th.ink_dim);
}

/// Each meter's TOP and BOTTOM pickers, side by side over the meters.
fn pick_row(ui: &mut Ui, view: &mut AmpMeters, th: &AmpTheme, s: f32) {
    let rect = band(ui, th, 28.0 * s);
    let half = rect.width() / 2.0;
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, th.screen_line));
    let ink = PickInk {
        label: th.ink_dim,
        top: th.ink,
        bottom: th.inner,
        none: th.screen_line,
        top_text: th.ink,
        bottom_text: th.inner,
        size: 11.0,
        spacing: 1.0,
        hover: "Pick any reading for either needle. The top needle is the dark one on the outer \
                scale, the bottom one blue on the inner scale.",
    };
    for i in 0..2 {
        let r = Rect::from_min_size(
            pos2(rect.left() + half * i as f32 + 10.0 * s, rect.top()),
            vec2(half - 12.0 * s, rect.height() - 1.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(r)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.spacing_mut().item_spacing = vec2(5.0 * s, 0.0);
        pickers(&mut child, i, &mut view.pairs[i], s, &ink);
    }
}

/// The two meters, as white analog faces side by side.
fn meters(ui: &mut Ui, sh: &Shown, view: &mut AmpMeters, th: &AmpTheme) {
    let s = sh.s;
    let w = ui.available_width();
    let face_w = (w - 1.0) / 2.0;
    let dial_h = face_w.min(sh.nominal / 2.0) * 0.5;
    let h = dial_h + 46.0 * s;
    let rect = band(ui, th, h + 1.0);
    for i in 0..2 {
        let (top, bottom) = sh.dials(view, i);
        let r = Rect::from_min_size(
            pos2(rect.left() + (face_w + 1.0) * i as f32, rect.top()),
            vec2(face_w, h),
        );
        face_meter(ui, r, dial_h, s, th, &top, bottom.as_ref());
    }
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, th.screen_line));
}

/// A white meter face: the top reading on the outer scale with a dark
/// needle, the bottom one on the inner scale in blue, both live, and under
/// them each one's name and reading.
fn face_meter(
    ui: &Ui,
    rect: Rect,
    dial_h: f32,
    s: f32,
    th: &AmpTheme,
    top: &Dial,
    bottom: Option<&Dial>,
) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, th.face);
    let dial = Rect::from_min_size(rect.min, vec2(rect.width(), dial_h));
    let pivot = pos2(dial.center().x, dial.bottom() - 2.0 * s);
    // The scale sweeps 72° either side of vertical, as the DUO-ART's do.
    let sweep = 72f32.to_radians();
    let outer = (dial_h * 0.74).min(rect.width() * 0.38);
    let inner = outer * 0.72;
    let at = |frac: f32, r: f32| {
        let a = -std::f32::consts::FRAC_PI_2 - sweep + frac.clamp(0.0, 1.2) * 2.0 * sweep;
        pivot + Vec2::new(a.cos(), a.sin()) * r
    };
    let arc = |from: f32, to: f32, r: f32, stroke: Stroke| {
        let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
        if to > from {
            let n = ((to - from) * 48.0).ceil().max(1.0) as usize;
            let pts = (0..=n).map(|i| at(from + (to - from) * i as f32 / n as f32, r));
            p.add(Shape::line(pts.collect(), stroke));
        }
    };

    // One scale: the arc in its ink up to the warning, then yellow and red
    // on the arc itself, ticks, and the figures outside (outer) or inside
    // (inner).
    let scale = |d: &Dial, radius: f32, ink: Color32, out: bool| {
        let thin = Stroke::new(1.4 * s, ink);
        let thick = |c: Color32| Stroke::new(3.5 * s, c);
        let figure = |v: f32| if d.full < 20.0 { format!("{v:.1}") } else { format!("{v:.0}") };
        let font = FontId::monospace(if out { 9.5 } else { 8.5 } * s);
        let label_r = if out { radius + 11.0 * s } else { radius - 15.0 * s };
        match d.zones {
            Zones::Above(warn, alarm) => {
                arc(0.0, warn, radius, thin);
                arc(warn, alarm, radius, thick(th.warn));
                arc(alarm, 1.0, radius, thick(th.alarm));
                // A level past the end of a rescaled scale is marked just
                // beyond it, in its colour and with its value, so the
                // colours are always on show.
                let beyond =
                    [(warn, th.warn), (alarm, th.alarm)].into_iter().filter(|&(f, _)| f > 1.0);
                for (k, (f, col)) in beyond.enumerate() {
                    let pos = 1.07 + 0.07 * k as f32;
                    p.line_segment(
                        [at(pos, radius + 2.0 * s), at(pos, radius - 6.0 * s)],
                        thick(col),
                    );
                    p.text(
                        at(pos, label_r),
                        egui::Align2::CENTER_CENTER,
                        figure(d.full * f),
                        font.clone(),
                        col,
                    );
                }
            }
            Zones::Below(low) => {
                arc(0.0, low, radius, thick(th.warn));
                arc(low, 1.0, radius, thin);
            }
        }
        for i in 0..=10 {
            let f = i as f32 / 10.0;
            let major = i % 2 == 0;
            let len = if major { 6.0 } else { 3.5 } * s;
            let (a, b) = if out { (radius, radius + len) } else { (radius, radius - len) };
            p.line_segment([at(f, a), at(f, b)], Stroke::new(1.1 * s, ink));
            if major {
                p.text(
                    at(f, label_r),
                    egui::Align2::CENTER_CENTER,
                    figure(d.full * f),
                    font.clone(),
                    ink,
                );
            }
        }
    };
    scale(top, outer, th.face_ink, true);
    if let Some(d) = bottom {
        scale(d, inner, th.face_inner, false);
    }

    // A tapered needle: a sharp tip, a narrow base and a short tail past
    // the pivot.
    let needle = |d: &Dial, len: f32, base: f32, col: Color32| {
        if !d.live {
            return;
        }
        let f = (d.value / d.full.max(1e-6)).min(1.03);
        let tip = at(f, len);
        let dir = (tip - pivot).normalized();
        let tail = pivot - dir * 10.0 * s;
        let n = vec2(-dir.y, dir.x) * base * s;
        p.add(Shape::convex_polygon(
            vec![tip, pivot + n, tail + n * 0.6, tail - n * 0.6, pivot - n],
            col,
            Stroke::NONE,
        ));
    };
    if let Some(d) = bottom {
        needle(d, inner - 3.0 * s, 1.5, th.face_inner);
    }
    needle(top, outer + 3.0 * s, 2.0, th.face_ink);
    p.circle_filled(pivot, 4.5 * s, th.face_ink);
    p.circle_filled(pivot, 1.6 * s, th.face);

    // Red across the top while the radio's watts can't be trusted (AJ3G,
    // 18 Oct 2026).
    if let Some(t) = unsure_warning(top, bottom) {
        p.text(
            pos2(rect.center().x, rect.top() + 4.0 * s),
            egui::Align2::CENTER_TOP,
            t,
            label_font(12.5 * s),
            th.alarm,
        );
    }

    // Under the dial: each needle's name, its scale and where it came from,
    // and its reading, in its needle's colour.
    let row = |d: &Dial, y: f32, ring: &str, ink: Color32, big: bool| {
        let left = rect.left() + 20.0 * s;
        let right = rect.right() - 10.0 * s;
        let reading = d.readout.as_ref().map(|t| {
            p.layout_no_wrap(t.clone(), FontId::monospace(if big { 15.0 } else { 13.0 } * s), ink)
        });
        let rw = reading.as_ref().map_or(0.0, |g| g.size().x);
        let words = format!("{} · {ring}{}{}", d.name, d.note, d.source);
        let full = p.layout_no_wrap(words.clone(), label_font(11.5 * s), th.face_dim);
        let size = legend_size(full.size().x, right - rw - 8.0 * s - left, s * 11.5 / 12.0);
        let g = if size < 11.5 * s {
            p.layout_no_wrap(words, label_font(size), th.face_dim)
        } else {
            full
        };
        p.circle_filled(pos2(rect.left() + 12.0 * s, y), 3.5 * s, ink);
        p.galley(pos2(left, y - g.size().y / 2.0), g, th.face_dim);
        if let Some(g) = reading {
            p.galley(pos2(right - g.size().x, y - g.size().y / 2.0), g, ink);
        }
    };
    let y0 = dial.bottom() + 13.0 * s;
    row(top, y0, "outer", th.face_ink, true);
    if let Some(d) = bottom {
        row(d, y0 + 19.0 * s, "inner", th.face_inner, false);
    }
}

/// The seven readings across the screen, each with a bar along its scale.
/// Returns whether the temperature was clicked (°C/°F).
fn tile_row(ui: &mut Ui, sh: &Shown, th: &AmpTheme) -> bool {
    let s = sh.s;
    let rect = band(ui, th, 44.0 * s);
    let w = rect.width() / 7.0;
    let unit_now = if sh.r.celsius == Some(false) { "°C" } else { "°F" };
    let mut clicked = false;
    for (i, t) in tiles(sh).into_iter().enumerate() {
        let Tile { name, value, gauge, hover, frac, level } = t;
        let r = Rect::from_min_size(
            pos2(rect.left() + w * i as f32, rect.top()),
            vec2(w, rect.height() - 1.0),
        );
        let temp = gauge == Some(AmpGauge::Temp) && sh.live;
        let resp = ui.interact(
            r,
            ui.id().with(("amp-tile", i)),
            if temp { Sense::click() } else { Sense::hover() },
        );
        let p = ui.painter_at(r);
        if i > 0 {
            p.vline(r.left() + 0.5, r.y_range(), Stroke::new(1.0, th.screen_line));
        }
        let pad = 7.0 * s;
        let g = text(ui, name, label_font(10.0 * s), th.ink_dim, 1.2 * s);
        let name_w = g.size().x;
        p.galley(pos2(r.left() + pad, r.top() + 5.0 * s), g, th.ink_dim);
        if temp {
            // The other scale, underlined in blue as a link is: the tile
            // switches to it when clicked.
            let g = text(ui, unit_now, label_font(10.0 * s), th.status, 0.0);
            let at = pos2(r.left() + pad + name_w + 4.0 * s, r.top() + 5.0 * s);
            let u = Rect::from_min_size(at, g.size());
            p.galley(at, g, th.status);
            p.hline(u.x_range(), u.bottom(), Stroke::new(1.0 * s, th.status));
        }
        let ink = level_ink(th, level);
        let shown = if sh.live { value } else { "—".to_string() };
        let vg = p.layout_no_wrap(
            shown,
            FontId::monospace(13.0 * s),
            if sh.live { ink } else { th.ink_dim },
        );
        // Narrow windows set the figure smaller rather than over the next
        // tile.
        let vg = if vg.size().x > w - 2.0 * pad {
            let k = (w - 2.0 * pad) / vg.size().x;
            p.layout_no_wrap(vg.text().to_string(), FontId::monospace(13.0 * s * k), ink)
        } else {
            vg
        };
        p.galley(pos2(r.left() + pad, r.top() + 18.0 * s), vg, ink);
        let track = Rect::from_min_size(
            pos2(r.left() + pad, r.bottom() - 7.0 * s),
            vec2(w - 2.0 * pad, 3.0 * s),
        );
        p.rect_filled(track, 1.5 * s, th.screen_line);
        if sh.live && frac > 0.0 {
            let mut bar = track;
            bar.set_width(track.width() * frac);
            let col = match level {
                Some(AmpLevel::Yellow) => th.warn,
                Some(AmpLevel::Red) => th.alarm,
                _ => th.diag,
            };
            p.rect_filled(bar, 1.5 * s, col);
        }
        let resp = if temp {
            if resp.hovered() {
                p.rect_stroke(r.shrink(1.5), 2.0, Stroke::new(1.5 * s, th.ink), StrokeKind::Inside);
            }
            resp.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            resp
        };
        if resp.on_hover_text(hover).clicked() {
            clicked = true;
        }
    }
    clicked
}

/// Input power, efficiency and gain while transmitting: the number holds
/// the peak, the bar under it is live with the held peak as a tick.
fn performance(ui: &mut Ui, sh: &Shown, view: &AmpMeters, th: &AmpTheme) {
    let s = sh.s;
    let rect = band(ui, th, 76.0 * s);
    let p = ui.painter_at(rect);
    let pad = 10.0 * s;
    let head = text(ui, "PERFORMANCE", label_font(10.5 * s), th.status, 1.3 * s);
    p.galley(pos2(rect.left() + pad, rect.top() + 6.0 * s), head, th.status);
    let note =
        text(ui, "numbers hold peaks · bars are live", label_font(10.5 * s), th.ink_dim, 0.6 * s);
    p.galley(pos2(rect.right() - pad - note.size().x, rect.top() + 6.0 * s), note, th.ink_dim);

    let full = super::perf_full(&sh.limits);
    let boxes = [
        ("INPUT POWER", "W", 0, "Supply volts × drain amps"),
        ("EFFICIENCY", "%", 0, "Output power ÷ input power"),
        ("GAIN", "dB", 1, "Output power over drive, in decibels"),
    ];
    let gap = 8.0 * s;
    let bw = (rect.width() - 2.0 * pad - 2.0 * gap) / 3.0;
    let perf = &view.perf;
    for (i, (name, unit, places, hover)) in boxes.into_iter().enumerate() {
        let b = Rect::from_min_size(
            pos2(rect.left() + pad + (bw + gap) * i as f32, rect.top() + 22.0 * s),
            vec2(bw, 46.0 * s),
        );
        ui.interact(b, ui.id().with(("amp-perf", i)), Sense::hover()).on_hover_text(hover);
        p.rect_stroke(b, 3.0, Stroke::new(1.0, th.screen_line), StrokeKind::Inside);
        let x = b.left() + 8.0 * s;
        let g = text(ui, name, label_font(10.0 * s), th.ink_dim, 1.2 * s);
        p.galley(pos2(x, b.top() + 4.0 * s), g, th.ink_dim);
        let fmt = |v: f32| format!("{v:.places$}");
        let held = perf.peaks[i].get(sh.now);
        let value = if held > 0.0 { fmt(held) } else { "—".to_string() };
        let vr = p.text(
            pos2(x, b.top() + 17.0 * s),
            egui::Align2::LEFT_TOP,
            value,
            FontId::monospace(16.0 * s),
            th.ink,
        );
        p.text(
            pos2(vr.right() + 4.0 * s, vr.bottom() - 2.0 * s),
            egui::Align2::LEFT_BOTTOM,
            unit,
            FontId::monospace(10.0 * s),
            th.ink_dim,
        );
        if let Some(a) = perf.live {
            p.text(
                pos2(b.right() - 8.0 * s, vr.bottom() - 2.0 * s),
                egui::Align2::RIGHT_BOTTOM,
                format!("live {} {unit}", fmt(a[i])),
                label_font(10.0 * s),
                th.ink_dim,
            );
        }
        let track =
            Rect::from_min_size(pos2(x, b.bottom() - 8.0 * s), vec2(b.width() - 16.0 * s, 3.0 * s));
        p.rect_filled(track, 1.5 * s, th.screen_line);
        let frac = |v: f32| (v / full[i]).clamp(0.0, 1.0);
        if let Some(a) = perf.live {
            let mut bar = track;
            bar.set_width(track.width() * frac(a[i]));
            p.rect_filled(bar, 1.5 * s, th.inner);
        }
        if held > 0.0 {
            let px = track.left() + track.width() * frac(held);
            p.line_segment(
                [pos2(px, track.top() - 3.0 * s), pos2(px, track.bottom() + 3.0 * s)],
                Stroke::new(2.0 * s, th.ink),
            );
        }
    }
}

/// The tuner's network, as in the panel's own look, in the screen's ink.
fn tuner(ui: &mut Ui, sh: &Shown, th: &AmpTheme) {
    let Some(atu) = sh.st.atu.as_ref() else { return };
    let s = sh.s;
    let h = (sh.nominal * 0.15).max(104.0 * s);
    let rect = band(ui, th, h + 22.0 * s);
    let p = ui.painter_at(rect);
    let pad = 10.0 * s;
    let head = text(ui, "TUNER · ATU-500", label_font(10.5 * s), th.status, 1.3 * s);
    p.galley(pos2(rect.left() + pad, rect.top() + 6.0 * s), head, th.status);
    let side = text(ui, net_caption(atu), label_font(10.5 * s), th.ink_dim, 0.6 * s);
    p.galley(pos2(rect.right() - pad - side.size().x, rect.top() + 6.0 * s), side, th.ink_dim);
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + 20.0 * s), rect.max);
    let art =
        Rect::from_center_size(body.center(), vec2(sh.nominal.min(body.width()), body.height()));
    let ink = NetInk {
        wire: th.ink,
        value: th.ink,
        small: th.ink_dim,
        lit: th.inner,
        bg: th.screen,
        caption: None,
    };
    if atu_bypassed(atu) {
        bypass_drawing(&p, art, s, &ink);
    } else if let Some(n) = atu.network {
        lc_network(&p, art, s, n, sh.st.model, &ink);
    }
}

/// The status line, in blue: the amplifier's mode, band and antenna, where
/// its band comes from, and the tuner.
fn status_bar(ui: &mut Ui, sh: &Shown, view: &AmpMeters, th: &AmpTheme) {
    let s = sh.s;
    let rect = band(ui, th, 22.0 * s);
    let r = &sh.r;
    let mut items: Vec<String> = Vec::new();
    if sh.live {
        items.push(if r.ptt { "PTT mode".into() } else { "OFF · bypass".into() });
        items.push(r.band.map_or("No band", amp_band_label).to_string());
        if sh.st.model.antennas() > 1 {
            items.push(format!("ANT {}", r.antenna.max(1)));
        }
    }
    items.push(match view.radio {
        Some((radio, true)) => format!("Band follows the {radio}"),
        Some((radio, false)) => format!("{radio} · band set here"),
        None => "No radio".into(),
    });
    if let Some(atu) = sh.st.atu.as_ref().filter(|_| sh.live) {
        items.push(if atu.active { "ATU ON".into() } else { "ATU BYP".into() });
    }
    let col = if sh.live { th.status } else { th.ink_dim };
    let mut x = rect.left() + 10.0 * s;
    let p = ui.painter_at(rect);
    for it in items {
        let g = text(ui, &it, label_font(11.5 * s), col, 0.6 * s);
        let w = g.size().x;
        p.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, col);
        x += w + 14.0 * s;
    }
}

/// The diagnostic line, in green: ready or keyed, with the temperature and
/// the supply.
fn diag_bar(ui: &mut Ui, sh: &Shown, th: &AmpTheme) {
    let s = sh.s;
    let rect = band(ui, th, 22.0 * s);
    let r = &sh.r;
    let (words, col) = if sh.live {
        let state = if super::rf_present(r) { "Keyed" } else { "Ready" };
        let unit = if r.celsius == Some(false) { "°F" } else { "°C" };
        (format!("{state} · {:.0} {unit} · supply {:.1} V", r.temperature, r.vdd_v), th.diag)
    } else {
        ("Waiting for the amplifier".to_string(), th.ink_dim)
    };
    let g = text(ui, &words, label_font(11.5 * s), col, 0.6 * s);
    ui.painter_at(rect).galley(
        pos2(rect.left() + 10.0 * s, rect.center().y - g.size().y / 2.0),
        g,
        col,
    );
}

/// The warnings now, worst first, in their colours; empty when there are
/// none.
fn warnings(sh: &Shown) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    let (state, tone) = link_state(sh.st, sh.transmitting);
    if matches!(tone, Tone::Red | Tone::Yellow) {
        out.push((state, tone));
    }
    if sh.past {
        out.push(("SWR at or past the warning: check the antenna".into(), Tone::Red));
    }
    let high: Vec<(&str, AmpLevel)> = [
        ("FWD", AmpGauge::Fwd),
        ("RFL", AmpGauge::Rfl),
        ("DRV", AmpGauge::Drive),
        ("VDD", AmpGauge::Vdd),
        ("IDD", AmpGauge::Idd),
        ("TEMP", AmpGauge::Temp),
    ]
    .into_iter()
    .filter_map(|(n, g)| sh.level(g).filter(|&l| l != AmpLevel::Green).map(|l| (n, l)))
    .collect();
    if !high.is_empty() {
        let worst = high.iter().map(|h| h.1).max().unwrap_or(AmpLevel::Yellow);
        let names: Vec<&str> = high.iter().map(|h| h.0).collect();
        let tone = if worst == AmpLevel::Red { Tone::Red } else { Tone::Yellow };
        let what = if names == ["VDD"] { "low" } else { "past its warning" };
        out.push((format!("{} {what}", names.join(", ")), tone));
    }
    out
}

/// The message line: warnings on the left, the tuner on the right.
fn message_line(ui: &mut Ui, sh: &Shown, th: &AmpTheme) {
    let s = sh.s;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 22.0 * s), Sense::hover());
    let p = ui.painter_at(rect);
    let font = label_font(11.5 * s);
    let mut right = rect.right() - 10.0 * s;
    if let Some((state, tone, swr)) = sh.st.atu.as_ref().and_then(atu_words) {
        let col = tone_col(th, tone);
        let words = match swr {
            Some(swr) => format!("ATU {state} · {swr}"),
            None => format!("ATU {state}"),
        };
        let g = text(ui, &words, font.clone(), col, 0.6 * s);
        right -= g.size().x;
        p.galley(pos2(right, rect.center().y - g.size().y / 2.0), g, col);
        right -= 14.0 * s;
    }
    let mut x = rect.left() + 10.0 * s;
    for (words, tone) in warnings(sh) {
        let col = tone_col(th, tone);
        let g = text(ui, &words, font.clone(), col, 0.6 * s);
        if x + g.size().x > right {
            break;
        }
        let gw = g.size().x;
        p.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, col);
        x += gw + 14.0 * s;
    }
}

/// The six soft keys under the screen, each with its label above it: lit in
/// its tone when on, faded when it can't be pressed.
fn soft_keys(ui: &mut Ui, th: &AmpTheme, keys: &Keys, s: f32, actions: &mut Vec<AmpAction>) {
    let slots: [Option<&Key>; 6] = [
        Some(&keys.ptt),
        keys.ant.as_ref(),
        Some(&keys.dn),
        Some(&keys.up),
        keys.tune.as_ref(),
        keys.atu.as_ref(),
    ];
    let gap = 8.0 * s;
    let w = ui.available_width();
    let slot_w = (w - 5.0 * gap) / 6.0;
    let (row, _) = ui.allocate_exact_size(vec2(w, 46.0 * s), Sense::hover());
    for (i, k) in slots.into_iter().enumerate() {
        let Some(k) = k else { continue };
        let slot = Rect::from_min_size(
            pos2(row.left() + (slot_w + gap) * i as f32, row.top()),
            vec2(slot_w, row.height()),
        );
        let id = ui.id().with(("amp-soft-key", i));
        let resp = ui.interact(slot, id, if k.enabled { Sense::click() } else { Sense::hover() });
        let p = ui.painter_at(slot.expand(3.0 * s));
        let ink = if !k.enabled {
            th.label.gamma_multiply(0.35)
        } else if k.lit && k.tone != Tone::Plain {
            tone_col(th, k.tone)
        } else {
            th.label
        };
        // The tuner's key says what it switches, as the DUO-ART's do.
        let label = if i == 5 { format!("ATU {}", k.text) } else { k.text.clone() };
        let g = text(ui, &label, label_font(11.0 * s), ink, 1.3 * s);
        p.galley(pos2(slot.center().x - g.size().x / 2.0, slot.top()), g, ink);

        let cap_w = slot_w.min(90.0 * s);
        let mut cap = Rect::from_center_size(
            pos2(slot.center().x, slot.bottom() - 13.0 * s),
            vec2(cap_w, 24.0 * s),
        );
        if resp.is_pointer_button_down_on() {
            cap = cap.translate(vec2(0.0, 1.0));
        }
        let hover = resp.hovered() && k.enabled;
        let lift = |c: Color32| if hover { c.lerp_to_gamma(Color32::WHITE, 0.08) } else { c };
        let fade = |c: Color32| if k.enabled { c } else { c.gamma_multiply(0.5) };
        p.rect_filled(cap.translate(vec2(0.0, 2.0 * s)), 5.0 * s, Color32::from_gray(0x0a));
        gradient(&p, cap.shrink(1.0), fade(lift(th.key_top)), fade(lift(th.key_bottom)));
        p.hline(
            cap.x_range().shrink(4.0 * s),
            cap.top() + 1.5,
            Stroke::new(1.0, th.key_top.lerp_to_gamma(Color32::WHITE, 0.12)),
        );
        // A lit key shows its state as a light in the cap.
        if k.lit && k.tone != Tone::Plain {
            let led = Rect::from_center_size(cap.center(), vec2(cap_w * 0.36, 3.0 * s));
            p.rect_filled(led, 1.5 * s, tone_col(th, k.tone));
        }
        p.rect_stroke(cap, 5.0 * s, Stroke::new(1.0, th.key_edge), StrokeKind::Inside);
        // Anything that can be clicked must look it (AJ3G).
        if hover {
            p.rect_stroke(
                cap.expand(1.0),
                5.0 * s,
                Stroke::new(1.5 * s, th.ink),
                StrokeKind::Outside,
            );
        }
        let resp = if k.enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
        if resp.on_hover_text(k.hover).on_disabled_hover_text(&k.why_not).clicked() && k.enabled {
            actions.push(k.action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardrock::{AmpReadings, AmpStatus};

    fn shown(st: &AmpStatus) -> Shown<'_> {
        Shown {
            st,
            r: st.readings.unwrap_or_default(),
            limits: st.model.limits(),
            rig: Default::default(),
            live: st.connected,
            past: false,
            range: PowerRange::Full,
            now: 0.0,
            s: 1.0,
            width: 720.0,
            nominal: 720.0,
            transmitting: false,
        }
    }

    #[test]
    fn a_quiet_amplifier_has_no_warnings_and_a_hot_one_does() {
        let r = AmpReadings {
            vdd_v: 53.0,
            temperature: 30.0,
            celsius: Some(true),
            ptt: true,
            ..Default::default()
        };
        let st = AmpStatus {
            configured: true,
            connected: true,
            readings: Some(r),
            ..Default::default()
        };
        assert!(warnings(&shown(&st)).is_empty());
        let hot = AmpReadings { temperature: 60.0, ..r };
        let st = AmpStatus { readings: Some(hot), ..st };
        let w = warnings(&shown(&st));
        assert_eq!(w.len(), 1);
        assert!(w[0].0.starts_with("TEMP"));
        assert_ne!(w[0].1, Tone::Blue);
    }

    #[test]
    fn neutral_states_are_never_yellow_or_red() {
        let th = &super::super::amptheme::ELAD;
        for t in [Tone::Plain, Tone::Blue, Tone::Green] {
            assert_ne!(tone_col(th, t), th.warn);
            assert_ne!(tone_col(th, t), th.alarm);
        }
        assert_eq!(level_ink(th, Some(AmpLevel::Green)), th.ink);
    }
}
