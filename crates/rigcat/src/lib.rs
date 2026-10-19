//! A radio's CAT port, for the panel: what the radio measures of its own
//! transmission, its dial and its settings, its transmit power, and its TUNE
//! carrier or an ordinary transmission of the panel's tone.
//!
//! [`Protocol`] is what each radio's dialect provides; [`elad`] is the
//! FDM-DUO's, the only one so far. Another radio is another module and
//! another [`RigModel`]. [`RigLink`] runs the port on a thread of its own.
//! What it may send, and when, is safety interlocks #5 and #6.

pub mod elad;
mod link;
pub mod protocol;

pub use link::RigLink;
pub use protocol::{Frame, FrameKind, Item, PowerStep, Protocol, Reply};

use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RigModel {
    #[default]
    FdmDuo,
}

impl RigModel {
    pub const ALL: [RigModel; 1] = [RigModel::FdmDuo];

    pub fn label(self) -> &'static str {
        match self {
            RigModel::FdmDuo => "ELAD FDM-DUO",
        }
    }

    /// The baud rates the radio's CAT port can be set to.
    pub fn bauds(self) -> &'static [u32] {
        match self {
            RigModel::FdmDuo => &elad::BAUDS,
        }
    }

    pub fn default_baud(self) -> u32 {
        match self {
            RigModel::FdmDuo => elad::DEFAULT_BAUD,
        }
    }

    pub fn protocol(self) -> Box<dyn Protocol> {
        match self {
            RigModel::FdmDuo => Box::new(elad::FdmDuo::default()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RigConfig {
    pub enabled: bool,
    pub model: RigModel,
    /// COM3, for example.
    pub path: String,
    pub baud: u32,
    /// Send the radio's frequency to the amplifier, so its band follows the
    /// radio between transmissions (interlock #3).
    pub follow_band: bool,
}

impl Default for RigConfig {
    fn default() -> Self {
        RigConfig {
            enabled: false,
            model: RigModel::FdmDuo,
            path: String::new(),
            baud: RigModel::FdmDuo.default_baud(),
            // As in SDRoxide.
            follow_band: true,
        }
    }
}

/// A power reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Watts {
    pub w: f32,
    /// False when the radio flags it as not to be trusted.
    pub valid: bool,
}

/// An SWR reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swr {
    pub ratio: f32,
    pub valid: bool,
    /// The radio dropped back to receive on a high SWR.
    pub tripped: bool,
}

/// One setting as the radio last answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingRow {
    pub item: Item,
    /// The answer, in words; `None` if it didn't parse.
    pub value: Option<String>,
    /// The answer as it came. `None` if none came; `?` if the radio refused
    /// the command.
    pub raw: Option<String>,
}

/// How TUNE keys the radio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keying {
    /// An ordinary transmission (`TX;` on the FDM-DUO) of the panel's tone at
    /// the Drive level, through the radio's USB audio. It keys the radio's
    /// PTT output to the amplifier, as SDRoxide's TUNE did.
    #[default]
    Audio,
    /// The radio's own TUNE carrier (`TX2;`), at its power setting.
    Carrier,
}

/// A TUNE in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    pub since: Instant,
    /// When it unkeys by itself.
    pub until: Instant,
    pub keying: Keying,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RigStatus {
    pub configured: bool,
    pub connected: bool,
    pub error: Option<String>,
    pub model: RigModel,
    pub baud: u32,
    pub freq_hz: Option<u64>,
    pub mode: Option<&'static str>,
    pub fwd: Option<Watts>,
    pub rfl: Option<Watts>,
    pub swr: Option<Swr>,
    /// The radio's transmit power setting (menu 34 on the FDM-DUO), as its
    /// own code; see `power_steps`.
    pub power_code: Option<u8>,
    pub power_steps: &'static [PowerStep],
    pub power_name: &'static str,
    /// Where the radio takes its transmit audio from (menu 32 on the
    /// FDM-DUO), in words.
    pub tx_input: Option<String>,
    /// See [`Protocol::audio_ceiling`].
    pub audio_ceiling: f32,
    pub tuning: Option<Tuning>,
    /// Why the last TUNE ended, when it wasn't the operator.
    pub tune_note: Option<String>,
    pub settings: Vec<SettingRow>,
    pub settings_read: Option<Instant>,
}

impl RigStatus {
    /// The radio says it is putting out power it trusts: the surest sign it
    /// is transmitting. Below 0.5 W, and in a fold-back, the FDM-DUO flags
    /// its reading and this is false, so the panel also looks at the
    /// amplifier's DRV.
    pub fn keyed(&self) -> bool {
        self.fwd.is_some_and(|p| p.valid && p.w > 0.0)
    }

    /// Forward watts, where the radio trusts them.
    pub fn fwd_w(&self) -> Option<f32> {
        self.fwd.filter(|p| p.valid).map(|p| p.w)
    }

    /// Reflected watts, where the radio trusts them.
    pub fn rfl_w(&self) -> Option<f32> {
        self.rfl.filter(|p| p.valid).map(|p| p.w)
    }

    pub fn swr_ratio(&self) -> Option<f32> {
        self.swr.filter(|s| s.valid).map(|s| s.ratio)
    }

    /// The power step the radio is on.
    pub fn power(&self) -> Option<&'static PowerStep> {
        let code = self.power_code?;
        self.power_steps.iter().find(|p| p.code == code)
    }
}
