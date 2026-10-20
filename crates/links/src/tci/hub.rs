// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-tci/src/server/hub.rs, GPL-3.0-or-later. Requests go to the
// `Rig` trait here instead of SDRoxide's engine, the hub polls the state
// itself, and it paces and watches the transmit audio by itself.

//! The hub thread: owns the listener and the client registry, fans realtime
//! frames out to subscribers, and makes every protocol decision
//! (subscriptions, the transmit key, status broadcast). Client threads are
//! pure transport, so all the rules are here.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded, unbounded};
use tracing::{debug, info, warn};
use tungstenite::Bytes;

use super::client::{self, Out, Up};
use super::protocol as p;
use super::text::{self, ClientReq, RX};
use super::{AUDIO_RATE_HZ, AudioBlock, Ctl, IqBlock, Shared, Subs, TX_RING, TciConfig, TxSensors};
use crate::{PttKind, PttRequest, PttSource, Rig, RigState};

/// How often the hub wakes to accept, fan out and pace transmit audio.
const POLL: Duration = Duration::from_millis(5);
/// How often the rig's state is read and changes sent.
const STATE_EVERY: Duration = Duration::from_millis(100);
/// Realtime frames a client may have queued before it counts as stalled.
const DATA_LANE: usize = 64;
/// How long a stalled client keeps its slot.
const STALL_LIMIT: Duration = Duration::from_secs(5);
/// How far ahead of real time the keying client is asked for audio.
const TX_LEAD_FRAMES: usize = (AUDIO_RATE_HZ / 5) as usize;
/// Frames asked for in one chrono at most.
const CHRONO_MAX: u32 = 2048;
/// How long a client keyed for its own audio may send none before the radio
/// is unkeyed.
pub(crate) const TX_AUDIO_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a granted key may go without the radio reporting it transmits
/// before the grant is taken as dropped.
const KEY_CONFIRM: Duration = Duration::from_secs(3);

pub(crate) struct HubParams {
    pub listener: TcpListener,
    pub cfg: TciConfig,
    pub rig: Arc<dyn Rig>,
    pub ctl_rx: Receiver<Ctl>,
    pub iq_rx: Receiver<IqBlock>,
    pub aud_rx: Receiver<AudioBlock>,
    pub tel_rx: Receiver<TxSensors>,
    pub tx_prod: rtrb::Producer<f32>,
    pub shared: Arc<Shared>,
}

struct Slot {
    text_tx: Sender<String>,
    data_tx: Sender<Bytes>,
    subs: Arc<Subs>,
    iq_rate: u32,
    handle: Option<JoinHandle<()>>,
    stalled_since: Option<Instant>,
}

/// The client that holds the transmit key.
struct Key {
    id: u64,
    kind: PttKind,
    since: Instant,
    /// When its last transmit audio arrived.
    last_audio: Option<Instant>,
    /// Frames asked for since keying, for the chrono pacing.
    asked: u64,
    /// Whether the radio has said it transmits since the grant.
    seen_on_air: bool,
}

pub(crate) fn spawn(params: HubParams) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("tci-hub".into())
        .spawn(move || Hub::new(params).run())
        .expect("spawn TCI server hub")
}

struct Hub {
    listener: TcpListener,
    cfg: TciConfig,
    rig: Arc<dyn Rig>,
    ctl_rx: Receiver<Ctl>,
    iq_rx: Receiver<IqBlock>,
    aud_rx: Receiver<AudioBlock>,
    tel_rx: Receiver<TxSensors>,
    tx_prod: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    /// What was last broadcast, for the change diff.
    sent: RigState,
    last_state: Instant,
    up_tx: Sender<Up>,
    up_rx: Receiver<Up>,
    clients: HashMap<u64, Slot>,
    next_id: u64,
    key: Option<Key>,
}

impl Hub {
    fn new(pr: HubParams) -> Hub {
        let (up_tx, up_rx) = unbounded();
        let sent = pr.rig.state();
        Hub {
            listener: pr.listener,
            cfg: pr.cfg,
            rig: pr.rig,
            ctl_rx: pr.ctl_rx,
            iq_rx: pr.iq_rx,
            aud_rx: pr.aud_rx,
            tel_rx: pr.tel_rx,
            tx_prod: pr.tx_prod,
            shared: pr.shared,
            sent,
            last_state: Instant::now(),
            up_tx,
            up_rx,
            clients: HashMap::new(),
            next_id: 1,
            key: None,
        }
    }

    fn run(mut self) {
        info!(addr = ?self.listener.local_addr().ok(), "TCI server listening");
        loop {
            self.accept_new();
            if self.drain_ctl() {
                break;
            }
            self.drain_streams();
            self.drain_up();
            if self.last_state.elapsed() >= STATE_EVERY {
                self.last_state = Instant::now();
                self.poll_state();
            }
            self.pace_tx();
            self.reap_stalled();
            std::thread::sleep(POLL);
        }
        // SAFETY INTERLOCK: a server that stops while a client holds the key
        // unkeys the radio (safety interlock #8).
        self.release_key("the TCI server stopped");
        for (_, slot) in self.clients.drain() {
            drop(slot.text_tx);
            drop(slot.data_tx);
            if let Some(h) = slot.handle {
                let _ = h.join();
            }
        }
        info!("TCI server stopped");
    }

    fn accept_new(&mut self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((s, _peer)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    warn!("TCI server accept: {e}");
                    return;
                }
            };
            if self.clients.len() >= self.cfg.max_clients.max(1) {
                warn!(max = self.cfg.max_clients, "TCI server at its client limit; refusing");
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let (text_tx, text_rx) = unbounded::<String>();
            let (data_tx, data_rx) = bounded::<Bytes>(DATA_LANE);
            let st = self.rig.state();
            for line in text::connect_burst(&self.cfg.device_name, &st, self.cfg.allow_tx) {
                let _ = text_tx.send(line);
            }
            let handle = client::spawn(client::ClientParams {
                id,
                stream,
                out: Out { text_rx, data_rx },
                up_tx: self.up_tx.clone(),
            });
            self.clients.insert(
                id,
                Slot {
                    text_tx,
                    data_tx,
                    subs: Arc::new(Subs::default()),
                    iq_rate: 0,
                    handle: Some(handle),
                    stalled_since: None,
                },
            );
            self.shared.clients.store(self.clients.len(), Ordering::Relaxed);
            info!(id, clients = self.clients.len(), "TCI client connected");
        }
    }

    /// True when the server should stop.
    fn drain_ctl(&mut self) -> bool {
        loop {
            match self.ctl_rx.try_recv() {
                Ok(Ctl::Stop) | Err(TryRecvError::Disconnected) => return true,
                Ok(Ctl::Config(cfg)) => self.cfg = cfg,
                Ok(Ctl::Revoke) => self.revoke_key(),
                Err(TryRecvError::Empty) => return false,
            }
        }
    }

    fn poll_state(&mut self) {
        let st = self.rig.state();
        let lines = text::diff_lines(Some(&self.sent), &st);
        for line in lines {
            self.broadcast(&line);
        }
        let meter = text::rx_smeter(&st);
        for slot in self.clients.values() {
            if slot.subs.rx_sensors.load(Ordering::Relaxed) {
                let _ = slot.text_tx.send(meter.clone());
            }
        }
        // The panel decides when transmitting stops. Once the radio has been
        // seen on the air for a client and is off it again, the client no
        // longer holds the key; nor does one whose grant never reached the air.
        let on_air = st.ptt || st.tune;
        let mut dropped = false;
        if let Some(k) = &mut self.key {
            if on_air {
                k.seen_on_air = true;
            } else if k.seen_on_air || k.since.elapsed() > KEY_CONFIRM {
                dropped = true;
            }
        }
        if dropped {
            // Releasing again on the panel's side is harmless and makes sure.
            self.release_key("the radio stopped transmitting");
        }
        self.sent = st;
    }

    fn drain_streams(&mut self) {
        while let Ok(block) = self.iq_rx.try_recv() {
            if block.rate == 0 {
                continue;
            }
            let pkt = Bytes::from(p::build_iq(block.rate, RX, &block.data));
            self.fan_out(pkt, |s| s.iq_on.load(Ordering::Relaxed));
        }
        while let Ok(AudioBlock(mono)) = self.aud_rx.try_recv() {
            let pkt = Bytes::from(p::build_rx_audio(AUDIO_RATE_HZ, RX, &mono));
            self.fan_out(pkt, |s| s.audio_on.load(Ordering::Relaxed));
        }
        while let Ok(t) = self.tel_rx.try_recv() {
            let line = p::tx_sensors(RX, t.mic_db, t.fwd_w, t.rev_w, t.swr);
            for slot in self.clients.values() {
                if slot.subs.tx_sensors.load(Ordering::Relaxed) {
                    let _ = slot.text_tx.send(line.clone());
                }
            }
        }
    }

    /// Push one built packet to every subscriber, once encoded.
    fn fan_out(&mut self, pkt: Bytes, wants: impl Fn(&Subs) -> bool) {
        let now = Instant::now();
        for slot in self.clients.values_mut() {
            if !wants(&slot.subs) {
                continue;
            }
            match slot.data_tx.try_send(pkt.clone()) {
                Ok(()) => slot.stalled_since = None,
                Err(TrySendError::Full(_)) => {
                    let n = slot.subs.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_power_of_two() {
                        debug!(dropped = n, "TCI client behind; dropping stream frames");
                    }
                    slot.stalled_since.get_or_insert(now);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }

    fn reap_stalled(&mut self) {
        let dead: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, s)| s.stalled_since.is_some_and(|t| t.elapsed() > STALL_LIMIT))
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            warn!(id, "TCI client not reading for {STALL_LIMIT:?}; disconnecting");
            self.remove(id);
        }
    }

    /// Ask the keying client for audio to stay [`TX_LEAD_FRAMES`] ahead of
    /// real time, and unkey it when its audio stops.
    fn pace_tx(&mut self) {
        let Some(k) = &mut self.key else { return };
        if k.kind != PttKind::TciAudio {
            return;
        }
        let quiet_since = k.last_audio.unwrap_or(k.since);
        if quiet_since.elapsed() > TX_AUDIO_TIMEOUT {
            // SAFETY INTERLOCK: a client keyed for its own audio that stops
            // sending it is unkeyed, so a hung program cannot leave a carrier
            // or silence on the air (safety interlock #8).
            warn!(id = k.id, "TCI: no transmit audio for {TX_AUDIO_TIMEOUT:?}; unkeying");
            self.release_key("no transmit audio");
            return;
        }
        let due =
            (k.since.elapsed().as_secs_f64() * AUDIO_RATE_HZ as f64) as u64 + TX_LEAD_FRAMES as u64;
        let queued = TX_RING - self.tx_prod.slots();
        if k.asked >= due || queued >= TX_LEAD_FRAMES {
            return;
        }
        let frames = ((due - k.asked) as u32).min(CHRONO_MAX);
        k.asked += frames as u64;
        let id = k.id;
        if let Some(slot) = self.clients.get(&id) {
            let _ = slot.data_tx.try_send(Bytes::from(p::build_chrono(AUDIO_RATE_HZ, RX, frames)));
        }
    }

    fn drain_up(&mut self) {
        while let Ok(msg) = self.up_rx.try_recv() {
            match msg {
                Up::Text(id, text) => self.on_client_text(id, &text),
                Up::TxAudio(id, mono) => self.on_tx_audio(id, &mono),
                Up::Gone(id) => {
                    info!(id, "TCI client disconnected");
                    self.remove(id);
                }
            }
        }
    }

    fn on_client_text(&mut self, id: u64, text: &str) {
        debug!(id, text, "TCI server: client ->");
        for (cmd, args) in p::parse_status(text) {
            let echo = if args.is_empty() { format!("{cmd};") } else { format!("{cmd}:{args};") };
            let st = self.rig.state();
            let Some(req) = text::translate(&cmd, &args, &st) else {
                debug!(id, cmd, args, "TCI server: unhandled command");
                continue;
            };
            let result = match req {
                ClientReq::SetFreq(vfo, hz) => Some(self.rig.set_freq(vfo, hz)),
                ClientReq::SetCenter(hz) => Some(self.rig.set_center(hz)),
                ClientReq::SetMode(m) if m == st.mode => Some(Ok(())),
                ClientReq::SetMode(m) => Some(self.rig.set_mode(m, None)),
                ClientReq::SetSplit(on) if on == st.split => Some(Ok(())),
                ClientReq::SetSplit(on) => Some(self.rig.set_split(on)),
                ClientReq::Key { on, tci_audio } => {
                    let kind = if tci_audio { PttKind::TciAudio } else { PttKind::RigAudio };
                    self.on_key(id, on, kind, &echo);
                    None
                }
                ClientReq::Tune(on) => {
                    self.on_key(id, on, PttKind::Tune, &echo);
                    None
                }
                ClientReq::Iq(on) => {
                    self.with_subs(id, |s| s.iq_on.store(on, Ordering::Relaxed));
                    self.recompute_iq();
                    self.send_to(id, echo.clone());
                    None
                }
                ClientReq::IqRate(hz) => {
                    if let Some(slot) = self.clients.get_mut(&id) {
                        slot.iq_rate = hz;
                    }
                    self.recompute_iq();
                    self.send_to(id, echo.clone());
                    // The rate the panel really gives, which may differ.
                    if st.iq_rate > 0 && st.iq_rate != hz {
                        self.send_to(id, p::iq_samplerate(st.iq_rate));
                    }
                    None
                }
                ClientReq::Audio(on) => {
                    self.with_subs(id, |s| s.audio_on.store(on, Ordering::Relaxed));
                    self.recompute_audio();
                    self.send_to(id, echo.clone());
                    None
                }
                ClientReq::TxSensors(on) => {
                    self.with_subs(id, |s| s.tx_sensors.store(on, Ordering::Relaxed));
                    self.send_to(id, echo.clone());
                    None
                }
                ClientReq::RxSensors(on) => {
                    self.with_subs(id, |s| s.rx_sensors.store(on, Ordering::Relaxed));
                    self.send_to(id, echo.clone());
                    None
                }
                ClientReq::Ignored => {
                    match text::truth_for(&cmd, &st) {
                        Some(line) => self.send_to(id, line),
                        None => self.send_to(id, echo.clone()),
                    }
                    None
                }
            };
            match result {
                Some(Ok(())) => self.broadcast(&echo),
                Some(Err(why)) => {
                    info!(id, cmd, %why, "TCI: the panel refused");
                    self.send_to(id, echo);
                    if let Some(line) = text::truth_for(&cmd, &self.rig.state()) {
                        self.send_to(id, line);
                    }
                }
                None => {}
            }
        }
    }

    /// Grant or release the transmit key. Exactly one client may hold it.
    fn on_key(&mut self, id: u64, on: bool, kind: PttKind, echo: &str) {
        let source = PttSource::Tci { client: id };
        let off_line = |kind: PttKind| {
            if kind == PttKind::Tune { p::tune(RX, false) } else { p::trx(RX, false, false) }
        };
        if !on {
            // Unkeying is never refused. A client that does not hold the key
            // still gets its echo, and the radio's real state.
            self.send_to(id, echo.to_string());
            if self.key.as_ref().is_some_and(|k| k.id == id) {
                self.release_key("the client unkeyed");
            }
            return;
        }
        // SAFETY INTERLOCK: a client keys the radio only when transmit is
        // allowed here, the radio can transmit, no other client holds the key,
        // and the panel grants it (safety interlock #8).
        let st = self.rig.state();
        if !self.cfg.allow_tx || !st.can_tx {
            debug!(id, "TCI: transmit is not allowed");
            self.send_to(id, off_line(kind));
            return;
        }
        if let Some(k) = &self.key
            && k.id != id
        {
            debug!(id, holder = k.id, "TCI: another client holds the key");
            self.send_to(id, off_line(kind));
            return;
        }
        match self.rig.request_ptt(PttRequest { source, kind }) {
            Ok(()) => {
                // Clear out anything a previous over left queued.
                if self.key.as_ref().is_none_or(|k| k.kind != kind) {
                    self.key = Some(Key {
                        id,
                        kind,
                        since: Instant::now(),
                        last_audio: None,
                        asked: 0,
                        seen_on_air: false,
                    });
                }
                self.shared.tx_keyed.store(true, Ordering::Relaxed);
                self.broadcast(echo);
            }
            Err(why) => {
                info!(id, %why, "TCI: the panel refused to transmit");
                self.send_to(id, echo.to_string());
                self.send_to(id, off_line(kind));
            }
        }
    }

    /// Drop the key and tell the panel to stop transmitting for the client.
    fn release_key(&mut self, why: &str) {
        let Some(k) = self.key.take() else { return };
        debug!(id = k.id, why, "TCI: releasing the key");
        self.shared.tx_keyed.store(false, Ordering::Relaxed);
        self.rig.release_ptt(PttSource::Tci { client: k.id });
        let line =
            if k.kind == PttKind::Tune { p::tune(RX, false) } else { p::trx(RX, false, false) };
        self.send_to(k.id, line);
    }

    /// The panel took the transmitter back: tell the client, without asking
    /// the panel to unkey.
    fn revoke_key(&mut self) {
        let Some(k) = self.key.take() else { return };
        self.shared.tx_keyed.store(false, Ordering::Relaxed);
        let line =
            if k.kind == PttKind::Tune { p::tune(RX, false) } else { p::trx(RX, false, false) };
        self.send_to(k.id, line);
    }

    fn on_tx_audio(&mut self, id: u64, mono: &[f32]) {
        let Some(k) = &mut self.key else { return };
        if k.id != id || k.kind != PttKind::TciAudio {
            return;
        }
        k.last_audio = Some(Instant::now());
        for &s in mono {
            if self.tx_prod.push(s).is_err() {
                break;
            }
        }
    }

    fn with_subs(&self, id: u64, f: impl FnOnce(&Subs)) {
        if let Some(slot) = self.clients.get(&id) {
            f(&slot.subs);
        }
    }

    /// The highest rate any I/Q subscriber asked for, or 0.
    fn recompute_iq(&mut self) {
        let rate = self
            .clients
            .values()
            .filter(|s| s.subs.iq_on.load(Ordering::Relaxed))
            .map(|s| if s.iq_rate == 0 { 192_000 } else { s.iq_rate })
            .max()
            .unwrap_or(0);
        self.shared.iq_rate.store(rate, Ordering::Relaxed);
    }

    fn recompute_audio(&mut self) {
        let any = self.clients.values().any(|s| s.subs.audio_on.load(Ordering::Relaxed));
        self.shared.audio_on.store(any, Ordering::Relaxed);
    }

    fn send_to(&self, id: u64, line: String) {
        debug!(id, line, "TCI server: -> client");
        if let Some(slot) = self.clients.get(&id) {
            let _ = slot.text_tx.send(line);
        }
    }

    fn broadcast(&self, line: &str) {
        debug!(line, "TCI server: -> all clients");
        for slot in self.clients.values() {
            let _ = slot.text_tx.send(line.to_string());
        }
    }

    fn remove(&mut self, id: u64) {
        if self.key.as_ref().is_some_and(|k| k.id == id) {
            // SAFETY INTERLOCK: a client that vanishes mid-over unkeys the
            // radio (safety interlock #8).
            self.release_key("the client disconnected");
        }
        let Some(slot) = self.clients.remove(&id) else { return };
        drop(slot.text_tx);
        drop(slot.data_tx);
        if let Some(h) = slot.handle {
            let _ = h.join();
        }
        self.recompute_iq();
        self.recompute_audio();
        self.shared.clients.store(self.clients.len(), Ordering::Relaxed);
    }
}
