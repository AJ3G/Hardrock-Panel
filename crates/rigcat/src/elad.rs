//! The ELAD FDM-DUO's CAT port: ASCII, `;`-terminated, 8N1 at one of the four
//! rates menu 70 `CAT BAUD` offers.
//!
//! Only READ forms are here. Nothing in this module can build a frame that
//! keys the radio or changes a setting (safety interlock #5, in
//! `docs/safety-interlocks.md`). The command forms and value tables are those
//! SDRoxide's ELAD driver uses (`sdroxide-cat/src/elad.rs`), written from the
//! *ELAD FDM-DUO User Manual* v2.6 §6 and checked on AJ3G's DUO: `FP` and `RP`
//! answer milliwatts (18 Oct 2026, TUNE at about 6 W into a dummy load:
//! `FP 006091;RP 000001;WR0 01.02;`).

/// The rates menu 70 `CAT BAUD` offers. At any other rate the DUO hears nothing.
pub const BAUDS: [u32; 4] = [9_600, 38_400, 57_600, 115_200];

/// The rate the FDM-DUO ships on.
pub const DEFAULT_BAUD: u32 = 38_400;

/// Read while transmitting and between: forward power, reflected power, SWR.
pub const TELEMETRY: [&str; 3] = ["FP;", "RP;", "WR;"];

/// Read once a second: the dial and the mode.
pub const DIAL: [&str; 2] = ["FA;", "MD;"];

/// Asked to see whether the radio is there at all.
pub const PING: &str = "FA;";

/// One row of the settings the radio can be asked for over CAT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// The READ frame that asks for it.
    pub cmd: &'static str,
    /// The radio's own menu number, where the setting is one of its menus.
    pub menu: Option<u8>,
    pub name: &'static str,
}

const fn item(cmd: &'static str, menu: Option<u8>, name: &'static str) -> Item {
    Item { cmd, menu, name }
}

/// Everything the DUO answers a READ for, as far as the command table goes.
/// Other menus (37 `CW IN`, 45 keyer speed and the rest) have no CAT command
/// in it and can only be read on the radio.
pub const SETTINGS: &[Item] = &[
    item("DT;", None, "Model"),
    item("VS;", None, "Firmware"),
    item("FA;", None, "VFO A"),
    item("FB;", None, "VFO B"),
    item("FR;", None, "Receive VFO"),
    item("MD;", None, "Mode"),
    item("RF;", None, "Receive filter"),
    item("SP;", None, "Split"),
    item("RT;", None, "RIT"),
    item("AN;", Some(31), "ANTENNAS"),
    item("TI;", Some(32), "TX IN"),
    item("TP;", Some(34), "TX POWER"),
    item("SM0;", None, "S-meter"),
];

/// Whether `frame` may be sent to the radio: only the READ frames above.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #5): the radio link never
/// keys the radio or changes its settings. `TX;` keys the transmitter with
/// no parameter at all, so "a frame with no parameter is a read" is not
/// safe; and `RC;` clears the RIT offset the same way. Keying the DUO from
/// here could put power into the amplifier when the operator did not mean to
/// transmit, on whatever band and antenna it is on. So the link sends only
/// frames on this list, and refuses anything else even from its own code.
pub fn is_read(frame: &str) -> bool {
    TELEMETRY.contains(&frame) || DIAL.contains(&frame) || SETTINGS.iter().any(|i| i.cmd == frame)
}

/// The prefix of the answer to a READ frame: its command letters.
pub fn reply_prefix(frame: &str) -> &str {
    &frame[..2.min(frame.len())]
}

/// A power reading from `FP` or `RP`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Watts {
    pub w: f32,
    /// False when the radio flags it (`!`) as not to be trusted: in receive,
    /// below 0.5 W, and as TUNE ramps up and down.
    pub valid: bool,
}

/// An SWR reading from `WR`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swr {
    pub ratio: f32,
    pub valid: bool,
    /// The radio dropped back to receive on a high SWR.
    pub tripped: bool,
}

/// `FP` / `RP` body: a flag (space or `!`), then six digits of milliwatts.
pub fn parse_watts(body: &str) -> Option<Watts> {
    let mut chars = body.chars();
    let valid = match chars.next()? {
        ' ' => true,
        '!' => false,
        _ => return None,
    };
    let mw: u32 = chars.as_str().trim().parse().ok()?;
    Some(Watts { w: mw as f32 / 1000.0, valid })
}

/// `WR` body: the high-SWR trip flag, a space or `!`, then `nn.nn`.
pub fn parse_swr(body: &str) -> Option<Swr> {
    let mut chars = body.chars();
    let tripped = chars.next()? == '1';
    let valid = match chars.next()? {
        ' ' => true,
        '!' => false,
        _ => return None,
    };
    let ratio: f32 = chars.as_str().trim().parse().ok()?;
    // Below 1:1 is a misread frame, not a match.
    Some(Swr { ratio, valid: valid && ratio >= 1.0, tripped })
}

/// `FA` / `FB` body: eleven digits of hertz.
pub fn parse_freq(body: &str) -> Option<u64> {
    (body.len() == 11).then(|| body.parse().ok()).flatten()
}

pub fn mode_name(digit: char) -> Option<&'static str> {
    Some(match digit {
        '1' => "LSB",
        '2' => "USB",
        '3' => "CW",
        '4' => "FM",
        '5' => "AM",
        '7' => "CW-R",
        _ => return None,
    })
}

/// Menu 34 `TX POWER`'s numbered steps, in watts, indexed by `TP`'s value.
/// 09 is MAX.
pub const TX_POWER_STEPS_W: [f32; 9] = [0.3, 0.5, 1.0, 1.2, 1.5, 2.0, 3.0, 4.0, 5.0];

/// `RF` filter widths, in Hz: SSB by index, CW by (width, index), AM by index.
const RF_SSB_HZ: &[u32] = &[
    1600, 1700, 1800, 1900, 2000, 2100, 2200, 2300, 2400, 2500, 2600, 2700, 2800, 2900, 3000, 3100,
    4000, 5000, 6000,
];
const RF_SSB_DATA_HZ: &[(u32, u32)] = &[(19, 300), (20, 600), (21, 1000)];
const RF_CW: &[(u32, u32)] = &[(11, 100), (12, 300), (13, 500), (14, 1000), (15, 1500), (16, 2600)];
const RF_AM_HZ: &[u32] = &[2500, 3000, 3500, 4000, 4500, 5000, 5500, 6000];

/// `SM` reading → dBm, from the manual's table (not evenly spaced: there is
/// no 7, and the steps double above S9).
const SMETER_CAL: &[(f32, f32)] = &[
    (0.0, -127.0),
    (2.0, -121.0),
    (3.0, -115.0),
    (4.0, -109.0),
    (5.0, -103.0),
    (6.0, -97.0),
    (8.0, -91.0),
    (9.0, -85.0),
    (10.0, -79.0),
    (11.0, -73.0),
    (12.0, -63.0),
    (14.0, -53.0),
    (16.0, -43.0),
    (18.0, -33.0),
    (20.0, -23.0),
    (22.0, -13.0),
];

fn dbm_from_smeter(reading: u32) -> f32 {
    let r = reading as f32;
    let (first, last) = (SMETER_CAL[0], SMETER_CAL[SMETER_CAL.len() - 1]);
    if r <= first.0 {
        return first.1;
    }
    for w in SMETER_CAL.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if r <= x1 {
            return y0 + (y1 - y0) * (r - x0) / (x1 - x0);
        }
    }
    last.1
}

fn off_on(body: &str) -> Option<String> {
    Some(
        match body {
            "0" => "Off",
            "1" => "On",
            _ => return None,
        }
        .into(),
    )
}

/// What a READ's answer means, in words, or `None` where it doesn't parse.
/// `prefix` is the command letters, `body` what follows them.
pub fn describe(prefix: &str, body: &str) -> Option<String> {
    let mhz = |hz: u64| format!("{:.6} MHz", hz as f64 / 1e6);
    match prefix {
        "FA" | "FB" => parse_freq(body).map(mhz),
        "FR" => Some(
            match body {
                "0" => "A",
                "1" => "B",
                _ => return None,
            }
            .into(),
        ),
        "MD" => body.chars().next().and_then(mode_name).map(String::from),
        "SP" | "RT" => off_on(body),
        "AN" => Some(
            match body {
                "1" => "1: RTX socket for receive and transmit",
                "2" => "2: receive on RX, transmit on RTX",
                _ => return None,
            }
            .into(),
        ),
        "TI" => Some(
            match body {
                "0" => "Microphone",
                "1" => "USB audio",
                "2" => "Auto",
                _ => return None,
            }
            .into(),
        ),
        "TP" => {
            let code: usize = body.trim().parse().ok()?;
            match code {
                9 => Some("MAX".into()),
                c => TX_POWER_STEPS_W.get(c).map(|w| format!("{w} W")),
            }
        }
        "RF" => {
            let mut c = body.chars();
            let mode = c.next()?;
            let idx: u32 = c.as_str().parse().ok()?;
            let width = match mode {
                '1' | '2' => RF_SSB_HZ
                    .get(idx as usize)
                    .copied()
                    .or_else(|| RF_SSB_DATA_HZ.iter().find(|(i, _)| *i == idx).map(|(_, w)| *w)),
                '3' | '7' => RF_CW.iter().find(|(i, _)| *i == idx).map(|(_, w)| *w),
                '5' => RF_AM_HZ.get(idx as usize).copied(),
                _ => None,
            };
            let m = mode_name(mode).unwrap_or("?");
            Some(match width {
                Some(w) => format!("{w} Hz ({m})"),
                None => format!("position {idx} ({m})"),
            })
        }
        "SM" => {
            let r: u32 = body.strip_prefix('0')?.parse().ok()?;
            Some(format!("{:.0} dBm", dbm_from_smeter(r)))
        }
        "FP" | "RP" => parse_watts(body)
            .map(|p| format!("{:.2} W{}", p.w, if p.valid { "" } else { " (flagged)" })),
        "WR" => parse_swr(body).map(|s| {
            let mut t = format!("{:.2}:1", s.ratio);
            if !s.valid {
                t.push_str(" (flagged)");
            }
            if s.tripped {
                t.push_str(", tripped on high SWR");
            }
            t
        }),
        // Shown as the radio gives them.
        "DT" | "VS" => Some(body.trim().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_that_keys_or_sets_the_radio_can_be_sent() {
        for f in ["TX;", "TX0;", "TX2;", "RX;", "RC;", "TP09;", "AN1;", "TI1;", "FA00014074000;"] {
            assert!(!is_read(f), "{f} must be refused");
        }
        for f in TELEMETRY.iter().chain(DIAL.iter()).chain(SETTINGS.iter().map(|i| &i.cmd)) {
            assert!(is_read(f));
            // A READ ends at its command letters (SM carries its fixed 0).
            assert!(f.len() <= 4 && f.ends_with(';'), "{f}");
            assert!(!f.starts_with("TX") && !f.starts_with("RX"));
        }
        assert!(is_read(PING));
    }

    #[test]
    fn power_is_milliwatts_with_its_trust_flag() {
        // AJ3G's DUO, TUNE at about 6 W into a dummy load.
        assert_eq!(parse_watts(" 006091"), Some(Watts { w: 6.091, valid: true }));
        assert_eq!(parse_watts(" 000001"), Some(Watts { w: 0.001, valid: true }));
        assert_eq!(parse_watts("!000000"), Some(Watts { w: 0.0, valid: false }));
        assert_eq!(parse_watts("x000100"), None);
    }

    #[test]
    fn swr_carries_trip_and_trust_flags() {
        assert_eq!(parse_swr("0 01.02"), Some(Swr { ratio: 1.02, valid: true, tripped: false }));
        assert_eq!(parse_swr("1 09.90"), Some(Swr { ratio: 9.9, valid: true, tripped: true }));
        assert!(!parse_swr("0!01.00").unwrap().valid);
        assert!(!parse_swr("0 00.50").unwrap().valid);
    }

    #[test]
    fn settings_read_as_the_radio_means_them() {
        assert_eq!(describe("FA", "00014074000").unwrap(), "14.074000 MHz");
        assert_eq!(describe("TP", "09").unwrap(), "MAX");
        assert_eq!(describe("TP", "08").unwrap(), "5 W");
        assert_eq!(describe("TP", "00").unwrap(), "0.3 W");
        assert_eq!(describe("TI", "1").unwrap(), "USB audio");
        assert_eq!(describe("RF", "208").unwrap(), "2400 Hz (USB)");
        assert_eq!(describe("RF", "313").unwrap(), "500 Hz (CW)");
        assert_eq!(describe("RF", "507").unwrap(), "6000 Hz (AM)");
        assert_eq!(describe("RF", "401").unwrap(), "position 1 (FM)");
        assert_eq!(describe("SM", "00011").unwrap(), "-73 dBm");
        assert_eq!(describe("MD", "7").unwrap(), "CW-R");
        assert_eq!(describe("SP", "0").unwrap(), "Off");
        assert!(describe("TP", "12").is_none());
        assert!(describe("ZZ", "1").is_none());
    }
}
