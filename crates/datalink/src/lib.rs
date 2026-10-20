//! Aviation and maritime data links from the radio's I/Q.
//!
//! The decode cores are airframesio/xng's (MIT OR Apache-2.0, vendored under
//! `vendor/xng`); this crate fits them to the panel: it takes the DUO's
//! interleaved I/Q at whatever rate it runs (192 kHz to 6.144 MHz), splits
//! out every wanted channel at once, runs the channels' decoders in parallel
//! on worker threads, and reports each message at its true radio frequency.
//!
//! | Link | Channel rate | Fits the DUO's I/Q rates |
//! |---|---|---|
//! | HFDL | 12 kS/s per channel | all; a 24 kS/s lane too |
//! | VHF ACARS | 24 kS/s per channel | all, if the channels lie inside the span |
//! | VDL Mode 2 | 50 kS/s per channel | all, if the channels lie inside the span |
//! | AIS | 48 kS/s per channel | all (the two channels are 50 kHz apart) |
//! | Mode S / ADS-B | the I/Q rate, whole | 3.072 and 6.144 MHz only |
//!
//! Mode S's pulses are half a microsecond long, so it wants at least 2 MS/s;
//! xng's demodulator takes a fractional number of samples a microsecond, so
//! the DUO's 3.072 and 6.144 MHz rates go in as they are. Below that the I/Q
//! is too narrow for it, and [`LinkConfig::check`] says so.
//!
//! On VHF and up the DUO sits behind a converter, so every frequency here is
//! true RF: the I/Q centre the radio reports, corrected by the reference's
//! ppm (0 for AJ3G's GPS-locked radio), plus the converter's local oscillator
//! ([`Tuning`]). Every message is stamped in UTC from the system clock.

pub mod hfdl_table;
pub mod link;
pub mod worker;

pub use decoders::FreqCal;
pub use hfdl_table::{HFDL_STATIONS, HfdlStation, HfdlTable};
pub use link::{Decoded, LinkConfig, LinkDecoder, LinkMode, Tuning};
pub use worker::{LinkEvent, LinkWorker};
/// xng's message model, which every [`Decoded`] carries in full.
pub use xng_types::{Message, MessageBody};
