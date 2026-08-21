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

use anyhow::{anyhow, Context as _, Result};
use eframe::egui;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
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

use super::host::{Geometry, OverlayHost, OverlayUi};

#[path = "host_layer_frame.rs"]
mod frame;
#[path = "host_layer_gl.rs"]
mod gl;
#[path = "host_layer_input.rs"]
mod input;
#[path = "host_layer_layout.rs"]
mod layout;
#[path = "host_layer_scale.rs"]
pub(super) mod scale;
use gl::Gl;

/// The namespace the compositor lists this surface under. `hyprctl layers`
/// shows it, and it is the acceptance check for M6.1.
const NAMESPACE: &str = "oc-voice";

pub struct LayerShellHost {
    pub geometry: Geometry,
    /// `middle` / `left` / `right` / `focused` / a connector name.
    pub monitor: String,
    /// Where to go when this compositor has no layer shell. Taking the whole
    /// overlay down over a protocol the machine simply does not implement
    /// would be the worst possible way to ship a default.
    pub fallback: Option<Box<dyn OverlayHost>>,
}

impl OverlayHost for LayerShellHost {
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()> {
        let geom = self.geometry;
        let conn = Connection::connect_to_env().context("connecting to the Wayland display")?;
        let (globals, mut queue) = registry_queue_init(&conn)?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| anyhow!("wl_compositor unavailable: {e}"))?;
        let shell = match LayerShell::bind(&globals, &qh) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "no wlr-layer-shell here; falling back to a toplevel window");
                return match self.fallback {
                    Some(host) => host.run(ui),
                    None => Err(anyhow!("this compositor has no wlr-layer-shell: {e}")),
                };
            }
        };

        let mut state = State {
            registry: RegistryState::new(&globals),
            output: OutputState::new(&globals, &qh),
            seat: SeatState::new(&globals, &qh),
            compositor,
            shell,
            geometry: geom,
            want_monitor: self.monitor,
            ordered_outputs: Vec::new(),
            x_offset: 0.0,
            rebuild: false,
            layer: None,
            fractional: None,
            viewport: None,
            fractional_surface: None,
            pointer: None,
            gl: None,
            egui: egui::Context::default(),
            ui,
            size: (geom.width as u32, geom.height as u32),
            logical: (geom.width as u32, geom.height as u32),
            scale: 1.0,
            events: Vec::new(),
            cursor: None,
            exit: false,
        };

        // Outputs have to be known before the surface is created: which screen
        // it lands on is a creation argument, not something to fix afterwards.
        // That is the difference from the toplevel host in one line.
        queue.roundtrip(&mut state)?;
        state.fractional = scale::Fractional::bind(&globals, &qh);
        if state.fractional.is_none() {
            warn!("no fractional scaling here; falling back to integer buffer scale");
        }
        state.build_surface(&qh);

        while !state.exit {
            queue.blocking_dispatch(&mut state)?;
            if let Some(request) = state.ui.take_layout_request() {
                state.apply_layout(request, &qh);
            }
            if state.rebuild {
                state.rebuild = false;
                state.build_surface(&qh);
            }
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
    /// Kept so the surface can be built more than once. A layer surface is
    /// bound to an output, and an output that goes away takes the surface
    /// with it — see `closed`.
    compositor: CompositorState,
    shell: LayerShell,
    geometry: Geometry,
    want_monitor: String,
    /// Names left to right, so a step is an index move rather than a guess.
    /// Filled at every rebuild, because monitors come and go.
    ordered_outputs: Vec<String>,
    /// How far the surface sits from the monitor's horizontal centre.
    x_offset: f32,
    rebuild: bool,
    layer: Option<LayerSurface>,
    /// The fractional-scale pair, when the compositor offers it. Without it
    /// the integer path stays, which is soft but correct.
    fractional: Option<scale::Fractional>,
    viewport: Option<wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport>,
    fractional_surface:
        Option<wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1>,
    pointer: Option<wl_pointer::WlPointer>,
    gl: Option<Gl>,
    egui: egui::Context,
    ui: Box<dyn OverlayUi>,
    /// Buffer size in physical pixels — what GL draws into.
    size: (u32, u32),
    /// The size the compositor agreed to, in logical pixels. Kept because a
    /// scale change has to recompute the buffer from it, and the configure
    /// that carried it may have arrived before the scale did.
    logical: (u32, u32),
    scale: f32,
    events: Vec<egui::Event>,
    cursor: Option<egui::Pos2>,
    exit: bool,
}

impl State {
    /// Create the layer surface on the output the config asks for.
    ///
    /// Called again whenever the compositor closes the surface, which is not
    /// an error condition: a layer surface belongs to one output, so unplugging
    /// a monitor — or a DPMS transition, which is how this was found — destroys
    /// it. A toplevel survives that because the compositor just moves the
    /// window. Treating it as fatal made a screen blink take the whole app
    /// down, transcription included.
    fn build_surface(&mut self, qh: &QueueHandle<Self>) {
        // The EGL surface holds the old wl_surface; it has to go first.
        self.gl = None;
        self.layer = None;

        self.ordered_outputs = layout::ordered_output_names(&self.output);
        let output = pick_output(&self.output, &self.want_monitor);
        match output {
            Some(ref o) => {
                info!(monitor = ?self.output.info(o).and_then(|i| i.name), "overlay output chosen")
            }
            None => warn!(want = %self.want_monitor, "no output matched; compositor chooses"),
        }

        let surface = self.compositor.create_surface(qh);
        let layer = self.shell.create_layer_surface(
            qh,
            surface,
            // Overlay, not Top: this sits above fullscreen windows, which is
            // where a subtitle for a meeting belongs.
            Layer::Overlay,
            Some(NAMESPACE),
            output.as_ref(),
        );
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(self.geometry.width as u32, self.geometry.height as u32);
        let x = self.x_offset as i32;
        layer.set_margin(0, -x, self.geometry.bottom_margin as i32, x);
        // Never take the keyboard. The whole reason `nofocus` had to be a
        // window rule is that a toplevel takes focus by default and dictated
        // text then lands in the overlay instead of the window being written
        // to. Here it is one declared value.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if let Some(f) = self.fractional.as_ref() {
            let (fs, vp) = f.attach(layer.wl_surface(), qh);
            self.fractional_surface = Some(fs);
            self.viewport = Some(vp);
        }
        layer.commit();
        self.layer = Some(layer);
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // Not fatal, and measured: waking the monitors with `dpms on` made
        // HDMI-A-1 disappear and come back, the compositor destroyed the
        // surface, and the whole process exited cleanly with the recording
        // still open. Rebuild instead.
        warn!("layer surface closed by the compositor; rebuilding");
        self.rebuild = true;
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
            self.logical = (w, h);
        }
        self.size = self.buffer_size();
        self.apply_viewport();
        if self.gl.is_none() {
            if let Err(e) = self.init_gl(conn, layer.wl_surface()) {
                warn!(error = ?e, "could not create the GL context for the layer surface");
                self.exit = true;
                return;
            }
            info!(size = ?self.size, scale = self.scale, "layer surface configured");
            // First moment the output is known well enough to bound the
            // controls that resize this surface.
            let (w, h) = (self.geometry.width, self.geometry.height);
            self.fit_to_output(w, h);
        } else {
            self.resize_gl();
        }
        self.draw(qh);
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new: i32,
    ) {
        // This arrives *after* the first configure, so the buffer already
        // exists at the old scale. Storing the number and stopping there left
        // a 900x350 buffer on a surface the compositor now reads as 1800x700
        // — measured on the 1.67x panel, where the compositor reports 2.
        //
        // Integer scale only; `wp_fractional_scale_v1` is the follow-up. On a
        // 1.67x output this renders at 2x and the compositor downscales:
        // slightly soft, correct in size. Wrong size would be worse.
        if self.fractional.is_some() {
            // The viewport already carries the mapping. Setting a buffer
            // scale on top of it applies the correction twice.
            return;
        }
        self.scale = new as f32;
        surface.set_buffer_scale(new);
        self.size = self.buffer_size();
        self.resize_gl();
        info!(scale = new, buffer = ?self.size, "output scale changed");
        self.draw(qh);
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
