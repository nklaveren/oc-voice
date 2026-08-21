//! Moving and resizing the overlay from inside it.
//!
//! A layer surface is not a window. `windowrule` does not match it, the
//! compositor will not drag it, and `hyprctl clients` does not list it — so
//! everything a window manager used to offer has to be offered by the app.
//! Which is arguably where it belonged: the controls now sit on the thing
//! they control.
//!
//! Size and margin are cheap — the surface is told and redraws. Changing
//! monitor is not: a layer surface belongs to the output it was created on,
//! so moving means destroying and rebuilding. That path already existed, for
//! the monitor that gets unplugged.

use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::shell::WaylandSurface as _;
use tracing::info;
use wayland_client::QueueHandle;

use super::State;

/// Output names left to right. The same ordering `pick_output` uses, exposed
/// so stepping between them is an index move.
/// The logical size of the output the surface is on. The compositor is the
/// only one that knows it, and the UI needs it to keep its own controls on
/// screen.
pub(super) fn output_logical_size(outputs: &OutputState, want: &str) -> Option<(f32, f32)> {
    let mut all: Vec<(i32, (i32, i32), Option<String>)> = outputs
        .outputs()
        .filter_map(|o| {
            let info = outputs.info(&o)?;
            let size = info.logical_size?;
            Some((info.logical_position.unwrap_or((0, 0)).0, size, info.name))
        })
        .collect();
    all.sort_by_key(|(x, _, _)| *x);
    let pick = all
        .iter()
        .find(|(_, _, n)| n.as_deref() == Some(want))
        .or_else(|| all.get(all.len() / 2))?;
    Some((pick.1 .0 as f32, pick.1 .1 as f32))
}

pub(super) fn ordered_output_names(outputs: &OutputState) -> Vec<String> {
    let mut all: Vec<(i32, String)> = outputs
        .outputs()
        .filter_map(|o| {
            let info = outputs.info(&o)?;
            Some((info.logical_position.unwrap_or((0, 0)).0, info.name?))
        })
        .collect();
    all.sort_by_key(|(x, _)| *x);
    all.into_iter().map(|(_, n)| n).collect()
}

impl State {
    /// Carry out what the Settings panel asked for.
    ///
    /// Size and margin are cheap: the surface is told and redraws. Changing
    /// monitor is not — a layer surface belongs to the output it was created
    /// on, so moving means destroying and rebuilding, which is the same path
    /// a monitor being unplugged already takes.
    pub(super) fn apply_layout(
        &mut self,
        request: crate::ui::host::LayoutRequest,
        qh: &QueueHandle<Self>,
    ) {
        if self.layer.is_none() {
            return;
        }
        if let Some((w, h)) = request.size {
            // The host is the authority on what fits: it is the only side
            // that has ever seen the output. A UI asking for more than the
            // screen gets the screen, not an off-screen surface.
            let (w, h) = self.fit_to_output(w, h);
            self.geometry.width = w;
            self.geometry.height = h;
            if let Some(layer) = self.layer.as_ref() {
                layer.set_size(w as u32, h as u32);
                layer.commit();
            }
        }
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        if request.bottom_margin.is_some() || request.x_offset.is_some() {
            if let Some(m) = request.bottom_margin {
                self.geometry.bottom_margin = m;
            }
            if let Some(x) = request.x_offset {
                self.x_offset = x;
            }
            self.place(layer);
        }
        if let Some(step) = request.monitor_step {
            // Rebuilding destroys the surface and makes a new one. With a
            // single output the step lands on the same monitor, so doing it
            // anyway was pure risk for no movement — and it took the overlay
            // off screen, which is the worst possible outcome for a button
            // whose honest answer is "there is nowhere to go".
            let before = self.want_monitor.clone();
            self.step_monitor(step);
            if self.want_monitor != before {
                self.rebuild = true;
            } else {
                info!(monitor = %before, "no other output to move to");
            }
            let _ = qh;
        }
    }

    /// Put the surface where the layout says.
    ///
    /// Anchored bottom-**left**, not bottom alone. With only `BOTTOM` the
    /// compositor centres the surface and ignores horizontal margins
    /// entirely, which is why dragging moved it up and down and never
    /// sideways. Anchoring a side makes the margin an absolute position, and
    /// centring becomes arithmetic this side does.
    pub(super) fn place(&self, layer: &smithay_client_toolkit::shell::wlr_layer::LayerSurface) {
        let centred = output_logical_size(&self.output, &self.want_monitor)
            .map(|(ow, _)| (ow - self.geometry.width).max(0.0) / 2.0)
            .unwrap_or(0.0);
        let left = (centred + self.x_offset).max(0.0) as i32;
        layer.set_margin(0, 0, self.geometry.bottom_margin as i32, left);
        layer.commit();
    }

    /// Clamp a requested size to the output, and tell the UI what the
    /// ceiling is so its own sliders stop there too.
    pub(super) fn fit_to_output(&mut self, w: f32, h: f32) -> (f32, f32) {
        let Some((ow, oh)) = output_logical_size(&self.output, &self.want_monitor) else {
            return (w, h);
        };
        self.ui.set_bounds(ow, oh);
        (w.min(ow), h.min(oh))
    }

    /// Move `want_monitor` to the neighbour in spatial order, clamped.
    ///
    /// Clamped rather than wrapping: on three screens, pressing right twice
    /// from the middle should land on the right one and stay there, not
    /// reappear on the far left.
    fn step_monitor(&mut self, step: i32) {
        if self.ordered_outputs.is_empty() {
            return;
        }
        let current = self
            .ordered_outputs
            .iter()
            .position(|n| *n == self.want_monitor)
            .unwrap_or(self.ordered_outputs.len() / 2) as i32;
        let next = (current + step).clamp(0, self.ordered_outputs.len() as i32 - 1) as usize;
        self.want_monitor = self.ordered_outputs[next].clone();
        info!(monitor = %self.want_monitor, "overlay moving to another output");
    }
}
