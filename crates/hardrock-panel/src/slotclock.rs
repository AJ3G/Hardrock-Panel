//! DECODE's slot timer and decode light, as WSJT-X and SDRoxide show them: a
//! bar filling across the mode's T/R period, the seconds into it and the UTC
//! (the system clock, which AJ3G's station keeps on GPS-disciplined NTP), the
//! slot's place (1st, even, or 2nd, odd), and a light that is lit while the
//! decoder works on the slot just finished, then shows how many it decoded.

use eframe::egui::{self, Color32, RichText, Sense, Stroke, StrokeKind, vec2};

use crate::theme;

/// Where `utc` falls among slots of `period_s`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SlotTime {
    /// UTC of the slot's start, Unix seconds.
    pub start: f64,
    /// Seconds into it.
    pub into_s: f64,
    /// How far through, 0 to 1.
    pub frac: f64,
    /// An even slot (WSJT-X's "1st"): its number from 00:00 UTC is even.
    pub even: bool,
}

impl SlotTime {
    pub(crate) fn at(utc: f64, period_s: f64) -> Self {
        let n = (utc / period_s).floor();
        let start = n * period_s;
        let into_s = (utc - start).clamp(0.0, period_s);
        SlotTime { start, into_s, frac: into_s / period_s, even: (n as i64).rem_euclid(2) == 0 }
    }
}

/// What the decode light shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Light {
    /// Not decoding.
    Off,
    /// Decoding, but no whole slot heard yet.
    Waiting,
    /// The slot that started at this UTC is being decoded.
    Decoding(i64),
    /// The last slot decoded, and how many it held.
    Done(i64, usize),
}

/// The light, from the time now, when the decoder started (its first audio),
/// how late into a slot it may join (`max_late_s`), and the last slot it
/// reported.
///
/// The slot just finished is being decoded if the decoder heard it from
/// (near enough) its start and has not reported it yet; it stays lit until
/// the result comes or the next slot ends.
pub(crate) fn light(
    utc: f64,
    period_s: f64,
    started: Option<f64>,
    max_late_s: f64,
    last: Option<(i64, usize)>,
) -> Light {
    let Some(started) = started else { return Light::Off };
    let now = SlotTime::at(utc, period_s);
    let finished = now.start - period_s;
    let heard = started <= finished + max_late_s;
    let reported = last.is_some_and(|(s, _)| s as f64 >= finished - 0.5);
    match last {
        _ if heard && !reported => Light::Decoding(finished.round() as i64),
        Some((s, n)) => Light::Done(s, n),
        None => Light::Waiting,
    }
}

/// The bar, the seconds and the UTC, the slot's place and the light, one row.
pub(crate) fn row(ui: &mut egui::Ui, utc: f64, period_s: f64, light: Light) {
    let t = SlotTime::at(utc, period_s);
    let blue = theme::CYAN();
    let dim = theme::CYAN_DIM();
    ui.horizontal(|ui| {
        // The slot's place.
        let (place, hover) = if t.even {
            ("1st", "An even slot: its number from 00:00 UTC is even (WSJT-X's Tx even/1st)")
        } else {
            ("2nd", "An odd slot (WSJT-X's 2nd)")
        };
        ui.label(RichText::new(place).monospace().size(12.5).strong().color(blue))
            .on_hover_text(hover);

        // The bar, across what is left after the readouts.
        let w = (ui.available_width() - 260.0).max(80.0);
        let h = 14.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, theme::INPUT_BG());
        let mut fill = rect;
        fill.set_width(rect.width() * t.frac as f32);
        p.rect_filled(fill, 2.0, blue.gamma_multiply(0.75));
        // A tick every second up to 30 s periods, every 10 s past that, so the
        // scale stays readable.
        let step = if period_s <= 30.0 { 1.0 } else { 10.0 };
        let mut s = step;
        while s < period_s {
            let x = rect.left() + rect.width() * (s / period_s) as f32;
            let tall = if period_s <= 30.0 { s % 5.0 == 0.0 } else { s % 60.0 == 0.0 };
            let y0 = if tall { rect.top() } else { rect.bottom() - h * 0.4 };
            p.line_segment(
                [egui::pos2(x, y0), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, dim),
            );
            s += step;
        }
        p.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::LINE()), StrokeKind::Inside);
        resp.on_hover_text(format!(
            "{:.1} s into this {period_s} s slot, which started at {}",
            t.into_s,
            crate::decode::hms(t.start)
        ));

        ui.label(
            RichText::new(format!("{:>5.1}/{period_s} s", t.into_s))
                .monospace()
                .size(12.5)
                .color(theme::TEXT()),
        );
        ui.label(
            RichText::new(format!("{} UTC", crate::decode::hms(utc)))
                .monospace()
                .size(12.5)
                .color(theme::TEXT_STRONG()),
        )
        .on_hover_text("The system clock, kept on UTC by NTP from GPS");
        lamp(ui, light);
    });
}

/// The decode light: a lamp and what it means.
fn lamp(ui: &mut egui::Ui, light: Light) {
    let blue = theme::CYAN();
    let (lit, text, hover) = match light {
        Light::Off => (false, "—".to_string(), "Not decoding".to_string()),
        Light::Waiting => {
            (false, "WAIT".to_string(), "Waiting for the first whole slot".to_string())
        }
        Light::Decoding(s) => (
            true,
            "DECODING".to_string(),
            format!("Decoding the slot that started at {}", crate::decode::hms(s as f64)),
        ),
        Light::Done(s, n) => (
            false,
            format!("{n} DECODED"),
            format!("{n} decoded from the slot that started at {}", crate::decode::hms(s as f64)),
        ),
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    let c = rect.center();
    let p = ui.painter();
    if lit {
        p.circle_filled(c, 7.0, blue.gamma_multiply(0.3));
        p.circle_filled(c, 5.0, blue);
    } else {
        p.circle_filled(c, 5.0, Color32::TRANSPARENT);
        p.circle_stroke(c, 5.0, Stroke::new(1.5, theme::CYAN_DIM()));
    }
    let r =
        resp.union(ui.label(RichText::new(text).monospace().size(12.5).strong().color(if lit {
            blue
        } else {
            theme::TEXT()
        })));
    r.on_hover_text(hover);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_is_placed_from_00_utc() {
        // 01:15:37.5 is 7.5 s into FT8's even slot at 01:15:30.
        let utc = 1_792_545_337.5;
        let t = SlotTime::at(utc, 15.0);
        assert_eq!((t.start, t.into_s, t.frac), (1_792_545_330.0, 7.5, 0.5));
        assert!(t.even);
        // FT4's 7.5 s slots: 01:15:37.5 starts one, odd.
        let t = SlotTime::at(utc, 7.5);
        assert_eq!((t.start, t.into_s), (utc, 0.0));
        assert!(!t.even);
        // WSPR's two minutes from the even minute: 01:14:00.
        let t = SlotTime::at(utc, 120.0);
        assert_eq!(t.start, 1_792_545_240.0);
        assert!(!t.even, "01:14 is slot 14_937_877, odd");
    }

    #[test]
    fn the_light_follows_the_decoder() {
        let p = 15.0;
        let t0 = 1_792_545_330.0; // A slot boundary.
        assert_eq!(light(t0 + 3.0, p, None, 1.0, None), Light::Off);
        // Started 3 s into a slot: that slot is not decoded, so wait.
        assert_eq!(light(t0 + 3.0, p, Some(t0 + 3.0), 1.0, None), Light::Waiting);
        assert_eq!(light(t0 + 16.0, p, Some(t0 + 3.0), 1.0, None), Light::Waiting);
        // The next slot was heard whole: lit once it ends, until reported.
        assert_eq!(light(t0 + 31.0, p, Some(t0 + 3.0), 1.0, None), Light::Decoding(1_792_545_345));
        let done = Some((1_792_545_345, 12));
        assert_eq!(light(t0 + 32.0, p, Some(t0 + 3.0), 1.0, done), Light::Done(1_792_545_345, 12));
        // The slot after ends: lit again, the old count kept for later.
        assert_eq!(light(t0 + 45.5, p, Some(t0 + 3.0), 1.0, done), Light::Decoding(1_792_545_360));
        // Joined within the second the decoder allows: that slot counts.
        assert_eq!(light(t0 + 15.2, p, Some(t0 + 0.4), 1.0, None), Light::Decoding(1_792_545_330));
    }
}
