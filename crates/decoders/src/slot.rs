//! UTC time and the WSJT modes' slots.
//!
//! AJ3G's computer keeps UTC by NTP with a GPS reference behind it, and the
//! same GPS reference clocks the radio. So slot timing comes straight from the
//! system clock: no offset is learnt from the decodes, and each decode's DT is
//! reported as measured, so a clock that drifted would show there. The panel
//! stamps each block of audio with the UTC time of its first sample (the time
//! it was read, less the receive chain's delay, which is known).

use std::time::{SystemTime, UNIX_EPOCH};

/// The system clock's UTC time now, in Unix seconds with a fraction.
pub fn utc_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The start of the slot of `period_s` seconds that `utc` falls in. Slots
/// start on whole multiples of the period from 00:00 UTC.
pub fn slot_start(utc: f64, period_s: f64) -> f64 {
    (utc / period_s).floor() * period_s
}

/// A finished slot of audio, from its UTC start.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// UTC of the slot's first sample, a whole multiple of the period.
    pub start_utc: f64,
    /// Exactly `period × rate` samples; where nothing was heard, zeros.
    pub audio: Vec<f32>,
    /// Where in the slot the audio began, in seconds: 0 for a slot heard
    /// from its start.
    pub first_s: f64,
}

/// Collects time-stamped audio into whole UTC slots.
///
/// Each block goes where its time stamp says, so a gap leaves silence rather
/// than shifting what follows, and the slot boundary is the clock's, not a
/// sample count's.
pub struct SlotRecorder {
    rate: f64,
    period_s: f64,
    len: usize,
    cur: Option<Slot>,
    /// Where the last block ended, to catch stamps that jitter backwards.
    next_index: usize,
}

impl SlotRecorder {
    pub fn new(rate: f64, period_s: f64) -> Self {
        let len = (rate * period_s).round() as usize;
        SlotRecorder { rate, period_s, len, cur: None, next_index: 0 }
    }

    pub fn period_s(&self) -> f64 {
        self.period_s
    }

    /// Add audio whose first sample was at `utc`. Returns every slot this
    /// block finished.
    pub fn push(&mut self, audio: &[f32], utc: f64) -> Vec<Slot> {
        let mut done = Vec::new();
        let mut t = utc;
        let mut rest = audio;
        while !rest.is_empty() {
            let start = slot_start(t, self.period_s);
            if self.cur.as_ref().is_some_and(|s| s.start_utc < start - 1e-6)
                && let Some(s) = self.cur.take()
            {
                done.push(s);
            }
            let slot = self.cur.get_or_insert_with(|| {
                let first_s = t - start;
                Slot { start_utc: start, audio: vec![0.0; self.len], first_s }
            });
            // Within a slot, a stamp a sample or two off from where the last
            // block ended is jitter: carry on from where it ended.
            let mut at = ((t - slot.start_utc) * self.rate).round() as usize;
            if self.next_index > 0 && at.abs_diff(self.next_index) <= 2 {
                at = self.next_index;
            }
            let room = self.len.saturating_sub(at);
            if room == 0 {
                // Stamped within half a sample of the boundary: it starts the
                // next slot.
                t = slot.start_utc + self.period_s;
                self.next_index = 0;
                if let Some(s) = self.cur.take() {
                    done.push(s);
                }
                continue;
            }
            let n = room.min(rest.len());
            slot.audio[at..at + n].copy_from_slice(&rest[..n]);
            self.next_index = if at + n >= self.len { 0 } else { at + n };
            rest = &rest[n..];
            t = slot.start_utc + (at + n) as f64 / self.rate;
            if at + n >= self.len {
                // Full: finish it now rather than wait for the next block.
                if let Some(s) = self.cur.take() {
                    done.push(s);
                }
                // The next sample starts the next slot exactly.
                t = (t / self.period_s).round() * self.period_s;
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_start_on_the_period_from_midnight() {
        assert_eq!(slot_start(1_700_000_007.3, 15.0), 1_700_000_000.0 - 5.0);
        assert_eq!(slot_start(1_700_000_100.0, 120.0), 1_700_000_040.0);
    }

    #[test]
    fn blocks_fill_slots_by_their_time_stamps() {
        let rate = 100.0;
        let mut r = SlotRecorder::new(rate, 15.0);
        let start = 1_699_999_995.0; // a slot boundary
        let mut slots = Vec::new();
        // 31 s of blocks of 0.37 s, with a 1 s gap in the middle.
        let mut t = start;
        let mut k = 0.0f32;
        while t < start + 31.0 {
            let n = 37;
            if !(t > start + 20.0 && t < start + 21.0) {
                let block: Vec<f32> = (0..n).map(|i| k + i as f32).collect();
                slots.extend(r.push(&block, t));
            }
            k += n as f32;
            t += n as f64 / rate;
        }
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].start_utc, start);
        assert_eq!(slots[0].first_s, 0.0);
        assert_eq!(slots[0].audio.len(), 1500);
        // Sample i of the first slot is the i-th sample sent.
        assert!(slots[0].audio.iter().enumerate().all(|(i, &v)| v == i as f32));
        assert_eq!(slots[1].start_utc, start + 15.0);
        // The gap is silence and later samples are where their stamps say.
        let s = &slots[1];
        let at = |sec: f64| s.audio[((sec - 15.0) * rate) as usize];
        assert_eq!(at(20.5), 0.0);
        assert!((at(25.0) - 2500.0).abs() < 1.0);
    }

    #[test]
    fn a_slot_joined_late_says_so() {
        let mut r = SlotRecorder::new(100.0, 15.0);
        let start = 1_699_999_995.0;
        let out = r.push(&vec![1.0; 1400], start + 2.0);
        assert_eq!(out.len(), 1);
        assert!((out[0].first_s - 2.0).abs() < 1e-9);
        assert_eq!(out[0].audio[199], 0.0);
        assert_eq!(out[0].audio[200], 1.0);
    }
}
