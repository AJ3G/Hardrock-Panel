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
    /// The radio's dial as last read back and believed (see
    /// [`plausible_dial`]).
    dial: std::cell::Cell<Option<f64>>,
}

/// Whether a dial read back from the radio can be where its I/Q is: inside
/// what the receiver covers. A transmission has been seen to be the time a
/// read-back is least trustworthy; a 0 or a number off the end of the
/// receiver's range is ignored and the last good one kept, rather than the
/// whole span (and the receiver with it) jumping there.
pub fn plausible_dial(hz: f64, range: (f64, f64)) -> bool {
    hz.is_finite() && hz >= range.0 && hz <= range.1
}

impl EladSource {
    /// Wrap an open handle that was opened at `center_hz`.
    pub fn new(handle: EladHandle, center_hz: f64) -> Self {
        EladSource { handle, center_hz, dial: std::cell::Cell::new(None) }
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
        if let Some(hz) = self.handle.tuned_hz()
            && plausible_dial(hz, self.handle.model.rx_range_hz())
        {
            self.dial.set(Some(hz));
        }
        self.dial.get().unwrap_or(self.center_hz)
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

/// What a radio's I/Q might do while it transmits, for [`TxSim`]. Nothing
/// here has been measured on an FDM-DUO; these are the plausible cases the
/// scope has to come through cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TxEffects {
    /// The stream stops (the driver keeps running, nothing arrives).
    pub gap: bool,
    /// The samples are replaced by a carrier this strong, dBFS (0 is full
    /// scale), with the noise raised to `strong - 30`, and clipped to ±1.
    pub strong_dbfs: Option<f32>,
    /// The samples are all exactly zero (a receiver muted for the over).
    pub zeros: bool,
    /// The samples are not numbers (a converter fed garbage).
    pub nan: bool,
    /// The centre the source reports: another VFO, 0 or garbage.
    pub centre_hz: Option<f64>,
}

/// Wraps a source and, between `key_s` and `unkey_s` seconds of stream time
/// (and again every `every_s`, when set), does to it what a transmission
/// might: see [`TxEffects`]. Its `keyed` flag says when it is "on the air",
/// for a test or the demo to drive the scope's `transmitting` from.
pub struct TxSim<S: IqSource> {
    inner: S,
    pub key_s: f64,
    pub unkey_s: f64,
    pub every_s: Option<f64>,
    pub effects: TxEffects,
    keyed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Samples taken from the inner source: the script's clock.
    t: u64,
    phase: f64,
}

impl<S: IqSource> TxSim<S> {
    pub fn new(inner: S, key_s: f64, unkey_s: f64, effects: TxEffects) -> Self {
        TxSim {
            inner,
            key_s,
            unkey_s,
            every_s: None,
            effects,
            keyed: Default::default(),
            t: 0,
            phase: 0.0,
        }
    }

    /// Key again every `secs` (a run of overs).
    pub fn repeat(mut self, secs: f64) -> Self {
        self.every_s = Some(secs);
        self
    }

    /// Whether it is transmitting now, shared.
    pub fn keyed(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.keyed.clone()
    }

    fn on_air(&self) -> bool {
        let mut s = self.t as f64 / self.inner.sample_rate().max(1.0);
        if let Some(e) = self.every_s.filter(|e| *e > 0.0) {
            s %= e;
        }
        s >= self.key_s && s < self.unkey_s
    }
}

impl<S: IqSource> IqSource for TxSim<S> {
    fn sample_rate(&self) -> f64 {
        self.inner.sample_rate()
    }

    fn center_hz(&self) -> f64 {
        match self.effects.centre_hz {
            Some(hz) if self.keyed.load(std::sync::atomic::Ordering::Relaxed) => hz,
            _ => self.inner.center_hz(),
        }
    }

    fn set_center_hz(&mut self, hz: f64) {
        self.inner.set_center_hz(hz);
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let n = self.inner.read(out);
        let on = self.on_air();
        self.keyed.store(on, std::sync::atomic::Ordering::Relaxed);
        self.t += (n / 2) as u64;
        if !on || n == 0 {
            return n;
        }
        let e = self.effects;
        if e.gap {
            return 0;
        }
        if e.zeros {
            out[..n].fill(0.0);
        }
        if e.nan {
            out[..n].fill(f32::NAN);
        }
        if let Some(db) = e.strong_dbfs {
            let a = 10f32.powf(db / 20.0);
            let noise = 10f32.powf((db - 30.0) / 20.0);
            let w = std::f64::consts::TAU * 1000.0 / self.inner.sample_rate().max(1.0);
            for p in out[..n].chunks_exact_mut(2) {
                self.phase = (self.phase + w) % std::f64::consts::TAU;
                let (s, c) = self.phase.sin_cos();
                p[0] = (a * c as f32 + p[0] * noise * 30.0).clamp(-1.0, 1.0);
                p[1] = (a * s as f32 + p[1] * noise * 30.0).clamp(-1.0, 1.0);
            }
        }
        n
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn label(&self) -> String {
        self.inner.label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn sim(fx: TxEffects) -> TxSim<SyntheticSource> {
        let src = SyntheticSource::new(1000.0, 7e6, -60.0, vec![]);
        TxSim::new(src, 1.0, 2.0, fx)
    }

    #[test]
    fn a_simulated_over_stops_floods_or_moves_the_stream_then_gives_it_back() {
        // A second of samples a read.
        let mut buf = vec![0.0f32; 2000];
        let mut s = sim(TxEffects { gap: true, centre_hz: Some(0.0), ..Default::default() });
        assert_eq!(s.read(&mut buf), 2000);
        assert!(!s.keyed().load(Ordering::Relaxed));
        assert_eq!(s.center_hz(), 7e6);
        // Keyed from 1 s to 2 s: nothing arrives and the centre reads 0.
        assert_eq!(s.read(&mut buf), 0);
        assert!(s.keyed().load(Ordering::Relaxed));
        assert_eq!(s.center_hz(), 0.0);
        assert_eq!(s.read(&mut buf), 2000);
        assert!(!s.keyed().load(Ordering::Relaxed));
        assert_eq!(s.center_hz(), 7e6);

        let mut s = sim(TxEffects { strong_dbfs: Some(0.0), ..Default::default() });
        s.read(&mut buf);
        let quiet = buf.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        s.read(&mut buf);
        let loud = buf.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(quiet < 0.01 && loud > 0.9 && loud <= 1.0, "{quiet} {loud}");

        let mut s = sim(TxEffects { zeros: true, ..Default::default() }).repeat(3.0);
        for want_zero in [false, true, false, false, true] {
            s.read(&mut buf);
            assert_eq!(buf.iter().all(|x| *x == 0.0), want_zero);
        }
    }

    #[test]
    fn a_dial_read_back_of_nothing_or_nonsense_is_not_believed() {
        let duo = (10_000.0, 54_000_000.0);
        assert!(plausible_dial(14_074_000.0, duo));
        assert!(!plausible_dial(0.0, duo));
        assert!(!plausible_dial(4_294_967_295.0, duo));
        assert!(!plausible_dial(f64::NAN, duo));
    }
}
