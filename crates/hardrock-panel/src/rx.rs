//! The panel's own receiver (decision 22): the I/Q from the ELAD over USB,
//! or the demo band; the `sdr` pipeline that runs the spectrum and the
//! receiver; the receiver's audio on the sound output chosen under SETTINGS;
//! and the taps that copy the I/Q and the audio to the decoders and to TCI.
//!
//! As in SDR Console, the I/Q centre is the DUO's VFO A and stays put; the
//! receiver moves inside the span and the centre moves only when the scope
//! asks. To transmit where the panel listens, the radio is put in split with
//! VFO B on the receiver (`split_frames`), through the CAT link, which
//! refuses any change while the radio transmits (interlock #5).
//!
//! What moves the radio from here: VFO A (a recentre), VFO B and split
//! (transmit where you listen), the mode and the receive filter (at 192 kHz,
//! where the DUO's own audio is the one heard). Every one goes through the
//! CAT link's `set`, except with no CAT link, when a recentre goes to the
//! ELAD over USB; that is interlock #10.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError, bounded};
use elad_iq::{EladConfig, EladHandle};
use rigcat::{Frame, Protocol, RigLink, RigStatus, Vfo};
use sdr::{AudioOut, Command, Mode, Pipeline, PipelineConfig, Snapshot};
use serde::{Deserialize, Serialize};

use crate::scope::{self, Scope, ScopeOut};

/// Where the I/Q comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IqChoice {
    /// No scope and no receiver.
    #[default]
    Off,
    /// The ELAD radio's USB I/Q (the FDM-DUO's second USB device).
    Elad,
    /// `sdr`'s synthetic band: a few stations around 14.2 MHz, to see the
    /// scope and the decoders without hardware.
    Demo,
}

impl IqChoice {
    pub(crate) const ALL: [IqChoice; 3] = [IqChoice::Off, IqChoice::Elad, IqChoice::Demo];

    pub(crate) fn label(self) -> &'static str {
        match self {
            IqChoice::Off => "Off",
            IqChoice::Elad => "ELAD over USB",
            IqChoice::Demo => "Demo signals",
        }
    }
}

/// The receiver's settings, kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RxSettings {
    pub source: IqChoice,
    /// The I/Q's complex rate, Hz: the scope's SPAN. On the FDM-DUO this says
    /// how its stream is read; the radio's own rate is set on the radio.
    pub rate: u32,
    /// The sound output the receiver plays on; `None` is the system's default.
    pub audio_out: Option<String>,
    pub volume: f32,
    pub muted: bool,
    /// Put the radio in split with VFO B on the receiver, so it transmits
    /// where the panel listens (decision 22).
    pub tx_follows_rx: bool,
    /// Where the receiver and the I/Q centre were, the radio's own Hz. The
    /// centre opens the ELAD when VFO A can't be read over CAT.
    pub rx_hz: f64,
    pub centre_hz: f64,
    /// The receiver's mode, as `sdr::Mode::label` names it, and passband.
    pub mode: String,
    pub passband: (f32, f32),
}

impl Default for RxSettings {
    fn default() -> Self {
        RxSettings {
            source: IqChoice::Off,
            rate: elad_iq::ELAD_DEFAULT_RATE_HZ,
            audio_out: None,
            volume: 0.5,
            muted: false,
            tx_follows_rx: true,
            rx_hz: 14_212_500.0,
            centre_hz: 14_200_000.0,
            mode: "USB".into(),
            passband: Mode::Usb.default_passband(),
        }
    }
}

pub(crate) fn mode_of(label: &str) -> Mode {
    Mode::ALL.into_iter().find(|m| m.label() == label).unwrap_or(Mode::Usb)
}

/// The radio's own name for a receiver mode (`Protocol::modes`).
pub(crate) fn rig_mode(m: Mode) -> &'static str {
    match m {
        Mode::Lsb => "LSB",
        Mode::Usb => "USB",
        Mode::Cw => "CW",
        Mode::Cwr => "CW-R",
        Mode::Am => "AM",
        Mode::Fm => "FM",
    }
}

/// The receiver mode for the radio's own name of one.
pub(crate) fn sdr_mode(m: &str) -> Option<Mode> {
    Some(match m {
        "LSB" => Mode::Lsb,
        "USB" => Mode::Usb,
        "CW" => Mode::Cw,
        "CW-R" | "CWR" => Mode::Cwr,
        "AM" => Mode::Am,
        "FM" => Mode::Fm,
        _ => return None,
    })
}

/// The DUO plays its own receive audio at this rate and below; above it the
/// panel's receiver is the only one heard (decision 22).
pub(crate) const DUO_AUDIO_MAX_RATE: u32 = 192_000;

/// One block of I/Q for a tap, with its rate and when it was heard.
pub(crate) struct IqBlock<'a> {
    pub iq: &'a [f32],
    pub rate: f64,
    /// The span's centre, the radio's own Hz.
    pub center: f64,
    /// UTC (Unix seconds, system clock) of the first sample.
    pub utc: f64,
}

pub(crate) type AudioSink = Box<dyn Fn(&[f32], f64) + Send + Sync>;
pub(crate) type IqSink = Box<dyn Fn(&IqBlock) + Send + Sync>;

/// Who gets a copy of the I/Q and of the receiver's audio, by name ("decode",
/// "tci"). Called on the pipeline's thread: each sink hands the block to its
/// own queue and returns.
#[derive(Default)]
pub(crate) struct Taps {
    audio: Mutex<Vec<(&'static str, AudioSink)>>,
    iq: Mutex<Vec<(&'static str, IqSink)>>,
}

impl Taps {
    pub(crate) fn set_audio(&self, key: &'static str, sink: Option<AudioSink>) {
        let mut v = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        v.retain(|(k, _)| *k != key);
        if let Some(s) = sink {
            v.push((key, s));
        }
    }

    pub(crate) fn set_iq(&self, key: &'static str, sink: Option<IqSink>) {
        let mut v = self.iq.lock().unwrap_or_else(|e| e.into_inner());
        v.retain(|(k, _)| *k != key);
        if let Some(s) = sink {
            v.push((key, s));
        }
    }

    fn feed_audio(&self, a: &[f32]) {
        let v = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        if v.is_empty() {
            return;
        }
        let utc = utc_now() - a.len() as f64 / sdr::AUDIO_RATE;
        for (_, s) in v.iter() {
            s(a, utc);
        }
    }

    fn feed_iq(&self, iq: &[f32], center: f64, rate: f64) {
        let v = self.iq.lock().unwrap_or_else(|e| e.into_inner());
        if v.is_empty() {
            return;
        }
        let utc = utc_now() - (iq.len() / 2) as f64 / rate.max(1.0);
        let b = IqBlock { iq, rate, center, utc };
        for (_, s) in v.iter() {
            s(&b);
        }
    }
}

/// The system clock, UTC seconds since 1970 (NTP/GPS-disciplined at AJ3G's;
/// decision 26).
pub(crate) fn utc_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// What the radio is doing, as far as the receiver needs to know.
#[derive(Clone, Copy)]
pub(crate) struct RadioNow<'a> {
    pub link: Option<&'a RigLink>,
    /// The radio's status, when its CAT link is connected.
    pub rig: Option<&'a RigStatus>,
    /// The radio or the amplifier says it is transmitting.
    pub transmitting: bool,
}

impl RadioNow<'_> {
    fn cat(&self) -> Option<(&RigLink, &RigStatus)> {
        match (self.link, self.rig) {
            (Some(l), Some(r)) if r.connected => Some((l, r)),
            _ => None,
        }
    }
}

/// The receiver as it runs.
pub(crate) struct Rx {
    pipeline: Option<Pipeline>,
    /// What the pipeline was started from.
    running: IqChoice,
    rate: u32,
    /// An ELAD being opened on a thread of its own (it can take seconds).
    opening: Option<(Receiver<Result<EladHandle, String>>, f64)>,
    /// Why the source couldn't be opened, as the driver says it (the WinUSB
    /// note on Windows), or why it is waiting.
    pub error: Option<String>,
    /// What the ELAD driver warned about when it opened.
    pub notes: Vec<String>,
    pub label: String,
    audio: Option<AudioOut>,
    audio_dev: Option<Option<String>>,
    pub audio_error: Option<String>,
    pub taps: Arc<Taps>,
    /// The receiver, the radio's own Hz, its mode and passband.
    pub rx_hz: f64,
    pub mode: Mode,
    pub passband: (f32, f32),
    /// The I/Q centre last seen, and a centre the panel asked for.
    centre: f64,
    asked_centre: Option<(f64, Instant)>,
    offset_sent: f64,
    /// Since when the receiver has been where it is (for the split's settle).
    rx_moved: Instant,
    last_rx: f64,
    split: SplitState,
    /// The mode and filter width last sent to the DUO.
    duo_sent: Option<(Mode, u32)>,
    /// Restart at the scope's new SPAN.
    pub restart: bool,
}

impl Default for Rx {
    fn default() -> Self {
        let s = RxSettings::default();
        Rx {
            pipeline: None,
            running: IqChoice::Off,
            rate: s.rate,
            opening: None,
            error: None,
            notes: Vec::new(),
            label: String::new(),
            audio: None,
            audio_dev: None,
            audio_error: None,
            taps: Arc::new(Taps::default()),
            rx_hz: s.rx_hz,
            mode: Mode::Usb,
            passband: s.passband,
            centre: s.centre_hz,
            asked_centre: None,
            offset_sent: f64::NAN,
            rx_moved: Instant::now(),
            last_rx: s.rx_hz,
            split: SplitState::default(),
            duo_sent: None,
            restart: false,
        }
    }
}

impl Rx {
    /// A receiver set up as it was last run.
    pub(crate) fn new(s: &RxSettings) -> Rx {
        Rx {
            rate: s.rate,
            rx_hz: s.rx_hz,
            last_rx: s.rx_hz,
            mode: mode_of(&s.mode),
            passband: s.passband,
            centre: s.centre_hz,
            ..Rx::default()
        }
    }

    /// Whether the pipeline is running (or the ELAD opening).
    pub(crate) fn active(&self) -> bool {
        self.pipeline.is_some() || self.opening.is_some()
    }

    pub(crate) fn source(&self) -> IqChoice {
        self.running
    }

    pub(crate) fn opening(&self) -> bool {
        self.opening.is_some()
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.pipeline.as_ref().map(Pipeline::snapshot).unwrap_or_default()
    }

    /// The I/Q centre, the radio's own Hz.
    pub(crate) fn centre(&self) -> f64 {
        self.centre
    }

    /// The I/Q rate the pipeline runs at, 0 when it doesn't.
    pub(crate) fn rate(&self) -> f64 {
        self.pipeline.as_ref().map_or(0.0, |p| p.lock().sample_rate)
    }

    /// Stop the pipeline and let the radio's USB go.
    pub(crate) fn stop(&mut self) {
        self.pipeline = None;
        self.opening = None;
        self.running = IqChoice::Off;
        self.label.clear();
        self.notes.clear();
        self.offset_sent = f64::NAN;
    }

    /// (Re)start from `s.source` at `s.rate`.
    pub(crate) fn start(&mut self, s: &RxSettings, scope: &Scope, now: RadioNow<'_>) {
        self.stop();
        self.error = None;
        self.rate = s.rate;
        match s.source {
            IqChoice::Off => {}
            IqChoice::Demo => {
                let centre = self.centre;
                let src = sdr::SyntheticSource::demo(s.rate as f64, centre);
                self.spawn(Box::new(src), scope);
                self.running = IqChoice::Demo;
            }
            IqChoice::Elad => {
                // SAFETY INTERLOCK (docs/safety-interlocks.md, #10): opening
                // the ELAD tunes the DUO to the centre over USB, outside the
                // CAT link, so not while it transmits.
                if now.transmitting {
                    self.error =
                        Some("Waiting for the radio to stop transmitting to open its I/Q.".into());
                    self.running = IqChoice::Elad;
                    return;
                }
                // VFO A as the radio reads it, so opening changes nothing.
                let centre = now.rig.filter(|r| r.connected).and_then(|r| r.freq_hz);
                let centre = centre.map_or(self.centre, |hz| hz as f64);
                self.centre = centre;
                let cfg = EladConfig { sample_rate_hz: s.rate, ..EladConfig::default() };
                let (tx, rx) = bounded(1);
                let spawned =
                    std::thread::Builder::new().name("elad-open".into()).spawn(move || {
                        let _ = tx.send(EladHandle::open(&cfg, centre).map_err(|e| e.to_string()));
                    });
                match spawned {
                    Ok(_) => self.opening = Some((rx, centre)),
                    Err(e) => self.error = Some(e.to_string()),
                }
                self.running = IqChoice::Elad;
            }
        }
    }

    fn spawn(&mut self, src: Box<dyn sdr::IqSource>, scope: &Scope) {
        self.label = src.label();
        // A receiver left outside this span (another band last time) starts
        // on the dial, rather than asking for the radio to be moved to it.
        if (self.rx_hz - self.centre).abs() > 0.45 * src.sample_rate() {
            self.rx_hz = self.centre;
        }
        let cfg = PipelineConfig {
            spectrum: scope.spectrum_config(0.0),
            offset_hz: self.rx_hz - self.centre,
            mode: self.mode,
            passband: Some(self.passband),
            history: scope::PIPELINE_HISTORY,
            ..Default::default()
        };
        let p = Pipeline::spawn(src, cfg);
        let t = self.taps.clone();
        p.send(Command::SetIqTap(Some(Box::new(move |iq, c, r| t.feed_iq(iq, c, r)))));
        let t = self.taps.clone();
        p.send(Command::SetAudioTap(Some(Box::new(move |a| t.feed_audio(a)))));
        self.offset_sent = self.rx_hz - self.centre;
        // The sound card is opened again for the new pipeline's feed.
        self.audio = None;
        self.audio_dev = None;
        self.pipeline = Some(p);
    }

    /// Once a frame: finish opening the ELAD, keep the sound output, the
    /// offset, the split and the DUO's mode in step.
    pub(crate) fn frame(&mut self, s: &mut RxSettings, scope: &Scope, now: RadioNow<'_>) {
        if let Some((rx, centre)) = &self.opening {
            let centre = *centre;
            match rx.try_recv() {
                Ok(Ok(h)) => {
                    self.opening = None;
                    // The DUO's knob moves its I/Q: follow it over USB.
                    h.follow_radio_dial(true);
                    self.notes = h.warnings.clone();
                    self.spawn(Box::new(sdr::EladSource::new(h, centre)), scope);
                }
                Ok(Err(e)) => {
                    self.opening = None;
                    self.error = Some(e);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.opening = None;
                    self.error = Some("The ELAD driver stopped while opening.".into());
                }
            }
        }
        // An ELAD that waited for the end of a transmission.
        if self.running == IqChoice::Elad
            && self.pipeline.is_none()
            && self.opening.is_none()
            && !now.transmitting
            && self.error.as_deref().is_some_and(|e| e.starts_with("Waiting"))
        {
            self.start(s, scope, now);
        }
        self.keep_audio(s);
        let Some(p) = &self.pipeline else { return };
        let snap_centre = p.lock().center_hz;
        if snap_centre > 0.0 && snap_centre != self.centre {
            let asked = self.asked_centre.is_some_and(|(c, _)| (c - snap_centre).abs() < 1.0);
            if !asked {
                // The dial was turned on the radio: the receiver moves with it.
                self.rx_hz += snap_centre - self.centre;
            }
            self.centre = snap_centre;
            self.asked_centre = None;
        }
        if self.asked_centre.is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(3)) {
            self.asked_centre = None;
        }
        let off = self.rx_hz - self.centre;
        if off != self.offset_sent {
            p.set_offset_hz(off);
            self.offset_sent = off;
        }
        if self.rx_hz != self.last_rx {
            self.last_rx = self.rx_hz;
            self.rx_moved = Instant::now();
        }
        s.rx_hz = self.rx_hz;
        s.centre_hz = self.centre;
        s.mode = self.mode.label().into();
        s.passband = self.passband;

        // Transmit where the panel listens: only on the radio whose I/Q this
        // is, and once the receiver has settled. SAFETY INTERLOCK
        // (docs/safety-interlocks.md, #9): VFO B and split are not touched
        // while transmitting; the link refuses them then too (#5).
        if self.running == IqChoice::Elad
            && let Some((link, r)) = now.cat()
            && !now.transmitting
            && self.rx_moved.elapsed() >= SPLIT_SETTLE
        {
            let proto = r.model.protocol();
            let frames = self.split.frames(&*proto, r, self.rx_hz, s.tx_follows_rx);
            // Read back after each; refused while transmitting (#5).
            link.set(frames);
        }
    }

    /// Open the sound output, or another one, and keep its volume.
    fn keep_audio(&mut self, s: &RxSettings) {
        let Some(p) = &self.pipeline else {
            self.audio = None;
            return;
        };
        if self.audio_dev.as_ref() != Some(&s.audio_out) {
            self.audio_dev = Some(s.audio_out.clone());
            self.audio = None;
            match AudioOut::open(s.audio_out.as_deref(), s.volume) {
                Ok((out, feed)) => {
                    p.set_audio(Some(feed));
                    self.audio = Some(out);
                    self.audio_error = None;
                }
                Err(e) => {
                    p.set_audio(None);
                    self.audio_error = Some(e);
                }
            }
        }
        if let Some(a) = &self.audio {
            a.set_volume(s.volume);
            a.set_muted(s.muted);
        }
    }

    /// The sound output in use, and its underruns and dropped samples.
    pub(crate) fn audio_state(&self) -> Option<(String, u64, u64)> {
        self.audio.as_ref().map(|a| {
            let (u, d) = a.stats();
            (a.device().to_string(), u, d)
        })
    }

    /// Tune the receiver. Not while transmitting: the radio follows it in
    /// split (interlock #9).
    pub(crate) fn tune(&mut self, hz: f64, transmitting: bool) {
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #9): in split the
        // radio transmits on the receiver, so it doesn't move while keyed.
        if !transmitting && hz.is_finite() && hz > 0.0 {
            self.rx_hz = hz;
        }
    }

    /// Move the I/Q centre, and so the DUO's VFO A, to `hz`.
    pub(crate) fn recentre(&mut self, hz: f64, now: RadioNow<'_>) {
        let Some(p) = &self.pipeline else { return };
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #9 and #10): the
        // centre is VFO A, never moved while transmitting.
        if now.transmitting {
            return;
        }
        match self.running {
            IqChoice::Demo => p.send(Command::SetCenter(hz)),
            IqChoice::Elad => {
                if let Some((link, r)) = now.cat() {
                    // VFO A over CAT, read back; the I/Q follows it, as the
                    // USB read-back of the dial shows (interlock #5).
                    if let Some(f) = r.model.protocol().set_freq(Vfo::A, hz.round() as u64) {
                        link.set(vec![f]);
                    }
                } else {
                    // SAFETY INTERLOCK (docs/safety-interlocks.md, #10): with
                    // no CAT link the centre is moved over USB, which retunes
                    // the DUO outside interlock #5, so never while it
                    // transmits (checked above).
                    p.send(Command::SetCenter(hz));
                }
            }
            IqChoice::Off => return,
        }
        self.asked_centre = Some((hz, Instant::now()));
    }

    /// Set the receiver's mode and passband; at 192 kHz, also the DUO's mode
    /// and receive filter, since its own audio is the one heard there.
    pub(crate) fn set_mode(&mut self, m: Mode, pb: Option<(f32, f32)>, now: RadioNow<'_>) {
        self.mode = m;
        self.passband = pb.unwrap_or_else(|| m.default_passband());
        if let Some(p) = &self.pipeline {
            p.set_mode(m);
            p.set_passband(self.passband.0, self.passband.1);
        }
        self.sync_duo_mode(now);
    }

    pub(crate) fn set_passband(&mut self, pb: (f32, f32), now: RadioNow<'_>) {
        self.passband = pb;
        if let Some(p) = &self.pipeline {
            p.set_passband(pb.0, pb.1);
        }
        self.sync_duo_mode(now);
    }

    fn sync_duo_mode(&mut self, now: RadioNow<'_>) {
        if self.running != IqChoice::Elad || self.rate > DUO_AUDIO_MAX_RATE || now.transmitting {
            return;
        }
        let Some((link, r)) = now.cat() else { return };
        let width = (self.passband.1 - self.passband.0).abs().round() as u32;
        if self.duo_sent == Some((self.mode, width)) {
            return;
        }
        let proto = r.model.protocol();
        let name = rig_mode(self.mode);
        let mut frames = Vec::new();
        if r.mode != Some(name)
            && let Some(f) = proto.set_mode(name)
        {
            frames.push(f);
        }
        frames.extend(proto.set_filter(name, width));
        link.set(frames);
        self.duo_sent = Some((self.mode, width));
    }

    /// Act on what the operator did on the scope.
    pub(crate) fn apply_scope(&mut self, out: ScopeOut, s: &mut RxSettings, now: RadioNow<'_>) {
        if let Some(p) = &self.pipeline {
            for c in out.commands {
                p.send(c);
            }
        }
        if let Some(m) = out.mode {
            self.set_mode(m, out.passband, now);
        } else if let Some(pb) = out.passband {
            self.set_passband(pb, now);
        }
        if let Some(hz) = out.tune_to {
            self.tune(hz, now.transmitting);
        }
        if let Some(c) = out.recentre {
            self.recentre(c, now);
        }
        if let Some(rate) = out.sample_rate
            && rate != s.rate
        {
            s.rate = rate;
            self.restart = true;
        }
    }
}

/// How long the receiver must stay put before VFO B is moved to it, so a
/// drag doesn't send a frame a frame.
const SPLIT_SETTLE: Duration = Duration::from_millis(300);
/// How long before the same change is asked for again, if the radio's
/// read-back hasn't shown it.
const SPLIT_RETRY: Duration = Duration::from_millis(1500);

/// Transmit where you listen (decision 22): what the panel last asked of
/// VFO B and split.
#[derive(Default)]
pub(crate) struct SplitState {
    /// The panel put the radio in split (and so may take it out).
    made: bool,
    last: Option<(Vec<Frame>, Instant)>,
}

impl SplitState {
    /// The frames that make the radio transmit on the receiver: split on and
    /// VFO B there while the receiver is off VFO A; split off once they
    /// coincide, or the toggle is turned off, if the panel put it on. Nothing
    /// while the radio transmits (the link refuses it then too, #5).
    pub(crate) fn frames(
        &mut self,
        proto: &dyn Protocol,
        r: &RigStatus,
        rx_hz: f64,
        on: bool,
    ) -> Vec<Frame> {
        if !r.connected || r.transmitting() {
            return Vec::new();
        }
        let Some(dial) = r.freq_hz else { return Vec::new() };
        let rx = rx_hz.round().max(0.0) as u64;
        let mut want = Vec::new();
        if on && rx != dial {
            if r.freq_b_hz != Some(rx) {
                want.extend(proto.set_freq(Vfo::B, rx));
            }
            if r.split != Some(true) {
                want.extend(proto.set_split(true));
            }
            self.made = true;
        } else if self.made {
            if r.split != Some(false) {
                want.extend(proto.set_split(false));
            } else {
                self.made = false;
            }
        }
        if want.is_empty() {
            self.last = None;
            return want;
        }
        if self.last.as_ref().is_some_and(|(f, t)| *f == want && t.elapsed() < SPLIT_RETRY) {
            return Vec::new();
        }
        self.last = Some((want.clone(), Instant::now()));
        want
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duo(fa: u64, fb: Option<u64>, split: Option<bool>) -> RigStatus {
        RigStatus {
            connected: true,
            freq_hz: Some(fa),
            freq_b_hz: fb,
            split,
            ..RigStatus::default()
        }
    }

    fn text(f: &[Frame]) -> Vec<String> {
        f.iter().map(|f| String::from_utf8_lossy(&f.bytes).into_owned()).collect()
    }

    #[test]
    fn the_radio_transmits_where_the_receiver_is() {
        let proto = rigcat::RigModel::FdmDuo.protocol();
        let mut s = SplitState::default();
        // Off the dial: VFO B to the receiver, then split.
        let r = duo(14_200_000, Some(14_200_000), Some(false));
        let f = s.frames(&*proto, &r, 14_212_500.4, true);
        assert_eq!(text(&f), ["FB00014212500;", "SP1;"]);
        // Not asked again before the radio reads it back.
        assert!(s.frames(&*proto, &r, 14_212_500.0, true).is_empty());
        // Once it has, nothing more.
        let r = duo(14_200_000, Some(14_212_500), Some(true));
        assert!(s.frames(&*proto, &r, 14_212_500.0, true).is_empty());
        // Back on the dial: split off.
        let f = s.frames(&*proto, &r, 14_200_000.0, true);
        assert_eq!(text(&f), ["SP0;"]);
        let r = duo(14_200_000, Some(14_212_500), Some(false));
        assert!(s.frames(&*proto, &r, 14_200_000.0, true).is_empty());
    }

    #[test]
    fn split_is_never_touched_while_transmitting_and_left_alone_when_off() {
        let proto = rigcat::RigModel::FdmDuo.protocol();
        let mut s = SplitState::default();
        let mut r = duo(14_200_000, None, Some(false));
        r.tx_state = Some(1);
        assert!(s.frames(&*proto, &r, 14_212_500.0, true).is_empty());
        // With the toggle off the panel leaves an operator's own split be.
        let r = duo(14_200_000, Some(14_250_000), Some(true));
        assert!(s.frames(&*proto, &r, 14_212_500.0, false).is_empty());
        // But takes out a split it made itself.
        let r0 = duo(14_200_000, Some(14_200_000), Some(false));
        assert_eq!(s.frames(&*proto, &r0, 14_212_500.0, true).len(), 2);
        let f = s.frames(&*proto, &r, 14_212_500.0, false);
        assert_eq!(text(&f), ["SP0;"]);
    }

    #[test]
    fn taps_reach_every_sink_with_a_utc_stamp() {
        let t = Taps::default();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        t.set_audio("a", Some(Box::new(move |a, utc| g.lock().unwrap().push((a.len(), utc)))));
        t.feed_audio(&[0.0; 480]);
        t.set_audio("a", None);
        t.feed_audio(&[0.0; 480]);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 480);
        assert!((utc_now() - got[0].1 - 0.01).abs() < 1.0);
    }

    #[test]
    fn modes_go_both_ways() {
        for m in Mode::ALL {
            assert_eq!(sdr_mode(rig_mode(m)), Some(m));
            assert_eq!(mode_of(m.label()), m);
        }
    }
}
