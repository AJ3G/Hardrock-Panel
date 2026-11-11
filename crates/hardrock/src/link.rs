//! The amplifier on a thread of its own: open the port, poll the readings, carry
//! out the panel's buttons, and keep a snapshot the panel can read without
//! waiting on the serial port.
//!
//! Every round trip to the amplifier blocks for tens of milliseconds, which a
//! window redrawing sixty times a second cannot spare.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::types::{
    AmpAction, AmpAlerts, AmpConfig, AmpGauge, AmpLevel, AmpLimits, AmpReadings, AmpStatus,
    AtuNetwork, AtuReadout,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use tracing::{debug, info, warn};

use crate::{Amp, AtuParams, Band, Command, KeyMode, Lights, Response, TempScale, TuneResult};

/// How often the readings are asked for. Four a second is what HobbyPCB's own
/// panel manages, and what a meter needs to look alive.
const POLL: Duration = Duration::from_millis(250);
/// How often the tuner's network is read while nothing is happening. During a
/// tune it is read every poll.
const ATU_POLL: Duration = Duration::from_secs(1);
/// How long after a click the tuner is read back: long enough for its relays
/// to settle, so the read confirms the change instead of showing the old state.
const ATU_SETTLE: Duration = Duration::from_millis(300);
/// How often the temperature is read with `HRTP`. Its value is the one the
/// amplifier's own screen shows, and it carries the scale letter; the status
/// frame's temperature lagged it by several degrees after a C/F toggle (AJ3G,
/// 17 Oct 2026: the screen read 75 °F at once while the status frame crept up
/// from 71), so `HRTP` is what the panel shows.
const TEMP_POLL: Duration = Duration::from_secs(1);
/// The same while transmitting, when the amplifier often leaves a query
/// unanswered: each miss holds up the live readings for the query's timeout,
/// and the heat spreader changes far more slowly than they do.
const TEMP_POLL_TX: Duration = Duration::from_secs(10);
/// How old an `HRTP` reading may be before the status frame's own
/// temperature is shown instead. AJ3G, 18 Oct 2026: the panel held 122 °F
/// while the amplifier's screen said 133 °F, because a reading that stops
/// coming was kept for good. A little over the transmitting interval, so a
/// slow read while keyed is still used.
const TEMP_STALE: Duration = Duration::from_secs(15);
/// How long after a C/F toggle the scale is read on every poll.
const SCALE_SETTLE: Duration = Duration::from_secs(3);
/// How often a band change held back by interlock #3 is tried again.
const BAND_RETRY: Duration = Duration::from_millis(500);
/// Missed status answers in a row before the link is called lost and
/// reopened: about 3.2 s, with another read (as unanswered) between each.
const MISSES: u32 = 4;
/// While transmitting, how long the link waits with no answer to a status
/// read before calling it lost (see [`link_lost`]). The HR500 often leaves
/// queries unanswered while keyed: four in a row (two or three seconds)
/// dropped the link mid-over, and the panel unkeyed the over with it (AJ3G,
/// 11 Nov 2026: "amplifier is not connected" was cutting his QSOs and CQs).
const TX_GRACE: Duration = Duration::from_secs(5);
/// Longest wait between tries at reopening the port. Trying is cheap and
/// disturbs nothing, and a USB lead plugged back in should be picked up within
/// seconds (AJ3G, 18 Oct: at 30 s it took minutes).
const RETRY_MAX: Duration = Duration::from_secs(2);
/// Pings tried after opening before giving up: a USB serial adapter just
/// plugged in can drop the first bytes.
const OPEN_PINGS: u32 = 3;
/// Drive above this, with the amplifier in line, counts as a transmission for
/// holding and clearing the status indicators. `HRST` says nothing of TX.
const KEYED_DRIVE_W: f32 = 0.5;

/// Watches for the amplifier going off line in the middle of a transmission,
/// which is what a trip is. `HRST` has no trip flag, so it is read from the
/// power: in line the amplifier has gain, off line its output is no more than
/// the drive passed through. A real HR500 trip (160 m overdrive, Oct 2026)
/// read 0 W out on 5.6 W in, with VDD sagging from 57 to 42 V as the
/// protection cut the drain supply.
#[derive(Default)]
struct TripWatch {
    keyed_polls: u32,
    dead_polls: u32,
    /// The last reading of this transmission with the amplifier in line.
    last_good: Option<AmpReadings>,
}

impl TripWatch {
    /// Polls of a key-up let pass before judging, while relays settle.
    const SETTLE: u32 = 2;
    /// Polls in a row with no gain before calling it a trip.
    const CONFIRM: u32 = 2;

    fn reset(&mut self) {
        *self = TripWatch::default();
    }

    /// One reading taken while keyed in PTT. `true` once the amplifier has
    /// been seen off line.
    fn keyed(&mut self, l: &AmpLimits, r: &AmpReadings) -> bool {
        self.keyed_polls += 1;
        let no_gain = r.drive_w > 1.0 && r.fwd_w < r.drive_w * 3.0;
        if !no_gain {
            self.last_good = Some(*r);
        }
        // The drain supply collapsing under drive is the protection itself
        // acting, so it needs no confirming.
        let sagged = r.vdd_v > 0.0 && r.vdd_v < l.vdd_low_v;
        if no_gain && sagged {
            return true;
        }
        self.dead_polls =
            if no_gain && self.keyed_polls > Self::SETTLE { self.dead_polls + 1 } else { 0 };
        self.dead_polls >= Self::CONFIRM
    }

    /// Mark the transmission tripped, blaming the likeliest reading when none
    /// was caught past its trip level.
    fn trip(&self, alerts: &mut AmpAlerts, l: &AmpLimits, now: &AmpReadings) {
        alerts.tripped = true;
        if let (true, Some(g)) = (alerts.trips().is_empty(), self.cause(l, now)) {
            alerts.blame(g);
        }
    }

    /// The likeliest cause of a trip when no reading was caught past its
    /// trip level; the amplifier tends not to answer in the moment before
    /// it trips. Drive that jumped since the last good reading is the drive;
    /// otherwise whichever reading stood nearest its trip level.
    fn cause(&self, l: &AmpLimits, now: &AmpReadings) -> Option<AmpGauge> {
        let g = self.last_good?;
        if now.drive_w > g.drive_w * 1.5 {
            return Some(AmpGauge::Drive);
        }
        [
            (AmpGauge::Fwd, g.fwd_w / l.fwd_trip_w),
            (AmpGauge::Rfl, g.rfl_w / l.rfl_trip_w),
            (AmpGauge::Drive, g.drive_w / l.drive_trip_w),
            (AmpGauge::Idd, g.idd_a / l.idd_trip_a),
        ]
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
    }
}

/// The amplifier's own lights as the panel's alerts, given whether a
/// transmission has been seen since the link opened.
///
/// Until this link has seen a transmission, yellows show green. A red is a
/// trip and is shown whenever it happened.
///
/// Why: the HR500 holds a yellow until the next key-up, and can start up with
/// DRV yellow with nothing wrong (a firmware quirk). From remote the operator
/// can't see the amplifier, so a yellow there reads as a fault when there is
/// none, and a warning that is often false teaches people to ignore it
/// (AJ3G, 18 Oct 2026). Decision 1 in docs/hardrock/design-decisions.md.
fn lights_seen(l: Lights, seen_tx: bool) -> AmpAlerts {
    let a = lights_alerts(l);
    if seen_tx {
        return a;
    }
    let calm = |v: AmpLevel| if v == AmpLevel::Yellow { AmpLevel::Green } else { v };
    AmpAlerts {
        fwd: calm(a.fwd),
        rfl: calm(a.rfl),
        drive: calm(a.drive),
        vdd: calm(a.vdd),
        idd: calm(a.idd),
        ..a
    }
}

/// The amplifier's own lights as the panel's alerts. Red on any of them is
/// a trip.
fn lights_alerts(l: Lights) -> AmpAlerts {
    let level = |d: u8| match d {
        3 => AmpLevel::Red,
        2 => AmpLevel::Yellow,
        _ => AmpLevel::Green,
    };
    let a = AmpAlerts {
        fwd: level(l.fwd),
        rfl: level(l.rfl),
        drive: level(l.drv),
        vdd: level(l.vdd),
        idd: level(l.idd),
        tripped: false,
        from_amp: true,
    };
    AmpAlerts { tripped: [a.fwd, a.rfl, a.drive, a.vdd, a.idd].contains(&AmpLevel::Red), ..a }
}

enum Msg {
    Action(AmpAction),
    Frequency(u64),
}

/// Whether the link should call the amplifier lost and reopen the port:
/// `misses` status reads unanswered in a row, the last answer at
/// `last_answer`.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): at rest the link is
/// lost after [`MISSES`] unanswered reads, as always. While transmitting (the
/// panel says the radio is keyed, or the last answer showed the amplifier
/// keyed) the amplifier often leaves its queries unanswered, so it is lost
/// only once [`TX_GRACE`] has also passed with no answer. Meanwhile the last
/// readings stay in the snapshot with their age (`readings_at`), and the
/// panel judges by that age, never taking a held reading as live.
fn link_lost(misses: u32, last_answer: Instant, now: Instant, transmitting: bool) -> bool {
    misses >= MISSES && (!transmitting || now.saturating_duration_since(last_answer) >= TX_GRACE)
}

/// A running link to the amplifier. Dropping it closes the port.
pub struct AmpLink {
    tx: Option<Sender<Msg>>,
    status: Arc<Mutex<AmpStatus>>,
    /// The radio is keyed, as the panel last said (see [`link_lost`]).
    radio_keyed: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AmpLink {
    pub fn start(cfg: AmpConfig) -> AmpLink {
        let (tx, rx) = bounded(16);
        let status = Arc::new(Mutex::new(AmpStatus {
            configured: true,
            model: cfg.model,
            ..AmpStatus::default()
        }));
        let st = status.clone();
        let radio_keyed = Arc::new(AtomicBool::new(false));
        let keyed = radio_keyed.clone();
        let thread = thread::Builder::new()
            .name("hardrock".into())
            .spawn(move || worker(cfg, rx, st, keyed))
            .ok();
        AmpLink { tx: Some(tx), status, radio_keyed, thread }
    }

    /// Tell the link whether the radio is transmitting, every frame: while
    /// it is, unanswered reads are given [`TX_GRACE`] before the link is
    /// called lost (see [`link_lost`]).
    pub fn set_transmitting(&self, keyed: bool) {
        self.radio_keyed.store(keyed, Ordering::Relaxed);
    }

    /// The latest snapshot.
    pub fn status(&self) -> AmpStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Carry out a button. The control shows as asked at once (see
    /// [`AmpStatus::pending`]); the command goes out ahead of the next read.
    pub fn action(&self, a: AmpAction) {
        if let Some(tx) = &self.tx {
            queue(tx, &self.status, a);
        }
    }

    /// Tell the amplifier the radio's frequency; it picks its band from it.
    /// Held until the amplifier shows no RF (interlock #3), then sent.
    pub fn set_frequency(&self, hz: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::Frequency(hz));
        }
    }
}

impl Drop for AmpLink {
    fn drop(&mut self) {
        // Closing the channel is the worker's signal to stop.
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn set(status: &Mutex<AmpStatus>, f: impl FnOnce(&mut AmpStatus)) {
    f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
}

fn open(cfg: &AmpConfig) -> crate::Result<Amp<Box<dyn serialport::SerialPort>>> {
    let port = serialport::new(&cfg.serial.path, cfg.serial.baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(50))
        .open()?;
    let _ = port.clear(serialport::ClearBuffer::All);
    let mut amp = Amp::new(port).with_timeout(Duration::from_millis(400));
    let mut last = None;
    for _ in 0..OPEN_PINGS {
        match amp.query(&Command::Ping) {
            Ok(_) => return Ok(amp),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("OPEN_PINGS > 0"))
}

/// What the operator is told when the port won't open or the amp won't answer.
fn open_error(path: &str, e: &crate::Error) -> String {
    match e {
        // serialport reports a port another program holds ("Access is
        // denied" on Windows) as NoDevice, the same as one that isn't there.
        // AJ3G, 19 Oct 2026: told "COM5 not found" with the amplifier plugged
        // in, he unplugged it and reinstalled before it connected. A port that
        // is still listed is there, so it is busy.
        crate::Error::Serial(se)
            if se.kind() == serialport::ErrorKind::NoDevice && port_listed(path) =>
        {
            in_use(path)
        }
        crate::Error::Serial(se)
            if matches!(
                se.kind(),
                serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
            ) =>
        {
            in_use(path)
        }
        crate::Error::Serial(se) if se.kind() == serialport::ErrorKind::NoDevice => {
            format!("{path} not found. Check the amplifier's USB lead.")
        }
        crate::Error::Serial(se)
            if matches!(se.kind(), serialport::ErrorKind::Io(std::io::ErrorKind::NotFound)) =>
        {
            format!("{path} not found. Check the amplifier's USB lead.")
        }
        crate::Error::Timeout(_) => {
            format!(
                "{path} is open but the amplifier isn't answering. Is it on, at the set baud rate?"
            )
        }
        _ => format!("{path}: {e}"),
    }
}

/// Whether the system lists `path` as a serial port.
fn port_listed(path: &str) -> bool {
    serialport::available_ports()
        .map(|v| v.iter().any(|p| p.port_name.eq_ignore_ascii_case(path)))
        .unwrap_or(false)
}

fn in_use(path: &str) -> String {
    format!(
        "{path} is in use by another program: HobbyPCB's control panel, SDRoxide's amplifier \
         link, or a second copy of this panel. Close it and this connects by itself."
    )
}

fn worker(
    cfg: AmpConfig,
    rx: Receiver<Msg>,
    status: Arc<Mutex<AmpStatus>>,
    radio_keyed: Arc<AtomicBool>,
) {
    if cfg.serial.path.trim().is_empty() {
        set(&status, |s| s.error = Some("No serial port chosen".into()));
        // Nothing to do but wait to be dropped.
        while rx.recv().is_ok() {
            set(&status, |s| s.pending.clear());
        }
        return;
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        match open(&cfg) {
            Ok(amp) => {
                info!(port = %cfg.serial.path, baud = cfg.serial.baud, "hardrock: connected");
                backoff = Duration::from_secs(1);
                set(&status, |s| {
                    s.connected = true;
                    s.error = None;
                });
                if !run(amp, &rx, &status, cfg.model.limits(), &radio_keyed) {
                    return;
                }
                set(&status, |s| {
                    s.connected = false;
                    s.readings = None;
                    s.readings_at = None;
                    s.atu = None;
                    s.pending.clear();
                    s.error = Some("The amplifier stopped answering".into());
                });
            }
            Err(e) => {
                debug!("hardrock: {e}");
                set(&status, |s| {
                    s.connected = false;
                    s.error = Some(open_error(&cfg.serial.path, &e));
                });
            }
        }
        // Wait out the backoff, still draining the channel so a drop is seen.
        let until = Instant::now() + backoff;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
                // Nothing is sent while the port is closed.
                Ok(_) => set(&status, |s| s.pending.clear()),
            }
        }
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

/// One read the link makes between the panel's clicks. Each is a single
/// query, and the channel is looked at before every one, so a click waits
/// behind at most one read (up to the reply timeout when the amplifier skips
/// its answer) rather than a whole round of them. Before (AJ3G, 21 Oct 2026:
/// the AMP buttons were slow), a click waited behind the status, the
/// temperature and four tuner reads, well over a second when the amplifier
/// left the tuner's queries unanswered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    /// `HRST`, every reading at once.
    Status,
    /// A band change held back by interlock #3: a status read, then the
    /// frequency if it is clear.
    Band,
    /// `HRTP`.
    Temp,
    /// The tuner, one query each: `HRTT`, `HRTB`, `HRTMP`, and `HRTR` once a
    /// tune has ended.
    AtuTuning,
    AtuActive,
    AtuParams,
    AtuResult,
}

/// The last status frame the amplifier answered: when, and whether it showed
/// RF and how much drive. Interlocks #1 and #4 take it in place of a read of
/// their own when it is no older than [`FRESH`].
#[derive(Clone, Copy, Debug)]
struct Clear {
    at: Instant,
    /// Keyed, or drive or output showing (see [`rf_in`]).
    rf: bool,
    drive_w: f32,
}

/// How old a status answer may be and still stand for the read interlocks
/// #1 and #4 make just before a command. Shorter than any transmission can
/// start and show on the amplifier's meters, and short enough that it is
/// only ever used for a click that arrives straight after a status read;
/// any later click gets a read of its own, as before.
const FRESH: Duration = Duration::from_millis(100);

/// How long a clicked control is shown as asked while the amplifier is read
/// back still in its old state (it often answers the first read after a
/// change with the old one); after that the control shows what is read.
const PENDING_HOLD: Duration = Duration::from_secs(1);

/// The pause between cancelling a tune (`HRTU;`) and bypassing the tuner,
/// so the tuner has let the tune go before it is told to bypass.
const CANCEL_SETTLE: Duration = Duration::from_millis(100);

/// Whether a status frame shows RF in the amplifier: keyed (its TX light),
/// drive of 0.1 W or more, or output of 1 W or more. Interlocks #1 and #3
/// refuse a relay switch when it does.
fn rf_in(s: &crate::Status) -> bool {
    s.lights().is_some_and(|l| l.tx) || s.forward_w >= 1.0 || s.drive_w >= 0.1
}

/// Put a click on the link's channel and show it as asked at once (see
/// [`AmpStatus::pending`]). Not shown when the channel is full and the
/// click is dropped.
fn queue(tx: &Sender<Msg>, status: &Mutex<AmpStatus>, a: AmpAction) {
    if tx.try_send(Msg::Action(a)).is_ok() && a.control().is_some() {
        set(status, |s| {
            s.pending.retain(|p| p.control() != a.control());
            s.pending.push(a);
        });
    }
}

/// A click the amplifier has been sent and not yet read back doing.
#[derive(Clone, Copy, Debug)]
struct Sent {
    /// As sent: a band step is the band it stepped to.
    action: AmpAction,
    at: Instant,
}

/// Whether a reading shows `a` done.
fn shows(a: AmpAction, r: &AmpReadings, atu_active: Option<bool>) -> bool {
    match a {
        AmpAction::SetPtt(on) => r.ptt == on,
        AmpAction::SetAntenna(n) => r.antenna == n.clamp(1, 2),
        AmpAction::SetBand(code) => r.band == Some(code),
        AmpAction::SetAtu(on) => atu_active == Some(on),
        _ => true,
    }
}

/// The reads due now, most urgent first: the status frame when its time
/// has come, except that straight after one, one other due read goes first,
/// so a run of unanswered status reads (each its full timeout) cannot shut
/// out the temperature and the tuner.
fn next_read(
    now: Instant,
    last: Option<Read>,
    status_due: Instant,
    extras: impl IntoIterator<Item = (Read, bool)>,
) -> Option<Read> {
    let extra = extras.into_iter().find(|&(_, due)| due).map(|(r, _)| r);
    let status = now >= status_due;
    match (status, extra) {
        (true, Some(e)) if last == Some(Read::Status) => Some(e),
        (true, _) => Some(Read::Status),
        (false, e) => e,
    }
}

/// Talk to an open amplifier until it stops answering (`true`) or the link is
/// dropped (`false`).
fn run<P: std::io::Read + std::io::Write>(
    mut amp: Amp<P>,
    rx: &Receiver<Msg>,
    status: &Mutex<AmpStatus>,
    limits: AmpLimits,
    radio_keyed: &AtomicBool,
) -> bool {
    let has_atu = matches!(amp.query(&Command::AtuPresent), Ok(Response::AtuPresent(true)));
    set(status, |s| s.atu = has_atu.then(AtuReadout::default));
    let mut celsius: Option<bool> = None;
    let mut misses = 0;
    // When the amplifier last answered a status read (it answered the pings
    // that opened the port).
    let mut last_answer = Instant::now();
    let mut next_status = Instant::now();
    let mut next_atu = Instant::now();
    let mut next_temp = Instant::now();
    // The tuner's queries still to make in this reading of it.
    let mut atu_left: Vec<Read> = Vec::new();
    let mut last_read: Option<Read> = None;
    // A C/F toggle not yet seen on the amplifier, and how long to keep looking.
    let mut scale_wanted: Option<(bool, Instant)> = None;
    // The temperature as `HRTP` last gave it, in the scale it gave, and when.
    let mut screen_temp: Option<(f32, Instant)> = None;
    let mut was_tuning = false;
    let mut alerts = AmpAlerts::default();
    let mut was_keyed = false;
    // A transmission seen since this link opened (see `lights_seen`).
    let mut seen_tx = false;
    let mut watch = TripWatch::default();
    // When the operator last switched PTT/OFF, so the amplifier dropping to
    // OFF by itself mid-transmission can be told from a click.
    let mut ptt_set_at: Option<Instant> = None;
    // The radio's frequency waiting to be sent (#3), and when to try next.
    let mut want_hz: Option<u64> = None;
    let mut next_band_try = Instant::now();
    // The last status the amplifier answered (#1, #4).
    let mut clear: Option<Clear> = None;
    // Clicks sent and not yet read back.
    let mut sent: Vec<Sent> = Vec::new();
    loop {
        // ── the panel first: a click goes out before the next read ─────────
        let now = Instant::now();
        let extras = [
            (Read::Band, want_hz.is_some() && now >= next_band_try),
            (Read::Temp, scale_wanted.is_some_and(|(_, until)| now < until) || now >= next_temp),
        ];
        let atu_due = has_atu && (!atu_left.is_empty() || was_tuning || now >= next_atu);
        if has_atu && atu_left.is_empty() && atu_due {
            atu_left = vec![Read::AtuParams, Read::AtuActive, Read::AtuTuning];
        }
        let atu_next = atu_left.last().copied();
        let read = next_read(
            now,
            last_read,
            next_status,
            extras.into_iter().chain(atu_next.map(|r| (r, true))),
        );
        let wait = match read {
            Some(_) => Duration::ZERO,
            None => {
                let mut until = next_status;
                if want_hz.is_some() {
                    until = until.min(next_band_try);
                }
                until = until.min(next_temp);
                if has_atu {
                    until = until.min(next_atu);
                }
                until.saturating_duration_since(now)
            }
        };
        match rx.recv_timeout(wait) {
            Err(RecvTimeoutError::Disconnected) => return false,
            Ok(Msg::Action(a)) => {
                if matches!(a, AmpAction::SetPtt(_)) {
                    ptt_set_at = Some(Instant::now());
                }
                // A band step is taken from the band last asked for while
                // it is still being read back, so two quick clicks step two
                // bands.
                let band_base = sent
                    .iter()
                    .find_map(|s| match s.action {
                        AmpAction::SetBand(c) => Some(c),
                        _ => None,
                    })
                    .or_else(|| status.lock().unwrap_or_else(|e| e.into_inner()).readings?.band);
                let done = act(&mut amp, a, band_base, &limits, clear);
                if let Ok(Some(_)) = done {
                    // Anything the amplifier said before this is out of date.
                    clear = None;
                }
                match done {
                    Err(e) => {
                        warn!("hardrock: {a:?}: {e}");
                        drop_pending(status, &mut sent, a);
                        if let Some(why) = refused_words(a, false) {
                            notify(status, why);
                        }
                    }
                    Ok(None) => {
                        drop_pending(status, &mut sent, a);
                        if let Some(why) = refused_words(a, true) {
                            notify(status, why);
                        }
                    }
                    Ok(Some(did)) => {
                        set(status, |st| st.notice = None);
                        // The drawing follows the click at once rather than
                        // waiting for the relays: the amplifier answers a
                        // read straight after with the old state.
                        if let AmpAction::SetAtu(on) = a {
                            set(status, |st| {
                                let atu = st.atu.get_or_insert_with(AtuReadout::default);
                                atu.active = on;
                                if let Some(n) = atu.network.as_mut() {
                                    n.bypassed = !on;
                                }
                            });
                        }
                        if did.control().is_some() {
                            sent.retain(|s| s.action.control() != did.control());
                            sent.push(Sent { action: did, at: Instant::now() });
                            set(status, |st| {
                                st.pending.retain(|p| p.control() != did.control());
                                st.pending.push(did);
                            });
                        }
                        // Read the result back next.
                        next_status = Instant::now();
                    }
                }
                // Read the tuner back once the relays have settled, to
                // confirm the click.
                next_atu = Instant::now() + ATU_SETTLE;
                atu_left.clear();
                // After a C/F toggle the scale is read on every poll until
                // the amplifier has switched: the status reading arrives in
                // the new unit, and a letter left over from before put the
                // wrong one beside it until the next ten-second read (AJ3G).
                if let AmpAction::SetCelsius(c) = a {
                    scale_wanted = Some((c, Instant::now() + SCALE_SETTLE));
                }
                // Look at the channel again before any read.
                continue;
            }
            Ok(Msg::Frequency(hz)) => {
                want_hz = Some(hz);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        // A click the amplifier still reads in its old state is let go.
        let expired: Vec<AmpAction> =
            sent.iter().filter(|s| s.at.elapsed() >= PENDING_HOLD).map(|s| s.action).collect();
        for a in expired {
            debug!("hardrock: {a:?} not read back; showing the amplifier's state");
            drop_pending(status, &mut sent, a);
        }

        let Some(read) = read else { continue };
        last_read = Some(read);
        match read {
            // SAFETY INTERLOCK (docs/safety-interlocks.md, #3): the radio's
            // frequency, which sets the amplifier's band, is never sent while
            // there is RF in the amplifier. A band change switches the
            // low-pass filter relays; under power that arcs the contacts and
            // can leave the output devices driving an open or wrong filter.
            // It is held and sent once the amplifier shows no transmission,
            // no drive and no output. As in #1, no answer to the status read
            // counts as "not safe". This read is always its own, never
            // `clear`.
            Read::Band => {
                next_band_try = Instant::now() + BAND_RETRY;
                match amp.status() {
                    Ok(s) if !rf_in(&s) => {
                        if let Some(hz) = want_hz.take() {
                            let _ = amp.set_frequency(hz);
                        }
                    }
                    _ => debug!("hardrock: band change held: transmitting or no answer"),
                }
            }
            Read::Temp => {
                next_temp = Instant::now() + if was_keyed { TEMP_POLL_TX } else { TEMP_POLL };
                match amp.query(&Command::GetTemperature) {
                    Ok(Response::Temperature(v, Some(scale))) => {
                        celsius = Some(scale == TempScale::C);
                        screen_temp = Some((v, Instant::now()));
                    }
                    // No scale letter: the scale last seen still holds.
                    Ok(Response::Temperature(v, None)) if celsius.is_some() => {
                        screen_temp = Some((v, Instant::now()));
                    }
                    other => debug!("HRTP not read: {other:?}"),
                }
                let settling = scale_wanted.filter(|&(_, until)| Instant::now() < until);
                scale_wanted = settling.filter(|&(want, _)| celsius != Some(want));
            }
            Read::Status => {
                let asked = Instant::now();
                next_status = asked + POLL;
                match amp.status() {
                    Ok(s) => {
                        misses = 0;
                        last_answer = Instant::now();
                        clear =
                            Some(Clear { at: Instant::now(), rf: rf_in(&s), drive_w: s.drive_w });
                        let r = AmpReadings {
                            fwd_w: s.forward_w,
                            rfl_w: s.reflected_w,
                            drive_w: s.drive_w,
                            swr: s.swr,
                            vdd_v: s.vdd_v,
                            idd_a: s.idd_a,
                            temperature: shown_temp(
                                fresh_temp(screen_temp, Instant::now()),
                                s.temperature,
                                s.mode == KeyMode::Ptt || was_keyed,
                            ),
                            celsius,
                            ptt: s.mode == KeyMode::Ptt,
                            band: s.band.map(Band::code),
                            antenna: s.antenna,
                        };
                        if let Some(l) = s.lights() {
                            // The amplifier's own lights: shown as sent.
                            seen_tx |= l.tx;
                            alerts = lights_seen(l, seen_tx);
                            was_keyed = l.tx;
                        } else {
                            // Older firmware sends no lights, so they are
                            // worked out as on the amplifier's screen: a
                            // key-up clears the indicators, and the worst of
                            // the transmission stays lit. A reading past a
                            // trip level is only shown as a trip once the
                            // amplifier is seen to go off line.
                            let keyed = r.ptt && r.drive_w > KEYED_DRIVE_W;
                            if keyed && !was_keyed {
                                alerts = AmpAlerts::default();
                                watch.reset();
                            }
                            if keyed {
                                alerts.hold(&limits, &r);
                                if watch.keyed(&limits, &r) && !alerts.tripped {
                                    watch.trip(&mut alerts, &limits, &r);
                                }
                            }
                            let clicked_off =
                                ptt_set_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                            if was_keyed
                                && !r.ptt
                                && r.drive_w > KEYED_DRIVE_W
                                && !clicked_off
                                && !alerts.tripped
                            {
                                // Still driven, but the amplifier switched
                                // itself to OFF.
                                watch.trip(&mut alerts, &limits, &r);
                            }
                            was_keyed = keyed;
                        }
                        let tx_light = s.lights().map(|l| l.tx);
                        set(status, |st| {
                            st.readings = Some(r);
                            st.readings_at = Some(last_answer);
                            st.alerts = alerts;
                            st.transmitting = tx_light;
                        });
                        // Clicks sent before this read and shown in it.
                        let done: Vec<AmpAction> = sent
                            .iter()
                            .filter(|c| c.at <= asked && shows(c.action, &r, None))
                            .map(|c| c.action)
                            .filter(|a| !matches!(a, AmpAction::SetAtu(_)))
                            .collect();
                        for a in done {
                            drop_pending(status, &mut sent, a);
                        }
                    }
                    Err(e) => {
                        misses += 1;
                        debug!("hardrock: status: {e}");
                        let keyed = radio_keyed.load(Ordering::Relaxed) || was_keyed;
                        if link_lost(misses, last_answer, Instant::now(), keyed) {
                            return true;
                        }
                        if misses == MISSES && keyed {
                            info!(
                                "hardrock: no answer while transmitting; the link is held for up \
                                 to {} s",
                                TX_GRACE.as_secs()
                            );
                        }
                    }
                }
            }
            Read::AtuTuning => {
                atu_left.pop();
                let tuning = matches!(amp.query(&Command::IsTuning), Ok(Response::Tuning(true)));
                if was_tuning && !tuning {
                    atu_left.insert(0, Read::AtuResult);
                }
                was_tuning = tuning;
                set(status, |st| st.atu.get_or_insert_with(AtuReadout::default).tuning = tuning);
            }
            Read::AtuActive => {
                atu_left.pop();
                let asked = Instant::now();
                if let Ok(Response::AtuActive(a)) = amp.query(&Command::GetAtuActive) {
                    set(status, |st| st.atu.get_or_insert_with(AtuReadout::default).active = a);
                    // ON or BYP sent, its relays given time to settle, and
                    // the tuner still reads the other way: say so.
                    let stuck = sent.iter().find_map(|c| match c.action {
                        AmpAction::SetAtu(on) if on != a && c.at + ATU_SETTLE <= asked => Some(on),
                        _ => None,
                    });
                    if let Some(on) = stuck {
                        warn!("hardrock: tuner {} asked, still reads otherwise", atu_word(on));
                        notify(
                            status,
                            if on {
                                "The tuner still reads bypassed after ON.".into()
                            } else {
                                "The tuner still reads in line after BYP.".into()
                            },
                        );
                    }
                    let done: Vec<AmpAction> = sent
                        .iter()
                        .filter(|c| {
                            c.at <= asked && shows(c.action, &AmpReadings::default(), Some(a))
                        })
                        .filter(|c| matches!(c.action, AmpAction::SetAtu(_)))
                        .map(|c| c.action)
                        .collect();
                    for a in done {
                        drop_pending(status, &mut sent, a);
                    }
                }
            }
            Read::AtuParams => {
                atu_left.pop();
                if let Ok(Response::AtuParams(p)) = amp.query(&Command::AtuParams) {
                    set(status, |st| {
                        st.atu.get_or_insert_with(AtuReadout::default).network = Some(network_of(p))
                    });
                }
            }
            Read::AtuResult => {
                atu_left.pop();
                if let Ok(Response::TuneResult(r)) = amp.query(&Command::LastTuneResult) {
                    set(status, |st| {
                        st.atu.get_or_insert_with(AtuReadout::default).last_result =
                            Some(result_text(r))
                    });
                }
            }
        }
        if matches!(read, Read::AtuTuning | Read::AtuActive | Read::AtuParams | Read::AtuResult)
            && atu_left.is_empty()
        {
            next_atu = Instant::now() + ATU_POLL;
        }
    }
}

/// Show the operator why a click did nothing (see [`AmpStatus::notice`]).
fn notify(status: &Mutex<AmpStatus>, why: String) {
    set(status, |st| {
        st.notice = Some(why);
        st.notice_at = Some(Instant::now());
    });
}

/// ON or BYP, as the tuner's key reads.
fn atu_word(on: bool) -> &'static str {
    if on { "ON" } else { "BYP" }
}

/// Why a click was refused, in words for the AMP window: `refused` when
/// interlock #1 or #4 refused it, else when the amplifier didn't answer.
/// `None` for clicks that need no word (TUNE, the temperature scale).
fn refused_words(a: AmpAction, refused: bool) -> Option<String> {
    let what = match a {
        AmpAction::SetAtu(on) => format!("the tuner can't switch to {}", atu_word(on)),
        AmpAction::SetAntenna(_) => "the antenna relay can't switch".into(),
        AmpAction::SetBand(_) | AmpAction::BandUp | AmpAction::BandDown => {
            "the band can't change".into()
        }
        AmpAction::SetPtt(true) if refused => {
            return Some("Lower the drive first: it is over the amplifier's limit for PTT.".into());
        }
        AmpAction::SetPtt(_) if !refused => "PTT ON/OFF".into(),
        _ => return None,
    };
    Some(if refused {
        format!("Unkey first: {what} with RF on.")
    } else {
        format!("The amplifier didn't answer, so {what} wasn't sent. Try again.")
    })
}

/// Stop showing `a`'s control as asked: done, refused or not read back.
fn drop_pending(status: &Mutex<AmpStatus>, sent: &mut Vec<Sent>, a: AmpAction) {
    sent.retain(|s| s.action.control() != a.control());
    set(status, |st| st.pending.retain(|p| p.control() != a.control()));
}

fn network_of(p: AtuParams) -> AtuNetwork {
    AtuNetwork {
        l_step: p.l_step,
        c_step: p.c_step,
        bypassed: p.bypassed,
        cap_output: p.cap_output,
        swr: p.swr(),
    }
}

fn result_text(r: TuneResult) -> String {
    match r {
        TuneResult::Tuned => "Tuned".into(),
        TuneResult::Aborted => "Aborted".into(),
        TuneResult::SwrTooHigh => "SWR above 6:1".into(),
        TuneResult::NotBelow2to1 => "Failed (above 2:1)".into(),
        TuneResult::PowerTooHigh => "Power too high".into(),
        TuneResult::PowerTooLow => "Lo power".into(),
        TuneResult::Unknown(c) => format!("Result {c}"),
    }
}

/// Carries out one button. `Ok(Some(..))` with what was sent, a band step
/// as the band it stepped to; `Ok(None)` when it was refused (a relay switch
/// while transmitting, or going in line overdriven). A band step is taken
/// from `band_now`.
///
/// `clear` is the last status the amplifier answered. Interlocks #1 and #4
/// read the amplifier's status just before the command; a status answered
/// within [`FRESH`] that passes the same test stands in for that read, so a
/// click straight after a poll does not wait for a second one.
fn act<P: std::io::Read + std::io::Write>(
    amp: &mut Amp<P>,
    a: AmpAction,
    band_now: Option<u8>,
    limits: &AmpLimits,
    clear: Option<Clear>,
) -> crate::Result<Option<AmpAction>> {
    let fresh = clear.filter(|c| c.at.elapsed() <= FRESH);
    // SAFETY INTERLOCK (docs/safety-interlocks.md, #1): never switch a relay
    // in the RF path (antenna, band filter, tuner) with RF on it.
    // Hot-switching opens the load under power: the reflected power spike
    // makes the amplifier raise its RFL flag (seen on AJ3G's HR500, 17 Oct
    // 2026), stresses the VRF2944 output devices, and arcs and pits the relay
    // contacts. The panel greys those buttons while transmitting (#2); this
    // is the last check, made against the amplifier itself just before the
    // command goes out, so it also covers a remote client or a click that
    // raced the key-up. Things are left as they are if the amplifier is
    // keyed, has drive or output, or does not answer (it often skips its
    // status reply mid-transmission), because "don't know" is not "safe".
    // A status the amplifier answered no more than FRESH (100 ms) ago and
    // showing no RF stands in for the read; anything older, or showing RF,
    // and the status is read again here.
    let switches_rf = matches!(
        a,
        AmpAction::SetAntenna(_)
            | AmpAction::SetBand(_)
            | AmpAction::BandUp
            | AmpAction::BandDown
            | AmpAction::SetAtu(_)
    );
    if switches_rf && !fresh.is_some_and(|c| !c.rf) && rf_in(&amp.status()?) {
        warn!("hardrock: {a:?} refused: transmitting");
        return Ok(None);
    }
    // SAFETY INTERLOCK (docs/safety-interlocks.md, #4): never put the
    // amplifier in line (OFF to PTT) while the radio is driving it harder
    // than its drive limit. In OFF the radio's power goes straight through,
    // so a 100 W radio running barefoot is fine there; switched in line, it
    // would drive the amplifier far past its trip level and can damage the
    // output devices. Checked against the amplifier itself just before the
    // command goes out; no answer refuses it, as in #1. A status answered no
    // more than FRESH ago with the drive under the limit stands in for the
    // read, as in #1. The panel greys the button too (#4, AMP window).
    if a == AmpAction::SetPtt(true) {
        let drive_w = match fresh {
            Some(c) if c.drive_w <= limits.drive_warn_w => c.drive_w,
            _ => amp.status()?.drive_w,
        };
        if drive_w > limits.drive_warn_w {
            warn!("hardrock: {a:?} refused: drive {drive_w:.1} W is over the amplifier's limit");
            return Ok(None);
        }
    }
    // Codes run 1 = 10 m to 10 = 160 m, so "up" in frequency is down in code.
    let step = |up: bool| match band_now {
        Some(c) if up => c.saturating_sub(1).max(1),
        Some(c) => (c + 1).min(10),
        None => Band::M20.code(),
    };
    let did = match a {
        AmpAction::BandUp => AmpAction::SetBand(step(true)),
        AmpAction::BandDown => AmpAction::SetBand(step(false)),
        other => other,
    };
    match did {
        AmpAction::SetPtt(on) => amp.set_mode(if on { KeyMode::Ptt } else { KeyMode::Off }),
        AmpAction::SetAntenna(n) => amp.set_antenna(n.clamp(1, 2)),
        // A step past the end of the bands sends nothing.
        AmpAction::SetBand(code) if Some(code) == band_now && a != AmpAction::SetBand(code) => {
            Ok(())
        }
        AmpAction::SetBand(code) => match Band::from_code(code) {
            Some(b) => amp.set_band(b),
            None => Ok(()),
        },
        AmpAction::Tune => amp.send(&Command::Tune),
        AmpAction::SetAtu(false) => {
            // A tune still running, or armed and waiting for a carrier, keeps
            // the tuner in line: AJ3G pressed BYP after a failed tune and it
            // stayed in. HRTU cancels a tune in progress, but it is also the
            // TUNE button and starts one, so it goes only when the tuner says
            // it is tuning now; no answer, not sent.
            if let Ok(Response::Tuning(true)) = amp.query(&Command::IsTuning) {
                info!("hardrock: BYP while the tuner tunes: cancelling the tune first");
                amp.send(&Command::Tune)?;
                std::thread::sleep(CANCEL_SETTLE);
            }
            amp.send(&Command::SetAtuActive(false))
        }
        AmpAction::SetAtu(true) => amp.send(&Command::SetAtuActive(true)),
        AmpAction::SetCelsius(c) => {
            amp.send(&Command::SetTempScale(if c { TempScale::C } else { TempScale::F }))
        }
        AmpAction::BandUp | AmpAction::BandDown => Ok(()),
    }
    .map(|()| Some(did))
}

/// The temperature the panel shows. At rest, `HRTP`'s (the amplifier's own
/// screen), falling back to the status frame's when it stops coming. Keyed,
/// `HRTP` is read only every [`TEMP_POLL_TX`], so the status frame's, which
/// comes with every poll, is shown whenever it is the higher: the heat
/// spreader only climbs while transmitting, and a reading 10 s old was 11 °F
/// behind the amplifier's screen (AJ3G, 18 Oct 2026: 122 °F against 133 °F).
fn shown_temp(screen: Option<f32>, frame: f32, keyed: bool) -> f32 {
    match screen {
        Some(v) if keyed => v.max(frame),
        Some(v) => v,
        None => frame,
    }
}

/// The `HRTP` temperature while it is recent enough to show (see
/// [`TEMP_STALE`]).
fn fresh_temp(screen: Option<(f32, Instant)>, now: Instant) -> Option<f32> {
    screen.filter(|&(_, at)| now.saturating_duration_since(at) <= TEMP_STALE).map(|(v, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A simulated amplifier on a serial port: it answers each query after
    /// `latency`, or never when `skip` says so (the HR500 often leaves a
    /// query unanswered, mid-transmission most of all), and logs when each
    /// command reached it.
    #[derive(Clone)]
    struct SimPort(Arc<Mutex<Sim>>);

    struct Sim {
        latency: Duration,
        skip: fn(&str, u32) -> bool,
        /// Bytes written that don't yet end in `;`.
        partial: String,
        replies: VecDeque<(Instant, Vec<u8>)>,
        /// Every command, as it arrived.
        log: Vec<(Instant, String)>,
        queries: u32,
        ptt: bool,
        antenna: u8,
        atu: bool,
        /// Forward power in watts and drive in tenths, as the status frame
        /// reports them.
        fwd: u32,
        drive: u32,
        band: u8,
        /// The antenna relay ignores its command.
        stuck: bool,
        /// The tuner is tuning (`HRTT1`); `HRTU` starts or cancels a tune.
        tuning: bool,
        /// The tuner's relays ignore `HRTB`.
        atu_stuck: bool,
        /// Nothing is answered at all.
        silent: bool,
    }

    impl SimPort {
        fn new(latency: Duration, skip: fn(&str, u32) -> bool) -> SimPort {
            SimPort(Arc::new(Mutex::new(Sim {
                latency,
                skip,
                partial: String::new(),
                replies: VecDeque::new(),
                log: Vec::new(),
                queries: 0,
                ptt: false,
                antenna: 1,
                atu: false,
                fwd: 0,
                drive: 0,
                band: 7,
                stuck: false,
                tuning: false,
                atu_stuck: false,
                silent: false,
            })))
        }

        fn sim(&self) -> std::sync::MutexGuard<'_, Sim> {
            self.0.lock().unwrap()
        }

        /// When `cmd` was first seen at or after `since`.
        fn seen(&self, cmd: &str, since: Instant) -> Option<Instant> {
            self.sim().log.iter().find(|(t, c)| *t >= since && c == cmd).map(|(t, _)| *t)
        }
    }

    impl Sim {
        fn answer(&self, cmd: &str) -> Option<String> {
            Some(match cmd {
                "HRAP" => "HRAP1;".into(),
                "HRAA" => "HRAA;".into(),
                "HRST" => format!(
                    "HRST-{:03}-000-{:03}-000-058-000-075-{}-{:02}-{}-;",
                    self.fwd,
                    self.drive,
                    u8::from(self.ptt),
                    self.band,
                    self.antenna
                ),
                "HRTP" => "HRTP75F;".into(),
                "HRTB" => format!("HRTB{};", u8::from(self.atu)),
                "HRTT" => format!("HRTT{};", u8::from(self.tuning)),
                "HRTMP" => "HRTM-L012-C034-H1-B5-S000-F145-;".into(),
                "HRTR" => "HRTRS;".into(),
                _ => return None,
            })
        }

        fn command(&mut self, cmd: &str) {
            let now = Instant::now();
            self.log.push((now, cmd.to_string()));
            match cmd {
                "HRMD0" => self.ptt = false,
                "HRMD1" => self.ptt = true,
                "HRAN1" if !self.stuck => self.antenna = 1,
                "HRAN2" if !self.stuck => self.antenna = 2,
                c if c.starts_with("HRBN") && c.len() > 4 => {
                    self.band = c[4..].parse().unwrap_or(self.band)
                }
                // A tune in progress keeps the tuner in line.
                "HRTB0" if !self.atu_stuck && !self.tuning => self.atu = false,
                "HRTB1" if !self.atu_stuck => self.atu = true,
                "HRTU" => self.tuning = !self.tuning,
                _ => {}
            }
            if let Some(a) = self.answer(cmd) {
                self.queries += 1;
                if !self.silent && !(self.skip)(cmd, self.queries) {
                    self.replies.push_back((now + self.latency, a.into_bytes()));
                }
            }
        }
    }

    impl std::io::Write for SimPort {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut sim = self.sim();
            sim.partial.push_str(&String::from_utf8_lossy(buf));
            while let Some(i) = sim.partial.find(';') {
                let cmd: String = sim.partial.drain(..=i).collect();
                sim.command(cmd.trim_end_matches(';'));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl std::io::Read for SimPort {
        /// As a serial port with a short read timeout: whatever has arrived,
        /// or `TimedOut` after a few milliseconds of nothing.
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let until = Instant::now() + Duration::from_millis(5);
            loop {
                {
                    let mut sim = self.sim();
                    if let Some((at, _)) = sim.replies.front()
                        && *at <= Instant::now()
                    {
                        let (_, r) = sim.replies.pop_front().unwrap();
                        buf[..r.len()].copy_from_slice(&r);
                        return Ok(r.len());
                    }
                }
                if Instant::now() >= until {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// A link on a simulated amplifier, with the same reply timeout as a real
    /// port.
    struct SimLink {
        port: SimPort,
        tx: Option<Sender<Msg>>,
        status: Arc<Mutex<AmpStatus>>,
        thread: Option<JoinHandle<()>>,
    }

    impl SimLink {
        fn start(port: SimPort) -> SimLink {
            let (tx, rx) = bounded(16);
            let status = Arc::new(Mutex::new(AmpStatus {
                configured: true,
                connected: true,
                ..AmpStatus::default()
            }));
            let (p, st) = (port.clone(), status.clone());
            let thread = thread::spawn(move || {
                let amp = Amp::new(p).with_timeout(Duration::from_millis(400));
                let keyed = AtomicBool::new(false);
                run(amp, &rx, &st, crate::types::AmpModel::Hr500.limits(), &keyed);
            });
            SimLink { port, tx: Some(tx), status, thread: Some(thread) }
        }

        fn action(&self, a: AmpAction) {
            queue(self.tx.as_ref().unwrap(), &self.status, a);
        }

        fn status(&self) -> AmpStatus {
            self.status.lock().unwrap().clone()
        }
    }

    impl Drop for SimLink {
        fn drop(&mut self) {
            self.tx = None;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Click `n` times at scattered moments, alternating `a` and `b`, and
    /// give the time from each click to its command reaching the amplifier:
    /// (mean, worst) in milliseconds.
    fn click_latency(
        link: &SimLink,
        n: u32,
        clicks: [(AmpAction, &str); 2],
    ) -> (f64, f64, Vec<f64>) {
        // Let the first round of reads start.
        thread::sleep(Duration::from_millis(300));
        let mut got = Vec::new();
        let mut seed: u32 = 12345;
        for i in 0..n {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            thread::sleep(Duration::from_millis(u64::from(seed >> 16) % 700 + 100));
            let (a, cmd) = clicks[i as usize % 2];
            let t0 = Instant::now();
            link.action(a);
            let deadline = t0 + Duration::from_secs(5);
            let at = loop {
                if let Some(t) = link.port.seen(cmd, t0) {
                    break t;
                }
                assert!(Instant::now() < deadline, "{cmd} never sent");
                thread::sleep(Duration::from_millis(1));
            };
            got.push(at.duration_since(t0).as_secs_f64() * 1000.0);
        }
        let mean = got.iter().sum::<f64>() / got.len() as f64;
        let worst = got.iter().cloned().fold(0.0, f64::max);
        (mean, worst, got)
    }

    /// The tuner's and the temperature's queries go unanswered, as they often
    /// do while the amplifier is busy; the status frame is answered.
    fn skips_extras(cmd: &str, _: u32) -> bool {
        matches!(cmd, "HRTP" | "HRTB" | "HRTT" | "HRTMP" | "HRTR")
    }

    fn answers_all(_: &str, _: u32) -> bool {
        false
    }

    /// Measures how long a click waits before its command goes out, against
    /// a simulated amplifier that answers everything and one that leaves
    /// the tuner's and the temperature's queries unanswered. Run with
    /// `--nocapture` to see the figures.
    #[test]
    fn a_click_goes_out_without_waiting_for_a_round_of_reads() {
        let mut worst = Vec::new();
        for (name, skip) in [
            ("answers everything", answers_all as fn(&str, u32) -> bool),
            ("skips HRTP and the tuner reads", skips_extras),
        ] {
            let link = SimLink::start(SimPort::new(Duration::from_millis(25), skip));
            // PTT/OFF (the OFF to PTT one checks the drive first, #4) and
            // the antenna, a relay checked against a status read (#1).
            let ptt = click_latency(
                &link,
                16,
                [(AmpAction::SetPtt(true), "HRMD1"), (AmpAction::SetPtt(false), "HRMD0")],
            );
            let ant = click_latency(
                &link,
                16,
                [(AmpAction::SetAntenna(2), "HRAN2"), (AmpAction::SetAntenna(1), "HRAN1")],
            );
            eprintln!(
                "click to send, amplifier {name}: PTT/OFF mean {:.0} ms, worst {:.0} ms; \
                 ANT mean {:.0} ms, worst {:.0} ms",
                ptt.0, ptt.1, ant.0, ant.1
            );
            worst.push((ptt.1, ant.1));
        }
        // Answered, a click waits at most for one reply in flight and the
        // status read of #1 or #4; with replies skipped, for one timeout
        // (400 ms) at most, where it waited behind a whole round before.
        assert!(worst[0].0 < 150.0 && worst[0].1 < 150.0, "{worst:?}");
        assert!(worst[1].0 < 520.0 && worst[1].1 < 520.0, "{worst:?}");
    }

    /// The commands that reached the simulated amplifier.
    fn log(port: &SimPort) -> Vec<String> {
        port.sim().log.iter().map(|(_, c)| c.clone()).collect()
    }

    fn fresh(rf: bool, drive_w: f32, ago: Duration) -> Option<Clear> {
        Some(Clear { at: Instant::now() - ago, rf, drive_w })
    }

    #[test]
    fn a_relay_click_straight_after_a_clear_status_goes_without_another_read() {
        let l = crate::types::AmpModel::Hr500.limits();
        let run = |clear: Option<Clear>, fwd: u32| {
            let port = SimPort::new(Duration::from_millis(5), answers_all);
            port.sim().fwd = fwd;
            let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(100));
            let r = act(&mut amp, AmpAction::SetAntenna(2), None, &l, clear).unwrap();
            (r, log(&port))
        };
        // A status answered 20 ms ago with no RF stands in for the read.
        let (r, sent) = run(fresh(false, 0.0, Duration::from_millis(20)), 0);
        assert_eq!((r, sent), (Some(AmpAction::SetAntenna(2)), vec!["HRAN2".to_string()]));
        // Older than FRESH: read again.
        let (_, sent) = run(fresh(false, 0.0, Duration::from_millis(150)), 0);
        assert_eq!(sent, ["HRST", "HRAN2"]);
        // SAFETY INTERLOCK #1: one that showed RF is never taken as clear; the
        // read shows output, and nothing is switched.
        let (r, sent) = run(fresh(true, 0.0, Duration::from_millis(20)), 100);
        assert_eq!((r, sent), (None, vec!["HRST".to_string()]));
        // No status to go by, and the amplifier shows output.
        let (r, sent) = run(None, 100);
        assert_eq!((r, sent), (None, vec!["HRST".to_string()]));
    }

    #[test]
    fn a_relay_click_with_no_answer_to_its_status_read_is_refused() {
        let l = crate::types::AmpModel::Hr500.limits();
        let port = SimPort::new(Duration::from_millis(5), |c, _| c == "HRST");
        let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(50));
        for a in [AmpAction::SetAntenna(2), AmpAction::SetAtu(true), AmpAction::BandUp] {
            assert!(
                act(&mut amp, a, Some(7), &l, fresh(false, 0.0, Duration::from_secs(1))).is_err()
            );
        }
        assert!(log(&port).iter().all(|c| c == "HRST"));
    }

    #[test]
    fn going_in_line_takes_a_fresh_status_only_when_the_drive_was_under_the_limit() {
        let l = crate::types::AmpModel::Hr500.limits();
        let run = |clear: Option<Clear>, drive: u32| {
            let port = SimPort::new(Duration::from_millis(5), answers_all);
            port.sim().drive = drive;
            let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(100));
            let r = act(&mut amp, AmpAction::SetPtt(true), None, &l, clear).unwrap();
            (r, log(&port))
        };
        let (r, sent) = run(fresh(false, 2.0, Duration::from_millis(20)), 20);
        assert_eq!((r, sent), (Some(AmpAction::SetPtt(true)), vec!["HRMD1".to_string()]));
        // SAFETY INTERLOCK #4: over the limit, read again; still over, refused.
        let (r, sent) = run(fresh(false, 20.0, Duration::from_millis(20)), 200);
        assert_eq!((r, sent), (None, vec!["HRST".to_string()]));
        let (r, sent) = run(fresh(false, 2.0, Duration::from_millis(150)), 200);
        assert_eq!((r, sent), (None, vec!["HRST".to_string()]));
        // Going to OFF needs no read.
        let port = SimPort::new(Duration::from_millis(5), answers_all);
        let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(100));
        act(&mut amp, AmpAction::SetPtt(false), None, &l, None).unwrap();
        assert_eq!(log(&port), ["HRMD0"]);
    }

    #[test]
    fn bypass_cancels_a_tune_the_tuner_reports_and_never_starts_one() {
        let l = crate::types::AmpModel::Hr500.limits();
        let run = |tuning: bool, skip: fn(&str, u32) -> bool| {
            let port = SimPort::new(Duration::from_millis(5), skip);
            port.sim().atu = true;
            port.sim().tuning = tuning;
            let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(100));
            let r = act(&mut amp, AmpAction::SetAtu(false), None, &l, None).unwrap();
            let sent = log(&port);
            let sim = port.sim();
            (r, sent, sim.atu, sim.tuning)
        };
        // Tuning (a failed tune left armed): HRTU cancels it, then HRTB0.
        let (r, sent, atu, tuning) = run(true, answers_all);
        assert_eq!(r, Some(AmpAction::SetAtu(false)));
        assert_eq!(sent, ["HRST", "HRTT", "HRTU", "HRTB0"]);
        assert!(!atu && !tuning);
        // Not tuning: HRTU would start a tune, so it isn't sent.
        let (_, sent, atu, tuning) = run(false, answers_all);
        assert_eq!(sent, ["HRST", "HRTT", "HRTB0"]);
        assert!(!atu && !tuning);
        // No answer to HRTT: not sent either.
        let (_, sent, _, tuning) = run(true, |c, _| c == "HRTT");
        assert_eq!(sent, ["HRST", "HRTT", "HRTB0"]);
        assert!(tuning);
        // ON never reads the tuning state.
        let port = SimPort::new(Duration::from_millis(5), answers_all);
        let mut amp = Amp::new(port.clone()).with_timeout(Duration::from_millis(100));
        act(&mut amp, AmpAction::SetAtu(true), None, &l, None).unwrap();
        assert_eq!(log(&port), ["HRST", "HRTB1"]);
    }

    #[test]
    fn a_refused_or_unconfirmed_tuner_click_says_why() {
        let port = SimPort::new(Duration::from_millis(10), answers_all);
        port.sim().atu = true;
        let link = SimLink::start(port.clone());
        assert!(wait_for(&link, |s| s.atu.as_ref().is_some_and(|a| a.active)));
        // SAFETY INTERLOCK #1: RF on, BYP refused; the window says why and
        // the tuner still shows in line.
        port.sim().fwd = 100;
        assert!(wait_for(&link, |s| s.readings.unwrap().fwd_w > 50.0));
        link.action(AmpAction::SetAtu(false));
        assert!(wait_for(&link, |s| s.notice.is_some()));
        let st = link.status();
        assert!(st.notice.as_deref().unwrap().starts_with("Unkey first"), "{:?}", st.notice);
        assert!(st.notice_at.is_some());
        assert!(st.atu.unwrap().active);
        assert!(!log(&port).iter().any(|c| c == "HRTB0"));
        // No RF, but the relays don't follow: read back and said.
        port.sim().fwd = 0;
        port.sim().atu_stuck = true;
        assert!(wait_for(&link, |s| s.readings.unwrap().fwd_w < 1.0));
        link.action(AmpAction::SetAtu(false));
        assert!(wait_for(&link, |s| s.notice.as_deref().is_some_and(|n| n.contains("still"))));
        assert!(log(&port).iter().any(|c| c == "HRTB0"));
        assert!(wait_for(&link, |s| s.pending.is_empty() && s.atu.as_ref().unwrap().active));
    }

    /// Wait until `f` holds of the link's status, or a few seconds.
    fn wait_for(link: &SimLink, f: impl Fn(&AmpStatus) -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(4);
        while Instant::now() < until {
            if f(&link.status()) {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_click_shows_as_asked_until_the_amplifier_reads_it_back() {
        let link = SimLink::start(SimPort::new(Duration::from_millis(10), answers_all));
        assert!(wait_for(&link, |s| s.readings.is_some()));
        link.action(AmpAction::SetAntenna(2));
        // At once, before the link has done anything.
        assert_eq!(link.status().pending, [AmpAction::SetAntenna(2)]);
        assert!(wait_for(&link, |s| s.pending.is_empty()));
        assert_eq!(link.status().readings.unwrap().antenna, 2);
        // A band step shows as the band it steps to, and two quick steps
        // step two bands.
        link.action(AmpAction::BandUp);
        link.action(AmpAction::BandUp);
        assert!(wait_for(&link, |s| s.pending.is_empty()));
        assert!(wait_for(&link, |s| s.readings.unwrap().band == Some(5)));
        // TUNE is shown only as read back.
        link.action(AmpAction::Tune);
        assert!(link.status().pending.is_empty());
    }

    #[test]
    fn a_refused_or_unread_click_goes_back_to_what_the_amplifier_reads() {
        let port = SimPort::new(Duration::from_millis(10), answers_all);
        let link = SimLink::start(port.clone());
        assert!(wait_for(&link, |s| s.readings.is_some()));
        // Output showing: the antenna is not switched (#1) and the button
        // shows the antenna it is on.
        port.sim().fwd = 100;
        assert!(wait_for(&link, |s| s.readings.unwrap().fwd_w > 50.0));
        link.action(AmpAction::SetAntenna(2));
        assert!(wait_for(&link, |s| s.pending.is_empty()));
        assert_eq!(link.status().readings.unwrap().antenna, 1);
        assert!(!log(&port).iter().any(|c| c == "HRAN2"));
        // Sent, but the relay never moves: let go after PENDING_HOLD.
        port.sim().fwd = 0;
        port.sim().stuck = true;
        assert!(wait_for(&link, |s| s.readings.unwrap().fwd_w < 1.0));
        let t0 = Instant::now();
        link.action(AmpAction::SetAntenna(2));
        assert!(wait_for(&link, |s| s.pending.is_empty()));
        assert!(t0.elapsed() >= PENDING_HOLD);
        assert!(log(&port).iter().any(|c| c == "HRAN2"));
    }

    #[test]
    fn the_status_is_never_shut_out_and_neither_are_the_other_reads() {
        let t = Instant::now();
        let later = t + Duration::from_secs(1);
        // Status due, nothing else: status.
        assert_eq!(next_read(t, None, t, [(Read::Temp, false)]), Some(Read::Status));
        // Status due and another read due: status first, then the other.
        assert_eq!(next_read(t, Some(Read::Temp), t, [(Read::Temp, true)]), Some(Read::Status));
        assert_eq!(next_read(t, Some(Read::Status), t, [(Read::Temp, true)]), Some(Read::Temp));
        // Nothing due: wait.
        assert_eq!(next_read(t, Some(Read::Status), later, [(Read::Temp, false)]), None);
    }

    #[test]
    fn transmitting_the_link_waits_five_seconds_before_it_is_lost() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        // At rest: four misses, whatever the time.
        assert!(!link_lost(3, t0, at(1200), false));
        assert!(link_lost(4, t0, at(1600), false));
        // Transmitting: four misses are not enough until 5 s have passed
        // since the last answer.
        assert!(!link_lost(4, t0, at(1600), true));
        assert!(!link_lost(12, t0, at(4900), true));
        assert!(link_lost(13, t0, at(5000), true));
        // An answer resets it: the misses count from zero again.
        assert!(!link_lost(0, t0, at(9000), true));
        // Once the transmission ends, the old rule applies at once.
        assert!(link_lost(13, t0, at(4900), false));
    }

    /// Run a link on an amplifier that stops answering after `quiet_after`,
    /// the radio keyed or not, and give how long after the silence began
    /// the link was given up, and the readings and their age just before.
    fn time_to_lost(keyed: bool) -> (Duration, Option<AmpStatus>) {
        let port = SimPort::new(Duration::from_millis(25), answers_all);
        let status = Arc::new(Mutex::new(AmpStatus::default()));
        let radio_keyed = Arc::new(AtomicBool::new(keyed));
        let (_tx, rx) = bounded::<Msg>(16);
        let (p, st, k) = (port.clone(), status.clone(), radio_keyed.clone());
        let done = Arc::new(Mutex::new(None));
        let d = done.clone();
        thread::spawn(move || {
            let amp = Amp::new(p).with_timeout(Duration::from_millis(400));
            run(amp, &rx, &st, crate::types::AmpModel::Hr500.limits(), &k);
            *d.lock().unwrap() = Some(Instant::now());
        });
        thread::sleep(Duration::from_millis(600));
        port.sim().silent = true;
        let quiet = Instant::now();
        let mut held = None;
        loop {
            if let Some(t) = *done.lock().unwrap() {
                return (t.duration_since(quiet), held);
            }
            assert!(quiet.elapsed() < Duration::from_secs(10), "never given up");
            held = Some(status.lock().unwrap().clone());
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn unanswered_reads_drop_the_link_at_rest_but_not_mid_transmission() {
        let ((rest, _), (keyed, held)) = thread::scope(|s| {
            let a = s.spawn(|| time_to_lost(false));
            let b = s.spawn(|| time_to_lost(true));
            (a.join().unwrap(), b.join().unwrap())
        });
        // Four status reads of 400 ms, with another read (each as
        // unanswered) between each: about 3.2 s, as before.
        assert!(rest < Duration::from_millis(3700), "{rest:?}");
        assert!(keyed >= TX_GRACE - Duration::from_millis(300), "{keyed:?}");
        assert!(keyed < TX_GRACE + Duration::from_millis(1500), "{keyed:?}");
        // Until then the last readings were kept, with their true age.
        let held = held.unwrap();
        assert!(held.readings.is_some());
        let age = held.readings_at.unwrap().elapsed();
        assert!(age >= TX_GRACE - Duration::from_millis(400), "{age:?}");
    }

    #[test]
    fn a_peak_past_a_trip_level_is_not_a_trip() {
        let l = crate::types::AmpModel::Hr500.limits();
        let mut w = TripWatch::default();
        // 450 W out of 8 W in, IDD spiking past 20 A: still in line.
        let busy =
            AmpReadings { drive_w: 8.0, fwd_w: 450.0, idd_a: 21.0, ..AmpReadings::default() };
        assert!((0..10).all(|_| !w.keyed(&l, &busy)));
        // The first polls of a key-up see no output yet, and are let pass.
        let mut w = TripWatch::default();
        let starting = AmpReadings { drive_w: 5.0, fwd_w: 4.0, ..AmpReadings::default() };
        assert!(!w.keyed(&l, &starting) && !w.keyed(&l, &starting));
        assert!(!w.keyed(&l, &busy));
        // Off line mid-over: the drive comes straight out.
        assert!(!w.keyed(&l, &starting));
        assert!(w.keyed(&l, &starting));
    }

    #[test]
    fn a_real_overdrive_trip_is_seen_and_blamed_on_the_drive() {
        // AJ3G's HR500 on 160 m, from hrctl's log: in line at 2.4 W in, then
        // tripped with 5.6 W in, nothing out and the drain supply cut.
        let l = crate::types::AmpModel::Hr500.limits();
        let good = AmpReadings {
            fwd_w: 305.0,
            rfl_w: 1.0,
            drive_w: 2.4,
            vdd_v: 57.0,
            idd_a: 8.0,
            ptt: true,
            ..AmpReadings::default()
        };
        let tripped = AmpReadings {
            fwd_w: 0.0,
            rfl_w: 0.0,
            drive_w: 5.6,
            vdd_v: 42.0,
            idd_a: 0.0,
            ptt: true,
            ..AmpReadings::default()
        };
        let mut w = TripWatch::default();
        assert!((0..4).all(|_| !w.keyed(&l, &good)));
        // The supply sagging makes it a trip at the first such reading.
        assert!(w.keyed(&l, &tripped));
        assert_eq!(w.cause(&l, &tripped), Some(AmpGauge::Drive));

        let mut a = AmpAlerts::default();
        a.hold(&l, &good);
        a.hold(&l, &tripped);
        a.tripped = true;
        assert!(a.trips().is_empty());
        a.blame(AmpGauge::Drive);
        assert_eq!(a.trips(), vec!["DRV"]);
        assert_eq!(a.get(AmpGauge::Drive), crate::types::AmpLevel::Red);
    }

    #[test]
    fn a_yellow_held_from_before_the_link_opened_shows_green() {
        // Idle, DRV yellow and FWD red still latched from an earlier over.
        let l = Lights { tx: false, fwd: 3, rfl: 1, drv: 2, vdd: 1, idd: 1 };
        let a = lights_seen(l, false);
        assert_eq!(a.drive, AmpLevel::Green);
        // A trip is still shown.
        assert_eq!(a.fwd, AmpLevel::Red);
        assert!(a.tripped);
        // Once a transmission has been seen, the lights are shown as sent.
        assert_eq!(lights_seen(l, true).drive, AmpLevel::Yellow);
    }

    #[test]
    fn tuner_results_read_as_the_panel_words_them() {
        assert_eq!(result_text(TuneResult::PowerTooLow), "Lo power");
        assert_eq!(result_text(TuneResult::Tuned), "Tuned");
    }

    #[test]
    fn network_carries_the_swr() {
        let p = AtuParams::parse("HRTM-L012-C034-H1-B5-S000-F145-").unwrap();
        let n = network_of(p);
        assert_eq!((n.l_step, n.c_step, n.cap_output), (12, 34, true));
        assert!(n.swr.is_some());
    }

    #[test]
    fn a_temperature_that_stops_coming_gives_way() {
        let t0 = Instant::now();
        assert_eq!(fresh_temp(Some((122.0, t0)), t0 + Duration::from_secs(10)), Some(122.0));
        assert_eq!(fresh_temp(Some((122.0, t0)), t0 + Duration::from_secs(16)), None);
        assert_eq!(fresh_temp(None, t0), None);
    }

    #[test]
    fn keyed_the_live_frame_keeps_the_temperature_current() {
        // At rest the amplifier's screen reading is shown.
        assert_eq!(shown_temp(Some(75.0), 73.0, false), 75.0);
        // Keyed, the frame's reading is shown once it has climbed past the
        // last screen reading.
        assert_eq!(shown_temp(Some(122.0), 133.0, true), 133.0);
        assert_eq!(shown_temp(Some(122.0), 120.0, true), 122.0);
        // No screen reading: the frame's.
        assert_eq!(shown_temp(None, 90.0, false), 90.0);
    }
}
