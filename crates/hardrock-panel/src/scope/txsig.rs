//! The scope over a transmission (decisions 74 and 83): nothing the DUO's
//! I/Q gives while keyed is shown (it is silent or flooded by the
//! transmitter). What the panel is sending (a TX window over, TX TUNE's tone,
//! a RIG TUNE carrier) is analysed as it goes out by the TX monitor
//! ([`super::txmon`]) and drawn where it lands in RF, as ordinary spectrum
//! frames. What an outside program sends is not known here, so then the
//! band is held at its last received level, dimmed, and scrolls on.
//!
//! Here: what is on the air and where (split or not, USB or LSB), and how
//! many dimmed rows a repaint owes the waterfall in the outside case.

use eframe::egui::Color32;

/// Our own transmission's colour, deep orange: GLOBE's and the TX window's
/// (`globe::TX_COLOUR`, repeated here so the scope stands alone, as the
/// scope demo builds it). Clear of the warning yellow (AJ3G, 11 Nov 2026).
pub const TX_INK: Color32 = Color32::from_rgb(0xff, 0x7a, 0x1a);

/// Audio the panel sent to the radio since the scope last looked, every
/// sample in order, at its output's rate and level (`audio::Player::recent`,
/// `audio::Tone::recent`).
#[derive(Debug, Clone, PartialEq)]
pub struct TxAudio {
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// The most samples taken from a tap in one repaint: two seconds at 48 kHz.
/// A repaint later than that has lost the older ones anyway (the tap holds
/// a second).
pub const TX_TAP: usize = 96_000;

/// What the radio is transmitting that the panel knows of, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct TxSignal {
    /// The transmit dial, the radio's own Hz: VFO B in split, else the
    /// receive VFO.
    pub dial_hz: f64,
    /// Lower sideband: the audio lands below the dial.
    pub lower: bool,
    pub what: TxWhat,
    /// What is on the air, in a few words: "FT8 over", "TX TUNE"…
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TxWhat {
    /// The audio the panel is playing into the radio's USB audio: what it
    /// sent since the last repaint.
    Audio(TxAudio),
    /// The radio's own TUNE carrier (RIG TUNE, `TX2;`), at the dial.
    Carrier,
}

impl TxSignal {
    /// From the radio's status, what the panel is playing (`audio`: `None`
    /// while it plays nothing) and the TX window's over in progress
    /// (`over`, "FT8 over", "WSPR beacon"): `None` when the panel sends
    /// nothing it knows of (an outside program keying, or no dial read yet).
    pub fn from_rig(
        r: &rigcat::RigStatus,
        audio: Option<TxAudio>,
        over: Option<&str>,
    ) -> Option<TxSignal> {
        let dial_hz = tx_dial(r.freq_hz, r.freq_b_hz, r.vfo, r.split)?;
        let lower = r.mode == Some("LSB");
        let keying = r.tuning.map(|t| t.keying);
        let what = match audio {
            Some(a) => TxWhat::Audio(a),
            None if keying == Some(rigcat::Keying::Carrier) => TxWhat::Carrier,
            None => return None,
        };
        let name = match keying {
            Some(k) => on_air(k, over),
            None => over.unwrap_or("our audio").to_string(),
        };
        Some(TxSignal { dial_hz, lower, what, name })
    }
}

/// What the panel has on the air, named as the operator knows it, for a
/// TUNE of the panel's keyed with `keying`: TX TUNE's tone, RIG TUNE's
/// carrier, or the TX window's over (`over`: "FT8 over", "WSPR beacon").
pub fn on_air(keying: rigcat::Keying, over: Option<&str>) -> String {
    match keying {
        rigcat::Keying::Audio => "TX TUNE".into(),
        rigcat::Keying::Carrier => "RIG TUNE".into(),
        rigcat::Keying::Data => over.unwrap_or("a data over").into(),
    }
}

/// The dial the radio transmits on, Hz: the receive VFO (`vfo`: 0 A, 1 B),
/// or the other one in split. A memory channel (2) is taken as VFO A.
pub fn tx_dial(
    freq_a: Option<u64>,
    freq_b: Option<u64>,
    vfo: Option<u8>,
    split: Option<bool>,
) -> Option<f64> {
    let rx_b = vfo == Some(1);
    let tx_b = rx_b != split.unwrap_or(false);
    let hz = if tx_b { freq_b } else { freq_a }?;
    (hz > 0).then_some(hz as f64)
}

/// Where audio at `audio_hz` lands in RF from the dial: above it on USB,
/// below it on LSB.
pub fn place(dial_hz: f64, lower: bool, audio_hz: f64) -> f64 {
    if lower { dial_hz - audio_hz } else { dial_hz + audio_hz }
}

/// A dimmed waterfall row laid down while the I/Q was not shown and our
/// signal was not known: when (UTC s, the row's own time).
#[derive(Debug, Clone, PartialEq)]
pub struct TxRow {
    pub utc: f64,
}

/// The rows laid down while held, oldest first, and the one at `utc`.
pub fn row_at(rows: &std::collections::VecDeque<TxRow>, utc: f64) -> Option<&TxRow> {
    let i = rows.partition_point(|r| r.utc < utc - 1e-6);
    rows.get(i).filter(|r| (r.utc - utc).abs() <= 1e-6)
}

/// How many dimmed rows are owed at `now` when the last went at `last`
/// (NaN: none yet, so one now) and a row is due every `period` s, and when
/// the newest of them counts as laid. At most four at once; after a long
/// gap (a hitch, a paused display) one, and the count starts again.
pub fn held_rows_due(last: f64, now: f64, period: f64) -> (u32, f64) {
    let period = period.max(1e-3);
    if !last.is_finite() || now - last > 1.0 || now < last {
        return (1, now);
    }
    let n = ((now - last) / period).floor() as u32;
    if n == 0 {
        return (0, last);
    }
    let n = n.min(4);
    (n, if n == 4 { now } else { last + n as f64 * period })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transmit_dial_is_the_receive_vfo_or_the_other_one_in_split() {
        let (a, b) = (Some(14_074_000), Some(14_075_500));
        assert_eq!(tx_dial(a, b, Some(0), Some(false)), Some(14_074_000.0));
        assert_eq!(tx_dial(a, b, Some(0), Some(true)), Some(14_075_500.0));
        assert_eq!(tx_dial(a, b, None, None), Some(14_074_000.0));
        // Receiving on B: B, or A in split.
        assert_eq!(tx_dial(a, b, Some(1), Some(false)), Some(14_075_500.0));
        assert_eq!(tx_dial(a, b, Some(1), Some(true)), Some(14_074_000.0));
        // Not read yet: nothing to place.
        assert_eq!(tx_dial(a, None, Some(0), Some(true)), None);
        assert_eq!(tx_dial(Some(0), b, Some(0), Some(false)), None);
    }

    #[test]
    fn audio_lands_above_the_dial_on_usb_and_below_on_lsb() {
        assert_eq!(place(14_074_000.0, false, 1500.0), 14_075_500.0);
        assert_eq!(place(7_074_000.0, true, 1500.0), 7_072_500.0);
    }

    #[test]
    fn what_is_on_the_air_is_found_and_named() {
        // AJ3G's case: receive on A, transmit split on B as TX follows RX.
        let now = std::time::Instant::now();
        let tuning = |keying| Some(rigcat::Tuning { since: now, until: now, keying });
        let mut r = rigcat::RigStatus {
            freq_hz: Some(14_000_000),
            freq_b_hz: Some(14_074_000),
            vfo: Some(0),
            split: Some(true),
            mode: Some("USB"),
            tuning: tuning(rigcat::Keying::Data),
            ..Default::default()
        };
        let audio = TxAudio { rate: 48_000, samples: vec![0.0; 10] };
        let sig = TxSignal::from_rig(&r, Some(audio.clone()), Some("FT8 over")).unwrap();
        assert_eq!((sig.dial_hz, sig.lower), (14_074_000.0, false));
        assert_eq!(sig.name, "FT8 over");
        assert_eq!(sig.what, TxWhat::Audio(audio.clone()));
        r.tuning = tuning(rigcat::Keying::Audio);
        assert_eq!(TxSignal::from_rig(&r, Some(audio), None).unwrap().name, "TX TUNE");
        // RIG TUNE: the carrier, with no audio of ours.
        r.tuning = tuning(rigcat::Keying::Carrier);
        r.mode = Some("LSB");
        let sig = TxSignal::from_rig(&r, None, None).unwrap();
        assert_eq!((sig.what, sig.name.as_str(), sig.lower), (TxWhat::Carrier, "RIG TUNE", true));
        // Nothing of ours known: nothing to monitor.
        r.tuning = None;
        assert_eq!(TxSignal::from_rig(&r, None, None), None);
        assert_eq!(on_air(rigcat::Keying::Data, Some("WSPR beacon")), "WSPR beacon");
        assert_eq!(on_air(rigcat::Keying::Data, None), "a data over");
    }

    #[test]
    fn dimmed_rows_keep_the_waterfalls_pace() {
        let p = 1.0 / 30.0;
        // The first at once.
        assert_eq!(held_rows_due(f64::NAN, 10.0, p), (1, 10.0));
        // Not yet one due.
        assert_eq!(held_rows_due(10.0, 10.02, p), (0, 10.0));
        // Two due: the newest counts from where the rows fall, not now.
        let (n, at) = held_rows_due(10.0, 10.07, p);
        assert_eq!(n, 2);
        assert!((at - (10.0 + 2.0 * p)).abs() < 1e-9);
        // A hitch: at most four; a long gap: one, from now.
        assert_eq!(held_rows_due(10.0, 10.5, p), (4, 10.5));
        assert_eq!(held_rows_due(10.0, 15.0, p), (1, 15.0));
    }

    #[test]
    fn a_held_row_is_found_by_its_time() {
        let rows: std::collections::VecDeque<TxRow> =
            [1.0, 2.0, 3.0].iter().map(|&utc| TxRow { utc }).collect();
        assert_eq!(row_at(&rows, 2.0).map(|r| r.utc), Some(2.0));
        assert!(row_at(&rows, 2.5).is_none());
        assert!(row_at(&rows, 4.0).is_none());
    }
}
