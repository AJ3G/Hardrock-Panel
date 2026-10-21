//! DECODE's running log of every WSJT decode, in WSJT-X's `ALL.TXT` layout,
//! in a file of its own (`ALL.TXT`) beside the panel's saved settings:
//!
//! ```text
//! 261021_011530    14.074 Rx FT8    -12  0.2 1234 CQ K1ABC FN42
//! ```
//!
//! The slot's UTC start (`yyMMdd_hhmmss`), the dial in MHz (through the
//! transverter), `Rx`, the mode, SNR, DT, audio Hz and the message. It is only
//! ever appended to, a slot at a time, and is kept apart from the panel's
//! general log.

use std::io::Write as _;
use std::path::PathBuf;

/// The name the settings are kept under (`eframe::run_native`'s app name),
/// which picks their folder.
const APP_ID: &str = "Hardrock Panel";
const FILE: &str = "ALL.TXT";

/// The folder the panel's settings are kept in: on Windows
/// `%APPDATA%\Hardrock Panel\data`, on Linux `~/.local/share/hardrockpanel`,
/// on macOS `~/Library/Application Support/Hardrock-Panel`.
pub(crate) fn settings_dir() -> Option<PathBuf> {
    eframe::storage_dir(APP_ID)
}

/// UTC Unix seconds as (year, month, day, hour, minute, second).
// Howard Hinnant's civil_from_days, as SDRoxide's sdroxide-net/src/wsprnet.rs
// (https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later) writes it.
pub(crate) fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32, d as u32, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32)
}

/// One decode as an `ALL.TXT` line (no newline).
pub(crate) fn all_txt_line(
    slot_utc: i64,
    dial_hz: f64,
    mode: &str,
    snr_db: f32,
    dt_s: f32,
    audio_hz: f32,
    message: &str,
) -> String {
    let (y, mo, d, h, mi, s) = civil(slot_utc);
    format!(
        "{:02}{mo:02}{d:02}_{h:02}{mi:02}{s:02}{:>10.3} Rx {mode:<6}{:>4}{:>5.1}{:>5} {message}",
        y.rem_euclid(100),
        dial_hz / 1e6,
        snr_db.round() as i32,
        dt_s,
        audio_hz.round() as i32,
    )
}

/// The decode log, opened for each slot's lines.
#[derive(Default)]
pub(crate) struct DecodeLog {
    /// Why the last write failed, until one works.
    pub error: Option<String>,
    path: Option<PathBuf>,
}

impl DecodeLog {
    /// Where the log is (or would be).
    pub(crate) fn path(&mut self) -> Option<&PathBuf> {
        if self.path.is_none() {
            self.path = settings_dir().map(|d| d.join(FILE));
        }
        self.path.as_ref()
    }

    /// Add one slot's lines.
    pub(crate) fn append(&mut self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let Some(path) = self.path().cloned() else {
            self.error = Some("No folder for the decode log on this system".into());
            return;
        };
        let r = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            let mut text = String::new();
            for l in lines {
                text.push_str(l);
                text.push('\n');
            }
            f.write_all(text.as_bytes())
        })();
        self.error = r.err().map(|e| format!("Decode log {}: {e}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_read_as_the_civil_date() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        // 2000-02-29 23:59:59, a leap day in a century leap year.
        assert_eq!(civil(951_868_799), (2000, 2, 29, 23, 59, 59));
        // 2026-10-21 01:15:30.
        assert_eq!(civil(1_792_545_330), (2026, 10, 21, 1, 15, 30));
        assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn a_decode_is_written_as_wsjtx_writes_it() {
        let l =
            all_txt_line(1_792_545_330, 14_074_000.0, "FT8", -12.4, 0.2, 1234.0, "CQ K1ABC FN42");
        assert_eq!(l, "261021_011530    14.074 Rx FT8    -12  0.2 1234 CQ K1ABC FN42");
        let l =
            all_txt_line(1_792_545_330, 7_038_600.0, "WSPR", 3.0, -1.0, 1490.0, "K1ABC FN42 37");
        assert_eq!(l, "261021_011530     7.039 Rx WSPR     3 -1.0 1490 K1ABC FN42 37");
    }

    #[test]
    fn lines_are_appended_to_the_file() {
        let dir = std::env::temp_dir().join(format!("hrp-decodelog-{}", std::process::id()));
        let mut log = DecodeLog { error: None, path: Some(dir.join(FILE)) };
        log.append(&["one".into()]);
        log.append(&["two".into(), "three".into()]);
        assert_eq!(log.error, None);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(text, "one\ntwo\nthree\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
