//! What GLOBE fetches, and only while its window is open: closing it stops
//! the fetcher, and never opening it means no request is made (as
//! SDRoxide's 3D view does, its manual's §7.14).
//!
//! | Host | What | Every |
//! | --- | --- | --- |
//! | services.swpc.noaa.gov | Kp and A, the Kp forecast, 10.7 cm flux, GOES X-ray, solar wind | 5 to 60 min |
//! | services.swpc.noaa.gov | The OVATION aurora grid | 15 min |
//! | prop.kc2g.com | Ionosonde MUF(3000) (GIRO, gathered by KC2G) | 15 min |
//! | nowcoast.noaa.gov | The global infrared cloud mosaic (NOAA/NESDIS GMGSI) | 30 min |
//! | api.open-meteo.com | The weather at the station's grid | 15 min |
//! | api.weather.gov | Weather alerts at the station (United States only) | 15 min |
//! | retrieve.pskreporter.info | Who heard the station's callsign in the last hour (PSK REPORTER on) | 5 min |
//!
//! Each is parsed here by a plain function, so the tests read sample answers
//! and the container never calls out. Nothing is ever sent but the request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::field::{Reception, Source};
use crate::station::{LatLon, grid_to_latlon};

/// Space weather, as last fetched.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SpaceWx {
    /// The latest planetary K index and A, and when.
    pub kp: Option<f64>,
    pub a: Option<f64>,
    /// Three-hour Kp bins, oldest first: (Unix seconds, Kp, forecast).
    pub kp_bins: Vec<(f64, f64, bool)>,
    /// 10.7 cm solar flux, sfu.
    pub sfi: Option<f64>,
    /// GOES 0.1–0.8 nm X-ray flux, W/m².
    pub xray: Option<f64>,
    /// The solar wind: Bz (GSM) nT, and speed km/s.
    pub bz: Option<f64>,
    pub wind: Option<f64>,
}

/// One ionosonde's last sounding.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ionosonde {
    pub name: String,
    pub pos: LatLon,
    /// MUF(3000)F2, MHz.
    pub mufd: f64,
    pub utc: f64,
}

/// The weather at the station.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocalWx {
    pub temp_c: Option<f64>,
    pub humidity: Option<f64>,
    /// WMO weather code.
    pub code: Option<i64>,
    pub wind_kmh: Option<f64>,
    pub wind_from_deg: Option<f64>,
    pub gust_kmh: Option<f64>,
    pub pressure_hpa: Option<f64>,
    /// The pressure's change over the last three hours, hPa.
    pub trend_hpa: Option<f64>,
}

/// The aurora's chance, percent, on a 1° grid: 360 longitudes (0 to 359 E)
/// by 181 latitudes (90 S to 90 N).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Aurora(pub Vec<u8>);

impl Aurora {
    pub(crate) fn at(&self, p: LatLon) -> f32 {
        let lon = (p.lon.rem_euclid(360.0).round() as usize) % 360;
        let lat = ((p.lat + 90.0).round().clamp(0.0, 180.0)) as usize;
        self.0.get(lat * 360 + lon).copied().unwrap_or(0) as f32
    }
}

/// The cloud mosaic as brightness 0 to 255, equirectangular, row 0 at 90° N.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Clouds {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// What a source last did: fetched at, or failed with.
#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub ok_at: Option<f64>,
    pub error: Option<String>,
}

/// Everything fetched, shared with the window.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fetched {
    pub space: SpaceWx,
    pub aurora: Option<Aurora>,
    pub sondes: Vec<Ionosonde>,
    pub clouds: Option<Clouds>,
    pub wx: Option<LocalWx>,
    pub alerts: Vec<String>,
    /// PSK Reporter: who heard the station, the whole last hour.
    pub heard_me: Vec<Reception>,
    /// Bumped each time anything arrives.
    pub serial: u64,
    /// By source name: SWPC, OVATION, KC2G, CLOUDS, WX, NWS, PSKR.
    pub status: Vec<(&'static str, Status)>,
}

impl Fetched {
    fn mark(&mut self, name: &'static str, r: Result<(), String>) {
        let now = decoders::slot::utc_now();
        let st = match self.status.iter_mut().find(|(n, _)| *n == name) {
            Some((_, s)) => s,
            None => {
                self.status.push((name, Status::default()));
                &mut self.status.last_mut().expect("just pushed").1
            }
        };
        match r {
            Ok(()) => {
                st.ok_at = Some(now);
                st.error = None;
            }
            Err(e) => st.error = Some(e),
        }
        self.serial += 1;
    }
}

/// What the fetcher needs to know about the station.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ask {
    pub qth: Option<LatLon>,
    pub call: Option<String>,
    pub pskr: bool,
    pub clouds: bool,
}

/// The fetcher's thread, while the window is open.
pub(crate) struct Fetcher {
    stop: Arc<AtomicBool>,
    pub data: Arc<Mutex<Fetched>>,
    ask: Arc<Mutex<Ask>>,
}

impl Drop for Fetcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

const SWPC: &str = "https://services.swpc.noaa.gov";

impl Fetcher {
    pub(crate) fn start(ask: Ask) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let data = Arc::new(Mutex::new(Fetched::default()));
        let ask = Arc::new(Mutex::new(ask));
        let (s, d, a) = (stop.clone(), data.clone(), ask.clone());
        std::thread::Builder::new()
            .name("globe-fetch".into())
            .spawn(move || run(s, d, a))
            .expect("the globe's fetch thread starts");
        tracing::info!("globe: fetching while the window is open");
        Fetcher { stop, data, ask }
    }

    pub(crate) fn set_ask(&self, ask: Ask) {
        if let Ok(mut a) = self.ask.lock()
            && *a != ask
        {
            *a = ask;
        }
    }
}

fn agent(call: Option<&str>) -> ureq::Agent {
    let ua = match call {
        Some(c) => format!("hardrock-panel/{} ({c})", env!("CARGO_PKG_VERSION")),
        None => format!("hardrock-panel/{}", env!("CARGO_PKG_VERSION")),
    };
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .user_agent(ua)
        .build()
        .into()
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())
}

fn get_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// When each source is next due.
struct Due {
    name: &'static str,
    every: Duration,
    next: Instant,
}

fn run(stop: Arc<AtomicBool>, data: Arc<Mutex<Fetched>>, ask: Arc<Mutex<Ask>>) {
    let now = Instant::now();
    let mut due: Vec<Due> = [
        ("SWPC", 5),
        ("OVATION", 15),
        ("KC2G", 15),
        ("CLOUDS", 30),
        ("WX", 15),
        ("NWS", 15),
        ("PSKR", 5),
    ]
    .into_iter()
    .map(|(name, min)| Due { name, every: Duration::from_secs(min * 60), next: now })
    .collect();
    let mut slow_swpc: Option<Instant> = None;
    let mut last_ask: Option<Ask> = None;
    while !stop.load(Ordering::Relaxed) {
        let a = ask.lock().map(|a| a.clone()).unwrap_or(Ask {
            qth: None,
            call: None,
            pskr: false,
            clouds: false,
        });
        // A new grid or callsign: the station's own sources again at once.
        if last_ask.as_ref().is_some_and(|l| {
            l.qth != a.qth || l.call != a.call || (!l.pskr && a.pskr) || (!l.clouds && a.clouds)
        }) {
            for d in &mut due {
                if matches!(d.name, "WX" | "NWS" | "PSKR" | "CLOUDS") {
                    d.next = Instant::now();
                }
            }
        }
        last_ask = Some(a.clone());
        let ag = agent(a.call.as_deref());
        for d in &mut due {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if Instant::now() < d.next {
                continue;
            }
            d.next = Instant::now() + d.every;
            let r = match d.name {
                "SWPC" => {
                    let slow = slow_swpc.is_none_or(|t| t.elapsed() > Duration::from_secs(3600));
                    if slow {
                        slow_swpc = Some(Instant::now());
                    }
                    fetch_swpc(&ag, &data, slow)
                }
                "OVATION" => get_text(&ag, &format!("{SWPC}/json/ovation_aurora_latest.json"))
                    .and_then(|t| parse_ovation(&t))
                    .map(|g| with(&data, |f| f.aurora = Some(g))),
                "KC2G" => get_text(&ag, "https://prop.kc2g.com/api/stations.json")
                    .and_then(|t| parse_kc2g(&t))
                    .map(|s| with(&data, |f| f.sondes = s)),
                "CLOUDS" if a.clouds => get_bytes(&ag, CLOUDS_URL)
                    .and_then(|b| parse_clouds(&b))
                    .map(|c| with(&data, |f| f.clouds = Some(c))),
                "WX" => match a.qth {
                    Some(q) => get_text(&ag, &open_meteo_url(q))
                        .and_then(|t| parse_open_meteo(&t))
                        .map(|w| with(&data, |f| f.wx = Some(w))),
                    None => Err("no grid in SETTINGS → Station".into()),
                },
                "NWS" => match a.qth {
                    Some(q) => get_text(
                        &ag,
                        &format!(
                            "https://api.weather.gov/alerts/active?point={:.4},{:.4}",
                            q.lat, q.lon
                        ),
                    )
                    .and_then(|t| parse_nws_alerts(&t))
                    .map(|al| with(&data, |f| f.alerts = al)),
                    None => Err("no grid in SETTINGS → Station".into()),
                },
                "PSKR" if a.pskr => match &a.call {
                    Some(c) => get_text(&ag, &pskr_url(c))
                        .map(|t| parse_pskreporter(&t))
                        .map(|r| with(&data, |f| f.heard_me = r)),
                    None => Err("no callsign in SETTINGS → Station".into()),
                },
                // Turned off: asked again as soon as it is turned on.
                _ => {
                    d.next = Instant::now();
                    continue;
                }
            };
            if let Err(e) = &r {
                tracing::info!("globe: {} fetch failed: {e}", d.name);
            }
            if let Ok(mut f) = data.lock() {
                f.mark(d.name, r);
            }
        }
        for _ in 0..20 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn with(data: &Mutex<Fetched>, f: impl FnOnce(&mut Fetched)) {
    if let Ok(mut d) = data.lock() {
        f(&mut d);
    }
}

/// The SWPC products: every five minutes the X-ray and the solar wind, every
/// hour (`slow`) Kp, its forecast and the flux.
fn fetch_swpc(ag: &ureq::Agent, data: &Mutex<Fetched>, slow: bool) -> Result<(), String> {
    let mut space = data.lock().map(|d| d.space.clone()).unwrap_or_default();
    let mut errs = Vec::new();
    let mut take = |name: &str, r: Result<(), String>| {
        if let Err(e) = r {
            errs.push(format!("{name}: {e}"));
        }
    };
    take(
        "X-ray",
        get_text(ag, &format!("{SWPC}/json/goes/primary/xrays-6-hour.json")).and_then(|t| {
            space.xray = Some(parse_xray(&t)?);
            Ok(())
        }),
    );
    take(
        "plasma",
        get_text(ag, &format!("{SWPC}/products/solar-wind/plasma-5-minute.json")).and_then(|t| {
            space.wind = Some(latest_column(&t, "speed")?);
            Ok(())
        }),
    );
    take(
        "mag",
        get_text(ag, &format!("{SWPC}/products/solar-wind/mag-5-minute.json")).and_then(|t| {
            space.bz = Some(latest_column(&t, "bz_gsm")?);
            Ok(())
        }),
    );
    if slow || space.kp.is_none() {
        take(
            "Kp",
            get_text(ag, &format!("{SWPC}/products/noaa-planetary-k-index.json")).and_then(|t| {
                let (kp, a, bins) = parse_kp(&t)?;
                space.kp = Some(kp);
                space.a = a;
                let mut all = bins;
                if let Ok(t) =
                    get_text(ag, &format!("{SWPC}/products/noaa-planetary-k-index-forecast.json"))
                    && let Ok(fc) = parse_kp_forecast(&t)
                {
                    let last = all.last().map(|b| b.0).unwrap_or(0.0);
                    all.extend(fc.into_iter().filter(|b| b.2 && b.0 > last));
                }
                space.kp_bins = all;
                Ok(())
            }),
        );
        take(
            "F10.7",
            get_text(ag, &format!("{SWPC}/json/f107_cm_flux.json")).and_then(|t| {
                space.sfi = Some(parse_f107(&t)?);
                Ok(())
            }),
        );
    }
    with(data, |f| f.space = space);
    if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
}

/// SWPC's times: "2026-10-22 15:00:00", "2026-10-22T15:00:00Z",
/// "2026-10-22T15:00:00.000" — Unix seconds, UTC.
pub(crate) fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00"));
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, dd) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next().unwrap_or("0").parse().ok()?;
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60) as f64 + sec)
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A product as rows of named fields, whichever way SWPC writes it: a table
/// whose first row names the columns, or a list of objects.
fn rows(t: &str) -> Result<Vec<serde_json::Map<String, Value>>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
    let list = v.as_array().ok_or("not a list")?;
    if let Some(Value::Array(head)) = list.first() {
        let names: Vec<String> =
            head.iter().map(|h| h.as_str().unwrap_or("").to_string()).collect();
        Ok(list[1..]
            .iter()
            .filter_map(|r| r.as_array())
            .map(|r| names.iter().cloned().zip(r.iter().cloned()).collect())
            .collect())
    } else {
        Ok(list.iter().filter_map(|r| r.as_object().cloned()).collect())
    }
}

fn field<'a>(r: &'a serde_json::Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|n| r.get(*n))
}

/// The planetary K index: the latest Kp, A (running), and every bin.
pub(crate) fn parse_kp(t: &str) -> Result<(f64, Option<f64>, Vec<(f64, f64, bool)>), String> {
    let mut bins = Vec::new();
    let mut a = None;
    for r in rows(t)? {
        let (Some(at), Some(kp)) = (
            field(&r, &["time_tag"]).and_then(|v| v.as_str()).and_then(parse_time),
            field(&r, &["Kp", "kp", "kp_index"]).and_then(num),
        ) else {
            continue;
        };
        a = field(&r, &["a_running", "A"]).and_then(num).or(a);
        bins.push((at, kp, false));
    }
    let last = bins.last().ok_or("no Kp in the answer")?.1;
    Ok((last, a, bins))
}

/// The Kp forecast's bins: (Unix seconds, Kp, forecast), the forecast ones
/// marked as such ("predicted"; "observed" and "estimated" are not).
pub(crate) fn parse_kp_forecast(t: &str) -> Result<Vec<(f64, f64, bool)>, String> {
    Ok(rows(t)?
        .iter()
        .filter_map(|r| {
            let at = field(r, &["time_tag"]).and_then(|v| v.as_str()).and_then(parse_time)?;
            let kp = field(r, &["kp", "Kp"]).and_then(num)?;
            let fc = field(r, &["observed"]).and_then(|v| v.as_str()) == Some("predicted");
            Some((at, kp, fc))
        })
        .collect())
}

/// The latest 10.7 cm flux, sfu.
pub(crate) fn parse_f107(t: &str) -> Result<f64, String> {
    rows(t)?
        .iter()
        .filter_map(|r| {
            let at = field(r, &["time_tag"]).and_then(|v| v.as_str()).and_then(parse_time)?;
            Some((at, field(r, &["flux"]).and_then(num)?))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
        .ok_or_else(|| "no flux in the answer".into())
}

/// The latest GOES long-channel (0.1–0.8 nm) X-ray flux, W/m².
pub(crate) fn parse_xray(t: &str) -> Result<f64, String> {
    rows(t)?
        .iter()
        .filter(|r| field(r, &["energy"]).and_then(|v| v.as_str()) == Some("0.1-0.8nm"))
        .filter_map(|r| {
            let at = field(r, &["time_tag"]).and_then(|v| v.as_str()).and_then(parse_time)?;
            Some((at, field(r, &["flux"]).and_then(num)?))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
        .ok_or_else(|| "no X-ray flux in the answer".into())
}

/// The newest row's `col` that has a figure (the solar wind's speed, Bz).
pub(crate) fn latest_column(t: &str, col: &str) -> Result<f64, String> {
    rows(t)?
        .iter()
        .filter_map(|r| {
            let at = field(r, &["time_tag"]).and_then(|v| v.as_str()).and_then(parse_time)?;
            Some((at, field(r, &[col]).and_then(num)?))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
        .ok_or_else(|| format!("no {col} in the answer"))
}

/// An X-ray flux as its class: A, B, C, M or X and the figure, "C1.4".
pub(crate) fn xray_class(flux: f64) -> String {
    let (c, base) = match flux {
        f if f >= 1e-4 => ('X', 1e-4),
        f if f >= 1e-5 => ('M', 1e-5),
        f if f >= 1e-6 => ('C', 1e-6),
        f if f >= 1e-7 => ('B', 1e-7),
        _ => ('A', 1e-8),
    };
    format!("{c}{:.1}", flux / base)
}

/// OVATION: {"coordinates": [[lon, lat, chance], …]}, lon 0 to 359, lat −90
/// to 90.
pub(crate) fn parse_ovation(t: &str) -> Result<Aurora, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
    let list = v.get("coordinates").and_then(|c| c.as_array()).ok_or("no coordinates")?;
    let mut g = vec![0u8; 360 * 181];
    for p in list {
        let Some(p) = p.as_array() else { continue };
        let (Some(lon), Some(lat), Some(a)) =
            (p.first().and_then(num), p.get(1).and_then(num), p.get(2).and_then(num))
        else {
            continue;
        };
        let lon = (lon.rem_euclid(360.0).round() as usize) % 360;
        let lat = (lat + 90.0).round().clamp(0.0, 180.0) as usize;
        g[lat * 360 + lon] = a.clamp(0.0, 100.0) as u8;
    }
    Ok(Aurora(g))
}

/// KC2G's stations: each sounder's place and its MUF(3000), the ones that
/// have one. Longitudes may come 0 to 360, figures as strings.
pub(crate) fn parse_kc2g(t: &str) -> Result<Vec<Ionosonde>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
    let list = v.as_array().ok_or("not a list")?;
    Ok(list
        .iter()
        .filter_map(|s| {
            let st = s.get("station")?;
            let lat = st.get("latitude").and_then(num)?;
            let lon = st.get("longitude").and_then(num)?;
            let mufd = s.get("mufd").and_then(num)?;
            Some(Ionosonde {
                name: st.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                pos: LatLon { lat, lon: (lon + 540.0).rem_euclid(360.0) - 180.0 },
                mufd,
                utc: s.get("time").and_then(|x| x.as_str()).and_then(parse_time).unwrap_or(0.0),
            })
        })
        .collect())
}

fn open_meteo_url(q: LatLon) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.3}&longitude={:.3}\
         &current=temperature_2m,relative_humidity_2m,weather_code,wind_speed_10m,wind_direction_10m,wind_gusts_10m,pressure_msl\
         &hourly=pressure_msl&past_hours=3&forecast_hours=1&timezone=UTC&wind_speed_unit=kmh",
        q.lat, q.lon
    )
}

/// Open-Meteo's current weather, with the pressure three hours ago for its
/// trend.
pub(crate) fn parse_open_meteo(t: &str) -> Result<LocalWx, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
    let c = v.get("current").ok_or("no current weather")?;
    let g = |k: &str| c.get(k).and_then(num);
    let p = g("pressure_msl");
    let first = v
        .get("hourly")
        .and_then(|h| h.get("pressure_msl"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find_map(num));
    Ok(LocalWx {
        temp_c: g("temperature_2m"),
        humidity: g("relative_humidity_2m"),
        code: c.get("weather_code").and_then(|x| x.as_i64()),
        wind_kmh: g("wind_speed_10m"),
        wind_from_deg: g("wind_direction_10m"),
        gust_kmh: g("wind_gusts_10m"),
        pressure_hpa: p,
        trend_hpa: p.zip(first).map(|(now, then)| now - then),
    })
}

/// A WMO weather code in a few words, and whether it is a thunderstorm.
pub(crate) fn wmo_words(code: i64) -> (&'static str, bool) {
    match code {
        0 => ("clear", false),
        1 => ("mostly clear", false),
        2 => ("partly cloudy", false),
        3 => ("overcast", false),
        45 | 48 => ("fog", false),
        51..=57 => ("drizzle", false),
        61..=67 => ("rain", false),
        71..=77 => ("snow", false),
        80..=82 => ("showers", false),
        85 | 86 => ("snow showers", false),
        95 => ("thunderstorm", true),
        96..=99 => ("thunderstorm, hail", true),
        _ => ("—", false),
    }
}

/// The NWS's active alerts at a point: each one's event name ("Severe
/// Thunderstorm Warning").
pub(crate) fn parse_nws_alerts(t: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
    let list = v.get("features").and_then(|f| f.as_array()).ok_or("no features")?;
    let mut out: Vec<String> = list
        .iter()
        .filter_map(|f| f.get("properties")?.get("event")?.as_str().map(str::to_string))
        .collect();
    out.dedup();
    Ok(out)
}

fn pskr_url(call: &str) -> String {
    format!(
        "https://retrieve.pskreporter.info/query?senderCallsign={}&flowStartSeconds=-3600&rronly=1&noactive=1",
        call.replace('/', "%2F")
    )
}

/// The value of `name="…"` in an XML tag's text.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')? + i;
    Some(&tag[i..j])
}

/// PSK Reporter's answer: each `<receptionReport …/>` with a locator, as a
/// reception from the station that heard AJ3G.
pub(crate) fn parse_pskreporter(t: &str) -> Vec<Reception> {
    t.split("<receptionReport")
        .skip(1)
        .filter_map(|rest| {
            let tag = &rest[..rest.find('>')?];
            let tag = format!(" {tag}");
            let call = attr(&tag, "receiverCallsign")?.to_string();
            let grid = attr(&tag, "receiverLocator")?;
            let g4: String = grid.chars().take(if grid.len() >= 6 { 6 } else { 4 }).collect();
            let pos = grid_to_latlon(&g4)
                .or_else(|| grid_to_latlon(&grid.chars().take(4).collect::<String>()))?;
            Some(Reception {
                call,
                grid: g4,
                pos,
                rf_hz: attr(&tag, "frequency")?.parse().ok()?,
                mode: attr(&tag, "mode").unwrap_or("").to_string(),
                snr: attr(&tag, "sNR").and_then(|s| s.parse().ok()),
                utc: attr(&tag, "flowStartSeconds")?.parse().ok()?,
                src: Source::PskReporter,
                row: None,
            })
        })
        .collect()
}

/// nowCOAST's GMGSI longwave infrared mosaic, the whole Earth, 1024 × 512.
const CLOUDS_URL: &str = "https://nowcoast.noaa.gov/geoserver/satellite/wms?service=WMS&version=1.3.0&request=GetMap\
     &layers=global_longwave_imagery_mosaic&styles=&format=image/png&transparent=false\
     &crs=EPSG:4326&bbox=-90,-180,90,180&width=1024&height=512";

/// A PNG as brightness, 0 to 255.
pub(crate) fn parse_clouds(b: &[u8]) -> Result<Clouds, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(b));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("too big")?];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let n = info.color_type.samples();
    let px = (0..w * h)
        .map(|i| {
            let p = &buf[i * n..i * n + n];
            match n {
                1 | 2 => p[0],
                _ => ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8,
            }
        })
        .collect();
    Ok(Clouds { w, h, px })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_whichever_way_swpc_writes_them() {
        let t = parse_time("2026-10-22 15:00:00").unwrap();
        assert_eq!(t, 1_792_681_200.0);
        assert_eq!(parse_time("2026-10-22T15:00:00Z"), Some(t));
        assert_eq!(parse_time("2026-10-22T15:00:00.000"), Some(t));
        assert_eq!(parse_time("1970-01-01"), Some(0.0));
    }

    #[test]
    fn kp_is_read_as_a_table_or_as_objects() {
        let table = r#"[["time_tag","Kp","a_running","station_count"],
            ["2026-10-22 09:00:00.000","2.33","9","8"],["2026-10-22 12:00:00.000","3.00","15","8"]]"#;
        let (kp, a, bins) = parse_kp(table).unwrap();
        assert_eq!((kp, a, bins.len()), (3.0, Some(15.0), 2));
        let objects =
            r#"[{"time_tag":"2026-10-22T12:00:00","Kp":2.67,"a_running":12,"station_count":8}]"#;
        assert_eq!(parse_kp(objects).unwrap().0, 2.67);
        assert!(parse_kp("[]").is_err());
        let fc = r#"[["time_tag","kp","observed","noaa_scale"],["2026-10-22 12:00:00","3.00","observed",null],
            ["2026-10-22 15:00:00","3.33","estimated",null],["2026-10-22 18:00:00","3.67","predicted",null]]"#;
        let f = parse_kp_forecast(fc).unwrap();
        assert_eq!(f.iter().filter(|b| b.2).count(), 1);
    }

    #[test]
    fn flux_xray_and_solar_wind_take_the_newest_figure() {
        let f = r#"[{"time_tag":"2026-10-21T20:00:00","frequency":2800,"flux":160},
            {"time_tag":"2026-10-22T20:00:00","frequency":2800,"flux":168}]"#;
        assert_eq!(parse_f107(f).unwrap(), 168.0);
        let x = r#"[{"time_tag":"2026-10-22T15:00:00Z","satellite":18,"flux":1.4e-6,"energy":"0.1-0.8nm"},
            {"time_tag":"2026-10-22T15:01:00Z","satellite":18,"flux":9.0e-8,"energy":"0.05-0.4nm"}]"#;
        assert_eq!(parse_xray(x).unwrap(), 1.4e-6);
        assert_eq!(xray_class(1.4e-6), "C1.4");
        assert_eq!(xray_class(2.3e-5), "M2.3");
        let plasma = r#"[["time_tag","density","speed","temperature"],
            ["2026-10-22 15:00:00.000","4.1","405.2","60000"],["2026-10-22 15:05:00.000","4.0","412.0","61000"],
            ["2026-10-22 15:10:00.000",null,null,null]]"#;
        assert_eq!(latest_column(plasma, "speed").unwrap(), 412.0);
    }

    #[test]
    fn the_aurora_grid_and_the_ionosondes_are_read() {
        let o = r#"{"Observation Time":"2026-10-22T15:00:00Z","coordinates":[[0,-90,0],[288,65,42],[287,65,30]]}"#;
        let g = parse_ovation(o).unwrap();
        assert_eq!(g.at(LatLon { lat: 65.0, lon: -72.0 }), 42.0);
        let k = r#"[{"station":{"name":"Wallops Is","code":"WP937","latitude":"37.9","longitude":"284.5"},
            "time":"2026-10-22T15:00:00","mufd":24.6,"fof2":7.1},
            {"station":{"name":"Nowhere","latitude":"1","longitude":"2"},"time":"2026-10-22T15:00:00","mufd":null}]"#;
        let s = parse_kc2g(k).unwrap();
        assert_eq!(s.len(), 1);
        assert!((s[0].pos.lon + 75.5).abs() < 1e-9 && s[0].mufd == 24.6);
    }

    #[test]
    fn the_weather_and_its_alerts_are_read() {
        let w = r#"{"current":{"time":"2026-10-22T15:45","temperature_2m":14.2,"relative_humidity_2m":71,
            "weather_code":3,"wind_speed_10m":19.0,"wind_direction_10m":310,"wind_gusts_10m":31.0,"pressure_msl":1018.2},
            "hourly":{"time":["2026-10-22T12:00","2026-10-22T13:00"],"pressure_msl":[1019.6,1019.0]}}"#;
        let x = parse_open_meteo(w).unwrap();
        assert_eq!(x.temp_c, Some(14.2));
        assert_eq!(x.code, Some(3));
        assert!((x.trend_hpa.unwrap() + 1.4).abs() < 1e-9);
        assert_eq!(wmo_words(95), ("thunderstorm", true));
        let a = r#"{"features":[{"properties":{"event":"Wind Advisory","severity":"Moderate"}}]}"#;
        assert_eq!(parse_nws_alerts(a).unwrap(), vec!["Wind Advisory".to_string()]);
        assert!(parse_nws_alerts(r#"{"features":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn psk_reporter_reports_become_stations_that_heard_the_station() {
        let x = r#"<?xml version="1.0"?><receptionReports>
            <receptionReport receiverCallsign="EA5XYZ" receiverLocator="IM98wk" senderCallsign="AJ3G" senderLocator="FN20"
              frequency="14075123" flowStartSeconds="1792681200" mode="FT8" isSender="1" sNR="-13" />
            <receptionReport receiverCallsign="NOGRID" senderCallsign="AJ3G" frequency="14075123" flowStartSeconds="1792681200" mode="FT8" />
            </receptionReports>"#;
        let r = parse_pskreporter(x);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].call.as_str(), r[0].grid.as_str(), r[0].snr),
            ("EA5XYZ", "IM98wk", Some(-13.0))
        );
        assert_eq!(r[0].src, Source::PskReporter);
        assert_eq!(r[0].band(), Some("20"));
    }

    #[test]
    fn a_cloud_picture_is_read_as_brightness() {
        let mut png = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png, 2, 1);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[0, 0, 0, 255, 255, 255]).unwrap();
        }
        let c = parse_clouds(&png).unwrap();
        assert_eq!((c.w, c.h), (2, 1));
        assert_eq!(c.px, vec![0, 255]);
    }
}
