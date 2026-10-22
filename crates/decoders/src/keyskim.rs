// The detector, the noise floor and the per-track bookkeeping follow
// SDRoxide's wide skimmers (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-skimmer/src/cw.rs and digi.rs, GPL-3.0-or-later. The
// classifier, the lanes and the worker are this panel's own.

//! The keyboard skimmer: CW, PSK31 and RTTY read off the whole I/Q window at
//! once, as SDRoxide's CW, PSK and RTTY skimmers do (decision 56).
//!
//! **Finding the signals.** A streaming STFT over a window of up to
//! 192 kHz (the I/Q itself at 192 kHz or less; above that, a 192 kHz cut of
//! it around the scope's view) with about 47 Hz bins and eight frames a
//! window (375 a second). Each bin's power is smoothed over about a quarter
//! of a second and set against a per-region median noise floor, as
//! SDRoxide's skimmers do; a peak that stands 6 dB over the floor for
//! 80 ms, in the view and outside the WSJT modes' sub-bands, becomes a
//! track.
//!
//! **Telling the modes apart.** SDRoxide gates each skimmer to its mode's
//! segments of the band plan; this one looks at the signal instead, for its
//! first 1.2 s (the probe):
//! * a second tone 170 Hz from it (3 to 4 bins), keyed against it (their
//!   powers' correlation below -0.25), is **RTTY**, its mark the upper tone;
//! * otherwise a carrier that spends more than 12 % of the probe in key-up
//!   gaps of 40 ms or more is **CW** (PSK31's phase reversals dip its power
//!   for a few milliseconds only);
//! * otherwise it is **PSK31**.
//!
//! **Reading them.** CW from its bin's magnitude, through SDRoxide's timing
//! decoder ([`crate::morse::CwDecoder`]) at the frame rate, as SDRoxide's CW
//! skimmer does. PSK31 and RTTY not from the bins (SDRoxide's first cut,
//! which it calls coarse) but from a lane of their own: the signal cut out
//! of the window by an [`IqChannel`], at 500 Hz complex into the panel's
//! [`BpskCore`] for PSK31 and 2 kHz into its [`RttyRx`] (45.45 baud, 170 Hz
//! shift, with its AFC) for RTTY — the same demodulators DECODE uses. The
//! callsign is the last callsign-shaped word of the text
//! ([`crate::callsign::find_callsign`]).
//!
//! Tracks are capped ([`KeySkimConfig::max_channels`], strongest first),
//! and one thread does all of it ([`KeySkimWorker`]) behind a queue that
//! drops blocks rather than make the I/Q tap wait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select, unbounded};
use rustfft::{Fft, FftPlanner};
use sdr::Complex32;

use crate::callsign::find_callsign;
use crate::modem::psk::BpskCore;
use crate::modem::rtty::RttyRx;
use crate::morse::CwDecoder;
use crate::resample::{IqChannel, deinterleave};

/// The keyboard modes the skimmer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyMode {
    Cw,
    Psk,
    Rtty,
}

impl KeyMode {
    /// The mode's tag on the scope's labels.
    pub fn label(self) -> &'static str {
        match self {
            KeyMode::Cw => "CW",
            KeyMode::Psk => "PSK",
            KeyMode::Rtty => "RTTY",
        }
    }
}

/// One signal the skimmer is reading.
#[derive(Debug, Clone, PartialEq)]
pub struct KeySpot {
    /// The track's own number, for as long as the signal is tracked.
    pub id: u64,
    pub mode: KeyMode,
    /// Where it is, in the frame of the I/Q's centre, Hz: CW's carrier,
    /// PSK31's carrier, RTTY's mark/space centre.
    pub hz: f64,
    /// The last callsign-shaped word of its text.
    pub call: Option<String>,
    /// The text read so far (the newest [`TEXT_CAP`] characters).
    pub text: String,
    /// Its level over the noise in its ~47 Hz bin, dB.
    pub snr_db: f32,
    /// CW's speed, WPM (0 for PSK31 and RTTY).
    pub wpm: u16,
    /// Heard within the last 1.5 s.
    pub active: bool,
    /// UTC it was last heard, s.
    pub heard_utc: f64,
}

impl KeySpot {
    /// It is calling CQ: "CQ" is one of its words.
    pub fn cq(&self) -> bool {
        self.text.split_whitespace().any(|w| w == "CQ")
    }
}

/// What the skimmer reads, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct KeySkimConfig {
    pub cw: bool,
    pub psk: bool,
    pub rtty: bool,
    /// The most signals tracked (and read) at once.
    pub max_channels: usize,
    /// The scope's view, Hz in the I/Q centre's frame: only signals in it
    /// are tracked, and above 192 kHz the window is cut around it. `None`:
    /// the whole window.
    pub view: Option<(f64, f64)>,
    /// Stretches never tracked, Hz: the WSJT modes' sub-bands, whose
    /// signals would read as PSK31 garbage.
    pub exclude: Vec<(f64, f64)>,
}

impl Default for KeySkimConfig {
    fn default() -> Self {
        KeySkimConfig {
            cw: true,
            psk: true,
            rtty: true,
            max_channels: MAX_CHANNELS,
            view: None,
            exclude: Vec::new(),
        }
    }
}

impl KeySkimConfig {
    fn enabled(&self, m: KeyMode) -> bool {
        match m {
            KeyMode::Cw => self.cw,
            KeyMode::Psk => self.psk,
            KeyMode::Rtty => self.rtty,
        }
    }

    fn any(&self) -> bool {
        self.cw || self.psk || self.rtty
    }
}

/// The default cap on signals tracked at once.
pub const MAX_CHANNELS: usize = 32;
/// The widest window skimmed, Hz; wider I/Q is cut down to it.
pub const SKIM_RATE: f64 = 192_000.0;
/// The bin the transform aims for, Hz (a 4096-point window at 192 kHz).
const BIN_HZ: f64 = 46.875;
/// Frames per window: the frame rate is eight times the bin.
const FRAMES_PER_WINDOW: usize = 8;
/// Frames of noise-floor priming before anything is detected.
const WARMUP: u32 = 64;
/// Bins per region for the median noise floor (~3 kHz).
const REGION_BINS: usize = 64;
/// A region median is scaled by this to a mean (exponential power).
const MEDIAN_TO_MEAN: f32 = std::f32::consts::LOG2_E;
/// The smoothed power's step per frame (~0.27 s).
const SMOOTH: f32 = 0.01;
/// A peak this far over the floor (smoothed power, ~4 dB) is a candidate.
const DETECT: f32 = 2.5;
/// A track is still on while its smoothed power is this far over the floor,
/// or a frame's is [`KEY_ON`] over it.
const KEEP: f32 = 1.8;
const KEY_ON: f32 = 10.0;
/// Bins either side of a peak where no second peak is taken (~234 Hz): one
/// signal, or an RTTY pair, is one track.
const PEAK_SPACING: i64 = 5;
/// Frames a candidate must persist to become a track (~80 ms).
const CONFIRM: u32 = 30;
/// Frames a new track is watched before its mode is decided (~1.2 s).
const PROBE: u32 = 450;
/// Frames into the probe at which its frequency is measured again (~0.3 s).
const REFINE: u32 = 120;
/// RTTY: the second tone's smoothed power against the first's.
const PAIR_FRAC: f32 = 0.15;
/// RTTY: the two tones' powers must be anti-correlated at least this much.
const PAIR_CORR: f32 = -0.25;
/// CW: key-up gaps this long (frames, ~40 ms) count as keying ...
const LONG_OFF: u32 = 15;
/// ... and a CW signal spends at least this share of the probe in them.
const CW_OFF_SHARE: f32 = 0.12;
/// A track not heard for this long is dropped, s.
const PRUNE_S: f64 = 6.0;
/// A spot is "active" within this of being heard, s.
const ACTIVE_S: f64 = 1.5;
/// A track that has read nothing worth showing in this long stops being
/// read (a steady carrier, a beacon, something not in these modes), s.
const MUTE_S: f64 = 20.0;
/// The newest characters kept per track.
pub const TEXT_CAP: usize = 64;
/// Lane rates: PSK31 at 16 samples a symbol, RTTY at 44 a bit.
const PSK_LANE: f64 = 500.0;
const RTTY_LANE: f64 = 2000.0;
const RTTY_SHIFT: f64 = 170.0;
/// A lane stops reading this long after its signal was last loud, s, and
/// what it read later than [`TRIM_SLACK_S`] after that is taken off again.
const LANE_HOLD_S: f64 = 0.3;
const TRIM_SLACK_S: f64 = 0.1;
/// RTTY: frames of the pair's correlation needed before deciding, and the
/// longest a probe may run waiting for them.
const PAIR_FRAMES: u32 = 150;
const PROBE_MAX: u32 = 3 * PROBE;
const RTTY_BAUD: f64 = 45.45;
/// CW's timing decoder re-fits this often per track, s.
const CW_HOP_S: f32 = 0.5;
/// A CW spot's speed must be in this range.
const CW_WPM: std::ops::RangeInclusive<u16> = 8..=45;
/// Share of the window either side of its centre that is skimmed: past
/// this the decimating filters roll off.
const USABLE: f64 = 0.45;

/// What a track is being read as.
enum Reader {
    /// Being probed: whatever it might be is read meanwhile, each into a
    /// text of its own, so that the mode decided keeps its first second: a
    /// CW decoder (it is cheap), a PSK31 lane unless it is already a pair,
    /// and an RTTY lane (a lone tone may be RTTY idling on mark).
    Probe {
        cw: Box<CwDecoder>,
        cw_text: String,
        psk: Option<(Lane, String)>,
        rtty: Option<(Lane, String)>,
    },
    Cw(Box<CwDecoder>),
    Lane(Lane),
    /// Tracked so that it is not found again, but not read: its mode is
    /// switched off, or nothing came of reading it.
    Muted,
}

/// A narrow lane cut from the window and its demodulator.
struct Lane {
    /// Its centre, Hz from the window's.
    off_hz: f64,
    chan: IqChannel,
    demod: Demod,
    buf: Vec<Complex32>,
}

enum Demod {
    Psk(BpskCore),
    Rtty(Box<RttyRx>),
}

impl Lane {
    fn psk(rate: f64, off_hz: f64) -> Self {
        Lane {
            off_hz,
            chan: IqChannel::new(rate, PSK_LANE, off_hz),
            demod: Demod::Psk(BpskCore::new((PSK_LANE / 31.25) as f32)),
            buf: Vec::new(),
        }
    }

    fn rtty(rate: f64, off_hz: f64) -> Self {
        Lane {
            off_hz,
            chan: IqChannel::new(rate, RTTY_LANE, off_hz),
            demod: Demod::Rtty({
                let mut rx = RttyRx::new(RTTY_LANE, 0.0, RTTY_BAUD, RTTY_SHIFT);
                rx.set_usos(true);
                Box::new(rx)
            }),
            buf: Vec::new(),
        }
    }

    /// Read a block into `text`; how many characters it added.
    fn process(&mut self, iq: &[Complex32], text: &mut String) -> usize {
        self.buf.clear();
        self.chan.process(iq, &mut self.buf);
        let mut s = String::new();
        match &mut self.demod {
            Demod::Psk(core) => {
                for &z in &self.buf {
                    core.push(z, &mut s);
                }
            }
            Demod::Rtty(rx) => s = rx.process_iq(&self.buf),
        }
        append_text(text, &s)
    }

    /// Move the lane's centre (its demodulator's loops carry on and take
    /// up what is left).
    fn retune(&mut self, off_hz: f64) {
        if (off_hz - self.off_hz).abs() > 0.5 {
            self.off_hz = off_hz;
            self.chan.set_offset_hz(off_hz);
        }
    }

    /// How far off the lane's centre the demodulator has found the signal.
    fn pull_hz(&self) -> f64 {
        match &self.demod {
            Demod::Psk(core) => core.freq_offset() as f64 * PSK_LANE,
            Demod::Rtty(rx) => rx.afc_offset_hz() as f64,
        }
    }

    fn mode(&self) -> KeyMode {
        match self.demod {
            Demod::Psk(_) => KeyMode::Psk,
            Demod::Rtty(_) => KeyMode::Rtty,
        }
    }
}

struct Track {
    id: u64,
    /// Its bin, signed from the window's centre: for RTTY the stronger tone.
    bin: i64,
    /// RTTY's other tone, bins from `bin`, while it might be one.
    pair: Option<i64>,
    /// Where it is, Hz from the window's centre (refined as it is read).
    off_hz: f64,
    reader: Reader,
    text: String,
    born_s: f64,
    last_on_s: f64,
    /// Its power smoothed over ~25 ms, and when that was last well over
    /// the floor: the lanes read only while it is.
    fast: f32,
    last_loud_s: f64,
    level: f32,
    /// The lane's characters added in all, with marks of how many by when
    /// over the last two seconds, and whether its signal has gone quiet:
    /// for [`trim_tail`].
    added: u64,
    marks: Vec<(f64, u64)>,
    quiet: bool,
    /// The text last grew (for muting), s.
    grew_s: f64,
    /// A decaying peak of its bin's power, and its level over the floor.
    peak: f32,
    snr_db: f32,
    /// Smoothed power at bin-1, bin, bin+1 while keyed: for the sub-bin
    /// frequency (as SDRoxide's CW skimmer).
    pk: [f32; 3],
    // The probe's statistics.
    frames: u32,
    off_run: u32,
    long_off: u32,
    /// Sums for the pair's correlation (x, y, xx, yy, xy), over this many
    /// frames since the pair was found.
    corr: [f64; 5],
    corr_frames: u32,
}

impl Track {
    fn mode(&self) -> Option<KeyMode> {
        match &self.reader {
            Reader::Cw(_) => Some(KeyMode::Cw),
            Reader::Lane(l) => Some(l.mode()),
            _ => None,
        }
    }

    fn covers(&self, bin: i64) -> bool {
        (self.bin - bin).abs() <= PEAK_SPACING
            || self.pair.is_some_and(|d| (self.bin + d - bin).abs() <= PEAK_SPACING)
    }

    fn pair_corr(&self) -> f32 {
        let n = self.corr_frames.max(1) as f64;
        let [x, y, xx, yy, xy] = self.corr;
        let cov = xy / n - (x / n) * (y / n);
        let vx = xx / n - (x / n).powi(2);
        let vy = yy / n - (y / n).powi(2);
        if vx <= 0.0 || vy <= 0.0 { 0.0 } else { (cov / (vx * vy).sqrt()) as f32 }
    }
}

/// The skimmer itself, synchronous: blocks of I/Q in, spots out. The
/// worker ([`KeySkimWorker`]) runs one on a thread.
pub struct KeySkimmer {
    cfg: KeySkimConfig,
    /// The I/Q as last seen: its rate and centre.
    in_rate: f64,
    in_centre: f64,
    /// The cut down to [`SKIM_RATE`], for wider I/Q, and its centre.
    cut: Option<IqChannel>,
    rate: f64,
    centre: f64,
    stft: Stft,
    /// Which bins may hold a track (the view, the exclusions, the edges and
    /// the DC spike).
    allowed: Vec<bool>,
    tracks: Vec<Track>,
    confirm: Vec<(i64, u32)>,
    next_id: u64,
    /// The window's samples as seconds, and the UTC of its sample 0.
    now_s: f64,
    utc0: f64,
    skim: Vec<Complex32>,
}

/// The streaming transform and its per-bin state.
struct Stft {
    n: usize,
    hop: usize,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    inbuf: Vec<Complex32>,
    read_pos: usize,
    scratch: Vec<Complex32>,
    power: Vec<f32>,
    smooth: Vec<f32>,
    floor: Vec<f32>,
    med: Vec<u32>,
    frames: u32,
}

impl Stft {
    fn new(rate: f64) -> Self {
        let n = ((rate / BIN_HZ).round() as usize).next_power_of_two().max(256);
        let window = (0..n)
            .map(|i| {
                let x = std::f32::consts::PI * i as f32 / (n as f32 - 1.0);
                x.sin().powi(2)
            })
            .collect();
        Stft {
            n,
            hop: n / FRAMES_PER_WINDOW,
            fft: FftPlanner::<f32>::new().plan_fft_forward(n),
            window,
            inbuf: Vec::with_capacity(n * 4),
            read_pos: 0,
            scratch: vec![Complex32::default(); n],
            power: vec![0.0; n],
            smooth: vec![0.0; n],
            floor: vec![0.0; n],
            med: Vec::with_capacity(REGION_BINS),
            frames: 0,
        }
    }

    fn index(&self, off: i64) -> usize {
        off.rem_euclid(self.n as i64) as usize
    }

    fn offset(&self, k: usize) -> i64 {
        if k <= self.n / 2 { k as i64 } else { k as i64 - self.n as i64 }
    }

    /// One frame's power, floor and smoothing.
    fn analyse(&mut self) {
        self.fft.process(&mut self.scratch);
        for (p, z) in self.power.iter_mut().zip(&self.scratch) {
            *p = z.norm_sqr();
        }
        // The floor: each region's median, scaled to a mean, smoothed; once
        // a window of fresh samples (as SDRoxide's skimmers).
        if self.frames % FRAMES_PER_WINDOW as u32 == 0 {
            let a = if self.frames < WARMUP { 0.5 } else { 0.1 };
            let seed = self.frames == 0;
            for base in (0..self.n).step_by(REGION_BINS) {
                let end = (base + REGION_BINS).min(self.n);
                self.med.clear();
                self.med.extend(self.power[base..end].iter().map(|p| p.to_bits()));
                let mid = self.med.len() / 2;
                self.med.select_nth_unstable(mid);
                let est = f32::from_bits(self.med[mid]) * MEDIAN_TO_MEAN;
                for f in &mut self.floor[base..end] {
                    *f = if seed { est } else { *f + a * (est - *f) };
                }
            }
        }
        for (s, p) in self.smooth.iter_mut().zip(&self.power) {
            *s += SMOOTH * (p - *s);
        }
        self.frames = self.frames.saturating_add(1);
    }
}

impl KeySkimmer {
    pub fn new(cfg: KeySkimConfig) -> Self {
        KeySkimmer {
            cfg,
            in_rate: 0.0,
            in_centre: 0.0,
            cut: None,
            rate: 0.0,
            centre: 0.0,
            stft: Stft::new(SKIM_RATE),
            allowed: Vec::new(),
            tracks: Vec::new(),
            confirm: Vec::new(),
            next_id: 1,
            now_s: 0.0,
            utc0: 0.0,
            skim: Vec::new(),
        }
    }

    pub fn config(&self) -> &KeySkimConfig {
        &self.cfg
    }

    /// New settings. A mode switched off stops being read at once; its
    /// tracks are kept, muted, so they are not found again as something
    /// else.
    pub fn set_config(&mut self, cfg: KeySkimConfig) {
        if cfg == self.cfg {
            return;
        }
        let moved = cfg.view != self.cfg.view;
        self.cfg = cfg;
        for t in &mut self.tracks {
            if let Some(m) = t.mode()
                && !self.cfg.enabled(m)
            {
                t.reader = Reader::Muted;
            }
        }
        if moved && self.in_rate > SKIM_RATE * 1.01 && self.window_moves() {
            // Re-placed on the next block.
            self.rate = 0.0;
        }
        self.mask();
    }

    /// Signals being tracked now (read or not).
    pub fn tracks(&self) -> usize {
        self.tracks.len()
    }

    /// Where the window is: its rate and centre, Hz.
    pub fn window(&self) -> (f64, f64) {
        (self.rate, self.centre)
    }

    /// Complex I/Q at `rate` around `centre` (Hz), its first sample at
    /// `utc`.
    pub fn push(&mut self, iq: &[Complex32], rate: f64, centre: f64, utc: f64) {
        if !self.cfg.any() || rate <= 0.0 {
            return;
        }
        if rate != self.in_rate || centre != self.in_centre {
            self.in_rate = rate;
            self.in_centre = centre;
            self.rate = 0.0;
        }
        if self.rate == 0.0 {
            self.place();
        }
        // The window's samples.
        let mut skim = std::mem::take(&mut self.skim);
        skim.clear();
        match &mut self.cut {
            Some(c) => c.process(iq, &mut skim),
            None => skim.extend_from_slice(iq),
        }
        self.utc0 = utc - self.now_s;
        // The lanes, on the whole block, while their signal is on: a lane
        // left reading the noise after a station stops prints hash.
        let end_s = self.now_s + skim.len() as f64 / self.rate;
        for t in &mut self.tracks {
            if self.now_s - t.last_loud_s > LANE_HOLD_S {
                if !t.quiet {
                    t.quiet = true;
                    let until = t.last_loud_s + TRIM_SLACK_S;
                    trim_tail(&mut t.text, &mut t.added, &t.marks, until);
                }
                continue;
            }
            t.quiet = false;
            match &mut t.reader {
                Reader::Lane(l) => {
                    let n = l.process(&skim, &mut t.text);
                    if n > 0 {
                        t.grew_s = self.now_s;
                        t.added += n as u64;
                    }
                    t.marks.push((end_s, t.added));
                    let old = t.marks.iter().take_while(|m| m.0 < end_s - 2.0).count();
                    t.marks.drain(..old);
                }
                Reader::Probe { psk, rtty, .. } => {
                    for (l, text) in [psk, rtty].into_iter().flatten() {
                        l.process(&skim, text);
                    }
                }
                _ => {}
            }
        }
        // The transform, frame by frame.
        self.stft.inbuf.extend_from_slice(&skim);
        let n = self.stft.n;
        let hop = self.stft.hop;
        let mut consumed = 0usize;
        while self.stft.read_pos + n <= self.stft.inbuf.len() {
            let base = self.stft.read_pos;
            for i in 0..n {
                self.stft.scratch[i] = self.stft.inbuf[base + i] * self.stft.window[i];
            }
            self.stft.analyse();
            self.stft.read_pos += hop;
            consumed += hop;
            self.on_frame(self.now_s + consumed as f64 / self.rate);
        }
        if self.stft.read_pos >= n {
            self.stft.inbuf.drain(..self.stft.read_pos);
            self.stft.read_pos = 0;
        }
        self.now_s += skim.len() as f64 / self.rate;
        self.skim = skim;
    }

    /// Whether the view has moved far enough in wide I/Q that the window
    /// must be cut somewhere else.
    fn window_moves(&self) -> bool {
        let Some((lo, hi)) = self.cfg.view else { return false };
        let half = USABLE * self.rate;
        lo < self.centre - half || hi > self.centre + half
    }

    /// Place the window in the I/Q and start afresh: every track was
    /// measured against the old one.
    fn place(&mut self) {
        let (rate, centre) = if self.in_rate > SKIM_RATE * 1.01 {
            // Around the view, inside the span's usable part.
            let reach = (USABLE * self.in_rate - 0.5 * SKIM_RATE).max(0.0);
            let want = self.cfg.view.map_or(self.in_centre, |(lo, hi)| 0.5 * (lo + hi));
            let c = want.clamp(self.in_centre - reach, self.in_centre + reach);
            self.cut = Some(IqChannel::new(self.in_rate, SKIM_RATE, c - self.in_centre));
            (SKIM_RATE, c)
        } else {
            self.cut = None;
            (self.in_rate, self.in_centre)
        };
        self.stft = Stft::new(rate);
        self.rate = rate;
        self.centre = centre;
        self.tracks.clear();
        self.confirm.clear();
        self.mask();
    }

    fn bin_hz(&self) -> f64 {
        self.rate / self.stft.n as f64
    }

    fn mask(&mut self) {
        let n = self.stft.n;
        let bin_hz = self.bin_hz();
        if !bin_hz.is_finite() || self.rate <= 0.0 {
            self.allowed = vec![false; n];
            return;
        }
        let dc = self.in_centre - self.centre;
        let usable = USABLE * self.rate;
        self.allowed = (0..n)
            .map(|k| {
                let off = self.stft.offset(k) as f64 * bin_hz;
                let hz = self.centre + off;
                off.abs() <= usable
                    && (off - dc).abs() > 2.5 * bin_hz
                    && self.cfg.view.is_none_or(|(lo, hi)| hz >= lo && hz <= hi)
                    && !self.cfg.exclude.iter().any(|&(a, b)| hz >= a - bin_hz && hz <= b + bin_hz)
            })
            .collect();
        let allowed = &self.allowed;
        let st = &self.stft;
        self.tracks.retain(|t| allowed[st.index(t.bin)]);
    }

    fn on_frame(&mut self, now: f64) {
        let st = &self.stft;
        if st.frames < WARMUP {
            return;
        }
        let n = st.n;
        let bin_hz = self.rate / n as f64;
        // Candidates: peaks of the smoothed power over the floor, strongest
        // first, one per signal.
        let mut cands: Vec<(f32, i64)> = (0..n)
            .filter(|&k| self.allowed[k] && st.smooth[k] > st.floor[k] * DETECT)
            .map(|k| (st.smooth[k] / st.floor[k].max(1e-30), st.offset(k)))
            .collect();
        cands.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let mut centres: Vec<i64> = Vec::new();
        for &(_, off) in &cands {
            if centres.iter().all(|&c| (c - off).abs() > PEAK_SPACING) {
                centres.push(off);
            }
        }
        let confirm: Vec<(i64, u32)> = centres
            .iter()
            .map(|&off| {
                let prev =
                    self.confirm.iter().find(|(b, _)| (b - off).abs() <= 1).map_or(0, |&(_, c)| c);
                (off, prev + 1)
            })
            .collect();
        self.confirm = confirm;
        // New tracks, strongest first, up to the cap.
        for i in 0..self.confirm.len() {
            let (off, count) = self.confirm[i];
            if count < CONFIRM
                || self.tracks.len() >= self.cfg.max_channels
                || self.tracks.iter().any(|t| t.covers(off))
            {
                continue;
            }
            self.spawn(off, now, bin_hz);
        }
        // Every track: its bookkeeping, its CW decoder.
        let st = &self.stft;
        for t in &mut self.tracks {
            let k = st.index(t.bin);
            let (p, floor) = (st.power[k], st.floor[k].max(1e-30));
            let mut on = p > floor * KEY_ON || st.smooth[k] > floor * KEEP;
            if let Some(d) = t.pair {
                let k2 = st.index(t.bin + d);
                on |= st.smooth[k2] > st.floor[k2] * KEEP;
            }
            if on {
                t.last_on_s = now;
            }
            // Loud: its power over ~25 ms well over the floor and within
            // 11 dB of its own level, so that the end of a strong signal is
            // seen at once, not when it has sunk to the floor.
            t.fast += 0.1 * (p - t.fast);
            let mut loud = t.fast > (floor * KEEP).max(t.level * 0.08);
            if let Some(d) = t.pair {
                let k2 = st.index(t.bin + d);
                loud |= st.power[k2] > st.floor[k2] * KEY_ON;
            }
            if loud {
                t.last_loud_s = now;
                // Its level: the power of its key-down (or carrier) frames.
                if p * p > floor * t.peak {
                    t.level += 0.02 * (p - t.level);
                }
                t.snr_db = 10.0 * (t.level / floor).log10();
            }
            t.peak = p.max(t.peak * 0.995);
            if p > floor * KEY_ON {
                let km = st.index(t.bin - 1);
                let kp = st.index(t.bin + 1);
                for (a, q) in t.pk.iter_mut().zip([st.smooth[km], st.smooth[k], st.smooth[kp]]) {
                    *a += 0.1 * (q - *a);
                }
            }
            let keyed_up = p < (floor * t.peak).sqrt();
            if let Reader::Probe { cw, cw_text, rtty, psk } = &mut t.reader {
                t.frames += 1;
                if t.frames == REFINE {
                    // A better fix on it than the one it was found on.
                    let sm = |d: i64| st.smooth[st.index(t.bin + d)];
                    let own = sub_bin(t.bin, [sm(-1), sm(0), sm(1)]) * bin_hz;
                    t.off_hz = own + t.pair.map_or(0.0, |d| 0.5 * RTTY_SHIFT * d.signum() as f64);
                    if let Some((l, _)) = psk {
                        l.retune(own);
                    }
                    if let Some((l, _)) = rtty {
                        l.retune(
                            own + 0.5 * RTTY_SHIFT * t.pair.map_or(-1.0, |d| d.signum() as f64),
                        );
                    }
                }
                if keyed_up {
                    t.off_run += 1;
                    if t.off_run == LONG_OFF {
                        t.long_off += LONG_OFF;
                    } else if t.off_run > LONG_OFF {
                        t.long_off += 1;
                    }
                } else {
                    t.off_run = 0;
                }
                if let Some(d) = t.pair {
                    let (x, y) = (p as f64, st.power[st.index(t.bin + d)] as f64);
                    let c = &mut t.corr;
                    c[0] += x;
                    c[1] += y;
                    c[2] += x * x;
                    c[3] += y * y;
                    c[4] += x * y;
                    t.corr_frames += 1;
                } else if t.frames % FRAMES_PER_WINDOW as u32 == 0
                    && let Some(d) = pair_of(st, t.bin)
                {
                    // RTTY's other tone has come up (it idles on mark): a
                    // pair after all, read from its middle.
                    t.pair = Some(d);
                    t.off_hz = sub_bin(t.bin, t.pk) * bin_hz + 0.5 * RTTY_SHIFT * d.signum() as f64;
                    // The lane already running took this tone for mark,
                    // with space below; the other way up it starts again.
                    if d > 0 && self.cfg.rtty {
                        *rtty = Some((Lane::rtty(self.rate, t.off_hz), String::new()));
                    }
                }
                feed_cw(cw, p, cw_text);
            } else if let Reader::Cw(cw) = &mut t.reader {
                let before = t.text.len();
                feed_cw(cw, p, &mut t.text);
                if t.text.len() != before {
                    t.grew_s = now;
                }
            }
        }
        // Probes that have run their time: decide.
        let rate = self.rate;
        let cfg = &self.cfg;
        let st = &self.stft;
        for t in &mut self.tracks {
            if !matches!(t.reader, Reader::Probe { .. }) || t.frames < PROBE {
                continue;
            }
            // A pair found late: give it time to show it is one.
            if t.pair.is_some() && t.corr_frames < PAIR_FRAMES && t.frames < PROBE_MAX {
                continue;
            }
            let Reader::Probe { cw, cw_text, psk, rtty: rtty_lane } =
                std::mem::replace(&mut t.reader, Reader::Muted)
            else {
                unreachable!()
            };
            let rtty = t.pair.is_some() && t.pair_corr() < PAIR_CORR;
            let off_share = t.long_off as f32 / t.frames.max(1) as f32;
            let mode = if rtty {
                KeyMode::Rtty
            } else if off_share > CW_OFF_SHARE {
                KeyMode::Cw
            } else {
                KeyMode::Psk
            };
            if !rtty {
                // Two carriers, not a pair: this one alone, at its own bin.
                t.pair = None;
            }
            // Where it is, again, from 1.2 s of smoothed power rather than
            // the 80 ms it was found on: PSK31's Costas loop pulls in only a
            // few hertz.
            let sm = |d: i64| st.smooth[st.index(t.bin + d)];
            let own = sub_bin(t.bin, [sm(-1), sm(0), sm(1)]) * bin_hz;
            t.off_hz = own + t.pair.map_or(0.0, |d| 0.5 * RTTY_SHIFT * d.signum() as f64);
            // The decided mode keeps what it read; the others are dropped.
            t.grew_s = now;
            t.reader = if !cfg.enabled(mode) {
                Reader::Muted
            } else {
                let (reader, text) = match mode {
                    KeyMode::Cw => (Reader::Cw(cw), cw_text),
                    KeyMode::Psk => {
                        let (mut l, text) =
                            psk.unwrap_or_else(|| (Lane::psk(rate, t.off_hz), String::new()));
                        l.retune(t.off_hz);
                        (Reader::Lane(l), text)
                    }
                    KeyMode::Rtty => {
                        let (mut l, text) = rtty_lane
                            .unwrap_or_else(|| (Lane::rtty(rate, t.off_hz), String::new()));
                        l.retune(t.off_hz);
                        (Reader::Lane(l), text)
                    }
                };
                t.text = text;
                reader
            };
        }
        // Reading nothing for long enough: stop reading it.
        for t in &mut self.tracks {
            if matches!(t.reader, Reader::Cw(_) | Reader::Lane(_)) && now - t.grew_s > MUTE_S {
                t.reader = Reader::Muted;
            }
        }
        self.tracks.retain(|t| now - t.last_on_s < PRUNE_S);
    }

    fn spawn(&mut self, off: i64, now: f64, bin_hz: f64) {
        let st = &self.stft;
        let k = st.index(off);
        let main = st.smooth[k];
        let pair = pair_of(st, off);
        let pk = [st.smooth[st.index(off - 1)], main, st.smooth[st.index(off + 1)]];
        let own = sub_bin(off, pk) * bin_hz;
        let off_hz = match pair {
            // The middle of the pair.
            Some(d) => own + 0.5 * RTTY_SHIFT * d.signum() as f64,
            None => own,
        };
        let psk =
            (pair.is_none() && self.cfg.psk).then(|| (Lane::psk(self.rate, off_hz), String::new()));
        // A lone tone as RTTY's mark, space below it.
        let rtty_at = if pair.is_some() { off_hz } else { own - 0.5 * RTTY_SHIFT };
        let rtty = self.cfg.rtty.then(|| (Lane::rtty(self.rate, rtty_at), String::new()));
        let mut cw = CwDecoder::new((self.rate / st.hop as f64) as f32);
        cw.set_hop_s(CW_HOP_S);
        let id = self.next_id;
        self.next_id += 1;
        self.tracks.push(Track {
            id,
            bin: off,
            pair,
            off_hz,
            reader: Reader::Probe { cw: Box::new(cw), cw_text: String::new(), psk, rtty },
            text: String::new(),
            born_s: now,
            last_on_s: now,
            last_loud_s: now,
            fast: st.power[k],
            level: st.power[k],
            added: 0,
            marks: Vec::new(),
            quiet: false,
            grew_s: now,
            peak: st.power[k],
            snr_db: 10.0 * (main / st.floor[k].max(1e-30)).log10(),
            pk,
            frames: 0,
            off_run: 0,
            long_off: 0,
            corr: [0.0; 5],
            corr_frames: 0,
        });
    }

    /// The signals worth showing: a decided mode that is on, and text with
    /// something in it (for CW also a plausible speed).
    pub fn spots(&self) -> Vec<KeySpot> {
        let bin_hz = self.bin_hz();
        self.tracks
            .iter()
            .filter_map(|t| {
                let mode = t.mode()?;
                let text = t.text.trim();
                let meaty = text.chars().filter(|c| c.is_ascii_alphanumeric()).count();
                let call = find_callsign(&text.to_ascii_uppercase());
                let (hz, wpm) = match &t.reader {
                    Reader::Cw(cw) => {
                        let wpm = cw.wpm().round().clamp(0.0, 99.0) as u16;
                        if !CW_WPM.contains(&wpm) {
                            return None;
                        }
                        // Noise reads as runs of E, I and T.
                        let real = text
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() && !matches!(c, 'E' | 'I' | 'T'))
                            .count();
                        if call.is_none() && real < 3 {
                            return None;
                        }
                        (sub_bin(t.bin, t.pk) * bin_hz, wpm)
                    }
                    Reader::Lane(l) => {
                        if meaty < 4 {
                            return None;
                        }
                        (l.off_hz + l.pull_hz(), 0)
                    }
                    _ => return None,
                };
                let _ = t.born_s;
                Some(KeySpot {
                    id: t.id,
                    mode,
                    hz: self.centre + hz,
                    call,
                    text: text.to_string(),
                    snr_db: t.snr_db,
                    wpm,
                    active: self.now_s - t.last_on_s < ACTIVE_S,
                    heard_utc: self.utc0 + t.last_on_s,
                })
            })
            .collect()
    }
}

/// RTTY's other tone, 170 Hz from the one at `off`: the strongest of the
/// bins three and four either side, if it is comparable. Bins from `off`.
fn pair_of(st: &Stft, off: i64) -> Option<i64> {
    let main = st.smooth[st.index(off)];
    [-4i64, -3, 3, 4]
        .into_iter()
        .map(|d| (d, st.smooth[st.index(off + d)]))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|&(d, s)| s > main * PAIR_FRAC && s > st.floor[st.index(off + d)] * KEEP)
        .map(|(d, _)| d)
}

/// A track's frequency in bins, to a fraction of one: a parabola through
/// the logarithm of the smoothed power at its bin and either side. (Through
/// the power itself, as SDRoxide's CW skimmer does, is out by up to a tenth
/// of a bin on a Hann window, about 5 Hz: more than PSK31's Costas loop pulls
/// in at once.)
fn sub_bin(bin: i64, [a, b, c]: [f32; 3]) -> f64 {
    let ln = |x: f32| (x.max(1e-30) as f64).ln();
    let (a, b, c) = (ln(a), ln(b), ln(c));
    let denom = a - 2.0 * b + c;
    let d = if denom < 0.0 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
    bin as f64 + d
}

fn feed_cw(cw: &mut CwDecoder, power: f32, text: &mut String) {
    let s = cw.push(&[power.sqrt()]);
    append_text(text, &s);
}

/// Take off the end of `text` what a lane read after its signal stopped:
/// the characters added after the last of `marks` (when, and how many had
/// been added by then) at or before `until`. A demodulator's squelch takes
/// a second or so to close on the noise, and what it prints meanwhile can
/// even look like a callsign.
fn trim_tail(text: &mut String, added: &mut u64, marks: &[(f64, u64)], until: f64) {
    let Some(&(_, keep)) = marks.iter().rev().find(|(t, _)| *t <= until) else { return };
    let drop = added.saturating_sub(keep) as usize;
    for _ in 0..drop.min(text.chars().count()) {
        text.pop();
    }
    *added = keep;
    while text.ends_with(' ') {
        text.pop();
    }
}

/// Append decoded characters to a track's text: control characters as
/// spaces, runs of spaces as one, the newest [`TEXT_CAP`] kept.
fn append_text(buf: &mut String, s: &str) -> usize {
    let mut added = 0;
    for c in s.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' && (buf.is_empty() || buf.ends_with(' ')) {
            continue;
        }
        buf.push(c);
        added += 1;
    }
    let over = buf.chars().count().saturating_sub(TEXT_CAP);
    if over > 0 {
        let cut = buf.char_indices().nth(over).map_or(0, |(i, _)| i);
        buf.drain(..cut);
    }
    added
}

/// A block of the radio's I/Q.
struct Block {
    iq: Vec<f32>,
    rate: f64,
    centre: f64,
    utc: f64,
}

/// What the worker last had to say.
#[derive(Debug, Clone, Default)]
pub struct KeySkimState {
    pub spots: Vec<KeySpot>,
    /// Signals tracked.
    pub tracks: usize,
    /// The worker's time per second of I/Q, smoothed: 0.1 is a tenth of a
    /// core.
    pub load: f32,
}

/// I/Q blocks held before they are dropped.
const QUEUE: usize = 128;

/// A [`KeySkimmer`] on a thread of its own.
pub struct KeySkimWorker {
    tx: Option<Sender<Block>>,
    ctl: Sender<KeySkimConfig>,
    state: Arc<Mutex<KeySkimState>>,
    dropped: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl KeySkimWorker {
    pub fn spawn(cfg: KeySkimConfig) -> Self {
        let (tx, rx) = bounded::<Block>(QUEUE);
        let (ctl, ctl_rx) = unbounded::<KeySkimConfig>();
        let state = Arc::new(Mutex::new(KeySkimState::default()));
        let out = state.clone();
        let thread = std::thread::Builder::new()
            .name("skim-keys".into())
            .spawn(move || run(cfg, rx, ctl_rx, out))
            .expect("spawn the keyboard skimmer");
        KeySkimWorker {
            tx: Some(tx),
            ctl,
            state,
            dropped: Arc::new(AtomicU64::new(0)),
            thread: Some(thread),
        }
    }

    /// Interleaved I/Q at `rate` around `centre` (Hz), its first sample at
    /// `utc`. Never waits: a block there is no room for is dropped and
    /// counted.
    pub fn push_iq(&self, iq: &[f32], rate: f64, centre: f64, utc: f64) {
        let Some(tx) = &self.tx else { return };
        let b = Block { iq: iq.to_vec(), rate, centre, utc };
        if let Err(TrySendError::Full(_)) = tx.try_send(b) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn set_config(&self, cfg: KeySkimConfig) {
        let _ = self.ctl.send(cfg);
    }

    /// What it has found, as of its last report (about four a second).
    pub fn state(&self) -> KeySkimState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for KeySkimWorker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    cfg: KeySkimConfig,
    rx: Receiver<Block>,
    ctl: Receiver<KeySkimConfig>,
    out: Arc<Mutex<KeySkimState>>,
) {
    let mut sk = KeySkimmer::new(cfg);
    let mut iq = Vec::new();
    let mut since = 0.0f64;
    let mut load = 0.0f32;
    loop {
        select! {
            recv(ctl) -> c => match c {
                Ok(c) => sk.set_config(c),
                Err(_) => return,
            },
            recv(rx) -> b => {
                let Ok(b) = b else { return };
                let t = std::time::Instant::now();
                iq.clear();
                deinterleave(&b.iq, &mut iq);
                sk.push(&iq, b.rate, b.centre, b.utc);
                let dur = iq.len() as f64 / b.rate.max(1.0);
                if dur > 0.0 {
                    let l = (t.elapsed().as_secs_f64() / dur) as f32;
                    load += 0.05 * (l - load);
                }
                since += dur;
                if since >= 0.25 {
                    since = 0.0;
                    let s = KeySkimState { spots: sk.spots(), tracks: sk.tracks(), load };
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = s;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::KeySignal;

    /// Complex Gaussian noise, `sigma` a component, from a fixed seed.
    struct Noise(u64);

    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        fn add(&mut self, iq: &mut [Complex32], sigma: f32) {
            for z in iq {
                let r = (-2.0 * self.uniform().ln()).sqrt() * sigma as f64;
                let a = 2.0 * std::f64::consts::PI * self.uniform();
                *z += Complex32::new((r * a.cos()) as f32, (r * a.sin()) as f32);
            }
        }
    }

    const RATE: f64 = 192_000.0;
    const CENTRE: f64 = 14_050_000.0;

    /// Noise per component for an SNR of 0 dB in 2.5 kHz against a signal
    /// of amplitude 1 at 192 kHz: noise power 2σ² over the whole rate.
    fn sigma_for(snr_2500_db: f64, amp: f64) -> f32 {
        let p_sig = amp * amp;
        let p_noise_2500 = p_sig / 10f64.powf(snr_2500_db / 10.0);
        ((p_noise_2500 * RATE / 2500.0) / 2.0).sqrt() as f32
    }

    /// Run `signals` with noise for `seconds` through a skimmer, in 20 ms
    /// blocks as the tap gives them, and return what it says at the end
    /// (every spot seen along the way, newest per track).
    fn skim(
        signals: &mut [KeySignal],
        sigma: f32,
        seconds: f64,
        cfg: KeySkimConfig,
    ) -> Vec<KeySpot> {
        let mut sk = KeySkimmer::new(cfg);
        let mut noise = Noise(0x1234_5678_9abc_def1);
        let block = (RATE * 0.02) as usize;
        let mut seen: Vec<KeySpot> = Vec::new();
        let blocks = (seconds / 0.02) as usize;
        for b in 0..blocks {
            let t0 = b as f64 * 0.02;
            let mut iq = vec![Complex32::default(); block];
            for s in signals.iter_mut() {
                s.add_iq(&mut iq, RATE, CENTRE, t0);
            }
            noise.add(&mut iq, sigma);
            sk.push(&iq, RATE, CENTRE, 1_000.0 + t0);
            if b % 12 == 0 {
                for s in sk.spots() {
                    match seen.iter_mut().find(|o| o.id == s.id) {
                        Some(o) => *o = s,
                        None => seen.push(s),
                    }
                }
            }
        }
        seen
    }

    fn report(spots: &[KeySpot]) {
        for s in spots {
            eprintln!(
                "  {:>4} {:>12.1} {:>6.1} dB {:>2} wpm {:?} {:?}",
                s.mode.label(),
                s.hz,
                s.snr_db,
                s.wpm,
                s.call,
                s.text
            );
        }
    }

    /// The test band: four CW stations at 16 to 30 WPM, three PSK31 and
    /// three RTTY, spread over the window, all at one level, each sending
    /// one over (so each is heard once, from its start).
    fn band(amp_dbfs: f32) -> Vec<(KeySignal, &'static str)> {
        let a = amp_dbfs;
        let c = CENTRE;
        vec![
            (KeySignal::cw(c - 71_000.0, a, 16.0, "CQ CQ DE W2XYZ W2XYZ K", 60.0), "W2XYZ"),
            (KeySignal::cw(c - 42_300.0, a, 22.0, "CQ CQ DE K4ABC K4ABC K", 60.0), "K4ABC"),
            (KeySignal::cw(c - 20_050.0, a, 26.0, "CQ TEST DE N9XYZ N9XYZ TEST", 60.0), "N9XYZ"),
            (KeySignal::cw(c - 6_400.0, a, 30.0, "TEST DE VE3ABC VE3ABC", 60.0), "VE3ABC"),
            (KeySignal::psk(c + 11_000.0, a, "CQ CQ CQ DE KB1XYZ KB1XYZ PSE K ", 60.0), "KB1XYZ"),
            (KeySignal::psk(c + 21_600.0, a, "CQ CQ DE W5ABC W5ABC K ", 60.0), "W5ABC"),
            (KeySignal::psk(c + 38_150.0, a, "VE3XYZ DE G4XYZ G4XYZ 73 ", 60.0), "G4XYZ"),
            (KeySignal::rtty(c + 52_500.0, a, "CQ CQ CQ DE N2ABC N2ABC K ", 60.0), "N2ABC"),
            (KeySignal::rtty(c + 63_800.0, a, "CQ TEST DE K7XYZ K7XYZ ", 60.0), "K7XYZ"),
            (KeySignal::rtty(c + 77_200.0, a, "CQ CQ DE JA1ABC JA1ABC K ", 60.0), "JA1ABC"),
        ]
    }

    /// Of `band`'s stations, those whose spot names it, in its mode, within
    /// `tol` Hz of it.
    fn found(sent: &[(KeySignal, &str)], spots: &[KeySpot], tol: f64) -> Vec<bool> {
        sent.iter()
            .map(|(s, call)| {
                spots.iter().any(|p| {
                    p.mode == s.mode
                        && p.call.as_deref() == Some(*call)
                        && (p.hz - s.hz).abs() < tol
                })
            })
            .collect()
    }

    fn run_band(snr_db: f64, seconds: f64) -> (Vec<(KeySignal, &'static str)>, Vec<KeySpot>) {
        let mut b = band(0.0);
        let mut sigs: Vec<KeySignal> = Vec::new();
        for (s, _) in b.drain(..) {
            sigs.push(s);
        }
        let spots = skim(&mut sigs, sigma_for(snr_db, 1.0), seconds, KeySkimConfig::default());
        let calls = band(0.0).into_iter().map(|(_, c)| c);
        (sigs.into_iter().zip(calls).collect(), spots)
    }

    #[test]
    fn cw_psk31_and_rtty_across_the_window_are_found_and_their_calls_read() {
        let (sent, spots) = run_band(6.0, 26.0);
        report(&spots);
        let got = found(&sent, &spots, 12.0);
        for ((s, call), ok) in sent.iter().zip(&got) {
            assert!(ok, "{} {call} at {:.0} not read", s.mode.label(), s.hz);
        }
        // Nothing else: one spot a station.
        assert_eq!(spots.len(), sent.len(), "spots beside the stations");
        let w = spots.iter().find(|p| p.call.as_deref() == Some("K4ABC")).unwrap();
        assert!((20..=24).contains(&w.wpm), "{} WPM", w.wpm);
        assert!(w.cq());
        let g = spots.iter().find(|p| p.call.as_deref() == Some("G4XYZ")).unwrap();
        assert!(!g.cq() && g.wpm == 0);
    }

    /// Callsigns read against SNR (in 2.5 kHz), for the report; run with
    /// `cargo test --release -p decoders keyskim::tests::calls_read_against_snr
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn calls_read_against_snr() {
        eprintln!("{:>6} {:>5} {:>5} {:>5}", "SNR", "CW", "PSK", "RTTY");
        for snr in [10.0, 6.0, 3.0, 0.0, -3.0, -6.0, -9.0, -12.0, -15.0] {
            let (sent, spots) = run_band(snr, 26.0);
            let got = found(&sent, &spots, 15.0);
            let count = |m: KeyMode| {
                let n = sent.iter().filter(|(s, _)| s.mode == m).count();
                let k = sent.iter().zip(&got).filter(|((s, _), ok)| s.mode == m && **ok).count();
                format!("{k}/{n}")
            };
            let wrong = spots
                .iter()
                .filter(|p| p.call.is_some())
                .filter(|p| !sent.iter().any(|(_, c)| p.call.as_deref() == Some(*c)))
                .count();
            eprintln!(
                "{snr:>+6.0} {:>5} {:>5} {:>5}   {wrong} wrong calls, {} spots",
                count(KeyMode::Cw),
                count(KeyMode::Psk),
                count(KeyMode::Rtty),
                spots.len()
            );
        }
    }
}
