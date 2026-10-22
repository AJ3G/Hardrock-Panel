//! The raw I/Q on the GPU: one ring of complex samples in a storage buffer,
//! filled from the pipeline's I/Q tap, for the GPU spectrum and anything
//! else on the GPU that wants the samples (the cyclostationary view).
//!
//! # The path
//!
//! The pipeline thread's tap ([`GpuIq::sink`]) copies each block into a
//! lock-free ring on the CPU (`rtrb`, about half a second at 6144 kHz) and
//! returns; it never waits, and drops a whole block if the ring is full. A
//! thread of its own, `scope-gpu-iq`, drains that ring every couple of
//! milliseconds in large batches and writes them, cast to bytes with no
//! per-sample copy, into the GPU ring with `Queue::write_buffer`, then
//! submits, and only then publishes the new write index. Anything recorded
//! after reading the index is submitted after those writes, so it sees them;
//! nothing on the UI thread or the I/Q threads ever waits for the GPU.
//!
//! `write_buffer` rather than mapped staging buffers: wgpu copies the bytes
//! into its own staging memory, which it recycles once the GPU has used it,
//! and the copy into the ring is ordered on the queue. Mapping our own
//! buffers would need a map, a wait for the GPU to finish with each one
//! before reuse, and a poll loop, for the same one copy on the CPU.
//!
//! At 6144 kHz the ring takes 6.144 M × 8 bytes = 49.2 MB/s across the bus,
//! well under 1 % of a PCIe 4.0 ×8 link, and nothing else comes the other
//! way but a few kilobytes of readouts a frame.
//!
//! # For other GPU users (`pub(crate)`)
//!
//! * [`GpuIq::buffer`]: the ring, [`RING`] complex samples as interleaved
//!   `f32`, WGSL `array<vec2<f32>>`; stream sample `i` is at `i % RING`.
//! * [`GpuIq::written`]: the write index, samples in the stream so far, all
//!   of them on the queue.
//! * [`GpuIq::oldest_safe`]: the oldest sample that will not be overwritten
//!   before work recorded now runs.
//! * [`GpuIq::epoch_at`] and [`GpuIq::epochs`]: the centre and rate in force
//!   from each stream position on.
//! * [`GpuIq::stream`]: which stream the positions count in; a new sink
//!   starts a new stream from position 0.
//!
//! The ring is only fed while [`GpuIq::set_enabled`] says so: with the FFT
//! on the CPU nothing crosses the bus for it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui_wgpu::wgpu;

/// Complex samples in the GPU ring: 2²³, 1.37 s at 6144 kHz, 64 MB.
pub const RING: u32 = 1 << 23;
/// What a reader keeps clear of the write index's lap: samples the uploader
/// may write between a reader reading the index and its work running.
pub const SAFE: u64 = 1 << 21;
/// The CPU-side ring, floats: about half a second at 6144 kHz.
const HOST_RING: usize = 2 * (1 << 22);
/// The most written in one go, complex samples.
const MAX_BATCH: usize = 1 << 18;
/// How long the uploader waits for more before writing what it has.
const BATCH_WAIT: Duration = Duration::from_millis(2);
/// Epochs kept.
const EPOCHS: usize = 32;

/// A run of the stream at one centre and rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Epoch {
    /// The stream position it starts at.
    pub start: u64,
    pub center_hz: f64,
    pub rate: f64,
}

#[derive(Default)]
struct Shared {
    written: AtomicU64,
    stop: AtomicBool,
    dropped: AtomicU64,
    bytes: AtomicU64,
    epochs: Mutex<VecDeque<Epoch>>,
}

/// The pipeline thread's end: a block in, at once.
struct Feed {
    prod: rtrb::Producer<f32>,
    pushed: u64,
    last: Option<(f64, f64)>,
    shared: Arc<Shared>,
}

impl Feed {
    fn push(&mut self, iq: &[f32], center: f64, rate: f64) {
        let iq = &iq[..iq.len() & !1];
        if iq.is_empty() {
            return;
        }
        if self.prod.slots() < iq.len() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.last != Some((center, rate)) {
            self.last = Some((center, rate));
            let mut e = self.shared.epochs.lock().unwrap_or_else(|e| e.into_inner());
            e.push_back(Epoch { start: self.pushed, center_hz: center, rate });
            while e.len() > EPOCHS {
                e.pop_front();
            }
        }
        if self.prod.push_entire_slice(iq).is_ok() {
            self.pushed += (iq.len() / 2) as u64;
        }
    }
}

/// The ring on the GPU, its uploader thread and the tap that feeds it.
pub struct GpuIq {
    buffer: wgpu::Buffer,
    queue: wgpu::Queue,
    feed: Arc<Mutex<Option<Feed>>>,
    run: Mutex<Run>,
    enabled: Arc<AtomicBool>,
    stream: AtomicU64,
}

/// One stream's uploader.
struct Run {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
}

impl Run {
    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl GpuIq {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuIq {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scope-iq-ring"),
            size: RING as u64 * 8,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        GpuIq {
            buffer,
            queue: queue.clone(),
            feed: Arc::new(Mutex::new(None)),
            run: Mutex::new(Run {
                shared: Arc::new(Shared::default()),
                thread: None,
                started: Instant::now(),
            }),
            enabled: Arc::new(AtomicBool::new(true)),
            stream: AtomicU64::new(0),
        }
    }

    /// Feed the ring (true) or let the tap return at once.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// The stream the positions count in: changes with every new sink.
    pub(crate) fn stream(&self) -> u64 {
        self.stream.load(Ordering::Acquire)
    }

    fn shared(&self) -> Arc<Shared> {
        lock(&self.run).shared.clone()
    }

    /// The tap for a new pipeline: `sink(iq, centre, rate)`, called on the
    /// pipeline's thread. Making one starts the stream afresh, with its own
    /// uploader thread.
    pub fn sink(&self) -> impl Fn(&[f32], f64, f64) + Send + Sync + 'static {
        let mut run = lock(&self.run);
        run.stop();
        let shared = Arc::new(Shared::default());
        let (prod, mut cons) = rtrb::RingBuffer::new(HOST_RING);
        *lock(&self.feed) = Some(Feed { prod, pushed: 0, last: None, shared: shared.clone() });
        let (queue, buffer, sh) = (self.queue.clone(), self.buffer.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("scope-gpu-iq".into())
            .spawn(move || {
                elad_iq::priority::raise_current_thread(elad_iq::priority::Priority::AboveNormal);
                upload_loop(
                    &sh,
                    &mut cons,
                    |off, bytes| queue.write_buffer(&buffer, off, bytes),
                    || {
                        crate::gpu_gate::submit(|| queue.submit(std::iter::empty()));
                    },
                );
            })
            .ok();
        *run = Run { shared, thread, started: Instant::now() };
        self.stream.fetch_add(1, Ordering::AcqRel);
        let (feed, enabled) = (self.feed.clone(), self.enabled.clone());
        move |iq, c, r| {
            if !enabled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(f) = lock(&feed).as_mut() {
                f.push(iq, c, r);
            }
        }
    }

    pub(crate) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub(crate) fn ring_len(&self) -> u32 {
        RING
    }

    pub(crate) fn written(&self) -> u64 {
        self.shared().written.load(Ordering::Acquire)
    }

    pub(crate) fn oldest_safe(&self) -> u64 {
        self.written().saturating_sub(RING as u64 - SAFE)
    }

    /// The epochs whose start has been written, oldest first.
    pub(crate) fn epochs(&self) -> Vec<Epoch> {
        let s = self.shared();
        let w = s.written.load(Ordering::Acquire);
        let e = s.epochs.lock().unwrap_or_else(|e| e.into_inner());
        e.iter().copied().filter(|e| e.start < w).collect()
    }

    /// The centre and rate at stream position `at`.
    pub(crate) fn epoch_at(&self, at: u64) -> Option<Epoch> {
        self.epochs().into_iter().rev().find(|e| e.start <= at)
    }

    /// Blocks the tap dropped because the uploader was behind.
    pub fn dropped(&self) -> u64 {
        self.shared().dropped.load(Ordering::Relaxed)
    }

    /// Bytes sent to the GPU a second since the stream started.
    pub fn bytes_per_sec(&self) -> f64 {
        let run = lock(&self.run);
        let t = run.started.elapsed().as_secs_f64().max(1e-3);
        run.shared.bytes.load(Ordering::Relaxed) as f64 / t
    }
}

impl Drop for GpuIq {
    fn drop(&mut self) {
        *lock(&self.feed) = None;
        lock(&self.run).stop();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The uploader: drain the CPU ring in batches into the GPU ring through
/// `write(byte offset, bytes)`, `submit()` each batch, then publish.
fn upload_loop(
    shared: &Shared,
    cons: &mut rtrb::Consumer<f32>,
    mut write: impl FnMut(u64, &[u8]),
    mut submit: impl FnMut(),
) {
    let mut since = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        let have = cons.slots() & !1;
        if have == 0 || (have < 2 * 8192 && since.elapsed() < BATCH_WAIT) {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        since = Instant::now();
        let take = have.min(2 * MAX_BATCH);
        let Ok(chunk) = cons.read_chunk(take) else { continue };
        let mut at = shared.written.load(Ordering::Relaxed);
        let (a, b) = chunk.as_slices();
        for part in [a, b] {
            at = write_wrapped(&mut write, at, part);
        }
        chunk.commit_all();
        submit();
        shared.bytes.fetch_add(take as u64 * 4, Ordering::Relaxed);
        shared.written.store(at, Ordering::Release);
    }
}

/// Write interleaved `iq` at stream position `at` into the ring, in two
/// pieces where it laps; returns the position after it.
fn write_wrapped(write: &mut impl FnMut(u64, &[u8]), at: u64, iq: &[f32]) -> u64 {
    let mut at = at;
    let mut rest = iq;
    while rest.len() >= 2 {
        let slot = (at % RING as u64) as usize;
        let room = RING as usize - slot;
        let n = (rest.len() / 2).min(room);
        let (now, later) = rest.split_at(n * 2);
        write(slot as u64 * 8, bytemuck::cast_slice(now));
        at += n as u64;
        rest = later;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_written_whole_and_lap_the_ring() {
        let mut writes = Vec::new();
        let start = RING as u64 - 3;
        let iq: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let end = write_wrapped(&mut |off, b: &[u8]| writes.push((off, b.len())), start, &iq);
        assert_eq!(end, start + 10);
        // 3 samples to the end of the ring, 7 from its start.
        assert_eq!(writes, [((RING as u64 - 3) * 8, 24), (0, 56)]);
    }

    #[test]
    fn the_tap_marks_new_centres_and_drops_rather_than_waits() {
        let shared = Arc::new(Shared::default());
        let (prod, mut cons) = rtrb::RingBuffer::new(64);
        let mut f = Feed { prod, pushed: 0, last: None, shared: shared.clone() };
        f.push(&[0.0; 20], 7e6, 192e3);
        f.push(&[0.0; 20], 7e6, 192e3);
        f.push(&[0.0; 20], 7.1e6, 192e3);
        // Full: the next block is dropped whole, and counted.
        f.push(&[0.0; 20], 7.1e6, 192e3);
        assert_eq!(f.pushed, 30);
        assert_eq!(shared.dropped.load(Ordering::Relaxed), 1);
        let e: Vec<Epoch> = shared.epochs.lock().unwrap().iter().copied().collect();
        assert_eq!(
            e,
            [
                Epoch { start: 0, center_hz: 7e6, rate: 192e3 },
                Epoch { start: 20, center_hz: 7.1e6, rate: 192e3 }
            ]
        );
        // The uploader takes it all and publishes the count once written.
        let (stop_tx, written) = (shared.clone(), Arc::new(Mutex::new(Vec::new())));
        let w = written.clone();
        let mut submits = 0;
        let mut calls = 0;
        upload_loop(
            &shared,
            &mut cons,
            |off, b| w.lock().unwrap().push((off, b.len())),
            || {
                submits += 1;
                calls += 1;
                stop_tx.stop.store(true, Ordering::Relaxed);
            },
        );
        assert_eq!(submits, 1);
        assert_eq!(calls, 1);
        assert_eq!(shared.written.load(Ordering::Relaxed), 30);
        assert_eq!(written.lock().unwrap().iter().map(|w| w.1).sum::<usize>(), 240);
    }
}
