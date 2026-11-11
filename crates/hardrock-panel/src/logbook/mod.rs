//! The logbook (decision 69): every QSO, kept in an ADIF file that is the
//! master copy (`hardrock-panel-log.adi`, beside the panel's settings), so
//! any logger can read it. A QSO added is appended to it at once; an edit,
//! a delete, an import or a confirmation rewrites it whole.
//!
//! Also what the log knows about a station ([`Logbook::worked`]: dupe, new
//! call, new band, new DXCC, new grid), ADIF import and export (`adif`),
//! callsign lookup (`lookup`), the eQSL and LoTW uploads and confirmation
//! downloads (`upload`), and the LOG window (`window`).
//!
//! Much of it is ported from SDRoxide (https://github.com/dividebysandwich/sdroxide,
//! GPL-3.0-or-later): its `QsoRecord` and ADIF reader and writer
//! (`sdroxide-types/src/digi.rs`), its logbook window
//! (`sdroxide-ui/src/app/logbook.rs`), its file decoding
//! (`sdroxide-ui/src/download.rs`), and its lookups and uploads
//! (`sdroxide-net/src/lookup.rs`, `upload.rs`).

pub(crate) mod adif;
pub(crate) mod lookup;
mod net;
pub(crate) mod upload;
mod window;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use window::{LogUi, settings_ui, show};

/// The master file's name, in the settings folder.
pub(crate) const FILE: &str = "hardrock-panel-log.adi";

/// How close in time two QSOs with the same call on the same band must be for
/// an import to take them as one (AJ3G's plan: 2 minutes).
pub(crate) const IMPORT_DUPE_SECS: i64 = 120;

/// A QSL service a QSO can be sent to and confirmed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Service {
    Lotw,
    Eqsl,
}

impl Service {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Service::Lotw => "LoTW",
            Service::Eqsl => "eQSL",
        }
    }
}

/// One QSO's state with one QSL service (or the paper card).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Qsl {
    /// Sent (uploaded) to the service.
    pub sent: bool,
    /// When, `YYYYMMDD`, if known.
    pub sent_on: String,
    /// Confirmed by the other station through the service.
    pub rcvd: bool,
    pub rcvd_on: String,
}

/// One QSO.
///
/// Text fields are empty when unknown; `rst_sent` and `rst_rcvd` are kept as
/// written (`-12`, `+04`, `59`, `599`), so nothing is lost to a parse.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct QsoRecord {
    /// The logbook's own id, for this run only (0 until added). Not written to
    /// the file.
    pub id: u64,
    pub call: String,
    /// Their Maidenhead locator, 4 to 8 characters.
    pub grid: String,
    /// The report sent them.
    pub rst_sent: String,
    /// The report they sent.
    pub rst_rcvd: String,
    /// The RF frequency of the QSO, Hz (0 unknown).
    pub freq_hz: f64,
    /// ADIF MODE (`FT8`, `MFSK`, `SSB`, `CW`).
    pub mode: String,
    /// ADIF SUBMODE (`FT4` under `MFSK`), or empty.
    pub submode: String,
    /// ADIF band (`20m`).
    pub band: String,
    /// Start and end, Unix seconds UTC.
    pub start_utc: i64,
    pub end_utc: i64,
    /// The operator's call and grid at the time (STATION_CALLSIGN,
    /// MY_GRIDSQUARE).
    pub my_call: String,
    pub my_grid: String,
    pub comment: String,
    /// Their name, town, state or province, and (US) county.
    pub name: String,
    pub qth: String,
    pub state: String,
    pub county: String,
    /// Their DXCC entity's name, ADIF number, zones and continent.
    pub country: String,
    pub dxcc: Option<u16>,
    pub cq_zone: Option<u8>,
    pub itu_zone: Option<u8>,
    pub continent: String,
    /// The operator's power, W (TX_PWR).
    pub tx_pwr_w: Option<f32>,
    pub qsl_via: String,
    pub lotw: Qsl,
    pub eqsl: Qsl,
    /// The paper card.
    pub card: Qsl,
    /// Every other ADIF field the record came with, kept so a log imported
    /// from another program loses nothing when this one rewrites the file.
    pub extra: Vec<(String, String)>,
}

/// Modes ADIF files as a SUBMODE under MFSK.
const MFSK_SUBMODES: [&str; 4] = ["FT4", "FST4", "FST4W", "JS8"];

impl QsoRecord {
    /// The mode as the operator knows it: the submode when there is one.
    pub(crate) fn mode_shown(&self) -> &str {
        if self.submode.is_empty() { &self.mode } else { &self.submode }
    }

    /// Upper-case the call and mode, and put `FT4` and its kind under ADIF's
    /// `MFSK`.
    pub(crate) fn normalise(&mut self) {
        self.call = self.call.trim().to_ascii_uppercase();
        self.mode = self.mode.trim().to_ascii_uppercase();
        self.submode = self.submode.trim().to_ascii_uppercase();
        if self.submode.is_empty() && MFSK_SUBMODES.contains(&self.mode.as_str()) {
            self.submode = std::mem::replace(&mut self.mode, "MFSK".into());
        }
        self.band = self.band.trim().to_ascii_lowercase();
        if self.band.is_empty() && self.freq_hz > 0.0 {
            self.band = adif_band(self.freq_hz).to_string();
        }
        if self.end_utc < self.start_utc {
            self.end_utc = self.start_utc;
        }
    }

    pub(crate) fn qsl(&self, s: Service) -> &Qsl {
        match s {
            Service::Lotw => &self.lotw,
            Service::Eqsl => &self.eqsl,
        }
    }

    pub(crate) fn qsl_mut(&mut self, s: Service) -> &mut Qsl {
        match s {
            Service::Lotw => &mut self.lotw,
            Service::Eqsl => &mut self.eqsl,
        }
    }

    /// Confirmed through any service or by card.
    pub(crate) fn confirmed(&self) -> bool {
        self.lotw.rcvd || self.eqsl.rcvd || self.card.rcvd
    }

    /// The first four characters of the grid, upper case, if it has them.
    fn grid4(&self) -> Option<String> {
        grid4(&self.grid)
    }
}

/// The first four characters of a locator, upper case, if they are a valid
/// one.
pub(crate) fn grid4(g: &str) -> Option<String> {
    let g = g.trim().get(..4)?;
    (crate::station::grid_check(g) == crate::station::GridCheck::Valid)
        .then(|| g.to_ascii_uppercase())
}

/// ADIF's band name for a frequency. Taken from SDRoxide's `adif_band`: the
/// edges sit above each band's top in the widest region that has it, so a
/// frequency inside any region's allocation gets the right name. Empty for
/// 11 m and above 3 cm, where ADIF has no band this program would be right to
/// name.
pub(crate) fn adif_band(freq_hz: f64) -> &'static str {
    match freq_hz / 1e6 {
        m if m < 0.1357 => "",
        m if m < 0.1378 => "2190m",
        m if m < 0.472 => "",
        m if m < 0.479 => "630m",
        m if m < 1.8 => "",
        m if m < 2.0 => "160m",
        m if m < 4.0 => "80m",
        m if m < 5.5 => "60m",
        m if m < 7.3 => "40m",
        m if m < 10.5 => "30m",
        m if m < 14.5 => "20m",
        m if m < 18.2 => "17m",
        m if m < 21.5 => "15m",
        m if m < 25.0 => "12m",
        m if m < 27.5 => "",
        m if m < 29.8 => "10m",
        m if m < 54.1 => "6m",
        m if m < 70.6 => "4m",
        m if m < 148.1 => "2m",
        m if m < 225.1 => "1.25m",
        m if m < 450.1 => "70cm",
        m if m < 928.1 => "33cm",
        m if m < 1300.1 => "23cm",
        m if m < 2450.1 => "13cm",
        m if m < 3500.1 => "9cm",
        m if m < 5925.1 => "6cm",
        m if m < 10500.1 => "3cm",
        _ => "",
    }
}

/// What the log says about working a station now.
// For the TX window and DECODE's badges, not yet wired.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Worked {
    /// Already worked on this band and mode.
    pub dupe: bool,
    /// Never worked, on any band or mode.
    pub new_call: bool,
    /// Its DXCC entity has been worked, but not on this band.
    pub new_band: bool,
    /// Its DXCC entity has never been worked.
    pub new_dxcc: bool,
    /// Its grid square (the first four characters) has never been worked.
    pub new_grid: bool,
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportReport {
    pub added: usize,
    /// The same call on the same band within [`IMPORT_DUPE_SECS`] of one in
    /// the log (or earlier in the same file).
    pub dupes: usize,
    /// No call, or no date.
    pub unreadable: usize,
    /// Listening reports (`SWL` set), not QSOs.
    pub swl: usize,
    /// The code page the file was read in when it wasn't Unicode.
    pub assumed: Option<&'static str>,
}

impl ImportReport {
    pub(crate) fn summary(&self) -> String {
        let mut s = format!("{} added, {} already in the log", self.added, self.dupes);
        if self.unreadable > 0 {
            s.push_str(&format!(", {} without a call or date skipped", self.unreadable));
        }
        if self.swl > 0 {
            s.push_str(&format!(", {} listening reports left out", self.swl));
        }
        if let Some(cp) = self.assumed {
            s.push_str(&format!(" (read as {cp})"));
        }
        s
    }
}

/// What the log has worked, kept up to date as QSOs come and go so
/// [`Logbook::worked`] costs a few hash lookups per decode row.
#[derive(Default)]
struct Index {
    calls: HashSet<String>,
    /// (call, band, mode shown)
    call_band_mode: HashSet<(String, String, String)>,
    /// DXCC entities by primary prefix.
    entities: HashSet<String>,
    entity_band: HashSet<(String, String)>,
    grids: HashSet<String>,
    /// ADIF DXCC numbers seen with each entity, by primary prefix, and how
    /// often.
    numbers: HashMap<String, HashMap<u16, u32>>,
}

impl Index {
    fn note(&mut self, r: &QsoRecord) {
        if r.call.is_empty() {
            return;
        }
        self.calls.insert(r.call.clone());
        self.call_band_mode.insert((r.call.clone(), r.band.clone(), r.mode_shown().to_string()));
        if let Some(e) = crate::dxcc::lookup(&r.call) {
            if let Some(n) = r.dxcc {
                *self.numbers.entry(e.prefix.clone()).or_default().entry(n).or_default() += 1;
            }
            self.entity_band.insert((e.prefix.clone(), r.band.clone()));
            self.entities.insert(e.prefix);
        }
        if let Some(g) = r.grid4() {
            self.grids.insert(g);
        }
    }

    fn build(recs: &[QsoRecord]) -> Index {
        let mut ix = Index::default();
        for r in recs {
            ix.note(r);
        }
        ix
    }
}

/// The log: its QSOs and the file they are kept in.
pub(crate) struct Logbook {
    /// The master file; `None` keeps the log in memory only (tests, or a
    /// system with no settings folder).
    path: Option<PathBuf>,
    recs: Vec<QsoRecord>,
    next_id: u64,
    index: Index,
    /// Why the file couldn't be read. While set, nothing is written to it, so
    /// a log that couldn't be read is never replaced by an empty one.
    read_error: Option<String>,
    /// The last failed write, until one works.
    pub error: Option<String>,
    /// Bumped on every change, for anything that caches what it draws.
    generation: u64,
    /// A copy of the file as it was has been kept (`.bak`) this run.
    backed_up: bool,
}

impl Default for Logbook {
    fn default() -> Self {
        Logbook::in_memory()
    }
}

impl Logbook {
    /// Where the master file is: in the panel's settings folder.
    pub(crate) fn default_path() -> Option<PathBuf> {
        crate::decodelog::settings_dir().map(|d| d.join(FILE))
    }

    /// The log in its usual place ([`Logbook::default_path`]).
    pub(crate) fn load_default() -> Logbook {
        match Logbook::default_path() {
            Some(p) => Logbook::load(&p),
            None => {
                let mut l = Logbook::in_memory();
                l.error = Some("No settings folder on this system: the log isn't saved".into());
                l
            }
        }
    }

    /// A log kept nowhere.
    pub(crate) fn in_memory() -> Logbook {
        Logbook {
            path: None,
            recs: Vec::new(),
            next_id: 1,
            index: Index::default(),
            read_error: None,
            error: None,
            generation: 0,
            backed_up: false,
        }
    }

    /// Read the master file at `path`, creating it (with only its header) if it
    /// isn't there.
    pub(crate) fn load(path: &Path) -> Logbook {
        let mut l = Logbook { path: Some(path.to_path_buf()), ..Logbook::in_memory() };
        match std::fs::read(path) {
            Ok(bytes) => {
                let text = adif::decode_text(&bytes).text;
                let (recs, _) = adif::parse(&text);
                for r in recs {
                    l.push(r);
                }
                l.index = Index::build(&l.recs);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Err(e) = l.write_all() {
                    l.error = Some(e);
                }
            }
            Err(e) => {
                let why = format!("Couldn't read the log {}: {e}", path.display());
                tracing::warn!("{why}");
                l.read_error = Some(why);
            }
        }
        l
    }

    /// Where the log is kept.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Every QSO, in the order they were added or read.
    pub(crate) fn records(&self) -> &[QsoRecord] {
        &self.recs
    }

    pub(crate) fn get(&self, id: u64) -> Option<&QsoRecord> {
        self.recs.iter().find(|r| r.id == id)
    }

    /// Changes on every edit to the log.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Why the log can't be used, if it can't: unreadable, or its last write
    /// failed.
    pub(crate) fn problem(&self) -> Option<&str> {
        self.read_error.as_deref().or(self.error.as_deref())
    }

    fn push(&mut self, mut r: QsoRecord) -> u64 {
        r.normalise();
        r.id = self.next_id;
        self.next_id += 1;
        let id = r.id;
        self.recs.push(r);
        id
    }

    /// The ADIF DXCC number for the entity whose primary prefix is `prefix`,
    /// as the QSOs in the log that carry one give it (the country file has
    /// none).
    pub(crate) fn dxcc_number(&self, prefix: &str) -> Option<u16> {
        self.index.numbers.get(prefix)?.iter().max_by_key(|(_, n)| **n).map(|(d, _)| *d)
    }

    /// Fill what the country file knows (country, zones, continent) and the
    /// DXCC number the log has learnt, where `r` has none. Never overwrites.
    pub(crate) fn fill_offline(&self, r: &mut QsoRecord) -> bool {
        let Some(e) = crate::dxcc::lookup(&r.call) else { return false };
        let before = r.clone();
        if r.country.trim().is_empty() {
            r.country = e.name.clone();
        }
        if r.continent.trim().is_empty() {
            r.continent = e.continent.to_string();
        }
        r.cq_zone = r.cq_zone.or(Some(e.cq_zone).filter(|z| *z > 0));
        r.itu_zone = r.itu_zone.or(Some(e.itu_zone).filter(|z| *z > 0));
        r.dxcc = r.dxcc.or_else(|| self.dxcc_number(&e.prefix));
        *r != before
    }

    /// Add a QSO, filling its country, zones and continent from the country
    /// file where it has none, and append it to the file at once. Returns its
    /// id.
    pub(crate) fn add(&mut self, mut r: QsoRecord) -> u64 {
        r.normalise();
        self.fill_offline(&mut r);
        let id = self.push(r);
        let r = self.recs.last().expect("just pushed");
        self.index.note(r);
        self.generation += 1;
        let line = adif::record(r);
        if let Err(e) = self.append(&line) {
            self.error = Some(e);
        }
        id
    }

    /// Replace the QSO with `r.id` by `r` (an edit), and save.
    pub(crate) fn update(&mut self, mut r: QsoRecord) -> bool {
        r.normalise();
        let Some(slot) = self.recs.iter_mut().find(|q| q.id == r.id) else { return false };
        *slot = r;
        self.changed();
        true
    }

    /// Move a QSO's end time on, as when the other station resends its
    /// R+report after the QSO was logged, and save.
    // For the TX window and DECODE's badges, not yet wired.
    #[allow(dead_code)]
    pub(crate) fn update_end(&mut self, id: u64, end_utc: i64) {
        if let Some(r) = self.recs.iter_mut().find(|q| q.id == id)
            && end_utc > r.end_utc
        {
            r.end_utc = end_utc;
            self.changed();
        }
    }

    /// Delete QSOs, and save.
    pub(crate) fn delete(&mut self, ids: &[u64]) -> usize {
        let before = self.recs.len();
        self.recs.retain(|r| !ids.contains(&r.id));
        let n = before - self.recs.len();
        if n > 0 {
            self.changed();
        }
        n
    }

    /// Change many QSOs at once with `f` (which says whether it changed each),
    /// and save once if any changed. Returns how many changed.
    pub(crate) fn modify(&mut self, mut f: impl FnMut(&mut QsoRecord) -> bool) -> usize {
        let mut n = 0;
        for r in &mut self.recs {
            if f(r) {
                n += 1;
            }
        }
        if n > 0 {
            self.changed();
        }
        n
    }

    /// The calls with a QSO that has no grid, each once, leaving out `/`
    /// calls (a portable station's grid is wherever it was that day).
    pub(crate) fn calls_without_grid(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .recs
            .iter()
            .filter(|r| r.grid.trim().is_empty() && !r.call.contains('/') && !r.call.is_empty())
            .map(|r| r.call.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The grid `call` has in another QSO in the log: the latest one that
    /// gives a valid grid. Not for `/` calls.
    pub(crate) fn grid_in_log(&self, call: &str) -> Option<String> {
        if call.contains('/') {
            return None;
        }
        self.recs
            .iter()
            .filter(|r| r.call.eq_ignore_ascii_case(call) && grid4(&r.grid).is_some())
            .max_by_key(|r| r.start_utc)
            .map(|r| r.grid.trim().to_string())
    }

    /// Give every QSO with a call in `grids` and no grid of its own that
    /// call's grid (never a `/` call, never over a grid already there), and
    /// save once. Returns (call, grid, QSOs filled) for each call that filled
    /// any, in call order.
    pub(crate) fn fill_grids(
        &mut self,
        grids: &HashMap<String, String>,
    ) -> Vec<(String, String, usize)> {
        let mut by_call: std::collections::BTreeMap<String, (String, usize)> = Default::default();
        self.modify(|r| {
            if !r.grid.trim().is_empty() || r.call.contains('/') {
                return false;
            }
            let Some(g) = grids.get(&r.call).filter(|g| grid4(g).is_some()) else { return false };
            r.grid = g.trim().to_string();
            by_call.entry(r.call.clone()).or_insert_with(|| (r.grid.clone(), 0)).1 += 1;
            true
        });
        by_call.into_iter().map(|(c, (g, n))| (c, g, n)).collect()
    }

    /// After any change but an add: the index again, and the whole file.
    fn changed(&mut self) {
        self.index = Index::build(&self.recs);
        self.generation += 1;
        if let Err(e) = self.write_all() {
            self.error = Some(e);
        } else {
            self.error = None;
        }
    }

    /// What the log says about working `call` now on `band` in `mode`, with
    /// `grid` if it sent one. `mode` is compared as shown (`FT4`, not `MFSK`).
    pub(crate) fn worked(&self, call: &str, band: &str, mode: &str, grid: Option<&str>) -> Worked {
        let call = call.trim().to_ascii_uppercase();
        if call.is_empty() {
            return Worked::default();
        }
        let band = band.trim().to_ascii_lowercase();
        let mode = mode.trim().to_ascii_uppercase();
        let ix = &self.index;
        let mut w = Worked {
            dupe: ix.call_band_mode.contains(&(call.clone(), band.clone(), mode)),
            new_call: !ix.calls.contains(&call),
            ..Worked::default()
        };
        if let Some(e) = crate::dxcc::lookup(&call) {
            w.new_dxcc = !ix.entities.contains(&e.prefix);
            w.new_band = !w.new_dxcc && !ix.entity_band.contains(&(e.prefix, band));
        }
        if let Some(g) = grid.and_then(grid4) {
            w.new_grid = !ix.grids.contains(&g);
        }
        w
    }

    /// The last QSO with `call`, for "worked before: when and where".
    // For the TX window and DECODE's badges, not yet wired.
    #[allow(dead_code)]
    pub(crate) fn last_with(&self, call: &str) -> Option<&QsoRecord> {
        let call = call.trim();
        self.recs.iter().filter(|r| r.call.eq_ignore_ascii_case(call)).max_by_key(|r| r.start_utc)
    }

    /// How many QSOs there are with `call`.
    // For the TX window and DECODE's badges, not yet wired.
    #[allow(dead_code)]
    pub(crate) fn count_with(&self, call: &str) -> usize {
        let call = call.trim();
        self.recs.iter().filter(|r| r.call.eq_ignore_ascii_case(call)).count()
    }

    /// Read an ADIF document into the log, leaving out QSOs it already has
    /// (the same call and band within [`IMPORT_DUPE_SECS`]), and save.
    pub(crate) fn import_text(&mut self, text: &str) -> ImportReport {
        let (recs, swl) = adif::parse(text);
        let mut rep = ImportReport { swl, ..ImportReport::default() };
        let mut seen: HashMap<(String, String), Vec<i64>> = HashMap::new();
        for r in &self.recs {
            seen.entry((r.call.clone(), r.band.clone())).or_default().push(r.start_utc);
        }
        for mut r in recs {
            r.normalise();
            if r.call.is_empty() || r.start_utc == 0 {
                rep.unreadable += 1;
                continue;
            }
            let times = seen.entry((r.call.clone(), r.band.clone())).or_default();
            if times.iter().any(|t| (t - r.start_utc).abs() <= IMPORT_DUPE_SECS) {
                rep.dupes += 1;
                continue;
            }
            times.push(r.start_utc);
            self.push(r);
            rep.added += 1;
        }
        if rep.added > 0 {
            self.changed();
        }
        rep
    }

    /// [`Logbook::import_text`] from a file, in whatever encoding it is.
    pub(crate) fn import_file(&mut self, path: &Path) -> Result<ImportReport, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let d = adif::decode_text(&bytes);
        let mut rep = self.import_text(&d.text);
        rep.assumed = d.assumed;
        Ok(rep)
    }

    /// The QSOs with these ids (all of them for `None`), as an ADIF document.
    pub(crate) fn export(&self, ids: Option<&[u64]>) -> String {
        adif::document(self.recs.iter().filter(|r| ids.is_none_or(|ids| ids.contains(&r.id))))
    }

    /// Mark QSOs as sent to `service` on `day` (`YYYYMMDD`), and save.
    pub(crate) fn mark_sent(&mut self, ids: &[u64], service: Service, day: &str) -> usize {
        self.modify(|r| {
            if !ids.contains(&r.id) || r.qsl(service).sent {
                return false;
            }
            let q = r.qsl_mut(service);
            q.sent = true;
            q.sent_on = day.to_string();
            true
        })
    }

    /// Mark the QSOs that downloaded confirmations match (same call and band,
    /// the same mode when both say, within a day) as confirmed by `service`, and
    /// save. Returns how many were newly confirmed.
    pub(crate) fn apply_confirmations(&mut self, confs: &[QsoRecord], service: Service) -> usize {
        let mut confs: Vec<QsoRecord> = confs.to_vec();
        for c in &mut confs {
            c.normalise();
        }
        self.modify(|r| {
            if r.qsl(service).rcvd {
                return false;
            }
            let Some(c) = confs.iter().find(|c| {
                c.call == r.call
                    && c.band == r.band
                    && (c.mode.is_empty() || c.mode_shown() == r.mode_shown() || c.mode == r.mode)
                    && (c.start_utc - r.start_utc).abs() < 86_400
            }) else {
                return false;
            };
            let day = c.qsl(service).rcvd_on.clone();
            let q = r.qsl_mut(service);
            q.rcvd = true;
            q.rcvd_on = day;
            // A confirmation was sent, so ours must have been.
            q.sent = true;
            true
        })
    }

    fn append(&mut self, line: &str) -> Result<(), String> {
        use std::io::Write as _;
        if let Some(why) = &self.read_error {
            return Err(format!("Not saved: {why}"));
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        if !path.exists() {
            return self.write_all();
        }
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| format!("Log {}: {e}", path.display()))?;
        f.write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| format!("Log {}: {e}", path.display()))
    }

    /// Write the whole file again, through a temporary file so a failed write
    /// never leaves half a log. The file as it was is kept once a run, as
    /// `.bak`.
    fn write_all(&mut self) -> Result<(), String> {
        if let Some(why) = &self.read_error {
            return Err(format!("Not saved: {why}"));
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        let err = |e: std::io::Error| format!("Log {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        if !self.backed_up && path.exists() {
            let _ = std::fs::copy(&path, path.with_extension("adi.bak"));
            self.backed_up = true;
        }
        let tmp = path.with_extension("adi.tmp");
        std::fs::write(&tmp, adif::document(self.recs.iter())).map_err(err)?;
        std::fs::rename(&tmp, &path).map_err(err)
    }
}

/// A login to an online service. Kept in the settings file, which is plain
/// text, and never shown in a log or a debug print.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Credentials {
    pub user: String,
    pub password: String,
}

impl Credentials {
    pub(crate) fn is_set(&self) -> bool {
        !self.user.trim().is_empty() && !self.password.is_empty()
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "***" })
            .finish()
    }
}

/// Which service looks callsigns up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LookupService {
    /// Free, and the default (AJ3G: HamQTH first, for testing).
    #[default]
    HamQth,
    /// QRZ.com's XML data, which needs a subscription.
    Qrz,
}

impl LookupService {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LookupService::HamQth => "HamQTH",
            LookupService::Qrz => "QRZ.com XML",
        }
    }
}

/// SETTINGS → Logbook, kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LogSettings {
    /// The LOG window was popped out.
    pub popped: bool,
    /// Log a finished QSO without asking (on), or ask first with a card.
    pub auto_log: bool,
    pub lookup: LookupService,
    pub hamqth: Credentials,
    pub qrz: Credentials,
    /// QRZ.com XML is switched on (off until AJ3G subscribes).
    pub qrz_enabled: bool,
    pub eqsl: Credentials,
    /// eQSL's QTH nickname, for an account with more than one QTH profile.
    pub eqsl_qth: String,
    /// The LoTW website login, for downloading confirmations only. Uploads go
    /// through TQSL, which keeps its own certificate and password.
    pub lotw: Credentials,
    /// Where `tqsl` is, if not in its usual place.
    pub tqsl_path: String,
    /// TQSL's station location to sign with (`-l`); empty for TQSL's own
    /// choice.
    pub tqsl_location: String,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            popped: false,
            auto_log: true,
            lookup: LookupService::HamQth,
            hamqth: Credentials::default(),
            qrz: Credentials::default(),
            qrz_enabled: false,
            eqsl: Credentials::default(),
            eqsl_qth: String::new(),
            lotw: Credentials::default(),
            tqsl_path: String::new(),
            tqsl_location: String::new(),
        }
    }
}

impl LogSettings {
    /// The lookup service to use and its login, if it can be used.
    pub(crate) fn lookup_login(&self) -> Result<(LookupService, Credentials), String> {
        match self.lookup {
            LookupService::Qrz if !self.qrz_enabled => {
                Err("QRZ.com XML is switched off in SETTINGS → Logbook".into())
            }
            LookupService::Qrz if !self.qrz.is_set() => {
                Err("Set your QRZ.com login in SETTINGS → Logbook".into())
            }
            LookupService::HamQth if !self.hamqth.is_set() => {
                Err("Set your HamQTH login in SETTINGS → Logbook".into())
            }
            LookupService::Qrz => Ok((LookupService::Qrz, self.qrz.clone())),
            LookupService::HamQth => Ok((LookupService::HamQth, self.hamqth.clone())),
        }
    }
}

/// Today (UTC) as ADIF writes a date.
pub(crate) fn adif_today() -> String {
    let (y, m, d, ..) = crate::decodelog::civil(chrono::Utc::now().timestamp());
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests;
