//! The panel's own decoders.
//!
//! * [`wsjt`]: FT8, FT4, WSPR, JT65, JT9, FST4 and Q65 from 12 kHz audio in
//!   UTC slots ([`slot`]), by mfsk-core.
//! * [`modem`]: the keyboard and picture modes (RTTY, PSK31, NAVTEX, SSTV,
//!   WEFAX and more), ported from SDRoxide.
//! * [`resample`]: from the `sdr` crate's 48 kHz audio and wide I/Q to the
//!   rates the decoders want.
pub mod modem;
pub mod resample;
pub mod slot;
pub mod wsjt;
