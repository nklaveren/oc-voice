//! The overlay as a `zwlr_layer_shell_v1` surface — M6.1.
//!
//! A layer surface is not an application window. The compositor puts it on a
//! named layer, anchored to edges, and **never tiles it and never gives it
//! focus** unless it asks. So none of `host_toplevel.rs` has a counterpart
//! here: there is nothing to float, nothing to pin, nothing to move, and no
//! race with the compositor to lose. That absence is the whole milestone.
//!
//! Rendering is GL through glutin and `egui_glow`, both already in the tree
//! via eframe — which is why this costs no new dependency and no egui bump.
//! glutin creates and resizes the `wl_egl_window` itself, so the surface it
//! is handed is the plain `wl_surface`.
//!
//! Off by default. `EframeHost` is still what ships until this has proven
//! pointer input and fractional scaling on real hardware.

use std::num::NonZeroU32;

use anyhow::{anyhow, Context as _, Result};
use eframe::egui;
use glutin::surface::GlSurface as _;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::SeatState,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use tracing::{info, warn};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_surface},
    Connection, QueueHandle,
};

use super::host::{Flow, Geometry, OverlayHost, OverlayUi};

#[path = "host_layer_gl.rs"]
mod gl;
#[path = "host_layer_input.rs"]
mod input;
use gl::Gl;

/// The namespace the compositor lists this surface under. `hyprctl layers`
/// shows it, and it is the acceptance check for M6.1.
const NAMESPACE: &str = "oc-voice";

pub struct LayerShellHost {
    pub geometry: Geometry,
    /// `middle` / `left` / `right` / `focused` / a connector name.
    pub monitor: String,
}

impl OverlayHost for LayerShellHost {
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland display")?;
        let (globals, mut queue) = registry_queue_init(&conn)?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| anyhow!("wl_compositor unavailable: {e}"))?;
        let shell = LayerShell::bind(&globals, &qh)
            .map_err(|e| anyhow!("this compositor has no wlr-layer-shell: {e}"))?;

        let mut state = State {
            registry: RegistryState::new(&globals),
            output: OutputState::new(&globals, &qh),
            seat: SeatState::new(&globals, &qh),
            layer: None,
            pointer: None,
            gl: None,
            egui: egui::Context::default(),
            ui,
            size: (self.geometry.width as u32, self.geometry.height as u32),
            scale: 1.0,
            events: Vec::new(),
            cursor: None,
            exit: false,
        };

        // Outputs have to be known before the surface is created: which screen
        // it lands on is a creation argument, not something to fix afterwards.
        // That is the difference from the toplevel host in one line.
        queue.roundtrip(&mut state)?;
        let output = pick_output(&state.output, &self.monitor);
        if let Some(ref o) = output {
            info!(monitor = ?state.output.info(o).and_then(|i| i.name), "overlay output chosen");
        } else {
            warn!(want = %self.monitor, "no output matched; letting the compositor choose");
        }

        let surface = compositor.create_surface(&qh);
        let layer = shell.create_layer_surface(
            &qh,
            surface,
            // Overlay, not Top: this sits above fullscreen windows, which is
            // where a subtitle for a meeting belongs.
            Layer::Overlay,
            Some(NAMESPACE),
            output.as_ref(),
        );
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(self.geometry.width as u32, self.geometry.height as u32);
        layer.set_margin(0, 0, self.geometry.bottom_margin as i32, 0);
        // Never take the keyboard. The whole reason `nofocus` had to be a
        // window rule is that a toplevel takes focus by default and dictated
        // text then lands in the overlay instead of the window being written
        // to. Here it is one declared value.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        state.layer = Some(layer);

        while !state.exit {
            queue.blocking_dispatch(&mut state)?;
        }
        state.ui.on_exit();
        Ok(())
    }
}

/// Left-to-right by the outputs' own coordinates, like the toplevel host does
/// with `hyprctl monitors` — connector order is not spatial order.
fn pick_output(outputs: &OutputState, want: &str) -> Option<wl_output::WlOutput> {
    let mut all: Vec<(i32, wl_output::WlOutput, Option<String>)> = outputs
        .outputs()
        .filter_map(|o| {
            let info = outputs.info(&o)?;
            Some((info.logical_position.unwrap_or((0, 0)).0, o, info.name))
        })
        .collect();
    if all.is_empty() {
        return None;
    }
    all.sort_by_key(|(x, _, _)| *x);
    let by_name = |n: &str| all.iter().find(|(_, _, name)| name.as_deref() == Some(n));
    match want {
        "left" => all.first(),
        "right" => all.last(),
        "middle" | "center" | "focused" | "active" => all.get(all.len() / 2),
        name => by_name(name).or_else(|| all.get(all.len() / 2)),
    }
    .map(|(_, o, _)| o.clone())
}

struct State {
    registry: RegistryState,
    output: OutputState,
    seat: SeatState,
    layer: Option<LayerSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    gl: Option<Gl>,
    egui: egui::Context,
    ui: Box<dyn OverlayUi>,
    size: (u32, u32),
    scale: f32,
    events: Vec<egui::Event>,
    cursor: Option<egui::Pos2>,
    exit: bool,
}

impl State {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
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
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w as f32 / ppp, h as f32 / ppp),
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

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = c.new_size;
        if w != 0 && h != 0 {
            self.size = (
                (w as f32 * self.scale) as u32,
                (h as f32 * self.scale) as u32,
            );
        }
        if self.gl.is_none() {
            if let Err(e) = self.init_gl(conn, layer.wl_surface()) {
                warn!(error = ?e, "could not create the GL context for the layer surface");
                self.exit = true;
                return;
            }
            info!(size = ?self.size, scale = self.scale, "layer surface configured");
        } else if let Some(gl) = self.gl.as_ref() {
            let (pw, ph) = self.size;
            gl.surface.resize(
                &gl.context,
                NonZeroU32::new(pw.max(1)).unwrap(),
                NonZeroU32::new(ph.max(1)).unwrap(),
            );
        }
        self.draw(qh);
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new: i32,
    ) {
        // Integer scale only; `wp_fractional_scale_v1` is the follow-up. On a
        // 1.5x or 1.67x output the compositor reports 2 here and downscales,
        // which is soft but correct in size — wrong size would be worse.
        self.scale = new as f32;
        surface.set_buffer_scale(new);
        info!(scale = new, "output scale changed");
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);
