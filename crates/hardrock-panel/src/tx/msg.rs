//! FT8 messages as text: reading a decoded line into who it is from and to
//! (for the sequencer and the decode list), and packing a line the panel is
//! to send into the 77 bits mfsk-core encodes.
//!
//! The reading follows SDRoxide's `Decode` fields
//! (sdroxide-types/src/digi.rs, GPL-3.0-or-later, like this program).

use mfsk_core::msg::wsjt77;

/// One decoded FT8 line, as the sequencer reads it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Decode {
    /// UTC of the start of the slot it was heard in, s.
    pub slot_utc: i64,
    pub snr_db: i16,
    pub dt: f32,
    /// Its audio offset from the dial, Hz.
    pub audio_hz: f32,
    pub message: String,
    /// Who it is addressed to; `None` for a CQ or free text.
    pub to: Option<String>,
    /// Who sent it.
    pub from: Option<String>,
    /// The 4-character grid it carried, if that was its payload.
    pub grid: Option<String>,
    pub is_cq: bool,
    /// The word between CQ and the call: DX, a continent, a prefix or an
    /// activity (POTA, TEST); `None` for a plain CQ.
    pub cq_to: Option<String>,
    /// 13-character free text, addressed to no one.
    pub free_text: bool,
}

/// A callsign token as decoded: a hashed call `<W1AW>` is the call inside.
fn call_of(t: &str) -> String {
    t.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(t).to_ascii_uppercase()
}

/// True for a 4-character Maidenhead grid ("FN20"), not "RR73".
pub(crate) fn is_grid4(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 4
        && (b'A'..=b'R').contains(&b[0].to_ascii_uppercase())
        && (b'A'..=b'R').contains(&b[1].to_ascii_uppercase())
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && !t.eq_ignore_ascii_case("RR73")
}

/// Something shaped like a callsign: letters and digits (and `/`), with a
/// digit and a letter in it.
fn looks_like_call(t: &str) -> bool {
    let t = t.trim_start_matches('<').trim_end_matches('>');
    t.len() >= 3
        && t.len() <= 13
        && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'/')
        && t.bytes().any(|b| b.is_ascii_digit())
        && t.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Read a decoded line.
pub(crate) fn parse(
    text: &str,
    slot_utc: i64,
    snr_db: i16,
    dt: f32,
    audio_hz: f32,
) -> Decode {
    let mut d = Decode {
        slot_utc,
        snr_db,
        dt,
        audio_hz,
        message: text.trim().to_string(),
        ..Default::default()
    };
    let t: Vec<&str> = text.split_whitespace().collect();
    if t.first() == Some(&"CQ") {
        d.is_cq = true;
        // "CQ W1AW FN31", "CQ DX W1AW FN31", "CQ POTA W1AW".
        let (mod_, call_i) = match t.get(1) {
            Some(m) if t.len() >= 3 && !looks_like_call(m) || t.len() >= 4 => (Some(*m), 2),
            _ => (None, 1),
        };
        d.cq_to = mod_.map(|m| m.to_ascii_uppercase());
        d.from = t.get(call_i).filter(|c| looks_like_call(c)).map(|c| call_of(c));
        d.grid = t.get(call_i + 1).filter(|g| is_grid4(g)).map(|g| g.to_ascii_uppercase());
        return d;
    }
    if t.len() >= 2 && looks_like_call(t[0]) && looks_like_call(t[1]) {
        d.to = Some(call_of(t[0]));
        d.from = Some(call_of(t[1]));
        d.grid = t.get(2).filter(|g| is_grid4(g)).map(|g| g.to_ascii_uppercase());
        return d;
    }
    d.free_text = true;
    d
}

/// Whether a CQ is open to us: a plain CQ, an activity (POTA, TEST), our
/// continent, DX when we are on another continent from them, or a prefix
/// our call starts with. `my_cont` and `their_cont` are "NA", "EU" and the
/// like, when known.
pub(crate) fn cq_includes(
    cq_to: Option<&str>,
    my_call: &str,
    my_cont: Option<&str>,
    their_cont: Option<&str>,
) -> bool {
    let Some(to) = cq_to else { return true };
    let to = to.to_ascii_uppercase();
    const CONTINENTS: [&str; 7] = ["AF", "AN", "AS", "EU", "NA", "OC", "SA"];
    if CONTINENTS.contains(&to.as_str()) {
        return my_cont.is_none_or(|m| m == to);
    }
    if to == "DX" {
        return match (my_cont, their_cont) {
            (Some(m), Some(t)) => m != t,
            _ => true,
        };
    }
    // Activities and contests anyone may answer.
    const OPEN: [&str; 13] = [
        "POTA", "SOTA", "WWFF", "IOTA", "BOTA", "LOTA", "TEST", "FD", "RU", "WW", "ALL",
        "QRP", "FT",
    ];
    if OPEN.contains(&to.as_str()) || to.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    my_call.to_ascii_uppercase().starts_with(&to)
}

/// Format an SNR as an FT8 report: `-13`, `+02`, `+00`.
pub(crate) fn fmt_report(db: i16) -> String {
    let db = db.clamp(-30, 49);
    if db < 0 { format!("-{:02}", -db) } else { format!("+{db:02}") }
}

/// Pack `text` into FT8's 77 bits: a standard message
/// (`<call> <call> [grid|report|R report|RRR|RR73|73]`, `CQ [mod] <call>
/// [grid]`), a message with one non-standard call, or free text of up to 13
/// characters. `None` when it can't be sent as it is (the panel then says
/// so and sends nothing).
pub(crate) fn pack(text: &str) -> Option<[u8; 77]> {
    let text = text.trim().to_ascii_uppercase();
    let t: Vec<&str> = text.split_whitespace().collect();
    let std = |c: &str| wsjt77::is_standard_callsign(c);
    match t.as_slice() {
        ["CQ", call] if std(call) => wsjt77::pack77("CQ", call, ""),
        ["CQ", call, grid] if std(call) && is_grid4(grid) => wsjt77::pack77("CQ", call, grid),
        ["CQ", m, call, grid] if std(call) && is_grid4(grid) => {
            // A directed CQ: "CQ DX", "CQ POTA" ride in the first call field.
            wsjt77::pack77(&format!("CQ {m}"), call, grid)
        }
        ["CQ", call] | ["CQ", call, _] => wsjt77::pack77_type4(call, "", "", true),
        [a, b] if std(a) && std(b) => wsjt77::pack77(a, b, ""),
        [a, b, r] if std(a) && std(b) => wsjt77::pack77(a, b, r),
        [a, b, r] if std(a) ^ std(b) && matches!(*r, "RRR" | "RR73" | "73") => {
            let (non, s) = if std(a) { (*b, *a) } else { (*a, *b) };
            wsjt77::pack77_type4(non, s, r, false)
        }
        _ if text.len() <= 13 => wsjt77::pack77_free_text(&text),
        _ => None,
    }
    .filter(|bits| wsjt77::unpack77(bits).is_some())
}

/// What `text` will read as on the air, once packed: free text is cut to
/// 13 characters, a hashed call is shown in brackets.
#[cfg(test)]
pub(crate) fn on_air(text: &str) -> Option<String> {
    pack(text).and_then(|b| wsjt77::unpack77(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_lines_are_read_for_who_and_what() {
        let d = parse("CQ W1AW FN31", 0, -10, 0.1, 1500.0);
        assert!(d.is_cq && d.to.is_none());
        assert_eq!((d.from.as_deref(), d.grid.as_deref(), d.cq_to.as_deref()), (Some("W1AW"), Some("FN31"), None));
        let d = parse("CQ DX ZS6ABC KG33", 0, -2, 0.0, 950.0);
        assert_eq!((d.cq_to.as_deref(), d.from.as_deref()), (Some("DX"), Some("ZS6ABC")));
        let d = parse("CQ POTA KD9QRS EN52", 0, -8, 0.3, 640.0);
        assert_eq!((d.cq_to.as_deref(), d.from.as_deref()), (Some("POTA"), Some("KD9QRS")));
        let d = parse("AJ3G JA1ABC PM95", 0, -14, 0.2, 1840.0);
        assert_eq!((d.to.as_deref(), d.from.as_deref(), d.grid.as_deref()), (Some("AJ3G"), Some("JA1ABC"), Some("PM95")));
        let d = parse("AJ3G JA1ABC RR73", 0, -14, 0.2, 1840.0);
        assert_eq!(d.grid, None);
        let d = parse("<AJ3G> JA1ABC -10", 0, -14, 0.2, 1840.0);
        assert_eq!(d.to.as_deref(), Some("AJ3G"));
        assert!(parse("TNX 73 GL", 0, 0, 0.0, 0.0).free_text);
    }

    #[test]
    fn a_directed_cq_is_open_only_to_whom_it_names() {
        assert!(cq_includes(None, "AJ3G", Some("NA"), Some("EU")));
        assert!(cq_includes(Some("DX"), "AJ3G", Some("NA"), Some("EU")));
        assert!(!cq_includes(Some("DX"), "AJ3G", Some("NA"), Some("NA")));
        assert!(cq_includes(Some("NA"), "AJ3G", Some("NA"), Some("EU")));
        assert!(!cq_includes(Some("EU"), "AJ3G", Some("NA"), Some("EU")));
        assert!(cq_includes(Some("POTA"), "AJ3G", Some("NA"), Some("NA")));
        assert!(!cq_includes(Some("JA"), "AJ3G", Some("NA"), Some("AS")));
        assert!(cq_includes(Some("AJ"), "AJ3G", Some("NA"), Some("NA")));
    }

    #[test]
    fn what_the_sequencer_sends_packs_and_reads_back() {
        for m in [
            "CQ AJ3G FN20",
            "CQ DX AJ3G FN20",
            "JA1ABC AJ3G FN20",
            "JA1ABC AJ3G -12",
            "JA1ABC AJ3G R-12",
            "JA1ABC AJ3G RR73",
            "JA1ABC AJ3G RRR",
            "JA1ABC AJ3G 73",
            "TNX 73 GL",
        ] {
            assert_eq!(on_air(m).as_deref(), Some(m), "{m}");
        }
        assert_eq!(fmt_report(-7), "-07");
        assert_eq!(fmt_report(3), "+03");
        // Too long for free text and not a standard message.
        assert!(pack("THIS IS FAR TOO LONG TO SEND").is_none());
    }
}
