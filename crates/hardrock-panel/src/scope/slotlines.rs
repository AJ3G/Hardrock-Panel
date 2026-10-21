//! Slot lines on the waterfall (AJ3G's request, decision 51): while DECODE
//! runs a mode with T/R slots, a green line across the waterfall where each
//! slot's capture starts and a red one where it ends, when the decoder takes
//! the slot's audio. WSJT-X draws one line per T/R period at its boundary,
//! labelled with the UTC; these are drawn the same way, a thin full-width
//! line with the time at the left, and they scroll with the rows as the UTC
//! ticks do (they are placed by each row's own time).
//!
//! Red is kept for warnings everywhere else on the panel. These two are the
//! named exception, as the waterfall palettes are (see `palette.rs`): AJ3G
//! asked for green and red by name, and here they mean "capture starts" and
//! "capture ends", WSJT-X users' convention, not a warning.

use std::collections::VecDeque;

use eframe::egui::Color32;

/// The start line's green.
pub const START_INK: Color32 = Color32::from_rgb(0x3c, 0xe0, 0x5a);
/// The end line's red: the agreed exception to "red for warnings only".
pub const END_INK: Color32 = Color32::from_rgb(0xff, 0x48, 0x48);

/// The most decoder runs remembered (each start, stop or change of mode is
/// one).
const RUNS: usize = 32;

/// What the decoder is doing with slots, from DECODE each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeSlots {
    /// The mode's T/R period, s.
    pub period_s: f64,
    /// UTC of the first slot it takes whole (its capture's start).
    pub first_utc: f64,
}

/// One run of the decoder: its slots from `first_utc`, until it stopped
/// (`None` while it runs).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Run {
    slots: DecodeSlots,
    stop_utc: Option<f64>,
}

/// A line to draw: a slot boundary, and whether a capture starts there
/// (green), ends there (red), or both (one slot ends as the next starts).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub utc: f64,
    pub start: bool,
    pub end: bool,
}

/// The decoder's runs, kept so lines already drawn stay on the rows they
/// belong to after DECODE stops or changes mode.
#[derive(Debug, Clone, Default)]
pub struct SlotLines {
    runs: VecDeque<Run>,
}

impl SlotLines {
    /// Each frame: what the decoder is doing (`None` when it is not running a
    /// mode with slots) at `utc`.
    pub fn feed(&mut self, now: Option<DecodeSlots>, utc: f64) {
        let open = self.runs.back().filter(|r| r.stop_utc.is_none()).map(|r| r.slots);
        if open == now {
            return;
        }
        if open.is_some()
            && let Some(r) = self.runs.back_mut()
        {
            r.stop_utc = Some(utc);
        }
        if let Some(slots) = now
            && slots.period_s > 0.0
        {
            self.runs.push_back(Run { slots, stop_utc: None });
            while self.runs.len() > RUNS {
                self.runs.pop_front();
            }
        }
    }

    /// The lines between `t_lo` and `t_hi` (UTC), newest first. A capture
    /// starts at each slot boundary from the run's first slot until it
    /// stopped, and ends at each boundary a whole period after one started,
    /// if the decoder was still running then.
    pub fn marks(&self, t_lo: f64, t_hi: f64) -> Vec<Mark> {
        let mut out: Vec<Mark> = Vec::new();
        for r in &self.runs {
            let (p, first) = (r.slots.period_s, r.slots.first_utc);
            let stop = r.stop_utc.unwrap_or(f64::INFINITY);
            let hi = t_hi.min(stop);
            let lo = t_lo.max(first);
            if hi < lo {
                continue;
            }
            let mut k = ((lo - first) / p).ceil().max(0.0);
            loop {
                let b = first + k * p;
                if b > hi + 1e-6 {
                    break;
                }
                let m = Mark { utc: b, start: b < stop, end: k >= 1.0 };
                match out.iter_mut().find(|o| (o.utc - b).abs() < 1e-3) {
                    Some(o) => {
                        o.start |= m.start;
                        o.end |= m.end;
                    }
                    None => out.push(m),
                }
                k += 1.0;
            }
        }
        out.retain(|m| m.start || m.end);
        out.sort_by(|a, b| b.utc.total_cmp(&a.utc));
        out
    }
}

/// The age of the row a boundary at `t` falls on: the newest row laid down
/// before `t`, the row after it being at or after `t` (the same crossing the
/// UTC ticks use). `times` is the rows' times, newest first. `None` when `t`
/// is newer than the newest row or not later than the oldest.
pub fn age_at(times: &[f64], t: f64) -> Option<u32> {
    // Newest first, so the times fall: count the rows at or after `t`.
    let n = times.partition_point(|&x| x >= t);
    (n > 0 && n < times.len()).then_some(n as u32)
}

/// Where a row's top edge is on screen, points: `scroll_frac` (0 to 1) of
/// the way to the next row as it glides in.
pub fn row_y(top: f32, age: u32, scroll_frac: f32, pts_per_row: f32) -> f32 {
    top + (age as f32 - (1.0 - scroll_frac)) * pts_per_row
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: f64 = 1_700_000_010.0; // A 15 s (and 7.5 s) boundary.

    #[test]
    fn a_run_marks_each_capture_start_and_end() {
        let mut l = SlotLines::default();
        // FT8 started 3 s into a slot: the first slot it takes is the next.
        let ft8 = DecodeSlots { period_s: 15.0, first_utc: B + 15.0 };
        l.feed(Some(ft8), B + 3.0);
        let m = l.marks(B, B + 46.0);
        assert_eq!(
            m,
            vec![
                Mark { utc: B + 45.0, start: true, end: true },
                Mark { utc: B + 30.0, start: true, end: true },
                Mark { utc: B + 15.0, start: true, end: false },
            ]
        );
        // Stopped 50 s in: the slot started at 45 s never ends.
        l.feed(None, B + 50.0);
        assert_eq!(l.marks(B, B + 200.0)[0], Mark { utc: B + 45.0, start: true, end: true });
        // Stopped on a boundary: that slot's end is drawn, no new start.
        let mut l = SlotLines::default();
        l.feed(Some(ft8), B);
        l.feed(None, B + 30.0);
        assert_eq!(l.marks(B, B + 99.0)[0], Mark { utc: B + 30.0, start: false, end: true });
    }

    #[test]
    fn a_change_of_mode_keeps_the_old_lines() {
        let mut l = SlotLines::default();
        let ft8 = DecodeSlots { period_s: 15.0, first_utc: B };
        let ft4 = DecodeSlots { period_s: 7.5, first_utc: B + 22.5 };
        l.feed(Some(ft8), B);
        l.feed(Some(ft8), B + 10.0); // Unchanged: still one run.
        l.feed(Some(ft4), B + 21.0);
        let m = l.marks(B, B + 31.0);
        let at = |t: f64| m.iter().find(|x| x.utc == B + t).copied();
        assert_eq!(at(0.0), Some(Mark { utc: B, start: true, end: false }));
        assert_eq!(at(15.0), Some(Mark { utc: B + 15.0, start: true, end: true }));
        assert_eq!(at(22.5), Some(Mark { utc: B + 22.5, start: true, end: false }));
        assert_eq!(at(30.0), Some(Mark { utc: B + 30.0, start: true, end: true }));
        assert_eq!(m.len(), 4);
        // WSPR's two-minute slots, from the even minute.
        let mut l = SlotLines::default();
        l.feed(Some(DecodeSlots { period_s: 120.0, first_utc: B + 30.0 }), B + 30.0);
        let m = l.marks(B, B + 400.0);
        let t: Vec<f64> = m.iter().map(|x| x.utc - B).collect();
        assert_eq!(t, vec![390.0, 270.0, 150.0, 30.0]);
    }

    #[test]
    fn a_boundary_lands_where_the_rows_cross_it() {
        // Rows every 0.5 s, newest first, the newest laid down at B + 10.2.
        let times: Vec<f64> = (0..40).map(|i| B + 10.2 - 0.5 * i as f64).collect();
        // B + 10 lies between row 0 (10.2) and row 1 (9.7): row 1's top.
        assert_eq!(age_at(&times, B + 10.0), Some(1));
        // On a row's own time: the line goes under it, as the ticks do.
        assert_eq!(age_at(&times, B + 9.7), Some(2));
        // Not laid down yet, or older than the history.
        assert_eq!(age_at(&times, B + 10.5), None);
        assert_eq!(age_at(&times, B - 30.0), None);
        // A gap (rows held over a transmission): the line sits at its edge.
        let gap = [B + 20.0, B + 19.5, B + 5.0, B + 4.5];
        assert_eq!(age_at(&gap, B + 15.0), Some(2));
    }

    #[test]
    fn rows_glide_down_as_they_come() {
        // 2 points a row: a row is fully in place at scroll 1 (and paused).
        assert_eq!(row_y(100.0, 3, 1.0, 2.0), 106.0);
        assert_eq!(row_y(100.0, 3, 0.5, 2.0), 105.0);
        assert_eq!(row_y(100.0, 0, 0.0, 2.0), 98.0);
    }
}
