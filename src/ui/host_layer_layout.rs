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
        let Some(layer) = self.layer.as_ref() else {
            return;
        };
        if let Some((w, h)) = request.size {
            self.geometry.width = w;
            self.geometry.height = h;
            layer.set_size(w as u32, h as u32);
            layer.commit();
        }
        if let Some(margin) = request.bottom_margin {
            self.geometry.bottom_margin = margin;
            layer.set_margin(0, 0, margin as i32, 0);
            layer.commit();
        }
        if let Some(step) = request.monitor_step {
            self.step_monitor(step);
            self.rebuild = true;
            let _ = qh;
        }
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
