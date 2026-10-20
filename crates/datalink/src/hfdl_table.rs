//! HFDL ground stations and the frequencies they use.
//!
//! The built-in list is what the stations have been heard on, taken from the
//! EiBi shortwave schedule (Eike Bierwirth, https://www.eibispace.de/) as
//! SDRoxide ships it in its fallback schedule. Frequencies are the SSB
//! carrier (the channel's "dial" frequency, upper sideband), in kHz. The
//! schedule has no entry for Krasnoyarsk (ID 14), so its list is empty until
//! the station's own system table is heard.
//!
//! The authoritative list is the one the network broadcasts: every ground
//! station sends the system table (HFNPDU 0xD0) now and then, and
//! [`HfdlTable::learn`] takes it in from the decoder's `systable-complete`
//! events, replacing the built-in list.

use serde_json::Value;

/// One ground station and its frequencies.
#[derive(Debug, Clone, PartialEq)]
pub struct HfdlStation {
    /// The station's HFDL ID (1 to 17; 12 is unassigned).
    pub id: u8,
    pub name: &'static str,
    /// SSB carrier frequencies, kHz.
    pub khz: &'static [u32],
}

/// The built-in list, by ID.
pub const HFDL_STATIONS: &[HfdlStation] = &[
    HfdlStation {
        id: 1,
        name: "San Francisco, USA",
        khz: &[5508, 6559, 8927, 10081, 11327, 13276, 17919, 21934],
    },
    HfdlStation {
        id: 2,
        name: "Molokai, Hawaii",
        khz: &[5514, 6565, 8912, 8936, 10027, 11312, 11348, 13276, 13312, 13324, 17919, 21937],
    },
    HfdlStation {
        id: 3,
        name: "Reykjavik, Iceland",
        khz: &[3900, 5720, 6712, 8977, 11184, 15025, 17985],
    },
    HfdlStation {
        id: 4,
        name: "Riverhead, New York",
        khz: &[5652, 6661, 8912, 11387, 13276, 17919, 21931],
    },
    HfdlStation {
        id: 5,
        name: "Auckland, New Zealand",
        khz: &[5583, 6535, 8921, 10084, 13351, 17916],
    },
    HfdlStation {
        id: 6,
        name: "Hat Yai, Thailand",
        khz: &[5655, 6535, 8825, 10066, 13270, 17928, 21949],
    },
    HfdlStation {
        id: 7,
        name: "Shannon, Ireland",
        khz: &[2998, 3455, 5547, 6532, 8843, 8942, 10081, 11384],
    },
    HfdlStation {
        id: 8,
        name: "Johannesburg, South Africa",
        khz: &[3016, 4681, 5529, 8834, 11321, 13321, 17922, 21949],
    },
    HfdlStation {
        id: 9,
        name: "Barrow, Alaska",
        khz: &[
            2944, 2992, 3007, 3497, 4654, 4687, 5529, 5538, 5544, 6646, 8927, 8936, 10027, 10093,
            11354, 17919, 17934, 21928, 21937,
        ],
    },
    HfdlStation {
        id: 10,
        name: "Muan, South Korea",
        khz: &[2941, 5502, 6619, 8939, 10060, 13342, 17958, 21931],
    },
    HfdlStation { id: 11, name: "Albrook, Panama", khz: &[5589, 6589, 8894, 10063, 13264, 17901] },
    HfdlStation {
        id: 13,
        name: "Santa Cruz, Bolivia",
        khz: &[4660, 6628, 8957, 11318, 13315, 17916, 21997],
    },
    HfdlStation { id: 14, name: "Krasnoyarsk, Russia", khz: &[] },
    HfdlStation {
        id: 15,
        name: "Al Muharraq, Bahrain",
        khz: &[2986, 5544, 6646, 8885, 10030, 13312, 17967, 21982],
    },
    HfdlStation {
        id: 16,
        name: "Agana, Guam",
        khz: &[5451, 6652, 8927, 11306, 13312, 17919, 21928],
    },
    HfdlStation { id: 17, name: "Canarias, Spain", khz: &[6529, 8948, 11348, 13303, 17928, 21955] },
];

/// The built-in entry for station `id`.
pub fn station(id: u8) -> Option<&'static HfdlStation> {
    HFDL_STATIONS.iter().find(|s| s.id == id)
}

/// The station list in use: the built-in one until the network's own system
/// table has been heard.
#[derive(Debug, Clone, PartialEq)]
pub struct HfdlTable {
    /// `(id, name, carrier frequencies in Hz)` per station.
    pub stations: Vec<(u8, String, Vec<u64>)>,
    /// The system-table version learnt, or `None` for the built-in list.
    pub version: Option<u16>,
}

impl Default for HfdlTable {
    fn default() -> Self {
        HfdlTable {
            stations: HFDL_STATIONS
                .iter()
                .map(|s| {
                    (s.id, s.name.to_string(), s.khz.iter().map(|&k| k as u64 * 1000).collect())
                })
                .collect(),
            version: None,
        }
    }
}

impl HfdlTable {
    pub fn new() -> Self {
        HfdlTable::default()
    }

    /// Every carrier frequency in use, in Hz, sorted, each once.
    pub fn all_hz(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.stations.iter().flat_map(|s| s.2.iter().copied()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The carriers whose whole channel (carrier to carrier + 3 kHz) lies in
    /// `lo_hz..=hi_hz`: what a span of I/Q can decode at once.
    pub fn in_span(&self, lo_hz: f64, hi_hz: f64) -> Vec<u64> {
        self.all_hz()
            .into_iter()
            .filter(|&f| f as f64 >= lo_hz && f as f64 + 3_000.0 <= hi_hz)
            .collect()
    }

    /// The stations that use carrier `hz`.
    pub fn stations_on(&self, hz: u64) -> Vec<u8> {
        self.stations.iter().filter(|s| s.2.contains(&hz)).map(|s| s.0).collect()
    }

    /// Take in a decoded event: a complete system table newer than the one
    /// held replaces it. Returns whether it did.
    pub fn learn(&mut self, kind: &str, details: &Value) -> bool {
        if kind != "systable-complete" {
            return false;
        }
        let Some(version) = details["version"].as_u64().map(|v| v as u16) else { return false };
        if self.version.is_some_and(|v| !xng_mode_hfdl::systable::version_is_newer(version, v)) {
            return false;
        }
        let Some(list) = details["stations"].as_array() else { return false };
        let stations: Vec<(u8, String, Vec<u64>)> = list
            .iter()
            .filter_map(|s| {
                let id = s["gs_id"].as_u64()? as u8;
                let name = s["gs_name"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| station(id).map(|b| b.name.to_string()))
                    .unwrap_or_else(|| format!("ID {id}"));
                let freqs = s["frequencies"]
                    .as_array()?
                    .iter()
                    .filter_map(|f| f["freq_hz"].as_u64())
                    .collect();
                Some((id, name, freqs))
            })
            .collect();
        if stations.is_empty() {
            return false;
        }
        self.stations = stations;
        self.version = Some(version);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_list_has_riverhead_on_21931() {
        let t = HfdlTable::new();
        assert!(t.stations_on(21_931_000).contains(&4));
        assert_eq!(station(4).unwrap().name, "Riverhead, New York");
        // 8.8 to 9.0 MHz: the 8.9 MHz band, all inside a 192 kHz span.
        let v = t.in_span(8_820_000.0, 9_000_000.0);
        assert!(v.contains(&8_912_000) && v.contains(&8_977_000));
        assert!(v.iter().all(|&f| (8_820_000..=8_997_000).contains(&f)));
    }

    #[test]
    fn a_heard_system_table_replaces_it() {
        use xng_mode_hfdl::systable::{GroundStation, GsFrequency, SystemTable};
        let table = SystemTable {
            version: 53,
            stations: vec![GroundStation {
                gs_id: 4,
                gs_name: None,
                utc_sync: true,
                lat: 40.9,
                lon: -72.6,
                spdu_version: 0,
                frequencies: vec![GsFrequency { freq_hz: 21_931_000, master_frame_slot: 2 }],
            }],
        };
        let mut t = HfdlTable::new();
        assert!(t.learn("systable-complete", &serde_json::to_value(&table).unwrap()));
        assert_eq!(t.version, Some(53));
        assert_eq!(t.stations, vec![(4, "Riverhead, New York".to_string(), vec![21_931_000])]);
        // An older table does not replace a newer one.
        let older = SystemTable { version: 52, ..table };
        assert!(!t.learn("systable-complete", &serde_json::to_value(&older).unwrap()));
    }
}
