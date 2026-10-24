//! The keyboard and picture modems, ported from SDRoxide's DSP crate. Each is
//! a streaming receiver over mono audio at whatever rate it is built for,
//! with the matching transmitter beside it (used here by the tests, which
//! send a known message through and read it back).

pub mod cw;
pub mod fec;
pub mod hell;
pub mod hell_font;
pub mod mfsk;
pub mod navtex;
pub mod olivia;
pub mod psk;
pub mod rtty;
pub mod sstv;
pub mod sstv_slant;
pub mod thor;
pub mod types;
pub mod wefax;
