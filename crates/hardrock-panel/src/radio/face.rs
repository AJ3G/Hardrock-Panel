//! The radio's face: its LCD, with the dial as clickable seven-segment
//! digits, the annunciators, the segment meter and the text line, and the
//! knobs beside it; and the key caps everything else on the window wears.
//! Colours and slant come from the [`RigTheme`].

use std::time::Instant;

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Mesh, Pos2, Rect, Response, Sense,
    Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::logic::{self, RadioPrefs};
use super::rigtheme::RigTheme;
use super::zoom::{k as zoom, z};
use super::{Face, Pop};

/// The case lettering's font, `size` at scale 1 (see [`super::zoom`]).
pub(crate) fn label_font(size: f32) -> FontId {
    FontId::new(z(size), FontFamily::Name("chakra-bold".into()))
}

/// The LCD's and the readouts' figures, `size` at scale 1.
pub(crate) fn data_font(size: f32) -> FontId {
    FontId::monospace(z(size))
}

/// A key cap: a rounded key that lights in the status blue when it is on.
/// Greyed by the `Ui` when it is disabled.
pub(crate) fn cap(ui: &mut Ui, th: &RigTheme, lit: bool, text: &str) -> Response {
    let font = label_font(12.5);
    let ink = if lit { th.cap_lit_ink } else { th.cap_ink };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, ink);
    let size = vec2(galley.size().x + z(22.0), z(26.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let down = resp.is_pointer_button_down_on();
        let r = if down { rect.translate(vec2(0.0, 1.0)) } else { rect };
        let cr = CornerRadius::same(5);
        if lit {
            p.rect_filled(r.expand(z(3.0)), CornerRadius::same(8), th.cap_lit.gamma_multiply(0.25));
            p.rect_filled(r, cr, th.cap_lit);
        } else {
            let hover = resp.hovered() && ui.is_enabled();
            let lift = |c: Color32| if hover { c.lerp_to_gamma(Color32::WHITE, 0.08) } else { c };
            gradient(p, r, &[lift(th.cap_top), lift(th.cap_bottom)], 5.0);
            p.hline(
                r.x_range().shrink(4.0),
                r.top() + 1.0,
                Stroke::new(1.0, th.cap_top.lerp_to_gamma(Color32::WHITE, 0.12)),
            );
        }
        p.rect_stroke(r, cr, Stroke::new(1.0, Color32::from_gray(0x11)), StrokeKind::Inside);
        let at = r.center() - galley.size() / 2.0;
        p.galley(at, galley, ink);
    }
    if ui.is_enabled() { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// A silk-screen caption.
pub(crate) fn caption(ui: &mut Ui, th: &RigTheme, text: &str) -> Response {
    ui.label(egui::RichText::new(text).font(label_font(11.0)).color(th.dim))
}

/// A vertical gradient through `stops`, evenly spaced, in a rect with the
/// corners cut square (the radius is drawn over by the caller's stroke).
pub(crate) fn gradient(p: &egui::Painter, r: Rect, stops: &[Color32], _radius: f32) {
    let mut m = Mesh::default();
    let n = stops.len().max(2) - 1;
    for (i, c) in stops.iter().enumerate() {
        let y = r.top() + r.height() * i as f32 / n as f32;
        m.colored_vertex(pos2(r.left(), y), *c);
        m.colored_vertex(pos2(r.right(), y), *c);
        if i > 0 {
            let k = (i as u32) * 2;
            m.add_triangle(k - 2, k - 1, k);
            m.add_triangle(k - 1, k, k + 1);
        }
    }
    p.add(Shape::mesh(m));
}

/// The segments each digit lights: a top, b top right, c bottom right,
/// d bottom, e bottom left, f top left, g middle.
const SEGS: [&str; 10] =
    ["abcdef", "bc", "abged", "abgcd", "fgbc", "afgcd", "afgedc", "abc", "abcdefg", "abcfgd"];

/// The digits' size before scaling: width, height, gap, segment thickness.
const DW: f32 = 26.0;
const DH: f32 = 54.0;
const GAP: f32 = 6.0;
const T: f32 = 4.0;

/// One seven-segment digit at `o` (top left), `k` times its unit size.
fn seven(p: &egui::Painter, o: Pos2, k: f32, skew: f32, d: Option<u8>, on: Color32, off: Color32) {
    let lit = d.map_or("", |d| SEGS[d as usize]);
    let (w, h, t) = (DW, DH, T);
    let segs = [
        ('a', [t, 0.0, w - 2.0 * t, t]),
        ('d', [t, h - t, w - 2.0 * t, t]),
        ('g', [t, h / 2.0 - t / 2.0, w - 2.0 * t, t]),
        ('f', [0.0, t, t, h / 2.0 - t]),
        ('b', [w - t, t, t, h / 2.0 - t]),
        ('e', [0.0, h / 2.0, t, h / 2.0 - t]),
        ('c', [w - t, h / 2.0, t, h / 2.0 - t]),
    ];
    for (name, [x, y, sw, sh]) in segs {
        // Slanted as the radio's own: x moves left as y goes down.
        let pt = |px: f32, py: f32| pos2(o.x + (px + (h - py) * skew) * k, o.y + py * k);
        let pts = vec![pt(x, y), pt(x + sw, y), pt(x + sw, y + sh), pt(x, y + sh)];
        let c = if lit.contains(name) { on } else { off };
        p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
    }
}

/// How many wheel notches up (positive) or down were turned over the widget
/// this frame. The scroll is taken, so the window doesn't scroll as well.
fn wheel_steps(ui: &Ui, resp: &Response) -> i64 {
    if !resp.hovered() {
        return 0;
    }
    ui.ctx().input_mut(|i| {
        let mut n = 0;
        for e in &i.events {
            if let egui::Event::MouseWheel { delta, .. } = e {
                n += delta.y.signum() as i64;
            }
        }
        i.smooth_scroll_delta = egui::Vec2::ZERO;
        n
    })
}

/// The LCD's backlight now: the receive colour, or in transmit the transmit
/// colour. The panel's own choice, whatever the radio's menu 73 TX BACKLIGHT
/// says for its own screen (AJ3G: set to red, it stayed blue with menu 73
/// off).
pub(crate) fn backlight(f: &Face<'_>, prefs: &RadioPrefs) -> Color32 {
    let tx = f.s.transmitting();
    f.th.backlight(if tx { prefs.tx_light } else { prefs.rx_light })
}

/// Step the dial by `dir` at the place value `place`, from the frequency
/// last asked for if the radio hasn't read back yet (so fast clicks add up),
/// else from the read-back.
pub(crate) fn step(f: &mut Face<'_>, place: u64, dir: i64) {
    let Some(rb) = logic::dial_hz(f.s) else { return };
    let base = match f.st.pending {
        Some((hz, t)) if hz != rb && t.elapsed().as_secs_f32() < 1.5 => hz,
        _ => rb,
    };
    let to = logic::step_digit(base, place, dir);
    if to != base {
        tune_to(f, to, None);
    }
}

/// Send the receive VFO to `if_hz`, and the mode if given.
pub(crate) fn tune_to(f: &mut Face<'_>, if_hz: u64, mode: Option<&str>) {
    match logic::tune_frames(f.p, f.s, if_hz, mode) {
        Ok(v) => {
            f.out.frames.extend(v);
            f.st.pending = Some((if_hz, Instant::now()));
        }
        Err(e) => f.st.note = Some(e),
    }
}

/// The LCD, `width` wide.
pub(crate) fn lcd(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs, width: f32) {
    let th = f.th;
    let xvtr = prefs.xvtr_hz;
    let n = logic::digit_count(xvtr);
    let dots = (0..n - 1).filter(|i| (n - 1 - i).is_multiple_of(3)).count();
    let units = n as f32 * (DW + GAP) - GAP + dots as f32 * GAP;
    let pad = z(16.0); // bezel and padding, each side
    let mode_w = z(70.0);
    let k =
        ((width - 2.0 * pad - mode_w - DH * th.skew) / units).clamp(0.5 * zoom(), 1.25 * zoom());
    let digits_h = (DH + 24.0) * k;
    let height = 2.0 * pad + z(14.0 + 4.0 + 14.0 + 6.0) + digits_h + z(6.0 + 14.0 + 6.0 + 18.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect.expand(2.0));
    let bl = backlight(f, prefs);
    p.rect_filled(rect, CornerRadius::same(6), th.bezel);
    let glass = rect.shrink(z(6.0));
    gradient(
        &p,
        glass,
        &[bl.lerp_to_gamma(Color32::WHITE, 0.12), bl, bl.lerp_to_gamma(Color32::BLACK, 0.2)],
        0.0,
    );
    let ink = th.lcd_ink;
    let ghost = ink.gamma_multiply(th.lcd_ghost);
    let inner = glass.shrink(pad - z(6.0));
    let locked = f.lock.is_some();
    let mut y = inner.top();

    // Annunciators. Some are keys: click to change.
    annunciators(ui, f, prefs, pos2(inner.left(), y), inner.width());
    y += z(14.0 + 4.0);
    // The receive settings the radio's own display shows.
    receive_row(ui, f, prefs, pos2(inner.left(), y), inner.width());
    y += z(14.0 + 6.0);

    // The dial.
    let rb = logic::dial_hz(f.s);
    let shown = rb.map(|h| logic::shown(h, xvtr));
    let ds = match shown {
        Some(h) => logic::digits(h, n),
        None => vec![None; n],
    };
    let top = y + 12.0 * k;
    let mut x = inner.left() + z(2.0);
    for (i, d) in ds.iter().enumerate() {
        let pl = logic::place(i, n);
        let dr = Rect::from_min_size(pos2(x - 3.0 * k, y), vec2((DW + 6.0) * k, digits_h));
        if logic::steppable(i, n, xvtr) && rb.is_some() {
            let id = ui.id().with(("radio-digit", i));
            let r = ui.interact(dr, id, if locked { Sense::hover() } else { Sense::click() });
            let sel = f.st.sel == Some(i);
            if !locked && (r.hovered() || sel) {
                p.rect_filled(dr, CornerRadius::same(3), ink.gamma_multiply(0.08));
            }
            if !locked {
                let a = if r.hovered() || sel { 0.85 } else { 0.22 };
                let cx = dr.center().x + DH * th.skew * k * 0.5;
                let c = ink.gamma_multiply(a);
                p.add(Shape::convex_polygon(
                    vec![
                        pos2(cx - z(6.0), y + 9.0 * k),
                        pos2(cx, y + 2.0 * k),
                        pos2(cx + z(6.0), y + 9.0 * k),
                    ],
                    c,
                    Stroke::NONE,
                ));
                let b = y + digits_h;
                p.add(Shape::convex_polygon(
                    vec![
                        pos2(cx - z(6.0), b - 9.0 * k),
                        pos2(cx + z(6.0), b - 9.0 * k),
                        pos2(cx, b - 2.0 * k),
                    ],
                    c,
                    Stroke::NONE,
                ));
                if r.clicked() {
                    let up = r.interact_pointer_pos().is_none_or(|q| q.y < dr.center().y);
                    f.st.sel = Some(i);
                    step(f, pl, if up { 1 } else { -1 });
                }
                let w = wheel_steps(ui, &r);
                if w != 0 {
                    f.st.sel = Some(i);
                    step(f, pl, w);
                }
                r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(
                    "Click the top half to step this digit up, the bottom half down, or scroll \
                     over it. With the pointer on the dial the arrow keys step it, and typing a \
                     number starts F INPUT.",
                );
            } else {
                r.on_hover_cursor(CursorIcon::NotAllowed).on_hover_text(f.lock.unwrap_or_default());
            }
        }
        seven(&p, pos2(x, top), k, th.skew, *d, ink, ghost);
        x += (DW + GAP) * k;
        if i < n - 1 && (n - 1 - i).is_multiple_of(3) {
            p.rect_filled(
                Rect::from_min_size(
                    pos2(x - 2.0 * k, top + (DH - 5.0) * k),
                    vec2(4.0 * k, 4.0 * k),
                ),
                0.0,
                ink,
            );
            x += GAP * k;
        }
    }
    let dial = Rect::from_min_max(pos2(inner.left(), y), pos2(x, y + digits_h));
    keys_on_dial(ui, f, dial, n);

    // The mode, on the LCD: opens MODE.
    let mode = f.s.mode.unwrap_or("—");
    let text =
        if f.st.digi.is_some() && mode == "USB" { "USB-D".to_string() } else { mode.to_string() };
    let mr = Rect::from_min_size(
        pos2(inner.right() - mode_w + z(6.0), y + digits_h - z(30.0)),
        vec2(mode_w - z(6.0), z(24.0)),
    );
    let r = ui.interact(
        mr,
        ui.id().with("radio-lcd-mode"),
        if locked { Sense::hover() } else { Sense::click() },
    );
    if !locked {
        p.rect_stroke(
            mr,
            CornerRadius::same(3),
            Stroke::new(1.0, ink.gamma_multiply(0.35)),
            StrokeKind::Inside,
        );
    }
    p.text(mr.center(), egui::Align2::CENTER_CENTER, text, label_font(17.0), ink);
    if r.clicked() {
        f.st.toggle(Pop::Mode);
    }
    if !locked {
        r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Change mode");
    }
    y += digits_h + z(6.0);

    // The segment meter: S in receive, forward power in transmit.
    let tx = f.s.transmitting();
    let nseg = 30;
    let (key, lit, val) = if tx {
        let max = f.s.power_steps.iter().map(|p| p.watts).fold(1.0, f32::max);
        match f.s.fwd_w() {
            Some(w) => ("PWR", ((w / max) * nseg as f32).round() as usize, format!("{w:.1} W")),
            None => ("PWR", 0, "— W".into()),
        }
    } else {
        match logic::s_dbm(f.p, f.s) {
            Some(d) => ("S", logic::s_segments(d, nseg), logic::s_units(d)),
            None => ("S", 0, "—".into()),
        }
    };
    let font = label_font(11.5);
    p.text(pos2(inner.left(), y + z(7.0)), egui::Align2::LEFT_CENTER, key, font.clone(), ink);
    p.text(pos2(inner.right(), y + z(7.0)), egui::Align2::RIGHT_CENTER, &val, font, ink);
    let (l, r) = (inner.left() + z(34.0), inner.right() - z(62.0));
    let sw = (r - l) / nseg as f32;
    for i in 0..nseg {
        let sr =
            Rect::from_min_size(pos2(l + i as f32 * sw, y + z(1.0)), vec2(sw - z(2.0), z(12.0)));
        p.rect_filled(sr, 1.0, if i < lit.min(nseg) { ink.gamma_multiply(0.9) } else { ghost });
    }
    y += z(14.0 + 6.0);

    // The text line: power setting or TX, RIT, level or SWR.
    let mono = data_font(15.0);
    let left = if let Some(t) = f.s.tuning {
        match t.keying {
            rigcat::Keying::Audio => "TX TUNE".to_string(),
            rigcat::Keying::Carrier => "RIG TUNE".to_string(),
        }
    } else {
        format!("TX PWR {}", f.s.power().map_or("—", |p| p.label))
    };
    p.text(pos2(inner.left(), y + z(9.0)), egui::Align2::LEFT_CENTER, left, mono.clone(), ink);
    if f.s.rit_on == Some(true) {
        let rit =
            logic::rit_hz(f.s).map_or("RIT".into(), |h| format!("RIT {:+.2}", h as f32 / 1000.0));
        p.text(
            pos2(inner.center().x, y + z(9.0)),
            egui::Align2::CENTER_CENTER,
            rit,
            mono.clone(),
            ink,
        );
    }
    let right = if tx {
        f.s.swr_ratio().map_or("SWR —".into(), |s| format!("SWR {s:.2}"))
    } else {
        logic::s_dbm(f.p, f.s).map_or(String::new(), |d| format!("{d:.0} dBm"))
    };
    p.text(pos2(inner.right(), y + z(9.0)), egui::Align2::RIGHT_CENTER, right, mono, ink);
}

/// The arrow keys and typed digits, with the pointer on the dial.
fn keys_on_dial(ui: &mut Ui, f: &mut Face<'_>, dial: Rect, n: usize) {
    if f.lock.is_some() || ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let over = ui.rect_contains_pointer(dial);
    if !over {
        return;
    }
    let (left, right, up, down, typed) = ui.input(|i| {
        let typed: String = i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Text(t) if t.chars().all(|c| c.is_ascii_digit() || c == '.') => {
                    Some(t.clone())
                }
                _ => None,
            })
            .collect();
        (
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            typed,
        )
    });
    let first = (0..n).find(|i| logic::steppable(*i, n, f.xvtr)).unwrap_or(0);
    let sel = f.st.sel.unwrap_or(n.saturating_sub(5)).max(first);
    if left {
        f.st.sel = Some(sel.saturating_sub(1).max(first));
    }
    if right {
        f.st.sel = Some((sel + 1).min(n - 1));
    }
    if up || down {
        f.st.sel = Some(sel);
        step(f, logic::place(sel, n), if up { 1 } else { -1 });
    }
    if !typed.is_empty() {
        f.st.pop = Some(Pop::Freq);
        f.st.entry = typed;
        f.st.entry_msg = None;
        f.st.focus_entry = true;
    }
}

/// The LCD's annunciators. ATT, LP, ANT and IN are keys.
fn annunciators(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs, at: Pos2, width: f32) {
    let s = f.s;
    let v = |k: &str| s.value(k);
    let ant = match v("AN") {
        Some("2") => "ANT 2",
        _ => "ANT 1",
    };
    let input = match v("TI") {
        Some("0") => "IN MIC",
        Some("2") => "IN AUTO",
        _ => "IN USB",
    };
    let vfo = match s.vfo {
        Some(1) => "VFO B",
        Some(2) => "MEM",
        _ => "VFO A",
    };
    let tx = s.transmitting();
    // The receive filter's width, always (blank until the radio answers).
    let fil = logic::filter_hz(f.p, s).map_or("FIL ----".to_string(), logic::filter_text);
    // (text, lit, the control it changes)
    let items: [(&str, bool, Option<&str>); 12] = [
        (&fil, v("RF").is_some(), None),
        ("LP", v("LP") == Some("1"), Some("LP")),
        ("ATT", v("AT") == Some("1"), Some("AT")),
        (ant, v("AN").is_some(), Some("AN")),
        (input, v("TI").is_some(), Some("TI")),
        ("CAT", s.connected, None),
        ("SPLIT", s.split == Some(true), None),
        ("RIT", s.rit_on == Some(true), None),
        (vfo, s.vfo.is_some(), None),
        ("XVTR", prefs.xvtr_hz != 0, None),
        ("RX", s.connected && !tx, None),
        ("TX", tx, None),
    ];
    let ink = f.th.lcd_ink;
    let font = label_font(11.0);
    let mut x = at.x;
    for (text, lit, cmd) in items {
        let a = if lit { 0.92 } else { 0.14 };
        let g = ui.painter().layout_no_wrap(text.to_string(), font.clone(), ink.gamma_multiply(a));
        let r = Rect::from_min_size(pos2(x, at.y), vec2(g.size().x, z(14.0)));
        if r.right() > at.x + width {
            break;
        }
        if let Some(c) = cmd.filter(|_| f.lock.is_none())
            && logic::find(f.p, c).is_some_and(|c| logic::settable(f.p, c))
        {
            let resp = ui.interact(r.expand(2.0), ui.id().with(("radio-ann", c)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_stroke(
                    r.expand(2.0),
                    2.0,
                    Stroke::new(1.0, ink.gamma_multiply(0.5)),
                    StrokeKind::Inside,
                );
            }
            if resp.clicked() {
                annunciator_click(f, c);
            }
            let hover = match c {
                "LP" => "Receiver low-pass filter (menu 2): click to switch",
                "AT" => "Receiver attenuator (menu 1): click to switch",
                "AN" => "Antennas (menu 31): click for RTX only, or receive on RX",
                _ => "Where the radio takes transmit audio from (menu 32): click to choose",
            };
            resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(hover);
        }
        ui.painter().galley(r.min, g, ink.gamma_multiply(a));
        x = r.right() + z(10.0);
    }
}

/// The LCD's second row: AGC, noise blanker and reduction, notch, squelch,
/// and in split the frequency the radio transmits on. Read only; the RX
/// pop-out sets them. Each is read with the settings (every 10 s).
fn receive_row(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs, at: Pos2, width: f32) {
    let s = f.s;
    let rb = |cmd: &str| logic::find(f.p, cmd).and_then(|c| logic::readback(c, s));
    let on = |cmd: &str| rb(cmd).is_some_and(|v| !v.trim().trim_start_matches('0').is_empty());
    let agc = match rb("GC") {
        Some("1") => "MAN GAIN",
        _ => "AGC",
    };
    let tx_on = s.freq_b_hz.filter(|_| s.split == Some(true)).map(|hz| {
        let khz = hz as f64 / 1e3;
        format!("TX {khz:.3}")
    });
    let items: Vec<(String, bool)> = vec![
        (agc.to_string(), rb("GC").is_some()),
        ("NB".into(), on("NB")),
        ("NR".into(), on("NR")),
        ("NOTCH".into(), on("NO")),
        ("SQL".into(), on("SQ")),
        (tx_on.clone().unwrap_or_else(|| "TX ----".into()), tx_on.is_some()),
        // As marked in SETTINGS: the radio doesn't report it.
        ("REF 10M".into(), prefs.ref_10mhz),
    ];
    let ink = f.th.lcd_ink;
    let font = label_font(11.0);
    let mut x = at.x;
    for (text, lit) in items {
        let a = if lit { 0.92 } else { 0.14 };
        let g = ui.painter().layout_no_wrap(text, font.clone(), ink.gamma_multiply(a));
        let r = Rect::from_min_size(pos2(x, at.y), vec2(g.size().x, z(14.0)));
        if r.right() > at.x + width {
            break;
        }
        ui.painter().galley(r.min, g, ink.gamma_multiply(a));
        x = r.right() + z(10.0);
    }
}

fn annunciator_click(f: &mut Face<'_>, cmd: &'static str) {
    let Some(c) = logic::find(f.p, cmd) else { return };
    let to = match (cmd, logic::readback(c, f.s)) {
        ("TI", _) => {
            f.st.confirm = Some(super::Confirm::Input);
            return;
        }
        ("AN", Some("2")) => "1",
        ("AN", _) => "2",
        (_, Some("1")) => "0",
        _ => "1",
    };
    f.send_control(c, to);
}

/// The knobs beside the LCD. The wheel over the big one tunes by the digit
/// last picked (100 Hz until one is).
pub(crate) fn knobs(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs) {
    let (rect, resp) = ui.allocate_exact_size(vec2(z(104.0), z(150.0)), Sense::hover());
    let p = ui.painter_at(rect);
    let knob = |c: Pos2, r: f32| {
        p.circle_filled(c + vec2(0.0, z(3.0)), r + z(1.0), Color32::from_black_alpha(140));
        let (dark, light) =
            (Color32::from_rgb(0x1c, 0x1e, 0x21), Color32::from_rgb(0x4d, 0x52, 0x59));
        p.circle_filled(c, r, dark);
        // A radial gradient, lit from the top left.
        let hl = c - vec2(r * 0.3, r * 0.4);
        for i in 0..10 {
            let t = i as f32 / 10.0;
            p.circle_filled(c + (hl - c) * t, r * 0.8 * (1.0 - t), dark.lerp_to_gamma(light, t));
        }
        p.circle_stroke(c, r, Stroke::new(1.0, Color32::from_gray(10)));
        p.line_segment(
            [c - vec2(0.0, r * 0.92), c - vec2(0.0, r * 0.74)],
            Stroke::new(z(2.0), f.th.label),
        );
    };
    let big = pos2(rect.center().x, rect.top() + z(48.0));
    knob(big, z(46.0));
    let (l, r) = (
        pos2(rect.left() + z(26.0), rect.top() + z(118.0)),
        pos2(rect.right() - z(26.0), rect.top() + z(118.0)),
    );
    knob(l, z(20.0));
    knob(r, z(20.0));
    let font = label_font(10.5);
    p.text(l + vec2(0.0, z(26.0)), egui::Align2::CENTER_CENTER, "E1", font.clone(), f.th.dim);
    p.text(r + vec2(0.0, z(26.0)), egui::Align2::CENTER_CENTER, "E2", font, f.th.dim);
    if f.lock.is_none() && logic::dial_hz(f.s).is_some() {
        let over = resp.hover_pos().is_some_and(|q| q.distance(big) <= z(46.0));
        let hover = if over { resp.clone() } else { resp.clone().with_new_rect(Rect::NOTHING) };
        let w = if over { wheel_steps(ui, &hover) } else { 0 };
        if w != 0 {
            let n = logic::digit_count(prefs.xvtr_hz);
            let pl = f.st.sel.map_or(100, |i| logic::place(i, n));
            step(f, pl, w);
        }
        if over {
            resp.on_hover_text(
                "Scroll to tune by the digit last picked on the dial (100 Hz until one is).",
            );
        }
    }
}
