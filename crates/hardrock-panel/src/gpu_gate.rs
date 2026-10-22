//! When a thread other than the UI's may submit to the GPU's one queue.
//!
//! wgpu reconfigures a window's surface (on every resize, and when a window
//! moves to a screen with other scaling) only once the queue is idle, and
//! refuses if another thread submits while it waits ("Failed to wait for
//! GPU to come idle before reconfiguring the Surface"). The surface is then
//! left at its old size while egui draws for the new one: every frame after
//! fails validation (a scissor bigger than the target) and is dropped, which
//! is the stutter and hang AJ3G saw dragging windows between his screens,
//! and the glitching while two windows overlapped (22 Oct 2026).
//!
//! The scope's I/Q uploader and the SCF view submit from threads of their
//! own, so they go through [`submit`]: only while the UI thread is in the
//! app's own code ([`open`] to [`close`]), never while eframe handles a
//! window's events or paints, which is where it reconfigures. The other
//! windows are painted inside their `show_viewport_immediate` call, so the
//! gate is shut around each ([`Shut`]). With no frame for [`IDLE`] (nothing
//! on screen to repaint) a waiting thread goes ahead rather than stall.

use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// No frame for this long and a background submit goes ahead.
pub const IDLE: Duration = Duration::from_millis(250);

#[derive(Debug)]
struct State {
    open: bool,
    /// Submits running now.
    busy: u32,
    /// When the UI thread last opened it.
    opened: Instant,
}

struct Gate {
    state: Mutex<State>,
    cv: Condvar,
}

static GATE: LazyLock<Gate> = LazyLock::new(|| Gate {
    state: Mutex::new(State { open: false, busy: 0, opened: Instant::now() }),
    cv: Condvar::new(),
});

fn lock() -> MutexGuard<'static, State> {
    GATE.state.lock().unwrap_or_else(|e| e.into_inner())
}

/// The UI thread is in the app's code: background submits may go.
pub fn open() {
    let mut s = lock();
    s.open = true;
    s.opened = Instant::now();
    GATE.cv.notify_all();
}

/// The UI thread is about to let eframe paint or reconfigure: wait for any
/// submit under way, and hold the rest.
pub fn close() {
    let mut s = lock();
    s.open = false;
    while s.busy > 0 {
        s = GATE.cv.wait(s).unwrap_or_else(|e| e.into_inner());
    }
}

/// Shut while it lives, then open again if it was: around a window that is
/// painted in place (`show_viewport_immediate`).
pub struct Shut(bool);

pub fn shut() -> Shut {
    let was = lock().open;
    close();
    Shut(was)
}

impl Drop for Shut {
    fn drop(&mut self) {
        if self.0 {
            open();
        }
    }
}

/// Closes the gate when dropped: at the end of the app's frame, whichever
/// way it returns.
pub struct Closing;

impl Drop for Closing {
    fn drop(&mut self) {
        close();
    }
}

/// Run `f` (a queue submit, from a thread other than the UI's) once the gate
/// is open, or the UI has been idle for [`IDLE`].
pub fn submit<R>(f: impl FnOnce() -> R) -> R {
    {
        let mut s = lock();
        while !s.open && s.opened.elapsed() < IDLE {
            s = GATE.cv.wait_timeout(s, IDLE / 5).unwrap_or_else(|e| e.into_inner()).0;
        }
        s.busy += 1;
    }
    let r = f();
    let mut s = lock();
    s.busy -= 1;
    GATE.cv.notify_all();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_background_submit_waits_for_the_ui_and_never_runs_while_it_is_shut() {
        // One test owns the gate: it is global.
        open();
        let in_submit = Arc::new(AtomicBool::new(false));
        let shut_saw_one = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let t = {
            let (inside, stop) = (in_submit.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    submit(|| {
                        inside.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_micros(200));
                        inside.store(false, Ordering::SeqCst);
                    });
                    n += 1;
                }
                n
            })
        };
        // The UI's frames: open for a while, then shut while "painting",
        // and nothing may be submitting then.
        for _ in 0..40 {
            open();
            std::thread::sleep(Duration::from_millis(2));
            {
                let _s = shut();
                for _ in 0..20 {
                    if in_submit.load(Ordering::SeqCst) {
                        shut_saw_one.store(true, Ordering::SeqCst);
                    }
                    std::thread::sleep(Duration::from_micros(100));
                }
            }
            close();
            for _ in 0..20 {
                if in_submit.load(Ordering::SeqCst) {
                    shut_saw_one.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_micros(100));
            }
        }
        // Idle: no frames, and it still goes ahead after IDLE.
        let started = Instant::now();
        submit(|| ());
        assert!(started.elapsed() <= IDLE + Duration::from_millis(100));
        stop.store(true, Ordering::Relaxed);
        open();
        let n = t.join().unwrap();
        assert!(n > 10, "the submits ran while open ({n})");
        assert!(!shut_saw_one.load(Ordering::SeqCst), "a submit ran while the gate was shut");
    }
}
