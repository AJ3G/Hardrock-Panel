//! This program's share of the graphics card, as Windows' Task Manager reads
//! it in its GPU column (AJ3G, 8 Nov 2026: "that should calculate % of GPU
//! this program is utilizing").
//!
//! Windows keeps a performance counter for each engine of each graphics
//! adapter that each process uses: `\GPU Engine(pid_<pid>_..._engtype_<type>)
//! \Utilization Percentage`. Task Manager adds up a process's engines of each
//! type (3D, Compute, Copy, Video…) and shows the busiest type; so does this.
//! A thread reads them once a second through the Performance Data Helper
//! (pdh.dll). Elsewhere than Windows, and if the counters can't be read
//! (an old Windows, a driver without them), there is no figure.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The figure, read by the scope's status line.
pub struct GpuLoad {
    /// Percent × 100, or `u32::MAX` while there is no figure.
    pct: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl GpuLoad {
    /// Start reading, on its own thread.
    pub fn spawn() -> GpuLoad {
        let pct = Arc::new(AtomicU32::new(u32::MAX));
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        {
            let (p, s) = (pct.clone(), stop.clone());
            let _ =
                std::thread::Builder::new().name("gpu-load".into()).spawn(move || pdh::run(&p, &s));
        }
        GpuLoad { pct, stop }
    }

    /// This program's GPU load, percent, when it can be read.
    pub fn percent(&self) -> Option<f32> {
        match self.pct.load(Ordering::Relaxed) {
            u32::MAX => None,
            v => Some(v as f32 / 100.0),
        }
    }
}

impl Drop for GpuLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Task Manager's figure from one reading of every engine: `(instance name,
/// percent)`. The engines of each type are added up, and the busiest type is
/// the process's load.
pub fn busiest_type<'a>(engines: impl IntoIterator<Item = (&'a str, f64)>) -> f64 {
    let mut by_type: Vec<(&str, f64)> = Vec::new();
    for (name, v) in engines {
        let t = name.rsplit_once("engtype_").map_or("", |(_, t)| t);
        match by_type.iter_mut().find(|(k, _)| *k == t) {
            Some((_, sum)) => *sum += v.max(0.0),
            None => by_type.push((t, v.max(0.0))),
        }
    }
    by_type.into_iter().map(|(_, v)| v).fold(0.0, f64::max).min(100.0)
}

#[cfg(windows)]
mod pdh {
    use super::busiest_type;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
    const PDH_MORE_DATA: i32 = 0x8000_07D2u32 as i32;
    const ERROR_SUCCESS: i32 = 0;

    #[repr(C)]
    struct Item {
        name: *const u16,
        status: u32,
        value: f64,
    }

    #[link(name = "pdh")]
    unsafe extern "system" {
        fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut *mut c_void) -> i32;
        fn PdhAddEnglishCounterW(
            query: *mut c_void,
            path: *const u16,
            user: usize,
            counter: *mut *mut c_void,
        ) -> i32;
        fn PdhCollectQueryData(query: *mut c_void) -> i32;
        fn PdhGetFormattedCounterArrayW(
            counter: *mut c_void,
            format: u32,
            size: *mut u32,
            count: *mut u32,
            items: *mut c_void,
        ) -> i32;
        fn PdhCloseQuery(query: *mut c_void) -> i32;
    }

    /// One query on this process's engines.
    struct Query {
        q: *mut c_void,
        c: *mut c_void,
    }

    impl Query {
        fn open() -> Option<Query> {
            let path =
                format!("\\GPU Engine(pid_{}_*)\\Utilization Percentage", std::process::id());
            let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let (mut q, mut c) = (std::ptr::null_mut(), std::ptr::null_mut());
            // SAFETY: plain PDH calls with out-pointers to locals; the query
            // is closed on drop.
            unsafe {
                if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != ERROR_SUCCESS {
                    return None;
                }
                if PdhAddEnglishCounterW(q, wide.as_ptr(), 0, &mut c) != ERROR_SUCCESS {
                    PdhCloseQuery(q);
                    return None;
                }
                PdhCollectQueryData(q);
            }
            Some(Query { q, c })
        }

        /// The percent now: the busiest engine type's sum.
        fn read(&self) -> Option<f64> {
            // SAFETY: the buffer is sized as PDH asks, and the names it
            // points into live in that buffer, read before it is dropped.
            unsafe {
                if PdhCollectQueryData(self.q) != ERROR_SUCCESS {
                    return None;
                }
                let (mut size, mut count) = (0u32, 0u32);
                let r = PdhGetFormattedCounterArrayW(
                    self.c,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    std::ptr::null_mut(),
                );
                if r != PDH_MORE_DATA || size == 0 {
                    // No engines in use yet reads as none.
                    return Some(0.0);
                }
                let mut buf = vec![0u64; (size as usize).div_ceil(8)];
                if PdhGetFormattedCounterArrayW(
                    self.c,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    buf.as_mut_ptr().cast(),
                ) != ERROR_SUCCESS
                {
                    return None;
                }
                let items = std::slice::from_raw_parts(buf.as_ptr().cast::<Item>(), count as usize);
                let named: Vec<(String, f64)> = items
                    .iter()
                    .filter(|i| i.status == 0 && !i.name.is_null())
                    .map(|i| {
                        let mut n = 0;
                        while *i.name.add(n) != 0 {
                            n += 1;
                        }
                        let s = String::from_utf16_lossy(std::slice::from_raw_parts(i.name, n));
                        (s, i.value)
                    })
                    .collect();
                Some(busiest_type(named.iter().map(|(n, v)| (n.as_str(), *v))))
            }
        }
    }

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: opened in `open`, closed once.
            unsafe {
                PdhCloseQuery(self.q);
            }
        }
    }

    /// Read once a second until stopped. The query is made again every 30 s,
    /// so engines the process starts using later are counted.
    pub(super) fn run(pct: &AtomicU32, stop: &AtomicBool) {
        let mut query: Option<(Query, Instant)> = None;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            if query.as_ref().is_none_or(|(_, t)| t.elapsed() > Duration::from_secs(30)) {
                query = Query::open().map(|q| (q, Instant::now()));
                continue;
            }
            let v = query.as_ref().and_then(|(q, _)| q.read());
            pct.store(v.map_or(u32::MAX, |v| (v * 100.0).round() as u32), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busiest_engine_type_is_the_load_as_task_manager_shows() {
        let p = "pid_1234_luid_0x00000000_0x0000D1C5_phys_0_eng_";
        let engines = [
            (format!("{p}0_engtype_3D"), 12.0),
            (format!("{p}1_engtype_3D"), 5.5),
            (format!("{p}2_engtype_Copy"), 3.0),
            (format!("{p}3_engtype_Compute_0"), 9.0),
        ];
        let v = busiest_type(engines.iter().map(|(n, v)| (n.as_str(), *v)));
        assert!((v - 17.5).abs() < 1e-9);
        assert_eq!(busiest_type(std::iter::empty()), 0.0);
    }

    #[test]
    fn off_windows_there_is_no_figure() {
        if !cfg!(windows) {
            assert_eq!(GpuLoad::spawn().percent(), None);
        }
    }
}
