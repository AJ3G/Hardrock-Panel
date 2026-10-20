//! The controls under the TFT: the DUO-ART-style soft keys, then rows of
//! chips and sliders for everything else, as in the mockup.

use eframe::egui::{self, Align2, Color32, CursorIcon, Rect, RichText, Sense, Stroke, pos2, vec2};

use super::bandplan::{self, DataMode, Use};
use super::levels::s_unit;
use super::paint::{Ctx, fft_label};
use super::palette::Palette;
use super::settings::{
    FFT_SIZES, FPS_CHOICES, FftOverlap, FftWindow, LEVEL_MAX, LEVEL_MIN, MIN_RANGE_DB, STEPS_HZ,
    Scale, TopView, WaterfallSpeed,
};
use super::view;
use super::{Scope, ScopeOut};
use crate::{chrome, theme};

/// The soft keys, in order.
const KEYS: [&str; 8] = ["SPAN", "FIT", "SCALE", "AVG", "PEAK", "PERSIST", "MKR", "PAUSE"];

/// The row of soft keys under the screen: a label above a key cap, the label
/// lit while the key's setting is on.
pub(super) fn soft_keys(
    sc: &mut Scope,
    ui: &mut egui::Ui,
    snap: &sdr::Snapshot,
    out: &mut ScopeOut,
) {
    ui.add_space(6.0);
    let w = ui.available_width();
    let gap = 8.0;
    let kw = ((w - gap * (KEYS.len() as f32 - 1.0)) / KEYS.len() as f32).max(40.0);
    let h = 44.0;
    let (row, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    for (i, k) in KEYS.iter().enumerate() {
        let r =
            Rect::from_min_size(pos2(row.left() + i as f32 * (kw + gap), row.top()), vec2(kw, h));
        let (text, on) = key_label(sc, k, snap);
        let lab_r = Rect::from_min_size(r.min, vec2(kw, 16.0));
        let cap =
            Rect::from_center_size(pos2(r.center().x, r.bottom() - 13.0), vec2(kw.min(64.0), 22.0));
        let resp = ui
            .interact(cap.union(lab_r), ui.id().with(("scope-key", i)), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(key_help(k));
        let p = ui.painter();
        let col = if on { theme::scope().accent } else { theme::TEXT() };
        p.text(lab_r.center(), Align2::CENTER_CENTER, text, egui::FontId::proportional(11.5), col);
        let fill =
            if resp.is_pointer_button_down_on() { theme::FILL_ACTIVE() } else { theme::FILL() };
        p.rect_filled(cap, 3.0, fill);
        p.rect_stroke(cap, 3.0, Stroke::new(1.0, theme::LINE_LIT()), egui::StrokeKind::Inside);
        if resp.hovered() {
            // Clickable things look clickable (AJ3G).
            p.rect_stroke(
                cap.expand(1.0),
                3.0,
                Stroke::new(1.5, theme::TEXT_STRONG()),
                egui::StrokeKind::Outside,
            );
        }
        if resp.clicked() {
            press(sc, k, snap, out);
        }
    }
}

fn key_label(sc: &Scope, k: &str, snap: &sdr::Snapshot) -> (String, bool) {
    let s = &sc.settings;
    match k {
        "SPAN" => (format!("SPAN {}k", (snap.sample_rate / 1000.0).round()), false),
        "FIT" => ("FIT".into(), s.fit),
        "SCALE" => (format!("SCALE {}", s.scale.key_label()), false),
        "AVG" => ("AVG".into(), s.avg),
        "PEAK" => ("PEAK".into(), s.peak),
        "PERSIST" => ("PERSIST".into(), s.persist),
        "MKR" => ("MKR→PEAK".into(), sc.markers.any()),
        "PAUSE" => ("PAUSE".into(), sc.paused),
        _ => (k.into(), false),
    }
}

fn key_help(k: &str) -> &'static str {
    match k {
        "SPAN" => "The next IQ span (sample rate). Above 192 kHz the DUO mutes its own audio",
        "FIT" => "Keep the floor just under the noise and the ceiling over the strongest signal",
        "SCALE" => "Levels in dBm, S-units, or both",
        "AVG" => "Average four frames",
        "PEAK" => "Peak-hold trace",
        "PERSIST" => "Phosphor trace: where the trace has been, fading",
        "MKR" => "Put the active marker on the strongest signal in view, and follow it",
        "PAUSE" => "Freeze the display; the stream and the receiver keep running",
        _ => "",
    }
}

fn press(sc: &mut Scope, k: &str, snap: &sdr::Snapshot, out: &mut ScopeOut) {
    let s = &mut sc.settings;
    match k {
        "SPAN" => {
            let rates = sdr::SUPPORTED_RATES;
            let cur = rates.iter().position(|&r| r as f64 == snap.sample_rate);
            let next = cur.map_or(0, |i| (i + 1) % rates.len());
            out.sample_rate = Some(rates[next]);
            sc.want_view = None;
        }
        "FIT" => s.fit = !s.fit,
        "SCALE" => s.scale = s.scale.next(),
        "AVG" => s.avg = !s.avg,
        "PEAK" => s.peak = !s.peak,
        "PERSIST" => {
            s.persist = !s.persist;
            sc.persist.clear();
        }
        "MKR" => {
            if let Some(f) = sc.shown().cloned() {
                let v = view::view(super::span_of(snap), sc.want_view);
                sc.markers.snap_to_peak(&f, v.lo, v.hi);
            }
        }
        "PAUSE" => sc.paused = !sc.paused,
        _ => {}
    }
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

fn note(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t).size(10.5).color(theme::TEXT()).weak());
}

fn chip(ui: &mut egui::Ui, on: bool, t: &str) -> egui::Response {
    chrome::chip(ui, on, RichText::new(t).size(12.0)).on_hover_cursor(CursorIcon::PointingHand)
}

/// Everything under the soft keys.
pub(super) fn rows(sc: &mut Scope, ui: &mut egui::Ui, cx: &Ctx, out: &mut ScopeOut) {
    ui.add_space(6.0);
    ui.separator();
    level_sliders(sc, ui);
    let tx = cx.rig.transmitting;

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Scale");
        for sv in Scale::ALL {
            if chip(ui, sc.settings.scale == sv, sv.label()).clicked() {
                sc.settings.scale = sv;
            }
        }
        note(ui, "· S9 = −73 dBm, 6 dB per S-unit");
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Scope");
        for t in TopView::ALL {
            if chip(ui, sc.settings.top == t, t.label()).clicked() {
                sc.settings.top = t;
                if t != TopView::Spectrum && sc.settings.split < 0.55 {
                    sc.settings.set_split(0.55);
                }
            }
        }
        caption(ui, "· 3D");
        ui.add_enabled_ui(sc.settings.top.has_surface(), |ui| {
            if chip(ui, sc.settings.solid_3d, "Solid").clicked() {
                sc.settings.solid_3d = true;
            }
            if chip(ui, !sc.settings.solid_3d, "Lines").clicked() {
                sc.settings.solid_3d = false;
            }
            ui.add(
                egui::Slider::new(&mut sc.settings.surface_rows_per_sec, 4.0..=60.0)
                    .text("rows/s")
                    .max_decimals(0),
            )
            .on_hover_text("How fast the surface flows back: its depth in time");
        });
        caption(ui, "· Zoom");
        let v = cx.view;
        let mid = if v.contains(cx.rig.rx_hz) { cx.rig.rx_hz } else { v.center() };
        let zoomed = v.zoomed(cx.span);
        if ui.add_enabled(zoomed, egui::Button::new("−")).on_hover_text("Zoom out").clicked() {
            sc.want_view = Some(view::zoom_at(cx.span, v, mid, 0.5));
        }
        if chip(ui, false, "+").on_hover_text("Zoom in").clicked() {
            sc.want_view = Some(view::zoom_at(cx.span, v, mid, 2.0));
        }
        if ui.add_enabled(zoomed, egui::Button::new("Whole span")).clicked() {
            sc.want_view = None;
        }
        note(
            ui,
            "· ctrl + wheel zooms at the pointer · drag to pan · double-click for the whole span",
        );
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Data");
        for d in DataMode::ALL {
            let on = sc.digi == Some(d) && cx.rig.mode == sdr::Mode::Usb;
            let r = ui.add_enabled_ui(!tx, |ui| chip(ui, on, d.label())).inner;
            let r = r.on_hover_text(format!(
                "USB at the {} calling frequency on this band, passband {:.0}–{:.0} Hz",
                d.label(),
                d.passband().0,
                d.passband().1
            ));
            let r = r.on_disabled_hover_text("Locked while the radio transmits");
            // SAFETY INTERLOCK 8: no retuning while transmitting.
            if r.clicked() && !tx {
                let t = bandplan::data_tune(d, cx.map.shown(cx.rig.rx_hz));
                out.tune_to = Some(cx.map.radio(t.dial_hz));
                out.passband = Some(t.passband);
                out.mode = Some(sdr::Mode::Usb);
                out.data_mode = Some(d);
                sc.digi = Some(d);
                sc.digi_note = t.note;
            }
        }
        if let Some(n) = &sc.digi_note {
            note(ui, n);
        }
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Band plan · IARU Region 2");
        chrome::checkbox(ui, &mut sc.settings.band_plan, "Show");
        for u in Use::ALL {
            let [r, g, b] = u.rgb();
            let (sw, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(sw, 1.0, theme::data_ink((r, g, b)));
            note(ui, u.name());
        }
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Step");
        for st in STEPS_HZ {
            let t = if st >= 1000 { format!("{} kHz", st / 1000) } else { format!("{st} Hz") };
            if chip(ui, sc.settings.step_hz == st, &t).clicked() {
                sc.settings.step_hz = st;
            }
        }
        note(ui, "· click snaps to it, wheel moves by it · drag the RX marker to tune, the filter edges to set the width");
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Waterfall");
        for p in Palette::ALL {
            let r = palette_chip(ui, p, sc.settings.palette == p);
            if r.clicked() {
                sc.settings.palette = p;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "Speed");
        for sp in WaterfallSpeed::ALL {
            if chip(ui, sc.settings.wf_speed == sp, sp.label()).clicked() {
                sc.settings.wf_speed = sp;
            }
        }
        chrome::checkbox(ui, &mut sc.settings.wf_smooth, "Smooth");
        chrome::checkbox(ui, &mut sc.settings.wf_auto, "Auto level").on_hover_text(
            "The waterfall's levels follow the noise floor, apart from the spectrum's",
        );
        chrome::checkbox(ui, &mut sc.settings.wf_time, "UTC ticks");
        ui.add(
            egui::Slider::new(&mut sc.settings.wf_contrast, 0.25..=4.0)
                .logarithmic(true)
                .text("contrast"),
        );
        ui.add(egui::Slider::new(&mut sc.settings.wf_brightness, -0.5..=0.5).text("brightness"));
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "FFT");
        for n in FFT_SIZES {
            if chip(ui, sc.settings.fft_size == n, &fft_label(n)).clicked() {
                sc.settings.fft_size = n;
            }
        }
        let bin = cx.span.width / sc.settings.fft_size as f64;
        note(
            ui,
            &format!(
                "· {bin:.1} Hz bins, {:.0} ms",
                sc.settings.fft_size as f64 / cx.span.width * 1000.0
            ),
        );
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "Window");
        for w in FftWindow::ALL {
            if chip(ui, sc.settings.window == w, w.label()).clicked() {
                sc.settings.window = w;
            }
        }
        caption(ui, "· Overlap");
        for o in FftOverlap::ALL {
            if chip(ui, sc.settings.overlap == o, o.label()).clicked() {
                sc.settings.overlap = o;
            }
        }
        caption(ui, "· fps");
        for f in FPS_CHOICES {
            if chip(ui, sc.settings.fps == f, &format!("{f:.0}")).clicked() {
                sc.settings.fps = f;
            }
        }
        chrome::checkbox(ui, &mut sc.settings.zoom_fft, "Zoom FFT").on_hover_text(
            "Re-resolve a zoomed view with its own FFT instead of stretching the span's bins",
        );
        chrome::checkbox(ui, &mut sc.settings.noise_line, "Noise floor");
        ui.add(egui::Slider::new(&mut sc.settings.persist_secs, 0.2..=10.0).text("s persistence"));
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Markers");
        for i in 0..2 {
            if chip(ui, sc.markers.active == i, &format!("M{}", i + 1))
                .on_hover_text("The marker the next right-click places")
                .clicked()
            {
                sc.markers.active = i;
            }
        }
        if chip(ui, false, "To peak").clicked()
            && let Some(f) = sc.shown().cloned()
        {
            sc.markers.snap_to_peak(&f, cx.view.lo, cx.view.hi);
        }
        if chip(ui, false, "Clear").clicked() {
            sc.markers.clear();
        }
        note(ui, "· right-click places one, drag to move it · readouts to 0.1 Hz");
    });

    ui.horizontal_wrapped(|ui| {
        caption(ui, "Transverter LO");
        let mut mhz = sc.settings.transverter_hz / 1e6;
        let r = ui.add(egui::DragValue::new(&mut mhz).speed(0.01).max_decimals(6).suffix(" MHz"));
        let r = r.on_hover_text(
            "An up/down converter's local oscillator, signed: every frequency shown is the \
             radio's plus this. 0 for none.",
        );
        if r.changed() {
            sc.settings.transverter_hz = (mhz * 1e6).round();
            sc.xvtr_text = view::fmt_freq(sc.settings.transverter_hz.abs());
        }
        caption(ui, "· Calibration");
        ui.add(
            egui::DragValue::new(&mut sc.settings.ppm).speed(0.001).max_decimals(3).suffix(" ppm"),
        )
        .on_hover_text("What the radio's reference is off by. 0 with a GPS-disciplined one");
    });
}

fn level_sliders(sc: &mut Scope, ui: &mut egui::Ui) {
    let scale = sc.settings.scale;
    let fmt = move |v: f64| -> String {
        if scale == Scale::SUnits { s_unit(v as f32) } else { format!("{v:.0} dBm") }
    };
    ui.horizontal(|ui| {
        caption(ui, "Floor");
        let mut f = sc.settings.floor_dbm;
        let r = ui.add(
            egui::Slider::new(&mut f, LEVEL_MIN..=LEVEL_MAX - MIN_RANGE_DB)
                .custom_formatter(move |v, _| fmt(v)),
        );
        if r.changed() {
            sc.settings.set_floor(f);
        }
        caption(ui, "Ceil");
        let mut c = sc.settings.ceil_dbm;
        let r = ui.add(
            egui::Slider::new(&mut c, LEVEL_MIN + MIN_RANGE_DB..=LEVEL_MAX)
                .custom_formatter(move |v, _| fmt(v)),
        );
        if r.changed() {
            sc.settings.set_ceil(c);
        }
        caption(ui, "Height");
        let mut h = sc.settings.height;
        if ui
            .add(
                egui::Slider::new(
                    &mut h,
                    super::settings::HEIGHT_MIN..=super::settings::HEIGHT_MAX,
                )
                .max_decimals(0),
            )
            .changed()
        {
            sc.settings.set_height(h);
        }
    });
}

/// A palette chip: a swatch of its ramp, then its name.
fn palette_chip(ui: &mut egui::Ui, p: Palette, on: bool) -> egui::Response {
    // Room for the swatch: as many of the chip font's spaces as span it.
    let space = (chrome::chip_width(ui, "  ", None) - chrome::chip_width(ui, " ", None)).max(1.0);
    let pad = " ".repeat((26.0 / space).ceil() as usize);
    let r = chip(ui, on, &format!("{pad}{}", p.label()));
    let sw =
        Rect::from_min_size(pos2(r.rect.left() + 6.0, r.rect.center().y - 4.0), vec2(22.0, 8.0));
    let painter = ui.painter();
    let n = 11;
    for i in 0..n {
        let [rr, g, b] = p.at(i as f32 / (n - 1) as f32);
        let x0 = sw.left() + sw.width() * i as f32 / n as f32;
        painter.rect_filled(
            Rect::from_min_size(pos2(x0, sw.top()), vec2(sw.width() / n as f32 + 0.5, sw.height())),
            0.0,
            Color32::from_rgb(rr, g, b),
        );
    }
    r
}
