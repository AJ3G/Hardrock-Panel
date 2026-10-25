// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-elad/src/lib.rs, GPL-3.0-or-later. See NOTICE.md.

//! Native ELAD FDM-DUO / FDM-S2 / FDM-S1 driver.
//!
//! Pure Rust over `nusb`: no libusb, no gr-elad, no SoapySDR module, so this
//! backend is in every build variant on every platform.
//!
//! # Shape
//!
//! One blocking thread owns the device ([`stream`]), control reaches it over a
//! crossbeam channel, and samples come back through an `rtrb` ring of
//! interleaved `f32` — the same shape as SDRoxide's AirspyHF and
//! RTL-SDR backends. [`protocol`] holds the wire with no `nusb` in it, which
//! is what makes the fiddly half of this driver testable without hardware.
//!
//! # What the hardware is
//!
//! All three devices are direct-sampling receivers — a 122.88 MHz ADC (61.44 on
//! the FDM-S1) behind a switchable low-pass bank and a 12 dB pad — with an FPGA
//! digital down-converter delivering one complex channel over a bulk endpoint.
//! Tuning is therefore not a synthesiser but a 32-bit phase increment written
//! into that DDC, which is why an FDM-S2 can hear above its own clock: the
//! modulo in [`protocol::tuning_word`] is an alias, on purpose.
//!
//! The FDM-DUO is a transceiver wrapped around the same receiver, and it is
//! **three USB devices**: this one, an FTDI bridge carrying its Kenwood-derived
//! CAT protocol, and a C-Media sound card carrying demodulated receive audio
//! and transmit audio. This crate drives only the first. Nothing here
//! transmits, on any model.
//!
//! # The sample rate is an FPGA image, not a register
//!
//! The DDC delivers 192, 384, 768, 1536, 3072 or 6144 kHz and **no request in
//! the vendor protocol selects between them** — because each rate is a
//! different FPGA image. On an FDM-S1 or FDM-S2 the host loads that image at
//! every power-up with ELAD's own `elad-firmware` loader, which is also why an
//! untouched sampler answers every command and streams nothing at all: see
//! [`fpga`], the whole of which exists for issue #178. So on a sampler
//! `EladConfig::sample_rate_hz` really is a command, issued once per open.
//!
//! On the FDM-DUO the radio boots its own stand-alone image at 192 kHz, and a
//! wider rate is a remote image the host uploads over USB and holds with
//! `IQ1;` — see [`remote`], decoded from a capture of SDR Console doing it. The
//! driver still measures the throughput a couple of seconds in and says so if
//! it disagrees with the setting (see `handle::RxStats::check_rate`).
//!
//! # Provenance
//!
//! The vendor protocol — device ids, the control transfers, the tuning
//! arithmetic, the EEPROM calibration map and the sample scaling — is
//! transcribed from ELAD's own [gr-elad](https://github.com/ELADIT/gr-elad),
//! `lib/fdm_source_c_impl.cc` (GPL-3.0-or-later, the same licence as this
//! workspace). No code is copied; the constants and the sequence are what
//! matter and both are cited where they are used. The FDM-DUO's CAT commands
//! come from the *ELAD FDM-DUO User Manual* v2.6, §6.
//!
//! The FPGA load in [`fpga`] is the one thing `gr-elad` does not do at all — it
//! assumes something else already has — and comes instead from ELAD's Linux
//! loader and from [SoapyELAD](https://github.com/DisagioDigitale/SoapyELAD),
//! which drives an FDM-S2 on Linux and is verified against one.
//!
//! # Mostly not verified against hardware
//!
//! This driver was written from that reference, not on a bench. Receiving on an
//! FDM-DUO has since been run on one (hardware 2.9, firmware 4.9, at 192 kHz):
//! the I/Q order was confirmed by retuning against broadcast carriers, and the
//! tuning word against the calibrated clock and the dial read-back are pinned by
//! tests built from the radio's own bytes. Transmit, the higher
//! rates and the FDM-S1/S2 have not. Every uncertain point carries a comment
//! saying so, and [`diagnostics`] renders a session trace built for exactly one
//! purpose: making a first bug report from an owner actionable.

pub mod convert;
pub mod device;
pub mod error;
pub mod fpga;
pub mod handle;
pub mod priority;
pub mod protocol;
pub mod remote;
pub mod stream;
pub mod trace;
pub mod types;
pub mod usb;

pub use device::Device;
pub use error::{Error, Result};
pub use handle::EladHandle;
pub use protocol::Model;
pub use remote::set_transmitting;
pub use trace::{FIELD_REPORT_HINT, diagnostics, note_dial, note_retune};
pub use types::{
    Complex32, ELAD_ATTENUATOR_DB, ELAD_DEFAULT_RATE_HZ, ELAD_SAMPLE_RATES, EladConfig, EladDevice,
};
pub use usb::list;
