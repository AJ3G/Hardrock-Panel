// The AGC modes and the manual-gain ceiling are ported from SDRoxide
// (https://github.com/dividebysandwich/sdroxide), crates/sdroxide-types/src/mode.rs
// and src/state.rs, GPL-3.0-or-later. `Mode` is this crate's own, cut down to
// what the panel's receiver offers. See NOTICE.md.

//! The small shared types the DSP is configured with.

/// One complex sample, `I` in `re` and `Q` in `im`.
pub type Complex32 = num_complex::Complex<f32>;

/// The highest manual gain the AGC accepts while switched off, in dB.
pub const MAX_MANUAL_GAIN_DB: f32 = 120.0;

/// Receiver AGC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgcMode {
    /// A fixed gain: [`crate::Receiver::set_gain_db`].
    Off,
    Slow,
    #[default]
    Med,
    Fast,
}

impl AgcMode {
    pub const ALL: [AgcMode; 4] = [AgcMode::Off, AgcMode::Slow, AgcMode::Med, AgcMode::Fast];

    pub fn label(self) -> &'static str {
        match self {
            AgcMode::Off => "Off",
            AgcMode::Slow => "Slow",
            AgcMode::Med => "Med",
            AgcMode::Fast => "Fast",
        }
    }

    /// Hang time in milliseconds; `None` means AGC disabled.
    pub fn hang_ms(self) -> Option<f32> {
        match self {
            AgcMode::Off => None,
            AgcMode::Slow => Some(1000.0),
            AgcMode::Med => Some(500.0),
            AgcMode::Fast => Some(100.0),
        }
    }
}

/// What the software receiver demodulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Mode {
    Lsb,
    #[default]
    Usb,
    /// CW on the upper side: the passband sits above the dial, around the
    /// pitch.
    Cw,
    /// CW reversed: the passband below the dial.
    Cwr,
    Am,
    /// Narrow FM, ±5 kHz deviation.
    Fm,
}

impl Mode {
    pub const ALL: [Mode; 6] = [Mode::Lsb, Mode::Usb, Mode::Cw, Mode::Cwr, Mode::Am, Mode::Fm];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb => "USB",
            Mode::Cw => "CW",
            Mode::Cwr => "CWR",
            Mode::Am => "AM",
            Mode::Fm => "FM",
        }
    }

    /// The passband a mode opens with, `[low, high]` in Hz relative to the dial
    /// (negative is below it), as SDR Console draws it.
    pub fn default_passband(self) -> (f32, f32) {
        match self {
            Mode::Usb => (100.0, 2700.0),
            Mode::Lsb => (-2700.0, -100.0),
            Mode::Cw => (350.0, 850.0),
            Mode::Cwr => (-850.0, -350.0),
            Mode::Am => (-4500.0, 4500.0),
            Mode::Fm => (-6000.0, 6000.0),
        }
    }

    /// Whether the wanted sideband is below the dial.
    pub fn is_lower(self) -> bool {
        matches!(self, Mode::Lsb | Mode::Cwr)
    }

    /// Whether this is a single-sideband (product-detected) mode.
    pub fn is_sideband(self) -> bool {
        matches!(self, Mode::Lsb | Mode::Usb | Mode::Cw | Mode::Cwr)
    }
}
