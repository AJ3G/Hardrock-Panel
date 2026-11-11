//! The DXCC entity, CQ and ITU zones and continent of a callsign, from the
//! country file `cty.dat` built in, with no lookup and no network.
//!
//! Ported from SDRoxide's `sdroxide-types/src/entity.rs`
//! (https://github.com/dividebysandwich/sdroxide, GPL-3.0-or-later). The
//! country file, `assets/cty.dat`, is Jim Reisert AD1C's
//! (https://www.country-files.com/), free to redistribute with that credit.
//! It is parsed once, the first time a call is looked up, into a
//! longest-prefix matcher.
//!
//! Also the beam heading (short and long path) and great-circle distance to a
//! station, from its grid when it sent one and from its entity's centre when it
//! didn't. The sums themselves are [`crate::station`]'s.
//!
//! The country file carries no ADIF DXCC numbers, so [`Entity::dxcc`] is
//! `None` from here; the logbook learns the numbers from QSOs that carry one
//! (an import, or a callsign lookup) and fills them in (see
//! [`crate::logbook::Logbook::dxcc_number`]).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::station::{LatLon, bearing_deg, distance_km, grid_to_latlon};

/// The country file.
const CTY: &str = include_str!("../assets/cty.dat");

/// A callsign's DXCC entity, as the country file gives it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entity {
    /// The entity's name (`"Fed. Rep. of Germany"`).
    pub name: String,
    /// The entity's primary prefix (`"DL"`), the country file's own key for
    /// it, which unlike the name is never reworded. Two calls are in the same
    /// DXCC entity exactly when these match.
    pub prefix: String,
    /// The flag the entity flies, a code [`crate::flags::Flags`] draws
    /// (`"DE"`, `"GB-SCT"`), or `""` for none (decision 105).
    pub flag: &'static str,
    /// The ADIF DXCC entity number. The country file has none: always `None`
    /// from [`lookup`].
    pub dxcc: Option<u16>,
    /// `NA`, `SA`, `EU`, `AF`, `AS`, `OC` or `AN`.
    pub continent: &'static str,
    pub cq_zone: u8,
    pub itu_zone: u8,
    /// The entity's nominal centre (or the sub-area's, for the parts the
    /// country file lists apart for WAE: Sicily, Shetland), degrees north and
    /// east. For a large entity this can be two thousand kilometres from the
    /// station.
    pub lat: f64,
    pub lon: f64,
}

/// One prefix (or exact call) and the zones and continent it carries.
struct Pfx {
    key: &'static str,
    ent: usize,
    cq: u8,
    itu: u8,
    cont: &'static str,
}

/// One entity line of the country file.
struct Ent {
    name: &'static str,
    /// The primary prefix, without the WAE `*`.
    prefix: &'static str,
    /// For a WAE-only area (a `*` line), the entity it counts as for DXCC.
    dxcc_parent: Option<&'static str>,
    lat: f64,
    lon: f64,
}

struct Cty {
    ents: Vec<Ent>,
    /// Prefixes by first byte, each bucket longest first.
    by_first: HashMap<u8, Vec<Pfx>>,
    /// Whole-call overrides (the `=CALL` entries).
    exact: HashMap<&'static str, Pfx>,
}

fn cty() -> &'static Cty {
    static C: OnceLock<Cty> = OnceLock::new();
    C.get_or_init(parse)
}

/// The DXCC entity each WAE-only area of the country file counts as. These
/// are the six `*` lines in it: they are worked for WAE, but for DXCC they
/// are part of another entity.
fn wae_parent(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "4U1V" => "OE",       // Vienna Intl Ctr: Austria
        "GM/s" => "GM",       // Shetland: Scotland
        "IG9" | "IT9" => "I", // African Italy, Sicily: Italy
        "JW/b" => "JW",       // Bear Island: Svalbard
        "TA1" => "TA",        // European Turkey: Turkey
        _ => return None,
    })
}

fn parse() -> Cty {
    let mut ents = Vec::new();
    let mut by_first: HashMap<u8, Vec<Pfx>> = HashMap::new();
    let mut exact: HashMap<&'static str, Pfx> = HashMap::new();
    let lines: Vec<&'static str> = CTY.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // An entity line is unindented, with eight colon-separated fields.
        if line.starts_with(char::is_whitespace) || !line.contains(':') {
            i += 1;
            continue;
        }
        let f: Vec<&'static str> = line.split(':').collect();
        if f.len() < 8 {
            i += 1;
            continue;
        }
        let cq: u8 = f[1].trim().parse().unwrap_or(0);
        let itu: u8 = f[2].trim().parse().unwrap_or(0);
        let cont = f[3].trim();
        // The country file's longitude is west-positive.
        let lat: f64 = f[4].trim().parse().unwrap_or(0.0);
        let lon: f64 = f[5].trim().parse().map(|w: f64| -w).unwrap_or(0.0);
        let starred = f[7].trim();
        let prefix = starred.trim_start_matches('*');
        let dxcc_parent = if starred.starts_with('*') { wae_parent(prefix) } else { None };
        let ent = ents.len();
        ents.push(Ent { name: f[0].trim(), prefix, dxcc_parent, lat, lon });
        // The prefix list runs over the indented lines until one ends `;`.
        i += 1;
        while i < lines.len() {
            let l = lines[i].trim();
            let ends = l.ends_with(';');
            for tok in l.trim_end_matches(';').split(',') {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                let (is_exact, key, cqo, ituo, conto) = parse_token(tok);
                if key.is_empty() {
                    continue;
                }
                let p = Pfx {
                    key,
                    ent,
                    cq: cqo.unwrap_or(cq),
                    itu: ituo.unwrap_or(itu),
                    cont: conto.unwrap_or(cont),
                };
                if is_exact {
                    exact.insert(key, p);
                } else {
                    by_first.entry(key.as_bytes()[0]).or_default().push(p);
                }
            }
            i += 1;
            if ends {
                break;
            }
        }
    }
    for v in by_first.values_mut() {
        v.sort_by_key(|p| std::cmp::Reverse(p.key.len()));
    }
    Cty { ents, by_first, exact }
}

/// One prefix token: an optional `=` (a whole call), the prefix or call, then
/// optional `(cq)`, `[itu]` and `{continent}` overrides. `<lat/lon>` and
/// `~offset~` are ignored.
fn parse_token(
    tok: &'static str,
) -> (bool, &'static str, Option<u8>, Option<u8>, Option<&'static str>) {
    let is_exact = tok.starts_with('=');
    let body = if is_exact { &tok[1..] } else { tok };
    let end = body.find(['(', '[', '{', '<', '~']).unwrap_or(body.len());
    let between = |open: char, close: char| {
        let a = body.find(open)?;
        let b = body[a..].find(close)? + a;
        Some(body[a + 1..b].trim())
    };
    (
        is_exact,
        &body[..end],
        between('(', ')').and_then(|s| s.parse().ok()),
        between('[', ']').and_then(|s| s.parse().ok()),
        between('{', '}'),
    )
}

impl Cty {
    /// The name and primary prefix an entity line counts as for DXCC: its
    /// own, or for a WAE-only area its parent entity's.
    fn dxcc_of(&self, e: &Ent) -> (&'static str, &'static str) {
        match e.dxcc_parent.and_then(|pp| self.ents.iter().find(|x| x.prefix == pp)) {
            Some(parent) => (parent.name, parent.prefix),
            None => (e.name, e.prefix),
        }
    }

    fn find(&self, call: &str) -> Option<&Pfx> {
        let up = call.trim().to_ascii_uppercase();
        if up.is_empty() {
            return None;
        }
        if let Some(p) = self.exact.get(up.as_str()) {
            return Some(p);
        }
        let key = dxcc_key(&up);
        if let Some(p) = self.exact.get(key.as_str()) {
            return Some(p);
        }
        let bucket = self.by_first.get(key.as_bytes().first()?)?;
        bucket.iter().find(|p| key.starts_with(p.key))
    }

    fn entity(&self, p: &Pfx) -> Entity {
        let e = &self.ents[p.ent];
        // A WAE-only area counts as its DXCC entity, by that entity's name and
        // prefix, but keeps its own zones, continent and centre.
        let (name, prefix) = self.dxcc_of(e);
        Entity {
            name: name.to_string(),
            prefix: prefix.to_string(),
            flag: crate::entity_flags::flag_for_prefix(prefix),
            dxcc: None,
            continent: p.cont,
            cq_zone: p.cq,
            itu_zone: p.itu,
            lat: e.lat,
            lon: e.lon,
        }
    }
}

/// The DXCC entity, zones and continent of `call`, or `None` when the country
/// file has no prefix for it. A `/` call is placed by its location part
/// (`DL/W1AW` is in Germany, `W1AW/KH6` in Hawaii), and `/P`, `/M`, `/QRP`
/// and the like are ignored.
pub(crate) fn lookup(call: &str) -> Option<Entity> {
    let c = cty();
    c.find(call).map(|p| c.entity(p))
}

/// The flag code and entity name of `call`, as [`lookup`] would give them but
/// without building an [`Entity`]: for the decode lists, which ask for every
/// row they draw, every frame. `None` when the country file has no prefix for
/// the call; a code of `""` when the entity flies no flag we ship.
pub(crate) fn flag_of(call: &str) -> Option<(&'static str, &'static str)> {
    let c = cty();
    let p = c.find(call)?;
    let (name, prefix) = c.dxcc_of(&c.ents[p.ent]);
    Some((crate::entity_flags::flag_for_prefix(prefix), name))
}

/// The entity a token names when it is meant *as* a prefix (the `JA` of
/// `CQ JA`): only a prefix the country file lists, never a longest match, so
/// `CQ ZZZZ` names nothing.
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
pub(crate) fn lookup_prefix(prefix: &str) -> Option<Entity> {
    let c = cty();
    let up = prefix.trim().to_ascii_uppercase();
    let bucket = c.by_first.get(up.as_bytes().first()?)?;
    bucket.iter().find(|p| p.key == up).map(|p| c.entity(p))
}

/// How many entities the country file lists for DXCC (the WAE-only areas left
/// out).
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
pub(crate) fn entity_count() -> usize {
    cty().ents.iter().filter(|e| e.dxcc_parent.is_none()).count()
}

/// The part of a `/` call that says where it is: pure suffixes (`/P`, `/M`,
/// `/MM`, `/QRP`, a lone digit…) dropped, then the shorter of what is left,
/// which is the location prefix rather than the operator's home call.
fn dxcc_key(call: &str) -> String {
    if !call.contains('/') {
        return call.to_string();
    }
    const SUFFIXES: &[&str] = &["P", "M", "MM", "AM", "QRP", "A", "LH", "J", "R", "T"];
    let parts: Vec<&str> = call.split('/').filter(|p| !p.is_empty()).collect();
    let mut cand: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| {
            !(SUFFIXES.contains(p) || (p.len() == 1 && p.bytes().all(|b| b.is_ascii_digit())))
        })
        .collect();
    if cand.is_empty() {
        cand = parts;
    }
    cand.iter().copied().min_by_key(|c| c.len()).unwrap_or("").to_string()
}

/// Where a station is, and whether that is its own grid or only its
/// entity's centre (a guess, to be shown as one).
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fix {
    pub pos: LatLon,
    /// From the grid it sent; `false` when from its DXCC entity's centre.
    pub from_grid: bool,
}

/// Where `call` is: its grid's centre when `grid` is a valid locator, else its
/// DXCC entity's centre.
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
pub(crate) fn locate(call: &str, grid: Option<&str>) -> Option<Fix> {
    if let Some(p) = grid.and_then(grid_to_latlon) {
        return Some(Fix { pos: p, from_grid: true });
    }
    let e = lookup(call)?;
    Some(Fix { pos: LatLon { lat: e.lat, lon: e.lon }, from_grid: false })
}

/// The way to a station: great-circle distance, and the beam heading both ways
/// round.
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Path {
    pub km: f64,
    /// Short-path heading, degrees from true north.
    pub short_deg: f64,
    /// Long-path heading: the short path's opposite.
    pub long_deg: f64,
    /// Measured to the station's grid; `false` when to its entity's centre.
    pub from_grid: bool,
}

/// The path from `from` to a place.
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
pub(crate) fn path(from: LatLon, to: LatLon) -> Path {
    let short = bearing_deg(from, to);
    Path {
        km: distance_km(from, to),
        short_deg: short,
        long_deg: (short + 180.0).rem_euclid(360.0),
        from_grid: true,
    }
}

/// The path from the operator at `from` to `call`, placed by its grid when it
/// sent one and by its entity otherwise ([`locate`]).
// For the TX window and DECODE's badges (decision 69), not yet wired.
#[allow(dead_code)]
pub(crate) fn path_to(from: LatLon, call: &str, grid: Option<&str>) -> Option<Path> {
    let fix = locate(call, grid)?;
    Some(Path { from_grid: fix.from_grid, ..path(from, fix.pos) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(call: &str) -> String {
        lookup(call).map(|e| e.name).unwrap_or_default()
    }

    #[test]
    fn common_calls_resolve() {
        assert_eq!(name("W1AW"), "United States");
        assert_eq!(name("aj3g"), "United States");
        assert_eq!(name("DL1ABC"), "Fed. Rep. of Germany");
        assert_eq!(name("JA1XYZ"), "Japan");
        assert_eq!(name("G0ABC"), "England");
        assert_eq!(name("VK2DEF"), "Australia");
        assert_eq!(name("VE3ABC"), "Canada");
        assert_eq!(name("ZS6ABC"), "South Africa");
        assert_eq!(lookup(""), None);
        let de = lookup("DL1ABC").unwrap();
        assert_eq!(
            (de.cq_zone, de.itu_zone, de.continent, de.prefix.as_str()),
            (14, 28, "EU", "DL")
        );
        assert_eq!(de.dxcc, None);
    }

    #[test]
    fn a_portable_call_is_where_it_operates() {
        assert_eq!(name("W1AW/KH6"), "Hawaii");
        assert_eq!(name("DL/W1AW"), "Fed. Rep. of Germany");
        assert_eq!(name("G0ABC/P"), "England");
        assert_eq!(name("W1AW/QRP"), "United States");
    }

    /// The country file's WAE-only areas count as their DXCC entity, so
    /// Sicily is a new band slot for Italy and never a new country.
    #[test]
    fn wae_areas_count_as_their_dxcc_entity() {
        let it9 = lookup("IT9ABC").unwrap();
        assert_eq!((it9.name.as_str(), it9.prefix.as_str()), ("Italy", "I"));
        assert_eq!(lookup("GM4ABC").unwrap().prefix, "GM");
        assert_eq!(lookup("TA1ABC").unwrap().prefix, "TA");
        // And keeps its own continent: African Italy is in Africa.
        assert_eq!(lookup("IG9ABC").unwrap().continent, "AF");
        assert!(entity_count() > 330, "{} entities", entity_count());
    }

    /// The flag table is hand-written against the country file, so it goes
    /// stale silently when cty.dat is updated and gains an entity. Every
    /// entity line has a row (a flagless one says so with `""`), every row
    /// names an entity, and every flag a row names is a PNG we ship.
    #[test]
    fn every_entity_has_a_flag_row_and_its_flag_is_shipped() {
        let c = cty();
        let prefixes: Vec<&str> = c.ents.iter().map(|e| e.prefix).collect();
        let missing: Vec<&str> =
            prefixes.iter().copied().filter(|p| !crate::entity_flags::covers_prefix(p)).collect();
        assert!(missing.is_empty(), "no flag-table row for DXCC prefixes {missing:?}");
        let stale: Vec<&str> = crate::entity_flags::all_prefixes()
            .into_iter()
            .filter(|p| !prefixes.contains(p))
            .collect();
        assert!(stale.is_empty(), "flag-table rows for unknown DXCC prefixes {stale:?}");
        for p in prefixes {
            let f = crate::entity_flags::flag_for_prefix(p);
            assert!(f.is_empty() || crate::flags::shipped(f), "{p} names {f}, which has no PNG");
        }
    }

    #[test]
    fn a_call_resolves_to_its_flag() {
        let flag = |call: &str| lookup(call).map(|e| e.flag);
        assert_eq!(flag("DL1ABC"), Some("DE"));
        assert_eq!(flag("W1AW"), Some("US"));
        // Pieces of a country that fly their own flag.
        assert_eq!(flag("KL7ABC"), Some("US-AK"));
        assert_eq!(flag("GM4ABC"), Some("GB-SCT"));
        // A WAE-only area flies its DXCC entity's flag.
        assert_eq!(flag("IT9ABC"), Some("IT"));
        // A dependency flies the flag of whoever administers it.
        assert_eq!(flag("FT4XA"), Some("TF"));
        // The light lookup agrees with the full one.
        assert_eq!(flag_of("DL1ABC"), Some(("DE", "Fed. Rep. of Germany")));
        assert_eq!(flag_of("IT9ABC"), Some(("IT", "Italy")));
        // Unknown: no entity, so no flag.
        assert_eq!(flag_of(""), None);
        assert_eq!(crate::entity_flags::flag_for_prefix("ZZZZ"), "");
    }

    #[test]
    fn a_prefix_must_be_listed_to_name_an_entity() {
        assert_eq!(lookup_prefix("JA").unwrap().name, "Japan");
        assert_eq!(lookup_prefix("ZZZZ"), None);
    }

    #[test]
    fn longitudes_are_east_positive() {
        let us = lookup("K1ABC").unwrap();
        assert!(us.lon < -60.0 && us.lon > -130.0, "the USA at {}", us.lon);
        let de = lookup("DL1ABC").unwrap();
        assert!(de.lon > 5.0 && de.lon < 20.0, "Germany at {}", de.lon);
    }

    #[test]
    fn the_path_comes_from_the_grid_or_else_the_entity() {
        let me = grid_to_latlon("FN20").unwrap();
        let p = path_to(me, "JA1ABC", Some("PM95")).unwrap();
        assert!(p.from_grid);
        // FN20 to PM95 is about 10 900 km, a little west of north.
        assert!((10_500.0..11_300.0).contains(&p.km), "{} km", p.km);
        assert!((320.0..345.0).contains(&p.short_deg), "{}°", p.short_deg);
        assert!((p.long_deg - (p.short_deg - 180.0)).abs() < 1e-9);
        let g = path_to(me, "JA1ABC", None).unwrap();
        assert!(!g.from_grid);
        assert!((9_500.0..12_000.0).contains(&g.km), "{} km", g.km);
        assert_eq!(path_to(me, "", None), None);
    }
}
