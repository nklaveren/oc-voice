//! The Settings view.
//!
//! **Not a window over the overlay — the overlay itself, showing something
//! else.** A layer surface is one rectangle with a fixed extent: anything
//! drawn outside it does not exist, and no z-order changes that, because it
//! is not an occlusion problem. A floating dialog inside a 900x350 strip is
//! either clipped at the edges or forces the strip to grow around it, and
//! both were tried before this.
//!
//! So Settings is a mode. The strip stops showing the transcript and shows
//! its own configuration, in exactly the space that exists, scrolling when
//! there is not enough. Nothing to grow, nothing to restore, nothing to clip.
//!
//! It exists at all because of M6.1: `windowrule` no longer matches a layer
//! surface, so opacity, size and position have nowhere else to live.

use super::*;
use crate::ui::host::LayoutRequest;

/// Floors for the sliders. The ceilings are not constants — they come from
/// the output, because a constant ceiling is how the overlay grew to 2400 px
/// on a 1533 px panel and carried its own controls off the screen.
const MIN_W: f32 = 320.0;
const MIN_H: f32 = 120.0;
/// Used only until the host reports the real output size, one configure away.
const FALLBACK_W: f32 = 1280.0;
const FALLBACK_H: f32 = 720.0;
/// Never let the surface fill its screen: an overlay that covers the desktop
/// is a modal dialog, and this one has no business being one.
const SCREEN_FRACTION: f32 = 0.9;
/// Pixels per frame while an arrow is held. At ~60 fps that is ~120 px a
/// second: responsive, and it stops where you meant it to.
const REPEAT: f32 = 2.0;

/// A button that reports while it is *held*, not when it is released.
///
/// `clicked()` fires once on release, so resizing by 2 px a press would take
/// sixty presses. Holding is the gesture a stepper already implies.
fn held(ui: &mut egui::Ui, label: &str, hint: &str) -> bool {
    let r = ui.button(label).on_hover_text(hint);
    if r.is_pointer_button_down_on() {
        // egui only repaints on input, and a held button sends none — without
        // this the repeat stops the moment the mouse stops moving.
        ui.ctx().request_repaint();
        return true;
    }
    false
}

impl OverlayApp {
    /// The biggest the overlay may get, and the biggest bottom margin that
    /// still leaves it on screen.
    pub(super) fn bounds(&self) -> (f32, f32, f32) {
        let (w, h) = self.max_size.unwrap_or((FALLBACK_W, FALLBACK_H));
        (
            (w * SCREEN_FRACTION).max(MIN_W),
            (h * SCREEN_FRACTION).max(MIN_H),
            (h - self.layout.height).max(0.0),
        )
    }

    /// Pull the layout back inside what the screen can hold.
    ///
    /// Called when the bounds arrive, not only when a control moves: a size
    /// saved on a 3440 px monitor gets restored on a 1533 px laptop, and
    /// without this it comes back off-screen with its own controls out of
    /// reach.
    pub(super) fn clamp_layout(&mut self) {
        let (max_w, max_h, max_margin) = self.bounds();
        let before = self.layout;
        self.layout.width = self.layout.width.clamp(MIN_W, max_w);
        self.layout.height = self.layout.height.clamp(MIN_H, max_h);
        self.layout.bottom_margin = self.layout.bottom_margin.clamp(0.0, max_margin);
        let half = (max_w - self.layout.width).max(0.0) / 2.0;
        self.layout.x_offset = self.layout.x_offset.clamp(-half, half);
        if self.layout != before {
            self.push_size();
            self.save_layout();
        }
    }

    /// Open or close the view. No surface resizing: the view fits whatever
    /// the surface is, which is the whole point of it not being a window.
    pub(super) fn set_settings_open(&mut self, open: bool) {
        if open != self.show_settings {
            self.show_settings = open;
            if !open {
                self.save_layout();
            }
        }
    }

    fn request(&mut self, r: LayoutRequest) {
        self.layout_request = Some(r);
    }

    fn push_size(&mut self) {
        self.request(LayoutRequest {
            size: Some((self.layout.width, self.layout.height)),
            ..Default::default()
        });
    }

    /// Grow or shrink by one frame's worth, while an arrow is held.
    fn grow(&mut self, dw: f32, dh: f32) {
        let (max_w, max_h, _) = self.bounds();
        self.layout.width = (self.layout.width + dw).clamp(MIN_W, max_w);
        self.layout.height = (self.layout.height + dh).clamp(MIN_H, max_h);
        self.push_size();
    }

    /// Move by a drag delta. Screen coordinates go down and the bottom margin
    /// goes up, which is why the sign flips.
    pub(super) fn drag_by(&mut self, delta: egui::Vec2) {
        if delta == egui::Vec2::ZERO {
            return;
        }
        let (max_w, _, max_margin) = self.bounds();
        let half = (max_w - self.layout.width).max(0.0) / 2.0;
        self.layout.x_offset = (self.layout.x_offset + delta.x).clamp(-half, half);
        self.layout.bottom_margin = (self.layout.bottom_margin - delta.y).clamp(0.0, max_margin);
        self.request(LayoutRequest {
            bottom_margin: Some(self.layout.bottom_margin),
            x_offset: Some(self.layout.x_offset),
            ..Default::default()
        });
    }

    /// The whole strip, showing configuration instead of speech.
    pub(super) fn settings_view(&mut self, ui: &mut egui::Ui) {
        let before = self.layout;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("\u{2699} Window").strong().size(15.0));
            ui.add_space(8.0);
            // The arrows resize, held rather than clicked. Position is not
            // here: dragging the strip is the gesture for that, and a control
            // duplicating a gesture is a second place for them to disagree.
            if held(ui, " < ", "segure: mais estreito") {
                self.grow(-REPEAT, 0.0);
            }
            if held(ui, " > ", "segure: mais largo") {
                self.grow(REPEAT, 0.0);
            }
            if held(ui, " /\\ ", "segure: mais alto") {
                self.grow(0.0, REPEAT);
            }
            if held(ui, " \\/ ", "segure: mais baixo") {
                self.grow(0.0, -REPEAT);
            }
            ui.add_space(10.0);
            ui.label("monitor:");
            if ui.button("<").on_hover_text("anterior").clicked() {
                self.request(LayoutRequest {
                    monitor_step: Some(-1),
                    ..Default::default()
                });
            }
            if ui.button(">").on_hover_text("próximo").clicked() {
                self.request(LayoutRequest {
                    monitor_step: Some(1),
                    ..Default::default()
                });
            }
            ui.add_space(10.0);
            if ui.button("Fechar").clicked() {
                self.set_settings_open(false);
            }
        });

        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (max_w, max_h, _) = self.bounds();
                ui.add(
                    egui::Slider::new(&mut self.layout.opacity, 0.15..=1.0)
                        .text("opacity")
                        .fixed_decimals(2),
                );
                ui.add(
                    egui::Slider::new(&mut self.layout.width, MIN_W..=max_w)
                        .text("width")
                        .fixed_decimals(0),
                );
                ui.add(
                    egui::Slider::new(&mut self.layout.height, MIN_H..=max_h)
                        .text("height")
                        .fixed_decimals(0),
                );
                ui.add_space(6.0);
                ui.label("Language:");
                ui.horizontal_wrapped(|ui| {
                    let current = crate::lock_settings(&self.settings).language.clone();
                    for lang in LANGUAGES {
                        if ui.selectable_label(current == *lang, *lang).clicked() {
                            crate::lock_settings(&self.settings).language = lang.to_string();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("arraste a barra para mover")
                        .italics()
                        .size(12.0)
                        .color(egui::Color32::from_gray(130)),
                );
            });

        // Only what moved is asked for: sending the whole layout every frame
        // would rebuild the EGL surface sixty times a second under a slider.
        if self.layout.width != before.width || self.layout.height != before.height {
            self.push_size();
        }
        if self.layout != before {
            self.save_layout();
        }
    }
}
