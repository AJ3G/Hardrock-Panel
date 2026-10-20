//! The look settings the theme is written in terms of, carried over from
//! SDRoxide's `sdroxide-types` so the panel looks the same as it does there.

use serde::{Deserialize, Serialize};

/// A usage class on the band-plan strip painted along the bottom of the
/// waterfall — what an allocation is *for*, which is the only thing the strip
/// colours by.
///
/// Here rather than in the widget that draws it because the operator can
/// retint every class, and the picked colours are kept in
/// [`UiSettings::bandplan_colors`], indexed by [`BandplanKind::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandplanKind {
    /// Amateur band as a whole, drawn when zoomed out past its sub-segments.
    Ham,
    /// CW sub-segment of an amateur band.
    Cw,
    /// Digital-mode sub-segment.
    Digi,
    /// Voice sub-segment.
    Phone,
    /// Beacon sub-segment.
    Beacon,
    /// Shortwave/tropical broadcasting allocation.
    Broadcast,
    /// Longwave and mediumwave AM broadcasting.
    Am,
    /// Citizens' band.
    Cb,
}

impl BandplanKind {
    /// Every class, in the order the settings colour pickers and
    /// [`BandplanKind::index`] use. Anything indexed by class —
    /// [`UiSettings::bandplan_colors`] — is this wide.
    pub const ALL: [BandplanKind; 8] = [
        BandplanKind::Ham,
        BandplanKind::Cw,
        BandplanKind::Digi,
        BandplanKind::Phone,
        BandplanKind::Beacon,
        BandplanKind::Broadcast,
        BandplanKind::Am,
        BandplanKind::Cb,
    ];

    /// How many classes there are, i.e. the width of every per-class array.
    pub const COUNT: usize = BandplanKind::ALL.len();

    /// This class's position in [`BandplanKind::ALL`].
    pub fn index(self) -> usize {
        match self {
            BandplanKind::Ham => 0,
            BandplanKind::Cw => 1,
            BandplanKind::Digi => 2,
            BandplanKind::Phone => 3,
            BandplanKind::Beacon => 4,
            BandplanKind::Broadcast => 5,
            BandplanKind::Am => 6,
            BandplanKind::Cb => 7,
        }
    }

    /// Short label for the settings picker beside the swatch.
    pub fn label(self) -> &'static str {
        match self {
            BandplanKind::Ham => "Ham",
            BandplanKind::Cw => "CW",
            BandplanKind::Digi => "Digital",
            BandplanKind::Phone => "Voice",
            BandplanKind::Beacon => "Beacon",
            BandplanKind::Broadcast => "Broadcast",
            BandplanKind::Am => "AM / LW / MW",
            BandplanKind::Cb => "CB",
        }
    }

    /// The stock RGB the strip shades this class with (r, g, b).
    ///
    /// Where a class starts, not where it has to stay: the operator can retint
    /// any of them from the UI settings tab, and what they chose is kept in
    /// [`UiSettings::bandplan_colors`]. Clients read that, not this.
    ///
    /// The blocks are painted at about 60% opacity over a near-black
    /// waterfall, so a saturated hue lands a good deal darker than it looks
    /// here — which is why the two broadcast classes read as brown and why
    /// they are worth being able to change (issue #145).
    pub fn default_color(self) -> (u8, u8, u8) {
        match self {
            BandplanKind::Ham => (0x2C, 0x9E, 0x8C),
            BandplanKind::Cw => (0xE6, 0xB0, 0x3C),
            BandplanKind::Digi => (0x2E, 0xC4, 0xE6),
            BandplanKind::Phone => (0x4C, 0xC9, 0x6A),
            BandplanKind::Beacon => (0xE0, 0x5A, 0xA0),
            BandplanKind::Broadcast => (0xE8, 0x82, 0x2E),
            BandplanKind::Am => (0xC9, 0x6A, 0x3C),
            BandplanKind::Cb => (0x9A, 0x6C, 0xE0),
        }
    }
}

/// Coarse font-size step for one family of hand-painted labels — the skimmer
/// boxes, the panadapter's own labels, the popup menus. Each family maps the
/// three steps onto its own point sizes, so `Small` here is "the small end of
/// that family's range", not one absolute size.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontSize {
    Small,
    Large,
    /// Declared last for the same reason as [`UiTheme::Default`]: serde
    /// demands the catch-all be the final variant, so a typo in a hand-edited
    /// config degrades to the middle size instead of throwing the whole
    /// config away.
    #[default]
    #[serde(other)]
    Medium,
}

impl FontSize {
    pub const ALL: [FontSize; 3] = [FontSize::Small, FontSize::Medium, FontSize::Large];

    pub fn label(self) -> &'static str {
        match self {
            FontSize::Small => "Small",
            FontSize::Medium => "Medium",
            FontSize::Large => "Large",
        }
    }
}

/// Which layout the window wears. `Auto` picks one from the viewport size; the
/// rest force it — for testing the compact strips without a phone to hand, and
/// for anyone who would rather have the menus in a small desktop window than a
/// control strip wrapped over three rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutMode {
    Desktop,
    Tablet,
    Phone,
    /// A small desktop screen: the tablet tier's menus, always on the
    /// single-row strip, with the operating panels' chips and spacing pulled
    /// in. For 1366×768 and the like, where the tablet layout fits but spends
    /// more of a short screen on chrome than it has to spare (issue #211).
    ///
    /// Appended rather than slotted in beside `Tablet`, because this is
    /// serialised into `config.toml` and inserting a variant would rename
    /// everyone else's.
    Small,
    /// Picks one from the viewport size — the default, and where serde sends a
    /// value this build has never heard of.
    ///
    /// Declared last for the same reason as [`FontSize::Medium`]: the catch-all
    /// must be final. Without `#[serde(other)]` a `config.toml` written by a
    /// build with a later layout — `Small` was the last one added — fails the
    /// **whole** file, and `Settings::load` quarantines it: the radio, audio,
    /// speech and alerts settings go with the one unknown value. Degrading to
    /// `Auto` costs a layout, not a configuration.
    #[default]
    #[serde(other)]
    Auto,
}

impl LayoutMode {
    pub const ALL: [LayoutMode; 5] = [
        LayoutMode::Auto,
        LayoutMode::Desktop,
        LayoutMode::Tablet,
        LayoutMode::Small,
        LayoutMode::Phone,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LayoutMode::Auto => "Auto",
            LayoutMode::Desktop => "Desktop",
            LayoutMode::Tablet => "Tablet",
            LayoutMode::Small => "Small screen",
            LayoutMode::Phone => "Phone",
        }
    }
}

/// Colour theme for the UI chrome. Every theme recolours the same set of
/// roles (backgrounds, borders, accents, text); content colours — waterfall
/// palettes, band plan, map — are untouched. The phosphor themes are
/// monochrome on purpose, except that transmit/SWR/error indications stay red
/// so an operator never has to wonder whether RF is leaving the antenna.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiTheme {
    GreenPhosphor,
    AmberPhosphor,
    TealOrange,
    Rainbow,
    /// The Nordic palette: polar-night navy grounds, snow-storm text, frost
    /// cyan accents and a purple chrome. Already a dark theme — NordDark is
    /// its near-black echo.
    Nord,
    /// The same accents as [`UiTheme::Nord`] on near-black grounds, so the
    /// panels sit almost in the page behind them.
    NordDark,
    /// The warm Gruvbox palette: parchment text on near-black browns, blue and
    /// aqua accents, and the iconic bright-orange chrome.
    Gruvbox,
    /// The Everforest palette: green-tinged deep blues, soft cream text, and
    /// mossy accent hues.
    Everforest,
    /// Ethan Schoonover's Solarized dark: teal, blue and magenta accents on
    /// the deep blue-grey base03/base02 grounds.
    SolarizedDark,
    /// Solarized on paper: the same accents carried down until they read as
    /// text on the pale base3 ground.
    Solarized,
    /// The Dracula palette: graphite mantles, mint-bright cyan, soft magenta
    /// chrome.
    Dracula,
    /// Catppuccin Mocha, the dark flavour: a deep indigo base, sky-blue and
    /// mauve accents.
    CatppuccinMocha,
    /// Catppuccin Latte, the light flavour: a cream base with teal and blue
    /// accents taken down to where they read as text on it.
    CatppuccinLatte,
    /// A near-monochrome light theme: white panels, graphite lines and a
    /// single restrained blue accent — the modern development-tool look.
    ModernMinimalist,
    /// White panels, near-black ink, dark saturated accents — the one theme
    /// that inverts the ground. The instruments (panadapter, S-meter, map,
    /// solar globe) keep their dark glass: a waterfall has no bright-ground
    /// form, and a signal display is read the same way in every theme.
    Light,
    /// White on black at the highest contrast the screen can give, with every
    /// dim shade in the UI pulled up to meet it — nothing is decoratively
    /// faint. For low vision, for glare, and for a display that has lost its
    /// contrast.
    HighContrast,
    /// ELAD's own: the graphite of the FDM-DUO's and the DUO-ART's anodised
    /// cases, their key caps and silk-screen lettering, with the status blue
    /// of the DUO-ART's screen as the accent. The whole panel wears it while
    /// the FDM-DUO is the chosen radio (decision 39), so the top bar, the
    /// scope and every window match the radio's face and the amplifier's
    /// screen.
    Elad,
    /// The classic navy/cyan/pink look. Declared last because serde demands
    /// the catch-all be the final variant: it also swallows an unrecognised
    /// value in a hand-edited config, so a typo degrades to the default theme
    /// instead of throwing the whole config away.
    #[default]
    #[serde(other)]
    Default,
}

impl UiTheme {
    pub const ALL: [UiTheme; 18] = [
        UiTheme::Default,
        UiTheme::Elad,
        UiTheme::Light,
        UiTheme::HighContrast,
        UiTheme::GreenPhosphor,
        UiTheme::AmberPhosphor,
        UiTheme::TealOrange,
        UiTheme::Rainbow,
        UiTheme::Nord,
        UiTheme::NordDark,
        UiTheme::Gruvbox,
        UiTheme::Everforest,
        UiTheme::SolarizedDark,
        UiTheme::Solarized,
        UiTheme::Dracula,
        UiTheme::CatppuccinMocha,
        UiTheme::CatppuccinLatte,
        UiTheme::ModernMinimalist,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UiTheme::Default => "Default",
            UiTheme::Light => "Light",
            UiTheme::HighContrast => "High contrast",
            UiTheme::GreenPhosphor => "Green phosphor",
            UiTheme::AmberPhosphor => "Amber phosphor",
            UiTheme::TealOrange => "Teal / orange",
            UiTheme::Rainbow => "Rainbow",
            UiTheme::Nord => "Nord",
            UiTheme::NordDark => "Nord dark",
            UiTheme::Gruvbox => "Gruvbox",
            UiTheme::Everforest => "Everforest",
            UiTheme::SolarizedDark => "Solarized dark",
            UiTheme::Solarized => "Solarized",
            UiTheme::Dracula => "Dracula",
            UiTheme::CatppuccinMocha => "Catppuccin mocha",
            UiTheme::CatppuccinLatte => "Catppuccin latte",
            UiTheme::ModernMinimalist => "Modern minimalist",
            UiTheme::Elad => "ELAD",
        }
    }
}

/// The shape a piece of chrome wears — one list serves both the buttons and
/// the windows, each chosen separately.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChromeStyle {
    Rectangular,
    Rounded,
    Gradient,
    Bevel,
    /// Drawn as if the screen were a character display: frames built out of
    /// `+`, `-` and `|`, buttons wearing `[` and `]`, tick boxes reading
    /// `[X]`, and everything set in the monospace face.
    Terminal,
    /// Rounded key caps lit from above with a dark edge, as on the FDM-DUO's
    /// and the DUO-ART's fronts; windows as their anodised cases. What the
    /// ELAD look wears (decision 39).
    Keycap,
    /// The classic cut-corner look. Last for the same reason as
    /// [`UiTheme::Default`]: the serde catch-all must be the final variant.
    #[default]
    #[serde(other)]
    Angled,
}

impl ChromeStyle {
    pub const ALL: [ChromeStyle; 7] = [
        ChromeStyle::Angled,
        ChromeStyle::Rectangular,
        ChromeStyle::Rounded,
        ChromeStyle::Gradient,
        ChromeStyle::Bevel,
        ChromeStyle::Terminal,
        ChromeStyle::Keycap,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChromeStyle::Angled => "Angled",
            ChromeStyle::Rectangular => "Rectangular",
            ChromeStyle::Rounded => "Rounded",
            ChromeStyle::Gradient => "Gradient",
            ChromeStyle::Bevel => "3D bevel",
            ChromeStyle::Terminal => "Terminal",
            ChromeStyle::Keycap => "Key caps",
        }
    }
}

/// Where a spot came from. Drives the overlay colour and the map marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpotKind {
    /// A DX-cluster telnet node (`DX de …`).
    DxCluster,
    /// Parks On The Air activator spot.
    Pota,
    /// Summits On The Air activator spot.
    Sota,
    /// A PSK Reporter reception report (someone heard us / a monitored call).
    PskReporter,
    /// A station connected to FreeDV Reporter (<https://qso.freedv.org/>).
    /// Appended last so the postcard discriminants above never move.
    FreeDv,
    /// A longwave/shortwave broadcast station from [`crate::broadcast`].
    ///
    /// Unlike every other kind this one is synthesised in the client from a
    /// bundled table rather than reported by a feed, so it never travels over
    /// the wire — but it shares `Spot` so the overlay, the list and the map
    /// render it through one path. Appended last, as above.
    Broadcast,
}

impl SpotKind {
    /// Every kind, in the order the SPOTS window's filter chips, the settings
    /// colour pickers and [`SpotKind::index`] all use. Anything indexed by kind
    /// — the filter array, [`crate::UiSettings::spot_colors`] — is this wide.
    pub const ALL: [SpotKind; 6] = [
        SpotKind::DxCluster,
        SpotKind::Pota,
        SpotKind::Sota,
        SpotKind::PskReporter,
        SpotKind::FreeDv,
        SpotKind::Broadcast,
    ];

    /// How many kinds there are, i.e. the width of every per-kind array.
    pub const COUNT: usize = SpotKind::ALL.len();

    /// This kind's position in [`SpotKind::ALL`].
    pub fn index(self) -> usize {
        match self {
            SpotKind::DxCluster => 0,
            SpotKind::Pota => 1,
            SpotKind::Sota => 2,
            SpotKind::PskReporter => 3,
            SpotKind::FreeDv => 4,
            SpotKind::Broadcast => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SpotKind::DxCluster => "DX",
            SpotKind::Pota => "POTA",
            SpotKind::Sota => "SOTA",
            SpotKind::PskReporter => "PSK",
            SpotKind::FreeDv => "FREEDV",
            SpotKind::Broadcast => "BC",
        }
    }

    /// The stock per-kind RGB tint for the overlay/marker (r, g, b).
    ///
    /// Where a label starts, not where it has to stay: the operator can retint
    /// any kind from the UI settings tab, and what they chose is kept in
    /// [`crate::UiSettings::spot_colors`]. Clients read that, not this.
    pub fn default_color(self) -> (u8, u8, u8) {
        match self {
            SpotKind::DxCluster => (120, 220, 255),   // cyan
            SpotKind::Pota => (120, 230, 140),        // green
            SpotKind::Sota => (255, 190, 90),         // amber
            SpotKind::PskReporter => (210, 150, 255), // violet
            SpotKind::FreeDv => (255, 130, 160),      // pink
            // Yellow-green. This used to be the same burnt orange the band-plan
            // overlay shades broadcast allocations with, so that a label matched
            // the band it sat in — but a broadcast box is *text*, and at that
            // saturation on the near-black waterfall it read as brown and was
            // hard to make out (issue #145). The band shading keeps the orange;
            // the label takes the brightest hue the other five leave free, which
            // is the gap between SOTA's amber and POTA's green.
            SpotKind::Broadcast => (190, 240, 120),
        }
    }
}
