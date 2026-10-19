//! The radio on a thread of its own: open the port, read, run TUNE, and
//! reopen the port if the radio stops answering.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use tracing::{debug, info, warn};

use crate::protocol::{Frame, FrameKind, Incoming, Protocol, Reply};
use crate::{RigConfig, RigStatus, SettingRow, Tuning};

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
/// How long the panel may go without saying it still wants the TUNE carrier
/// before the link unkeys by itself (interlock #6). The panel says so ten
/// times a second.
const TUNE_ALIVE: Duration = Duration::from_secs(1);
/// The longest TUNE the link will key, whatever it is asked for.
pub const TUNE_MAX: Duration = Duration::from_secs(60);

enum Msg {
    /// Read every setting again now.
    ReadSettings,
    /// Set the radio's transmit power step (menu 34 on the FDM-DUO).
    SetPower(u8),
    /// Key the TUNE carrier for at most this long.
    TuneStart(Duration),
    /// The panel still wants the carrier.
    TuneAlive,
    /// Unkey, and why.
    TuneStop(Option<String>),
}

/// A running link to the radio. Dropping it unkeys any TUNE and closes the
/// port.
pub struct RigLink {
    tx: Option<Sender<Msg>>,
    status: Arc<Mutex<RigStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl RigLink {
    pub fn start(cfg: RigConfig) -> RigLink {
        let (tx, rx) = bounded(16);
        let proto = cfg.model.protocol();
        let status = Arc::new(Mutex::new(RigStatus {
            configured: true,
            model: cfg.model,
            baud: cfg.baud,
            power_steps: proto.power_steps(),
            power_name: proto.power_name(),
            ..RigStatus::default()
        }));
        let st = status.clone();
        let thread = thread::Builder::new()
            .name("rigcat".into())
            .spawn(move || worker(cfg, proto, rx, st))
            .ok();
        RigLink { tx: Some(tx), status, thread }
    }

    pub fn status(&self) -> RigStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn send(&self, m: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(m);
        }
    }

    /// Read every setting again.
    pub fn read_settings(&self) {
        self.send(Msg::ReadSettings);
    }

    /// Set the radio's transmit power step, by its code.
    pub fn set_power(&self, code: u8) {
        self.send(Msg::SetPower(code));
    }

    /// Key the radio's TUNE carrier for at most `max` (and never more than
    /// [`TUNE_MAX`]). The caller must call [`RigLink::tune_alive`] at least
    /// once a second while it wants the carrier, and checks interlock #6
    /// before calling this.
    pub fn tune_start(&self, max: Duration) {
        self.send(Msg::TuneStart(max.min(TUNE_MAX)));
    }

    pub fn tune_alive(&self) {
        self.send(Msg::TuneAlive);
    }

    /// Unkey the TUNE carrier. `why` is shown when it wasn't the operator.
    pub fn tune_stop(&self, why: Option<String>) {
        // Not dropped if the queue is full: unkeying matters most.
        if let Some(tx) = &self.tx {
            let _ = tx.send_timeout(Msg::TuneStop(why), Duration::from_millis(200));
        }
    }
}

impl Drop for RigLink {
    fn drop(&mut self) {
        // Closing the channel is the worker's signal to unkey and stop.
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
    /// The radio answered that it doesn't know the command.
    Refused,
    /// The link would not send the frame (interlock #5).
    NotAllowed,
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
    proto: Box<dyn Protocol>,
    /// Answers already in, not yet looked at.
    pending: std::collections::VecDeque<Incoming>,
    /// Answers that came while waiting for a different one, for the caller
    /// to fold in.
    strays: Vec<Reply>,
}

impl<P: Read + Write> Rig<P> {
    fn new(port: P, proto: Box<dyn Protocol>) -> Self {
        Rig { port, proto, pending: Default::default(), strays: Vec::new() }
    }

    fn write(&mut self, frame: &Frame) -> Result<(), Error> {
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #5): only frames the
        // radio's protocol lists as allowed reach the radio. See
        // `elad::allowed` for why.
        if !self.proto.allowed(frame) {
            warn!("rigcat: refused to send {:?}", String::from_utf8_lossy(&frame.bytes));
            return Err(Error::NotAllowed);
        }
        self.port.write_all(&frame.bytes)?;
        self.port.flush()?;
        Ok(())
    }

    /// Send a frame that has no answer.
    fn send(&mut self, frame: &Frame) -> Result<(), Error> {
        self.write(frame)
    }

    /// Send a READ and return its answer. Any other answer that arrives on
    /// the way is kept in `strays`.
    fn query(&mut self, frame: &Frame) -> Result<Reply, Error> {
        self.write(frame)?;
        let Some(want) = self.proto.reply_key(frame) else {
            return Err(Error::Timeout);
        };
        let until = Instant::now() + ANSWER;
        let mut chunk = [0u8; 256];
        loop {
            while let Some(m) = self.pending.pop_front() {
                match m {
                    Incoming::Refused => return Err(Error::Refused),
                    Incoming::Reply(r) if r.key == want => return Ok(r),
                    Incoming::Reply(r) => self.strays.push(r),
                }
            }
            if Instant::now() >= until {
                return Err(Error::Timeout);
            }
            match self.port.read(&mut chunk) {
                Ok(0) => {}
                Ok(n) => {
                    let got = self.proto.deframe(&chunk[..n]);
                    self.pending.extend(got);
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn open(
    cfg: &RigConfig,
    proto: Box<dyn Protocol>,
) -> Result<Rig<Box<dyn serialport::SerialPort>>, (Error, Box<dyn Protocol>)> {
    let port = match serialport::new(&cfg.path, cfg.baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(50))
        .open()
    {
        Ok(p) => p,
        Err(e) => return Err((Error::Serial(e), proto)),
    };
    let _ = port.clear(serialport::ClearBuffer::All);
    let mut rig = Rig::new(port, proto);
    let ping = rig.proto.ping();
    let mut last = Error::Timeout;
    for _ in 0..OPEN_PINGS {
        match rig.query(&ping) {
            Ok(_) => return Ok(rig),
            Err(e) => last = e,
        }
    }
    Err((last, rig.proto))
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
        Error::Timeout | Error::Refused | Error::NotAllowed => format!(
            "{path} is open but the radio isn't answering. Is it on, and is its CAT baud rate \
             (menu 70 on the FDM-DUO) set to {}?",
            cfg.baud
        ),
        Error::Serial(se) => format!("{path}: {se}"),
        Error::Io(io) => format!("{path}: {io}"),
    }
}

fn worker(
    cfg: RigConfig,
    mut proto: Box<dyn Protocol>,
    rx: Receiver<Msg>,
    status: Arc<Mutex<RigStatus>>,
) {
    if cfg.path.trim().is_empty() {
        set(&status, |s| s.error = Some("No serial port chosen".into()));
        while rx.recv().is_ok() {}
        return;
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        match open(&cfg, proto) {
            Ok(rig) => {
                info!(port = %cfg.path, baud = cfg.baud, "rigcat: connected");
                backoff = Duration::from_secs(1);
                set(&status, |s| {
                    s.connected = true;
                    s.error = None;
                });
                let (more, back) = run(rig, &rx, &status);
                proto = back;
                if !more {
                    return;
                }
                set(&status, |s| {
                    *s = RigStatus {
                        configured: true,
                        model: s.model,
                        baud: s.baud,
                        power_steps: s.power_steps,
                        power_name: s.power_name,
                        tune_note: s.tune_note.take(),
                        settings: std::mem::take(&mut s.settings),
                        settings_read: s.settings_read,
                        error: Some("The radio stopped answering".into()),
                        ..RigStatus::default()
                    }
                });
            }
            Err((e, back)) => {
                proto = back;
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
                // A TUNE asked for with no radio connected is not kept.
                Ok(_) => {}
            }
        }
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

/// The TUNE carrier, as the link keeps it.
struct TuneState {
    tuning: Tuning,
    alive: Instant,
}

/// Unkey, best effort: twice, in case the first is lost.
fn unkey<P: Read + Write>(rig: &mut Rig<P>, status: &Mutex<RigStatus>, why: Option<String>) {
    let f = rig.proto.unkey();
    for _ in 0..2 {
        if let Err(e) = rig.send(&f) {
            warn!("rigcat: unkey: {e:?}");
        }
    }
    if let Some(w) = &why {
        info!("rigcat: TUNE stopped: {w}");
    }
    set(status, |s| {
        s.tuning = None;
        s.tune_note = why;
    });
}

/// Talk to an open radio until it stops answering (`true`) or the link is
/// dropped (`false`). Hands its protocol back for the next open.
fn run<P: Read + Write>(
    mut rig: Rig<P>,
    rx: &Receiver<Msg>,
    status: &Mutex<RigStatus>,
) -> (bool, Box<dyn Protocol>) {
    let mut misses = 0;
    let mut next_dial = Instant::now();
    let mut next_settings = Instant::now();
    let (mut settings_once, mut asked) = (false, false);
    let mut tune: Option<TuneState> = None;
    loop {
        let msg = rx.recv_timeout(if tune.is_some() { Duration::from_millis(50) } else { POLL });
        match msg {
            Err(RecvTimeoutError::Disconnected) => {
                if tune.is_some() {
                    unkey(&mut rig, status, None);
                }
                return (false, rig.proto);
            }
            Ok(Msg::ReadSettings) => {
                next_settings = Instant::now();
                asked = true;
            }
            Ok(Msg::SetPower(code)) => {
                let f = rig.proto.set_power(code);
                if let Err(e) = rig.send(&f) {
                    warn!("rigcat: set power: {e:?}");
                }
                // Read it back, so the panel shows what the radio took.
                next_settings = Instant::now();
                asked = true;
            }
            Ok(Msg::TuneStart(max)) => {
                if tune.is_none() {
                    let now = Instant::now();
                    let f = rig.proto.key_tune();
                    debug_assert_eq!(f.kind, FrameKind::Key);
                    match rig.send(&f) {
                        Ok(()) => {
                            let tuning = Tuning { since: now, until: now + max };
                            info!("rigcat: TUNE keyed for at most {max:?}");
                            tune = Some(TuneState { tuning, alive: now });
                            set(status, |s| {
                                s.tuning = Some(tuning);
                                s.tune_note = None;
                            });
                        }
                        Err(e) => warn!("rigcat: TUNE: {e:?}"),
                    }
                }
            }
            Ok(Msg::TuneAlive) => {
                if let Some(t) = &mut tune {
                    t.alive = Instant::now();
                }
            }
            Ok(Msg::TuneStop(why)) => {
                // Sent even with no TUNE of ours running: unkeying is never
                // the wrong thing to do when asked.
                unkey(&mut rig, status, why);
                tune = None;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): the TUNE carrier
        // stops by itself at its time limit, if the panel stops asking for
        // it (frozen or closed), and if the radio drops to receive on a
        // high SWR, whatever the panel does.
        if let Some(t) = &tune {
            let now = Instant::now();
            let tripped =
                status.lock().unwrap_or_else(|e| e.into_inner()).swr.is_some_and(|s| s.tripped);
            let why = if now >= t.tuning.until {
                Some("time limit reached".to_string())
            } else if now.duration_since(t.alive) > TUNE_ALIVE {
                Some("the panel stopped asking for it".to_string())
            } else if tripped {
                Some("the radio dropped to receive on a high SWR".to_string())
            } else {
                None
            };
            if let Some(w) = why {
                unkey(&mut rig, status, Some(w));
                tune = None;
            }
        }

        let mut answered = false;
        let mut ask = |rig: &mut Rig<P>, frame: &Frame| -> Option<String> {
            let res = rig.query(frame);
            let strays = std::mem::take(&mut rig.strays);
            set(status, |s| {
                for r in strays.iter().chain(res.as_ref().ok()) {
                    rig.proto.apply(s, r);
                }
            });
            match res {
                Ok(r) => {
                    answered = true;
                    Some(r.body)
                }
                Err(Error::Refused) => {
                    answered = true;
                    Some("?".into())
                }
                Err(_) => None,
            }
        };

        for f in rig.proto.telemetry() {
            if ask(&mut rig, &f).is_none() {
                // A reading that didn't come is not the last one seen.
                set(status, |s| match f.bytes.as_slice() {
                    b"FP;" => s.fwd = None,
                    b"RP;" => s.rfl = None,
                    _ => s.swr = None,
                });
            }
        }
        if Instant::now() >= next_dial {
            next_dial = Instant::now() + DIAL_POLL;
            for f in rig.proto.dial() {
                ask(&mut rig, &f);
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
            let items = rig.proto.settings();
            let mut rows = Vec::with_capacity(items.len());
            for item in items {
                let f = rig.proto.read(item);
                let key = rig.proto.reply_key(&f).unwrap_or_default();
                let body = ask(&mut rig, &f);
                let reply = body.as_ref().map(|b| Reply { key: key.clone(), body: b.clone() });
                let (value, raw) = match &reply {
                    Some(r) if r.body == "?" => (None, Some("?".to_string())),
                    Some(r) => (rig.proto.describe(r), Some(rig.proto.raw(r))),
                    None => (None, None),
                };
                rows.push(SettingRow { item: *item, value, raw });
            }
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
                if tune.is_some() {
                    unkey(&mut rig, status, Some("the radio stopped answering".into()));
                }
                return (true, rig.proto);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elad::FdmDuo;
    use std::collections::VecDeque;

    /// A pretend DUO: answers each READ from a table, and records everything
    /// it was sent.
    #[derive(Default)]
    struct FakeDuo {
        sent: Vec<String>,
        out: VecDeque<u8>,
        pending: String,
    }

    impl Write for FakeDuo {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.pending.push_str(&String::from_utf8_lossy(b));
            while let Some(i) = self.pending.find(';') {
                let cmd: String = self.pending.drain(..=i).collect();
                let reply = match cmd.as_str() {
                    "FA;" => Some("FA00014074000;"),
                    "FP;" => Some("FP 006091;"),
                    "RP;" => Some("RP 000001;"),
                    "WR;" => Some("WR0 01.02;"),
                    "TP;" => Some("TP09;"),
                    "MD;" => Some("MD2;"),
                    c if c.starts_with("TP") || c == "TX2;" || c == "RX;" => None,
                    _ => Some("?;"),
                };
                if let Some(r) = reply {
                    self.out.extend(r.bytes());
                }
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

    fn rig() -> Rig<FakeDuo> {
        Rig::new(FakeDuo::default(), Box::new(FdmDuo::default()))
    }

    #[test]
    fn a_read_returns_its_own_answer_and_a_refusal_is_said() {
        let mut r = rig();
        assert_eq!(r.query(&Frame::read("FP;")).unwrap().body, " 006091");
        assert_eq!(r.query(&Frame::read("TP;")).unwrap().body, "09");
        assert!(matches!(r.query(&Frame::read("VS;")), Err(Error::Refused)));
    }

    #[test]
    fn a_frame_not_on_the_list_never_reaches_the_radio() {
        let mut r = rig();
        for f in ["TX;", "TX0;", "TX1;", "RC;", "AN2;"] {
            assert!(matches!(r.send(&Frame::new(f, FrameKind::Key)), Err(Error::NotAllowed)));
        }
        assert!(r.port.sent.is_empty());
    }

    /// `run` on a pretend DUO, on its own thread. The handle gives back
    /// every frame the radio was sent, once the channel is closed.
    fn running() -> (Sender<Msg>, Arc<Mutex<RigStatus>>, JoinHandle<Vec<String>>) {
        let (tx, rx) = bounded(16);
        let st = Arc::new(Mutex::new(RigStatus::default()));
        let s2 = st.clone();
        let h = thread::spawn(move || {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let port = Tapped { inner: FakeDuo::default(), sent: sent.clone() };
            let _ = run(Rig::new(port, Box::new(FdmDuo::default())), &rx, &s2);
            sent.lock().unwrap().clone()
        });
        (tx, st, h)
    }

    struct Tapped {
        inner: FakeDuo,
        sent: Arc<Mutex<Vec<String>>>,
    }

    impl Write for Tapped {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let n = self.inner.write(b)?;
            self.sent.lock().unwrap().extend(self.inner.sent.drain(..));
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Read for Tapped {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(b)
        }
    }

    #[test]
    fn answers_fill_the_status() {
        let d = FdmDuo::default();
        let mut s = RigStatus { power_steps: d.power_steps(), ..RigStatus::default() };
        let r = |k: &str, b: &str| Reply { key: k.into(), body: b.into() };
        d.apply(&mut s, &r("FP", " 006091"));
        d.apply(&mut s, &r("RP", " 000001"));
        d.apply(&mut s, &r("WR", "0 01.02"));
        d.apply(&mut s, &r("FA", "00007074000"));
        d.apply(&mut s, &r("TP", "09"));
        assert!(s.keyed());
        assert_eq!(s.fwd_w(), Some(6.091));
        assert_eq!(s.rfl_w(), Some(0.001));
        assert_eq!(s.swr_ratio(), Some(1.02));
        assert_eq!(s.freq_hz, Some(7_074_000));
        assert_eq!(s.power().map(|p| p.label), Some("MAX"));
        // In receive the radio flags its readings: not keyed, nothing shown.
        d.apply(&mut s, &r("FP", "!000000"));
        d.apply(&mut s, &r("WR", "0!00.00"));
        assert!(!s.keyed());
        assert_eq!(s.fwd_w(), None);
        assert_eq!(s.swr_ratio(), None);
    }

    #[test]
    fn tune_keys_the_carrier_and_unkeys_when_the_panel_stops_asking() {
        let (tx, st, h) = running();
        tx.send(Msg::SetPower(2)).unwrap();
        tx.send(Msg::TuneStart(Duration::from_secs(30))).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(st.lock().unwrap().tuning.is_some());
        // No TuneAlive: after a second the link unkeys by itself.
        thread::sleep(Duration::from_millis(1300));
        let s = st.lock().unwrap().clone();
        assert!(s.tuning.is_none());
        assert_eq!(s.tune_note.as_deref(), Some("the panel stopped asking for it"));
        drop(tx);
        let sent = h.join().unwrap();
        assert!(sent.contains(&"TP02;".to_string()));
        let key = sent.iter().position(|c| c == "TX2;").unwrap();
        assert!(sent[key..].contains(&"RX;".to_string()));
        // Never an ordinary transmission.
        assert!(!sent.iter().any(|c| c == "TX;" || c == "TX0;" || c == "TX1;"));
    }

    #[test]
    fn tune_stops_at_its_time_limit_and_when_the_link_is_dropped() {
        let (tx, st, h) = running();
        tx.send(Msg::TuneStart(Duration::from_millis(400))).unwrap();
        for _ in 0..8 {
            tx.send(Msg::TuneAlive).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(st.lock().unwrap().tune_note.as_deref(), Some("time limit reached"));
        tx.send(Msg::TuneStart(Duration::from_secs(30))).unwrap();
        thread::sleep(Duration::from_millis(200));
        drop(tx);
        let sent = h.join().unwrap();
        assert_eq!(sent.iter().filter(|c| *c == "TX2;").count(), 2);
        assert_eq!(sent.last().map(String::as_str), Some("RX;"));
    }
}
