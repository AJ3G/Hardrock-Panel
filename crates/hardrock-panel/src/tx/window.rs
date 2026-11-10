//! The TX window: mode, ENABLE / HALT / STOP QSO, the slot bar, the decode
//! list for working stations, the QSO with its transcript and WSJT-X's six
//! message lines, the queue, and WSPR's beacon settings. Laid out after
//! notes/tx-mockup.png.

use eframe::egui::{self, Color32, RichText, Sense, vec2};
use rigcat::RigLink;

use super::{FreqMode, ListRow, Sort, Tx, TxMode, TxSettings, sched, wave};
use crate::station::StationSettings;
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
            tx.enabled = !tx.enabled;
            tx.note = None;
            if tx.enabled && s.mode == TxMode::Ft8 {
                tx.qso.reset_watchdog();
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
    let foot = 4.0;
    let h = (ui.available_height() - foot).max(200.0);
    let w = ui.available_width();
    let left_w = (w * 0.56).max(320.0).min(w - 300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui(vec2(left_w, h), |ui| {
            ui.set_min_size(vec2(left_w, h));
            ui.vertical(|ui| decode_list(ui, tx, s, c, out));
        });
        ui.separator();
        ui.allocate_ui(vec2(ui.available_width(), h), |ui| {
            ui.vertical(|ui| {
                egui::ScrollArea::vertical().id_salt("tx-right").auto_shrink([false, false]).show(
                    ui,
                    |ui| match s.mode {
                        TxMode::Ft8 => qso_panel(ui, tx, s, c),
                        TxMode::Wspr => wspr_panel(ui, tx, s, c),
                    },
                );
            });
        });
    });
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
    if s.mode == TxMode::Wspr {
        ui.label(
            RichText::new("WSPR's decodes are in DECODE and on GLOBE; who heard you is GLOBE's WSPR HEARD ME.")
                .size(11.5)
                .color(theme::TEXT()),
        );
        return;
    }
    // Turns, newest first; within each, calling-you rows pinned to the top.
    let mut turns: Vec<(i64, Vec<usize>)> = Vec::new();
    for (i, r) in tx.rows.iter().enumerate() {
        match turns.last_mut() {
            Some((t, v)) if *t == r.d.slot_utc => v.push(i),
            _ => turns.push((r.d.slot_utc, vec![i])),
        }
    }
    let me_call = c.station.call().unwrap_or_default();
    let dx = tx.qso.dx_call().map(str::to_string);
    let mut act: Option<(usize, Act)> = None;
    egui::ScrollArea::vertical().id_salt("tx-decodes").auto_shrink([false, false]).show(ui, |ui| {
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
            idx.sort_by_key(|&i| !rows[i].to_me);
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
        RichText::new("Double-click = REPLY · Shift+click sets TX audio only · + queues")
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
            tx.picked = Some(r.row);
            if s.freq == FreqMode::Manual {
                s.audio_hz = r.d.audio_hz.round();
            }
        }
        Act::Queue => tx.queue(&r),
    }
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

fn qso_panel(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>) {
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
    egui::Frame::new().fill(theme::INPUT_BG()).inner_margin(6).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        let lines = tx.qso.transcript();
        for l in lines.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
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
        let next = tx.hand.clone().or_else(|| tx.qso.plan_tx());
        ui.label(mono(format!("next: {}", next.unwrap_or_else(|| "nothing".into()))).color(theme::CYAN_DIM()));
    });
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
        if chrome::chip(ui, tx.qso.step() == crate::tx::qso::Step::CallingCq, "CALL CQ").clicked() {
            tx.call_cq(s);
        }
        if chrome::chip(ui, false, "Generate Std Msgs").clicked() {
            tx.generate(s);
        }
        for (v, l, tip) in [
            (&mut s.skip_tx1, "Skip Tx1", "Answer a CQ with the report, not the grid"),
            (&mut s.auto_seq, "Auto Seq", "Send the right reply to what you hear, through to the log"),
            (&mut s.call_first, "Call 1st", "Answer the first (strongest new) caller to your CQ by itself"),
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
}

fn wspr_panel(ui: &mut egui::Ui, tx: &mut Tx, s: &mut TxSettings, c: &Ctx<'_>) {
    ui.label(RichText::new("WSPR BEACON").strong().color(theme::TEXT_STRONG()));
    let call = c.station.call().unwrap_or_default();
    let grid: String = c.station.grid().unwrap_or_default().chars().take(4).collect();
    let ok = wave::Message::wspr(&call, &grid, s.wspr_dbm, 1500.0).is_some();
    ui.label(mono(format!("{call} {grid} {}", s.wspr_dbm)).color(if ok { theme::TEXT_STRONG() } else { theme::YELLOW() }));
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
            if chrome::chip(ui, s.wspr_pct == p, l).clicked() {
                s.wspr_pct = p;
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
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Each beacon keys for 110.6 s with the amplifier in line. Every TUNE check holds for the whole of \
             it, and none starts (or one going out stops) while the amplifier's temperature is in its \
             warning range.",
        )
        .size(11.0)
        .color(theme::TEXT()),
    );
    if let Some((text, _, hz, end)) = tx.over() {
        ui.label(mono(format!("On the air: {text} at {hz:.0} Hz, {:.0} s left", (end - c.now).max(0.0))).color(tx_ink()));
    }
}
