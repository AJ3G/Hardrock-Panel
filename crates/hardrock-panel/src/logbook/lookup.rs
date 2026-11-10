//! Callsign lookup: HamQTH (free, the default) and QRZ.com's XML data (a
//! subscription; switched off until AJ3G has one). Both log in for a session
//! key, kept while it lasts, then ask one call at a time.
//!
//! Ported from SDRoxide's `sdroxide-net/src/lookup.rs`
//! (https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later).
//!
//! FILL MISSING runs a batch of these on its own thread, paced so the services
//! don't block the account ([`pace`]), and fills only the fields a QSO has
//! empty ([`fill_missing`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use super::net::{Http, scrub, tag_text, urlencode};
use super::{Credentials, LookupService, QsoRecord};

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
                grid: t("grid"),
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
                grid: t("grid"),
                country: t("country"),
                dxcc: tag_text(body, "dxcc").and_then(|s| s.parse().ok()).filter(|v| *v > 0),
                cq_zone: num("cqzone"),
                itu_zone: num("ituzone"),
            })
        }
    }
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

/// A lookup batch running on its own thread.
pub(crate) struct Batch {
    pub rx: Receiver<Msg>,
    stop: Arc<AtomicBool>,
    pub total: usize,
    pub done: usize,
}

impl Batch {
    /// Look up `calls` one by one, [`pace`] apart.
    pub(crate) fn start(service: LookupService, login: Credentials, calls: Vec<String>) -> Batch {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let total = calls.len();
        let st = stop.clone();
        let spawned = std::thread::Builder::new().name("log-lookup".into()).spawn(move || {
            let web = super::net::Web::new();
            run(&web, service, login, calls, &st, &tx, pace(service));
        });
        if let Err(e) = spawned {
            let (tx, rx2) = channel();
            let _ = tx.send(Msg::Failed(format!("couldn't start the lookups: {e}")));
            return Batch { rx: rx2, stop, total, done: 0 };
        }
        Batch { rx, stop, total, done: 0 }
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
}
