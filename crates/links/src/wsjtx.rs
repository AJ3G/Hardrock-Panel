// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-wsjtx/src/lib.rs and msg.rs, GPL-3.0-or-later. The WSPR
// decode message is this crate's own, from WSJT-X's NetworkMessage.hpp.

//! The **WSJT-X UDP messages**: the panel announcing its own FT8, FT4 and
//! WSPR decodes the way WSJT-X does, so GridTracker, JTAlert, N1MM+ and Log4OM
//! work with it unchanged. They listen on UDP port 2237.
//!
//! The format is Qt's `QDataStream` (big-endian) behind a four-word header, as
//! WSJT-X's `NetworkMessage.hpp` defines it. UDP gives no feedback, so a field
//! of the wrong width does not fail, it silently stops a logger seeing
//! anything: the tests read every field back.
//!
//! Output only: nothing is read from the socket.
//!
//! | Qt type      | on the wire                                              |
//! |--------------|----------------------------------------------------------|
//! | `QString`    | `u32` byte count then UTF-8; `0xFFFFFFFF` means null      |
//! | `bool`       | one byte                                                  |
//! | `QTime`      | `u32` milliseconds since midnight                         |
//! | `QDateTime`  | `i64` Julian day, `u32` ms, `u8` time spec (1 = UTC)      |
//! | `double`     | 8 bytes IEEE-754                                          |

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use tracing::{debug, info};

/// The conventional WSJT-X UDP port.
pub const DEFAULT_PORT: u16 = 2237;

const MAGIC: u32 = 0xadbc_cbda;
/// The schema WSJT-X 2.x speaks.
const SCHEMA: u32 = 3;

const T_HEARTBEAT: u32 = 0;
const T_STATUS: u32 = 1;
const T_DECODE: u32 = 2;
const T_CLEAR: u32 = 3;
const T_CLOSE: u32 = 6;
const T_WSPR_DECODE: u32 = 10;
const T_LOGGED_ADIF: u32 = 12;

/// One decoded message, as the clients' decode window shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodeInfo {
    /// False replays an old decode.
    pub new: bool,
    /// Unix seconds of the slot the message was decoded from.
    pub slot_utc: i64,
    pub snr_db: i32,
    /// Time offset from the slot start, in seconds.
    pub dt: f64,
    /// Audio offset within the passband, in Hz.
    pub audio_hz: u32,
    /// WSJT-X's one-character mode code: `~` FT8, `+` FT4, `#` JT65, `@` JT9,
    /// `:` Q65, or the mode's name.
    pub mode: String,
    pub message: String,
    pub low_confidence: bool,
}

/// One WSPR spot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WsprDecodeInfo {
    pub new: bool,
    pub slot_utc: i64,
    pub snr_db: i32,
    pub dt: f64,
    /// The signal's RF frequency in Hz (dial plus audio offset).
    pub freq_hz: u64,
    pub drift_hz: i32,
    pub callsign: String,
    pub grid: String,
    pub power_dbm: i32,
}

/// Where this station is and what it is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusInfo {
    pub dial_hz: u64,
    pub mode: String,
    pub dx_call: String,
    pub report: String,
    pub tx_enabled: bool,
    pub transmitting: bool,
    pub decoding: bool,
    pub rx_df_hz: u32,
    pub tx_df_hz: u32,
    pub de_call: String,
    pub de_grid: String,
    pub dx_grid: String,
    pub tx_watchdog: bool,
    /// Transmit/receive period in seconds (FT8 15, FT4 7).
    pub tr_period_s: u32,
    pub tx_message: String,
}

fn u32_(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn i32_(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn u64_(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn f64_(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn bool_(out: &mut Vec<u8>, v: bool) {
    out.push(u8::from(v));
}

/// A `QString`; empty is sent as null, as WSJT-X sends its unset fields.
fn str_(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        u32_(out, 0xFFFF_FFFF);
        return;
    }
    u32_(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

/// A `QTime`: milliseconds since midnight UTC.
fn time_(out: &mut Vec<u8>, unix: i64) {
    u32_(out, (unix.rem_euclid(86_400) * 1000) as u32);
}

fn header(out: &mut Vec<u8>, kind: u32, id: &str) {
    u32_(out, MAGIC);
    u32_(out, SCHEMA);
    u32_(out, kind);
    str_(out, id);
}

pub fn heartbeat(id: &str, version: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    header(&mut out, T_HEARTBEAT, id);
    u32_(&mut out, SCHEMA);
    str_(&mut out, version);
    str_(&mut out, "");
    out
}

pub fn decode(id: &str, d: &DecodeInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    header(&mut out, T_DECODE, id);
    bool_(&mut out, d.new);
    time_(&mut out, d.slot_utc);
    i32_(&mut out, d.snr_db);
    f64_(&mut out, d.dt);
    u32_(&mut out, d.audio_hz);
    str_(&mut out, &d.mode);
    str_(&mut out, &d.message);
    bool_(&mut out, d.low_confidence);
    bool_(&mut out, false); // off air
    out
}

pub fn wspr_decode(id: &str, d: &WsprDecodeInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    header(&mut out, T_WSPR_DECODE, id);
    bool_(&mut out, d.new);
    time_(&mut out, d.slot_utc);
    i32_(&mut out, d.snr_db);
    f64_(&mut out, d.dt);
    u64_(&mut out, d.freq_hz);
    i32_(&mut out, d.drift_hz);
    str_(&mut out, &d.callsign);
    str_(&mut out, &d.grid);
    i32_(&mut out, d.power_dbm);
    bool_(&mut out, false); // off air
    out
}

pub fn status(id: &str, s: &StatusInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    header(&mut out, T_STATUS, id);
    u64_(&mut out, s.dial_hz);
    str_(&mut out, &s.mode);
    str_(&mut out, &s.dx_call);
    str_(&mut out, &s.report);
    str_(&mut out, &s.mode); // tx mode
    bool_(&mut out, s.tx_enabled);
    bool_(&mut out, s.transmitting);
    bool_(&mut out, s.decoding);
    u32_(&mut out, s.rx_df_hz);
    u32_(&mut out, s.tx_df_hz);
    str_(&mut out, &s.de_call);
    str_(&mut out, &s.de_grid);
    str_(&mut out, &s.dx_grid);
    bool_(&mut out, s.tx_watchdog);
    str_(&mut out, ""); // sub-mode
    bool_(&mut out, false); // fast mode
    out.push(0); // special operation mode: none
    u32_(&mut out, 0); // frequency tolerance
    u32_(&mut out, s.tr_period_s);
    str_(&mut out, "Default"); // configuration name
    str_(&mut out, &s.tx_message);
    out
}

pub fn clear(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    header(&mut out, T_CLEAR, id);
    out
}

pub fn close(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    header(&mut out, T_CLOSE, id);
    out
}

/// A logged contact as one bare ADIF record (the fields and `<EOR>`).
pub fn logged_adif(id: &str, adif: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(adif.len() + 32);
    header(&mut out, T_LOGGED_ADIF, id);
    str_(&mut out, adif);
    out
}

/// A configured sender. Every send is fire-and-forget: an error is logged and
/// dropped, because a logger that is not running must never hold up the
/// panel.
pub struct WsjtxUdp {
    sock: UdpSocket,
    dest: SocketAddr,
    id: String,
}

impl WsjtxUdp {
    /// Bind a socket and resolve `host:port` (unicast or multicast). `id` is
    /// the station id clients show, "WSJT-X" for programs that expect it.
    pub fn start(host: &str, port: u16, id: &str) -> Result<Self, String> {
        let host = if host.trim().is_empty() { "127.0.0.1" } else { host.trim() };
        let dest = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("{host}:{port}: {e}"))?
            .next()
            .ok_or_else(|| format!("{host}:{port}: no address"))?;
        let sock = UdpSocket::bind(if dest.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
            .map_err(|e| e.to_string())?;
        if dest.ip().is_multicast() {
            let _ = sock.set_multicast_ttl_v4(2);
        }
        info!(%dest, id, "WSJT-X UDP started");
        Ok(WsjtxUdp { sock, dest, id: id.to_string() })
    }

    pub fn dest(&self) -> SocketAddr {
        self.dest
    }

    fn send(&self, packet: Vec<u8>) {
        if let Err(e) = self.sock.send_to(&packet, self.dest) {
            debug!(dest = %self.dest, error = %e, "WSJT-X UDP send failed");
        }
    }

    /// "Still here", about every 15 s.
    pub fn heartbeat(&self, version: &str) {
        self.send(heartbeat(&self.id, version));
    }
    pub fn decode(&self, d: &DecodeInfo) {
        self.send(decode(&self.id, d));
    }
    pub fn wspr_decode(&self, d: &WsprDecodeInfo) {
        self.send(wspr_decode(&self.id, d));
    }
    pub fn status(&self, s: &StatusInfo) {
        self.send(status(&self.id, s));
    }
    pub fn clear(&self) {
        self.send(clear(&self.id));
    }
    pub fn logged_adif(&self, adif: &str) {
        self.send(logged_adif(&self.id, adif));
    }
    pub fn close(&self) {
        self.send(close(&self.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads fields back the way a client does.
    struct Reader<'a> {
        b: &'a [u8],
        at: usize,
    }

    impl<'a> Reader<'a> {
        fn new(b: &'a [u8]) -> Self {
            Reader { b, at: 0 }
        }
        fn u8(&mut self) -> u8 {
            self.at += 1;
            self.b[self.at - 1]
        }
        fn bool(&mut self) -> bool {
            self.u8() != 0
        }
        fn u32(&mut self) -> u32 {
            let v = u32::from_be_bytes(self.b[self.at..self.at + 4].try_into().unwrap());
            self.at += 4;
            v
        }
        fn i32(&mut self) -> i32 {
            self.u32() as i32
        }
        fn u64(&mut self) -> u64 {
            let v = u64::from_be_bytes(self.b[self.at..self.at + 8].try_into().unwrap());
            self.at += 8;
            v
        }
        fn f64(&mut self) -> f64 {
            f64::from_bits(self.u64())
        }
        fn str(&mut self) -> Option<String> {
            let n = self.u32();
            if n == 0xFFFF_FFFF {
                return None;
            }
            let s = String::from_utf8(self.b[self.at..self.at + n as usize].to_vec()).unwrap();
            self.at += n as usize;
            Some(s)
        }
        fn header(&mut self) -> (u32, u32, Option<String>) {
            assert_eq!(self.u32(), MAGIC);
            let schema = self.u32();
            let kind = self.u32();
            (schema, kind, self.str())
        }
        fn at_end(&self) -> bool {
            self.at == self.b.len()
        }
    }

    #[test]
    fn every_message_opens_with_the_same_header() {
        for (packet, kind) in [
            (heartbeat("HRP", "0.1.0"), T_HEARTBEAT),
            (clear("HRP"), T_CLEAR),
            (close("HRP"), T_CLOSE),
            (logged_adif("HRP", "<CALL:5>W9XYZ<EOR>"), T_LOGGED_ADIF),
        ] {
            let mut r = Reader::new(&packet);
            let (schema, got, id) = r.header();
            assert_eq!(schema, SCHEMA);
            assert_eq!(got, kind);
            assert_eq!(id.as_deref(), Some("HRP"));
        }
    }

    #[test]
    fn a_decode_reads_back_field_for_field() {
        let d = DecodeInfo {
            new: true,
            slot_utc: 1_700_000_000,
            snr_db: -13,
            dt: 0.25,
            audio_hz: 1234,
            mode: "~".into(),
            message: "CQ W9XYZ EM48".into(),
            low_confidence: false,
        };
        let packet = decode("WSJT-X", &d);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_DECODE);
        assert!(r.bool());
        assert_eq!(r.u32(), ((22 * 3600 + 13 * 60 + 20) * 1000) as u32);
        assert_eq!(r.i32(), -13);
        assert_eq!(r.f64(), 0.25);
        assert_eq!(r.u32(), 1234);
        assert_eq!(r.str().as_deref(), Some("~"));
        assert_eq!(r.str().as_deref(), Some("CQ W9XYZ EM48"));
        assert!(!r.bool());
        assert!(!r.bool());
        assert!(r.at_end());
    }

    #[test]
    fn a_wspr_spot_reads_back_field_for_field() {
        let d = WsprDecodeInfo {
            new: true,
            slot_utc: 1_700_000_040,
            snr_db: -22,
            dt: 0.5,
            freq_hz: 14_097_055,
            drift_hz: -1,
            callsign: "K1ABC".into(),
            grid: "FN42".into(),
            power_dbm: 37,
        };
        let packet = wspr_decode("WSJT-X", &d);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_WSPR_DECODE);
        assert!(r.bool());
        assert_eq!(r.u32(), ((22 * 3600 + 14 * 60) * 1000) as u32);
        assert_eq!(r.i32(), -22);
        assert_eq!(r.f64(), 0.5);
        assert_eq!(r.u64(), 14_097_055);
        assert_eq!(r.i32(), -1);
        assert_eq!(r.str().as_deref(), Some("K1ABC"));
        assert_eq!(r.str().as_deref(), Some("FN42"));
        assert_eq!(r.i32(), 37);
        assert!(!r.bool());
        assert!(r.at_end());
    }

    #[test]
    fn status_carries_the_full_schema_3_field_list() {
        let s = StatusInfo {
            dial_hz: 14_074_000,
            mode: "FT8".into(),
            decoding: true,
            de_call: "AJ3G".into(),
            tr_period_s: 15,
            ..StatusInfo::default()
        };
        let packet = status("WSJT-X", &s);
        let mut r = Reader::new(&packet);
        assert_eq!(r.header().1, T_STATUS);
        assert_eq!(r.u64(), 14_074_000);
        assert_eq!(r.str().as_deref(), Some("FT8"));
        assert_eq!(r.str(), None);
        assert_eq!(r.str(), None);
        assert_eq!(r.str().as_deref(), Some("FT8"));
        assert!(!r.bool());
        assert!(!r.bool());
        assert!(r.bool());
        assert_eq!(r.u32(), 0);
        assert_eq!(r.u32(), 0);
        assert_eq!(r.str().as_deref(), Some("AJ3G"));
        assert_eq!(r.str(), None);
        assert_eq!(r.str(), None);
        assert!(!r.bool());
        assert_eq!(r.str(), None);
        assert!(!r.bool());
        assert_eq!(r.u8(), 0);
        assert_eq!(r.u32(), 0);
        assert_eq!(r.u32(), 15);
        assert_eq!(r.str().as_deref(), Some("Default"));
        assert_eq!(r.str(), None);
        assert!(r.at_end());
    }

    #[test]
    fn datagrams_arrive_at_a_listener() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let port = rx.local_addr().unwrap().port();
        let tx = WsjtxUdp::start("127.0.0.1", port, "HRP").unwrap();
        tx.heartbeat("0.1.0");
        let mut buf = [0u8; 512];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(Reader::new(&buf[..n]).header().1, T_HEARTBEAT);
    }
}
