//! The radio on a thread of its own: open the port, read, and reopen it if the
//! radio stops answering.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use tracing::{debug, info, warn};

use crate::elad;
use crate::{RigConfig, RigStatus, SettingRow};

/// How often power and SWR are read. Three short frames at 38400 baud take a
/// few milliseconds each way.
const POLL: Duration = Duration::from_millis(150);
/// How often the dial and mode are read.
const DIAL_POLL: Duration = Duration::from_secs(1);
/// How often the settings are read again on their own, between transmissions.
const SETTINGS_POLL: Duration = Duration::from_secs(10);
/// How long to wait for an answer.
const ANSWER: Duration = Duration::from_millis(300);
/// Missed answers in a row before the link is called lost and reopened.
const MISSES: u32 = 4;
const RETRY_MAX: Duration = Duration::from_secs(2);
const OPEN_PINGS: u32 = 3;

enum Msg {
    /// Read every setting again now.
    ReadSettings,
}

/// A running link to the radio. Dropping it closes the port.
pub struct RigLink {
    tx: Option<Sender<Msg>>,
    status: Arc<Mutex<RigStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl RigLink {
    pub fn start(cfg: RigConfig) -> RigLink {
        let (tx, rx) = bounded(4);
        let status = Arc::new(Mutex::new(RigStatus {
            configured: true,
            model: cfg.model,
            baud: cfg.baud,
            ..RigStatus::default()
        }));
        let st = status.clone();
        let thread =
            thread::Builder::new().name("rigcat".into()).spawn(move || worker(cfg, rx, st)).ok();
        RigLink { tx: Some(tx), status, thread }
    }

    pub fn status(&self) -> RigStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Read every setting again.
    pub fn read_settings(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::ReadSettings);
        }
    }
}

impl Drop for RigLink {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn set(status: &Mutex<RigStatus>, f: impl FnOnce(&mut RigStatus)) {
    f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
}

#[derive(Debug)]
enum Error {
    Serial(serialport::Error),
    Io(std::io::Error),
    /// No answer in time.
    Timeout,
    /// The radio answered `?;`: it doesn't know the command.
    Refused,
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// A radio on a byte stream: a serial port in use, anything `Read + Write` in
/// tests.
struct Rig<P: Read + Write> {
    port: P,
    buf: String,
}

impl<P: Read + Write> Rig<P> {
    fn new(port: P) -> Self {
        Rig { port, buf: String::new() }
    }

    /// Send one READ and return the body of its answer. Any other answer that
    /// arrives on the way is handed to `other`.
    fn query(&mut self, frame: &str, mut other: impl FnMut(&str)) -> Result<String, Error> {
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #5): only READ frames
        // reach the radio. See `elad::is_read` for why.
        if !elad::is_read(frame) {
            warn!("rigcat: refused to send {frame}: not a read");
            return Err(Error::Refused);
        }
        self.port.write_all(frame.as_bytes())?;
        self.port.flush()?;
        let want = elad::reply_prefix(frame);
        let until = Instant::now() + ANSWER;
        let mut chunk = [0u8; 256];
        loop {
            while let Some(i) = self.buf.find(';') {
                let msg: String = self.buf.drain(..=i).collect();
                let msg = msg.trim_matches(|c: char| c == ';' || c == '\r' || c == '\n');
                if msg == "?" {
                    return Err(Error::Refused);
                }
                if let Some(body) = msg.strip_prefix(want) {
                    return Ok(body.to_string());
                }
                other(msg);
            }
            if Instant::now() >= until {
                return Err(Error::Timeout);
            }
            match self.port.read(&mut chunk) {
                Ok(0) => {}
                Ok(n) => self.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn open(cfg: &RigConfig) -> Result<Rig<Box<dyn serialport::SerialPort>>, Error> {
    let port = serialport::new(&cfg.path, cfg.baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(Error::Serial)?;
    let _ = port.clear(serialport::ClearBuffer::All);
    let mut rig = Rig::new(port);
    let mut last = Error::Timeout;
    for _ in 0..OPEN_PINGS {
        match rig.query(elad::PING, |_| {}) {
            Ok(_) => return Ok(rig),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn port_listed(path: &str) -> bool {
    serialport::available_ports()
        .map(|v| v.iter().any(|p| p.port_name.eq_ignore_ascii_case(path)))
        .unwrap_or(false)
}

/// What the operator is told when the port won't open or the radio won't
/// answer.
fn open_error(cfg: &RigConfig, e: &Error) -> String {
    let path = &cfg.path;
    let busy = || {
        format!(
            "{path} is in use by another program: SDRoxide, ELAD's FDM-SW2, a logger, or a \
             second copy of this panel. Close it and this connects by itself."
        )
    };
    match e {
        // serialport reports a port another program holds as NoDevice, the
        // same as one that isn't there; a port still listed is busy.
        Error::Serial(se) if se.kind() == serialport::ErrorKind::NoDevice && port_listed(path) => {
            busy()
        }
        Error::Serial(se)
            if matches!(
                se.kind(),
                serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
            ) =>
        {
            busy()
        }
        Error::Serial(se)
            if matches!(
                se.kind(),
                serialport::ErrorKind::NoDevice
                    | serialport::ErrorKind::Io(std::io::ErrorKind::NotFound)
            ) =>
        {
            format!("{path} not found. Check the radio's CAT USB lead.")
        }
        Error::Timeout | Error::Refused => format!(
            "{path} is open but the radio isn't answering. Is it on, and is menu 70 CAT BAUD \
             set to {}?",
            cfg.baud
        ),
        Error::Serial(se) => format!("{path}: {se}"),
        Error::Io(io) => format!("{path}: {io}"),
    }
}

fn worker(cfg: RigConfig, rx: Receiver<Msg>, status: Arc<Mutex<RigStatus>>) {
    if cfg.path.trim().is_empty() {
        set(&status, |s| s.error = Some("No serial port chosen".into()));
        while rx.recv().is_ok() {}
        return;
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        match open(&cfg) {
            Ok(rig) => {
                info!(port = %cfg.path, baud = cfg.baud, "rigcat: connected");
                backoff = Duration::from_secs(1);
                set(&status, |s| {
                    s.connected = true;
                    s.error = None;
                });
                if !run(rig, &rx, &status) {
                    return;
                }
                set(&status, |s| {
                    *s = RigStatus {
                        configured: true,
                        model: s.model,
                        baud: s.baud,
                        settings: std::mem::take(&mut s.settings),
                        settings_read: s.settings_read,
                        error: Some("The radio stopped answering".into()),
                        ..RigStatus::default()
                    }
                });
            }
            Err(e) => {
                debug!("rigcat: {e:?}");
                set(&status, |s| {
                    s.connected = false;
                    s.error = Some(open_error(&cfg, &e));
                });
            }
        }
        let until = Instant::now() + backoff;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
                Ok(_) => {}
            }
        }
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

/// Fold one answer into the status.
fn apply(s: &mut RigStatus, prefix: &str, body: &str) {
    match prefix {
        "FP" => s.fwd = elad::parse_watts(body),
        "RP" => s.rfl = elad::parse_watts(body),
        "WR" => s.swr = elad::parse_swr(body),
        "FA" => s.freq_hz = elad::parse_freq(body).or(s.freq_hz),
        "MD" => s.mode = body.chars().next().and_then(elad::mode_name),
        "TP" => s.tx_power = elad::describe("TP", body),
        _ => {}
    }
}

/// An answer that turned up while waiting for a different one.
fn stray(status: &Mutex<RigStatus>, msg: &str) {
    if msg.len() >= 2 && msg.is_char_boundary(2) {
        let (p, b) = msg.split_at(2);
        set(status, |s| apply(s, p, b));
    }
}

/// Talk to an open radio until it stops answering (`true`) or the link is
/// dropped (`false`).
fn run<P: Read + Write>(mut rig: Rig<P>, rx: &Receiver<Msg>, status: &Mutex<RigStatus>) -> bool {
    let mut misses = 0;
    let mut next_dial = Instant::now();
    let mut next_settings = Instant::now();
    let (mut settings_once, mut asked) = (false, false);
    loop {
        match rx.recv_timeout(POLL) {
            Err(RecvTimeoutError::Disconnected) => return false,
            Ok(Msg::ReadSettings) => {
                next_settings = Instant::now();
                asked = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let mut answered = false;
        let mut ask = |rig: &mut Rig<P>, frame: &str| -> Option<String> {
            match rig.query(frame, |m| stray(status, m)) {
                Ok(body) => {
                    answered = true;
                    let p = elad::reply_prefix(frame);
                    set(status, |s| apply(s, p, &body));
                    Some(body)
                }
                Err(Error::Refused) => {
                    answered = true;
                    Some("?".into())
                }
                Err(_) => None,
            }
        };
        for f in elad::TELEMETRY {
            if ask(&mut rig, f).is_none() {
                // A reading that didn't come is not the last one seen.
                set(status, |s| match f {
                    "FP;" => s.fwd = None,
                    "RP;" => s.rfl = None,
                    _ => s.swr = None,
                });
            }
        }
        if Instant::now() >= next_dial {
            next_dial = Instant::now() + DIAL_POLL;
            for f in elad::DIAL {
                ask(&mut rig, f);
            }
        }
        // The settings take a dozen round trips. Read at once when the port
        // opens and when asked; the read every few seconds waits while the
        // radio transmits, when the power readings matter more.
        let keyed = status.lock().unwrap_or_else(|e| e.into_inner()).keyed();
        if Instant::now() >= next_settings && (!keyed || !settings_once || asked) {
            settings_once = true;
            asked = false;
            next_settings = Instant::now() + SETTINGS_POLL;
            let rows: Vec<SettingRow> = elad::SETTINGS
                .iter()
                .map(|&item| {
                    let raw = ask(&mut rig, item.cmd);
                    let value = raw.as_deref().and_then(|b| {
                        if b == "?" {
                            None
                        } else {
                            elad::describe(elad::reply_prefix(item.cmd), b)
                        }
                    });
                    let raw = raw.map(|b| {
                        if b == "?" {
                            "?".into()
                        } else {
                            format!("{}{b}", elad::reply_prefix(item.cmd))
                        }
                    });
                    SettingRow { item, value, raw }
                })
                .collect();
            set(status, |s| {
                s.settings = rows;
                s.settings_read = Some(Instant::now());
            });
        }
        if answered {
            misses = 0;
        } else {
            misses += 1;
            if misses >= MISSES {
                warn!("rigcat: the radio stopped answering");
                return true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A pretend DUO: answers each READ from a table, and records what it was
    /// sent.
    struct FakeDuo {
        sent: Vec<String>,
        out: VecDeque<u8>,
        pending: String,
    }

    impl FakeDuo {
        fn new() -> Self {
            FakeDuo { sent: vec![], out: VecDeque::new(), pending: String::new() }
        }
    }

    impl Write for FakeDuo {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.pending.push_str(&String::from_utf8_lossy(b));
            while let Some(i) = self.pending.find(';') {
                let cmd: String = self.pending.drain(..=i).collect();
                let reply = match cmd.as_str() {
                    "FA;" => "FA00014074000;",
                    "FP;" => "FP 006091;",
                    "RP;" => "RP 000001;",
                    "WR;" => "WR0 01.02;",
                    "TP;" => "TP09;",
                    "MD;" => "MD2;",
                    _ => "?;",
                };
                self.out.extend(reply.bytes());
                self.sent.push(cmd);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeDuo {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = b.len().min(self.out.len());
            for (i, byte) in self.out.drain(..n).enumerate() {
                b[i] = byte;
            }
            Ok(n)
        }
    }

    #[test]
    fn a_read_returns_its_own_answer_and_a_refusal_is_said() {
        let mut rig = Rig::new(FakeDuo::new());
        assert_eq!(rig.query("FP;", |_| {}).unwrap(), " 006091");
        assert_eq!(rig.query("TP;", |_| {}).unwrap(), "09");
        assert!(matches!(rig.query("VS;", |_| {}), Err(Error::Refused)));
    }

    #[test]
    fn a_frame_that_is_not_a_read_never_reaches_the_radio() {
        let mut rig = Rig::new(FakeDuo::new());
        for f in ["TX;", "RX;", "TP08;", "RC;"] {
            assert!(matches!(rig.query(f, |_| {}), Err(Error::Refused)));
        }
        assert!(rig.port.sent.is_empty());
    }

    #[test]
    fn answers_fill_the_status() {
        let mut s = RigStatus::default();
        apply(&mut s, "FP", " 006091");
        apply(&mut s, "RP", " 000001");
        apply(&mut s, "WR", "0 01.02");
        apply(&mut s, "FA", "00007074000");
        apply(&mut s, "TP", "09");
        assert!(s.keyed());
        assert_eq!(s.fwd_w(), Some(6.091));
        assert_eq!(s.rfl_w(), Some(0.001));
        assert_eq!(s.swr_ratio(), Some(1.02));
        assert_eq!(s.freq_hz, Some(7_074_000));
        assert_eq!(s.tx_power.as_deref(), Some("MAX"));
        // In receive the radio flags its readings: not keyed, nothing shown.
        apply(&mut s, "FP", "!000000");
        apply(&mut s, "WR", "0!00.00");
        assert!(!s.keyed());
        assert_eq!(s.fwd_w(), None);
        assert_eq!(s.swr_ratio(), None);
    }
}
