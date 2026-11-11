//! Callsign lookup: HamQTH (free, the default) and QRZ.com's XML data (a
//! subscription; switched off until AJ3G has one). Both log in for a session
//! key, kept while it lasts, then ask one call at a time.
//!
//! Ported from SDRoxide's `sdroxide-net/src/lookup.rs`
//! (https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later).
//!
//! FILL MISSING runs a batch of these on its own thread, paced so the services
//! don't block the account ([`pace`]), and fills only the fields a QSO has
//! empty ([`fill_missing`]). For a grid it asks, in turn (decision 75): QRZ.com
//! XML and HamQTH when there is a login for them, callook.info (the FCC's data,
//! no login) for a US call, and then, with no network, the same call's grid
//! elsewhere in the log and the grid it was last decoded with ([`FillBatch`],
//! [`Source`]). Only the callsign is sent (and the service's own login).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use super::net::{Http, scrub, tag_text, urlencode};
use super::{Credentials, LogSettings, LookupService, QsoRecord};

/// What a lookup service says about a callsign. `None` or empty where it says
/// nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CallInfo {
    pub call: String,
    pub name: String,
    pub qth: String,
    pub state: String,
    pub county: String,
    pub grid: String,
    pub country: String,
    pub dxcc: Option<u16>,
    pub cq_zone: Option<u8>,
    pub itu_zone: Option<u8>,
}

/// The time between one lookup and the next in a batch. HamQTH asks programs
/// to be gentle; QRZ.com allows more, but a thousand-QSO log at one a second
/// is still done in a quarter of an hour.
pub(crate) fn pace(s: LookupService) -> Duration {
    match s {
        LookupService::HamQth => Duration::from_millis(1500),
        LookupService::Qrz => Duration::from_millis(1000),
    }
}

/// The program name the services are told.
const PROGRAM: &str = "hardrock-panel";

/// A logged-in lookup service.
pub(crate) struct Session<'a, H: Http> {
    http: &'a H,
    service: LookupService,
    login: Credentials,
    key: Option<String>,
}

impl<'a, H: Http> Session<'a, H> {
    pub(crate) fn new(http: &'a H, service: LookupService, login: Credentials) -> Self {
        Session { http, service, login, key: None }
    }

    /// An error message with the login taken out.
    fn clean(&self, e: &str) -> String {
        scrub(e, &[&self.login.password])
    }

    fn log_in(&mut self) -> Result<String, String> {
        let (u, p) = (urlencode(self.login.user.trim()), urlencode(&self.login.password));
        let body = match self.service {
            LookupService::HamQth => {
                self.http.get(&format!("https://www.hamqth.com/xml.php?u={u}&p={p}"))
            }
            LookupService::Qrz => self.http.get(&format!(
                "https://xmldata.qrz.com/xml/current/?username={u};password={p};agent={PROGRAM}"
            )),
        }
        .map_err(|e| self.clean(&e))?;
        let key = match self.service {
            LookupService::HamQth => tag_text(&body, "session_id"),
            LookupService::Qrz => tag_text(&body, "key"),
        };
        key.ok_or_else(|| {
            let why = tag_text(&body, "error").unwrap_or_else(|| "no session given".into());
            format!("{} login: {}", self.service.label(), self.clean(&why))
        })
    }

    /// Look `call` up. `Ok(None)` when the service doesn't know it; `Err` when
    /// the service can't be used (a login refused, no connection), which ends
    /// a batch.
    pub(crate) fn lookup(&mut self, call: &str) -> Result<Option<CallInfo>, String> {
        let call = call.trim().to_ascii_uppercase();
        for attempt in 0..2 {
            let key = match &self.key {
                Some(k) => k.clone(),
                None => {
                    let k = self.log_in()?;
                    self.key = Some(k.clone());
                    k
                }
            };
            let c = urlencode(&call);
            let url = match self.service {
                LookupService::HamQth => {
                    format!("https://www.hamqth.com/xml.php?id={key}&callsign={c}&prg={PROGRAM}")
                }
                LookupService::Qrz => {
                    format!("https://xmldata.qrz.com/xml/current/?s={key};callsign={c}")
                }
            };
            let body = self.http.get(&url).map_err(|e| self.clean(&e))?;
            match parse(self.service, &body, &call) {
                Reply::Found(i) => return Ok(Some(i)),
                Reply::NotFound => return Ok(None),
                Reply::SessionGone if attempt == 0 => self.key = None,
                Reply::SessionGone => {
                    return Err(format!("{}: the session keeps expiring", self.service.label()));
                }
                Reply::Error(e) => {
                    return Err(format!("{}: {}", self.service.label(), self.clean(&e)));
                }
            }
        }
        Err(format!("{}: no session", self.service.label()))
    }
}

/// What a lookup reply says.
#[derive(Debug, PartialEq)]
enum Reply {
    Found(CallInfo),
    NotFound,
    /// The session key has expired: log in again.
    SessionGone,
    Error(String),
}

fn parse(service: LookupService, body: &str, call: &str) -> Reply {
    if let Some(e) = tag_text(body, "error") {
        let l = e.to_ascii_lowercase();
        return if l.contains("not found") {
            Reply::NotFound
        } else if l.contains("session") || l.contains("timeout") || l.contains("expired") {
            Reply::SessionGone
        } else {
            Reply::Error(e)
        };
    }
    let t = |n: &str| tag_text(body, n).unwrap_or_default();
    let num = |n: &str| tag_text(body, n).and_then(|s| s.parse().ok()).filter(|v: &u8| *v > 0);
    match service {
        LookupService::HamQth => {
            if tag_text(body, "search").is_none() && t("callsign").is_empty() {
                return Reply::NotFound;
            }
            let name = Some(t("adr_name")).filter(|s| !s.is_empty()).unwrap_or_else(|| t("nick"));
            Reply::Found(CallInfo {
                call: call.to_string(),
                name,
                qth: Some(t("qth")).filter(|s| !s.is_empty()).unwrap_or_else(|| t("adr_city")),
                state: t("us_state"),
                county: t("us_county"),
                grid: grid_or_position(&t("grid"), &t("latitude"), &t("longitude")),
                country: t("country"),
                dxcc: tag_text(body, "adif").and_then(|s| s.parse().ok()).filter(|v| *v > 0),
                cq_zone: num("cq"),
                itu_zone: num("itu"),
            })
        }
        LookupService::Qrz => {
            if t("call").is_empty() {
                return Reply::NotFound;
            }
            let name = format!("{} {}", t("fname"), t("name")).trim().to_string();
            Reply::Found(CallInfo {
                call: call.to_string(),
                name,
                qth: t("addr2"),
                state: t("state"),
                county: t("county"),
                grid: grid_or_position(&t("grid"), &t("lat"), &t("lon")),
                country: t("country"),
                dxcc: tag_text(body, "dxcc").and_then(|s| s.parse().ok()).filter(|v| *v > 0),
                cq_zone: num("cqzone"),
                itu_zone: num("ituzone"),
            })
        }
    }
}

/// The grid a service gave, or else the 6-character one its latitude and
/// longitude are in (HamQTH often has a position but no grid: AJ3G, 11 Nov
/// 2026). Empty when it gave neither.
fn grid_or_position(grid: &str, lat: &str, lon: &str) -> String {
    if super::grid4(grid).is_some() {
        return grid.trim().to_string();
    }
    match (lat.trim().parse::<f64>(), lon.trim().parse::<f64>()) {
        // 0, 0 is what some records hold for "unknown".
        (Ok(la), Ok(lo)) if !(la == 0.0 && lo == 0.0) => latlon_to_grid(la, lo).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The 6-character Maidenhead locator (`FN31pr`) a point is in: the inverse
/// of `station::grid_to_latlon`. `None` off the globe.
pub(crate) fn latlon_to_grid(lat: f64, lon: f64) -> Option<String> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    // The north pole and the antimeridian belong to the last square, not one
    // past it.
    let x = (lon + 180.0).min(359.999_999);
    let y = (lat + 90.0).min(179.999_999);
    let (fx, fy) = ((x / 20.0) as u8, (y / 10.0) as u8);
    let (sx, sy) = (((x % 20.0) / 2.0) as u8, (y % 10.0) as u8);
    let (ux, uy) = (((x % 2.0) * 12.0) as u8, ((y % 1.0) * 24.0) as u8);
    Some(format!(
        "{}{}{}{}{}{}",
        (b'A' + fx) as char,
        (b'A' + fy) as char,
        (b'0' + sx) as char,
        (b'0' + sy) as char,
        (b'a' + ux.min(23)) as char,
        (b'a' + uy.min(23)) as char,
    ))
}

/// callook.info's reply for a US call: the FCC's licence data, with its grid.
/// `Ok(None)` for a call it doesn't hold.
fn parse_callook(body: &str, call: &str) -> Result<Option<CallInfo>, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("callook.info: an unreadable reply ({e})"))?;
    let text = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    match text("/status").as_str() {
        "VALID" => {}
        // UPDATING: the FCC data is being reloaded; as good as not found.
        "INVALID" | "UPDATING" => return Ok(None),
        s => return Err(format!("callook.info: status {s:?}")),
    }
    // "NEWINGTON, CT 06111"
    let line2 = text("/address/line2");
    let (town, state) = match line2.rsplit_once(',') {
        Some((t, rest)) => {
            (t.trim().to_string(), rest.split_whitespace().next().unwrap_or("").to_string())
        }
        None => (String::new(), String::new()),
    };
    Ok(Some(CallInfo {
        call: call.to_string(),
        name: text("/name"),
        qth: town,
        state,
        grid: grid_or_position(
            &text("/location/gridsquare"),
            &text("/location/latitude"),
            &text("/location/longitude"),
        ),
        ..CallInfo::default()
    }))
}

/// Ask callook.info about a US call.
fn callook<H: Http>(http: &H, call: &str) -> Result<Option<CallInfo>, String> {
    let body = http.get(&format!("https://callook.info/{}/json", urlencode(call)))?;
    parse_callook(&body, call)
}

/// A call callook.info can know: one the FCC licenses (the US and its
/// territories), with no `/`.
pub(crate) fn fcc_call(call: &str) -> bool {
    const US: [&str; 10] = ["K", "KL", "KH6", "KP4", "KH2", "KP2", "KH0", "KH8", "KH9", "KP1"];
    !call.contains('/')
        && crate::dxcc::lookup(call).is_some_and(|e| US.contains(&e.prefix.as_str()))
}

/// The fields FILL MISSING fills, all empty in `r`: what is worth a lookup.
pub(crate) fn wants_lookup(r: &QsoRecord) -> bool {
    let blank = |s: &str| s.trim().is_empty();
    blank(&r.name)
        || blank(&r.qth)
        || blank(&r.grid)
        || blank(&r.country)
        || r.cq_zone.is_none()
        || r.itu_zone.is_none()
        || r.dxcc.is_none()
        || (is_us(r) && (blank(&r.state) || blank(&r.county)))
}

/// A US station, the only kind with a state and county worth asking for.
fn is_us(r: &QsoRecord) -> bool {
    r.dxcc.is_some_and(|d| d == 291 || d == 6 || d == 110)
        || crate::dxcc::lookup(&r.call)
            .is_some_and(|e| matches!(e.prefix.as_str(), "K" | "KL" | "KH6"))
}

/// Fill the fields `r` has empty from `i`, and say whether any were. What is
/// in `r` is never changed. For a `/` call (`DL/W1AW`, `W1AW/KH6`) only the
/// name is taken: the lookup is of the operator's home station, and its town,
/// grid and zones are not where the QSO was.
pub(crate) fn fill_missing(r: &mut QsoRecord, i: &CallInfo) -> bool {
    let before = r.clone();
    let fill = |dst: &mut String, src: &str| {
        if dst.trim().is_empty() && !src.trim().is_empty() {
            *dst = src.trim().to_string();
        }
    };
    fill(&mut r.name, &i.name);
    if !r.call.contains('/') {
        fill(&mut r.qth, &i.qth);
        fill(&mut r.state, &i.state);
        fill(&mut r.county, &i.county);
        fill(&mut r.country, &i.country);
        if r.grid.trim().is_empty() && super::grid4(&i.grid).is_some() {
            r.grid = i.grid.clone();
        }
        r.dxcc = r.dxcc.or(i.dxcc);
        r.cq_zone = r.cq_zone.or(i.cq_zone);
        r.itu_zone = r.itu_zone.or(i.itu_zone);
    }
    *r != before
}

/// What a lookup batch sends back.
#[derive(Debug)]
pub(crate) enum Msg {
    Found(CallInfo),
    NotFound(String),
    /// The batch stopped: the service can't be used.
    Failed(String),
    /// The batch is finished.
    Done,
}

/// A lookup batch running on its own thread (the form's LOOKUP).
pub(crate) struct Batch {
    pub rx: Receiver<Msg>,
    stop: Arc<AtomicBool>,
}

impl Batch {
    /// Look up `calls` one by one, [`pace`] apart.
    pub(crate) fn start(service: LookupService, login: Credentials, calls: Vec<String>) -> Batch {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let spawned = std::thread::Builder::new().name("log-lookup".into()).spawn(move || {
            let web = super::net::Web::new();
            run(&web, service, login, calls, &st, &tx, pace(service));
        });
        if let Err(e) = spawned {
            let (tx, rx2) = channel();
            let _ = tx.send(Msg::Failed(format!("couldn't start the lookups: {e}")));
            return Batch { rx: rx2, stop };
        }
        Batch { rx, stop }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run<H: Http>(
    http: &H,
    service: LookupService,
    login: Credentials,
    calls: Vec<String>,
    stop: &AtomicBool,
    tx: &Sender<Msg>,
    gap: Duration,
) {
    let mut s = Session::new(http, service, login);
    for (n, call) in calls.into_iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if n > 0 {
            // In small steps, so STOP is quick.
            let until = std::time::Instant::now() + gap;
            while std::time::Instant::now() < until && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let m = match s.lookup(&call) {
            Ok(Some(i)) => Msg::Found(i),
            Ok(None) => Msg::NotFound(call),
            Err(e) => {
                tracing::warn!("logbook lookup: {e}");
                let _ = tx.send(Msg::Failed(e));
                return;
            }
        };
        if tx.send(m).is_err() {
            return;
        }
    }
    let _ = tx.send(Msg::Done);
}

/// Where FILL MISSING found a grid, in the order it asks (decision 75).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Source {
    Qrz,
    HamQth,
    Callook,
    /// The same call's grid in another QSO in the log.
    Log,
    /// The grid the station was decoded sending this run (DECODE's heard
    /// stations).
    Heard,
}

impl Source {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Source::Qrz => "QRZ.com",
            Source::HamQth => "HamQTH",
            Source::Callook => "callook.info",
            Source::Log => "the log",
            Source::Heard => "decodes",
        }
    }

    /// The time between one request to this source and the next.
    fn pace(self) -> Duration {
        match self {
            Source::Qrz => pace(LookupService::Qrz),
            Source::HamQth => pace(LookupService::HamQth),
            Source::Callook => Duration::from_millis(1000),
            Source::Log | Source::Heard => Duration::ZERO,
        }
    }
}

/// The logins FILL MISSING has: QRZ.com XML when switched on and set, and
/// HamQTH when set. callook.info needs none.
#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub qrz: Option<Credentials>,
    pub hamqth: Option<Credentials>,
}

impl Plan {
    pub(crate) fn of(s: &LogSettings) -> Plan {
        Plan {
            qrz: (s.qrz_enabled && s.qrz.is_set()).then(|| s.qrz.clone()),
            hamqth: s.hamqth.is_set().then(|| s.hamqth.clone()),
        }
    }

    pub(crate) fn has_login(&self) -> bool {
        self.qrz.is_some() || self.hamqth.is_some()
    }
}

/// A call FILL MISSING asks about, and what its QSOs lack.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Want {
    pub call: String,
    /// Some field a login service fills is empty ([`wants_lookup`]).
    pub any: bool,
    /// Some QSO with it has no grid.
    pub grid: bool,
}

impl Want {
    /// Whether any source on the network could help with this call under
    /// `plan`.
    pub(crate) fn worth_asking(&self, plan: &Plan) -> bool {
        (plan.has_login() && (self.any || self.grid)) || (self.grid && fcc_call(&self.call))
    }
}

/// What a FILL MISSING batch sends back.
#[derive(Debug)]
pub(crate) enum FillMsg {
    /// What the sources said about a call, in the order asked; the first to
    /// give a field is the one used. Empty when none knew it.
    Call {
        call: String,
        found: Vec<(Source, CallInfo)>,
    },
    /// A source can't be used (a refused login, no connection): it is left
    /// out for the rest of the batch, and the others go on.
    SourceFailed(Source, String),
    Done,
}

/// FILL MISSING's lookups on their own thread.
pub(crate) struct FillBatch {
    pub rx: Receiver<FillMsg>,
    stop: Arc<AtomicBool>,
    pub total: usize,
    pub done: usize,
}

impl FillBatch {
    pub(crate) fn start(plan: Plan, wants: Vec<Want>) -> FillBatch {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let total = wants.len();
        let st = stop.clone();
        let spawned = std::thread::Builder::new().name("log-fill".into()).spawn(move || {
            let web = super::net::Web::new();
            run_fill(&web, plan, wants, &st, &tx, true);
        });
        if let Err(e) = spawned {
            let (tx, rx2) = channel();
            let _ = tx.send(FillMsg::SourceFailed(
                Source::HamQth,
                format!("couldn't start the lookups: {e}"),
            ));
            let _ = tx.send(FillMsg::Done);
            return FillBatch { rx: rx2, stop, total, done: 0 };
        }
        FillBatch { rx, stop, total, done: 0 }
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for FillBatch {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Wait until `src` may be asked again (when `paced`), in small steps so STOP
/// is quick. False when stopped.
fn take_turn(
    last: &mut HashMap<Source, Instant>,
    src: Source,
    paced: bool,
    stop: &AtomicBool,
) -> bool {
    if paced && let Some(t) = last.get(&src) {
        let until = *t + src.pace();
        while Instant::now() < until {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    last.insert(src, Instant::now());
    !stop.load(Ordering::Relaxed)
}

/// Ask about each call in turn: the login services in order (the second only
/// for a grid the first didn't give), then callook.info for a US call still
/// without one. Each call is asked once per source.
fn run_fill<H: Http>(
    http: &H,
    plan: Plan,
    wants: Vec<Want>,
    stop: &AtomicBool,
    tx: &Sender<FillMsg>,
    paced: bool,
) {
    let mut sessions: Vec<(Source, Session<'_, H>)> = Vec::new();
    if let Some(l) = plan.qrz {
        sessions.push((Source::Qrz, Session::new(http, LookupService::Qrz, l)));
    }
    if let Some(l) = plan.hamqth {
        sessions.push((Source::HamQth, Session::new(http, LookupService::HamQth, l)));
    }
    let mut callook_ok = true;
    let mut last = HashMap::new();
    let mut asked: std::collections::HashSet<String> = std::collections::HashSet::new();
    for w in wants {
        let call = w.call.trim().to_ascii_uppercase();
        if call.is_empty() || !asked.insert(call.clone()) {
            continue;
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut found: Vec<(Source, CallInfo)> = Vec::new();
        let has_grid =
            |f: &[(Source, CallInfo)]| f.iter().any(|(_, i)| super::grid4(&i.grid).is_some());
        let mut n = 0;
        while n < sessions.len() {
            let needed =
                if found.is_empty() { w.any || w.grid } else { w.grid && !has_grid(&found) };
            if !needed {
                break;
            }
            let src = sessions[n].0;
            if !take_turn(&mut last, src, paced, stop) {
                return;
            }
            match sessions[n].1.lookup(&call) {
                Ok(Some(i)) => found.push((src, i)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("logbook lookup: {e}");
                    sessions.remove(n);
                    if tx.send(FillMsg::SourceFailed(src, e)).is_err() {
                        return;
                    }
                    continue;
                }
            }
            n += 1;
        }
        if w.grid && !has_grid(&found) && callook_ok && fcc_call(&call) {
            if !take_turn(&mut last, Source::Callook, paced, stop) {
                return;
            }
            match callook(http, &call) {
                Ok(Some(i)) => found.push((Source::Callook, i)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("logbook lookup: callook.info: {e}");
                    callook_ok = false;
                    let _ = tx.send(FillMsg::SourceFailed(Source::Callook, e));
                }
            }
        }
        if tx.send(FillMsg::Call { call, found }).is_err() {
            return;
        }
    }
    let _ = tx.send(FillMsg::Done);
}

#[cfg(test)]
mod tests {
    use super::super::net::fake::Fake;
    use super::*;

    const HAMQTH_SESSION: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<session><session_id>09b0ae90050be03c452ad235a1f2915ad684393c</session_id></session>
</HamQTH>"#;

    const HAMQTH_OK1RR: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<search>
<callsign>ok1rr</callsign>
<nick>Petr</nick>
<qth>Neratovice</qth>
<country>Czech Republic</country>
<adif>503</adif>
<itu>28</itu>
<cq>15</cq>
<grid>jo70gg</grid>
<adr_name>Petr Hlozek</adr_name>
<adr_city>Neratovice</adr_city>
</search>
</HamQTH>"#;

    const HAMQTH_NOT_FOUND: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<session><error>Callsign not found</error></session></HamQTH>"#;

    const HAMQTH_EXPIRED: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<session><error>Session does not exist or expired</error></session></HamQTH>"#;

    const HAMQTH_BAD_LOGIN: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<session><error>Wrong user name or password</error></session></HamQTH>"#;

    const QRZ_SESSION: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
<Session><Key>2331uf894c4bd29f3923f3bacf02c532d7bd9</Key><Count>123</Count>
<SubExp>Wed Jan 1 12:34:03 2027</SubExp><GMTime>Sun Nov 10 21:25:34 2026</GMTime></Session>
</QRZDatabase>"#;

    const QRZ_AA7BQ: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
<Callsign><call>AA7BQ</call><fname>FRED L</fname><name>LLOYD</name>
<addr2>SCOTTSDALE</addr2><state>AZ</state><country>United States</country>
<grid>DM43dp</grid><county>Maricopa</county><dxcc>291</dxcc>
<cqzone>3</cqzone><ituzone>6</ituzone></Callsign>
<Session><Key>2331uf894c4bd29f3923f3bacf02c532d7bd9</Key><Count>124</Count></Session>
</QRZDatabase>"#;

    fn login() -> Credentials {
        Credentials { user: "AJ3G".into(), password: "s3cret&pw".into() }
    }

    #[test]
    fn hamqth_logs_in_once_and_looks_up() {
        let web = Fake::with(&[
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_SESSION),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_OK1RR),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_NOT_FOUND),
        ]);
        let mut s = Session::new(&web, LookupService::HamQth, login());
        let i = s.lookup("ok1rr").unwrap().unwrap();
        assert_eq!(i.name, "Petr Hlozek");
        assert_eq!(i.qth, "Neratovice");
        assert_eq!(i.grid, "jo70gg");
        assert_eq!((i.dxcc, i.cq_zone, i.itu_zone), (Some(503), Some(15), Some(28)));
        assert_eq!(s.lookup("N0CALL").unwrap(), None);
        let asked = web.asked.borrow();
        assert_eq!(asked.len(), 3, "one login for both: {asked:?}");
        assert!(asked[0].contains("p=s3cret%26pw"));
        assert!(asked[1].contains("id=09b0ae90050be03c452ad235a1f2915ad684393c&callsign=OK1RR"));
    }

    #[test]
    fn an_expired_session_logs_in_again() {
        let web = Fake::with(&[
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_SESSION),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_EXPIRED),
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_SESSION),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_OK1RR),
        ]);
        let mut s = Session::new(&web, LookupService::HamQth, login());
        assert!(s.lookup("OK1RR").unwrap().is_some());
    }

    #[test]
    fn a_refused_login_ends_the_batch_without_the_password() {
        let web = Fake::with(&[("https://www.hamqth.com/xml.php?u=", HAMQTH_BAD_LOGIN)]);
        let (tx, rx) = channel();
        let stop = AtomicBool::new(false);
        run(
            &web,
            LookupService::HamQth,
            login(),
            vec!["OK1RR".into(), "W1AW".into()],
            &stop,
            &tx,
            Duration::ZERO,
        );
        match rx.try_recv().unwrap() {
            Msg::Failed(e) => {
                assert!(e.contains("Wrong user name"), "{e}");
                assert!(!e.contains("s3cret"), "{e}");
            }
            m => panic!("{m:?}"),
        }
        assert!(rx.try_recv().is_err(), "nothing after the failure");
    }

    #[test]
    fn qrz_reads_its_own_field_names() {
        let web = Fake::with(&[
            ("https://xmldata.qrz.com/xml/current/?username=AJ3G;password=", QRZ_SESSION),
            (
                "https://xmldata.qrz.com/xml/current/?s=2331uf894c4bd29f3923f3bacf02c532d7bd9;callsign=AA7BQ",
                QRZ_AA7BQ,
            ),
        ]);
        let mut s = Session::new(&web, LookupService::Qrz, login());
        let i = s.lookup("aa7bq").unwrap().unwrap();
        assert_eq!(i.name, "FRED L LLOYD");
        assert_eq!(
            (i.qth.as_str(), i.state.as_str(), i.county.as_str()),
            ("SCOTTSDALE", "AZ", "Maricopa")
        );
        assert_eq!((i.dxcc, i.cq_zone, i.itu_zone), (Some(291), Some(3), Some(6)));
    }

    #[test]
    fn only_empty_fields_are_filled() {
        let i = CallInfo {
            call: "AA7BQ".into(),
            name: "FRED L LLOYD".into(),
            qth: "SCOTTSDALE".into(),
            state: "AZ".into(),
            county: "Maricopa".into(),
            grid: "DM43dp".into(),
            country: "United States".into(),
            dxcc: Some(291),
            cq_zone: Some(3),
            itu_zone: Some(6),
        };
        let mut r = QsoRecord {
            call: "AA7BQ".into(),
            name: "Fred".into(),
            grid: "DM43".into(),
            cq_zone: Some(4),
            ..QsoRecord::default()
        };
        assert!(fill_missing(&mut r, &i));
        assert_eq!(r.name, "Fred", "the operator's own entry stays");
        assert_eq!(r.grid, "DM43");
        assert_eq!(r.cq_zone, Some(4));
        assert_eq!((r.qth.as_str(), r.state.as_str(), r.dxcc), ("SCOTTSDALE", "AZ", Some(291)));
        assert!(!fill_missing(&mut r, &i), "nothing left to fill");
        // A portable call takes only the name.
        let mut p = QsoRecord { call: "AA7BQ/KH6".into(), ..QsoRecord::default() };
        fill_missing(&mut p, &i);
        assert_eq!((p.name.as_str(), p.grid.as_str(), p.dxcc), ("FRED L LLOYD", "", None));
    }

    #[test]
    fn a_position_is_turned_into_its_grid() {
        // W1AW, Newington CT.
        assert_eq!(latlon_to_grid(41.714775, -72.727260).as_deref(), Some("FN31pr"));
        assert_eq!(latlon_to_grid(50.30, 14.50).as_deref(), Some("JO70gh"));
        assert_eq!(latlon_to_grid(-90.0, -180.0).as_deref(), Some("AA00aa"));
        assert_eq!(latlon_to_grid(90.0, 180.0).as_deref(), Some("RR99xx"));
        assert_eq!(latlon_to_grid(91.0, 0.0), None);
        // Every subsquare's centre comes back to itself.
        for g in ["FN20xr", "JO65ab", "PM95vq", "GG66mk", "RE78ir", "AA00aa", "RR99xx"] {
            let p = crate::station::grid_to_latlon(g).unwrap();
            assert_eq!(latlon_to_grid(p.lat, p.lon).as_deref(), Some(g));
        }
        // A grid given wins; a position fills in for none; 0,0 is no position.
        assert_eq!(grid_or_position("FN20", "41.7", "-72.7"), "FN20");
        assert_eq!(grid_or_position("", "41.714775", "-72.727260"), "FN31pr");
        assert_eq!(grid_or_position("", "0", "0"), "");
        assert_eq!(grid_or_position("", "", ""), "");
    }

    const HAMQTH_NO_GRID: &str = r#"<?xml version="1.0"?>
<HamQTH version="2.8" xmlns="https://www.hamqth.com">
<search><callsign>dl1abc</callsign><nick>Hans</nick><country>Germany</country>
<latitude>52.52</latitude><longitude>13.40</longitude></search></HamQTH>"#;

    #[test]
    fn hamqth_with_a_position_and_no_grid_gives_the_grid() {
        match parse(LookupService::HamQth, HAMQTH_NO_GRID, "DL1ABC") {
            Reply::Found(i) => assert_eq!(i.grid, "JO62qm"),
            r => panic!("{r:?}"),
        }
    }

    const CALLOOK_W1AW: &str = r#"{"status":"VALID","type":"CLUB","current":{"callsign":"W1AW","operClass":""},
"previous":{"callsign":"","operClass":""},"trustee":{"callsign":"W1AW","name":"ARRL"},
"name":"ARRL HQ OPERATORS CLUB","address":{"line1":"225 MAIN ST","line2":"NEWINGTON, CT 06111","attn":""},
"location":{"latitude":"41.714775","longitude":"-72.727260","gridsquare":"FN31pr"},
"otherInfo":{"grantDate":"05\/04\/2020","frn":"0004511143"}}"#;

    #[test]
    fn callook_replies_are_read() {
        let i = parse_callook(CALLOOK_W1AW, "W1AW").unwrap().unwrap();
        assert_eq!(i.grid, "FN31pr");
        assert_eq!(
            (i.name.as_str(), i.qth.as_str(), i.state.as_str()),
            ("ARRL HQ OPERATORS CLUB", "NEWINGTON", "CT")
        );
        let no_grid = CALLOOK_W1AW.replace(r#""gridsquare":"FN31pr""#, r#""gridsquare":"""#);
        assert_eq!(parse_callook(&no_grid, "W1AW").unwrap().unwrap().grid, "FN31pr");
        assert_eq!(parse_callook(r#"{"status":"INVALID"}"#, "W9ZZZ").unwrap(), None);
        assert!(parse_callook("<html>busy</html>", "W1AW").is_err());
    }

    fn fill(web: &Fake, plan: Plan, wants: Vec<Want>) -> Vec<FillMsg> {
        let (tx, rx) = channel();
        run_fill(web, plan, wants, &AtomicBool::new(false), &tx, false);
        rx.try_iter().collect()
    }

    fn want(call: &str, any: bool, grid: bool) -> Want {
        Want { call: call.into(), any, grid }
    }

    const QRZ_NO_GRID: &str = r#"<QRZDatabase><Callsign><call>OK1RR</call><fname>Petr</fname>
<country>Czech Republic</country></Callsign><Session><Key>k</Key></Session></QRZDatabase>"#;

    #[test]
    fn grids_are_asked_of_qrz_then_hamqth_then_callook() {
        let both = Plan { qrz: Some(login()), hamqth: Some(login()) };
        // QRZ.com gives no grid: HamQTH is asked, and its grid is the one.
        let web = Fake::with(&[
            ("https://xmldata.qrz.com/xml/current/?username=", QRZ_SESSION),
            ("https://xmldata.qrz.com/xml/current/?s=", QRZ_NO_GRID),
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_SESSION),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_OK1RR),
        ]);
        let m = fill(&web, both.clone(), vec![want("OK1RR", true, true)]);
        match &m[0] {
            FillMsg::Call { call, found } => {
                assert_eq!(call, "OK1RR");
                let srcs: Vec<Source> = found.iter().map(|f| f.0).collect();
                assert_eq!(srcs, [Source::Qrz, Source::HamQth]);
                assert_eq!(found[1].1.grid, "jo70gg");
            }
            m => panic!("{m:?}"),
        }
        assert!(matches!(m[1], FillMsg::Done));
        // QRZ.com gives the grid: nothing else is asked.
        let web = Fake::with(&[
            ("https://xmldata.qrz.com/xml/current/?username=", QRZ_SESSION),
            ("https://xmldata.qrz.com/xml/current/?s=", QRZ_AA7BQ),
        ]);
        fill(&web, both.clone(), vec![want("AA7BQ", true, true)]);
        assert_eq!(web.asked.borrow().len(), 2);
        // Neither has the US call: callook.info is asked, with only the call.
        let web = Fake::with(&[
            ("https://xmldata.qrz.com/xml/current/?username=", QRZ_SESSION),
            (
                "https://xmldata.qrz.com/xml/current/?s=",
                "<QRZDatabase><Session><Error>Not found: W1AW</Error></Session></QRZDatabase>",
            ),
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_SESSION),
            ("https://www.hamqth.com/xml.php?id=", HAMQTH_NOT_FOUND),
            ("https://callook.info/W1AW/json", CALLOOK_W1AW),
        ]);
        let m = fill(&web, both, vec![want("W1AW", false, true)]);
        match &m[0] {
            FillMsg::Call { found, .. } => {
                assert_eq!(found.len(), 1);
                assert_eq!((found[0].0, found[0].1.grid.as_str()), (Source::Callook, "FN31pr"));
            }
            m => panic!("{m:?}"),
        }
        assert_eq!(web.asked.borrow().last().unwrap(), "https://callook.info/W1AW/json");
    }

    #[test]
    fn with_no_login_only_us_calls_go_to_callook() {
        let none = Plan::default();
        assert!(want("W1AW", false, true).worth_asking(&none));
        assert!(!want("OK1RR", true, true).worth_asking(&none), "callook.info is the FCC's");
        assert!(!want("W1AW", true, false).worth_asking(&none), "callook is asked for grids only");
        assert!(!want("W1AW/KH6", false, true).worth_asking(&none));
        let web = Fake::with(&[("https://callook.info/W1AW/json", CALLOOK_W1AW)]);
        // A call twice is asked once.
        let m = fill(&web, none, vec![want("W1AW", false, true), want("w1aw", false, true)]);
        assert_eq!(web.asked.borrow().len(), 1);
        assert_eq!(m.len(), 2, "{m:?}");
    }

    #[test]
    fn a_refused_login_leaves_that_service_out_and_goes_on() {
        let plan = Plan { qrz: None, hamqth: Some(login()) };
        let web = Fake::with(&[
            ("https://www.hamqth.com/xml.php?u=", HAMQTH_BAD_LOGIN),
            ("https://callook.info/W1AW/json", CALLOOK_W1AW),
        ]);
        let m = fill(&web, plan, vec![want("W1AW", true, true), want("K1ABC", true, true)]);
        match &m[0] {
            FillMsg::SourceFailed(Source::HamQth, e) => assert!(!e.contains("s3cret"), "{e}"),
            m => panic!("{m:?}"),
        }
        assert!(matches!(&m[1], FillMsg::Call { found, .. } if found.len() == 1));
        // K1ABC: HamQTH isn't asked again; callook.info has no reply scripted,
        // so it fails and is left out too.
        assert!(matches!(&m[2], FillMsg::SourceFailed(Source::Callook, _)));
        assert!(matches!(&m[3], FillMsg::Call { found, .. } if found.is_empty()));
        assert!(matches!(m[4], FillMsg::Done));
        assert_eq!(web.asked.borrow().iter().filter(|u| u.contains("hamqth")).count(), 1);
    }

    #[test]
    fn a_grid_from_any_source_fills_only_an_empty_grid() {
        let i = CallInfo { call: "W1AW".into(), grid: "FN31pr".into(), ..CallInfo::default() };
        let mut r = QsoRecord { call: "W1AW".into(), ..QsoRecord::default() };
        assert!(fill_missing(&mut r, &i));
        assert_eq!(r.grid, "FN31pr");
        let mut r = QsoRecord { call: "W1AW".into(), grid: "FN31".into(), ..QsoRecord::default() };
        let other = CallInfo { grid: "EM10".into(), ..i };
        assert!(!fill_missing(&mut r, &other));
        assert_eq!(r.grid, "FN31");
    }
}
