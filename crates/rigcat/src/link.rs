//! The radio on a thread of its own: open the port, read, run TUNE, and
//! reopen the port if the radio stops answering.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded};
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
/// How long after the radio was last seen transmitting the settings wait
/// before their own read. That read is a dozen round trips or more, and the
/// first rounds after an over are the ones that see the radio back in
/// receive, which everything held over the transmission waits for.
const SETTINGS_AFTER_TX: Duration = Duration::from_secs(2);
/// Settings read each round while a settings read is under way.
const SWEEP_STEP: usize = 8;
/// How long to wait for an answer.
const ANSWER: Duration = Duration::from_millis(300);
/// Missed answers in a row before the link is called lost and reopened.
const MISSES: u32 = 4;
const RETRY_MAX: Duration = Duration::from_secs(2);
const OPEN_PINGS: u32 = 3;
/// How long the panel may go without saying it still wants the TUNE carrier
/// before the link unkeys by itself (interlock #6). The panel says so every
/// frame, and [`RigLink::tune_alive`] passes one on every [`ALIVE_EVERY`].
const TUNE_ALIVE: Duration = Duration::from_secs(1);
/// How often [`RigLink::tune_alive`] passes a keep-alive on: well inside
/// [`TUNE_ALIVE`].
const ALIVE_EVERY: Duration = Duration::from_millis(200);

enum Msg {
    /// Read every setting again now.
    ReadSettings,
    /// Set the radio's transmit power step (menu 34 on the FDM-DUO).
    SetPower(u8),
    /// Key TUNE, this way, for at most this long; with a dial, only if the
    /// radio's transmit dial reads exactly that just before (decision 106).
    TuneStart(Duration, Keying, Option<u64>),
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
    /// When the last keep-alive went: the panel asks every frame, the link
    /// needs one a second, so most are not sent.
    alive_sent: Mutex<Option<Instant>>,
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
        RigLink { tx: Some(tx), status, thread, alive_sent: Mutex::new(None) }
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

    /// Key TUNE for at most `max` (and never more than [`Keying::max_keyed`] allows). The
    /// caller must call [`RigLink::tune_alive`] at least once a second while
    /// it wants it, and checks interlock #6 before calling this. With
    /// [`Keying::Audio`] the caller's tone must already be playing; the link
    /// reads the radio's input and mode first and keys only if they pass
    /// (interlock #7).
    pub fn tune_start(&self, max: Duration, keying: Keying) {
        self.send(Msg::TuneStart(max.min(keying.max_keyed()), keying, None));
    }

    /// [`RigLink::tune_start`], keyed only if the radio's transmit dial
    /// ([`RigStatus::tx_dial_hz`]), read again straight before, is exactly
    /// `tx_dial_hz`: the dial the caller worked its audio out for (the TX
    /// window's over in split, decision 106). Anywhere else the over would
    /// land off where it was meant to go, so it isn't keyed and
    /// [`RigStatus::tune_note`] says why.
    pub fn tune_start_on(&self, max: Duration, keying: Keying, tx_dial_hz: u64) {
        self.send(Msg::TuneStart(max.min(keying.max_keyed()), keying, Some(tx_dial_hz)));
    }

    /// Set the radio's transmit audio to its USB input (menu 32 on the
    /// FDM-DUO).
    pub fn use_usb_audio(&self) {
        self.send(Msg::UsbAudio);
    }

    /// Tell the link the caller still wants the TUNE. Sent at most every
    /// [`ALIVE_EVERY`], however often it is called, so the keep-alives never
    /// fill the queue ahead of a stop.
    pub fn tune_alive(&self) {
        let mut last = self.alive_sent.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_none_or(|t| t.elapsed() >= ALIVE_EVERY) {
            *last = Some(Instant::now());
            self.send(Msg::TuneAlive);
        }
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

/// Read the dials again (VFO A, VFO B, and `GI` for split and the receive
/// VFO) and say whether the radio would transmit on exactly `want` Hz.
fn check_tx_dial<P: Read + Write>(
    rig: &mut Rig<P>,
    status: &Mutex<RigStatus>,
    want: u64,
) -> Result<(), String> {
    for r in rig.proto.dial() {
        let got = rig.query(&r);
        let strays = std::mem::take(&mut rig.strays);
        set(status, |s| {
            for x in strays.iter().chain(got.as_ref().ok()) {
                rig.proto.apply(s, x);
            }
        });
        if got.is_err() {
            return Err("The radio didn't answer the check of its transmit dial.".into());
        }
    }
    let s = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match s.tx_dial_hz() {
        Some(hz) if hz == want => Ok(()),
        Some(hz) => Err(format!(
            "The radio's transmit dial reads {hz} Hz, not the {want} Hz the over was worked out \
             for."
        )),
        None => Err("The radio's transmit dial couldn't be read.".into()),
    }
}

/// Unkey, best effort: twice, in case the first is lost.
fn unkey<P: Read + Write>(rig: &mut Rig<P>, status: &Mutex<RigStatus>, why: Option<String>) {
    let f = rig.proto.unkey();
    for _ in 0..2 {
        if let Err(e) = rig.send(&f) {
            warn!("rigcat: unkey: {e:?}");
        }
    }
    // Every unkey is logged, with its reason when it has one (AJ3G,
    // 11 Nov 2026: a TUNE ended with nothing in the log to say why).
    match &why {
        Some(w) => info!("rigcat: TUNE stopped: {w}"),
        None => info!("rigcat: unkeyed as asked"),
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
    // A power step was just set: read it back this round.
    let mut power_set = false;
    // A settings read under way: the next item, the rows so far, and
    // whether it goes on while the radio transmits.
    let mut sweep: Option<(usize, Vec<SettingRow>, bool)> = None;
    // What the last full read found silent or refused, so the log says it
    // once, and again only when it changes.
    let mut quiet_logged = String::new();
    let mut tune: Option<TuneState> = None;
    let mut last_tx: Option<Instant> = None;
    loop {
        // Every message waiting is handled before the round's reads, not one
        // a round: a TUNE, its stop or a click never queues behind the
        // keep-alives that come while a TUNE runs.
        let mut msg =
            rx.recv_timeout(if tune.is_some() { Duration::from_millis(50) } else { POLL });
        loop {
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
                    // Read it back at once, so the power keys show what the
                    // radio took: the settings sweep could take seconds to
                    // reach it, and a key that stayed dark read as ignored
                    // (AJ3G, 24 Oct 2026).
                    power_set = true;
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
                Ok(Msg::TuneStart(max, keying, on)) if tune.is_none() => {
                    // SAFETY INTERLOCK (docs/safety-interlocks.md, #7): an
                    // over worked out for one transmit dial is keyed only
                    // once the radio reads that dial back, just before.
                    let key = match on.map(|hz| check_tx_dial(&mut rig, status, hz)) {
                        Some(Err(why)) => Err(why),
                        _ => match keying {
                            Keying::Carrier => Ok(rig.proto.key_tune()),
                            Keying::Audio | Keying::Data => key_audio(&mut rig, status),
                        },
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
                                    s.set_note = Some(format!(
                                        "Not sent: {}",
                                        String::from_utf8_lossy(&f.bytes)
                                    ))
                                });
                                continue;
                            }
                        }
                        // Read it back: the radio doesn't acknowledge a set, so
                        // the panel shows what the read says it took.
                        // Each set and what the radio says it took go in the
                        // log, so a setting the radio ignores can be seen
                        // there (AJ3G's NR on E1, 31 Oct 2026). A read-back
                        // that differs is asked once more after a moment,
                        // in case the radio was still applying it.
                        if let Some(rb) = read_back(rig.proto.as_ref(), &f) {
                            let sent = String::from_utf8_lossy(&f.bytes).to_string();
                            let mut took = String::new();
                            for attempt in 0..2 {
                                if attempt == 1 {
                                    std::thread::sleep(Duration::from_millis(250));
                                }
                                let res = rig.query(&rb);
                                let strays = std::mem::take(&mut rig.strays);
                                set(status, |s| {
                                    for r in strays.iter().chain(res.as_ref().ok()) {
                                        rig.proto.apply(s, r);
                                    }
                                });
                                took = match &res {
                                    Ok(r) => format!("{}{};", r.key, r.body),
                                    Err(e) => format!("{e:?}"),
                                };
                                // Only the noise controls wait and ask again:
                                // a dial or volume set mustn't hold the link.
                                let slow = matches!(sent.get(..2), Some("NR" | "NC" | "NB" | "NK"));
                                if took == sent || !slow {
                                    break;
                                }
                            }
                            info!("rigcat: set {sent} read back {took}");
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
            msg = match rx.try_recv() {
                Ok(m) => Ok(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            };
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

        if std::mem::take(&mut power_set)
            && let Some(f) = rig.proto.read_power()
        {
            ask(&mut rig, &f);
        }
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
        // While the radio says it transmits, the dial (with `GI`, which says
        // receive or transmit) is read every round rather than once a second,
        // so the end of an over is seen within one round instead of up to a
        // second later: the scope, held over the transmission, waits for it.
        let on_air =
            status.lock().unwrap_or_else(|e| e.into_inner()).tx_state.is_some_and(|t| t != 0);
        // Receiving, the S-meter every round, as the radio's own bar moves.
        if !on_air && tune.is_none() {
            for f in rig.proto.meter() {
                ask(&mut rig, &f);
            }
        }
        if Instant::now() >= next_dial || on_air {
            next_dial = Instant::now() + DIAL_POLL;
            for f in rig.proto.dial() {
                ask(&mut rig, &f);
            }
        }
        // The settings are some seventy round trips. Read at once when the
        // port opens and when asked; the read every few seconds waits while
        // the radio transmits, when the power readings matter more, and for
        // [`SETTINGS_AFTER_TX`] after, so it never stands between the end of
        // an over and the reads that see it. A few at a time each round, so
        // a click (a band change, a mode) waits for a few reads at most
        // rather than the whole list.
        if tune.is_some() || status.lock().unwrap_or_else(|e| e.into_inner()).transmitting() {
            last_tx = Some(Instant::now());
        }
        let busy = last_tx.is_some_and(|t| t.elapsed() < SETTINGS_AFTER_TX);
        if sweep.is_none() && Instant::now() >= next_settings && (!busy || !settings_once || asked)
        {
            // The first read, and one asked for, go on even while busy.
            let force = !settings_once || asked;
            settings_once = true;
            asked = false;
            next_settings = Instant::now() + SETTINGS_POLL;
            sweep = Some((0, Vec::new(), force));
        }
        if let Some((at, rows, force)) = sweep.as_mut()
            && (!busy || *force)
        {
            let items = rig.proto.settings();
            let mut end = (*at + SWEEP_STEP).min(items.len());
            for (i, item) in items[*at..end].iter().enumerate() {
                // Something asked for (a TUNE, its stop, a click) goes first:
                // the read takes up again next round where it left off.
                if !rx.is_empty() {
                    end = *at + i;
                    break;
                }
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
            *at = end;
            if end >= items.len() {
                let rows = std::mem::take(rows);
                sweep = None;
                let quiet = quiet_settings(&rows);
                if quiet != quiet_logged {
                    info!("rigcat: settings read: {quiet}");
                    quiet_logged = quiet;
                }
                set(status, |s| {
                    s.settings = rows;
                    s.settings_read = Some(Instant::now());
                });
            }
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

/// The settings the radio didn't answer, or refused, in one line for the
/// log. The panel shows "—" for these, and a set of one can't be checked
/// (AJ3G, 11 Nov 2026: NB and NR wouldn't take from the panel, and NK was
/// one the DUO never answered; "make sure there are no others").
fn quiet_settings(rows: &[SettingRow]) -> String {
    let list = |want: fn(&Option<String>) -> bool| -> String {
        let v: Vec<&str> = rows.iter().filter(|r| want(&r.raw)).map(|r| r.item.cmd).collect();
        if v.is_empty() { "none".into() } else { v.join(" ").replace(';', "") }
    };
    format!(
        "{} read; no answer to {}; refused {}",
        rows.len(),
        list(|r| r.is_none()),
        list(|r| r.as_deref() == Some("?"))
    )
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
        /// When set, `GI` says transmitting while this is true (an over that
        /// ends part way through a test).
        air: Option<Arc<std::sync::atomic::AtomicBool>>,
        /// Menu 34 as last set (MAX until then).
        power: Option<u8>,
        /// In split (`GI`'s P7), and VFO B as last set (14.075 MHz until
        /// then).
        split: bool,
        fb: Option<u64>,
    }

    impl Write for FakeDuo {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.pending.push_str(&String::from_utf8_lossy(b));
            while let Some(i) = self.pending.find(';') {
                let cmd: String = self.pending.drain(..=i).collect();
                if cmd == "TP;" {
                    self.out.extend(format!("TP{:02};", self.power.unwrap_or(9)).bytes());
                    self.sent.push(cmd);
                    continue;
                }
                if let Some(c) = cmd.strip_prefix("TP").and_then(|c| c.strip_suffix(';')) {
                    self.power = c.parse().ok();
                }
                if cmd == "FB;" {
                    self.out.extend(format!("FB{:011};", self.fb.unwrap_or(14_075_000)).bytes());
                    self.sent.push(cmd);
                    continue;
                }
                if let Some(c) = cmd.strip_prefix("FB").and_then(|c| c.strip_suffix(';')) {
                    self.fb = c.parse().ok();
                    self.sent.push(cmd);
                    continue;
                }
                if cmd == "GI;" && self.split && !self.on_air {
                    self.out.extend(b"GI0000002010000;");
                    self.sent.push(cmd);
                    continue;
                }
                let reply = match cmd.as_str() {
                    "FA;" => Some("FA00014074000;"),
                    "FP;" if self.quiet => Some("FP!000000;"),
                    "FP;" => Some("FP 006091;"),
                    "RP;" => Some("RP 000001;"),
                    "WR;" => Some("WR0 01.02;"),
                    "MD;" => Some("MD2;"),
                    "RF;" => Some("RF0000;"),
                    "TI;" if self.mic => Some("TI0;"),
                    "TI;" => Some("TI1;"),
                    "GI;"
                        if self.on_air
                            || self
                                .air
                                .as_ref()
                                .is_some_and(|a| a.load(std::sync::atomic::Ordering::Relaxed)) =>
                    {
                        Some("GI0000012000000;")
                    }
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
    fn a_power_step_set_is_read_back_at_once() {
        let (tx, st, h) = running();
        // Wait out the first settings sweep, which reads TP among the rest.
        for _ in 0..100 {
            if st.lock().unwrap().settings_read.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(st.lock().unwrap().power_code, Some(9));
        tx.send(Msg::SetPower(6)).unwrap();
        thread::sleep(Duration::from_millis(350));
        assert_eq!(st.lock().unwrap().power_code, Some(6));
        drop(tx);
        let sent = h.join().unwrap();
        let set = sent.iter().position(|c| c == "TP06;").unwrap();
        assert_eq!(sent[set + 1], "TP;");
    }

    #[test]
    fn tune_keys_the_carrier_and_unkeys_when_the_panel_stops_asking() {
        let (tx, st, h) = running();
        tx.send(Msg::SetPower(2)).unwrap();
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Carrier, None)).unwrap();
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
        tx.send(Msg::TuneStart(Duration::from_millis(400), Keying::Carrier, None)).unwrap();
        for _ in 0..8 {
            tx.send(Msg::TuneAlive).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(st.lock().unwrap().tune_note.as_deref(), Some("time limit reached"));
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Carrier, None)).unwrap();
        thread::sleep(Duration::from_millis(200));
        drop(tx);
        let sent = h.join().unwrap();
        assert_eq!(sent.iter().filter(|c| *c == "TX2;").count(), 2);
        assert_eq!(sent.last().map(String::as_str), Some("RX;"));
    }

    /// AJ3G: TUNE was slow to unkey. A stop behind a full queue of
    /// keep-alives was handled one message a round, seconds later.
    #[test]
    fn a_stop_behind_a_queue_of_keep_alives_unkeys_at_once() {
        let (tx, st, h) = running();
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Carrier, None)).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(st.lock().unwrap().tuning.is_some());
        for _ in 0..15 {
            tx.send(Msg::TuneAlive).unwrap();
        }
        tx.send(Msg::TuneStop(None)).unwrap();
        let t = Instant::now();
        while st.lock().unwrap().tuning.is_some() {
            assert!(t.elapsed() < Duration::from_millis(400), "still keyed");
            thread::sleep(Duration::from_millis(5));
        }
        drop(tx);
        let sent = h.join().unwrap();
        let key = sent.iter().position(|c| c == "TX2;").unwrap();
        assert!(sent[key..].contains(&"RX;".to_string()));
    }

    #[test]
    fn tune_with_drive_reads_the_input_and_mode_then_keys_an_ordinary_transmission() {
        let (tx, st, h) = running();
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Audio, None)).unwrap();
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
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Audio, None)).unwrap();
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

    /// Decision 106: an over worked out for a transmit dial is keyed only
    /// when the radio reads that dial back straight before; VFO B in split.
    #[test]
    fn a_data_over_keys_only_on_the_transmit_dial_it_was_worked_out_for() {
        let (tx, st, h) = running_on(FakeDuo { split: true, quiet: true, ..FakeDuo::default() });
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Data, Some(14_076_000))).unwrap();
        thread::sleep(Duration::from_millis(300));
        let s = st.lock().unwrap().clone();
        assert!(s.tuning.is_none());
        assert!(s.tune_note.as_deref().is_some_and(|n| n.contains("14075000 Hz")), "{s:?}");
        // VFO B moved, and read back: now it keys.
        tx.send(Msg::Set(vec![FdmDuo::default().set_freq(crate::Vfo::B, 14_076_000).unwrap()]))
            .unwrap();
        tx.send(Msg::TuneStart(Duration::from_secs(30), Keying::Data, Some(14_076_000))).unwrap();
        thread::sleep(Duration::from_millis(300));
        let s = st.lock().unwrap().clone();
        assert_eq!(s.tuning.map(|t| t.keying), Some(Keying::Data), "{s:?}");
        tx.send(Msg::TuneStop(None)).unwrap();
        thread::sleep(Duration::from_millis(100));
        drop(tx);
        let sent = h.join().unwrap();
        assert_eq!(sent.iter().filter(|c| *c == "TX;").count(), 1);
        let key = sent.iter().position(|c| c == "TX;").unwrap();
        let fb = sent[..key].iter().rposition(|c| c == "FB;").unwrap();
        let set = sent.iter().position(|c| c == "FB00014076000;").unwrap();
        assert!(set < fb, "VFO B read again after the move, before the key");
        assert_eq!(&sent[key - 2..key], ["TI;", "MD;"]);
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

    /// A click doesn't wait behind the settings' seventy reads: it goes out
    /// within a few of them.
    #[test]
    fn a_set_goes_out_during_the_settings_read() {
        let d = FdmDuo::default();
        let (tx, _st, h) = running_on(FakeDuo { quiet: true, ..Default::default() });
        // The first settings read starts after the first round's wait.
        thread::sleep(Duration::from_millis(400));
        tx.send(Msg::Set(vec![d.set_freq(crate::Vfo::A, 7_074_000).unwrap()])).unwrap();
        thread::sleep(Duration::from_millis(4000));
        drop(tx);
        let sent = h.join().unwrap();
        let set = sent.iter().position(|f| f == "FA00007074000;").expect("sent");
        let last = sent.iter().rposition(|f| f == "RI;").unwrap_or_else(|| panic!("{sent:?}"));
        assert!(set < last, "the set waited for the whole settings read: {sent:?}");
        let first = sent.iter().position(|f| f == "DT;").expect("the settings read started");
        assert!(first < set, "{sent:?}");
        let reads = sent[first..set].iter().filter(|f| f.ends_with(';') && f.len() <= 5).count();
        assert!(reads <= 3 * SWEEP_STEP + 12, "{reads} reads before the set: {sent:?}");
    }

    /// The end of an over is seen within a round of power readings, not at
    /// the next once-a-second dial read: the radio's `GI` is read every
    /// round while it says it transmits, and the settings' own read waits
    /// until a while after the over.
    #[test]
    fn the_end_of_an_over_is_seen_within_a_round() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let air = Arc::new(AtomicBool::new(true));
        let duo = FakeDuo { quiet: true, air: Some(air.clone()), ..Default::default() };
        let (tx, st, h) = running_on(duo);
        // Long enough for the first settings read and a dial read to pass.
        thread::sleep(Duration::from_millis(1300));
        assert!(st.lock().unwrap().transmitting(), "GI says TX");
        // Let the once-a-second dial read go by, then unkey just after it:
        // the worst case before.
        thread::sleep(Duration::from_millis(150));
        air.store(false, Ordering::Relaxed);
        let t = Instant::now();
        while st.lock().unwrap().transmitting() && t.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(5));
        }
        let took = t.elapsed();
        assert!(!st.lock().unwrap().transmitting(), "still TX after {took:?}");
        assert!(took < Duration::from_millis(400), "the end of the over took {took:?} to see");
        drop(tx);
        let sent = h.join().unwrap();
        let gi: Vec<usize> =
            sent.iter().enumerate().filter(|(_, f)| *f == "GI;").map(|(i, _)| i).collect();
        assert!(gi.len() > 5, "GI read every round while on air: {}", gi.len());
    }

    #[test]
    fn the_log_names_settings_the_radio_left_unanswered() {
        use crate::protocol::Item;
        let row = |cmd, raw: Option<&str>| SettingRow {
            item: Item { cmd, menu: None, name: "" },
            value: None,
            raw: raw.map(str::to_string),
        };
        let rows = [row("NB;", Some("NB0")), row("NK;", None), row("XY;", Some("?"))];
        assert_eq!(quiet_settings(&rows), "3 read; no answer to NK; refused XY");
        assert_eq!(quiet_settings(&rows[..1]), "1 read; no answer to none; refused none");
    }
}
