// The packet layout follows SDRoxide's crates/sdroxide-net/src/pskupload.rs
// (https://github.com/dividebysandwich/sdroxide), GPL-3.0-or-later, and the
// record formats WSJT-X sends.

//! PSK Reporter: what this station hears, as IPFIX (RFC 5101) over UDP to
//! `report.pskreporter.info:4739`, in PSK Reporter's own information elements
//! (enterprise number 30351), following its developer notes
//! (<https://pskreporter.info/pskdev.html>):
//!
//! * A datagram is the 16-byte IPFIX header, the two record descriptors
//!   (templates) in the first three datagrams and then about once an hour
//!   (UDP loses some; the server keeps them), the receiver record (who is
//!   listening) and the sender records (who was heard).
//! * No more than one datagram every five minutes unless one is full, timed
//!   from when the uploader started (not the clock, so stations do not all
//!   send at once), with a few seconds of random lead on top.
//! * Each callsign once in a batch (the best report kept), and once an hour
//!   unless something about it changed (its grid, band or mode).
//!
//! The records are WSJT-X's: the receiver's callsign, locator and decoding
//! software; each sender's callsign, frequency (five bytes, so a transverter's
//! microwave band fits), SNR, mode, locator, information source (1: decoded
//! automatically) and the time it was heard.

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use super::{Service, Status};

pub(crate) const HOST: &str = "report.pskreporter.info";
pub(crate) const PORT: u16 = 4739;

/// PSK Reporter's IANA private enterprise number.
const ENTERPRISE: u32 = 30351;
/// The template IDs (any from 256 up), as PSK Reporter's own examples number
/// them.
const TEMPLATE_RECEIVER: u16 = 0x9992;
const TEMPLATE_SENDER: u16 = 0x9993;
/// IPFIX set IDs for a template set and an options template set.
const SET_TEMPLATE: u16 = 2;
const SET_OPTIONS_TEMPLATE: u16 = 3;
/// `dateTimeSeconds` / `flowStartSeconds`, an IANA element (no enterprise).
const IANA_FLOW_START_SECONDS: u16 = 150;
/// Strings longer than this are cut (one length byte; 255 means "longer").
const MAX_STRING: usize = 254;
/// Datagrams stay inside a 1500-byte Ethernet MTU with room for headers.
pub(crate) const MAX_PACKET: usize = 1400;
/// The shortest time between batches.
pub(crate) const INTERVAL: Duration = Duration::from_secs(300);
/// The descriptors are sent again after this.
const DESCRIPTORS_EVERY: Duration = Duration::from_secs(3600);
/// An unchanged report of a callsign is not sent again within this.
const SAME_AGAIN: Duration = Duration::from_secs(3600);
/// The most reports held for one batch.
const MAX_PENDING: usize = 2000;

/// Who is listening.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Listener {
    pub call: String,
    pub grid: String,
    /// `decodingSoftware`: "Hardrock Panel 0.1.0".
    pub software: String,
}

/// One station heard.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Report {
    pub call: String,
    /// Empty when it gave none and none is known.
    pub grid: String,
    /// The signal's own frequency (dial plus audio, through the transverter), Hz.
    pub freq_hz: u64,
    pub snr_db: i8,
    /// "FT8", "FT4".
    pub mode: String,
    /// When it was heard (its slot's start), Unix seconds.
    pub utc: u32,
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// An IPFIX variable-length string: one length byte, then UTF-8, cut at a
/// character boundary to [`MAX_STRING`] bytes.
fn push_str(out: &mut Vec<u8>, s: &str) {
    let mut n = s.len().min(MAX_STRING);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    out.push(n as u8);
    out.extend_from_slice(&s.as_bytes()[..n]);
}

/// A field specifier: a PSK Reporter element has the enterprise bit set and
/// is followed by the enterprise number; `0xFFFF` is variable length.
fn push_field(out: &mut Vec<u8>, id: u16, len: u16, enterprise: bool) {
    if enterprise {
        push_u16(out, 0x8000 | id);
        push_u16(out, len);
        push_u32(out, ENTERPRISE);
    } else {
        push_u16(out, id);
        push_u16(out, len);
    }
}

/// Close the set begun at `start`: pad it with zeros to a multiple of four
/// bytes and write its length (padding included) after its ID.
fn end_set(out: &mut Vec<u8>, start: usize) {
    while !(out.len() - start).is_multiple_of(4) {
        out.push(0);
    }
    let len = (out.len() - start) as u16;
    out[start + 2..start + 4].copy_from_slice(&len.to_be_bytes());
}

fn begin_set(out: &mut Vec<u8>, id: u16) -> usize {
    let start = out.len();
    push_u16(out, id);
    push_u16(out, 0);
    start
}

/// The receiver information descriptor: an options template of three
/// strings, receiverCallsign (2), receiverLocator (4), decodingSoftware (8).
fn receiver_descriptor(out: &mut Vec<u8>) {
    let start = begin_set(out, SET_OPTIONS_TEMPLATE);
    push_u16(out, TEMPLATE_RECEIVER);
    push_u16(out, 3); // field count
    push_u16(out, 0); // scope field count
    push_field(out, 2, 0xFFFF, true);
    push_field(out, 4, 0xFFFF, true);
    push_field(out, 8, 0xFFFF, true);
    end_set(out, start);
}

/// The sender information descriptor: senderCallsign (1), frequency (5, five
/// bytes), sNR (6, one byte), mode (10), senderLocator (3),
/// informationSource (11, one byte) and flowStartSeconds (IANA 150).
fn sender_descriptor(out: &mut Vec<u8>) {
    let start = begin_set(out, SET_TEMPLATE);
    push_u16(out, TEMPLATE_SENDER);
    push_u16(out, 7); // field count
    push_field(out, 1, 0xFFFF, true);
    push_field(out, 5, 5, true);
    push_field(out, 6, 1, true);
    push_field(out, 10, 0xFFFF, true);
    push_field(out, 3, 0xFFFF, true);
    push_field(out, 11, 1, true);
    push_field(out, IANA_FLOW_START_SECONDS, 4, false);
    end_set(out, start);
}

fn sender_record(out: &mut Vec<u8>, r: &Report) {
    push_str(out, &r.call);
    out.extend_from_slice(&r.freq_hz.min((1 << 40) - 1).to_be_bytes()[3..]);
    out.push(r.snr_db as u8);
    push_str(out, &r.mode);
    push_str(out, &r.grid);
    out.push(1); // informationSource: decoded automatically
    push_u32(out, r.utc);
}

/// One datagram: the header, the descriptors if asked, the receiver record
/// and as many of `reports` as fit in [`MAX_PACKET`]. Returns it and how many
/// reports it took (at least one, if there are any).
///
/// `seq` is the IPFIX sequence number, the data records sent before this
/// datagram; `domain` the observation domain ID, random for the session.
pub(crate) fn encode(
    rx: &Listener,
    reports: &[Report],
    export_utc: u32,
    seq: u32,
    domain: u32,
    descriptors: bool,
) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(MAX_PACKET);
    push_u16(&mut out, 10); // IPFIX version
    push_u16(&mut out, 0); // length, written last
    push_u32(&mut out, export_utc);
    push_u32(&mut out, seq);
    push_u32(&mut out, domain);
    if descriptors {
        receiver_descriptor(&mut out);
        sender_descriptor(&mut out);
    }
    let start = begin_set(&mut out, TEMPLATE_RECEIVER);
    push_str(&mut out, &rx.call);
    push_str(&mut out, &rx.grid);
    push_str(&mut out, &rx.software);
    end_set(&mut out, start);

    let mut used = 0;
    if !reports.is_empty() {
        let start = begin_set(&mut out, TEMPLATE_SENDER);
        for r in reports {
            let before = out.len();
            sender_record(&mut out, r);
            // Room for up to three bytes of padding.
            if used > 0 && out.len() + 3 > MAX_PACKET {
                out.truncate(before);
                break;
            }
            used += 1;
        }
        end_set(&mut out, start);
    }
    let total = out.len() as u16;
    out[2..4].copy_from_slice(&total.to_be_bytes());
    (out, used)
}

/// The reports waiting for the next batch, and what was sent in the last
/// hour.
#[derive(Default)]
pub(crate) struct Batch {
    pending: Vec<Report>,
    /// By callsign: when it was last sent, and with which grid, band and mode.
    sent: HashMap<String, (Instant, String, u64, String)>,
}

/// A band, near enough to tell one report from another (500 kHz).
fn band_of(hz: u64) -> u64 {
    hz / 500_000
}

impl Batch {
    /// Add a report: the strongest of one callsign on one band and mode in a
    /// batch is kept; one sent unchanged within the hour is dropped.
    pub(crate) fn add(&mut self, r: Report, now: Instant) {
        if let Some((at, grid, band, mode)) = self.sent.get(&r.call)
            && now.duration_since(*at) < SAME_AGAIN
            && *grid == r.grid
            && *band == band_of(r.freq_hz)
            && *mode == r.mode
        {
            return;
        }
        let same = |e: &Report| {
            e.call == r.call && band_of(e.freq_hz) == band_of(r.freq_hz) && e.mode == r.mode
        };
        if let Some(e) = self.pending.iter_mut().find(|e| same(e)) {
            // Keep the strongest, and a grid once one is known.
            let grid = if r.grid.is_empty() { e.grid.clone() } else { r.grid.clone() };
            if r.snr_db >= e.snr_db {
                *e = r;
            }
            e.grid = grid;
            return;
        }
        if self.pending.len() < MAX_PENDING {
            self.pending.push(r);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Take the batch to send, and remember it as sent.
    pub(crate) fn take(&mut self, now: Instant) -> Vec<Report> {
        self.sent.retain(|_, (at, ..)| now.duration_since(*at) < SAME_AGAIN);
        let out = std::mem::take(&mut self.pending);
        for r in &out {
            self.sent
                .insert(r.call.clone(), (now, r.grid.clone(), band_of(r.freq_hz), r.mode.clone()));
        }
        out
    }
}

/// The uploader's thread: collects reports and sends a batch every
/// [`INTERVAL`] (from its start, plus `lead`), until its handle goes.
pub(crate) struct Uploader {
    tx: Sender<Report>,
}

impl Uploader {
    /// Start sending as `rx` to `host:port`; what happens is told on `status`.
    pub(crate) fn spawn(
        rx: Listener,
        host: String,
        port: u16,
        status: Sender<Status>,
    ) -> std::io::Result<Self> {
        let (tx, reports) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("pskreporter".into())
            .spawn(move || run(rx, host, port, reports, status))?;
        Ok(Uploader { tx })
    }

    pub(crate) fn report(&self, r: Report) {
        let _ = self.tx.send(r);
    }
}

/// A number that differs from run to run, for the observation domain and the
/// lead.
fn session_random() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    h.finish() as u32
}

fn run(rx: Listener, host: String, port: u16, reports: Receiver<Report>, status: Sender<Status>) {
    let random = session_random();
    let domain = random | 1;
    // Up to 30 s past the five minutes, different each run.
    let lead = Duration::from_millis(u64::from(random % 30_000));
    let mut batch = Batch::default();
    let mut socket: Option<UdpSocket> = None;
    let mut seq: u32 = 0;
    let mut descriptors_left = 3u32;
    let mut descriptors_at = Instant::now();
    let mut due = Instant::now() + INTERVAL + lead;
    let tell = |text: String, ok: bool, n: usize| {
        let _ = status.send(Status { service: Service::PskReporter, text, ok, sent: n });
    };
    loop {
        match reports.recv_timeout(Duration::from_millis(500)) {
            Ok(r) => {
                batch.add(r, Instant::now());
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The handle went (the tick was cleared, or the panel closed):
            // nothing more is sent.
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        if now < due {
            continue;
        }
        due = now + INTERVAL;
        if batch.len() == 0 {
            continue;
        }
        if now.duration_since(descriptors_at) >= DESCRIPTORS_EVERY {
            descriptors_left = 3;
            descriptors_at = now;
        }
        let todo = batch.take(now);
        match send(&host, port, &mut socket, &rx, &todo, &mut seq, domain, &mut descriptors_left) {
            Ok(n) => tell(format!("{n} spot{} sent", if n == 1 { "" } else { "s" }), true, n),
            Err(e) => tell(e, false, 0),
        }
    }
}

/// Send `todo` in as many datagrams as it takes, from the one socket (the
/// same source port every time, as PSK Reporter asks).
#[allow(clippy::too_many_arguments)]
fn send(
    host: &str,
    port: u16,
    socket: &mut Option<UdpSocket>,
    rx: &Listener,
    todo: &[Report],
    seq: &mut u32,
    domain: u32,
    descriptors_left: &mut u32,
) -> Result<usize, String> {
    let addr: SocketAddr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("{host}: no IPv4 address"))?;
    if socket.is_none() {
        *socket = Some(UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?);
    }
    let sock = socket.as_ref().expect("bound above");
    let mut sent = 0;
    while sent < todo.len() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        let (pkt, used) = encode(rx, &todo[sent..], now, *seq, domain, *descriptors_left > 0);
        sock.send_to(&pkt, addr).map_err(|e| format!("{host}: {e}"))?;
        *descriptors_left = descriptors_left.saturating_sub(1);
        // The receiver record and the sender records.
        *seq = seq.wrapping_add(used as u32 + 1);
        sent += used;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx() -> Listener {
        Listener {
            call: "AJ3G".into(),
            grid: "FN20".into(),
            software: "Hardrock Panel 0.1.0".into(),
        }
    }

    fn report(call: &str, snr: i8) -> Report {
        Report {
            call: call.into(),
            grid: "EN37".into(),
            freq_hz: 14_075_234,
            snr_db: snr,
            mode: "FT8".into(),
            utc: 1_792_545_330,
        }
    }

    #[test]
    fn the_descriptors_are_byte_for_byte_the_published_layout() {
        let mut d = Vec::new();
        receiver_descriptor(&mut d);
        #[rustfmt::skip]
        let want: &[u8] = &[
            0x00, 0x03, 0x00, 0x24, // options template set, 36 bytes
            0x99, 0x92, 0x00, 0x03, 0x00, 0x00, // template 0x9992, 3 fields, no scope
            0x80, 0x02, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // receiverCallsign
            0x80, 0x04, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // receiverLocator
            0x80, 0x08, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // decodingSoftware
            0x00, 0x00, // padding
        ];
        assert_eq!(d, want);

        let mut d = Vec::new();
        sender_descriptor(&mut d);
        #[rustfmt::skip]
        let want: &[u8] = &[
            0x00, 0x02, 0x00, 0x3C, // template set, 60 bytes
            0x99, 0x93, 0x00, 0x07, // template 0x9993, 7 fields
            0x80, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // senderCallsign
            0x80, 0x05, 0x00, 0x05, 0x00, 0x00, 0x76, 0x8F, // frequency, 5 bytes
            0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x76, 0x8F, // sNR, 1 byte
            0x80, 0x0A, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // mode
            0x80, 0x03, 0xFF, 0xFF, 0x00, 0x00, 0x76, 0x8F, // senderLocator
            0x80, 0x0B, 0x00, 0x01, 0x00, 0x00, 0x76, 0x8F, // informationSource, 1 byte
            0x00, 0x96, 0x00, 0x04, // flowStartSeconds (IANA 150), 4 bytes
        ];
        assert_eq!(d, want);
    }

    #[test]
    fn a_datagram_is_byte_for_byte_right() {
        let (pkt, used) =
            encode(&rx(), &[report("W9XYZ", -12)], 0x6AF6_D2C0, 7, 0x1234_5678, false);
        assert_eq!(used, 1);
        let mut want: Vec<u8> = vec![
            0x00, 0x0A, 0x00, 0x00, // version 10, length (below)
            0x6A, 0xF6, 0xD2, 0xC0, // export time
            0x00, 0x00, 0x00, 0x07, // sequence number
            0x12, 0x34, 0x56, 0x78, // observation domain
            // The receiver record: 4 + 5 + 5 + 21 = 35 bytes, padded to 36.
            0x99, 0x92, 0x00, 0x24,
        ];
        want.extend_from_slice(b"\x04AJ3G\x04FN20\x14Hardrock Panel 0.1.0");
        want.push(0);
        // The sender set: 4 + 6 + 5 + 1 + 4 + 5 + 1 + 4 = 30 bytes, padded to 32.
        want.extend_from_slice(&[0x99, 0x93, 0x00, 0x20]);
        want.extend_from_slice(b"\x05W9XYZ");
        want.extend_from_slice(&[0x00, 0x00, 0xD6, 0xC5, 0x62]); // 14 075 234 Hz
        want.push(0xF4); // -12 dB
        want.extend_from_slice(b"\x03FT8\x04EN37");
        want.push(0x01);
        want.extend_from_slice(&[0x6A, 0xD8, 0x12, 0x32]); // heard at 1 792 545 330
        want.extend_from_slice(&[0, 0]);
        let n = want.len() as u16;
        want[2..4].copy_from_slice(&n.to_be_bytes());
        assert_eq!(pkt, want);
        assert_eq!(n, 16 + 36 + 32);
    }

    #[test]
    fn the_first_datagrams_carry_the_descriptors() {
        let (pkt, _) = encode(&rx(), &[report("W9XYZ", -12)], 0, 0, 1, true);
        let mut at = 16;
        let mut sets = Vec::new();
        while at < pkt.len() {
            let id = u16::from_be_bytes([pkt[at], pkt[at + 1]]);
            let len = u16::from_be_bytes([pkt[at + 2], pkt[at + 3]]) as usize;
            assert_eq!(len % 4, 0);
            sets.push(id);
            at += len;
        }
        assert_eq!(at, pkt.len());
        assert_eq!(sets, [3, 2, 0x9992, 0x9993]);
        assert_eq!(u16::from_be_bytes([pkt[2], pkt[3]]) as usize, pkt.len());
    }

    #[test]
    fn a_big_batch_goes_in_several_datagrams() {
        let all: Vec<Report> = (0..300).map(|i| report(&format!("W{i}XYZ"), -10)).collect();
        let mut sent = 0;
        let mut n = 0;
        while sent < all.len() {
            let (pkt, used) = encode(&rx(), &all[sent..], 0, 0, 1, n == 0);
            assert!(pkt.len() <= MAX_PACKET, "{} bytes", pkt.len());
            assert!(used > 0);
            sent += used;
            n += 1;
        }
        assert!(n > 1);
    }

    #[test]
    fn a_long_string_is_cut_at_a_character() {
        let mut out = Vec::new();
        push_str(&mut out, &"é".repeat(200));
        assert_eq!(out[0], 254);
        assert_eq!(out.len(), 255);
        assert!(std::str::from_utf8(&out[1..]).is_ok());
    }

    #[test]
    fn a_callsign_goes_once_a_batch_and_once_an_hour_unchanged() {
        let t = Instant::now();
        let mut b = Batch::default();
        b.add(report("W9XYZ", -15), t);
        b.add(report("W9XYZ", -8), t);
        b.add(Report { grid: String::new(), ..report("W9XYZ", -3) }, t);
        b.add(report("K1ABC", -20), t);
        assert_eq!(b.len(), 2);
        let sent = b.take(t);
        assert_eq!(sent[0].snr_db, -3);
        assert_eq!(sent[0].grid, "EN37", "the grid heard earlier is kept");
        // The same again within the hour: not sent.
        b.add(report("W9XYZ", -1), t + Duration::from_secs(600));
        assert_eq!(b.len(), 0);
        // On another band, it is.
        b.add(Report { freq_hz: 7_075_000, ..report("W9XYZ", -1) }, t);
        assert_eq!(b.len(), 1);
        // An hour on, the same report goes again.
        b.take(t);
        b.add(report("K1ABC", -20), t + Duration::from_secs(3601));
        assert_eq!(b.len(), 1);
    }
}
