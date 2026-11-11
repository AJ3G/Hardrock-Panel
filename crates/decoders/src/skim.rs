//! The wide skimmer: FT8, FT4 and WSPR decoded on every other calling
//! frequency of the I/Q span at once, as SDRoxide's skimmers read every
//! signal in their window rather than only the one the receiver is on
//! (decision 54).
//!
//! Each dial is a USB dial in the span. [`SsbLane`] lifts the dial's
//! 60–2940 Hz out of the wide I/Q as 12 kHz upper-sideband audio, which a
//! [`WsjtDecoder`] of its own for each mode on that dial then decodes in
//! slots exactly as the panel's receiver audio is decoded.
//!
//! One [`WideSkimmer`] does every mode and dial (decisions 110 and 111):
//!
//! * One thread, below normal priority, does the lanes for every dial,
//!   behind a queue that drops (and counts) blocks rather than make the I/Q
//!   tap wait. The I/Q is transformed once for all of them
//!   ([`crate::channelizer`]); each lane takes only its own few bins.
//! * Its decoders are background ones ([`crate::background`]): low
//!   priority, and a few slots decoded at a time.
//! * The dials are changed in place ([`WideSkimmer::set_dials`]): a dial
//!   still in the span keeps its decoder and its lane, so scrolling the span
//!   loses no slot in progress (AJ3G, 11 Nov 2026: "When at a SPAN of 6144
//!   and I scroll up and down the band, it erases spots"). A dial that left
//!   is kept a slot longer, so the slot it finished still comes out, and
//!   taken up again if it comes back by then.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError, bounded};
use sdr::Complex32;

use crate::channelizer::{Channelizer, Frame, Tap};
use crate::resample::{IqChannel, Resampler, deinterleave};
use crate::worker::{WsjtConfig, WsjtDecoder, WsjtSlot};
use crate::wsjt::{self, WsjtMode};

/// The most dials of one mode a skimmer decodes: each is a WSJT decoder of
/// its own (two threads, and a slot's decode every period).
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

/// One dial's upper sideband, out of the wide I/Q: the dial's bins of each
/// [`Frame`] in, 12 kHz real audio out, as a receiver in USB would hear it
/// from the dial.
pub struct SsbLane {
    rate: f64,
    off: f64,
    tap: Tap,
    chan: IqChannel,
    up: Resampler<Complex32>,
    /// Where in the 1500 Hz mix's eight-sample turn the next sample is.
    turn: usize,
    cut: Vec<Complex32>,
    lane: Vec<Complex32>,
    wide: Vec<Complex32>,
}

impl SsbLane {
    /// A lane on the dial `dial_off_hz` from the I/Q centre, out of `ch`.
    pub fn new(ch: &Channelizer, dial_off_hz: f64) -> Self {
        let (bin, rest) = ch.bin_of(dial_off_hz + LANE_CENTRE);
        SsbLane {
            rate: ch.rate(),
            off: dial_off_hz,
            tap: Tap::new(bin),
            chan: IqChannel::new(ch.out_rate(), LANE_RATE, rest),
            up: Resampler::new(LANE_RATE, wsjt::RATE),
            turn: 0,
            cut: Vec::new(),
            lane: Vec::new(),
            wide: Vec::new(),
        }
    }

    /// The I/Q rate it was made for.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The dial moved in the span (the span's centre moved). The lane keeps
    /// running: its filters and the decoder behind it never notice.
    pub fn set_dial_offset(&mut self, ch: &Channelizer, dial_off_hz: f64) {
        if dial_off_hz == self.off {
            return;
        }
        self.off = dial_off_hz;
        let (bin, rest) = ch.bin_of(dial_off_hz + LANE_CENTRE);
        self.tap.set_bin(bin);
        self.chan.set_offset_hz(rest);
    }

    /// Whether a dial `dial_off_hz` from the centre of a span `rate` wide
    /// has all of its lane inside the span's usable part.
    pub fn fits(rate: f64, dial_off_hz: f64) -> bool {
        let a = dial_off_hz;
        let b = dial_off_hz + 2.0 * LANE_CENTRE;
        a.abs().max(b.abs()) <= USABLE * rate
    }

    /// This lane's part of a frame, as 12 kHz USB audio appended to `out`.
    pub fn take(&mut self, f: &Frame, out: &mut Vec<f32>) {
        self.cut.clear();
        self.tap.take(f, &mut self.cut);
        self.lane.clear();
        self.chan.process(&self.cut, &mut self.lane);
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

/// One lane with its own [`Channelizer`]: for a single dial heard alone (the
/// demo band's tests), where there is nothing to share the transform with.
pub struct SoloLane {
    ch: Channelizer,
    lane: SsbLane,
}

impl SoloLane {
    /// A lane on the dial `dial_off_hz` from the I/Q centre, at I/Q rate
    /// `rate`.
    pub fn new(rate: f64, dial_off_hz: f64) -> Self {
        let ch = Channelizer::new(rate);
        let lane = SsbLane::new(&ch, dial_off_hz);
        SoloLane { ch, lane }
    }

    /// Complex I/Q at the lane's rate in, 12 kHz USB audio appended to
    /// `out`.
    pub fn process(&mut self, iq: &[Complex32], out: &mut Vec<f32>) {
        let lane = &mut self.lane;
        self.ch.process(iq, 0.0, |f| lane.take(f, out));
    }
}

/// A block of the radio's I/Q.
struct Block {
    iq: Vec<f32>,
    rate: f64,
    centre: f64,
    utc: f64,
}

/// What one dial heard in a slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SkimSlot {
    pub mode: WsjtMode,
    /// The USB dial, in the same frame as the I/Q's centre, Hz.
    pub dial_hz: f64,
    pub slot: WsjtSlot,
}

/// A mode on a dial, and the decoder that hears it.
#[derive(Clone)]
struct Listener {
    mode: WsjtMode,
    dial: f64,
    dec: Arc<WsjtDecoder>,
}

impl Listener {
    fn is(&self, mode: WsjtMode, dial: f64) -> bool {
        self.mode == mode && self.dial == dial
    }
}

/// What [`plan`] makes of a change of dials.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Wanted and not running nor retiring: a decoder to start.
    start: Vec<(WsjtMode, f64)>,
    /// Retiring and wanted again: taken back as they are.
    revive: Vec<(WsjtMode, f64)>,
    /// Running and no longer wanted: retired.
    retire: Vec<(WsjtMode, f64)>,
}

/// From the modes and dials `running` and `retiring` to `want`: anything in
/// both is left alone.
fn plan(
    running: &[(WsjtMode, f64)],
    retiring: &[(WsjtMode, f64)],
    want: &[(WsjtMode, Vec<f64>)],
) -> Plan {
    let want: Vec<(WsjtMode, f64)> =
        want.iter().flat_map(|(m, ds)| ds.iter().take(MAX_SEGMENTS).map(|&d| (*m, d))).collect();
    let mut p = Plan::default();
    for &w in &want {
        if running.contains(&w) {
            continue;
        }
        if retiring.contains(&w) {
            p.revive.push(w);
        } else {
            p.start.push(w);
        }
    }
    p.retire = running.iter().copied().filter(|r| !want.contains(r)).collect();
    p
}

/// How long a dial that left the span is kept after it left: its last slot's
/// decode, then a margin, s.
fn keep_s(mode: WsjtMode) -> f64 {
    mode.period_s() + 10.0
}

/// The lane thread's view of the listeners: changed with a new generation.
type Shared = Mutex<(u64, Vec<Listener>)>;

/// Every WSJT mode on every dial asked for, out of the I/Q at once.
pub struct WideSkimmer {
    my_calls: Vec<String>,
    running: Vec<Listener>,
    retiring: Vec<(Listener, Instant)>,
    shared: Arc<Shared>,
    tx: Option<Sender<Block>>,
    /// Blocks the lane thread had no room for.
    lane_dropped: Arc<AtomicU64>,
    /// Blocks dropped by decoders since retired.
    retired_dropped: u64,
    thread: Option<JoinHandle<()>>,
}

/// I/Q blocks the lane thread holds before it drops them.
const QUEUE: usize = 256;

impl WideSkimmer {
    /// A skimmer with no dials yet, resolving hashed calls naming
    /// `my_calls`.
    pub fn new(my_calls: &[String]) -> Self {
        let (tx, rx) = bounded::<Block>(QUEUE);
        let shared: Arc<Shared> = Arc::new(Mutex::new((0, Vec::new())));
        let seen = shared.clone();
        let thread = std::thread::Builder::new()
            .name("skim-lanes".into())
            .spawn(move || {
                crate::background::lower_current_thread();
                lanes(rx.iter(), &seen);
            })
            .expect("spawn skimmer lane thread");
        WideSkimmer {
            my_calls: my_calls.to_vec(),
            running: Vec::new(),
            retiring: Vec::new(),
            shared,
            tx: Some(tx),
            lane_dropped: Arc::new(AtomicU64::new(0)),
            retired_dropped: 0,
            thread: Some(thread),
        }
    }

    /// One mode on `dials` (USB dials, in the I/Q centre's frame, Hz; at most
    /// [`MAX_SEGMENTS`] are taken).
    pub fn spawn(mode: WsjtMode, dials: &[f64], my_calls: &[String]) -> Self {
        let mut s = Self::new(my_calls);
        s.set_dials(&[(mode, dials.to_vec())]);
        s
    }

    /// Decode each mode on its dials from now on (USB dials, in the I/Q
    /// centre's frame, Hz; at most [`MAX_SEGMENTS`] a mode). A mode and dial
    /// already decoded carries on undisturbed; one no longer asked for is
    /// retired after its last slot; one asked for again before that is
    /// taken back. Returns whether anything changed.
    pub fn set_dials(&mut self, want: &[(WsjtMode, Vec<f64>)]) -> bool {
        self.expire(Instant::now());
        let running: Vec<_> = self.running.iter().map(|l| (l.mode, l.dial)).collect();
        let retiring: Vec<_> = self.retiring.iter().map(|(l, _)| (l.mode, l.dial)).collect();
        let p = plan(&running, &retiring, want);
        if p == Plan::default() {
            return false;
        }
        for (mode, dial) in &p.retire {
            if let Some(i) = self.running.iter().position(|l| l.is(*mode, *dial)) {
                let l = self.running.remove(i);
                tracing::info!("{} skimmer on {:.3} MHz left the span", mode.label(), dial / 1e6);
                let until = Instant::now() + Duration::from_secs_f64(keep_s(*mode));
                self.retiring.push((l, until));
            }
        }
        for (mode, dial) in &p.revive {
            if let Some(i) = self.retiring.iter().position(|(l, _)| l.is(*mode, *dial)) {
                tracing::info!(
                    "{} skimmer on {:.3} MHz back in the span",
                    mode.label(),
                    dial / 1e6
                );
                let (l, _) = self.retiring.remove(i);
                self.running.push(l);
            }
        }
        for &(mode, dial) in &p.start {
            tracing::info!("{} skimmer started on {:.3} MHz", mode.label(), dial / 1e6);
            let mut cfg = WsjtConfig::new(mode);
            cfg.input_rate = wsjt::RATE;
            cfg.background = true;
            cfg.my_calls.extend(self.my_calls.iter().cloned());
            self.running.push(Listener { mode, dial, dec: Arc::new(WsjtDecoder::spawn(cfg)) });
        }
        let mut sh = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        sh.0 += 1;
        sh.1 = self.running.clone();
        true
    }

    /// Retire the decoders kept past their time, on a thread of their own:
    /// one may be in a decode, and stopping it waits for that.
    fn expire(&mut self, now: Instant) {
        let (gone, keep): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.retiring).into_iter().partition(|(_, t)| now >= *t);
        self.retiring = keep;
        for (l, _) in gone {
            self.retired_dropped += l.dec.dropped();
            let mut dec = l.dec;
            let _ = std::thread::Builder::new().name("skim-retire".into()).spawn(move || {
                // The lane thread lets go of it at its next block.
                for _ in 0..200 {
                    match Arc::try_unwrap(dec) {
                        Ok(d) => return drop(d),
                        Err(d) => dec = d,
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
    }

    /// Whether it decodes nothing, and has nothing still to finish.
    pub fn is_idle(&self) -> bool {
        self.running.is_empty() && self.retiring.is_empty()
    }

    /// The modes it decodes and their dials, in the order first asked for.
    pub fn dials(&self) -> Vec<(WsjtMode, Vec<f64>)> {
        let mut out: Vec<(WsjtMode, Vec<f64>)> = Vec::new();
        for l in &self.running {
            match out.iter_mut().find(|(m, _)| *m == l.mode) {
                Some((_, ds)) => ds.push(l.dial),
                None => out.push((l.mode, vec![l.dial])),
            }
        }
        for (_, ds) in &mut out {
            ds.sort_by(f64::total_cmp);
        }
        out
    }

    /// Interleaved I/Q at `rate` around `centre` (Hz), its first sample
    /// heard at `utc`. Never waits: a block the lane thread has no room for
    /// is dropped and counted.
    pub fn push_iq(&self, iq: &[f32], rate: f64, centre: f64, utc: f64) {
        push(self.tx.as_ref(), &self.lane_dropped, iq, rate, centre, utc);
    }

    /// A handle for the I/Q tap: [`Self::push_iq`] from another thread
    /// while the skimmer itself is changed here.
    pub fn input(&self) -> SkimInput {
        SkimInput { tx: self.tx.clone(), dropped: self.lane_dropped.clone() }
    }

    /// Every slot finished since last asked, on every dial (those retiring
    /// too).
    pub fn results(&mut self) -> Vec<SkimSlot> {
        self.expire(Instant::now());
        let mut out = Vec::new();
        for l in self.running.iter().chain(self.retiring.iter().map(|(l, _)| l)) {
            out.extend(l.dec.results().try_iter().map(|slot| SkimSlot {
                mode: l.mode,
                dial_hz: l.dial,
                slot,
            }));
        }
        out
    }

    /// Blocks dropped, by the lane thread and the decoders.
    pub fn dropped(&self) -> u64 {
        self.lane_dropped()
            + self.retired_dropped
            + self
                .running
                .iter()
                .chain(self.retiring.iter().map(|(l, _)| l))
                .map(|l| l.dec.dropped())
                .sum::<u64>()
    }

    /// Blocks of I/Q the lane thread had no room for: it is behind.
    pub fn lane_dropped(&self) -> u64 {
        self.lane_dropped.load(Ordering::Relaxed)
    }

    /// Blocks waiting for the lane thread.
    pub fn queued(&self) -> usize {
        self.tx.as_ref().map_or(0, |t| t.len())
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

/// The I/Q tap's side of a [`WideSkimmer`].
#[derive(Clone)]
pub struct SkimInput {
    tx: Option<Sender<Block>>,
    dropped: Arc<AtomicU64>,
}

impl SkimInput {
    /// As [`WideSkimmer::push_iq`].
    pub fn push_iq(&self, iq: &[f32], rate: f64, centre: f64, utc: f64) {
        push(self.tx.as_ref(), &self.dropped, iq, rate, centre, utc);
    }
}

fn push(
    tx: Option<&Sender<Block>>,
    dropped: &AtomicU64,
    iq: &[f32],
    rate: f64,
    centre: f64,
    utc: f64,
) {
    let Some(tx) = tx else { return };
    let b = Block { iq: iq.to_vec(), rate, centre, utc };
    if let Err(TrySendError::Full(_)) = tx.try_send(b) {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The lane thread: each block of I/Q through the shared transform, each
/// dial's lane out of it, and its audio to every decoder on that dial.
fn lanes(blocks: impl Iterator<Item = Block>, shared: &Shared) {
    let mut ch: Option<Channelizer> = None;
    // One lane a dial, whatever modes listen on it.
    let mut lanes: Vec<(f64, SsbLane, Vec<f32>)> = Vec::new();
    let (mut seen_gen, mut listeners) = (u64::MAX, Vec::<Listener>::new());
    let mut iq = Vec::new();
    for b in blocks {
        {
            let sh = shared.lock().unwrap_or_else(|e| e.into_inner());
            if sh.0 != seen_gen {
                seen_gen = sh.0;
                listeners = sh.1.clone();
            }
        }
        if listeners.is_empty() {
            ch = None;
            lanes.clear();
            continue;
        }
        let ch = match &mut ch {
            Some(c) if c.rate() == b.rate => c,
            _ => {
                lanes.clear();
                ch.insert(Channelizer::new(b.rate))
            }
        };
        // Lanes for the dials listened to and in the span; a dial out of it
        // has its decoders hear a gap, and start their slots again when it
        // is back.
        lanes.retain(|(d, _, _)| {
            listeners.iter().any(|l| l.dial == *d) && SsbLane::fits(b.rate, d - b.centre)
        });
        for l in &listeners {
            let off = l.dial - b.centre;
            match lanes.iter_mut().find(|(d, _, _)| *d == l.dial) {
                Some((_, lane, _)) => lane.set_dial_offset(ch, off),
                None if SsbLane::fits(b.rate, off) => {
                    lanes.push((l.dial, SsbLane::new(ch, off), Vec::new()))
                }
                None => {}
            }
        }
        if lanes.is_empty() {
            continue;
        }
        iq.clear();
        deinterleave(&b.iq, &mut iq);
        let mut first = None;
        for (_, _, a) in &mut lanes {
            a.clear();
        }
        ch.process(&iq, b.utc, |f| {
            first.get_or_insert(f.utc);
            for (_, lane, a) in &mut lanes {
                lane.take(f, a);
            }
        });
        let Some(utc) = first else { continue };
        for (dial, _, a) in &lanes {
            if a.is_empty() {
                continue;
            }
            for l in listeners.iter().filter(|l| l.dial == *dial) {
                l.dec.push(a, utc);
            }
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
        let mut lane = SoloLane::new(rate, dial_off);
        let mut out = Vec::new();
        lane.process(&tone(rate, dial_off + 1234.0, 192_000), &mut out);
        assert!((out.len() as i64 - 12_000).abs() < 100, "{}", out.len());
        let tail = &out[2000..];
        let on = level_db(tail, 12_000.0, 1234.0);
        assert!(on > -9.0, "the tone at {on:.1} dB");
        // 1234 Hz below the dial is the other sideband: not heard at all,
        // neither where it is nor folded onto 1234 Hz.
        let mut lane = SoloLane::new(rate, dial_off);
        let mut out = Vec::new();
        lane.process(&tone(rate, dial_off - 1234.0, 192_000), &mut out);
        let off = level_db(&out[2000..], 12_000.0, 1234.0);
        assert!(off < on - 50.0, "the lower sideband at {off:.1} dB against {on:.1}");
        // In blocks, as the tap gives them, the same.
        let mut lane = SoloLane::new(rate, dial_off);
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

    /// A slot of FT8 ("CQ K1ABC FN42") 1500 Hz above `dial`, as I/Q at
    /// `rate` around `centre(t)` (seconds from the stream's start) with a
    /// little noise, in blocks of `block_s` with their UTC, from a second
    /// before the slot to two after. Each block waits for the lane thread,
    /// so a fast test machine drops nothing. Returns the slot's UTC.
    fn send_ft8(
        sk: &WideSkimmer,
        rate: f64,
        dial: f64,
        centre: impl Fn(f64) -> f64,
        block_s: f64,
    ) -> i64 {
        let bits = mfsk_core::msg::wsjt77::pack77("CQ", "K1ABC", "FN42").unwrap();
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits);
        let slot_utc = 1_700_000_010.0;
        let start = slot_utc - 1.0;
        let n = (rate * 17.0) as usize;
        let sps = (rate * 0.16) as usize;
        let k0 = (rate * 1.5) as usize; // 0.5 s into the slot
        let block = (rate * block_s) as usize;
        let mut z = Complex32::new(0.05, 0.0);
        let mut rng = 1u32;
        let mut iq = Vec::with_capacity(block * 2);
        let mut i = 0;
        while i < n {
            let c = centre(i as f64 / rate);
            let end = (i + block).min(n);
            iq.clear();
            while i < end {
                // A symbol at a time, by a turn of the phasor each sample.
                let s = i.checked_sub(k0).map(|k| k / sps).filter(|&s| s < tones.len());
                let run = match s {
                    Some(s) => (k0 + (s + 1) * sps).min(end) - i,
                    None if i < k0 => (k0.min(end)) - i,
                    None => end - i,
                };
                let f = s.map(|s| dial + 1500.0 + tones[s] as f64 * 6.25 - c);
                let rot = f.map(|f| {
                    let p = 2.0 * PI * f / rate;
                    Complex32::new(p.cos() as f32, p.sin() as f32)
                });
                for _ in 0..run {
                    let (mut re, mut im) = (0.0, 0.0);
                    if let Some(r) = rot {
                        z *= r;
                        re = z.re;
                        im = z.im;
                    }
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    let nz = (rng as f32 / u32::MAX as f32 - 0.5) * 0.01;
                    iq.push(re + nz);
                    iq.push(im - nz);
                }
                z /= z.norm() / 0.05;
                i += run;
            }
            let t = (i - iq.len() / 2) as f64 / rate;
            sk.push_iq(&iq, rate, c, start + t);
            while sk.queued() > 1 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        slot_utc as i64
    }

    /// The first slot with decodes, waiting up to 90 s.
    fn first_decoded(sk: &mut WideSkimmer) -> SkimSlot {
        let t = Instant::now();
        while t.elapsed().as_secs() < 90 {
            if let Some(s) = sk.results().into_iter().find(|s| !s.slot.decodes.is_empty()) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("no slot decoded");
    }

    #[test]
    fn ft8_on_another_dial_of_the_span_is_decoded() {
        // 10 kHz above the centre of a 48 kHz span, and 1.234 MHz below the
        // centre of a 6144 kHz one (the panel's widest).
        for (rate, off) in [(48_000.0, 10_000.0), (6_144_000.0, -1_234_000.0)] {
            let centre = 7_000_000.0;
            let dial = centre + off;
            let mut sk = WideSkimmer::spawn(WsjtMode::Ft8, &[dial], &[]);
            assert_eq!(sk.dials(), vec![(WsjtMode::Ft8, vec![dial])]);
            let slot = send_ft8(&sk, rate, dial, |_| centre, 0.1);
            let s = first_decoded(&mut sk);
            assert_eq!((s.mode, s.dial_hz, s.slot.slot_utc), (WsjtMode::Ft8, dial, slot));
            let d = &s.slot.decodes[0];
            assert_eq!(d.message, "CQ K1ABC FN42", "{rate}");
            assert!((d.audio_hz - 1500.0).abs() < 10.0, "{rate}: {}", d.audio_hz);
            assert_eq!(sk.dropped(), 0, "{rate}");
        }
    }

    #[test]
    fn a_slot_heard_while_the_span_scrolls_is_still_decoded() {
        // The centre moves twice in the slot, the dial staying in the span:
        // its lane and decoder carry on, and the slot decodes.
        let rate = 48_000.0;
        let dial = 7_074_000.0;
        let mut sk = WideSkimmer::spawn(WsjtMode::Ft8, &[dial], &[]);
        let centre = |t: f64| match t {
            t if t < 5.0 => dial - 10_000.0,
            t if t < 9.0 => dial - 4_000.0,
            _ => dial + 6_000.0,
        };
        let slot = send_ft8(&sk, rate, dial, centre, 0.1);
        let s = first_decoded(&mut sk);
        assert_eq!(s.slot.slot_utc, slot);
        assert_eq!(s.slot.decodes[0].message, "CQ K1ABC FN42");
    }

    #[test]
    #[ignore = "a measurement"]
    fn lanes_cost_at_6144k() {
        // A second of 6144 kHz I/Q through the shared transform and twenty
        // lanes, against the old way: each lane mixing and filtering every
        // sample itself.
        let rate = 6_144_000.0;
        let iq = tone(rate, 1_000_000.0, rate as usize);
        let mut ch = Channelizer::new(rate);
        let mut lanes: Vec<SsbLane> =
            (0..20).map(|i| SsbLane::new(&ch, -2_000_000.0 + i as f64 * 200_000.0)).collect();
        let mut out = Vec::new();
        let t = Instant::now();
        for c in iq.chunks(30_000) {
            ch.process(c, 0.0, |f| lanes.iter_mut().for_each(|l| l.take(f, &mut out)));
        }
        let shared = t.elapsed();
        let mut old = IqChannel::new(rate, LANE_RATE, 1_000_000.0);
        let mut v = Vec::new();
        let t = Instant::now();
        for c in iq.chunks(30_000) {
            old.process(c, &mut v);
        }
        eprintln!("1 s of I/Q: 20 lanes shared {shared:?}; one lane the old way {:?}", t.elapsed());
    }

    #[test]
    fn a_change_of_dials_starts_only_the_new_and_retires_only_the_gone() {
        use WsjtMode::{Ft4, Ft8, Wspr};
        let p = plan(
            &[(Ft8, 1.0), (Ft8, 2.0), (Ft4, 5.0)],
            &[(Ft8, 3.0)],
            &[(Ft8, vec![2.0, 3.0, 4.0]), (Wspr, vec![5.0])],
        );
        assert_eq!(p.start, vec![(Ft8, 4.0), (Wspr, 5.0)]);
        assert_eq!(p.revive, vec![(Ft8, 3.0)]);
        assert_eq!(p.retire, vec![(Ft8, 1.0), (Ft4, 5.0)]);
        // Nothing to do: nothing done.
        assert_eq!(plan(&[(Ft8, 1.0)], &[], &[(Ft8, vec![1.0])]), Plan::default());
    }

    #[test]
    fn a_dial_still_in_the_span_keeps_its_decoder() {
        use WsjtMode::{Ft4, Ft8};
        let mut sk = WideSkimmer::new(&[]);
        assert!(sk.is_idle());
        assert!(sk.set_dials(&[(Ft8, vec![1.0, 2.0]), (Ft4, vec![5.0])]));
        let dec =
            |sk: &WideSkimmer, m, d| sk.running.iter().find(|l| l.is(m, d)).map(|l| l.dec.clone());
        let two = dec(&sk, Ft8, 2.0).unwrap();
        let one = dec(&sk, Ft8, 1.0).unwrap();
        // The span scrolled: 1 left, 3 came into view, 2 and FT4's 5 stayed.
        assert!(sk.set_dials(&[(Ft8, vec![2.0, 3.0]), (Ft4, vec![5.0])]));
        assert!(Arc::ptr_eq(&two, &dec(&sk, Ft8, 2.0).unwrap()));
        assert!(dec(&sk, Ft8, 3.0).is_some());
        assert!(dec(&sk, Ft8, 1.0).is_none());
        assert_eq!(sk.dials(), vec![(Ft8, vec![2.0, 3.0]), (Ft4, vec![5.0])]);
        // The same again: nothing changes.
        assert!(!sk.set_dials(&[(Ft8, vec![2.0, 3.0]), (Ft4, vec![5.0])]));
        // 1 came back before its time was up: the same decoder, taken back.
        assert!(sk.set_dials(&[(Ft8, vec![1.0, 2.0, 3.0]), (Ft4, vec![5.0])]));
        assert!(Arc::ptr_eq(&one, &dec(&sk, Ft8, 1.0).unwrap()));
        // Everything gone: retired, and once their time is up, idle.
        assert!(sk.set_dials(&[]));
        assert!(sk.dials().is_empty());
        assert!(!sk.is_idle());
        sk.expire(Instant::now() + Duration::from_secs(60));
        assert!(sk.is_idle());
    }
}
