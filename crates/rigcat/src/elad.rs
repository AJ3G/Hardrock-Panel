//! The ELAD FDM-DUO's CAT port: ASCII, `;`-terminated, 8N1 at one of the four
//! rates menu 70 `CAT BAUD` offers.
//!
//! Reads, menu 34 (`TP`), the radio's own TUNE carrier (`TX2;`) and unkey
//! (`RX;`), and nothing else (safety interlocks #5 and #6, in
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

/// Read with the telemetry while receiving: the S-meter, so RADIO's meter
/// moves with the band rather than once a settings read (10 s).
pub const METER: [&str; 1] = ["SM0;"];

/// Read once a second: the dial, VFO B, the mode, `GI`, which carries
/// receive or transmit, the VFO, split and RIT in one answer, and the
/// receive filter, so a filter changed on the radio shows within a second.
pub const DIAL: [&str; 5] = ["FA;", "FB;", "MD;", "GI;", "RF;"];

/// Asked to see whether the radio is there at all.
pub const PING: &str = "FA;";

use std::sync::OnceLock;

use crate::RigStatus;
use crate::protocol::{
    Control, Frame, FrameKind, Group, Incoming, Item, Kind, PowerStep, Protocol, Reply, Vfo,
};

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

const fn ctl(
    cmd: &'static str,
    read: &'static str,
    prefix: &'static str,
    menu: Option<u8>,
    name: &'static str,
    what: &'static str,
    group: Group,
    kind: Kind,
) -> Control {
    Control { cmd, read, prefix, menu, name, what, group, kind }
}

const fn range(lo: i32, hi: i32, step: i32, width: usize, unit: &'static str) -> Kind {
    Kind::Range { lo, hi, step, width, unit }
}

const OFF_ON: Kind = Kind::Toggle;
const RO: Kind = Kind::ReadOnly;
use Group::{Cw, Info, Menu, Radio, Receive, Tune, TxAudio};

/// The power steps by code, for `TL` (menu 55) as well as `TP`.
const POWER_CHOICES: &[(&str, &str)] = &[
    ("0", "0.3 W"),
    ("1", "0.5 W"),
    ("2", "1 W"),
    ("3", "1.2 W"),
    ("4", "1.5 W"),
    ("5", "2 W"),
    ("6", "3 W"),
    ("7", "4 W"),
    ("8", "5 W"),
    ("9", "MAX"),
];

/// Every setting of the FDM-DUO the panel shows, from the command table in
/// §6 of ELAD's manual (v2.13 for the list and menu numbers, rev 2.3 for the
/// value tables; see docs/design-decisions.md). Where the manual leaves the
/// value's encoding unclear the control is read only until it is checked on
/// a radio. Factory reset (`DF`), service mode (`SE`), firmware update
/// (`UU`), a CAT baud change (`BR`) and IQ transmit (`IQ`) are not here and
/// can't be sent (interlock #5). Kenwood's commands that the DUO accepts and
/// ignores (`AG`, `RG`, `VX`, `PC`, `IS`, `SH`/`SL` and the rest) are left
/// out, so no control looks as if it works when it doesn't.
pub const CONTROLS: &[Control] = &[
    // Radio face
    ctl("AT", "AT;", "", Some(1), "RX ATT", "Receiver input attenuator", Radio, OFF_ON),
    ctl("LP", "LP;", "", Some(2), "RX LP", "Receiver low-pass filter", Radio, OFF_ON),
    ctl(
        "AN",
        "AN;",
        "",
        Some(31),
        "ANTENNAS",
        "One antenna on RTX, or receive on the RX socket too",
        Radio,
        Kind::Choice(&[("1", "1: RTX for both"), ("2", "2: RX socket for receive")]),
    ),
    ctl(
        "TI",
        "TI;",
        "",
        Some(32),
        "TX IN",
        "Where transmit audio comes from in SSB, AM and FM",
        Radio,
        Kind::Choice(&[("0", "Microphone"), ("1", "USB audio"), ("2", "Auto")]),
    ),
    ctl("RT", "RT;", "", None, "RIT", "Receiver offset on or off", Radio, OFF_ON),
    ctl(
        "SP",
        "SP;",
        "",
        None,
        "SPLIT",
        "Transmit on VFO B",
        Radio,
        Kind::Choice(&[("0", "Off"), ("1", "On (remote)"), ("2", "On (stand-alone)")]),
    ),
    ctl(
        "FR",
        "FR;",
        "",
        None,
        "VFO",
        "Which VFO or memory the radio uses",
        Radio,
        Kind::Choice(&[("0", "A"), ("1", "B"), ("2", "Memory")]),
    ),
    // Receive: the radio's own audio
    ctl(
        "VM",
        "VM;",
        "",
        None,
        "VOLUME",
        "The radio's main volume",
        Receive,
        range(0, 100, 1, 3, ""),
    ),
    ctl(
        "VA",
        "VA;",
        "",
        Some(6),
        "AUX VOL",
        "Auxiliary audio output volume",
        Receive,
        range(0, 100, 1, 3, ""),
    ),
    ctl(
        "GC",
        "GC;",
        "",
        None,
        "AGC",
        "AGC, or manual gain",
        Receive,
        Kind::Choice(&[("0", "AGC"), ("1", "Manual")]),
    ),
    ctl(
        "GS",
        "GS;",
        "",
        None,
        "AGC SPEED / GAIN",
        "AGC speed, or the manual gain level",
        Receive,
        RO,
    ),
    ctl("NB", "NB;", "", None, "NB", "Noise blanker", Receive, OFF_ON),
    ctl(
        "NK",
        "NK;",
        "0",
        None,
        "NB LEVEL",
        "Noise blanker level, 0 off",
        Receive,
        range(0, 10, 1, 2, ""),
    ),
    ctl("NR", "NR;", "", None, "NR", "Noise reduction", Receive, OFF_ON),
    ctl(
        "NC",
        "NC;",
        "0",
        None,
        "NR LEVEL",
        "Noise reduction level, 0 off",
        Receive,
        range(0, 10, 1, 2, ""),
    ),
    ctl("NO", "NO;", "0", None, "NOTCH", "Automatic notch, 0 off", Receive, range(0, 2, 1, 2, "")),
    ctl("SQ", "SQ;", "0", None, "SQUELCH", "Squelch, 0 off", Receive, range(0, 10, 1, 2, "")),
    ctl("RF", "RF;", "", None, "RX FILTER", "Receive filter, per mode", Receive, RO),
    ctl(
        "MT",
        "MT;",
        "",
        Some(8),
        "MUTE IN TX",
        "Mute the receiver while sending CW (first) and SSB (second)",
        Receive,
        Kind::Choice(&[("00", "Neither"), ("10", "CW"), ("01", "SSB"), ("11", "CW and SSB")]),
    ),
    ctl(
        "BP",
        "BP;",
        "",
        Some(10),
        "FILBYPASS",
        "Bypass the receive pre-selection filters",
        Receive,
        OFF_ON,
    ),
    ctl(
        "AX",
        "AX;",
        "",
        Some(50),
        "TX ATT",
        "Keep the attenuator in during transmit",
        Receive,
        OFF_ON,
    ),
    // CW
    ctl("CS", "CS;", "", Some(45), "CW SPEED", "Keyer and decoder speeds", Cw, RO),
    ctl("PI", "PI;", "", None, "CW PITCH", "CW pitch", Cw, range(0, 1000, 10, 4, "Hz")),
    ctl("VT", "VT;", "", Some(11), "TONE VOL", "CW sidetone volume", Cw, range(0, 100, 1, 3, "")),
    ctl(
        "IA",
        "IA;",
        "",
        Some(41),
        "IAMBIC",
        "Iambic mode",
        Cw,
        Kind::Choice(&[("0", "A"), ("1", "B")]),
    ),
    ctl(
        "KT",
        "KT;",
        "",
        Some(39),
        "PADDLE",
        "Paddle wiring: key on tip or ring, tip dot or dash",
        Cw,
        RO,
    ),
    ctl(
        "CI",
        "CI;",
        "",
        Some(37),
        "CW IN",
        "Key, paddle, or either with DTR",
        Cw,
        Kind::Choice(&[("0", "Key"), ("1", "Paddle"), ("2", "Key + DTR"), ("3", "Paddle + DTR")]),
    ),
    ctl("CD", "CD;", "", Some(38), "CW DELAY", "Break-in delay", Cw, range(0, 1000, 10, 4, "ms")),
    ctl(
        "HT",
        "HT;",
        "",
        Some(71),
        "HOLD TIME",
        "Key hold time",
        Cw,
        range(200, 2500, 100, 4, "ms"),
    ),
    ctl(
        "ET",
        "ET;",
        "",
        Some(72),
        "REPEAT",
        "Key auto-repeat interval",
        Cw,
        range(100, 1500, 100, 4, "ms"),
    ),
    ctl(
        "CK",
        "CK;",
        "",
        Some(59),
        "AUTO CW",
        "Switch to CW when the key is pressed",
        Cw,
        Kind::Choice(&[("0", "Off"), ("1", "On, then back"), ("2", "On, stay in CW")]),
    ),
    ctl("DE", "DE;", "", Some(44), "CW DECODER", "CW decoder", Cw, OFF_ON),
    ctl("WT", "WT;", "", Some(43), "DECODE THR", "CW decoder threshold", Cw, RO),
    // Transmit audio and the transmitter
    ctl(
        "TE",
        "TE;",
        "",
        Some(30),
        "TX ENABLE",
        "Allow the radio to transmit at all",
        TxAudio,
        OFF_ON,
    ),
    ctl(
        "TT",
        "TT;",
        "",
        Some(33),
        "TX OUT",
        "Full power on the antenna, or 0 dBm on RF OUT",
        TxAudio,
        Kind::Choice(&[("0", "Antenna"), ("1", "0 dBm RF OUT")]),
    ),
    ctl(
        "TB",
        "TB;",
        "0",
        Some(35),
        "TX BW",
        "Transmit audio bandwidth",
        TxAudio,
        Kind::Choice(&[
            ("00", "50-4000"),
            ("01", "100-2700"),
            ("02", "100-3000"),
            ("03", "100-3500"),
            ("04", "100-4000"),
            ("05", "200-2700"),
            ("06", "200-3000"),
            ("07", "200-3500"),
            ("08", "200-4000"),
            ("09", "300-2700"),
            ("10", "300-3000"),
            ("11", "300-3500"),
            ("12", "300-4000"),
        ]),
    ),
    ctl(
        "MG",
        "MG;",
        "",
        Some(36),
        "MIC GAIN",
        "Microphone gain: 50 is 0 dB, half a dB a step",
        TxAudio,
        range(26, 74, 1, 3, ""),
    ),
    ctl(
        "NT",
        "NT;",
        "0",
        Some(51),
        "NOISE GATE",
        "Transmit noise gate, 0 off",
        TxAudio,
        range(0, 10, 1, 2, ""),
    ),
    ctl(
        "CG",
        "CG;",
        "0",
        Some(52),
        "COMP",
        "Speech compression, 0 off",
        TxAudio,
        range(0, 10, 1, 3, ""),
    ),
    ctl(
        "FD",
        "FD;",
        "0",
        Some(53),
        "FM DEV",
        "FM transmit deviation",
        TxAudio,
        Kind::Choice(&[("0", "2.5 kHz"), ("1", "5 kHz")]),
    ),
    // TUNE and PTT
    ctl(
        "TL",
        "TL;",
        "",
        Some(55),
        "TUNE POWER",
        "Power of the radio's TUNE carrier",
        Tune,
        Kind::Choice(POWER_CHOICES),
    ),
    ctl("TU", "TU;", "", Some(49), "TUNE TIME", "How long the radio's TUNE lasts", Tune, RO),
    ctl(
        "PT",
        "PT;",
        "",
        Some(56),
        "TUNE PTT",
        "Key PTT OUT, and so the amplifier, during the radio's TUNE",
        Tune,
        OFF_ON,
    ),
    ctl(
        "PD",
        "PD;",
        "",
        Some(57),
        "PTT DELAY",
        "PTT OUT before RF in SSB, AM and FM, for the amplifier's relay",
        Tune,
        range(0, 1000, 10, 4, "ms"),
    ),
    ctl(
        "TC",
        "TC;",
        "",
        Some(58),
        "PTT ON CW",
        "What the microphone's PTT does in CW",
        Tune,
        Kind::Choice(&[("0", "Prepare a CW message"), ("1", "Key PTT OUT")]),
    ),
    ctl("TR", "TR;", "", Some(54), "RTS", "Key PTT from the CAT port's RTS", Tune, RO),
    // Everything else in MENU
    ctl("SA", "SA;", "", Some(3), "SNAP", "Round the dial to the tuning step", Menu, RO),
    ctl("TH", "TH;", "", Some(4), "AGC TH", "AGC threshold", Menu, RO),
    ctl("QS", "QS;", "", Some(7), "QUICKSTEP", "Tuning step for QuickStep", Menu, RO),
    ctl("FM", "FM;", "", Some(14), "FM MODE", "Offer FM in the mode cycle", Menu, OFF_ON),
    ctl("RN", "RN;", "", Some(15), "RIT STEP", "RIT tuning step", Menu, RO),
    ctl("TM", "TM;", "", Some(16), "VFO TRACK", "VFO tracking", Menu, RO),
    ctl(
        "TV",
        "TV;",
        "",
        Some(47),
        "TX VIEW",
        "What the display shows while transmitting",
        Menu,
        RO,
    ),
    ctl("UD", "UD;", "", Some(48), "UP/DN ACC", "Microphone UP/DOWN acceleration", Menu, RO),
    ctl(
        "OS",
        "OS;",
        "",
        Some(60),
        "OFFSET",
        "Show the dial with a transverter offset",
        Menu,
        OFF_ON,
    ),
    ctl("OV", "OV;", "", Some(61), "OFFSET VALUE", "The transverter offset", Menu, RO),
    ctl("FF", "FF;", "", Some(62), "F4 / F5", "What the F4 and F5 keys do", Menu, RO),
    ctl(
        "BH",
        "BH;",
        "",
        Some(73),
        "TX BACKLIGHT",
        "Change the backlight colour in transmit",
        Menu,
        OFF_ON,
    ),
    ctl("LB", "LB;", "", None, "BACKLIGHT RGB", "The backlight colour for each state", Menu, RO),
    // Read only
    ctl("DT", "DT;", "", None, "MODEL", "Radio type", Info, RO),
    ctl("VS", "VS;", "", Some(84), "FIRMWARE", "Firmware versions", Info, RO),
    ctl("SN", "SN;", "", Some(83), "SERIAL", "Serial number", Info, RO),
    ctl(
        "BR",
        "BR;",
        "",
        Some(70),
        "CAT BAUD",
        "CAT baud rate; changing it here would cut the link",
        Info,
        Kind::Choice(&[("0", "9600"), ("1", "38400"), ("2", "57600"), ("3", "115200")]),
    ),
    ctl("TP", "TP;", "", Some(34), "TX POWER", "Transmit power (the 1W/3W/5W/MAX keys)", Tune, RO),
];

/// Controls that can't be sent whatever their kind says: `BR` would cut the
/// link the panel talks over.
const NEVER_SET: &[&str] = &["BR"];

pub fn control(cmd: &str) -> Option<&'static Control> {
    CONTROLS.iter().find(|c| c.cmd == cmd)
}

/// Everything read every few seconds: the settings above plus the RIT offset,
/// filters and memories' channel.
fn all_settings() -> &'static [Item] {
    static ALL: OnceLock<Vec<Item>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Item> = SETTINGS.to_vec();
        for c in CONTROLS {
            if !v.iter().any(|i| i.cmd == c.read) {
                v.push(Item { cmd: c.read, menu: c.menu, name: c.name });
            }
        }
        v.push(item("RV;", None, "RIT offset"));
        v.push(item("MC;", None, "Memory channel"));
        v.push(item("RI;", None, "RSSI"));
        v
    })
}

/// SAFETY INTERLOCK (docs/safety-interlocks.md, #5): the SET frames the link
/// may send beyond those in [`allowed`]'s first list: the dial (`FA`/`FB`,
/// eleven digits, inside the radio's 9 kHz to 54 MHz), the mode, split, the
/// VFO, RIT, a receive filter from the radio's tables, a memory recall, a
/// stored CW message's text, start (`KeyCw`) and stop, and the controls in
/// [`CONTROLS`] with a value their kind allows. The link sends none of them
/// while the radio transmits.
pub fn allowed_set(frame: &str) -> bool {
    let Some(body) = frame.strip_suffix(';') else { return false };
    if body.len() < 2 || !body.is_char_boundary(2) {
        return false;
    }
    let (k, v) = body.split_at(2);
    match k {
        "FA" | "FB" => parse_freq(v).is_some_and(|hz| (9_000..=54_000_000).contains(&hz)),
        "MD" => matches!(v, "1" | "2" | "3" | "4" | "5" | "7"),
        "RC" => v.is_empty(),
        "RU" | "RD" => v.len() == 5 && v.parse::<u32>().is_ok_and(|n| n <= 50_000),
        "RF" => {
            let mut c = v.chars();
            let m = c.next();
            let idx: Option<u32> = c.as_str().parse().ok().filter(|_| c.as_str().len() == 2);
            match (m, idx) {
                (Some('1' | '2'), Some(i)) => i <= 21,
                (Some('3' | '7'), Some(i)) => (7..=16).contains(&i),
                (Some('5'), Some(i)) => i <= 7,
                (Some('4'), Some(i)) => i <= 2,
                _ => false,
            }
        }
        "MC" => v.len() == 3 && v.parse::<u16>().is_ok_and(|n| n < 200),
        "CM" => {
            v.len() == 33
                && v.as_bytes()[0].is_ascii_digit()
                && v[1..].chars().all(crate::protocol::cw_char)
        }
        "SW" => is_cw_send(frame) || frame == CW_STOP,
        _ => !NEVER_SET.contains(&k) && control(k).is_some_and(|c| c.accepts(v)),
    }
}

/// Stop a stored CW message.
pub const CW_STOP: &str = "SW0000;";

/// `SW0nn1;`: send stored CW message nn (1 to 10).
pub fn is_cw_send(frame: &str) -> bool {
    frame.len() == 7
        && frame.starts_with("SW0")
        && frame.ends_with("1;")
        && frame[3..5].parse::<u8>().is_ok_and(|n| (1..=10).contains(&n))
}

/// Whether `frame` is one of the READ frames above.
pub fn is_read(frame: &str) -> bool {
    TELEMETRY.contains(&frame)
        || METER.contains(&frame)
        || DIAL.contains(&frame)
        || all_settings().iter().any(|i| i.cmd == frame)
}

/// The radio's own TUNE carrier: `TX2;`.
pub const TUNE: &str = "TX2;";
/// An ordinary transmission, of whatever audio is on the radio's transmit
/// input (menu 32). `TX0;` and `TX1;` are the same and are never sent.
pub const KEY_AUDIO: &str = "TX;";
pub const UNKEY: &str = "RX;";
/// Menu 32 `TX IN` to the USB audio input.
pub const USB_AUDIO: &str = "TI1;";

/// Whether `frame` may be sent at all, as `kind`.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #5): the link sends only the
/// READ frames above, menu 34's power steps (`TP00;` to `TP09;`), the TUNE
/// carrier (`TX2;`), the ordinary transmission (`TX;`), menu 32 to USB audio
/// (`TI1;`) and unkey (`RX;`). Anything else is refused, even from this code:
/// `RC;` clears the RIT offset with no parameter at all, so "a bare command
/// is a read" is not safe. When the carrier may be keyed is interlock #6;
/// `TX;` also needs interlock #7, in the link.
pub fn allowed(frame: &str, kind: FrameKind) -> bool {
    match kind {
        FrameKind::Read => is_read(frame),
        FrameKind::SetPower => (0..=TX_POWER_MAX).any(|c| frame == format!("TP{c:02};")),
        FrameKind::Key => frame == TUNE,
        FrameKind::KeyAudio => frame == KEY_AUDIO,
        FrameKind::SetInput => frame == USB_AUDIO,
        FrameKind::Unkey => frame == UNKEY || frame == CW_STOP,
        FrameKind::SetSetting => allowed_set(frame) && !frame.starts_with("SW"),
        FrameKind::KeyCw => is_cw_send(frame),
    }
}

/// The share of full transmit audio, as a power fraction, that Drive 100%
/// sends the FDM-DUO, as in SDRoxide (`DUO_AUDIO_POWER_CEILING` there).
///
/// AJ3G measured it on 20 m on 18 Oct 2026 with an LP-100A, menu 34 at MAX:
/// 0.94 W at 1% of full audio, 2.08 W at 5%, 3.72 W at 10%, 5.39 W at 15%,
/// 6.85 W at 20%, then 8.0 to 8.3 W from 24% up. A straight line to about
/// 8 W at 24%, and nothing above it, so Drive is scaled into that 24%: 100%
/// is about the full output of the menu 34 setting, 50% about half. With menu
/// 34 at 5 W the line was about 0.7 times as steep and capped near 6 W, with
/// the knee in the same place.
pub const AUDIO_CEILING: f32 = 0.24;

/// Menu 34's MAX code.
pub const TX_POWER_MAX: u8 = 9;

/// Menu 34 `TX POWER`, as power steps. MAX is "more than 5 W by an amount
/// ELAD does not publish"; AJ3G measured the DUO's ceiling at 8.0 to 8.3 W
/// on 20 m (18 Oct 2026, LP-100A), so it is taken as 8.3 W against the
/// amplifier's drive limit.
pub const POWER_STEPS: [PowerStep; 10] = [
    PowerStep { label: "0.3 W", watts: 0.3, code: 0 },
    PowerStep { label: "0.5 W", watts: 0.5, code: 1 },
    PowerStep { label: "1 W", watts: 1.0, code: 2 },
    PowerStep { label: "1.2 W", watts: 1.2, code: 3 },
    PowerStep { label: "1.5 W", watts: 1.5, code: 4 },
    PowerStep { label: "2 W", watts: 2.0, code: 5 },
    PowerStep { label: "3 W", watts: 3.0, code: 6 },
    PowerStep { label: "4 W", watts: 4.0, code: 7 },
    PowerStep { label: "5 W", watts: 5.0, code: 8 },
    PowerStep { label: "MAX", watts: 8.3, code: 9 },
];

/// The prefix of the answer to a READ frame: its command letters.
pub fn reply_prefix(frame: &str) -> &str {
    &frame[..2.min(frame.len())]
}

/// `FP` / `RP` body: a flag (space or `!`), then six digits of milliwatts.
pub fn parse_watts(body: &str) -> Option<crate::Watts> {
    let mut chars = body.chars();
    let valid = match chars.next()? {
        ' ' => true,
        '!' => false,
        _ => return None,
    };
    let mw: u32 = chars.as_str().trim().parse().ok()?;
    Some(crate::Watts { w: mw as f32 / 1000.0, valid })
}

/// `WR` body: the high-SWR trip flag, a space or `!`, then `nn.nn`.
pub fn parse_swr(body: &str) -> Option<crate::Swr> {
    let mut chars = body.chars();
    let tripped = chars.next()? == '1';
    let valid = match chars.next()? {
        ' ' => true,
        '!' => false,
        _ => return None,
    };
    let ratio: f32 = chars.as_str().trim().parse().ok()?;
    // Below 1:1 is a misread frame, not a match.
    Some(crate::Swr { ratio, valid: valid && ratio >= 1.0, tripped })
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
            let code: u8 = body.trim().parse().ok()?;
            POWER_STEPS.iter().find(|p| p.code == code).map(|p| p.label.to_string())
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
        k => control(k).map(|c| c.label(c.value_of(body))),
    }
}

/// The FDM-DUO's dialect.
#[derive(Default)]
pub struct FdmDuo {
    buf: String,
}

impl Protocol for FdmDuo {
    fn ping(&self) -> Frame {
        Frame::read(PING)
    }

    fn telemetry(&self) -> Vec<Frame> {
        TELEMETRY.iter().map(|f| Frame::read(*f)).collect()
    }

    fn meter(&self) -> Vec<Frame> {
        METER.iter().map(|f| Frame::read(*f)).collect()
    }

    fn dial(&self) -> Vec<Frame> {
        DIAL.iter().map(|f| Frame::read(*f)).collect()
    }

    fn settings(&self) -> &'static [Item] {
        all_settings()
    }

    fn read(&self, item: &Item) -> Frame {
        Frame::read(item.cmd)
    }

    fn reply_key(&self, frame: &Frame) -> Option<String> {
        (frame.kind == FrameKind::Read)
            .then(|| reply_prefix(&String::from_utf8_lossy(&frame.bytes)).to_string())
    }

    fn deframe(&mut self, bytes: &[u8]) -> Vec<Incoming> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(i) = self.buf.find(';') {
            let msg: String = self.buf.drain(..=i).collect();
            let msg = msg.trim_end_matches(';').trim_start_matches(['\r', '\n']);
            if msg == "?" {
                out.push(Incoming::Refused);
            } else if msg.len() >= 2 && msg.is_char_boundary(2) {
                let (k, b) = msg.split_at(2);
                out.push(Incoming::Reply(Reply { key: k.to_string(), body: b.to_string() }));
            }
        }
        out
    }

    fn apply(&self, s: &mut RigStatus, r: &Reply) {
        let b = r.body.as_str();
        match r.key.as_str() {
            "FP" => s.fwd = parse_watts(b),
            "RP" => s.rfl = parse_watts(b),
            "WR" => s.swr = parse_swr(b),
            "FA" => s.freq_hz = parse_freq(b).or(s.freq_hz),
            "MD" => s.mode = b.chars().next().and_then(mode_name),
            "TP" => s.power_code = b.trim().parse().ok().filter(|c| *c <= TX_POWER_MAX),
            "TI" => s.tx_input = describe("TI", b),
            "FB" => s.freq_b_hz = parse_freq(b).or(s.freq_b_hz),
            "SP" => s.split = b.trim().parse::<u8>().ok().map(|v| v != 0),
            "FR" => s.vfo = b.trim().parse().ok(),
            "RT" => s.rit_on = b.trim().parse::<u8>().ok().map(|v| v != 0),
            "GI" => {
                // P1 RIT, P2, P3 memory (3), P4 RX/TX/TUNE, P5 mode, P6 VFO,
                // P7 split, P8 (4).
                // Only a block of exactly the documented 13 digits is read:
                // a longer or shorter one would shift every field, and a
                // memory or mode digit read as P4 would say "transmitting"
                // for good.
                let c: Vec<char> = b.trim().chars().collect();
                if c.len() == 13 && c.iter().all(|ch| ch.is_ascii_digit()) {
                    s.rit_on = c[0].to_digit(10).map(|v| v != 0);
                    s.tx_state = c[5].to_digit(10).map(|v| v as u8);
                    s.vfo = c[7].to_digit(10).map(|v| v as u8);
                    s.split = c[8].to_digit(10).map(|v| v != 0);
                } else {
                    s.tx_state = None;
                }
            }
            _ => {}
        }
        s.values.insert(r.key.clone(), b.to_string());
    }

    fn describe(&self, r: &Reply) -> Option<String> {
        describe(&r.key, &r.body)
    }

    fn raw(&self, r: &Reply) -> String {
        format!("{}{}", r.key, r.body)
    }

    fn allowed(&self, frame: &Frame) -> bool {
        std::str::from_utf8(&frame.bytes).is_ok_and(|f| allowed(f, frame.kind))
    }

    fn power_steps(&self) -> &'static [PowerStep] {
        &POWER_STEPS
    }

    fn power_name(&self) -> &'static str {
        "TX POWER (menu 34)"
    }

    fn set_power(&self, code: u8) -> Frame {
        Frame::new(format!("TP{:02};", code.min(TX_POWER_MAX)), FrameKind::SetPower)
    }

    fn key_tune(&self) -> Frame {
        Frame::new(TUNE, FrameKind::Key)
    }

    fn unkey(&self) -> Frame {
        Frame::new(UNKEY, FrameKind::Unkey)
    }

    fn key_audio(&self) -> Option<Frame> {
        Some(Frame::new(KEY_AUDIO, FrameKind::KeyAudio))
    }

    fn audio_reads(&self) -> Vec<Frame> {
        vec![Frame::read("TI;"), Frame::read("MD;")]
    }

    /// Menu 32 on USB audio, so the microphone is not what goes out, and the
    /// mode USB or LSB, where a tone is a single clean carrier. In CW, FM or
    /// AM, `TX;` would send something else.
    fn audio_ready(&self, s: &RigStatus) -> Result<(), String> {
        match s.tx_input.as_deref() {
            Some("USB audio") => {}
            Some(other) => {
                return Err(format!(
                    "The radio's TX IN (menu 32) is on {other}: TUNE would send that. Set it to \
                     USB audio (the USB AUDIO button does)."
                ));
            }
            None => return Err("The radio's TX IN (menu 32) hasn't been read.".into()),
        }
        match s.mode {
            Some("USB" | "LSB") => Ok(()),
            Some(m) => Err(format!("The radio is in {m}. TUNE with drive needs USB or LSB.")),
            None => Err("The radio's mode hasn't been read.".into()),
        }
    }

    fn audio_input(&self) -> Option<Frame> {
        Some(Frame::new(USB_AUDIO, FrameKind::SetInput))
    }

    fn audio_ceiling(&self) -> f32 {
        AUDIO_CEILING
    }

    fn controls(&self) -> &'static [Control] {
        CONTROLS
    }

    fn set_control(&self, c: &Control, value: &str, last: Option<&str>) -> Result<Frame, String> {
        if NEVER_SET.contains(&c.cmd) {
            return Err(format!("{} isn't changed from the panel", c.name));
        }
        let last = last.map(|l| c.value_of(l));
        Ok(Frame::new(format!("{}{};", c.cmd, c.encode(value, last)?), FrameKind::SetSetting))
    }

    fn set_freq(&self, vfo: Vfo, hz: u64) -> Option<Frame> {
        let k = if vfo == Vfo::A { "FA" } else { "FB" };
        (9_000..=54_000_000)
            .contains(&hz)
            .then(|| Frame::new(format!("{k}{hz:011};"), FrameKind::SetSetting))
    }

    fn set_mode(&self, mode: &str) -> Option<Frame> {
        let d = match mode {
            "LSB" => '1',
            "USB" => '2',
            "CW" => '3',
            "FM" => '4',
            "AM" => '5',
            "CW-R" | "CWR" => '7',
            _ => return None,
        };
        Some(Frame::new(format!("MD{d};"), FrameKind::SetSetting))
    }

    fn modes(&self) -> &'static [&'static str] {
        &["LSB", "USB", "CW", "CW-R", "AM", "FM"]
    }

    fn select_vfo(&self, vfo: Vfo) -> Option<Frame> {
        Some(Frame::new(if vfo == Vfo::A { "FR0;" } else { "FR1;" }, FrameKind::SetSetting))
    }

    /// `SP1;` (split, remote) on, and `SP0;`, the only way off: `FR0;`
    /// doesn't cancel it.
    fn set_split(&self, on: bool) -> Option<Frame> {
        Some(Frame::new(if on { "SP1;" } else { "SP0;" }, FrameKind::SetSetting))
    }

    fn set_rit(&self, hz: i32) -> Vec<Frame> {
        let n = hz.unsigned_abs().min(50_000);
        if n == 0 {
            return vec![Frame::new("RC;", FrameKind::SetSetting)];
        }
        let k = if hz > 0 { "RU" } else { "RD" };
        vec![
            Frame::new("RC;", FrameKind::SetSetting),
            Frame::new(format!("{k}{n:05};"), FrameKind::SetSetting),
        ]
    }

    fn set_rit_on(&self, on: bool) -> Option<Frame> {
        Some(Frame::new(if on { "RT1;" } else { "RT0;" }, FrameKind::SetSetting))
    }

    fn filters(&self, mode: &str) -> Vec<u32> {
        filter_table(mode).into_iter().map(|(_, w)| w).collect()
    }

    fn set_filter(&self, mode: &str, hz: u32) -> Option<Frame> {
        let d = self.set_mode(mode)?.bytes[2] as char;
        let (idx, _) = filter_table(mode).into_iter().min_by_key(|(_, w)| w.abs_diff(hz))?;
        Some(Frame::new(format!("RF{d}{idx:02};"), FrameKind::SetSetting))
    }

    fn recall_memory(&self, n: u16) -> Option<Frame> {
        (n < 200).then(|| Frame::new(format!("MC{n:03};"), FrameKind::SetSetting))
    }

    fn cw_send(&self, n: u8) -> Option<Frame> {
        (1..=10).contains(&n).then(|| Frame::new(format!("SW0{n:02}1;"), FrameKind::KeyCw))
    }

    fn cw_stop(&self) -> Option<Frame> {
        Some(Frame::new(CW_STOP, FrameKind::Unkey))
    }

    fn cw_store(&self, n: u8, text: &str) -> Option<Frame> {
        let t = text.to_ascii_uppercase();
        ((1..=10).contains(&n)
            && t.chars().count() <= 32
            && t.chars().all(crate::protocol::cw_char))
        .then(|| Frame::new(format!("CM{}{t:<32};", n - 1), FrameKind::SetSetting))
    }
}

/// `RF`'s index and width in Hz for a mode, narrowest first. FM's three
/// positions aren't widths, so FM has none here.
fn filter_table(mode: &str) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = match mode {
        "LSB" | "USB" => RF_SSB_HZ
            .iter()
            .enumerate()
            .map(|(i, w)| (i as u32, *w))
            .chain(RF_SSB_DATA_HZ.iter().copied())
            .collect(),
        "CW" | "CW-R" | "CWR" => RF_CW.to_vec(),
        "AM" => RF_AM_HZ.iter().enumerate().map(|(i, w)| (i as u32, *w)).collect(),
        _ => Vec::new(),
    };
    v.sort_by_key(|(_, w)| *w);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Swr, Watts};

    #[test]
    fn only_reads_menu_34_menu_32_to_usb_and_keying_can_be_sent() {
        use FrameKind::*;
        // Keying only as what it is; settings only as settings.
        for f in ["TX0;", "TX1;", "RC;", "AN1;", "TI0;", "TI2;", "FA00014074000;", "TP10;", "TP9;"]
        {
            for k in [Key, KeyAudio, SetPower, SetInput, Unkey, KeyCw, Read] {
                assert!(!allowed(f, k), "{f} must be refused as {k:?}");
            }
        }
        for (f, k) in [
            ("TX2;", Key),
            ("TX;", KeyAudio),
            ("TI1;", SetInput),
            ("RX;", Unkey),
            ("TP00;", SetPower),
            ("TP09;", SetPower),
        ] {
            assert!(allowed(f, k), "{f}");
            assert!(!is_read(f));
        }
        let d = FdmDuo::default();
        assert_eq!(d.key_tune(), Frame::new("TX2;", FrameKind::Key));
        assert_eq!(d.unkey().kind, FrameKind::Unkey);
        assert_eq!(d.set_power(9).bytes, b"TP09;");
        assert_eq!(d.set_power(42).bytes, b"TP09;");
        assert!(d.reply_key(&d.key_tune()).is_none());
        assert_eq!(d.key_audio(), Some(Frame::new("TX;", FrameKind::KeyAudio)));
        assert!(d.reply_key(&d.key_audio().unwrap()).is_none());
        for f in TELEMETRY
            .iter()
            .chain(METER.iter())
            .chain(DIAL.iter())
            .chain(SETTINGS.iter().map(|i| &i.cmd))
        {
            assert!(is_read(f));
            // A READ ends at its command letters (SM carries its fixed 0).
            assert!(f.len() <= 4 && f.ends_with(';'), "{f}");
            assert!(!f.starts_with("TX") && !f.starts_with("RX"));
        }
        assert!(is_read(PING));
    }

    #[test]
    fn answers_split_across_reads_are_put_back_together() {
        let mut d = FdmDuo::default();
        assert!(d.deframe(b"FP 0060").is_empty());
        assert_eq!(
            d.deframe(b"91;?;SM00011;"),
            vec![
                Incoming::Reply(Reply { key: "FP".into(), body: " 006091".into() }),
                Incoming::Refused,
                Incoming::Reply(Reply { key: "SM".into(), body: "00011".into() }),
            ]
        );
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

    #[test]
    fn tune_with_drive_needs_usb_audio_and_a_sideband() {
        let d = FdmDuo::default();
        let mut s = RigStatus::default();
        let r = |k: &str, b: &str| Reply { key: k.into(), body: b.into() };
        assert!(d.audio_ready(&s).is_err());
        d.apply(&mut s, &r("TI", "0"));
        d.apply(&mut s, &r("MD", "2"));
        assert!(d.audio_ready(&s).unwrap_err().contains("Microphone"));
        d.apply(&mut s, &r("TI", "2"));
        assert!(d.audio_ready(&s).is_err(), "Auto may still take the microphone");
        d.apply(&mut s, &r("TI", "1"));
        assert_eq!(d.audio_ready(&s), Ok(()));
        d.apply(&mut s, &r("MD", "3"));
        assert!(d.audio_ready(&s).unwrap_err().contains("CW"));
        d.apply(&mut s, &r("MD", "1"));
        assert_eq!(d.audio_ready(&s), Ok(()));
    }

    #[test]
    fn settings_go_only_as_settings_and_only_with_values_their_controls_take() {
        use FrameKind::*;
        for f in [
            "FA00014074000;",
            "FB00007074000;",
            "MD2;",
            "MD7;",
            "SP0;",
            "SP1;",
            "FR1;",
            "RC;",
            "RU00500;",
            "RD12000;",
            "RF213;",
            "RF313;",
            "RF507;",
            "MC042;",
            "AT1;",
            "AN2;",
            "TI0;",
            "VM050;",
            "NK005;",
            "MG050;",
            "PD0030;",
            "TL9;",
            "PT0;",
            "TB005;",
            "CG0003;",
            "HT1200;",
            "MT11;",
        ] {
            assert!(allowed(f, SetSetting), "{f}");
            assert!(!allowed(f, Key) && !allowed(f, KeyAudio), "{f}");
        }
        for f in [
            // Factory reset, service mode, firmware update, IQ transmit, baud.
            "DF15214;",
            "SE1;",
            "UU1;",
            "IQ1;",
            "BR3;",
            // Kenwood look-alikes the DUO ignores.
            "AG0100;",
            "RG100;",
            "VX1;",
            "PC050;",
            // Off the dial, the wrong width, values the control doesn't take.
            "FA00000001000;",
            "FA00060000000;",
            "FA14074000;",
            "MD6;",
            "MD0;",
            "RF222;",
            "MC200;",
            "RU60000;",
            "MG080;",
            "PD2000;",
            "HT1250;",
            "AT2;",
            "TB013;",
            // Read-only controls.
            "TU030;",
            "QS05;",
            "GS02;",
        ] {
            assert!(!allowed(f, SetSetting), "{f} must be refused");
        }
        // A stored CW message transmits: only as KeyCw, and stop as Unkey.
        assert!(allowed("SW0031;", KeyCw) && !allowed("SW0031;", SetSetting));
        assert!(!allowed("SW0111;", KeyCw) && !allowed("SW0001;", KeyCw));
        assert!(allowed(CW_STOP, Unkey) && !allowed(CW_STOP, SetSetting));
    }

    #[test]
    fn the_set_frames_are_the_ones_the_table_allows() {
        let d = FdmDuo::default();
        let ok = |f: Option<Frame>| {
            let f = f.unwrap();
            assert!(d.allowed(&f), "{:?}", String::from_utf8_lossy(&f.bytes));
            String::from_utf8(f.bytes).unwrap()
        };
        assert_eq!(ok(d.set_freq(Vfo::A, 14_074_000)), "FA00014074000;");
        assert_eq!(ok(d.set_freq(Vfo::B, 7_038_600)), "FB00007038600;");
        assert!(d.set_freq(Vfo::A, 60_000_000).is_none());
        assert_eq!(ok(d.set_mode("CW-R")), "MD7;");
        assert_eq!(ok(d.set_split(false)), "SP0;");
        assert_eq!(ok(d.set_filter("USB", 2450)), "RF208;");
        assert_eq!(ok(d.set_filter("USB", 290)), "RF219;");
        assert_eq!(ok(d.set_filter("CW", 480)), "RF313;");
        assert_eq!(ok(d.recall_memory(7)), "MC007;");
        assert_eq!(ok(d.cw_send(3)), "SW0031;");
        assert_eq!(ok(d.cw_stop()), "SW0000;");
        assert_eq!(ok(d.cw_store(1, "cq de aj3g")), format!("CM0{:<32};", "CQ DE AJ3G"));
        let rit: Vec<String> = d
            .set_rit(-1200)
            .into_iter()
            .map(|f| {
                assert!(d.allowed(&f));
                String::from_utf8(f.bytes).unwrap()
            })
            .collect();
        assert_eq!(rit, ["RC;", "RD01200;"]);
        // A number is padded to the width the radio answered with.
        let pd = control("PD").unwrap();
        assert_eq!(
            String::from_utf8(d.set_control(pd, "30", Some("0000")).unwrap().bytes).unwrap(),
            "PD0030;"
        );
        let cg = control("CG").unwrap();
        assert_eq!(
            String::from_utf8(d.set_control(cg, "3", Some("0000")).unwrap().bytes).unwrap(),
            "CG0003;"
        );
        assert!(d.set_control(control("BR").unwrap(), "3", None).is_err());
        assert!(d.set_control(control("TU").unwrap(), "30", None).is_err());
        for c in CONTROLS {
            assert!(is_read(c.read), "{} must be readable", c.cmd);
        }
    }

    #[test]
    fn gi_carries_transmit_vfo_split_and_rit() {
        let d = FdmDuo::default();
        let mut s = RigStatus::default();
        d.apply(&mut s, &Reply { key: "GI".into(), body: "1000012110000".into() });
        assert_eq!(
            (s.rit_on, s.tx_state, s.vfo, s.split),
            (Some(true), Some(1), Some(1), Some(true))
        );
        assert!(s.transmitting());
        assert!(s.tx_reason().is_some_and(|r| r.contains("GI 1000012110000")));
        d.apply(&mut s, &Reply { key: "GI".into(), body: "0000002000000".into() });
        assert_eq!(s.tx_state, Some(0));
        assert!(!s.transmitting());
        assert_eq!(s.value("GI"), Some("0000002000000"));
        assert_eq!(s.tx_reason(), None);
    }

    #[test]
    fn a_gi_block_of_another_length_is_not_read_as_transmitting() {
        let d = FdmDuo::default();
        let mut s = RigStatus::default();
        // One digit short or long shifts the mode digit into P4.
        for body in ["000002000000", "00000002000000", "00000A2000000"] {
            d.apply(&mut s, &Reply { key: "GI".into(), body: body.into() });
            assert_eq!(s.tx_state, None, "{body}");
            assert!(!s.transmitting(), "{body}");
        }
    }
}
