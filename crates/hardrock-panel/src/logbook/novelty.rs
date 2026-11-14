//! What is new about a station, band by band (decision 122): the badges
//! the decode lists, the scope's labels and GLOBE's spots carry, the line
//! the log gets after each QSO, and the LOG window's NEEDED report.
//!
//! As SDRoxide's `Novelty` and `Highlight` (`sdroxide-types/src/awards.rs`)
//! rank them, rarest first: an entity never worked, an entity new on this
//! band, a grid never worked, a grid new on this band, then a call never
//! worked and a dupe. A band is a band whatever the mode: any mode counts.
//! The colours are magenta, purple and cyan shades: yellow and red are kept
//! for warnings.

use std::collections::{BTreeMap, HashSet};

use eframe::egui::{self, Color32, Pos2, Rect};

use super::{QsoRecord, Worked, grid4};
use crate::theme;

/// One thing new about a station, rarest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Tag {
    /// Its DXCC entity has never been worked.
    Dxcc,
    /// Its entity has been worked, but not on this band.
    DxccBand,
    /// Its grid square has never been worked.
    Grid,
    /// Its grid has been worked, but not on this band.
    GridBand,
    /// The call has never been worked.
    Call,
    /// Already worked on this band and mode.
    Dupe,
}

impl Tag {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tag::Dxcc => "DXCC",
            Tag::DxccBand => "DXCC·BAND",
            Tag::Grid => "GRID",
            Tag::GridBand => "GRID·BAND",
            Tag::Call => "NEW",
            Tag::Dupe => "DUPE",
        }
    }

    pub(crate) fn tip(self) -> &'static str {
        match self {
            Tag::Dxcc => "A DXCC entity you have never worked",
            Tag::DxccBand => "A DXCC entity you have worked, but never on this band",
            Tag::Grid => "A grid square you have never worked",
            Tag::GridBand => "A grid square you have worked, but never on this band",
            Tag::Call => "A call you have never worked",
            Tag::Dupe => "Already in the log on this band and mode",
        }
    }

    /// An award's tag (DXCC or grid): what the scope's labels and GLOBE's
    /// spots show, where a NEW on almost every call would be clutter.
    pub(crate) fn award(self) -> bool {
        matches!(self, Tag::Dxcc | Tag::DxccBand | Tag::Grid | Tag::GridBand)
    }

    /// Its colour: magenta for the rarest, then purple, then cyan, never the
    /// warnings' yellow or red. The Light theme has them darker, as ink.
    pub(crate) fn colour(self) -> Color32 {
        let light = theme::is_light();
        let rgb = |d: u32, l: u32| {
            let c = if light { l } else { d };
            Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
        };
        match self {
            Tag::Dxcc => rgb(0xff5ce1, 0xa3157f),
            Tag::DxccBand => rgb(0xb88cff, 0x6a3fc0),
            Tag::Grid => rgb(0x3fd8ff, 0x00708a),
            Tag::GridBand => rgb(0x86b8d8, 0x3a6680),
            Tag::Call => theme::CYAN_DIM(),
            Tag::Dupe => theme::gray(130),
        }
    }
}

impl Worked {
    /// What is new about the station, rarest first. NEW is left out when
    /// the entity itself is new: DXCC says more.
    pub(crate) fn tags(&self) -> Vec<Tag> {
        [
            (self.new_dxcc, Tag::Dxcc),
            (self.new_band, Tag::DxccBand),
            (self.new_grid, Tag::Grid),
            (self.new_grid_band, Tag::GridBand),
            (self.new_call && !self.new_dxcc, Tag::Call),
            (self.dupe, Tag::Dupe),
        ]
        .into_iter()
        .filter_map(|(on, t)| on.then_some(t))
        .collect()
    }

    /// The rarest award tag, for a label with room for one.
    pub(crate) fn top_award(&self) -> Option<Tag> {
        self.tags().into_iter().find(|t| t.award())
    }

    /// Something new for the log: a call, an entity or a grid, on any band
    /// or on this one ("New only").
    pub(crate) fn is_new(&self) -> bool {
        self.new_call || self.new_dxcc || self.new_band || self.new_grid || self.new_grid_band
    }
}

/// Draw `tag` as a small outlined badge, its left edge's middle at `at`,
/// `size` points high text, faded by `alpha`. Returns where it went.
pub(crate) fn paint(p: &egui::Painter, at: Pos2, tag: Tag, size: f32, alpha: f32) -> Rect {
    let col = tag.colour().gamma_multiply(alpha);
    let g = p.layout_no_wrap(tag.label().into(), egui::FontId::proportional(size), col);
    let r = Rect::from_min_size(
        egui::pos2(at.x, at.y - 0.5 * g.size().y - 1.0),
        g.size() + egui::vec2(6.0, 2.0),
    );
    p.rect_filled(r, 2.0, col.gamma_multiply(0.22));
    p.rect_stroke(r, 2.0, egui::Stroke::new(1.0, col), egui::StrokeKind::Inside);
    p.galley(r.min + egui::vec2(3.0, 1.0), g, col);
    r
}

/// How wide [`paint`] draws `tag`, points.
pub(crate) fn width(p: &egui::Painter, tag: Tag, size: f32) -> f32 {
    p.layout_no_wrap(tag.label().into(), egui::FontId::proportional(size), Color32::WHITE).size().x
        + 6.0
}

/// Something a QSO added to the log: an entity or a grid, all-time or on
/// its band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct New {
    pub tag: Tag,
    /// The entity's name, or the grid's four characters.
    pub what: String,
    pub band: String,
}

/// The log line for a QSO's news: "New on 20m: Italy (DXCC·BAND), JN61
/// (GRID·BAND)"; `None` when there is none.
pub(crate) fn news_line(band: &str, news: &[New]) -> Option<String> {
    if news.is_empty() {
        return None;
    }
    let parts: Vec<String> =
        news.iter().map(|n| format!("{} ({})", n.what, n.tag.label())).collect();
    let band = if band.is_empty() { "this band" } else { band };
    Some(format!("New on {band}: {}", parts.join(", ")))
}

/// The bands in order of frequency, for the NEEDED report.
const BAND_ORDER: [&str; 23] = [
    "2190m", "630m", "160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m",
    "4m", "2m", "1.25m", "70cm", "33cm", "23cm", "13cm", "9cm", "6cm", "3cm",
];

/// One band's line of the NEEDED report: entities and grids worked, and of
/// those, confirmed (LoTW, eQSL or card).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BandTally {
    /// The band, or "All" for the whole log.
    pub band: String,
    pub dxcc: usize,
    pub dxcc_conf: usize,
    pub grids: usize,
    pub grids_conf: usize,
}

#[derive(Default)]
struct Sets {
    dxcc: HashSet<String>,
    dxcc_conf: HashSet<String>,
    grids: HashSet<String>,
    grids_conf: HashSet<String>,
}

impl Sets {
    fn note(&mut self, entity: Option<&str>, grid: Option<&str>, confirmed: bool) {
        if let Some(e) = entity {
            self.dxcc.insert(e.to_string());
            if confirmed {
                self.dxcc_conf.insert(e.to_string());
            }
        }
        if let Some(g) = grid {
            self.grids.insert(g.to_string());
            if confirmed {
                self.grids_conf.insert(g.to_string());
            }
        }
    }

    fn tally(&self, band: &str) -> BandTally {
        BandTally {
            band: band.to_string(),
            dxcc: self.dxcc.len(),
            dxcc_conf: self.dxcc_conf.len(),
            grids: self.grids.len(),
            grids_conf: self.grids_conf.len(),
        }
    }
}

/// The NEEDED report's figures: each band's, in order of frequency (bands
/// not in ADIF's list after, by name), then the whole log's ("All").
pub(crate) fn tally(recs: &[QsoRecord]) -> Vec<BandTally> {
    let mut by_band: BTreeMap<String, Sets> = BTreeMap::new();
    let mut all = Sets::default();
    for r in recs {
        if r.call.is_empty() {
            continue;
        }
        let entity = crate::dxcc::lookup(&r.call).map(|e| e.prefix);
        let grid = grid4(&r.grid);
        let conf = r.confirmed();
        all.note(entity.as_deref(), grid.as_deref(), conf);
        if !r.band.is_empty() {
            by_band.entry(r.band.clone()).or_default().note(
                entity.as_deref(),
                grid.as_deref(),
                conf,
            );
        }
    }
    let rank = |b: &str| BAND_ORDER.iter().position(|o| *o == b).unwrap_or(BAND_ORDER.len());
    let mut out: Vec<BandTally> = by_band.iter().map(|(b, s)| s.tally(b)).collect();
    out.sort_by(|a, b| rank(&a.band).cmp(&rank(&b.band)).then_with(|| a.band.cmp(&b.band)));
    out.push(all.tally("All"));
    out
}
