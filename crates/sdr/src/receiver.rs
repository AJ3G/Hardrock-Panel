//! One software receiver inside the span, SDR Console style: the radio's centre
//! stays where it is and this tunes anywhere within the I/Q it delivers.
//!
//! The chain is SDRoxide's: an NCO moves the dial to DC at the device rate, a
//! ladder of half-band /2 stages takes the rate down to 384 or 192 kHz, one FIR
//! decimator lands on exactly 48 kHz, and the demodulator's band-pass sets the
//! passband there. Every ELAD rate is 192 kHz times a power of two, so the
//! audio is 48 kHz with no resampling.
//!
//! # Levels
//!
//! A carrier of complex magnitude `A` (0 dBFS = 1.0) in the passband reads
//! `20·log10(A)` dBFS on [`Receiver::level_dbfs`] — the scale the spectrum
//! uses — and, demodulated in a sideband mode with the AGC off at 0 dB gain,
//! comes out as a tone of amplitude `A`.

use crate::agc::Agc;
use crate::ddc::Ddc;
use crate::demod::{AmDemod, Demodulator, FmDemod, SsbDemod};
use crate::types::{AgcMode, Complex32, MAX_MANUAL_GAIN_DB, Mode};

/// The audio rate, in Hz.
pub const AUDIO_RATE: f64 = 48_000.0;

/// The device rates this receiver is built and tested for: every rate an ELAD
/// DDC delivers.
pub const SUPPORTED_RATES: [u32; 6] = elad_iq::ELAD_SAMPLE_RATES;

/// How [`Receiver`] decimates from `in_rate`: the number of half-band /2
/// stages, the factor of the final FIR decimator, and the rate that comes out.
pub fn decimation_plan(in_rate: f64) -> (usize, usize, f64) {
    let mut rate = in_rate;
    let mut halfbands = 0;
    while rate > AUDIO_RATE * 8.0 {
        halfbands += 1;
        rate /= 2.0;
    }
    let m = (rate / AUDIO_RATE).round().max(1.0) as usize;
    (halfbands, m, rate / m as f64)
}

/// The software receiver.
pub struct Receiver {
    rate: f64,
    ddc: Ddc,
    offset_hz: f64,
    mode: Mode,
    passband: (f32, f32),
    demod: Box<dyn Demodulator>,
    agc: Agc,
    agc_mode: AgcMode,
    gain_db: f32,
    calibration_db: f32,
    iq: Vec<Complex32>,
    channel: Vec<Complex32>,
}

impl Receiver {
    /// A receiver for I/Q at `sample_rate`, on the centre, in USB.
    pub fn new(sample_rate: f64) -> Self {
        let mode = Mode::Usb;
        let passband = mode.default_passband();
        let mut r = Receiver {
            rate: sample_rate,
            ddc: Ddc::new(sample_rate, AUDIO_RATE),
            offset_hz: 0.0,
            mode,
            passband,
            demod: make_demod(mode, passband),
            agc: Agc::new(AUDIO_RATE),
            agc_mode: AgcMode::Med,
            gain_db: 20.0,
            calibration_db: 0.0,
            iq: Vec::new(),
            channel: Vec::new(),
        };
        r.agc.set_mode(r.agc_mode);
        r.agc.set_manual_gain_db(r.gain_db);
        r
    }

    pub fn sample_rate(&self) -> f64 {
        self.rate
    }

    /// The rate the decimators land on: [`AUDIO_RATE`] for every supported
    /// input rate.
    pub fn audio_rate(&self) -> f64 {
        self.ddc.out_rate()
    }

    /// Rebuild the chain for a new device rate, keeping every setting.
    pub fn set_sample_rate(&mut self, rate: f64) {
        if rate == self.rate {
            return;
        }
        self.rate = rate;
        self.ddc = Ddc::new(rate, AUDIO_RATE);
        self.ddc.set_offset_hz(self.offset_hz);
        self.demod = make_demod(self.mode, self.passband);
    }

    /// Where the dial is, in Hz from the centre. Kept inside the span.
    pub fn set_offset_hz(&mut self, hz: f64) {
        let half = self.rate / 2.0;
        self.offset_hz = hz.clamp(-half, half);
        self.ddc.set_offset_hz(self.offset_hz);
    }

    pub fn offset_hz(&self) -> f64 {
        self.offset_hz
    }

    /// Change the mode. The passband becomes that mode's default.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.passband = mode.default_passband();
        self.demod = make_demod(mode, self.passband);
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The passband, `[low, high]` in Hz relative to the dial — USB
    /// `100..2700`, LSB `-2700..-100`. For LSB and CWR a pair given as positive
    /// magnitudes (`100..2700`) is mirrored below the dial.
    pub fn set_passband(&mut self, low_hz: f32, high_hz: f32) {
        let (mut lo, mut hi) = (low_hz.min(high_hz), low_hz.max(high_hz));
        if self.mode.is_lower() && lo >= 0.0 && hi > 0.0 {
            (lo, hi) = (-hi, -lo);
        }
        // Nothing wider than the 48 kHz channel can hold.
        let edge = (AUDIO_RATE / 2.0) as f32 * 0.9;
        lo = lo.clamp(-edge, edge);
        hi = hi.clamp(-edge, edge);
        if hi - lo < 50.0 {
            hi = lo + 50.0;
        }
        self.passband = (lo, hi);
        self.demod.set_filter(lo, hi);
    }

    pub fn passband(&self) -> (f32, f32) {
        self.passband
    }

    pub fn set_agc(&mut self, mode: AgcMode) {
        // Switching off carries the level the AGC had settled at, so the
        // audio does not jump.
        if mode == AgcMode::Off && self.agc_mode != AgcMode::Off {
            self.gain_db = self.agc.gain_db().clamp(0.0, MAX_MANUAL_GAIN_DB);
            self.agc.set_manual_gain_db(self.gain_db);
        }
        self.agc_mode = mode;
        self.agc.set_mode(mode);
    }

    pub fn agc(&self) -> AgcMode {
        self.agc_mode
    }

    /// The fixed gain applied while the AGC is off, in dB (0 to 120).
    pub fn set_gain_db(&mut self, db: f32) {
        self.gain_db = db.clamp(0.0, MAX_MANUAL_GAIN_DB);
        self.agc.set_manual_gain_db(self.gain_db);
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// The most the AGC may add, in dB.
    pub fn set_max_gain_db(&mut self, db: f32) {
        self.agc.set_max_gain_db(db);
    }

    /// Added to dBFS to give dBm at the antenna; the same number the spectrum
    /// is calibrated with.
    pub fn set_calibration_db(&mut self, db: f32) {
        self.calibration_db = db;
    }

    /// The passband's power before the AGC, in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.power_dbfs()
    }

    /// The passband's power in dBm, for an S-meter.
    pub fn level_dbm(&self) -> f32 {
        self.level_dbfs() + self.calibration_db
    }

    /// Consume interleaved I/Q at the device rate and append 48 kHz mono audio
    /// to `out`.
    pub fn process(&mut self, iq: &[f32], out: &mut Vec<f32>) {
        self.iq.clear();
        self.iq.extend(iq.chunks_exact(2).map(|p| Complex32::new(p[0], p[1])));
        self.process_complex_inner(out);
    }

    /// [`Self::process`] for samples already complex.
    pub fn process_complex(&mut self, iq: &[Complex32], out: &mut Vec<f32>) {
        self.iq.clear();
        self.iq.extend_from_slice(iq);
        self.process_complex_inner(out);
    }

    fn process_complex_inner(&mut self, out: &mut Vec<f32>) {
        self.channel.clear();
        self.ddc.process(&self.iq, &mut self.channel);
        let start = out.len();
        self.demod.process(&self.channel, out);
        self.agc.process(&mut out[start..]);
    }
}

fn make_demod(mode: Mode, (lo, hi): (f32, f32)) -> Box<dyn Demodulator> {
    match mode {
        Mode::Lsb | Mode::Usb | Mode::Cw | Mode::Cwr => Box::new(SsbDemod::new(AUDIO_RATE, lo, hi)),
        Mode::Am => Box::new(AmDemod::new(AUDIO_RATE, lo, hi)),
        Mode::Fm => Box::new(FmDemod::new(AUDIO_RATE, lo, hi)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{IqSource, SynthSignal, SyntheticSource};

    /// Run `secs` of `src` through `rx`, returning the audio.
    fn run(rx: &mut Receiver, src: &mut dyn IqSource, secs: f64) -> Vec<f32> {
        let total = (secs * src.sample_rate()) as usize;
        let mut buf = vec![0.0f32; 2 * 32_768];
        let mut audio = Vec::new();
        let mut done = 0;
        while done < total {
            let want = (2 * (total - done)).min(buf.len());
            let n = src.read(&mut buf[..want]);
            rx.process(&buf[..n], &mut audio);
            done += n / 2;
        }
        audio
    }

    /// Amplitude and frequency of the strongest tone in `x` (after the first
    /// `skip` samples), by a Goertzel scan around `near`.
    fn tone(x: &[f32], near: f64) -> (f64, f32) {
        let mut best = (0.0, 0.0f32);
        let n = x.len() as f64;
        let mut f = near - 100.0;
        while f <= near + 100.0 {
            let w = std::f64::consts::TAU * f / AUDIO_RATE;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, &s) in x.iter().enumerate() {
                re += s as f64 * (w * i as f64).cos();
                im -= s as f64 * (w * i as f64).sin();
            }
            let a = (2.0 * (re * re + im * im).sqrt() / n) as f32;
            if a > best.1 {
                best = (f, a);
            }
            f += 2.0;
        }
        best
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn fixed_gain(rx: &mut Receiver) {
        rx.set_agc(AgcMode::Off);
        rx.set_gain_db(0.0);
    }

    #[test]
    fn every_supported_rate_decimates_to_exactly_48k() {
        for rate in SUPPORTED_RATES {
            let (hb, m, out) = decimation_plan(rate as f64);
            assert_eq!(out, AUDIO_RATE, "{rate}");
            assert_eq!((1usize << hb) * m, rate as usize / 48_000, "{rate}");
            assert!(m == 4 || m == 8, "{rate}: final factor {m}");
            let rx = Receiver::new(rate as f64);
            assert_eq!(rx.audio_rate(), AUDIO_RATE);
        }
    }

    #[test]
    fn every_supported_rate_yields_the_right_tone_at_the_right_level_and_length() {
        for rate in SUPPORTED_RATES {
            let rate = rate as f64;
            // Dial at +12.5 kHz, carrier 1 kHz above it, -30 dBFS.
            let mut src = SyntheticSource::new(
                rate,
                0.0,
                -140.0,
                vec![SynthSignal::carrier(13_500.0, -30.0)],
            );
            let mut rx = Receiver::new(rate);
            rx.set_offset_hz(12_500.0);
            fixed_gain(&mut rx);
            let audio = run(&mut rx, &mut src, 0.5);
            // 0.5 s in, 24000 samples out, less the filters' fill.
            assert!(
                audio.len() > 23_000 && audio.len() <= 24_000,
                "{rate}: {} samples",
                audio.len()
            );
            let tail = &audio[audio.len() - 12_000..];
            let (f, a) = tone(tail, 1000.0);
            assert!((f - 1000.0).abs() <= 2.0, "{rate}: tone at {f} Hz");
            let want = 10f32.powf(-30.0 / 20.0);
            let db = 20.0 * (a / want).log10();
            assert!(db.abs() < 0.5, "{rate}: tone {db:+.2} dB from the carrier");
            assert!((rx.level_dbfs() + 30.0).abs() < 0.5, "{rate}: meter {}", rx.level_dbfs());
        }
    }

    #[test]
    fn usb_at_12k5_hears_a_1khz_tone_and_rejects_what_is_outside_the_passband() {
        let rate = 192_000.0;
        let want = 10f32.powf(-40.0 / 20.0);
        let level = |carrier_hz: f64| {
            let mut src = SyntheticSource::new(
                rate,
                0.0,
                -200.0,
                vec![SynthSignal::carrier(carrier_hz, -40.0)],
            );
            let mut rx = Receiver::new(rate);
            rx.set_offset_hz(12_500.0);
            fixed_gain(&mut rx);
            let audio = run(&mut rx, &mut src, 0.4);
            rms(&audio[audio.len() - 9600..]) * std::f32::consts::SQRT_2
        };
        let inside = level(13_500.0);
        assert!((20.0 * (inside / want).log10()).abs() < 0.5);
        // The opposite sideband, 1 kHz below the dial; 4 kHz above it, past the
        // top of the passband; 30 kHz away, past the channel; and at the centre.
        for hz in [11_500.0, 16_500.0, 42_500.0, 0.0, -12_500.0] {
            let out = level(hz);
            let rej = 20.0 * (inside / out.max(1e-12)).log10();
            assert!(rej > 50.0, "carrier at {hz} Hz only {rej:.1} dB down");
        }
    }

    #[test]
    fn lsb_hears_below_the_dial_and_a_positive_passband_is_mirrored() {
        let rate = 384_000.0;
        let mut rx = Receiver::new(rate);
        rx.set_mode(Mode::Lsb);
        assert_eq!(rx.passband(), (-2700.0, -100.0));
        rx.set_passband(200.0, 2400.0);
        assert_eq!(rx.passband(), (-2400.0, -200.0));
        rx.set_offset_hz(-50_000.0);
        fixed_gain(&mut rx);
        let mut src =
            SyntheticSource::new(rate, 0.0, -200.0, vec![SynthSignal::carrier(-51_200.0, -20.0)]);
        let audio = run(&mut rx, &mut src, 0.3);
        let (f, a) = tone(&audio[audio.len() - 9600..], 1200.0);
        assert!((f - 1200.0).abs() <= 2.0);
        assert!((20.0 * a.log10() + 20.0).abs() < 0.5);
    }

    #[test]
    fn am_and_fm_demodulate() {
        let rate = 768_000.0;
        // AM: a carrier and two sidebands 1 kHz either side at half its
        // amplitude each is 100 % modulation.
        let sigs = vec![
            SynthSignal::carrier(100_000.0, -20.0),
            SynthSignal::carrier(101_000.0, -26.02),
            SynthSignal::carrier(99_000.0, -26.02),
        ];
        let mut src = SyntheticSource::new(rate, 0.0, -200.0, sigs);
        let mut rx = Receiver::new(rate);
        rx.set_mode(Mode::Am);
        rx.set_offset_hz(100_000.0);
        fixed_gain(&mut rx);
        let audio = run(&mut rx, &mut src, 0.3);
        let (f, a) = tone(&audio[audio.len() - 9600..], 1000.0);
        assert!((f - 1000.0).abs() <= 2.0, "AM tone at {f}");
        assert!((a - 0.1).abs() < 0.01, "AM tone amplitude {a}");

        // FM: a carrier 1 kHz off the dial is DC after the discriminator, and
        // the high-pass takes it out — what is left is near silence.
        let mut src =
            SyntheticSource::new(rate, 0.0, -200.0, vec![SynthSignal::carrier(-199_000.0, -20.0)]);
        let mut rx = Receiver::new(rate);
        rx.set_mode(Mode::Fm);
        rx.set_offset_hz(-200_000.0);
        fixed_gain(&mut rx);
        let audio = run(&mut rx, &mut src, 0.5);
        assert!(rms(&audio[audio.len() - 4800..]) < 1e-3);
        assert!((rx.level_dbfs() + 20.0).abs() < 0.5);
    }

    #[test]
    fn the_agc_brings_a_weak_signal_up_to_a_steady_level() {
        let rate = 192_000.0;
        let mut out = Vec::new();
        for level in [-90.0, -50.0] {
            let mut src =
                SyntheticSource::new(rate, 0.0, -200.0, vec![SynthSignal::carrier(1_000.0, level)]);
            let mut rx = Receiver::new(rate);
            rx.set_agc(AgcMode::Fast);
            rx.set_calibration_db(-20.0);
            let audio = run(&mut rx, &mut src, 1.5);
            out.push(rms(&audio[audio.len() - 4800..]));
            assert!((rx.level_dbm() - (level - 20.0)).abs() < 0.5);
        }
        assert!(out[0] > 0.1 && out[1] > 0.1, "{out:?}");
        assert!((20.0 * (out[0] / out[1]).log10()).abs() < 1.0, "{out:?}");
    }

    #[test]
    fn the_offset_stays_inside_the_span() {
        let mut rx = Receiver::new(192_000.0);
        rx.set_offset_hz(500_000.0);
        assert_eq!(rx.offset_hz(), 96_000.0);
        rx.set_sample_rate(1_536_000.0);
        rx.set_offset_hz(500_000.0);
        assert_eq!(rx.offset_hz(), 500_000.0);
    }

    /// Throughput at every rate, in release:
    /// `cargo test --release -p sdr -- --ignored --nocapture throughput`.
    #[test]
    #[ignore]
    fn throughput() {
        use crate::spectrum::{Spectrum, SpectrumConfig};
        for rate in SUPPORTED_RATES {
            let rate = rate as f64;
            let mut src =
                SyntheticSource::new(rate, 0.0, -60.0, vec![SynthSignal::usb(12_500.0, -40.0)]);
            let secs = 2.0;
            let mut blocks = Vec::new();
            let mut buf = vec![0.0f32; 2 * 65_536];
            let mut have = 0usize;
            while (have as f64) < secs * rate {
                let n = src.read(&mut buf);
                blocks.push(buf[..n].to_vec());
                have += n / 2;
            }
            let mut rx = Receiver::new(rate);
            rx.set_offset_hz(12_500.0);
            let mut sp = Spectrum::new(
                SpectrumConfig { fft_size: 65_536, fps: 30.0, ..Default::default() },
                rate,
            );
            let mut audio = Vec::new();
            let mut frames = Vec::new();
            let t = std::time::Instant::now();
            for b in &blocks {
                rx.process(b, &mut audio);
                audio.clear();
            }
            let rx_s = t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            for b in &blocks {
                sp.process(b, 0.0, &mut frames);
                frames.clear();
            }
            let sp_s = t.elapsed().as_secs_f64();
            println!(
                "{:>5} kHz: receiver {:6.1}x real time ({:5.1} % of a core), \
                 spectrum 64k@30fps {:6.1}x ({:5.1} %)",
                rate / 1000.0,
                secs / rx_s,
                100.0 * rx_s / secs,
                secs / sp_s,
                100.0 * sp_s / secs
            );
        }
    }
}
