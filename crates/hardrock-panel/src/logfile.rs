//! The panel's log, to a file beside its saved settings as well as to
//! stderr. The Windows release build has no console, so without the file a
//! message that says "see the log" would point at nothing.
//!
//! The file is kept small: once it passes [`CAP`] it is moved to
//! `hardrock-panel.log.1` (replacing the one before) and a new one is begun,
//! so at most two caps' worth is ever on disk. A panic anywhere is written to
//! it too, with its thread and place.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The log's name in its folder.
pub const FILE_NAME: &str = "hardrock-panel.log";
/// How big the log may grow before it is moved aside, bytes.
pub const CAP: u64 = 2 * 1024 * 1024;
/// How much of the end of the log COPY LOG copies, bytes.
pub const TAIL: u64 = 64 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// Where the log is being written, when it could be opened.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// "the log at <path>", or what to say when there is no log file.
pub fn where_it_is() -> String {
    match path() {
        Some(p) => format!("the log is at {}", p.display()),
        None => "no log file could be opened".to_string(),
    }
}

/// The last [`TAIL`] bytes of the log (from a line's start), for a bug
/// report, with its path on the first line.
pub fn tail() -> Option<String> {
    let p = path()?;
    let mut f = File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if from > 0 { text.split_once('\n').map_or(&*text, |(_, r)| r) } else { &text };
    Some(format!("Hardrock Panel log ({})\n{text}", p.display()))
}

/// A file that moves itself aside to `<name>.1` when it passes `cap`.
pub struct CappedFile {
    path: PathBuf,
    file: File,
    written: u64,
    cap: u64,
}

impl CappedFile {
    /// Open `path` for appending, moving it aside first if it is already
    /// over `cap`.
    pub fn open(path: PathBuf, cap: u64) -> io::Result<CappedFile> {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size >= cap {
            let _ = std::fs::rename(&path, aside(&path));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(CappedFile { path, file, written, cap })
    }

    fn roll(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let _ = std::fs::rename(&self.path, aside(&self.path));
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

/// The name the full log is moved to.
fn aside(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

impl Write for CappedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.cap {
            self.roll()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Start logging: to stderr, and to [`FILE_NAME`] in `dir` (made if need
/// be) when it can be opened. `RUST_LOG` sets the level, `info` by default.
pub fn init(dir: Option<PathBuf>) {
    let filter =
        || tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file = dir.and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        let p = d.join(FILE_NAME);
        let f = CappedFile::open(p.clone(), CAP).ok()?;
        let _ = PATH.set(p);
        Some(f)
    });
    let stderr = tracing_subscriber::fmt::layer().with_writer(io::stderr);
    let to_file =
        file.map(|f| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(Mutex::new(f)));
    let _ = tracing_subscriber::registry().with(filter()).with(stderr).with(to_file).try_init();
    if let Some(p) = path() {
        tracing::info!("Hardrock Panel {}, logging to {}", env!("CARGO_PKG_VERSION"), p.display());
    }
    // A panic goes to the log too (the default hook writes only to stderr).
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let at = info.location().map(|l| l.to_string()).unwrap_or_default();
        tracing::error!("panic in thread {name} at {at}: {}", panic_text(info.payload()));
        before(info);
    }));
}

/// A panic's message from its payload (what `panic!` was given), or a
/// placeholder when it was not text.
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic with no message".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_log_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("hrp-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.log");
        let mut f = CappedFile::open(p.clone(), 100).unwrap();
        f.write_all(&[b'a'; 80]).unwrap();
        f.write_all(&[b'b'; 40]).unwrap();
        f.flush().unwrap();
        assert_eq!(std::fs::read(aside(&p)).unwrap(), vec![b'a'; 80]);
        assert_eq!(std::fs::read(&p).unwrap(), vec![b'b'; 40]);
        drop(f);
        // Opened again over the cap, it starts afresh.
        std::fs::write(&p, [b'c'; 120]).unwrap();
        let f = CappedFile::open(p.clone(), 100).unwrap();
        assert_eq!(f.written, 0);
        assert_eq!(std::fs::read(aside(&p)).unwrap().len(), 120);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panics_text_is_read_from_its_payload() {
        let r = std::panic::catch_unwind(|| panic!("SCF sizes {}", 4));
        assert_eq!(panic_text(&*r.unwrap_err()), "SCF sizes 4");
        let r = std::panic::catch_unwind(|| panic!("plain"));
        assert_eq!(panic_text(&*r.unwrap_err()), "plain");
        let r = std::panic::catch_unwind(|| std::panic::panic_any(7u8));
        assert_eq!(panic_text(&*r.unwrap_err()), "a panic with no message");
    }
}
