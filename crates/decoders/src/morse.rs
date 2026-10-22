// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/cw.rs, GPL-3.0-or-later.

//! CW (Morse): the alphabet, and the envelope-domain timing engine that reads
//! keying, as SDRoxide's CW skimmer uses it on one STFT bin per signal. The
//! wide skimmer ([`crate::keyskim`]) feeds each CW track's bin magnitude
//! through a [`CwDecoder`] of its own.
//!
//! The decoder is deliberately *not* the usual "measure each mark as it ends,
//! call it a dit or a dah against a running average, and adapt" design. That
//! shape has to commit to a decision threshold and to a speed before it has seen
//! the evidence, and on a weak or fading signal both guesses are wrong in the
//! same direction: a threshold set too high clips the marks short, which drags
//! the speed estimate up, which reclassifies the next dah as a dit. It is a
//! feedback loop with no way back, and it is why such decoders produce long runs
//! of `E`/`T`/`I` on exactly the signals you most want copied.
//!
//! [`CwDecoder`] instead keeps a few seconds of the keying envelope and, on each
//! hop, *searches* for the (threshold, dit length, edge bias) that make the run
//! lengths best fit Morse's own timing model — 1 and 3 units keyed, 1, 3 and 7
//! units of silence. The residual of that fit is a real confidence number: noise
//! has no timing model to fit, so its best cost stays high and nothing is
//! emitted. Only then are characters read off, and only the ones that have not
//! been emitted before. Nothing is decided before the evidence is in, and one
//! wrong element cannot poison what follows.
//!
//! The idea of scoring a candidate (speed, threshold) pair by how well the
//! resulting intervals fit Morse timing is Georgi Gerganov's, from ggmorse.
//! SDRoxide's own notes on what it does differently are kept below.

// ─── Morse alphabet ──────────────────────────────────────────────────────────

/// International Morse (ITU-R M.1677-1) plus the punctuation and prosigns that
/// turn up in ordinary QSOs. `.` is a dit, `-` a dah.
const TABLE: &[(char, &str)] = &[
    ('A', ".-"),
    ('B', "-..."),
    ('C', "-.-."),
    ('D', "-.."),
    ('E', "."),
    ('F', "..-."),
    ('G', "--."),
    ('H', "...."),
    ('I', ".."),
    ('J', ".---"),
    ('K', "-.-"),
    ('L', ".-.."),
    ('M', "--"),
    ('N', "-."),
    ('O', "---"),
    ('P', ".--."),
    ('Q', "--.-"),
    ('R', ".-."),
    ('S', "..."),
    ('T', "-"),
    ('U', "..-"),
    ('V', "...-"),
    ('W', ".--"),
    ('X', "-..-"),
    ('Y', "-.--"),
    ('Z', "--.."),
    ('0', "-----"),
    ('1', ".----"),
    ('2', "..---"),
    ('3', "...--"),
    ('4', "....-"),
    ('5', "....."),
    ('6', "-...."),
    ('7', "--..."),
    ('8', "---.."),
    ('9', "----."),
    ('.', ".-.-.-"),
    (',', "--..--"),
    ('?', "..--.."),
    ('/', "-..-."),
    ('=', "-...-"),
    ('+', ".-.-."),
    ('-', "-....-"),
    (':', "---..."),
    ('(', "-.--."),
    (')', "-.--.-"),
    ('"', ".-..-."),
    ('\'', ".----."),
    ('@', ".--.-."),
    // The accented letters ITU-R M.1677-1 lists beside the plain alphabet.
    // They are ordinary Morse, not an extension: a Nordic or German operator
    // sends them without comment, and a receiver that has never been told
    // about them copies each one as a `?` (issue #382).
    //
    // One character per code, because the table has to invert: several of
    // these are shared between languages — `.-.-` is A-umlaut and also
    // A-E-ligature, `---.` is O-umlaut and also O-slash — and which of them
    // was meant is not in the signal. [`fold`] is what lets the *transmit*
    // side accept the others and send the code they share.
    //
    // `----` is deliberately absent. M.1677 lists it as the digraph CH (and
    // Slovene/Croatian S-caron), and it is not a character this table can
    // hold — but more to the point, four dahs is exactly what a run of
    // mis-timed T's looks like, and turning that into a letter would be
    // turning noise into words.
    ('Ä', ".-.-"),
    ('Å', ".--.-"),
    ('Ç', "-.-.."),
    ('È', ".-..-"),
    ('É', "..-.."),
    ('Ñ', "--.--"),
    ('Ö', "---."),
    ('Ü', "..--"),
];

/// Decode a `.-` element string to a character, or `None` if it is not Morse.
pub fn morse_decode(code: &str) -> Option<char> {
    TABLE.iter().find(|(_, c)| *c == code).map(|(ch, _)| *ch)
}

/// Encode a character to its `.-` element string. Case-insensitive; `None` for
/// anything with no Morse representation.
pub fn morse_encode(ch: char) -> Option<&'static str> {
    let up = fold(ch);
    TABLE.iter().find(|(c, _)| *c == up).map(|(_, code)| *code)
}

/// The character [`TABLE`] actually carries for `ch`: upper case, with the
/// national letters that *share* one Morse code folded onto the one entry that
/// holds it.
///
/// Two things are going on here, and the second is the reason this is not
/// `to_ascii_uppercase`. The ASCII version leaves every accented letter alone,
/// so a lower-case `ä` never matched the table at all. And Morse gives one code
/// to letters several languages spell differently — `.-.-` is Ä to a German and
/// Æ to a Dane, `---.` is Ö and Ø, `.--.-` is Å and À — so the sender's own
/// spelling has to reach the one entry the table holds for that code.
///
/// A character whose upper case is more than one letter — German ß, which
/// upper-cases to "SS" — is left alone rather than truncated to its first
/// letter: sending S for ß would put a *different word* on the air, and no code
/// at all is the answer the keyer already had for it.
fn fold(ch: char) -> char {
    let mut up = ch.to_uppercase();
    let (Some(c), None) = (up.next(), up.next()) else { return ch };
    match c {
        // ITU-R M.1677-1 writes the first three of these as one row each —
        // "à å", "ä", "é" — and the national alphabets that use the same code
        // for their own letter follow from that: Danish and Norwegian Æ and Ø,
        // Polish Ą, Ć, Ł, Ń and Ę, Esperanto Ĉ and Ŭ.
        'Æ' | 'Ą' => 'Ä',
        'Ø' => 'Ö',
        'À' => 'Å',
        'Ć' | 'Ĉ' => 'Ç',
        'Ł' => 'È',
        'Ę' | 'Đ' => 'É',
        'Ń' => 'Ñ',
        'Ŭ' => 'Ü',
        c => c,
    }
}

// ─── the envelope-domain timing engine ───────────────────────────────────────

/// Seconds of envelope held for the fit. Long enough that a slow sender still
/// puts several characters in it (at 10 WPM a word is about 4 s), short enough
/// that a speed change is followed rather than averaged away.
const WINDOW_S: f32 = 6.0;
/// Re-fit and emit this often. The decode latency is roughly this plus the run
/// that terminates the character.
const HOP_S: f32 = 0.1;
/// Noise-floor estimation block. Short enough to follow a changing band, long
/// enough that a block of ordinary keying still contains silence to measure.
const BLOCK_S: f32 = 0.3;
/// Decision thresholds tried, in dB above the estimated noise level.
///
/// Referring them to the *noise* rather than to the signal is what makes QSB
/// survivable. The receiver's noise floor does not fade — only the signal on
/// top of it does — so one threshold a few dB above the floor holds good from
/// the peak of a fade to the trough, where a threshold set as a fraction of the
/// mark level would climb as the signal sank and go deaf exactly when it was
/// needed. The fit picks which of these to use.
const THRESHOLDS_DB: &[f32] = &[1.0, 2.0, 3.0, 4.5, 6.0, 8.0, 10.0, 13.0, 16.0, 20.0, 25.0, 31.0];
/// A Rayleigh envelope's 20th percentile sits this far below its mean (0.668σ
/// against 1.253σ). Scaling the measured percentile by it turns a robust order
/// statistic — one that a 50 % keying duty cycle cannot pull upwards — into the
/// noise level the thresholds above are quoted against.
const P20_TO_MEAN: f32 = 1.876;
/// How far above the whole-window noise estimate a single block's may sit
/// (a ratio, ~4 dB). Bounds how much a stretch of dense keying can lift the
/// floor under itself.
const FLOOR_HEADROOM: f32 = 1.6;
/// Speed search bounds (WPM). Below 6 the window holds too little to fit; above
/// 70 the envelope rate stops resolving a dit.
const WPM_MIN: f32 = 6.0;
const WPM_MAX: f32 = 70.0;
/// Coarse speed grid ratio, then the fine one used around the coarse winner.
const WPM_COARSE_STEP: f32 = 1.05;
const WPM_FINE_STEP: f32 = 1.008;
/// Fraction of a transmission's intervals that may fail to fit the timing model
/// before it stops being read as Morse. This is the decoder's squelch, and it
/// is what keeps noise off the panel.
const MAX_COST: f32 = 0.35;
/// Consecutive hops that must agree before anything is emitted. A run of noise
/// can fit *a* timing model, but not the same one twice running; a real signal
/// gives the same speed hop after hop. Costs one hop of latency.
const LOCK_HOPS: u32 = 2;
/// How far the fitted speed may move between hops and still count as the same
/// signal for [`LOCK_HOPS`].
const LOCK_WPM_TOL: f32 = 0.12;
/// Width, in units, of the window a run's timing counts as "fitting" within.
///
/// The fit is scored by how many intervals land inside it, softly — one minus
/// the mean of a Gaussian bump on each residual — rather than by the mean
/// squared residual. The difference matters entirely for rejecting noise. A
/// squared-error score can always be improved by finding an interpretation
/// under which a few intervals are outliers and then discounting them, and with
/// a threshold, a speed, a spacing unit and an edge bias to choose from, noise
/// finds one: measured, its best mean-squared fit came out *better* than a real
/// signal's at 0 dB. Here an interval that does not fit contributes its full
/// weight however badly it misses, so the score is the fraction of the
/// transmission the model actually explains, which noise cannot fake.
const FIT_SIGMA: f32 = 0.40;
/// How far below the mark level a decision threshold has to stay. Closer than
/// this and it is measuring the shape of the keying envelope's top, not the
/// keying.
const THRESHOLD_HEADROOM_DB: f32 = 6.0;
/// Highest threshold always worth trying, whatever the measured level.
const THRESHOLD_FLOOR_DB: f32 = 10.0;
/// Shortest run, as a fraction of the fitted dit, that is still an element
/// rather than the envelope crossing the threshold on its way past.
const DEGLITCH: f32 = 0.35;
/// Least signal-to-noise, in the detector's own bandwidth, that is read as a
/// signal at all.
///
/// A Rayleigh envelope's ninetieth percentile sits only 4.7 dB above the noise
/// level estimated from its twentieth, so pure noise cannot reach this however
/// well its run lengths happen to fit; a signal weak enough to be worth calling
/// marginal is already well past it.
const MIN_SNR_DB: f32 = 8.0;
/// Spacing-unit candidates, as a multiple of the element unit. 1.0 is ordinary
/// timing; the tail covers Farnsworth down to roughly a third of the character
/// speed, which is as slow as the practice sending goes.
const SPACE_RATIOS: &[f32] =
    &[1.0, 1.15, 1.35, 1.6, 1.9, 2.2, 2.6, 3.0, 3.5, 4.1, 4.8, 5.6, 6.5, 8.0];
/// Never emit more than this much backlog at once. A decoder that has just
/// found the speed after a spell of noise has a whole window of unemitted
/// characters behind it; dumping all six seconds reads as a glitch.
const CATCHUP_S: f32 = 3.0;
/// Longest element string still worth looking up. Anything longer is a burst of
/// noise inside one character, not a character.
const MAX_ELEMENTS: usize = 8;

/// One thresholded run of the envelope: `[start, end)` in window samples.
#[derive(Clone, Copy)]
struct Run {
    on: bool,
    start: u32,
    end: u32,
}

impl Run {
    fn len(&self) -> f32 {
        (self.end - self.start) as f32
    }
}

/// The timing model fitted to one run list.
#[derive(Clone, Copy, Debug)]
struct Fit {
    /// Dit length, in envelope samples.
    unit: f32,
    /// Spacing unit, in envelope samples: what a character gap divides by three
    /// and a word gap by seven.
    ///
    /// The same as `unit` for ordinary sending, and longer under Farnsworth,
    /// where the characters are sent at full speed and only the spacing is
    /// stretched. Fitting it separately is what keeps an 18-WPM-at-8-WPM signal
    /// from being read as 8 WPM with every dah turned into a dit.
    space: f32,
    /// Edge bias, in envelope samples. Thresholding a shaped keying envelope
    /// below its half-amplitude point lengthens *every* mark and shortens every
    /// gap by the same amount, so the two are one parameter rather than noise —
    /// fitting it is what lets a low threshold (good for weak signals) be used
    /// without the mark/gap classification drifting.
    bias: f32,
    /// Fraction of intervals the model does not explain. The fit quality, and
    /// the decoder's confidence.
    cost: f32,
    /// Character- and word-spacing gaps the spacing unit was fitted from.
    longs: u32,
}

/// A character read off the fitted run list.
///
/// Emission is keyed to window sample indices, so re-fitting the same audio
/// cannot emit the same character twice. `commit` is deliberately *not* the end
/// of the character: it sits a fixed unit into the silence that terminated it,
/// far from any run boundary, because a boundary is exactly what moves by a
/// sample or two when the next hop picks a slightly different threshold — and a
/// commit point that lands on one drops every other character.
struct Decoded {
    ch: char,
    start: u32,
    commit: u32,
}

/// Streaming Morse decoder over a keying-envelope signal.
///
/// Feed it an amplitude-like envelope (magnitude, not power) at a fixed rate
/// with [`CwDecoder::push`]; it returns newly decoded text. The envelope may be
/// anything that rises with the keying — a narrowband audio detector's output,
/// or one STFT bin of a wideband skimmer.
pub struct CwDecoder {
    rate: f32,
    /// Rolling envelope window.
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`, so emission survives the window
    /// sliding underneath it.
    base: u64,
    /// Absolute index up to which characters have already been emitted.
    emitted_end: u64,
    since_hop: usize,
    hop: usize,

    /// Envelope in dB above the local noise level (`normalize`).
    norm: Vec<f32>,
    /// Per-block (noise, mark) percentiles, and the noise estimate widened over
    /// neighbouring blocks.
    blocks: Vec<(f32, f32)>,
    spread: Vec<f32>,
    /// Scratch for the per-block percentiles.
    pct: Vec<f32>,
    /// Runs before the de-glitch pass.
    raw: Vec<Run>,
    runs: Vec<Run>,
    best_runs: Vec<Run>,
    chars: Vec<Decoded>,

    /// Speed of the most recent fit, whether or not it was good enough to
    /// believe. Only the hop-to-hop agreement check uses this.
    fit_wpm: f32,
    /// Speed of the most recent *locked* fit — what the caller is told.
    /// Between transmissions the decoder goes on fitting whatever the noise
    /// offers, and a readout that followed it would be a random number
    /// generator with a WPM label on it.
    wpm: f32,
    cost: f32,
    snr_db: f32,
    /// Consecutive hops whose fit agreed with the one before it.
    good_hops: u32,
    /// Spacing unit as a multiple of the element unit, smoothed across hops.
    ///
    /// It comes off a coarse scan, so hop to hop it steps between grid points;
    /// used raw, the boundary it sets between "between characters" and "between
    /// words" moves with it and sprays word breaks through the middle of a
    /// callsign. What it is measuring — whether this operator sends Farnsworth
    /// spacing — does not change during an over, so it is worth averaging.
    /// `None` until a window with enough spacing in it has been seen: starting
    /// from an assumed 1.0 and averaging towards the truth would put word
    /// breaks through the first second of every Farnsworth transmission.
    space_ratio: Option<f32>,
    /// Operator-locked speed, when the panel's speed control is not on auto.
    lock_wpm: Option<f32>,
}

impl CwDecoder {
    /// `rate` is the envelope sample rate in Hz.
    pub fn new(rate: f32) -> Self {
        let cap = (WINDOW_S * rate) as usize + 1;
        CwDecoder {
            rate,
            buf: Vec::with_capacity(cap),
            base: 0,
            emitted_end: 0,
            since_hop: 0,
            hop: (HOP_S * rate) as usize,
            norm: Vec::with_capacity(cap),
            blocks: Vec::new(),
            spread: Vec::new(),
            pct: Vec::new(),
            raw: Vec::new(),
            runs: Vec::new(),
            best_runs: Vec::new(),
            chars: Vec::new(),
            fit_wpm: 20.0,
            wpm: 20.0,
            cost: f32::INFINITY,
            snr_db: -30.0,
            good_hops: 0,
            space_ratio: None,
            lock_wpm: None,
        }
    }

    /// How often to re-fit and emit, in seconds. The default suits one signal
    /// being read by an operator; a skimmer running this over hundreds of
    /// simultaneous signals trades latency for the CPU back.
    pub fn set_hop_s(&mut self, seconds: f32) {
        self.hop = (seconds.clamp(HOP_S, WINDOW_S / 2.0) * self.rate) as usize;
    }

    /// Pin the speed search to `wpm` (±10%), or `None` to search the full range.
    pub fn set_speed_lock(&mut self, wpm: Option<f32>) {
        self.lock_wpm = wpm.map(|w| w.clamp(WPM_MIN, WPM_MAX));
    }

    /// Forget everything: the window, the emitted-index bookkeeping and the
    /// fitted speed. For a retune, where the old signal's timing says nothing
    /// about the new one's.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.base = 0;
        self.emitted_end = 0;
        self.since_hop = 0;
        self.cost = f32::INFINITY;
        self.snr_db = -30.0;
        self.good_hops = 0;
        self.space_ratio = None;
    }

    /// Speed of the last good fit, in words per minute.
    pub fn wpm(&self) -> f32 {
        self.wpm
    }

    /// Mean squared timing residual of the last fit, in dits². Below
    /// [`MAX_COST`] the decoder is copying; well above it there is no Morse
    /// there.
    pub fn cost(&self) -> f32 {
        self.cost
    }

    /// True while the decoder is copying: the last fits were good enough and
    /// agreed with each other.
    pub fn locked(&self) -> bool {
        self.good_hops >= LOCK_HOPS
    }

    /// Signal-to-noise of the envelope, in dB: the fitted mark level over the
    /// noise floor, in whatever bandwidth the caller's detector has. The
    /// caller scales it to a reference bandwidth if it wants a comparable
    /// figure.
    pub fn raw_snr_db(&self) -> f32 {
        self.snr_db
    }

    /// Feed envelope samples; returns whatever new text they completed.
    pub fn push(&mut self, env: &[f32]) -> String {
        self.buf.extend_from_slice(env);
        self.since_hop += env.len();
        let cap = (WINDOW_S * self.rate) as usize;
        if self.buf.len() > cap {
            let drop = self.buf.len() - cap;
            self.buf.drain(..drop);
            self.base += drop as u64;
        }
        if self.since_hop < self.hop {
            return String::new();
        }
        self.since_hop = 0;
        self.analyze()
    }

    fn analyze(&mut self) -> String {
        // Enough blocks that the noise estimate is worth having, and enough
        // signal that a character is not read off half of itself.
        if self.buf.len() < (4.0 * BLOCK_S * self.rate) as usize {
            return String::new();
        }
        self.normalize();
        if self.snr_db < MIN_SNR_DB {
            self.cost = f32::INFINITY;
            self.good_hops = 0;
            return String::new();
        }

        // The run list depends only on the threshold, and the timing fit only
        // on the run list — so build the runs once per threshold and score the
        // whole speed grid against each, rather than re-encoding the envelope
        // for every (speed, threshold) pair.
        let (wpm_lo, wpm_hi) = match self.lock_wpm {
            Some(w) => (w * 0.9, w * 1.1),
            None => (WPM_MIN, WPM_MAX),
        };
        // The speed does not change between hops, so last hop's unit sets how
        // short a run has to be to be a glitch. Before there is one, nothing is
        // absorbed.
        let min_run =
            if self.good_hops > 0 { (DEGLITCH * 1.2 * self.rate / self.wpm) as u32 } else { 0 };
        // A threshold within a few dB of the mark level does not measure the
        // keying, it clips it: the shaped top of a dit spends less time above
        // such a threshold than a dah's does, in a ratio that has nothing to do
        // with 1 and 3. Not trying them at all is cheaper than scoring them and
        // safer than hoping they score badly.
        // …but never rule out the low end: on a marginal signal the measured
        // level understates the marks, and a decoder that declines to try any
        // threshold at all copies nothing.
        let t_max = (self.snr_db - THRESHOLD_HEADROOM_DB).max(THRESHOLD_FLOOR_DB);
        let mut best: Option<Fit> = None;
        for &t in THRESHOLDS_DB.iter().take_while(|&&t| t <= t_max) {
            self.encode_runs(t, min_run);
            // Two truncated edge runs plus something to fit between them.
            if self.runs.len() < 6 {
                continue;
            }
            let Some(fit) = fit_runs(&self.runs[1..self.runs.len() - 1], self.rate, wpm_lo, wpm_hi)
            else {
                continue;
            };
            if best.is_none_or(|b| fit.cost < b.cost) {
                best = Some(fit);
                self.best_runs.clear();
                self.best_runs.extend_from_slice(&self.runs);
            }
        }

        let Some(fit) = best else {
            self.cost = f32::INFINITY;
            self.good_hops = 0;
            return String::new();
        };
        self.cost = fit.cost;
        let wpm = 1.2 * self.rate / fit.unit;
        // The first hop of a new signal has nothing to agree with, so it stands
        // as the reference for the next one rather than being thrown away.
        let agrees =
            self.good_hops == 0 || (wpm - self.fit_wpm).abs() < LOCK_WPM_TOL * self.fit_wpm;
        self.fit_wpm = wpm;
        if fit.cost > MAX_COST || !agrees {
            self.good_hops = 0;
            return String::new();
        }
        self.good_hops += 1;
        if self.good_hops < LOCK_HOPS {
            return String::new();
        }
        self.wpm = wpm;

        // Only from a window with enough spacing in it to have measured
        // anything: one or two gaps will fit whatever ratio the scan tries
        // first, and a single such hop dragging the average down is enough to
        // put a word break inside the next callsign.
        if fit.longs >= 2 {
            let ratio = fit.space / fit.unit;
            self.space_ratio = Some(self.space_ratio.map_or(ratio, |p| 0.85 * p + 0.15 * ratio));
        }
        self.decode_runs(&fit);
        self.emit()
    }

    /// Restate the envelope as dB above the locally estimated noise level.
    ///
    /// Everything downstream — the thresholds, the SNR readout — is then
    /// referred to the noise rather than to the signal, which is what makes a
    /// single threshold hold good across a fade.
    fn normalize(&mut self) {
        let n = self.buf.len();
        let bn = ((BLOCK_S * self.rate) as usize).max(8);
        // Whole blocks only, the last one swallowing the remainder. A short
        // tail block is the worst case there is for an order statistic: at the
        // window edge it can be thirty samples of one dah, whose 20th
        // percentile is the mark level rather than the noise.
        let nb = (n / bn).max(1);
        self.blocks.clear();
        for b in 0..nb {
            let s = b * bn;
            let e = if b + 1 == nb { n } else { (b + 1) * bn };
            self.pct.clear();
            self.pct.extend_from_slice(&self.buf[s..e]);
            let lo = percentile(&mut self.pct, 0.20);
            let hi = percentile(&mut self.pct, 0.90);
            self.blocks.push((lo, hi));
        }

        // A receiver's noise floor does not move much over six seconds, and
        // what does move it (AGC riding the keying) moves it *with* the signal,
        // which is the one thing an estimate that followed it closely would
        // cancel out. So take a whole-window figure and let the per-block one
        // only go below it: a run of dense keying can then no longer drag the
        // floor up over its own marks, which is what makes a threshold set
        // above it go deaf for a hop and split a character in two.
        self.pct.clear();
        self.pct.extend(self.blocks.iter().map(|b| b.0));
        let global = percentile(&mut self.pct, 0.25);
        // Never claim more dynamic range than a receiver has. Fed a synthetic
        // signal with no noise on it at all the floor estimate goes to zero,
        // every ring and every shaping tail is then hundreds of dB "above the
        // noise", and the thresholds lose all meaning.
        let ceil = self.blocks.iter().fold(0.0f32, |a, w| a.max(w.1));
        let min_floor = ceil * 1e-4;
        self.spread.clear();
        for b in 0..nb {
            let s = b.saturating_sub(2);
            let e = (b + 3).min(nb);
            let lo = self.blocks[s..e].iter().fold(f32::INFINITY, |a, w| a.min(w.0));
            let lo = lo.min(global * FLOOR_HEADROOM).max(min_floor);
            self.spread.push((lo * P20_TO_MEAN).max(1e-12));
        }

        // Reported against the whole window, not per block: the number the
        // panel shows should be the signal's, and a strong signal's SNR does
        // not drop because it happens to be between characters.
        self.pct.clear();
        self.pct.extend(self.blocks.iter().map(|b| b.1));
        let g_hi = percentile(&mut self.pct, 0.9);
        self.pct.clear();
        self.pct.extend(self.spread.iter().copied());
        let g_lo = percentile(&mut self.pct, 0.5);
        self.snr_db = 20.0 * (g_hi / g_lo).max(1.0).log10();

        // Interpolate the noise level between block centres so a band that
        // changes level mid-block does not step the normalised envelope.
        self.norm.clear();
        self.norm.resize(n, 0.0);
        let last = nb - 1;
        for i in 0..n {
            let x = i as f32 / bn as f32 - 0.5;
            let b0 = x.floor().max(0.0).min(last as f32) as usize;
            let b1 = (b0 + 1).min(last);
            let t = (x - b0 as f32).clamp(0.0, 1.0);
            let f = self.spread[b0] + t * (self.spread[b1] - self.spread[b0]);
            self.norm[i] = (20.0 * (self.buf[i].max(1e-12) / f).log10()).clamp(-30.0, 80.0);
        }
    }

    /// Run-length encode `norm > t` into [`Run`]s, absorbing any run shorter
    /// than `min_run` samples into the one before it.
    ///
    /// At the edge of copy the envelope of a mark dips through the threshold
    /// several times — three independent noise samples inside one dit is enough
    /// — and each dip costs two spurious intervals that fit nothing. Measured on
    /// a signal at +3 dB in 500 Hz, a six-second window held seventy-five
    /// intervals where clean keying holds thirty-three, and the fit was scored
    /// on the wreckage. Nothing that brief is an element at any speed the
    /// decoder searches, so it is not one.
    fn encode_runs(&mut self, t: f32, min_run: u32) {
        self.raw.clear();
        let n = self.norm.len();
        let mut on = self.norm[0] > t;
        let mut start = 0u32;
        for i in 1..n {
            let cur = self.norm[i] > t;
            if cur != on {
                self.raw.push(Run { on, start, end: i as u32 });
                on = cur;
                start = i as u32;
            }
        }
        self.raw.push(Run { on, start, end: n as u32 });

        self.runs.clear();
        for &r in &self.raw {
            match self.runs.last_mut() {
                Some(last) if last.on == r.on || r.end - r.start < min_run => last.end = r.end,
                _ => self.runs.push(r),
            }
        }
    }

    /// Read characters off the fitted run list.
    ///
    /// The first run is truncated by the window edge, so its length means
    /// nothing and it is skipped. The *last* is not skipped when it is silence:
    /// a gap that has already run three units is a finished character however
    /// much longer it turns out to be, and waiting for it to close would lose
    /// the last character of every over.
    fn decode_runs(&mut self, fit: &Fit) {
        self.chars.clear();
        let runs = &self.best_runs;
        if runs.len() < 3 {
            return;
        }
        let (u, v) = (fit.unit, fit.unit * self.space_ratio.unwrap_or(1.0));
        let mut sym = String::new();
        let mut start = 0u32;
        for (i, r) in runs.iter().enumerate().skip(1) {
            let last = i + 1 == runs.len();
            if r.on {
                if last {
                    break; // a mark still in progress is not yet an element
                }
                if sym.is_empty() {
                    start = r.start;
                }
                sym.push(if r.len() - fit.bias > 2.0 * u { '-' } else { '.' });
                if sym.chars().count() > MAX_ELEMENTS {
                    sym.clear();
                }
                continue;
            }
            // Gaps carry the bias the other way: the threshold that lengthened
            // the mark shortened the silence beside it by the same amount.
            let g = r.len() + fit.bias;
            if g < 2.0 * u {
                continue; // between elements of one character
            }
            if !sym.is_empty() {
                let ch = morse_decode(&sym).unwrap_or('?');
                sym.clear();
                self.chars.push(Decoded { ch, start, commit: r.start + u as u32 });
            }
            if g > 5.0 * v {
                // Between words. Placed at fixed offsets into the gap rather
                // than at its edges: the offsets are the same whether this gap
                // is five units long or five seconds, and whether it has closed
                // yet, so a later hop places it identically and it is emitted
                // exactly once.
                //
                // In element units, deliberately, even though what decided this
                // *was* a word gap is the spacing unit. The spacing unit comes
                // off a coarse scan and can step 15 % between hops; a marker
                // placed at a multiple of it would then move far enough to be
                // emitted twice — and to carry the emitted-index past the start
                // of the next character, swallowing it. The element unit moves
                // by a fraction of a percent. A word gap is at least five
                // element units wide, so three of them always lands inside it.
                self.chars.push(Decoded {
                    ch: ' ',
                    start: r.start + (2.0 * u) as u32,
                    commit: r.start + (3.0 * u) as u32,
                });
            }
        }
    }

    /// Emit the decoded characters this window has not already produced.
    fn emit(&mut self) -> String {
        let n = self.buf.len() as u64;
        let catchup = (CATCHUP_S * self.rate) as u64;
        self.emitted_end = self.emitted_end.max((self.base + n).saturating_sub(catchup));
        let mut out = String::new();
        for c in &self.chars {
            let a = self.base + c.start as u64;
            let b = self.base + c.commit as u64;
            // `start >= emitted_end` as well as `commit > emitted_end`: a re-fit
            // that merged two already-emitted characters into one would
            // otherwise emit the merged result a second time.
            if a >= self.emitted_end && b > self.emitted_end {
                out.push(c.ch);
                self.emitted_end = b;
            }
        }
        out
    }
}

/// Search the speed grid for the dit length that makes `runs` fit Morse timing
/// best. Coarse geometric pass, then a fine one around its winner.
fn fit_runs(runs: &[Run], rate: f32, wpm_lo: f32, wpm_hi: f32) -> Option<Fit> {
    let mut best: Option<Fit> = None;
    let mut wpm = wpm_lo;
    while wpm <= wpm_hi {
        if let Some(f) = eval_fit(runs, 1.2 * rate / wpm)
            && best.is_none_or(|b| f.cost < b.cost)
        {
            best = Some(f);
        }
        wpm *= WPM_COARSE_STEP;
    }
    let coarse = best?;
    let centre = 1.2 * rate / coarse.unit;
    let mut wpm = (centre / WPM_COARSE_STEP).max(wpm_lo);
    let hi = (centre * WPM_COARSE_STEP).min(wpm_hi);
    while wpm <= hi {
        if let Some(f) = eval_fit(runs, 1.2 * rate / wpm)
            && best.is_none_or(|b| f.cost < b.cost)
        {
            best = Some(f);
        }
        wpm *= WPM_FINE_STEP;
    }
    best
}

/// Score one candidate dit length against a run list: fit the edge bias and the
/// spacing unit, then take the fraction of intervals the model fails to explain.
fn eval_fit(runs: &[Run], unit: f32) -> Option<Fit> {
    // Below a couple of envelope samples per dit the run lengths are
    // quantisation noise and any speed "fits".
    if unit < 2.0 {
        return None;
    }

    // Fit the bias from the intervals whose nominal length is not in doubt:
    // marks (1 or 3 units, whichever is nearer) and inter-element gaps. Longer
    // gaps are left out because an operator's — or Farnsworth's — character and
    // word spacing is not held to the nominal 3 and 7.
    let mut bias = 0.0f32;
    for _ in 0..3 {
        let mut acc = 0.0f32;
        let mut n = 0u32;
        for r in runs {
            let l = r.len();
            if r.on {
                let c = if l - bias > 2.0 * unit { 3.0 } else { 1.0 };
                acc += l - c * unit;
                n += 1;
            } else if l + bias < 2.0 * unit {
                acc += unit - l;
                n += 1;
            }
        }
        if n == 0 {
            return None;
        }
        bias = (acc / n as f32).clamp(-0.45 * unit, 0.45 * unit);
    }

    // Marks and inter-element gaps: scored against the element unit, two-sided
    // and strictly. These are the intervals Morse actually fixes.
    let mut fit = 0.0f32;
    let mut n = 0u32;
    let (mut dits, mut dahs, mut longs) = (0u32, 0u32, 0u32);
    for r in runs {
        let l = r.len();
        if r.on {
            let dah = l - bias > 2.0 * unit;
            if dah {
                dahs += 1;
            } else {
                dits += 1;
            }
            fit += inlier((l - ((if dah { 3.0 } else { 1.0 }) * unit + bias)) / unit);
            n += 1;
        } else if l + bias <= 2.0 * unit {
            fit += inlier((l + bias - unit) / unit);
            n += 1;
        } else {
            longs += 1;
        }
    }
    if n < 4 || dits + dahs < 3 {
        return None;
    }

    // Everything longer than an element gap is character or word spacing, which
    // Farnsworth stretches — 18 WPM characters at 8 WPM spacing puts thirteen
    // dits between letters, not three. So the spacing unit is fitted separately
    // rather than assumed equal to the element unit, and by scanning rather
    // than by iterating from a seed: seeded refinement converges to whatever
    // cluster it started nearest, which for real text is the wrong one.
    let mut space = unit;
    if longs > 0 {
        let mut best = f32::NEG_INFINITY;
        for &ratio in SPACE_RATIOS {
            let s = unit * ratio;
            let mut acc = 0.0f32;
            for r in runs {
                let g = r.len() + bias;
                if !r.on && g > 2.0 * unit {
                    acc += inlier(gap_dev(g, s));
                }
            }
            // Prefer plain timing where it explains the gaps as well: stretched
            // spacing is the unusual case, and letting it win ties would hand a
            // noise fit a free parameter to hide behind.
            acc -= 0.02 * longs as f32 * ratio.ln();
            if acc > best {
                best = acc;
                space = s;
            }
        }
        fit += best;
        n += longs;
    }

    let mut cost = 1.0 - fit / n as f32;
    // Morse at the right speed uses both element lengths. A candidate that
    // explains everything with one of them is usually the true speed's third or
    // triple — real text breaks the tie, and this nudges it.
    if dits == 0 || dahs == 0 {
        cost += 0.25;
    }
    Some(Fit { unit, space, bias, cost, longs })
}

/// How well one interval fits: 1 dead on, falling to 0 over [`FIT_SIGMA`].
fn inlier(dev: f32) -> f32 {
    (-(dev * dev) / (2.0 * FIT_SIGMA * FIT_SIGMA)).exp()
}

/// Deviation of one character-or-word gap `g` from a spacing unit `s`, in
/// spacing units.
fn gap_dev(g: f32, s: f32) -> f32 {
    if g <= 5.0 * s {
        (g - 3.0 * s) / s
    } else {
        // Between words. An operator pausing to think sends a gap longer than
        // seven units and is still sending Morse, so over-length is charged at
        // a fraction of the rate — but it cannot be free. Exempting it entirely
        // lets the spacing unit collapse to the element unit and call every gap
        // in a Farnsworth transmission a word gap, which reads back as every
        // letter its own word.
        let d = (g - 7.0 * s) / s;
        if d > 0.0 { d * 0.55 } else { d }
    }
}

/// `q`-quantile of `v`, which is reordered in place.
fn percentile(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = ((v.len() - 1) as f32 * q).round() as usize;
    let (_, m, _) = v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    *m
}
