//! ADIF (`.adi`) reading and writing, and reading a text file in whatever
//! encoding a logger wrote it in.
//!
//! Ported from SDRoxide's `sdroxide-types/src/digi.rs` (its ADIF reader and
//! writer), `sdroxide-types/src/text.rs` (the Windows code pages) and
//! `sdroxide-ui/src/download.rs` (`decode_text`), all GPL-3.0-or-later
//! (https://github.com/dividebysandwich/sdroxide).

use super::{Qsl, QsoRecord};

/// The fields this program reads into [`QsoRecord`]'s own fields; every other
/// one is kept in [`QsoRecord::extra`].
const KNOWN: &[&str] = &[
    "CALL",
    "QSO_DATE",
    "TIME_ON",
    "QSO_DATE_OFF",
    "TIME_OFF",
    "BAND",
    "MODE",
    "SUBMODE",
    "FREQ",
    "GRIDSQUARE",
    "RST_SENT",
    "RST_RCVD",
    "NAME",
    "QTH",
    "STATE",
    "CNTY",
    "COUNTRY",
    "DXCC",
    "CQZ",
    "ITUZ",
    "CONT",
    "TX_PWR",
    "COMMENT",
    "QSL_VIA",
    "STATION_CALLSIGN",
    "MY_GRIDSQUARE",
    "LOTW_QSL_SENT",
    "LOTW_QSLSDATE",
    "LOTW_QSL_RCVD",
    "LOTW_QSLRDATE",
    "EQSL_QSL_SENT",
    "EQSL_QSLSDATE",
    "EQSL_QSL_RCVD",
    "EQSL_QSLRDATE",
    "QSL_SENT",
    "QSLSDATE",
    "QSL_RCVD",
    "QSLRDATE",
];

/// The header the master file and every export start with.
fn header() -> String {
    let ver = env!("CARGO_PKG_VERSION");
    format!(
        "ADIF from hardrock-panel\r\n{}{}{}<EOH>\r\n",
        field("ADIF_VER", "3.1.4"),
        field("PROGRAMID", "hardrock-panel"),
        field("PROGRAMVERSION", ver)
    )
}

/// One field, its length counted in bytes as ADIF says.
fn field(name: &str, value: &str) -> String {
    format!("<{name}:{}>{value}", value.len())
}

/// `YYYYMMDD` and `HHMMSS` for a Unix time.
pub(crate) fn date_time(unix: i64) -> (String, String) {
    let (y, m, d, h, mi, s) = crate::decodelog::civil(unix);
    (format!("{y:04}{m:02}{d:02}"), format!("{h:02}{mi:02}{s:02}"))
}

/// A whole ADIF document: the header and one record a line.
pub(crate) fn document<'a>(recs: impl Iterator<Item = &'a QsoRecord>) -> String {
    let mut out = header();
    for r in recs {
        out.push_str(&record(r));
        out.push_str("\r\n");
    }
    out
}

/// One QSO as a bare ADIF record ending in `<EOR>`: also what the WSJT-X UDP
/// link's "QSO logged" ADIF message carries.
pub(crate) fn record(r: &QsoRecord) -> String {
    let mut out = String::new();
    let mut put = |name: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            out.push_str(&field(name, v));
        }
    };
    let (date, time) = date_time(r.start_utc);
    let (date_off, time_off) = date_time(r.end_utc.max(r.start_utc));
    put("CALL", &r.call);
    put("QSO_DATE", &date);
    put("TIME_ON", &time);
    put("QSO_DATE_OFF", &date_off);
    put("TIME_OFF", &time_off);
    put("BAND", &r.band);
    put("MODE", &r.mode);
    put("SUBMODE", &r.submode);
    if r.freq_hz > 0.0 {
        put("FREQ", &format!("{:.6}", r.freq_hz / 1e6));
    }
    put("GRIDSQUARE", &r.grid);
    put("RST_SENT", &r.rst_sent);
    put("RST_RCVD", &r.rst_rcvd);
    put("NAME", &r.name);
    put("QTH", &r.qth);
    put("STATE", &r.state);
    put("CNTY", &r.county);
    put("COUNTRY", &r.country);
    put("DXCC", &r.dxcc.map(|v| v.to_string()).unwrap_or_default());
    put("CQZ", &r.cq_zone.map(|v| v.to_string()).unwrap_or_default());
    put("ITUZ", &r.itu_zone.map(|v| v.to_string()).unwrap_or_default());
    put("CONT", &r.continent);
    put("TX_PWR", &r.tx_pwr_w.map(|v| format!("{v}")).unwrap_or_default());
    put("COMMENT", &r.comment);
    put("QSL_VIA", &r.qsl_via);
    put("STATION_CALLSIGN", &r.my_call);
    put("MY_GRIDSQUARE", &r.my_grid);
    let mut qsl = |q: &Qsl, sent: &str, sdate: &str, rcvd: &str, rdate: &str| {
        if q.sent {
            put(sent, "Y");
            put(sdate, &q.sent_on);
        }
        if q.rcvd {
            put(rcvd, "Y");
            put(rdate, &q.rcvd_on);
        }
    };
    qsl(&r.lotw, "LOTW_QSL_SENT", "LOTW_QSLSDATE", "LOTW_QSL_RCVD", "LOTW_QSLRDATE");
    qsl(&r.eqsl, "EQSL_QSL_SENT", "EQSL_QSLSDATE", "EQSL_QSL_RCVD", "EQSL_QSLRDATE");
    qsl(&r.card, "QSL_SENT", "QSLSDATE", "QSL_RCVD", "QSLRDATE");
    for (k, v) in &r.extra {
        put(k, v);
    }
    out.push_str("<EOR>");
    out
}

/// The byte offset just past the `len`th character of `s`, if it has that
/// many.
fn char_end(s: &str, len: usize) -> Option<usize> {
    let mut chars = s.char_indices();
    for _ in 0..len {
        chars.next()?;
    }
    Some(chars.next().map_or(s.len(), |(off, _)| off))
}

/// One field's value at `start`, for the length its tag declared, and where
/// reading carries on.
///
/// ADIF counts the length in bytes, but some exporters (QRZ's logbook among
/// them) count characters, and a file read from a Windows code page has its
/// byte counts turned into character counts by the decoding. Both readings
/// agree on plain ASCII; where they differ, the one that lands just before the
/// next tag wins, bytes first. If neither does, the value runs to the next
/// `<`, so a bad count costs its own field and not the rest of the record.
fn value(adif: &str, start: usize, len: usize) -> (String, usize) {
    let rest = &adif[start..];
    let as_bytes = (len <= rest.len() && rest.is_char_boundary(len)).then_some(len);
    let as_chars = char_end(rest, len);
    if as_bytes.is_some() && as_bytes == as_chars {
        return (rest[..len].to_string(), start + len);
    }
    let opens_a_tag = |end: &usize| {
        let after = rest[*end..].trim_start();
        after.is_empty() || after.starts_with('<')
    };
    if let Some(end) = as_bytes.filter(opens_a_tag).or_else(|| as_chars.filter(opens_a_tag)) {
        return (rest[..end].to_string(), start + end);
    }
    let end = rest.find('<').unwrap_or(rest.len());
    (rest[..end].trim_end().to_string(), start + end)
}

/// Each record of an ADIF document, as `(UPPER_CASE_NAME, value)` pairs, to
/// `visit`. The header is skipped; a last record with no `<EOR>` still counts.
fn visit(adif: &str, mut visit: impl FnMut(&[(String, String)])) {
    let mut fields: Vec<(String, String)> = Vec::new();
    let bytes = adif.as_bytes();
    let mut i = 0usize;
    let mut in_header = adif.to_ascii_uppercase().contains("<EOH>");
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(close) = adif[i..].find('>') else { break };
        let tag = &adif[i + 1..i + close];
        i += close + 1;
        let mut parts = tag.splitn(3, ':');
        let name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
        match name.as_str() {
            "EOH" => {
                in_header = false;
                fields.clear();
                continue;
            }
            "EOR" => {
                if !fields.is_empty() {
                    visit(&fields);
                }
                fields.clear();
                continue;
            }
            _ => {}
        }
        let len: usize = parts.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
        let v = if len > 0 {
            let (v, next) = value(adif, i, len);
            i = next;
            v
        } else {
            String::new()
        };
        if !in_header && !name.is_empty() {
            fields.push((name, v));
        }
    }
    if !fields.is_empty() {
        visit(&fields);
    }
}

/// An ADIF document's QSOs, and how many records were listening reports
/// (`SWL` set), which are left out: the log is QSOs, and a station heard is
/// not one worked.
pub(crate) fn parse(adif: &str) -> (Vec<QsoRecord>, usize) {
    let mut recs = Vec::new();
    let mut swl = 0;
    visit(adif, |f| {
        if f.iter().any(|(k, v)| k == "SWL" && v.trim().eq_ignore_ascii_case("Y")) {
            swl += 1;
        } else {
            recs.push(from_fields(f));
        }
    });
    (recs, swl)
}

fn from_fields(f: &[(String, String)]) -> QsoRecord {
    let get = |k: &str| {
        f.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default()
    };
    let yes = |k: &str| get(k).eq_ignore_ascii_case("Y");
    let qsl = |sent: &str, sdate: &str, rcvd: &str, rdate: &str| Qsl {
        sent: yes(sent),
        sent_on: get(sdate),
        rcvd: yes(rcvd),
        rcvd_on: get(rdate),
    };
    let mut r = QsoRecord {
        call: get("CALL").to_ascii_uppercase(),
        grid: get("GRIDSQUARE"),
        rst_sent: get("RST_SENT"),
        rst_rcvd: get("RST_RCVD"),
        freq_hz: get("FREQ").parse::<f64>().map(|m| m * 1e6).unwrap_or(0.0),
        mode: get("MODE"),
        submode: get("SUBMODE"),
        band: get("BAND"),
        my_call: get("STATION_CALLSIGN"),
        my_grid: get("MY_GRIDSQUARE"),
        comment: get("COMMENT"),
        name: get("NAME"),
        qth: get("QTH"),
        state: get("STATE"),
        county: get("CNTY"),
        country: get("COUNTRY"),
        dxcc: get("DXCC").parse().ok(),
        cq_zone: get("CQZ").parse().ok(),
        itu_zone: get("ITUZ").parse().ok(),
        continent: get("CONT"),
        tx_pwr_w: get("TX_PWR").trim_end_matches(['W', 'w']).trim().parse().ok(),
        qsl_via: get("QSL_VIA"),
        lotw: qsl("LOTW_QSL_SENT", "LOTW_QSLSDATE", "LOTW_QSL_RCVD", "LOTW_QSLRDATE"),
        eqsl: qsl("EQSL_QSL_SENT", "EQSL_QSLSDATE", "EQSL_QSL_RCVD", "EQSL_QSLRDATE"),
        card: qsl("QSL_SENT", "QSLSDATE", "QSL_RCVD", "QSLRDATE"),
        extra: f
            .iter()
            .filter(|(k, v)| !KNOWN.contains(&k.as_str()) && !v.trim().is_empty())
            .cloned()
            .collect(),
        ..QsoRecord::default()
    };
    if r.my_call.is_empty() {
        r.my_call = get("OPERATOR");
    }
    let date = get("QSO_DATE");
    let on = get("TIME_ON");
    r.start_utc = parse_date_time(&date, &on).unwrap_or(0);
    let off = get("TIME_OFF");
    let date_off = get("QSO_DATE_OFF");
    r.end_utc = match (date_off.is_empty(), off.is_empty()) {
        (_, true) => r.start_utc,
        (false, false) => parse_date_time(&date_off, &off).unwrap_or(r.start_utc),
        // An end time with no date of its own, earlier than the start: the QSO
        // ran past midnight.
        (true, false) => match parse_date_time(&date, &off) {
            Some(t) if t < r.start_utc => t + 86_400,
            Some(t) => t,
            None => r.start_utc,
        },
    };
    r
}

/// ADIF's `YYYYMMDD` and `HHMM` or `HHMMSS` as Unix seconds UTC.
pub(crate) fn parse_date_time(date: &str, time: &str) -> Option<i64> {
    let d = date.trim();
    let t = time.trim();
    if d.len() != 8 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = d[0..4].parse().ok()?;
    let m: u32 = d[4..6].parse().ok()?;
    let day: u32 = d[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (h, mi, s) = match t.len() {
        6 => (num(&t[0..2])?, num(&t[2..4])?, num(&t[4..6])?),
        4 => (num(&t[0..2])?, num(&t[2..4])?, 0),
        0 => (0, 0, 0),
        _ => return None,
    };
    Some(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + s)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's days_from_civil,
/// the inverse of [`crate::decodelog::civil`]).
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A text file's contents, and the code page that had to be assumed to read
/// it, if any.
pub(crate) struct Decoded {
    pub text: String,
    pub assumed: Option<&'static str>,
}

/// Read a text file's bytes as text: by its byte-order mark, as UTF-8 if it
/// is that, and otherwise as Windows-1251 when it reads as Cyrillic or
/// Windows-1252 when not. A log's calls, dates, bands and modes are ASCII in
/// all of these, so a wrong guess costs at most the spelling of a name, where
/// refusing the file would cost the log.
pub(crate) fn decode_text(bytes: &[u8]) -> Decoded {
    let unicode = |text: String| Decoded { text, assumed: None };
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return unicode(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return unicode(from_utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return unicode(from_utf16(rest, u16::from_be_bytes));
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return unicode(text.to_string());
    }
    let (name, table): (_, fn(u8) -> char) = if looks_cyrillic(bytes) {
        ("Windows-1251", cp1251_char)
    } else {
        ("Windows-1252", cp1252_char)
    };
    Decoded { text: bytes.iter().map(|&b| table(b)).collect(), assumed: Some(name) }
}

fn from_utf16(bytes: &[u8], pair: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks_exact(2).map(|c| pair([c[0], c[1]]));
    char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// Cyrillic spells whole words in high bytes; Western European text drops an
/// accent into ASCII here and there. Three high bytes in a row is a word in
/// one and a spelling nobody uses in the other.
fn looks_cyrillic(bytes: &[u8]) -> bool {
    let (mut run, mut longest) = (0usize, 0usize);
    for &b in bytes {
        run = if b >= 0x80 { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest >= 3
}

/// Windows-1252: Latin-1 but for 0x80–0x9F.
#[rustfmt::skip]
fn cp1252_char(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{fffd}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{fffd}', 'Ž', '\u{fffd}',
        '\u{fffd}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{fffd}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Windows-1251 (Cyrillic): from 0xC0 the Russian alphabet in Unicode's order;
/// 0x80–0xBF a table.
#[rustfmt::skip]
fn cp1251_char(b: u8) -> char {
    const HIGH: [char; 64] = [
        'Ђ', 'Ѓ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ',
        'ђ', '‘', '’', '“', '”', '•', '–', '—', '\u{fffd}', '™', 'љ', '›', 'њ', 'ќ', 'ћ', 'џ',
        '\u{a0}', 'Ў', 'ў', 'Ј', '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '\u{ad}', '®', 'Ї',
        '°', '±', 'І', 'і', 'ґ', 'µ', '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї',
    ];
    match b {
        0x80..=0xbf => HIGH[(b - 0x80) as usize],
        0xc0..=0xff => char::from_u32(0x410 + (b - 0xc0) as u32).unwrap_or(char::REPLACEMENT_CHARACTER),
        _ => b as char,
    }
}
