//! RBN: the CW skimmer's stations for the Reverse Beacon Network (AJ3G, 7 Nov
//! 2026: "allow CW SKIMS to go to RBN").
//!
//! The RBN takes spots from RBN Aggregator, its own program, which reads a
//! skimmer's telnet port (CW Skimmer's is 7300) and passes what it reads on
//! under the node's callsign. So the panel does what CW Skimmer does: a
//! telnet port on this computer only (127.0.0.1) that asks for a callsign and
//! then sends each spot as a cluster line, "DX de AJ3G-#: 14025.0 W1AW CW
//! 12 dB 22 WPM CQ 2154Z". Nothing leaves the computer unless Aggregator is
//! running and pointed at it.
//!
//! What is spotted is held to what CW Skimmer spots, since a busted call on
//! the RBN reaches every cluster: a station calling CQ (or TEST), its call
//! read at least twice in what it sent, not the operator's own, once a band
//! in ten minutes.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// CW Skimmer's telnet port, where RBN Aggregator looks by default.
pub(crate) const PORT: u16 = 7300;

/// The same call on the same band is spotted again after this long, s.
const AGAIN_S: f64 = 600.0;

/// One spot for the RBN.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spot {
    pub call: String,
    /// Where it is, kHz to 0.1.
    pub khz: f64,
    pub snr_db: i32,
    pub wpm: u16,
    /// UTC heard, s.
    pub utc: f64,
}

/// The cluster line for `spot` from the node `node` (the operator's call),
/// as CW Skimmer's telnet port writes it.
pub(crate) fn line(node: &str, s: &Spot) -> String {
    let t = s.utc.max(0.0) as u64 % 86_400;
    format!(
        "DX de {:<10}{:>8.1}  {:<13}CW   {:>3} dB  {:>2} WPM  CQ      {:02}{:02}Z\r\n",
        format!("{node}-#:"),
        s.khz,
        s.call,
        s.snr_db,
        s.wpm,
        t / 3600,
        t / 60 % 60
    )
}

/// Chooses which of the CW skimmer's stations are spotted.
#[derive(Default)]
pub(crate) struct Picker {
    /// (call, MHz) → UTC last spotted.
    last: HashMap<(String, u64), f64>,
}

impl Picker {
    /// A CW signal read as `text` with the call `call`, at `rf_hz`: the spot
    /// to send, if it is one. `me` is the operator's own call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn offer(
        &mut self,
        me: &str,
        call: &str,
        text: &str,
        rf_hz: f64,
        snr_db: f32,
        wpm: u16,
        utc: f64,
    ) -> Option<Spot> {
        if call.len() < 3 || call.eq_ignore_ascii_case(me) || rf_hz <= 0.0 || wpm == 0 {
            return None;
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        let calling = words.iter().any(|w| *w == "CQ" || *w == "TEST");
        let times = words.iter().filter(|w| w.eq_ignore_ascii_case(call)).count();
        if !calling || times < 2 {
            return None;
        }
        let key = (call.to_ascii_uppercase(), (rf_hz / 1e6) as u64);
        if self.last.get(&key).is_some_and(|&t| utc - t < AGAIN_S) {
            return None;
        }
        self.last.retain(|_, t| utc - *t < AGAIN_S);
        self.last.insert(key, utc);
        Some(Spot {
            call: call.to_ascii_uppercase(),
            khz: (rf_hz / 100.0).round() / 10.0,
            snr_db: snr_db.round() as i32,
            wpm,
            utc,
        })
    }
}

/// The telnet port, while RBN is ticked. Dropping it closes the port and
/// every connection.
pub(crate) struct Server {
    clients: Arc<Mutex<Vec<TcpStream>>>,
    stop: Arc<AtomicBool>,
    pub port: u16,
}

impl Server {
    /// Listen on 127.0.0.1:`port`, sending `node` in each greeting.
    pub(crate) fn spawn(node: &str, port: u16) -> std::io::Result<Server> {
        let l = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
        l.set_nonblocking(true)?;
        let port = l.local_addr()?.port();
        let clients = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s, node) = (clients.clone(), stop.clone(), node.to_string());
        std::thread::Builder::new().name("rbn-telnet".into()).spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((stream, _)) => {
                        let (c, node) = (c.clone(), node.clone());
                        let _ = std::thread::Builder::new()
                            .name("rbn-login".into())
                            .spawn(move || login(stream, &node, &c));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        })?;
        tracing::info!("rbn: skimmer telnet listening on 127.0.0.1:{port}");
        Ok(Server { clients, stop, port })
    }

    /// Send `line` to everyone logged in; a connection that fails is dropped.
    pub(crate) fn send(&self, line: &str) {
        let mut c = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        c.retain_mut(|s| s.write_all(line.as_bytes()).is_ok());
    }

    /// How many are logged in.
    pub(crate) fn connected(&self) -> usize {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut c = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        for s in c.drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// CW Skimmer's login: ask the callsign, greet, then send spots. What the
/// other end sends after (commands Aggregator may try) is read and ignored.
fn login(mut s: TcpStream, node: &str, clients: &Mutex<Vec<TcpStream>>) {
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
    if s.write_all(b"Please enter your callsign: ").is_err() {
        return;
    }
    let mut r = BufReader::new(match s.try_clone() {
        Ok(r) => r,
        Err(_) => return,
    });
    let mut who = String::new();
    if r.read_line(&mut who).unwrap_or(0) == 0 {
        return;
    }
    let who = who.trim().to_ascii_uppercase();
    let hello = format!(
        "\r\nHello {who}, this is {node}-# (Hardrock Panel's CW skimmer)\r\n{node}-# >\r\n"
    );
    if s.write_all(hello.as_bytes()).is_err() {
        return;
    }
    tracing::info!("rbn: {who} logged in");
    let _ = s.set_read_timeout(None);
    clients.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    let mut sink = String::new();
    while r.read_line(&mut sink).unwrap_or(0) > 0 {
        sink.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cq_with_the_call_twice_is_spotted_once_in_ten_minutes() {
        let mut p = Picker::default();
        let s = p.offer("AJ3G", "W1AW", "CQ CQ DE W1AW W1AW K", 14_025_040.0, 12.4, 22, 1000.0);
        let s = s.expect("spotted");
        assert_eq!((s.call.as_str(), s.khz, s.snr_db, s.wpm), ("W1AW", 14_025.0, 12, 22));
        assert_eq!(p.offer("AJ3G", "W1AW", "CQ W1AW W1AW", 14_025_000.0, 9.0, 22, 1300.0), None);
        // Another band, or after ten minutes: again.
        assert!(p.offer("AJ3G", "W1AW", "CQ W1AW W1AW", 7_025_000.0, 9.0, 22, 1300.0).is_some());
        assert!(p.offer("AJ3G", "W1AW", "CQ W1AW W1AW", 14_025_000.0, 9.0, 22, 1601.0).is_some());
    }

    #[test]
    fn what_cw_skimmer_would_not_spot_is_not_spotted() {
        let mut p = Picker::default();
        // The call read once: maybe busted.
        assert_eq!(p.offer("AJ3G", "W1AW", "CQ CQ DE W1AW K", 14e6, 9.0, 22, 0.0), None);
        // Not calling CQ: a QSO in progress.
        assert_eq!(p.offer("AJ3G", "W1AW", "W1AW DE K2XX 599 W1AW", 14e6, 9.0, 22, 0.0), None);
        // The operator's own signal.
        assert_eq!(p.offer("AJ3G", "AJ3G", "CQ AJ3G AJ3G", 14e6, 9.0, 22, 0.0), None);
        // A contest CQ counts.
        assert!(p.offer("AJ3G", "K3LR", "TEST K3LR K3LR", 14e6, 9.0, 30, 0.0).is_some());
    }

    #[test]
    fn the_line_is_cw_skimmers() {
        let s = Spot { call: "W1AW".into(), khz: 14_025.0, snr_db: 12, wpm: 22, utc: 79_440.0 };
        assert_eq!(
            line("AJ3G", &s),
            "DX de AJ3G-#:    14025.0  W1AW         CW    12 dB  22 WPM  CQ      2204Z\r\n"
        );
    }

    #[test]
    fn a_telnet_client_logs_in_and_gets_the_spots() {
        let srv = Server::spawn("AJ3G", 0).expect("listen");
        let mut c = TcpStream::connect(("127.0.0.1", srv.port)).expect("connect");
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut prompt = [0u8; 28];
        std::io::Read::read_exact(&mut r, &mut prompt).unwrap();
        assert_eq!(&prompt, b"Please enter your callsign: ");
        c.write_all(b"AJ3G\r\n").unwrap();
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        l.clear();
        r.read_line(&mut l).unwrap();
        assert!(l.contains("Hello AJ3G"), "{l}");
        l.clear();
        r.read_line(&mut l).unwrap();
        let t = std::time::Instant::now();
        while srv.connected() == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let s = Spot { call: "W1AW".into(), khz: 14_025.0, snr_db: 12, wpm: 22, utc: 0.0 };
        srv.send(&line("AJ3G", &s));
        l.clear();
        r.read_line(&mut l).unwrap();
        assert!(l.starts_with("DX de AJ3G-#:"), "{l}");
    }
}
