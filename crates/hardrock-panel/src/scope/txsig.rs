//! The scope over a transmission (decision 74, rewording 40): the waterfall
//! and the trace keep moving, but nothing the DUO's I/Q gives while keyed is
//! shown (it is silent or flooded by the transmitter). The band is shown at
//! its last received level, dimmed, and our own signal is drawn where it
//! lands in RF: the spectrum of the audio the panel is sending (a TX window
//! over, TX TUNE's tone), placed from the transmit dial by the sideband, in
//! the TX colour. A RIG TUNE carrier is drawn at the dial; what an outside
//! program sends is not known here, so only the dimmed band scrolls then.
//!
//! All of it is plain arithmetic, tested here: where the transmit dial is
//! (split or not), where an audio frequency lands (USB or LSB), the audio's
//! spectrum, and how many dimmed rows a repaint owes the waterfall.

use eframe::egui::Color32;

/// Our own transmission's colour: GLOBE's and the TX window's
/// (`globe::TX_COLOUR`, repeated here so the scope stands alone, as the
/// scope demo builds it).
pub const TX_INK: Color32 = Color32::from_rgb(0xe0, 0x6c, 0xf5);

/// The last stretch of audio the panel sent to the radio, at its output's
/// rate and level (`audio::Player::recent`, `audio::Tone::recent`).
#[derive(Debug, Clone, PartialEq)]
pub struct TxAudio {
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// Samples of our audio the scope looks at: 8192 at 48 kHz is 0.17 s and
/// 5.9 Hz a bin, about one FT8 tone (6.25 Hz apart, 0.16 s each).
pub const TX_WINDOW: usize = 8192;
/// The highest audio frequency drawn: an SSB transmitter passes nothing above.
const AUDIO_MAX_HZ: f64 = 4000.0;
/// How far below its peak the drawn spectrum goes, dB.
pub const FLOOR_DB: f32 = -70.0;
/// Bins within this of the peak are marked on the waterfall, dB.
const SEG_DB: f32 = -24.0;

/// What the radio is transmitting that the panel knows of, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct TxSignal {
    /// The transmit dial, the radio's own Hz: VFO B in split, else the
    /// receive VFO.
    pub dial_hz: f64,
    /// Lower sideband: the audio lands below the dial.
    pub lower: bool,
    pub what: TxWhat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TxWhat {
    /// The audio the panel is playing into the radio's USB audio.
    Audio(TxAudio),
    /// The radio's own TUNE carrier (RIG TUNE, `TX2;`), drawn at the dial.
    Carrier,
}

impl TxSignal {
    /// From the radio's status and what the panel is playing (`audio`):
    /// `None` when the panel sends nothing it knows of (an outside program
    /// keying, or no dial read yet).
    pub fn from_rig(r: &rigcat::RigStatus, audio: Option<TxAudio>) -> Option<TxSignal> {
        let dial_hz = tx_dial(r.freq_hz, r.freq_b_hz, r.vfo, r.split)?;
        let lower = r.mode == Some("LSB");
        let what = match audio {
            Some(a) => TxWhat::Audio(a),
            None if r.tuning.is_some_and(|t| t.keying == rigcat::Keying::Carrier) => {
                TxWhat::Carrier
            }
            None => return None,
        };
        Some(TxSignal { dial_hz, lower, what })
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

/// A stretch of our signal marked on the waterfall: radio Hz, and how strong
/// (0 to 1, 1 at the peak).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub lo: f64,
    pub hi: f64,
    pub strength: f32,
}

/// Our signal this repaint: its spectrum as (radio Hz, dB from its peak),
/// rising in frequency, and the parts strong enough to mark on the
/// waterfall.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TxShape {
    pub points: Vec<(f64, f32)>,
    pub segs: Vec<Seg>,
}

/// The shape of `sig`, or `None` when it is silent.
pub fn shape(sig: &TxSignal) -> Option<TxShape> {
    match &sig.what {
        TxWhat::Carrier => {
            let d = sig.dial_hz;
            Some(TxShape {
                points: vec![(d - 1.0, FLOOR_DB), (d, 0.0), (d + 1.0, FLOOR_DB)],
                segs: vec![Seg { lo: d - 0.5, hi: d + 0.5, strength: 1.0 }],
            })
        }
        TxWhat::Audio(a) => audio_shape(a, sig.dial_hz, sig.lower),
    }
}

fn audio_shape(a: &TxAudio, dial_hz: f64, lower: bool) -> Option<TxShape> {
    let n = pow2_floor(a.samples.len().min(TX_WINDOW));
    if n < 256 || a.rate == 0 {
        return None;
    }
    let x = &a.samples[a.samples.len() - n..];
    let pw = power_spectrum(x);
    let bin_hz = a.rate as f64 / n as f64;
    let top = ((AUDIO_MAX_HZ / bin_hz) as usize).min(pw.len() - 1);
    let peak = pw[1..=top].iter().copied().fold(0.0f32, f32::max);
    if !(peak > 1e-12) {
        return None;
    }
    let mut points: Vec<(f64, f32)> = (1..=top)
        .map(|k| {
            let rel = 10.0 * (pw[k] / peak).max(1e-12).log10();
            (place(dial_hz, lower, k as f64 * bin_hz), rel.max(FLOOR_DB))
        })
        .collect();
    if lower {
        points.reverse();
    }
    // The strong parts, merged where they touch.
    let mut segs: Vec<Seg> = Vec::new();
    let half = bin_hz / 2.0;
    for &(hz, db) in points.iter().filter(|p| p.1 >= SEG_DB) {
        let strength = 1.0 - db / SEG_DB;
        match segs.last_mut() {
            Some(s) if hz - half <= s.hi + 1e-6 => {
                s.hi = hz + half;
                s.strength = s.strength.max(strength);
            }
            _ => segs.push(Seg { lo: hz - half, hi: hz + half, strength }),
        }
    }
    Some(TxShape { points, segs })
}

fn pow2_floor(n: usize) -> usize {
    if n == 0 { 0 } else { 1 << (usize::BITS - 1 - n.leading_zeros()) }
}

/// The power in each bin, 0 to n/2, of `x` (a power of two long) under a
/// Hann window: a plain radix-2 FFT, enough for 8192 points a repaint.
fn power_spectrum(x: &[f32]) -> Vec<f32> {
    let n = x.len();
    let bits = n.trailing_zeros();
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    for (i, &v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
        let j = i.reverse_bits() >> (usize::BITS - bits);
        re[j] = v * w;
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
                let (wr, wi) = (c as f32, s as f32);
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
    (0..=n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
}

/// A waterfall row laid down while the I/Q was not shown: when (UTC s, the
/// row's own time), and our signal on it.
#[derive(Debug, Clone, PartialEq)]
pub struct TxRow {
    pub utc: f64,
    pub segs: Vec<Seg>,
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

    fn tone(hz: f32, rate: u32, n: usize) -> TxAudio {
        let samples =
            (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / rate as f32).cos()).collect();
        TxAudio { rate, samples }
    }

    fn peak_hz(s: &TxShape) -> f64 {
        s.points.iter().copied().fold((0.0, f32::MIN), |a, p| if p.1 > a.1 { p } else { a }).0
    }

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
    fn the_tune_tone_is_drawn_1_khz_from_the_transmit_dial() {
        let a = tone(1000.0, 48_000, TX_WINDOW);
        let bin = 48_000.0 / TX_WINDOW as f64;
        let usb = TxSignal { dial_hz: 14_200_000.0, lower: false, what: TxWhat::Audio(a.clone()) };
        let s = shape(&usb).unwrap();
        assert!((peak_hz(&s) - 14_201_000.0).abs() <= bin, "{}", peak_hz(&s));
        assert_eq!(s.segs.len(), 1);
        assert!(s.segs[0].lo < 14_201_000.0 && s.segs[0].hi > 14_201_000.0);
        assert!(s.segs[0].hi - s.segs[0].lo < 8.0 * bin, "a tone is narrow");
        assert!(s.points.windows(2).all(|w| w[0].0 < w[1].0), "rising in frequency");
        let lsb = TxSignal { dial_hz: 3_573_000.0, lower: true, what: TxWhat::Audio(a) };
        let s = shape(&lsb).unwrap();
        assert!((peak_hz(&s) - 3_572_000.0).abs() <= bin);
        assert!(s.points.windows(2).all(|w| w[0].0 < w[1].0), "rising in frequency");
        assert!(s.points.iter().all(|p| p.0 < 3_573_000.0));
    }

    #[test]
    fn an_ft8_offset_and_split_put_the_signal_on_vfo_b_plus_the_offset() {
        // AJ3G's case: receive on A at 14.074, transmit split on B 14.074
        // as TX follows RX, an over at 1234 Hz audio.
        let r = rigcat::RigStatus {
            freq_hz: Some(14_000_000),
            freq_b_hz: Some(14_074_000),
            vfo: Some(0),
            split: Some(true),
            mode: Some("USB"),
            ..Default::default()
        };
        let sig = TxSignal::from_rig(&r, Some(tone(1234.0, 12_000, 4096))).unwrap();
        assert_eq!((sig.dial_hz, sig.lower), (14_074_000.0, false));
        let s = shape(&sig).unwrap();
        assert!((peak_hz(&s) - 14_075_234.0).abs() <= 3.0, "{}", peak_hz(&s));
        // Nothing of ours known: nothing drawn.
        assert_eq!(TxSignal::from_rig(&r, None), None);
    }

    #[test]
    fn rig_tune_is_a_carrier_at_the_dial_and_silence_is_nothing() {
        let now = std::time::Instant::now();
        let r = rigcat::RigStatus {
            freq_hz: Some(7_100_000),
            mode: Some("LSB"),
            tuning: Some(rigcat::Tuning {
                since: now,
                until: now,
                keying: rigcat::Keying::Carrier,
            }),
            ..Default::default()
        };
        let sig = TxSignal::from_rig(&r, None).unwrap();
        assert_eq!(sig.what, TxWhat::Carrier);
        let s = shape(&sig).unwrap();
        assert_eq!(peak_hz(&s), 7_100_000.0);
        let quiet = TxAudio { rate: 48_000, samples: vec![0.0; TX_WINDOW] };
        let sig = TxSignal { dial_hz: 7e6, lower: false, what: TxWhat::Audio(quiet) };
        assert_eq!(shape(&sig), None);
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
            [1.0, 2.0, 3.0].iter().map(|&utc| TxRow { utc, segs: vec![] }).collect();
        assert_eq!(row_at(&rows, 2.0).map(|r| r.utc), Some(2.0));
        assert!(row_at(&rows, 2.5).is_none());
        assert!(row_at(&rows, 4.0).is_none());
    }
}
