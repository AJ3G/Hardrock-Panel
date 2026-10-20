//! How the AMP window looks beside a given radio. With no radio, or one with
//! no look of its own, it keeps the panel's own chrome. With an ELAD radio
//! it is drawn as the screen of ELAD's DUO-ART 120 amplifier: a colour TFT
//! behind a black bezel, in a black case, with six soft keys under it, so the
//! two windows look like one maker's station (AJ3G: "the rig GUI will change
//! based on the rig chosen to match its theme loosly").
//!
//! Only the colours and the way things are drawn change. Every reading,
//! picker and control is the same in every look, and so is every interlock
//! (see `keys` in `panel.rs`).
//!
//! It is hardware, so like the radio's face it keeps one dark look whatever
//! the panel's theme.

use eframe::egui::Color32;

use crate::theme::StationLook;

/// A radio maker's look for the AMP window.
#[derive(Debug)]
pub(crate) struct AmpTheme {
    /// The case: top and body, and its edge line.
    pub case_hi: Color32,
    pub case: Color32,
    pub case_line: Color32,
    /// The black surround of the screen.
    pub bezel: Color32,
    /// The screen's black, and the lines that divide it into bands.
    pub screen: Color32,
    pub screen_line: Color32,
    /// Text on the screen, and its secondary text.
    pub ink: Color32,
    pub ink_dim: Color32,
    /// The status line's blue and the diagnostic line's green. Blue is also
    /// every neutral state; green is in line, tuned and ready.
    pub status: Color32,
    pub diag: Color32,
    /// The second (inner) needle and its scale, on the screen.
    pub inner: Color32,
    /// Warnings and alarms, and nothing else.
    pub warn: Color32,
    pub alarm: Color32,
    /// The analog meters' white face, its outer scale and needle, its inner
    /// scale and needle, and its small print.
    pub face: Color32,
    pub face_ink: Color32,
    pub face_inner: Color32,
    pub face_dim: Color32,
    /// Silk-screen lettering on the case.
    pub label: Color32,
    /// The soft keys: top and bottom of their gradient, and their edge.
    pub key_top: Color32,
    pub key_bottom: Color32,
    pub key_edge: Color32,
}

/// ELAD's DUO-ART 120: a 5" colour TFT with a blue status line and a green
/// diagnostic line, white analog-style meters, and six soft keys. ELAD shows
/// a selected setting in red; here it is blue, since red only warns.
pub(crate) static ELAD: AmpTheme = AmpTheme {
    case_hi: Color32::from_rgb(0x2b, 0x2e, 0x33),
    case: Color32::from_rgb(0x1d, 0x1f, 0x22),
    case_line: Color32::from_rgb(0x33, 0x37, 0x3c),
    bezel: Color32::from_rgb(0x0b, 0x0c, 0x0d),
    screen: Color32::from_rgb(0x05, 0x08, 0x0d),
    screen_line: Color32::from_rgb(0x1b, 0x2a, 0x3d),
    ink: Color32::from_rgb(0xe9, 0xee, 0xf4),
    ink_dim: Color32::from_rgb(0x87, 0x96, 0xa8),
    status: Color32::from_rgb(0x4a, 0xa8, 0xff),
    diag: Color32::from_rgb(0x41, 0xd1, 0x7a),
    inner: Color32::from_rgb(0x3f, 0xd0, 0xe8),
    warn: Color32::from_rgb(0xff, 0xc2, 0x3d),
    alarm: Color32::from_rgb(0xff, 0x4b, 0x3e),
    face: Color32::from_rgb(0xf6, 0xf4, 0xee),
    face_ink: Color32::from_rgb(0x15, 0x18, 0x1c),
    face_inner: Color32::from_rgb(0x0b, 0x63, 0xc4),
    face_dim: Color32::from_rgb(0x5d, 0x66, 0x70),
    label: Color32::from_rgb(0xc3, 0xc9, 0xd1),
    key_top: Color32::from_rgb(0x3a, 0x3e, 0x44),
    key_bottom: Color32::from_rgb(0x26, 0x29, 0x2d),
    key_edge: Color32::from_rgb(0x11, 0x11, 0x11),
};

/// The look for the panel's whole look (see [`StationLook::for_radio`]):
/// ELAD's beside the FDM-DUO, the panel's own (`None`) otherwise.
pub(crate) fn amp_theme(look: StationLook) -> Option<&'static AmpTheme> {
    match look {
        StationLook::Elad => Some(&ELAD),
        StationLook::Panel => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigcat::{RigConfig, RigModel};

    #[test]
    fn the_duo_gets_elads_look_and_no_radio_keeps_the_panels_own() {
        let mut rig = RigConfig::default();
        assert!(amp_theme(StationLook::for_radio(&rig, None)).is_none());
        rig.enabled = true;
        rig.model = RigModel::FdmDuo;
        assert!(std::ptr::eq(amp_theme(StationLook::for_radio(&rig, None)).unwrap(), &ELAD));
        // The command line's --theme wins either way.
        assert!(amp_theme(StationLook::for_radio(&rig, Some(StationLook::Panel))).is_none());
        let none = RigConfig::default();
        assert!(amp_theme(StationLook::for_radio(&none, Some(StationLook::Elad))).is_some());
    }
}
