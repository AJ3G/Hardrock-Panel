//! The TX monitor (decision 83, after SDRoxide's `tx_analyzer` and
//! `make_tx_frame`): while the radio is keyed with something the panel is
//! sending, the scope shows that, analysed as it goes out, as ordinary
//! spectrum frames. The trace, the waterfall, the 3D surface and the zoom
//! all keep moving with nothing but our own audio in them.
//!
//! - **Every sample.** The audio taps (`audio::Player::recent`,
//!   `audio::Tone::recent`) give each sample sent since the last repaint;
//!   all of them go into one analyser ([`TxMonitor`], an [`sdr::Spectrum`]
//!   fed the audio as I/Q with Q at zero), at the scope's own frame rate,
//!   window and averaging, with half-overlapped segments so none is missed.
//!   A RIG TUNE carrier, whose audio the panel does not make, is fed as a
//!   steady carrier at the dial.
//! - **A window for a few kilohertz.** The transform is sized for what is on
//!   screen ([`monitor_fft_size`]), not the panadapter's FFT size, so it is
//!   full at once; the last [`MAX_FFT`] samples are kept and played into a
//!   new analyser when the size changes.
//! - **Where it lands.** Each frame is laid across the span shown
//!   ([`rf_frame`]): audio at `f` lands at the transmit dial plus `f` on USB,
//!   minus `f` on LSB; the rest of the span is at the band's last noise
//!   floor.
//! - **At a fixed strength** (decision 113). Each frame is drawn so the
//!   over's loudest audio ([`TxMonitor::reference`], a peak held over the
//!   over) sits [`TX_MON_BELOW_CEIL_DB`] under the top of the dB scale,
//!   whatever the panel's LEVEL and the radio's TX AUD: the shape of the
//!   audio is kept, its absolute level is not. The waterfall, the 3D surface
//!   and the trace all take our signal at that strength in the palette.

use std::collections::VecDeque;

use sdr::{Spectrum, SpectrumConfig, SpectrumFrame};

use super::view::Span;

/// How far below the top of the dB scale the loudest of our audio is drawn,
/// dB, whatever its real level (decision 113, AJ3G 11 Nov 2026: drawn at the
/// audio's own level, which with LEVEL at 50 % and a low TX AUD is well under
/// full scale, it had "not enough gain"). Just under the ceiling, so the
/// peak is the top of the palette without being clipped flat.
pub const TX_MON_BELOW_CEIL_DB: f32 = 3.0;
/// Audio quieter than this, dBFS, is silence: it is drawn at the floor, not
/// raised to the fixed strength (digital silence, the averaging dying away
/// after the over).
pub const TX_MON_SILENT_DBFS: f32 = -100.0;
/// How fast the held peak ([`TxMonitor::reference`]) lets go, dB a second:
/// slow, so a pause in speech or the fade at the end of the over shows as
/// the audio falling away, not as quiet audio raised to full strength.
const REF_DECAY_DB_PER_S: f32 = 6.0;
/// The audio drawn, Hz from the dial: an SSB transmitter passes nothing
/// above about 3.5 kHz (SDRoxide's `TX_MONITOR_VIEW_HZ`), and a carrier at
/// the dial is drawn with its skirt.
pub const AUDIO_MAX_HZ: f64 = 3500.0;
const AUDIO_MIN_HZ: f64 = -50.0;
/// The largest and smallest transform: 16384 points is a third of a second
/// at 48 kHz, 2.9 Hz a bin.
pub const MAX_FFT: usize = 16_384;
const MIN_FFT: usize = sdr::MIN_FFT_SIZE;
/// Points across the view a transform should resolve (SDRoxide's
/// `RESOLVED_PER_VIEW`).
const RESOLVED_PER_VIEW: f64 = 1024.0;
/// The rate a RIG TUNE carrier is made at.
const CARRIER_RATE: u32 = 48_000;

/// The transform for audio at `rate` shown across `view_hz`: enough points
/// to resolve [`RESOLVED_PER_VIEW`] across the view, a power of two from
/// 2048 to [`MAX_FFT`].
pub fn monitor_fft_size(rate: f64, view_hz: f64) -> usize {
    if !(rate > 0.0 && view_hz > 0.0) {
        return MIN_FFT;
    }
    let want = (rate / view_hz * RESOLVED_PER_VIEW).ceil().min(MAX_FFT as f64) as usize;
    want.next_power_of_two().clamp(MIN_FFT, MAX_FFT)
}

/// The analyser over what the panel sends, and the frames it has made
/// since they were last taken.
#[derive(Default)]
pub struct TxMonitor {
    spec: Option<Spectrum>,
    /// The rate and transform the analyser was made for.
    built: Option<(u32, usize, sdr::Window)>,
    /// The newest samples, for a new analyser to start full.
    hist: VecDeque<f32>,
    hist_rate: u32,
    frames: Vec<SpectrumFrame>,
    /// When a made-up carrier or silence was last made up to (the scope's
    /// clock).
    carrier_at: Option<f64>,
    /// The over's loudest audio lately, dBFS, and the frame rate it lets go
    /// at ([`TxMonitor::reference`]).
    held_peak: Option<f32>,
    fps: f32,
    scratch: Vec<f32>,
}

impl TxMonitor {
    /// Feed what was sent, `x` at `rate`, through an analyser set as `cfg`
    /// (its size from [`monitor_fft_size`]).
    pub fn feed(&mut self, rate: u32, x: &[f32], cfg: SpectrumConfig) {
        if rate == 0 {
            return;
        }
        self.carrier_at = None;
        self.prepare(rate, cfg);
        self.keep(x);
        let spec = self.spec.as_mut().expect("made by prepare");
        interleave(x, &mut self.scratch);
        spec.process(&self.scratch, 0.0, &mut self.frames);
    }

    /// A RIG TUNE carrier at the dial, made up to `now` (seconds, the
    /// scope's clock): full scale, so it reads like full-scale audio. The
    /// first call fills a whole window at once.
    pub fn feed_carrier(&mut self, now: f64, cfg: SpectrumConfig) {
        // A real constant of 1/2: DC at 1/2, brought to full scale with the
        // real tone's 6 dB as everything else is.
        self.made(now, 0.5, CARRIER_RATE, MAX_FFT, cfg);
    }

    /// Silence made up to `now`: what goes out once the panel stops sending
    /// while the monitor is still shown (the guard after an over), so the
    /// display moves on rather than stopping.
    pub fn feed_silence(&mut self, now: f64, cfg: SpectrumConfig) {
        let rate = if self.hist_rate > 0 { self.hist_rate } else { CARRIER_RATE };
        self.made(now, 0.0, rate, 0, cfg);
    }

    /// `level` made up to `now` at `rate`, `first` samples of it the first
    /// time.
    fn made(&mut self, now: f64, level: f32, rate: u32, first: usize, cfg: SpectrumConfig) {
        let n = match self.carrier_at {
            Some(t) => ((now - t).clamp(0.0, 0.25) * rate as f64) as usize,
            None => first,
        };
        self.carrier_at = Some(now);
        let x = vec![level; n];
        self.prepare(rate, cfg);
        self.keep(&x);
        let spec = self.spec.as_mut().expect("made by prepare");
        interleave(&x, &mut self.scratch);
        spec.process(&self.scratch, 0.0, &mut self.frames);
    }

    /// The frames made since the last call, oldest first: audio spectra,
    /// dBFS, centred on 0 Hz.
    pub fn take(&mut self) -> Vec<SpectrumFrame> {
        std::mem::take(&mut self.frames)
    }

    /// Nothing is being sent: forget the carrier's clock (the analyser and
    /// what it last heard stay, for the next over to start full).
    pub fn idle(&mut self) {
        self.carrier_at = None;
        self.held_peak = None;
    }

    /// The audio level, dBFS, to draw at the fixed strength for `audio`
    /// (one of [`TxMonitor::take`]'s frames, in order): the loudest the over
    /// has been, held, letting go at [`REF_DECAY_DB_PER_S`], and never below
    /// this frame's own peak. A steady signal (a data mode, a tune carrier)
    /// is so drawn with its peak at the fixed strength in every frame.
    pub fn reference(&mut self, audio: &SpectrumFrame) -> f32 {
        let peak = audio_peak_dbfs(audio);
        let decay = REF_DECAY_DB_PER_S / self.fps.max(1.0);
        let held = match (self.held_peak, peak) {
            (Some(h), Some(p)) => Some((h - decay).max(p)),
            (Some(h), None) => Some(h - decay),
            (None, p) => p,
        };
        self.held_peak = held.filter(|&h| h > TX_MON_SILENT_DBFS);
        self.held_peak.unwrap_or(0.0)
    }

    /// An analyser for `rate` and `cfg`: a new one, started from the kept
    /// samples, when the rate, size or window changed; else this one with
    /// the frame rate and averaging brought up to date.
    fn prepare(&mut self, rate: u32, cfg: SpectrumConfig) {
        let cfg = SpectrumConfig {
            fft_size: cfg.fft_size.clamp(MIN_FFT, MAX_FFT),
            overlap: sdr::Overlap::Half,
            peak_hold: false,
            calibration_db: 0.0,
            ..cfg
        };
        self.fps = cfg.fps;
        if rate != self.hist_rate {
            self.hist.clear();
            self.hist_rate = rate;
        }
        let want = (rate, cfg.fft_size, cfg.window);
        if self.built == Some(want)
            && let Some(s) = self.spec.as_mut()
        {
            if s.config() != cfg {
                s.set_config(cfg);
            }
            return;
        }
        let mut spec = Spectrum::new(cfg, rate as f64);
        // Start full: the newest samples kept, silence before them.
        let have = self.hist.len().min(cfg.fft_size);
        let mut x = vec![0.0f32; cfg.fft_size - have];
        x.extend(self.hist.iter().skip(self.hist.len() - have));
        interleave(&x, &mut self.scratch);
        let mut primed = Vec::new();
        spec.process(&self.scratch, 0.0, &mut primed);
        self.spec = Some(spec);
        self.built = Some(want);
    }

    fn keep(&mut self, x: &[f32]) {
        let skip = x.len().saturating_sub(MAX_FFT);
        self.hist.extend(&x[skip..]);
        while self.hist.len() > MAX_FFT {
            self.hist.pop_front();
        }
    }
}

/// Real samples as I/Q with Q at zero.
fn interleave(x: &[f32], out: &mut Vec<f32>) {
    out.clear();
    out.extend(x.iter().flat_map(|&v| [v, 0.0]));
}

/// The loudest of the audio we draw (from just under the dial to
/// [`AUDIO_MAX_HZ`]), dBFS; `None` when it is all silence
/// ([`TX_MON_SILENT_DBFS`]).
pub fn audio_peak_dbfs(audio: &SpectrumFrame) -> Option<f32> {
    let p = (0..audio.db.len())
        .filter(|&k| (AUDIO_MIN_HZ..=AUDIO_MAX_HZ).contains(&audio.bin_hz(k)))
        .map(|k| audio.db[k])
        .fold(f32::MIN, f32::max);
    (p > TX_MON_SILENT_DBFS).then_some(p)
}

/// Where and how a TX monitor frame is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lay {
    /// The transmit dial, the radio's own Hz.
    pub dial_hz: f64,
    /// Lower sideband: audio lands below the dial.
    pub lower: bool,
    /// The band the frame covers, and its bins.
    pub band: Span,
    pub bins: usize,
    /// The level everything not ours is drawn at (the band's noise floor),
    /// and the top of the dB scale, dBm.
    pub floor_dbm: f32,
    pub ceil_dbm: f32,
    /// The audio level, dBFS, drawn [`TX_MON_BELOW_CEIL_DB`] under the top of
    /// the scale ([`TxMonitor::reference`]).
    pub ref_dbfs: f32,
}

/// The audio spectrum `audio` (dBFS, centred on 0 Hz, as [`TxMonitor`]
/// makes it) as a spectrum frame across `lay.band`: each bin's audio
/// frequency from the dial by the sideband, the strongest audio bin it
/// covers (or the two either side of it, between, when it covers less than
/// one), raised so `lay.ref_dbfs` sits [`TX_MON_BELOW_CEIL_DB`] below the top
/// of the scale, never below the floor; outside the audio, the floor. A
/// frame of silence is the floor everywhere.
pub fn rf_frame(audio: &SpectrumFrame, lay: Lay, seq: u64) -> SpectrumFrame {
    let n = lay.bins.max(2);
    let m = if audio_peak_dbfs(audio).is_some() { audio.db.len() } else { 0 };
    let lift = lay.ceil_dbm - TX_MON_BELOW_CEIL_DB - lay.ref_dbfs;
    let ab = audio.bin_width();
    let a0 = audio.start_hz();
    let rb = lay.band.width / n as f64;
    let lo = lay.band.center - lay.band.width / 2.0;
    let sign = if lay.lower { -1.0 } else { 1.0 };
    let at = |p: f64| -> f32 {
        let i = p.floor().clamp(0.0, (m - 1) as f64) as usize;
        let j = (i + 1).min(m - 1);
        let t = (p - i as f64).clamp(0.0, 1.0) as f32;
        audio.db[i] + (audio.db[j] - audio.db[i]) * t
    };
    let db = (0..n)
        .map(|k| {
            let f = sign * (lo + k as f64 * rb - lay.dial_hz);
            if m < 2 || !(AUDIO_MIN_HZ..=AUDIO_MAX_HZ).contains(&f) {
                return lay.floor_dbm;
            }
            // The bin's span of audio, in audio bins.
            let p = (f - a0) / ab;
            let half = 0.5 * rb / ab;
            let v = if half <= 0.5 {
                at(p)
            } else {
                let i0 = (p - half).ceil().max(0.0) as usize;
                let i1 = ((p + half).floor() as usize).min(m - 1);
                audio.db[i0.min(i1)..=i1].iter().copied().fold(f32::MIN, f32::max)
            };
            (v + lift).max(lay.floor_dbm)
        })
        .collect();
    SpectrumFrame {
        center_hz: lay.band.center,
        span_hz: lay.band.width,
        db,
        peak: None,
        seq,
        // The analyser's own resolution, in this frame's bins.
        enbw: (audio.enbw as f64 * ab / rb) as f32,
        noise_dbm: lay.floor_dbm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(n: usize) -> SpectrumConfig {
        SpectrumConfig { fft_size: n, fps: 30.0, ..SpectrumConfig::default() }
    }

    fn tone(hz: f64, amp: f32, rate: u32, n: usize, from: usize) -> Vec<f32> {
        (from..from + n)
            .map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / rate as f64).cos() as f32)
            .collect()
    }

    /// The strongest bin at or above 0 Hz (a real tone is mirrored below).
    fn peak(f: &SpectrumFrame) -> (f64, f32) {
        let above = f.db.iter().enumerate().filter(|(k, _)| f.bin_hz(*k) >= -1e-6);
        let (k, v) = above.fold((0, f32::MIN), |a, (k, &v)| if v > a.1 { (k, v) } else { a });
        (f.bin_hz(k), v)
    }

    const SPAN: Span = Span { center: 14_100_000.0, width: 153_600.0 };

    /// Laid out to draw `a` at the fixed strength, as the scope does.
    fn lay(a: &SpectrumFrame, dial_hz: f64, lower: bool, band: Span, bins: usize) -> Lay {
        let ref_dbfs = TxMonitor::default().reference(a);
        Lay { dial_hz, lower, band, bins, floor_dbm: -125.0, ceil_dbm: -40.0, ref_dbfs }
    }

    /// Where the loudest of our audio is drawn.
    const TOP: f32 = -40.0 - TX_MON_BELOW_CEIL_DB;

    #[test]
    fn the_window_is_sized_for_the_view_not_the_panadapter() {
        // The whole span: the smallest; FT8's 3 kHz: about 3 Hz bins;
        // WSPR's 200 Hz: as fine as it goes, a third of a second.
        assert_eq!(monitor_fft_size(48_000.0, 153_600.0), 2048);
        assert_eq!(monitor_fft_size(48_000.0, 3000.0), 16_384);
        assert_eq!(monitor_fft_size(48_000.0, 12_000.0), 4096);
        assert_eq!(monitor_fft_size(48_000.0, 200.0), MAX_FFT);
        assert_eq!(monitor_fft_size(0.0, 200.0), 2048);
    }

    #[test]
    fn every_sample_is_analysed_at_the_scopes_frame_rate() {
        let mut m = TxMonitor::default();
        // One second in repaint-sized pieces: 30 frames, none lost or doubled.
        let mut at = 0;
        let mut frames = Vec::new();
        for _ in 0..60 {
            m.feed(48_000, &tone(1500.0, 0.5, 48_000, 800, at), cfg(4096));
            at += 800;
            frames.extend(m.take());
        }
        assert!((29..=31).contains(&frames.len()), "{}", frames.len());
        // The tone at its level (-6 dBFS, less the real tone's 6 dB here),
        // at 1500 Hz, once a window of it has gone out.
        for f in &frames[3..] {
            let (hz, db) = peak(f);
            assert!((hz - 1500.0).abs() < 15.0, "{hz}");
            assert!((db + 12.04).abs() < 1.0, "{db}");
        }
    }

    #[test]
    fn a_change_of_size_starts_the_new_analyser_full() {
        let mut m = TxMonitor::default();
        let mut at = 0;
        for _ in 0..20 {
            m.feed(48_000, &tone(1500.0, 1.0, 48_000, 1600, at), cfg(2048));
            at += 1600;
        }
        m.take();
        m.feed(48_000, &tone(1500.0, 1.0, 48_000, 1600, at), cfg(16_384));
        let f = m.take();
        assert!(!f.is_empty());
        assert_eq!(f[0].db.len(), 16_384);
        let (hz, db) = peak(&f[0]);
        assert!((hz - 1500.0).abs() < 6.0 && (db + 6.02).abs() < 1.0, "{hz} {db}");
    }

    #[test]
    fn our_audio_lands_from_the_transmit_dial_by_the_sideband() {
        let mut m = TxMonitor::default();
        m.feed(48_000, &tone(1234.0, 1.0, 48_000, 16_384, 0), cfg(16_384));
        let a = m.take().pop().unwrap();
        // USB: above the dial, its peak just under the top of the scale.
        let f = rf_frame(&a, lay(&a, 14_074_000.0, false, SPAN, 16_384), 1);
        let (hz, db) = peak(&f);
        assert!((hz - 14_075_234.0).abs() < 2.0 * f.bin_width(), "{hz}");
        assert!((db - TOP).abs() < 1.5, "{db}");
        // The rest of the span at the floor, the noise line there too.
        assert_eq!(f.db[0], -125.0);
        assert_eq!(*f.db.last().unwrap(), -125.0);
        assert_eq!(f.noise_dbm, -125.0);
        assert!(f.db.iter().all(|&v| v >= -125.0));
        // LSB: below it.
        let f = rf_frame(&a, lay(&a, 14_074_000.0, true, SPAN, 16_384), 2);
        assert!((peak(&f).0 - 14_072_766.0).abs() < 2.0 * f.bin_width());
        // A view zoomed on it: the tone between two bins still reads its
        // level, and is a few hertz wide, not a block.
        let zoom = Span { center: 14_075_234.0, width: 400.0 };
        let f = rf_frame(&a, lay(&a, 14_074_000.0, false, zoom, 4096), 3);
        let (hz, db) = peak(&f);
        assert!((hz - 14_075_234.0).abs() < 1.5, "{hz}");
        assert!((db - TOP).abs() < 1.5, "{db}");
        let lit = f.db.iter().filter(|&&v| v > TOP - 20.0).count() as f64 * f.bin_width();
        assert!(lit < 25.0, "{lit} Hz above -20 dB");
    }

    #[test]
    fn rig_tune_is_a_full_scale_carrier_at_the_dial_at_once() {
        let mut m = TxMonitor::default();
        m.feed_carrier(10.0, cfg(4096));
        let mut frames = m.take();
        m.feed_carrier(10.1, cfg(4096));
        frames.extend(m.take());
        assert!(!frames.is_empty(), "frames from the first repaint");
        let band = Span { center: 7_100_000.0, width: 153_600.0 };
        let f = rf_frame(
            frames.last().unwrap(),
            lay(frames.last().unwrap(), 7_100_000.0, true, band, 16_384),
            1,
        );
        let (hz, db) = peak(&f);
        assert!((hz - 7_100_000.0).abs() < 2.0 * f.bin_width(), "{hz}");
        assert!((db - TOP).abs() < 1.5, "{db}");
    }

    #[test]
    fn silence_is_the_floor_everywhere() {
        let mut m = TxMonitor::default();
        m.feed(48_000, &vec![0.0; 8000], cfg(2048));
        let a = m.take().pop().unwrap();
        let f = rf_frame(&a, lay(&a, 14_074_000.0, false, SPAN, 8192), 1);
        assert!(f.db.iter().all(|&v| v == -125.0));
        // After the over, silence is made up at the frame rate.
        m.feed_silence(5.0, cfg(2048));
        assert!(m.take().is_empty(), "nothing owed the first time");
        m.feed_silence(5.2, cfg(2048));
        assert!((5..=7).contains(&m.take().len()));
    }

    /// Decision 113: our signal is drawn at a fixed strength, whatever the
    /// LEVEL and TX AUD it went out at, with its shape kept.
    #[test]
    fn our_signal_is_drawn_just_under_the_ceiling_whatever_its_level() {
        let two = |amp: f32| -> SpectrumFrame {
            let mut m = TxMonitor::default();
            let x: Vec<f32> = tone(1000.0, amp, 48_000, 16_384, 0)
                .iter()
                .zip(tone(2000.0, amp * 0.1, 48_000, 16_384, 0))
                .map(|(a, b)| a + b)
                .collect();
            m.feed(48_000, &x, cfg(16_384));
            m.take().pop().unwrap()
        };
        let level_at = |f: &SpectrumFrame, hz: f64| {
            f.db.iter()
                .enumerate()
                .filter(|(k, _)| (f.bin_hz(*k) - hz).abs() < 30.0)
                .map(|(_, &v)| v)
                .fold(f32::MIN, f32::max)
        };
        for amp in [1.0, 0.05, 0.003] {
            let a = two(amp);
            let f = rf_frame(&a, lay(&a, 14_074_000.0, false, SPAN, 16_384), 1);
            let hi = level_at(&f, 14_075_000.0);
            let lo = level_at(&f, 14_076_000.0);
            assert!((hi - TOP).abs() < 1.5, "{amp}: {hi}");
            // The second tone, a tenth the amplitude, 20 dB under it.
            assert!((hi - lo - 20.0).abs() < 1.5, "{amp}: {lo}");
            assert_eq!(f.db[0], -125.0);
        }
    }

    #[test]
    fn the_reference_holds_the_overs_peak_and_lets_go_slowly() {
        let frame = |amp: f32| {
            let mut m = TxMonitor::default();
            m.feed(48_000, &tone(1500.0, amp, 48_000, 4096, 0), cfg(4096));
            m.take().pop().unwrap()
        };
        let (loud, quiet) = (frame(0.5), frame(0.05));
        let mut m = TxMonitor::default();
        m.prepare(48_000, cfg(4096));
        let r0 = m.reference(&loud);
        assert!((r0 - audio_peak_dbfs(&loud).unwrap()).abs() < 1e-3);
        // A quieter frame a moment later is drawn quieter, not raised.
        let r1 = m.reference(&quiet);
        assert!((r0 - r1 - 6.0 / 30.0).abs() < 1e-3, "{r0} {r1}");
        // Silence: nothing to draw, and the held peak goes in the end.
        let s = SpectrumFrame { db: vec![-200.0; 64], ..quiet.clone() };
        assert_eq!(audio_peak_dbfs(&s), None);
        let f =
            rf_frame(&s, Lay { ref_dbfs: r1, ..lay(&loud, 14_074_000.0, false, SPAN, 1024) }, 1);
        assert!(f.db.iter().all(|&v| v == -125.0));
        m.idle();
        assert_eq!(m.reference(&s), 0.0);
        assert_eq!(m.held_peak, None);
    }
}
