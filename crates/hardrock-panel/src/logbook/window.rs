//! The LOG window: the QSOs a day at a time, newest first, with + NEW ENTRY,
//! EDIT and DEL, IMPORT and ADIF export, FILL MISSING, the eQSL and LoTW
//! uploads and their confirmations, and each QSO's QSL state (↑ sent, ✔
//! confirmed). Docked over the panel or popped out, like GLOBE and DECODE.
//! Also SETTINGS → Logbook.
//!
//! After SDRoxide's logbook window (`sdroxide-ui/src/app/logbook.rs`,
//! https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Vec2};

use super::lookup::{self, Batch, Msg};
use super::{
    Credentials, LogSettings, Logbook, LookupService, QsoRecord, Service, adif, adif_band,
    adif_today, upload,
};
use crate::station::StationSettings;
use crate::{chrome, theme};

/// What the window asks of the rest of the panel.
#[derive(Default)]
pub(crate) struct LogOut {
    /// Pop out (`true`) or dock (`false`).
    pub pop: Option<bool>,
}

/// A new or edited QSO, as typed: kept as text so half-typed fields don't
/// fight the operator, and read into a [`QsoRecord`] on SAVE. An edit starts
/// from the QSO as it was, so what the form doesn't show survives.
#[derive(Clone, Default)]
struct Form {
    base: QsoRecord,
    call: String,
    grid: String,
    freq_mhz: String,
    mode: String,
    rst_sent: String,
    rst_rcvd: String,
    date: String,
    time: String,
    name: String,
    qth: String,
    state: String,
    country: String,
    pwr: String,
    comment: String,
    /// Why SAVE didn't, last time.
    problem: Option<String>,
}

fn date_text(unix: i64) -> (String, String) {
    let (y, m, d, h, mi, _) = crate::decodelog::civil(unix);
    (format!("{y:04}-{m:02}-{d:02}"), format!("{h:02}:{mi:02}"))
}

impl Form {
    fn new_at(now: i64, freq_hz: Option<u64>, mode: &str) -> Form {
        let (date, time) = date_text(now);
        Form {
            freq_mhz: freq_hz.map(|f| format!("{:.6}", f as f64 / 1e6)).unwrap_or_default(),
            mode: mode.to_string(),
            date,
            time,
            ..Form::default()
        }
    }

    fn of(r: &QsoRecord) -> Form {
        let (date, time) = date_text(r.start_utc);
        Form {
            base: r.clone(),
            call: r.call.clone(),
            grid: r.grid.clone(),
            freq_mhz: if r.freq_hz > 0.0 {
                format!("{:.6}", r.freq_hz / 1e6)
            } else {
                String::new()
            },
            mode: r.mode_shown().to_string(),
            rst_sent: r.rst_sent.clone(),
            rst_rcvd: r.rst_rcvd.clone(),
            date,
            time,
            name: r.name.clone(),
            qth: r.qth.clone(),
            state: r.state.clone(),
            country: r.country.clone(),
            pwr: r.tx_pwr_w.map(|p| format!("{p}")).unwrap_or_default(),
            comment: r.comment.clone(),
            problem: None,
        }
    }

    /// The band the frequency typed is in.
    fn band(&self) -> String {
        self.freq_mhz
            .trim()
            .parse::<f64>()
            .map(|m| adif_band(m * 1e6).to_string())
            .unwrap_or_default()
    }

    /// The QSO the form describes, or what is wrong with it.
    fn record(&self, st: &StationSettings) -> Result<QsoRecord, String> {
        let call = self.call.trim().to_ascii_uppercase();
        if !crate::station::is_callsign(&call) {
            return Err("Enter their callsign".into());
        }
        let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
        let (d, t) = (digits(&self.date), digits(&self.time));
        let start =
            adif::parse_date_time(&d, &t).ok_or("Date as 2026-11-10 and UTC time as 21:15")?;
        let mut r = self.base.clone();
        let freq = self.freq_mhz.trim();
        r.freq_hz = if freq.is_empty() {
            0.0
        } else {
            freq.parse::<f64>().map_err(|_| "Frequency in MHz, as 14.074")? * 1e6
        };
        r.band = if r.freq_hz > 0.0 { adif_band(r.freq_hz).to_string() } else { r.band.clone() };
        let mode = self.mode.trim().to_ascii_uppercase();
        if mode.is_empty() {
            return Err("Enter the mode".into());
        }
        if mode != r.mode_shown() {
            (r.mode, r.submode) = (mode, String::new());
        }
        // The start may move; the end keeps its length after it.
        let len = (r.end_utc - r.start_utc).max(0);
        r.start_utc = start;
        r.end_utc = start + len;
        r.call = call;
        let g = self.grid.trim();
        if !g.is_empty() && crate::station::grid_check(g) != crate::station::GridCheck::Valid {
            return Err("Their grid as FN20 or FN20xr".into());
        }
        r.grid = g.to_string();
        r.rst_sent = self.rst_sent.trim().to_string();
        r.rst_rcvd = self.rst_rcvd.trim().to_string();
        r.name = self.name.trim().to_string();
        r.qth = self.qth.trim().to_string();
        r.state = self.state.trim().to_ascii_uppercase();
        r.country = self.country.trim().to_string();
        r.tx_pwr_w = self.pwr.trim().trim_end_matches(['W', 'w']).trim().parse().ok();
        r.comment = self.comment.trim().to_string();
        if r.my_call.is_empty() {
            r.my_call = st.call().unwrap_or_default();
        }
        if r.my_grid.is_empty() {
            r.my_grid = st.grid().unwrap_or_default();
        }
        Ok(r)
    }
}

/// A line of the list: a day's heading, or a QSO (by index into the log).
#[derive(Clone, Copy)]
enum Item {
    Day {
        start: i64,
        count: usize,
    },
    /// Its index in the log, its id, and how many QSOs with the same call
    /// came before it.
    Qso {
        idx: usize,
        id: u64,
        earlier: usize,
    },
}

/// What an eQSL upload sends back: the QSOs now at eQSL, and a line to show.
type EqslDone = Result<(Vec<u64>, String), String>;
/// What TQSL sends back (mark sent?, a line to show), and the QSOs sent.
type LotwJob = (Receiver<(bool, String)>, Vec<u64>);
/// One service's confirmation download.
type Confirmed = (Service, Result<Vec<QsoRecord>, String>);

/// The window's state: the selection, the form, and the jobs running.
#[derive(Default)]
pub(crate) struct LogUi {
    selected: BTreeSet<u64>,
    /// The QSO clicked last, for a Shift-click range.
    anchor: Option<u64>,
    form: Option<Form>,
    /// The file IMPORT reads and ADIF export writes.
    file: String,
    /// Only calls containing this.
    search: String,
    /// The last thing done, and whether it went wrong.
    status: Option<(String, bool)>,
    /// DEL pressed once: press again to delete.
    confirm_delete: Option<Instant>,
    /// FILL MISSING's lookups, and the calls they are for.
    batch: Option<Batch>,
    /// LOOKUP in the form.
    form_lookup: Option<Batch>,
    eqsl: Option<Receiver<EqslDone>>,
    lotw: Option<LotwJob>,
    confirms: Option<Receiver<Confirmed>>,
    /// TQSL, looked for every few seconds.
    tqsl: Option<(Instant, Result<PathBuf, String>)>,
    /// The list, built for (log generation, search).
    items: Vec<Item>,
    built: Option<(u64, String)>,
}

impl LogUi {
    fn say(&mut self, msg: impl Into<String>, problem: bool) {
        let m = msg.into();
        if problem {
            tracing::warn!("logbook: {m}");
        } else {
            tracing::info!("logbook: {m}");
        }
        self.status = Some((m, problem));
    }

    /// TQSL, or why the LoTW button is greyed.
    fn tqsl(&mut self, s: &LogSettings) -> Result<PathBuf, String> {
        let stale = self.tqsl.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(5));
        if stale {
            self.tqsl = Some((Instant::now(), upload::find_tqsl(&s.tqsl_path)));
        }
        self.tqsl.as_ref().expect("just set").1.clone()
    }

    /// Anything running on another thread.
    fn busy(&self) -> bool {
        self.batch.is_some()
            || self.form_lookup.is_some()
            || self.eqsl.is_some()
            || self.lotw.is_some()
            || self.confirms.is_some()
    }

    /// The QSOs a send acts on: those selected, or else every one not yet
    /// sent to `service`.
    fn to_send(&self, log: &Logbook, service: Service) -> Vec<u64> {
        if self.selected.is_empty() {
            log.records().iter().filter(|r| !r.qsl(service).sent).map(|r| r.id).collect()
        } else {
            self.selected.iter().copied().filter(|id| log.get(*id).is_some()).collect()
        }
    }

    /// Take in what the jobs on other threads have finished.
    fn poll(&mut self, log: &mut Logbook) {
        if let Some(b) = &mut self.batch {
            loop {
                match b.rx.try_recv() {
                    Ok(Msg::Found(i)) => {
                        b.done += 1;
                        log.modify(|r| r.call == i.call && lookup::fill_missing(r, &i));
                    }
                    Ok(Msg::NotFound(_)) => b.done += 1,
                    Ok(Msg::Failed(e)) => {
                        self.batch = None;
                        self.say(format!("FILL MISSING stopped: {e}"), true);
                        break;
                    }
                    Ok(Msg::Done) | Err(TryRecvError::Disconnected) => {
                        let n = b.done;
                        self.batch = None;
                        self.say(format!("FILL MISSING: {n} calls looked up"), false);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        if let Some(b) = &self.form_lookup {
            match b.rx.try_recv() {
                Ok(Msg::Found(i)) => {
                    if let Some(f) = &mut self.form {
                        let mut r = QsoRecord {
                            call: f.call.trim().to_ascii_uppercase(),
                            name: f.name.clone(),
                            qth: f.qth.clone(),
                            state: f.state.clone(),
                            country: f.country.clone(),
                            grid: f.grid.clone(),
                            ..QsoRecord::default()
                        };
                        if r.call == i.call && lookup::fill_missing(&mut r, &i) {
                            (f.name, f.qth, f.state, f.country, f.grid) =
                                (r.name, r.qth, r.state, r.country, r.grid);
                            // Zones and DXCC aren't on the form: keep them for SAVE.
                            f.base.dxcc = f.base.dxcc.or(r.dxcc);
                            f.base.cq_zone = f.base.cq_zone.or(r.cq_zone);
                            f.base.itu_zone = f.base.itu_zone.or(r.itu_zone);
                            f.base.county = if f.base.county.is_empty() {
                                r.county
                            } else {
                                f.base.county.clone()
                            };
                        }
                    }
                    self.form_lookup = None;
                }
                Ok(Msg::NotFound(c)) => {
                    self.form_lookup = None;
                    self.say(format!("{c}: not found"), false);
                }
                Ok(Msg::Failed(e)) => {
                    self.form_lookup = None;
                    self.say(e, true);
                }
                Ok(Msg::Done) | Err(TryRecvError::Disconnected) => self.form_lookup = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.eqsl {
            match rx.try_recv() {
                Ok(Ok((ids, msg))) => {
                    self.eqsl = None;
                    log.mark_sent(&ids, Service::Eqsl, &adif_today());
                    self.say(msg, false);
                }
                Ok(Err(e)) => {
                    self.eqsl = None;
                    self.say(e, true);
                }
                Err(TryRecvError::Disconnected) => self.eqsl = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((rx, ids)) = &self.lotw {
            match rx.try_recv() {
                Ok((ok, msg)) => {
                    if ok {
                        let ids = ids.clone();
                        log.mark_sent(&ids, Service::Lotw, &adif_today());
                    }
                    self.lotw = None;
                    self.say(msg, !ok);
                }
                Err(TryRecvError::Disconnected) => self.lotw = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut said = Vec::new();
        if let Some(rx) = &self.confirms {
            loop {
                match rx.try_recv() {
                    Ok((service, Ok(recs))) => {
                        let n = log.apply_confirmations(&recs, service);
                        said.push((
                            format!("{}: {} confirmations, {n} new", service.label(), recs.len()),
                            false,
                        ));
                    }
                    Ok((_, Err(e))) => said.push((e, true)),
                    Err(TryRecvError::Disconnected) => {
                        self.confirms = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        for (m, bad) in said {
            self.say(m, bad);
        }
    }

    /// The list for the log as it is, rebuilt when the log or the search
    /// changes.
    fn refresh(&mut self, log: &Logbook) {
        let key = (log.generation(), self.search.trim().to_ascii_uppercase());
        if self.built.as_ref() == Some(&key) {
            return;
        }
        let recs = log.records();
        // How many QSOs with each call came before each QSO, over the whole log.
        let mut by_time: Vec<usize> = (0..recs.len()).collect();
        by_time.sort_by_key(|&i| recs[i].start_utc);
        let mut earlier = vec![0usize; recs.len()];
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for &i in &by_time {
            let n = seen.entry(recs[i].call.as_str()).or_default();
            earlier[i] = *n;
            *n += 1;
        }
        let mut order: Vec<usize> = (0..recs.len())
            .filter(|&i| key.1.is_empty() || recs[i].call.contains(&key.1))
            .collect();
        order.sort_by_key(|&i| std::cmp::Reverse(recs[i].start_utc));
        self.items.clear();
        let mut i = 0;
        while i < order.len() {
            let day = recs[order[i]].start_utc.div_euclid(86_400);
            let j = order[i..]
                .iter()
                .position(|&k| recs[k].start_utc.div_euclid(86_400) != day)
                .map_or(order.len(), |p| p + i);
            self.items.push(Item::Day { start: day * 86_400, count: j - i });
            self.items.extend(order[i..j].iter().map(|&k| Item::Qso {
                idx: k,
                id: recs[k].id,
                earlier: earlier[k],
            }));
            i = j;
        }
        self.selected.retain(|id| log.get(*id).is_some());
        self.built = Some(key);
    }
}

/// FILL MISSING: the country file's country, zones and continent at once,
/// then a lookup of every call still missing something.
fn fill_missing(ui_state: &mut LogUi, log: &mut Logbook, s: &LogSettings) {
    let offline = {
        let snapshot: Vec<QsoRecord> = log.records().to_vec();
        let mut filled = snapshot;
        let mut n = 0;
        for r in &mut filled {
            if log.fill_offline(r) {
                n += 1;
            }
        }
        if n > 0 {
            let mut it = filled.into_iter();
            log.modify(|r| {
                let new = it.next().expect("same length");
                if *r != new {
                    *r = new;
                    true
                } else {
                    false
                }
            });
        }
        n
    };
    let mut calls: Vec<String> =
        log.records().iter().filter(|r| lookup::wants_lookup(r)).map(|r| r.call.clone()).collect();
    calls.sort();
    calls.dedup();
    match s.lookup_login() {
        Ok(_) if calls.is_empty() => ui_state.say(
            format!("FILL MISSING: {offline} QSOs filled from cty.dat; nothing left to look up"),
            false,
        ),
        Ok((service, login)) => {
            let secs = (calls.len() as f32 * lookup::pace(service).as_secs_f32()).round();
            ui_state.say(
                format!(
                    "FILL MISSING: {offline} QSOs filled from cty.dat; looking up {} calls on {} (about {secs:.0} s)",
                    calls.len(),
                    service.label()
                ),
                false,
            );
            ui_state.batch = Some(Batch::start(service, login, calls));
        }
        Err(e) => ui_state
            .say(format!("FILL MISSING: {offline} QSOs filled from cty.dat. {e}"), offline == 0),
    }
}

/// Look `call` up for the form.
fn form_lookup(ui_state: &mut LogUi, s: &LogSettings, call: &str) {
    match s.lookup_login() {
        Ok((service, login)) => {
            ui_state.form_lookup = Some(Batch::start(service, login, vec![call.to_string()]))
        }
        Err(e) => ui_state.say(e, true),
    }
}

/// Download confirmations from every service with a login.
fn start_confirms(ui_state: &mut LogUi, s: &LogSettings) {
    let (eqsl, qth, lotw) = (s.eqsl.clone(), s.eqsl_qth.clone(), s.lotw.clone());
    if !eqsl.is_set() && !lotw.is_set() {
        ui_state.say(
            "Set your eQSL or LoTW login in SETTINGS → Logbook to download confirmations",
            true,
        );
        return;
    }
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new().name("log-confirm".into()).spawn(move || {
        let web = super::net::Web::new();
        if eqsl.is_set() {
            let _ = tx.send((Service::Eqsl, upload::eqsl_confirmations(&web, &eqsl, &qth)));
        }
        if lotw.is_set() {
            let _ = tx.send((Service::Lotw, upload::lotw_confirmations(&web, &lotw)));
        }
    });
    match spawned {
        Ok(_) => {
            ui_state.confirms = Some(rx);
            ui_state.say("Downloading confirmations…", false);
        }
        Err(e) => ui_state.say(format!("couldn't start the download: {e}"), true),
    }
}

/// Send to eQSL.
fn start_eqsl(ui_state: &mut LogUi, log: &Logbook, s: &LogSettings) {
    let ids = ui_state.to_send(log, Service::Eqsl);
    if ids.is_empty() {
        ui_state.say("eQSL: every QSO has been sent", false);
        return;
    }
    let recs: Vec<QsoRecord> = ids.iter().filter_map(|id| log.get(*id).cloned()).collect();
    let (login, qth): (Credentials, String) = (s.eqsl.clone(), s.eqsl_qth.clone());
    let (tx, rx) = channel();
    let n = recs.len();
    let spawned = std::thread::Builder::new().name("log-eqsl".into()).spawn(move || {
        let web = super::net::Web::new();
        let refs: Vec<&QsoRecord> = recs.iter().collect();
        let _ = tx.send(upload::eqsl_upload(&web, &login, &qth, &refs));
    });
    match spawned {
        Ok(_) => {
            ui_state.eqsl = Some(rx);
            ui_state.say(format!("Sending {n} QSOs to eQSL…"), false);
        }
        Err(e) => ui_state.say(format!("couldn't start the upload: {e}"), true),
    }
}

/// Sign and upload with TQSL.
fn start_lotw(ui_state: &mut LogUi, log: &Logbook, s: &LogSettings, tqsl: PathBuf) {
    let ids = ui_state.to_send(log, Service::Lotw);
    if ids.is_empty() {
        ui_state.say("LoTW: every QSO has been sent", false);
        return;
    }
    let recs: Vec<QsoRecord> = ids.iter().filter_map(|id| log.get(*id).cloned()).collect();
    let dir =
        log.path().and_then(|p| p.parent()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let n = recs.len();
    ui_state.lotw = Some((upload::lotw_upload(tqsl, s.tqsl_location.clone(), dir, recs), ids));
    ui_state.say(format!("TQSL is signing and uploading {n} QSOs…"), false);
}

/// The file the File box names, or the usual export beside the log.
fn file_or_default(ui_state: &LogUi, log: &Logbook) -> Option<PathBuf> {
    let f = ui_state.file.trim().trim_matches('"');
    if !f.is_empty() {
        return Some(PathBuf::from(f));
    }
    let dir = log.path()?.parent()?;
    Some(dir.join(format!("hardrock-panel-export-{}.adi", adif_today())))
}

fn import(ui_state: &mut LogUi, log: &mut Logbook, path: PathBuf) {
    match log.import_file(&path) {
        Ok(rep) => ui_state.say(format!("Imported {}: {}", path.display(), rep.summary()), false),
        Err(e) => ui_state.say(format!("Import: {e}"), true),
    }
}

fn export(ui_state: &mut LogUi, log: &Logbook) {
    let Some(path) = file_or_default(ui_state, log) else {
        ui_state.say("Type a file to export to in the File box", true);
        return;
    };
    if path
        .extension()
        .is_none_or(|e| !e.eq_ignore_ascii_case("adi") && !e.eq_ignore_ascii_case("adif"))
    {
        ui_state.say("Export to a file ending .adi", true);
        return;
    }
    let ids: Vec<u64> = ui_state.selected.iter().copied().collect();
    let sel = (!ids.is_empty()).then_some(ids.as_slice());
    let n = sel.map_or(log.records().len(), <[u64]>::len);
    match std::fs::write(&path, log.export(sel)) {
        Ok(()) => ui_state.say(format!("{n} QSOs written to {}", path.display()), false),
        Err(e) => ui_state.say(format!("{}: {e}", path.display()), true),
    }
}

/// The LOG window, when `open`; and every frame, open or not, whatever the
/// lookups and uploads have finished. `dial` is where the radio is and its
/// mode, for + NEW ENTRY.
pub(crate) fn show(
    ctx: &egui::Context,
    open: &mut bool,
    log: &mut Logbook,
    ui_state: &mut LogUi,
    s: &mut LogSettings,
    st: &StationSettings,
    dial: Option<(u64, Option<&str>)>,
) -> LogOut {
    ui_state.poll(log);
    let mut out = LogOut::default();
    if ui_state.busy() {
        crate::repaint::after(ctx, Duration::from_millis(250));
    }
    if !*open {
        return out;
    }
    if s.popped {
        let closed = ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("log-window"),
            crate::popout::builder(ctx, "log-window", "Log", [1100.0, 720.0], [760.0, 420.0]),
            |ui, _class| {
                crate::popout::track(ui.ctx(), "log-window");
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                    .show(ui, |ui| {
                        chrome::window_body_bg(ui);
                        body(ui, true, log, ui_state, s, st, dial, &mut out);
                    });
                ui.ctx().input(|i| i.viewport().close_requested())
            },
        );
        if closed {
            *open = false;
        }
    } else {
        egui::Window::new("Log")
            .id(egui::Id::new("log-window"))
            .open(open)
            .frame(chrome::window_frame())
            .resizable(true)
            .collapsible(false)
            .default_pos([24.0, 48.0])
            .default_size([
                (ctx.content_rect().width() - 48.0).clamp(640.0, 1040.0),
                (ctx.content_rect().height() - 72.0).clamp(320.0, 640.0),
            ])
            .min_size([640.0, 320.0])
            .constrain_to(ctx.content_rect().with_min_y(40.0))
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                body(ui, false, log, ui_state, s, st, dial, &mut out);
            });
    }
    out
}

fn caps(t: &str) -> RichText {
    RichText::new(t).size(10.5).strong().color(theme::TEXT())
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut egui::Ui,
    popped: bool,
    log: &mut Logbook,
    u: &mut LogUi,
    s: &mut LogSettings,
    st: &StationSettings,
    dial: Option<(u64, Option<&str>)>,
    out: &mut LogOut,
) {
    // A file dropped on the window is imported.
    let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    });
    for p in dropped {
        import(u, log, p);
    }
    top_bar(ui, popped, log, out);
    ui.add_space(4.0);
    toolbar(ui, log, u, s, st, dial);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(caps("FILE"));
        chrome::field(
            ui,
            egui::TextEdit::singleline(&mut u.file)
                .desired_width(420.0)
                .hint_text("an .adi file to import or export to (or drop one on this window)"),
        )
        .on_hover_text(
            "IMPORT reads this file. ADIF writes the selected QSOs, or all of them, to it; left \
             empty, ADIF writes hardrock-panel-export-<date>.adi beside the log",
        );
        ui.add_space(12.0);
        ui.label(caps("FIND"));
        chrome::field(
            ui,
            egui::TextEdit::singleline(&mut u.search).desired_width(110.0).hint_text("call"),
        );
    });
    if let Some(p) = log.problem() {
        ui.label(RichText::new(p).size(11.5).color(theme::YELLOW()));
    }
    if let Some((m, bad)) = &u.status {
        ui.label(RichText::new(m).size(11.5).color(if *bad {
            theme::YELLOW()
        } else {
            theme::CYAN()
        }));
    }
    if u.form.is_some() {
        ui.add_space(4.0);
        form(ui, log, u, s, st);
    }
    ui.add_space(4.0);
    list(ui, log, u);
}

fn top_bar(ui: &mut egui::Ui, popped: bool, log: &Logbook, out: &mut LogOut) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("LOGBOOK").size(13.0).strong().color(theme::TEXT_STRONG()));
        let n = log.records().len();
        let conf = log.records().iter().filter(|r| r.confirmed()).count();
        ui.label(
            RichText::new(format!("{n} QSOs · {conf} confirmed"))
                .monospace()
                .size(11.5)
                .color(theme::TEXT()),
        );
        if let Some(p) = log.path() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.label(RichText::new(name).size(10.5).weak()).on_hover_text(format!(
                "The master file, {}: ADIF, saved after every QSO, readable by any logger",
                p.display()
            ));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pop = if popped { "DOCK" } else { "POP OUT" };
            if chrome::chip(ui, false, pop).clicked() {
                out.pop = Some(!popped);
            }
        });
    });
}

fn toolbar(
    ui: &mut egui::Ui,
    log: &mut Logbook,
    u: &mut LogUi,
    s: &LogSettings,
    st: &StationSettings,
    dial: Option<(u64, Option<&str>)>,
) {
    let nsel = u.selected.len();
    ui.horizontal_wrapped(|ui| {
        if chrome::chip(ui, u.form.as_ref().is_some_and(|f| f.base.id == 0), "+ NEW ENTRY")
            .on_hover_text("Log a QSO by hand, at the radio's frequency and the time now")
            .clicked()
        {
            let now = chrono::Utc::now().timestamp();
            let mode = match dial.and_then(|d| d.1) {
                Some("USB" | "LSB") => "SSB",
                Some(m) => m,
                None => "SSB",
            };
            u.form = Some(Form::new_at(now, dial.map(|d| d.0), mode));
        }
        if chrome::chip_enabled(ui, nsel == 1, false, "EDIT").on_hover_text("Edit the selected QSO (or double-click it)").clicked()
            && let Some(r) = u.selected.first().and_then(|id| log.get(*id))
        {
            u.form = Some(Form::of(r));
        }
        let arming = u.confirm_delete.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        let del = if arming { format!("DEL {nsel}? AGAIN") } else { "DEL".to_string() };
        if chrome::chip_enabled(ui, nsel > 0, arming, &del)
            .on_hover_text("Delete the selected QSOs: press twice")
            .clicked()
        {
            if arming {
                let ids: Vec<u64> = u.selected.iter().copied().collect();
                let n = log.delete(&ids);
                u.selected.clear();
                u.confirm_delete = None;
                u.say(format!("{n} QSOs deleted (the file before this run is kept as .adi.bak)"), false);
            } else {
                u.confirm_delete = Some(Instant::now());
            }
        }
        ui.add_space(10.0);
        if chrome::chip(ui, false, "IMPORT")
            .on_hover_text(
                "Read the ADIF file named in FILE into the log (WSJT-X, Log4OM, N1MM, QRZ…). The same \
                 call on the same band within 2 minutes of one already here is skipped",
            )
            .clicked()
        {
            let f = u.file.trim().trim_matches('"').to_string();
            if f.is_empty() {
                u.say("Type the file to import in the FILE box, or drop it on this window", true);
            } else {
                import(u, log, PathBuf::from(f));
            }
        }
        let what = if nsel > 0 { format!("ADIF ({nsel})") } else { "ADIF".to_string() };
        if chrome::chip_enabled(ui, !log.records().is_empty(), false, &what)
            .on_hover_text("Export the selected QSOs, or the whole log, as ADIF to the FILE box's file")
            .clicked()
        {
            export(u, log);
        }
        ui.add_space(10.0);
        if let Some(b) = &u.batch {
            let label = format!("STOP {}/{}", b.done, b.total);
            if chrome::chip(ui, true, label).on_hover_text("Stop FILL MISSING's lookups").clicked() {
                b.stop();
                u.batch = None;
                u.say("FILL MISSING stopped", false);
            }
        } else if chrome::chip(ui, false, "FILL MISSING")
            .on_hover_text(format!(
                "Fill every QSO's empty name, QTH, state, county, grid, zones, DXCC and country: \
                 country, zones and continent from cty.dat at once, the rest from {}, one call at a \
                 time. What you entered is never changed",
                s.lookup.label()
            ))
            .clicked()
        {
            fill_missing(u, log, s);
        }
        ui.add_space(10.0);
        let send_what = if nsel > 0 { format!("{nsel} selected") } else { "every QSO not sent yet".into() };
        let eqsl_ok = s.eqsl.is_set() && u.eqsl.is_none();
        let r = chrome::chip_enabled(ui, eqsl_ok, false, "eQSL ↑");
        let r = if s.eqsl.is_set() {
            r.on_hover_text(format!("Send {send_what} to eQSL"))
        } else {
            r.on_disabled_hover_text("Set your eQSL login in SETTINGS → Logbook")
        };
        if r.clicked() {
            start_eqsl(u, log, s);
        }
        let tqsl = u.tqsl(s);
        let r = chrome::chip_enabled(ui, tqsl.is_ok() && u.lotw.is_none(), false, "LoTW ↑");
        let r = match &tqsl {
            Ok(p) => r.on_hover_text(format!("Sign and upload {send_what} with TQSL ({})", p.display())),
            Err(why) => r.on_disabled_hover_text(why),
        };
        if r.clicked()
            && let Ok(p) = tqsl
        {
            start_lotw(u, log, s, p);
        }
        if let Err(why) = &u.tqsl(s) {
            ui.add(egui::Label::new(RichText::new("no TQSL").size(10.5).weak()).wrap_mode(egui::TextWrapMode::Extend))
                .on_hover_text(why);
        }
        let can = (s.eqsl.is_set() || s.lotw.is_set()) && u.confirms.is_none();
        if chrome::chip_enabled(ui, can, false, "QSL ✔")
            .on_hover_text("Download the confirmations from eQSL and LoTW, and mark the QSOs ✔")
            .on_disabled_hover_text("Set your eQSL or LoTW website login in SETTINGS → Logbook")
            .clicked()
        {
            start_confirms(u, s);
        }
        let _ = st;
    });
}

fn form(
    ui: &mut egui::Ui,
    log: &mut Logbook,
    u: &mut LogUi,
    s: &LogSettings,
    st: &StationSettings,
) {
    let Some(f) = u.form.as_mut() else { return };
    let mut save = false;
    let mut cancel = false;
    let mut look = None;
    let band = f.band();
    let call = f.call.trim().to_ascii_uppercase();
    let before: Vec<&QsoRecord> = log
        .records()
        .iter()
        .filter(|r| r.call == call && r.id != f.base.id && !call.is_empty())
        .collect();
    egui::Frame::new()
        .fill(theme::ROW_BG())
        .stroke(egui::Stroke::new(1.0, theme::LINE_LIT()))
        .inner_margin(egui::Margin::same(9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    caps(if f.base.id == 0 { "NEW QSO" } else { "EDIT QSO" }).color(theme::CYAN()),
                );
                if let Some(last) = before.iter().max_by_key(|r| r.start_utc) {
                    let on_band = before.iter().any(|r| r.band == band);
                    let (d, t) = date_text(last.start_utc);
                    badge(ui, "WORKED BEFORE", theme::CYAN()).on_hover_text(format!(
                        "{} QSOs with {call}{}; the last {d} {t}Z on {} {}",
                        before.len(),
                        if on_band { format!(", on {band} too") } else { String::new() },
                        last.band,
                        last.mode_shown()
                    ));
                }
                if let Some(e) = crate::dxcc::lookup(&call) {
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · CQ {} · ITU {}",
                            e.name, e.continent, e.cq_zone, e.itu_zone
                        ))
                        .size(11.0)
                        .color(theme::TEXT()),
                    );
                }
            });
            ui.add_space(4.0);
            let lbl = |ui: &mut egui::Ui, t: &str| {
                ui.add_sized([64.0, 20.0], egui::Label::new(RichText::new(t).size(11.5)));
            };
            let field = |ui: &mut egui::Ui, w: f32, v: &mut String| {
                chrome::field(ui, egui::TextEdit::singleline(v).desired_width(w))
            };
            ui.horizontal_wrapped(|ui| {
                lbl(ui, "Call");
                let r = field(ui, 120.0, &mut f.call);
                if r.changed() {
                    f.call = f.call.to_ascii_uppercase();
                }
                let can_look = !f.call.trim().is_empty() && u.form_lookup.is_none();
                if chrome::chip_enabled(ui, can_look, false, "LOOKUP")
                    .on_hover_text(format!("Fill the empty fields from {}", s.lookup.label()))
                    .clicked()
                {
                    look = Some(f.call.trim().to_string());
                }
                lbl(ui, "Grid");
                field(ui, 80.0, &mut f.grid);
                lbl(ui, "Freq MHz");
                field(ui, 100.0, &mut f.freq_mhz);
                ui.label(RichText::new(&band).size(11.0).color(theme::TEXT()));
                lbl(ui, "Mode");
                field(ui, 60.0, &mut f.mode);
            });
            ui.horizontal_wrapped(|ui| {
                lbl(ui, "Sent");
                field(ui, 56.0, &mut f.rst_sent);
                lbl(ui, "Rcvd");
                field(ui, 56.0, &mut f.rst_rcvd);
                lbl(ui, "Date UTC");
                field(ui, 96.0, &mut f.date);
                lbl(ui, "Time");
                field(ui, 56.0, &mut f.time);
                if chrome::chip(ui, false, "NOW")
                    .on_hover_text("The date and time now, UTC")
                    .clicked()
                {
                    (f.date, f.time) = date_text(chrono::Utc::now().timestamp());
                }
                lbl(ui, "Power W");
                field(ui, 50.0, &mut f.pwr);
            });
            ui.horizontal_wrapped(|ui| {
                lbl(ui, "Name");
                field(ui, 150.0, &mut f.name);
                lbl(ui, "QTH");
                field(ui, 150.0, &mut f.qth);
                lbl(ui, "State");
                field(ui, 40.0, &mut f.state);
                lbl(ui, "Country");
                field(ui, 150.0, &mut f.country);
            });
            ui.horizontal_wrapped(|ui| {
                lbl(ui, "Comment");
                field(ui, 520.0, &mut f.comment);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if chrome::chip_accent(
                    ui,
                    false,
                    RichText::new(" SAVE ").strong(),
                    theme::GREEN(),
                    theme::INK_ON_CYAN(),
                )
                .clicked()
                {
                    save = true;
                }
                if chrome::chip(ui, false, "CANCEL").clicked() {
                    cancel = true;
                }
                if let Some(p) = &f.problem {
                    ui.label(RichText::new(p).size(11.5).color(theme::YELLOW()));
                }
            });
        });
    if let Some(c) = look {
        form_lookup(u, s, &c);
    }
    if cancel {
        u.form = None;
    } else if save && let Some(f) = u.form.as_mut() {
        match f.record(st) {
            Ok(r) if r.id == 0 => {
                let call = r.call.clone();
                let id = log.add(r);
                u.form = None;
                u.selected = BTreeSet::from([id]);
                u.say(format!("{call} logged"), false);
            }
            Ok(r) => {
                let call = r.call.clone();
                log.update(r);
                u.form = None;
                u.say(format!("{call} saved"), false);
            }
            Err(e) => f.problem = Some(e),
        }
    }
}

/// A small filled label.
fn badge(ui: &mut egui::Ui, text: &str, col: Color32) -> egui::Response {
    let font = FontId::proportional(10.0);
    let w = chrome::text_width(ui, text, font.clone()) + 10.0;
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    paint_badge(ui.painter(), rect, text, col);
    r
}

fn paint_badge(p: &egui::Painter, rect: egui::Rect, text: &str, col: Color32) {
    p.rect_filled(rect, 2.0, col.gamma_multiply(0.25));
    p.rect_stroke(rect, 2.0, egui::Stroke::new(1.0, col), egui::StrokeKind::Inside);
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.0), col);
}

/// The columns, from the row's left edge: (heading, x).
const COLS: [(&str, f32); 12] = [
    ("UTC", 6.0),
    ("CALL", 56.0),
    ("", 150.0),
    ("BAND", 186.0),
    ("MODE", 230.0),
    ("FREQ", 280.0),
    ("SENT", 352.0),
    ("RCVD", 396.0),
    ("GRID", 440.0),
    ("NAME", 500.0),
    ("COUNTRY", 640.0),
    ("", 0.0),
];

/// The QSL column's width, at the right.
const QSL_W: f32 = 60.0;

/// Where COUNTRY starts in a row `w` wide: after NAME, which gives up room to
/// it when the window is narrow.
fn country_x(w: f32) -> f32 {
    (w - QSL_W - 130.0).clamp(COLS[9].1 + 70.0, COLS[10].1)
}

fn list(ui: &mut egui::Ui, log: &mut Logbook, u: &mut LogUi) {
    u.refresh(log);
    let font = FontId::monospace(12.5);
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 7.0;
    // The headings.
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    let qsl_x = rect.right() - QSL_W;
    let heads = COLS[..COLS.len() - 1]
        .iter()
        .map(|&(h, x)| {
            let x = if h == "COUNTRY" { country_x(rect.width()) } else { x };
            (h, rect.left() + x, Align2::LEFT_CENTER)
        })
        .chain([
            ("L", qsl_x + 28.0, Align2::CENTER_CENTER),
            ("E", qsl_x + 44.0, Align2::CENTER_CENTER),
        ]);
    for (h, x, align) in heads {
        let r = ui.painter().text(
            egui::pos2(x, rect.center().y),
            align,
            h,
            FontId::proportional(10.5),
            theme::CYAN_DIM(),
        );
        if h.len() == 1 {
            let tip = if h == "L" { "LoTW" } else { "eQSL" };
            ui.interact(r.expand(3.0), ui.id().with(("log-qsl-head", h)), Sense::hover())
                .on_hover_text(format!("{tip}: ↑ sent, ✔ confirmed"));
        }
    }
    let mut clicked: Option<(u64, egui::Modifiers)> = None;
    let mut edit: Option<u64> = None;
    let recs = log.records();
    let items = &u.items;
    let selected = &u.selected;
    egui::ScrollArea::vertical().id_salt("log-list").auto_shrink(false).show_rows(
        ui,
        row_h,
        items.len(),
        |ui, range| {
            for it in &items[range] {
                let w = ui.available_width();
                match *it {
                    Item::Day { start, count } => {
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::hover());
                        let (d, _) = date_text(start);
                        ui.painter().text(
                            rect.left_center() + Vec2::new(6.0, 2.0),
                            Align2::LEFT_CENTER,
                            format!("{d}  ·  {count} QSO{}", if count == 1 { "" } else { "s" }),
                            FontId::proportional(11.5),
                            theme::CYAN(),
                        );
                        ui.painter().hline(
                            rect.x_range(),
                            rect.bottom() - 1.0,
                            egui::Stroke::new(1.0, theme::LINE()),
                        );
                    }
                    Item::Qso { idx, earlier, .. } => {
                        let r = &recs[idx];
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(w, row_h), Sense::click());
                        let sel = selected.contains(&r.id);
                        let p = ui.painter();
                        if sel {
                            p.rect_filled(rect, 0.0, theme::FILL_ACTIVE());
                        } else if resp.hovered() {
                            p.rect_filled(rect, 0.0, theme::ROW_HOVER());
                        }
                        let text = |x: f32, s: &str, col: Color32| {
                            p.text(
                                egui::pos2(rect.left() + x, rect.center().y),
                                Align2::LEFT_CENTER,
                                s,
                                font.clone(),
                                col,
                            );
                        };
                        let (_, t) = date_text(r.start_utc);
                        let dim = theme::TEXT();
                        let strong = theme::TEXT_STRONG();
                        text(COLS[0].1, &t, dim);
                        text(COLS[1].1, &r.call, strong);
                        text(COLS[3].1, &r.band, dim);
                        text(COLS[4].1, r.mode_shown(), dim);
                        if r.freq_hz > 0.0 {
                            text(COLS[5].1, &format!("{:.3}", r.freq_hz / 1e6), dim);
                        }
                        text(COLS[6].1, &r.rst_sent, dim);
                        text(COLS[7].1, &r.rst_rcvd, dim);
                        text(COLS[8].1, &r.grid, dim);
                        let cx = country_x(rect.width());
                        let fit = |s: &str, w: f32| {
                            s.chars().take((w / 7.5).max(0.0) as usize).collect::<String>()
                        };
                        text(COLS[9].1, &fit(&r.name, cx - COLS[9].1 - 8.0), dim);
                        text(cx, &fit(&r.country, rect.width() - QSL_W - cx), dim);
                        // Worked before this QSO.
                        if earlier > 0 {
                            let b = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + COLS[2].1, rect.center().y - 7.0),
                                Vec2::new(26.0, 14.0),
                            );
                            paint_badge(p, b, "WB", theme::CYAN());
                        }
                        // ↑ sent, ✔ confirmed, for LoTW and eQSL.
                        for (k, q) in [&r.lotw, &r.eqsl].into_iter().enumerate() {
                            let (g, col) = if q.rcvd {
                                ("✔", theme::GREEN())
                            } else if q.sent {
                                ("↑", theme::CYAN())
                            } else {
                                ("·", theme::TEXT())
                            };
                            p.text(
                                egui::pos2(qsl_x + 28.0 + k as f32 * 16.0, rect.center().y),
                                Align2::CENTER_CENTER,
                                g,
                                FontId::proportional(13.0),
                                col,
                            );
                        }
                        let resp = resp.on_hover_text(hover(r, earlier));
                        if resp.double_clicked() {
                            edit = Some(r.id);
                        } else if resp.clicked() {
                            clicked = Some((r.id, ui.input(|i| i.modifiers)));
                        }
                    }
                }
            }
            if items.is_empty() {
                ui.label(RichText::new("No QSOs yet.").weak());
            }
        },
    );
    if let Some((id, m)) = clicked {
        select(u, id, m);
    }
    if let Some(id) = edit
        && let Some(r) = log.get(id)
    {
        u.form = Some(Form::of(r));
        u.selected = BTreeSet::from([id]);
    }
}

/// A row clicked: select it alone, or with Ctrl add or remove it, or with
/// Shift take the run from the last one clicked.
fn select(u: &mut LogUi, id: u64, m: egui::Modifiers) {
    u.confirm_delete = None;
    let pos =
        |want: u64| u.items.iter().position(|it| matches!(it, Item::Qso { id, .. } if *id == want));
    if m.shift
        && let Some(a) = u.anchor
        && let (Some(p0), Some(p1)) = (pos(a), pos(id))
    {
        let (lo, hi) = (p0.min(p1), p0.max(p1));
        let run: Vec<u64> = u.items[lo..=hi]
            .iter()
            .filter_map(|it| match it {
                Item::Qso { id, .. } => Some(*id),
                Item::Day { .. } => None,
            })
            .collect();
        u.selected.extend(run);
        return;
    }
    if m.command || m.ctrl {
        if !u.selected.remove(&id) {
            u.selected.insert(id);
        }
    } else if u.selected.len() == 1 && u.selected.contains(&id) {
        u.selected.clear();
    } else {
        u.selected = BTreeSet::from([id]);
    }
    u.anchor = Some(id);
}

fn hover(r: &QsoRecord, earlier: usize) -> String {
    let (d, t) = date_text(r.start_utc);
    let (_, te) = date_text(r.end_utc);
    let mut s = format!("{} · {d} {t}–{te}Z · {} {}", r.call, r.band, r.mode_shown());
    let mut add = |label: &str, v: &str| {
        if !v.trim().is_empty() {
            s.push_str(&format!("\n{label}: {v}"));
        }
    };
    add("Name", &r.name);
    add("QTH", &r.qth);
    add("State", &r.state);
    add("County", &r.county);
    add("Country", &r.country);
    let zones = match (r.cq_zone, r.itu_zone) {
        (Some(c), Some(i)) => format!("CQ {c} · ITU {i}"),
        (Some(c), None) => format!("CQ {c}"),
        (None, Some(i)) => format!("ITU {i}"),
        _ => String::new(),
    };
    add("Zones", &zones);
    add("DXCC", &r.dxcc.map(|d| d.to_string()).unwrap_or_default());
    add("Power", &r.tx_pwr_w.map(|p| format!("{p} W")).unwrap_or_default());
    add("Comment", &r.comment);
    let qsl = |q: &super::Qsl| match (q.sent, q.rcvd) {
        (_, true) => format!("✔ confirmed {}", q.rcvd_on),
        (true, false) => format!("↑ sent {}", q.sent_on),
        _ => "not sent".into(),
    };
    s.push_str(&format!("\nLoTW: {}\neQSL: {}", qsl(&r.lotw), qsl(&r.eqsl)));
    if earlier > 0 {
        s.push_str(&format!(
            "\nWorked before: {earlier} earlier QSO{}",
            if earlier == 1 { "" } else { "s" }
        ));
    }
    s
}

/// SETTINGS → Logbook: the lookup service, the eQSL and LoTW logins, and
/// TQSL.
pub(crate) fn settings_ui(ui: &mut egui::Ui, s: &mut LogSettings) {
    ui.label(RichText::new("Logbook").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "The LOG keeps every QSO in hardrock-panel-log.adi beside these settings. The logins \
             below are only used when you press FILL MISSING, LOOKUP, eQSL, LoTW or QSL ✔ in LOG. \
             They are kept in the panel's settings file as plain text, and never written to its \
             log.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    chrome::checkbox(ui, &mut s.auto_log, "Log a finished QSO without asking").on_hover_text(
        "Off: a card asks before each QSO is logged, as WSJT-X's \"Prompt me to log QSO\"",
    );
    ui.add_space(6.0);
    let login = |ui: &mut egui::Ui, id: &str, c: &mut Credentials| {
        ui.horizontal(|ui| {
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut c.user).desired_width(110.0).hint_text("user"),
            );
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut c.password)
                    .password(true)
                    .desired_width(110.0)
                    .hint_text("password"),
            )
            .on_hover_text("Kept in the settings file as plain text");
            let _ = id;
        });
    };
    egui::Grid::new("log-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Lookup");
        ui.horizontal(|ui| {
            if chrome::chip(ui, s.lookup == LookupService::HamQth, "HamQTH").clicked() {
                s.lookup = LookupService::HamQth;
            }
            let r = chrome::chip_enabled(ui, s.qrz_enabled, s.lookup == LookupService::Qrz, "QRZ.com XML");
            if r.on_disabled_hover_text("Tick QRZ.com XML below once you have the subscription").clicked() {
                s.lookup = LookupService::Qrz;
            }
        });
        ui.end_row();
        ui.label("HamQTH");
        login(ui, "hamqth", &mut s.hamqth);
        ui.end_row();
        ui.label("QRZ.com");
        ui.vertical(|ui| {
            if chrome::checkbox(ui, &mut s.qrz_enabled, "I have a QRZ.com XML subscription").changed()
                && !s.qrz_enabled
            {
                s.lookup = LookupService::HamQth;
            }
            if s.qrz_enabled {
                login(ui, "qrz", &mut s.qrz);
            }
        });
        ui.end_row();
        ui.label("eQSL");
        ui.vertical(|ui| {
            login(ui, "eqsl", &mut s.eqsl);
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut s.eqsl_qth).desired_width(226.0).hint_text("QTH nickname (if several)"),
            );
        });
        ui.end_row();
        ui.label("LoTW website");
        login(ui, "lotw", &mut s.lotw);
        ui.end_row();
        ui.label("");
        ui.label(
            RichText::new("Only for downloading confirmations. Uploads are signed by TQSL with your certificate.")
                .size(11.0)
                .weak(),
        );
        ui.end_row();
        ui.label("TQSL");
        ui.vertical(|ui| {
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut s.tqsl_path).desired_width(300.0).hint_text("found by itself if installed"),
            );
            chrome::field(
                ui,
                egui::TextEdit::singleline(&mut s.tqsl_location)
                    .desired_width(300.0)
                    .hint_text("station location (as named in TQSL)"),
            );
            match upload::find_tqsl(&s.tqsl_path) {
                Ok(p) => ui.label(RichText::new(format!("Found: {}", p.display())).size(11.0).color(theme::CYAN())),
                Err(e) => ui.label(RichText::new(e).size(11.0).weak()),
            };
        });
        ui.end_row();
    });
}
