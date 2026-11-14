//! BANDS OPEN and the BANDS window (decision 127, AJ3G 14 Nov 2026, from
//! SDRoxide's globe overlay and its BANDS window).
//!
//! BANDS OPEN is a small box over the map: one bar a band heard, how much
//! of the world its paths reach, read from the same field as REACH in BAND
//! CONDITIONS, so it fades as the paths age. The BANDS window puts the
//! whole picture in one place: HamQSL's word beside what was heard, the IBP
//! beacon on each band now, and the meteor showers active today.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke, Vec2};

use super::{
    Globe, GlobeSettings, INK_DIM, band_conditions, caps, compass, ibp_beacons, meteor, rgb, sun,
};
use crate::station::{StationSettings, distance_km};
use crate::theme;

/// The scale BANDS OPEN's bars run to: decades of 1, 3, 10, 30 and 100 %
/// of the world, the first that holds the best band. Not the best band
/// itself, or a full bar would mean a quarter of the planet one minute and
/// one neighbour the next.
pub(super) fn scale(best: f64) -> f64 {
    [0.01, 0.03, 0.1, 0.3, 1.0].into_iter().find(|s| best <= *s).unwrap_or(1.0)
}

/// A share of the world as a per cent to two significant figures: a band
/// down to its last cell is a hundredth of a per cent, and "0.0 %" would
/// read as shut.
pub(super) fn pct(v: f64) -> String {
    let v = v * 100.0;
    if v >= 10.0 {
        format!("{v:.0} %")
    } else if v >= 1.0 {
        format!("{v:.1} %")
    } else {
        format!("{v:.2} %")
    }
}

/// The bands with reach, 160 m at the top: (band, reach).
pub(super) fn open_rows(g: &Globe) -> Vec<(&'static str, f64)> {
    let mut rows: Vec<(&'static str, f64)> =
        g.fields.iter().filter(|f| f.reach > 0.0).map(|f| (f.band, f.reach)).collect();
    // By wavelength, longest first; a band with no number in metres last.
    rows.sort_by(|a, b| {
        let m = |s: &str| s.parse::<f64>().unwrap_or(0.0);
        m(b.0).total_cmp(&m(a.0))
    });
    rows
}

/// Where the field's paths come from, as the sources on say: there is no
/// RBN here, so it is the station's own receptions and the reports fetched.
fn sources(s: &GlobeSettings) -> String {
    let mut v = Vec::new();
    if s.decode || s.skim {
        v.push("own receptions");
    }
    if s.pskr || s.pskr_band {
        v.push("PSK Reporter");
    }
    if s.wspr_heard {
        v.push("WSPR");
    }
    match v.len() {
        0 => "no sources on".into(),
        1 => format!("from {}", v[0]),
        n => format!("from {} and {}", v[..n - 1].join(", "), v[n - 1]),
    }
}

/// BANDS OPEN, in the map's bottom right corner (the VIEW keys are top
/// left, PROP's legend bottom left, the HEARD ME pop-up top right). Shown
/// while any band has reach and BANDS OPEN is on.
pub(super) fn open_box(painter: &egui::Painter, rect: Rect, g: &Globe, s: &GlobeSettings) {
    if !s.bands_open {
        return;
    }
    let rows = open_rows(g);
    if rows.is_empty() {
        return;
    }
    let sc = scale(rows.iter().map(|r| r.1).fold(0.0, f64::max));
    let small = egui::FontId::monospace(9.5);
    let font = egui::FontId::monospace(10.5);
    let lay = |t: String, f: &egui::FontId, c: Color32| painter.layout_no_wrap(t, f.clone(), c);
    let laid: Vec<_> = rows
        .iter()
        .map(|(b, r)| {
            let hue = rgb(super::picture::band_colour(b));
            (lay(format!("{b}M"), &small, hue), lay(pct(*r), &font, theme::CYAN()), hue, *r)
        })
        .collect();
    let title = lay("BANDS OPEN".into(), &small, theme::CYAN_DIM());
    let notes = [
        format!("full bar = {} of the world", pct(sc)),
        format!("where the paths heard went · {:.0} min memory", s.half_life_min),
        sources(s),
    ]
    .map(|t| lay(t, &small, INK_DIM));
    let key_w = laid.iter().map(|l| l.0.size().x).fold(0.0, f32::max);
    let val_w = laid.iter().map(|l| l.1.size().x).fold(0.0, f32::max);
    const BAR_H: f32 = 7.0;
    const MIN_BAR: f32 = 74.0;
    let row_h = laid.iter().map(|l| l.1.size().y + 3.0).fold(BAR_H + 5.0, f32::max);
    let (pad, gap) = (9.0, 8.0);
    let w = (key_w + MIN_BAR + val_w + gap * 2.0)
        .max(title.size().x)
        .max(notes.iter().map(|n| n.size().x).fold(0.0, f32::max))
        + pad * 2.0;
    let h = title.size().y
        + 5.0
        + laid.len() as f32 * row_h
        + 3.0
        + notes.iter().map(|n| n.size().y + 2.0).sum::<f32>()
        + pad * 2.0;
    // PROP's legend is 230 wide in the other corner: on a narrow map, sit
    // above it rather than over it.
    let lift = if s.prop && rect.width() < 230.0 + w + 30.0 { 64.0 } else { 0.0 };
    let panel = Rect::from_min_size(
        rect.right_bottom() - Vec2::new(w + 10.0, h + 10.0 + lift),
        Vec2::new(w, h),
    );
    painter.rect_filled(panel, 4.0, Color32::from_rgba_unmultiplied(14, 16, 19, 225));
    painter.rect_stroke(
        panel,
        4.0,
        Stroke::new(1.0, Color32::from_rgb(0x23, 0x27, 0x2c)),
        egui::StrokeKind::Inside,
    );
    let mut y = panel.top() + pad;
    let th = title.size().y;
    painter.galley(Pos2::new(panel.left() + pad, y), title, theme::CYAN_DIM());
    y += th + 5.0;
    let bar_x = panel.left() + pad + key_w + gap;
    let bar_w = (panel.right() - pad - val_w - gap - bar_x).max(MIN_BAR);
    for (key, val, hue, reach) in laid {
        let (ks, vs) = (key.size(), val.size());
        painter.galley(Pos2::new(panel.left() + pad, y + (row_h - ks.y) / 2.0), key, hue);
        let top = y + (row_h - BAR_H) / 2.0;
        // A grey track behind each bar, so a quiet band reads as quiet.
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(bar_x, top), Vec2::new(bar_w, BAR_H)),
            1.0,
            Color32::from_rgb(0x2a, 0x30, 0x37),
        );
        let fill = ((reach / sc).clamp(0.0, 1.0) as f32 * bar_w).max(1.0);
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(bar_x, top), Vec2::new(fill, BAR_H)),
            1.0,
            hue,
        );
        painter.galley(
            Pos2::new(panel.right() - pad - vs.x, y + (row_h - vs.y) / 2.0),
            val,
            theme::CYAN(),
        );
        y += row_h;
    }
    y += 3.0;
    for n in notes {
        let nh = n.size().y;
        painter.galley(Pos2::new(panel.left() + pad, y), n, INK_DIM);
        y += nh + 2.0;
    }
}

/// HamQSL's own time, "23 Oct 2026 0100 GMT", as Unix seconds.
pub(super) fn hamqsl_time(t: &str) -> Option<f64> {
    let w: Vec<&str> = t.split_whitespace().collect();
    let [d, mon, y, hm, ..] = w[..] else { return None };
    const MON: [&str; 12] =
        ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let lower = mon.to_ascii_lowercase();
    let m = MON.iter().position(|x| lower.starts_with(x))? as u32 + 1;
    let (d, y): (u32, i64) = (d.parse().ok()?, y.parse().ok()?);
    let hm: u32 = hm.parse().ok()?;
    if !(1..=31).contains(&d) || hm / 100 > 23 || hm % 100 > 59 {
        return None;
    }
    let days = crate::logbook::adif::days_from_civil(y, m, d);
    Some((days * 86_400) as f64 + ((hm / 100) * 3600 + (hm % 100) * 60) as f64)
}

/// The BANDS window, drawn while GLOBE is (docked or popped out), opened by
/// GLOBE's BANDS key.
pub(super) fn window(
    ctx: &egui::Context,
    g: &Globe,
    s: &mut GlobeSettings,
    st: &StationSettings,
    now: f64,
) {
    if !s.bands_window {
        return;
    }
    let mut open = true;
    egui::Window::new("Bands")
        .id(egui::Id::new("globe-bands-window"))
        .open(&mut open)
        .frame(crate::chrome::window_frame())
        .resizable(true)
        .collapsible(false)
        .default_size([430.0, 600.0])
        .constrain_to(ctx.content_rect())
        .show(ctx, |ui| {
            crate::chrome::window_body_bg(ui);
            egui::ScrollArea::vertical().id_salt("globe-bands-scroll").show(ui, |ui| {
                body(ui, g, st, now);
            });
        });
    if !open {
        s.bands_window = false;
    }
}

fn body(ui: &mut egui::Ui, g: &Globe, st: &StationSettings, now: f64) {
    let qth = st.pos();
    ui.horizontal(|ui| {
        let day = qth.is_none_or(|q| sun::elevation(q, now) > 0.0);
        ui.label(
            RichText::new(if day { "DAY" } else { "NIGHT" })
                .monospace()
                .size(11.0)
                .strong()
                .color(theme::CYAN()),
        );
        let fetched = g.got.status.iter().find(|(n, _)| *n == "HAMQSL").and_then(|(_, s)| s.ok_at);
        let age = g.got.hamqsl.as_ref().and_then(|h| hamqsl_time(&h.updated)).or(fetched);
        let words = match (&g.got.hamqsl, age) {
            (Some(_), Some(at)) => {
                format!("· forecast from HamQSL.com, {} old", age_words(now - at))
            }
            (Some(_), None) => "· forecast from HamQSL.com".into(),
            (None, _) => "· waiting for HamQSL.com".into(),
        };
        ui.label(RichText::new(words).size(10.5).color(theme::TEXT()));
    });
    ui.add_space(4.0);
    let muf_now = qth.and_then(|q| {
        g.got
            .sondes
            .iter()
            .filter(|so| distance_km(q, so.pos) <= 3000.0)
            .min_by(|a, b| distance_km(q, a.pos).total_cmp(&distance_km(q, b.pos)))
            .map(|so| so.mufd)
    });
    band_conditions(ui, g, st, now, muf_now);
    ui.add_space(8.0);
    ibp_beacons(ui, st, now);
    ui.add_space(8.0);
    meteors(ui, st, now);
}

/// "12 min", "3 h", as an age.
fn age_words(s: f64) -> String {
    let m = (s / 60.0).max(0.0);
    if m < 1.0 {
        "under a minute".into()
    } else if m < 90.0 {
        format!("{m:.0} min")
    } else {
        format!("{:.0} h", m / 60.0)
    }
}

/// METEOR SHOWERS: the showers active today, strongest first, each radiant
/// placed for the station.
fn meteors(ui: &mut egui::Ui, st: &StationSettings, now: f64) {
    let Some(q) = st.pos() else {
        ui.label(caps("METEOR SHOWERS"));
        ui.label(
            RichText::new("Set your grid under SETTINGS → Station.")
                .size(10.5)
                .color(theme::TEXT()),
        );
        return;
    };
    ui.label(caps(&format!(
        "METEOR SHOWERS · RADIANTS PLACED FOR {:.0}°{}, {:.0}°{}",
        q.lat.abs(),
        if q.lat >= 0.0 { "N" } else { "S" },
        q.lon.abs(),
        if q.lon >= 0.0 { "E" } else { "W" }
    )));
    let active = meteor::active(q, now);
    if active.is_empty() {
        ui.label(RichText::new("No major shower today.").size(10.5).color(theme::TEXT()));
    }
    let mono = |t: String, c: Color32| RichText::new(t).monospace().size(10.5).color(c);
    egui::Grid::new("globe-meteors").num_columns(4).spacing([8.0, 1.0]).show(ui, |ui| {
        for a in &active {
            let s = a.shower;
            ui.label(mono(s.name.to_string(), theme::TEXT_STRONG())).on_hover_text(format!(
                "{} ({}): {} km/s, from {}. Peak {} {} at a ZHR of {}",
                s.name,
                s.code,
                s.kms,
                s.parent,
                s.peak.1,
                MONTHS[(s.peak.0 - 1) as usize],
                s.zhr
            ));
            ui.label(mono(s.code.to_string(), theme::TEXT()));
            let peak = match a.days_to_peak {
                0 => " · PEAK".to_string(),
                d if d > 0 => format!(" · peak in {d} d"),
                d => format!(" · peak {} d ago", -d),
            };
            ui.label(mono(
                format!("ZHR {}{peak}", s.zhr),
                if a.days_to_peak == 0 { theme::GREEN() } else { theme::TEXT_STRONG() },
            ));
            if a.alt > 0.0 {
                ui.label(mono(
                    format!("radiant {:.0}° up, {}", a.alt, compass(a.az)),
                    theme::CYAN(),
                ));
            } else {
                ui.label(mono(format!("radiant down ({:.0}°)", a.alt), theme::TEXT()));
            }
            ui.end_row();
        }
    });
    ui.label(
        RichText::new(
            "Dates, rates and radiants from the International Meteor Organization's list. A \
             meteor's trail opens a path for a second or two: meteor scatter on 6 m and 2 m, \
             pings on 10 m. Best with the radiant up and near the peak.",
        )
        .size(10.5)
        .color(theme::TEXT()),
    );
}

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_runs_in_decades_and_the_per_cent_keeps_two_figures() {
        assert_eq!(scale(0.0), 0.01);
        assert_eq!(scale(0.046), 0.1);
        assert_eq!(scale(0.12), 0.3);
        assert_eq!(scale(0.5), 1.0);
        assert_eq!(pct(0.046), "4.6 %");
        assert_eq!(pct(0.12), "12 %");
        assert_eq!(pct(0.0084), "0.84 %");
        assert_eq!(pct(0.3), "30 %");
    }

    #[test]
    fn hamqsls_time_reads_as_utc() {
        // 23 Oct 2026 01:00 UTC.
        assert_eq!(hamqsl_time("23 Oct 2026 0100 GMT"), Some(1_792_717_200.0));
        assert_eq!(hamqsl_time("1 jan 1970 0000 GMT"), Some(0.0));
        assert_eq!(hamqsl_time(""), None);
        assert_eq!(hamqsl_time("23 Foo 2026 0100 GMT"), None);
        assert_eq!(hamqsl_time("23 Oct 2026 2500 GMT"), None);
    }

    #[test]
    fn bands_open_lists_the_bands_with_reach_longest_first_and_fades_with_them() {
        let mut g = Globe::default();
        let f = |band: &'static str, reach: f64| super::super::field::BandField {
            band,
            cells: Vec::new(),
            reach,
            peak: 0.0,
            paths: 1.0,
            best_db: None,
        };
        g.fields = vec![f("20", 0.12), f("160", 0.0084), f("40", 0.0), f("10", 0.046)];
        assert_eq!(open_rows(&g), vec![("160", 0.0084), ("20", 0.12), ("10", 0.046)]);
        g.fields.iter_mut().for_each(|f| f.reach = 0.0);
        assert!(open_rows(&g).is_empty());
    }

    #[test]
    fn the_footer_names_the_sources_on() {
        let mut s = GlobeSettings::default();
        assert_eq!(sources(&s), "from own receptions and PSK Reporter");
        s.wspr_heard = true;
        assert_eq!(sources(&s), "from own receptions, PSK Reporter and WSPR");
        s.pskr_band = false;
        assert_eq!(sources(&s), "from own receptions and WSPR");
    }
}
