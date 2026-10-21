// The query follows SDRoxide's crates/sdroxide-net/src/wsprnet.rs
// (https://github.com/dividebysandwich/sdroxide), GPL-3.0-or-later, and what
// WSJT-X and wsprd post.

//! WSPRnet: each WSPR slot's spots, posted to `http://wsprnet.org/post/` as
//! WSJT-X does, one form-encoded POST per spot (`function=wspr`), or, for a
//! slot that heard nothing, one saying the station listened
//! (`function=wsprstat`). Frequencies in MHz to six places, the date `YYMMDD`
//! and the time `HHMM` of the slot, `mode=2` (WSPR-2).
//!
//! Plain HTTP over a `TcpStream`, as the clients have always posted; the
//! server answers with how many spots it added.

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use super::{Service, Status};
use crate::decodelog::civil;

pub(crate) const HOST: &str = "wsprnet.org";
const PORT: u16 = 80;
const PATH: &str = "/post/";
const TIMEOUT: Duration = Duration::from_secs(20);
/// The most slots held while WSPRnet can't be reached (an hour and more).
const MAX_SLOTS: usize = 40;

/// Who is listening.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Listener {
    pub call: String,
    pub grid: String,
    /// `version=`: "Hardrock Panel 0.1.0".
    pub version: String,
}

/// One WSPR station heard.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spot {
    pub call: String,
    pub grid: String,
    pub snr_db: i32,
    pub dt_s: f32,
    pub drift_hz: i32,
    /// The signal's frequency, Hz.
    pub freq_hz: f64,
    pub power_dbm: i32,
}

/// One slot's worth, to post.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Slot {
    /// UTC of its start, Unix seconds.
    pub utc: i64,
    /// The receiver's dial, Hz (through the transverter).
    pub dial_hz: f64,
    pub spots: Vec<Spot>,
}

fn mhz(hz: f64) -> String {
    format!("{:.6}", hz / 1e6)
}

/// The fields of one spot's post.
pub(crate) fn spot_fields(me: &Listener, slot: &Slot, s: &Spot) -> Vec<(&'static str, String)> {
    let (y, mo, d, h, mi, _) = civil(slot.utc);
    vec![
        ("function", "wspr".into()),
        ("rcall", me.call.clone()),
        ("rgrid", me.grid.clone()),
        ("rqrg", mhz(slot.dial_hz)),
        ("date", format!("{:02}{mo:02}{d:02}", y.rem_euclid(100))),
        ("time", format!("{h:02}{mi:02}")),
        ("sig", s.snr_db.to_string()),
        ("dt", format!("{:.1}", s.dt_s)),
        ("drift", s.drift_hz.to_string()),
        ("tqrg", mhz(s.freq_hz)),
        ("tcall", s.call.clone()),
        ("tgrid", s.grid.clone()),
        ("dbm", s.power_dbm.to_string()),
        ("version", me.version.clone()),
        ("mode", "2".into()),
    ]
}

/// The fields for a slot that heard nothing: listening, not transmitting.
pub(crate) fn quiet_fields(me: &Listener, slot: &Slot) -> Vec<(&'static str, String)> {
    vec![
        ("function", "wsprstat".into()),
        ("rcall", me.call.clone()),
        ("rgrid", me.grid.clone()),
        ("rqrg", mhz(slot.dial_hz)),
        ("tpct", "0".into()),
        ("tqrg", mhz(slot.dial_hz)),
        ("dbm", "0".into()),
        ("version", me.version.clone()),
        ("mode", "2".into()),
    ]
}

/// `k=v&…`, each value percent-encoded (a `/` in a callsign, the spaces in
/// the version).
pub(crate) fn form(fields: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        for b in v.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    out
}

/// The whole HTTP request for a form body.
pub(crate) fn request(body: &str, version: &str) -> String {
    format!(
        "POST {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: {}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        version.replace(' ', "-"),
        body.len()
    )
}

/// What the server said: its status code, and the number of spots it says
/// it added, if it says.
pub(crate) fn read_reply(reply: &str) -> Result<Option<u32>, String> {
    let status = reply.lines().next().unwrap_or("");
    let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(format!("WSPRnet answered \"{}\"", status.trim()));
    }
    // "1 out of 1 spot(s) added", or "1 spot(s) added".
    let added = reply.lines().find_map(|l| {
        let head = &l[..l.find(" spot(s) added")?];
        let head = head.split(" out of ").next().unwrap_or(head);
        head.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    });
    Ok(added)
}

fn post(body: &str, version: &str) -> Result<Option<u32>, String> {
    let addr = (HOST, PORT)
        .to_socket_addrs()
        .map_err(|e| format!("{HOST}: {e}"))?
        .next()
        .ok_or_else(|| format!("{HOST}: no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| format!("{HOST}: {e}"))?;
    s.set_read_timeout(Some(TIMEOUT)).ok();
    s.set_write_timeout(Some(TIMEOUT)).ok();
    s.write_all(request(body, version).as_bytes()).map_err(|e| format!("{HOST}: {e}"))?;
    let mut reply = Vec::new();
    // `Connection: close`: the server ends it. Cap what is read.
    s.take(64 * 1024).read_to_end(&mut reply).map_err(|e| format!("{HOST}: {e}"))?;
    read_reply(&String::from_utf8_lossy(&reply))
}

/// The uploader's thread: posts each slot as it comes, keeps what could not
/// be posted for next time, until its handle goes.
pub(crate) struct Uploader {
    tx: Sender<Slot>,
}

impl Uploader {
    pub(crate) fn spawn(me: Listener, status: Sender<Status>) -> std::io::Result<Self> {
        let (tx, slots) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("wsprnet".into()).spawn(move || run(me, slots, status))?;
        Ok(Uploader { tx })
    }

    pub(crate) fn slot(&self, s: Slot) {
        let _ = self.tx.send(s);
    }
}

/// How long after a failed post to try again, if no new slot comes first.
const RETRY: Duration = Duration::from_secs(60);

fn run(me: Listener, slots: Receiver<Slot>, status: Sender<Status>) {
    let mut queue: VecDeque<Slot> = VecDeque::new();
    let mut retry_at: Option<Instant> = None;
    loop {
        match slots.recv_timeout(Duration::from_secs(1)) {
            Ok(s) => {
                if queue.len() >= MAX_SLOTS {
                    queue.pop_front();
                }
                queue.push_back(s);
            }
            Err(RecvTimeoutError::Timeout) => {
                if queue.is_empty() || retry_at.is_none_or(|t| Instant::now() < t) {
                    continue;
                }
            }
            // The tick was cleared, or the panel closed: nothing more is sent.
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let (sent, added, err) = flush(&me, &mut queue);
        retry_at = err.is_some().then(|| Instant::now() + RETRY);
        let plural = if sent == 1 { "" } else { "s" };
        let text = match (&err, added) {
            (Some(e), _) => e.clone(),
            (None, _) if sent == 0 => "listening reported, nothing heard".into(),
            (None, Some(a)) => format!("{sent} spot{plural} sent, {a} added"),
            (None, None) => format!("{sent} spot{plural} sent"),
        };
        let _ = status.send(Status { service: Service::Wsprnet, text, ok: err.is_none(), sent });
    }
}

/// Post what is queued, oldest first, until done or one fails (it and what
/// is behind it stay for next time). Returns the spots posted, how many the
/// server said it added (if it said each time), and the failure.
fn flush(me: &Listener, queue: &mut VecDeque<Slot>) -> (usize, Option<u32>, Option<String>) {
    let (mut sent, mut added) = (0usize, Some(0u32));
    while let Some(slot) = queue.front_mut() {
        if slot.spots.is_empty()
            && let Err(e) = post(&form(&quiet_fields(me, slot)), &me.version)
        {
            return (sent, added, Some(e));
        }
        while let Some(s) = slot.spots.first() {
            match post(&form(&spot_fields(me, slot, s)), &me.version) {
                Ok(n) => {
                    added = added.zip(n).map(|(a, n)| a + n);
                    sent += 1;
                    slot.spots.remove(0);
                }
                Err(e) => return (sent, added, Some(e)),
            }
        }
        queue.pop_front();
    }
    (sent, added, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Listener {
        Listener {
            call: "AJ3G".into(),
            grid: "FN20".into(),
            version: "Hardrock Panel 0.1.0".into(),
        }
    }

    fn slot() -> Slot {
        Slot {
            utc: 1_792_545_240, // 2026-10-21 01:14 UTC
            dial_hz: 7_038_600.0,
            spots: vec![Spot {
                call: "PJ4/K1ABC".into(),
                grid: "FK52".into(),
                snr_db: -24,
                dt_s: 0.3,
                drift_hz: -1,
                freq_hz: 7_040_087.0,
                power_dbm: 37,
            }],
        }
    }

    #[test]
    fn a_spot_is_posted_as_wsjtx_posts_it() {
        let s = slot();
        let body = form(&spot_fields(&me(), &s, &s.spots[0]));
        assert_eq!(
            body,
            "function=wspr&rcall=AJ3G&rgrid=FN20&rqrg=7.038600&date=261021&time=0114&sig=-24\
             &dt=0.3&drift=-1&tqrg=7.040087&tcall=PJ4%2FK1ABC&tgrid=FK52&dbm=37\
             &version=Hardrock%20Panel%200.1.0&mode=2"
        );
        let req = request(&body, &me().version);
        assert!(req.starts_with("POST /post/ HTTP/1.1\r\nHost: wsprnet.org\r\n"));
        assert!(req.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(req.ends_with(&format!("\r\n\r\n{body}")));
    }

    #[test]
    fn a_quiet_slot_says_it_listened() {
        let s = Slot { spots: vec![], ..slot() };
        assert_eq!(
            form(&quiet_fields(&me(), &s)),
            "function=wsprstat&rcall=AJ3G&rgrid=FN20&rqrg=7.038600&tpct=0&tqrg=7.038600&dbm=0\
             &version=Hardrock%20Panel%200.1.0&mode=2"
        );
    }

    #[test]
    fn the_reply_is_read() {
        let ok = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n0 out of 1 spot(s) added\n";
        assert_eq!(read_reply(ok), Ok(Some(0)));
        assert_eq!(read_reply("HTTP/1.1 200 OK\r\n\r\n2 spot(s) added"), Ok(Some(2)));
        assert_eq!(read_reply("HTTP/1.1 200 OK\r\n\r\n"), Ok(None));
        assert!(read_reply("HTTP/1.1 503 Service Unavailable\r\n\r\n").is_err());
        assert!(read_reply("").is_err());
    }
}
