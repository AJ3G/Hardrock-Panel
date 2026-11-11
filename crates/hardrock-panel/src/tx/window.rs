//! The TX window: mode, ENABLE / HALT / STOP QSO, the slot bar, the decode
//! list for working stations, the QSO with its transcript and WSJT-X's six
//! message lines, the queue, and WSPR's beacon settings. Laid out after
//! notes/tx-mockup.png.

use eframe::egui::{self, Color32, RichText, Sense, vec2};
use rigcat::RigLink;

use super::{
    FreqMode, ListRow, Sort, Tx, TxMode, TxSettings, WsprKey, WsprRow, WsprSort, sched, wave,
};
use crate::globe::field::Reception;
use crate::globe::wspr::WsprReport;
use crate::station::{self, StationSettings};
use crate::{chrome, theme};

/// What the window asks of the panel.
#[derive(Default)]
pub(crate) struct WindowOut {
    pub pop: Option<bool>,
    /// DECODE's row to pick, for GLOBE and the scope.
    pub pick_row: Option<u64>,
    /// Set DECODE to FT8 (or WSPR) and open it.
    pub decode_mode: Option<TxMode>,
}

/// Where things stand outside the window.
pub(crate) struct Ctx<'a> {
    pub now: f64,
    pub link: Option<&'a RigLink>,
    pub station: &'a StationSettings,
    /// What DECODE is running, if anything.
    pub decoding: Option<&'static str>,
    /// Why a transmission would be refused now, if it would be.
    pub blocked: Option<String>,
    /// The planned drive at this mode's level, W, and the radio's band.
    pub watts: Option<f32>,
    pub band: Option<&'static str>,
    /// The radio, for the drive beside each LEVEL.
    pub rig: Option<&'a rigcat::RigStatus>,
    /// Who heard the station, as GLOBE fetched it: PSK Reporter's reports
    /// and WSPR HEARD ME's.
    pub heard_me: (&'a [Reception], &'a [WsprReport]),
    /// The true RF the panel listens on, Hz: for the WSPR lists' band.
    pub heard_hz: Option<f64>,
    /// The transverter's offset the panel knows of, Hz (0 without one).
    pub xvtr_hz: f64,
}

pub(crate) fn window(
    ctx: &egui::Context,
    open: &mut bool,
    tx: &mut Tx,
    s: &mut TxSettings,
    c: Ctx<'_>,
) -> WindowOut {
    let mut out = WindowOut::default();
    let title = match s.mode {
        TxMode::Ft8 => "TX · FT8",
        TxMode::Wspr => "TX · WSPR",
    };
    crate::repaint::after(ctx, std::time::Duration::from_millis(200));
    if s.popped {
        let _shut = crate::gpu_gate::shut();
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("tx-window"),
            crate::popout::builder(ctx, "tx-window", title, [1180.0, 760.0], [760.0, 480.0]),
            |ui, _class| {
                crate::popout::track(ui.ctx(), "tx-window");
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        body(ui, true, tx, s, &c, &mut out);
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        egui::Window::new(title)
            .id(egui::Id::new("tx-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([40.0, 60.0])
            .default_size([1120.0, 660.0])
            .min_size([640.0, 360.0])
            // Within the screen, below the top bar: its room would otherwise
            // grow with what it holds.
            .max_height((ctx.content_rect().height() - 80.0).max(360.0))
            .max_width((ctx.content_rect().width() - 20.0).max(640.0))
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                body(ui, false, tx, s, &c, &mut out);
            });
    }
    out
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(RichText::new(t).size(10.5).color(theme::CYAN_DIM()).strong());
}

fn mono(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).monospace().size(12.0)
}

/// The transmit colour, as GLOBE's strobe has it.
fn tx_ink() -> Color32 {
    crate::globe::TX_COLOUR
}

fn body(ui: &mut egui::Ui, popped: bool, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>, out: &mut WindowOut) {
    if tx.lines.iter().all(|l| l.is_empty()) {
        tx.generate(s);
    }
    // The top row.
    ui.horizontal_wrapped(|ui| {
        caption(ui, "MODE");
        let keyed = tx.over().is_some();
        for (m, l) in [(TxMode::Ft8, "FT8"), (TxMode::Wspr, "WSPR")] {
            if chrome::chip_enabled(ui, !keyed, s.mode == m, l).clicked() && s.mode != m {
                s.mode = m;
                tx.enabled = false;
                tx.note = None;
            }
        }
        ui.add_space(10.0);
        let label = if tx.enabled { "TX ENABLED" } else { "ENABLE TX" };
        if chrome::chip(ui, tx.enabled, label)
            .on_hover_text(
                "The master switch: off at every start. Nothing is sent without it, and an over \
                 stopped by an interlock turns it off.",
            )
            .clicked()
        {
            if tx.enabled {
                tx.disable(s);
            } else {
                tx.enable(s);
            }
        }
        if chrome::chip(ui, false, "HALT TX").on_hover_text("Unkey now, and turn ENABLE TX off").clicked() {
            tx.halt(c.link);
        }
        if s.mode == TxMode::Ft8
            && chrome::chip(ui, false, "STOP QSO")
                .on_hover_text("Send nothing more in this exchange; the over going out finishes")
                .clicked()
        {
            tx.qso.stop();
            tx.hand = None;
        }
        ui.add_space(10.0);
        let call = c.station.call().unwrap_or_else(|| "no call".into());
        let grid = c.station.grid().unwrap_or_else(|| "no grid".into());
        ui.label(RichText::new("AS").size(10.5).color(theme::CYAN_DIM()));
        ui.label(mono(format!("{call} · {grid}")).color(theme::TEXT_STRONG()));
        let pop = if popped { "DOCK" } else { "POP OUT" };
        if chrome::chip(ui, false, pop).clicked() {
            out.pop = Some(!popped);
        }
    });
    ui.add_space(4.0);
    slot_bar(ui, tx, s, c);
    // The interlocks, as they stand.
    let (text, col) = match &c.blocked {
        Some(why) => (format!("Would not key: {why}"), theme::YELLOW()),
        None => (
            format!(
                "Keyed through the TUNE path: {} · drive {} · TX IN USB audio · any interlock \
                 failing unkeys at once",
                c.band.unwrap_or("band ?"),
                c.watts.map_or("?".into(), |w| format!("{w:.1} W"))
            ),
            theme::GREEN(),
        ),
    };
    ui.label(RichText::new(text).size(11.0).color(col));
    // DECODE must run the mode for the sequencer (FT8) to hear anything.
    let want = match s.mode {
        TxMode::Ft8 => "FT8",
        TxMode::Wspr => "WSPR",
    };
    if c.decoding != Some(want) {
        ui.horizontal(|ui| {
            let now = c.decoding.map_or("not running".to_string(), |k| format!("on {k}"));
            ui.label(RichText::new(format!("DECODE is {now}.")).size(11.5).color(theme::TEXT()));
            if chrome::chip(ui, false, format!("DECODE {want}")).clicked() {
                out.decode_mode = Some(s.mode);
            }
        });
    }
    if let Some(n) = &tx.note {
        ui.label(RichText::new(n).size(11.5).color(theme::YELLOW()));
    }
    ui.add_space(4.0);
    // The two columns share what is left of the window, each scrolling
    // its own. Nothing in them asks for more room than it is given, so the
    // window resizes freely both ways (AJ3G, 11 Nov 2026: made smaller, it
    // grew back).
    let area = ui.available_rect_before_wrap();
    let area = egui::Rect::from_min_size(area.min, area.size().max(vec2(0.0, 0.0)));
    let gap = 13.0;
    let left_w = ((area.width() - gap) * 0.56).max(0.0);
    let left = egui::Rect::from_min_size(area.min, vec2(left_w, area.height()));
    let right = egui::Rect::from_min_max(egui::pos2(left.right() + gap, area.top()), area.max);
    column(ui, left, |ui| decode_list(ui, tx, s, c, out));
    let x = left.right() + gap / 2.0;
    ui.painter().vline(x, area.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
    column(ui, right, |ui| {
        let col_h = ui.available_height();
        let id = egui::Id::new("tx-right-rest");
        let o = egui::ScrollArea::vertical().id_salt("tx-right").auto_shrink([false, false]).show(ui, |ui| match s.mode {
            TxMode::Ft8 => {
                let rest = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(col_h);
                qso_panel(ui, tx, s, c, (col_h - rest).max(0.0))
            }
            TxMode::Wspr => {
                wspr_panel(ui, tx, s, c);
                0.0
            }
        });
        // What the panel holds besides the transcript, for next frame's
        // share of the room.
        let rest = o.content_size.y - o.inner;
        ui.ctx().data_mut(|d| d.insert_temp(id, rest));
    });
    ui.advance_cursor_after_rect(area);
}

/// A column of the window in `rect`, clipped to it. A child that is not
/// allocated in the window: whatever it holds, even a few points more than
/// `rect` (a footer measured a hair short), never becomes the window's
/// content size, which egui's resize keeps the window at least as big as —
/// the window grew back after being made shorter (AJ3G, 11 Nov 2026). The
/// caller claims `rect`'s room itself.
fn column(ui: &mut egui::Ui, rect: egui::Rect, add: impl FnOnce(&mut egui::Ui)) {
    let b = egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min));
    let mut child = ui.new_child(b);
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    add(&mut child);
}

/// The slot bar: cyan while listening, the transmit colour while keyed,
/// with what is going out and when the slot ends; the DT readout beside it.
fn slot_bar(ui: &mut egui::Ui, tx: &Tx, s: &TxSettings, c: &Ctx<'_>) {
    let period = match s.mode {
        TxMode::Ft8 => sched::FT8_PERIOD,
        TxMode::Wspr => sched::WSPR_PERIOD,
    };
    let t0 = sched::slot_start(c.now, period);
    let frac = ((c.now - t0 as f64) / period as f64).clamp(0.0, 1.0) as f32;
    ui.horizontal(|ui| {
        caption(ui, "SLOT");
        let w = (ui.available_width() - 130.0).max(120.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 2.0, theme::INPUT_BG());
        let keyed = tx.on_air();
        let fill = if keyed { tx_ink() } else { theme::CYAN().gamma_multiply(0.6) };
        let mut r = rect;
        r.set_width(rect.width() * frac);
        p.rect_filled(r, 2.0, fill);
        if let Some((_, mode, _, end)) = tx.over() {
            let len = match mode {
                TxMode::Ft8 => sched::FT8_PERIOD,
                TxMode::Wspr => sched::WSPR_PERIOD,
            } as f64;
            let x = rect.left() + rect.width() * (((end - t0 as f64) / len).clamp(0.0, 1.0) as f32);
            p.vline(x, rect.y_range(), egui::Stroke::new(1.5, theme::TEXT_STRONG()));
        }
        let even = sched::is_even(t0, period);
        let what = match tx.over() {
            Some((text, ..)) if keyed => format!("TRANSMITTING · {text}"),
            Some((text, ..)) => format!("keying · {text}"),
            None if !tx.enabled => "TX off".to_string(),
            None => match s.mode {
                TxMode::Ft8 => format!(
                    "receiving · {} · our turn {} in {:.0} s",
                    if even { "even" } else { "odd" },
                    if tx.tx_even { "even" } else { "odd" },
                    sched::ft8_next_in(c.now, tx.tx_even)
                ),
                TxMode::Wspr => {
                    let call = c.station.call().unwrap_or_default();
                    match sched::wspr_next_in(&call, c.now, s.wspr_pct) {
                        Some(t) => format!("receiving · next beacon in {:.0}:{:02}", t as i64 / 60, t as i64 % 60),
                        None => "receiving · TRANSMIT is off".into(),
                    }
                }
            },
        };
        let ink = theme::TEXT_STRONG();
        p.text(rect.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, what, egui::FontId::monospace(11.0), ink);
        p.text(
            rect.right_center() - vec2(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!("{:.1} s", (t0 + period) as f64 - c.now),
            egui::FontId::monospace(11.0),
            theme::TEXT(),
        );
        // DT: our clock against the stations heard; amber past 0.5 s.
        let (dt, col) = match tx.dt {
            Some(d) if d.abs() > 0.5 => (format!("DT {d:+.2} s"), theme::YELLOW()),
            Some(d) => (format!("DT {d:+.2} s"), theme::TEXT()),
            None => ("DT –".into(), theme::TEXT()),
        };
        ui.label(mono(dt).color(col))
            .on_hover_text("Your clock against the stations you hear (the median of the last slot). Past 0.5 s, sync Windows' time.");
    });
}

fn decode_list(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>, out: &mut WindowOut) {
    if s.mode == TxMode::Wspr {
        wspr_list(ui, tx, s, c);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("DECODES").strong().color(theme::TEXT_STRONG()));
        ui.add_space(8.0);
        caption(ui, "SORT");
        for (k, l) in [(Sort::Time, "Time"), (Sort::Snr, "SNR"), (Sort::Distance, "Dist"), (Sort::Country, "Country")] {
            if chrome::chip(ui, s.sort == k, l).clicked() {
                s.sort = k;
            }
        }
        if chrome::chip(ui, s.cq_only, "CQ only").clicked() {
            s.cq_only = !s.cq_only;
        }
        if chrome::chip(ui, s.new_only, "New only").clicked() {
            s.new_only = !s.new_only;
        }
        if chrome::chip(ui, false, "CLEAR").clicked() {
            tx.rows.clear();
        }
    });
    let turns = slot_groups(&tx.rows);
    let me_call = c.station.call().unwrap_or_default();
    let dx = tx.qso.dx_call().map(str::to_string);
    let mut act: Option<(usize, Act)> = None;
    let foot = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
    let list_h = (ui.available_height() - foot).max(0.0);
    egui::ScrollArea::vertical().id_salt("tx-decodes").auto_shrink([false, false]).max_height(list_h).show(ui, |ui| {
        for (t, mut idx) in turns {
            let rows = &tx.rows;
            idx.retain(|&i| {
                let r = &rows[i];
                r.to_me || r.ours || ((!s.cq_only || r.d.is_cq) && (!s.new_only || r.badges.new_call || r.badges.new_dxcc || r.badges.new_grid || r.badges.new_band))
            });
            if idx.is_empty() {
                continue;
            }
            match s.sort {
                Sort::Time => {}
                Sort::Snr => idx.sort_by_key(|&i| std::cmp::Reverse(rows[i].d.snr_db)),
                Sort::Distance => idx.sort_by(|&a, &b| {
                    let d = |i: usize| rows[i].path.map_or(-1.0, |p| p.1);
                    d(b).total_cmp(&d(a))
                }),
                Sort::Country => idx.sort_by(|&a, &b| rows[a].country.cmp(&rows[b].country)),
            }
            idx.sort_by_key(|&i| (!rows[i].ours, !rows[i].to_me));
            let even = sched::is_even(t, sched::FT8_PERIOD);
            ui.label(
                RichText::new(format!("{} · {} · {} decodes", crate::decode::hms(t as f64), if even { "EVEN" } else { "ODD" }, idx.len()))
                    .size(10.5)
                    .color(theme::CYAN_DIM()),
            );
            for i in idx {
                if let Some(a) = row_ui(ui, &tx.rows[i], tx.picked == Some(tx.rows[i].row), dx.as_deref(), &me_call) {
                    act = Some((i, a));
                }
            }
        }
        if tx.rows.is_empty() {
            ui.label(RichText::new("Nothing heard yet.").size(11.5).color(theme::TEXT()));
        }
    });
    ui.label(
        RichText::new("Double-click = REPLY (turns ENABLE TX on) · Shift+click sets TX audio only · + queues")
            .size(10.5)
            .color(theme::CYAN_DIM()),
    );
    let Some((i, a)) = act else { return };
    let r = tx.rows[i].clone();
    match a {
        Act::Pick { shift } => {
            tx.picked = Some(r.row);
            if r.row != 0 {
                out.pick_row = Some(r.row);
            }
            // MANUAL: a click moves the TX audio there; HOLD never; AUTO
            // chooses for itself. Shift sets the audio only, in any but HOLD.
            if s.freq == FreqMode::Manual || (shift && s.freq == FreqMode::Auto) {
                s.audio_hz = r.d.audio_hz.round();
                if s.freq == FreqMode::Auto {
                    s.freq = FreqMode::Manual;
                }
            }
        }
        Act::Reply => {
            tx.reply(s, &r, c.now);
            if s.mode == TxMode::Ft8 {
                tx.enable(s);
            }
            tx.picked = Some(r.row);
            if s.freq == FreqMode::Manual {
                s.audio_hz = r.d.audio_hz.round();
            }
        }
        Act::Queue => tx.queue(&r),
    }
}

/// The list's turns, newest slot first, one for each slot however late its
/// decodes came (a slot's that finish after the next one's made a second
/// heading for it: AJ3G, 11 Nov 2026), each with its rows' indexes.
fn slot_groups(rows: &std::collections::VecDeque<ListRow>) -> Vec<(i64, Vec<usize>)> {
    let mut by_slot: std::collections::BTreeMap<i64, Vec<usize>> = Default::default();
    for (i, r) in rows.iter().enumerate() {
        by_slot.entry(r.d.slot_utc).or_default().push(i);
    }
    by_slot.into_iter().rev().collect()
}

/// A report of our beacon, from wspr.live, WSPRnet or PSK Reporter.
#[derive(Debug, Clone, PartialEq)]
struct HeardMe {
    utc: f64,
    call: String,
    snr: Option<f32>,
    km: Option<f64>,
    rf_hz: f64,
    site: &'static str,
}

/// The reports of our WSPR beacon since `since`, newest first: one per
/// receiver per 2 min cycle, the WSPR sites' before PSK Reporter's.
fn heard_me(
    pskr: &[Reception],
    wspr: &[WsprReport],
    me: Option<station::LatLon>,
    since: f64,
) -> Vec<HeardMe> {
    let km = |r: &Reception| me.map(|p| station::distance_km(p, r.pos));
    let mut v: Vec<HeardMe> = wspr
        .iter()
        .map(|w| HeardMe { utc: w.rec.utc, call: w.rec.call.clone(), snr: w.rec.snr, km: w.km.or_else(|| km(&w.rec)), rf_hz: w.rec.rf_hz, site: w.site })
        .chain(pskr.iter().filter(|r| r.mode.eq_ignore_ascii_case("WSPR")).map(|r| HeardMe {
            utc: r.utc,
            call: r.call.clone(),
            snr: r.snr,
            km: km(r),
            rf_hz: r.rf_hz,
            site: "PSK Reporter",
        }))
        .filter(|m| m.utc >= since)
        .collect();
    let cycle = |t: f64| sched::slot_start(t, sched::WSPR_PERIOD);
    let mut seen = std::collections::HashSet::new();
    v.retain(|m| seen.insert((m.call.clone(), cycle(m.utc))));
    v.sort_by(|a, b| b.utc.total_cmp(&a.utc));
    v
}

/// A column of a WSPR table: its heading, its width in characters, whether
/// its figures sit to the right, and the sort its heading sets, if any.
struct Col {
    head: &'static str,
    chars: usize,
    right: bool,
    key: Option<WsprKey>,
}

const fn col(head: &'static str, chars: usize, right: bool, key: Option<WsprKey>) -> Col {
    Col { head, chars, right, key }
}

/// HEARD's columns: what our decoder heard (decision 90).
const HEARD_COLS: [Col; 10] = [
    col("UTC", 4, false, Some(WsprKey::Utc)),
    col("SNR", 3, true, Some(WsprKey::Snr)),
    col("DRIFT", 3, true, None),
    col("MHz", 11, true, None),
    col("CALL", 10, false, Some(WsprKey::Call)),
    col("GRID", 6, false, None),
    col("DBM", 3, true, None),
    col("KM", 5, true, Some(WsprKey::Km)),
    col("BRG", 4, true, None),
    col("HZ", 4, true, None),
];

/// HEARD ME's columns: who heard our beacon.
const ME_COLS: [Col; 6] = [
    col("UTC", 4, false, Some(WsprKey::Utc)),
    col("CALL", 10, false, Some(WsprKey::Call)),
    col("SNR", 3, true, Some(WsprKey::Snr)),
    col("KM", 5, true, Some(WsprKey::Km)),
    col("MHz", 11, true, None),
    col("FROM", 12, false, None),
];

/// What the WSPR lists sort by.
trait WsprCols {
    fn utc(&self) -> f64;
    fn snr(&self) -> Option<f32>;
    fn call(&self) -> &str;
    fn km(&self) -> Option<f64>;
    fn rf_hz(&self) -> f64;
}

impl WsprCols for WsprRow {
    fn utc(&self) -> f64 {
        self.h.slot_utc as f64
    }
    fn snr(&self) -> Option<f32> {
        Some(self.h.snr_db)
    }
    fn call(&self) -> &str {
        &self.h.call
    }
    fn km(&self) -> Option<f64> {
        self.path.map(|p| p.1)
    }
    fn rf_hz(&self) -> f64 {
        self.h.rf_hz
    }
}

impl WsprCols for HeardMe {
    fn utc(&self) -> f64 {
        self.utc
    }
    fn snr(&self) -> Option<f32> {
        self.snr
    }
    fn call(&self) -> &str {
        &self.call
    }
    fn km(&self) -> Option<f64> {
        self.km
    }
    fn rf_hz(&self) -> f64 {
        self.rf_hz
    }
}

/// The rows on `band` (all of them when the band isn't known, and any
/// without a frequency), sorted as `sort` says: rows with no figure for
/// the column go last either way (decisions 90, 92).
fn shown_rows<'a, T: WsprCols + 'a>(
    rows: impl Iterator<Item = &'a T>,
    band: Option<&str>,
    sort: WsprSort,
) -> Vec<&'a T> {
    use std::cmp::Ordering;
    let mut v: Vec<&T> = rows
        .filter(|r| {
            band.is_none_or(|b| r.rf_hz() <= 0.0 || crate::logbook::adif_band(r.rf_hz()) == b)
        })
        .collect();
    let num = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) if sort.desc => b.total_cmp(&a),
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    v.sort_by(|a, b| match sort.key {
        WsprKey::Utc => num(Some(a.utc()), Some(b.utc())),
        WsprKey::Snr => num(a.snr().map(f64::from), b.snr().map(f64::from)),
        WsprKey::Km => num(a.km(), b.km()),
        WsprKey::Call => {
            let o = a.call().cmp(b.call());
            if sort.desc { o.reverse() } else { o }
        }
    });
    v
}

/// The width of each column, px: its figures or its heading with the sort
/// arrow, whichever is wider, and a space.
fn col_widths(ui: &egui::Ui, cols: &[Col]) -> Vec<f32> {
    let cw = ui.fonts_mut(|f| f.glyph_width(&egui::FontId::monospace(12.0), '0'));
    cols.iter().map(|c| (c.chars.max(c.head.chars().count() + 2) + 1) as f32 * cw).collect()
}

/// A table's headings, in its columns. A sortable one is in the accent
/// colour and underlined under the pointer; the sorted one is bright, with
/// ▼ or ▲. A click sorts by it, a second the other way.
fn table_head(ui: &mut egui::Ui, salt: &str, cols: &[Col], widths: &[f32], sort: &mut WsprSort) {
    let h = 18.0;
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let font = egui::FontId::monospace(12.0);
    let mut x = rect.left();
    for (i, (c, &w)) in cols.iter().zip(widths).enumerate() {
        let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, h));
        x += w;
        let sorted = c.key.is_some() && c.key == Some(sort.key);
        let mut ink = theme::CYAN_DIM();
        let mut hovered = false;
        if let Some(k) = c.key {
            let resp = ui
                .interact(cell, ui.id().with((salt, i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Sort by this column; click again for the other way");
            if resp.clicked() {
                sort.click(k);
            }
            hovered = resp.hovered();
            ink = if sorted || hovered { theme::TEXT_STRONG() } else { theme::CYAN() };
        }
        let arrow = if !sorted {
            ""
        } else if sort.desc {
            "▼"
        } else {
            "▲"
        };
        let (text, pos, align) = if c.right {
            let t =
                if arrow.is_empty() { c.head.to_string() } else { format!("{arrow} {}", c.head) };
            (
                t,
                cell.right_center() - vec2(ui.fonts_mut(|f| f.glyph_width(&font, '0')), 0.0),
                egui::Align2::RIGHT_CENTER,
            )
        } else {
            let t =
                if arrow.is_empty() { c.head.to_string() } else { format!("{} {arrow}", c.head) };
            (t, cell.left_center(), egui::Align2::LEFT_CENTER)
        };
        let r = ui.painter().text(pos, align, text, font.clone(), ink);
        if hovered {
            ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, ink));
        }
    }
}

/// A row of a table: each cell in its column, figures to the right.
fn table_row(ui: &mut egui::Ui, cols: &[Col], widths: &[f32], cells: &[String], ink: Color32) {
    let h = ui.fonts_mut(|f| f.row_height(&egui::FontId::monospace(12.0))) + 2.0;
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let font = egui::FontId::monospace(12.0);
    let cw = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let p = ui.painter();
    let mut x = rect.left();
    for ((c, &w), t) in cols.iter().zip(widths).zip(cells) {
        let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, h));
        x += w;
        if c.right {
            p.text(
                cell.right_center() - vec2(cw, 0.0),
                egui::Align2::RIGHT_CENTER,
                t,
                font.clone(),
                ink,
            );
        } else {
            p.text(cell.left_center(), egui::Align2::LEFT_CENTER, t, font.clone(), ink);
        }
    }
}

/// A WSPR table: its headings, then its rows scrolling under them, the two
/// scrolling sideways together when the column is narrower than the table.
/// By UTC the rows come in their 2 minute cycles; by anything else, one
/// list.
#[allow(clippy::too_many_arguments)]
fn wspr_table<T: WsprCols>(
    ui: &mut egui::Ui,
    salt: &str,
    cols: &[Col],
    rows: &[&T],
    sort: &mut WsprSort,
    height: Option<f32>,
    cells: impl Fn(&T) -> Vec<String>,
    empty: &str,
) {
    let widths = col_widths(ui, cols);
    let total: f32 = widths.iter().sum();
    egui::ScrollArea::horizontal().id_salt((salt, "x")).auto_shrink([false, true]).show(ui, |ui| {
        let w = total.max(ui.available_width());
        ui.set_min_width(w);
        table_head(ui, salt, cols, &widths, sort);
        let mut inner =
            egui::ScrollArea::vertical().id_salt((salt, "y")).auto_shrink([false, false]);
        if let Some(h) = height {
            inner = inner.max_height(h);
        }
        inner.show(ui, |ui| {
            ui.set_min_width(w);
            let cycle = |t: f64| sched::slot_start(t, sched::WSPR_PERIOD);
            let mut i = 0;
            while i < rows.len() {
                let n = if sort.key == WsprKey::Utc {
                    let t = cycle(rows[i].utc());
                    let n = rows[i..].iter().take_while(|r| cycle(r.utc()) == t).count();
                    ui.label(
                        RichText::new(format!("{} · {n}", crate::decode::hms(t as f64)))
                            .size(10.5)
                            .color(theme::CYAN_DIM()),
                    );
                    n
                } else {
                    rows.len()
                };
                for r in &rows[i..i + n] {
                    table_row(ui, cols, &widths, &cells(r), theme::TEXT());
                }
                i += n;
            }
            if rows.is_empty() {
                ui.label(RichText::new(empty).size(11.5).color(theme::TEXT()));
            }
        });
    });
}

/// WSPR's list (AJ3G, 11 Nov 2026): who our decoder heard, and who heard
/// our beacon, as tables sorted by a click on a heading (decision 90), on
/// the band the panel is on (decision 92). Nothing to REPLY to.
fn wspr_list(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>) {
    let band = c.heard_hz.map(crate::logbook::adif_band).filter(|b| !b.is_empty());
    let me = heard_me(c.heard_me.0, c.heard_me.1, c.station.pos(), tx.wspr_cleared);
    let heard = shown_rows(tx.wspr_rows.iter(), band, s.wspr_heard_sort);
    let me_shown = shown_rows(me.iter(), band, s.wspr_me_sort);
    let elsewhere = tx.wspr_rows.len() - heard.len() + me.len() - me_shown.len();
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("WSPR").strong().color(theme::TEXT_STRONG()));
        if let Some(b) = band {
            ui.label(RichText::new(b).size(11.0).color(theme::TEXT())).on_hover_text(
                "The lists show this band only; what was heard on other bands is kept for when you go back",
            );
        }
        if elsewhere > 0 {
            ui.label(RichText::new(format!("· {elsewhere} on other bands")).size(10.5).color(theme::CYAN_DIM()));
        }
        clear = chrome::chip(ui, false, "CLEAR").on_hover_text("Empty both lists, on every band").clicked();
    });
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
    let half = ((ui.available_height() - 6.0 * row_h) / 2.0).max(row_h);

    caption(ui, "HEARD");
    wspr_table(
        ui,
        "tx-wspr-heard",
        &HEARD_COLS,
        &heard,
        &mut s.wspr_heard_sort,
        Some(half),
        |r| wspr_heard_cells(r).to_vec(),
        "Nothing heard on this band yet: DECODE runs WSPR while this window is open.",
    );
    caption(ui, "HEARD ME");
    wspr_table(
        ui,
        "tx-wspr-me",
        &ME_COLS,
        &me_shown,
        &mut s.wspr_me_sort,
        None,
        |m| heard_me_cells(m).to_vec(),
        "No reports of your beacon on this band yet. They are asked for every two minutes while you \
         beacon, GLOBE open or shut: from WSPRnet and wspr.live with GLOBE's ASK WSPRNET + WSPR.LIVE on, \
         and from PSK Reporter with its PSK REPORTER on. The sites take a few minutes.",
    );
    if clear {
        tx.wspr_rows.clear();
        tx.wspr_cleared = c.now;
    }
}

/// HEARD's cells, in [`HEARD_COLS`]' order.
fn wspr_heard_cells(r: &WsprRow) -> [String; 10] {
    let h = &r.h;
    let t = crate::decode::hms(h.slot_utc as f64).replace(':', "");
    let (km, brg) = match r.path {
        Some((b, km)) => (format!("{km:.0}"), format!("{b:.0}°")),
        None => ("–".into(), "–".into()),
    };
    [
        t.get(..4).unwrap_or(&t).to_string(),
        format!("{:+.0}", h.snr_db),
        format!("{:+.0}", h.drift_hz),
        if h.rf_hz > 0.0 { super::mhz_cell(h.rf_hz) } else { "–".into() },
        h.call.clone(),
        if h.grid.is_empty() { "–".into() } else { h.grid.clone() },
        h.dbm.to_string(),
        km,
        brg,
        format!("{:.0}", h.audio_hz),
    ]
}

/// HEARD ME's cells, in [`ME_COLS`]' order.
fn heard_me_cells(m: &HeardMe) -> [String; 6] {
    let t = crate::decode::hms(m.utc).replace(':', "");
    [
        t.get(..4).unwrap_or(&t).to_string(),
        m.call.clone(),
        m.snr.map_or("–".into(), |s| format!("{s:+.0}")),
        m.km.map_or("–".into(), |k| format!("{k:.0}")),
        if m.rf_hz > 0.0 { super::mhz_cell(m.rf_hz) } else { "–".into() },
        m.site.to_string(),
    ]
}

enum Act {
    Pick { shift: bool },
    Reply,
    Queue,
}

fn row_ui(ui: &mut egui::Ui, r: &ListRow, picked: bool, dx: Option<&str>, me: &str) -> Option<Act> {
    let mut act = None;
    let h = 20.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let p = ui.painter_at(rect);
    // Calling you: the accent band (not red: red is for warnings). A CQ
    // open to you: green. Dupes fade.
    let band = if r.ours {
        Some(tx_ink().gamma_multiply(0.22))
    } else if r.to_me {
        Some(theme::CYAN().gamma_multiply(0.30))
    } else if r.cq_ok {
        Some(theme::GREEN().gamma_multiply(0.20))
    } else if resp.hovered() {
        Some(theme::ROW_HOVER())
    } else {
        None
    };
    if let Some(b) = band {
        p.rect_filled(rect, 0.0, b);
    }
    let working = dx.is_some_and(|d| r.d.from.as_deref() == Some(d) || r.d.to.as_deref() == Some(d) && r.d.from.as_deref() == Some(me));
    if working || picked {
        p.rect_stroke(rect.shrink(0.5), 0.0, egui::Stroke::new(1.0, theme::TEXT_STRONG()), egui::StrokeKind::Inside);
    }
    let dim = r.badges.dupe && !r.to_me;
    let ink = if dim { theme::TEXT().gamma_multiply(0.45) } else if r.to_me { theme::TEXT_STRONG() } else { theme::TEXT() };
    let font = egui::FontId::monospace(12.0);
    let y = rect.center().y;
    let x = std::cell::Cell::new(rect.left() + 4.0);
    let put = |t: String, col: Color32, wpx: f32| {
        p.text(egui::pos2(x.get(), y), egui::Align2::LEFT_CENTER, t, font.clone(), col);
        x.set(x.get() + wpx);
    };
    put(if r.to_me { "▶".into() } else { " ".into() }, theme::CYAN(), 14.0);
    let snr_col = match r.d.snr_db {
        s if s >= 0 => theme::GREEN(),
        s if s >= -15 => ink,
        _ => ink.gamma_multiply(0.7),
    };
    if r.ours {
        put("TX".into(), tx_ink(), 34.0);
        put(String::new(), ink, 34.0);
    } else {
        put(format!("{:+03}", r.d.snr_db), snr_col, 34.0);
        put(format!("{:4.1}", r.d.dt), ink, 34.0);
    }
    put(format!("{:4.0}", r.d.audio_hz), ink, 42.0);
    let msg_w = (w * 0.40).clamp(150.0, 260.0);
    if r.to_me {
        // Bold, as the monospace face has no bold: drawn twice, a hair apart.
        p.text(egui::pos2(x.get() + 0.7, y), egui::Align2::LEFT_CENTER, &r.d.message, font.clone(), ink);
    }
    put(r.d.message.clone(), if r.ours { tx_ink() } else { ink }, msg_w);
    let wide = w > 520.0;
    if let Some(cty) = &r.country {
        let name: String = if wide { cty.chars().take(16).collect() } else { cty.chars().take(6).collect() };
        put(name, ink, if wide { 120.0 } else { 52.0 });
    } else {
        put(String::new(), ink, if wide { 120.0 } else { 52.0 });
    }
    if let Some(cont) = r.continent {
        put(cont.into(), theme::continent_color(cont), 24.0);
    } else {
        x.set(x.get() + 24.0);
    }
    if let Some((brg, km, guess)) = r.path {
        let col = if guess { ink.gamma_multiply(0.6) } else { ink };
        put(format!("{brg:3.0}° {km:5.0} km"), col, 106.0);
    }
    // Badges.
    let b = r.badges;
    let tags: Vec<&str> = [
        (b.new_dxcc, "DXCC"),
        (b.new_band && !b.new_dxcc, "BAND"),
        (b.new_grid, "GRID"),
        (b.new_call && !b.new_dxcc, "NEW"),
        (b.dupe, "DUPE"),
    ]
    .into_iter()
    .filter(|t| t.0)
    .map(|t| t.1)
    .collect();
    let right = rect.right() - 76.0;
    let mut bx = x.get().min(right - 4.0);
    for t in tags.iter().take(2) {
        let g = p.layout_no_wrap(t.to_string(), egui::FontId::monospace(10.0), theme::INK_ON_CYAN());
        let br = egui::Rect::from_min_size(egui::pos2(bx, y - 7.0), g.size() + vec2(6.0, 2.0));
        if br.right() > right {
            break;
        }
        let fill = if *t == "DUPE" { theme::LINE() } else { theme::CYAN_DIM() };
        p.rect_filled(br, 2.0, fill);
        p.galley(br.min + vec2(3.0, 1.0), g, theme::INK_ON_CYAN());
        bx = br.right() + 4.0;
    }
    // REPLY and +.
    if !r.ours && r.d.from.is_some() {
        let rr = egui::Rect::from_min_size(egui::pos2(rect.right() - 74.0, rect.top() + 1.0), vec2(48.0, h - 2.0));
        let pr = egui::Rect::from_min_size(egui::pos2(rect.right() - 22.0, rect.top() + 1.0), vec2(20.0, h - 2.0));
        if ui.put(rr, egui::Button::new(RichText::new("REPLY").size(10.5)).small()).clicked() {
            act = Some(Act::Reply);
        }
        if ui.put(pr, egui::Button::new(RichText::new("+").size(11.0)).small()).on_hover_text("Queue this station").clicked() {
            act = Some(Act::Queue);
        }
    }
    let resp = resp.on_hover_ui(|ui| hover(ui, r));
    if act.is_none() {
        if resp.double_clicked() && !r.ours && r.d.from.is_some() {
            act = Some(Act::Reply);
        } else if resp.clicked() {
            act = Some(Act::Pick { shift: ui.input(|i| i.modifiers.shift) });
        }
    }
    act
}

fn hover(ui: &mut egui::Ui, r: &ListRow) {
    ui.label(mono(r.d.message.clone()).strong());
    if let Some(c) = &r.country {
        ui.label(format!(
            "{c} · {} · CQ zone {} · ITU zone {}",
            r.continent.unwrap_or("?"),
            r.cq_zone.map_or("?".into(), |z| z.to_string()),
            r.itu_zone.map_or("?".into(), |z| z.to_string())
        ));
    }
    if let Some(g) = &r.d.grid {
        ui.label(format!("Grid {g}"));
    }
    if r.rf_hz > 0.0 {
        ui.label(format!("Heard on {:.3} kHz ({:.0} Hz audio)", r.rf_hz / 1e3, r.d.audio_hz));
    }
    if let Some((b, km, guess)) = r.path {
        let lp = (b + 180.0) % 360.0;
        ui.label(format!(
            "{km:.0} km · short path {b:.0}° · long path {lp:.0}°{}",
            if guess { " (from the country's centre)" } else { "" }
        ));
    }
    if r.d.is_cq {
        let to = r.d.cq_to.as_deref().unwrap_or("anyone");
        let open = if r.cq_ok { "open to you" } else { "not for you" };
        ui.label(format!("CQ to {to}: {open}"));
    }
    let b = r.badges;
    if b.dupe {
        ui.label("Already worked on this band and mode");
    }
}

/// The FT8 QSO panel. `spare` is the room the column has beyond what the
/// panel needs, for the transcript; returns the transcript's height.
fn qso_panel(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>, spare: f32) -> f32 {
    // The station in hand.
    ui.horizontal(|ui| {
        ui.label(RichText::new("QSO").strong().color(theme::TEXT_STRONG()));
        ui.label(RichText::new(tx.qso.step().label()).size(11.0).color(tx_ink()));
    });
    match tx.qso.dx_call() {
        Some(call) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(call).monospace().size(20.0).color(theme::TEXT_STRONG()));
                if let Some(g) = tx.qso.dx_grid() {
                    ui.label(mono(g));
                }
                // Where we copy them: the offset alone (AJ3G, 11 Nov 2026).
                if let Some((_, hz, _)) = tx.dx_mark() {
                    ui.label(mono(format!("{hz:.0} Hz")))
                        .on_hover_text("The audio offset you copy them at, from their latest decode");
                }
            });
            let (rcvd, sent) = tx.qso.reports();
            let f = |v: Option<i16>| v.map_or("–".into(), crate::tx::msg::fmt_report);
            let wd = tx.qso.watchdog_left(c.now as i64).map_or(String::new(), |t| format!(" · watchdog {}:{:02}", t / 60, t % 60));
            ui.label(mono(format!("rcvd {} · sent {} · calls {} of {}{wd}", f(rcvd), f(sent), tx.qso.calls(), s.max_calls)));
        }
        None => {
            ui.label(RichText::new("No station picked: REPLY on a decode, or CALL CQ.").size(11.5).color(theme::TEXT()));
        }
    }
    if tx.qso.watchdog() {
        ui.label(RichText::new("The TX watchdog stopped the run. Pick a line or REPLY to go on.").size(11.5).color(theme::YELLOW()));
    }
    if let Some(l) = tx.to_log.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Log {} ?", l.call)).color(theme::TEXT_STRONG()));
            if chrome::chip(ui, false, "LOG IT").clicked() {
                tx.log_this(c.now);
            }
            if chrome::chip(ui, false, "DISCARD").clicked() {
                tx.to_log = None;
            }
        });
    }
    ui.add_space(4.0);
    caption(ui, "TRANSCRIPT");
    // The whole QSO, kept at its newest line; as tall as dragged (16 lines
    // to start), or as the window's room allows (AJ3G, 11 Nov 2026).
    let row_h = ui.fonts_mut(|f| f.row_height(&egui::FontId::monospace(12.0))) + ui.spacing().item_spacing.y;
    let min_h = 3.0 * row_h;
    let shown_h = s.transcript_h.max(spare).max(min_h);
    egui::Frame::new().fill(theme::INPUT_BG()).inner_margin(6).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::ScrollArea::vertical()
            .id_salt("tx-transcript")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(shown_h)
            .show(ui, |ui| {
                for l in tx.qso.transcript() {
                    let t = crate::decode::hms(l.utc as f64).replace(':', "");
                    let (tag, col) = if l.note {
                        ("  ", theme::CYAN_DIM())
                    } else if l.tx {
                        ("TX", tx_ink())
                    } else {
                        ("RX", theme::GREEN())
                    };
                    let col = if l.done { theme::TEXT_STRONG() } else { col };
                    ui.label(mono(format!("{t} {tag} {}", l.text)).color(col));
                }
            });
        let next = tx.hand.clone().or_else(|| tx.qso.plan_tx());
        ui.label(mono(format!("next: {}", next.unwrap_or_else(|| "nothing".into()))).color(theme::CYAN_DIM()));
    });
    // Its grip: drag to make it taller or shorter.
    let (grip, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), Sense::drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeVertical).on_hover_text("Drag to size the transcript");
    let ink = if resp.hovered() || resp.dragged() { theme::TEXT_STRONG() } else { theme::LINE() };
    ui.painter().hline(grip.center().x - 18.0..=grip.center().x + 18.0, grip.center().y, egui::Stroke::new(2.0, ink));
    if resp.dragged() {
        s.transcript_h = (shown_h + resp.drag_delta().y).clamp(min_h, 4000.0);
    }
    ui.add_space(4.0);
    // Tx1 to Tx6, with Next and Now.
    ui.horizontal(|ui| {
        caption(ui, "MESSAGES");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| caption(ui, "NEXT · NOW"));
    });
    let next_line = tx.hand.clone().or_else(|| tx.qso.plan_tx());
    let mut pick: Option<(usize, bool)> = None;
    for i in 0..6 {
        ui.horizontal(|ui| {
            ui.label(mono(format!("TX{}", i + 1)).color(theme::CYAN_DIM()));
            let w = (ui.available_width() - 150.0).max(120.0);
            if i == 4 {
                let resp = ui.add_sized([w, 18.0], egui::TextEdit::singleline(&mut tx.lines[4]).char_limit(13).font(egui::TextStyle::Monospace));
                if resp.double_clicked() {
                    pick = Some((4, true));
                }
                egui::ComboBox::from_id_salt("tx5-macros").width(22.0).selected_text("").show_ui(ui, |ui| {
                    for m in &s.macros {
                        if ui.selectable_label(false, m).clicked() {
                            tx.lines[4] = m.clone();
                        }
                    }
                });
            } else {
                let resp = ui.add_sized([w, 18.0], egui::TextEdit::singleline(&mut tx.lines[i]).font(egui::TextStyle::Monospace));
                if resp.double_clicked() {
                    pick = Some((i, true));
                }
                if i == 3 && chrome::chip(ui, s.rrr, "RRR").on_hover_text("Tx4 sends RRR rather than RR73").clicked() {
                    s.rrr = !s.rrr;
                    tx.generate(s);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(format!("Tx{}", i + 1)).on_hover_text("Now: this slot, if there is still time").clicked() {
                    pick = Some((i, true));
                }
                let is_next = next_line.as_deref().is_some_and(|n| n == tx.lines[i].trim().to_ascii_uppercase());
                if ui.radio(is_next, "").on_hover_text("Next: in your next slot").clicked() {
                    pick = Some((i, false));
                }
            });
        });
    }
    if let Some((i, now)) = pick {
        if i == 5 && tx.qso.dx_call().is_none() {
            tx.call_cq(s);
        }
        tx.pick_line(s, i, now, c.now);
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if chrome::chip(ui, tx.qso.step() == crate::tx::qso::Step::CallingCq, "CALL CQ")
            .on_hover_text("Call CQ from your next slot: turns ENABLE TX on")
            .clicked()
        {
            tx.call_cq(s);
            tx.enable(s);
        }
        if chrome::chip(ui, false, "Generate Std Msgs").clicked() {
            tx.generate(s);
        }
        for (v, l, tip) in [
            (&mut s.skip_tx1, "Skip Tx1", "Answer a CQ with the report, not the grid"),
            (&mut s.auto_seq, "Auto Seq", "Send the right reply to what you hear, through to the log"),
            (&mut s.call_first, "Call 1st", "Answer the first (strongest new) caller to your CQ by itself"),
            (
                &mut s.call_after_rr73,
                "Call after RR73",
                "Waiting for a station working someone else: call them as soon as they send that station \
                 RR73, RRR or 73, rather than wait for their next CQ",
            ),
            (&mut s.tx_even, "TX even/1st", "Call CQ in the even slots (:00, :30)"),
        ] {
            if chrome::chip(ui, *v, l).on_hover_text(tip).clicked() {
                *v = !*v;
            }
        }
        if chrome::chip(ui, false, "LOG THIS").on_hover_text("Log the contact in hand now").clicked() {
            tx.log_this(c.now);
        }
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        caption(ui, "REPORT");
        ui.add(egui::DragValue::new(&mut tx.report).range(-30..=49).speed(0.2));
        caption(ui, "TX AUDIO");
        let keyed = tx.over().is_some();
        // SAFETY INTERLOCK (#9): the bar, and so this, doesn't move while
        // transmitting.
        ui.add_enabled(!keyed, egui::DragValue::new(&mut s.audio_hz).range(200.0..=2900.0).speed(1.0).suffix(" Hz"));
        let next = match s.freq {
            FreqMode::Auto => FreqMode::Manual,
            FreqMode::Manual => FreqMode::Hold,
            FreqMode::Hold => FreqMode::Auto,
        };
        if chrome::chip(ui, s.freq != FreqMode::Manual, s.freq.label())
            .on_hover_text(
                "AUTO picks the clearest gap in your own period before each over; MANUAL follows the bar and \
                 a decode clicked; HOLD moves only by a drag or a typed value. Dragging the bar on AUTO \
                 switches to MANUAL.",
            )
            .clicked()
        {
            s.freq = next;
        }
        caption(ui, "LEVEL");
        chrome::slider_readout(
            ui,
            40.0,
            egui::Slider::new(&mut s.ft8_level, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        drive(ui, c, s.ft8_level);
    });
    // The queue.
    ui.horizontal_wrapped(|ui| {
        caption(ui, "QUEUE");
        let mut drop = None;
        for q in tx.qso.queue() {
            if chrome::chip(ui, false, &q.call).on_hover_text("Click to drop it").clicked() {
                drop = Some(q.call.clone());
            }
        }
        if let Some(d) = drop {
            tx.qso.queue_remove(&d);
        }
        if tx.qso.queue().next().is_some() && chrome::chip(ui, false, "CLEAR").clicked() {
            let calls: Vec<String> = tx.qso.queue().map(|q| q.call.clone()).collect();
            for c in calls {
                tx.qso.queue_remove(&c);
            }
        }
    });
    let also: Vec<String> = tx.qso.also_calling().to_vec();
    if !also.is_empty() {
        ui.horizontal_wrapped(|ui| {
            caption(ui, "ALSO CALLING");
            for a in also {
                if chrome::chip(ui, false, &a).on_hover_text("Queue this station").clicked()
                    && let Some(r) = tx.rows.iter().find(|r| r.d.from.as_deref() == Some(a.as_str())).cloned()
                {
                    tx.queue(&r);
                }
            }
        });
    }
    shown_h
}

/// The drive a LEVEL gives, beside its slider: "≈ 2.5 W", or "— W" before
/// the radio's power step is read (AJ3G, 11 Nov 2026).
fn drive(ui: &mut egui::Ui, c: &Ctx<'_>, level: f32) {
    let w = c.rig.and_then(|r| crate::tune::planned_watts(r, rigcat::Keying::Data, level));
    // Never wrapped: at the window's narrowest "≈" and "4.2 W" fell on two
    // lines.
    ui.add(egui::Label::new(mono(drive_text(w).replace(' ', "\u{a0}")).color(theme::TEXT())).wrap_mode(egui::TextWrapMode::Extend))
        .on_hover_text("The radio's power setting times this level: what the radio puts out into the amplifier");
}

fn drive_text(w: Option<f32>) -> String {
    w.map_or("— W".into(), |w| format!("≈ {w:.1} W"))
}

fn wspr_panel(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>) {
    ui.label(RichText::new("WSPR BEACON").strong().color(theme::TEXT_STRONG()));
    let call = c.station.call().unwrap_or_default();
    let grid: String = c.station.grid().unwrap_or_default().chars().take(4).collect();
    let ok = wave::Message::wspr(&call, &grid, s.wspr_dbm, 1500.0).is_some();
    ui.horizontal(|ui| {
        ui.label(mono(format!("{call} {grid} {}", s.wspr_dbm)).color(if ok { theme::TEXT_STRONG() } else { theme::YELLOW() }));
        // The offset the beacon goes (or last went) out on.
        if let Some(hz) = tx.wspr_offset(s) {
            ui.label(mono(format!("{hz:.0} Hz")))
                .on_hover_text("The audio offset of the beacon on the air, or of the last one with ROAM on");
        }
    });
    // Where it lands (decision 89): the transmit dial, the offset, and any
    // transverter.
    let on_air = tx.over().filter(|o| o.1 == TxMode::Wspr).map(|o| o.2);
    let roam_unknown = on_air.is_none() && s.wspr_roam && tx.wspr_offset(s).is_none();
    let audio = on_air.unwrap_or_else(|| tx.wspr_bar_audio(s));
    match c.rig.and_then(|r| super::tx_rf_hz(r, audio as f64, c.xvtr_hz)) {
        Some(rf) => {
            let (pre, post) = if roam_unknown {
                ("TX ≈ ", " · ROAM picks each beacon's offset")
            } else {
                ("TX ", "")
            };
            ui.label(mono(format!("{pre}{}{post}", super::rf_text(rf))).color(tx_ink())).on_hover_text(
                "Your beacon's RF: the transmit dial (VFO B in split), plus the audio offset (minus on LSB), \
                 plus the transverter's offset",
            );
        }
        None => {
            ui.label(
                RichText::new("TX RF: the radio's dial hasn't been read.")
                    .size(11.0)
                    .color(theme::TEXT()),
            );
        }
    }
    if !ok {
        ui.label(
            RichText::new("Type 1 only: a plain callsign and a 4-character grid. A compound call can't be sent.")
                .size(11.5)
                .color(theme::YELLOW()),
        );
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        caption(ui, "TRANSMIT");
        for (p, l) in [(0u8, "OFF"), (10, "10 %"), (20, "20 %"), (33, "33 %"), (50, "50 %")] {
            let tip = if p == 0 {
                "No beacons: turns ENABLE TX off"
            } else {
                "This share of the 2 minute slots"
            };
            if chrome::chip(ui, s.wspr_pct == p, l).on_hover_text(tip).clicked() {
                tx.set_duty(s, p);
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        caption(ui, "POWER");
        egui::ComboBox::from_id_salt("wspr-dbm")
            .selected_text(format!("{} dBm · {}", s.wspr_dbm, wave::dbm_label(s.wspr_dbm)))
            .show_ui(ui, |ui| {
                for d in wave::WSPR_DBM {
                    ui.selectable_value(&mut s.wspr_dbm, d, format!("{d} dBm · {}", wave::dbm_label(d)));
                }
            })
            .response
            .on_hover_text("What goes on the air in the message: it must be what you really radiate.");
    });
    ui.horizontal_wrapped(|ui| {
        if chrome::chip(ui, s.wspr_roam, "ROAM").on_hover_text("A new offset in 1400 to 1600 Hz each beacon").clicked() {
            s.wspr_roam = !s.wspr_roam;
        }
        if !s.wspr_roam {
            caption(ui, "AUDIO");
            ui.add(egui::DragValue::new(&mut s.wspr_audio_hz).range(1410.0..=1590.0).suffix(" Hz"));
        }
        caption(ui, "LEVEL");
        chrome::slider_readout(
            ui,
            40.0,
            egui::Slider::new(&mut s.wspr_level, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        drive(ui, c, s.wspr_level);
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Each beacon keys for 110.6 s with the amplifier in line. Every TUNE check holds for the whole of \
             it, and none starts (or one going out stops) while the amplifier is over 155 °F (68 °C), \
             a step below its own 158 °F trip.",
        )
        .size(11.0)
        .color(theme::TEXT()),
    );
    if let Some((text, _, hz, end)) = tx.over() {
        ui.label(mono(format!("On the air: {text} at {hz:.0} Hz, {:.0} s left", (end - c.now).max(0.0))).color(tx_ink()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::msg;

    pub(crate) fn row(text: &str, slot: i64, ours: bool) -> ListRow {
        ListRow {
            d: msg::parse(text, slot, -10, 0.1, 1500.0),
            row: 1,
            rf_hz: 14_075_500.0,
            country: None,
            continent: None,
            cq_zone: None,
            itu_zone: None,
            path: None,
            badges: Default::default(),
            to_me: false,
            cq_ok: false,
            ours,
        }
    }

    #[test]
    fn a_slot_whose_decodes_come_late_keeps_one_heading() {
        // Newest first, as the list holds them: slot 30's decodes, then
        // our over in slot 45, then slot 30's late ones, then slot 15's.
        let rows: std::collections::VecDeque<ListRow> = [
            row("CQ K1ABC FN42", 30, false),
            row("UN7GBX AJ3G -18", 45, true),
            row("CQ W9XYZ EN52", 30, false),
            row("CQ JA1ABC PM95", 15, false),
        ]
        .into_iter()
        .collect();
        let g = slot_groups(&rows);
        assert_eq!(g.iter().map(|t| t.0).collect::<Vec<_>>(), vec![45, 30, 15]);
        assert_eq!(g[1].1, vec![0, 2]);
    }

    #[test]
    fn heard_me_keeps_one_report_per_receiver_per_cycle_newest_first() {
        let rec = |call: &str, utc: f64, mode: &str| Reception {
            call: call.into(),
            grid: "IO91".into(),
            pos: station::grid_to_latlon("IO91").unwrap(),
            rf_hz: 14_097_071.0,
            mode: mode.into(),
            snr: Some(-22.0),
            utc,
            src: crate::globe::field::Source::PskReporter,
            row: None,
            from: None,
        };
        let w = |call: &str, utc: f64| WsprReport {
            rec: rec(call, utc, "WSPR"),
            drift_hz: Some(0.0),
            power_dbm: Some(37.0),
            km: Some(5900.0),
            site: "wspr.live",
        };
        let t = 1_800_000_000.0;
        let wspr = [w("G4XYZ", t), w("DL1ABC", t + 120.0)];
        let pskr = [rec("G4XYZ", t + 30.0, "WSPR"), rec("K1ABC", t + 30.0, "FT8"), rec("VK2XX", t + 1.0, "WSPR")];
        let v = heard_me(&pskr, &wspr, None, 0.0);
        let calls: Vec<_> = v.iter().map(|m| (m.call.as_str(), m.site)).collect();
        assert_eq!(calls, vec![("DL1ABC", "wspr.live"), ("VK2XX", "PSK Reporter"), ("G4XYZ", "wspr.live")]);
        // CLEAR hides what came before it.
        assert_eq!(heard_me(&pskr, &wspr, None, t + 60.0).len(), 1);
    }

    fn wspr_row(
        call: &str,
        slot: i64,
        snr: f32,
        km: Option<f64>,
        rf_hz: f64,
    ) -> crate::tx::WsprRow {
        crate::tx::WsprRow {
            h: crate::decode::WsprHeard {
                slot_utc: slot,
                snr_db: snr,
                drift_hz: 1.0,
                audio_hz: 1503.0,
                call: call.into(),
                grid: "FN42".into(),
                dbm: 37,
                rf_hz,
            },
            path: km.map(|k| (45.0, k)),
        }
    }

    #[test]
    fn a_wspr_row_fills_its_columns() {
        let r = wspr_row(
            "K1ABC",
            1_800_000_000 - 1_800_000_000 % 120,
            -18.0,
            Some(1234.0),
            10_140_123.0,
        );
        let c = wspr_heard_cells(&r);
        assert_eq!(c.len(), HEARD_COLS.len());
        assert_eq!(
            &c[1..],
            &["-18", "+1", "10.140 123", "K1ABC", "FN42", "37", "1234", "45°", "1503"]
        );
        // Every figure fits its column.
        for (cell, col) in c.iter().zip(&HEARD_COLS) {
            assert!(cell.chars().count() <= col.chars, "{cell} in {}", col.head);
        }
    }

    #[test]
    fn the_wspr_lists_sort_by_a_heading_and_show_one_band() {
        let t = 1_800_000_000 - 1_800_000_000 % 120;
        let rows = [
            wspr_row("K1ABC", t + 120, -18.0, Some(1200.0), 10_140_150.0),
            wspr_row("G4XYZ", t + 120, -5.0, Some(5900.0), 10_140_180.0),
            wspr_row("VK2XX", t, -25.0, None, 10_140_110.0),
            wspr_row("DL1ABC", t, -10.0, Some(6500.0), 14_097_100.0),
        ];
        let calls =
            |v: Vec<&crate::tx::WsprRow>| v.iter().map(|r| r.h.call.clone()).collect::<Vec<_>>();
        let mut sort = WsprSort::default();
        // Newest first, as heard, on 30 m only: DL1ABC is on 20 m.
        assert_eq!(calls(shown_rows(rows.iter(), Some("30m"), sort)), ["K1ABC", "G4XYZ", "VK2XX"]);
        // Every band when the band isn't known.
        assert_eq!(shown_rows(rows.iter(), None, sort).len(), 4);
        sort.click(WsprKey::Km);
        assert_eq!(sort, WsprSort { key: WsprKey::Km, desc: true });
        // Farthest first; no distance last.
        assert_eq!(calls(shown_rows(rows.iter(), Some("30m"), sort)), ["G4XYZ", "K1ABC", "VK2XX"]);
        sort.click(WsprKey::Km);
        // Nearest first; no distance still last.
        assert_eq!(calls(shown_rows(rows.iter(), Some("30m"), sort)), ["K1ABC", "G4XYZ", "VK2XX"]);
        sort.click(WsprKey::Snr);
        assert_eq!(
            calls(shown_rows(rows.iter(), None, sort)),
            ["G4XYZ", "DL1ABC", "K1ABC", "VK2XX"]
        );
        sort.click(WsprKey::Call);
        assert!(!sort.desc);
        assert_eq!(
            calls(shown_rows(rows.iter(), None, sort)),
            ["DL1ABC", "G4XYZ", "K1ABC", "VK2XX"]
        );
        sort.click(WsprKey::Utc);
        sort.click(WsprKey::Utc);
        // Oldest first.
        assert_eq!(calls(shown_rows(rows.iter(), Some("30m"), sort))[0], "VK2XX");
    }

    #[test]
    fn a_heard_me_row_fills_its_columns() {
        let m = HeardMe {
            utc: 1_800_000_000.0,
            call: "G4XYZ".into(),
            snr: Some(-22.0),
            km: None,
            rf_hz: 144_490_500.0,
            site: "wspr.live",
        };
        let c = heard_me_cells(&m);
        assert_eq!(&c[1..], &["G4XYZ", "-22", "–", "144.490 500", "wspr.live"]);
        for (cell, col) in c.iter().zip(&ME_COLS) {
            assert!(cell.chars().count() <= col.chars, "{cell} in {}", col.head);
        }
    }

    #[test]
    fn the_drive_reads_in_watts_or_a_dash() {
        assert_eq!(drive_text(Some(2.5)), "≈ 2.5 W");
        assert_eq!(drive_text(None), "— W");
    }
}
