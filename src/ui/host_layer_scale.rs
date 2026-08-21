//! Asking the output what scale it actually wants.
//!
//! `wl_surface.set_buffer_scale` takes an integer, so on a 1.67x panel the
//! compositor reports 2, the client renders at 2x, and the compositor
//! downscales. The size is right and the edges are soft — measured on this
//! laptop, where a 900x350 overlay became an 1800x700 buffer squeezed into
//! 1503x585.
//!
//! `wp_fractional_scale_v1` reports the real number in 120ths, and
//! `wp_viewporter` maps whatever buffer was drawn onto the logical size the
//! surface occupies. Together they let the buffer be exactly the pixels the
//! screen has. **They come as a pair**: with a viewport destination set, the
//! buffer scale must stay 1, or the two corrections multiply.
//!
//! Both are optional. A compositor without them falls back to the integer
//! path, which is worse-looking and correct.

use wayland_client::{protocol::wl_surface, Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use super::State;

/// The protocol reports scale in 120ths, so 1.67x arrives as 200.
const DENOMINATOR: f32 = 120.0;

/// The pair, when the compositor offers them.
pub(super) struct Fractional {
    manager: WpFractionalScaleManagerV1,
    viewporter: WpViewporter,
}

impl Fractional {
    /// Bind both or neither: one without the other cannot express a
    /// fractional scale, and half of the mechanism is worse than none.
    pub(super) fn bind(
        globals: &wayland_client::globals::GlobalList,
        qh: &QueueHandle<State>,
    ) -> Option<Self> {
        let manager: WpFractionalScaleManagerV1 = globals.bind(qh, 1..=1, ()).ok()?;
        let viewporter: WpViewporter = globals.bind(qh, 1..=1, ()).ok()?;
        Some(Fractional {
            manager,
            viewporter,
        })
    }

    /// Attach both objects to a surface. The returned viewport has to outlive
    /// the surface's buffers, so the caller keeps it.
    pub(super) fn attach(
        &self,
        surface: &wl_surface::WlSurface,
        qh: &QueueHandle<State>,
    ) -> (WpFractionalScaleV1, WpViewport) {
        (
            self.manager.get_fractional_scale(surface, qh, ()),
            self.viewporter.get_viewport(surface, qh, ()),
        )
    }
}

/// Turn the protocol's 120ths into the factor the renderer uses.
pub(super) fn factor(reported: u32) -> f32 {
    (reported as f32 / DENOMINATOR).max(0.1)
}

impl Dispatch<WpFractionalScaleV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.set_fractional_scale(factor(scale), qh);
        }
    }
}

// The three below are pure factories: they are created, used and destroyed
// without ever sending an event back.
impl Dispatch<WpFractionalScaleManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpViewporter,
        _: <WpViewporter as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpViewport,
        _: <WpViewport as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocols_units_are_hundred_twentieths() {
        // 1.67x arrives as 200, not as 167 and not as 2. Getting this wrong
        // scales the whole overlay by 120 or by 1/120, so it is pinned rather
        // than left to a comment.
        assert!((factor(120) - 1.0).abs() < 1e-6);
        assert!((factor(200) - 1.6667).abs() < 1e-3);
        assert!((factor(180) - 1.5).abs() < 1e-6);
        assert!((factor(240) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_nonsense_scale_never_reaches_zero() {
        // A zero would make the buffer zero-sized and the surface unmappable.
        assert!(factor(0) > 0.0);
    }
}
