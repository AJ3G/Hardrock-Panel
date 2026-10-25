//! Popped-out windows open where they were last put, at the size they were
//! left (AJ3G, 25 Oct 2026). Each window's place is kept in egui's persisted
//! memory, which eframe saves with the rest of the panel's settings; a window
//! never popped out opens at its default size wherever the system puts it.
//! Docked windows already keep their place in that same memory.

use eframe::egui;
use serde::{Deserialize, Serialize};

/// Where a popped-out window was, in points: its outer corner and its inner
/// size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn saved_id(key: &str) -> egui::Id {
    egui::Id::new(("popout-place", key))
}

fn opened_id(key: &str) -> egui::Id {
    egui::Id::new(("popout-opened", key))
}

/// The builder for `key`'s window, at its remembered place and size or else
/// `size`. The place is read once when the window opens and held while it
/// stays open: egui moves a window whose builder changes, so a builder that
/// followed every drag would fight it.
pub(crate) fn builder(
    ctx: &egui::Context,
    key: &str,
    title: &str,
    size: [f32; 2],
    min: [f32; 2],
) -> egui::ViewportBuilder {
    let frame = ctx.cumulative_frame_nr();
    let place = ctx.data_mut(|d| {
        let held = d.get_temp::<(Option<Place>, u64)>(opened_id(key));
        // Shown last frame: still open, keep what it opened with.
        let place = match held {
            Some((p, f)) if frame <= f + 1 => p,
            _ => d.get_persisted::<Place>(saved_id(key)),
        };
        d.insert_temp(opened_id(key), (place, frame));
        place
    });
    let b = egui::ViewportBuilder::default().with_title(title).with_min_inner_size(min);
    match place.filter(|p| plausible(p, min)) {
        Some(p) => b.with_position([p.x, p.y]).with_inner_size([p.w, p.h]),
        None => b.with_inner_size(size),
    }
}

/// Inside `key`'s viewport: note where the window is now, to open there next
/// time. Not while minimised, when the system reports a place off screen.
pub(crate) fn track(ctx: &egui::Context, key: &str) {
    let (outer, inner, minimized) = ctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.inner_rect, v.minimized)
    });
    if minimized == Some(true) {
        return;
    }
    let (Some(o), Some(r)) = (outer, inner) else { return };
    let now = Place { x: o.min.x, y: o.min.y, w: r.width(), h: r.height() };
    ctx.data_mut(|d| {
        if d.get_persisted::<Place>(saved_id(key)) != Some(now) {
            d.insert_persisted(saved_id(key), now);
        }
    });
}

/// A place worth opening at: a real size, and a corner not flung far off any
/// screen (Windows parks minimised windows at -32000).
fn plausible(p: &Place, min: [f32; 2]) -> bool {
    p.w >= min[0] * 0.5
        && p.h >= min[1] * 0.5
        && p.x.is_finite()
        && p.y.is_finite()
        && p.x > -10_000.0
        && p.y > -10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimised_or_tiny_place_is_not_reopened() {
        let min = [480.0, 420.0];
        assert!(plausible(&Place { x: 100.0, y: 50.0, w: 600.0, h: 820.0 }, min));
        assert!(plausible(&Place { x: -1900.0, y: 0.0, w: 600.0, h: 820.0 }, min));
        assert!(!plausible(&Place { x: -32000.0, y: -32000.0, w: 600.0, h: 820.0 }, min));
        assert!(!plausible(&Place { x: 100.0, y: 50.0, w: 10.0, h: 10.0 }, min));
    }

    #[test]
    fn the_place_is_held_while_the_window_stays_open() {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_persisted(saved_id("t"), Place { x: 10.0, y: 20.0, w: 600.0, h: 500.0 })
        });
        let _ = builder(&ctx, "t", "T", [700.0, 700.0], [400.0, 400.0]);
        // Dragged: the new place is saved for next time, but the builder
        // keeps the one the window opened with.
        ctx.data_mut(|d| {
            d.insert_persisted(saved_id("t"), Place { x: 300.0, y: 20.0, w: 600.0, h: 500.0 })
        });
        let held = ctx.data_mut(|d| d.get_temp::<(Option<Place>, u64)>(opened_id("t")));
        assert_eq!(held.and_then(|h| h.0).map(|p| p.x), Some(10.0));
    }
}
