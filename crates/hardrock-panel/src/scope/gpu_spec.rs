//! When the GPU spectrum transforms what: the bookkeeping side of the FFT on
//! the GPU, run on the UI thread with no samples in hand.
//!
//! The samples are on the GPU already ([`super::gpu_iq`]); this side only
//! counts them. [`Sched`] is `sdr::Spectrum::process`'s timing with the
//! data taken out: the same frame rate limiter, the same Welch segments
//! every N/2 or N/4 samples averaged into the next frame, the newest N when
//! frames come faster than segments, and the same resets on a new centre.
//! It turns a stretch of the stream into a list of [`Op`]s, which the GPU
//! side records into compute passes in order. [`ZoomSched`] does the same
//! for the zoom FFT's decimated stream, and works out the decimator's
//! phases. All of it is plain arithmetic and tested here.

use sdr::Window;

/// Which spectrum an op is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inst {
    Main,
    Zoom,
}

/// Where a pooled row goes in the GPU's row buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dst {
    /// The waterfall row being gathered (a slow row is the strongest of
    /// several frames).
    Pend,
    /// The 3D surface's new row.
    Surface,
    /// The trace on screen, and its peak-hold trace.
    Trace,
    PeakTrace,
}

/// One step for the GPU, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Transform `count` segments, the first starting at stream sample
    /// `start` and each `step` on, and add their powers to the Welch sum.
    Segments { inst: Inst, start: u64, step: u32, count: u32 },
    /// Finish a frame: the mean of the `count` segments summed since the
    /// last one, or (`latest`) one transform of the N samples from there.
    Frame {
        inst: Inst,
        latest: Option<u64>,
        count: u32,
        /// Average into the running average; else start it from this frame.
        primed: bool,
        peak: bool,
        peak_reset: bool,
    },
    /// Pool the frame (or its peak hold) over fractional bins `a..b` into
    /// `cols` columns at `dst`; `max` keeps the strongest of this and what
    /// is there.
    Pool { inst: Inst, peak: bool, a: f64, b: f64, cols: u32, dst: Dst, max: bool },
    /// The gathered waterfall row into waterfall slot `slot`.
    WfRow { slot: u32 },
    /// The new 3D row into these surface slots.
    SurfaceRow { slots: Vec<u32> },
    /// Decimate `outs` zoom samples (see [`ZoomSched`]).
    Decim(Decim),
    /// The strongest bin in each `[lo, hi]` of the frame, with its
    /// neighbours, for the markers.
    Peaks { inst: Inst, queries: Vec<(u32, u32)> },
}

/// One batch of the zoom's decimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decim {
    /// The first output's newest input, a stream sample.
    pub q_first: u64,
    pub outs: u32,
    pub factor: u32,
    /// The mixer's phase at `q_first`, and per output, in turns as 32-bit
    /// fractions; and per tap of the filter.
    pub phase0: u32,
    pub phase_step: u32,
    pub tap_step: u32,
    /// The first output's index in the zoom stream.
    pub m_first: u64,
}

/// How a spectrum is set up on the GPU. `generation` changes when the buffers must
/// be built again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstCfg {
    pub n: u32,
    pub window: Window,
    pub generation: u64,
}

/// The frame timing of one spectrum, after `sdr::Spectrum`.
#[derive(Debug, Clone)]
pub struct Sched {
    n: usize,
    step: Option<usize>,
    hop: f64,
    rate: f64,
    /// Samples taken so far: the stream position.
    pos: u64,
    /// Where the current run of samples began (a centre change starts
    /// one): only samples after it make a transform.
    epoch: u64,
    countdown: f64,
    seg_countdown: f64,
    acc_n: u32,
    primed: bool,
    peak_valid: bool,
    /// Frames made since the epoch began.
    frames: u64,
}

/// What [`Sched::advance`] reports about each frame it made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Made {
    /// The stream position the frame was made at.
    pub at: u64,
}

impl Sched {
    pub fn new(n: usize, rate: f64, fps: f32, overlap: sdr::Overlap, start: u64) -> Sched {
        let rate = rate.max(1.0);
        Sched {
            n,
            step: overlap.step(n),
            hop: rate / fps.clamp(0.1, sdr::spectrum::MAX_FPS) as f64,
            rate,
            pos: start,
            epoch: start,
            countdown: 0.0,
            seg_countdown: 0.0,
            acc_n: 0,
            primed: false,
            peak_valid: false,
            frames: 0,
        }
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Samples between frames.
    pub fn hop(&self) -> f64 {
        self.hop
    }

    /// The frame rate changed: keep everything else.
    pub fn set_fps(&mut self, fps: f32) {
        self.hop = self.rate / fps.clamp(0.1, sdr::spectrum::MAX_FPS) as f64;
    }

    pub fn set_overlap(&mut self, o: sdr::Overlap) {
        let step = o.step(self.n);
        if step != self.step {
            self.step = step;
            self.acc_n = 0;
            self.seg_countdown = 0.0;
        }
    }

    /// Peak hold went off: the next frame with it on starts it afresh.
    pub fn forget_peaks(&mut self) {
        self.peak_valid = false;
    }

    /// A new run of samples from `at` (a new centre): the averages and
    /// peaks start again and nothing before `at` is transformed.
    pub fn restart(&mut self, at: u64) {
        self.epoch = at;
        self.pos = self.pos.max(at);
        self.countdown = 0.0;
        self.seg_countdown = 0.0;
        self.acc_n = 0;
        self.primed = false;
        self.peak_valid = false;
        self.frames = 0;
    }

    fn filled(&self) -> usize {
        (self.pos - self.epoch).min(self.n as u64) as usize
    }

    /// Skip to near `upto` when the stream has run too far ahead (the UI
    /// stalled, or the scope was not drawn): the Welch sum starts again,
    /// the averages carry on.
    pub fn catch_up(&mut self, upto: u64, keep: u64) {
        if upto > self.pos + keep {
            let to = upto.saturating_sub(self.n as u64).max(self.epoch);
            if to > self.pos {
                self.pos = to;
                self.acc_n = 0;
                self.countdown = 0.0;
                self.seg_countdown = 0.0;
            }
        }
    }

    /// Take the stream up to `upto`, appending ops for `inst` to `out`, and
    /// stop early after `max_frames` frames. Returns the frames made.
    pub fn advance(
        &mut self,
        upto: u64,
        inst: Inst,
        peak_hold: bool,
        max_frames: usize,
        out: &mut Vec<Op>,
    ) -> Vec<Made> {
        let mut made = Vec::new();
        let n = self.n;
        let mut seg_run: Option<(u64, u32)> = None;
        let flush = |run: &mut Option<(u64, u32)>, out: &mut Vec<Op>, step: usize| {
            if let Some((start, count)) = run.take() {
                out.push(Op::Segments { inst, start, step: step as u32, count });
            }
        };
        while self.pos < upto && made.len() < max_frames {
            let have = (upto - self.pos) as f64;
            let mut next = self.countdown;
            if self.step.is_some() {
                next = next.min(self.seg_countdown);
            }
            let due = next.ceil().max(1.0);
            let take = have.min(due) as u64;
            self.pos += take;
            self.countdown -= take as f64;
            if let Some(step) = self.step {
                self.seg_countdown -= take as f64;
                if self.seg_countdown <= 0.0 {
                    self.seg_countdown += step as f64;
                    if self.filled() >= n {
                        let start = self.pos - n as u64;
                        seg_run = match seg_run {
                            Some((s, c)) if s + c as u64 * step as u64 == start => Some((s, c + 1)),
                            other => {
                                let mut r = other;
                                flush(&mut r, out, step);
                                Some((start, 1))
                            }
                        };
                        self.acc_n += 1;
                    }
                }
            }
            if self.countdown <= 0.0 {
                self.countdown += self.hop;
                if self.filled() >= n {
                    if let Some(step) = self.step {
                        flush(&mut seg_run, out, step);
                    }
                    let latest = (self.acc_n == 0).then(|| self.pos - n as u64);
                    out.push(Op::Frame {
                        inst,
                        latest,
                        count: self.acc_n,
                        primed: self.primed,
                        peak: peak_hold,
                        peak_reset: peak_hold && !self.peak_valid,
                    });
                    self.acc_n = 0;
                    self.primed = true;
                    self.peak_valid = peak_hold;
                    self.frames += 1;
                    made.push(Made { at: self.pos });
                } else {
                    self.countdown = self.countdown.min((n - self.filled()) as f64);
                }
            }
        }
        if let Some(step) = self.step {
            flush(&mut seg_run, out, step);
        }
        made
    }
}

/// The zoom's decimator: which outputs are due and with what phases, and
/// the timing of the zoom's own spectrum over them.
#[derive(Debug, Clone)]
pub struct ZoomSched {
    pub factor: u32,
    pub taps: u32,
    /// The region's offset from the span's centre, Hz, and the span's rate.
    offset: f64,
    rate: f64,
    /// The next output's index, and its newest input's stream position.
    m_next: u64,
    q_next: u64,
    /// Where the mixer's phase is counted from.
    q_ref: u64,
    pub spec: Sched,
}

/// The decimator's filter length for a factor: 40 taps an output sample,
/// enough for a Blackman-Harris window to bring what folds into the clean
/// 80 % of the band down by about 90 dB.
pub fn zoom_taps(factor: u32) -> u32 {
    40 * factor + 1
}

/// `turns` (any real number) as a 32-bit fraction of a turn.
pub fn turns_u32(turns: f64) -> u32 {
    (turns.rem_euclid(1.0) * 4_294_967_296.0).round() as u64 as u32
}

impl ZoomSched {
    /// A zoom by `factor` on the region `offset` Hz from the centre of a
    /// span sampled at `rate`, its first output once the filter has `start`
    /// and the samples after it in hand.
    pub fn new(
        factor: u32,
        offset: f64,
        rate: f64,
        n: usize,
        fps: f32,
        overlap: sdr::Overlap,
        start: u64,
    ) -> ZoomSched {
        let taps = zoom_taps(factor);
        let q0 = start + taps as u64 - 1;
        ZoomSched {
            factor,
            taps,
            offset,
            rate,
            m_next: 0,
            q_next: q0,
            q_ref: q0,
            spec: Sched::new(n, rate / factor as f64, fps, overlap, 0),
        }
    }

    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn out_rate(&self) -> f64 {
        self.rate / self.factor as f64
    }

    /// Move the region (a pan): the decimator runs on, the zoom's spectrum
    /// starts again from the next output.
    pub fn set_offset(&mut self, offset: f64) {
        if offset != self.offset {
            self.offset = offset;
            self.q_ref = self.q_next;
            self.spec.restart(self.m_next);
        }
    }

    /// Decimate what has come up to `written` (stream samples), at most
    /// `max_in` samples' worth, skipping ahead first if more than `keep`
    /// are waiting; then run the zoom spectrum's timing.
    pub fn advance(
        &mut self,
        written: u64,
        keep: u64,
        max_in: u64,
        peak_hold: bool,
        max_frames: usize,
        out: &mut Vec<Op>,
    ) -> Vec<Made> {
        let d = self.factor as u64;
        if written > self.q_next + keep {
            // Too far behind: jump, and start the zoom's spectrum again.
            let skip = (written - self.q_next - keep / 2) / d;
            self.q_next += skip * d;
            self.m_next += skip;
            self.q_ref = self.q_next;
            self.spec.restart(self.m_next);
        }
        if written > self.q_next {
            let avail = (written - 1 - self.q_next) / d + 1;
            let outs = avail.min((max_in / d).max(1)) as u32;
            let per = self.offset / self.rate;
            out.push(Op::Decim(Decim {
                q_first: self.q_next,
                outs,
                factor: self.factor,
                phase0: turns_u32(per * (self.q_next - self.q_ref) as f64),
                phase_step: turns_u32(per * d as f64),
                tap_step: turns_u32(per),
                m_first: self.m_next,
            }));
            self.q_next += outs as u64 * d;
            self.m_next += outs as u64;
        }
        let m = self.m_next;
        self.spec.catch_up(m, (self.spec.hop() * 8.0) as u64 + self.spec.n() as u64);
        self.spec.advance(m, Inst::Zoom, peak_hold, max_frames, out)
    }
}

/// The decimator's low-pass filter for `factor`: a Blackman-Harris windowed
/// sinc cut at half the decimated rate, summing to one so a carrier keeps
/// its level.
pub fn zoom_filter(factor: u32) -> Vec<f32> {
    let l = zoom_taps(factor) as usize;
    let mid = (l - 1) as f64 / 2.0;
    let d = factor as f64;
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    let mut h: Vec<f64> = (0..l)
        .map(|k| {
            let x = std::f64::consts::TAU * k as f64 / (l - 1) as f64;
            let w = A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos();
            let t = (k as f64 - mid) / d;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
            };
            w * sinc
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for v in &mut h {
        *v /= sum;
    }
    h.into_iter().map(|v| v as f32).collect()
}

/// The four-step split of an `n`-point FFT: n1 × n2 with both at most
/// 2048, and their logs.
pub fn split(n: u32) -> (u32, u32, u32, u32) {
    let l = n.trailing_zeros();
    let l1 = l.div_ceil(2);
    let l2 = l - l1;
    (1 << l1, 1 << l2, l1, l2)
}

/// ω_N^m = exp(-2πi·m/N) for m in 0..N, interleaved, from f64.
pub fn twiddles(n: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity(2 * n as usize);
    for m in 0..n {
        let a = -std::f64::consts::TAU * m as f64 / n as f64;
        v.push(a.cos() as f32);
        v.push(a.sin() as f32);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdr::Overlap;

    fn frames(ops: &[Op]) -> Vec<&Op> {
        ops.iter().filter(|o| matches!(o, Op::Frame { .. })).collect()
    }

    #[test]
    fn frames_come_at_the_frame_rate_like_the_cpu_spectrum() {
        // As sdr's the_frame_rate_is_limited_by_the_sample_count: a second
        // at 6144 kHz and 25 frames a second.
        let mut s = Sched::new(2048, 6_144_000.0, 25.0, Overlap::Latest, 0);
        let mut ops = Vec::new();
        for i in 1..=100u64 {
            s.advance(i * 61_440, Inst::Main, false, 100, &mut ops);
        }
        let f = frames(&ops);
        assert!((24..=26).contains(&f.len()), "{} frames", f.len());
        // Every frame is one transform of the newest N.
        for op in f {
            let Op::Frame { latest, count, .. } = op else { unreachable!() };
            assert!(latest.is_some());
            assert_eq!(*count, 0);
        }
    }

    #[test]
    fn welch_segments_are_batched_and_counted_into_the_frame() {
        let rate = 1_536_000.0;
        let mut s = Sched::new(2048, rate, 30.0, Overlap::Half, 0);
        let mut ops = Vec::new();
        s.advance(rate as u64, Inst::Main, false, 1000, &mut ops);
        let f = frames(&ops);
        assert!((30..=31).contains(&f.len()), "{}", f.len());
        // Between frames, the segments in one batch, 1024 apart.
        let mut segs = 0;
        let mut counted = 0;
        for op in &ops {
            match op {
                Op::Segments { start, step, count, .. } => {
                    assert_eq!(*step, 1024);
                    assert_eq!(start % 1024, 0);
                    segs += count;
                }
                Op::Frame { count, latest, .. } => {
                    counted += count;
                    assert!(latest.is_none());
                }
                _ => {}
            }
        }
        // Every segment lands in a frame, but for those since the last one,
        // which wait for the next.
        assert!(segs >= counted && segs - counted <= 51, "{segs} segments, {counted} framed");
        assert!(segs >= 1490, "{segs}: every sample is seen");
        // The first frame starts the average, the rest add to it.
        assert!(matches!(f[0], Op::Frame { primed: false, .. }));
        assert!(matches!(f[1], Op::Frame { primed: true, .. }));
    }

    #[test]
    fn a_big_fft_at_a_low_rate_overlaps_rather_than_stalling() {
        // As sdr's test of the same name: 2 s at 192 kHz, 128k points.
        let mut s = Sched::new(131_072, 192_000.0, 30.0, Overlap::Latest, 0);
        let mut ops = Vec::new();
        for i in 1..=200u64 {
            s.advance(i * 1920, Inst::Main, false, 100, &mut ops);
        }
        let n = frames(&ops).len();
        assert!((38..=42).contains(&n), "{n} frames");
    }

    #[test]
    fn a_new_centre_starts_the_averages_and_peaks_again() {
        let mut s = Sched::new(4096, 192_000.0, 60.0, Overlap::Half, 0);
        let mut ops = Vec::new();
        s.advance(50_000, Inst::Main, true, 100, &mut ops);
        let f = frames(&ops);
        assert!(matches!(f[0], Op::Frame { peak_reset: true, .. }));
        assert!(matches!(f[1], Op::Frame { peak_reset: false, primed: true, .. }));
        s.restart(60_000);
        let mut ops = Vec::new();
        s.advance(60_000 + 4095, Inst::Main, true, 100, &mut ops);
        assert!(frames(&ops).is_empty(), "nothing until N samples of the new centre");
        s.advance(70_000, Inst::Main, true, 100, &mut ops);
        let f = frames(&ops);
        assert!(matches!(f[0], Op::Frame { primed: false, peak_reset: true, .. }));
        // No segment reaches back before the new centre.
        for op in &ops {
            if let Op::Segments { start, .. } = op {
                assert!(*start >= 60_000);
            }
            if let Op::Frame { latest: Some(st), .. } = op {
                assert!(*st >= 60_000);
            }
        }
    }

    #[test]
    fn a_stalled_ui_skips_ahead_instead_of_queueing() {
        let mut s = Sched::new(8192, 6_144_000.0, 60.0, Overlap::ThreeQuarters, 0);
        let mut ops = Vec::new();
        s.advance(100_000, Inst::Main, false, 8, &mut ops);
        s.catch_up(10_000_000, 400_000);
        assert!(s.pos() >= 10_000_000 - 8192);
        ops.clear();
        let made = s.advance(10_000_000, Inst::Main, false, 8, &mut ops);
        assert!(made.len() <= 1);
    }

    #[test]
    fn the_split_keeps_both_factors_in_a_workgroup() {
        assert_eq!(split(4096), (64, 64, 6, 6));
        assert_eq!(split(8192), (128, 64, 7, 6));
        assert_eq!(split(1 << 20), (1024, 1024, 10, 10));
        assert_eq!(split(1 << 22), (2048, 2048, 11, 11));
        let tw = twiddles(8);
        assert!((tw[2] - 0.70710677).abs() < 1e-7 && (tw[3] + 0.70710677).abs() < 1e-7);
    }

    #[test]
    fn the_zoom_filter_keeps_the_band_and_rejects_what_folds() {
        let d = 16;
        let h = zoom_filter(d);
        assert_eq!(h.len() as u32, zoom_taps(d));
        let gain = |f: f64| {
            // |H(f)| at f (cycles a sample at the input rate).
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (k, &v) in h.iter().enumerate() {
                let a = -std::f64::consts::TAU * f * k as f64;
                re += v as f64 * a.cos();
                im += v as f64 * a.sin();
            }
            20.0 * (re * re + im * im).sqrt().log10()
        };
        let out = 1.0 / d as f64;
        assert!(gain(0.0).abs() < 1e-4);
        assert!(gain(0.4 * out).abs() < 0.1, "passband {}", gain(0.4 * out));
        assert!(gain(0.6 * out) < -85.0, "stopband {}", gain(0.6 * out));
        assert!(gain(0.9 * out) < -85.0);
    }

    #[test]
    fn the_decimator_runs_on_with_exact_phases() {
        let rate = 1_536_000.0;
        let mut z = ZoomSched::new(64, 200_000.0, rate, 4096, 30.0, Overlap::Half, 1000);
        let mut ops = Vec::new();
        z.advance(1000 + 64 * 100, 1 << 22, 1 << 20, false, 8, &mut ops);
        let Op::Decim(d) = ops[0] else { panic!("{ops:?}") };
        // The first output once the filter is full.
        assert_eq!(d.q_first, 1000 + zoom_taps(64) as u64 - 1);
        assert_eq!(d.m_first, 0);
        assert_eq!(d.phase0, 0);
        assert_eq!(d.phase_step, turns_u32(200_000.0 * 64.0 / rate));
        let mut ops = Vec::new();
        z.advance(1000 + 64 * 200, 1 << 22, 1 << 20, false, 8, &mut ops);
        let Op::Decim(d2) = ops[0] else { panic!() };
        assert_eq!(d2.m_first, d.outs as u64);
        assert_eq!(d2.q_first, d.q_first + 64 * d.outs as u64);
        let want = turns_u32(200_000.0 / rate * (64 * d.outs) as f64);
        assert_eq!(d2.phase0, want);
        assert_eq!(z.out_rate(), rate / 64.0);
        assert_eq!(turns_u32(-0.25), 3 << 30);
    }
}
