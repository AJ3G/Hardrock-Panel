//! The LOG window: the QSOs a day at a time, newest first, with + NEW ENTRY,
//! EDIT and DEL, IMPORT and ADIF export, FILL MISSING, the eQSL and LoTW
//! uploads and their confirmations, and each QSO's QSL state (↑ sent, ✔
//! confirmed). Docked over the panel or popped out, like GLOBE and DECODE.
//! Also SETTINGS → Logbook.
//!
//! After SDRoxide's logbook window (`sdroxide-ui/src/app/logbook.rs`,
//! https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Vec2};

use super::lookup::{self, Batch, FillBatch, FillMsg, Msg, Plan, Source, Want};
use super::{
    Credentials, LogSettings, Logbook, LookupService, QsoRecord, Service, adif, adif_band,
    adif_today, upload,
};
use crate::station::{Heard, StationSettings};
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
    /// FILL MISSING, while its lookups run.
    fill: Option<FillRun>,
    /// When the log's grid-less calls were last looked for among the
    /// stations decoded ([`LogUi::heard_again`]).
    heard_at: Option<Instant>,
    /// The calls with a QSO that has no grid, for the log generation.
    gridless: Option<(u64, Vec<String>)>,
    /// The .adi files offered by FILE's drop-down, found when it opens.
    found_files: Vec<PathBuf>,
    /// BROWSE…'s file dialog, open on its own thread.
    #[cfg(windows)]
    browse: Option<Receiver<Option<PathBuf>>>,
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

/// A FILL MISSING run: its lookups, and the tally for the report at the end.
struct FillRun {
    batch: FillBatch,
    tally: Tally,
}

/// What a FILL MISSING run has done so far.
#[derive(Default)]
struct Tally {
    /// QSOs given a country, zones or continent by cty.dat.
    offline: usize,
    /// Grids filled, by where they came from.
    grids: BTreeMap<Source, usize>,
    /// Sources that couldn't be used, and why.
    failed: Vec<String>,
    /// No QRZ.com or HamQTH login was set.
    no_login: bool,
}

/// How often the log looks for its grid-less calls among the stations
/// decoded: the file is saved at most this often for it.
const HEARD_EVERY: Duration = Duration::from_secs(15);

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
        self.fill.is_some()
            || self.form_lookup.is_some()
            || self.eqsl.is_some()
            || self.lotw.is_some()
            || self.confirms.is_some()
            || self.browsing()
    }

    #[cfg(windows)]
    fn browsing(&self) -> bool {
        self.browse.is_some()
    }

    #[cfg(not(windows))]
    fn browsing(&self) -> bool {
        false
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
    fn poll(&mut self, log: &mut Logbook, heard: &Heard) {
        let mut finished = false;
        if let Some(f) = &mut self.fill {
            loop {
                match f.batch.rx.try_recv() {
                    Ok(FillMsg::Call { call, found }) => {
                        f.batch.done += 1;
                        let grids = &mut f.tally.grids;
                        log.modify(|r| {
                            if r.call != call {
                                return false;
                            }
                            let mut changed = false;
                            for (src, i) in &found {
                                let had = !r.grid.trim().is_empty();
                                changed |= lookup::fill_missing(r, i);
                                if !had && !r.grid.trim().is_empty() {
                                    *grids.entry(*src).or_default() += 1;
                                }
                            }
                            changed
                        });
                    }
                    Ok(FillMsg::SourceFailed(src, e)) => {
                        f.tally.failed.push(format!("{} left out: {e}", src.label()));
                    }
                    Ok(FillMsg::Done) | Err(TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        if finished && let Some(f) = self.fill.take() {
            self.finish_fill(f, log, heard);
        }
        #[cfg(windows)]
        if let Some(rx) = &self.browse {
            match rx.try_recv() {
                Ok(p) => {
                    self.browse = None;
                    if let Some(p) = p {
                        self.file = p.display().to_string();
                    }
                }
                Err(TryRecvError::Disconnected) => self.browse = None,
                Err(TryRecvError::Empty) => {}
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

    /// FILL MISSING's end: the grids no service gave, from the log and the
    /// stations decoded, and the report.
    fn finish_fill(&mut self, mut f: FillRun, log: &mut Logbook, heard: &Heard) {
        f.batch.stop();
        for (src, n) in fill_grids_offline(log, |c| heard.get(c).map(|st| st.grid.clone())) {
            *f.tally.grids.entry(src).or_default() += n;
        }
        let missing = log.calls_without_grid();
        let (msg, bad) = fill_report(&f.tally, &missing);
        self.say(msg, bad);
    }

    /// Stations decoded sending a grid that QSOs in the log lack: fill
    /// them (empty grids only), saving once for all of them, at most every
    /// [`HEARD_EVERY`] (AJ3G: "If its missing and that station is heard again
    /// fill the data.").
    fn heard_again(&mut self, log: &mut Logbook, heard: &Heard) {
        if self.heard_at.is_some_and(|t| t.elapsed() < HEARD_EVERY) || self.fill.is_some() {
            return;
        }
        self.heard_at = Some(Instant::now());
        if self.gridless.as_ref().is_none_or(|(g, _)| *g != log.generation()) {
            self.gridless = Some((log.generation(), log.calls_without_grid()));
        }
        let Some((_, calls)) = &self.gridless else { return };
        let grids: HashMap<String, String> = calls
            .iter()
            .filter_map(|c| heard.get(c).map(|st| (c.clone(), st.grid.clone())))
            .collect();
        if grids.is_empty() {
            return;
        }
        let done = log.fill_grids(&grids);
        if let Some(m) = heard_note(&done) {
            self.say(m, false);
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
/// then the services for every call still missing something (a grid from
/// QRZ.com, HamQTH or callook.info, in that order), then the log's and the
/// decodes' grids for what they didn't give (decision 75).
fn fill_missing(ui_state: &mut LogUi, log: &mut Logbook, s: &LogSettings, heard: &Heard) {
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
    let plan = Plan::of(s);
    let wants: Vec<Want> =
        wants(log.records()).into_iter().filter(|w| w.worth_asking(&plan)).collect();
    let run = FillRun {
        tally: Tally { offline, no_login: !plan.has_login(), ..Tally::default() },
        batch: FillBatch::start(plan.clone(), wants.clone()),
    };
    if wants.is_empty() {
        ui_state.finish_fill(run, log, heard);
        return;
    }
    let first = match (&plan.qrz, &plan.hamqth) {
        (Some(_), _) => Source::Qrz,
        (None, Some(_)) => Source::HamQth,
        (None, None) => Source::Callook,
    };
    let secs = wants.len() as f32 * pace_of(first);
    ui_state.say(
        format!(
            "FILL MISSING: {offline} QSOs filled from cty.dat; looking up {} calls (about {:.0} s)",
            wants.len(),
            secs.round()
        ),
        false,
    );
    ui_state.fill = Some(run);
}

fn pace_of(s: Source) -> f32 {
    match s {
        Source::Qrz => lookup::pace(LookupService::Qrz).as_secs_f32(),
        Source::HamQth => lookup::pace(LookupService::HamQth).as_secs_f32(),
        _ => 1.0,
    }
}

/// Each call in the log that lacks something, and what.
fn wants(recs: &[QsoRecord]) -> Vec<Want> {
    let mut by: BTreeMap<&str, Want> = BTreeMap::new();
    for r in recs {
        let any = lookup::wants_lookup(r);
        let grid = r.grid.trim().is_empty() && !r.call.contains('/');
        if r.call.is_empty() || !(any || grid) {
            continue;
        }
        let w = by.entry(r.call.as_str()).or_insert_with(|| Want {
            call: r.call.clone(),
            any: false,
            grid: false,
        });
        w.any |= any;
        w.grid |= grid;
    }
    by.into_values().collect()
}

/// The grids no service gave: each grid-less call's grid elsewhere in the
/// log, then the one it was decoded with (`heard`). Returns how many QSOs
/// each filled.
fn fill_grids_offline(
    log: &mut Logbook,
    heard: impl Fn(&str) -> Option<String>,
) -> Vec<(Source, usize)> {
    let mut out = Vec::new();
    let calls = log.calls_without_grid();
    let from_log: HashMap<String, String> =
        calls.iter().filter_map(|c| log.grid_in_log(c).map(|g| (c.clone(), g))).collect();
    let n: usize = log.fill_grids(&from_log).iter().map(|d| d.2).sum();
    if n > 0 {
        out.push((Source::Log, n));
    }
    let from_heard: HashMap<String, String> =
        log.calls_without_grid().into_iter().filter_map(|c| heard(&c).map(|g| (c, g))).collect();
    let n: usize = log.fill_grids(&from_heard).iter().map(|d| d.2).sum();
    if n > 0 {
        out.push((Source::Heard, n));
    }
    out
}

/// FILL MISSING's report: grids filled and from where, the calls still
/// without one, what couldn't be used, and the login hint. And whether it is
/// a problem (nothing could be done).
fn fill_report(f: &Tally, missing: &[String]) -> (String, bool) {
    let total: usize = f.grids.values().sum();
    let mut m =
        format!("FILL MISSING: {} QSOs filled from cty.dat; {total} grids filled", f.offline);
    if total > 0 {
        let parts: Vec<String> =
            f.grids.iter().map(|(s, n)| format!("{} {n}", s.label())).collect();
        m.push_str(&format!(" ({})", parts.join(", ")));
    }
    if !missing.is_empty() {
        const SHOWN: usize = 8;
        let mut list = missing.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(" ");
        if missing.len() > SHOWN {
            list.push_str(&format!(" and {} more", missing.len() - SHOWN));
        }
        m.push_str(&format!("; no grid found for {list}"));
    }
    for e in &f.failed {
        m.push_str(&format!(". {e}"));
    }
    if f.no_login {
        m.push_str(
            ". No QRZ.com or HamQTH login: set one in SETTINGS → Logbook for grids outside the \
             US (callook.info, the log and the decodes were still used)",
        );
    }
    let bad = !f.failed.is_empty() || (f.offline == 0 && total == 0 && f.no_login);
    (m, bad)
}

/// LOG's note for grids filled from decodes: "grid FN20 for K1ABC from a
/// decode, 3 QSOs".
fn heard_note(done: &[(String, String, usize)]) -> Option<String> {
    let (c, g, n) = done.first()?;
    let qsos = if *n == 1 { "1 QSO".to_string() } else { format!("{n} QSOs") };
    let mut m = format!("grid {g} for {c} from a decode, {qsos}");
    if done.len() > 1 {
        let more: usize = done[1..].iter().map(|d| d.2).sum();
        m.push_str(&format!("; and {} more calls ({more} QSOs)", done.len() - 1));
    }
    Some(m)
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

fn import(ui_state: &mut LogUi, log: &mut Logbook, s: &mut LogSettings, path: PathBuf) {
    match log.import_file(&path) {
        Ok(rep) => {
            s.note_import(&path);
            ui_state.say(format!("Imported {}: {}", path.display(), rep.summary()), false)
        }
        Err(e) => ui_state.say(format!("Import: {e}"), true),
    }
}

/// The .adi files worth offering in FILE's drop-down, each once, the ones
/// that exist: WSJT-X's log, the files imported lately, then the .adi files
/// directly in the log's folder (the panel's exports) and the operator's
/// Documents and Downloads folders, newest first in each. Never the master
/// log itself. Looked for when the drop-down opens, not every frame.
fn adif_files(s: &LogSettings, log: &Logbook) -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut fixed = Vec::new();
    if let Some(d) = env("LOCALAPPDATA") {
        fixed.push(d.join("WSJT-X").join("wsjtx_log.adi"));
    }
    if let Some(h) = env("HOME") {
        fixed.push(h.join(".local/share/WSJT-X/wsjtx_log.adi"));
    }
    fixed.extend(s.recent_imports.iter().map(PathBuf::from));
    let mut dirs = Vec::new();
    if let Some(d) = log.path().and_then(|p| p.parent()) {
        dirs.push(d.to_path_buf());
    }
    if let Some(home) = env("USERPROFILE").or_else(|| env("HOME")) {
        dirs.push(home.join("Documents"));
        dirs.push(home.join("Downloads"));
    }
    find_adif(&fixed, &dirs, log.path())
}

/// [`adif_files`]' search: `fixed` where they exist, then each of `dirs`'
/// .adi and .adif files (not below it), newest first; leaving out `master`.
fn find_adif(
    fixed: &[PathBuf],
    dirs: &[PathBuf],
    master: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    const MOST: usize = 40;
    let is_adif = |p: &std::path::Path| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("adi") || e.eq_ignore_ascii_case("adif"))
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let add = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if Some(p.as_path()) != master && !out.contains(&p) && out.len() < MOST {
            out.push(p);
        }
    };
    for p in fixed {
        if p.is_file() {
            add(p.clone(), &mut out);
        }
    }
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut here: Vec<(std::time::SystemTime, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let m = e.metadata().ok().filter(|m| m.is_file())?;
                is_adif(&p).then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), p))
            })
            .collect();
        here.sort_by_key(|h| std::cmp::Reverse(h.0));
        for (_, p) in here {
            add(p, &mut out);
        }
    }
    out
}

/// BROWSE…: the system's file dialog, on its own thread so the panel keeps
/// drawing while it is open.
#[cfg(windows)]
fn browse(ui_state: &mut LogUi) {
    let (tx, rx) = channel();
    let start = PathBuf::from(ui_state.file.trim().trim_matches('"'));
    let spawned = std::thread::Builder::new().name("log-browse".into()).spawn(move || {
        let mut d = rfd::FileDialog::new()
            .set_title("ADIF file to import")
            .add_filter("ADIF", &["adi", "adif"])
            .add_filter("All files", &["*"]);
        if let Some(dir) = start.parent().filter(|d| d.is_dir()) {
            d = d.set_directory(dir);
        }
        let _ = tx.send(d.pick_file());
    });
    match spawned {
        Ok(_) => ui_state.browse = Some(rx),
        Err(e) => ui_state.say(format!("couldn't open the file dialog: {e}"), true),
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
/// lookups and uploads have finished, and grids the log lacks from the
/// stations decoded (`heard`). `dial` is where the radio is and its mode, for
/// + NEW ENTRY.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ctx: &egui::Context,
    open: &mut bool,
    log: &mut Logbook,
    ui_state: &mut LogUi,
    s: &mut LogSettings,
    st: &StationSettings,
    dial: Option<(u64, Option<&str>)>,
    heard: &Heard,
) -> LogOut {
    ui_state.poll(log, heard);
    ui_state.heard_again(log, heard);
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
                        body(ui, true, log, ui_state, s, st, dial, heard, &mut out);
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
                body(ui, false, log, ui_state, s, st, dial, heard, &mut out);
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
    heard: &Heard,
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
        import(u, log, s, p);
    }
    top_bar(ui, popped, log, out);
    ui.add_space(4.0);
    toolbar(ui, log, u, s, st, dial, heard);
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
        file_picker(ui, log, u, s);
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

/// FILE's drop-down of .adi files found, and (Windows) BROWSE…. Picking one
/// puts it in the FILE box (AJ3G, 11 Nov 2026).
fn file_picker(ui: &mut egui::Ui, log: &Logbook, u: &mut LogUi, s: &LogSettings) {
    let mut picked = None;
    let r = ui.menu_button("▾", |ui| {
        if u.found_files.is_empty() {
            u.found_files = adif_files(s, log);
        }
        if u.found_files.is_empty() {
            ui.label(RichText::new("No .adi files found").size(11.0).weak());
        }
        for p in &u.found_files {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
            if ui.button(RichText::new(name).size(11.5)).on_hover_text(dir).clicked() {
                picked = Some(p.clone());
                ui.close();
            }
        }
    });
    r.response.on_hover_text(
        "ADIF files found: WSJT-X's log, recent imports, the panel's exports, and those in your \
         Documents and Downloads folders",
    );
    if r.inner.is_none() {
        // Shut: look again when it next opens.
        u.found_files.clear();
    }
    #[cfg(windows)]
    if chrome::chip_enabled(ui, u.browse.is_none(), false, "BROWSE…")
        .on_hover_text("Pick the ADIF file with Windows' file dialog")
        .clicked()
    {
        browse(u);
    }
    if let Some(p) = picked {
        u.file = p.display().to_string();
    }
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
    s: &mut LogSettings,
    st: &StationSettings,
    dial: Option<(u64, Option<&str>)>,
    heard: &Heard,
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
                import(u, log, s, PathBuf::from(f));
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
        if let Some(f) = &u.fill {
            let label = format!("STOP {}/{}", f.batch.done, f.batch.total);
            if chrome::chip(ui, true, label).on_hover_text("Stop FILL MISSING's lookups").clicked()
                && let Some(f) = u.fill.take()
            {
                // What was found so far stays; the log's and the decodes'
                // grids still fill the rest.
                u.finish_fill(f, log, heard);
            }
        } else if chrome::chip(ui, false, "FILL MISSING")
            .on_hover_text(
                "Fill every QSO's empty name, QTH, state, county, grid, zones, DXCC and country: \
                 country, zones and continent from cty.dat at once, the rest from QRZ.com XML and \
                 HamQTH (with a login in SETTINGS → Logbook), one call at a time. A grid still \
                 missing comes from callook.info for a US call, then from the same call's other \
                 QSOs, then from the grid it was decoded with. Only the callsign is sent. What \
                 you entered is never changed",
            )
            .clicked()
        {
            fill_missing(u, log, s, heard);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn qso(call: &str, grid: &str, start: i64) -> QsoRecord {
        QsoRecord {
            call: call.into(),
            grid: grid.into(),
            band: "20m".into(),
            mode: "FT8".into(),
            start_utc: start,
            ..QsoRecord::default()
        }
    }

    #[test]
    fn missing_grids_come_from_the_log_then_the_decodes() {
        let mut log = Logbook::in_memory();
        log.add(qso("K1ABC", "", 1000));
        log.add(qso("K1ABC", "FN42", 500));
        log.add(qso("K1ABC", "FN43", 900));
        log.add(qso("DL1ABC", "", 1000));
        log.add(qso("DL1ABC", "", 1100));
        log.add(qso("G4XYZ", "", 1000));
        log.add(qso("W1AW/KH6", "", 1000));
        let heard = |c: &str| match c {
            "DL1ABC" => Some("JO62".to_string()),
            "K1ABC" => Some("EM00".to_string()),
            "W1AW/KH6" => Some("BL11".to_string()),
            _ => None,
        };
        let got = fill_grids_offline(&mut log, heard);
        assert_eq!(got, [(Source::Log, 1), (Source::Heard, 2)]);
        let grid = |c: &str| {
            log.records().iter().filter(|r| r.call == c).map(|r| r.grid.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            grid("K1ABC"),
            ["FN43", "FN42", "FN43"],
            "the latest in the log, before decodes"
        );
        assert_eq!(grid("DL1ABC"), ["JO62", "JO62"]);
        assert_eq!(grid("G4XYZ"), [""]);
        assert_eq!(grid("W1AW/KH6"), [""], "a portable call keeps no grid");
        assert_eq!(log.calls_without_grid(), ["G4XYZ"]);
    }

    #[test]
    fn calls_wanting_something_are_listed_once() {
        let mut full = qso("OK1RR", "JO70", 0);
        full.name = "Petr".into();
        full.qth = "Neratovice".into();
        full.country = "Czech Republic".into();
        (full.dxcc, full.cq_zone, full.itu_zone) = (Some(503), Some(15), Some(28));
        let recs = [qso("W1AW", "", 0), qso("W1AW", "FN31", 1), full, qso("DL/W1AW", "", 0)];
        let w = wants(&recs);
        assert_eq!(
            w,
            [
                Want { call: "DL/W1AW".into(), any: true, grid: false },
                Want { call: "W1AW".into(), any: true, grid: true },
            ]
        );
    }

    #[test]
    fn the_report_says_where_grids_came_from_and_what_is_missing() {
        let mut t = Tally { offline: 4, no_login: true, ..Tally::default() };
        t.grids.insert(Source::Callook, 2);
        t.grids.insert(Source::Heard, 1);
        let missing: Vec<String> = (0..10).map(|i| format!("G{i}ABC")).collect();
        let (m, bad) = fill_report(&t, &missing);
        assert!(m.contains("3 grids filled (callook.info 2, decodes 1)"), "{m}");
        assert!(m.contains("no grid found for G0ABC G1ABC"), "{m}");
        assert!(m.contains("and 2 more"), "{m}");
        assert!(m.contains("SETTINGS → Logbook"), "{m}");
        assert!(!bad);
        let (m, bad) = fill_report(&Tally { no_login: true, ..Tally::default() }, &[]);
        assert!(bad, "{m}");
        let (m, _) = fill_report(&Tally::default(), &[]);
        assert!(!m.contains("SETTINGS"), "{m}");
    }

    #[test]
    fn a_decode_note_names_the_grid_and_count() {
        assert_eq!(heard_note(&[]), None);
        let one = [("K1ABC".to_string(), "FN20".to_string(), 3)];
        assert_eq!(heard_note(&one).as_deref(), Some("grid FN20 for K1ABC from a decode, 3 QSOs"));
        let two = [one[0].clone(), ("G4XYZ".into(), "IO91".into(), 1)];
        assert_eq!(
            heard_note(&two).as_deref(),
            Some("grid FN20 for K1ABC from a decode, 3 QSOs; and 1 more calls (1 QSOs)")
        );
    }

    #[test]
    fn heard_again_fills_and_saves_once() {
        let dir = std::env::temp_dir().join(format!("hrp-log-heard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.adi");
        let mut log = Logbook::load(&path);
        log.add(qso("K1ABC", "", 1000));
        log.add(qso("K1ABC", "", 2000));
        log.add(qso("K1ABC", "FN31", 3000));
        let mut heard = Heard::default();
        heard.note("K1ABC", "FN20", 5000.0);
        heard.note("G4XYZ", "IO91", 5000.0);
        let mut u = LogUi::default();
        let gen0 = log.generation();
        u.heard_again(&mut log, &heard);
        assert_eq!(log.generation(), gen0 + 1, "one save for the lot");
        let grids: Vec<&str> = log.records().iter().map(|r| r.grid.as_str()).collect();
        assert_eq!(grids, ["FN20", "FN20", "FN31"]);
        assert_eq!(u.status.as_ref().unwrap().0, "grid FN20 for K1ABC from a decode, 2 QSOs");
        let again = Logbook::load(&path);
        assert_eq!(again.records()[0].grid, "FN20", "saved");
        // Not again until HEARD_EVERY has passed.
        log.add(qso("K1ABC", "", 4000));
        u.heard_again(&mut log, &heard);
        assert_eq!(log.records()[3].grid, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adif_files_are_found_shallowly_without_the_master() {
        let dir = std::env::temp_dir().join(format!("hrp-log-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("deeper")).unwrap();
        for f in ["a.adi", "b.ADIF", "c.txt", "master.adi", "deeper/d.adi"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let wsjtx = dir.join("deeper/d.adi");
        let master = dir.join("master.adi");
        let found = find_adif(
            &[wsjtx.clone(), dir.join("gone.adi")],
            &[dir.clone(), dir.join("nowhere")],
            Some(&master),
        );
        assert_eq!(found[0], wsjtx, "the named files first");
        let mut rest: Vec<String> =
            found[1..].iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
        rest.sort();
        assert_eq!(rest, ["a.adi", "b.ADIF"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_imports_keep_the_last_eight() {
        let mut s = LogSettings::default();
        for i in 0..10 {
            s.note_import(std::path::Path::new(&format!("/x/{i}.adi")));
        }
        s.note_import(std::path::Path::new("/x/5.adi"));
        assert_eq!(s.recent_imports.len(), 8);
        assert_eq!(s.recent_imports[0], "/x/5.adi");
        assert_eq!(s.recent_imports.iter().filter(|p| *p == "/x/5.adi").count(), 1);
    }
}
