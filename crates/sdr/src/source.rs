//! Where the I/Q comes from: the ELAD radio, or a synthetic band for tests and
//! for the panel's demo mode.

use std::time::{Duration, Instant};

use elad_iq::EladHandle;

/// A stream of interleaved I/Q (`I0, Q0, I1, Q1, …`, full scale 1.0).
pub trait IqSource: Send {
    /// The complex sample rate, in Hz.
    fn sample_rate(&self) -> f64;
    /// The frequency at the middle of the span right now, in Hz.
    fn center_hz(&self) -> f64;
    /// Ask the source to move its centre. Not every source can.
    fn set_center_hz(&mut self, _hz: f64) {}
    /// Fill `out` with as many interleaved floats as are ready (always an even
    /// count) and return how many. Zero means nothing is ready yet; the caller
    /// waits a little and asks again.
    fn read(&mut self, out: &mut [f32]) -> usize;
    /// Whether the source is still delivering, or could.
    fn is_alive(&self) -> bool {
        true
    }
    /// A one-line description for the UI.
    fn label(&self) -> String;
}

/// The ELAD radio as an [`IqSource`].
pub struct EladSource {
    handle: EladHandle,
    center_hz: f64,
}

impl EladSource {
    /// Wrap an open handle that was opened at `center_hz`.
    pub fn new(handle: EladHandle, center_hz: f64) -> Self {
        EladSource { handle, center_hz }
    }

    pub fn handle(&self) -> &EladHandle {
        &self.handle
    }

    pub fn handle_mut(&mut self) -> &mut EladHandle {
        &mut self.handle
    }

    /// Release the device and hand back the handle.
    pub fn into_handle(self) -> EladHandle {
        self.handle
    }
}

impl IqSource for EladSource {
    fn sample_rate(&self) -> f64 {
        self.handle.sample_rate()
    }

    /// The radio's own dial when it has reported one (an FDM-DUO following its
    /// knob), else the centre last asked for.
    fn center_hz(&self) -> f64 {
        self.handle.tuned_hz().unwrap_or(self.center_hz)
    }

    fn set_center_hz(&mut self, hz: f64) {
        self.center_hz = hz;
        self.handle.set_center_hz(hz);
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        self.handle.rx_read(out)
    }

    fn is_alive(&self) -> bool {
        self.handle.is_alive()
    }

    fn label(&self) -> String {
        self.handle.label.clone()
    }
}

/// One signal in a [`SyntheticSource`].
#[derive(Debug, Clone, PartialEq)]
pub struct SynthSignal {
    /// Offset from the centre, in Hz: the carrier, or for SSB the dial.
    pub offset_hz: f64,
    /// Level in dBFS (complex magnitude; for SSB the peak of the envelope).
    pub level_dbfs: f32,
    pub kind: SynthKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthKind {
    /// An unmodulated carrier.
    Carrier,
    /// Speech-like upper sideband: three tones above the dial, syllabically
    /// keyed on and off.
    Usb,
    /// The same below the dial.
    Lsb,
    /// A carrier keyed in a slow Morse-like rhythm.
    Cw,
}

impl SynthSignal {
    pub fn carrier(offset_hz: f64, level_dbfs: f32) -> Self {
        SynthSignal { offset_hz, level_dbfs, kind: SynthKind::Carrier }
    }
    pub fn usb(offset_hz: f64, level_dbfs: f32) -> Self {
        SynthSignal { offset_hz, level_dbfs, kind: SynthKind::Usb }
    }
    pub fn lsb(offset_hz: f64, level_dbfs: f32) -> Self {
        SynthSignal { offset_hz, level_dbfs, kind: SynthKind::Lsb }
    }
    pub fn cw(offset_hz: f64, level_dbfs: f32) -> Self {
        SynthSignal { offset_hz, level_dbfs, kind: SynthKind::Cw }
    }
}

/// A complex oscillator walked by recurrence, renormalised now and then.
#[derive(Clone)]
struct Osc {
    re: f64,
    im: f64,
    c: f64,
    s: f64,
}

impl Osc {
    fn new(hz: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * hz / rate;
        Osc { re: 1.0, im: 0.0, c: w.cos(), s: w.sin() }
    }
    #[inline]
    fn next(&mut self) -> (f32, f32) {
        let out = (self.re as f32, self.im as f32);
        let re = self.re * self.c - self.im * self.s;
        self.im = self.re * self.s + self.im * self.c;
        self.re = re;
        out
    }
    fn renorm(&mut self) {
        let m = (self.re * self.re + self.im * self.im).sqrt();
        self.re /= m;
        self.im /= m;
    }
}

struct Voice {
    oscs: Vec<(Osc, f32)>,
    amp: f32,
    kind: SynthKind,
}

/// SSB "speech": tones at these audio frequencies, with these shares of the
/// envelope.
const SSB_TONES: [(f64, f32); 3] = [(450.0, 0.5), (1100.0, 0.3), (1900.0, 0.2)];

/// Noise plus a few signals at given offsets, at any rate.
///
/// Optionally paced to real time, which is what the panel's demo mode wants;
/// unpaced, as the tests use it, it produces samples as fast as it is asked.
pub struct SyntheticSource {
    rate: f64,
    center_hz: f64,
    /// Standard deviation of each of I and Q.
    sigma: f32,
    voices: Vec<Voice>,
    rng: u64,
    /// Samples produced, for keying and pacing.
    t: u64,
    realtime: Option<Instant>,
    /// Pacing: never more than this much at once.
    max_block: usize,
}

impl SyntheticSource {
    /// `noise_dbfs` is the total complex noise power over the whole span.
    pub fn new(rate: f64, center_hz: f64, noise_dbfs: f32, signals: Vec<SynthSignal>) -> Self {
        let sigma = (10f32.powf(noise_dbfs / 10.0) / 2.0).sqrt();
        let voices = signals
            .iter()
            .map(|s| {
                let amp = 10f32.powf(s.level_dbfs / 20.0);
                let oscs = match s.kind {
                    SynthKind::Carrier | SynthKind::Cw => vec![(Osc::new(s.offset_hz, rate), 1.0)],
                    SynthKind::Usb | SynthKind::Lsb => {
                        let sign = if s.kind == SynthKind::Usb { 1.0 } else { -1.0 };
                        SSB_TONES
                            .iter()
                            .map(|&(f, share)| (Osc::new(s.offset_hz + sign * f, rate), share))
                            .collect()
                    }
                };
                Voice { oscs, amp, kind: s.kind }
            })
            .collect();
        SyntheticSource {
            rate,
            center_hz,
            sigma,
            voices,
            rng: 0x9E37_79B9_7F4A_7C15,
            t: 0,
            realtime: None,
            max_block: 1 << 16,
        }
    }

    /// The panel's demo band: 192 kHz around 14.200 MHz with a few stations.
    pub fn demo(rate: f64, center_hz: f64) -> Self {
        let sigs = vec![
            SynthSignal::usb(-61_000.0, -50.0),
            SynthSignal::usb(12_500.0, -40.0),
            SynthSignal::cw(-25_300.0, -65.0),
            SynthSignal::lsb(43_000.0, -75.0),
            SynthSignal::carrier(80_000.0, -85.0),
        ];
        SyntheticSource::new(rate, center_hz, -60.0, sigs).paced()
    }

    /// Deliver samples no faster than real time.
    pub fn paced(mut self) -> Self {
        self.realtime = Some(Instant::now());
        self
    }

    /// Gaussian-ish noise: the sum of four uniforms, scaled to unit variance.
    #[inline]
    fn noise(&mut self) -> f32 {
        let mut acc = 0u32;
        let mut x = self.rng;
        for _ in 0..2 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            acc = acc.wrapping_add((x & 0xFFFF) as u32).wrapping_add(((x >> 16) & 0xFFFF) as u32);
        }
        self.rng = x;
        // Four uniforms on 0..65536: mean 131070, variance 4·65536²/12.
        (acc as f32 - 131_070.0) * (1.0 / 37_837.0)
    }

    /// Whether a keyed voice is on at sample `t`.
    fn keyed(kind: SynthKind, t: u64, rate: f64) -> f32 {
        let secs = t as f64 / rate;
        match kind {
            SynthKind::Carrier => 1.0,
            // "Syllables": on 0.25 s of every 0.4 s.
            SynthKind::Usb | SynthKind::Lsb => {
                if (secs % 0.4) < 0.25 {
                    1.0
                } else {
                    0.0
                }
            }
            // 60 ms elements: dit dah dit, gap.
            SynthKind::Cw => {
                const PATTERN: [u8; 12] = [1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0, 0];
                PATTERN[((secs / 0.06) as usize) % PATTERN.len()] as f32
            }
        }
    }
}

impl IqSource for SyntheticSource {
    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn center_hz(&self) -> f64 {
        self.center_hz
    }

    fn set_center_hz(&mut self, hz: f64) {
        self.center_hz = hz;
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let mut want = (out.len() / 2).min(self.max_block);
        if let Some(start) = self.realtime {
            let due = (start.elapsed().as_secs_f64() * self.rate) as u64;
            let ready = due.saturating_sub(self.t) as usize;
            if ready == 0 {
                std::thread::sleep(Duration::from_millis(2));
                return 0;
            }
            want = want.min(ready);
        }
        let rate = self.rate;
        for (i, p) in out[..want * 2].chunks_exact_mut(2).enumerate() {
            let t = self.t + i as u64;
            let (mut re, mut im) = (self.noise() * self.sigma, self.noise() * self.sigma);
            for v in &mut self.voices {
                // The envelope is a gate, moved on a 64-sample grid.
                let gate = SyntheticSource::keyed(v.kind, t - t % 64, rate);
                for (o, share) in &mut v.oscs {
                    let (a, b) = o.next();
                    let g = v.amp * *share * gate;
                    re += a * g;
                    im += b * g;
                }
            }
            p[0] = re;
            p[1] = im;
        }
        self.t += want as u64;
        for v in &mut self.voices {
            for (o, _) in &mut v.oscs {
                o.renorm();
            }
        }
        want * 2
    }

    fn label(&self) -> String {
        format!("Demo signals ({} kHz)", self.rate / 1000.0)
    }
}
