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
    /// How many samples into a slot it is finished: `len`, or fewer with
    /// [`Self::with_cut`].
    cut: usize,
    /// The start of a slot finished early, whose rest is not kept.
    closed: Option<f64>,
}

impl SlotRecorder {
    pub fn new(rate: f64, period_s: f64) -> Self {
        let len = (rate * period_s).round() as usize;
        SlotRecorder { rate, period_s, len, cur: None, next_index: 0, cut: len, closed: None }
    }

    /// Finish each slot `cut_s` seconds in, rather than at its end: once
    /// every transmission in it has ended (WSPR's 110.6 s), so it can be
    /// decoded without waiting out the slot. The slot is still `period × rate`
    /// samples long, silent after the cut, and what is heard after it until
    /// the next slot is not kept.
    pub fn with_cut(mut self, cut_s: f64) -> Self {
        self.cut = ((self.rate * cut_s).round() as usize).clamp(1, self.len);
        self
    }

    pub fn period_s(&self) -> f64 {
        self.period_s
    }

    /// The slot being filled, if one is: its start (UTC), its audio up to the
    /// last sample heard, and where in it the audio began (seconds).
    pub fn partial(&self) -> Option<(f64, &[f32], f64)> {
        let s = self.cur.as_ref()?;
        Some((s.start_utc, &s.audio[..self.next_index.min(s.audio.len())], s.first_s))
    }

    /// Add audio whose first sample was at `utc`. Returns every slot this
    /// block finished.
    pub fn push(&mut self, audio: &[f32], utc: f64) -> Vec<Slot> {
        let mut done = Vec::new();
        let mut t = utc;
        let mut rest = audio;
        while !rest.is_empty() {
            let start = slot_start(t, self.period_s);
            if self.closed.is_some_and(|c| (c - start).abs() < 1e-6) {
                // The slot was finished at its cut: skip to the next one.
                let end = start + self.period_s;
                let to_end = (((end - t) * self.rate).round() as usize).max(1);
                if to_end >= rest.len() {
                    break;
                }
                rest = &rest[to_end..];
                t = end;
                self.closed = None;
                continue;
            }
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
            let room = self.cut.saturating_sub(at);
            if room == 0 && self.cut < self.len {
                // Past the cut: finish it, and skip the rest of the slot.
                self.closed = Some(slot.start_utc);
                self.next_index = 0;
                if let Some(s) = self.cur.take() {
                    done.push(s);
                }
                continue;
            }
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
            self.next_index = if at + n >= self.cut { 0 } else { at + n };
            rest = &rest[n..];
            t = slot.start_utc + (at + n) as f64 / self.rate;
            if at + n >= self.cut && self.cut < self.len {
                // At the cut: finish it now; the rest of the slot is skipped.
                self.closed = Some(slot.start_utc);
                if let Some(s) = self.cur.take() {
                    done.push(s);
                }
            } else if at + n >= self.len {
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
    fn the_slot_being_filled_can_be_seen() {
        let mut r = SlotRecorder::new(100.0, 15.0);
        let start = 1_699_999_995.0;
        assert!(r.partial().is_none());
        r.push(&[1.0; 50], start + 2.0);
        r.push(&[2.0; 30], start + 2.5);
        let (at, audio, first_s) = r.partial().expect("a slot");
        assert_eq!(at, start);
        assert_eq!(audio.len(), 280);
        assert_eq!((audio[199], audio[200], audio[279]), (0.0, 1.0, 2.0));
        assert!((first_s - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_cut_slot_is_finished_at_the_cut_and_its_rest_skipped() {
        // 120 s slots finished 113 s in, at 100 samples a second.
        let mut r = SlotRecorder::new(100.0, 120.0).with_cut(113.0);
        let start = 1_700_000_040.0;
        let mut done = Vec::new();
        let mut t = start;
        let mut k = 0.0f32;
        // 250 s of 0.37 s blocks: one slot, its tail, and most of the next.
        while t < start + 250.0 {
            let block: Vec<f32> = (0..37).map(|i| k + i as f32).collect();
            for s in r.push(&block, t) {
                // Finished as soon as the block that reaches 113 s is in.
                assert!(t + 0.37 >= s.start_utc + 113.0 && t < s.start_utc + 113.0, "{t}");
                done.push(s);
            }
            k += 37.0;
            t += 0.37;
        }
        assert_eq!(done.len(), 2);
        let s = &done[0];
        assert_eq!((s.start_utc, s.audio.len()), (start, 12_000));
        // Heard up to the cut, silent after it.
        assert!(s.audio[..11_300].iter().enumerate().all(|(i, &v)| v == i as f32));
        assert!(s.audio[11_300..].iter().all(|&v| v == 0.0));
        // The next slot starts on its boundary with the samples sent then.
        let n = &done[1];
        assert_eq!((n.start_utc, n.first_s), (start + 120.0, 0.0));
        assert!((n.audio[0] - 12_000.0).abs() < 1.0, "{}", n.audio[0]);
        assert!((n.audio[500] - 12_500.0).abs() < 1.0);
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
