//! GLOBE (decision 65): the Earth, as a globe or a flat map, with AJ3G's
//! station on it, a path to every station heard in the last hour (DECODE,
//! the skimmers, and from PSK Reporter the stations that heard AJ3G), the
//! propagation those paths show (PROP), the grey line, clouds, the aurora
//! and the city lights, and beside it the space weather and the weather at
//! the station.
//!
//! It follows SDRoxide's 3D view (https://github.com/dividebysandwich/sdroxide,
//! GPL-3.0-or-later, its manual's §7) without its satellites, planets or Sun,
//! in the panel's own look. Nothing it shows can key the radio: TUNE moves
//! the receiver as a DATA key does, with every check that has.

pub(crate) mod fetch;
pub(crate) mod field;
mod geo;
mod picture;
pub(crate) mod sphere;
mod sun;

use std::sync::mpsc;
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2, epaint};
use serde::{Deserialize, Serialize};

use crate::scope::{self, bandplan};
use crate::station::{LatLon, StationSettings, bearing_deg, distance_km};
use crate::{chrome, theme};
use fetch::{Ask, Fetched, Fetcher};
use field::{BandField, History, Reception, Source};
use sphere::{Proj, Projector, V3, View, great_circle, small_circle, vec_ll, vec_of};

/// What GLOBE shows, kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GlobeSettings {
    pub stations: bool,
    pub paths: bool,
    pub prop: bool,
    pub grey: bool,
    pub clouds: bool,
    pub aurora: bool,
    pub cities: bool,
    pub borders: bool,
    pub grid: bool,
    /// PROP: every band in its colour, or `band` alone in the ramp.
    pub all_bands: bool,
    pub band: String,
    pub decode: bool,
    pub skim: bool,
    /// Ask PSK Reporter who heard the station (off until turned on).
    pub pskr: bool,
    pub proj: Proj,
    /// How long PROP's evidence takes to halve, minutes.
    pub half_life_min: f64,
    /// Popped out into its own window.
    pub popped: bool,
}

impl Default for GlobeSettings {
    fn default() -> Self {
        GlobeSettings {
            stations: true,
            paths: true,
            prop: true,
            grey: true,
            clouds: true,
            aurora: true,
            cities: true,
            borders: true,
            grid: false,
            all_bands: false,
            band: "20".into(),
            decode: true,
            skim: true,
            pskr: false,
            proj: Proj::Globe,
            half_life_min: 45.0,
            popped: false,
        }
    }
}

impl GlobeSettings {
    fn source_on(&self, s: Source) -> bool {
        match s {
            Source::Decode => self.decode,
            Source::Skim => self.skim,
            Source::PskReporter => self.pskr,
        }
    }
}

/// What the picture was last painted from.
#[derive(Clone, Debug, PartialEq)]
struct PicKey {
    /// The subsolar point to the nearest quarter degree (a minute of time).
    sun: (i32, i32),
    layers: [bool; 5],
    prop: Option<(Option<&'static str>, u64)>,
    fetched: u64,
}

/// GLOBE as it runs.
pub(crate) struct Globe {
    history: History,
    fetcher: Option<Fetcher>,
    got: Fetched,
    centre: LatLon,
    zoom: f64,
    /// Turned to the station the first time there is one.
    placed: bool,
    /// The station picked: call, band, source.
    sel: Option<(String, Option<&'static str>, Source)>,
    fields: Vec<BandField>,
    /// Bumped each time the field is built again.
    fields_id: u64,
    built: Option<(f64, usize, (bool, bool, bool), u64)>,
    floor: Option<f64>,
    tex: Option<egui::TextureHandle>,
    painting: Option<mpsc::Receiver<Vec<u8>>>,
    painted: Option<(PicKey, f64)>,
}

impl Default for Globe {
    fn default() -> Self {
        Globe {
            history: History::default(),
            fetcher: None,
            got: Fetched::default(),
            centre: LatLon { lat: 25.0, lon: -45.0 },
            zoom: 1.0,
            placed: false,
            sel: None,
            fields: Vec::new(),
            fields_id: 0,
            built: None,
            floor: None,
            tex: None,
            painting: None,
            painted: None,
        }
    }
}

/// What the operator asked of the rest of the panel.
#[derive(Default)]
pub(crate) struct GlobeOut {
    /// TUNE: a data mode's dial, as its DATA key would go there.
    pub tune: Option<scope::DataPick>,
    /// SHOW IN DECODE: open DECODE on this line.
    pub show_row: Option<u64>,
    pub pop: Option<bool>,
}

/// How often the field is built again, and the picture painted at most.
const BUILD_EVERY_S: f64 = 15.0;
const PAINT_EVERY_S: f64 = 3.0;

impl Globe {
    /// A station heard (or, from PSK Reporter, hearing AJ3G).
    pub(crate) fn add(&mut self, r: Reception) {
        self.history.add(r);
    }

    /// The window is closed: stop fetching.
    pub(crate) fn idle(&mut self) {
        if self.fetcher.take().is_some() {
            tracing::info!("globe: closed, fetching stopped");
        }
    }

    fn step(&mut self, s: &GlobeSettings, st: &StationSettings, now: f64) {
        let ask = Ask { qth: st.pos(), call: st.call(), pskr: s.pskr, clouds: s.clouds };
        match &self.fetcher {
            Some(f) => f.set_ask(ask),
            None => self.fetcher = Some(Fetcher::start(ask)),
        }
        if let Some(f) = &self.fetcher
            && let Ok(d) = f.data.lock()
            && d.serial != self.got.serial
        {
            self.got = d.clone();
            for r in &self.got.heard_me {
                self.history.add(r.clone());
            }
        }
        self.history.prune(now);
        let Some(qth) = st.pos() else { return };
        if !self.placed {
            self.centre = LatLon { lat: (qth.lat - 15.0).clamp(-60.0, 60.0), lon: qth.lon + 30.0 };
            self.placed = true;
        }
        let key = (s.decode, s.skim, s.pskr);
        let hl = (s.half_life_min * 60.0).max(60.0);
        let due = match self.built {
            None => true,
            Some((at, n, k, h)) => {
                now - at > BUILD_EVERY_S
                    || n != self.history.len() && now - at > 2.0
                    || k != key
                    || h != hl as u64
            }
        };
        if due {
            let recs = self.history.all();
            self.fields = field::build(&recs, qth, now, hl, &|src| s.source_on(src));
            self.floor = field::heard_floor(
                &recs.into_iter().filter(|r| s.source_on(r.src)).collect::<Vec<_>>(),
                qth,
                now - 1800.0,
            );
            self.fields_id += 1;
            self.built = Some((now, self.history.len(), key, hl as u64));
        }
    }

    /// Paint the picture again when what it shows has changed, on a thread.
    fn repaint_picture(&mut self, ctx: &egui::Context, s: &GlobeSettings, now: f64) {
        if let Some(rx) = &self.painting {
            match rx.try_recv() {
                Ok(px) => {
                    let img =
                        egui::ColorImage::from_rgba_unmultiplied([geo::TEX_W, geo::TEX_H], &px);
                    let opts = egui::TextureOptions::LINEAR;
                    match &mut self.tex {
                        Some(t) => t.set(img, opts),
                        None => self.tex = Some(ctx.load_texture("globe-map", img, opts)),
                    }
                    self.painting = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.painting = None,
            }
        }
        let sun = sun::subsolar(now);
        let one = (!s.all_bands).then(|| band_static(&s.band));
        let key = PicKey {
            sun: ((sun.lat * 4.0).round() as i32, (sun.lon * 4.0).round() as i32),
            layers: [s.grey, s.grey, s.cities, s.clouds, s.aurora],
            prop: s.prop.then_some((one, self.fields_id)),
            fetched: self.got.serial,
        };
        if self.painted.as_ref().is_some_and(|(k, at)| *k == key || now - at < PAINT_EVERY_S) {
            return;
        }
        let recipe = picture::Recipe {
            sun,
            night: s.grey,
            grey: s.grey,
            lights: s.cities,
            clouds: s.clouds.then(|| self.got.clouds.clone()).flatten(),
            aurora: s.aurora.then(|| self.got.aurora.clone()).flatten(),
            prop: s.prop.then(|| picture::Prop { fields: self.fields.clone(), one }),
        };
        let (tx, rx) = mpsc::channel();
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("globe-paint".into())
            .spawn(move || {
                let _ = tx.send(picture::paint(&recipe));
                c.request_repaint();
            })
            .expect("the globe's paint thread starts");
        self.painting = Some(rx);
        self.painted = Some((key, now));
    }

    /// The stations the map and the list show: the latest of each, from the
    /// sources that are on, on the band PROP shows when it shows one.
    fn shown(&self, s: &GlobeSettings) -> Vec<Reception> {
        self.history
            .latest()
            .into_iter()
            .filter(|r| s.source_on(r.src))
            .filter(|r| s.all_bands || r.band() == Some(band_static(&s.band)))
            .collect()
    }
}

/// The band name as the band plan has it, for a setting read from disk.
fn band_static(b: &str) -> &'static str {
    bandplan::HAM_BANDS.iter().find(|h| h.name == b).map(|h| h.name).unwrap_or("20")
}

/// The HF bands GLOBE offers.
const BANDS: [&str; 10] = ["160", "80", "60", "40", "30", "20", "17", "15", "12", "10"];

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

const QTH_GREEN: Color32 = Color32::from_rgb(0x41, 0xd1, 0x7a);
const INK: Color32 = Color32::from_rgb(0xe9, 0xee, 0xf4);
const INK_DIM: Color32 = Color32::from_rgb(0x9f, 0xb0, 0xc3);

/// Draw the window, docked or popped out. Clears `open` when it is closed.
pub(crate) fn window(
    ctx: &egui::Context,
    open: &mut bool,
    g: &mut Globe,
    s: &mut GlobeSettings,
    st: &StationSettings,
) -> GlobeOut {
    let mut out = GlobeOut::default();
    let now = decoders::slot::utc_now();
    g.step(s, st, now);
    g.repaint_picture(ctx, s, now);
    // The clocks tick, and the fetches land.
    crate::repaint::after(ctx, Duration::from_secs(1));
    if s.popped {
        let _shut = crate::gpu_gate::shut();
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("globe-window"),
            egui::ViewportBuilder::default()
                .with_title("Globe")
                .with_inner_size([1360.0, 860.0])
                .with_min_inner_size([900.0, 560.0]),
            |ui, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        body(ui, true, g, s, st, now, &mut out);
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        egui::Window::new("Globe")
            .id(egui::Id::new("globe-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([24.0, 44.0])
            .default_size([1240.0, 780.0])
            .min_size([900.0, 560.0])
            .constrain_to(ctx.content_rect().with_min_y(40.0))
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                body(ui, false, g, s, st, now, &mut out);
            });
    }
    if !*open {
        g.idle();
    }
    out
}

fn caps(t: &str) -> RichText {
    RichText::new(t).size(10.5).strong().color(theme::TEXT())
}

fn body(
    ui: &mut egui::Ui,
    popped: bool,
    g: &mut Globe,
    s: &mut GlobeSettings,
    st: &StationSettings,
    now: f64,
    out: &mut GlobeOut,
) {
    top_bar(ui, popped, st, now, out);
    toolbar(ui, g, s);
    ui.add_space(6.0);
    let shown = g.shown(s);
    // Everything is sized to the room left, less the gaps between, so the
    // window never measures more than it was given and grows frame by frame.
    let avail = ui.available_size();
    let gap = ui.spacing().item_spacing;
    let foot = 18.0 + gap.y * 2.0;
    let h = (avail.y - foot).max(300.0);
    let (left_w, right_w) = (250.0, 300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_w, h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_size(Vec2::new(left_w, h));
                ui.set_max_width(left_w);
                heard_list(ui, g, s, st, &shown);
            },
        );
        let map_w = (ui.available_width() - right_w - gap.x * 2.0).max(300.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(map_w, h), Sense::click_and_drag());
        map(ui, rect, &resp, g, s, st, now, &shown);
        ui.allocate_ui_with_layout(
            Vec2::new(right_w, h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_size(Vec2::new(right_w, h));
                ui.set_max_width(right_w);
                egui::ScrollArea::vertical()
                    .id_salt("globe-side")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        side(ui, g, st, now, out);
                    });
            },
        );
    });
    footer(ui, g);
}

fn top_bar(ui: &mut egui::Ui, popped: bool, st: &StationSettings, now: f64, out: &mut GlobeOut) {
    ui.horizontal(|ui| {
        let who = match (st.call(), st.grid()) {
            (Some(c), Some(gr)) => format!("{c} · {gr}"),
            (None, Some(gr)) => gr,
            _ => "Set your callsign and grid in SETTINGS → Station".into(),
        };
        ui.label(RichText::new(who).monospace().size(12.5).color(theme::TEXT_STRONG()));
        if let Some(p) = st.pos() {
            ui.label(
                RichText::new(format!(
                    "{:.1}°{} {:.1}°{}",
                    p.lat.abs(),
                    if p.lat >= 0.0 { "N" } else { "S" },
                    p.lon.abs(),
                    if p.lon >= 0.0 { "E" } else { "W" }
                ))
                .monospace()
                .size(11.5)
                .color(theme::TEXT()),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pop = if popped { "DOCK" } else { "POP OUT" };
            if chrome::chip(ui, false, pop).clicked() {
                out.pop = Some(!popped);
            }
            ui.add_space(10.0);
            let (y, mo, d, h, mi, se) = crate::decodelog::civil(now as i64);
            ui.label(
                RichText::new(format!("{y}-{mo:02}-{d:02}"))
                    .monospace()
                    .size(11.0)
                    .color(theme::TEXT()),
            );
            ui.label(
                RichText::new(format!("{h:02}:{mi:02}:{se:02}Z"))
                    .monospace()
                    .size(14.0)
                    .strong()
                    .color(theme::TEXT_STRONG()),
            );
            ui.label(caps("UTC"));
            ui.add_space(12.0);
            let local = chrono::Local::now();
            let off = local.offset().local_minus_utc();
            let zone = if off % 3600 == 0 {
                format!("UTC{:+}", off / 3600)
            } else {
                format!("UTC{:+}:{:02}", off / 3600, (off.abs() % 3600) / 60)
            };
            ui.label(RichText::new(zone).monospace().size(11.0).color(theme::TEXT()));
            ui.label(
                RichText::new(local.format("%H:%M:%S").to_string())
                    .monospace()
                    .size(14.0)
                    .strong()
                    .color(theme::TEXT_STRONG()),
            );
            ui.label(caps("LOCAL"));
        });
    });
    ui.add_space(4.0);
}

fn toolbar(ui: &mut egui::Ui, g: &mut Globe, s: &mut GlobeSettings) {
    ui.horizontal_wrapped(|ui| {
        ui.label(caps("MAP"));
        if chrome::chip(ui, s.proj == Proj::Globe, "GLOBE").clicked() {
            s.proj = Proj::Globe;
        }
        if chrome::chip(ui, s.proj == Proj::Flat, "FLAT").clicked() {
            s.proj = Proj::Flat;
        }
        ui.add_space(10.0);
        ui.label(caps("LAYERS"));
        for (on, label) in [
            (&mut s.stations, "STATIONS"),
            (&mut s.paths, "PATHS"),
            (&mut s.prop, "PROP MAP"),
            (&mut s.grey, "GREY LINE"),
            (&mut s.clouds, "CLOUDS"),
            (&mut s.aurora, "AURORA"),
            (&mut s.cities, "CITIES"),
            (&mut s.borders, "BORDERS"),
            (&mut s.grid, "GRID"),
        ] {
            if chrome::chip(ui, *on, label).clicked() {
                *on = !*on;
            }
        }
    });
    ui.add_space(3.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(caps("PROP"));
        if chrome::chip(ui, !s.all_bands, "ONE BAND").clicked() {
            s.all_bands = false;
        }
        if chrome::chip(ui, s.all_bands, "ALL BANDS").clicked() {
            s.all_bands = true;
        }
        ui.add_space(10.0);
        ui.label(caps("BAND"));
        let heard: Vec<&'static str> = g
            .history
            .latest()
            .iter()
            .filter(|r| s.source_on(r.src))
            .filter_map(|r| r.band())
            .collect();
        for b in BANDS {
            let has = heard.contains(&b);
            let r = chrome::chip(ui, !s.all_bands && s.band == b, b).on_hover_text(if has {
                format!("Paths heard on {b} m")
            } else {
                format!("Nothing heard on {b} m yet")
            });
            if r.clicked() {
                s.band = b.into();
                s.all_bands = false;
            }
        }
        ui.add_space(10.0);
        ui.label(caps("SOURCES"));
        for (on, label, tip) in [
            (&mut s.decode, "DECODE", "The stations in the DECODE log"),
            (
                &mut s.skim,
                "SKIM",
                "SKIM WIDE, and the keyboard skimmer's stations whose grid is known",
            ),
            (
                &mut s.pskr,
                "PSK REPORTER",
                "Asks PSK Reporter every 5 minutes who heard your callsign in the last hour",
            ),
        ] {
            if chrome::chip(ui, *on, label).on_hover_text(tip).clicked() {
                *on = !*on;
            }
        }
        ui.add_space(10.0);
        ui.label(caps("HALF-LIFE"));
        ui.add(
            egui::DragValue::new(&mut s.half_life_min).range(5.0..=180.0).speed(1.0).suffix(" min"),
        );
    });
}

fn age_text(now: f64, utc: f64) -> String {
    let m = ((now - utc) / 60.0).max(0.0);
    if m < 1.0 { "now".into() } else { format!("{:.0}m", m) }
}

fn band_label(b: Option<&str>) -> String {
    b.map(|b| format!("{b}m")).unwrap_or_else(|| "—".into())
}

fn is_sel(g: &Globe, r: &Reception) -> bool {
    g.sel.as_ref().is_some_and(|(c, b, src)| *c == r.call && *b == r.band() && *src == r.src)
}

fn heard_list(
    ui: &mut egui::Ui,
    g: &mut Globe,
    s: &mut GlobeSettings,
    st: &StationSettings,
    shown: &[Reception],
) {
    let all = g.history.latest();
    ui.horizontal(|ui| {
        ui.label(caps("HEARD · 60 MIN"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} on the map", shown.len()))
                    .size(11.0)
                    .color(theme::TEXT()),
            );
        });
    });
    ui.label(
        RichText::new(format!(
            "{:<10}{:>4} {:<5}{:>4}{:>7}{:>4}",
            "CALL",
            "BAND",
            "MODE",
            "SNR",
            st.dist_unit.label().to_uppercase(),
            "AGE"
        ))
        .monospace()
        .size(10.5)
        .color(theme::TEXT()),
    );
    let qth = st.pos();
    let now = decoders::slot::utc_now();
    egui::ScrollArea::vertical().id_salt("globe-heard").auto_shrink([false, false]).show(ui, |ui| {
        if all.is_empty() {
            ui.label(
                RichText::new("Nothing heard yet. Stations appear here as DECODE and the skimmers hear them, with a grid.")
                    .size(11.5)
                    .color(theme::TEXT()),
            );
        }
        for r in &all {
            let on_map = s.source_on(r.src) && (s.all_bands || r.band() == Some(band_static(&s.band)));
            let km = qth.map(|q| distance_km(q, r.pos)).unwrap_or(0.0);
            let snr = r.snr.map(|v| format!("{:+.0}", v)).unwrap_or_else(|| "—".into());
            let call = if r.src.reverse() { format!("→{}", r.call) } else { r.call.clone() };
            let line = format!(
                "{:<10}{:>4} {:<5}{:>4}{:>7}{:>4}",
                truncate(&call, 10),
                band_label(r.band()),
                truncate(&r.mode, 5),
                snr,
                format!("{:.0}", st.dist_unit.of_km(km)),
                age_text(now, r.utc)
            );
            let text = RichText::new(line).monospace().size(11.0).color(if on_map { theme::TEXT_STRONG() } else { theme::TEXT() });
            let tip = if r.src.reverse() { format!("{} heard you ({})", r.call, r.src.label()) } else { format!("Heard by {}", r.src.label()) };
            if ui.add(egui::Button::selectable(is_sel(g, r), text)).on_hover_text(tip).clicked() {
                g.sel = Some((r.call.clone(), r.band(), r.src));
                // A station on another band: the map goes to its band.
                if !on_map && !s.all_bands && let Some(b) = r.band() {
                    s.band = b.to_string();
                }
            }
        }
    });
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The arc's height over the surface at its middle, as a fraction of the
/// Earth's radius: longer paths bow further out, so both ends of a long one
/// stay in sight.
fn arc_lift(km: f64) -> f64 {
    0.03 + 0.2 * (km / 20_015.0)
}

#[allow(clippy::too_many_arguments)]
fn map(
    ui: &mut egui::Ui,
    rect: Rect,
    resp: &egui::Response,
    g: &mut Globe,
    s: &GlobeSettings,
    st: &StationSettings,
    now: f64,
    shown: &[Reception],
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(0x07, 0x09, 0x0c));
    let view = |g: &Globe| View {
        proj: s.proj,
        centre: g.centre,
        zoom: g.zoom,
        cx: rect.center().x as f64,
        cy: rect.center().y as f64,
        w: rect.width() as f64,
        h: rect.height() as f64,
    };
    // Turn and zoom.
    if resp.dragged() {
        let d = resp.drag_delta();
        g.centre = Projector::new(view(g)).dragged(d.x as f64, d.y as f64);
    }
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            g.zoom = (g.zoom * (dy as f64 / 300.0).exp()).clamp(0.6, 6.0);
        }
    }
    let p = Projector::new(view(g));
    let pos = |x: f64, y: f64| Pos2::new(x as f32, y as f32);
    let lines = |pts: &[V3], stroke: Stroke| {
        for run in p.runs(pts, None) {
            painter.add(egui::Shape::line(run.iter().map(|&(x, y)| pos(x, y)).collect(), stroke));
        }
    };

    // The picture.
    match (&g.tex, s.proj) {
        (Some(tex), Proj::Globe) => {
            painter.circle_filled(
                pos(p.view.cx, p.view.cy),
                (p.r * 1.03) as f32,
                Color32::from_rgba_unmultiplied(0x4a, 0xa8, 0xff, 18),
            );
            let mut mesh = epaint::Mesh::with_texture(tex.id());
            let step = 3.0;
            let mut lat = -90.0;
            while lat < 90.0 {
                let mut lon = -180.0;
                while lon < 180.0 {
                    let c = [
                        (lat, lon),
                        (lat, lon + step),
                        (lat + step, lon + step),
                        (lat + step, lon),
                    ];
                    let vs: Vec<V3> = c.iter().map(|&(a, b)| vec_of(a, b)).collect();
                    if vs.iter().any(|&v| p.project(v).z > -0.05) {
                        let base = mesh.vertices.len() as u32;
                        for (k, &(a, b)) in c.iter().enumerate() {
                            let (x, y) = p.project_clamped(vs[k]);
                            let uv = Pos2::new(
                                ((b + 180.0) / 360.0) as f32,
                                ((90.0 - a) / 180.0) as f32,
                            );
                            mesh.vertices.push(epaint::Vertex {
                                pos: pos(x, y),
                                uv,
                                color: Color32::WHITE,
                            });
                        }
                        mesh.indices.extend_from_slice(&[
                            base,
                            base + 1,
                            base + 2,
                            base,
                            base + 2,
                            base + 3,
                        ]);
                    }
                    lon += step;
                }
                lat += step;
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        (Some(tex), Proj::Flat) => {
            let (top, bot) = (p.flat_y(90.0) as f32, p.flat_y(-90.0) as f32);
            let left = p.flat_x(-180.0) as f32;
            let w = p.r as f32;
            // The map starts at the longitude opposite the middle.
            let u0 = (((p.view.centre.lon + 180.0) / 360.0).rem_euclid(1.0)) as f32 + 0.5;
            let u0 = u0.rem_euclid(1.0);
            let split = left + (1.0 - u0) * w;
            painter.image(
                tex.id(),
                Rect::from_min_max(Pos2::new(left, top), Pos2::new(split, bot)),
                Rect::from_min_max(Pos2::new(u0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.image(
                tex.id(),
                Rect::from_min_max(Pos2::new(split, top), Pos2::new(left + w, bot)),
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(u0, 1.0)),
                Color32::WHITE,
            );
        }
        (None, Proj::Globe) => {
            painter.circle_filled(
                pos(p.view.cx, p.view.cy),
                p.r as f32,
                rgb(picture::OCEAN.map(|x| x as u8)),
            );
        }
        (None, Proj::Flat) => {
            painter.rect_filled(
                Rect::from_min_max(
                    pos(p.flat_x(-180.0), p.flat_y(90.0)),
                    pos(p.flat_x(180.0), p.flat_y(-90.0)),
                ),
                0.0,
                rgb(picture::OCEAN.map(|x| x as u8)),
            );
        }
    }

    // The graticule, the Maidenhead fields, the map's lines.
    let geo = geo::geo();
    let grat = Stroke::new(0.8, Color32::from_rgb(0x16, 0x27, 0x3a));
    for lon in (-180..180).step_by(30) {
        lines(
            &(0..=60).map(|i| vec_of(-90.0 + i as f64 * 3.0, lon as f64)).collect::<Vec<_>>(),
            grat,
        );
    }
    for lat in [-60, -30, 0, 30, 60] {
        lines(
            &(0..=120).map(|i| vec_of(lat as f64, -180.0 + i as f64 * 3.0)).collect::<Vec<_>>(),
            grat,
        );
    }
    if s.grid {
        let st_ = Stroke::new(0.8, Color32::from_rgb(0x26, 0x40, 0x5c));
        for lon in (-180..180).step_by(20) {
            lines(
                &(0..=60).map(|i| vec_of(-90.0 + i as f64 * 3.0, lon as f64)).collect::<Vec<_>>(),
                st_,
            );
        }
        for lat in (-80..=80).step_by(10) {
            lines(
                &(0..=120).map(|i| vec_of(lat as f64, -180.0 + i as f64 * 3.0)).collect::<Vec<_>>(),
                st_,
            );
        }
        for a in 0..18u8 {
            for b in 0..18u8 {
                let v = vec_of(b as f64 * 10.0 - 85.0, a as f64 * 20.0 - 170.0);
                let sp = p.project(v);
                if sp.z > 0.35 {
                    painter.text(
                        pos(sp.x, sp.y),
                        egui::Align2::CENTER_CENTER,
                        format!("{}{}", (b'A' + a) as char, (b'A' + b) as char),
                        egui::FontId::monospace(10.0),
                        Color32::from_rgb(0x5e, 0x7c, 0x9c),
                    );
                }
            }
        }
    }
    if s.borders {
        let ss = Stroke::new(0.6, Color32::from_rgb(0x3a, 0x54, 0x70));
        for l in &geo.states {
            lines(l, ss);
        }
        let bs = Stroke::new(0.9, Color32::from_rgb(0x6f, 0x8f, 0xb2));
        for l in &geo.borders {
            lines(l, bs);
        }
    }
    let cs = Stroke::new(1.0, Color32::from_rgb(0x86, 0xa7, 0xcc));
    for l in &geo.coast {
        lines(l, cs);
    }
    let sun_at = sun::subsolar(now);
    let sun_v = vec_ll(sun_at);
    if s.grey {
        for run in p.runs(&small_circle(sun_v, -6.0, 240), None) {
            painter.add(egui::Shape::dashed_line(
                &run.iter().map(|&(x, y)| pos(x, y)).collect::<Vec<_>>(),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xa9, 0xc4, 0xff, 180)),
                4.0,
                5.0,
            ));
        }
        lines(
            &small_circle(sun_v, 0.0, 240),
            Stroke::new(1.6, Color32::from_rgba_unmultiplied(0xd6, 0xe4, 0xff, 230)),
        );
    }
    if s.proj == Proj::Globe {
        painter.circle_stroke(
            pos(p.view.cx, p.view.cy),
            p.r as f32,
            Stroke::new(1.0, Color32::from_rgb(0x2a, 0x4a, 0x6e)),
        );
    }

    // Cities.
    if s.cities {
        let tier_min = if g.zoom >= 2.4 {
            0
        } else if g.zoom >= 1.7 {
            1
        } else if g.zoom >= 1.25 {
            2
        } else {
            3
        };
        let mut taken: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
        for c in &geo.places {
            let sp = p.project(c.v);
            if sp.z < 0.15 || !rect.contains(pos(sp.x, sp.y)) {
                continue;
            }
            painter.circle_filled(
                pos(sp.x, sp.y),
                1.3,
                Color32::from_rgba_unmultiplied(0xc9, 0xd3, 0xde, 190),
            );
            if c.tier < tier_min {
                continue;
            }
            let (gx, gy) =
                (((sp.x + 4.0) / 64.0).floor() as i32, ((sp.y - 5.0) / 12.0).floor() as i32);
            let w = ((c.name.len() as f64 * 5.6 + 4.0) / 64.0).ceil() as i32;
            if (0..=w).any(|k| taken.contains(&(gx + k, gy))) {
                continue;
            }
            for k in 0..=w {
                taken.insert((gx + k, gy));
            }
            painter.text(
                pos(sp.x + 4.0, sp.y),
                egui::Align2::LEFT_CENTER,
                &c.name,
                egui::FontId::proportional(10.0),
                INK_DIM,
            );
        }
    }

    // Paths, stations, the station.
    let qth = st.pos();
    let sel = shown.iter().find(|r| is_sel(g, r)).cloned();
    if let Some(q) = qth {
        let qv = vec_ll(q);
        if s.paths {
            for r in shown.iter().filter(|r| !is_sel(g, r)) {
                let km = distance_km(q, r.pos);
                let pts = great_circle(qv, vec_ll(r.pos), 48);
                let h = arc_lift(km);
                let lift = |i: usize| 1.0 + h * (std::f64::consts::PI * i as f64 / 48.0).sin();
                let age = ((now - r.utc) / 60.0).max(0.0);
                let fade = (1.0 - 0.7 * (age / 45.0).min(1.0)) as f32;
                let c = if s.all_bands {
                    r.band().map(picture::band_colour).unwrap_or([0xc9, 0xd3, 0xde])
                } else {
                    // New paths cyan, dimming to slate blue over 40 minutes.
                    let t = (age / 40.0).min(1.0);
                    let (new, old) = ([0x3f, 0xd0, 0xe8], [0x3a, 0x5f, 0x9a]);
                    [0, 1, 2].map(|k| (new[k] as f64 + (old[k] as f64 - new[k] as f64) * t) as u8)
                };
                let stroke = Stroke::new(
                    1.1,
                    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (220.0 * fade) as u8),
                );
                for run in p.runs(&pts, Some(&lift)) {
                    let v: Vec<Pos2> = run.iter().map(|&(x, y)| pos(x, y)).collect();
                    if r.src.reverse() {
                        painter.add(egui::Shape::dashed_line(&v, stroke, 4.0, 4.0));
                    } else {
                        painter.add(egui::Shape::line(v, stroke));
                    }
                }
            }
            if let Some(r) = &sel {
                let km = distance_km(q, r.pos);
                let pts = great_circle(qv, vec_ll(r.pos), 64);
                let h = arc_lift(km);
                let lift = |i: usize| 1.0 + h * (std::f64::consts::PI * i as f64 / 64.0).sin();
                for run in p.runs(&pts, Some(&lift)) {
                    let v: Vec<Pos2> = run.iter().map(|&(x, y)| pos(x, y)).collect();
                    painter.add(egui::Shape::line(
                        v.clone(),
                        Stroke::new(4.0, Color32::from_rgba_unmultiplied(0x4a, 0xa8, 0xff, 90)),
                    ));
                    painter.add(egui::Shape::line(v, Stroke::new(2.0, INK)));
                }
            }
        }
        if s.stations {
            let mut best: Option<(f32, Reception)> = None;
            let click = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
            for r in shown {
                let sp = p.project(vec_ll(r.pos));
                if !sp.seen() {
                    continue;
                }
                let at = pos(sp.x, sp.y);
                let age = ((now - r.utc) / 60.0).max(0.0);
                let selected = is_sel(g, r);
                let c = r.band().map(picture::band_colour).unwrap_or([0xc9, 0xd3, 0xde]);
                if selected {
                    painter.circle_filled(at, 6.0, Color32::from_rgb(0x0b, 0x0c, 0x0d));
                    painter.circle_stroke(at, 6.0, Stroke::new(2.0, INK));
                } else {
                    let a = (255.0 * (1.0 - 0.55 * (age / 45.0).min(1.0))) as u8;
                    let col = if s.all_bands {
                        Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
                    } else {
                        Color32::from_rgba_unmultiplied(0x3f, 0xd0, 0xe8, a)
                    };
                    painter.circle_filled(at, if age < 5.0 { 4.0 } else { 3.0 }, col);
                    painter.circle_stroke(
                        at,
                        if age < 5.0 { 4.0 } else { 3.0 },
                        Stroke::new(1.0, Color32::from_rgb(0x05, 0x06, 0x07)),
                    );
                }
                if selected || age <= 3.0 {
                    painter.text(
                        at + Vec2::new(8.0, -9.0),
                        egui::Align2::LEFT_CENTER,
                        &r.call,
                        egui::FontId::monospace(if selected { 12.0 } else { 10.0 }),
                        if selected { INK } else { INK_DIM },
                    );
                }
                if let Some(cp) = click {
                    let d = cp.distance(at);
                    if d < 10.0 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                        best = Some((d, r.clone()));
                    }
                }
            }
            if let Some((_, r)) = best {
                g.sel = Some((r.call.clone(), r.band(), r.src));
            }
        }
        let qp = p.project(qv);
        if qp.seen() {
            let at = pos(qp.x, qp.y);
            painter.circle_stroke(
                at,
                8.0,
                Stroke::new(3.0, Color32::from_rgba_unmultiplied(5, 6, 7, 180)),
            );
            painter.circle_stroke(at, 7.0, Stroke::new(2.0, QTH_GREEN));
            painter.text(
                at + Vec2::new(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                st.call().unwrap_or_else(|| "QTH".into()),
                egui::FontId::monospace(11.0),
                QTH_GREEN,
            );
        }
    }
    let sp = p.project(sun_v);
    if sp.seen() {
        painter.circle_stroke(pos(sp.x, sp.y), 5.0, Stroke::new(1.2, INK));
        painter.circle_filled(pos(sp.x, sp.y), 1.6, INK);
    }

    // The view keys and the legend, over the map.
    let mut ui_over = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)));
    ui_over.horizontal(|ui| {
        ui.label(caps("VIEW"));
        let here = qth.map(vec_ll);
        if chrome::chip(ui, false, "HOME").on_hover_text("Centre on your station").clicked()
            && let Some(q) = qth
        {
            g.centre = q;
        }
        let sel_v = sel.as_ref().map(|r| vec_ll(r.pos));
        if chrome::chip(ui, false, "DX").on_hover_text("Centre on the station picked").clicked()
            && let Some(v) = sel_v
        {
            g.centre = sphere::ll_of(v);
        }
        if chrome::chip(ui, false, "PATH")
            .on_hover_text("Centre on the middle of the path picked")
            .clicked()
            && let (Some(a), Some(b)) = (here, sel_v)
        {
            g.centre = sphere::ll_of(sphere::slerp(a, b, 0.5));
        }
        if chrome::chip(ui, false, "−").on_hover_text("Zoom out").clicked() {
            g.zoom = (g.zoom / 1.25).max(0.6);
        }
        if chrome::chip(ui, false, "+").on_hover_text("Zoom in").clicked() {
            g.zoom = (g.zoom * 1.25).min(6.0);
        }
    });
    legend(&painter, rect, g, s);
}

fn legend(painter: &egui::Painter, rect: Rect, g: &Globe, s: &GlobeSettings) {
    if !s.prop {
        return;
    }
    let box_ =
        Rect::from_min_size(rect.left_bottom() + Vec2::new(10.0, -64.0), Vec2::new(230.0, 54.0));
    painter.rect_filled(box_, 4.0, Color32::from_rgba_unmultiplied(14, 16, 19, 225));
    painter.rect_stroke(
        box_,
        4.0,
        Stroke::new(1.0, Color32::from_rgb(0x23, 0x27, 0x2c)),
        egui::StrokeKind::Inside,
    );
    let title = if s.all_bands {
        "PROP · ALL BANDS, EACH IN ITS COLOUR".to_string()
    } else {
        format!("PROP · {} M · HEARD PATHS", s.band)
    };
    painter.text(
        box_.left_top() + Vec2::new(10.0, 12.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::monospace(9.5),
        INK_DIM,
    );
    let bar = Rect::from_min_size(box_.left_top() + Vec2::new(10.0, 22.0), Vec2::new(210.0, 8.0));
    let n = 30;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let c = if s.all_bands {
            let bands: Vec<&BandField> = g.fields.iter().collect();
            if bands.is_empty() {
                rgb(picture::RAMP[0])
            } else {
                rgb(picture::band_colour(
                    bands[((t * (bands.len() - 1) as f32).round()) as usize].band,
                ))
            }
        } else {
            rgb(picture::ramp(t).map(|x| x as u8))
        };
        let w = bar.width() / n as f32;
        painter.rect_filled(
            Rect::from_min_size(
                bar.left_top() + Vec2::new(i as f32 * w, 0.0),
                Vec2::new(w + 0.5, bar.height()),
            ),
            0.0,
            c,
        );
    }
    let peak = g
        .fields
        .iter()
        .filter(|f| s.all_bands || f.band == band_static(&s.band))
        .map(|f| f.peak)
        .fold(0.0, f32::max);
    painter.text(
        bar.left_bottom() + Vec2::new(0.0, 10.0),
        egui::Align2::LEFT_CENTER,
        "weak",
        egui::FontId::monospace(9.5),
        INK_DIM,
    );
    painter.text(
        bar.right_bottom() + Vec2::new(0.0, 10.0),
        egui::Align2::RIGHT_CENTER,
        if peak > 0.0 {
            format!("top ≈ {:.0} weighted paths", peak.max(1.0))
        } else {
            "nothing heard yet".into()
        },
        egui::FontId::monospace(9.5),
        INK_DIM,
    );
}

fn row(ui: &mut egui::Ui, k: &str, v: RichText) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 16.0],
            egui::Label::new(RichText::new(k).monospace().size(11.0).color(theme::TEXT())),
        );
        ui.add(egui::Label::new(v).wrap());
    });
}

fn val(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).monospace().size(12.0).color(theme::TEXT_STRONG())
}

fn warn(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).monospace().size(12.0).color(theme::YELLOW())
}

fn alarm(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).monospace().size(12.0).color(theme::ALERT())
}

fn sun_word(e: f64) -> String {
    if e > 0.0 {
        format!("up {:.0}°", e)
    } else if e > -6.0 {
        format!("grey line, {:.0}°", e)
    } else {
        format!("down {:.0}°", -e)
    }
}

fn side(ui: &mut egui::Ui, g: &mut Globe, st: &StationSettings, now: f64, out: &mut GlobeOut) {
    let qth = st.pos();
    // The station picked.
    let sel = g.history.latest().into_iter().find(|r| is_sel(g, r));
    match (&sel, qth) {
        (Some(r), Some(q)) => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&r.call)
                        .monospace()
                        .size(20.0)
                        .strong()
                        .color(theme::TEXT_STRONG()),
                );
                ui.label(RichText::new(&r.grid).monospace().size(12.0).color(theme::TEXT()));
            });
            let snr = r.snr.map(|v| format!(" · {:+.0} dB", v)).unwrap_or_default();
            let who = if r.src.reverse() {
                format!(" · heard you ({})", r.src.label())
            } else {
                String::new()
            };
            ui.label(
                RichText::new(format!(
                    "{} · {}{snr} · {} ago{who}",
                    r.mode,
                    band_label(r.band()),
                    age_text(now, r.utc)
                ))
                .monospace()
                .size(11.0)
                .color(theme::TEXT()),
            );
            let km = distance_km(q, r.pos);
            let brg = bearing_deg(q, r.pos);
            row(
                ui,
                "DISTANCE",
                val(format!("{:.0} {}", st.dist_unit.of_km(km), st.dist_unit.label())),
            );
            row(
                ui,
                "BEARING",
                val(format!(
                    "SP {:03.0}° · LP {:03.0}°",
                    brg.round() % 360.0,
                    (brg + 180.0).round() % 360.0
                )),
            );
            let hops = (km / 3000.0).ceil().max(1.0);
            row(ui, "HOPS", val(format!("{hops:.0} × F2, about {:.0} km each", km / hops)));
            row(ui, "SUN AT DX", val(sun_word(sun::elevation(r.pos, now))));
            row(ui, "SUN HERE", val(sun_word(sun::elevation(q, now))));
            ui.horizontal(|ui| {
                let pick = tune_pick(r);
                let label = match &pick {
                    Some(p) => format!("TUNE {}", scope::digimodes::mhz(p.dial)),
                    None => "TUNE".into(),
                };
                let t = chrome::chip_enabled(ui, pick.is_some(), false, &label).on_hover_text(
                    match &pick {
                        Some(_) => "Its mode's dial on its band, as the DATA key would go",
                        None => "TUNE goes to a data mode's dial; this mode has none",
                    },
                );
                if t.clicked() {
                    out.tune = pick;
                }
                if chrome::chip_enabled(ui, r.row.is_some(), false, "SHOW IN DECODE").clicked() {
                    out.show_row = r.row;
                }
            });
        }
        _ => {
            ui.label(caps("STATION"));
            ui.label(
                RichText::new("Click a station on the map, or a line in HEARD, for its path.")
                    .size(11.5)
                    .color(theme::TEXT()),
            );
        }
    }
    ui.separator();

    // Propagation.
    ui.label(caps("PROPAGATION"));
    let sonde = qth.and_then(|q| {
        g.got
            .sondes
            .iter()
            .map(|s| (distance_km(q, s.pos), s))
            .filter(|(d, _)| *d <= 3000.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
    });
    match sonde {
        Some((d, so)) => row(
            ui,
            "MUF 3000",
            val(format!(
                "{:.1} MHz · {}, {:.0} {}, {} old",
                so.mufd,
                so.name,
                st.dist_unit.of_km(d),
                st.dist_unit.label(),
                age_text(now, so.utc)
            )),
        ),
        None => row(
            ui,
            "MUF 3000",
            val(if g.got.sondes.is_empty() {
                "waiting for KC2G"
            } else {
                "no sounder within 3000 km"
            }),
        ),
    }
    match g.floor {
        Some(f) => row(ui, "HEARD ≥", val(format!("{f:.1} MHz from paths heard"))),
        None => row(ui, "HEARD ≥", val("not enough paths near you yet")),
    }
    if let (Some(f), Some((_, so))) = (g.floor, sonde)
        && f > so.mufd
    {
        ui.label(
            RichText::new("Decodes show the band better than the sounder's model.")
                .size(11.0)
                .color(theme::CYAN()),
        );
    }
    ui.add_space(4.0);
    ui.label(caps("BANDS OPEN · SHARE OF THE WORLD REACHED"));
    let best = g.fields.iter().map(|f| f.reach).fold(0.0001, f64::max);
    let scale = [0.01, 0.03, 0.1, 0.3, 1.0].into_iter().find(|x| *x >= best).unwrap_or(1.0);
    for f in &g.fields {
        ui.horizontal(|ui| {
            ui.add_sized(
                [40.0, 14.0],
                egui::Label::new(
                    RichText::new(format!("{} m", f.band))
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT()),
                ),
            );
            let (r, _) = ui.allocate_exact_size(Vec2::new(170.0, 8.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, theme::FILL());
            let mut fill = r;
            fill.set_width(r.width() * (f.reach / scale).min(1.0) as f32);
            ui.painter().rect_filled(fill, 2.0, rgb(picture::band_colour(f.band)));
            ui.label(
                RichText::new(format!("{:.1}%", f.reach * 100.0))
                    .monospace()
                    .size(11.0)
                    .color(theme::TEXT_STRONG()),
            );
        });
    }
    let quiet: Vec<&str> =
        BANDS.iter().copied().filter(|b| !g.fields.iter().any(|f| f.band == *b)).collect();
    ui.label(
        RichText::new(format!(
            "Scale tops at {:.0}%.{}",
            scale * 100.0,
            if quiet.len() < BANDS.len() && !quiet.is_empty() {
                format!(" Nothing heard on {} m.", quiet.join(", "))
            } else {
                String::new()
            }
        ))
        .size(10.5)
        .color(theme::TEXT()),
    );
    ui.separator();

    // Space weather.
    ui.label(caps("SPACE WEATHER"));
    let sw = &g.got.space;
    let dash = || "—".to_string();
    egui::Grid::new("globe-space").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
        let kp = sw.kp;
        let cell = |ui: &mut egui::Ui, k: &str, v: RichText| {
            ui.vertical(|ui| {
                ui.label(RichText::new(k).size(9.5).strong().color(theme::TEXT()));
                ui.label(v);
            });
        };
        cell(ui, "SFI", val(sw.sfi.map(|v| format!("{v:.0}")).unwrap_or_else(dash)));
        cell(
            ui,
            "KP",
            match kp {
                // Kp 4 is worth watching (yellow); 5 is a geomagnetic storm.
                Some(k) if k >= 5.0 => alarm(format!("{k:.1}")),
                Some(k) if k >= 4.0 => warn(format!("{k:.1}")),
                Some(k) => val(format!("{k:.1}")),
                None => val(dash()),
            },
        );
        cell(ui, "A", val(sw.a.map(|v| format!("{v:.0}")).unwrap_or_else(dash)));
        cell(
            ui,
            "X-RAY",
            match sw.xray {
                Some(x) if x >= 1e-4 => alarm(fetch::xray_class(x)),
                Some(x) if x >= 1e-5 => warn(fetch::xray_class(x)),
                Some(x) => val(fetch::xray_class(x)),
                None => val(dash()),
            },
        );
        ui.end_row();
        cell(
            ui,
            "BZ nT",
            match sw.bz {
                Some(b) if b <= -10.0 => warn(format!("{b:+.1}")),
                Some(b) => val(format!("{b:+.1}")),
                None => val(dash()),
            },
        );
        cell(ui, "WIND km/s", val(sw.wind.map(|v| format!("{v:.0}")).unwrap_or_else(dash)));
        let here = qth.zip(g.got.aurora.as_ref()).map(|(q, a)| a.at(q));
        cell(
            ui,
            "AURORA HERE",
            match here {
                Some(p) if p >= 25.0 => warn(format!("{p:.0}%")),
                Some(p) => val(format!("{p:.0}%")),
                None => val(dash()),
            },
        );
        ui.end_row();
    });
    if !sw.kp_bins.is_empty() {
        ui.label(
            RichText::new("KP · LAST DAY, THEN FORECAST").size(9.5).strong().color(theme::TEXT()),
        );
        let bins: Vec<&(f64, f64, bool)> =
            sw.kp_bins.iter().filter(|b| b.0 >= now - 86_400.0).take(24).collect();
        let (r, _) = ui.allocate_exact_size(Vec2::new(270.0, 34.0), Sense::hover());
        let n = bins.len().max(1) as f32;
        let w = r.width() / n;
        for (i, b) in bins.iter().enumerate() {
            let h = (b.1 / 9.0).clamp(0.02, 1.0) as f32 * r.height();
            let c = if b.1 >= 5.0 {
                theme::ALERT()
            } else if b.1 >= 4.0 {
                theme::YELLOW()
            } else if b.2 {
                theme::CYAN_DIM()
            } else {
                theme::CYAN()
            };
            let bar = Rect::from_min_max(
                Pos2::new(r.left() + i as f32 * w + 1.0, r.bottom() - h),
                Pos2::new(r.left() + (i + 1) as f32 * w - 1.0, r.bottom()),
            );
            ui.painter().rect_filled(bar, 1.0, c);
        }
        ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, theme::LINE()));
    }
    ui.separator();

    // The weather at the station.
    ui.label(caps(&format!("WEATHER AT {}", st.call().unwrap_or_else(|| "THE STATION".into()))));
    match &g.got.wx {
        Some(w) => {
            let (words, storm) = w.code.map(fetch::wmo_words).unwrap_or(("—", false));
            let now_t = format!(
                "{} · {words}",
                w.temp_c.map(|t| format!("{t:.0} °C")).unwrap_or_else(dash)
            );
            row(ui, "NOW", if storm { warn(now_t) } else { val(now_t) });
            if let Some(h) = w.humidity {
                row(ui, "HUMIDITY", val(format!("{h:.0}%")));
            }
            let dir = w.wind_from_deg.map(compass).unwrap_or("");
            row(
                ui,
                "WIND",
                val(format!(
                    "{dir} {} km/h{}",
                    w.wind_kmh.map(|v| format!("{v:.0}")).unwrap_or_else(dash),
                    w.gust_kmh.map(|v| format!(", gusts {v:.0}")).unwrap_or_default()
                )),
            );
            let trend = w
                .trend_hpa
                .map(|t| {
                    if t <= -1.0 {
                        ", falling"
                    } else if t >= 1.0 {
                        ", rising"
                    } else {
                        ", steady"
                    }
                })
                .unwrap_or("");
            row(
                ui,
                "PRESSURE",
                val(format!(
                    "{} hPa{trend}",
                    w.pressure_hpa.map(|p| format!("{p:.0}")).unwrap_or_else(dash)
                )),
            );
        }
        None => row(
            ui,
            "NOW",
            val(if qth.is_some() {
                "waiting for Open-Meteo"
            } else {
                "set your grid in SETTINGS → Station"
            }),
        ),
    }
    if g.got.alerts.is_empty() {
        row(ui, "ALERTS", val("none (NWS, US only)"));
    } else {
        row(ui, "ALERTS", warn(g.got.alerts.join(", ")));
    }
}

fn compass(deg: f64) -> &'static str {
    const P: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    P[((deg.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

/// TUNE: the reception's mode's dial on its band, as that mode's DATA key
/// would go.
fn tune_pick(r: &Reception) -> Option<scope::DataPick> {
    let digi = scope::digimodes::digi(&r.mode)?;
    let band = bandplan::band_at(r.rf_hz)?;
    let dial = *digi.dials.iter().find(|&&d| (d as f64) >= band.lo && (d as f64) <= band.hi)?;
    Some(scope::DataPick { digi, dial, note: None })
}

fn footer(ui: &mut egui::Ui, g: &Globe) {
    let now = decoders::slot::utc_now();
    ui.horizontal(|ui| {
        for (name, st) in &g.got.status {
            let t = match (&st.error, st.ok_at) {
                (Some(e), _) => {
                    let r = ui.label(
                        RichText::new(format!("{name} failed"))
                            .monospace()
                            .size(10.5)
                            .color(theme::YELLOW()),
                    );
                    r.on_hover_text(e.clone());
                    continue;
                }
                (None, Some(at)) => format!("{name} {}", age_text(now, at)),
                (None, None) => format!("{name} …"),
            };
            ui.label(RichText::new(t).monospace().size(10.5).color(theme::TEXT()));
        }
        if g.got.status.is_empty() {
            ui.label(
                RichText::new("Fetching space weather, weather and the MUF…")
                    .monospace()
                    .size(10.5)
                    .color(theme::TEXT()),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} receptions this hour", g.history.len()))
                    .monospace()
                    .size(10.5)
                    .color(theme::TEXT()),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station::grid_to_latlon;

    #[test]
    fn tune_goes_to_the_modes_dial_on_the_stations_band() {
        let r = Reception {
            call: "JA1ABC".into(),
            grid: "PM95".into(),
            pos: grid_to_latlon("PM95").unwrap(),
            rf_hz: 21_075_400.0,
            mode: "FT8".into(),
            snr: Some(-16.0),
            utc: 0.0,
            src: Source::Decode,
            row: Some(3),
        };
        let p = tune_pick(&r).unwrap();
        assert_eq!(p.dial, 21_074_000);
        assert_eq!(p.digi.name, "FT8");
        // CW has no data dial.
        assert!(tune_pick(&Reception { mode: "CW".into(), ..r }).is_none());
    }

    #[test]
    fn compass_points() {
        assert_eq!(compass(310.0), "NW");
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(135.0), "SE");
    }
}
