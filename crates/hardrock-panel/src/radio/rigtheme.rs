//! How each radio's window looks: the colours of its case, keys and LCD,
//! the backlights it offers, and which of its settings go in which pop-out.
//! The window's layout and behaviour are the same for every radio; a Yaesu,
//! Kenwood or Icom gets a theme loosely after its own front panel here
//! (AJ3G: "the rig GUI will change based on the rig chosen to match its theme
//! loosly").
//!
//! It is hardware, so it keeps one dark look whatever the panel's theme.

use eframe::egui::Color32;
use rigcat::RigModel;

/// One radio's front panel.
pub(crate) struct RigTheme {
    /// Silk-screened on the case.
    pub brand: &'static str,
    pub model: &'static str,
    /// The case, top and body, and its edge lines.
    pub case_hi: Color32,
    pub case: Color32,
    pub case_line: Color32,
    /// The screen's black surround.
    pub bezel: Color32,
    /// The LCD's segment ink. Unlit segments are this, faint.
    pub lcd_ink: Color32,
    /// How much of the ink an unlit segment shows, 0 to 1.
    pub lcd_ghost: f32,
    /// The backlights the operator can pick, for receive and for transmit.
    pub backlights: &'static [(&'static str, Color32)],
    /// The key caps: top and bottom of their gradient, their lettering, and
    /// the light of one that is on.
    pub cap_top: Color32,
    pub cap_bottom: Color32,
    pub cap_ink: Color32,
    pub cap_lit: Color32,
    pub cap_lit_ink: Color32,
    /// Silk-screen labels and captions.
    pub label: Color32,
    pub dim: Color32,
    /// The readouts' and pop-outs' black.
    pub screen: Color32,
    pub ink: Color32,
    /// Slant of the LCD's seven-segment digits, as a share of their height.
    pub skew: f32,
    /// The settings in each pop-out, by command, in order. `None` puts every
    /// setting of that group there.
    pub rx: Option<&'static [&'static str]>,
    pub cw: Option<&'static [&'static str]>,
    pub tx: Option<&'static [&'static str]>,
}

/// The ELAD FDM-DUO: a segment LCD with an RGB backlight that follows
/// receive and transmit, under black anodised keys.
pub(crate) const ELAD: RigTheme = RigTheme {
    brand: "ELAD",
    model: "FDM-DUO",
    case_hi: Color32::from_rgb(0x2b, 0x2e, 0x33),
    case: Color32::from_rgb(0x1d, 0x1f, 0x22),
    case_line: Color32::from_rgb(0x33, 0x37, 0x3c),
    bezel: Color32::from_rgb(0x0b, 0x0c, 0x0d),
    lcd_ink: Color32::from_rgb(0x0a, 0x0c, 0x12),
    lcd_ghost: 0.12,
    backlights: &[
        ("Blue", Color32::from_rgb(0x4f, 0x97, 0xff)),
        ("Red", Color32::from_rgb(0xff, 0x4a, 0x3d)),
        ("Yellow", Color32::from_rgb(0xff, 0xb0, 0x4a)),
    ],
    cap_top: Color32::from_rgb(0x3a, 0x3e, 0x44),
    cap_bottom: Color32::from_rgb(0x26, 0x29, 0x2d),
    cap_ink: Color32::from_rgb(0xc3, 0xc9, 0xd1),
    cap_lit: Color32::from_rgb(0x4a, 0xa8, 0xff),
    cap_lit_ink: Color32::from_rgb(0x15, 0x17, 0x1a),
    label: Color32::from_rgb(0xc3, 0xc9, 0xd1),
    dim: Color32::from_rgb(0x87, 0x96, 0xa8),
    screen: Color32::from_rgb(0x05, 0x08, 0x0d),
    ink: Color32::from_rgb(0xe9, 0xee, 0xf4),
    skew: 0.105,
    rx: Some(&["VM", "VA", "GC", "GS", "NB", "NK", "NR", "NC", "NO", "SQ", "RF", "MT"]),
    cw: Some(&["CS", "PI", "VT", "IA", "KT", "CI", "CD", "HT", "ET", "CK", "DE", "WT"]),
    tx: Some(&["MG", "CG", "NT", "TB", "FD", "TE", "TT"]),
};

/// The front panel for `model`.
pub(crate) fn rig_theme(model: RigModel) -> &'static RigTheme {
    match model {
        RigModel::FdmDuo => &ELAD,
    }
}

impl RigTheme {
    /// Backlight `i`, or the first where there is no such one.
    pub(crate) fn backlight(&self, i: usize) -> Color32 {
        self.backlights.get(i).or(self.backlights.first()).map_or(Color32::GRAY, |b| b.1)
    }
}
