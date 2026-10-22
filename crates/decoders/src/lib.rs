//! The panel's own decoders.
//!
//! * [`wsjt`]: FT8, FT4, WSPR, JT65, JT9, FST4 and Q65 from 12 kHz audio in
//!   UTC slots ([`slot`]), by mfsk-core.
//! * [`modem`]: the keyboard and picture modes (RTTY, PSK31, NAVTEX, SSTV,
//!   WEFAX and more), ported from SDRoxide.
//! * [`resample`]: from the `sdr` crate's 48 kHz audio and wide I/Q to the
//!   rates the decoders want.
//! * [`skim`]: the wide skimmer, FT8 or FT4 on every other dial in the I/Q.
//! * [`keyskim`]: the keyboard skimmer, CW, PSK31 and RTTY across the I/Q,
//!   with [`morse`] (CW's timing decoder) and [`callsign`].
//! * [`demo`]: FT8, CW, PSK31 and RTTY stations for the panel's demo band.
pub mod autoid;
pub mod callsign;
pub mod demo;
pub mod freq;
pub mod image;
pub mod keyskim;
pub mod modem;
pub mod morse;
pub mod resample;
pub mod skim;
pub mod slot;
pub mod worker;
pub mod wsjt;

pub use freq::FreqCal;
pub use image::{ImageUpdate, RgbaImage};
pub use worker::{
    ImageCmd, ImageDecoder, ImageEvent, ImageMode, JttyDecoder, JttyEvent, TextDecoder, TextEvent,
    TextMode, WsjtConfig, WsjtDecoder, WsjtSlot,
};
pub use wsjt::{Fst4Period, Q65Mode, WsjtDecode, WsjtMode, WsprSpot};
