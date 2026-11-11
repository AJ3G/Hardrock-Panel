//! The wide skimmer's decodes, kept in the background (decision 110).
//!
//! At a wide span SKIM WIDE runs a WSJT decoder for every FT8, FT4 and WSPR
//! calling frequency in it, and their slots all end at the same moment. Run
//! as the receiver's own decoder is, at normal priority on the shared rayon
//! pool, they took every core at once at each slot's end, and the whole
//! panel froze for a moment (AJ3G, 11 Nov 2026, at 6144 kHz). So a
//! skimmer's slot is decoded:
//!
//! * on a pool of its own, of [`permits`] threads at below-normal priority,
//!   so the decoders' parallel inner loops never take the receiver's rayon
//!   threads or more than those cores;
//! * after waiting its turn at a gate of as many places: the slots are
//!   decoded a few at a time instead of all at once.
//!
//! The receiver's own DECODE goes round all of this and keeps its priority.

use std::sync::{Condvar, Mutex, OnceLock};

/// How many skimmer slots decode at once on a machine of `cores`: two cores
/// left for the I/Q path, the UI and the receiver's decoder, at least one
/// and never more than three.
pub fn permits_for(cores: usize) -> usize {
    cores.saturating_sub(2).clamp(1, 3)
}

/// [`permits_for`] this machine.
pub fn permits() -> usize {
    permits_for(std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// A counting gate: at most its size of holders at once, the rest wait.
pub struct Gate {
    free: Mutex<usize>,
    turn: Condvar,
}

impl Gate {
    pub fn new(size: usize) -> Self {
        Gate { free: Mutex::new(size.max(1)), turn: Condvar::new() }
    }

    /// Wait for a place; it is given back when the pass is dropped.
    pub fn enter(&self) -> Pass<'_> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.turn.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Pass(self)
    }
}

/// A place at a [`Gate`].
pub struct Pass<'a>(&'a Gate);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.turn.notify_one();
    }
}

struct Background {
    gate: Gate,
    pool: Option<rayon::ThreadPool>,
}

fn background() -> &'static Background {
    static B: OnceLock<Background> = OnceLock::new();
    B.get_or_init(|| {
        let n = permits();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("skim-decode-{i}"))
            .start_handler(|_| {
                lower_current_thread();
            })
            .build()
            .map_err(|e| tracing::warn!("skimmer decode pool: {e}; using the shared one"))
            .ok();
        tracing::info!("skimmer decodes: {n} at a time, below normal priority");
        Background { gate: Gate::new(n), pool }
    })
}

/// Run a skimmer's decode `f` in its turn, on the background pool.
pub fn decode<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    let b = background();
    let _pass = b.gate.enter();
    match &b.pool {
        Some(p) => p.install(f),
        None => f(),
    }
}

/// Lower the calling thread below normal priority, where the system allows.
pub fn lower_current_thread() {
    elad_iq::priority::lower_current_thread();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_few_decode_at_once_and_never_none() {
        assert_eq!(permits_for(1), 1);
        assert_eq!(permits_for(2), 1);
        assert_eq!(permits_for(4), 2);
        assert_eq!(permits_for(32), 3);
    }

    #[test]
    fn the_gate_lets_only_its_size_through_at_once() {
        let gate = Arc::new(Gate::new(2));
        let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (gate, now, most) = (gate.clone(), now.clone(), most.clone());
                std::thread::spawn(move || {
                    let _p = gate.enter();
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    now.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(most.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_decode_runs_and_returns_its_result() {
        assert_eq!(decode(|| 6 * 7), 42);
    }
}
