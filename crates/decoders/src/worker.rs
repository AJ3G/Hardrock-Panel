//! The decoders on their own threads.
//!
//! The panel's audio thread must never wait on a decoder, so each worker takes
//! its audio through a bounded queue that drops (and counts) blocks rather
//! than block when the decoder falls behind, and hands its results back on a
//! channel the UI polls. Dropping a worker stops its threads.
//!
//! Every block of audio carries the UTC time of its first sample, from the
//! system clock (see [`crate::slot`]); every result is stamped in UTC from it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};

use crate::image::{Assembler, ImageUpdate};
use crate::modem::navtex::NavtexRx;
use crate::modem::olivia::OliviaRx;
use crate::modem::psk::PskRx;
use crate::modem::rtty::RttyRx;
use crate::modem::sstv::SstvRx;
use crate::modem::thor::ThorRx;
use crate::modem::wefax::{Ioc, Lpm, WefaxRx};
use crate::resample::Resampler;
use crate::slot::{Slot, SlotRecorder, slot_start};
use crate::wsjt::{self, Msk144Live, WsjtDecode, WsjtMode, WsjtSession};

/// Blocks a worker holds before it starts dropping them. At the panel's
/// usual 10–20 ms blocks this is several seconds.
const QUEUE: usize = 512;

enum Input<C> {
    Audio(Vec<f32>, f64),
    Cmd(C),
}

/// The sending half every worker shares: the queue, the drop count and the
/// threads to join.
struct Feed<C> {
    tx: Option<Sender<Input<C>>>,
    dropped: Arc<AtomicU64>,
    threads: Vec<JoinHandle<()>>,
}

impl<C: Send + 'static> Feed<C> {
    fn new() -> (Self, Receiver<Input<C>>) {
        let (tx, rx) = bounded(QUEUE);
        (Feed { tx: Some(tx), dropped: Arc::new(AtomicU64::new(0)), threads: Vec::new() }, rx)
    }

    fn audio(&self, audio: &[f32], utc: f64) {
        if let Some(tx) = &self.tx {
            match tx.try_send(Input::Audio(audio.to_vec(), utc)) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => {}
                Err(TrySendError::Full(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn cmd(&self, c: C) {
        if let Some(tx) = &self.tx {
            // Commands wait for room: they are rare and must not be lost.
            let _ = tx.send(Input::Cmd(c));
        }
    }
}

impl<C> Drop for Feed<C> {
    fn drop(&mut self) {
        self.tx = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

// ─────────────────────────────── WSJT ───────────────────────────────

/// How to run a WSJT decoder.
#[derive(Debug, Clone)]
pub struct WsjtConfig {
    pub mode: WsjtMode,
    /// The rate of the audio pushed in; 48 kHz from the `sdr` crate's
    /// receiver. It is brought to 12 kHz here.
    pub input_rate: f64,
    /// The operator's own callsigns, so hashed messages naming them resolve
    /// from the first slot.
    pub my_calls: Vec<String>,
    /// A slot joined later than this (seconds after its start) is not decoded:
    /// too little of its signals was heard. One second keeps every station
    /// starting on time; MSK144's pings come any time, so it takes any slot.
    pub max_late_s: f64,
}

impl WsjtConfig {
    pub fn new(mode: WsjtMode) -> Self {
        let max_late_s = if mode == WsjtMode::Msk144 { mode.period_s() } else { 1.0 };
        WsjtConfig { mode, input_rate: sdr::AUDIO_RATE, my_calls: Vec::new(), max_late_s }
    }

    /// The UTC start of the first slot the decoder takes whole when its audio
    /// starts at `first_utc`: the slot it joined, if it joined no later than
    /// [`Self::max_late_s`], or else the next one.
    pub fn first_slot(&self, first_utc: f64) -> f64 {
        let p = self.mode.period_s();
        let start = slot_start(first_utc, p);
        if first_utc - start <= self.max_late_s { start } else { start + p }
    }

    /// When the slot starting at `slot_utc` is captured: from its start until
    /// a whole period later, when the slot thread has its last sample and
    /// hands it to the decoder. The recorder keeps the whole period whatever
    /// the mode sends in it (WSPR's 110.6 s signal sits in a 120 s capture).
    pub fn capture(&self, slot_utc: f64) -> (f64, f64) {
        (slot_utc, slot_utc + self.mode.period_s())
    }
}

/// One slot's decodes, or (MSK144) what one pass heard as the slot runs.
#[derive(Debug, Clone, PartialEq)]
pub struct WsjtSlot {
    pub mode: WsjtMode,
    /// UTC of the slot's start, Unix seconds.
    pub slot_utc: i64,
    /// At the slot's end, every message heard (possibly none): sorted by
    /// audio frequency, or for MSK144 in the order first heard, each at its
    /// best SNR. In a live report, the messages new to the slot and those
    /// heard again at a better SNR.
    pub decodes: Vec<WsjtDecode>,
    /// How long decoding took, so the panel can see a slow machine. For
    /// MSK144's slot end, the passes as the slot ran are not counted.
    pub decode_ms: u32,
    /// A report during the slot (MSK144 only, about once a second while
    /// there is something new); the slot's end is reported with this false.
    pub live: bool,
}

/// What the slot thread hands the decode thread.
enum SlotMsg {
    /// The next audio of the slot starting at this UTC, as it is heard
    /// (MSK144 only).
    Live(f64, Vec<f32>),
    /// A finished slot.
    Done(Slot),
}

/// How much new audio an MSK144 live pass waits for, samples at 12 kHz.
const LIVE_STEP: usize = 12_000;

/// A WSJT mode decoding slot after slot on its own threads: one collects the
/// audio into UTC slots, the other decodes each finished slot.
pub struct WsjtDecoder {
    feed: Feed<()>,
    results: Receiver<WsjtSlot>,
}

/// Larger than this between where a block was expected to start and its time
/// stamp, and the stream is taken to have restarted (seconds).
const RESYNC_S: f64 = 0.05;

impl WsjtDecoder {
    pub fn spawn(cfg: WsjtConfig) -> Self {
        let (mut feed, input) = Feed::<()>::new();
        let (slot_tx, slot_rx) = unbounded::<SlotMsg>();
        let (res_tx, results) = unbounded();
        let mode = cfg.mode;
        let in_rate = cfg.input_rate;
        let max_late = cfg.max_late_s;

        feed.threads.push(
            std::thread::Builder::new()
                .name(format!("wsjt-{}-slots", mode.label()))
                .spawn(move || {
                    let mut rec = SlotRecorder::new(wsjt::RATE, mode.period_s());
                    let mut rs: Option<Resampler<f32>> = None;
                    let mut buf = Vec::new();
                    // UTC of the resampler's first input, outputs so far, and
                    // where the next block should start.
                    let (mut t0, mut outs, mut expect) = (0.0f64, 0u64, f64::NAN);
                    let live = mode == WsjtMode::Msk144;
                    // The slot (its UTC) and how much of it was sent live.
                    let mut sent = (f64::NAN, 0usize);
                    for msg in input.iter() {
                        let Input::Audio(audio, utc) = msg else { continue };
                        // (`expect` is NaN before the first block, which also resyncs.)
                        if (utc - expect).abs().partial_cmp(&RESYNC_S)
                            != Some(std::cmp::Ordering::Less)
                        {
                            rs = (in_rate != wsjt::RATE)
                                .then(|| Resampler::new(in_rate, wsjt::RATE));
                            t0 = utc;
                            outs = 0;
                        }
                        expect = utc + audio.len() as f64 / in_rate;
                        let (block, delay) = match &mut rs {
                            Some(r) => {
                                buf.clear();
                                r.process(&audio, &mut buf);
                                (&buf[..], r.delay())
                            }
                            None => (&audio[..], 0.0),
                        };
                        let first = t0 + (outs as f64 - delay) / wsjt::RATE;
                        outs += block.len() as u64;
                        for s in rec.push(block, first) {
                            if s.first_s > max_late {
                                tracing::info!(
                                    "{} slot {}: joined {:.2} s in, too late to decode",
                                    mode.label(),
                                    s.start_utc,
                                    s.first_s
                                );
                            } else if slot_tx.send(SlotMsg::Done(s)).is_err() {
                                return;
                            }
                        }
                        if live && let Some((at, audio, first_s)) = rec.partial() {
                            if at != sent.0 {
                                sent = (at, 0);
                            }
                            if first_s <= max_late && audio.len() >= sent.1 + LIVE_STEP {
                                let new = audio[sent.1..].to_vec();
                                sent.1 = audio.len();
                                if slot_tx.send(SlotMsg::Live(at, new)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                })
                .expect("spawn wsjt slot thread"),
        );

        let calls = cfg.my_calls;
        feed.threads.push(
            std::thread::Builder::new()
                .name(format!("wsjt-{}-decode", mode.label()))
                .spawn(move || {
                    let mut session = WsjtSession::new();
                    session.seed(&calls.iter().map(String::as_str).collect::<Vec<_>>());
                    let mut live: Option<Msk144Live> = None;
                    let mut next = None;
                    loop {
                        let msg = match next.take() {
                            Some(m) => m,
                            None => match slot_rx.recv() {
                                Ok(m) => m,
                                Err(_) => return,
                            },
                        };
                        let t = Instant::now();
                        let (slot_utc, decodes, is_live) = match msg {
                            SlotMsg::Live(at, audio) => {
                                let slot_utc = at.round() as i64;
                                let l = match &mut live {
                                    Some(l) if l.slot_utc() == slot_utc => l,
                                    _ => live.insert(Msk144Live::new(slot_utc)),
                                };
                                l.push(&audio);
                                // Behind: take all this slot's audio waiting,
                                // then decode once.
                                while let Ok(m) = slot_rx.try_recv() {
                                    match m {
                                        SlotMsg::Live(a, more) if a == at => l.push(&more),
                                        other => {
                                            next = Some(other);
                                            break;
                                        }
                                    }
                                }
                                (slot_utc, l.pass(&mut session), true)
                            }
                            SlotMsg::Done(s) => {
                                let slot_utc = s.start_utc.round() as i64;
                                // Each slot in the log: how loud its audio was
                                // and how late it was joined, so a slot with
                                // no decodes can be told from no audio.
                                let rms = (s.audio.iter().map(|x| x * x).sum::<f32>()
                                    / s.audio.len().max(1) as f32)
                                    .sqrt();
                                tracing::info!(
                                    "{} slot {slot_utc}: audio rms {rms:.4}, joined {:.2} s in",
                                    mode.label(),
                                    s.first_s
                                );
                                let decodes = match live.take() {
                                    Some(mut l) if l.slot_utc() == slot_utc => {
                                        l.push(&s.audio[l.heard().min(s.audio.len())..]);
                                        l.finish(&mut session);
                                        l.decodes().to_vec()
                                    }
                                    _ => wsjt::decode_slot(mode, &s.audio, slot_utc, &mut session),
                                };
                                (slot_utc, decodes, false)
                            }
                        };
                        let decode_ms = t.elapsed().as_millis() as u32;
                        tracing::debug!(
                            mode = mode.label(),
                            slot_utc,
                            n = decodes.len(),
                            decode_ms,
                            live = is_live
                        );
                        if !is_live {
                            tracing::info!(
                                "{} slot {slot_utc}: {} decoded in {decode_ms} ms",
                                mode.label(),
                                decodes.len()
                            );
                        }
                        if is_live && decodes.is_empty() {
                            continue;
                        }
                        let slot = WsjtSlot { mode, slot_utc, decodes, decode_ms, live: is_live };
                        if res_tx.send(slot).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn wsjt decode thread"),
        );
        WsjtDecoder { feed, results }
    }

    /// Audio at the configured rate whose first sample was heard at `utc`
    /// (Unix seconds from the system clock, less the receive chain's delay).
    pub fn push(&self, audio: &[f32], utc: f64) {
        self.feed.audio(audio, utc);
    }

    /// Finished slots, as they come.
    pub fn results(&self) -> &Receiver<WsjtSlot> {
        &self.results
    }

    /// Blocks dropped because the decoder fell behind.
    pub fn dropped(&self) -> u64 {
        self.feed.dropped.load(Ordering::Relaxed)
    }
}

// ─────────────────────────────── text ───────────────────────────────

/// A keyboard mode and its tuning, in audio Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextMode {
    Rtty {
        center_hz: f64,
        baud: f64,
        shift_hz: f64,
        reverse: bool,
    },
    Psk31 {
        carrier_hz: f64,
    },
    /// NAVTEX (SITOR-B) on 518 or 490 kHz, heard 1700 Hz up on USB.
    Navtex {
        reverse: bool,
    },
    Olivia {
        audio_hz: f64,
        tones: usize,
        bw_hz: f64,
    },
    Thor {
        audio_hz: f64,
        baud: f64,
    },
}

impl TextMode {
    /// Amateur RTTY: 45.45 baud, 170 Hz shift, on 2125/2295 Hz.
    pub const RTTY_45: TextMode =
        TextMode::Rtty { center_hz: 2210.0, baud: 45.45, shift_hz: 170.0, reverse: false };
    pub const PSK31: TextMode = TextMode::Psk31 { carrier_hz: 1000.0 };
    pub const NAVTEX: TextMode = TextMode::Navtex { reverse: false };
}

enum TextRx {
    Rtty(RttyRx),
    Psk(PskRx),
    Navtex(NavtexRx),
    Olivia(OliviaRx),
    Thor(ThorRx),
}

impl TextRx {
    fn new(mode: TextMode, rate: f64) -> Self {
        match mode {
            TextMode::Rtty { center_hz, baud, shift_hz, reverse } => {
                let mut r = RttyRx::new(rate, center_hz, baud, shift_hz);
                r.set_reverse(reverse);
                TextRx::Rtty(r)
            }
            TextMode::Psk31 { carrier_hz } => TextRx::Psk(PskRx::new(rate, carrier_hz)),
            TextMode::Navtex { reverse } => {
                let mut r = NavtexRx::new(rate);
                r.set_reverse(reverse);
                TextRx::Navtex(r)
            }
            TextMode::Olivia { audio_hz, tones, bw_hz } => {
                TextRx::Olivia(OliviaRx::new(rate, audio_hz, tones, bw_hz))
            }
            TextMode::Thor { audio_hz, baud } => TextRx::Thor(ThorRx::new(rate, audio_hz, baud)),
        }
    }

    fn process(&mut self, audio: &[f32]) -> String {
        match self {
            TextRx::Rtty(r) => r.process(audio),
            TextRx::Psk(r) => r.process(audio),
            TextRx::Navtex(r) => r.process(audio),
            TextRx::Olivia(r) => r.process(audio),
            TextRx::Thor(r) => r.process(audio),
        }
    }
}

/// Text as it is decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct TextEvent {
    /// UTC (Unix seconds) at the end of the audio the text came out of.
    pub utc: f64,
    pub text: String,
}

/// A keyboard-mode receiver on its own thread.
pub struct TextDecoder {
    feed: Feed<TextMode>,
    events: Receiver<TextEvent>,
}

impl TextDecoder {
    /// `rate` is the rate of the audio pushed in (any; 48 kHz from the
    /// receiver needs no resampling).
    pub fn spawn(mode: TextMode, rate: f64) -> Self {
        let (mut feed, input) = Feed::<TextMode>::new();
        let (tx, events) = unbounded();
        feed.threads.push(
            std::thread::Builder::new()
                .name("text-decoder".into())
                .spawn(move || {
                    let mut rx = TextRx::new(mode, rate);
                    for msg in input.iter() {
                        match msg {
                            Input::Cmd(m) => rx = TextRx::new(m, rate),
                            Input::Audio(audio, utc) => {
                                let text = rx.process(&audio);
                                if !text.is_empty() {
                                    let utc = utc + audio.len() as f64 / rate;
                                    if tx.send(TextEvent { utc, text }).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                })
                .expect("spawn text decoder"),
        );
        TextDecoder { feed, events }
    }

    pub fn push(&self, audio: &[f32], utc: f64) {
        self.feed.audio(audio, utc);
    }

    /// Change mode or tuning; the receiver starts afresh.
    pub fn retune(&self, mode: TextMode) {
        self.feed.cmd(mode);
    }

    pub fn events(&self) -> &Receiver<TextEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.feed.dropped.load(Ordering::Relaxed)
    }
}

// ─────────────────────────────── pictures ───────────────────────────────

/// A picture mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageMode {
    /// SSTV: the mode comes from each picture's VIS header.
    Sstv,
    /// WEFAX: `auto_start` starts and stops on the start and stop tones.
    Wefax { ioc: Ioc, lpm: Lpm, auto_start: bool },
}

/// What the operator can do to a picture receiver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageCmd {
    /// Begin a WEFAX picture now without waiting for the start tone.
    Start,
    /// End the WEFAX picture in progress, keeping what has arrived.
    Stop,
    /// Change mode; the receiver starts afresh.
    Mode(ImageMode),
}

/// A picture update, stamped in UTC.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageEvent {
    pub utc: f64,
    pub update: ImageUpdate,
    /// For [`ImageUpdate::Row`]: that row's RGBA bytes.
    pub row: Option<Vec<u8>>,
}

enum PicRx {
    Sstv(Box<SstvRx>),
    Wefax(Box<WefaxRx>),
}

impl PicRx {
    fn new(mode: ImageMode, rate: f64) -> Self {
        match mode {
            ImageMode::Sstv => PicRx::Sstv(Box::new(SstvRx::new(rate))),
            ImageMode::Wefax { ioc, lpm, auto_start } => {
                let mut r = WefaxRx::new(rate);
                r.set_ioc(ioc);
                r.set_lpm(lpm);
                r.set_auto_start(auto_start);
                r.set_auto_stop(auto_start);
                PicRx::Wefax(Box::new(r))
            }
        }
    }
}

/// An SSTV or WEFAX receiver on its own thread.
pub struct ImageDecoder {
    feed: Feed<ImageCmd>,
    events: Receiver<ImageEvent>,
}

impl ImageDecoder {
    pub fn spawn(mode: ImageMode, rate: f64) -> Self {
        let (mut feed, input) = Feed::<ImageCmd>::new();
        let (tx, events) = unbounded();
        feed.threads.push(
            std::thread::Builder::new()
                .name("image-decoder".into())
                .spawn(move || {
                    let mut rx = PicRx::new(mode, rate);
                    let mut asm = Assembler::new();
                    let (mut sstv_ev, mut wefax_ev, mut ups) = (Vec::new(), Vec::new(), Vec::new());
                    let mut now = 0.0;
                    for msg in input.iter() {
                        match msg {
                            Input::Cmd(ImageCmd::Mode(m)) => {
                                rx = PicRx::new(m, rate);
                                asm = Assembler::new();
                            }
                            Input::Cmd(ImageCmd::Start) => {
                                if let PicRx::Wefax(r) = &mut rx {
                                    r.start_manual(&mut wefax_ev);
                                }
                            }
                            Input::Cmd(ImageCmd::Stop) => {
                                if let PicRx::Wefax(r) = &mut rx {
                                    r.stop_manual(&mut wefax_ev);
                                }
                            }
                            Input::Audio(audio, utc) => {
                                now = utc + audio.len() as f64 / rate;
                                match &mut rx {
                                    PicRx::Sstv(r) => r.process(&audio, &mut sstv_ev),
                                    PicRx::Wefax(r) => r.process(&audio, &mut wefax_ev),
                                }
                            }
                        }
                        asm.sstv(sstv_ev.drain(..), &mut ups);
                        asm.wefax(wefax_ev.drain(..), &mut ups);
                        for update in ups.drain(..) {
                            let row = match (&update, asm.image()) {
                                (ImageUpdate::Row { y }, Some(img)) if *y < img.height => {
                                    Some(img.row(*y).to_vec())
                                }
                                _ => None,
                            };
                            if tx.send(ImageEvent { utc: now, update, row }).is_err() {
                                return;
                            }
                        }
                    }
                })
                .expect("spawn image decoder"),
        );
        ImageDecoder { feed, events }
    }

    pub fn push(&self, audio: &[f32], utc: f64) {
        self.feed.audio(audio, utc);
    }

    pub fn command(&self, c: ImageCmd) {
        self.feed.cmd(c);
    }

    pub fn events(&self) -> &Receiver<ImageEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.feed.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::psk::PskTx;
    use crate::modem::sstv::SstvTx;
    use crate::modem::types::SstvMode;
    use std::time::Duration;

    #[test]
    fn each_mode_captures_its_whole_period_from_its_slot_start() {
        use crate::wsjt::{Fst4Period, Q65Mode};
        // 98 s after an even minute (a WSPR slot's start).
        let base = 1_700_000_160.0;
        let t = base + 98.0;
        for (mode, period, start) in [
            (WsjtMode::Ft8, 15.0, base + 90.0),
            (WsjtMode::Ft4, 7.5, base + 97.5),
            (WsjtMode::Wspr, 120.0, base),
            (WsjtMode::Jt65, 60.0, base + 60.0),
            (WsjtMode::Jt9, 60.0, base + 60.0),
            (WsjtMode::Fst4(Fst4Period::P60), 60.0, base + 60.0),
            (WsjtMode::Q65(Q65Mode::A30), 30.0, base + 90.0),
            (WsjtMode::Msk144, 15.0, base + 90.0),
        ] {
            let cfg = WsjtConfig::new(mode);
            let s = slot_start(t, mode.period_s());
            assert_eq!(s, start, "{}", mode.label());
            assert_eq!(cfg.capture(s), (s, s + period), "{}", mode.label());
        }
    }

    #[test]
    fn the_first_slot_taken_is_the_one_joined_on_time() {
        let ft8 = WsjtConfig::new(WsjtMode::Ft8);
        let b = 1_700_000_010.0; // A 15 s boundary.
        assert_eq!(ft8.first_slot(b + 0.6), b);
        assert_eq!(ft8.first_slot(b + 1.0), b);
        assert_eq!(ft8.first_slot(b + 1.1), b + 15.0, "too late: the next slot");
        // WSPR the same, on the even minute.
        let wspr = WsjtConfig::new(WsjtMode::Wspr);
        let m = 1_700_000_040.0; // 22:34:00, an even minute.
        assert_eq!(wspr.first_slot(m + 30.0), m + 120.0);
        assert_eq!(wspr.first_slot(m + 0.2), m);
        // MSK144's pings come any time: the slot joined is always taken.
        let msk = WsjtConfig::new(WsjtMode::Msk144);
        assert_eq!(msk.first_slot(b + 14.0), b);
    }

    #[test]
    fn ft8_from_48k_audio_lands_in_its_slot_with_dt_near_zero() {
        // An FT8 signal starting 0.5 s into the slot at 12:00:15 UTC, made at
        // 12 kHz and brought up to the receiver's 48 kHz.
        let slot_utc = 1_700_000_010i64;
        let bits = mfsk_core::msg::wsjt77::pack77("CQ", "AJ3G", "FN20").unwrap();
        let tones = mfsk_core::ft8::wave_gen::message_to_tones(&bits);
        let burst = mfsk_core::ft8::wave_gen::tones_to_f32(&tones, 1500.0, 0.3);
        let mut a12 = vec![0.0f32; 12_000 * 18];
        let at = 12_000 / 2 + 2 * 12_000; // 2 s of lead-in before the slot
        a12[at..at + burst.len()].copy_from_slice(&burst);
        let mut up = Resampler::<f32>::new(12_000.0, 48_000.0);
        let mut a48 = Vec::new();
        up.process(&a12, &mut a48);
        let lead = up.delay() / 48_000.0;
        let start = slot_utc as f64 - 2.0 - lead;

        let d = WsjtDecoder::spawn(WsjtConfig::new(WsjtMode::Ft8));
        for (i, c) in a48.chunks(4800).enumerate() {
            d.push(c, start + (i * 4800) as f64 / 48_000.0);
        }
        let slot = loop {
            let s = d.results().recv_timeout(Duration::from_secs(60)).expect("a slot");
            if s.slot_utc == slot_utc {
                break s;
            }
        };
        let m = slot.decodes.iter().find(|m| m.message == "CQ AJ3G FN20").expect("decode");
        assert!(m.dt_s.abs() < 0.05, "{m:?}");
        assert!((m.audio_hz - 1500.0).abs() < 3.0, "{m:?}");
        assert_eq!(d.dropped(), 0);
    }

    #[test]
    fn an_msk144_ping_is_reported_while_its_slot_runs() {
        // A 144 ms ping 6.3 s into the slot at 12:00:15 UTC, in noise, at
        // 12 kHz brought up to 48 kHz, with 2 s of lead-in.
        let slot_utc = 1_700_000_010i64;
        let mut x: u32 = 1;
        let mut a12: Vec<f32> = (0..12_000 * 19)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.1
            })
            .collect();
        let ping = crate::wsjt::tests::msk144_burst("CQ AJ3G FN20", 1480.0, 2, 0.05);
        let at = 12_000 * 2 + 12_000 * 63 / 10;
        for (o, s) in a12[at..].iter_mut().zip(&ping) {
            *o += s;
        }
        let mut up = Resampler::<f32>::new(12_000.0, 48_000.0);
        let mut a48 = Vec::new();
        up.process(&a12, &mut a48);
        let start = slot_utc as f64 - 2.0 - up.delay() / 48_000.0;

        let d = WsjtDecoder::spawn(WsjtConfig::new(WsjtMode::Msk144));
        let chunks: Vec<_> = a48.chunks(4800).collect();
        // Up to 10 s into the slot: anything reported now is live.
        let upto = 120;
        for (i, c) in chunks[..upto].iter().enumerate() {
            d.push(c, start + (i * 4800) as f64 / 48_000.0);
        }
        // (The lead-in's slot ends first, with nothing in it.)
        let s = loop {
            let s = d.results().recv_timeout(Duration::from_secs(30)).expect("a live report");
            if s.slot_utc == slot_utc {
                break s;
            }
        };
        assert!(s.live, "{s:?}");
        let m = s.decodes.iter().find(|m| m.message == "CQ AJ3G FN20").expect("decode");
        assert!((m.dt_s - 6.3).abs() < 0.3, "{m:?}");
        assert!((m.audio_hz - 1480.0).abs() < 10.0, "{m:?}");
        // The rest, past the slot's end: the slot's end lists it once.
        for (i, c) in chunks.iter().enumerate().skip(upto) {
            d.push(c, start + (i * 4800) as f64 / 48_000.0);
        }
        let end = loop {
            let s = d.results().recv_timeout(Duration::from_secs(30)).expect("the slot end");
            if !s.live && s.slot_utc == slot_utc {
                break s;
            }
        };
        assert_eq!(end.decodes.len(), 1, "{end:?}");
        assert_eq!(end.decodes[0].message, "CQ AJ3G FN20");
        assert_eq!(d.dropped(), 0);
    }

    #[test]
    fn psk31_text_comes_back_stamped() {
        let rate = 48_000.0;
        let mut tx = PskTx::new(rate, 1000.0);
        tx.push_text("  CQ CQ DE AJ3G AJ3G K  ");
        let d = TextDecoder::spawn(TextMode::PSK31, rate);
        let mut block = vec![0.0f32; 4800];
        let mut t = 1_700_000_000.0;
        while !tx.drained() {
            let n = tx.next_block(&mut block);
            d.push(&block[..n], t);
            t += n as f64 / rate;
        }
        d.push(&vec![0.0; 48_000], t);
        let mut text = String::new();
        while let Ok(e) = d.events().recv_timeout(Duration::from_secs(5)) {
            assert!(e.utc > 1_700_000_000.0 && e.utc <= t + 1.0);
            text.push_str(&e.text);
            if text.contains("AJ3G K") {
                break;
            }
        }
        assert!(text.contains("DE AJ3G"), "{text:?}");
    }

    #[test]
    fn an_sstv_picture_comes_back_as_rgba() {
        let rate = 48_000.0;
        let mode = SstvMode::Robot36;
        let (w, h) = mode.dimensions();
        let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for _ in 0..w {
                // Top half red, bottom half blue.
                rgb.extend_from_slice(if y < h / 2 { &[220, 30, 30] } else { &[30, 30, 220] });
            }
        }
        let mut tx = SstvTx::new(mode, &rgb, w, h, rate, 0.0);
        let d = ImageDecoder::spawn(ImageMode::Sstv, rate);
        let mut block = vec![0.0f32; 4800];
        let mut t = 1_700_000_000.0;
        while !tx.done() {
            let n = tx.next_block(&mut block);
            d.push(&block[..n], t);
            t += n as f64 / rate;
        }
        d.push(&vec![0.0; 48_000], t);
        let img = loop {
            let e = d.events().recv_timeout(Duration::from_secs(60)).expect("events");
            if let ImageUpdate::Complete(img) = e.update {
                break img;
            }
        };
        assert_eq!((img.width, img.height), (w as u32, h as u32));
        let px = |x: u32, y: u32| &img.pixels[((y * img.width + x) * 4) as usize..][..4];
        let (top, bottom) = (px(160, 60), px(160, 200));
        assert!(top[0] > 150 && top[2] < 100, "{top:?}");
        assert!(bottom[2] > 150 && bottom[0] < 100, "{bottom:?}");
        assert_eq!(top[3], 255);
    }
}
