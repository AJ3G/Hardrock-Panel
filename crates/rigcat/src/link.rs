//! The radio on a thread of its own: open the port, read, run TUNE, and
//! reopen the port if the radio stops answering.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use tracing::{debug, info, warn};

use crate::protocol::{Frame, FrameKind, Incoming, Protocol, Reply};
use crate::{Keying, RigConfig, RigStatus, SettingRow, Tuning};

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
    /// Key TUNE, this way, for at most this long.
    TuneStart(Duration, Keying),
    /// Put the radio's transmit audio on its USB input.
    UsbAudio,
    /// The panel still wants the carrier.
    TuneAlive,
    /// Unkey, and why.
    TuneStop(Option<String>),
    /// Change settings: these frames in order, each read back after.
    Set(Vec<Frame>),
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
            audio_ceiling: proto.audio_ceiling(),
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

    /// Key TUNE for at most `max` (and never more than [`TUNE_MAX`]). The
    /// caller must call [`RigLink::tune_alive`] at least once a second while
    /// it wants it, and checks interlock #6 before calling this. With
    /// [`Keying::Audio`] the caller's tone must already be playing; the link
    /// reads the radio's input and mode first and keys only if they pass
    /// (interlock #7).
    pub fn tune_start(&self, max: Duration, keying: Keying) {
        self.send(Msg::TuneStart(max.min(TUNE_MAX), keying));
    }

    /// Set the radio's transmit audio to its USB input (menu 32 on the
    /// FDM-DUO).
    pub fn use_usb_audio(&self) {
        self.send(Msg::UsbAudio);
    }

    pub fn tune_alive(&self) {
        self.send(Msg::TuneAlive);
    }

    /// Change the radio's settings (the dial, the mode, a menu): the frames
    /// from [`crate::Protocol`]'s `set_*` methods, sent in order and each
    /// read back. Refused while the radio transmits (interlock #5); the
    /// reason is in [`RigStatus::set_note`].
    pub fn set(&self, frames: Vec<Frame>) {
        if !frames.is_empty() {
            self.send(Msg::Set(frames));
        }
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
    /// The next ordinary transmission may be sent: interlock #7's reads have
    /// just passed. Cleared by sending it.
    armed: bool,
}

impl<P: Read + Write> Rig<P> {
    fn new(port: P, proto: Box<dyn Protocol>) -> Self {
        Rig { port, proto, pending: Default::default(), strays: Vec::new(), armed: false }
    }

    fn write(&mut self, frame: &Frame) -> Result<(), Error> {
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #5): only frames the
        // radio's protocol lists as allowed reach the radio. See
        // `elad::allowed` for why.
        if !self.proto.allowed(frame) {
            warn!("rigcat: refused to send {:?}", String::from_utf8_lossy(&frame.bytes));
            return Err(Error::NotAllowed);
        }
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #7): an ordinary
        // transmission sends whatever audio is on the radio's input, so it
        // goes only straight after the reads that show that input is the
        // computer's and the mode a sideband (see `key_audio`).
        if frame.kind == FrameKind::KeyAudio && !std::mem::take(&mut self.armed) {
            warn!("rigcat: refused an ordinary transmission without its checks");
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
                        audio_ceiling: s.audio_ceiling,
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

/// The READ that shows what a SET did: the same command letters, except
/// that RIT's offset is read as `RV` and the clear and the CW keys have none.
fn read_back(proto: &dyn Protocol, set: &Frame) -> Option<Frame> {
    let text = String::from_utf8_lossy(&set.bytes);
    let k = text.get(..2)?;
    let cmd = match k {
        "RC" | "RU" | "RD" => "RV;".to_string(),
        "SW" | "CM" => return None,
        _ => proto
            .controls()
            .iter()
            .find(|c| c.cmd == k)
            .map(|c| c.read.to_string())
            .unwrap_or_else(|| format!("{k};")),
    };
    let f = Frame::read(cmd);
    proto.allowed(&f).then_some(f)
}

/// The TUNE carrier, as the link keeps it.
struct TuneState {
    tuning: Tuning,
    alive: Instant,
}

/// The ordinary transmission, once the radio has been read and shows it
/// would send the computer's audio (interlock #7); the reason if not.
fn key_audio<P: Read + Write>(
    rig: &mut Rig<P>,
    status: &Mutex<RigStatus>,
) -> Result<Frame, String> {
    let Some(f) = rig.proto.key_audio() else {
        return Err("This radio can't be keyed with the panel's audio.".into());
    };
    // Read now, not from the last settings read: the operator may have
    // changed menu 32 or the mode on the radio since.
    for r in rig.proto.audio_reads() {
        let got = rig.query(&r);
        let strays = std::mem::take(&mut rig.strays);
        set(status, |s| {
            for x in strays.iter().chain(got.as_ref().ok()) {
                rig.proto.apply(s, x);
            }
        });
        if got.is_err() {
            return Err("The radio didn't answer the check of its TX input and mode.".into());
        }
    }
    let s = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    rig.proto.audio_ready(&s)?;
    rig.armed = true;
    Ok(f)
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
            Ok(Msg::UsbAudio) => {
                if let Some(f) = rig.proto.audio_input() {
                    if let Err(e) = rig.send(&f) {
                        warn!("rigcat: TX input: {e:?}");
                    }
                    next_settings = Instant::now();
                    asked = true;
                }
            }
            Ok(Msg::TuneStart(max, keying)) if tune.is_none() => {
                let key = match keying {
                    Keying::Carrier => Ok(rig.proto.key_tune()),
                    Keying::Audio => key_audio(&mut rig, status),
                };
                if let Err(why) = &key {
                    info!("rigcat: TUNE not keyed: {why}");
                    set(status, |s| s.tune_note = Some(why.clone()));
                }
                if let Ok(f) = key {
                    let now = Instant::now();
                    match rig.send(&f) {
                        Ok(()) => {
                            let tuning = Tuning { since: now, until: now + max, keying };
                            info!("rigcat: TUNE ({keying:?}) keyed for at most {max:?}");
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
            // One at a time.
            Ok(Msg::TuneStart(..)) => {}
            Ok(Msg::Set(frames)) => {
                for f in frames {
                    // SAFETY INTERLOCK (docs/safety-interlocks.md, #5): no
                    // setting changes, and no stored CW message starts, while
                    // the radio transmits or a TUNE of ours runs. Unkeying
                    // frames are always let through.
                    let busy = tune.is_some()
                        || status.lock().unwrap_or_else(|e| e.into_inner()).transmitting();
                    if busy && f.kind != FrameKind::Unkey {
                        let what = String::from_utf8_lossy(&f.bytes).to_string();
                        info!("rigcat: not sent while transmitting: {what}");
                        set(status, |s| {
                            s.set_note = Some("Not changed: the radio is transmitting.".into())
                        });
                        break;
                    }
                    if !matches!(
                        f.kind,
                        FrameKind::SetSetting | FrameKind::KeyCw | FrameKind::Unkey
                    ) {
                        warn!("rigcat: Set carries a {:?} frame; not sent", f.kind);
                        continue;
                    }
                    match rig.send(&f) {
                        Ok(()) => set(status, |s| s.set_note = None),
                        Err(e) => {
                            warn!("rigcat: set: {e:?}");
                            set(status, |s| {
                                s.set_note =
                                    Some(format!("Not sent: {}", String::from_utf8_lossy(&f.bytes)))
                            });
                            continue;
                        }
                    }
                    // Read it back: the radio doesn't acknowledge a set, so
                    // the panel shows what the read says it took.
                    if let Some(rb) = read_back(rig.proto.as_ref(), &f) {
                        let res = rig.query(&rb);
                        let strays = std::mem::take(&mut rig.strays);
                        set(status, |s| {
                            for r in strays.iter().chain(res.as_ref().ok()) {
                                rig.proto.apply(s, r);
                            }
                        });
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
        /// Menu 32 is on the microphone.
        mic: bool,
        /// `GI` says the radio is transmitting.
        on_air: bool,
        /// Receiving: no trusted forward power.
        quiet: bool,
    }

    impl Write for FakeDuo {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.pending.push_str(&String::from_utf8_lossy(b));
            while let Some(i) = self.pending.find(';') {
                let cmd: String = self.pending.drain(..=i).collect();
                let reply = match cmd.as_str() {
                    "FA;" => Some("FA00014074000;"),
                    "FP;" if self.quiet => Some("FP!000000;"),
                    "FP;" => Some("FP 006091;"),
                    "RP;" => Some("RP 000001;"),
                    "WR;" => Some("WR0 01.02;"),
                    "TP;" => Some("TP09;"),
                    "MD;" => Some("MD2;"),
                    "TI;" if self.mic => Some("TI0;"),
                    "TI;" => Some("TI1;"),
                    "GI;" if self.on_air => Some("GI0000012000000;"),
                    "GI;" => Some("GI0000002000000;"),
                    c if c.starts_with("FA0") || c.starts_with("MD") && c.len() == 4 => None,
                    c if c.starts_with("TP") || ["TX2;", "TX;", "TI1;", "RX;"].contains(&c) => None,
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
        for f in ["TX0;", "TX1;", "RC;", "AN2;", "TI0;", "DF15214;", "SE1;"] {
            assert!(matches!(r.send(&Frame::new(f, FrameKind::Key)), Err(Error::NotAllowed)));
        }
        for f in ["DF15214;", "SE1;", "UU1;", "BR0;", "TX0;"] {
            assert!(matches!(
                r.send(&Frame::new(f, FrameKind::SetSetting)),
                Err(Error::NotAllowed)
            ));
        }
        // An ordinary transmission is on the list, but not without its checks.
        let tx = Frame::new("TX;", FrameKind::KeyAudio);
        assert!(matches!(r.send(&tx), Err(Error::NotAllowed)));
        assert!(r.port.sent.is_empty());
    }

    /// `run` on a pretend DUO, on its own thread. The handle gives back
    /// every frame the radio was sent, once the channel is closed.
    fn running() -> (Sender<Msg>, Arc<Mutex<RigStatus>>, JoinHandle<Vec<String>>) {
        running_on(FakeDuo::default())
    }

    fn running_on(duo: FakeDuo) -> (Sender<Msg>, Arc<Mutex<RigStatus>>, JoinHandle<Vec<String>>) {
        let (tx, rx) = bounded(16);
        let st = Arc::new(Mutex::new(RigStatus::default()));
        let s2 = st.clone();
        let h = thread::spawn(move || {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let port = Tapped { inner: duo, sent: sent.clone() };
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
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Carrier)).unwrap();
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
        tx.send(Msg::TuneStart(Duration::from_millis(400), Keying::Carrier)).unwrap();
        for _ in 0..8 {
            tx.send(Msg::TuneAlive).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(st.lock().unwrap().tune_note.as_deref(), Some("time limit reached"));
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Carrier)).unwrap();
        thread::sleep(Duration::from_millis(200));
        drop(tx);
        let sent = h.join().unwrap();
        assert_eq!(sent.iter().filter(|c| *c == "TX2;").count(), 2);
        assert_eq!(sent.last().map(String::as_str), Some("RX;"));
    }

    #[test]
    fn tune_with_drive_reads_the_input_and_mode_then_keys_an_ordinary_transmission() {
        let (tx, st, h) = running();
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Audio)).unwrap();
        for _ in 0..3 {
            tx.send(Msg::TuneAlive).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        let s = st.lock().unwrap().clone();
        assert_eq!(s.tuning.map(|t| t.keying), Some(Keying::Audio));
        tx.send(Msg::TuneStop(None)).unwrap();
        thread::sleep(Duration::from_millis(100));
        drop(tx);
        let sent = h.join().unwrap();
        let key = sent.iter().position(|c| c == "TX;").unwrap();
        // The checks come straight before it.
        assert_eq!(&sent[key - 2..key], ["TI;", "MD;"]);
        assert!(sent[key..].contains(&"RX;".to_string()));
        assert!(!sent.iter().any(|c| c == "TX2;"));
    }

    #[test]
    fn tune_with_drive_is_refused_while_the_radio_takes_the_microphone() {
        let (tx, st, h) = running_on(FakeDuo { mic: true, ..FakeDuo::default() });
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Audio)).unwrap();
        thread::sleep(Duration::from_millis(300));
        let s = st.lock().unwrap().clone();
        assert!(s.tuning.is_none());
        assert!(s.tune_note.as_deref().is_some_and(|n| n.contains("menu 32")), "{s:?}");
        tx.send(Msg::UsbAudio).unwrap();
        thread::sleep(Duration::from_millis(100));
        drop(tx);
        let sent = h.join().unwrap();
        assert!(!sent.iter().any(|c| c == "TX;"));
        assert!(sent.contains(&"TI1;".to_string()));
    }

    #[test]
    fn a_setting_is_sent_and_read_back_between_transmissions_only() {
        let d = FdmDuo::default();
        let (tx, _st, h) = running_on(FakeDuo { quiet: true, ..Default::default() });
        thread::sleep(Duration::from_millis(300));
        tx.send(Msg::Set(vec![d.set_freq(crate::Vfo::A, 7_074_000).unwrap()])).unwrap();
        thread::sleep(Duration::from_millis(300));
        drop(tx);
        let sent = h.join().unwrap();
        let i = sent.iter().position(|f| f == "FA00007074000;").expect("sent");
        assert_eq!(sent[i + 1], "FA;", "read back straight after");

        // SAFETY INTERLOCK #5: not while the radio says it transmits.
        let (tx, st, h) = running_on(FakeDuo { on_air: true, quiet: true, ..Default::default() });
        thread::sleep(Duration::from_millis(1300));
        assert_eq!(st.lock().unwrap().tx_state, Some(1));
        tx.send(Msg::Set(vec![
            d.set_freq(crate::Vfo::A, 7_074_000).unwrap(),
            d.cw_stop().unwrap(),
        ]))
        .unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(st.lock().unwrap().set_note.as_deref().unwrap_or("").contains("transmitting"));
        drop(tx);
        let sent = h.join().unwrap();
        assert!(!sent.iter().any(|f| f.starts_with("FA0")), "{sent:?}");
    }
}
