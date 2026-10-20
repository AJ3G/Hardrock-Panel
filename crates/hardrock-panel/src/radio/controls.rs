//! The keys under the radio's face and what they open: MODE with the data
//! modes, BAND, F INPUT, the RX, CW, TX audio and MEM pop-outs, the VFO and
//! RIT keys, the readouts, and the MENU window.
//!
//! Every setting is drawn from the radio's [`Control`] table by its
//! [`Kind`]: a toggle is an ON/OFF key, a choice a list, a range a slider
//! with its unit, text a field, and a read-only setting its value. Each
//! shows what the radio last answered, never what was asked for, and greys
//! while [`logic::locked`] says so.

use eframe::egui::{self, Color32, ComboBox, CornerRadius, RichText, Stroke, Ui};
use rigcat::{Control, Group, Kind, Reply, Vfo};

use super::face::{cap, caption, data_font, label_font};
use super::logic::{self, RadioPrefs};
use super::rigtheme::RigTheme;
use super::{Confirm, Face, MenuFilter, Pop};
use crate::chrome::{self, StyledCombo};

/// The pop-outs' black panel.
fn popout_frame(th: &RigTheme) -> egui::Frame {
    egui::Frame::new()
        .fill(th.screen)
        .stroke(Stroke::new(1.0, th.case_line))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 8))
}

/// MODE, BAND, F INPUT, the four pop-outs and MENU.
pub(crate) fn keys(ui: &mut Ui, f: &mut Face<'_>) {
    let th = f.th;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        let face_locked = f.lock.is_some();
        for (pop, text, hover) in [
            (Pop::Mode, "MODE", "The radio's mode, and the data modes"),
            (
                Pop::Band,
                "BAND",
                "Change band: each returns to the last frequency and mode used there",
            ),
            (Pop::Freq, "F INPUT", "Type a frequency, in MHz with a point or kHz without"),
        ] {
            let r = ui
                .add_enabled_ui(!face_locked, |ui| cap(ui, th, f.st.pop == Some(pop), text))
                .inner;
            let r = match f.lock {
                Some(why) => r.on_disabled_hover_text(why),
                None => r.on_hover_text(hover),
            };
            if r.clicked() {
                f.st.toggle(pop);
            }
        }
        ui.add_space(6.0);
        for (pop, text, hover) in [
            (Pop::Rx, "RX", "The radio's own receiver and audio"),
            (Pop::Cw, "CW", "The keyer, the CW decoder and the ten stored messages"),
            (Pop::Tx, "TX AUDIO", "Microphone gain, compression, transmit bandwidth and output"),
            (Pop::Mem, "MEM", "Memory channels 0 to 199"),
        ] {
            let r = ui
                .add_enabled_ui(f.s.connected, |ui| cap(ui, th, f.st.pop == Some(pop), text))
                .inner
                .on_hover_text(hover);
            if r.clicked() {
                f.st.toggle(pop);
            }
        }
        ui.add_space(6.0);
        let r = cap(ui, th, f.st.menu, "MENU").on_hover_text(
            "Every menu of the radio's with a CAT command, and the panel's own display settings",
        );
        if r.clicked() {
            f.st.menu = !f.st.menu;
        }
        if f.s.transmitting() {
            caption(ui, th, "locked: transmitting");
        }
    });
    // MODE, BAND and F INPUT close when the radio transmits, as their keys
    // grey (interlock #5); the others stay open, greyed, so CW STOP can be
    // reached.
    if f.lock.is_some() && matches!(f.st.pop, Some(Pop::Mode | Pop::Band | Pop::Freq)) {
        f.st.pop = None;
    }
}

/// The open pop-out, if any.
pub(crate) fn popout(ui: &mut Ui, f: &mut Face<'_>, prefs: &mut RadioPrefs) {
    let Some(pop) = f.st.pop else { return };
    ui.add_space(4.0);
    popout_frame(f.th).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        match pop {
            Pop::Mode => mode_pop(ui, f),
            Pop::Band => band_pop(ui, f, prefs),
            Pop::Freq => freq_pop(ui, f, prefs),
            Pop::Rx => group_pop(ui, f, f.th.rx, Group::Receive, rx_note(f)),
            Pop::Cw => {
                group_pop(ui, f, f.th.cw, Group::Cw, None);
                cw_messages(ui, f, prefs);
            }
            Pop::Tx => group_pop(ui, f, f.th.tx, Group::TxAudio, None),
            Pop::Mem => mem_pop(ui, f, prefs),
        }
    });
}

fn mode_pop(ui: &mut Ui, f: &mut Face<'_>) {
    let th = f.th;
    // FM shows only where the radio's own mode cycle offers it (menu 14).
    let fm_off = logic::find(f.p, "FM").is_some() && f.s.value("FM") == Some("0");
    ui.horizontal_wrapped(|ui| {
        for m in f.p.modes() {
            if *m == "FM" && fm_off {
                continue;
            }
            let lit = f.s.mode == Some(*m) && f.st.digi.is_none();
            if cap(ui, th, lit, m).clicked() {
                f.st.digi = None;
                f.st.digi_note = None;
                if let Some(fr) = f.p.set_mode(m) {
                    f.out.frames.push(fr);
                }
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        for d in logic::DIGIS {
            let r = cap(ui, th, f.st.digi == Some(d.name), d.name).on_hover_text(format!(
                "USB on {}'s frequency on this band, and a filter for its {} to {} Hz audio",
                d.name, d.lo, d.hi
            ));
            if r.clicked() {
                if logic::input_needs_change(f.s) {
                    f.st.confirm = Some(Confirm::Digi(d.name));
                } else {
                    digi(f, d.name, false);
                }
            }
        }
    });
    caption(
        ui,
        th,
        "CAT MD · TX TUNE needs USB or LSB · a data mode sets USB, its frequency on this band \
         and its audio passband",
    );
}

/// Go to data mode `name`, setting the radio's transmit input to USB audio
/// too if `usb_input`.
pub(crate) fn digi(f: &mut Face<'_>, name: &str, usb_input: bool) {
    let Some(d) = logic::digi(name) else { return };
    match logic::digi_frames(f.p, f.s, d, f.xvtr, usb_input) {
        Ok((v, note)) => {
            if let Some(fr) = v.first()
                && let Ok(t) = std::str::from_utf8(&fr.bytes)
                && let Some(hz) = t.get(2..13).and_then(|n| n.parse().ok())
            {
                f.st.pending = Some((hz, std::time::Instant::now()));
            }
            f.out.frames.extend(v);
            f.st.digi = Some(d.name);
            f.st.digi_note = note;
        }
        Err(e) => f.st.note = Some(e),
    }
}

fn band_pop(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs) {
    let th = f.th;
    let xvtr = prefs.xvtr_hz;
    let here = logic::dial_hz(f.s).and_then(|h| logic::band_of(logic::shown(h, xvtr), xvtr));
    ui.horizontal_wrapped(|ui| {
        for b in logic::bands_for(xvtr) {
            if cap(ui, th, here == Some(b), &b.label()).clicked() {
                let digi = f.st.digi.and_then(logic::digi).filter(|d| d.freq_on(b.name).is_some());
                if digi.is_none() {
                    f.st.digi = None;
                }
                let (rf, mode) = prefs.band_target(b, digi);
                match logic::if_of(rf, xvtr) {
                    Some(hz) => super::face::tune_to(f, hz, Some(&mode)),
                    None => {
                        f.st.note = Some(format!("{} is outside the radio's range.", b.label()))
                    }
                }
            }
        }
    });
    caption(
        ui,
        th,
        if xvtr == 0 {
            "Returns to the last frequency and mode used on that band · the HR500 follows"
        } else {
            "Through the transverter: the bands its offset brings inside the radio's 9 kHz to 54 MHz"
        },
    );
}

fn freq_pop(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs) {
    let th = f.th;
    let mut enter = false;
    ui.horizontal_wrapped(|ui| {
        caption(ui, th, "Frequency");
        let r = ui.add(
            egui::TextEdit::singleline(&mut f.st.entry)
                .font(data_font(15.0))
                .desired_width(110.0)
                .hint_text(if prefs.xvtr_hz == 0 { "14.074 or 7074" } else { "144.174" }),
        );
        if f.st.focus_entry {
            r.request_focus();
            f.st.focus_entry = false;
        }
        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        enter |= cap(ui, th, false, "ENTER").clicked();
        if cap(ui, th, false, "CANCEL").clicked() {
            f.st.pop = None;
        }
        let msg = f.st.entry_msg.clone().unwrap_or_else(|| {
            if prefs.xvtr_hz == 0 {
                "MHz with a point, or kHz without".into()
            } else {
                "The RF frequency: the radio is sent it less the transverter offset".into()
            }
        });
        caption(ui, th, &msg);
    });
    if enter {
        if f.lock.is_some() {
            f.st.entry_msg = Some("Not while transmitting".into());
            return;
        }
        match logic::entry_to_if(&f.st.entry, prefs.xvtr_hz) {
            Ok(hz) => {
                super::face::tune_to(f, hz, None);
                f.st.pop = None;
                f.st.entry.clear();
                f.st.entry_msg = None;
            }
            Err(e) => {
                f.st.entry_msg = Some(e);
                f.st.focus_entry = true;
            }
        }
    }
}

fn rx_note(f: &Face<'_>) -> Option<&'static str> {
    f.p.controls().iter().any(|c| c.cmd == "VM").then_some(
        "These act on the radio's own audio, which is live at 192 kHz span. Wider, the radio mutes \
         it and the panel's receiver takes over.",
    )
}

/// The controls `cmds` (or the whole `group`), one a row.
fn group_pop(
    ui: &mut Ui,
    f: &mut Face<'_>,
    cmds: Option<&'static [&'static str]>,
    group: Group,
    note: Option<&str>,
) {
    let list: Vec<&'static Control> = match cmds {
        Some(c) => c.iter().filter_map(|c| logic::find(f.p, c)).collect(),
        None => f.p.controls().iter().filter(|c| c.group == group).collect(),
    };
    egui::Grid::new(("radio-pop", group as u8)).num_columns(2).spacing([12.0, 5.0]).show(
        ui,
        |ui| {
            for c in list {
                ui.label(RichText::new(c.name).font(label_font(12.0)).color(f.th.label))
                    .on_hover_text(hover_of(c));
                control(ui, f, c);
                ui.end_row();
            }
        },
    );
    if let Some(n) = note {
        caption(ui, f.th, n);
    }
}

fn hover_of(c: &Control) -> String {
    match c.menu {
        Some(m) => format!("{} (menu {m}, CAT {})", c.what, c.cmd),
        None => format!("{} (CAT {})", c.what, c.cmd),
    }
}

/// One setting, drawn by its kind, showing the radio's read-back.
pub(crate) fn control(ui: &mut Ui, f: &mut Face<'_>, c: &'static Control) {
    let rb = logic::readback(c, f.s).map(str::to_string);
    let settable = logic::settable(f.p, c);
    // The receive filter is read only as a value, but set through the
    // protocol's filter table for the mode.
    let filters = f.s.mode.map(|m| f.p.filters(m)).unwrap_or_default();
    if c.cmd == "RF" && !filters.is_empty() {
        let mode = f.s.mode.unwrap_or_default();
        let now =
            f.s.value("RF").and_then(|b| f.p.describe(&Reply { key: "RF".into(), body: b.into() }));
        ui.add_enabled_ui(f.lock.is_none(), |ui| {
            ComboBox::from_id_salt("radio-rf")
                .selected_text(now.unwrap_or_else(|| "—".into()))
                .show_styled(ui, |ui| {
                    for w in &filters {
                        if ui.selectable_label(false, format!("{w} Hz")).clicked()
                            && let Some(fr) = f.p.set_filter(mode, *w)
                        {
                            f.out.frames.push(fr);
                        }
                    }
                });
        })
        .response
        .on_disabled_hover_text(f.lock.unwrap_or_default());
        return;
    }
    if !settable {
        let t = rb.as_deref().map_or("—".to_string(), |v| c.label(v));
        ui.horizontal(|ui| {
            ui.label(RichText::new(t).font(data_font(13.0)).color(f.th.ink));
            caption(ui, f.th, "read only");
        });
        return;
    }
    let lock = f.lock;
    let resp = ui.add_enabled_ui(lock.is_none(), |ui| match c.kind {
        Kind::Toggle => {
            let on = rb.as_deref() == Some("1");
            let text = match rb.as_deref() {
                Some("1") => "ON",
                Some("0") => "OFF",
                _ => "—",
            };
            if cap(ui, f.th, on, text).clicked() {
                f.send_control(c, if on { "0" } else { "1" });
            }
        }
        Kind::Choice(opts) => {
            let cur = rb.as_deref().map(|v| c.label(v)).unwrap_or_else(|| "—".into());
            ComboBox::from_id_salt(("radio-ctl", c.cmd)).selected_text(cur.clone()).show_styled(
                ui,
                |ui| {
                    for (v, l) in opts {
                        if ui.selectable_label(*l == cur, *l).clicked() && *l != cur {
                            f.send_control(c, v);
                        }
                    }
                },
            );
        }
        Kind::Range { lo, hi, step, unit, .. } => {
            let now: Option<i32> = rb.as_deref().and_then(|v| v.trim().parse().ok());
            let mut v = f.st.drags.get(c.cmd).copied().unwrap_or(now.unwrap_or(lo) as f64);
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = 150.0;
                let r = chrome::slider_vertical(
                    ui,
                    egui::Slider::new(&mut v, lo as f64..=hi as f64)
                        .step_by(step.max(1) as f64)
                        .show_value(false),
                );
                if r.dragged() {
                    f.st.drags.insert(c.cmd, v);
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    f.st.drags.remove(c.cmd);
                    let n = (v.round() as i32).clamp(lo, hi);
                    if Some(n) != now {
                        f.send_control(c, &n.to_string());
                    }
                }
                let shown = match (f.st.drags.get(c.cmd), now) {
                    (Some(d), _) => format!("→ {d:.0}"),
                    (None, Some(n)) => n.to_string(),
                    (None, None) => "—".into(),
                };
                let t = if unit.is_empty() { shown } else { format!("{shown} {unit}") };
                ui.label(RichText::new(t).font(data_font(13.0)).color(f.th.ink));
            });
        }
        Kind::Text { len } => {
            let mut buf =
                f.st.texts
                    .get(c.cmd)
                    .cloned()
                    .unwrap_or_else(|| rb.clone().unwrap_or_default().trim_end().to_string());
            ui.horizontal(|ui| {
                let r = ui
                    .add(egui::TextEdit::singleline(&mut buf).char_limit(len).desired_width(160.0));
                let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if go || cap(ui, f.th, false, "SET").clicked() {
                    f.send_control(c, &buf);
                }
            });
            f.st.texts.insert(c.cmd, buf);
        }
        Kind::ReadOnly => {}
    });
    if let Some(why) = lock {
        resp.response.on_disabled_hover_text(why);
    }
}

/// The ten stored CW messages: store, send (which transmits) and stop.
fn cw_messages(ui: &mut Ui, f: &mut Face<'_>, prefs: &mut RadioPrefs) {
    let th = f.th;
    if f.p.cw_send(1).is_none() {
        return;
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("STORED MESSAGES").font(label_font(12.0)).color(th.label));
        // Stopping is an unkey: always let through.
        let stop = ui
            .add_enabled_ui(f.s.connected, |ui| cap(ui, th, false, "STOP"))
            .inner
            .on_hover_text("Stop a stored message being sent");
        if stop.clicked()
            && let Some(fr) = f.p.cw_stop()
        {
            f.out.frames.push(fr);
        }
    });
    prefs.cw_msgs.resize(10, String::new());
    // A message transmits through the amplifier: only when TUNE could be
    // keyed at the full power setting (interlock #6), and never while
    // already transmitting (#5).
    let send_block = f.lock.or(f.carrier_blocked.as_deref());
    egui::Grid::new("radio-cw-msgs").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
        for n in 1..=10u8 {
            ui.label(RichText::new(n.to_string()).font(data_font(13.0)).color(th.dim));
            let text = &mut prefs.cw_msgs[n as usize - 1];
            ui.add_enabled_ui(f.lock.is_none(), |ui| {
                ui.add_sized(
                    [240.0, 22.0],
                    egui::TextEdit::singleline(text).char_limit(32).font(data_font(13.0)),
                );
            });
            let store = ui.add_enabled_ui(f.lock.is_none(), |ui| cap(ui, th, false, "STORE")).inner;
            if store.clicked() {
                match f.p.cw_store(n, text) {
                    Some(fr) => f.out.frames.push(fr),
                    None => {
                        f.st.note = Some("Only A-Z, 0-9, space and / ? . , = - , up to 32.".into())
                    }
                }
            }
            let send =
                ui.add_enabled_ui(send_block.is_none(), |ui| cap(ui, th, false, "SEND")).inner;
            let send = match send_block {
                Some(why) => send.on_disabled_hover_text(why),
                None => send.on_hover_text("Send this message: the radio transmits it"),
            };
            if send.clicked() {
                f.out.cw_send = f.p.cw_send(n);
            }
            ui.end_row();
        }
    });
    caption(ui, th, "The radio can't read its messages back: these are as last stored from here.");
}

fn mem_pop(ui: &mut Ui, f: &mut Face<'_>, prefs: &RadioPrefs) {
    let th = f.th;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("CHANNEL").font(label_font(12.0)).color(th.label));
        ui.add(egui::DragValue::new(&mut f.st.mem).range(0..=199));
        let lock = f.lock;
        let r = ui
            .add_enabled_ui(lock.is_none() && f.p.recall_memory(0).is_some(), |ui| {
                cap(ui, th, false, "RECALL")
            })
            .inner;
        if r.clicked()
            && let Some(fr) = f.p.recall_memory(f.st.mem)
        {
            f.out.frames.push(fr);
        }
        let dial = logic::dial_hz(f.s);
        let mode = f.s.mode.unwrap_or("USB");
        let can_store = dial.and_then(|h| f.p.store_memory(f.st.mem, h, mode));
        let r = ui
            .add_enabled_ui(lock.is_none() && can_store.is_some(), |ui| cap(ui, th, false, "STORE"))
            .inner;
        let r = if f.p.store_memory(0, 14_000_000, "USB").is_none() {
            r.on_disabled_hover_text(
                "Writing a memory over CAT isn't known for this radio yet: store it on the radio.",
            )
        } else {
            r.on_disabled_hover_text(lock.unwrap_or("The dial hasn't been read yet."))
        };
        if r.clicked()
            && let Some(fr) = can_store
        {
            f.out.frames.push(fr);
        }
        if let Some(mc) = f.s.value("MC") {
            caption(ui, th, &format!("radio on channel {}", mc.trim()));
        }
    });
    let _ = prefs;
}

/// A/B, A=B, SPLIT and RIT.
pub(crate) fn vfo_row(ui: &mut Ui, f: &mut Face<'_>) {
    let th = f.th;
    let lock = f.lock;
    ui.add_enabled_ui(lock.is_none(), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let b = logic::rx_vfo(f.s) == Vfo::B;
            if cap(ui, th, false, if b { "B/A" } else { "A/B" })
                .on_hover_text("Receive on the other VFO")
                .clicked()
            {
                f.push(f.p.select_vfo(if b { Vfo::A } else { Vfo::B }));
            }
            if cap(ui, th, false, "A=B").on_hover_text("VFO B to VFO A's frequency").clicked() {
                f.push(logic::a_to_b(f.p, f.s));
            }
            let split = f.s.split == Some(true);
            if cap(ui, th, split, "SPLIT").on_hover_text("Transmit on VFO B").clicked() {
                f.push(f.p.set_split(!split));
            }
            ui.add_space(6.0);
            let rit = f.s.rit_on == Some(true);
            if cap(ui, th, rit, "RIT").on_hover_text("Receiver offset on or off").clicked() {
                f.push(f.p.set_rit_on(!rit));
            }
            let off = logic::rit_hz(f.s);
            for (t, d) in [("−", -10), ("+", 10)] {
                if cap(ui, th, false, t).on_hover_text("Move the RIT offset 10 Hz").clicked() {
                    let v = f.p.set_rit(off.unwrap_or(0) + d);
                    f.out.frames.extend(v);
                }
            }
            if cap(ui, th, false, "CLR").on_hover_text("Clear the RIT offset").clicked() {
                let v = f.p.set_rit(0);
                f.out.frames.extend(v);
            }
            let t = off.map_or("RIT —".into(), |h| format!("RIT {:+} Hz", h));
            ui.label(RichText::new(t).font(data_font(13.0)).color(th.label));
        });
    })
    .response
    .on_disabled_hover_text(lock.unwrap_or_default());
}

/// FWD, RFL and SWR, live from the radio, and its TX input.
pub(crate) fn readouts(ui: &mut Ui, f: &Face<'_>) {
    let th = f.th;
    let s = f.s;
    let w = |p: Option<rigcat::Watts>| match p {
        Some(p) if p.valid => format!("{:.2} W", p.w),
        _ => "—".into(),
    };
    let swr = s.swr_ratio().map_or("—".into(), |r| format!("{r:.2}:1"));
    let input = logic::find(f.p, "TI")
        .and_then(|c| logic::readback(c, s).map(|v| c.label(v)))
        .unwrap_or_else(|| "—".into());
    let cells =
        [("FWD · FP", w(s.fwd)), ("RFL · RP", w(s.rfl)), ("SWR · WR", swr), ("TX IN · 32", input)];
    let width = ui.available_width();
    let cw = (width - 3.0) / 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), th.case_line);
    for (i, (k, v)) in cells.iter().enumerate() {
        let r = egui::Rect::from_min_size(
            rect.min + egui::vec2(i as f32 * (cw + 1.0), 0.0),
            egui::vec2(cw, 40.0),
        )
        .shrink(1.0);
        p.rect_filled(r, 0.0, th.screen);
        p.text(
            r.left_top() + egui::vec2(8.0, 5.0),
            egui::Align2::LEFT_TOP,
            *k,
            label_font(10.5),
            th.dim,
        );
        p.text(
            r.left_top() + egui::vec2(8.0, 19.0),
            egui::Align2::LEFT_TOP,
            v,
            data_font(14.0),
            th.ink,
        );
    }
    if s.swr.is_some_and(|x| x.tripped) {
        ui.label(
            RichText::new("The radio dropped to receive on a high SWR")
                .color(crate::theme::ALERT()),
        );
    }
}

/// The MENU window: every control the radio has, grouped, with the panel's
/// own display settings at the top. Shown in `ctx`, the radio window's
/// viewport.
pub(crate) fn menu_window(ctx: &egui::Context, f: &mut Face<'_>, prefs: &mut RadioPrefs) {
    let th = f.th;
    let mut open = f.st.menu;
    egui::Window::new(format!("{} menus", th.model))
        .id(egui::Id::new("radio-menu-window"))
        .open(&mut open)
        .frame(chrome::window_frame())
        .default_pos([220.0, 80.0])
        .default_size([760.0, 560.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            chrome::window_body_bg(ui);
            ui.horizontal_wrapped(|ui| {
                let read = match f.s.settings_read {
                    Some(t) => format!(
                        "Read over CAT every 10 s between transmissions · last {} s ago",
                        t.elapsed().as_secs()
                    ),
                    None => "Read over CAT every 10 s between transmissions".into(),
                };
                caption(ui, th, &read);
                if chrome::chip(ui, false, "READ AGAIN")
                    .on_hover_text("Ask the radio for every setting now")
                    .clicked()
                {
                    f.out.refresh = true;
                }
                caption(ui, th, "Show");
                ComboBox::from_id_salt("radio-menu-filter")
                    .selected_text(f.st.menu_filter.label())
                    .show_styled(ui, |ui| {
                        for m in MenuFilter::ALL {
                            ui.selectable_value(&mut f.st.menu_filter, m, m.label());
                        }
                    });
            });
            ui.add_space(6.0);
            panel_display(ui, th, prefs);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                menu_table(ui, f);
            });
            ui.add_space(4.0);
            let foot = f.s.set_note.clone().unwrap_or_else(|| {
                "Changes are sent when you pick a value and read back to confirm: each shows what \
                 the radio answered. The radio refuses changes while it transmits, so they are \
                 greyed then. Menus with no CAT command can only be changed on the radio."
                    .into()
            });
            caption(ui, th, &foot);
        });
    f.st.menu = open;
}

/// The panel's own settings for the radio window: the LCD's backlights and
/// the transverter offset. None of these is sent to the radio.
fn panel_display(ui: &mut Ui, th: &RigTheme, prefs: &mut RadioPrefs) {
    popout_frame(th).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("PANEL DISPLAY").font(label_font(12.5)).color(th.label));
        for (which, title) in
            [(false, "LCD backlight in receive"), (true, "LCD backlight in transmit")]
        {
            ui.horizontal_wrapped(|ui| {
                caption(ui, th, title);
                for (i, (name, col)) in th.backlights.iter().enumerate() {
                    let sel = if which { &mut prefs.tx_light } else { &mut prefs.rx_light };
                    if swatch(ui, th, *sel == i, *col, name).clicked() {
                        *sel = i;
                    }
                }
            });
        }
        caption(
            ui,
            th,
            "In transmit the radio's own menu 73 TX BACKLIGHT, when off, keeps the receive colour.",
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            caption(ui, th, "Transverter LO");
            let mut mhz = prefs.xvtr_hz as f64 / 1e6;
            let r = ui.add(
                egui::DragValue::new(&mut mhz)
                    .range(-12_000.0..=12_000.0)
                    .speed(0.001)
                    .fixed_decimals(6)
                    .suffix(" MHz"),
            );
            if r.changed() {
                prefs.xvtr_hz = (mhz * 1e6).round() as i64;
            }
            r.on_hover_text(
                "The converter's local oscillator, signed: the LCD shows the radio's frequency \
                 plus this, and typed entry takes the RF frequency. 0 with no converter. Kept \
                 by the panel; the radio is not told.",
            );
            if cap(ui, th, false, "NONE").on_hover_text("No transverter: offset 0").clicked() {
                prefs.xvtr_hz = 0;
            }
            if prefs.xvtr_hz != 0 {
                caption(ui, th, "XVTR on: the LCD shows RF, the radio stays on 9 kHz to 54 MHz");
            }
        });
    });
}

fn swatch(ui: &mut Ui, th: &RigTheme, sel: bool, col: Color32, name: &str) -> egui::Response {
    let font = label_font(12.0);
    let g =
        ui.painter().layout_no_wrap(name.to_uppercase(), font, if sel { th.ink } else { th.label });
    let size = egui::vec2(g.size().x + 34.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), th.case_hi);
    let edge = if sel { th.label } else { Color32::from_gray(0x3a) };
    p.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(if sel { 2.0 } else { 1.0 }, edge),
        egui::StrokeKind::Inside,
    );
    let sw = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(8.0, -6.0),
        egui::vec2(12.0, 12.0),
    );
    p.rect_filled(sw, 2.0, col);
    p.galley(rect.left_center() + egui::vec2(26.0, -g.size().y / 2.0), g, th.label);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn group_title(g: Group) -> &'static str {
    match g {
        Group::Radio => "Radio face",
        Group::Receive => "Receiver",
        Group::Cw => "CW",
        Group::TxAudio => "Transmit audio",
        Group::Tune => "TUNE and PTT",
        Group::Menu => "Other menus",
        Group::Info => "About the radio",
    }
}

const GROUPS: [Group; 7] = [
    Group::Radio,
    Group::Receive,
    Group::Cw,
    Group::TxAudio,
    Group::Tune,
    Group::Menu,
    Group::Info,
];

fn menu_table(ui: &mut Ui, f: &mut Face<'_>) {
    let th = f.th;
    let filter = f.st.menu_filter;
    for g in GROUPS {
        let list: Vec<&'static Control> =
            f.p.controls()
                .iter()
                .filter(|c| c.group == g)
                .filter(|c| match filter {
                    MenuFilter::All => true,
                    MenuFilter::Settable => logic::settable(f.p, c),
                    MenuFilter::Transmit => matches!(c.group, Group::TxAudio | Group::Tune),
                })
                .collect();
        if list.is_empty() {
            continue;
        }
        ui.add_space(6.0);
        let tx = matches!(g, Group::TxAudio | Group::Tune);
        ui.label(
            RichText::new(group_title(g).to_uppercase()).font(label_font(12.5)).color(if tx {
                th.cap_lit
            } else {
                th.label
            }),
        );
        egui::Grid::new(("radio-menu", g as u8))
            .num_columns(5)
            .spacing([12.0, 5.0])
            .striped(true)
            .show(ui, |ui| {
                for h in ["MENU", "NAME", "WHAT IT DOES", "SET TO", "CAT"] {
                    ui.label(RichText::new(h).font(label_font(10.5)).color(th.dim));
                }
                ui.end_row();
                for c in list {
                    ui.label(
                        RichText::new(c.menu.map(|m| m.to_string()).unwrap_or_else(|| "—".into()))
                            .font(data_font(13.0))
                            .color(th.dim),
                    );
                    ui.label(RichText::new(c.name).font(label_font(12.0)).color(th.label));
                    ui.scope(|ui| {
                        ui.set_width(240.0);
                        ui.add(
                            egui::Label::new(RichText::new(c.what).color(th.dim).size(12.0)).wrap(),
                        );
                    });
                    control(ui, f, c);
                    ui.label(RichText::new(c.cmd).font(data_font(12.0)).color(th.dim));
                    ui.end_row();
                }
            });
    }
}

/// The question a data mode or the IN annunciator asks before it changes
/// where the radio takes its transmit audio from.
pub(crate) fn confirm(ctx: &egui::Context, f: &mut Face<'_>) {
    let Some(q) = f.st.confirm else { return };
    if f.lock.is_some() {
        f.st.confirm = None;
        return;
    }
    let th = f.th;
    let mut done = false;
    egui::Modal::new(egui::Id::new("radio-confirm")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        match q {
            Confirm::Digi(name) => {
                ui.label(format!(
                    "{name} sends the computer's audio. The radio's TX IN (menu 32) is on {}: set it \
                     to USB audio too? It changes where the radio takes transmit audio from, and \
                     stays set after the panel closes.",
                    f.s.tx_input.as_deref().unwrap_or("a setting not yet read")
                ));
                ui.horizontal(|ui| {
                    if cap(ui, th, true, "SET USB AUDIO TOO").clicked() {
                        digi(f, name, true);
                        done = true;
                    }
                    if cap(ui, th, false, "LEAVE TX IN").clicked() {
                        digi(f, name, false);
                        done = true;
                    }
                    done |= cap(ui, th, false, "CANCEL").clicked();
                });
            }
            Confirm::Input => {
                ui.label(
                    "Where should the radio take its transmit audio from in SSB, AM and FM (menu \
                     32)? TX TUNE and the data modes need USB audio; on the microphone the radio \
                     sends whatever it picks up.",
                );
                ui.horizontal(|ui| {
                    if let Some(c) = logic::find(f.p, "TI")
                        && let Kind::Choice(opts) = c.kind
                    {
                        let now = logic::readback(c, f.s).map(str::to_string);
                        for (v, l) in opts {
                            if cap(ui, th, now.as_deref() == Some(*v), l).clicked() {
                                if now.as_deref() != Some(*v) {
                                    f.send_control(c, v);
                                }
                                done = true;
                            }
                        }
                    }
                    done |= cap(ui, th, false, "CANCEL").clicked();
                });
            }
        }
    });
    if done {
        f.st.confirm = None;
    }
}
