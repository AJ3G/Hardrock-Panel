// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-types/src/radio.rs — the ELAD-specific pieces only
// (`EladDevice`, `EladConfig`, `ELAD_SAMPLE_RATES`, …), so this driver does not
// pull in SDRoxide's whole configuration crate. GPL-3.0-or-later.

//! The configuration and enumeration types the driver is opened with.

/// One complex sample, `I` in `re` and `Q` in `im`.
pub type Complex32 = num_complex::Complex<f32>;

/// One ELAD receiver from a USB enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EladDevice {
    /// The serial number from the device's EEPROM, when one could be read.
    /// ELAD's devices carry no USB `iSerial`, so this is `None` from an
    /// enumeration.
    pub serial: Option<String>,
    /// Which model, from the USB product id.
    pub name: String,
    /// The USB product id.
    pub pid: u16,
    /// The address on the bus (`bus-port.port…`).
    pub path: String,
}

impl EladDevice {
    /// One-line label for a selection list.
    pub fn label(&self) -> String {
        match &self.serial {
            Some(s) => format!("{}  (serial {s})", self.name),
            None => format!("{}  (at {})", self.name, self.path),
        }
    }
}

/// The complex sample rates an ELAD DDC delivers, in Hz.
///
/// Each rate is a different FPGA image. See [`EladConfig::sample_rate_hz`].
pub const ELAD_SAMPLE_RATES: [u32; 6] =
    [192_000, 384_000, 768_000, 1_536_000, 3_072_000, 6_144_000];

/// The rate assumed until told otherwise: `gr-elad`'s default, and the one the
/// FDM-DUO's specification quotes for its I/Q channel.
pub const ELAD_DEFAULT_RATE_HZ: u32 = 192_000;

/// The attenuator's depth in dB. One pad, in or out.
pub const ELAD_ATTENUATOR_DB: f64 = 12.0;

/// How to open an ELAD receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EladConfig {
    /// Pin a device by its EEPROM serial; empty means "the first one found".
    pub serial: String,
    /// The DDC's rate in Hz — one of [`ELAD_SAMPLE_RATES`].
    ///
    /// On an FDM-S1/S2 this selects the FPGA image to load. On an FDM-DUO,
    /// above 192 kHz it uploads the remote image for the rate when one is known
    /// ([`crate::remote`]), and otherwise falls back to 192 kHz; the driver
    /// measures the throughput once running and warns if it disagrees.
    pub sample_rate_hz: u32,
    /// The 12 dB input attenuator.
    pub attenuator: bool,
    /// Whether the pre-selection filters are in circuit.
    pub preselector: bool,
}

impl Default for EladConfig {
    fn default() -> Self {
        EladConfig {
            serial: String::new(),
            sample_rate_hz: ELAD_DEFAULT_RATE_HZ,
            attenuator: false,
            preselector: true,
        }
    }
}
