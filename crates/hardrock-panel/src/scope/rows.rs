//! The waterfall's and the 3D surface's history, as rows for the GPU.
//!
//! Each row is one spectrum pooled (strongest bin per column) into the
//! texture's width over the band it covers, in dBm as half floats, with that
//! band stored beside it. Because every row carries its own band, the shader
//! can draw any view from any row: a zoom, a pan or the centre moving to
//! follow the receiver needs no resampling pass and loses no history, and a
//! new floor, ceiling or palette recolours what is already there (the
//! mockup's waterfall does the same, from its history).
//!
//! The band is stored relative to a reference frequency so it fits an f32 to
//! a fraction of a hertz; moving more than [`REF_RANGE_HZ`] from it (a band
//! change) starts the history afresh.
//!
//! This side decides which slot each row goes in, so the GPU side only
//! copies; it is all plain data and tested here. With the FFT on the GPU
//! the rows never come to the CPU at all: the same bookkeeping hands out
//! the slots and bands (`push_*_gpu`), the rows go out with no bins, and the
//! GPU pools its own spectrum into those slots.

use super::view::{Span, View, coverage};

/// Rows of waterfall history: about 34 s at 60 frames a second.
pub const WF_ROWS: u32 = 2048;
/// History rows in the 3D surface, as SDRoxide's `DEPTH`; the live spectrum is
/// drawn in front of them.
pub const DEPTH: u32 = 48;
/// Slots for the surface: the live row and [`DEPTH`] behind it, with room.
pub const SURFACE_ROWS: u32 = 64;
/// The most surface rows laid down in one repaint, so a hitch does not fill
/// the surface with copies of one spectrum (SDRoxide's `MAX_STEP`).
const MAX_STEP: u32 = 8;
/// How far the centre may move from the reference before history restarts.
pub const REF_RANGE_HZ: f64 = 4e6;
/// What an empty column holds, dBm: below any floor.
pub const EMPTY_DBM: f32 = -250.0;

/// One row for the GPU.
#[derive(Debug, Clone, PartialEq)]
pub struct RowData {
    pub slot: u32,
    /// The band the row covers, Hz from the reference.
    pub lo: f32,
    pub hi: f32,
    /// dBm, as half-float bits, `tex_w` of them; empty when the GPU writes
    /// the row itself and only the band is to be stored.
    pub bins: Vec<u16>,
}

/// The GPU spectrum's share of an upload: what to run, as the GPU side
/// defines it. Compared by identity.
#[derive(Clone)]
pub struct GpuPart(pub std::sync::Arc<super::gpu::GpuFrame>);

impl std::fmt::Debug for GpuPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuPart({} ops)", self.0.work.ops.len())
    }
}

impl PartialEq for GpuPart {
    fn eq(&self, other: &GpuPart) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Everything to write to the GPU this repaint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Upload {
    /// Increases with every upload, so the GPU side applies each once.
    pub seq: u64,
    /// Forget every row first (the reference moved).
    pub clear: bool,
    pub wf: Vec<RowData>,
    pub surface: Vec<RowData>,
    /// The surface slot that held the live row before this upload: keep a
    /// copy of it before anything is written, for the live row to move on
    /// from smoothly.
    pub prev: Option<u32>,
    pub gpu: Option<GpuPart>,
}

impl Upload {
    pub fn is_empty(&self) -> bool {
        !self.clear && self.wf.is_empty() && self.surface.is_empty() && self.gpu.is_none()
    }
}

/// What the GPU does for one frame's waterfall row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WfStep {
    /// The band to pool the frame over.
    pub cov: View,
    /// Keep the strongest of this frame and what is gathered (a slow row).
    pub max: bool,
    /// The row is complete: into this slot.
    pub slot: Option<u32>,
}

/// A slow waterfall row being gathered from several frames.
#[derive(Debug, Clone)]
struct Pending {
    cov: View,
    max: Vec<f32>,
    frames: u32,
}

/// The CPU side's bookkeeping for both histories.
#[derive(Debug, Clone)]
pub struct Rows {
    tex_w: usize,
    ref_hz: Option<f64>,
    /// The newest waterfall row's slot, and how many slots hold rows.
    wf_head: u32,
    wf_count: u32,
    /// When each waterfall row was laid down, UTC seconds, by slot.
    wf_times: Vec<f64>,
    pending: Option<Pending>,
    /// The live surface row's slot, and how many slots hold rows (live
    /// included).
    s3_head: u32,
    s3_count: u32,
    /// Rows of surface flow owed but not yet laid down: SDRoxide's carry,
    /// which glides the rows back between whole steps.
    s3_accum: f32,
    last_time: Option<f64>,
    /// A slow waterfall row the GPU is gathering: its band and frames.
    gpu_pending: Option<(View, u32)>,
    /// The live surface row's band, the one before it, when the live row
    /// last changed and how often it does.
    live_band: Option<(f32, f32)>,
    prev_band: Option<(f32, f32)>,
    live_at: f64,
    live_iv: f64,
    seq: u64,
    clear: bool,
    scratch: Vec<f32>,
}

impl Rows {
    pub fn new(tex_w: usize) -> Rows {
        Rows {
            tex_w: tex_w.max(16),
            ref_hz: None,
            wf_head: WF_ROWS - 1,
            wf_count: 0,
            wf_times: vec![f64::NAN; WF_ROWS as usize],
            pending: None,
            s3_head: 0,
            s3_count: 0,
            s3_accum: 0.0,
            last_time: None,
            gpu_pending: None,
            live_band: None,
            prev_band: None,
            live_at: f64::NAN,
            live_iv: 1.0 / 30.0,
            seq: 0,
            clear: false,
            scratch: Vec::new(),
        }
    }

    pub fn tex_w(&self) -> usize {
        self.tex_w
    }

    /// The reference the stored bands are relative to.
    pub fn ref_hz(&self) -> f64 {
        self.ref_hz.unwrap_or(0.0)
    }

    pub fn wf_head(&self) -> u32 {
        self.wf_head
    }

    pub fn wf_count(&self) -> u32 {
        self.wf_count
    }

    /// When the row `age` rows older than the newest was laid down, UTC
    /// seconds; `None` past the history.
    pub fn wf_time(&self, age: u32) -> Option<f64> {
        if age >= self.wf_count {
            return None;
        }
        let slot = (self.wf_head + WF_ROWS - age % WF_ROWS) % WF_ROWS;
        let t = self.wf_times[slot as usize];
        t.is_finite().then_some(t)
    }

    pub fn surface_head(&self) -> u32 {
        self.s3_head
    }

    pub fn surface_count(&self) -> u32 {
        self.s3_count
    }

    /// How far the live surface row has come from the previous spectrum to
    /// the newest at `now`, 0 to 1: the spectra come at the frame rate, the
    /// screen repaints faster, and the front row moves between them.
    pub fn live_t(&self, now: f64) -> f32 {
        if !self.live_at.is_finite() {
            return 1.0;
        }
        ((now - self.live_at) / self.live_iv.max(1e-3)).clamp(0.0, 1.0) as f32
    }

    /// The previous live row's band, when there is one to move on from.
    pub fn prev_band(&self) -> Option<(f32, f32)> {
        self.prev_band
    }

    /// The fraction of a row the surface has flowed since the last one.
    pub fn surface_carry(&self) -> f32 {
        self.s3_accum.clamp(0.0, 1.0)
    }

    /// Make sure `center` can be stored against the reference; restart the
    /// history when it cannot.
    pub fn use_center(&mut self, center: f64) {
        let ok = self.ref_hz.is_some_and(|r| (center - r).abs() <= REF_RANGE_HZ);
        if !ok {
            self.ref_hz = Some(center);
            self.clear = true;
            self.wf_count = 0;
            self.s3_count = 0;
            self.pending = None;
            self.gpu_pending = None;
            self.live_band = None;
            self.prev_band = None;
        }
    }

    /// Pool `db` (bins over `frame`) into a row over `cov`, into `scratch`.
    fn pool(&mut self, db: &[f32], frame: Span, cov: View) {
        let (a, b) = super::view::bin_range(frame.lo(), frame.width, db.len(), cov.lo, cov.hi);
        sdr::pool_max_range(db, a, b, self.tex_w, &mut self.scratch);
        self.scratch.resize(self.tex_w, EMPTY_DBM);
    }

    fn row(&self, slot: u32, cov: View, vals: &[f32]) -> RowData {
        let r = self.ref_hz();
        RowData {
            slot,
            lo: (cov.lo - r) as f32,
            hi: (cov.hi - r) as f32,
            bins: vals.iter().map(|&v| to_half(v)).collect(),
        }
    }

    /// Take one spectrum frame for the waterfall, one row every
    /// `frames_per_row` frames (the strongest of each bin over them).
    /// `view` is what is on screen now; the row keeps a little either side.
    pub fn push_waterfall(
        &mut self,
        db: &[f32],
        frame: Span,
        view: View,
        frames_per_row: u32,
        utc: f64,
        out: &mut Upload,
    ) {
        if db.is_empty() || frame.width <= 0.0 {
            return;
        }
        self.use_center(frame.center);
        let cov = coverage(frame, view);
        // A slow row gathers over one band; if the band moves, start again.
        let restart = self.pending.as_ref().is_none_or(|p| p.cov != cov);
        self.pool(db, frame, cov);
        if restart {
            self.pending = Some(Pending { cov, max: self.scratch.clone(), frames: 1 });
        } else if let Some(p) = self.pending.as_mut() {
            for (m, v) in p.max.iter_mut().zip(&self.scratch) {
                *m = m.max(*v);
            }
            p.frames += 1;
        }
        let ready = self.pending.as_ref().is_some_and(|p| p.frames >= frames_per_row.max(1));
        if ready && let Some(p) = self.pending.take() {
            self.wf_head = (self.wf_head + 1) % WF_ROWS;
            self.wf_count = (self.wf_count + 1).min(WF_ROWS);
            self.wf_times[self.wf_head as usize] = utc;
            out.wf.push(self.row(self.wf_head, p.cov, &p.max));
        }
    }

    /// [`Rows::push_waterfall`] for a spectrum on the GPU: what to do with
    /// this frame, and the row's band when it is complete.
    pub fn push_waterfall_gpu(
        &mut self,
        frame: Span,
        view: View,
        frames_per_row: u32,
        utc: f64,
        out: &mut Upload,
    ) -> Option<WfStep> {
        if frame.width <= 0.0 {
            return None;
        }
        self.use_center(frame.center);
        let cov = coverage(frame, view);
        let frames = match self.gpu_pending {
            Some((c, n)) if c == cov => n + 1,
            _ => 1,
        };
        let max = frames > 1;
        if frames < frames_per_row.max(1) {
            self.gpu_pending = Some((cov, frames));
            return Some(WfStep { cov, max, slot: None });
        }
        self.gpu_pending = None;
        self.wf_head = (self.wf_head + 1) % WF_ROWS;
        self.wf_count = (self.wf_count + 1).min(WF_ROWS);
        self.wf_times[self.wf_head as usize] = utc;
        out.wf.push(self.row(self.wf_head, cov, &[]));
        Some(WfStep { cov, max, slot: Some(self.wf_head) })
    }

    /// Advance the 3D surface to `now` (seconds) at `rows_per_sec`, and write
    /// the newest spectrum as its live front row (`db` is `None` when no new
    /// frame came, and only the flow moves).
    pub fn push_surface(
        &mut self,
        db: Option<(&[f32], Span)>,
        view: View,
        now: f64,
        rows_per_sec: f32,
        out: &mut Upload,
    ) {
        let db = db.filter(|(d, _)| !d.is_empty());
        let Some((cov, slots)) = self.surface_step(db.map(|d| d.1), view, now, rows_per_sec, out)
        else {
            return;
        };
        if let Some((db, frame)) = db {
            self.pool(db, frame, cov);
        }
        let row = self.row(0, cov, &self.scratch);
        for slot in slots {
            out.surface.push(RowData { slot, ..row.clone() });
        }
    }

    /// [`Rows::push_surface`] for a spectrum on the GPU: the rows go out
    /// with their bands only, and this says which band to pool the frame
    /// over and which slots it goes in (the live row's last).
    pub fn push_surface_gpu(
        &mut self,
        frame: Option<Span>,
        view: View,
        now: f64,
        rows_per_sec: f32,
        out: &mut Upload,
    ) -> Option<(View, Vec<u32>)> {
        let (cov, slots) = self.surface_step(frame, view, now, rows_per_sec, out)?;
        let row = self.row(0, cov, &[]);
        for &slot in &slots {
            out.surface.push(RowData { slot, ..row.clone() });
        }
        Some((cov, slots))
    }

    /// The surface's flow: which slots a new frame over `frame` goes in.
    fn surface_step(
        &mut self,
        frame: Option<Span>,
        view: View,
        now: f64,
        rows_per_sec: f32,
        out: &mut Upload,
    ) -> Option<(View, Vec<u32>)> {
        let dt = self.last_time.map_or(0.0, |t| (now - t).clamp(0.0, 0.3));
        self.last_time = Some(now);
        let Some(frame) = frame.filter(|f| f.width > 0.0) else {
            // Nothing new to lay down: still let the rows glide, up to a row.
            if self.s3_count > 0 {
                self.s3_accum = (self.s3_accum + dt as f32 * rows_per_sec).min(0.999);
            }
            return None;
        };
        self.use_center(frame.center);
        let cov = coverage(frame, view);
        self.s3_accum += dt as f32 * rows_per_sec;
        let due = (self.s3_accum.floor() as u32).min(MAX_STEP);
        self.s3_accum -= self.s3_accum.floor();
        if self.s3_count == 0 {
            self.s3_count = 1;
            self.prev_band = None;
        } else {
            out.prev = Some(self.s3_head);
            self.prev_band = self.live_band;
        }
        for _ in 0..due {
            // The live row stays where it is as the newest history row; a
            // copy of it becomes the new live row in the next slot.
            self.s3_head = (self.s3_head + 1) % SURFACE_ROWS;
            self.s3_count = (self.s3_count + 1).min(DEPTH + 1);
        }
        // The rows laid down this repaint, as copies of the live one, and
        // the live one.
        let mut slots: Vec<u32> =
            (1..due.min(DEPTH)).map(|k| (self.s3_head + SURFACE_ROWS - k) % SURFACE_ROWS).collect();
        slots.push(self.s3_head);
        let r = self.ref_hz();
        self.live_band = Some(((cov.lo - r) as f32, (cov.hi - r) as f32));
        if self.live_at.is_finite() {
            let gap = (now - self.live_at).clamp(0.0, 0.5);
            self.live_iv += (gap - self.live_iv) * 0.2;
        }
        self.live_at = now;
        Some((cov, slots))
    }

    /// Forget the surface, as when it is switched off: coming back to it
    /// should not show minutes-old rows as the last few seconds.
    pub fn clear_surface(&mut self) {
        self.s3_count = 0;
        self.live_band = None;
        self.prev_band = None;
        self.s3_accum = 0.0;
        self.last_time = None;
    }

    /// Finish this repaint's upload: stamp it, and say whether to clear.
    pub fn seal(&mut self, mut up: Upload) -> Upload {
        up.clear = std::mem::take(&mut self.clear);
        if !up.is_empty() {
            self.seq += 1;
        }
        up.seq = self.seq;
        up
    }
}

/// `v` as half-float bits, with anything not a number made empty.
pub fn to_half(v: f32) -> u16 {
    let v = if v.is_finite() { v.max(EMPTY_DBM) } else { EMPTY_DBM };
    half::f16::from_f32(v).to_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: Span = Span { center: 14_074_000.0, width: 192_000.0 };

    fn whole() -> View {
        View { lo: SPAN.lo(), hi: SPAN.hi() }
    }

    fn frame(n: usize, peak_at: usize) -> Vec<f32> {
        let mut v = vec![-120.0; n];
        v[peak_at] = -50.0;
        v
    }

    #[test]
    fn half_floats_keep_dbm_to_a_tenth() {
        for v in [-150.0f32, -73.3, -10.0, 0.0] {
            let back = half::f16::from_bits(to_half(v)).to_f32();
            assert!((back - v).abs() < 0.07, "{v} came back as {back}");
        }
        assert_eq!(half::f16::from_bits(to_half(f32::NEG_INFINITY)).to_f32(), EMPTY_DBM);
        assert_eq!(half::f16::from_bits(to_half(f32::NAN)).to_f32(), EMPTY_DBM);
    }

    #[test]
    fn waterfall_rows_fill_the_ring_newest_last() {
        let mut r = Rows::new(256);
        let mut up = Upload::default();
        for _ in 0..3 {
            r.push_waterfall(&frame(1024, 512), SPAN, whole(), 1, 0.0, &mut up);
        }
        let slots: Vec<u32> = up.wf.iter().map(|w| w.slot).collect();
        assert_eq!(slots, [0, 1, 2]);
        assert_eq!(r.wf_head(), 2);
        assert_eq!(r.wf_count(), 3);
        assert_eq!(r.wf_time(0), Some(0.0));
        assert_eq!(r.wf_time(3), None);
        let row = &up.wf[0];
        assert_eq!(row.bins.len(), 256);
        assert_eq!((row.lo, row.hi), (-96_000.0, 96_000.0));
        // The carrier lands in the middle column.
        let hot: Vec<usize> =
            (0..256).filter(|&i| half::f16::from_bits(row.bins[i]).to_f32() > -60.0).collect();
        assert_eq!(hot, [128]);
        let up = r.seal(up);
        assert!(up.clear, "the first frame sets the reference");
        assert_eq!(up.seq, 1);
        let up2 = r.seal(Upload::default());
        assert!(!up2.clear && up2.is_empty());
        assert_eq!(up2.seq, 1, "an empty upload is not a new one");
    }

    #[test]
    fn a_slow_waterfall_keeps_the_strongest_of_its_frames() {
        let mut r = Rows::new(128);
        let mut up = Upload::default();
        r.push_waterfall(&frame(1024, 100), SPAN, whole(), 4, 0.0, &mut up);
        r.push_waterfall(&frame(1024, 900), SPAN, whole(), 4, 0.0, &mut up);
        r.push_waterfall(&frame(1024, 500), SPAN, whole(), 4, 0.0, &mut up);
        assert!(up.wf.is_empty());
        r.push_waterfall(&frame(1024, 500), SPAN, whole(), 4, 0.0, &mut up);
        assert_eq!(up.wf.len(), 1);
        let hot =
            up.wf[0].bins.iter().filter(|&&b| half::f16::from_bits(b).to_f32() > -60.0).count();
        assert_eq!(hot, 3, "a short signal in any of the four frames shows");
    }

    #[test]
    fn rows_keep_a_margin_around_a_zoomed_view_and_move_with_it() {
        let mut r = Rows::new(128);
        let mut up = Upload::default();
        let v = View { lo: 14_070_000.0, hi: 14_078_000.0 };
        r.push_waterfall(&frame(4096, 10), SPAN, v, 1, 0.0, &mut up);
        assert_eq!((up.wf[0].lo, up.wf[0].hi), (-6000.0, 6000.0));
        // The centre follows the receiver 100 kHz up: same reference, so no
        // history is lost and the new row is stored against the old reference.
        let moved = Span { center: SPAN.center + 100_000.0, ..SPAN };
        r.push_waterfall(&frame(4096, 10), moved, whole_of(moved), 1, 0.0, &mut up);
        assert_eq!((up.wf[1].lo, up.wf[1].hi), (4000.0, 196_000.0));
        assert!(r.seal(up).clear, "only the very first frame cleared");
        let mut up = Upload::default();
        // A band change: a new reference and a fresh history.
        let far = Span { center: 7_074_000.0, ..SPAN };
        r.push_waterfall(&frame(4096, 10), far, whole_of(far), 1, 0.0, &mut up);
        assert_eq!(r.ref_hz(), 7_074_000.0);
        assert_eq!(r.wf_count(), 1);
        assert!(r.seal(up).clear);
    }

    fn whole_of(s: Span) -> View {
        View { lo: s.lo(), hi: s.hi() }
    }

    #[test]
    fn the_surface_flows_at_its_rate_behind_a_live_row() {
        let mut r = Rows::new(64);
        let f = frame(1024, 5);
        let mut up = Upload::default();
        r.push_surface(Some((&f, SPAN)), whole(), 10.0, 16.0, &mut up);
        // First call: the live row only.
        assert_eq!(up.surface.len(), 1);
        assert_eq!(r.surface_count(), 1);
        let head0 = r.surface_head();
        // A quarter second at 16 rows a second: four rows laid down.
        let mut up = Upload::default();
        r.push_surface(Some((&f, SPAN)), whole(), 10.25, 16.0, &mut up);
        assert_eq!(r.surface_count(), 5);
        assert_eq!(r.surface_head(), (head0 + 4) % SURFACE_ROWS);
        let mut slots: Vec<u32> = up.surface.iter().map(|s| s.slot).collect();
        slots.sort();
        assert_eq!(slots, [head0 + 1, head0 + 2, head0 + 3, head0 + 4]);
        // Half a row more: no new row, the carry says how far they glided.
        let mut up = Upload::default();
        r.push_surface(None, whole(), 10.28125, 16.0, &mut up);
        assert!(up.surface.is_empty());
        assert!((r.surface_carry() - 0.5).abs() < 1e-3);
        // A long stall is clamped, and the depth never exceeds DEPTH + live.
        for i in 0..100 {
            r.push_surface(
                Some((&f, SPAN)),
                whole(),
                11.0 + i as f64,
                16.0,
                &mut Upload::default(),
            );
        }
        assert_eq!(r.surface_count(), DEPTH + 1);
        r.clear_surface();
        assert_eq!(r.surface_count(), 0);
    }

    #[test]
    fn the_gpu_gets_slots_and_bands_and_no_bins() {
        let mut r = Rows::new(256);
        let mut up = Upload::default();
        let steps: Vec<WfStep> =
            (0..3).map(|_| r.push_waterfall_gpu(SPAN, whole(), 3, 5.0, &mut up).unwrap()).collect();
        // A slow row: started, then gathered, then complete in slot 0.
        assert_eq!(
            steps.iter().map(|s| (s.max, s.slot)).collect::<Vec<_>>(),
            [(false, None), (true, None), (true, Some(0))]
        );
        assert_eq!(up.wf.len(), 1);
        assert!(up.wf[0].bins.is_empty());
        assert_eq!((up.wf[0].lo, up.wf[0].hi), (-96_000.0, 96_000.0));
        assert_eq!(r.wf_time(0), Some(5.0));

        // The surface: the live row's slot, then on the next frame the slot
        // it was in is the one to keep as the previous spectrum.
        let mut up = Upload::default();
        let (_, slots) = r.push_surface_gpu(Some(SPAN), whole(), 1.0, 16.0, &mut up).unwrap();
        assert_eq!(slots, [r.surface_head()]);
        assert_eq!(up.prev, None);
        assert!(r.prev_band().is_none());
        let head = r.surface_head();
        let mut up = Upload::default();
        let (_, slots) = r.push_surface_gpu(Some(SPAN), whole(), 1.125, 16.0, &mut up).unwrap();
        assert_eq!(up.prev, Some(head));
        assert_eq!(r.prev_band(), Some((-96_000.0, 96_000.0)));
        assert_eq!(slots.len(), 2);
        assert!(up.surface.iter().all(|s| s.bins.is_empty()));
        // Halfway to the next spectrum, the front row is halfway there.
        let t = r.live_t(1.125 + r.live_iv / 2.0);
        assert!((t - 0.5).abs() < 1e-3, "{t}");
        assert_eq!(r.live_t(100.0), 1.0);
    }
}
