// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-tci/src/server/mod.rs, GPL-3.0-or-later.

//! The **TCI server**: the panel's radio as a TCI rig, so WSJT-X (its TCI rig
//! type), JTDX, MSHV and skimmers can drive it and take its I/Q and audio.
//!
//! The panel owns a [`TciServer`], pushes I/Q and receive audio into it (both
//! dropped rather than queued when a client falls behind, so a slow client
//! never holds up the panel), and reads the transmit audio of whichever
//! client holds the key. Control goes through the [`Rig`] the server was
//! started with: a hub thread asks it for the state ten times a second and
//! sends clients what changed, and hands it their requests. All socket work
//! is on the hub thread and one thread per client.
//!
//! **Model.** One transceiver (`trx_count:1`) with two channels (VFO A and B).
//! The streams are on receiver 0. `dds:` is the I/Q centre and `if:` a VFO's
//! offset from it.
//!
//! **Every command is echoed back**, even when it changes nothing: clients
//! use the echo as the acknowledgement and block on it (WSJT-X gives up on
//! the connection without it). A command the panel refuses is echoed, then
//! followed by the line that says what is true.
//!
//! **Transmit.** Off unless [`TciConfig::allow_tx`] is on; then a client's
//! `trx` or `tune` goes to [`Rig::request_ptt`], which may refuse. One client
//! at a time may hold the key. The hub paces the keying client's audio with
//! TCI's `TxChrono` packets and unkeys it if the audio stops. See safety
//! interlock #8.
//!
//! `RUST_LOG=links=debug` logs the whole conversation in both directions.

mod client;
mod hub;
pub mod protocol;
mod text;

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Sender, bounded, unbounded};

use crate::Rig;

/// The conventional TCI port (ExpertSDR3).
pub const DEFAULT_PORT: u16 = 50001;
/// TCI protocol version claimed in the `protocol:` line.
const TCI_VERSION: &str = "1.9";
/// Both audio streams (receive out and transmit in) run at 48 kHz.
pub const AUDIO_RATE_HZ: u32 = 48_000;
/// Transmit audio ring: one second at 48 kHz.
const TX_RING: usize = AUDIO_RATE_HZ as usize;

/// How the server is set up.
#[derive(Debug, Clone, PartialEq)]
pub struct TciConfig {
    /// The address to listen on. `127.0.0.1` keeps it to this computer.
    pub host: String,
    /// 0 asks the system for a free port.
    pub port: u16,
    // SAFETY INTERLOCK: off unless the operator turns it on. With it off the
    // burst says `receive_only:true` and every `trx`/`tune` is answered
    // `false` without asking the panel (safety interlock #8).
    /// Whether clients may ask to transmit at all.
    pub allow_tx: bool,
    pub max_clients: usize,
    /// The `device:` name clients show.
    pub device_name: String,
}

impl Default for TciConfig {
    fn default() -> Self {
        TciConfig {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            allow_tx: false,
            max_clients: 4,
            device_name: "hardrock-panel".into(),
        }
    }
}

/// Control traffic to the hub, never dropped.
pub(crate) enum Ctl {
    Config(TciConfig),
    /// The panel took the transmitter back: tell the keying client.
    Revoke,
    Stop,
}

/// One block of realtime data, dropped when the hub is behind.
pub(crate) struct IqBlock {
    pub rate: u32,
    pub data: Vec<f32>,
}
pub(crate) struct AudioBlock(pub Vec<f32>);

/// One client's subscriptions, written by the hub and read before fan-out.
#[derive(Default)]
pub(crate) struct Subs {
    pub iq_on: AtomicBool,
    pub audio_on: AtomicBool,
    pub tx_sensors: AtomicBool,
    pub rx_sensors: AtomicBool,
    pub dropped: AtomicUsize,
}

/// Totals the panel reads without waiting on the hub.
#[derive(Default)]
pub(crate) struct Shared {
    pub clients: AtomicUsize,
    /// The I/Q rate clients asked for; 0 when nobody wants I/Q.
    pub iq_rate: AtomicU32,
    pub audio_on: AtomicBool,
    /// Whether a client holds the transmit key.
    pub tx_keyed: AtomicBool,
}

/// Transmit metering for clients that asked for `tx_sensors`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TxSensors {
    pub mic_db: f32,
    pub fwd_w: f32,
    pub rev_w: f32,
    pub swr: f32,
}

/// The panel's handle on the running server. Dropping it stops the server
/// and unkeys any client that held the key.
pub struct TciServer {
    ctl_tx: Sender<Ctl>,
    iq_tx: Sender<IqBlock>,
    aud_tx: Sender<AudioBlock>,
    tel_tx: Sender<TxSensors>,
    tx_audio: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    addr: String,
    hub: Option<JoinHandle<()>>,
}

impl TciServer {
    /// Bind and start serving. The listener is bound before the hub starts,
    /// so a taken port (a local ExpertSDR3 on 50001) comes back as an error.
    pub fn start(cfg: &TciConfig, rig: Arc<dyn Rig>) -> Result<Self, String> {
        let want = crate::bind_addr(&cfg.host, cfg.port);
        let listener = TcpListener::bind(&want).map_err(|e| format!("bind {want}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| format!("{want}: {e}"))?;
        let addr = listener.local_addr().map(|a| a.to_string()).unwrap_or(want);

        let (ctl_tx, ctl_rx) = unbounded();
        let (iq_tx, iq_rx) = bounded(8);
        let (aud_tx, aud_rx) = bounded(16);
        let (tel_tx, tel_rx) = bounded(4);
        let (tx_prod, tx_audio) = rtrb::RingBuffer::<f32>::new(TX_RING);

        let shared = Arc::new(Shared::default());
        let hub = hub::spawn(hub::HubParams {
            listener,
            cfg: cfg.clone(),
            rig,
            ctl_rx,
            iq_rx,
            aud_rx,
            tel_rx,
            tx_prod,
            shared: shared.clone(),
        });
        Ok(TciServer { ctl_tx, iq_tx, aud_tx, tel_tx, tx_audio, shared, addr, hub: Some(hub) })
    }

    /// The `host:port` it is bound to.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn clients(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Change the settings that do not move the socket.
    pub fn set_config(&self, cfg: TciConfig) {
        let _ = self.ctl_tx.send(Ctl::Config(cfg));
    }

    /// Hand over interleaved I/Q (`I0, Q0, I1, …`) at `rate`. Costs one atomic
    /// load when no client wants I/Q. The rate should be the one in
    /// [`crate::RigState::iq_rate`].
    pub fn push_iq(&self, iq: &[f32], rate: u32) {
        if self.shared.iq_rate.load(Ordering::Relaxed) == 0 || iq.is_empty() {
            return;
        }
        let _ = self.iq_tx.try_send(IqBlock { rate, data: iq.to_vec() });
    }

    /// Hand over 48 kHz mono receive audio.
    pub fn push_audio(&self, mono: &[f32]) {
        if !self.shared.audio_on.load(Ordering::Relaxed) || mono.is_empty() {
            return;
        }
        let _ = self.aud_tx.try_send(AudioBlock(mono.to_vec()));
    }

    /// Publish transmit metering to clients that asked for it.
    pub fn push_tx_sensors(&self, t: TxSensors) {
        let _ = self.tel_tx.try_send(t);
    }

    /// The I/Q rate clients ask for (the highest), or `None` when nobody
    /// wants I/Q.
    pub fn wants_iq(&self) -> Option<u32> {
        match self.shared.iq_rate.load(Ordering::Relaxed) {
            0 => None,
            r => Some(r),
        }
    }

    pub fn wants_audio(&self) -> bool {
        self.shared.audio_on.load(Ordering::Relaxed)
    }

    /// Whether a client holds the transmit key.
    pub fn tx_keyed(&self) -> bool {
        self.shared.tx_keyed.load(Ordering::Relaxed)
    }

    /// Take up to `out.len()` samples of the keying client's 48 kHz mono
    /// transmit audio; returns how many there were. Only a client the panel
    /// granted the key with [`crate::PttKind::TciAudio`] fills this.
    pub fn read_tx_audio(&mut self, out: &mut [f32]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.tx_audio.pop() {
                Ok(s) => {
                    out[n] = s;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        n
    }

    /// Throw away queued transmit audio, so the next over starts clean.
    pub fn drain_tx_audio(&mut self) {
        while self.tx_audio.pop().is_ok() {}
    }

    /// The panel stopped transmitting for a client by itself (its interlocks,
    /// or the operator's own PTT): tell the client it no longer has the key.
    /// The hub also notices this from [`crate::RigState::ptt`].
    pub fn revoke_tx(&self) {
        let _ = self.ctl_tx.send(Ctl::Revoke);
    }
}

impl Drop for TciServer {
    fn drop(&mut self) {
        let _ = self.ctl_tx.send(Ctl::Stop);
        if let Some(h) = self.hub.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::protocol as p;
    use super::*;
    use crate::testrig::TestRig;
    use crate::{Mode, RigState};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    use tungstenite::{Message, WebSocket};

    type Ws = WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

    fn rig() -> Arc<TestRig> {
        Arc::new(TestRig::new(RigState {
            center_hz: 14_100_000.0,
            vfo_a_hz: 14_074_000.0,
            iq_rate: 192_000,
            can_tx: true,
            ..RigState::default()
        }))
    }

    fn cfg(allow_tx: bool) -> TciConfig {
        TciConfig { port: 0, allow_tx, ..TciConfig::default() }
    }

    fn connect(srv: &TciServer) -> Ws {
        let (ws, _) = tungstenite::connect(format!("ws://{}/", srv.addr())).expect("connect");
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        }
        ws
    }

    /// Read until a text frame contains `want` or `secs` pass, keeping all text
    /// and counting binary frames of type `bin`.
    fn read_until(ws: &mut Ws, want: &str, secs: f64) -> (String, Vec<Vec<u8>>) {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        let mut text = String::new();
        let mut bins = Vec::new();
        while Instant::now() < end {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    text.push_str(t.as_str());
                    if !want.is_empty() && text.contains(want) {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => bins.push(b.to_vec()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("read: {e}"),
            }
        }
        (text, bins)
    }

    fn send(ws: &mut Ws, s: &str) {
        ws.send(Message::Text(s.to_string().into())).unwrap();
    }

    #[test]
    fn a_client_gets_the_burst_and_tunes_the_radio() {
        let r = rig();
        let srv = TciServer::start(&cfg(false), r.clone()).unwrap();
        let mut ws = connect(&srv);
        let (burst, _) = read_until(&mut ws, "ready;", 5.0);
        assert!(burst.contains("receive_only:true;"), "{burst}");
        assert!(burst.contains("vfo:0,0,14074000;"), "{burst}");

        send(&mut ws, "vfo:0,0,14076000;modulation:0,digu;");
        let (got, _) = read_until(&mut ws, "modulation:0,digu;", 3.0);
        assert!(got.contains("vfo:0,0,14076000;"), "{got}");
        let st = r.state();
        assert_eq!(st.vfo_a_hz, 14_076_000.0);
        assert_eq!(st.mode, Mode::DataU);

        // I/Q reaches a subscriber.
        send(&mut ws, "iq_samplerate:192000;iq_start:0;");
        read_until(&mut ws, "iq_start:0;", 3.0);
        let end = Instant::now() + Duration::from_secs(3);
        while srv.wants_iq().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(srv.wants_iq(), Some(192_000));
        srv.push_iq(&[0.5, -0.5, 0.25, -0.25], 192_000);
        let (_, bins) = read_until(&mut ws, "never", 0.5);
        let iq = bins.iter().find_map(|b| {
            let h = p::parse_header(b)?;
            (h.dtype == p::DataType::Iq).then_some(h)
        });
        assert!(iq.is_some_and(|h| h.sample_rate == 192_000 && h.length == 4));
    }

    #[test]
    fn transmit_is_refused_while_not_allowed_and_when_the_panel_says_no() {
        let r = rig();
        r.allow_ptt.store(true, std::sync::atomic::Ordering::SeqCst);
        let srv = TciServer::start(&cfg(false), r.clone()).unwrap();
        let mut ws = connect(&srv);
        read_until(&mut ws, "ready;", 5.0);
        send(&mut ws, "trx:0,true,tci;");
        let (got, _) = read_until(&mut ws, "trx:0,false;", 3.0);
        assert!(got.contains("trx:0,false;"), "{got}");
        assert!(r.log().iter().all(|l| !l.starts_with("ptt")), "the panel was asked");
        drop(ws);
        drop(srv);

        r.allow_ptt.store(false, std::sync::atomic::Ordering::SeqCst);
        let srv = TciServer::start(&cfg(true), r.clone()).unwrap();
        let mut ws = connect(&srv);
        read_until(&mut ws, "ready;", 5.0);
        send(&mut ws, "trx:0,true,tci;");
        let (got, _) = read_until(&mut ws, "trx:0,false;", 3.0);
        assert!(got.contains("trx:0,false;"), "{got}");
        assert!(r.log().iter().any(|l| l == "ptt Tci { client: 1 } TciAudio"));
        assert!(!r.state().ptt);
        assert!(!srv.tx_keyed());
    }

    /// Keyed for its own audio, a client is paced with chronos, its audio
    /// reaches the panel, and when it stops sending the radio is unkeyed.
    #[test]
    fn a_keying_client_is_paced_and_unkeyed_when_its_audio_stops() {
        let r = rig();
        r.allow_ptt.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut srv = TciServer::start(&cfg(true), r.clone()).unwrap();
        let mut ws = connect(&srv);
        read_until(&mut ws, "ready;", 5.0);
        send(&mut ws, "audio_start:0;trx:0,true,tci;");
        let (_, bins) = read_until(&mut ws, "never", 0.5);
        assert!(r.state().ptt);
        assert!(srv.tx_keyed());
        let chronos = bins
            .iter()
            .filter_map(|b| p::parse_header(b))
            .filter(|h| h.dtype == p::DataType::TxChrono)
            .count();
        assert!(chronos > 0, "no TxChrono");

        let tone: Vec<f32> = (0..960).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        ws.send(Message::Binary(p::build_tx_audio(48_000, 0, &tone).into())).unwrap();
        let mut got = vec![0.0f32; 2000];
        let end = Instant::now() + Duration::from_secs(3);
        let mut n = 0;
        while n < tone.len() && Instant::now() < end {
            n += srv.read_tx_audio(&mut got[n..]);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(&got[..tone.len()], &tone[..]);

        // Now silence from the client: unkeyed within the timeout.
        let (text, _) =
            read_until(&mut ws, "trx:0,false;", hub::TX_AUDIO_TIMEOUT.as_secs_f64() + 2.0);
        assert!(text.contains("trx:0,false;"), "{text}");
        assert!(!r.state().ptt);
        assert!(r.log().iter().any(|l| l == "unptt Tci { client: 1 }"));
    }

    #[test]
    fn a_client_that_disconnects_while_keyed_unkeys_the_radio() {
        let r = rig();
        r.allow_ptt.store(true, std::sync::atomic::Ordering::SeqCst);
        let srv = TciServer::start(&cfg(true), r.clone()).unwrap();
        let mut ws = connect(&srv);
        read_until(&mut ws, "ready;", 5.0);
        send(&mut ws, "tune:0,true;");
        read_until(&mut ws, "tune:0,true;", 3.0);
        assert!(r.state().ptt);
        drop(ws);
        let end = Instant::now() + Duration::from_secs(5);
        while r.state().ptt && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!r.state().ptt, "still keyed after the client left");
        assert!(r.log().iter().any(|l| l == "ptt Tci { client: 1 } Tune"));
        drop(srv);
    }

    #[test]
    fn a_taken_port_is_an_error() {
        let r = rig();
        let srv = TciServer::start(&cfg(false), r.clone()).unwrap();
        let port: u16 = srv.addr().rsplit(':').next().unwrap().parse().unwrap();
        let taken = TciConfig { port, ..cfg(false) };
        assert!(TciServer::start(&taken, r).err().unwrap().contains("bind"));
    }
}
