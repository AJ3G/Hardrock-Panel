//! Outside programs (decision 25): the TCI server (port 50001), the rigctld
//! server (port 4532) and the WSJT-X UDP messages, all off until turned on
//! under SETTINGS, and the panel's side of the links' `Rig` trait.
//!
//! The servers run on their own threads and ask [`PanelRig`] what the radio
//! is doing and to change it. It answers from what the panel's loop last
//! published, and hands changes back to the loop as [`Req`]s, so the panel
//! makes them as it makes its own (the receiver, VFO A, the mode), with every
//! check its own controls have. A request to transmit is the one thing it
//! acts on at once, and only when the panel's own checks, worked out each
//! frame by `tune::outside_ptt_blocked` (interlocks #6 and #8), say the radio
//! may be keyed now.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, RichText};
use hardrock::AmpStatus;
use links::{
    PttKind, PttRequest, PttSource, Refused, RigState, RigctldConfig, RigctldServer, TciConfig,
    TciServer, Vfo, WsjtxUdp,
};
use rigcat::{Keying, RigLink, RigStatus};
use serde::{Deserialize, Serialize};

use crate::rx::{self, Rx};
use crate::{chrome, theme, tune};

/// What is kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct OutsideSettings {
    pub tci: bool,
    pub tci_port: u16,
    pub rigctld: bool,
    pub rigctld_port: u16,
    // SAFETY INTERLOCK (docs/safety-interlocks.md, #8): off unless the
    // operator turns it on; with it off neither server asks the panel to key.
    /// Outside programs may ask to transmit.
    pub allow_tx: bool,
    /// Send the panel's WSJT decodes as WSJT-X UDP messages.
    pub wsjtx: bool,
    pub wsjtx_host: String,
    pub wsjtx_port: u16,
}

impl Default for OutsideSettings {
    fn default() -> Self {
        OutsideSettings {
            tci: false,
            tci_port: links::tci::DEFAULT_PORT,
            rigctld: false,
            rigctld_port: links::rigctld::DEFAULT_PORT,
            allow_tx: false,
            wsjtx: false,
            wsjtx_host: "127.0.0.1".into(),
            wsjtx_port: links::wsjtx::DEFAULT_PORT,
        }
    }
}

/// A change an outside program asked for, for the panel's loop to make.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Req {
    /// Tune VFO A (with the panel's receiver: the receiver to it).
    FreqA(f64),
    /// Tune VFO B (only with no receiver: the panel sets B itself in split).
    FreqB(f64),
    Mode(sdr::Mode, Option<u32>),
    Split(bool),
    /// Move the I/Q centre (TCI's `dds`).
    Centre(f64),
}

/// How long the panel's checks stay good for a request to transmit. The
/// loop publishes them ten times a second or more; older means it isn't
/// running (a minimised window, say), and nothing is keyed.
const CHECKS_FRESH: Duration = Duration::from_millis(500);
/// How long a granted key may take to show in the radio's status before the
/// grant is dropped.
const KEY_SETTLE: Duration = Duration::from_millis(1500);

/// What the loop publishes for the links' threads.
struct Inner {
    state: RigState,
    /// Why an outside program may not key now; `None` when it may.
    blocked: Option<String>,
    transmitting: bool,
    /// The panel runs its own receiver: VFO A is the receiver, and VFO B and
    /// split are the panel's (decision 22).
    has_rx: bool,
    allow_tx: bool,
    updated: Instant,
    /// The program that holds the key, and when it was granted.
    holder: Option<(PttSource, Instant)>,
    /// When the panel last started a TUNE of its own.
    panel_tune: Option<Instant>,
    link: Weak<RigLink>,
    ctx: Option<egui::Context>,
}

struct Shared {
    inner: Mutex<Inner>,
    reqs: Sender<Req>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ask(&self, r: Req) -> Result<(), Refused> {
        let g = self.lock();
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #9): as the scope's
        // own controls, nothing an outside program asks moves the radio or
        // the receiver while it transmits.
        if g.transmitting {
            return Err(Refused::new("the radio is transmitting"));
        }
        if let Some(c) = &g.ctx {
            c.request_repaint();
        }
        drop(g);
        self.reqs.send(r).map_err(|_| Refused::new("the panel has closed"))
    }
}

/// The panel as the links see it.
pub(crate) struct PanelRig(Arc<Shared>);

impl links::Rig for PanelRig {
    fn state(&self) -> RigState {
        self.0.lock().state.clone()
    }

    fn set_freq(&self, vfo: Vfo, hz: f64) -> Result<(), Refused> {
        if !(9_000.0..=54_000_000.0).contains(&hz) {
            return Err(Refused::new("outside the radio's 9 kHz to 54 MHz"));
        }
        match vfo {
            Vfo::A => self.0.ask(Req::FreqA(hz)),
            Vfo::B if self.0.lock().has_rx => {
                Err(Refused::new("the panel sets VFO B itself, to transmit where its receiver is"))
            }
            Vfo::B => self.0.ask(Req::FreqB(hz)),
        }
    }

    fn set_mode(&self, mode: links::Mode, width_hz: Option<u32>) -> Result<(), Refused> {
        let m = match mode {
            links::Mode::Lsb | links::Mode::DataL | links::Mode::RttyR => sdr::Mode::Lsb,
            links::Mode::Usb | links::Mode::DataU | links::Mode::Rtty => sdr::Mode::Usb,
            links::Mode::Cw => sdr::Mode::Cw,
            links::Mode::CwR => sdr::Mode::Cwr,
            links::Mode::Am => sdr::Mode::Am,
            links::Mode::Fm => sdr::Mode::Fm,
            links::Mode::WideFm => return Err(Refused::new("the radio has no wide FM")),
        };
        self.0.ask(Req::Mode(m, width_hz))
    }

    fn set_split(&self, on: bool) -> Result<(), Refused> {
        if self.0.lock().has_rx {
            // The panel transmits where it listens: no split to outsiders.
            return if on {
                Err(Refused::new("the panel transmits where its receiver is; no split"))
            } else {
                Ok(())
            };
        }
        self.0.ask(Req::Split(on))
    }

    fn set_center(&self, hz: f64) -> Result<(), Refused> {
        if !self.0.lock().has_rx {
            return Err(Refused::new("the panel's receiver is off"));
        }
        self.0.ask(Req::Centre(hz))
    }

    // SAFETY INTERLOCK (docs/safety-interlocks.md, #8): granted only when the
    // panel's own checks for keying the radio pass right now: transmit from
    // outside allowed, the amplifier ready and on the radio's band, the
    // power setting under its drive limit, nothing else keyed (#6), and for
    // an ordinary transmission the radio's input on USB audio (#7, which the
    // link reads again before it keys).
    fn request_ptt(&self, req: PttRequest) -> Result<(), Refused> {
        let mut g = self.0.lock();
        if !g.allow_tx {
            return Err(Refused::new("transmitting from outside programs is off in SETTINGS"));
        }
        if req.kind == PttKind::TciAudio {
            return Err(Refused::new(
                "the panel doesn't take transmit audio over TCI: send it to the radio's USB \
                 audio and key with trx:0,true;",
            ));
        }
        if g.updated.elapsed() > CHECKS_FRESH {
            return Err(Refused::new("the panel isn't running its checks (is it minimised?)"));
        }
        if let Some(why) = &g.blocked {
            return Err(Refused::new(why.clone()));
        }
        if g.panel_tune.is_some_and(|t| t.elapsed() < KEY_SETTLE) {
            return Err(Refused::new("the panel is starting TUNE"));
        }
        if let Some((h, _)) = g.holder
            && h != req.source
        {
            return Err(Refused::new("another program holds the key"));
        }
        let Some(link) = g.link.upgrade() else {
            return Err(Refused::new("no radio is connected"));
        };
        let keying = if req.kind == PttKind::Tune { Keying::Carrier } else { Keying::Audio };
        g.holder = Some((req.source, Instant::now()));
        link.tune_start(rigcat_tune_max(), keying);
        if let Some(c) = &g.ctx {
            c.request_repaint();
        }
        tracing::info!("outside program keyed the radio: {:?} {:?}", req.source, req.kind);
        Ok(())
    }

    fn release_ptt(&self, source: PttSource) {
        let mut g = self.0.lock();
        if g.holder.is_some_and(|(h, _)| h == source) {
            g.holder = None;
            if let Some(link) = g.link.upgrade() {
                link.tune_stop(None);
            }
        }
    }
}

/// The longest an outside program's transmission runs: the link's own
/// limit for TUNE, a minute, whatever it is asked.
fn rigcat_tune_max() -> Duration {
    Duration::from_secs(60)
}

/// The servers and the UDP sender as they run.
pub(crate) struct Outside {
    shared: Arc<Shared>,
    reqs: Receiver<Req>,
    /// Behind a mutex: the server's transmit-audio ring isn't `Sync`, and the
    /// pipeline's thread pushes the streams into it.
    tci: Option<Arc<Mutex<TciServer>>>,
    rigctld: Option<RigctldServer>,
    pub wsjtx: Option<WsjtxUdp>,
    /// Why a server didn't start (a port another program holds).
    pub errors: Vec<String>,
    heartbeat: Option<Instant>,
    running: Option<OutsideSettings>,
}

impl Default for Outside {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Outside {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    state: RigState::default(),
                    blocked: Some("the panel hasn't checked yet".into()),
                    transmitting: false,
                    has_rx: false,
                    allow_tx: false,
                    updated: Instant::now(),
                    holder: None,
                    panel_tune: None,
                    link: Weak::new(),
                    ctx: None,
                }),
                reqs: tx,
            }),
            reqs: rx,
            tci: None,
            rigctld: None,
            wsjtx: None,
            errors: Vec::new(),
            heartbeat: None,
            running: None,
        }
    }
}

impl Outside {
    /// Start, restart or stop the servers to match `s`.
    pub(crate) fn apply(&mut self, s: &OutsideSettings, taps: &rx::Taps) {
        if self.running.as_ref() == Some(s) {
            return;
        }
        self.stop(taps);
        self.errors.clear();
        self.shared.lock().allow_tx = s.allow_tx;
        let rig = || -> Arc<dyn links::Rig> { Arc::new(PanelRig(self.shared.clone())) };
        if s.tci {
            let cfg = TciConfig { port: s.tci_port, allow_tx: s.allow_tx, ..TciConfig::default() };
            match TciServer::start(&cfg, rig()) {
                Ok(t) => {
                    let t = Arc::new(Mutex::new(t));
                    let a = t.clone();
                    taps.set_audio("tci", Some(Box::new(move |s, _| lock(&a).push_audio(s))));
                    let q = t.clone();
                    taps.set_iq(
                        "tci",
                        Some(Box::new(move |b| {
                            if b.rate <= TCI_IQ_MAX {
                                lock(&q).push_iq(b.iq, b.rate as u32);
                            }
                        })),
                    );
                    self.tci = Some(t);
                }
                Err(e) => self.errors.push(format!("TCI: {e}")),
            }
        }
        if s.rigctld {
            let cfg = RigctldConfig {
                port: s.rigctld_port,
                allow_tx: s.allow_tx,
                ..RigctldConfig::default()
            };
            match RigctldServer::start(&cfg, rig()) {
                Ok(r) => self.rigctld = Some(r),
                Err(e) => self.errors.push(format!("rigctld: {e}")),
            }
        }
        if s.wsjtx {
            match WsjtxUdp::start(&s.wsjtx_host, s.wsjtx_port, "hardrock-panel") {
                Ok(w) => self.wsjtx = Some(w),
                Err(e) => self.errors.push(format!("WSJT-X UDP: {e}")),
            }
        }
        self.running = Some(s.clone());
    }

    fn stop(&mut self, taps: &rx::Taps) {
        taps.set_audio("tci", None);
        taps.set_iq("tci", None);
        // Dropping a server unkeys any client that held the key.
        self.tci = None;
        self.rigctld = None;
        if let Some(w) = self.wsjtx.take() {
            w.close();
        }
        self.running = None;
    }

    /// Whether an outside program holds the radio's key now.
    pub(crate) fn holder(&self) -> Option<PttSource> {
        self.shared.lock().holder.map(|(h, _)| h)
    }

    /// The panel is starting a TUNE of its own: refuse outside programs the
    /// key until it shows keyed (and the checks refuse it from then on).
    pub(crate) fn panel_tune_starting(&self) {
        self.shared.lock().panel_tune = Some(Instant::now());
    }

    pub(crate) fn allow_tx(&self) -> bool {
        self.shared.lock().allow_tx
    }

    /// Once a frame: publish the radio's state and whether an outside program
    /// may key it, and take what the programs asked for.
    pub(crate) fn frame(&mut self, f: Frame<'_>) -> Vec<Req> {
        let now = Instant::now();
        let r = f.rig.filter(|r| r.connected);
        let state = rig_state(r, f.rx);
        let blocked = match (f.link, r) {
            (Some(_), Some(r)) => tune::outside_ptt_blocked(f.amp, r),
            _ => Some("no radio is connected".to_string()),
        };
        {
            let mut g = self.shared.lock();
            g.state = state;
            g.blocked = blocked;
            g.transmitting = f.transmitting;
            g.has_rx = f.rx.active();
            g.updated = now;
            g.link = f.link.map_or_else(Weak::new, Arc::downgrade);
            g.ctx = Some(f.ctx.clone());
            // A grant the radio never showed, or a transmission that ended
            // (an interlock, the time limit, STOP): the key is free again.
            if let Some((_, at)) = g.holder
                && r.is_none_or(|r| r.tuning.is_none())
                && at.elapsed() >= KEY_SETTLE
            {
                g.holder = None;
                if let Some(t) = &self.tci {
                    lock(t).revoke_tx();
                }
            }
        }
        if let (Some(t), Some(r)) = (&self.tci, r)
            && r.transmitting()
        {
            lock(t).push_tx_sensors(links::tci::TxSensors {
                mic_db: 0.0,
                fwd_w: r.fwd_w().unwrap_or(0.0),
                rev_w: r.rfl_w().unwrap_or(0.0),
                swr: r.swr_ratio().unwrap_or(1.0),
            });
        }
        if let Some(w) = &self.wsjtx
            && self.heartbeat.is_none_or(|t| t.elapsed() >= Duration::from_secs(15))
        {
            w.heartbeat(env!("CARGO_PKG_VERSION"));
            self.heartbeat = Some(now);
        }
        self.reqs.try_iter().collect()
    }

    /// One line on each server, for SETTINGS and the scope.
    pub(crate) fn summary(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(t) = &self.tci {
            let t = lock(t);
            v.push(format!("TCI on {} · {} client(s)", t.addr(), t.clients()));
        }
        if let Some(r) = &self.rigctld {
            v.push(format!("rigctld on {} · {} client(s)", r.addr(), r.clients()));
        }
        if let Some(w) = &self.wsjtx {
            v.push(format!("WSJT-X UDP to {}", w.dest()));
        }
        v
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// TCI's I/Q streams run at 48 to 384 kHz; a wider span is not offered.
const TCI_IQ_MAX: f64 = 384_000.0;

/// What [`Outside::frame`] looks at.
pub(crate) struct Frame<'a> {
    pub ctx: &'a egui::Context,
    pub amp: &'a AmpStatus,
    pub rig: Option<&'a RigStatus>,
    pub link: Option<&'a Arc<RigLink>>,
    pub rx: &'a Rx,
    pub transmitting: bool,
}

/// The radio as outside programs see it. With the panel's receiver running,
/// VFO A is the receiver (TCI's `dds` plus `if`), and there is no split:
/// the panel transmits where it listens.
fn rig_state(r: Option<&RigStatus>, rx: &Rx) -> RigState {
    let mut s = RigState { rx_ranges: vec![(9_000.0, 54_000_000.0)], ..RigState::default() };
    let dial = r.and_then(|r| r.freq_hz).map(|hz| hz as f64);
    if rx.active() {
        s.vfo_a_hz = rx.rx_hz;
        s.vfo_b_hz = rx.rx_hz;
        s.split = false;
        s.mode = links_mode(rx.mode);
        s.passband = rx.passband;
        s.center_hz = rx.centre();
        let rate = rx.rate();
        s.iq_rate = if rate > 0.0 && rate <= TCI_IQ_MAX { rate as u32 } else { 0 };
        s.strength_dbm = rx.snapshot().level_dbm.round() as i32;
    } else if let Some(r) = r {
        s.vfo_a_hz = dial.unwrap_or(s.vfo_a_hz);
        s.vfo_b_hz = r.freq_b_hz.map_or(s.vfo_a_hz, |hz| hz as f64);
        s.split = r.split.unwrap_or(false);
        if let Some(m) = r.mode.and_then(rx::sdr_mode) {
            s.mode = links_mode(m);
            s.passband = m.default_passband();
        }
    }
    if let Some(r) = r {
        s.ptt = r.transmitting();
        s.tune = r.tuning.is_some_and(|t| t.keying == Keying::Carrier);
        s.can_tx = true;
        let max = r.power_steps.iter().map(|p| p.watts).fold(0.0f32, f32::max);
        if let (Some(p), true) = (r.power(), max > 0.0) {
            s.drive_pct = (p.watts / max * 100.0).round() as u32;
        }
    }
    s
}

fn links_mode(m: sdr::Mode) -> links::Mode {
    match m {
        sdr::Mode::Lsb => links::Mode::Lsb,
        sdr::Mode::Usb => links::Mode::Usb,
        sdr::Mode::Cw => links::Mode::Cw,
        sdr::Mode::Cwr => links::Mode::CwR,
        sdr::Mode::Am => links::Mode::Am,
        sdr::Mode::Fm => links::Mode::Fm,
    }
}

/// The SETTINGS section.
pub(crate) fn settings_section(ui: &mut egui::Ui, s: &mut OutsideSettings, o: &Outside) {
    ui.label(RichText::new("Outside programs").size(14.0).strong().color(theme::CYAN()));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "For WSJT-X, JTDX, fldigi, loggers and skimmers that would rather drive the radio \
             through the panel than have its CAT port: TCI (the radio, its I/Q and the \
             receiver's audio) and Hamlib's rigctld (\"NET rigctl\", rig 2). Both listen on this \
             computer only. Taken with APPLY.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    egui::Grid::new("outside-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        chrome::checkbox(ui, &mut s.tci, "TCI server");
        ui.add(egui::DragValue::new(&mut s.tci_port).range(1024..=65535).prefix("port "));
        ui.end_row();
        chrome::checkbox(ui, &mut s.rigctld, "rigctld server");
        ui.add(egui::DragValue::new(&mut s.rigctld_port).range(1024..=65535).prefix("port "));
        ui.end_row();
        chrome::checkbox(ui, &mut s.wsjtx, "WSJT-X UDP decodes");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut s.wsjtx_host).desired_width(110.0));
            ui.add(egui::DragValue::new(&mut s.wsjtx_port).range(1024..=65535).prefix("port "));
        })
        .response
        .on_hover_text(
            "The panel's own FT8, FT4 and WSPR decodes, as WSJT-X sends them, for GridTracker, \
             JTAlert and loggers",
        );
        ui.end_row();
    });
    ui.add_space(4.0);
    chrome::checkbox(ui, &mut s.allow_tx, "Allow outside programs to transmit").on_hover_text(
        "Off: programs see a receive-only radio. On: a program may ask to transmit, and the \
         panel keys the radio only when its own TUNE checks pass: the amplifier connected, \
         ready and on the radio's band, the radio's power setting under the amplifier's drive \
         limit, and for audio the radio's TX IN on USB audio. It unkeys on any of TUNE's stop \
         conditions, after a minute, or when the program lets go or goes away.",
    );
    if s.allow_tx {
        ui.label(
            RichText::new(
                "Outside programs can key the radio, and so the amplifier. Each transmission is \
                 checked as TUNE is, and stops after a minute at most.",
            )
            .size(11.5)
            .color(theme::YELLOW()),
        );
    }
    for l in o.summary() {
        ui.label(RichText::new(l).size(11.5).color(theme::GREEN()));
    }
    for e in &o.errors {
        ui.label(RichText::new(e).size(11.5).color(theme::ALERT()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use links::Rig;

    fn rig(o: &Outside) -> PanelRig {
        PanelRig(o.shared.clone())
    }

    fn req() -> PttRequest {
        PttRequest { source: PttSource::Rigctld { client: 1 }, kind: PttKind::RigAudio }
    }

    #[test]
    fn transmit_is_refused_until_allowed_and_the_checks_pass() {
        let o = Outside::default();
        let r = rig(&o);
        // Off by default.
        let e = r.request_ptt(req()).unwrap_err();
        assert!(e.0.contains("off in SETTINGS"), "{e}");
        o.shared.lock().allow_tx = true;
        // The panel's checks say no.
        o.shared.lock().blocked = Some("The amplifier has tripped.".into());
        assert_eq!(r.request_ptt(req()).unwrap_err().0, "The amplifier has tripped.");
        // Checks that pass but have gone stale.
        {
            let mut g = o.shared.lock();
            g.blocked = None;
            g.updated = Instant::now() - Duration::from_secs(2);
        }
        assert!(r.request_ptt(req()).unwrap_err().0.contains("isn't running its checks"));
        // Fresh and passing, but no radio link to key.
        o.shared.lock().updated = Instant::now();
        assert!(r.request_ptt(req()).unwrap_err().0.contains("no radio"));
        // TCI's own audio is never taken.
        let tci = PttRequest { source: PttSource::Tci { client: 2 }, kind: PttKind::TciAudio };
        assert!(r.request_ptt(tci).unwrap_err().0.contains("USB audio"));
        // Letting go is never refused, and harmless when not keyed.
        r.release_ptt(req().source);
    }

    #[test]
    fn another_program_cannot_take_the_key() {
        let o = Outside::default();
        {
            let mut g = o.shared.lock();
            g.allow_tx = true;
            g.blocked = None;
            g.holder = Some((PttSource::Tci { client: 9 }, Instant::now()));
        }
        assert!(rig(&o).request_ptt(req()).unwrap_err().0.contains("another program"));
    }

    #[test]
    fn a_program_cannot_key_while_the_panel_starts_its_own_tune() {
        let o = Outside::default();
        {
            let mut g = o.shared.lock();
            g.allow_tx = true;
            g.blocked = None;
        }
        o.panel_tune_starting();
        assert!(rig(&o).request_ptt(req()).unwrap_err().0.contains("starting TUNE"));
    }

    #[test]
    fn nothing_moves_while_transmitting_and_vfo_b_is_the_panels() {
        let o = Outside::default();
        let r = rig(&o);
        o.shared.lock().transmitting = true;
        assert!(r.set_freq(Vfo::A, 14_074_000.0).is_err());
        o.shared.lock().transmitting = false;
        r.set_freq(Vfo::A, 14_074_000.0).unwrap();
        assert_eq!(o.reqs.try_recv(), Ok(Req::FreqA(14_074_000.0)));
        o.shared.lock().has_rx = true;
        assert!(r.set_freq(Vfo::B, 14_076_000.0).is_err());
        assert!(r.set_split(true).is_err());
        assert!(r.set_split(false).is_ok());
        assert!(r.set_freq(Vfo::A, 60_000_000.0).is_err());
        r.set_mode(links::Mode::DataU, Some(3000)).unwrap();
        assert_eq!(o.reqs.try_recv(), Ok(Req::Mode(sdr::Mode::Usb, Some(3000))));
    }
}
