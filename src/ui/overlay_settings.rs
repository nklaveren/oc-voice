//! The Settings panel, and the controls that move the overlay.
//!
//! It exists because of M6.1. A layer surface is not a window: `windowrule`
//! does not match it, the compositor will not drag or resize it, and
//! `hyprctl clients` does not list it. Everything a window manager used to
//! offer has to be offered here — which is arguably where it belonged, since
//! the controls now sit on the thing they control.
//!
//! **The panel must never be clipped by the size it controls.** Shrinking the
//! overlay with a slider that lives inside the overlay is a one-way door: the
//! panel goes off the surface and there is no way back to the slider that did
//! it. So opening Settings grows the surface to fit, and closing it restores
//! exactly what was there before.
//!
//! Size and position are *requests*: only the host owns the surface. Opacity
//! is not — it is paint, and paint never leaves the UI.

use super::*;
use crate::ui::host::LayoutRequest;

/// Bounds for the sliders. Wide enough to be useful, narrow enough that the
/// overlay cannot be driven into a state with no way back.
const MIN_W: f32 = 320.0;
const MAX_W: f32 = 2400.0;
const MIN_H: f32 = 120.0;
const MAX_H: f32 = 1200.0;
const MAX_MARGIN: f32 = 600.0;
/// One press of an arrow.
const NUDGE: f32 = 20.0;
/// What the panel needs to be fully visible, arrows included.
const PANEL_W: f32 = 620.0;
const PANEL_H: f32 = 400.0;

impl OverlayApp {
    /// Grow to fit the panel when it opens, restore when it closes.
    pub(super) fn settings_visibility_changed(&mut self, opening: bool) {
        if opening {
            self.size_before_settings = Some((self.layout.width, self.layout.height));
            let w = self.layout.width.max(PANEL_W);
            let h = self.layout.height.max(PANEL_H);
            if (w, h) != (self.layout.width, self.layout.height) {
                self.request(LayoutRequest {
                    size: Some((w, h)),
                    ..Default::default()
                });
            }
        } else if let Some((w, h)) = self.size_before_settings.take() {
            self.request(LayoutRequest {
                size: Some((w, h)),
                ..Default::default()
            });
        }
    }

    fn request(&mut self, r: LayoutRequest) {
        self.layout_request = Some(r);
    }

    pub(super) fn settings_window(&mut self, ui: &mut egui::Ui) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                // The move controls frame the box rather than sitting in it.
                // They are the one thing that must stay reachable no matter
                // what the sliders inside have done.
                ui.vertical_centered(|ui| {
                    if ui.button("   /\\   ").on_hover_text("subir").clicked() {
                        self.nudge_margin(NUDGE);
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button(" < ")
                        .on_hover_text("monitor à esquerda")
                        .clicked()
                    {
                        self.step_monitor(-1);
                    }
                    ui.vertical(|ui| self.settings_box(ui, &mut open));
                    if ui
                        .button(" > ")
                        .on_hover_text("monitor à direita")
                        .clicked()
                    {
                        self.step_monitor(1);
                    }
                });
                ui.vertical_centered(|ui| {
                    if ui.button("   \\/   ").on_hover_text("descer").clicked() {
                        self.nudge_margin(-NUDGE);
                    }
                });
            });
        if open != self.show_settings {
            self.settings_visibility_changed(open);
            self.show_settings = open;
            self.save_layout();
        }
    }

    fn nudge_margin(&mut self, by: f32) {
        self.layout.bottom_margin = (self.layout.bottom_margin + by).clamp(0.0, MAX_MARGIN);
        self.request(LayoutRequest {
            bottom_margin: Some(self.layout.bottom_margin),
            ..Default::default()
        });
        self.save_layout();
    }

    fn step_monitor(&mut self, step: i32) {
        self.request(LayoutRequest {
            monitor_step: Some(step),
            ..Default::default()
        });
    }

    fn settings_box(&mut self, ui: &mut egui::Ui, open: &mut bool) {
        ui.label("Language:");
        ui.horizontal(|ui| {
            let current = crate::lock_settings(&self.settings).language.clone();
            for lang in LANGUAGES {
                if ui.selectable_label(current == *lang, *lang).clicked() {
                    crate::lock_settings(&self.settings).language = lang.to_string();
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.label(
            egui::RichText::new("Window configuration")
                .strong()
                .size(14.0),
        );
        ui.add_space(4.0);

        let before = self.layout;
        ui.add(
            egui::Slider::new(&mut self.layout.opacity, 0.15..=1.0)
                .text("opacity")
                .fixed_decimals(2),
        );
        ui.add(
            egui::Slider::new(&mut self.layout.width, MIN_W..=MAX_W)
                .text("width")
                .fixed_decimals(0),
        );
        ui.add(
            egui::Slider::new(&mut self.layout.height, MIN_H..=MAX_H)
                .text("height")
                .fixed_decimals(0),
        );

        // Only what moved is asked for. Sending the whole layout every frame
        // would rebuild the EGL surface sixty times a second while a slider
        // is being dragged.
        if self.layout.width != before.width || self.layout.height != before.height {
            // While the panel is open the surface is grown to fit it, so the
            // slider records the intent and the restore on close applies it.
            self.size_before_settings = Some((self.layout.width, self.layout.height));
            let w = self.layout.width.max(PANEL_W);
            let h = self.layout.height.max(PANEL_H);
            self.request(LayoutRequest {
                size: Some((w, h)),
                ..Default::default()
            });
        }
        if self.layout != before {
            self.save_layout();
        }

        ui.add_space(8.0);
        if ui.button("Close").clicked() {
            *open = false;
        }
    }
}
