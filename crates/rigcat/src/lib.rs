//! A radio's CAT port, read only, for the panel to show what the radio itself
//! measures of its transmission beside the amplifier's readings.
//!
//! Only the ELAD FDM-DUO for now ([`elad`]). [`RigLink`] opens the port on a
//! thread of its own, reads forward and reflected power and SWR several times
//! a second, the dial once a second, and the radio's settings when asked. It
//! never keys the radio or changes a setting (safety interlock #5).

pub mod elad;
mod link;

pub use link::RigLink;

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
            baud: elad::DEFAULT_BAUD,
            // As in SDRoxide.
            follow_band: true,
        }
    }
}

/// One setting as the radio last answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingRow {
    pub item: elad::Item,
    /// The answer, in words; `None` if it didn't parse.
    pub value: Option<String>,
    /// The answer as it came, without the `;`. `None` if none came; `?` if the
    /// radio refused the command.
    pub raw: Option<String>,
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
    pub fwd: Option<elad::Watts>,
    pub rfl: Option<elad::Watts>,
    pub swr: Option<elad::Swr>,
    /// Menu 34 `TX POWER`, as last read.
    pub tx_power: Option<String>,
    pub settings: Vec<SettingRow>,
    pub settings_read: Option<Instant>,
}

impl RigStatus {
    /// The radio says it is putting out power it trusts: the surest sign it
    /// is transmitting. Below 0.5 W, and in a fold-back, it flags its reading
    /// and this is false, so the panel also looks at the amplifier's DRV.
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
}
