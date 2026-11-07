//! The radio's face: its LCD, with the dial as clickable seven-segment
//! digits, the annunciators, the segment meter and the text line, and the
//! knobs beside it; and the key caps everything else on the window wears.
//! Colours and slant come from the [`RigTheme`].

use std::time::Instant;

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Mesh, Pos2, Rect, Response, Sense,
    Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::knobs;
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

/// The LCD, `width` wide. Returns its height.
pub(crate) fn lcd(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs, width: f32) -> f32 {
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
    // The radio's E1+E2 lock holds the dial as well as the knobs.
    let dial_locked = locked || f.st.knobs.locked;
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
            let r = ui.interact(dr, id, if dial_locked { Sense::hover() } else { Sense::click() });
            let sel = f.st.sel == Some(i);
            if !dial_locked && (r.hovered() || sel) {
                p.rect_filled(dr, CornerRadius::same(3), ink.gamma_multiply(0.08));
            }
            if !dial_locked {
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
                r.on_hover_cursor(CursorIcon::NotAllowed)
                    .on_hover_text(f.lock.unwrap_or(LOCKED_NOTE));
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
        match f.rx_dbm.or_else(|| logic::s_dbm(f.p, f.s)) {
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
        f.rx_dbm.or_else(|| logic::s_dbm(f.p, f.s)).map_or(String::new(), |d| format!("{d:.0} dBm"))
    };
    p.text(pos2(inner.right(), y + z(9.0)), egui::Align2::RIGHT_CENTER, right, mono, ink);
    height
}

/// The arrow keys and typed digits, with the pointer on the dial.
fn keys_on_dial(ui: &mut Ui, f: &mut Face<'_>, dial: Rect, n: usize) {
    if f.lock.is_some() || f.st.knobs.locked || ui.ctx().egui_wants_keyboard_input() {
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

/// The knob column's width at scale 1.
pub(crate) const KNOBS_W: f32 = 140.0;

/// The main knob's diameter as a share of the LCD's height, and E1's and
/// E2's as a share of the main knob's, as on the FDM-DUO's face (decision
/// 53).
const MAIN_OF_LCD: f32 = 0.58;
const SMALL_OF_MAIN: f32 = 0.4;

/// What the dial and knobs say while the radio's E1+E2 lock is on.
const LOCKED_NOTE: &str = "Locked, as with E1 and E2 pressed together on the radio. Hold E1 or E2 \
                           to unlock.";

/// Brushed stainless steel, darker than satin aluminium (AJ3G, 22 Oct 2026:
/// "a darker, silver color. Almost like stainless steel", keeping the black
/// knobs' reflection on top): the rim, the body's shadow and highlight, and
/// the mark.
const SILVER_RIM: Color32 = Color32::from_rgb(0x45, 0x49, 0x4f);
const SILVER_LOW: Color32 = Color32::from_rgb(0x5a, 0x5f, 0x66);
const SILVER_HIGH: Color32 = Color32::from_rgb(0xc4, 0xc9, 0xcf);
const SILVER_EDGE: Color32 = Color32::from_rgb(0x1e, 0x20, 0x23);
const KNOB_MARK: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x1f);

/// A silver knob at `c`, radius `r`, its mark turned `angle` from the top.
/// Lit from the top left as the case's keys are, with concentric machining
/// rings, a knurled skirt and a darker rim. Lighter with a blue ring under
/// the pointer; dimmed when it can't be used.
fn silver_knob(
    p: &egui::Painter,
    th: &RigTheme,
    c: Pos2,
    r: f32,
    angle: f32,
    hover: bool,
    live: bool,
) {
    let lift = |col: Color32| if hover { col.lerp_to_gamma(Color32::WHITE, 0.12) } else { col };
    p.circle_filled(c + vec2(0.0, z(3.0)), r + z(1.0), Color32::from_black_alpha(150));
    p.circle_filled(c, r, lift(SILVER_RIM));
    // The knurled skirt: fine ridges round the rim.
    let ridges = ((r * 1.4) as usize).clamp(24, 96);
    for i in 0..ridges {
        let a = i as f32 / ridges as f32 * std::f32::consts::TAU + angle;
        let d = vec2(a.sin(), -a.cos());
        p.line_segment(
            [c + d * r * 0.86, c + d * (r - 1.0)],
            Stroke::new(1.0, Color32::from_black_alpha(55)),
        );
    }
    // The face: a radial gradient towards the highlight, which stays a
    // spot near the top left as the black knobs' did, so it reads as a
    // reflection on steel rather than a lit surface.
    let body = r * 0.84;
    let hl = c - vec2(body * 0.3, body * 0.4);
    let n = 16;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let col = SILVER_LOW.lerp_to_gamma(SILVER_HIGH, t.powf(1.6));
        p.circle_filled(c + (hl - c) * t, body * (1.0 - t), lift(col));
    }
    // The glint at its heart.
    p.circle_filled(hl, body * 0.1, Color32::from_white_alpha(90));
    // Machining rings, faint.
    for k in 1..9 {
        let rr = body * k as f32 / 9.0;
        let a =
            if k % 2 == 0 { Color32::from_white_alpha(22) } else { Color32::from_black_alpha(14) };
        p.circle_stroke(c, rr, Stroke::new(0.7, a));
    }
    p.circle_stroke(c, body, Stroke::new(1.0, Color32::from_white_alpha(110)));
    p.circle_stroke(c, r, Stroke::new(1.0, SILVER_EDGE));
    // The mark, dark on the silver.
    let d = vec2(angle.sin(), -angle.cos());
    p.line_segment([c + d * body * 0.88, c + d * body * 0.58], Stroke::new(z(2.5), KNOB_MARK));
    if hover {
        p.circle_stroke(c, r + z(2.0), Stroke::new(z(1.5), th.cap_lit.gamma_multiply(0.7)));
    }
    if !live {
        p.circle_filled(c, r + 0.5, th.case.gamma_multiply(0.55));
    }
}

/// One knob's input this frame.
struct KnobIn {
    resp: Response,
    hovered: bool,
    steps: i64,
    press: Option<knobs::Press>,
}

/// Read knob `k` (0 main, 1 E1, 2 E2) at `c`, radius `r`: the wheel over
/// it, a drag round it (`drag`; all three since AJ3G couldn't turn E1, 31
/// Oct 2026), and a push or hold.
fn knob_input(
    ui: &Ui,
    f: &mut Face<'_>,
    k: usize,
    c: Pos2,
    r: f32,
    live: bool,
    drag: bool,
) -> KnobIn {
    let rect = Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r));
    let sense = match (live, drag) {
        (false, _) => Sense::hover(),
        (true, true) => Sense::click_and_drag(),
        (true, false) => Sense::click(),
    };
    let resp = ui.interact(rect, ui.id().with(("radio-knob", k)), sense);
    let hovered = resp.hover_pos().is_some_and(|q| q.distance(c) <= r);
    let ks = &mut f.st.knobs;
    let mut steps = 0;
    if live && hovered {
        steps += ui.ctx().input_mut(|i| {
            let mut n = 0;
            for e in &i.events {
                if let egui::Event::MouseWheel { unit, delta, .. } = e {
                    let u = match unit {
                        egui::MouseWheelUnit::Point => knobs::WheelUnit::Point,
                        egui::MouseWheelUnit::Line => knobs::WheelUnit::Line,
                        egui::MouseWheelUnit::Page => knobs::WheelUnit::Page,
                    };
                    n += knobs::wheel_to_steps(u, delta.y, &mut ks.wheel[k]);
                }
            }
            // The wheel is the knob's: the window doesn't scroll as well.
            i.smooth_scroll_delta = egui::Vec2::ZERO;
            n
        });
    }
    if live && drag && resp.dragged() {
        if let Some(q) = resp.interact_pointer_pos() {
            let v = q - c;
            // Too near the centre the angle means nothing.
            if v.length() > r * 0.2 {
                let a = v.y.atan2(v.x);
                if let Some(last) = ks.drag_last[k] {
                    steps += knobs::angle_to_steps(a - last, &mut ks.drag[k]);
                }
                ks.drag_last[k] = Some(a);
            }
        }
    } else if drag {
        ks.drag_last[k] = None;
        ks.drag[k] = 0.0;
    }
    if steps != 0 {
        ks.press[k].cancel();
        ks.turned[k] += steps as f32 * knobs::DRAG_STEP_RAD;
    }
    let down = live && resp.is_pointer_button_down_on();
    let now = ui.input(|i| i.time);
    let press = ks.press[k].update(down, now, knobs::hold_s(f.s));
    if ks.press[k].waiting() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    KnobIn { resp, hovered, steps, press }
}

/// A setting turned to lately, if the radio hasn't read it back yet, else
/// the read-back: so quick turns add up.
fn knob_base(f: &Face<'_>, key: &'static str, rb: Option<i32>) -> Option<i32> {
    match f.st.knobs.pending.get(key) {
        Some((v, t)) if Some(*v) != rb && t.elapsed().as_secs_f32() < 1.5 => Some(*v),
        _ => rb,
    }
}

/// The dial digit the main knob tunes by, the one last picked, or the
/// 100 Hz digit.
fn tune_step(f: &Face<'_>, n: usize) -> u64 {
    f.st.sel.map_or(knobs::STEP_DEFAULT, |i| logic::place(i, n))
}

/// Move the selected dial digit `by` places (right positive), inside the
/// digits that step.
fn move_digit(f: &mut Face<'_>, n: usize, by: i64) {
    let first = (0..n).find(|i| logic::steppable(*i, n, f.xvtr)).unwrap_or(0) as i64;
    let now =
        f.st.sel.map_or(knobs::digit_of(knobs::STEP_DEFAULT, n).unwrap_or(0) as i64, |i| i as i64);
    f.st.sel = Some((now + by).clamp(first, n as i64 - 1) as usize);
}

/// The main knob: tune, or in MEM pick the channel; in STEP change the
/// step; digit by digit change the selected digit.
fn main_knob(f: &mut Face<'_>, n: usize, input: &KnobIn) {
    use knobs::{MainMode, Press};
    let ks = f.st.knobs.main;
    if input.steps != 0 {
        match ks {
            MainMode::Step => {
                let to = knobs::next_step(tune_step(f, n), input.steps);
                f.st.sel = knobs::digit_of(to, n).or(f.st.sel);
            }
            MainMode::Digit => {
                if f.st.sel.is_none() {
                    move_digit(f, n, 0);
                }
                step(f, tune_step(f, n), input.steps);
            }
            MainMode::Tune if f.s.vfo == Some(2) => {
                let now = f.s.value("MC").and_then(|v| v.trim().parse::<u16>().ok());
                let base = knob_base(f, "MC", now.map(i32::from)).unwrap_or(0) as u16;
                let to = knobs::next_memory(base, input.steps);
                if let Some(fr) = f.p.recall_memory(to) {
                    f.out.frames.push(fr);
                    f.st.knobs.pending.insert("MC", (i32::from(to), Instant::now()));
                }
            }
            MainMode::Tune => step(f, tune_step(f, n), input.steps),
        }
    }
    f.st.knobs.main = match (ks, input.press) {
        (MainMode::Tune, Some(Press::Push)) => MainMode::Step,
        (MainMode::Step, Some(Press::Push)) => MainMode::Tune,
        (MainMode::Tune | MainMode::Step, Some(Press::Hold)) => {
            if f.st.sel.is_none() {
                move_digit(f, n, 0);
            }
            MainMode::Digit
        }
        (MainMode::Digit, Some(_)) => MainMode::Tune,
        (m, None) => m,
    };
}

/// E1: change its setting, or push for the next one; digit by digit, move
/// the selected digit.
fn e1_knob(f: &mut Face<'_>, n: usize, input: &KnobIn) {
    use knobs::{E1, MainMode, Press};
    let digit = f.st.knobs.main == MainMode::Digit;
    match input.press {
        Some(Press::Push) if digit => move_digit(f, n, -1),
        Some(Press::Push) => f.st.knobs.e1 = f.st.knobs.e1.next(),
        Some(Press::Hold) if digit => f.st.knobs.main = MainMode::Tune,
        Some(Press::Hold) => f.st.knobs.locked = !f.st.knobs.locked,
        None => {}
    }
    if input.steps == 0 {
        return;
    }
    if digit {
        move_digit(f, n, input.steps);
        return;
    }
    let e = f.st.knobs.e1;
    // With an outside speaker the DUO's own is muted, and VOL turns the
    // panel's Sound (AJ3G, 31 Oct 2026).
    if e == E1::Vol
        && let Some(v) = f.panel_vol
    {
        let to = (v + input.steps as f32 * 0.01).clamp(0.0, 1.0);
        f.out.panel_volume = Some(to);
        return;
    }
    let Some(from) = knob_base(f, e.name(), knobs::e1_value(f.p, f.s, e)) else {
        f.st.note = Some(format!("E1: the radio hasn't answered {} yet.", e.name()));
        return;
    };
    let (lo, hi) = knobs::e1_range(e);
    let to = if e == E1::Agc {
        i32::from(input.steps > 0)
    } else {
        (from + input.steps as i32).clamp(lo, hi)
    };
    match knobs::e1_frames(f.p, f.s, e, from, to) {
        Ok(v) if !v.is_empty() => {
            f.out.frames.extend(v);
            f.st.knobs.pending.insert(e.name(), (to, Instant::now()));
        }
        Ok(_) => {}
        Err(err) => f.st.note = Some(err),
    }
}

/// E2: change the filter, CW pitch or RIT offset, or push for the next;
/// digit by digit, move the selected digit.
fn e2_knob(f: &mut Face<'_>, n: usize, input: &KnobIn) {
    use knobs::{E2, MainMode, Press};
    let digit = f.st.knobs.main == MainMode::Digit;
    match input.press {
        Some(Press::Push) if digit => move_digit(f, n, 1),
        Some(Press::Push) => f.st.knobs.e2 = f.st.knobs.e2.next(f.s),
        Some(Press::Hold) if digit => f.st.knobs.main = MainMode::Tune,
        Some(Press::Hold) => f.st.knobs.locked = !f.st.knobs.locked,
        None => {}
    }
    if input.steps == 0 {
        return;
    }
    if digit {
        move_digit(f, n, input.steps);
        return;
    }
    let d = input.steps;
    match f.st.knobs.e2.now(f.s) {
        E2::Filter => {
            let ws = knobs::widths(f.p, f.s);
            let now = knob_base(f, "FIL", logic::filter_hz(f.p, f.s).map(|w| w as i32));
            let (Some(now), Some(mode)) = (now, f.s.mode) else { return };
            if let Some(w) = knobs::next_width(&ws, now as u32, d)
                && w as i32 != now
                && let Some(fr) = f.p.set_filter(mode, w)
            {
                f.out.frames.push(fr);
                f.st.knobs.pending.insert("FIL", (w as i32, Instant::now()));
            }
        }
        E2::Pitch => {
            let Some(c) = logic::find(f.p, "PI") else { return };
            let rb = logic::readback(c, f.s).and_then(|v| v.trim().parse().ok());
            let Some(now) = knob_base(f, "PITCH", rb) else { return };
            let to = (now + 10 * d as i32).clamp(0, 1000);
            if to != now {
                f.send_control(c, &to.to_string());
                f.st.knobs.pending.insert("PITCH", (to, Instant::now()));
            }
        }
        E2::Rit => {
            let now = knob_base(f, "RIT", logic::rit_hz(f.s)).unwrap_or(0);
            let to = (now + 10 * d as i32).clamp(-50_000, 50_000);
            if to != now {
                let v = f.p.set_rit(to);
                f.out.frames.extend(v);
                f.st.knobs.pending.insert("RIT", (to, Instant::now()));
            }
        }
    }
}

/// E2's setting in words, as its caption shows it.
fn e2_text(f: &Face<'_>) -> String {
    use knobs::E2;
    let e = f.st.knobs.e2.now(f.s);
    let v = match e {
        E2::Filter => logic::filter_hz(f.p, f.s).map(logic::filter_text),
        E2::Pitch => logic::find(f.p, "PI")
            .and_then(|c| logic::readback(c, f.s))
            .and_then(|v| v.trim().parse::<i32>().ok())
            .map(|v| format!("PITCH {v}")),
        E2::Rit => logic::rit_hz(f.s).map(|h| format!("RIT {:+.2}", h as f32 / 1000.0)),
    };
    v.unwrap_or_else(|| format!("{} —", e.name()))
}

/// The knobs beside the LCD, `height` tall as it is: the main tuning knob
/// over E1 and E2, silver as the radio's. Each turns with the wheel over
/// it, the main knob also by dragging round it; a click is the radio's
/// push, a press held past its hold time (menu 71) the radio's push and
/// hold. See [`knobs`] for what each does.
pub(crate) fn knobs(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs, height: f32) {
    use knobs::{E1, E2, MainMode};
    let w = z(KNOBS_W);
    let (rect, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let line = z(13.0);
    // The main knob as large as the LCD's share allows, E1 and E2 under it
    // with a line of status between and their captions below.
    let fit = (height - z(2.0 + 3.0 + 4.0 + 3.0) - 2.0 * line) / (1.0 + SMALL_OF_MAIN);
    let d_main = (height * MAIN_OF_LCD).min(fit).min(w - z(8.0));
    let d_small = d_main * SMALL_OF_MAIN;
    let main_c = pos2(rect.center().x, rect.top() + z(2.0) + d_main / 2.0);
    let status_y = main_c.y + d_main / 2.0 + z(3.0) + line / 2.0;
    let small_y = status_y + line / 2.0 + z(4.0) + d_small / 2.0;
    let e1_c = pos2(rect.center().x - w / 4.0, small_y);
    let e2_c = pos2(rect.center().x + w / 4.0, small_y);
    let cap_y = small_y + d_small / 2.0 + z(3.0) + line / 2.0;

    // SAFETY INTERLOCK (#5, panel side): no knob changes anything while the
    // radio transmits or isn't connected; they grey with the rest of the
    // window, and the link would refuse the frames anyway.
    let live = f.lock.is_none();
    let n = logic::digit_count(prefs.xvtr_hz);
    let tuning = logic::dial_hz(f.s).is_some();
    let main_in = knob_input(ui, f, 0, main_c, d_main / 2.0, live && tuning, true);
    let e1_in = knob_input(ui, f, 1, e1_c, d_small / 2.0, live, true);
    let e2_in = knob_input(ui, f, 2, e2_c, d_small / 2.0, live, true);
    if live {
        if f.st.knobs.locked {
            // Locked, only a hold of E1 or E2 does anything: it unlocks.
            let hold = Some(knobs::Press::Hold);
            if e1_in.press == hold || e2_in.press == hold {
                f.st.knobs.locked = false;
            }
        } else {
            if tuning {
                main_knob(f, n, &main_in);
            }
            e1_knob(f, n, &e1_in);
            e2_knob(f, n, &e2_in);
        }
    }

    // Draw.
    let p = ui.painter_at(rect.expand(z(4.0)));
    let ks = &f.st.knobs;
    let th = f.th;
    let main_live = live && tuning && !ks.locked;
    silver_knob(
        &p,
        th,
        main_c,
        d_main / 2.0,
        ks.turned[0],
        main_in.hovered && main_live,
        main_live,
    );
    let small_live = live;
    silver_knob(&p, th, e1_c, d_small / 2.0, ks.turned[1], e1_in.hovered && small_live, small_live);
    silver_knob(&p, th, e2_c, d_small / 2.0, ks.turned[2], e2_in.hovered && small_live, small_live);

    // The status line under the main knob: what it does now.
    let step = tune_step(f, n);
    let (status, lit) = if ks.locked {
        ("LOCKED".to_string(), true)
    } else {
        match ks.main {
            MainMode::Step => (format!("STEP {}", knobs::step_text(step)), true),
            MainMode::Digit => ("DIGIT BY DIGIT".to_string(), true),
            MainMode::Tune if f.s.vfo == Some(2) => {
                let ch = f.s.value("MC").map_or("—".to_string(), |v| v.trim().to_string());
                (format!("MEM CH {ch}"), false)
            }
            MainMode::Tune => (format!("STEP {}", knobs::step_text(step)), false),
        }
    };
    let col = if lit { th.cap_lit } else { th.dim };
    p.text(pos2(main_c.x, status_y), egui::Align2::CENTER_CENTER, &status, label_font(10.0), col);
    let e1 = ks.e1;
    let e1_text = match f.panel_vol {
        // An outside speaker: VOL is the panel's Sound.
        Some(v) if e1 == knobs::E1::Vol => format!("VOL {:.0} PC", v * 100.0),
        _ => knobs::e1_text(e1, knobs::e1_value(f.p, f.s, e1)),
    };
    let e2_text = e2_text(f);
    let font = label_font(9.5);
    p.text(
        pos2(e1_c.x, cap_y),
        egui::Align2::CENTER_CENTER,
        format!("E1 {e1_text}"),
        font.clone(),
        th.dim,
    );
    p.text(pos2(e2_c.x, cap_y), egui::Align2::CENTER_CENTER, format!("E2 {e2_text}"), font, th.dim);

    // Pointer and hover notes.
    let hold = knobs::hold_s(f.s);
    let digit = ks.main == MainMode::Digit;
    let locked = ks.locked;
    let rx_vfo = if logic::rx_vfo(f.s) == rigcat::Vfo::B { "VFO B" } else { "VFO A" };
    let main_note = if locked {
        LOCKED_NOTE.to_string()
    } else {
        match ks.main {
            MainMode::Tune if f.s.vfo == Some(2) => format!(
                "Main knob, MEM: scroll or drag round to recall the next or previous memory \
                 channel (CAT MC). Click: STEP. Hold {hold:.1} s: digit by digit."
            ),
            MainMode::Tune => format!(
                "Main knob: scroll or drag round to tune {rx_vfo} by {} (CAT {}). Click: STEP, \
                 to choose the step. Hold {hold:.1} s: digit by digit.",
                knobs::step_text(step),
                if rx_vfo == "VFO B" { "FB" } else { "FA" },
            ),
            MainMode::Step => format!(
                "STEP: scroll to change the tuning step, now {}. The step is the panel's: the \
                 radio's own step has no known CAT command. Click to go back to tuning.",
                knobs::step_text(step)
            ),
            MainMode::Digit => "Digit by digit: scroll to change the selected digit; E1 and E2 \
                                move the selection. Click or hold to go back to tuning."
                .to_string(),
        }
    };
    let lock_tail = "Hold: lock the knobs and dial (the radio's E1 and E2 pressed together).";
    let e1_note = if locked {
        LOCKED_NOTE.to_string()
    } else if digit {
        "E1, digit by digit: scroll or click to move the selected digit (click: one left). \
         Hold: back to tuning."
            .to_string()
    } else {
        let what = match e1 {
            E1::Vol => "the radio's volume (CAT VM)",
            E1::Sql => "the squelch (CAT SQ)",
            E1::Agc => {
                "AGC or manual gain (CAT GC); the AGC speed and gain level have no known \
                        CAT encoding, so set them on the radio"
            }
            E1::Nr => {
                "the DUO's noise reduction, off or its level (CAT NR, NC). It works on the \
                 DUO's own audio, heard only at SPAN 192k; wider, use NR on SCOPE"
            }
            E1::Nb => "the noise blanker, off or its level (CAT NB, NK)",
        };
        format!(
            "E1: {e1_text}. Scroll or drag round to change {what}. Click: the next of VOL, SQL, AGC, NR, NB. \
             {lock_tail}"
        )
    };
    let e2_note = if locked {
        LOCKED_NOTE.to_string()
    } else if digit {
        "E2, digit by digit: scroll or click to move the selected digit (click: one right). \
         Hold: back to tuning."
            .to_string()
    } else {
        let what = match ks.e2.now(f.s) {
            E2::Filter if knobs::widths(f.p, f.s).is_empty() => {
                "the receive filter: not in this mode, whose filter codes aren't known"
            }
            E2::Filter => "the receive filter (CAT RF)",
            E2::Pitch => "the CW pitch, 10 Hz a step (CAT PI)",
            E2::Rit => "the RIT offset, 10 Hz a step (CAT RU, RD)",
        };
        format!(
            "E2: {e2_text}. Scroll or drag round to change {what}. Click: the next of FIL, PITCH (CW) and RIT \
             (while RIT is on). {lock_tail}"
        )
    };
    for (input, note, ok) in
        [(main_in, main_note, live && tuning), (e1_in, e1_note, live), (e2_in, e2_note, live)]
    {
        if !input.hovered {
            continue;
        }
        if ok {
            input.resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(note);
        } else {
            let why = f.lock.unwrap_or("The radio's dial hasn't been read yet.");
            input.resp.on_hover_cursor(CursorIcon::NotAllowed).on_hover_text(why);
        }
    }
}
