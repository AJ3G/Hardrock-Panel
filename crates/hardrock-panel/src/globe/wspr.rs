//! WSPR HEARD ME: who heard AJ3G's WSPR beacon in the last hour.
//!
//! Asked of wspr.live first (https://wspr.live, the public, read-only
//! ClickHouse mirror of the WSPRnet database), where most WSPR reception
//! reports are, and of WSPRnet's own spot search when wspr.live is down. The
//! WSPRnet half is ported from SDRoxide's "who heard us" poll
//! (`sdroxide-net/src/wsprnet.rs`, https://github.com/dividebysandwich/sdroxide,
//! GPL-3.0-or-later): its query, its percent-encoding and its defensive
//! reading of a reply whose numbers come as strings.
//!
//! Nothing is sent but the query, and the only thing in it about the station
//! is its callsign.

use serde_json::Value;

use super::fetch::parse_time;
use super::field::{Reception, Source};
use crate::station::grid_to_latlon;

/// wspr.live's query endpoint (ClickHouse over HTTP).
const WSPR_LIVE_URL: &str = "https://db1.wspr.live/";
/// WSPRnet's spot search, as SDRoxide reads it.
const WSPRNET_URL: &str = "https://www.wsprnet.org/drupal/wsprnet/spots/json";

/// One report of AJ3G's beacon: the station that heard it, as a reverse
/// reception, and what WSPR adds to a report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WsprReport {
    pub rec: Reception,
    /// The drift the receiver measured across the transmission, Hz.
    pub drift_hz: Option<f32>,
    /// The power AJ3G's beacon declared, dBm, as the receiver decoded it.
    pub power_dbm: Option<f32>,
    /// The distance the site worked out, km.
    pub km: Option<f64>,
    /// The site the report came from: "wspr.live" or "WSPRnet".
    pub site: &'static str,
}

/// The callsign as it may go into a query, uppercased: letters, digits and
/// `/` only, so nothing else can ride along in the SQL or the URL.
pub(crate) fn query_call(call: &str) -> Option<String> {
    let c = call.trim().to_ascii_uppercase();
    let ok = (3..=16).contains(&c.len())
        && c.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'/');
    ok.then_some(c)
}

/// Percent-encode everything that is not unreserved (SDRoxide's).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The wspr.live query: every report of `call` as the transmitter in the
/// last hour, newest first.
pub(crate) fn wspr_live_url(call: &str) -> Option<String> {
    let c = query_call(call)?;
    let sql = format!(
        "SELECT time, band, rx_sign, rx_loc, frequency, power, snr, drift, distance \
         FROM wspr.rx WHERE time > subtractHours(now(), 1) AND tx_sign = '{c}' \
         ORDER BY time DESC LIMIT 1000 FORMAT JSON"
    );
    Some(format!("{WSPR_LIVE_URL}?query={}", percent_encode(&sql)))
}

/// WSPRnet's search for spots of `call` (who heard it) in the last hour,
/// balloon telemetry left out, as SDRoxide asks.
pub(crate) fn wsprnet_url(call: &str) -> Option<String> {
    let c = query_call(call)?;
    Some(format!("{WSPRNET_URL}?callsign={}&minutes=60&exclude_special=1", percent_encode(&c)))
}

/// A number that may have come as a JSON number or as a string (ClickHouse
/// quotes its 64-bit integers; WSPRnet quotes everything).
fn num(o: &Value, key: &str) -> Option<f64> {
    match o.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text<'a>(o: &'a Value, key: &str) -> Option<&'a str> {
    o.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

/// A receiver's place from its locator: six characters when it gave them,
/// else four.
fn place(grid: &str) -> Option<(String, crate::station::LatLon)> {
    let g: String = grid.chars().take(if grid.len() >= 6 { 6 } else { 4 }).collect();
    let pos =
        grid_to_latlon(&g).or_else(|| grid_to_latlon(&grid.chars().take(4).collect::<String>()))?;
    Some((g, pos))
}

#[allow(clippy::too_many_arguments)]
fn report(
    rx: &str,
    grid: &str,
    rf_hz: f64,
    snr: Option<f64>,
    utc: f64,
    drift: Option<f64>,
    power: Option<f64>,
    km: Option<f64>,
    site: &'static str,
) -> Option<WsprReport> {
    let (grid, pos) = place(grid)?;
    Some(WsprReport {
        rec: Reception {
            call: rx.to_ascii_uppercase(),
            grid,
            pos,
            rf_hz,
            mode: "WSPR".into(),
            snr: snr.map(|s| s as f32),
            // The slot, not the second within it.
            utc: (utc / 120.0).floor() * 120.0,
            src: Source::WsprHeard,
            row: None,
            from: None,
        },
        drift_hz: drift.map(|d| d as f32),
        power_dbm: power.map(|p| p as f32),
        km,
        site,
    })
}

/// wspr.live's JSON answer: `{"meta": …, "data": [{"time": "2026-11-10
/// 21:14:00", "rx_sign": …, "frequency": "14097071", …}], "rows": …}`. A row
/// that lacks a field costs that row, not the answer.
pub(crate) fn parse_wspr_live(t: &str) -> Result<Vec<WsprReport>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| format!("wspr.live: {e}"))?;
    let rows = v.get("data").and_then(|d| d.as_array()).ok_or("wspr.live: no data")?;
    Ok(rows
        .iter()
        .filter_map(|o| {
            report(
                text(o, "rx_sign")?,
                text(o, "rx_loc")?,
                num(o, "frequency")?,
                num(o, "snr"),
                parse_time(text(o, "time")?)?,
                num(o, "drift"),
                num(o, "power"),
                num(o, "distance"),
                "wspr.live",
            )
        })
        .collect())
}

/// WSPRnet's spot search (ported from SDRoxide's `parse_spots`): a list of
/// objects with capitalised keys and numbers as strings. `Reporter` is the
/// station that heard the call.
pub(crate) fn parse_wsprnet(t: &str) -> Result<Vec<WsprReport>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| format!("WSPRnet: {e}"))?;
    let arr = v.as_array().ok_or("WSPRnet did not answer with a list")?;
    Ok(arr
        .iter()
        .filter_map(|o| {
            report(
                text(o, "Reporter")?,
                text(o, "ReporterGrid")?,
                num(o, "MHz")? * 1e6,
                num(o, "dB"),
                num(o, "Date")?,
                num(o, "Drift"),
                num(o, "Power"),
                num(o, "distance"),
                "WSPRnet",
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply in the shape ClickHouse's FORMAT JSON gives (the container
    /// can't reach wspr.live, so not a saved live reply): 64-bit integers
    /// quoted, the rest as numbers.
    const WSPR_LIVE: &str = r#"{
        "meta": [{"name":"time","type":"DateTime"},{"name":"band","type":"Int16"},
                 {"name":"rx_sign","type":"LowCardinality(String)"},{"name":"rx_loc","type":"LowCardinality(String)"},
                 {"name":"frequency","type":"UInt64"},{"name":"power","type":"Int8"},{"name":"snr","type":"Int8"},
                 {"name":"drift","type":"Int8"},{"name":"distance","type":"UInt16"}],
        "data": [
            {"time":"2026-11-10 21:14:00","band":14,"rx_sign":"G4XYZ","rx_loc":"IO91wm","frequency":"14097071","power":37,"snr":-22,"drift":0,"distance":5900},
            {"time":"2026-11-10 21:12:00","band":14,"rx_sign":"vk3tdx","rx_loc":"QF22","frequency":"14097063","power":37,"snr":-27,"drift":-1,"distance":16640},
            {"time":"2026-11-10 21:12:00","band":14,"rx_sign":"NOGRID","rx_loc":"","frequency":"14097063","power":37,"snr":-20,"drift":0,"distance":0}
        ],
        "rows": 3,
        "statistics": {"elapsed":0.01,"rows_read":3,"bytes_read":300}
    }"#;

    #[test]
    fn wspr_live_rows_become_stations_that_heard_the_station() {
        let r = parse_wspr_live(WSPR_LIVE).unwrap();
        assert_eq!(r.len(), 2);
        let g = &r[0];
        assert_eq!((g.rec.call.as_str(), g.rec.grid.as_str()), ("G4XYZ", "IO91wm"));
        assert_eq!(g.rec.src, Source::WsprHeard);
        assert!(g.rec.src.reverse());
        assert_eq!(g.rec.mode, "WSPR");
        assert_eq!(g.rec.rf_hz, 14_097_071.0);
        assert_eq!(g.rec.band(), Some("20"));
        assert_eq!(g.rec.snr, Some(-22.0));
        assert_eq!(g.rec.utc, parse_time("2026-11-10 21:14:00").unwrap());
        assert_eq!((g.drift_hz, g.power_dbm, g.km), (Some(0.0), Some(37.0), Some(5900.0)));
        assert_eq!(g.site, "wspr.live");
        assert_eq!(r[1].rec.call, "VK3TDX");
        assert_eq!(r[1].rec.grid, "QF22");
        assert!(parse_wspr_live("{}").is_err());
        assert!(parse_wspr_live("not json").is_err());
    }

    #[test]
    fn wsprnet_spots_are_read_as_sdroxide_reads_them() {
        let body = r#"[
          {"Spotnum":"1234","Date":"1785242040","Reporter":"G0XYZ",
           "ReporterGrid":"IO91np","dB":"-24","MHz":"14.097090","CallSign":"AJ3G",
           "Grid":"FN20","Power":"37","Drift":"-1","distance":"5400","azimuth":"51",
           "Band":"14","version":"","code":"0"},
          {"CallSign":"AJ3G","MHz":"14.097090","Date":"1785242040","dB":"-24"}
        ]"#;
        let r = parse_wsprnet(body).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].rec.call.as_str(), r[0].rec.grid.as_str()), ("G0XYZ", "IO91np"));
        assert!((r[0].rec.rf_hz - 14_097_090.0).abs() < 0.5);
        assert_eq!(r[0].rec.utc % 120.0, 0.0);
        assert_eq!((r[0].drift_hz, r[0].km, r[0].site), (Some(-1.0), Some(5400.0), "WSPRnet"));
        assert!(parse_wsprnet("{}").is_err());
    }

    #[test]
    fn only_the_callsign_goes_in_the_query() {
        let u = wspr_live_url("aj3g").unwrap();
        assert!(u.starts_with("https://db1.wspr.live/?query=SELECT"));
        assert!(u.contains("tx_sign%20%3D%20%27AJ3G%27"), "{u}");
        assert!(u.ends_with("FORMAT%20JSON"));
        // A compound call is escaped, not split.
        assert!(wspr_live_url("PJ4/AJ3G").unwrap().contains("PJ4%2FAJ3G"));
        assert_eq!(
            wsprnet_url("AJ3G").unwrap(),
            "https://www.wsprnet.org/drupal/wsprnet/spots/json?callsign=AJ3G&minutes=60&exclude_special=1"
        );
        // Anything that isn't a callsign is never sent.
        assert_eq!(wspr_live_url("AJ3G' OR 1=1 --"), None);
        assert_eq!(query_call("A"), None);
    }
}
