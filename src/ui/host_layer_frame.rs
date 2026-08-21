//! Turning the agreed size into pixels: buffer, viewport, and one frame.
//!
//! Split from the protocol handling because the two answer different
//! questions. Up there: which output, which surface, what the compositor
//! agreed to. Here: how many physical pixels that is, and what to draw into
//! them. The conversion between the two is where the 1.67x panel broke twice.

use std::num::NonZeroU32;

use eframe::egui;
use glutin::surface::GlSurface as _;
use smithay_client_toolkit::shell::WaylandSurface;
use tracing::{info, warn};
use wayland_client::QueueHandle;

use super::State;
use crate::ui::host::Flow;
use smithay_client_toolkit::compositor::FrameCallbackData;

impl State {
    /// A new fractional scale from the output. Resizes the buffer and tells
    /// the viewport what logical size it stands for.
    pub(super) fn set_fractional_scale(&mut self, factor: f32, qh: &QueueHandle<Self>) {
        if (self.scale - factor).abs() < f32::EPSILON {
            return;
        }
        self.scale = factor;
        self.size = self.buffer_size();
        self.apply_viewport();
        self.resize_gl();
        info!(scale = factor, buffer = ?self.size, "fractional scale");
        self.draw(qh);
    }

    /// Map the physical buffer onto the logical rectangle the surface owns.
    /// Only meaningful with a viewport — with `set_buffer_scale` the
    /// compositor derives the same thing from an integer, badly.
    pub(super) fn apply_viewport(&self) {
        if let Some(vp) = self.viewport.as_ref() {
            let (w, h) = self.logical;
            vp.set_destination(w.max(1) as i32, h.max(1) as i32);
        }
    }

    /// Physical pixels for the agreed logical size.
    ///
    /// Rounded, not truncated. A fractional scale rarely lands on a whole
    /// number — 900 x 200/120 is 1499.9998 — and `as u32` would throw the
    /// last column away, leaving a one-pixel seam along the edge for every
    /// dimension that happened to fall just short.
    pub(super) fn buffer_size(&self) -> (u32, u32) {
        let (w, h) = self.logical;
        (
            (w as f32 * self.scale).round().max(1.0) as u32,
            (h as f32 * self.scale).round().max(1.0) as u32,
        )
    }

    /// Follow `self.size` with the EGL window, which glutin owns.
    pub(super) fn resize_gl(&mut self) {
        let (w, h) = self.size;
        if let Some(gl) = self.gl.as_ref() {
            gl.surface.resize(
                &gl.context,
                NonZeroU32::new(w.max(1)).unwrap(),
                NonZeroU32::new(h.max(1)).unwrap(),
            );
        }
    }

    pub(super) fn draw(&mut self, qh: &QueueHandle<Self>) {
        if self.ui.tick() == Flow::Exit {
            self.exit = true;
            return;
        }
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        let Some(gl) = self.gl.as_mut() else { return };

        let (w, h) = self.size;
        let ppp = self.scale;
        let raw = egui::RawInput {
            // egui thinks in logical points; the buffer is physical. These are
            // the two ends of the same conversion, so it uses the logical size
            // the compositor agreed to rather than dividing back out of it.
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(self.logical.0 as f32, self.logical.1 as f32),
            )),
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        self.egui.set_pixels_per_point(ppp);

        // The context is cloned so the closure can borrow the UI separately —
        // both live in `self`.
        let ctx = self.egui.clone();
        let app = &mut self.ui;
        // `run_ui` hands over the same top-level `Ui` eframe builds for its
        // own `App::ui`, so both hosts satisfy the contract identically.
        let out = ctx.run_ui(raw, |ui| app.paint(ui));

        // Whatever the UI decided the cursor should be. eframe hands this to
        // winit; here it is ours to send, and not sending it is what left the
        // pointer wearing whatever image it walked in with.
        if let Some(c) = self.cursor_shape.as_mut() {
            c.set(out.platform_output.cursor_icon, self.enter_serial);
        }

        let dims: [u32; 2] = [w.max(1), h.max(1)];
        let clipped = ctx.tessellate(out.shapes, out.pixels_per_point);
        gl.painter.clear(dims, self.ui.clear_color());
        gl.painter.paint_and_update_textures(
            dims,
            out.pixels_per_point,
            &clipped,
            &out.textures_delta,
        );
        // Ask for the next frame before presenting, so the callback is already
        // registered when the compositor takes the buffer.
        layer
            .wl_surface()
            .frame(qh, FrameCallbackData(layer.wl_surface().clone()));
        if let Err(e) = gl.surface.swap_buffers(&gl.context) {
            warn!(error = %e, "swap_buffers failed");
            self.exit = true;
        }
    }
}
