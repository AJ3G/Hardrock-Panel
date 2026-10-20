// The AGC modes, the manual-gain fallback and the handling of impossible
// samples follow SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/agc.rs, GPL-3.0-or-later. See NOTICE.md. The
// feed-forward lookahead algorithm is this crate's own.

//! Feed-forward AGC with lookahead: the gain is worked out from the audio
//! *before* it is played, so a transient is met by a gain already down for it.
//!
//! # The algorithm
//!
//! For every sample, the gain that would put it exactly at the target is
//! `r = target / envelope` (capped at the maximum gain). The audio is delayed
//! by `A − 1` samples, `A` the attack time, and the gain applied to it is
//!
//! 1. the minimum of `r` over the last `W ≥ A` samples, which already includes
//!    every sample up to `A − 1` ahead of the one being played;
//! 2. smoothed by a running mean over the last `A` of those minima, so the
//!    gain ramps down over the attack time instead of stepping; and
//! 3. allowed to rise only after the hang time has passed since it last fell,
//!    and then with the decay time constant.
//!
//! Every value the mean in step 2 averages is a minimum over a window that
//! contains the sample being played, so the mean is no larger than that
//! sample's `r`; step 3 only ever holds the gain or moves it part way up
//! towards that mean. So **no sample leaves above the target** — the overshoot
//! is zero by construction, not by tuning — and a steady signal sits *at* the
//! target, because then every `r` is the same.
//!
//! # The envelope
//!
//! A sideband receiver hands the AGC the magnitude of its complex filter
//! output alongside the audio (the real part of it): the true envelope,
//! constant on a steady tone of any pitch. Driven by the audio's own sample
//! peaks instead (AM, FM, or a caller with no envelope), a steady tone reads
//! low by however far its peaks fall between samples — at most 0.02 dB for a
//! 1 kHz tone at 48 kHz — and [`PEAK_WINDOW_S`] keeps at least one peak of a
//! tone down to 50 Hz in every window.
//!
//! # Switched off
//!
//! Switched off it becomes a fixed manual gain rather than a bypass. A bypass
//! is unity gain on the demodulator's own output, and that output is whatever
//! the antenna delivered — an SSB signal sitting 60 dB below full scale stays
//! 60 dB below full scale, which no volume control can rescue. The tracker
//! keeps running while off so the manual level can be seeded from it and so
//! switching back on doesn't jump.

use std::collections::VecDeque;

use crate::types::{AgcMode, MAX_MANUAL_GAIN_DB};

/// The shortest window the minimum is taken over: 10 ms, half a period of
/// 50 Hz, so a sample-peak envelope always holds one peak of anything the
/// passband passes.
pub const PEAK_WINDOW_S: f32 = 0.010;

/// The loudest envelope the tracker will believe: sixty decibels above full
/// scale. Nothing a demodulator produces legitimately reaches it — the whole
/// chain works in units where 1.0 is full scale — so anything above this is a
/// fault upstream, and the only question is how long the receiver is deaf
/// afterwards: from here it is the hang and one decay.
const ENV_CEILING: f32 = 1_000.0;

/// Manual gain before anyone has set one. Matches `RxState::manual_gain_db`'s
/// own default, so a fresh chain and a fresh receiver agree.
const DEFAULT_MANUAL_GAIN_DB: f32 = 20.0;

/// Samples between recomputing the running mean's sum from scratch, so f64
/// rounding in the add-one-drop-one update never accumulates.
const RESUM_EVERY: u32 = 1 << 16;

/// What an AGC does, in the units an operator sets it in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgcParams {
    /// Lookahead, and the time the gain takes to come down: a transient is
    /// met by a gain that started falling this long before it arrived.
    pub attack_ms: f32,
    /// Time constant of the gain's rise once the hang has passed.
    pub decay_ms: f32,
    /// How long the gain is held after it last fell.
    pub hang_ms: f32,
    /// The peak level everything is brought to, dBFS (a full-scale sine is 0).
    pub target_dbfs: f32,
    /// The most the AGC may add, dB.
    pub max_gain_db: f32,
}

impl AgcParams {
    /// The listening presets. The target is the old fixed one (0.35 of full
    /// scale, −9.1 dBFS), which leaves room for the volume control.
    pub fn listening(mode: AgcMode) -> Self {
        let (hang_ms, decay_ms) = match mode {
            AgcMode::Fast => (100.0, 100.0),
            AgcMode::Med | AgcMode::Off => (500.0, 250.0),
            AgcMode::Slow => (1000.0, 500.0),
        };
        AgcParams { attack_ms: 2.0, decay_ms, hang_ms, target_dbfs: -9.1, max_gain_db: 90.0 }
    }

    /// The decoders' feed: −12 dBFS, leaving 12 dB for the peaks of several
    /// signals at once before anything clips, and slow, so a strong station
    /// keying up mid-slot does not make the weak ones beside it pump — the
    /// hang outlasts a CW element and an FT8 symbol many times over.
    pub const DECODER: AgcParams = AgcParams {
        attack_ms: 5.0,
        decay_ms: 2000.0,
        hang_ms: 2000.0,
        target_dbfs: -12.0,
        max_gain_db: 90.0,
    };

    /// Kept to what the algorithm can do: an attack of at least a sample and
    /// at most half a second (it is also the delay), a target at or below
    /// full scale.
    fn clamped(self) -> Self {
        AgcParams {
            attack_ms: self.attack_ms.clamp(0.1, 500.0),
            decay_ms: self.decay_ms.clamp(1.0, 60_000.0),
            hang_ms: self.hang_ms.clamp(0.0, 60_000.0),
            target_dbfs: self.target_dbfs.clamp(-60.0, 0.0),
            max_gain_db: self.max_gain_db.clamp(0.0, 120.0),
        }
    }
}

impl Default for AgcParams {
    fn default() -> Self {
        Self::listening(AgcMode::Med)
    }
}

pub struct Agc {
    rate: f32,
    params: AgcParams,
    enabled: bool,
    target: f32,
    max_gain: f32,
    /// Linear gain applied while the AGC is off.
    manual_gain: f32,
    /// The gain being applied, and the hang left before it may rise.
    gain: f32,
    hang_samples: u32,
    hang_left: u32,
    release_alpha: f32,
    /// The audio waiting `A − 1` samples, as a ring.
    delay: Vec<f32>,
    delay_at: usize,
    /// Sliding minimum of the wanted gain over the last `window` samples: a
    /// deque of (sample index, gain), increasing in both.
    mins: VecDeque<(u64, f32)>,
    window: u64,
    /// The last `A` minima, as a ring, and their sum.
    means: Vec<f32>,
    means_at: usize,
    sum: f64,
    since_resum: u32,
    /// Samples seen.
    n: u64,
    /// Samples that were not numbers, since the count was last read. Kept
    /// because surviving one is not the same as it being all right that one
    /// arrived: something upstream computed it, and the count is the only
    /// evidence of that there will ever be — see [`Agc::take_impossible`].
    impossible: u64,
}

impl Agc {
    pub fn new(sample_rate: f64) -> Self {
        Self::with_params(sample_rate, AgcParams::default())
    }

    pub fn with_params(sample_rate: f64, params: AgcParams) -> Self {
        let mut agc = Agc {
            rate: sample_rate as f32,
            params,
            enabled: true,
            target: 1.0,
            max_gain: 1.0,
            manual_gain: 1.0,
            // Starts at the most it may be, which `set_params` trims to the
            // maximum gain: what silence warrants. The first signal then meets
            // a gain coming down — never one coming up from nothing over the
            // decay time.
            gain: f32::MAX,
            hang_samples: 0,
            hang_left: 0,
            release_alpha: 0.0,
            delay: Vec::new(),
            delay_at: 0,
            mins: VecDeque::new(),
            window: 1,
            means: Vec::new(),
            means_at: 0,
            sum: 0.0,
            since_resum: 0,
            n: 0,
            impossible: 0,
        };
        agc.set_params(params);
        agc.set_manual_gain_db(DEFAULT_MANUAL_GAIN_DB);
        agc
    }

    /// New timing and level. A change of attack resizes the lookahead, which
    /// costs one attack's worth of audio; the others take effect at once.
    pub fn set_params(&mut self, params: AgcParams) {
        let p = params.clamped();
        let attack = ((self.rate * p.attack_ms / 1000.0).round() as usize).max(1);
        self.target = 10f32.powf(p.target_dbfs / 20.0);
        self.max_gain = 10f32.powf(p.max_gain_db / 20.0);
        self.hang_samples = (self.rate * p.hang_ms / 1000.0) as u32;
        self.release_alpha = 1.0 - (-1000.0 / (self.rate * p.decay_ms)).exp();
        if attack != self.means.len() || self.delay.len() != attack - 1 {
            // Start the lookahead again, as silence: everything the windows
            // hold says "full gain", which is what silence warrants, and the
            // gain itself is left where it was.
            self.delay = vec![0.0; attack - 1];
            self.delay_at = 0;
            let fill = self.gain.min(self.max_gain);
            self.means = vec![fill; attack];
            self.means_at = 0;
            self.sum = fill as f64 * attack as f64;
            self.mins.clear();
        }
        self.window = (attack as u64).max((self.rate * PEAK_WINDOW_S) as u64);
        self.gain = self.gain.min(self.max_gain);
        self.params = p;
    }

    pub fn params(&self) -> AgcParams {
        self.params
    }

    /// Switching between the two is glitch-free: the lookahead delay and the
    /// gain tracker run in both states, so nothing is flushed and re-enabling
    /// resumes at the level the signal already warrants. A preset's hang and
    /// decay replace the current ones; the attack and the target stay.
    pub fn set_mode(&mut self, mode: AgcMode) {
        self.enabled = mode != AgcMode::Off;
        if self.enabled {
            let preset = AgcParams::listening(mode);
            self.set_params(AgcParams {
                hang_ms: preset.hang_ms,
                decay_ms: preset.decay_ms,
                ..self.params
            });
        }
    }

    pub fn set_max_gain_db(&mut self, db: f32) {
        self.set_params(AgcParams { max_gain_db: db, ..self.params });
    }

    /// The fixed gain used while the AGC is off.
    pub fn set_manual_gain_db(&mut self, db: f32) {
        self.manual_gain = 10f32.powf(db.clamp(0.0, MAX_MANUAL_GAIN_DB) / 20.0);
    }

    /// What the tracker is applying right now, in dB. Seeds the manual gain
    /// when the operator switches the AGC off, so the level carries over
    /// instead of dropping to whatever was last set by hand.
    pub fn gain_db(&self) -> f32 {
        20.0 * self.gain.max(1e-9).log10()
    }

    /// Samples by which the output lags the input: the attack, less one.
    pub fn delay_samples(&self) -> usize {
        self.delay.len()
    }

    /// Samples that were not numbers since this was last called, and reset.
    ///
    /// Nothing in a receive chain should ever produce one, and the AGC is the
    /// stage where it does the most damage, so it is also the stage best placed
    /// to say that one came through. The engine reports it once per chain; the
    /// count is what turns the next report of a receiver gone quiet into a
    /// diagnosis.
    pub fn take_impossible(&mut self) -> u64 {
        std::mem::replace(&mut self.impossible, 0)
    }

    /// In place, with the envelope taken from the audio's own sample peaks.
    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            let x = self.finite(*s);
            *s = self.step(x, x.abs());
        }
    }

    /// In place, with the envelope given — one value per sample, the
    /// magnitude of the complex signal `samples` is the real part of.
    pub fn process_with_envelope(&mut self, samples: &mut [f32], envelope: &[f32]) {
        debug_assert_eq!(samples.len(), envelope.len());
        for (s, &e) in samples.iter_mut().zip(envelope) {
            let x = self.finite(*s);
            let e = if e.is_finite() { e } else { x.abs() };
            *s = self.step(x, e);
        }
    }

    /// A sample, or silence where there was no sample. Anything that is not a
    /// number is not audio: played it is a click at best and a muted stream at
    /// worst, and carried into the tracker it is worse than that.
    #[inline]
    fn finite(&mut self, x: f32) -> f32 {
        if x.is_finite() {
            x
        } else {
            self.impossible += 1;
            0.0
        }
    }

    /// One sample in, one (delayed, levelled) sample out.
    ///
    /// The tracker advances even while the AGC is off — the manual gain is what
    /// gets *applied* then, but keeping `gain` converged is what makes switching
    /// back on silent and gives [`Agc::gain_db`] something honest to report.
    ///
    /// The envelope is bounded, which is not housekeeping: the old feedback
    /// AGC carried its envelope from sample to sample, and an infinity there
    /// was an infinity for ever (issue #305). Here nothing carries the
    /// envelope itself, but an enormous one would still be believed for the
    /// whole hang, so it is trimmed to [`ENV_CEILING`].
    #[inline]
    fn step(&mut self, x: f32, envelope: f32) -> f32 {
        let e = envelope.min(ENV_CEILING);
        let want = if e * self.max_gain > self.target { self.target / e } else { self.max_gain };

        // 1. Sliding minimum over the window.
        let n = self.n;
        self.n += 1;
        while self.mins.back().is_some_and(|&(_, g)| g >= want) {
            self.mins.pop_back();
        }
        self.mins.push_back((n, want));
        while self.mins.front().is_some_and(|&(i, _)| i + self.window <= n) {
            self.mins.pop_front();
        }
        let min = self.mins.front().map_or(want, |&(_, g)| g);

        // 2. Running mean of the minima over the attack.
        let old = std::mem::replace(&mut self.means[self.means_at], min);
        self.means_at = (self.means_at + 1) % self.means.len();
        self.sum += min as f64 - old as f64;
        self.since_resum += 1;
        if self.since_resum >= RESUM_EVERY {
            self.since_resum = 0;
            self.sum = self.means.iter().map(|&v| v as f64).sum();
        }
        let mean = (self.sum / self.means.len() as f64) as f32;

        // 3. Fall at once to the mean; hold; then rise towards it. Holding
        // level counts as falling, so the hang runs from when the signal
        // goes, not from when the gain last moved.
        if mean <= self.gain {
            self.gain = mean;
            self.hang_left = self.hang_samples;
        } else if self.hang_left > 0 {
            self.hang_left -= 1;
        } else {
            self.gain += (mean - self.gain) * self.release_alpha;
        }

        let out = if self.delay.is_empty() {
            x
        } else {
            let d = std::mem::replace(&mut self.delay[self.delay_at], x);
            self.delay_at = (self.delay_at + 1) % self.delay.len();
            d
        };
        out * if self.enabled { self.gain } else { self.manual_gain }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// A steady tone at `amp`, `secs` of it.
    fn tone(amp: f32, secs: f32) -> Vec<f32> {
        let n = (RATE * secs) as usize;
        (0..n).map(|k| amp * (std::f32::consts::TAU * 1000.0 * k as f32 / RATE).sin()).collect()
    }

    /// A tone's envelope, for the envelope-driven path.
    fn flat(amp: f32, n: usize) -> Vec<f32> {
        vec![amp; n]
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    fn settled() -> Agc {
        let mut agc = Agc::new(RATE as f64);
        agc.process(&mut tone(0.01, 1.0));
        agc
    }

    /// The gain a −40 dB step up meets has already come down: nothing leaves
    /// above the target, with either envelope. The specification is under
    /// 0.5 dB; by construction it is none.
    #[test]
    fn a_step_up_does_not_overshoot() {
        for with_envelope in [false, true] {
            for p in [AgcParams::listening(AgcMode::Fast), AgcParams::DECODER] {
                let mut agc = Agc::with_params(RATE as f64, p);
                let target = 10f32.powf(p.target_dbfs / 20.0);
                let mut quiet = tone(0.01, 2.0);
                let mut loud = tone(1.0, 0.5);
                if with_envelope {
                    let (eq, el) = (flat(0.01, quiet.len()), flat(1.0, loud.len()));
                    agc.process_with_envelope(&mut quiet, &eq);
                    agc.process_with_envelope(&mut loud, &el);
                } else {
                    agc.process(&mut quiet);
                    agc.process(&mut loud);
                }
                let worst = db(peak(&quiet).max(peak(&loud)) / target);
                println!(
                    "step +40 dB, attack {} ms, envelope {with_envelope}: peak {worst:+.4} dB",
                    p.attack_ms
                );
                assert!(worst < 0.01, "{p:?} envelope {with_envelope}: {worst:+.3} dB over");
            }
        }
    }

    /// A steady signal sits at the target, within 0.1 dB, from far below
    /// it and from above it.
    #[test]
    fn steady_state_is_the_target() {
        for p in [AgcParams::listening(AgcMode::Med), AgcParams::DECODER] {
            let target = 10f32.powf(p.target_dbfs / 20.0);
            for amp in [0.000_1, 0.01, 0.2, 1.0, 3.0] {
                let mut agc = Agc::with_params(RATE as f64, p);
                let mut x = tone(amp, 3.0);
                agc.process(&mut x);
                let err = db(peak(&x[x.len() - 4_800..]) / target);
                println!("target {} dBFS, tone {amp}: {err:+.4} dB (sample peaks)", p.target_dbfs);
                assert!(err.abs() < 0.1, "{p:?} at {amp}: {err:+.3} dB");
                // And with the true envelope, exact.
                let mut agc = Agc::with_params(RATE as f64, p);
                let mut x = tone(amp, 3.0);
                let e = flat(amp, x.len());
                agc.process_with_envelope(&mut x, &e);
                let err = db(peak(&x[x.len() - 4_800..]) / target);
                println!("target {} dBFS, tone {amp}: {err:+.5} dB (envelope)", p.target_dbfs);
                assert!(err.abs() < 0.01, "{p:?} at {amp}, envelope: {err:+.4} dB");
            }
        }
    }

    /// After a signal goes away the gain holds for the hang, then rises with
    /// the decay time constant.
    #[test]
    fn the_gain_hangs_then_decays() {
        let p = AgcParams {
            attack_ms: 2.0,
            decay_ms: 200.0,
            hang_ms: 300.0,
            target_dbfs: -12.0,
            max_gain_db: 90.0,
        };
        let mut agc = Agc::with_params(RATE as f64, p);
        let n = 48_000;
        let mut loud = vec![0.5f32; n];
        agc.process_with_envelope(&mut loud, &flat(0.5, n));
        let held = agc.gain_db();
        // 40 dB down: the gain it wants is 40 dB more.
        let mut quiet = vec![0.005f32; n];
        let env = flat(0.005, n);
        let mut gains = Vec::new();
        for (s, e) in quiet.chunks_mut(48).zip(env.chunks(48)) {
            agc.process_with_envelope(s, e);
            gains.push(agc.gain_db());
        }
        // Each entry is 1 ms. The window (10 ms) and the hang (300 ms) pass
        // with the gain untouched...
        for (ms, g) in gains.iter().enumerate().take(305) {
            assert!((g - held).abs() < 0.01, "{ms} ms: moved to {g:.2} from {held:.2}");
        }
        // ...then it rises: a one-pole in linear gain from 1/0.5 towards
        // 1/0.005 of the target, 63 % of the way in one decay time.
        let start = 10f32.powf(held / 20.0);
        let goal = start * 100.0;
        let at = |ms: usize| 10f32.powf(gains[ms] / 20.0);
        assert!(at(320) > start, "still holding at 320 ms");
        let frac = (at(312 + 200) - start) / (goal - start);
        assert!((frac - 0.632).abs() < 0.03, "{frac:.3} of the way after one decay time");
    }

    /// The whole of issue #305: one sample that is not a number must not leave
    /// the receiver deaf.
    #[test]
    fn one_impossible_sample_does_not_leave_the_receiver_deaf() {
        for bad in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let mut agc = settled();
            assert!(agc.gain_db() > 25.0, "the tracker should have wound a -40 dBFS tone up");
            agc.process(&mut [bad; 8]);
            let mut after = tone(0.01, 0.5);
            agc.process(&mut after);
            let out = peak(&after[24_000 - 4_800..]);
            let target = 10f32.powf(-9.1 / 20.0);
            assert!(db(out / target).abs() < 0.1, "{bad:?} left the tone at {out:.4}");
            assert_eq!(agc.take_impossible(), 8);
        }
    }

    /// And nothing that is not a number reaches the speakers either.
    #[test]
    fn nothing_impossible_reaches_the_output() {
        let mut agc = settled();
        let mut block = vec![f32::NAN; 64];
        block.extend(vec![f32::INFINITY; 64]);
        block.extend(tone(0.01, 0.1));
        agc.process(&mut block);
        assert!(block.iter().all(|s| s.is_finite()), "a non-finite sample got through");
    }

    /// A merely enormous sample costs about a hang and a recovery.
    #[test]
    fn an_enormous_sample_costs_about_a_hang_and_a_recovery() {
        let mut agc = settled();
        agc.process(&mut [1e30f32; 8]);
        let mut after = tone(0.01, 3.0);
        agc.process(&mut after);
        let out = peak(&after[after.len() - 4_800..]);
        let target = 10f32.powf(-9.1 / 20.0);
        assert!(db(out / target).abs() < 0.1, "still at {:.1} dB, tone at {out:.4}", agc.gain_db());
    }

    /// Switched off it is a fixed gain, and one that goes on being applied
    /// whatever the tracker is doing.
    #[test]
    fn switched_off_it_is_the_manual_gain() {
        let mut agc = Agc::new(RATE as f64);
        agc.set_mode(AgcMode::Off);
        agc.set_manual_gain_db(20.0);
        let mut x = tone(0.01, 0.5);
        agc.process(&mut x);
        let out = peak(&x[24_000 - 4_800..]);
        assert!((out - 0.1).abs() < 0.001, "manual +20 dB on 0.01 gave {out:.4}");
    }

    /// The output lags by the attack, and nothing else: an impulse comes out
    /// `attack − 1` samples later.
    #[test]
    fn the_delay_is_the_attack() {
        let mut agc =
            Agc::with_params(RATE as f64, AgcParams { attack_ms: 5.0, ..AgcParams::DECODER });
        agc.set_mode(AgcMode::Off);
        agc.set_manual_gain_db(0.0);
        let mut x = vec![0.0f32; 1000];
        x[10] = 1.0;
        agc.process(&mut x);
        assert_eq!(agc.delay_samples(), 239);
        assert_eq!(x.iter().position(|&s| s != 0.0), Some(10 + 239));
    }
}
