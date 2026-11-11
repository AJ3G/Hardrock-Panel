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
//!   floor. Full-scale audio sits [`TX_MON_HEADROOM_DB`] below the top of the
//!   dB scale, whatever the scale.

use std::collections::VecDeque;

use sdr::{Spectrum, SpectrumConfig, SpectrumFrame};

use super::view::Span;

/// How far below the top of the dB scale full-scale TX audio is drawn, dB:
/// about as bright as a strong received signal, never clamped to the
/// ceiling (SDRoxide's `TX_MON_HEADROOM_DB`).
pub const TX_MON_HEADROOM_DB: f32 = 30.0;
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
/// A real tone of amplitude `A` reads `A/2` at each of its two frequencies
/// in a complex transform: this brings full-scale audio back to 0 dBFS.
const REAL_GAIN_DB: f32 = 6.0206;

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
}

/// The audio spectrum `audio` (dBFS, centred on 0 Hz, as [`TxMonitor`]
/// makes it) as a spectrum frame across `lay.band`: each bin's audio
/// frequency from the dial by the sideband, the strongest audio bin it
/// covers (or the two either side of it, between, when it covers less than
/// one), raised so full scale sits [`TX_MON_HEADROOM_DB`] below the top of
/// the scale, never below the floor; outside the audio, the floor.
pub fn rf_frame(audio: &SpectrumFrame, lay: Lay, seq: u64) -> SpectrumFrame {
    let n = lay.bins.max(2);
    let m = audio.db.len();
    let lift = REAL_GAIN_DB + lay.ceil_dbm - TX_MON_HEADROOM_DB;
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

    fn lay(dial_hz: f64, lower: bool, band: Span, bins: usize) -> Lay {
        Lay { dial_hz, lower, band, bins, floor_dbm: -125.0, ceil_dbm: -40.0 }
    }

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
        // USB: above the dial, full scale 30 dB under the top of the scale.
        let f = rf_frame(&a, lay(14_074_000.0, false, SPAN, 16_384), 1);
        let (hz, db) = peak(&f);
        assert!((hz - 14_075_234.0).abs() < 2.0 * f.bin_width(), "{hz}");
        assert!((db - (-40.0 - TX_MON_HEADROOM_DB)).abs() < 1.5, "{db}");
        // The rest of the span at the floor, the noise line there too.
        assert_eq!(f.db[0], -125.0);
        assert_eq!(*f.db.last().unwrap(), -125.0);
        assert_eq!(f.noise_dbm, -125.0);
        assert!(f.db.iter().all(|&v| v >= -125.0));
        // LSB: below it.
        let f = rf_frame(&a, lay(14_074_000.0, true, SPAN, 16_384), 2);
        assert!((peak(&f).0 - 14_072_766.0).abs() < 2.0 * f.bin_width());
        // A view zoomed on it: the tone between two bins still reads its
        // level, and is a few hertz wide, not a block.
        let zoom = Span { center: 14_075_234.0, width: 400.0 };
        let f = rf_frame(&a, lay(14_074_000.0, false, zoom, 4096), 3);
        let (hz, db) = peak(&f);
        assert!((hz - 14_075_234.0).abs() < 1.5, "{hz}");
        assert!((db + 70.0).abs() < 1.5, "{db}");
        let lit = f.db.iter().filter(|&&v| v > -70.0 - 20.0).count() as f64 * f.bin_width();
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
        let f = rf_frame(frames.last().unwrap(), lay(7_100_000.0, true, band, 16_384), 1);
        let (hz, db) = peak(&f);
        assert!((hz - 7_100_000.0).abs() < 2.0 * f.bin_width(), "{hz}");
        assert!((db - (-70.0)).abs() < 1.5, "{db}");
    }

    #[test]
    fn silence_is_the_floor_everywhere() {
        let mut m = TxMonitor::default();
        m.feed(48_000, &vec![0.0; 8000], cfg(2048));
        let a = m.take().pop().unwrap();
        let f = rf_frame(&a, lay(14_074_000.0, false, SPAN, 8192), 1);
        assert!(f.db.iter().all(|&v| v == -125.0));
        // After the over, silence is made up at the frame rate.
        m.feed_silence(5.0, cfg(2048));
        assert!(m.take().is_empty(), "nothing owed the first time");
        m.feed_silence(5.2, cfg(2048));
        assert!((5..=7).contains(&m.take().len()));
    }
}
