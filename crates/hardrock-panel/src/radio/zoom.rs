//! The radio window's scale: everything in it is drawn for a scale of 1 and
//! grows with its window (AJ3G, 21 Oct 2026: "Radio Panel ... do[es] not
//! scale when window is expanded").
//!
//! The scale is applied to the sizes themselves, never as a zoom of what was
//! drawn, so the text is laid out at the size it is shown and stays sharp:
//! [`scaled`] sets it for the window's contents and scales the `Ui`'s style
//! (text styles and spacing) for the widgets that take their sizes from it,
//! and [`z`] scales a size the code gives itself. Outside [`scaled`] the
//! scale is 1, so the TUNE strip drawn in the main window is unchanged.

use std::cell::Cell;

use eframe::egui;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
}

/// The scale in force.
pub(crate) fn k() -> f32 {
    SCALE.with(Cell::get)
}

/// `v`, a size at scale 1, at the scale in force.
pub(crate) fn z(v: f32) -> f32 {
    v * k()
}

/// A margin at the scale in force, for [`egui::Margin`]'s whole points.
pub(crate) fn zi(v: f32) -> i8 {
    z(v).round().clamp(0.0, 127.0) as i8
}

/// Draw `add` at scale `s`: sizes given with [`z`] and the `Ui`'s style
/// both scaled. The scale before is put back afterwards.
pub(crate) fn scaled<R>(ui: &mut egui::Ui, s: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let before = SCALE.with(|c| c.replace(s));
    let r = ui
        .scope(|ui| {
            scale_style(ui.style_mut(), s);
            add(ui)
        })
        .inner;
    SCALE.with(|c| c.set(before));
    r
}

/// Run `f` at scale 1, for a window of its own opened from inside
/// [`scaled`].
pub(crate) fn unscaled<R>(f: impl FnOnce() -> R) -> R {
    let before = SCALE.with(|c| c.replace(1.0));
    let r = f();
    SCALE.with(|c| c.set(before));
    r
}

/// The style's text sizes and spacing, `s` times.
fn scale_style(style: &mut egui::Style, s: f32) {
    for font in style.text_styles.values_mut() {
        font.size *= s;
    }
    let sp = &mut style.spacing;
    sp.item_spacing *= s;
    sp.button_padding *= s;
    sp.interact_size *= s;
    sp.indent *= s;
    sp.slider_width *= s;
    sp.slider_rail_height *= s;
    sp.combo_width *= s;
    sp.text_edit_width *= s;
    sp.icon_width *= s;
    sp.icon_width_inner *= s;
    sp.icon_spacing *= s;
    sp.combo_height *= s;
    sp.menu_margin *= s;
    sp.window_margin *= s;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_holds_only_inside_and_scales_the_style() {
        let ctx = egui::Context::default();
        let mut full = ctx.run_ui(Default::default(), |ui| {
            let body = egui::TextStyle::Body.resolve(ui.style()).size;
            assert_eq!(z(10.0), 10.0);
            scaled(ui, 2.0, |ui| {
                assert_eq!(z(10.0), 20.0);
                assert_eq!(zi(7.0), 14);
                assert_eq!(egui::TextStyle::Body.resolve(ui.style()).size, body * 2.0);
                // Nested, as a pop-out inside the window would be.
                scaled(ui, 1.0, |_| assert_eq!(z(10.0), 10.0));
                unscaled(|| assert_eq!(z(10.0), 10.0));
                assert_eq!(z(10.0), 20.0);
            });
            assert_eq!(z(10.0), 10.0);
            assert_eq!(egui::TextStyle::Body.resolve(ui.style()).size, body);
        });
        full.textures_delta.clear();
    }
}
