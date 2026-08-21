//! The EGL context behind the layer surface.
//!
//! glutin creates and resizes the `wl_egl_window` itself, so what it is handed
//! is the plain `wl_surface`. Kept apart from the protocol handling because
//! the two fail in completely different ways, and a file that mixes them makes
//! "no config with an alpha channel" look like a Wayland problem.

use std::num::NonZeroU32;
use std::ptr::NonNull;

use anyhow::{anyhow, Result};
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::surface::{Surface as GlutinSurface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use wayland_client::{protocol::wl_surface, Connection, Proxy};

use super::State;

/// The GL side: a context, a surface, and egui's painter for them.
pub(super) struct Gl {
    pub(super) context: PossiblyCurrentContext,
    pub(super) surface: GlutinSurface<WindowSurface>,
    pub(super) painter: egui_glow::Painter,
}

impl State {
    /// Build the GL context against a surface that already exists.
    ///
    /// Deferred to the first `configure` because that is when the compositor
    /// has agreed on a size — creating the EGL window before then means
    /// resizing it immediately afterwards.
    pub(super) fn init_gl(
        &mut self,
        conn: &Connection,
        surface: &wl_surface::WlSurface,
    ) -> Result<()> {
        let display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(conn.backend().display_ptr().cast())
                .ok_or_else(|| anyhow!("null wl_display"))?,
        ));
        let window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface.id().as_ptr().cast()).ok_or_else(|| anyhow!("null wl_surface"))?,
        ));

        // SAFETY: both handles come from live objects owned by this State and
        // outlive the display and surface created from them.
        let display = unsafe { Display::new(display_handle, DisplayApiPreference::Egl) }?;
        let template = ConfigTemplateBuilder::new()
            // The overlay draws its own rounded panel over the desktop.
            .with_alpha_size(8)
            .with_transparency(true)
            .build();
        let config = unsafe { display.find_configs(template) }?
            .reduce(|best, c| {
                if c.alpha_size() > best.alpha_size() {
                    c
                } else {
                    best
                }
            })
            .ok_or_else(|| anyhow!("no EGL config with an alpha channel"))?;

        let (w, h) = self.size;
        let attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            window_handle,
            NonZeroU32::new(w.max(1)).unwrap(),
            NonZeroU32::new(h.max(1)).unwrap(),
        );
        let gl_surface = unsafe { display.create_window_surface(&config, &attrs) }?;
        let context = unsafe {
            display.create_context(
                &config,
                &ContextAttributesBuilder::new().build(Some(window_handle)),
            )
        }?
        .make_current(&gl_surface)?;

        let gl = unsafe {
            std::sync::Arc::new(egui_glow::glow::Context::from_loader_function(|s| {
                let cs = std::ffi::CString::new(s).unwrap();
                display.get_proc_address(&cs).cast()
            }))
        };
        let painter =
            egui_glow::Painter::new(gl, "", None, false).map_err(|e| anyhow!("egui_glow: {e}"))?;

        self.gl = Some(Gl {
            context,
            surface: gl_surface,
            painter,
        });
        Ok(())
    }
}
