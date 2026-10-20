//! Thread priorities for the I/Q path.
//!
//! The ELAD's USB thread has to collect a bulk transfer every few
//! milliseconds at 6144 kHz or the radio's buffer overruns, and the DSP
//! thread behind it has to keep up with it; neither may wait behind a busy
//! UI or a decoder. On Windows each is raised with `SetThreadPriority` when
//! it starts. Elsewhere the scheduler is left alone: raising a thread above
//! normal needs privileges there (`CAP_SYS_NICE` or real-time limits), and
//! both threads spend most of their time blocked anyway.

/// How far above normal a thread is raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// The USB reader: it only waits for transfers and copies them into the
    /// ring, so it can be given the CPU the moment a transfer lands.
    TimeCritical,
    /// The DSP thread: real work, but bounded by the sample rate.
    Highest,
    /// A helper that only moves data on (the scope's GPU upload).
    AboveNormal,
}

/// Raise the calling thread to `p`. Returns whether the system accepted it;
/// `false` everywhere but Windows.
pub fn raise_current_thread(p: Priority) -> bool {
    imp::raise(p)
}

#[cfg(windows)]
mod imp {
    use super::Priority;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
        THREAD_PRIORITY_TIME_CRITICAL,
    };

    pub(super) fn raise(p: Priority) -> bool {
        let level = match p {
            Priority::TimeCritical => THREAD_PRIORITY_TIME_CRITICAL,
            Priority::Highest => THREAD_PRIORITY_HIGHEST,
            Priority::AboveNormal => THREAD_PRIORITY_ABOVE_NORMAL,
        };
        // SAFETY: GetCurrentThread returns a pseudo-handle for this thread
        // that needs no closing, and SetThreadPriority only reads it.
        unsafe { SetThreadPriority(GetCurrentThread(), level) != 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Priority;

    pub(super) fn raise(_: Priority) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raising_a_thread_never_fails_loudly() {
        let ok = std::thread::spawn(|| raise_current_thread(Priority::AboveNormal)).join().unwrap();
        assert_eq!(ok, cfg!(windows));
    }
}
