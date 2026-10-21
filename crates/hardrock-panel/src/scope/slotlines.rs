//! Slot lines on the waterfall (AJ3G's request, decision 51): while DECODE
//! runs a mode with T/R slots, a green line across the waterfall where each
//! slot starts, a red one where the mode's transmissions end inside it (an
//! on-time signal's last symbol: FT8 13.14 s in, FT4 5.54 s, WSPR 111.6 s),
//! and a faint yellow tint over the dead time from there to the next slot's
//! green, where no on-time signal is. AJ3G: "Green TX Start, RED when finish,
//! and yellow for the no information". WSJT-X draws one line per T/R period
//! at its boundary, labelled with the UTC; the green lines are drawn the same
//! way, a thin full-width line with the time at the left. Everything scrolls
//! with the rows as the UTC ticks do (it is placed by each row's own time).
//!
//! MSK144's pings come any time in the slot, so it has no end: no red line
//! and no tint. A slot cut short by DECODE stopping before its signal end
//! gets no red line and no tint either.
//!
//! Red and yellow are kept for warnings everywhere else on the panel. These
//! three are the named exception, as the waterfall palettes are (see
//! `palette.rs`): AJ3G asked for green, red and yellow by name, and here they
//! mean "transmissions start", "transmissions end" and "nothing sent", not a
//! warning.

use std::collections::VecDeque;

use eframe::egui::Color32;

/// The slot start's green.
pub const START_INK: Color32 = Color32::from_rgb(0x3c, 0xe0, 0x5a);
/// The signal end's red: part of the named exception to "red for warnings
/// only".
pub const END_INK: Color32 = Color32::from_rgb(0xff, 0x48, 0x48);
/// The dead time's yellow, 18 % over the rows (chosen by eye: at 15 % it
/// read as grey on the ELAD palette's navy) so a late or long signal still
/// shows through it: part of the same exception.
pub const DEAD_TINT: Color32 = Color32::from_rgba_premultiplied(0x2e, 0x29, 0x00, 0x2e);

/// The most decoder runs remembered (each start, stop or change of mode is
/// one).
const RUNS: usize = 32;

/// What the decoder is doing with slots, from DECODE each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeSlots {
    /// The mode's T/R period, s.
    pub period_s: f64,
    /// UTC of the first slot it takes whole.
    pub first_utc: f64,
    /// Where an on-time transmission ends, seconds into the slot; `None`
    /// for a mode whose signals come any time (MSK144).
    pub end_s: Option<f64>,
}

/// One run of the decoder: its slots from `first_utc`, until it stopped
/// (`None` while it runs).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Run {
    slots: DecodeSlots,
    stop_utc: Option<f64>,
}

/// What a line marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A slot's start: green, with its UTC.
    Start,
    /// Where the slot's transmissions end: red.
    End,
}

/// A line to draw, at its UTC.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub utc: f64,
    pub line: Line,
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

    /// Each slot of every run that starts by `t_hi` and whose period reaches
    /// past `t_lo`: its start, where its signal ends (if it has an end and
    /// the decoder ran until then) and when the run stopped.
    fn slots(&self, t_lo: f64, t_hi: f64) -> impl Iterator<Item = (f64, Option<f64>, f64)> + '_ {
        self.runs.iter().flat_map(move |r| {
            let (p, first) = (r.slots.period_s, r.slots.first_utc);
            let stop = r.stop_utc.unwrap_or(f64::INFINITY);
            let k0 = ((t_lo - first) / p).floor().max(0.0) - 1.0;
            let k0 = k0.max(0.0) as u64;
            (k0..)
                .map(move |k| first + k as f64 * p)
                .take_while(move |&b| b <= t_hi.min(stop))
                .filter(move |&b| b < stop && b + p >= t_lo)
                .map(move |b| {
                    let end = r.slots.end_s.map(|e| b + e).filter(|&e| e <= stop);
                    (b, end, stop)
                })
        })
    }

    /// The lines between `t_lo` and `t_hi` (UTC), newest first: green at
    /// each slot's start from the run's first slot until it stopped, red
    /// where each slot's signal ends if the decoder was still running then.
    pub fn marks(&self, t_lo: f64, t_hi: f64) -> Vec<Mark> {
        let inside = |t: f64| t >= t_lo && t <= t_hi;
        let mut out: Vec<Mark> = Vec::new();
        for (b, end, _) in self.slots(t_lo, t_hi) {
            if inside(b) {
                out.push(Mark { utc: b, line: Line::Start });
            }
            if let Some(e) = end.filter(|&e| inside(e)) {
                out.push(Mark { utc: e, line: Line::End });
            }
        }
        out.sort_by(|a, b| b.utc.total_cmp(&a.utc));
        out.dedup_by(|a, b| a.line == b.line && (a.utc - b.utc).abs() < 1e-3);
        out
    }

    /// The dead times between `t_lo` and `t_hi` (UTC), newest first, each
    /// `(from, to)`: from a slot's red line to the next slot's start, or to
    /// when the decoder stopped if that came first. None without a red line.
    pub fn dead(&self, t_lo: f64, t_hi: f64) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for r in &self.runs {
            let stop = r.stop_utc.unwrap_or(f64::INFINITY);
            let p = r.slots.period_s;
            let one = SlotLines { runs: VecDeque::from([*r]) };
            for (b, end, _) in one.slots(t_lo, t_hi) {
                let Some(e) = end else { continue };
                let (from, to) = (e.max(t_lo), (b + p).min(stop).min(t_hi));
                if to > from {
                    out.push((from, to));
                }
            }
        }
        out.sort_by(|a, b| b.0.total_cmp(&a.0));
        out
    }
}

/// The age of the row a boundary at `t` falls on: the newest row laid down
/// before `t`, the row after it being at or after `t` (the same crossing the
/// UTC ticks use). `times` is the rows' times, newest first. `None` when `t`
/// is newer than the newest row or not later than the oldest.
pub fn age_at(times: &[f64], t: f64) -> Option<u32> {
    let n = edge_at(times, t);
    (n > 0 && n < times.len() as u32).then_some(n)
}

/// The rows a span of time from `from` to `to` covers, as the ages of its
/// edges `(newer, older)`: the rows `newer..older`, whose times are in
/// `from..to`. The newer edge is where a line at `to` goes, the older where a
/// line at `from` goes. `None` when no row is in it.
pub fn age_span(times: &[f64], from: f64, to: f64) -> Option<(u32, u32)> {
    let (newer, older) = (edge_at(times, to), edge_at(times, from));
    (older > newer).then_some((newer, older))
}

/// How many rows were laid down at or after `t` (the times fall, newest
/// first): the age of the edge a line at `t` sits on.
fn edge_at(times: &[f64], t: f64) -> u32 {
    times.partition_point(|&x| x >= t) as u32
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
    const FT8: DecodeSlots =
        DecodeSlots { period_s: 15.0, first_utc: B + 15.0, end_s: Some(13.14) };

    fn at(m: &[Mark]) -> Vec<(f64, Line)> {
        m.iter().map(|x| (((x.utc - B) * 100.0).round() / 100.0, x.line)).collect()
    }

    #[test]
    fn green_at_each_slot_start_and_red_at_its_signal_end() {
        let mut l = SlotLines::default();
        // FT8 started 3 s into a slot: the first slot it takes is the next.
        l.feed(Some(FT8), B + 3.0);
        assert_eq!(
            at(&l.marks(B, B + 46.0)),
            vec![
                (45.0, Line::Start),
                (43.14, Line::End),
                (30.0, Line::Start),
                (28.14, Line::End),
                (15.0, Line::Start),
            ]
        );
        // Stopped 50 s in, before the 45 s slot's signal end: no red for it.
        l.feed(None, B + 50.0);
        assert_eq!(
            at(&l.marks(B + 40.0, B + 200.0)),
            vec![(45.0, Line::Start), (43.14, Line::End)]
        );
        // Stopped after the signal end: its red stays.
        let mut l = SlotLines::default();
        l.feed(Some(FT8), B);
        l.feed(None, B + 29.0);
        assert_eq!(at(&l.marks(B + 20.0, B + 99.0)), vec![(28.14, Line::End)]);
    }

    #[test]
    fn msk144_has_no_end_and_no_dead_time() {
        let mut l = SlotLines::default();
        l.feed(Some(DecodeSlots { end_s: None, ..FT8 }), B);
        assert_eq!(at(&l.marks(B, B + 31.0)), vec![(30.0, Line::Start), (15.0, Line::Start)]);
        assert!(l.dead(B, B + 31.0).is_empty());
    }

    #[test]
    fn a_change_of_mode_keeps_the_old_lines() {
        let mut l = SlotLines::default();
        let ft8 = DecodeSlots { first_utc: B, ..FT8 };
        let ft4 = DecodeSlots { period_s: 7.5, first_utc: B + 22.5, end_s: Some(5.54) };
        l.feed(Some(ft8), B);
        l.feed(Some(ft8), B + 10.0); // Unchanged: still one run.
        l.feed(Some(ft4), B + 21.0);
        assert_eq!(
            at(&l.marks(B, B + 31.0)),
            vec![
                (30.0, Line::Start),
                (28.04, Line::End),
                (22.5, Line::Start),
                (15.0, Line::Start),
                (13.14, Line::End),
                (0.0, Line::Start),
            ]
        );
        // WSPR's two-minute slots, its signal ending 111.59 s in.
        let mut l = SlotLines::default();
        l.feed(Some(DecodeSlots { period_s: 120.0, first_utc: B, end_s: Some(111.592) }), B);
        assert_eq!(
            at(&l.marks(B, B + 130.0)),
            vec![(120.0, Line::Start), (111.59, Line::End), (0.0, Line::Start)]
        );
    }

    #[test]
    fn the_dead_time_runs_from_red_to_the_next_green() {
        let mut l = SlotLines::default();
        l.feed(Some(FT8), B + 3.0);
        let d = l.dead(B, B + 50.0);
        let r = |x: f64| ((x - B) * 100.0).round() / 100.0;
        let d: Vec<(f64, f64)> = d.iter().map(|&(a, b)| (r(a), r(b))).collect();
        assert_eq!(d, vec![(43.14, 45.0), (28.14, 30.0)]);
        // Clipped to what is asked for.
        let d = l.dead(B + 29.0, B + 44.0);
        assert_eq!(
            d.iter().map(|&(a, b)| (r(a), r(b))).collect::<Vec<_>>(),
            vec![(43.14, 44.0), (29.0, 30.0)]
        );
        // Stopped in the dead time: it ends at the stop. Stopped before the
        // red line: none for that slot.
        l.feed(None, B + 44.0);
        let d = l.dead(B, B + 99.0);
        assert_eq!(
            d.iter().map(|&(a, b)| (r(a), r(b))).collect::<Vec<_>>(),
            vec![(43.14, 44.0), (28.14, 30.0)]
        );
        let mut l = SlotLines::default();
        l.feed(Some(FT8), B + 3.0);
        l.feed(None, B + 40.0);
        assert_eq!(l.dead(B, B + 99.0).len(), 1);
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
    fn the_tint_covers_the_rows_between_its_lines() {
        let times: Vec<f64> = (0..40).map(|i| B + 10.2 - 0.5 * i as f64).collect();
        // 8.14 to 10.0: rows 1 (9.7) to 4 (8.2), from the green line's row
        // edge (row 1's top) down to the red line's (row 5's top).
        assert_eq!(age_span(&times, B + 8.14, B + 10.0), Some((1, 5)));
        assert_eq!(age_at(&times, B + 10.0), Some(1));
        assert_eq!(age_at(&times, B + 8.14), Some(5));
        // Still running past the newest row: from the top.
        assert_eq!(age_span(&times, B + 9.0, B + 15.0), Some((0, 3)));
        // Older than the history at its start: down to the bottom.
        assert_eq!(age_span(&times, B - 40.0, B - 9.0), Some((39, 40)));
        // Between two rows, none in it.
        assert_eq!(age_span(&times, B + 9.8, B + 10.1), None);
    }

    #[test]
    fn rows_glide_down_as_they_come() {
        // 2 points a row: a row is fully in place at scroll 1 (and paused).
        assert_eq!(row_y(100.0, 3, 1.0, 2.0), 106.0);
        assert_eq!(row_y(100.0, 3, 0.5, 2.0), 105.0);
        assert_eq!(row_y(100.0, 0, 0.0, 2.0), 98.0);
    }
}
