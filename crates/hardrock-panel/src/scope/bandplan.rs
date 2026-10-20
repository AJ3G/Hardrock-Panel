//! The band plan strip and the data-mode buttons.
//!
//! The amateur segments are the IARU Region 2 (ITU Region 2) band plan,
//! simplified to its main segments for HF and 6 m, with the ARRL's Region 2
//! plans for 2 m, 1.25 m, 70 cm and 23 cm cut down the same way. A few other
//! services a transverter or a wideband converter shows are listed too: the
//! VHF airband, marine VHF and ADS-B. All of it is published public
//! information; it is a guide on screen, not a licence condition.
//!
//! The data-mode calling frequencies are the ones WSJT-X, WSPRnet and JS8Call
//! publish, and common RTTY and PSK31 practice.

use std::collections::BTreeMap;

/// What a segment of spectrum is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Use {
    Cw,
    /// Narrow digital modes.
    Digi,
    Beacon,
    /// Phone and all modes.
    Phone,
    All,
    Satellite,
    /// FM simplex and repeaters.
    Fm,
    /// Not amateur: the aeronautical VHF band (navigation and voice).
    Air,
    /// Not amateur: marine VHF.
    Marine,
    /// Not amateur: ADS-B and UAT aircraft position reports.
    Adsb,
}

impl Use {
    pub const ALL: [Use; 10] = [
        Use::Cw,
        Use::Digi,
        Use::Beacon,
        Use::Phone,
        Use::All,
        Use::Satellite,
        Use::Fm,
        Use::Air,
        Use::Marine,
        Use::Adsb,
    ];

    /// The name in the key and the cursor readout.
    pub fn name(self) -> &'static str {
        match self {
            Use::Cw => "CW",
            Use::Digi => "Narrow digital",
            Use::Beacon => "Beacons",
            Use::Phone => "Phone and all modes",
            Use::All => "All modes",
            Use::Satellite => "Satellite",
            Use::Fm => "FM",
            Use::Air => "Airband",
            Use::Marine => "Marine VHF",
            Use::Adsb => "ADS-B",
        }
    }

    /// The short label written on the strip when the segment is wide enough.
    pub fn short(self) -> &'static str {
        match self {
            Use::Cw => "CW",
            Use::Digi => "DIGI",
            Use::Beacon => "BCN",
            Use::Phone => "PHONE · ALL",
            Use::All => "ALL",
            Use::Satellite => "SAT",
            Use::Fm => "FM",
            Use::Air => "AIR",
            Use::Marine => "MARINE",
            Use::Adsb => "ADS-B",
        }
    }

    /// The segment's colour on the strip, as the mockup draws it: blues,
    /// teals and violets, never yellow or red, which are for warnings.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Use::Cw => [0x3d, 0x6f, 0xb8],
            Use::Digi => [0x23, 0x96, 0x8c],
            Use::Beacon => [0x8a, 0x6f, 0xd1],
            Use::Phone => [0x56, 0x70, 0x8a],
            Use::All => [0x6b, 0x74, 0x80],
            Use::Satellite => [0x7d, 0x5e, 0xa6],
            Use::Fm => [0x3a, 0x8a, 0xa6],
            Use::Air => [0x4a, 0x5d, 0x86],
            Use::Marine => [0x2b, 0x6e, 0x8c],
            Use::Adsb => [0x5f, 0x58, 0x9e],
        }
    }

    /// An amateur segment (the others are other services).
    pub fn amateur(self) -> bool {
        !matches!(self, Use::Air | Use::Marine | Use::Adsb)
    }
}

/// One segment: from, to (kHz) and its use.
pub type Segment = (f64, f64, Use);

/// The band plan, in kHz, in frequency order with no overlaps.
pub const BAND_PLAN: &[Segment] = &[
    // 160 m
    (1800.0, 1810.0, Use::Digi),
    (1810.0, 1840.0, Use::Cw),
    (1840.0, 1850.0, Use::Digi),
    (1850.0, 2000.0, Use::Phone),
    // 80 m
    (3500.0, 3570.0, Use::Cw),
    (3570.0, 3600.0, Use::Digi),
    (3600.0, 4000.0, Use::Phone),
    // 60 m
    (5330.5, 5406.5, Use::All),
    // 40 m
    (7000.0, 7040.0, Use::Cw),
    (7040.0, 7050.0, Use::Digi),
    (7050.0, 7300.0, Use::Phone),
    // 30 m
    (10100.0, 10130.0, Use::Cw),
    (10130.0, 10150.0, Use::Digi),
    // 20 m
    (14000.0, 14070.0, Use::Cw),
    (14070.0, 14099.0, Use::Digi),
    (14099.0, 14101.0, Use::Beacon),
    (14101.0, 14112.0, Use::Digi),
    (14112.0, 14350.0, Use::Phone),
    // 17 m
    (18068.0, 18095.0, Use::Cw),
    (18095.0, 18109.0, Use::Digi),
    (18109.0, 18111.0, Use::Beacon),
    (18111.0, 18168.0, Use::Phone),
    // 15 m
    (21000.0, 21070.0, Use::Cw),
    (21070.0, 21149.0, Use::Digi),
    (21149.0, 21151.0, Use::Beacon),
    (21151.0, 21450.0, Use::Phone),
    // 12 m
    (24890.0, 24915.0, Use::Cw),
    (24915.0, 24929.0, Use::Digi),
    (24929.0, 24931.0, Use::Beacon),
    (24931.0, 24990.0, Use::Phone),
    // 10 m
    (28000.0, 28070.0, Use::Cw),
    (28070.0, 28190.0, Use::Digi),
    (28190.0, 28225.0, Use::Beacon),
    (28225.0, 29300.0, Use::Phone),
    (29300.0, 29510.0, Use::Satellite),
    (29510.0, 29700.0, Use::Fm),
    // 6 m
    (50000.0, 50100.0, Use::Cw),
    (50100.0, 50300.0, Use::Phone),
    (50300.0, 50500.0, Use::Digi),
    (50500.0, 54000.0, Use::All),
    // VHF airband: navigation (VOR, ILS) then AM voice.
    (108000.0, 117975.0, Use::Air),
    (117975.0, 137000.0, Use::Air),
    // 2 m
    (144000.0, 144100.0, Use::Cw),
    (144100.0, 144275.0, Use::Phone),
    (144275.0, 144300.0, Use::Beacon),
    (144300.0, 144500.0, Use::Satellite),
    (144500.0, 145800.0, Use::Fm),
    (145800.0, 146000.0, Use::Satellite),
    (146000.0, 148000.0, Use::Fm),
    // Marine VHF, channels 1 to 88 and AIS.
    (156000.0, 162025.0, Use::Marine),
    // 1.25 m
    (222000.0, 222050.0, Use::Cw),
    (222050.0, 222060.0, Use::Beacon),
    (222060.0, 222150.0, Use::Phone),
    (222150.0, 225000.0, Use::Fm),
    // 70 cm
    (420000.0, 432000.0, Use::All),
    (432000.0, 432100.0, Use::Cw),
    (432100.0, 432300.0, Use::Phone),
    (432300.0, 432400.0, Use::Beacon),
    (432400.0, 435000.0, Use::All),
    (435000.0, 438000.0, Use::Satellite),
    (438000.0, 450000.0, Use::Fm),
    // UAT (978 MHz) and ADS-B (1090 MHz).
    (977000.0, 979000.0, Use::Adsb),
    (1089000.0, 1091000.0, Use::Adsb),
    // 23 cm
    (1240000.0, 1260000.0, Use::All),
    (1260000.0, 1270000.0, Use::Satellite),
    (1270000.0, 1295000.0, Use::Fm),
    (1295000.0, 1296000.0, Use::All),
    (1296000.0, 1296100.0, Use::Cw),
    (1296100.0, 1296400.0, Use::Phone),
    (1296400.0, 1300000.0, Use::All),
];

/// The segment `hz` falls in, if any.
pub fn segment_at(hz: f64) -> Option<Segment> {
    let khz = hz / 1000.0;
    // The plan is sorted: the last segment starting at or below `khz`.
    let i = BAND_PLAN.partition_point(|s| s.0 <= khz);
    let s = *BAND_PLAN.get(i.checked_sub(1)?)?;
    (khz < s.1).then_some(s)
}

/// The segments that overlap `[lo, hi]` Hz.
pub fn segments_in(lo: f64, hi: f64) -> impl Iterator<Item = Segment> {
    BAND_PLAN.iter().copied().filter(move |s| s.1 * 1000.0 > lo && s.0 * 1000.0 < hi)
}

/// What the cursor readout says about `hz` on the band plan.
pub fn describe(hz: f64) -> String {
    match segment_at(hz) {
        Some((a, b, u)) => format!("{} · {}–{} kHz", u.name(), fmt_khz(a), fmt_khz(b)),
        None => "outside the band plan".to_string(),
    }
}

fn fmt_khz(k: f64) -> String {
    if k.fract() == 0.0 { format!("{k:.0}") } else { format!("{k:.1}") }
}

/// An amateur band, by its usual name, with its edges in Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HamBand {
    pub name: &'static str,
    pub lo: f64,
    pub hi: f64,
}

/// The amateur bands the plan covers.
pub const HAM_BANDS: &[HamBand] = &[
    HamBand { name: "160", lo: 1.8e6, hi: 2.0e6 },
    HamBand { name: "80", lo: 3.5e6, hi: 4.0e6 },
    HamBand { name: "60", lo: 5.3305e6, hi: 5.4065e6 },
    HamBand { name: "40", lo: 7.0e6, hi: 7.3e6 },
    HamBand { name: "30", lo: 10.1e6, hi: 10.15e6 },
    HamBand { name: "20", lo: 14.0e6, hi: 14.35e6 },
    HamBand { name: "17", lo: 18.068e6, hi: 18.168e6 },
    HamBand { name: "15", lo: 21.0e6, hi: 21.45e6 },
    HamBand { name: "12", lo: 24.89e6, hi: 24.99e6 },
    HamBand { name: "10", lo: 28.0e6, hi: 29.7e6 },
    HamBand { name: "6", lo: 50.0e6, hi: 54.0e6 },
    HamBand { name: "2", lo: 144.0e6, hi: 148.0e6 },
    HamBand { name: "1.25", lo: 222.0e6, hi: 225.0e6 },
    HamBand { name: "70cm", lo: 420.0e6, hi: 450.0e6 },
    HamBand { name: "23cm", lo: 1240.0e6, hi: 1300.0e6 },
];

/// The band `hz` is in.
pub fn band_at(hz: f64) -> Option<&'static HamBand> {
    HAM_BANDS.iter().find(|b| hz >= b.lo && hz <= b.hi)
}

/// The band `hz` is in, or else the nearest one.
pub fn nearest_band(hz: f64) -> &'static HamBand {
    band_at(hz).unwrap_or_else(|| {
        HAM_BANDS
            .iter()
            .min_by(|a, b| dist(a, hz).total_cmp(&dist(b, hz)))
            .expect("the band list is not empty")
    })
}

fn dist(b: &HamBand, hz: f64) -> f64 {
    if hz < b.lo { b.lo - hz } else { hz - b.hi }
}

/// A data mode the scope has a button for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataMode {
    Ft8,
    Ft4,
    Wspr,
    Rtty,
    Psk31,
    Js8,
}

impl DataMode {
    pub const ALL: [DataMode; 6] = [
        DataMode::Ft8,
        DataMode::Ft4,
        DataMode::Wspr,
        DataMode::Rtty,
        DataMode::Psk31,
        DataMode::Js8,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DataMode::Ft8 => "FT8",
            DataMode::Ft4 => "FT4",
            DataMode::Wspr => "WSPR",
            DataMode::Rtty => "RTTY",
            DataMode::Psk31 => "PSK31",
            DataMode::Js8 => "JS8",
        }
    }

    /// The receive passband each mode wants, Hz above the (USB) dial: the
    /// audio range its decoder listens to.
    pub fn passband(self) -> (f32, f32) {
        match self {
            DataMode::Ft8 | DataMode::Ft4 | DataMode::Js8 => (200.0, 3000.0),
            DataMode::Wspr => (1400.0, 1600.0),
            DataMode::Rtty => (2000.0, 2420.0),
            DataMode::Psk31 => (300.0, 2700.0),
        }
    }

    /// The USB dial frequency on each band that has one, Hz.
    pub fn dials(self) -> &'static [(&'static str, f64)] {
        match self {
            DataMode::Ft8 => &[
                ("160", 1_840_000.0),
                ("80", 3_573_000.0),
                ("60", 5_357_000.0),
                ("40", 7_074_000.0),
                ("30", 10_136_000.0),
                ("20", 14_074_000.0),
                ("17", 18_100_000.0),
                ("15", 21_074_000.0),
                ("12", 24_915_000.0),
                ("10", 28_074_000.0),
                ("6", 50_313_000.0),
                ("2", 144_174_000.0),
                ("70cm", 432_174_000.0),
                ("23cm", 1_296_174_000.0),
            ],
            DataMode::Ft4 => &[
                ("80", 3_575_500.0),
                ("40", 7_047_500.0),
                ("30", 10_140_000.0),
                ("20", 14_080_000.0),
                ("17", 18_104_000.0),
                ("15", 21_140_000.0),
                ("12", 24_919_000.0),
                ("10", 28_180_000.0),
                ("6", 50_318_000.0),
                ("2", 144_170_000.0),
            ],
            DataMode::Wspr => &[
                ("160", 1_836_600.0),
                ("80", 3_568_600.0),
                ("60", 5_364_700.0),
                ("40", 7_038_600.0),
                ("30", 10_138_700.0),
                ("20", 14_095_600.0),
                ("17", 18_104_600.0),
                ("15", 21_094_600.0),
                ("12", 24_924_600.0),
                ("10", 28_124_600.0),
                ("6", 50_293_000.0),
                ("2", 144_489_000.0),
                ("70cm", 432_300_000.0),
            ],
            DataMode::Rtty => &[
                ("80", 3_580_000.0),
                ("40", 7_043_000.0),
                ("30", 10_142_000.0),
                ("20", 14_083_000.0),
                ("17", 18_101_000.0),
                ("15", 21_083_000.0),
                ("12", 24_920_000.0),
                ("10", 28_083_000.0),
                ("6", 50_300_000.0),
            ],
            DataMode::Psk31 => &[
                ("160", 1_838_000.0),
                ("80", 3_580_000.0),
                ("40", 7_070_000.0),
                ("30", 10_142_000.0),
                ("20", 14_070_000.0),
                ("17", 18_100_000.0),
                ("15", 21_070_000.0),
                ("12", 24_920_000.0),
                ("10", 28_120_000.0),
                ("6", 50_290_000.0),
            ],
            DataMode::Js8 => &[
                ("160", 1_842_000.0),
                ("80", 3_578_000.0),
                ("40", 7_078_000.0),
                ("30", 10_130_000.0),
                ("20", 14_078_000.0),
                ("17", 18_104_000.0),
                ("15", 21_078_000.0),
                ("12", 24_922_000.0),
                ("10", 28_078_000.0),
                ("6", 50_318_000.0),
            ],
        }
    }

    pub fn dial_on(self, band: &str) -> Option<f64> {
        self.dials().iter().find(|(b, _)| *b == band).map(|(_, f)| *f)
    }
}

/// Where a data-mode button sends the receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct DataTune {
    /// The dial, as shown (after any transverter offset).
    pub dial_hz: f64,
    pub passband: (f32, f32),
    /// Said beside the buttons when the mode has no frequency on this band and
    /// 20 m was used instead.
    pub note: Option<String>,
}

/// The dial for `mode` on the band `shown_hz` is in (or the nearest band),
/// falling back to 20 m when the mode has no frequency on that band.
pub fn data_tune(mode: DataMode, shown_hz: f64) -> DataTune {
    let band = nearest_band(shown_hz);
    match mode.dial_on(band.name) {
        Some(f) => DataTune { dial_hz: f, passband: mode.passband(), note: None },
        None => DataTune {
            dial_hz: mode.dial_on("20").expect("every data mode has a 20 m frequency"),
            passband: mode.passband(),
            note: Some(format!("No {} frequency on {} m, so 20 m", mode.label(), band.name)),
        },
    }
}

/// Every data-mode calling frequency inside `[lo, hi]` Hz, for the ticks on
/// the band plan strip, grouped by frequency (FT8 and JS8 may share one).
pub fn data_marks(lo: f64, hi: f64) -> BTreeMap<i64, Vec<DataMode>> {
    let mut m: BTreeMap<i64, Vec<DataMode>> = BTreeMap::new();
    for d in DataMode::ALL {
        for &(_, f) in d.dials() {
            if f >= lo && f <= hi {
                m.entry(f as i64).or_default().push(d);
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_is_sorted_and_does_not_overlap() {
        for w in BAND_PLAN.windows(2) {
            assert!(w[0].0 < w[0].1, "{:?}", w[0]);
            assert!(w[0].1 <= w[1].0, "{:?} then {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn a_frequency_finds_its_segment() {
        assert_eq!(segment_at(14_074_000.0).map(|s| s.2), Some(Use::Digi));
        assert_eq!(segment_at(14_030_000.0).map(|s| s.2), Some(Use::Cw));
        assert_eq!(segment_at(14_100_000.0).map(|s| s.2), Some(Use::Beacon));
        assert_eq!(segment_at(14_200_000.0).map(|s| s.2), Some(Use::Phone));
        // The top edge belongs to the next segment, or to none.
        assert_eq!(segment_at(14_070_000.0).map(|s| s.2), Some(Use::Digi));
        assert_eq!(segment_at(14_350_000.0), None);
        assert_eq!(segment_at(13_999_999.0), None);
        assert_eq!(segment_at(144_174_000.0).map(|s| s.2), Some(Use::Phone));
        assert_eq!(segment_at(146_520_000.0).map(|s| s.2), Some(Use::Fm));
        assert_eq!(segment_at(121_500_000.0).map(|s| s.2), Some(Use::Air));
        assert_eq!(segment_at(156_800_000.0).map(|s| s.2), Some(Use::Marine));
        assert_eq!(segment_at(1_090_000_000.0).map(|s| s.2), Some(Use::Adsb));
        assert_eq!(segment_at(1_296_200_000.0).map(|s| s.2), Some(Use::Phone));
        assert_eq!(segment_at(100.0), None);
        assert!(describe(14_074_000.0).starts_with("Narrow digital · 14070–14099"));
        assert_eq!(describe(9_000_000.0), "outside the band plan");
    }

    #[test]
    fn segments_on_screen_are_the_ones_that_overlap_it() {
        let s: Vec<_> = segments_in(14_060_000.0, 14_110_000.0).map(|s| s.2).collect();
        assert_eq!(s, [Use::Cw, Use::Digi, Use::Beacon, Use::Digi]);
    }

    #[test]
    fn bands_are_found_or_the_nearest_taken() {
        assert_eq!(band_at(7_100_000.0).unwrap().name, "40");
        assert_eq!(band_at(9_000_000.0), None);
        assert_eq!(nearest_band(9_000_000.0).name, "30");
        assert_eq!(nearest_band(145_000_000.0).name, "2");
        assert_eq!(nearest_band(100.0).name, "160");
    }

    #[test]
    fn data_buttons_go_to_the_bands_calling_frequency() {
        let t = data_tune(DataMode::Ft8, 7_150_000.0);
        assert_eq!(t.dial_hz, 7_074_000.0);
        assert_eq!(t.passband, (200.0, 3000.0));
        assert!(t.note.is_none());
        // RTTY has no 160 m frequency here: 20 m, and it says so.
        let t = data_tune(DataMode::Rtty, 1_900_000.0);
        assert_eq!(t.dial_hz, 14_083_000.0);
        assert!(t.note.unwrap().contains("160 m"));
        // Through a 2 m transverter: the 2 m frequency.
        assert_eq!(data_tune(DataMode::Ft8, 144_300_000.0).dial_hz, 144_174_000.0);
        assert_eq!(data_tune(DataMode::Wspr, 14_000_000.0).passband, (1400.0, 1600.0));
    }

    #[test]
    fn calling_frequencies_on_screen_are_grouped() {
        let m = data_marks(14_060_000.0, 14_100_000.0);
        assert_eq!(m.get(&14_074_000), Some(&vec![DataMode::Ft8]));
        assert_eq!(m.get(&14_070_000), Some(&vec![DataMode::Psk31]));
        assert!(m.contains_key(&14_095_600));
        assert!(!m.contains_key(&14_140_000));
    }
}
