use super::*;

fn qso(call: &str, band: &str, mode: &str, start: i64) -> QsoRecord {
    QsoRecord {
        call: call.into(),
        band: band.into(),
        mode: mode.into(),
        freq_hz: match band {
            "20m" => 14_075_000.0,
            "40m" => 7_075_000.0,
            _ => 0.0,
        },
        start_utc: start,
        end_utc: start + 75,
        rst_sent: "-12".into(),
        rst_rcvd: "+04".into(),
        my_call: "AJ3G".into(),
        my_grid: "FN20".into(),
        ..QsoRecord::default()
    }
}

/// 2026-11-10 21:15:00 UTC.
const T: i64 = 1_794_345_300;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hrp-logbook-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_qso_round_trips_through_adif() {
    let mut r = qso("JA1ABC", "20m", "FT8", T);
    r.grid = "PM95".into();
    r.name = "Jörg Ñandú".into();
    r.qth = "Tōkyō".into();
    r.country = "Japan".into();
    r.dxcc = Some(339);
    r.cq_zone = Some(25);
    r.itu_zone = Some(45);
    r.continent = "AS".into();
    r.tx_pwr_w = Some(50.0);
    r.comment = "<odd> text".into();
    r.lotw = Qsl { sent: true, sent_on: "20261111".into(), rcvd: true, rcvd_on: "20261112".into() };
    r.eqsl.sent = true;
    r.extra = vec![("PROP_MODE".into(), "F2".into())];
    let doc = adif::document([&r].into_iter());
    let (back, swl) = adif::parse(&doc);
    assert_eq!(swl, 0);
    assert_eq!(back.len(), 1);
    let mut b = back[0].clone();
    b.id = r.id;
    assert!((b.freq_hz - r.freq_hz).abs() < 0.5, "{}", b.freq_hz);
    b.freq_hz = r.freq_hz;
    assert_eq!(b, r);
    // A QSO past midnight keeps its end on the next day.
    let mut late = qso("W1AW", "40m", "CW", T + 2 * 3600 + 40 * 60); // 23:55
    late.end_utc = late.start_utc + 600;
    let (back, _) = adif::parse(&adif::record(&late));
    assert_eq!(back[0].end_utc, late.end_utc);
}

#[test]
fn ft4_goes_under_mfsk_and_is_shown_as_ft4() {
    let mut r = qso("K1ABC", "20m", "ft4", T);
    r.normalise();
    assert_eq!((r.mode.as_str(), r.submode.as_str(), r.mode_shown()), ("MFSK", "FT4", "FT4"));
    let rec = adif::record(&r);
    assert!(rec.contains("<MODE:4>MFSK<SUBMODE:3>FT4"), "{rec}");
}

#[test]
fn an_end_time_without_its_own_date_past_midnight_is_the_next_day() {
    let doc = "<CALL:4>W1AW<QSO_DATE:8>20261110<TIME_ON:6>235500<TIME_OFF:4>0005<BAND:3>40m<MODE:2>CW<EOR>";
    let (r, _) = adif::parse(doc);
    assert_eq!(r[0].end_utc - r[0].start_utc, 600);
}

#[test]
fn the_master_file_is_created_appended_to_and_read_back() {
    let dir = temp_dir("master");
    let path = dir.join(FILE);
    let mut log = Logbook::load(&path);
    assert!(path.exists(), "created with its header");
    assert_eq!(log.problem(), None);
    let id = log.add(qso("JA1ABC", "20m", "FT8", T));
    log.add(qso("VK3TDX", "20m", "FT8", T + 300));
    // Filled from cty.dat on the way in.
    assert_eq!(log.get(id).unwrap().country, "Japan");
    assert_eq!(log.get(id).unwrap().cq_zone, Some(25));
    log.update_end(id, T + 200);
    let again = Logbook::load(&path);
    assert_eq!(again.records().len(), 2);
    assert_eq!(again.records()[0].call, "JA1ABC");
    assert_eq!(again.records()[0].end_utc, T + 200);
    assert_eq!(again.records()[1].rst_rcvd, "+04");
    // Deleting rewrites the file, keeping the old one once as .bak.
    let mut log = again;
    let first = log.records()[0].id;
    assert_eq!(log.delete(&[first]), 1);
    assert_eq!(Logbook::load(&path).records().len(), 1);
    assert!(path.with_extension("adi.bak").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_log_is_never_overwritten() {
    let dir = temp_dir("unreadable");
    // A directory where the file should be cannot be read as one.
    let path = dir.join(FILE);
    std::fs::create_dir_all(&path).unwrap();
    let mut log = Logbook::load(&path);
    assert!(log.problem().is_some());
    log.add(qso("JA1ABC", "20m", "FT8", T));
    assert!(path.is_dir(), "left alone");
    assert!(log.problem().unwrap().contains("Couldn't read"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn worked_says_what_is_new() {
    let mut log = Logbook::in_memory();
    let mut a = qso("JA1ABC", "20m", "FT8", T);
    a.grid = "PM95".into();
    log.add(a);
    log.add(qso("DL1ABC", "40m", "CW", T));
    // The same call, band and mode: a dupe, and nothing new.
    let w = log.worked("ja1abc", "20m", "FT8", Some("PM95"));
    assert_eq!(w, Worked { dupe: true, ..Worked::default() });
    // Another mode on the same band is no dupe.
    assert!(!log.worked("JA1ABC", "20m", "FT4", None).dupe);
    // Another station in Japan on another band: a new call and a new band slot.
    let w = log.worked("JA2XYZ", "40m", "FT8", Some("PM84"));
    assert_eq!(w, Worked { new_call: true, new_band: true, new_grid: true, ..Worked::default() });
    // Japan on 20 m is old; the grid sent is the one worked.
    let w = log.worked("JA2XYZ", "20m", "FT8", Some("pm95xx"));
    assert_eq!(w, Worked { new_call: true, ..Worked::default() });
    // A country never worked.
    let w = log.worked("VK3TDX", "20m", "FT8", None);
    assert!(w.new_dxcc && w.new_call && !w.new_band && !w.new_grid);
    // Sicily is Italy, and Italy is new.
    assert!(log.worked("IT9ABC", "20m", "FT8", None).new_dxcc);
    log.add(qso("I1ABC", "20m", "FT8", T));
    let w = log.worked("IT9ABC", "40m", "FT8", None);
    assert!(!w.new_dxcc && w.new_band);
    assert_eq!(log.worked("", "20m", "FT8", None), Worked::default());
}

#[test]
fn import_skips_what_the_log_has_within_two_minutes() {
    let mut log = Logbook::in_memory();
    log.add(qso("JA1ABC", "20m", "FT8", T));
    let doc = "WSJT-X ADIF Export<eoh>\n\
        <call:6>JA1ABC <gridsquare:4>PM95 <mode:3>FT8 <qso_date:8>20261110 <time_on:6>211630 <band:3>20m <eor>\n\
        <call:6>JA1ABC <mode:3>FT8 <qso_date:8>20261110 <time_on:6>212000 <band:3>20m <eor>\n\
        <call:6>JA1ABC <mode:3>FT8 <qso_date:8>20261110 <time_on:6>211500 <band:3>40m <eor>\n\
        <call:6>VK3TDX <mode:3>FT8 <qso_date:8>20261110 <time_on:4>2130 <band:3>20m <eor>\n\
        <call:6>VK3TDX <mode:3>FT8 <qso_date:8>20261110 <time_on:4>2131 <band:3>20m <eor>\n\
        <call:6>W1AW/1 <mode:3>FT8 <qso_date:8>20261110 <time_on:4>2131 <band:3>20m <swl:1>Y <eor>\n\
        <mode:3>FT8 <qso_date:8>20261110 <time_on:4>2131 <band:3>20m <eor>\n";
    let rep = log.import_text(doc);
    // 21:16:30 is within 2 min of 21:15:00 (a dupe); 21:20 isn't; 40 m is
    // another band; the second VK3TDX is a minute after the first in the same
    // file.
    assert_eq!(rep, ImportReport { added: 3, dupes: 2, unreadable: 1, swl: 1, assumed: None });
    assert_eq!(log.records().len(), 4);
    // Importing the same file again adds nothing.
    assert_eq!(log.import_text(doc).added, 0);
}

#[test]
fn import_reads_byte_and_character_counted_lengths() {
    // "Jörg" is 4 characters and 5 bytes.
    let bytes = "<CALL:5>DL1AB<NAME:5>Jörg<QTH:6>Berlin<QSO_DATE:8>20261110<TIME_ON:4>2115<BAND:3>20m<MODE:3>FT8<EOR>";
    let chars = "<CALL:5>DL1AB<NAME:4>Jörg<QTH:6>Berlin<QSO_DATE:8>20261110<TIME_ON:4>2115<BAND:3>20m<MODE:3>FT8<EOR>";
    for doc in [bytes, chars] {
        let (r, _) = adif::parse(doc);
        assert_eq!((r[0].name.as_str(), r[0].qth.as_str()), ("Jörg", "Berlin"), "{doc}");
    }
    // A miscount that fits neither still costs only its own field.
    let (r, _) = adif::parse("<CALL:5>DL1AB<NAME:6>Jörg<QTH:6>Berlin<EOR>");
    assert_eq!((r[0].name.as_str(), r[0].qth.as_str()), ("Jörg", "Berlin"));
}

#[test]
fn import_reads_windows_code_pages() {
    let dir = temp_dir("codepages");
    let mut log = Logbook::in_memory();
    // "Jörg" and "Jyväskylä" in Windows-1252, lengths counted in its bytes.
    let mut w1252 = b"<CALL:5>OH1AB<NAME:4>J".to_vec();
    w1252.extend([0xf6]);
    w1252.extend(b"rg<QTH:9>Jyv");
    w1252.extend([0xe4]);
    w1252.extend(b"skyl");
    w1252.extend([0xe4]);
    w1252.extend(b"<QSO_DATE:8>20261110<TIME_ON:4>2115<BAND:3>20m<MODE:3>FT8<EOR>");
    let p = dir.join("w1252.adi");
    std::fs::write(&p, &w1252).unwrap();
    let rep = log.import_file(&p).unwrap();
    assert_eq!((rep.added, rep.assumed), (1, Some("Windows-1252")));
    assert_eq!(log.records()[0].name, "Jörg");
    assert_eq!(log.records()[0].qth, "Jyväskylä");
    // "Владимир" in Windows-1251.
    let mut w1251 = b"<CALL:5>UA3AB<NAME:8>".to_vec();
    w1251.extend([0xc2, 0xeb, 0xe0, 0xe4, 0xe8, 0xec, 0xe8, 0xf0]);
    w1251.extend(b"<QSO_DATE:8>20261110<TIME_ON:4>2116<BAND:3>20m<MODE:3>FT8<EOR>");
    let p = dir.join("w1251.adi");
    std::fs::write(&p, &w1251).unwrap();
    let rep = log.import_file(&p).unwrap();
    assert_eq!(rep.assumed, Some("Windows-1251"));
    assert_eq!(log.records()[1].name, "Владимир");
    // UTF-16 with its mark.
    let mut u16le = vec![0xff, 0xfe];
    for c in
        "<CALL:4>W1AW<QSO_DATE:8>20261110<TIME_ON:4>2117<BAND:3>40m<MODE:2>CW<EOR>".encode_utf16()
    {
        u16le.extend(c.to_le_bytes());
    }
    let p = dir.join("u16.adi");
    std::fs::write(&p, &u16le).unwrap();
    assert_eq!(log.import_file(&p).unwrap().added, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn confirmations_mark_the_matching_qsos() {
    let mut log = Logbook::in_memory();
    let a = log.add(qso("JA1ABC", "20m", "FT8", T));
    let b = log.add(qso("VK3TDX", "20m", "FT4", T));
    assert_eq!(log.mark_sent(&[a, b], Service::Lotw, "20261111"), 2);
    let mut c = qso("JA1ABC", "20m", "FT8", T + 30);
    c.lotw.rcvd = true;
    c.lotw.rcvd_on = "20261112".into();
    // Another band: not this QSO.
    let mut d = qso("VK3TDX", "40m", "FT4", T);
    d.lotw.rcvd = true;
    assert_eq!(log.apply_confirmations(&[c.clone(), d], Service::Lotw), 1);
    let r = log.get(a).unwrap();
    assert!(r.lotw.rcvd && r.confirmed() && !r.eqsl.rcvd);
    assert_eq!(r.lotw.rcvd_on, "20261112");
    assert!(!log.get(b).unwrap().lotw.rcvd);
    assert_eq!(log.apply_confirmations(&[c], Service::Lotw), 0, "already confirmed");
}

#[test]
fn the_log_learns_dxcc_numbers() {
    let mut log = Logbook::in_memory();
    let mut a = qso("JA1ABC", "20m", "FT8", T);
    a.dxcc = Some(339);
    log.add(a);
    assert_eq!(log.dxcc_number("JA"), Some(339));
    let id = log.add(qso("JA2XYZ", "20m", "FT8", T + 600));
    assert_eq!(log.get(id).unwrap().dxcc, Some(339));
    assert_eq!(log.dxcc_number("VK"), None);
}

#[test]
fn credentials_never_print_their_password() {
    let c = Credentials { user: "AJ3G".into(), password: "hunter2".into() };
    let s = LogSettings { eqsl: c.clone(), ..LogSettings::default() };
    assert!(!format!("{c:?}").contains("hunter2"));
    assert!(!format!("{s:?}").contains("hunter2"));
}

#[test]
fn bands_by_frequency() {
    assert_eq!(adif_band(14_074_000.0), "20m");
    assert_eq!(adif_band(7_038_600.0), "40m");
    assert_eq!(adif_band(475_700.0), "630m");
    assert_eq!(adif_band(27_185_000.0), "");
    assert_eq!(adif_band(50_313_000.0), "6m");
}
