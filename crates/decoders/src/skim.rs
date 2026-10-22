//! The wide skimmer: FT8 or FT4 decoded on every other segment of the I/Q
//! span at once, as SDRoxide's skimmers read every signal in their window
//! rather than only the one the receiver is on (decision 54).
//!
//! Each segment is a USB dial in the span. [`SsbLane`] lifts the dial's
//! 60–2940 Hz out of the wide I/Q as 12 kHz upper-sideband audio, which a
//! [`WsjtDecoder`] of its own then decodes in slots exactly as the panel's
//! receiver audio is decoded. One thread does the lanes for every segment,
//! behind a queue that drops (and counts) blocks rather than make the I/Q
//! tap wait.

use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Sender, TrySendError, bounded};
use sdr::Complex32;

use crate::resample::{IqChannel, Resampler, deinterleave};
use crate::worker::{WsjtConfig, WsjtDecoder, WsjtSlot};
use crate::wsjt::{self, WsjtMode};

/// The most segments one skimmer decodes: each is a WSJT decoder of its own
/// (two threads, and a slot's decode every period).
pub const MAX_SEGMENTS: usize = 8;

/// The lane's complex rate. Its filter keeps 90 % of its Nyquist, ±1440 Hz,
/// so a lane centred 1500 Hz above the dial holds 60–2940 Hz of audio and
/// nothing below the dial.
const LANE_RATE: f64 = 3200.0;
/// Where the lane is centred, Hz above the dial.
const LANE_CENTRE: f64 = 1500.0;
/// How far from the span's centre a lane may reach, as a share of the span:
/// past this the radio's own filters roll the band off.
const USABLE: f64 = 0.45;

/// One dial's upper sideband, out of the wide I/Q: complex I/Q in, 12 kHz
/// real audio out, as a receiver in USB would hear it from the dial.
pub struct SsbLane {
    rate: f64,
    chan: IqChannel,
    up: Resampler<Complex32>,
    /// Where in the 1500 Hz mix's eight-sample turn the next sample is.
    turn: usize,
    lane: Vec<Complex32>,
    wide: Vec<Complex32>,
}

impl SsbLane {
    /// A lane on the dial `dial_off_hz` from the I/Q centre, at I/Q rate
    /// `rate`.
    pub fn new(rate: f64, dial_off_hz: f64) -> Self {
        SsbLane {
            rate,
            chan: IqChannel::new(rate, LANE_RATE, dial_off_hz + LANE_CENTRE),
            up: Resampler::new(LANE_RATE, wsjt::RATE),
            turn: 0,
            lane: Vec::new(),
            wide: Vec::new(),
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The dial moved in the span (the span's centre moved).
    pub fn set_dial_offset(&mut self, dial_off_hz: f64) {
        self.chan.set_offset_hz(dial_off_hz + LANE_CENTRE);
    }

    /// Whether a dial `dial_off_hz` from the centre of a span `rate` wide
    /// has all of its lane inside the span's usable part.
    pub fn fits(rate: f64, dial_off_hz: f64) -> bool {
        let a = dial_off_hz;
        let b = dial_off_hz + 2.0 * LANE_CENTRE;
        a.abs().max(b.abs()) <= USABLE * rate
    }

    /// Complex I/Q at the lane's rate in, 12 kHz USB audio appended to
    /// `out`.
    pub fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>) {
        self.lane.clear();
        self.chan.process(iq, &mut self.lane);
        self.wide.clear();
        self.up.process(&self.lane, &mut self.wide);
        // Back up by the lane's centre: 1500 Hz at 12 kHz is an eighth of a
        // turn a sample. Every tone in the lane lands on positive audio, so
        // the real part is the sideband with no image folded into it.
        let turn: [Complex32; 8] = std::array::from_fn(|k| {
            let p = 2.0 * PI * LANE_CENTRE * k as f64 / wsjt::RATE;
            Complex32::new(p.cos() as f32, p.sin() as f32)
        });
        out.extend(self.wide.iter().map(|z| {
            let r = (z * turn[self.turn]).re;
            self.turn = (self.turn + 1) % 8;
            r
        }));
    }
}

/// A block of the radio's I/Q.
struct Block {
    iq: Vec<f32>,
    rate: f64,
    centre: f64,
    utc: f64,
}

/// What one segment heard in a slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SkimSlot {
    /// The segment's USB dial, in the same frame as the I/Q's centre, Hz.
    pub dial_hz: f64,
    pub slot: WsjtSlot,
}

/// One WSJT mode on several dials of the I/Q at once.
pub struct WideSkimmer {
    mode: WsjtMode,
    dials: Vec<f64>,
    tx: Option<Sender<Block>>,
    decoders: Vec<Arc<WsjtDecoder>>,
    dropped: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

/// I/Q blocks the lane thread holds before it drops them.
const QUEUE: usize = 256;

impl WideSkimmer {
    /// Decode `mode` on each of `dials` (USB dials, in the I/Q centre's
    /// frame, Hz; at most [`MAX_SEGMENTS`] are taken), resolving hashed
    /// calls naming `my_calls`.
    pub fn spawn(mode: WsjtMode, dials: &[f64], my_calls: &[String]) -> Self {
        let dials: Vec<f64> = dials.iter().copied().take(MAX_SEGMENTS).collect();
        let decoders: Vec<Arc<WsjtDecoder>> = dials
            .iter()
            .map(|_| {
                let mut cfg = WsjtConfig::new(mode);
                cfg.input_rate = wsjt::RATE;
                cfg.my_calls.extend(my_calls.iter().cloned());
                Arc::new(WsjtDecoder::spawn(cfg))
            })
            .collect();
        let (tx, rx) = bounded::<Block>(QUEUE);
        let feeds = decoders.clone();
        let lane_dials = dials.clone();
        let thread = std::thread::Builder::new()
            .name(format!("skim-{}", mode.label()))
            .spawn(move || {
                let mut lanes: Vec<Option<SsbLane>> = lane_dials.iter().map(|_| None).collect();
                let (mut iq, mut audio) = (Vec::new(), Vec::new());
                for b in rx.iter() {
                    iq.clear();
                    deinterleave(&b.iq, &mut iq);
                    for ((dial, lane), dec) in lane_dials.iter().zip(&mut lanes).zip(&feeds) {
                        let off = dial - b.centre;
                        if !SsbLane::fits(b.rate, off) {
                            // Out of the span: the decoder hears a gap, and
                            // starts its slots again when the dial is back.
                            *lane = None;
                            continue;
                        }
                        let l = match lane {
                            Some(l) if l.rate() == b.rate => {
                                l.set_dial_offset(off);
                                l
                            }
                            _ => lane.insert(SsbLane::new(b.rate, off)),
                        };
                        audio.clear();
                        l.process(&iq, &mut audio);
                        if !audio.is_empty() {
                            dec.push(&audio, b.utc);
                        }
                    }
                }
            })
            .expect("spawn skimmer lane thread");
        WideSkimmer {
            mode,
            dials,
            tx: Some(tx),
            decoders,
            dropped: Arc::new(AtomicU64::new(0)),
            thread: Some(thread),
        }
    }

    pub fn mode(&self) -> WsjtMode {
        self.mode
    }

    /// The dials it decodes.
    pub fn dials(&self) -> &[f64] {
        &self.dials
    }

    /// Interleaved I/Q at `rate` around `centre` (Hz), its first sample
    /// heard at `utc`. Never waits: a block the lane thread has no room for
    /// is dropped and counted.
    pub fn push_iq(&self, iq: &[f32], rate: f64, centre: f64, utc: f64) {
        let Some(tx) = &self.tx else { return };
        let b = Block { iq: iq.to_vec(), rate, centre, utc };
        if let Err(TrySendError::Full(_)) = tx.try_send(b) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Every slot finished since last asked, on every dial.
    pub fn results(&self) -> Vec<SkimSlot> {
        let mut out = Vec::new();
        for (dial, d) in self.dials.iter().zip(&self.decoders) {
            out.extend(d.results().try_iter().map(|slot| SkimSlot { dial_hz: *dial, slot }));
        }
        out
    }

    /// Blocks dropped, by the lane thread and the decoders.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
            + self.decoders.iter().map(|d| d.dropped()).sum::<u64>()
    }
}

impl Drop for WideSkimmer {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The dials of `all` (USB dials, Hz) that a skimmer can take in a span
/// `rate` wide around `centre`, leaving out any within 3 kHz of `skip` (the
/// receiver's own, which DECODE already decodes), nearest the centre first,
/// at most [`MAX_SEGMENTS`].
pub fn dials_in_span(all: &[f64], centre: f64, rate: f64, skip: Option<f64>) -> Vec<f64> {
    let mut v: Vec<f64> = all
        .iter()
        .copied()
        .filter(|&d| SsbLane::fits(rate, d - centre))
        .filter(|&d| skip.is_none_or(|s| (d - s).abs() >= 3000.0))
        .collect();
    v.sort_by(|a, b| (a - centre).abs().total_cmp(&(b - centre).abs()));
    v.truncate(MAX_SEGMENTS);
    v.sort_by(f64::total_cmp);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complex tone `hz` from the centre, `n` samples at `rate`.
    fn tone(rate: f64, hz: f64, n: usize) -> Vec<Complex32> {
        (0..n)
            .map(|i| {
                let p = 2.0 * PI * hz * i as f64 / rate;
                Complex32::new(p.cos() as f32, p.sin() as f32)
            })
            .collect()
    }

    /// Power at `hz` relative to a unit sine, dB.
    fn level_db(x: &[f32], rate: f64, hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let p = 2.0 * PI * hz * i as f64 / rate;
            re += v as f64 * p.cos();
            im += v as f64 * p.sin();
        }
        let amp = 2.0 * (re * re + im * im).sqrt() / x.len() as f64;
        20.0 * amp.max(1e-12).log10()
    }

    #[test]
    fn a_lane_hears_its_dials_upper_sideband_and_nothing_below_it() {
        let rate = 192_000.0;
        let dial_off = 20_000.0;
        // 1234 Hz above the dial: 1234 Hz of audio.
        let mut lane = SsbLane::new(rate, dial_off);
        let mut out = Vec::new();
        lane.process(&tone(rate, dial_off + 1234.0, 192_000), &mut out);
        assert!((out.len() as i64 - 12_000).abs() < 50, "{}", out.len());
        let tail = &out[2000..];
        let on = level_db(tail, 12_000.0, 1234.0);
        assert!(on > -9.0, "the tone at {on:.1} dB");
        // 1234 Hz below the dial is the other sideband: not heard at all,
        // neither where it is nor folded onto 1234 Hz.
        let mut lane = SsbLane::new(rate, dial_off);
        let mut out = Vec::new();
        lane.process(&tone(rate, dial_off - 1234.0, 192_000), &mut out);
        let off = level_db(&out[2000..], 12_000.0, 1234.0);
        assert!(off < on - 50.0, "the lower sideband at {off:.1} dB against {on:.1}");
        // In blocks, as the tap gives them, the same.
        let mut lane = SsbLane::new(rate, dial_off);
        let mut out = Vec::new();
        for c in tone(rate, dial_off + 1234.0, 192_000).chunks(1000) {
            lane.process(c, &mut out);
        }
        assert!((level_db(&out[2000..], 12_000.0, 1234.0) - on).abs() < 0.5);
    }

    #[test]
    fn only_dials_whose_lane_is_inside_the_span_are_skimmed() {
        let ft8 = [7_074_000.0, 10_136_000.0, 14_074_000.0, 18_100_000.0, 21_074_000.0];
        // 6.144 MHz around 16 MHz: 13.235 to 18.765 usable.
        let d = dials_in_span(&ft8, 16_000_000.0, 6_144_000.0, Some(14_074_000.0));
        assert_eq!(d, vec![18_100_000.0]);
        let d = dials_in_span(&ft8, 16_000_000.0, 6_144_000.0, None);
        assert_eq!(d, vec![14_074_000.0, 18_100_000.0]);
        // A dial right at the edge: its lane runs off it.
        assert!(SsbLane::fits(192_000.0, 80_000.0));
        assert!(!SsbLane::fits(192_000.0, 85_000.0));
        assert!(!SsbLane::fits(192_000.0, -86_500.0));
    }

    #[test]
    fn ft8_on_another_dial_of_the_span_is_decoded() {
        // A slot of FT8 1500 Hz above a dial 10 kHz above the centre of a
        // 48 kHz span (the lane does not care about the rate, and a narrow
        // span keeps the test quick), in blocks with their UTC, as the tap
        // gives them.
        let rate = 48_000.0;
        let centre = 7_000_000.0;
        let dial = centre + 10_000.0;
        let sk = WideSkimmer::spawn(WsjtMode::Ft8, &[dial], &[]);
        assert_eq!(sk.dials(), &[dial]);
        let bits = mfsk_core::msg::wsjt77::pack77("CQ", "K1ABC", "FN42").unwrap();
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits);
        let slot_utc = 1_700_000_010.0;
        let start = slot_utc - 1.0;
        let n = (rate * 17.0) as usize;
        let sps = (rate * 0.16) as usize;
        let k0 = (rate * 1.5) as usize; // 0.5 s into the slot
        let mut phase = 0.0f64;
        let mut iq = Vec::with_capacity(n * 2);
        let mut rng = 1u32;
        for i in 0..n {
            let s = i.checked_sub(k0).map(|k| k / sps).filter(|&s| s < tones.len());
            let (mut re, mut im) = (0.0f32, 0.0f32);
            if let Some(s) = s {
                let f = dial + 1500.0 + tones[s] as f64 * 6.25 - centre;
                phase += 2.0 * PI * f / rate;
                re = 0.05 * phase.cos() as f32;
                im = 0.05 * phase.sin() as f32;
            }
            // A little noise, so the decoder's AGC has a floor.
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let nz = (rng as f32 / u32::MAX as f32 - 0.5) * 0.01;
            iq.push(re + nz);
            iq.push(im - nz);
        }
        for (i, c) in iq.chunks(4800 * 2).enumerate() {
            sk.push_iq(c, rate, centre, start + (i * 4800) as f64 / rate);
        }
        let t = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t.elapsed().as_secs() < 90 {
            got.extend(sk.results().into_iter().filter(|s| !s.slot.decodes.is_empty()));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let s = got.first().expect("a slot decoded");
        assert_eq!(s.dial_hz, dial);
        let d = &s.slot.decodes[0];
        assert_eq!(d.message, "CQ K1ABC FN42");
        assert!((d.audio_hz - 1500.0).abs() < 10.0, "{}", d.audio_hz);
        assert_eq!(sk.dropped(), 0);
    }
}
