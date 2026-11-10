// Ported from SDRoxide's crates/sdroxide-digi/src/qso.rs
// (https://github.com/dividebysandwich/sdroxide), GPL-3.0-or-later like this
// program. Its Fox/Hound and contest layouts are left out; the logging
// point follows AJ3G's rule (10 Nov 2026, below).

//! The FT8 QSO sequencer: pure and deterministic. Given the station's call
//! and grid, the message templates and each slot's decodes, it decides the
//! next message to send and follows the exchange through to the log.
//!
//! When a QSO counts (AJ3G, 10 Nov 2026: "There is a demarcation point in
//! which the QSO can be logged and it should be logged"): once both calls
//! and both reports have gone over and one side has confirmed them.
//! Answering a CQ, that is their RR73, RRR or 73 arriving after our
//! R+report; calling CQ, it is our RR73 going out after their R+report.
//! Their closing 73 is a courtesy and isn't waited for. A contact is logged
//! once, however often its final message goes out again.

use std::collections::{HashMap, HashSet, VecDeque};

use super::msg::{Decode, fmt_report, is_grid4};

/// Give up waiting for a picked station to call CQ after this long, s.
const WAIT_CQ_S: i64 = 300;
/// Keep a finished contact live this long to re-send the final message if
/// the DX repeats theirs (they didn't hear our 73 / RR73), s.
const CONFIRM_S: i64 = 300;
/// How long a call addressed to us is remembered, for where a reply the
/// operator picks by hand opens, s.
const RECENT_CALL_S: i64 = 300;

/// Where the exchange stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Step {
    #[default]
    Idle,
    /// A station picked that isn't calling CQ: hold until they call CQ or
    /// call us, so as not to jump into their exchange.
    WaitCq,
    CallingCq,
    /// Answering a CQ: our grid (Tx1).
    TxGrid,
    /// Our report (Tx2).
    TxReport,
    /// R and our report (Tx3).
    TxRReport,
    /// RR73 (Tx4).
    TxRr73,
    /// 73 (Tx5 in WSJT-X's standard set; Tx5 here is free text).
    Tx73,
    /// Logged; the final message is sent again if they repeat theirs.
    Confirming,
}

impl Step {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Step::Idle => "Idle",
            Step::WaitCq => "Wait CQ",
            Step::CallingCq => "Calling CQ",
            Step::TxGrid => "Tx Grid",
            Step::TxReport => "Tx Report",
            Step::TxRReport => "Tx R+Report",
            Step::TxRr73 => "Tx RR73",
            Step::Tx73 => "Tx 73",
            Step::Confirming => "Confirming",
        }
    }
}

/// A transcript line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub utc: i64,
    /// We sent it; else we received it, or it is a note.
    pub tx: bool,
    pub text: String,
    /// A note about the station we called working someone else, or the
    /// sequencer's own remark.
    pub note: bool,
    /// The line that says the contact is complete and logged.
    pub done: bool,
}

/// A station marked to work next.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Queued {
    pub call: String,
    pub grid: Option<String>,
    pub snr_db: i16,
    pub audio_hz: f32,
    /// When it was last heard, UTC s: a station not heard for 10 minutes is
    /// skipped.
    pub heard_utc: i64,
    /// Hold until they call CQ (they were in someone else's exchange).
    pub wait_for_cq: bool,
}

/// The sequencer's settings.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Config {
    pub my_call: String,
    pub my_grid: String,
    /// Templates with {MYCALL} {MYGRID} {DX} {REPORT}.
    pub msg_cq: String,
    pub msg_grid: String,
    pub msg_report: String,
    pub msg_rreport: String,
    pub msg_rr73: String,
    pub msg_73: String,
    /// Stop after this many minutes with no reply and no touch; 0 = never.
    pub tx_watchdog_min: u32,
    /// Give up on a station after this many unanswered calls; 0 = never.
    pub max_tx_repeats: u32,
    /// Skip Tx1: answer a CQ with the report, not the grid.
    pub skip_tx1: bool,
    /// Call 1st: adopt the first (best) station answering our CQ by itself.
    pub call_first: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            my_call: String::new(),
            my_grid: String::new(),
            msg_cq: "CQ {MYCALL} {MYGRID}".into(),
            msg_grid: "{DX} {MYCALL} {MYGRID}".into(),
            msg_report: "{DX} {MYCALL} {REPORT}".into(),
            msg_rreport: "{DX} {MYCALL} R{REPORT}".into(),
            msg_rr73: "{DX} {MYCALL} RR73".into(),
            msg_73: "{DX} {MYCALL} 73".into(),
            tx_watchdog_min: 6,
            max_tx_repeats: 10,
            skip_tx1: false,
            call_first: true,
        }
    }
}

impl Config {
    /// Fill a template.
    pub(crate) fn fill(&self, tmpl: &str, dx: &str, report: Option<i16>) -> String {
        let grid: String = self.my_grid.chars().take(4).collect::<String>().to_ascii_uppercase();
        let rpt = report.map(fmt_report).unwrap_or_default();
        tmpl.replace("{MYCALL}", &self.my_call.to_ascii_uppercase())
            .replace("{MYGRID}", &grid)
            .replace("{DX}", dx)
            .replace("{REPORT}", &rpt)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A finished contact, for the log.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Logged {
    pub call: String,
    pub grid: Option<String>,
    pub rst_sent: Option<i16>,
    pub rst_rcvd: Option<i16>,
    pub start_utc: i64,
    pub end_utc: i64,
}

/// The payload half of a message (`<to> <from> PAYLOAD`).
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    Grid(String),
    Report(i16),
    RReport(i16),
    Rrr,
    Rr73,
    B73,
    Other,
}

fn classify(text: &str) -> Payload {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let Some(p) = toks.get(2) else { return Payload::Other };
    let num = |s: &str| -> Option<i16> {
        let v: i16 = s[1..].parse().ok()?;
        Some(if s.starts_with('-') { -v } else { v })
    };
    match *p {
        "RR73" => Payload::Rr73,
        "RRR" => Payload::Rrr,
        "73" => Payload::B73,
        s if (s.starts_with("R-") || s.starts_with("R+")) && s.len() > 2 => {
            num(&s[1..]).map(Payload::RReport).unwrap_or(Payload::Other)
        }
        s if (s.starts_with('-') || s.starts_with('+')) && s.len() > 1 => {
            num(s).map(Payload::Report).unwrap_or(Payload::Other)
        }
        s if is_grid4(s) => Payload::Grid(s.to_ascii_uppercase()),
        _ => Payload::Other,
    }
}

/// The message that answers what they just sent; `None` for a bare 73.
fn reply_step(p: &Payload) -> Option<Step> {
    match p {
        Payload::Grid(_) | Payload::Other => Some(Step::TxReport),
        Payload::Report(_) => Some(Step::TxRReport),
        Payload::RReport(_) => Some(Step::TxRr73),
        Payload::Rrr | Payload::Rr73 => Some(Step::Tx73),
        Payload::B73 => None,
    }
}

/// A station calling us, not one signing off.
fn is_call_to_us(d: &Decode, me: &str) -> bool {
    d.to.as_deref() == Some(me)
        && d.from.as_deref().is_some_and(|f| !f.is_empty())
        && !matches!(classify(&d.message), Payload::B73 | Payload::Rrr | Payload::Rr73)
}

#[derive(Debug, Clone)]
struct Dx {
    call: String,
    grid: Option<String>,
    rpt_sent: Option<i16>,
    rpt_rcvd: Option<i16>,
    started_utc: i64,
}

/// The sequencer.
pub(crate) struct Qso {
    pub cfg: Config,
    step: Step,
    dx: Option<Dx>,
    transcript: Vec<Line>,
    completed: VecDeque<Logged>,
    deadline_utc: i64,
    final_msg: Option<String>,
    resend: bool,
    logged: bool,
    /// A message picked or typed by hand for the next slot; the exchange
    /// carries on after it.
    manual: Option<String>,
    /// A sign-off (RR73 or 73) picked by hand: held, not undone by the DX
    /// repeating their report.
    manual_signoff: Option<Step>,
    worked_calls: HashSet<String>,
    worked_entities: HashSet<String>,
    progress_utc: i64,
    tx_since_progress: u32,
    watchdog: bool,
    queue: VecDeque<Queued>,
    called_us: HashMap<String, (i64, Payload)>,
    /// The last time each station was heard, and what it last sent: for the
    /// queue (skip a station gone quiet) and "also calling".
    heard: HashMap<String, i64>,
    /// Stations that answered our CQ while we took another.
    also_calling: Vec<String>,
}

impl Qso {
    pub(crate) fn new(cfg: Config) -> Qso {
        Qso {
            cfg,
            step: Step::Idle,
            dx: None,
            transcript: Vec::new(),
            completed: VecDeque::new(),
            deadline_utc: 0,
            final_msg: None,
            resend: false,
            logged: false,
            manual: None,
            manual_signoff: None,
            worked_calls: HashSet::new(),
            worked_entities: HashSet::new(),
            progress_utc: 0,
            tx_since_progress: 0,
            watchdog: false,
            queue: VecDeque::new(),
            called_us: HashMap::new(),
            heard: HashMap::new(),
            also_calling: Vec::new(),
        }
    }

    pub(crate) fn step(&self) -> Step {
        self.step
    }

    pub(crate) fn dx_call(&self) -> Option<&str> {
        self.dx.as_ref().map(|d| d.call.as_str())
    }

    pub(crate) fn dx_grid(&self) -> Option<&str> {
        self.dx.as_ref().and_then(|d| d.grid.as_deref())
    }

    /// The reports: what we send them, what they sent us.
    pub(crate) fn reports(&self) -> (Option<i16>, Option<i16>) {
        self.dx.as_ref().map_or((None, None), |d| (d.rpt_sent, d.rpt_rcvd))
    }

    /// Set the report we send (the Report box).
    pub(crate) fn set_report(&mut self, db: i16) {
        if let Some(d) = self.dx.as_mut() {
            d.rpt_sent = Some(db);
        }
    }

    pub(crate) fn transcript(&self) -> &[Line] {
        &self.transcript
    }

    pub(crate) fn queue(&self) -> impl Iterator<Item = &Queued> {
        self.queue.iter()
    }

    pub(crate) fn also_calling(&self) -> &[String] {
        &self.also_calling
    }

    pub(crate) fn watchdog(&self) -> bool {
        self.watchdog
    }

    /// Unanswered calls to the current station so far.
    pub(crate) fn calls(&self) -> u32 {
        self.tx_since_progress
    }

    /// Seconds left before the watchdog stops the sequencer.
    pub(crate) fn watchdog_left(&self, now_utc: i64) -> Option<i64> {
        let limit = self.cfg.tx_watchdog_min as i64 * 60;
        (limit > 0 && self.wants_tx() && self.progress_utc > 0)
            .then(|| (limit - (now_utc - self.progress_utc)).max(0))
    }

    /// Note a contact made earlier (from the log), so a second one with the
    /// same station waits its turn when several answer our CQ.
    pub(crate) fn note_worked(&mut self, call: &str, entity: Option<&str>) {
        self.worked_calls.insert(call.to_ascii_uppercase());
        if let Some(e) = entity {
            self.worked_entities.insert(e.to_string());
        }
    }

    fn progress(&mut self) {
        self.progress_utc = 0;
        self.tx_since_progress = 0;
    }

    fn operator_acted(&mut self) {
        self.progress();
        self.watchdog = false;
    }

    fn fresh_contact(&mut self) {
        self.transcript.clear();
        self.final_msg = None;
        self.resend = false;
        self.logged = false;
        self.manual = None;
        self.manual_signoff = None;
        self.also_calling.clear();
    }

    /// CALL CQ (Tx6).
    pub(crate) fn call_cq(&mut self) {
        self.dx = None;
        self.fresh_contact();
        self.operator_acted();
        self.step = Step::CallingCq;
    }

    /// Jump the exchange to `step`, as WSJT-X's Tx1 to Tx6 do.
    pub(crate) fn set_step(&mut self, step: Step) -> bool {
        let needs_dx = !matches!(step, Step::Idle | Step::CallingCq);
        if needs_dx && self.dx.is_none() {
            return false;
        }
        if step == Step::CallingCq {
            self.call_cq();
            return true;
        }
        self.manual = None;
        self.manual_signoff = matches!(step, Step::TxRr73 | Step::Tx73).then_some(step);
        self.operator_acted();
        self.step = step;
        true
    }

    /// Send `text` as it is in the next slot (a Tx line edited by hand, or
    /// Tx5's free text), then carry on with the exchange. Empty cancels.
    pub(crate) fn queue_text(&mut self, text: &str) {
        let text = text.trim().to_ascii_uppercase();
        self.manual = (!text.is_empty()).then_some(text);
        self.operator_acted();
    }

    /// The message queued by hand, if any.
    pub(crate) fn manual(&self) -> Option<&str> {
        self.manual.as_deref()
    }

    /// Record a message as it went out.
    pub(crate) fn record_tx(&mut self, text: &str, now_utc: i64) {
        self.transcript.push(Line { utc: now_utc, tx: true, text: text.into(), note: false, done: false });
    }

    fn note(&mut self, text: String, now_utc: i64) {
        self.transcript.push(Line { utc: now_utc, tx: false, text, note: true, done: false });
    }

    fn rcvd(&mut self, text: &str, now_utc: i64) {
        self.transcript.push(Line { utc: now_utc, tx: false, text: text.into(), note: false, done: false });
    }

    /// REPLY on a decoded station. `snr` is their signal here, the report
    /// we send. With `wait_for_cq` (they are working someone else) hold in
    /// Wait CQ. A station that has lately called us is answered from where
    /// their exchange stands, not from the top.
    pub(crate) fn start_qso(&mut self, from: &str, grid: Option<String>, snr: i16, wait_for_cq: bool, now_utc: i64) {
        self.fresh_contact();
        self.operator_acted();
        let from = from.to_ascii_uppercase();
        self.queue.retain(|q| !q.call.eq_ignore_ascii_case(&from));
        let heard = self
            .called_us
            .get(&from)
            .filter(|(t, _)| now_utc - *t <= RECENT_CALL_S)
            .map(|(_, p)| p.clone());
        let rpt_rcvd = match heard {
            Some(Payload::Report(r) | Payload::RReport(r)) => Some(r),
            _ => None,
        };
        self.dx = Some(Dx { call: from, grid, rpt_sent: Some(snr), rpt_rcvd, started_utc: now_utc });
        if wait_for_cq {
            self.step = Step::WaitCq;
            self.deadline_utc = now_utc + WAIT_CQ_S;
        } else {
            let open = if self.cfg.skip_tx1 { Step::TxReport } else { Step::TxGrid };
            self.step = heard.as_ref().and_then(reply_step).unwrap_or(open);
        }
    }

    /// STOP QSO: send nothing more; the contact stays on screen.
    pub(crate) fn stop(&mut self) {
        self.step = Step::Idle;
        self.manual = None;
        self.manual_signoff = None;
    }

    /// True while there is something to send in our next slot.
    pub(crate) fn wants_tx(&self) -> bool {
        if self.cfg.my_call.trim().is_empty() {
            return false;
        }
        if self.manual.is_some() {
            return true;
        }
        match self.step {
            Step::Idle | Step::WaitCq => false,
            Step::Confirming => self.resend,
            _ => true,
        }
    }

    /// Timeouts: the watchdog, a stale Wait CQ, the end of Confirming.
    pub(crate) fn tick(&mut self, now_utc: i64) -> bool {
        if self.progress_utc == 0 {
            self.progress_utc = now_utc;
        }
        let limit = self.cfg.tx_watchdog_min as i64 * 60;
        if limit > 0 && !self.watchdog && self.wants_tx() && now_utc - self.progress_utc >= limit {
            self.watchdog = true;
            self.manual = None;
            self.manual_signoff = None;
            self.step = Step::Idle;
            self.note(format!("transmit watchdog: {} minutes with no progress", self.cfg.tx_watchdog_min), now_utc);
            return true;
        }
        match self.step {
            Step::Confirming if self.resend => false,
            Step::WaitCq | Step::Confirming if now_utc >= self.deadline_utc => {
                self.step = Step::Idle;
                if self.step == Step::Idle && !self.logged {
                    self.dx = None;
                }
                self.resend = false;
                self.final_msg = None;
                true
            }
            _ => false,
        }
    }

    /// A batch of decodes from a slot. Returns true if anything changed.
    pub(crate) fn on_rx(&mut self, decodes: &[Decode], now_utc: i64) -> bool {
        let me = self.cfg.my_call.trim().to_ascii_uppercase();
        if me.is_empty() {
            return false;
        }
        for d in decodes {
            if let Some(f) = d.from.as_deref() {
                self.heard.insert(f.to_string(), now_utc);
            }
            if let Some(from) = d.from.as_deref().filter(|_| is_call_to_us(d, &me)) {
                self.called_us.insert(from.to_string(), (now_utc, classify(&d.message)));
            }
        }
        self.called_us.retain(|_, (t, _)| now_utc - *t <= RECENT_CALL_S);
        self.heard.retain(|_, t| now_utc - *t <= 1800);
        let mut changed = false;
        let dx_call = self.dx.as_ref().map(|d| d.call.clone());
        let answered_us = dx_call.as_deref().is_some_and(|dx| {
            decodes.iter().any(|d| d.from.as_deref() == Some(dx) && d.to.as_deref() == Some(me.as_str()))
        });
        let answerer = (self.step == Step::CallingCq && self.dx.is_none() && self.cfg.call_first)
            .then(|| self.pick_answerer(decodes, &me))
            .flatten();
        if self.step == Step::CallingCq && self.dx.is_none() {
            let others: Vec<String> = decodes
                .iter()
                .filter(|d| is_call_to_us(d, &me))
                .filter_map(|d| d.from.clone())
                .filter(|c| Some(c) != answerer.as_ref())
                .collect();
            if !others.is_empty() {
                for o in &others {
                    if !self.also_calling.contains(o) {
                        self.also_calling.push(o.clone());
                    }
                }
                let text = if answerer.is_some() {
                    format!("also calling: {}", others.join(", "))
                } else {
                    format!("calling you: {} (Call 1st is off: pick one)", others.join(", "))
                };
                self.note(text, now_utc);
                changed = true;
            }
        }
        for d in decodes {
            let Some(from) = d.from.clone().filter(|f| !f.is_empty()) else { continue };
            let to_me = d.to.as_deref() == Some(me.as_str());
            let other = (!to_me && !answered_us && !d.is_cq && dx_call.as_deref() == Some(from.as_str()))
                .then(|| d.to.clone().filter(|t| !t.is_empty() && *t != me))
                .flatten();

            if self.step == Step::WaitCq {
                if dx_call.as_deref() == Some(from.as_str()) && (d.is_cq || to_me) {
                    let payload = classify(&d.message);
                    let open = if self.cfg.skip_tx1 { Step::TxReport } else { Step::TxGrid };
                    let Some(resume) = (if d.is_cq { Some(open) } else { reply_step(&payload) }) else {
                        continue;
                    };
                    if let Some(dx) = self.dx.as_mut()
                        && dx.grid.is_none()
                    {
                        dx.grid = d.grid.clone();
                    }
                    if let Payload::Report(r) | Payload::RReport(r) = payload {
                        self.set_rcvd(r);
                    }
                    self.progress();
                    self.progress_utc = now_utc;
                    self.step = resume;
                    changed = true;
                } else if let Some(other) = other {
                    changed |= self.note_working(&from, &other, now_utc);
                }
                continue;
            }

            // They took someone else's call: hold until they are free,
            // rather than double into their exchange.
            if let Some(other) = other
                && matches!(self.step, Step::TxGrid | Step::TxReport | Step::TxRReport)
            {
                self.note_working(&from, &other, now_utc);
                self.step = Step::WaitCq;
                self.deadline_utc = now_utc + WAIT_CQ_S;
                changed = true;
                continue;
            }

            if !to_me {
                continue;
            }
            let payload = classify(&d.message);

            if self.step == Step::Confirming {
                if dx_call.as_deref() == Some(from.as_str()) {
                    self.rcvd(&d.message, now_utc);
                    if !matches!(payload, Payload::B73) {
                        self.resend = true;
                    }
                    changed = true;
                }
                continue;
            }

            if self.step == Step::CallingCq && self.dx.is_none() && answerer.as_deref() == Some(from.as_str()) {
                let grid = match &payload {
                    Payload::Grid(g) => Some(g.clone()),
                    _ => d.grid.clone(),
                };
                self.dx = Some(Dx { call: from.clone(), grid, rpt_sent: Some(d.snr_db), rpt_rcvd: None, started_utc: now_utc });
                self.logged = false;
                self.also_calling.retain(|c| *c != from);
                self.rcvd(&d.message, now_utc);
                self.progress();
                self.progress_utc = now_utc;
                changed |= self.advance(&payload, now_utc);
                if self.step == Step::CallingCq {
                    self.step = Step::TxReport;
                    changed = true;
                }
                continue;
            }

            if dx_call.as_deref() != Some(from.as_str()) {
                continue;
            }
            if let Some(dx) = self.dx.as_mut() {
                if dx.grid.is_none()
                    && let Payload::Grid(g) = &payload
                {
                    dx.grid = Some(g.clone());
                }
                if dx.rpt_sent.is_none() {
                    dx.rpt_sent = Some(d.snr_db);
                }
            }
            self.rcvd(&d.message, now_utc);
            self.progress();
            self.progress_utc = now_utc;
            changed |= self.advance(&payload, now_utc);
        }
        changed
    }

    /// Which of the stations answering our CQ to work first: one not worked
    /// this session, then (among signals within 6 dB) a new entity, then the
    /// strongest.
    fn pick_answerer(&self, decodes: &[Decode], me: &str) -> Option<String> {
        decodes
            .iter()
            .filter(|d| is_call_to_us(d, me))
            .filter_map(|d| Some((d.from.as_deref().filter(|f| !f.is_empty())?, d.snr_db)))
            .max_by_key(|(call, snr)| {
                let fresh = !self.worked_calls.contains(*call);
                let new_entity = crate::dxcc::lookup(call).is_some_and(|e| !self.worked_entities.contains(&e.name));
                (fresh, snr.div_euclid(6), new_entity, *snr)
            })
            .map(|(c, _)| c.to_string())
    }

    fn note_working(&mut self, dx: &str, other: &str, now_utc: i64) -> bool {
        let text = format!("{dx} is working {other}");
        if self.transcript.last().is_some_and(|l| l.note && l.text == text) {
            return false;
        }
        self.note(text, now_utc);
        true
    }

    fn advance(&mut self, payload: &Payload, now_utc: i64) -> bool {
        if self.step == Step::Idle {
            return false;
        }
        if let Payload::Report(r) | Payload::RReport(r) = payload {
            self.set_rcvd(*r);
        }
        // Answering a CQ: their RR73, RRR or 73 after our R+report is the
        // point the contact counts. It is logged now; our 73 still goes out.
        if matches!(payload, Payload::Rrr | Payload::Rr73 | Payload::B73)
            && matches!(self.step, Step::TxRReport | Step::Tx73 | Step::TxRr73)
            && self.dx.as_ref().is_some_and(|d| d.rpt_rcvd.is_some())
        {
            self.log_now(now_utc);
        }
        if self.manual_signoff.is_some() {
            return false;
        }
        let prev = self.step;
        match payload {
            Payload::Other => {}
            Payload::B73 => {
                if self.step == Step::TxRReport {
                    self.step = Step::Tx73;
                }
            }
            _ => {
                if let Some(s) = reply_step(payload) {
                    self.step = s;
                }
            }
        }
        self.step != prev
    }

    fn set_rcvd(&mut self, r: i16) {
        if let Some(dx) = self.dx.as_mut() {
            dx.rpt_rcvd = Some(r);
        }
    }

    /// Log the contact, once.
    fn log_now(&mut self, now_utc: i64) {
        let Some(dx) = self.dx.as_ref().filter(|_| !self.logged) else { return };
        self.worked_calls.insert(dx.call.clone());
        if let Some(e) = crate::dxcc::lookup(&dx.call) {
            self.worked_entities.insert(e.name.clone());
        }
        self.completed.push_back(Logged {
            call: dx.call.clone(),
            grid: dx.grid.clone(),
            rst_sent: dx.rpt_sent,
            rst_rcvd: dx.rpt_rcvd,
            start_utc: dx.started_utc,
            end_utc: now_utc,
        });
        let text = format!("QSO with {} complete: logged", dx.call);
        self.transcript.push(Line { utc: now_utc, tx: false, text, note: false, done: true });
        self.logged = true;
    }

    /// Enter Confirming after our final message.
    fn confirm(&mut self, now_utc: i64) {
        self.log_now(now_utc);
        self.step = Step::Confirming;
        self.manual_signoff = None;
        self.deadline_utc = now_utc + CONFIRM_S;
        self.resend = false;
    }

    /// The six standard messages for the station in hand (WSJT-X's
    /// Generate Std Msgs): Tx1 grid, Tx2 report, Tx3 R+report, Tx4 RR73,
    /// Tx5 (free text, left to the operator), Tx6 CQ.
    pub(crate) fn std_msgs(&self, rrr: bool) -> [String; 6] {
        let dx = self.dx_call().unwrap_or("DX");
        let rpt = self.dx.as_ref().and_then(|d| d.rpt_sent).or(Some(-10));
        let c = &self.cfg;
        [
            c.fill(&c.msg_grid, dx, None),
            c.fill(&c.msg_report, dx, rpt),
            c.fill(&c.msg_rreport, dx, rpt),
            if rrr { c.fill("{DX} {MYCALL} RRR", dx, None) } else { c.fill(&c.msg_rr73, dx, None) },
            c.fill(&c.msg_73, dx, None),
            c.fill(&c.msg_cq, dx, None),
        ]
    }

    /// The message for this slot, or `None` to stay off the air.
    pub(crate) fn plan_tx(&self) -> Option<String> {
        if self.cfg.my_call.trim().is_empty() {
            return None;
        }
        if let Some(m) = &self.manual {
            return Some(m.clone());
        }
        let dx = self.dx.as_ref();
        let call = dx.map(|d| d.call.as_str()).unwrap_or("");
        let rpt = dx.and_then(|d| d.rpt_sent);
        let c = &self.cfg;
        match self.step {
            Step::CallingCq => Some(c.fill(&c.msg_cq, call, None)),
            Step::TxGrid => Some(c.fill(&c.msg_grid, call, None)),
            Step::TxReport => Some(c.fill(&c.msg_report, call, rpt)),
            Step::TxRReport => Some(c.fill(&c.msg_rreport, call, rpt)),
            Step::TxRr73 => Some(c.fill(&c.msg_rr73, call, None)),
            Step::Tx73 => Some(c.fill(&c.msg_73, call, None)),
            Step::Confirming => self.resend.then(|| self.final_msg.clone()).flatten(),
            Step::Idle | Step::WaitCq => None,
        }
    }

    /// After each over has gone out.
    pub(crate) fn note_tx_sent(&mut self, now_utc: i64) {
        if let Some(m) = self.manual.take() {
            // A hand-picked line that is a sign-off closes the exchange as
            // the sequencer's own would.
            let p = classify(&m);
            if self.dx.as_ref().is_some_and(|d| m.starts_with(&format!("{} ", d.call)))
                && matches!(p, Payload::Rr73 | Payload::B73 | Payload::Rrr)
                && self.dx.as_ref().is_some_and(|d| d.rpt_rcvd.is_some())
            {
                self.final_msg = Some(m);
                self.confirm(now_utc);
            }
            return;
        }
        self.tx_since_progress += 1;
        let repeats = self.cfg.max_tx_repeats;
        if repeats > 0
            && self.tx_since_progress >= repeats
            && matches!(self.step, Step::TxGrid | Step::TxReport | Step::TxRReport)
        {
            let call = self.dx_call().unwrap_or_default().to_string();
            self.note(format!("no reply from {call} after {repeats} calls"), now_utc);
            self.step = Step::Idle;
            return;
        }
        match self.step {
            // Calling CQ: our RR73 after their R+report is when it counts.
            // Answering: our 73 follows their RR73 (already logged then).
            Step::TxRr73 | Step::Tx73 => {
                self.final_msg = self.plan_tx();
                if self.dx.as_ref().is_some_and(|d| d.rpt_rcvd.is_some()) || self.manual_signoff.is_some() {
                    self.confirm(now_utc);
                } else {
                    self.step = Step::Confirming;
                    self.deadline_utc = now_utc + CONFIRM_S;
                }
            }
            Step::Confirming => self.resend = false,
            _ => {}
        }
    }

    /// The contacts finished since last asked, for the log.
    pub(crate) fn take_completed(&mut self) -> Option<Logged> {
        self.completed.pop_front()
    }

    /// Log the contact in hand now (LOG THIS), whatever the exchange got to.
    pub(crate) fn log_by_hand(&mut self, now_utc: i64) -> bool {
        if self.dx.is_none() || self.logged {
            return false;
        }
        self.log_now(now_utc);
        true
    }

    /// + on a decode: mark a station to work. Again moves it to the end.
    pub(crate) fn queue_add(&mut self, q: Queued) {
        self.queue.retain(|x| !x.call.eq_ignore_ascii_case(&q.call));
        self.queue.push_back(q);
    }

    /// Drop a station from the queue; an empty call clears it.
    pub(crate) fn queue_remove(&mut self, call: &str) {
        if call.trim().is_empty() {
            self.queue.clear();
        } else {
            self.queue.retain(|q| !q.call.eq_ignore_ascii_case(call.trim()));
        }
    }

    /// REPLY while a contact is under way: put the station at the head of
    /// the queue rather than abandon the one in hand.
    pub(crate) fn queue_front(&mut self, q: Queued) {
        self.queue.retain(|x| !x.call.eq_ignore_ascii_case(&q.call));
        self.queue.push_front(q);
    }

    /// True while a contact is under way (REPLY then queues).
    pub(crate) fn busy(&self) -> bool {
        self.dx.is_some() && !matches!(self.step, Step::Idle | Step::Confirming)
    }

    /// The next queued station to call, once free. Stations not heard for
    /// 10 minutes, and any `dupe` says, are skipped with a note.
    pub(crate) fn take_next_queued(&mut self, now_utc: i64, dupe: impl Fn(&str) -> bool) -> Option<Queued> {
        if self.manual.is_some() || self.watchdog {
            return None;
        }
        let free = match self.step {
            Step::Idle => true,
            Step::CallingCq => self.dx.is_none(),
            Step::Confirming => !self.resend,
            _ => false,
        };
        if !free {
            return None;
        }
        while let Some(q) = self.queue.pop_front() {
            let last = self.heard.get(&q.call).copied().unwrap_or(q.heard_utc).max(q.heard_utc);
            if now_utc - last > 600 {
                self.note(format!("{} skipped: not heard for 10 minutes", q.call), now_utc);
                continue;
            }
            if dupe(&q.call) {
                self.note(format!("{} skipped: already in the log on this band", q.call), now_utc);
                continue;
            }
            return Some(q);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::msg::parse;

    fn cfg() -> Config {
        Config { my_call: "AJ3G".into(), my_grid: "FN20".into(), ..Default::default() }
    }

    fn d(msg: &str) -> Decode {
        parse(msg, 0, -10, 0.1, 1500.0)
    }

    #[test]
    fn answering_a_cq_logs_on_their_rr73_and_still_sends_73() {
        let mut q = Qso::new(cfg());
        q.start_qso("JA1ABC", Some("PM95".into()), -14, false, 0);
        assert_eq!(q.plan_tx().as_deref(), Some("JA1ABC AJ3G FN20"));
        q.note_tx_sent(15);
        assert!(q.on_rx(&[d("AJ3G JA1ABC -10")], 30));
        assert_eq!(q.plan_tx().as_deref(), Some("JA1ABC AJ3G R-14"));
        q.note_tx_sent(45);
        assert!(q.take_completed().is_none(), "not before they confirm");
        q.on_rx(&[d("AJ3G JA1ABC RR73")], 60);
        let l = q.take_completed().expect("logged on their RR73");
        assert_eq!((l.call.as_str(), l.rst_sent, l.rst_rcvd), ("JA1ABC", Some(-14), Some(-10)));
        assert_eq!(q.plan_tx().as_deref(), Some("JA1ABC AJ3G 73"));
        q.note_tx_sent(75);
        assert_eq!(q.step(), Step::Confirming);
        assert!(q.take_completed().is_none(), "logged once");
    }

    #[test]
    fn calling_cq_logs_when_our_rr73_goes_out() {
        let mut q = Qso::new(cfg());
        q.call_cq();
        assert_eq!(q.plan_tx().as_deref(), Some("CQ AJ3G FN20"));
        q.note_tx_sent(0);
        q.on_rx(&[d("AJ3G K1ABC FN42")], 15);
        assert_eq!(q.plan_tx().as_deref(), Some("K1ABC AJ3G -10"));
        q.note_tx_sent(30);
        q.on_rx(&[d("AJ3G K1ABC R-08")], 45);
        assert_eq!(q.plan_tx().as_deref(), Some("K1ABC AJ3G RR73"));
        assert!(q.take_completed().is_none());
        q.note_tx_sent(60);
        assert!(q.take_completed().is_some(), "logged as RR73 goes out");
        // They missed it and repeat R-08: RR73 again, no second entry.
        q.on_rx(&[d("AJ3G K1ABC R-08")], 75);
        assert_eq!(q.plan_tx().as_deref(), Some("K1ABC AJ3G RR73"));
        q.note_tx_sent(90);
        assert!(q.take_completed().is_none());
    }

    #[test]
    fn a_station_that_takes_someone_else_is_not_called_over() {
        let mut q = Qso::new(cfg());
        q.start_qso("W9XYZ", None, -5, false, 0);
        q.on_rx(&[d("K1ABC W9XYZ -12")], 15);
        assert_eq!(q.step(), Step::WaitCq);
        assert!(!q.wants_tx());
        q.on_rx(&[d("CQ W9XYZ EN52")], 60);
        assert_eq!(q.step(), Step::TxGrid);
    }

    #[test]
    fn several_answers_take_the_best_and_list_the_rest() {
        let mut q = Qso::new(cfg());
        q.call_cq();
        let mut a = d("AJ3G K1ABC FN42");
        a.snr_db = -20;
        let mut b = d("AJ3G EA8XYZ IL18");
        b.snr_db = 2;
        q.on_rx(&[a, b], 15);
        assert_eq!(q.dx_call(), Some("EA8XYZ"));
        assert_eq!(q.also_calling(), &["K1ABC".to_string()]);
    }

    #[test]
    fn the_queue_runs_in_order_and_skips_the_stale_and_the_dupes() {
        let mut q = Qso::new(cfg());
        let e = |c: &str, t: i64| Queued { call: c.into(), grid: None, snr_db: -10, audio_hz: 1500.0, heard_utc: t, wait_for_cq: false };
        q.queue_add(e("OLD1", 0));
        q.queue_add(e("DUPE1", 900));
        q.queue_add(e("NEXT1", 900));
        let n = q.take_next_queued(1000, |c| c == "DUPE1").expect("one");
        assert_eq!(n.call, "NEXT1");
        assert!(q.transcript().iter().any(|l| l.text.contains("OLD1 skipped")));
        assert!(q.transcript().iter().any(|l| l.text.contains("DUPE1 skipped")));
    }

    #[test]
    fn the_watchdog_stops_an_unanswered_cq_run() {
        let mut q = Qso::new(cfg());
        // Real UTC seconds: the clock starts at the first tick.
        let t0 = 1_800_000_000;
        q.call_cq();
        q.tick(t0);
        for t in (15..360).step_by(30) {
            q.note_tx_sent(t0 + t);
            q.tick(t0 + t);
        }
        assert!(q.tick(t0 + 361) || q.watchdog());
        assert!(!q.wants_tx());
    }

    #[test]
    fn a_hand_picked_line_goes_once_then_the_exchange_carries_on() {
        let mut q = Qso::new(cfg());
        q.start_qso("JA1ABC", None, -14, false, 0);
        q.queue_text("TNX 73 GL");
        assert_eq!(q.plan_tx().as_deref(), Some("TNX 73 GL"));
        q.note_tx_sent(15);
        assert_eq!(q.plan_tx().as_deref(), Some("JA1ABC AJ3G FN20"));
    }

    #[test]
    fn an_unconfigured_station_never_transmits() {
        let mut q = Qso::new(Config::default());
        q.call_cq();
        assert!(!q.wants_tx());
        assert_eq!(q.plan_tx(), None);
    }
}
