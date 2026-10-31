//! The offset bar (decision 55): a measuring cursor across the spectrum and
//! the waterfall that reads how far it is from the receiver's dial, in the
//! same hertz as DECODE's AF column, and where it is.
//!
//! It is also the transmit marker (31 Oct 2026): with TX follows RX on, the
//! radio transmits on it ([`tx_at`]). It stays on the frequency it was put
//! on, as a marker does, so a signal under it keeps its bar while the dial
//! moves and the reading follows the dial. SAFETY INTERLOCK
//! (docs/safety-interlocks.md, #9): it is locked while transmitting.

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

/// Where the radio transmits, worked out from the bar (31 Oct 2026, AJ3G:
/// "Shouldnt the TX Offset Pole allow TX anywhere inside the window on
/// Scope?"). TX follows RX then puts VFO B on `dial_hz`, in split while it is
/// off VFO A.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TxAt {
    /// VFO B: the dial the radio transmits from, Hz.
    pub dial_hz: f64,
    /// In a data mode, the audio offset the data program must send at for
    /// the signal to land on the bar, Hz from `dial_hz`.
    pub audio_hz: Option<f64>,
    /// The bar decided it; `false` is the receiver's own dial, as before the
    /// bar was a TX marker.
    pub from_bar: bool,
}

/// Where to transmit for the receiver at `rx_hz` (mode `lower` sideband or
/// not, passband `pb`), the bar `bar`, a data mode's audio range `data`
/// (`[lo, hi]` Hz from the dial, `None` outside the data modes) and the span
/// on screen `[span_lo, span_hi]`.
///
/// * No bar, a bar off the span, or a bar on another band than the
///   receiver's (left behind by a band change): the receiver's own dial.
///   A transmitter is never sent to a band the operator is not looking at.
/// * Voice and CW: the bar moves the transmission as far as it is from
///   where it is dropped (the middle of the passband), so a bar dropped
///   there transmits on the receiver, as before, and a bar 2 kHz up
///   transmits 2 kHz up.
/// * A data mode: the dial stays the receiver's and the bar's offset is the
///   audio offset (WSJT-X's red goalpost), while that lies in the mode's
///   audio range; past it, the dial moves so the middle of the range lands
///   on the bar.
pub fn tx_at(
    rx_hz: f64,
    lower: bool,
    pb: (f32, f32),
    bar: Option<OffsetBar>,
    data: Option<(u32, u32)>,
    span: (f64, f64),
) -> TxAt {
    let receiver = TxAt { dial_hz: rx_hz, audio_hz: None, from_bar: false };
    let Some(bar) = bar else { return receiver };
    let same_band = match (super::bandplan::band_at(rx_hz), super::bandplan::band_at(bar.hz)) {
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    };
    if !(span.0..=span.1).contains(&bar.hz) || !same_band {
        return receiver;
    }
    match data {
        Some((lo, hi)) => {
            let off = bar.offset_hz(rx_hz, lower);
            if (lo as f64..=hi as f64).contains(&off) {
                TxAt { dial_hz: rx_hz, audio_hz: Some(off.round()), from_bar: true }
            } else {
                let mid = (0.5 * (lo as f64 + hi as f64)).round();
                let dial = if lower { bar.hz + mid } else { bar.hz - mid };
                TxAt { dial_hz: dial.round(), audio_hz: Some(mid), from_bar: true }
            }
        }
        None => {
            let dropped = OffsetBar::at_receiver(rx_hz, pb).hz;
            TxAt { dial_hz: (rx_hz + bar.hz - dropped).round(), audio_hz: None, from_bar: true }
        }
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

    const SPAN: (f64, f64) = (13_990_000.0, 14_150_000.0);

    #[test]
    fn with_no_bar_or_a_bar_left_on_another_band_tx_stays_on_the_receiver() {
        let rx = 14_074_000.0;
        let at = tx_at(rx, false, (200.0, 3000.0), None, None, SPAN);
        assert_eq!(at, TxAt { dial_hz: rx, audio_hz: None, from_bar: false });
        // Left on 40 m by a band change: never transmit there.
        let bar = Some(OffsetBar { hz: 7_075_000.0 });
        let wide = (7_000_000.0, 14_400_000.0);
        assert!(!tx_at(rx, false, (200.0, 3000.0), bar, None, wide).from_bar);
        // Off the span on screen.
        let bar = Some(OffsetBar { hz: 14_300_000.0 });
        assert!(!tx_at(rx, false, (200.0, 3000.0), bar, None, SPAN).from_bar);
    }

    #[test]
    fn in_voice_the_bar_moves_tx_as_far_as_it_is_from_where_it_drops() {
        let rx = 14_100_000.0;
        let pb = (200.0, 3000.0);
        let dropped = OffsetBar::at_receiver(rx, pb);
        assert_eq!(tx_at(rx, false, pb, Some(dropped), None, SPAN).dial_hz, rx);
        let up = OffsetBar { hz: dropped.hz + 2000.0 };
        let at = tx_at(rx, false, pb, Some(up), None, SPAN);
        assert_eq!((at.dial_hz, at.audio_hz, at.from_bar), (rx + 2000.0, None, true));
        // LSB: the same rule.
        let lsb = (-3000.0, -200.0);
        let rx = 14_050_000.0;
        let down = OffsetBar { hz: OffsetBar::at_receiver(rx, lsb).hz - 5000.0 };
        assert_eq!(tx_at(rx, true, lsb, Some(down), None, SPAN).dial_hz, rx - 5000.0);
    }

    #[test]
    fn in_a_data_mode_the_bar_is_the_audio_offset_until_it_leaves_the_range() {
        let rx = 14_074_000.0;
        let ft8 = Some((200, 3000));
        let at = tx_at(rx, false, (200.0, 3000.0), Some(OffsetBar { hz: rx + 1234.4 }), ft8, SPAN);
        assert_eq!(at, TxAt { dial_hz: rx, audio_hz: Some(1234.0), from_bar: true });
        // 10 kHz up: the dial moves so 1600 Hz of audio lands on the bar.
        let bar = OffsetBar { hz: rx + 10_000.0 };
        let at = tx_at(rx, false, (200.0, 3000.0), Some(bar), ft8, SPAN);
        assert_eq!(at, TxAt { dial_hz: rx + 8400.0, audio_hz: Some(1600.0), from_bar: true });
        assert_eq!(bar.offset_hz(at.dial_hz, false), 1600.0);
    }
}
