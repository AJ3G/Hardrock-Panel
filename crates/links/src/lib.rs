//! Links for outside programs: the ways WSJT-X, JTDX, fldigi, loggers and
//! skimmers reach the panel's radio when AJ3G would rather use them than the
//! panel's own decoders.
//!
//! * [`TciServer`]: Expert Electronics' TCI over WebSocket (port 50001 by
//!   convention), with the wideband I/Q and receive audio streams and transmit
//!   audio from the client.
//! * [`RigctldServer`]: Hamlib's "NET rigctl" protocol (rig model 2, port
//!   4532), which every Hamlib program can use without a serial cable.
//! * [`WsjtxUdp`]: the WSJT-X UDP messages (port 2237) GridTracker, JTAlert
//!   and loggers listen for, sent from the panel's own FT8/FT4/WSPR decodes.
//!
//! Both servers are driven by a [`Rig`] the panel implements. They read its
//! [`RigState`] and ask it to tune, change mode or split, and to transmit.
//! Every request to transmit is a request: the panel may refuse it, and does
//! whenever its own interlocks say no (see docs/safety-interlocks.md, #8). A
//! request to stop transmitting is never refused.
//!
//! Ported from SDRoxide (<https://github.com/dividebysandwich/sdroxide>,
//! GPL-3.0-or-later): crates/sdroxide-tci (the server half),
//! crates/sdroxide-rigctld and crates/sdroxide-wsjtx, with SDRoxide's engine
//! commands replaced by the [`Rig`] trait.

pub mod rigctld;
pub mod tci;
pub mod wsjtx;

pub use rigctld::{RigctldConfig, RigctldServer};
pub use tci::{TciConfig, TciServer};
pub use wsjtx::{DecodeInfo, StatusInfo, WsjtxUdp, WsprDecodeInfo};

/// One of the radio's two VFOs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Vfo {
    #[default]
    A,
    B,
}

impl Vfo {
    pub fn other(self) -> Vfo {
        match self {
            Vfo::A => Vfo::B,
            Vfo::B => Vfo::A,
        }
    }
}

/// The modes outside programs can see and ask for. The panel maps them onto
/// its own; the names follow Hamlib's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Mode {
    Lsb,
    #[default]
    Usb,
    Cw,
    /// CW on the lower side.
    CwR,
    Am,
    /// Narrow FM.
    Fm,
    WideFm,
    /// Data on the upper sideband: Hamlib's `PKTUSB`, TCI's `digu`.
    DataU,
    /// Data on the lower sideband: Hamlib's `PKTLSB`, TCI's `digl`.
    DataL,
    Rtty,
    RttyR,
}

impl Mode {
    pub const ALL: [Mode; 11] = [
        Mode::Lsb,
        Mode::Usb,
        Mode::Cw,
        Mode::CwR,
        Mode::Am,
        Mode::Fm,
        Mode::WideFm,
        Mode::DataU,
        Mode::DataL,
        Mode::Rtty,
        Mode::RttyR,
    ];
}

/// Why the panel did not do what an outside program asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl Refused {
    pub fn new(why: impl Into<String>) -> Refused {
        Refused(why.into())
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which link, and which of its clients, asked to transmit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PttSource {
    Tci { client: u64 },
    Rigctld { client: u64 },
}

/// What an outside program wants to transmit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PttKind {
    /// An ordinary transmission of the audio the TCI client streams to us
    /// ([`TciServer::read_tx_audio`]). `trx:0,true,tci;`.
    TciAudio,
    /// An ordinary transmission of whatever the radio's own audio input
    /// carries: rigctld's `T 1`, or TCI's `trx:0,true;` without `tci`.
    RigAudio,
    /// A tuning carrier: TCI's `tune:0,true;`, rigctld's `G TUNE`.
    Tune,
}

/// A request to transmit from an outside program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PttRequest {
    pub source: PttSource,
    pub kind: PttKind,
}

/// The slice of the radio's state outside programs can see. The servers ask
/// [`Rig::state`] for it whenever they answer a query and, for TCI, ten times
/// a second to send what changed.
#[derive(Debug, Clone, PartialEq)]
pub struct RigState {
    pub vfo_a_hz: f64,
    pub vfo_b_hz: f64,
    /// The VFO receive is on.
    pub active_vfo: Vfo,
    /// Transmit on the other VFO.
    pub split: bool,
    pub mode: Mode,
    /// Passband edges in Hz relative to the dial.
    pub passband: (f32, f32),
    /// Whether the radio is transmitting, for whatever reason.
    pub ptt: bool,
    /// Whether a tuning carrier is on.
    pub tune: bool,
    /// Centre of the wideband I/Q, TCI's `dds`. Zero when there is none.
    pub center_hz: f64,
    /// The I/Q rate the panel feeds [`TciServer::push_iq`] at; zero when it
    /// has no I/Q to give, and TCI then does not advertise the stream.
    pub iq_rate: u32,
    /// Transmit power, 0 to 100 %, reported only.
    pub drive_pct: u32,
    /// Signal strength in dBm (S9 is −73 dBm).
    pub strength_dbm: i32,
    /// Whether this radio can transmit at all.
    pub can_tx: bool,
    /// Tunable ranges, in Hz. Empty means 0 to 6 GHz.
    pub rx_ranges: Vec<(f64, f64)>,
    /// Ranges the radio may transmit in. Empty means the receive ranges.
    pub tx_ranges: Vec<(f64, f64)>,
}

impl Default for RigState {
    fn default() -> Self {
        RigState {
            vfo_a_hz: 14_074_000.0,
            vfo_b_hz: 14_074_000.0,
            active_vfo: Vfo::A,
            split: false,
            mode: Mode::Usb,
            passband: (100.0, 2700.0),
            ptt: false,
            tune: false,
            center_hz: 0.0,
            iq_rate: 0,
            drive_pct: 0,
            strength_dbm: -73,
            can_tx: false,
            rx_ranges: Vec::new(),
            tx_ranges: Vec::new(),
        }
    }
}

impl RigState {
    pub fn freq(&self, vfo: Vfo) -> f64 {
        match vfo {
            Vfo::A => self.vfo_a_hz,
            Vfo::B => self.vfo_b_hz,
        }
    }

    /// The VFO transmit uses: the other one when split.
    pub fn tx_vfo(&self) -> Vfo {
        if self.split { self.active_vfo.other() } else { self.active_vfo }
    }

    /// The passband's width in Hz.
    pub fn passband_hz(&self) -> i32 {
        (self.passband.1 - self.passband.0).abs().round() as i32
    }
}

/// The radio as the links see it. The panel implements this.
///
/// Called from the links' own threads, so every method must return quickly:
/// queue the change for the panel's loop and answer, rather than waiting on
/// the radio. A setter returns `Err` with the reason when the panel will not
/// make the change, which the client is told.
pub trait Rig: Send + Sync + 'static {
    /// The radio as it is now.
    fn state(&self) -> RigState;

    /// Tune a VFO.
    fn set_freq(&self, vfo: Vfo, hz: f64) -> Result<(), Refused>;

    /// Change mode, and the passband's width when `width_hz` is given.
    fn set_mode(&self, mode: Mode, width_hz: Option<u32>) -> Result<(), Refused>;

    /// Transmit on the other VFO, or not.
    fn set_split(&self, on: bool) -> Result<(), Refused>;

    /// Receive on the other VFO.
    fn select_vfo(&self, _vfo: Vfo) -> Result<(), Refused> {
        Err(Refused::new("not supported"))
    }

    /// Move the centre of the I/Q span (TCI's `dds`).
    fn set_center(&self, _hz: f64) -> Result<(), Refused> {
        Err(Refused::new("not supported"))
    }

    // SAFETY INTERLOCK: an outside program may key the radio only when this
    // returns Ok. The panel refuses whenever its own checks would refuse its
    // own PTT (amplifier tripped or on the wrong band, drive over the limit,
    // the radio's input not on USB audio, …); see docs/safety-interlocks.md
    // #8. The servers also refuse before asking when their `allow_tx` is off.
    /// Ask to transmit. Granting it means the panel keys the radio (or has
    /// queued that) and will carry on until [`Rig::release_ptt`], or until it
    /// decides to stop by itself.
    fn request_ptt(&self, req: PttRequest) -> Result<(), Refused>;

    /// Stop transmitting on `source`'s behalf. Never refused, and called
    /// whenever a keying client unkeys, disconnects, stops sending audio, or
    /// its server stops. Harmless when `source` was not keyed.
    fn release_ptt(&self, source: PttSource);
}

/// Parse a `host:port`, or a bare port, as the address to bind.
pub(crate) fn bind_addr(host: &str, port: u16) -> String {
    let h = host.trim();
    let h = if h.is_empty() { "127.0.0.1" } else { h };
    if h.contains(':') && !h.starts_with('[') {
        format!("[{h}]:{port}")
    } else {
        format!("{h}:{port}")
    }
}

#[cfg(test)]
pub(crate) mod testrig {
    //! A [`Rig`] for the tests: it records what it was asked and grants or
    //! refuses transmit as told.

    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    pub struct TestRig {
        pub st: Mutex<RigState>,
        pub allow_ptt: AtomicBool,
        pub log: Mutex<Vec<String>>,
    }

    impl TestRig {
        pub fn new(st: RigState) -> TestRig {
            TestRig { st: Mutex::new(st), ..TestRig::default() }
        }
        pub fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
        fn note(&self, s: String) {
            self.log.lock().unwrap().push(s);
        }
    }

    impl Rig for TestRig {
        fn state(&self) -> RigState {
            self.st.lock().unwrap().clone()
        }
        fn set_freq(&self, vfo: Vfo, hz: f64) -> Result<(), Refused> {
            self.note(format!("freq {vfo:?} {hz}"));
            let mut s = self.st.lock().unwrap();
            match vfo {
                Vfo::A => s.vfo_a_hz = hz,
                Vfo::B => s.vfo_b_hz = hz,
            }
            Ok(())
        }
        fn set_mode(&self, mode: Mode, width_hz: Option<u32>) -> Result<(), Refused> {
            self.note(format!("mode {mode:?} {width_hz:?}"));
            self.st.lock().unwrap().mode = mode;
            Ok(())
        }
        fn set_split(&self, on: bool) -> Result<(), Refused> {
            self.note(format!("split {on}"));
            self.st.lock().unwrap().split = on;
            Ok(())
        }
        fn request_ptt(&self, req: PttRequest) -> Result<(), Refused> {
            self.note(format!("ptt {:?} {:?}", req.source, req.kind));
            if self.allow_ptt.load(Ordering::SeqCst) {
                self.st.lock().unwrap().ptt = true;
                Ok(())
            } else {
                Err(Refused::new("the amplifier is not ready"))
            }
        }
        fn release_ptt(&self, source: PttSource) {
            self.note(format!("unptt {source:?}"));
            self.st.lock().unwrap().ptt = false;
        }
    }
}
