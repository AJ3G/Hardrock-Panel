//! The offset bar (decision 55): a measuring cursor across the spectrum and
//! the waterfall that reads how far it is from the receiver's dial, in the
//! same hertz as DECODE's AF column, and where it is.
//!
//! It retunes nothing. It stays on the frequency it was put on, as a
//! marker does, so a signal under it keeps its bar while the dial moves and
//! the reading follows the dial.

/// The bar, where it is: the radio's own frequency, Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OffsetBar {
    pub hz: f64,
}

impl OffsetBar {
    /// Its offset from the dial `dial_hz`, Hz, as DECODE's AF column has it:
    /// the audio a signal under the bar would make in the receiver, so above
    /// the dial in USB and below it in LSB count up.
    pub fn offset_hz(&self, dial_hz: f64, lower: bool) -> f64 {
        if lower { dial_hz - self.hz } else { self.hz - dial_hz }
    }

    /// Dropped at the receiver: the middle of its passband (`[low, high]`
    /// Hz from the dial), where a signal it is listening to would be.
    pub fn at_receiver(rx_hz: f64, passband: (f32, f32)) -> OffsetBar {
        OffsetBar { hz: rx_hz + 0.5 * (passband.0 as f64 + passband.1 as f64) }
    }
}

/// An offset as the bar's readout writes it: whole hertz as DECODE's AF
/// column does, with its sign, and the thousands apart so a figure read
/// across the waterfall is read right ("+1 234 Hz", "-56 Hz").
/// The bar's colour: red, where AJ3G will place his transmit signal (22 Oct
/// 2026: "change that to RED, and thin it up a bit"). A named exception to
/// red-for-warnings-only, as the slot lines are (`slotlines.rs`).
pub const BAR_INK: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(0xff, 0x48, 0x48);

pub fn fmt_offset(hz: f64) -> String {
    let r = hz.round();
    let sign = if r < 0.0 { "-" } else { "+" };
    let a = r.abs() as u64;
    let body = if a >= 1000 { format!("{} {:03}", a / 1000, a % 1000) } else { a.to_string() };
    format!("{sign}{body} Hz")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_reads_its_offset_from_the_dial_as_decode_does() {
        let bar = OffsetBar { hz: 14_075_234.4 };
        assert!((bar.offset_hz(14_074_000.0, false) - 1234.4).abs() < 1e-6);
        // In LSB the audio rises as the signal goes down.
        let bar = OffsetBar { hz: 7_172_500.0 };
        assert_eq!(bar.offset_hz(7_175_000.0, true), 2500.0);
        // Below the dial in USB: negative, nothing heard there.
        assert_eq!(OffsetBar { hz: 14_073_900.0 }.offset_hz(14_074_000.0, false), -100.0);
        assert_eq!(fmt_offset(1234.4), "+1 234 Hz");
        assert_eq!(fmt_offset(-56.0), "-56 Hz");
        assert_eq!(fmt_offset(2500.0), "+2 500 Hz");
        assert_eq!(fmt_offset(0.2), "+0 Hz");
        assert_eq!(fmt_offset(-12_005.0), "-12 005 Hz");
    }

    #[test]
    fn dropped_at_the_receiver_it_is_in_the_middle_of_the_passband() {
        let b = OffsetBar::at_receiver(14_074_000.0, (200.0, 3000.0));
        assert_eq!(b.hz, 14_075_600.0);
        assert_eq!(b.offset_hz(14_074_000.0, false), 1600.0);
        // LSB's passband is below the dial; the reading is still positive.
        let b = OffsetBar::at_receiver(7_175_000.0, (-2900.0, -100.0));
        assert_eq!(b.offset_hz(7_175_000.0, true), 1500.0);
    }
}
