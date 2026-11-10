//! Sending QSOs to eQSL and LoTW, and bringing their confirmations back. Only
//! ever on a button press in the LOG window.
//!
//! - **eQSL**: its ADIF import (`importADIF.cfm`), with the eQSL login from
//!   SETTINGS → Logbook. Confirmations from its inbox download.
//! - **LoTW**: the QSOs are written to an ADIF file and TQSL's command line
//!   signs and uploads it (`tqsl -x -d -u -a compliant`). TQSL keeps the
//!   certificate and its password; the panel never sees either. Until TQSL is
//!   installed the LoTW button is greyed and says so ([`find_tqsl`]).
//!   Confirmations from LoTW's own report, with the LoTW website login.
//!
//! The eQSL and LoTW requests are SDRoxide's (`sdroxide-net/src/upload.rs`,
//! https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use super::net::{Http, scrub, strip_html, urlencode};
use super::{Credentials, QsoRecord, Service, adif};

/// How many QSOs go in one eQSL upload.
const EQSL_CHUNK: usize = 100;

/// One QSO as eQSL wants it: only what goes on the card. The comment, names
/// and the rest of the log stay out of it.
fn eqsl_record(r: &QsoRecord, qth_nickname: &str) -> String {
    let mut out = String::new();
    let mut put = |k: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            out.push_str(&format!("<{k}:{}>{v}", v.len()));
        }
    };
    let (date, time) = adif::date_time(r.start_utc);
    put("CALL", &r.call);
    put("QSO_DATE", &date);
    put("TIME_ON", &time[..4]);
    put("BAND", &r.band);
    put("MODE", &r.mode);
    put("SUBMODE", &r.submode);
    if r.freq_hz > 0.0 {
        put("FREQ", &format!("{:.6}", r.freq_hz / 1e6));
    }
    put("RST_SENT", &r.rst_sent);
    put("APP_EQSL_QTH_NICKNAME", qth_nickname);
    out.push_str("<EOR>");
    out
}

/// The ADIF document one eQSL upload sends.
fn eqsl_document(recs: &[&QsoRecord], qth_nickname: &str) -> String {
    let mut out = String::from(
        "hardrock-panel upload\r\n<ADIF_VER:5>3.1.4\r\n<PROGRAMID:14>hardrock-panel\r\n<EOH>\r\n",
    );
    for r in recs {
        out.push_str(&eqsl_record(r, qth_nickname));
        out.push_str("\r\n");
    }
    out
}

/// What eQSL said to an upload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EqslReply {
    /// "Result: N out of M records added".
    pub added: usize,
    pub of: usize,
    /// Records refused as already there.
    pub duplicates: usize,
    /// Its other errors and warnings, a line each.
    pub problems: Vec<String>,
}

impl EqslReply {
    /// Every record is at eQSL now: added, or there already.
    pub(crate) fn all_there(&self) -> bool {
        self.of > 0 && self.added + self.duplicates >= self.of
    }
}

/// Read eQSL's reply to an upload: an HTML page with a `Result:` line, and an
/// `Error:` or `Warning:` line for each record it didn't take.
pub(crate) fn parse_eqsl_reply(body: &str) -> Result<EqslReply, String> {
    let text = strip_html(body);
    let mut rep = EqslReply { added: 0, of: 0, duplicates: 0, problems: Vec::new() };
    let mut result = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let low = line.to_ascii_lowercase();
        if let Some(at) = low.find("result:") {
            let nums: Vec<usize> = low[at + 7..]
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|s| s.parse().ok())
                .collect();
            if let [a, b, ..] = nums[..] {
                rep.added = a;
                rep.of = b;
                result = true;
            }
        } else if low.contains("duplicate") {
            rep.duplicates += 1;
        } else if low.contains("error") || low.contains("warning") {
            rep.problems.push(line.chars().take(160).collect());
        }
    }
    if !result {
        return Err(rep.problems.first().cloned().unwrap_or_else(|| "eQSL gave no result".into()));
    }
    Ok(rep)
}

/// Upload `recs` to eQSL, a chunk at a time. Returns the ids now at eQSL
/// (added, or there already), and a line for the operator.
pub(crate) fn eqsl_upload<H: Http>(
    http: &H,
    login: &Credentials,
    qth_nickname: &str,
    recs: &[&QsoRecord],
) -> Result<(Vec<u64>, String), String> {
    if !login.is_set() {
        return Err("Set your eQSL login in SETTINGS → Logbook".into());
    }
    let clean = |e: &str| scrub(e, &[&login.password]);
    let mut there = Vec::new();
    let (mut added, mut dupes, mut problems) = (0, 0, Vec::new());
    for chunk in recs.chunks(EQSL_CHUNK) {
        let doc = eqsl_document(chunk, qth_nickname.trim());
        let mut fields =
            vec![("EQSL_USER", login.user.trim()), ("EQSL_PSWD", login.password.as_str())];
        if !qth_nickname.trim().is_empty() {
            fields.push(("QTHNickname", qth_nickname.trim()));
        }
        fields.push(("ADIFData", doc.as_str()));
        let body = http
            .post_form("https://www.eqsl.cc/qslcard/importADIF.cfm", &fields)
            .map_err(|e| format!("eQSL: {}", clean(&e)))?;
        let rep = parse_eqsl_reply(&body).map_err(|e| format!("eQSL: {}", clean(&e)))?;
        added += rep.added;
        dupes += rep.duplicates;
        if rep.all_there() {
            there.extend(chunk.iter().map(|r| r.id));
        } else {
            problems.extend(rep.problems.iter().map(|p| clean(p)));
        }
    }
    let mut msg = format!("eQSL: {added} added, {dupes} already there");
    if !problems.is_empty() {
        msg.push_str(&format!(
            "; some not taken, so those QSOs stay unsent ({}). Sending again is safe: eQSL skips duplicates",
            problems.join("; ")
        ));
    }
    Ok((there, msg))
}

/// The eQSL inbox: the confirmations other stations have sent, as QSOs marked
/// confirmed by eQSL. Two requests: one makes eQSL build the file and names
/// it, the second fetches it.
pub(crate) fn eqsl_confirmations<H: Http>(
    http: &H,
    login: &Credentials,
    qth_nickname: &str,
) -> Result<Vec<QsoRecord>, String> {
    if !login.is_set() {
        return Err("Set your eQSL login in SETTINGS → Logbook".into());
    }
    let clean = |e: &str| scrub(e, &[&login.password]);
    let mut url = format!(
        "https://www.eqsl.cc/qslcard/DownloadInBox.cfm?UserName={}&Password={}&RcvdSince=19700101",
        urlencode(login.user.trim()),
        urlencode(&login.password)
    );
    if !qth_nickname.trim().is_empty() {
        url.push_str(&format!("&QTHNickname={}", urlencode(qth_nickname.trim())));
    }
    let page = http.get(&url).map_err(|e| format!("eQSL: {}", clean(&e)))?;
    let low = page.to_ascii_lowercase();
    if low.contains("no such username/password") {
        return Err("eQSL: login refused".into());
    }
    if low.contains("no log entries") {
        return Ok(Vec::new());
    }
    let link = page
        .split(['"', '\'', ' ', '\n', '>', '<'])
        .find(|t| t.to_ascii_lowercase().ends_with(".adi"))
        .ok_or("eQSL: no download in its reply (check the login)")?;
    let adi = if link.starts_with("http") {
        link.to_string()
    } else {
        format!(
            "https://www.eqsl.cc/qslcard/{}",
            link.trim_start_matches("../").trim_start_matches('/')
        )
    };
    let body = http.get(&adi).map_err(|e| format!("eQSL: {}", clean(&e)))?;
    Ok(confirmed(&body, Service::Eqsl))
}

/// LoTW's report of confirmed QSOs (needs the LoTW website login, not TQSL).
pub(crate) fn lotw_confirmations<H: Http>(
    http: &H,
    login: &Credentials,
) -> Result<Vec<QsoRecord>, String> {
    if !login.is_set() {
        return Err("Set your LoTW website login in SETTINGS → Logbook".into());
    }
    let clean = |e: &str| scrub(e, &[&login.password]);
    let url = format!(
        "https://lotw.arrl.org/lotwuser/lotwreport.adi?login={}&password={}&qso_query=1&qso_qsl=yes&qso_qsldetail=yes",
        urlencode(login.user.trim()),
        urlencode(&login.password)
    );
    let body = http.get(&url).map_err(|e| format!("LoTW: {}", clean(&e)))?;
    let low = body.to_ascii_lowercase();
    if low.contains("username/password") || low.contains("<!doctype") || !low.contains("<eoh>") {
        return Err("LoTW: login refused".into());
    }
    Ok(confirmed(&body, Service::Lotw))
}

/// A confirmation download's records, marked confirmed by `service` on the
/// date it gives.
fn confirmed(adif_text: &str, service: Service) -> Vec<QsoRecord> {
    let (recs, _) = adif::parse(adif_text);
    recs.into_iter()
        .map(|mut r| {
            // The download's own QSL_RCVD and QSLRDATE are the service's,
            // not a paper card's.
            let card = std::mem::take(&mut r.card);
            let rcvd_date = r.extra.iter().find(|(k, _)| k == "RCVD_DATE").map(|(_, v)| v.clone());
            let q = r.qsl_mut(service);
            q.rcvd = true;
            q.rcvd_on = rcvd_date.unwrap_or(card.rcvd_on);
            r
        })
        .collect()
}

/// Where TQSL might be, on this system.
fn tqsl_candidates() -> Vec<PathBuf> {
    let mut c = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
            if let Some(p) = std::env::var_os(var) {
                c.push(PathBuf::from(p).join("TrustedQSL").join("tqsl.exe"));
            }
        }
        c.push(PathBuf::from(r"C:\Program Files (x86)\TrustedQSL\tqsl.exe"));
        c.push(PathBuf::from(r"C:\Program Files\TrustedQSL\tqsl.exe"));
    } else if cfg!(target_os = "macos") {
        c.push(PathBuf::from("/Applications/TrustedQSL/tqsl.app/Contents/MacOS/tqsl"));
        c.push(PathBuf::from("/Applications/tqsl.app/Contents/MacOS/tqsl"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            c.push(d.join(if cfg!(windows) { "tqsl.exe" } else { "tqsl" }));
        }
    }
    c
}

/// TQSL's program, from SETTINGS (`chosen`) or its usual places; or why it
/// can't be used, for the greyed LoTW button.
pub(crate) fn find_tqsl(chosen: &str) -> Result<PathBuf, String> {
    let chosen = chosen.trim();
    if !chosen.is_empty() {
        let p = PathBuf::from(chosen);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("TQSL isn't at {chosen} (SETTINGS → Logbook)"))
        };
    }
    tqsl_candidates().into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "TQSL isn't installed. Install ARRL's TQSL and your LoTW certificate to upload; \
         ADIF export works meanwhile, for signing later."
            .into()
    })
}

/// TQSL's arguments to sign and upload `file` without asking anything: `-x`
/// exit when done, `-d` no date-range question, `-u` upload, `-a compliant`
/// sign what is valid and skip duplicates, and `-l` the station location.
pub(crate) fn tqsl_args(file: &Path, location: &str) -> Vec<String> {
    let mut a: Vec<String> =
        ["-x", "-d", "-u", "-a", "compliant"].iter().map(|s| s.to_string()).collect();
    if !location.trim().is_empty() {
        a.push("-l".into());
        a.push(location.trim().to_string());
    }
    a.push(file.display().to_string());
    a
}

/// What TQSL's exit code means, and whether the QSOs can be marked sent.
pub(crate) fn tqsl_outcome(code: Option<i32>) -> (bool, String) {
    let (ok, what) = match code {
        Some(0) => (true, "all QSOs signed and uploaded"),
        Some(8) => (true, "nothing new: every QSO was already uploaded"),
        Some(9) => (true, "uploaded; some were already there and skipped"),
        Some(1) => (false, "cancelled"),
        Some(2) => (false, "refused by LoTW"),
        Some(3) => (false, "an unexpected answer from LoTW"),
        Some(4) => (false, "a TQSL error"),
        Some(5) => (false, "a TQSL library error (is the certificate loaded?)"),
        Some(6) => (false, "it couldn't open the file"),
        Some(7) => (false, "it couldn't write its output"),
        Some(10) => (false, "a command-line error"),
        Some(11) => (false, "it couldn't reach LoTW"),
        Some(_) => (false, "an error"),
        None => (false, "it was stopped"),
    };
    let code = code.map_or("none".to_string(), |c| c.to_string());
    (ok, format!("TQSL: {what} (exit {code})"))
}

/// Sign and upload `recs` with TQSL, on its own thread: the file is written
/// beside the log (`dir`), then TQSL runs. The reply is whether to mark them
/// sent, and a line for the operator.
pub(crate) fn lotw_upload(
    tqsl: PathBuf,
    location: String,
    dir: PathBuf,
    recs: Vec<QsoRecord>,
) -> Receiver<(bool, String)> {
    let (tx, rx) = channel();
    let job = move || {
        let file = dir.join("hardrock-panel-lotw-upload.adi");
        let out = (|| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(&file, adif::document(recs.iter()))
                .map_err(|e| format!("{}: {e}", file.display()))?;
            let run = std::process::Command::new(&tqsl)
                .args(tqsl_args(&file, &location))
                .output()
                .map_err(|e| format!("TQSL wouldn't start: {e}"))?;
            let (ok, mut msg) = tqsl_outcome(run.status.code());
            let said = String::from_utf8_lossy(&run.stderr);
            if let Some(l) = said.lines().map(str::trim).rfind(|l| !l.is_empty()) {
                msg.push_str(&format!(": {}", l.chars().take(160).collect::<String>()));
            }
            Ok::<_, String>((ok, msg))
        })();
        let out = out.unwrap_or_else(|e| (false, e));
        tracing::info!("logbook: {}", out.1);
        let _ = tx.send(out);
    };
    if let Err(e) = std::thread::Builder::new().name("log-tqsl".into()).spawn(job) {
        let (tx, rx) = channel();
        let _ = tx.send((false, format!("couldn't start TQSL: {e}")));
        return rx;
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::super::net::fake::Fake;
    use super::*;

    fn qso(id: u64, call: &str) -> QsoRecord {
        QsoRecord {
            id,
            call: call.into(),
            band: "20m".into(),
            mode: "FT8".into(),
            freq_hz: 14_075_123.0,
            rst_sent: "-12".into(),
            start_utc: 1_793_000_000,
            end_utc: 1_793_000_060,
            comment: "private note".into(),
            ..QsoRecord::default()
        }
    }

    fn login() -> Credentials {
        Credentials { user: "AJ3G".into(), password: "hunter2".into() }
    }

    #[test]
    fn an_eqsl_upload_sends_only_the_card_and_reads_the_result() {
        let web = Fake::with(&[(
            "https://www.eqsl.cc/qslcard/importADIF.cfm",
            "<HTML><BODY>Information: From: AJ3G<BR>Result: 2 out of 2 records added<BR></BODY></HTML>",
        )]);
        let (a, b) = (qso(1, "JA1ABC"), qso(2, "VK3TDX"));
        let (ids, msg) = eqsl_upload(&web, &login(), "", &[&a, &b]).unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert!(msg.contains("2 added"), "{msg}");
        let posted = &web.posted.borrow()[0];
        let data = &posted.iter().find(|(k, _)| k == "ADIFData").unwrap().1;
        assert!(data.contains("<CALL:6>JA1ABC") && data.contains("<TIME_ON:4>"), "{data}");
        assert!(!data.contains("private note"), "the comment stays home");
        assert!(posted.iter().all(|(k, _)| k != "QTHNickname"));
    }

    #[test]
    fn eqsl_duplicates_count_as_there_but_errors_dont() {
        let dup = "Result: 0 out of 1 records added<BR>Warning: Y=2026 M=11 D=10 JA1ABC 20M FT8 Bad record: Duplicate<BR>";
        let r = parse_eqsl_reply(dup).unwrap();
        assert!(r.all_there(), "{r:?}");
        let bad = "Result: 1 out of 2 records added<BR>Error: Bad record: invalid band<BR>";
        let r = parse_eqsl_reply(bad).unwrap();
        assert!(!r.all_there());
        assert_eq!(r.problems.len(), 1);
        assert!(parse_eqsl_reply("Error: No such Username/Password found").is_err());
    }

    #[test]
    fn eqsl_failures_never_show_the_password() {
        let web = Fake::default();
        web.replies.borrow_mut().push((
            "https://www.eqsl.cc/".into(),
            Err("POST ...EQSL_PSWD=hunter2 timed out".into()),
        ));
        let a = qso(1, "JA1ABC");
        let e = eqsl_upload(&web, &login(), "", &[&a]).unwrap_err();
        assert!(!e.contains("hunter2"), "{e}");
    }

    #[test]
    fn eqsl_confirmations_are_fetched_in_two_steps() {
        let web = Fake::with(&[
            (
                "https://www.eqsl.cc/qslcard/DownloadInBox.cfm?UserName=AJ3G&Password=hunter2",
                "<html>Your ADIF log file has been built. <A HREF=\"downloadedfiles/abc123.adi\">.ADI file</A></html>",
            ),
            (
                "https://www.eqsl.cc/qslcard/downloadedfiles/abc123.adi",
                "eQSL inbox<EOH>\n<CALL:6>JA1ABC<QSO_DATE:8>20261110<TIME_ON:4>2115<BAND:3>20M<MODE:3>FT8<RCVD_DATE:8>20261111<EOR>",
            ),
        ]);
        let c = eqsl_confirmations(&web, &login(), "").unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].eqsl.rcvd);
        assert_eq!(c[0].eqsl.rcvd_on, "20261111");
    }

    #[test]
    fn lotw_confirmations_and_a_refused_login() {
        let web = Fake::with(&[(
            "https://lotw.arrl.org/lotwuser/lotwreport.adi?login=AJ3G&password=hunter2&qso_query=1",
            "ARRL Logbook of the World Status Report\n<PROGRAMID:4>LoTW\n<eoh>\n<CALL:6>JA1ABC<BAND:3>20M<MODE:3>FT8<QSO_DATE:8>20261110<TIME_ON:6>211500<QSL_RCVD:1>Y<QSLRDATE:8>20261112<eor>",
        )]);
        let c = lotw_confirmations(&web, &login()).unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].lotw.rcvd && !c[0].eqsl.rcvd);
        assert_eq!(c[0].lotw.rcvd_on, "20261112");
        let web = Fake::with(&[(
            "https://lotw.arrl.org/",
            "<!DOCTYPE html><p>Username/password incorrect</p>",
        )]);
        assert!(lotw_confirmations(&web, &login()).is_err());
    }

    #[test]
    fn tqsl_is_asked_to_sign_and_upload_quietly() {
        let a = tqsl_args(Path::new("/x/log.adi"), " Home ");
        assert_eq!(a, ["-x", "-d", "-u", "-a", "compliant", "-l", "Home", "/x/log.adi"]);
        assert_eq!(tqsl_args(Path::new("f.adi"), "").len(), 6);
        assert!(tqsl_outcome(Some(0)).0);
        assert!(tqsl_outcome(Some(9)).0);
        assert!(!tqsl_outcome(Some(11)).0);
        assert!(!tqsl_outcome(None).0);
    }

    #[test]
    fn a_missing_tqsl_says_why() {
        let e = find_tqsl("/no/such/tqsl").unwrap_err();
        assert!(e.contains("/no/such/tqsl"));
    }
}
