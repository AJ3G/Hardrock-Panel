//! TX freq AUTO's ear for what did not decode (decision 115): a coarse
//! occupancy of the receive passband in each FT8 slot, from the receiver's
//! audio, so AUTO also steers clear of signal energy nothing decoded (a
//! station too weak or too crowded to copy, another mode, a carrier).
//!
//! The receiver's audio (48 kHz, [`crate::rx::Taps`]) is cut into 0.1 s
//! frames and each frame's power taken in 10 Hz bins (one 4800-point FFT,
//! about 0.1 ms of work each tenth of a second). Over a slot's message
//! part (0.5 s to 13 s into it) the powers are averaged per bin; a bin is
//! busy when its average sits [`OVER_DB`] above the slot's noise floor (the
//! quietest quarter of the bins). Averaging over the slot is what makes it
//! "signal for a good part of the slot": a short click or a lone noise
//! spike does not lift a bin's average 6 dB, while an FT8 signal of about
//! −10 dB SNR or better does. A slot the radio was keyed in (our own over
//! coming back) or only partly heard is dropped.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::{freq, sched};

/// The width of a bin, Hz.
pub(crate) const BIN_HZ: f64 = 10.0;
/// Points in a frame: one bin of [`BIN_HZ`] at the audio rate.
const N: usize = (sdr::AUDIO_RATE / BIN_HZ) as usize;
/// A bin this far over the noise floor, averaged over the slot, is busy, dB.
pub(crate) const OVER_DB: f32 = 6.0;
/// The noise floor: this quantile of the slot's bins.
const FLOOR_Q: f32 = 0.25;
/// The part of a slot looked at, s into it: an FT8 message's span.
const FROM_S: f64 = 0.5;
const TO_S: f64 = 13.0;
/// Frames a slot needs to count (of about 125).
const MIN_FRAMES: u32 = 80;
/// Slots kept, both parities: the last four of each.
const KEEP_SLOTS: usize = 8;
/// How long a slot's energy counts as busy, s (as a weak decode does).
pub(crate) const KEEP_S: i64 = freq::KEEP_S;

/// The occupancy meter: shared between the receiver's audio tap and TX.
#[derive(Clone, Default)]
pub(crate) struct Occupancy {
    inner: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    /// The radio is keyed now: the slot is our own over.
    keyed: AtomicBool,
    /// The tap is installed in the receiver.
    tapped: AtomicBool,
    m: Mutex<Meter>,
}

#[derive(Default)]
struct Meter {
    fft: Option<Arc<dyn Fft<f32>>>,
    window: Vec<f32>,
    scratch: Vec<Complex32>,
    /// Audio not yet framed, and the UTC of its first sample.
    buf: Vec<f32>,
    buf_utc: f64,
    /// The slot being measured: its start, frames taken, keyed in it, and
    /// each bin's summed power (bins from [`first_bin`]).
    slot: Option<i64>,
    frames: u32,
    spoiled: bool,
    sum: Vec<f32>,
    /// Finished slots: their start and busy spans, audio Hz.
    done: VecDeque<(i64, Vec<(f32, f32)>)>,
}

/// The first and last bins measured: AUTO's audio range.
fn first_bin() -> usize {
    (freq::LO_HZ as f64 / BIN_HZ).floor() as usize
}
fn last_bin() -> usize {
    (freq::HI_HZ as f64 / BIN_HZ).ceil() as usize
}

impl Occupancy {
    /// The radio is keyed (or not) now.
    pub(crate) fn set_keyed(&self, keyed: bool) {
        self.inner.keyed.store(keyed, Ordering::Relaxed);
    }

    /// Put the meter on the receiver's audio (`on`), or take it off.
    /// Off, what it measured is forgotten.
    pub(crate) fn tap(&self, taps: &crate::rx::Taps, on: bool) {
        if self.inner.tapped.swap(on, Ordering::Relaxed) == on {
            return;
        }
        if on {
            let me = self.clone();
            taps.set_audio("txbusy", Some(Box::new(move |a, utc| me.push(a, utc))));
        } else {
            taps.set_audio("txbusy", None);
            self.clear();
        }
    }

    /// Forget every slot (a band change).
    pub(crate) fn clear(&self) {
        let mut m = self.lock();
        m.buf.clear();
        m.slot = None;
        m.done.clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Meter> {
        self.inner.m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The receiver's audio: `a` from `utc` (its first sample, UTC s).
    pub(crate) fn push(&self, a: &[f32], utc: f64) {
        let keyed = self.inner.keyed.load(Ordering::Relaxed);
        let mut m = self.lock();
        // A gap in the audio (or the first block): start framing afresh.
        let expect = m.buf_utc + m.buf.len() as f64 / sdr::AUDIO_RATE;
        if m.buf.is_empty() || (utc - expect).abs() > 0.05 {
            m.buf.clear();
            m.buf_utc = utc;
        }
        m.buf.extend_from_slice(a);
        while m.buf.len() >= N {
            let mid = m.buf_utc + 0.5 * N as f64 / sdr::AUDIO_RATE;
            m.frame(mid, keyed);
            m.buf.drain(..N);
            m.buf_utc += N as f64 / sdr::AUDIO_RATE;
        }
    }

    /// The busy spans (audio Hz) of the slots of one parity (`even`) kept
    /// and no older than [`KEEP_S`] at `now`, UTC s.
    pub(crate) fn spans(&self, even: bool, now: f64) -> Vec<(f32, f32)> {
        let m = self.lock();
        m.done
            .iter()
            .filter(|(t, _)| {
                sched::is_even(*t, sched::FT8_PERIOD) == even && *t as f64 >= now - KEEP_S as f64
            })
            .flat_map(|(_, s)| s.iter().copied())
            .collect()
    }
}

impl Meter {
    /// One frame, its middle at `mid` UTC s.
    fn frame(&mut self, mid: f64, keyed: bool) {
        let period = sched::FT8_PERIOD as f64;
        let slot = ((mid / period).floor() * period) as i64;
        if self.slot != Some(slot) {
            self.finish();
            self.slot = Some(slot);
            self.frames = 0;
            self.spoiled = false;
            self.sum.clear();
            self.sum.resize(last_bin() - first_bin() + 1, 0.0);
        }
        self.spoiled |= keyed;
        let into = mid - slot as f64;
        if self.spoiled || !(FROM_S..=TO_S).contains(&into) {
            return;
        }
        let fft = self.fft.get_or_insert_with(|| FftPlanner::new().plan_fft_forward(N)).clone();
        if self.window.len() != N {
            self.window = (0..N)
                .map(|i| {
                    let x = std::f32::consts::TAU * i as f32 / N as f32;
                    0.5 - 0.5 * x.cos()
                })
                .collect();
        }
        let mut data: Vec<Complex32> = self.buf[..N]
            .iter()
            .zip(&self.window)
            .map(|(&s, &w)| Complex32::new(s * w, 0.0))
            .collect();
        if self.scratch.len() != fft.get_inplace_scratch_len() {
            self.scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        }
        fft.process_with_scratch(&mut data, &mut self.scratch);
        let b0 = first_bin();
        for (k, s) in self.sum.iter_mut().enumerate() {
            *s += data[b0 + k].norm_sqr();
        }
        self.frames += 1;
    }

    /// The slot measured is over: keep its busy spans if it was heard
    /// whole and not over our own over.
    fn finish(&mut self) {
        let Some(slot) = self.slot.take() else { return };
        if self.spoiled || self.frames < MIN_FRAMES {
            return;
        }
        let spans = spans_of(&self.sum, first_bin());
        self.done.push_back((slot, spans));
        while self.done.len() > KEEP_SLOTS {
            self.done.pop_front();
        }
    }
}

/// The busy spans, audio Hz, of a slot's per-bin power (`power[0]` is bin
/// `first`): runs of bins [`OVER_DB`] above the floor (the
/// [`FLOOR_Q`] quantile). None when there is no floor (silence).
pub(crate) fn spans_of(power: &[f32], first: usize) -> Vec<(f32, f32)> {
    let mut sorted: Vec<f32> = power.to_vec();
    sorted.sort_by(f32::total_cmp);
    let Some(&floor) = sorted.get((sorted.len() as f32 * FLOOR_Q) as usize) else {
        return Vec::new();
    };
    if !(floor.is_finite() && floor > 0.0) {
        return Vec::new();
    }
    let over = floor * 10f32.powf(OVER_DB / 10.0);
    let edge = |k: usize, hi: bool| {
        ((first + k) as f64 * BIN_HZ + if hi { 0.5 } else { -0.5 } * BIN_HZ) as f32
    };
    let mut out = Vec::new();
    let mut run: Option<usize> = None;
    for (k, &p) in power.iter().enumerate() {
        match (p > over, run) {
            (true, None) => run = Some(k),
            (false, Some(a)) => {
                out.push((edge(a, false), edge(k - 1, true)));
                run = None;
            }
            _ => {}
        }
    }
    if let Some(a) = run {
        out.push((edge(a, false), edge(power.len() - 1, true)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pseudo-random noise sample, about ±1.
    fn noise(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }

    /// `secs` of audio from `t0`: noise, plus an FT8-like signal hopping
    /// over 8 tones of 6.25 Hz from `sig_hz` (when given) at `amp`.
    fn feed(o: &Occupancy, t0: f64, secs: f64, sig_hz: Option<f32>, amp: f32) {
        let rate = sdr::AUDIO_RATE;
        let mut seed = 0x1234_5678_9abc_def1u64 ^ t0 as u64;
        let block = 4096;
        let total = (secs * rate) as usize;
        let mut phase = 0f32;
        let mut i = 0;
        while i < total {
            let n = block.min(total - i);
            let a: Vec<f32> = (0..n)
                .map(|j| {
                    let t = (i + j) as f64 / rate;
                    let mut s = 0.3 * noise(&mut seed);
                    if let Some(f) = sig_hz {
                        let sym = ((t / 0.16) as u64).wrapping_mul(2654435761) % 8;
                        let hz = f + 6.25 * sym as f32;
                        phase = (phase + std::f32::consts::TAU * hz / rate as f32)
                            % std::f32::consts::TAU;
                        s += amp * phase.sin();
                    }
                    s
                })
                .collect();
            o.push(&a, t0 + i as f64 / rate);
            i += n;
        }
    }

    #[test]
    fn an_undecoded_signal_in_a_slot_is_busy_where_it_is() {
        let o = Occupancy::default();
        // Slot 30 (even) with a signal at 1000 Hz, then into slot 45.
        feed(&o, 30.0, 16.0, Some(1000.0), 0.05);
        let s = o.spans(true, 46.0);
        assert!(!s.is_empty(), "nothing busy");
        assert!(s.iter().all(|&(a, b)| a >= 980.0 && b <= 1070.0), "{s:?}");
        assert!(s.iter().any(|&(a, b)| a <= 1010.0 && b >= 1040.0), "{s:?}");
        // The other parity has nothing; nor, after two minutes, this one.
        assert!(o.spans(false, 46.0).is_empty());
        assert!(o.spans(true, 30.0 + KEEP_S as f64 + 1.0).is_empty());
    }

    #[test]
    fn noise_alone_is_not_busy_and_a_keyed_slot_is_dropped() {
        let o = Occupancy::default();
        feed(&o, 30.0, 16.0, None, 0.0);
        assert!(o.spans(true, 46.0).is_empty());
        // Keyed in slot 60: our own over, not someone else's.
        o.set_keyed(true);
        feed(&o, 60.0, 15.0, Some(1500.0), 0.5);
        o.set_keyed(false);
        feed(&o, 75.0, 1.0, None, 0.0);
        assert!(o.spans(true, 76.0).is_empty());
        // A slot heard only in part is dropped too.
        let p = Occupancy::default();
        feed(&p, 36.0, 10.0, Some(1500.0), 0.5);
        assert!(p.spans(true, 46.0).is_empty());
    }

    #[test]
    fn spans_are_runs_of_bins_over_the_floor() {
        let mut p = vec![1.0f32; 100];
        p[10] = 10.0;
        p[11] = 10.0;
        p[50] = 3.0; // under 6 dB
        p[99] = 5.0;
        assert_eq!(spans_of(&p, 20), vec![(295.0, 315.0), (1185.0, 1195.0)]);
        assert!(spans_of(&[0.0; 10], 20).is_empty());
    }
}
