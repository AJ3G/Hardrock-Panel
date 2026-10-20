//! From where a decoder heard a signal to its true radio frequency.
//!
//! AJ3G's DUO is clocked from the same GPS reference as the computer, so the
//! radio's frequency is taken as exact: the correction is in parts per million
//! and is 0 unless set. It is kept so that a radio on its own crystal can be
//! put right without touching the decoders.

/// A frequency correction for the radio's reference.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FreqCal {
    /// How far the radio's reference is above nominal, in parts per million.
    /// A positive figure means the radio is really tuned higher than its dial
    /// says. 0 for a GPS-locked radio.
    pub ppm: f64,
}

impl FreqCal {
    /// What a frequency the radio was set to really is.
    pub fn true_hz(&self, nominal_hz: f64) -> f64 {
        nominal_hz * (1.0 + self.ppm * 1e-6)
    }

    /// The radio frequency of a signal heard at `audio_hz` in a receiver whose
    /// dial (the suppressed carrier) is at `dial_hz`; on lower sideband the
    /// audio runs downwards from the dial.
    pub fn rf_of_audio(&self, dial_hz: f64, audio_hz: f64, lsb: bool) -> f64 {
        let dial = self.true_hz(dial_hz);
        if lsb { dial - audio_hz } else { dial + audio_hz }
    }

    /// The radio frequency of a signal `offset_hz` from the centre of the
    /// radio's I/Q at `centre_hz`, behind a converter whose local oscillator
    /// adds `lo_hz` (0 without one; negative for a converter whose LO is above
    /// the band it converts).
    pub fn rf_of_iq(&self, centre_hz: f64, offset_hz: f64, lo_hz: f64) -> f64 {
        self.true_hz(centre_hz) + offset_hz + lo_hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gps_locked_radio_needs_nothing() {
        let c = FreqCal::default();
        assert_eq!(c.rf_of_audio(14_074_000.0, 1_234.0, false), 14_075_234.0);
        assert_eq!(c.rf_of_audio(7_074_000.0, 1_000.0, true), 7_073_000.0);
        assert_eq!(
            c.rf_of_iq(144_000_000.0 - 116_000_000.0, 975_000.0, 116_000_000.0),
            144_975_000.0
        );
    }

    #[test]
    fn a_reference_running_high_moves_the_dial_up() {
        let c = FreqCal { ppm: 1.0 };
        assert!((c.true_hz(10_000_000.0) - 10_000_010.0).abs() < 1e-6);
    }
}
