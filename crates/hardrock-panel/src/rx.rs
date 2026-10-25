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
use sdr::{AudioOut, Command, Mode, Pipeline, PipelineConfig, SliceSpec, Snapshot};
use serde::{Deserialize, Serialize};

use crate::scope::{self, Scope, ScopeOut};
use crate::slices::{self, SliceSettings};

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
    /// How the receiver decimates, filters and levels (decisions 38, 39).
    pub dsp: DspSettings,
    /// The slices besides A (decision 66).
    pub slices: Vec<SliceSettings>,
}

/// The receiver's DSP choices (decision 38), as SETTINGS → Receiver keeps
/// them: the decimator, the passband's shape, the listening AGC and the
/// decoders' AGC target.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DspSettings {
    pub decimator: Decimator,
    pub shape: Shape,
    pub agc_attack_ms: f32,
    pub agc_decay_ms: f32,
    pub agc_hang_ms: f32,
    pub agc_target_dbfs: f32,
    pub decoder_target_dbfs: f32,
}

impl Default for DspSettings {
    fn default() -> Self {
        let l = sdr::AgcParams::listening(sdr::AgcMode::Med);
        DspSettings {
            decimator: Decimator::CicFir,
            shape: Shape::Sharp,
            agc_attack_ms: l.attack_ms,
            agc_decay_ms: l.decay_ms,
            agc_hang_ms: l.hang_ms,
            agc_target_dbfs: l.target_dbfs,
            decoder_target_dbfs: sdr::AgcParams::DECODER.target_dbfs,
        }
    }
}

/// [`sdr::DecimatorKind`], as kept between runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Decimator {
    HalfBand,
    #[default]
    #[serde(other)]
    CicFir,
}

/// [`sdr::FilterShape`], as kept between runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Shape {
    Soft,
    BrickWall,
    #[default]
    #[serde(other)]
    Sharp,
}

impl Decimator {
    pub(crate) const ALL: [Decimator; 2] = [Decimator::CicFir, Decimator::HalfBand];

    pub(crate) fn kind(self) -> sdr::DecimatorKind {
        match self {
            Decimator::HalfBand => sdr::DecimatorKind::HalfBand,
            Decimator::CicFir => sdr::DecimatorKind::CicFir,
        }
    }
}

impl Shape {
    pub(crate) const ALL: [Shape; 3] = [Shape::Soft, Shape::Sharp, Shape::BrickWall];

    pub(crate) fn kind(self) -> sdr::FilterShape {
        match self {
            Shape::Soft => sdr::FilterShape::Soft,
            Shape::Sharp => sdr::FilterShape::Sharp,
            Shape::BrickWall => sdr::FilterShape::BrickWall,
        }
    }

    /// The audio delay the shape costs, ms, as measured (decision 38).
    pub(crate) fn delay_ms(self) -> u32 {
        match self {
            Shape::Soft => 9,
            Shape::Sharp => 24,
            Shape::BrickWall => 64,
        }
    }
}

impl DspSettings {
    /// The listening AGC's parameters.
    pub(crate) fn agc(&self) -> sdr::AgcParams {
        sdr::AgcParams {
            attack_ms: self.agc_attack_ms,
            decay_ms: self.agc_decay_ms,
            hang_ms: self.agc_hang_ms,
            target_dbfs: self.agc_target_dbfs,
            ..sdr::AgcParams::listening(sdr::AgcMode::Med)
        }
    }

    /// The decoders' AGC: its slow preset, at the chosen target.
    pub(crate) fn decoder_agc(&self) -> sdr::AgcParams {
        sdr::AgcParams { target_dbfs: self.decoder_target_dbfs, ..sdr::AgcParams::DECODER }
    }

    /// The commands that put these on a running pipeline.
    pub(crate) fn commands(&self) -> [Command; 4] {
        [
            Command::SetDecimator(self.decimator.kind()),
            Command::SetFilterShape(self.shape.kind()),
            Command::SetAgcParams(self.agc()),
            Command::SetDecoderAgc(self.decoder_agc()),
        ]
    }
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
            dsp: DspSettings::default(),
            slices: Vec::new(),
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
/// The demo band: the `sdr` crate's made-up stations, and FT8 (decision 54)
/// and FT4 for DECODE and the skimmer's labels to decode with no radio; and
/// a few CW, PSK31 and RTTY stations for the keyboard skimmer (decision 56).
/// They are written for 20 m and moved to whichever band the span is on
/// (AJ3G: "we should be able to see demo on all bands"), each mode to that
/// band's own dial; a mode with none there is left out.
struct DemoBand {
    band: sdr::SyntheticSource,
    ft8: decoders::demo::DemoFt8,
    ft4: decoders::demo::DemoFt8,
    /// JT9 and JTTY, made by their own transmitters.
    audio: Vec<decoders::demo::DemoAudio>,
    keys: decoders::demo::DemoKeys,
}

/// How far the demo's 20 m stations of mode `name` (`at` on 20 m) move for
/// the band nearest `centre`: to that band's dial of the mode, or for CW the
/// same distance up from the band's foot. `None`: no dial of it there.
fn demo_shift(name: &str, at: f64, centre: f64) -> Option<f64> {
    use crate::scope::{bandplan, digimodes};
    let band = bandplan::nearest_band(centre);
    if name == "CW" {
        return Some(band.lo - 14_000_000.0);
    }
    let dial = digimodes::digi(name)?
        .dials
        .iter()
        .map(|&d| d as f64)
        .find(|&d| d >= band.lo && d <= band.hi)?;
    Some(dial - at)
}

impl sdr::IqSource for DemoBand {
    fn sample_rate(&self) -> f64 {
        self.band.sample_rate()
    }
    fn center_hz(&self) -> f64 {
        self.band.center_hz()
    }
    fn set_center_hz(&mut self, hz: f64) {
        self.band.set_center_hz(hz);
    }
    fn read(&mut self, out: &mut [f32]) -> usize {
        let n = self.band.read(out);
        if n > 0 {
            let rate = self.band.sample_rate();
            // Paced to real time: the block's last sample is now.
            let utc = utc_now() - (n / 2) as f64 / rate;
            let c = self.band.center_hz();
            use decoders::demo::{DEMO_DIAL_HZ, DEMO_FT4_DIAL_HZ};
            if let Some(d) = demo_shift("FT8", DEMO_DIAL_HZ, c) {
                self.ft8.add(&mut out[..n], rate, c - d, utc);
            }
            if let Some(d) = demo_shift("FT4", DEMO_FT4_DIAL_HZ, c) {
                self.ft4.add(&mut out[..n], rate, c - d, utc);
            }
            for a in &self.audio {
                if let Some(d) = demo_shift(a.name, a.dial_hz, c) {
                    a.add(&mut out[..n], rate, c, utc, d);
                }
            }
            self.keys.add_moved(&mut out[..n], rate, c, utc, |m| match m {
                decoders::keyskim::KeyMode::Cw => demo_shift("CW", 0.0, c),
                decoders::keyskim::KeyMode::Psk => demo_shift("PSK31", 14_070_000.0, c),
                decoders::keyskim::KeyMode::Rtty => demo_shift("RTTY", 14_083_000.0, c),
            });
        }
        n
    }
    fn label(&self) -> String {
        self.band.label()
    }
}

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
    /// The radio's volume and the panel's kept as one.
    vol: VolSync,
    /// Restart at the scope's new SPAN.
    pub restart: bool,
    /// The DSP choices the pipeline was started with or last sent.
    dsp: DspSettings,
    /// The samples the pipeline had read, and when that last changed; when
    /// the radio last transmitted; and when the stream watchdog last
    /// reopened the ELAD.
    heard: (u64, Instant),
    tx_at: Option<Instant>,
    reopened_at: Option<Instant>,
    /// What the stream watchdog did, for the scope window.
    pub watchdog: Option<String>,
    /// The watchdog lost the stream and hasn't opened it again yet: a
    /// reopen that fails is tried again on its timer, whatever the error.
    lost: bool,
    /// The I/Q's real rate, measured: the pipeline's sample count when the
    /// window began, and the rate the last window found when it disagreed.
    rate_probe: Option<(Instant, u64)>,
    rate_seen: Option<u32>,
    /// SPAN changed to the rate the DUO is really sending, and why.
    pub rate_note: Option<String>,
    /// The slices last given the pipeline, and each one's sound output (or
    /// why it couldn't be opened) by id, with the device asked for.
    slices_sent: Option<Vec<SliceSpec>>,
    slice_audio: Vec<(u32, Option<String>, Result<AudioOut, String>)>,
    /// When the sound outputs were last checked for one that went away or
    /// came back.
    audio_checked: Option<Instant>,
}

/// How often [`Rx::recheck_audio`] looks.
const AUDIO_RECHECK: Duration = Duration::from_secs(2);

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
            vol: VolSync::default(),
            restart: false,
            dsp: s.dsp,
            heard: (0, Instant::now()),
            tx_at: None,
            reopened_at: None,
            watchdog: None,
            lost: false,
            rate_probe: None,
            rate_seen: None,
            rate_note: None,
            slices_sent: None,
            slice_audio: Vec::new(),
            audio_checked: None,
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
            dsp: s.dsp,
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

    /// The demo's filler signals, where they are now (the radio's own Hz),
    /// with their labels: AJ3G, of the pulsing voice in the middle, "May
    /// want to label that as SSB FAKE". Empty unless the demo runs.
    pub(crate) fn demo_fakes(&self) -> Vec<(f64, &'static str)> {
        if self.running != IqChoice::Demo || self.pipeline.is_none() {
            return Vec::new();
        }
        let c = self.centre();
        sdr::DEMO_SIGNALS
            .iter()
            .map(|&(off, _, kind)| {
                let (label, mid) = kind.fake_label();
                (c + off + mid, label)
            })
            .collect()
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

    /// The I/Q rate last asked for, whether or not it runs yet.
    pub(crate) fn asked_rate(&self) -> u32 {
        self.rate
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
        self.slices_sent = None;
        self.slice_audio.clear();
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
                let src = DemoBand {
                    band: sdr::SyntheticSource::demo(s.rate as f64, centre),
                    ft8: decoders::demo::DemoFt8::new(),
                    ft4: decoders::demo::DemoFt8::ft4(),
                    audio: vec![
                        decoders::demo::DemoAudio::jt9(),
                        decoders::demo::DemoAudio::jtty(),
                    ],
                    keys: decoders::demo::DemoKeys::new(),
                };
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
            agc_params: Some(self.dsp.agc()),
            decoder_agc: self.dsp.decoder_agc(),
            decimator: self.dsp.decimator.kind(),
            filter_shape: self.dsp.shape.kind(),
            ..Default::default()
        };
        let p = Pipeline::spawn(src, cfg);
        let t = self.taps.clone();
        p.send(Command::SetIqTap(Some(Box::new(move |iq, c, r| t.feed_iq(iq, c, r)))));
        let t = self.taps.clone();
        p.send(Command::SetAudioTap(Some(Box::new(move |a| t.feed_audio(a)))));
        self.offset_sent = self.rx_hz - self.centre;
        // The sound card is opened again for the new pipeline's feed, and the
        // slices started on it.
        self.audio = None;
        self.audio_dev = None;
        self.slices_sent = None;
        self.slice_audio.clear();
        self.pipeline = Some(p);
        self.heard = (0, Instant::now());
        self.rate_probe = None;
        self.rate_seen = None;
    }

    /// Once a frame: finish opening the ELAD, keep the sound output, the
    /// offset, the split and the DUO's mode in step.
    pub(crate) fn frame(&mut self, s: &mut RxSettings, scope: &Scope, now: RadioNow<'_>) {
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #10): the DUO's wide
        // image is never loaded while it transmits; the driver checks this
        // between every request of the load.
        elad_iq::set_transmitting(now.transmitting);
        elad_iq::set_split(now.cat().is_some_and(|(_, r)| r.split == Some(true)));
        if let Some((rx, centre)) = &self.opening {
            let centre = *centre;
            match rx.try_recv() {
                Ok(Ok(h)) => {
                    self.opening = None;
                    if self.lost {
                        self.lost = false;
                        self.watchdog = Some(format!(
                            "The ELAD's stream was lost and opened again at {}.",
                            clock_now()
                        ));
                    }
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
        if now.transmitting {
            self.tx_at = Some(Instant::now());
        }
        // An ELAD that has gone from USB (unplugged, or dropped off the bus,
        // as RF on the USB lead during a transmission can make it), or whose
        // lost stream wouldn't open again: looked for again on the
        // watchdog's timer, never while transmitting (#10). A stream that
        // ended and failed to reopen stayed down until the panel was
        // restarted (AJ3G, 25 Oct 2026). So did one Windows refused to open
        // (error 5) after a SPAN change, the old handle not yet let go.
        if self.running == IqChoice::Elad
            && self.pipeline.is_none()
            && self.opening.is_none()
            && !now.transmitting
            && self.error.as_deref().is_some_and(|e| {
                e.contains("no ELAD")
                    || e.to_ascii_lowercase().contains("cannot open")
                    || (self.lost && !e.starts_with("Waiting"))
            })
            && self.reopened_at.is_none_or(|t| t.elapsed() >= REOPEN_EVERY)
        {
            if self.lost {
                self.watchdog = Some(format!(
                    "The ELAD's stream was lost; trying to open it every 20 s, last at {}. \
                     TRY AGAIN tries now. If it keeps failing, unplug the radio's USB lead \
                     and plug it back in.",
                    clock_now()
                ));
            }
            self.start(s, scope, now);
            self.reopened_at = Some(Instant::now());
        }
        if let Some(e) = &mut self.error
            && e.contains("no ELAD")
            && !e.contains("transmission")
            && self.tx_at.is_some_and(|t| t.elapsed() < AFTER_TX_DROP)
        {
            e.push_str(
                ". It went from USB around a transmission: RF on the USB lead is the usual \
                 cause (ferrites or a USB isolator). Replug it; the panel looks again every 20 s.",
            );
        }
        self.watch_stream(s, scope, now);
        self.match_rate(s, now);
        self.sync_volume(s, now);
        self.recheck_audio(s);
        self.keep_audio(s);
        self.keep_slices(s);
        let Some(p) = &self.pipeline else { return };
        // In split the DUO's USB dial read-back is VFO B, so the stream stops
        // following it (`elad_iq::set_split`) and VFO A over CAT says where
        // the I/Q is: a band key or the knob moves it, and the I/Q centre is
        // tuned there over USB. SAFETY INTERLOCK (docs/safety-interlocks.md,
        // #10): not while transmitting, nor just after (`dial_settled`).
        let dial_settled = self.tx_at.is_none_or(|t| t.elapsed() >= DIAL_SETTLE);
        if self.running == IqChoice::Elad
            && !now.transmitting
            && dial_settled
            && let Some((_, r)) = now.cat()
            && r.split == Some(true)
            && let Some(a) = r.freq_hz.map(|hz| hz as f64)
            && a > 0.0
            && (a - self.centre).abs() >= 1.0
            && (a - p.lock().center_hz).abs() >= 1.0
        {
            p.send(Command::SetCenter(a));
            let asked = self.asked_centre.is_some_and(|(c, _)| (c - a).abs() < 1.0);
            elad_iq::note_retune(format!(
                "I/Q centre to {} over USB, VFO A read over CAT in split ({})",
                mhz(a),
                if asked { "as the panel asked" } else { "the dial moved on the radio" }
            ));
        }
        let snap_centre = p.lock().center_hz;
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #9): while the radio
        // transmits, and for a moment after, the dial it reads back is not
        // where it listens (in split it can be VFO B, the transmit
        // frequency): the receiver, and so VFO B with it, is not moved by it. Whatever it says once the over is done
        // is taken as a turn of the knob would be.
        if snap_centre > 0.0 && snap_centre != self.centre && dial_settled {
            let asked = self.asked_centre.is_some_and(|(c, _)| (c - snap_centre).abs() < 1.0);
            if self.running == IqChoice::Elad {
                let why = if asked { "as the panel asked" } else { "the dial moved on the radio" };
                elad_iq::note_retune(format!("I/Q centre now {} ({why})", mhz(snap_centre)));
            }
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
        if self.running == IqChoice::Elad {
            let cat = now.rig.filter(|r| r.connected).and_then(|r| r.freq_hz);
            elad_iq::note_dial(dial_line(cat, self.centre, self.rx_hz));
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

    /// The stream watchdog: an ELAD stream that has delivered nothing for
    /// [`STREAM_SILENT`] while the radio was not transmitting (or whose
    /// thread has ended) is opened again, as TRY AGAIN would, at most once
    /// in [`REOPEN_EVERY`]. Not while transmitting: opening it tunes the
    /// DUO over USB (interlock #10, checked again in `start`).
    fn watch_stream(&mut self, s: &RxSettings, scope: &Scope, now: RadioNow<'_>) {
        let Some(p) = &self.pipeline else { return };
        let (samples, alive) = {
            let g = p.lock();
            (g.samples, g.alive)
        };
        if samples != self.heard.0 {
            self.heard = (samples, Instant::now());
        }
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #10): reopening tunes
        // the DUO over USB, so never while it transmits (and `start` checks
        // again); the silence is counted from the end of the over.
        if self.running != IqChoice::Elad || now.transmitting {
            return;
        }
        let quiet = self.heard.1.elapsed().min(self.tx_at.map_or(Duration::MAX, |t| t.elapsed()));
        let due = self.reopened_at.is_none_or(|t| t.elapsed() >= REOPEN_EVERY);
        if (!alive || quiet >= STREAM_SILENT) && due {
            let why = if alive { "stopped sending I/Q" } else { "stream ended" };
            let msg = format!("The ELAD {why} at {}; opening it again.", clock_now());
            self.lost = true;
            self.start(s, scope, now);
            self.reopened_at = Some(Instant::now());
            self.heard = (0, Instant::now());
            self.watchdog = Some(msg);
        }
    }

    /// The FDM-DUO's I/Q rate is set on the radio, and nothing over USB
    /// changes it: SPAN only says how the stream is read. Read at 384 kHz,
    /// a DUO left at 192 kHz gave the receiver half the samples it needed,
    /// and its audio stuttered with every block (AJ3G, 25 Oct 2026). So the
    /// rate arriving is measured over two-second windows clear of any
    /// transmission, and when two in a row agree on another of the ELAD's
    /// rates, SPAN is set to it and the scope says so.
    fn match_rate(&mut self, s: &mut RxSettings, now: RadioNow<'_>) {
        let Some(p) = &self.pipeline else { return };
        let (samples, load) = {
            let g = p.lock();
            (g.samples, g.load)
        };
        let near_tx = self.tx_at.is_some_and(|t| t.elapsed() < RATE_WINDOW * 2);
        // A receiver that can't keep up reads fewer samples than arrive.
        if self.running != IqChoice::Elad || now.transmitting || near_tx || load > 0.8 {
            self.rate_probe = None;
            return;
        }
        let Some((t0, n0)) = self.rate_probe else {
            self.rate_probe = Some((Instant::now(), samples));
            return;
        };
        let dt = t0.elapsed();
        if dt < RATE_WINDOW {
            return;
        }
        self.rate_probe = Some((Instant::now(), samples));
        let measured = samples.saturating_sub(n0) as f64 / dt.as_secs_f64();
        let Some(r) = arriving_rate(measured, s.rate) else {
            self.rate_seen = None;
            return;
        };
        if self.rate_seen != Some(r) {
            self.rate_seen = Some(r);
            return;
        }
        self.rate_note = Some(format!(
            "The DUO is sending I/Q at {} kHz, and SPAN was {} kHz: SPAN set to {} kHz to \
             match. The DUO's I/Q rate is set on the radio; change it there for another span.",
            r / 1000,
            s.rate / 1000,
            r / 1000
        ));
        tracing::info!("I/Q arriving at {measured:.0} Hz, read as {}: SPAN set to {r}", s.rate);
        s.rate = r;
        self.restart = true;
    }

    /// One volume (AJ3G): the radio's VM (E1 on VOL, RADIO's VOLUME, the
    /// radio's own knob) and SCOPE's Sound slider follow each other, so the
    /// one heard, the radio's audio or the panel's, turns with either. Only
    /// while the panel plays into the ELAD's own USB audio: with another
    /// speaker picked, Sound and MUTE are that speaker's alone, and the
    /// radio's VM pulled the slider back (AJ3G, 24 Oct 2026).
    fn sync_volume(&mut self, s: &mut RxSettings, now: RadioNow<'_>) {
        if !sound_via_radio(s.audio_out.as_deref(), self.audio.as_ref().map(|a| a.device())) {
            // The radio's word wins again when it is picked again.
            self.vol = VolSync::default();
            return;
        }
        let Some((link, r)) = now.cat() else { return };
        let proto = r.model.protocol();
        let Some(c) = crate::radio::logic::find(&*proto, "VM") else { return };
        let radio = crate::radio::logic::readback(c, r).and_then(|v| v.trim().parse().ok());
        if let Some(to) = self.vol.step(radio, &mut s.volume, now.transmitting, Instant::now())
            && let Ok(f) = crate::radio::logic::control_frame(&*proto, c, &to.to_string(), r)
        {
            // Read back after; refused while transmitting (#5).
            link.set(vec![f]);
        }
    }

    /// Every two seconds, let go of a sound output that failed (turned off
    /// or unplugged) or couldn't be opened, and of "Default output" when
    /// Windows has made another device the default, so it is opened again
    /// next: a speaker turned on after the receiver started played nothing
    /// until the panel was restarted (AJ3G, 24 Oct 2026).
    fn recheck_audio(&mut self, s: &RxSettings) {
        if self.audio_checked.is_some_and(|t| t.elapsed() < AUDIO_RECHECK) {
            return;
        }
        self.audio_checked = Some(Instant::now());
        let default = sdr::default_output();
        let stale = |a: &AudioOut, want: Option<&str>| {
            a.failed() || (want.is_none() && default.as_deref().is_some_and(|d| d != a.device()))
        };
        let main_stale = match &self.audio {
            Some(a) => stale(a, s.audio_out.as_deref()),
            None => self.audio_error.is_some(),
        };
        if main_stale {
            self.audio_dev = None;
        }
        self.slice_audio.retain(|(_, dev, r)| r.as_ref().is_ok_and(|a| !stale(a, dev.as_deref())));
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

    /// Run the slices inside the span (decision 66), and keep each one's
    /// sound output, volume and mute.
    fn keep_slices(&mut self, s: &RxSettings) {
        let Some(p) = &self.pipeline else { return };
        let want = slices::specs(&s.slices, self.centre, p.lock().sample_rate);
        if self.slices_sent.as_ref() != Some(&want) {
            p.send(Command::SetSlices(want.clone()));
            self.slices_sent = Some(want.clone());
        }
        // A slice not running, or whose output was changed, lets its sound
        // card go; one running with none opens it.
        self.slice_audio.retain(|(id, dev, _)| {
            want.iter().any(|w| w.id == *id)
                && s.slices.iter().any(|x| x.id == *id && x.audio_out == *dev)
        });
        for w in &want {
            let Some(sl) = s.slices.iter().find(|x| x.id == w.id) else { continue };
            if !self.slice_audio.iter().any(|(id, _, _)| *id == w.id) {
                let opened =
                    AudioOut::open(sl.audio_out.as_deref(), sl.volume).map(|(out, feed)| {
                        p.send(Command::SetSliceAudio(w.id, Some(feed)));
                        out
                    });
                self.slice_audio.push((w.id, sl.audio_out.clone(), opened));
            }
            if let Some((_, _, Ok(a))) = self.slice_audio.iter().find(|(id, _, _)| *id == w.id) {
                a.set_volume(sl.volume);
                a.set_muted(sl.muted);
            }
        }
    }

    /// Each running slice's level, dBm, by id.
    pub(crate) fn slice_levels(&self) -> Vec<(u32, f32)> {
        self.pipeline.as_ref().map(|p| p.lock().slice_levels.clone()).unwrap_or_default()
    }

    /// Why a slice's sound output couldn't be opened.
    pub(crate) fn slice_audio_error(&self, id: u32) -> Option<&str> {
        self.slice_audio.iter().find_map(|(i, _, r)| match r {
            Err(e) if *i == id => Some(e.as_str()),
            _ => None,
        })
    }

    /// The sound output in use, and its underruns and dropped samples.
    pub(crate) fn audio_state(&self) -> Option<(String, u64, u64)> {
        self.audio.as_ref().map(|a| {
            let (u, d) = a.stats();
            (a.device().to_string(), u, d)
        })
    }

    /// Put SETTINGS → Receiver's DSP choices on the receiver, at once if it
    /// runs and at its next start either way.
    pub(crate) fn set_dsp(&mut self, d: DspSettings) {
        if d == self.dsp {
            return;
        }
        self.dsp = d;
        if let Some(p) = &self.pipeline {
            for c in d.commands() {
                p.send(c);
            }
        }
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
                        elad_iq::note_retune(format!("recentre to {} over CAT (VFO A)", mhz(hz)));
                    }
                } else {
                    // SAFETY INTERLOCK (docs/safety-interlocks.md, #10): with
                    // no CAT link the centre is moved over USB, which retunes
                    // the DUO outside interlock #5, so never while it
                    // transmits (checked above).
                    p.send(Command::SetCenter(hz));
                    elad_iq::note_retune(format!("recentre to {} over USB (no CAT)", mhz(hz)));
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
        let filter = duo_filter(&proto.filters(name), self.mode, self.passband);
        frames.extend(proto.set_filter(name, filter.unwrap_or(width)));
        link.set(frames);
        self.duo_sent = Some((self.mode, width));
    }

    /// Swap slice `id` with A (decision 66): A goes to the slice's dial,
    /// mode and passband, and the slice to A's. SAFETY INTERLOCK
    /// (docs/safety-interlocks.md, #9): A is where the radio transmits in
    /// split, so not while transmitting.
    pub(crate) fn swap_slice(&mut self, s: &mut RxSettings, id: u32, now: RadioNow<'_>) {
        if now.transmitting {
            return;
        }
        let Some(sl) = s.slices.iter_mut().find(|x| x.id == id) else { return };
        let (hz, m, pb) = (sl.rx_hz, sl.mode(), sl.passband);
        sl.rx_hz = self.rx_hz;
        sl.mode = self.mode.label().into();
        sl.passband = self.passband;
        self.tune(hz, now.transmitting);
        self.set_mode(m, Some(pb), now);
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
            self.rate_note = None;
            self.restart = true;
        }
    }
}

/// The radio's filter for the receiver's passband `pb` in mode `m`, among
/// `filters`: as a data mode's (`radio::logic::digi_filter`), so a WSPR or
/// PSK31 passband gets the narrow data filter it fits in. `None` for CW,
/// whose filters are centred on its pitch: the nearest width then.
pub(crate) fn duo_filter(filters: &[u32], m: Mode, pb: (f32, f32)) -> Option<u32> {
    let (a, b) = (pb.0.abs().round() as u32, pb.1.abs().round() as u32);
    let (lo, hi) = match m {
        Mode::Am | Mode::Fm => (0, a + b),
        // CW's own filters are centred on its pitch: the nearest width.
        Mode::Cw | Mode::Cwr => return None,
        _ => (a.min(b), a.max(b)),
    };
    crate::radio::logic::digi_filter(filters, lo, hi)
}

/// How long the receiver must stay put before VFO B is moved to it, so a
/// drag doesn't send a frame a frame.
const SPLIT_SETTLE: Duration = Duration::from_millis(300);

/// How long after a transmission the radio's dial read-back is believed
/// again: four of the ELAD driver's reads.
const DIAL_SETTLE: Duration = Duration::from_millis(1000);

/// An ELAD stream silent this long, with the radio not transmitting, is
/// opened again. The DUO streams all the time it is on, at every rate.
const STREAM_SILENT: Duration = Duration::from_secs(3);

/// An ELAD lost from USB this soon after a transmission is said to have
/// gone around it.
const AFTER_TX_DROP: Duration = Duration::from_secs(120);

/// The window the I/Q's real rate is measured over (see `match_rate`).
const RATE_WINDOW: Duration = Duration::from_secs(2);

/// The ELAD rate a stream measured at `measured` samples a second is really
/// at, when it is not `reading`: the rates are octaves apart, so anything
/// within a quarter of `reading` is taken as right, and a stream that has all
/// but stopped is the watchdog's business.
fn arriving_rate(measured: f64, reading: u32) -> Option<u32> {
    if measured < 0.25 * elad_iq::ELAD_SAMPLE_RATES[0] as f64
        || (measured / reading as f64 - 1.0).abs() <= 0.25
    {
        return None;
    }
    let nearest = elad_iq::ELAD_SAMPLE_RATES
        .iter()
        .copied()
        .min_by(|a, b| (*a as f64 - measured).abs().total_cmp(&(*b as f64 - measured).abs()))?;
    (nearest != reading).then_some(nearest)
}

/// The watchdog reopens no more often than this.
const REOPEN_EVERY: Duration = Duration::from_secs(20);

/// UTC now as `HH:MM:SS`.
fn clock_now() -> String {
    let t = utc_now() as u64 % 86_400;
    format!("{:02}:{:02}:{:02} UTC", t / 3600, t / 60 % 60, t % 60)
}

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

/// A frequency for the ELAD log, MHz to the hertz.
fn mhz(hz: f64) -> String {
    format!("{:.6} MHz", hz / 1e6)
}

/// The ELAD log's line for where the radio is now: its dial over CAT (when
/// connected), the I/Q centre and the panel's receiver.
fn dial_line(cat: Option<u64>, centre: f64, rx: f64) -> String {
    let dial = cat.map_or_else(
        || "dial not read (no CAT)".to_string(),
        |f| format!("dial {} (CAT)", mhz(f as f64)),
    );
    format!("{dial}, I/Q centre {}, receiver {}", mhz(centre), mhz(rx))
}

/// Whether the panel's sound goes into the ELAD's own USB audio: the
/// output `picked`, or with "Default output" the device `playing`.
fn sound_via_radio(picked: Option<&str>, playing: Option<&str>) -> bool {
    picked.or(playing).is_some_and(|d| crate::audio::radio_device(&[d.to_string()]).is_some())
}

/// The radio's VM, 0 to 100, against the panel's volume, 0 to 1.
#[derive(Default)]
pub(crate) struct VolSync {
    /// Where the two last agreed.
    agreed: Option<(i32, f32)>,
    /// A VM sent, and when, while its read-back is awaited.
    sent: Option<(i32, Instant)>,
}

/// At most one VM for the radio this often while the slider is dragged.
const VOL_SEND_EVERY: Duration = Duration::from_millis(150);
/// How long a VM sent is waited for before the radio's word is taken.
const VOL_WAIT: Duration = Duration::from_millis(1500);

impl VolSync {
    /// Given the radio's VM read back and the panel's volume: the panel's
    /// takes a change of the radio's, and a change of the panel's is the VM
    /// returned to send. The radio's wins at the start.
    pub(crate) fn step(
        &mut self,
        radio: Option<i32>,
        panel: &mut f32,
        transmitting: bool,
        now: Instant,
    ) -> Option<i32> {
        let v = radio?.clamp(0, 100);
        let Some((vm, vol)) = self.agreed else {
            *panel = v as f32 / 100.0;
            self.agreed = Some((v, *panel));
            return None;
        };
        if (*panel - vol).abs() > 1e-4 {
            if transmitting || self.sent.is_some_and(|(_, t)| now - t < VOL_SEND_EVERY) {
                return None;
            }
            let to = (*panel * 100.0).round().clamp(0.0, 100.0) as i32;
            self.agreed = Some((to, *panel));
            if to == vm {
                return None;
            }
            self.sent = Some((to, now));
            return Some(to);
        }
        if let Some((w, t)) = self.sent {
            if v != w && now - t < VOL_WAIT {
                return None;
            }
            self.sent = None;
        }
        if v != vm {
            *panel = v as f32 / 100.0;
            self.agreed = Some((v, *panel));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SWAP A puts A where the slice was and the slice where A was, and
    /// does nothing while transmitting (interlock #9).
    #[test]
    fn a_slice_swaps_with_a_but_not_while_transmitting() {
        let mut s = RxSettings::default();
        crate::slices::add(&mut s.slices, 14_095_600.0, Mode::Usb, (1400.0, 1600.0), None);
        let mut rx = Rx::new(&s);
        rx.rx_hz = 14_074_000.0;
        let keyed = RadioNow { link: None, rig: None, transmitting: true };
        rx.swap_slice(&mut s, 1, keyed);
        assert_eq!((rx.rx_hz, s.slices[0].rx_hz), (14_074_000.0, 14_095_600.0));
        let idle = RadioNow { transmitting: false, ..keyed };
        rx.swap_slice(&mut s, 1, idle);
        assert_eq!((rx.rx_hz, rx.passband), (14_095_600.0, (1400.0, 1600.0)));
        assert_eq!((s.slices[0].rx_hz, s.slices[0].passband), (14_074_000.0, s.passband));
    }

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
    fn the_elad_log_says_where_the_radio_is_now() {
        let l = dial_line(Some(7_038_600), 7_038_600.0, 7_040_100.0);
        assert_eq!(l, "dial 7.038600 MHz (CAT), I/Q centre 7.038600 MHz, receiver 7.040100 MHz");
        assert!(dial_line(None, 14.2e6, 14.2e6).starts_with("dial not read (no CAT)"));
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

    #[test]
    fn the_duos_filter_is_chosen_as_a_data_modes_is() {
        let f = rigcat::RigModel::FdmDuo.protocol().filters("USB");
        // WSPR's 1400 to 1600 Hz fits a narrow data filter; FT8 needs 3 kHz.
        let wspr = duo_filter(&f, Mode::Usb, (1400.0, 1600.0)).unwrap();
        assert!(wspr <= 1000, "{wspr}");
        let ft8 = duo_filter(&f, Mode::Usb, (200.0, 3000.0)).unwrap();
        assert!(ft8 >= 3000, "{ft8}");
        // LSB reads the same from below.
        assert_eq!(duo_filter(&f, Mode::Lsb, (-3000.0, -200.0)), Some(ft8));
        assert_eq!(duo_filter(&f, Mode::Cw, (350.0, 850.0)), None);
    }

    #[test]
    fn the_dsp_choices_default_to_decision_38s_and_are_kept() {
        let d = DspSettings::default();
        assert_eq!(d.decimator.kind(), sdr::DecimatorKind::CicFir);
        assert_eq!(d.shape.kind(), sdr::FilterShape::Sharp);
        assert_eq!(d.decoder_agc(), sdr::AgcParams::DECODER);
        assert_eq!(d.agc(), sdr::AgcParams::listening(sdr::AgcMode::Med));
        assert_eq!(Shape::ALL.map(Shape::delay_ms), [9, 24, 64]);
        let changed = DspSettings { shape: Shape::BrickWall, agc_hang_ms: 900.0, ..d };
        let back: DspSettings =
            serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
        assert_eq!(back, changed);
        // Older settings files, and a shape this build never heard of.
        let old: RxSettings = serde_json::from_str("{\"rate\": 192000}").unwrap();
        assert_eq!(old.dsp, d);
        let odd: DspSettings = serde_json::from_str("{\"shape\": \"Gaussian\"}").unwrap();
        assert_eq!(odd.shape, Shape::Sharp);
    }

    fn now(transmitting: bool) -> RadioNow<'static> {
        RadioNow { link: None, rig: None, transmitting }
    }

    fn wait(mut what: impl FnMut() -> bool) {
        let t = Instant::now();
        while !what() {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The dial read back while the radio transmits (VFO B in split, or
    /// nonsense) does not move the receiver; a real move of the dial is
    /// taken once the over is done and the read-back has settled.
    #[test]
    fn the_receiver_is_not_moved_by_the_dial_during_an_over() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings::default();
        let mut rx = Rx::new(&s);
        rx.centre = 14.2e6;
        rx.rx_hz = 14.21e6;
        let src = sdr::SyntheticSource::new(192_000.0, 14.2e6, -80.0, vec![]).paced();
        rx.spawn(Box::new(src), &scope);
        rx.frame(&mut s, &scope, now(false));
        // Keyed, and the I/Q's centre moves.
        rx.frame(&mut s, &scope, now(true));
        rx.pipeline.as_ref().unwrap().send(Command::SetCenter(14.25e6));
        wait(|| rx.snapshot().center_hz == 14.25e6);
        rx.frame(&mut s, &scope, now(true));
        assert_eq!((rx.rx_hz, rx.centre), (14.21e6, 14.2e6), "moved during the over");
        // Just unkeyed: the read-back is not trusted yet.
        rx.frame(&mut s, &scope, now(false));
        assert_eq!(rx.rx_hz, 14.21e6);
        std::thread::sleep(DIAL_SETTLE);
        rx.frame(&mut s, &scope, now(false));
        assert_eq!((rx.rx_hz, rx.centre), (14.26e6, 14.25e6), "the knob was turned");
    }

    /// An ELAD stream that goes quiet is opened again once the radio has
    /// stopped transmitting, and not during the over or just after it.
    #[test]
    fn a_stream_that_stays_silent_after_an_over_is_opened_again() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        // The reopen starts the demo, standing in for the ELAD.
        let mut s = RxSettings { source: IqChoice::Demo, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        let src = sdr::SyntheticSource::new(192_000.0, 14.2e6, -80.0, vec![]).paced();
        // Silent from 0.2 s on, as a DUO's I/Q might stop for an over and
        // not come back.
        let fx = sdr::TxEffects { gap: true, ..Default::default() };
        rx.spawn(Box::new(sdr::TxSim::new(src, 0.2, 1e9, fx)), &scope);
        rx.running = IqChoice::Elad;
        let t = Instant::now();
        while t.elapsed() < STREAM_SILENT + Duration::from_millis(500) {
            rx.frame(&mut s, &scope, now(true));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(rx.watchdog.is_none(), "reopened while transmitting");
        // Unkeyed: given STREAM_SILENT from the end of the over.
        rx.frame(&mut s, &scope, now(false));
        std::thread::sleep(STREAM_SILENT / 2);
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.watchdog.is_none(), "reopened too soon after the over");
        std::thread::sleep(STREAM_SILENT / 2 + Duration::from_millis(100));
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.watchdog.as_deref().is_some_and(|w| w.contains("stopped sending")));
        assert_eq!(rx.source(), IqChoice::Demo);
        let before = rx.snapshot().samples;
        wait(|| {
            rx.frame(&mut s, &scope, now(false));
            rx.snapshot().samples > before + 1000
        });
    }

    #[test]
    fn an_elad_gone_from_usb_after_an_over_is_said_so_and_looked_for_again() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        // The retry starts the demo, standing in for the ELAD coming back.
        let mut s = RxSettings { source: IqChoice::Demo, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.running = IqChoice::Elad;
        rx.error = Some("no ELAD device found on USB".into());
        rx.tx_at = Some(Instant::now());
        rx.reopened_at = Some(Instant::now());
        rx.frame(&mut s, &scope, now(false));
        let e = rx.error.clone().unwrap_or_default();
        assert!(e.contains("around a transmission"), "{e}");
        // Not looked for while transmitting, even when due.
        rx.reopened_at = Instant::now().checked_sub(REOPEN_EVERY);
        rx.frame(&mut s, &scope, now(true));
        assert!(rx.error.is_some() && rx.pipeline.is_none());
        // Due, and receiving: looked for again.
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.error.is_none());
        assert_eq!(rx.source(), IqChoice::Demo);
    }

    #[test]
    fn the_rate_the_duo_really_sends_is_found() {
        // Read as 384 kHz, arriving at 192: the DUO's own rate.
        assert_eq!(arriving_rate(191_996.0, 384_000), Some(192_000));
        assert_eq!(arriving_rate(770_000.0, 192_000), Some(768_000));
        // Right, or near enough.
        assert_eq!(arriving_rate(191_996.0, 192_000), None);
        assert_eq!(arriving_rate(330_000.0, 384_000), None);
        // All but stopped: the watchdog's business, not a rate.
        assert_eq!(arriving_rate(1_000.0, 384_000), None);
    }

    #[test]
    fn a_lost_stream_that_fails_to_reopen_is_tried_again() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        // The retry starts the demo, standing in for the ELAD coming back.
        let mut s = RxSettings { source: IqChoice::Demo, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.running = IqChoice::Elad;
        rx.lost = true;
        rx.error = Some("the ELAD device is busy".into());
        rx.reopened_at = Some(Instant::now());
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.pipeline.is_none(), "tried again before the timer");
        rx.reopened_at = Instant::now().checked_sub(REOPEN_EVERY);
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.error.is_none());
        assert_eq!(rx.source(), IqChoice::Demo);
        assert!(rx.watchdog.as_deref().is_some_and(|w| w.contains("every 20 s")));
    }

    /// Windows refusing the open after a SPAN change (AJ3G, 25 Oct): not a
    /// lost stream, but tried again on the same timer.
    #[test]
    fn an_open_windows_refused_is_tried_again() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings { source: IqChoice::Demo, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.running = IqChoice::Elad;
        rx.error = Some(
            "cannot open ELAD FDM-DUO at USBROOT(0)-1.4.1: failed to open device (error 5)".into(),
        );
        rx.reopened_at = Instant::now().checked_sub(REOPEN_EVERY);
        rx.frame(&mut s, &scope, now(false));
        assert!(rx.error.is_none());
        assert_eq!(rx.source(), IqChoice::Demo);
    }

    /// FT8 on the air, as I/Q in real time: each slot from 0.5 s, one
    /// station at `rf_hz`, in a little noise.
    struct Ft8Air {
        rate: f64,
        centre: f64,
        rf_hz: f64,
        tones: Vec<u8>,
        utc0: f64,
        t0: Instant,
        n: u64,
        phase: f64,
    }

    impl sdr::IqSource for Ft8Air {
        fn sample_rate(&self) -> f64 {
            self.rate
        }
        fn center_hz(&self) -> f64 {
            self.centre
        }
        fn read(&mut self, out: &mut [f32]) -> usize {
            // In bursts, late by up to 0.15 s, as USB and a busy machine
            // deliver it.
            let late = ((self.n / 19_200) * 2_654_435_761 % 150) as f64 / 1000.0;
            let due = ((self.t0.elapsed().as_secs_f64() - late).max(0.0) * self.rate) as u64;
            let due = due / 19_200 * 19_200;
            let k = ((due.saturating_sub(self.n)) as usize).min(out.len() / 2);
            for i in 0..k {
                let utc = self.utc0 + (self.n + i as u64) as f64 / self.rate;
                let into = utc.rem_euclid(15.0) - 0.5;
                let sym = (into / 0.16).floor();
                let (re, im) = if (0.0..79.0).contains(&sym) {
                    let f = self.rf_hz - self.centre + self.tones[sym as usize] as f64 * 6.25;
                    self.phase = (self.phase + std::f64::consts::TAU * f / self.rate)
                        % std::f64::consts::TAU;
                    (0.01 * self.phase.cos() as f32, 0.01 * self.phase.sin() as f32)
                } else {
                    (0.0, 0.0)
                };
                let j = (i as u32).wrapping_mul(2_654_435_761) >> 20;
                out[2 * i] = re + (j as f32 / 4096.0 - 0.5) * 1e-3;
                out[2 * i + 1] = im + ((j ^ 0x5a5) as f32 / 4096.0 - 0.5) * 1e-3;
            }
            self.n += k as u64;
            if k == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            2 * k
        }
        fn label(&self) -> String {
            "FT8 on the air".into()
        }
    }

    #[test]
    #[ignore = "real time, up to 35 s: cargo test -- --ignored ft8_on_the_air"]
    fn ft8_on_the_air_decodes_off_the_centre() {
        // As after the FT8 snap on 40 m from WSPR: the centre left at
        // 7.0386 MHz, the receiver on 7.074 MHz USB.
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings { source: IqChoice::Demo, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.centre = 7_038_600.0;
        rx.rx_hz = 7_074_000.0;
        rx.passband = (100.0, 3_000.0);
        let bits = mfsk_core::msg::wsjt77::pack77("CQ", "K1ABC", "FN42").unwrap();
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits).to_vec();
        let src = Ft8Air {
            rate: 192_000.0,
            centre: rx.centre,
            rf_hz: 7_075_500.0,
            tones,
            utc0: decoders::slot::utc_now(),
            t0: Instant::now(),
            n: 0,
            phase: 0.0,
        };
        rx.spawn(Box::new(src), &scope);
        rx.running = IqChoice::Demo;
        let ds = crate::decode::DecodeSettings {
            kind: crate::decode::Kind::Ft8,
            running: true,
            ..Default::default()
        };
        let st = crate::station::StationSettings::default();
        let at = crate::decode::Where { dial_hz: rx.rx_hz, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        let mut d = crate::decode::Decode::default();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(35) && d.rows.is_empty() {
            rx.frame(&mut s, &scope, now(false));
            d.frame(&ds, &st, &rx, at, None);
            std::thread::sleep(Duration::from_millis(20));
        }
        let texts: Vec<_> = d.rows.iter().map(|r| r.text.clone()).collect();
        assert!(texts.iter().any(|t| t == "CQ K1ABC FN42"), "{texts:?} {:?}", d.error);
    }

    /// The demo band end to end, as the panel runs it: FT8 decoded on
    /// `dial` and, with SKIM CW/PSK/RTTY, the keyboard stations read.
    fn demo_band_decodes(centre: f64, dial: f64, rate: u32) -> (Vec<String>, Vec<String>) {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings { source: IqChoice::Demo, rate, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.centre = centre;
        rx.rx_hz = dial;
        rx.passband = (100.0, 3_000.0);
        rx.start(&s, &scope, now(false));
        let ds = crate::decode::DecodeSettings {
            kind: crate::decode::Kind::Ft8,
            running: true,
            skim_keys: true,
            ..Default::default()
        };
        let st = crate::station::StationSettings::default();
        let at = crate::decode::Where { dial_hz: dial, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        let mut d = crate::decode::Decode::default();
        let mut keys = Vec::new();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(40) && (d.rows.is_empty() || keys.is_empty()) {
            rx.frame(&mut s, &scope, now(false));
            d.frame(&ds, &st, &rx, at, None);
            if let Some(f) = d.skim_feed(&ds) {
                keys = f.spots.iter().filter(|p| p.key.is_some()).map(|p| p.call.clone()).collect();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        (d.rows.iter().map(|r| r.text.clone()).collect(), keys)
    }

    #[test]
    #[ignore = "real time, up to 40 s: cargo test -- --ignored demo_band"]
    fn demo_band_on_20_m_decodes() {
        let (ft8, keys) = demo_band_decodes(14_050_000.0, 14_074_000.0, 192_000);
        eprintln!("FT8 {ft8:?}\nkeys {keys:?}");
        assert!(!ft8.is_empty() && !keys.is_empty());
    }

    #[test]
    #[ignore = "real time, up to 60 s: cargo test -- --ignored demo_band"]
    fn demo_band_followed_from_40_m_to_20_m_decodes() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings { source: IqChoice::Demo, rate: 192_000, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.centre = 7_050_000.0;
        rx.rx_hz = 7_074_000.0;
        rx.passband = (100.0, 3_000.0);
        rx.start(&s, &scope, now(false));
        let ds = crate::decode::DecodeSettings {
            kind: crate::decode::Kind::Ft8,
            running: true,
            ..Default::default()
        };
        let st = crate::station::StationSettings::default();
        let mut d = crate::decode::Decode::default();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            rx.frame(&mut s, &scope, now(false));
            let at = crate::decode::Where { dial_hz: rx.rx_hz, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
            d.frame(&ds, &st, &rx, at, None);
            std::thread::sleep(Duration::from_millis(20));
        }
        // To 20 m: the receiver, then the centre after it, as the scope asks.
        rx.tune(14_074_000.0, false);
        rx.recentre(14_074_000.0, now(false));
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(50)
            && !d.rows.iter().any(|r| r.rf_hz.is_some_and(|f| f > 14e6))
        {
            rx.frame(&mut s, &scope, now(false));
            let at = crate::decode::Where { dial_hz: rx.rx_hz, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
            d.frame(&ds, &st, &rx, at, None);
            std::thread::sleep(Duration::from_millis(20));
        }
        let rows: Vec<_> = d.rows.iter().map(|r| (r.rf_hz, r.text.clone())).collect();
        eprintln!("{rows:?} centre {} rx {}", rx.centre(), rx.rx_hz);
        assert!(d.rows.iter().any(|r| r.rf_hz.is_some_and(|f| f > 14e6)), "{rows:?}");
    }

    #[test]
    #[ignore = "real time, up to 40 s: cargo test -- --ignored demo_band"]
    fn demo_band_in_psk31_still_labels_ft8_and_ft4() {
        let scope = Scope::new(None, crate::scope::ScopeSettings::default());
        let mut s = RxSettings { source: IqChoice::Demo, rate: 192_000, ..RxSettings::default() };
        let mut rx = Rx::new(&s);
        rx.centre = 14_050_000.0;
        rx.rx_hz = 14_070_000.0;
        rx.passband = (100.0, 3_000.0);
        rx.start(&s, &scope, now(false));
        let ds = crate::decode::DecodeSettings {
            kind: crate::decode::Kind::Psk31,
            running: true,
            audio_hz: 1000.0,
            ..Default::default()
        };
        let st = crate::station::StationSettings::default();
        let at = crate::decode::Where { dial_hz: rx.rx_hz, lsb: false, xvtr_hz: 0.0, ppm: 0.0 };
        let mut d = crate::decode::Decode::default();
        let mut modes = std::collections::BTreeSet::new();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(40) && modes.len() < 2 {
            rx.frame(&mut s, &scope, now(false));
            d.frame(&ds, &st, &rx, at, None);
            if let Some(f) = d.skim_feed(&ds) {
                modes.extend(f.spots.iter().filter_map(|p| p.wsjt).map(|m| m.label()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(modes.into_iter().collect::<Vec<_>>(), ["FT4", "FT8"]);
    }

    #[test]
    #[ignore = "real time, up to 40 s: cargo test -- --ignored demo_band"]
    fn demo_band_on_40_m_decodes() {
        let (ft8, keys) = demo_band_decodes(7_050_000.0, 7_074_000.0, 192_000);
        eprintln!("FT8 {ft8:?}\nkeys {keys:?}");
        assert!(!ft8.is_empty() && !keys.is_empty());
    }

    #[test]
    fn only_the_elads_own_audio_ties_sound_to_the_radios_volume() {
        assert!(sound_via_radio(Some("FDM-DUO Audio"), None));
        assert!(sound_via_radio(None, Some("Speakers (ELAD FDM-DUO)")));
        assert!(!sound_via_radio(Some("Klipsch The Fives"), Some("FDM-DUO Audio")));
        assert!(!sound_via_radio(None, Some("Realtek Speakers")));
        assert!(!sound_via_radio(None, None));
    }

    #[test]
    fn the_radio_volume_and_the_panel_volume_follow_each_other() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut v = VolSync::default();
        let mut panel = 0.5;
        // Nothing read back yet: left alone.
        assert_eq!(v.step(None, &mut panel, false, t0), None);
        assert_eq!(panel, 0.5);
        // The radio's wins at the start.
        assert_eq!(v.step(Some(30), &mut panel, false, t0), None);
        assert_eq!(panel, 0.3);
        // E1 or the radio's knob: the panel follows.
        assert_eq!(v.step(Some(42), &mut panel, false, ms(10)), None);
        assert_eq!(panel, 0.42);
        // The Sound slider: the radio is sent it, and the old read-back
        // doesn't pull the slider back while it is awaited.
        panel = 0.6;
        assert_eq!(v.step(Some(42), &mut panel, false, ms(20)), Some(60));
        assert_eq!(v.step(Some(42), &mut panel, false, ms(40)), None);
        assert_eq!(panel, 0.6);
        // Dragged on: one VM each 150 ms at most.
        panel = 0.65;
        assert_eq!(v.step(Some(42), &mut panel, false, ms(60)), None);
        assert_eq!(v.step(Some(42), &mut panel, false, ms(180)), Some(65));
        assert_eq!(v.step(Some(65), &mut panel, false, ms(300)), None);
        assert_eq!(panel, 0.65);
        // Not while transmitting; sent after.
        panel = 0.2;
        assert_eq!(v.step(Some(65), &mut panel, true, ms(500)), None);
        assert_eq!(v.step(Some(65), &mut panel, false, ms(600)), Some(20));
        // A VM the radio never takes: its word stands after a while.
        assert_eq!(v.step(Some(65), &mut panel, false, ms(1000)), None);
        assert_eq!(panel, 0.2);
        assert_eq!(v.step(Some(65), &mut panel, false, ms(2200)), None);
        assert_eq!(panel, 0.65);
    }
}
