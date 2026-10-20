//! The SCF view on screen: the (f, α) plane (terrain or heat map) with its
//! axes in Hz, the readout of the strongest cycle frequencies, and the
//! controls under it.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Rect, RichText, Sense, Stroke, pos2, vec2,
};

use super::draw::{self, Look, PlanePaint};
use super::features::Axes;
use super::{CHANNELS, LANE_RATES, LaneKind, STRIPS, Scf, ScfSettings, Status};
use crate::scope::palette::Palette;
use crate::scope::view::FreqMap;
use crate::{chrome, theme};

/// What the view needs from the scope this frame.
pub struct Frame<'a> {
    pub snap: &'a sdr::Snapshot,
    pub palette: Palette,
    /// The TFT's height, points (the plane takes it all).
    pub height: f32,
    pub map: FreqMap,
    /// The scope's id, for its GPU resources.
    pub id: u64,
}

const TBAR: f32 = 22.0;
const READOUT_W: f32 = 236.0;
const LEFT: f32 = 52.0;
const BOTTOM: f32 = 20.0;

fn ground() -> Color32 {
    theme::scope().ground
}
fn edge() -> Color32 {
    theme::scope().line
}
fn grid() -> Color32 {
    theme::scope().line.gamma_multiply(0.6)
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
fn label(size: f32) -> FontId {
    FontId::proportional(size)
}
fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// A frequency in Hz or kHz, with as many places as its size wants.
pub fn fmt_hz(v: f64) -> String {
    let a = v.abs();
    if a >= 10_000.0 {
        format!("{:.1} kHz", v / 1000.0)
    } else if a >= 1000.0 {
        format!("{:.2} kHz", v / 1000.0)
    } else if a >= 100.0 {
        format!("{v:.0} Hz")
    } else if a >= 10.0 {
        format!("{v:.1} Hz")
    } else {
        format!("{v:.2} Hz")
    }
}

/// Tick spacing for `span` Hz: 1, 2 or 5 times a power of ten, at most
/// about `n` ticks.
pub fn tick_step(span: f64, n: f64) -> f64 {
    let raw = (span / n.max(1.0)).max(1e-6);
    let p = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * p).find(|s| *s >= raw).unwrap_or(10.0 * p)
}

fn ticks(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let mut v = Vec::new();
    let mut t = (lo / step).ceil() * step;
    while t <= hi + 1e-9 && v.len() < 64 {
        v.push(t);
        t += step;
    }
    v
}

fn chip(ui: &mut egui::Ui, on: bool, t: &str) -> egui::Response {
    chrome::chip(ui, on, RichText::new(t).size(12.0)).on_hover_cursor(CursorIcon::PointingHand)
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t.to_uppercase()).size(10.5).color(theme::CYAN_DIM()).strong());
}

impl Scf {
    /// Draw the view the whole width of `ui`, and its controls.
    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut ScfSettings, f: Frame) {
        let snap = f.snap;
        let pb = snap.passband;
        let shift = snap.offset_hz + (pb.0 as f64 + pb.1 as f64) / 2.0;
        let st = self.want(s, shift);

        let w = ui.available_width().max(240.0);
        let (outer, _) = ui.allocate_exact_size(vec2(w, TBAR + f.height), Sense::hover());
        let p = ui.painter_at(outer);
        p.rect_filled(outer, 2.0, ground());
        p.rect_stroke(outer, 2.0, Stroke::new(1.0, edge()), egui::StrokeKind::Inside);
        let tbar = Rect::from_min_size(outer.min, vec2(w, TBAR));
        let body = Rect::from_min_max(pos2(outer.left(), tbar.bottom()), outer.max);
        let ro_w = READOUT_W.min(w * 0.35);
        let plot_r = Rect::from_min_max(body.min, pos2(body.right() - ro_w, body.bottom()));
        let ro = Rect::from_min_max(pos2(plot_r.right(), body.top()), body.max);
        p.vline(ro.left(), ro.y_range(), Stroke::new(1.0, edge()));
        p.hline(tbar.x_range(), tbar.bottom() - 0.5, Stroke::new(1.0, edge()));

        let centre_hz = snap.center_hz + if s.lane == LaneKind::Narrow { shift } else { 0.0 };
        match (&st, self.unsupported) {
            (_, Some(why)) => {
                p.text(body.center(), Align2::CENTER_CENTER, why, label(13.0), dim());
            }
            (None, _) => {}
            (Some(st), None) => {
                top_bar(ui, tbar, st, s, f.map.shown(centre_hz));
                self.plane(ui, plot_r, st, s, &f);
                readout(ui, ro, st, f.map.shown(centre_hz));
                ui.ctx().request_repaint();
            }
        }
        self.controls(ui, s);
    }

    fn plane(&mut self, ui: &egui::Ui, r: Rect, st: &Status, s: &ScfSettings, f: &Frame) {
        let p = ui.painter_at(r);
        let plot = Rect::from_min_max(
            pos2(r.left() + if s.heat_map { LEFT } else { 8.0 }, r.top() + 8.0),
            pos2(r.right() - if s.heat_map { 10.0 } else { LEFT }, r.bottom() - BOTTOM),
        );
        let (Some((grid, version)), Some(ax), Some(cfg)) = (&st.grid, st.axes, st.cfg) else {
            let msg = if st.lane_rate > 0.0 { "Integrating…" } else { "Waiting for I/Q…" };
            p.text(r.center(), Align2::CENTER_CENTER, msg, label(13.0), dim());
            return;
        };
        // Ease the level scale, so the picture does not pump block to block.
        let target = (st.floor_db, st.ceil_db);
        let (fl, ce) = match self.shown_scale {
            Some((a, b)) => (a + (target.0 - a) * 0.15, b + (target.1 - b) * 0.15),
            None => target,
        };
        self.shown_scale = Some((fl, ce));
        let look = if s.heat_map { Look::Heat } else { Look::Terrain };
        if !s.heat_map {
            terrain_floor(&p, plot, &ax);
        }
        let cb = PlanePaint {
            id: f.id,
            grid: grid.clone(),
            version: *version,
            grid_f: cfg.grid_f,
            grid_a: cfg.grid_a,
            look,
            palette: f.palette,
            floor: fl,
            ceil: ce,
        };
        p.add(draw::shape(plot, cb));
        if s.heat_map {
            heat_axes(&p, plot, &ax);
        } else {
            terrain_axes(&p, plot, &ax);
        }
        // The features found, marked where they are.
        for (i, ft) in st.features.iter().enumerate() {
            let xl = ((ft.f_hz - ax.f_lo) / (ax.f_hi - ax.f_lo)) as f32;
            let d = ((ft.alpha_hz - ax.a_lo) / (ax.a_hi - ax.a_lo)) as f32;
            let at = if s.heat_map {
                pos2(plot.left() + xl * plot.width(), plot.bottom() - d * plot.height())
            } else {
                let (x, y) = draw::project(xl, d);
                pos2(plot.left() + x * plot.width(), plot.top() + y * plot.height())
            };
            p.circle_stroke(at, 6.0, Stroke::new(1.5, accent()));
            p.text(
                at + vec2(8.0, -8.0),
                Align2::LEFT_BOTTOM,
                format!("{}", i + 1),
                mono(11.0),
                ink(),
            );
        }
        let scale = if st.coherence { "coherence" } else { "SCF" };
        p.text(
            pos2(r.left() + 8.0, r.top() + 6.0),
            Align2::LEFT_TOP,
            format!("{scale} {fl:.0} … {ce:.0} dB"),
            label(11.0),
            dim(),
        );
    }

    fn controls(&mut self, ui: &mut egui::Ui, s: &mut ScfSettings) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            caption(ui, "SCF lane");
            if chip(ui, s.lane == LaneKind::Narrow, "Narrow")
                .on_hover_text("A few kHz around the receiver's passband: FT8, FT4, CW")
                .clicked()
            {
                s.lane = LaneKind::Narrow;
            }
            if chip(ui, s.lane == LaneKind::Wide, "Whole span")
                .on_hover_text("The whole I/Q span: wideband data with kHz symbol rates")
                .clicked()
            {
                s.lane = LaneKind::Wide;
            }
            ui.add_enabled_ui(s.lane == LaneKind::Narrow, |ui| {
                for hz in LANE_RATES {
                    if chip(ui, s.lane_hz == hz, &format!("{} kHz", hz / 1000)).clicked() {
                        s.lane_hz = hz;
                    }
                }
            });
            caption(ui, "· N′");
            for c in CHANNELS {
                if chip(ui, s.channels == c, &c.to_string())
                    .on_hover_text("Channelizer length: Δf = rate / N′")
                    .clicked()
                {
                    s.channels = c;
                }
            }
            caption(ui, "· N");
            egui::ComboBox::from_id_salt("scf-strip")
                .selected_text(s.strip.to_string())
                .show_ui(ui, |ui| {
                    for n in STRIPS {
                        let ok = n as u64 * s.channels as u64 <= super::engine::MAX_POINTS as u64;
                        if ui
                            .add_enabled(ok, egui::Button::selectable(s.strip == n, n.to_string()))
                            .clicked()
                        {
                            s.strip = n;
                        }
                    }
                })
                .response
                .on_hover_text("Strip length: Δα = rate / N, and a block is N samples");
            ui.add(
                egui::Slider::new(&mut s.avg_blocks, 1..=256).logarithmic(true).text("blocks avg"),
            )
            .on_hover_text(
                "Blocks averaged: linear at first, then exponential with this time constant",
            );
        });
        ui.horizontal_wrapped(|ui| {
            caption(ui, "Show");
            if chip(ui, s.coherence, "Coherence")
                .on_hover_text("|S|² over the power at both frequencies: flat for any noise, best for detection")
                .clicked()
            {
                s.coherence = true;
            }
            if chip(ui, !s.coherence, "SCF").on_hover_text("|S|² itself").clicked() {
                s.coherence = false;
            }
            caption(ui, "·");
            if chip(ui, !s.conjugate, "x·x*")
                .on_hover_text("Features at the symbol rate (α = k·Rs)")
                .clicked()
            {
                s.conjugate = false;
            }
            if chip(ui, s.conjugate, "x·x")
                .on_hover_text("Conjugate: features at twice the carrier (α = 2fc), for BPSK and AM")
                .clicked()
            {
                s.conjugate = true;
            }
            caption(ui, "· α to");
            if ui.button("−").on_hover_text("Halve the α axis").clicked() {
                s.alpha_max_hz /= 2.0;
            }
            ui.label(RichText::new(fmt_hz(s.alpha_max_hz)).size(12.0).color(theme::TEXT()));
            if ui.button("+").on_hover_text("Double the α axis").clicked() {
                s.alpha_max_hz *= 2.0;
            }
            caption(ui, "·");
            if chip(ui, !s.heat_map, "Terrain").clicked() {
                s.heat_map = false;
            }
            if chip(ui, s.heat_map, "Heat map").clicked() {
                s.heat_map = true;
            }
            caption(ui, "· Taper");
            if chip(ui, !s.kaiser, "Hamming").on_hover_text("Finest Δf; −43 dB leakage").clicked() {
                s.kaiser = false;
            }
            if chip(ui, s.kaiser, "Kaiser")
                .on_hover_text("−74 dB leakage: a strong signal's features stay in its own channels")
                .clicked()
            {
                s.kaiser = true;
            }
            if ui.button("Reset average").clicked() {
                self.reset();
            }
        });
        s.sanitise();
    }
}

fn top_bar(ui: &egui::Ui, r: Rect, st: &Status, s: &ScfSettings, centre_shown: f64) {
    let p = ui.painter_at(r);
    let fs = st.lane_rate;
    let lane = match s.lane {
        LaneKind::Narrow => format!("NARROW {} at {:.4} MHz", fmt_hz(fs), centre_shown / 1e6),
        LaneKind::Wide => format!("SPAN {}", fmt_hz(fs)),
    };
    p.text(
        pos2(r.left() + 10.0, r.center().y),
        Align2::LEFT_CENTER,
        format!("SCF · {lane}"),
        label(12.0),
        ink(),
    );
    if let Some(c) = st.cfg
        && fs > 0.0
    {
        let block = c.n as f64 / fs;
        let mid = format!(
            "N′ {} · Δf {} · N {} · Δα {} · {} blocks of {:.2} s ({:.0} s)",
            c.nc,
            fmt_hz(fs / c.nc as f64),
            c.n,
            fmt_hz(fs / c.n as f64),
            st.blocks.min(s.avg_blocks as u64),
            block,
            block * st.blocks.min(s.avg_blocks as u64) as f64,
        );
        p.text(r.center(), Align2::CENTER_CENTER, mid, label(11.5), dim());
        // The GPU keeping up is neutral; behind it is a warning.
        let load = st.ms_per_block as f64 / 1000.0 / block;
        let behind = load > 1.0 || st.gpu_drops > 0;
        let text = format!(
            "GPU {:.1} ms/block{}",
            st.ms_per_block,
            if st.gpu_drops > 0 { format!(" · {} dropped", st.gpu_drops) } else { String::new() }
        );
        let col = if behind { theme::YELLOW() } else { dim() };
        p.text(pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, text, label(11.5), col);
    }
    if let Some(e) = &st.error {
        p.text(r.center(), Align2::CENTER_CENTER, e, label(12.0), theme::YELLOW());
    }
}

/// The terrain's floor: the f lines running back and the α lines across.
fn terrain_floor(p: &egui::Painter, plot: Rect, ax: &Axes) {
    let at = |xl: f32, d: f32| {
        let (x, y) = draw::project(xl, d);
        pos2(plot.left() + x * plot.width(), plot.top() + y * plot.height())
    };
    let fstep = tick_step(ax.f_hi - ax.f_lo, 8.0);
    for t in ticks(ax.f_lo, ax.f_hi, fstep) {
        let xl = ((t - ax.f_lo) / (ax.f_hi - ax.f_lo)) as f32;
        p.line_segment([at(xl, 0.0), at(xl, 1.0)], Stroke::new(1.0, grid()));
    }
    let astep = tick_step(ax.a_hi - ax.a_lo, 6.0);
    for t in ticks(ax.a_lo, ax.a_hi, astep) {
        let d = ((t - ax.a_lo) / (ax.a_hi - ax.a_lo)) as f32;
        p.line_segment([at(0.0, d), at(1.0, d)], Stroke::new(1.0, grid()));
    }
}

fn terrain_axes(p: &egui::Painter, plot: Rect, ax: &Axes) {
    let at = |xl: f32, d: f32| {
        let (x, y) = draw::project(xl, d);
        pos2(plot.left() + x * plot.width(), plot.top() + y * plot.height())
    };
    let fstep = tick_step(ax.f_hi - ax.f_lo, 8.0);
    for t in ticks(ax.f_lo, ax.f_hi, fstep) {
        let xl = ((t - ax.f_lo) / (ax.f_hi - ax.f_lo)) as f32;
        p.text(at(xl, 0.0) + vec2(0.0, 3.0), end_align(xl), fmt_hz(t), label(10.5), dim());
    }
    let astep = tick_step(ax.a_hi - ax.a_lo, 6.0);
    for t in ticks(ax.a_lo, ax.a_hi, astep) {
        let d = ((t - ax.a_lo) / (ax.a_hi - ax.a_lo)) as f32;
        p.text(at(1.0, d) + vec2(4.0, 0.0), Align2::LEFT_CENTER, fmt_hz(t), label(10.5), dim());
    }
    p.text(at(1.0, 1.0) + vec2(4.0, -12.0), Align2::LEFT_BOTTOM, "α", label(12.0), ink());
    p.text(at(1.0, 0.0) + vec2(4.0, 3.0), Align2::LEFT_TOP, "f", label(12.0), ink());
}

/// A label under an axis: centred on its tick, but kept inside at the ends.
fn end_align(xl: f32) -> Align2 {
    if xl < 0.02 {
        Align2::LEFT_TOP
    } else if xl > 0.98 {
        Align2::RIGHT_TOP
    } else {
        Align2::CENTER_TOP
    }
}

fn heat_axes(p: &egui::Painter, plot: Rect, ax: &Axes) {
    p.rect_stroke(plot, 0.0, Stroke::new(1.0, edge()), egui::StrokeKind::Outside);
    let fstep = tick_step(ax.f_hi - ax.f_lo, 8.0);
    for t in ticks(ax.f_lo, ax.f_hi, fstep) {
        let x = plot.left() + ((t - ax.f_lo) / (ax.f_hi - ax.f_lo)) as f32 * plot.width();
        p.vline(x, plot.y_range(), Stroke::new(1.0, grid().gamma_multiply(0.5)));
        let xl = (x - plot.left()) / plot.width();
        p.text(pos2(x, plot.bottom() + 3.0), end_align(xl), fmt_hz(t), label(10.5), dim());
    }
    let astep = tick_step(ax.a_hi - ax.a_lo, 6.0);
    for t in ticks(ax.a_lo, ax.a_hi, astep) {
        let y = plot.bottom() - ((t - ax.a_lo) / (ax.a_hi - ax.a_lo)) as f32 * plot.height();
        p.hline(plot.x_range(), y, Stroke::new(1.0, grid().gamma_multiply(0.5)));
        p.text(pos2(plot.left() - 4.0, y), Align2::RIGHT_CENTER, fmt_hz(t), label(10.5), dim());
    }
    p.text(
        pos2(plot.left() - 4.0, plot.top() - 2.0),
        Align2::RIGHT_BOTTOM,
        "α",
        label(12.0),
        ink(),
    );
}

fn readout(ui: &egui::Ui, r: Rect, st: &Status, centre_shown: f64) {
    let p = ui.painter_at(r);
    let mut y = r.top() + 8.0;
    let x = r.left() + 10.0;
    let head = if st.conj { "Carriers (α = 2fc)" } else { "Cycle frequencies" };
    p.text(pos2(x, y), Align2::LEFT_TOP, head, label(12.0), ink());
    y += 18.0;
    if st.features.is_empty() {
        p.text(pos2(x, y), Align2::LEFT_TOP, "None over 5σ yet", label(11.5), dim());
        y += 16.0;
    }
    for (i, f) in st.features.iter().enumerate() {
        let main = if st.conj {
            format!("{} α {}  → fc {}", i + 1, fmt_hz(f.alpha_hz), fmt_hz(f.alpha_hz / 2.0))
        } else {
            // A rate, not an identification: a harmonic of something else
            // can land on it too.
            let m = f.mode.map(|m| format!("  ≈{m} rate")).unwrap_or_default();
            format!("{} α {}{m}", i + 1, fmt_hz(f.alpha_hz))
        };
        p.text(pos2(x, y), Align2::LEFT_TOP, main, mono(11.5), ink());
        y += 15.0;
        let sub = format!(
            "   {:.1}σ  +{:.1} dB  at {:.4} MHz",
            f.sigma,
            f.db,
            (centre_shown + f.f_hz) / 1e6
        );
        p.text(pos2(x, y), Align2::LEFT_TOP, sub, mono(10.5), dim());
        y += 17.0;
    }
    y += 6.0;
    let note = if st.conj {
        "x·x: a BPSK or AM carrier shows at twice its offset from the lane's centre."
    } else {
        "x·x*: α is the symbol (or keying) rate; its multiples are harmonics. \
         Presence and rate only: decoders still do the decoding."
    };
    let galley = ui.painter().layout(note.to_owned(), label(10.5), dim(), r.width() - 20.0);
    p.galley(pos2(x, y.min(r.bottom() - galley.size().y - 6.0)), galley, dim());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_fall_on_round_numbers() {
        assert_eq!(tick_step(50.0, 6.0), 10.0);
        assert_eq!(tick_step(3000.0, 8.0), 500.0);
        assert_eq!(ticks(-1500.0, 1500.0, 500.0).len(), 7);
        assert_eq!(fmt_hz(6.25), "6.25 Hz");
        assert_eq!(fmt_hz(1500.0), "1.50 kHz");
        assert_eq!(fmt_hz(-187.5), "-188 Hz");
    }
}
