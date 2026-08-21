//! Telling the compositor what the cursor should look like.
//!
//! A Wayland client owns the cursor image for as long as the pointer is over
//! its surface. This one never claimed it, so what you saw over the overlay
//! was whatever the last surface the pointer crossed had left behind: an
//! arrow off the desktop, a caret off a terminal, a pointing hand off a link.
//! Reported as "only Sim/Não have the mouse pointer", which is exactly what an
//! inherited image looks like — nothing about those two buttons was different.
//!
//! eframe got this from winit for free, which is why it only appeared with the
//! layer host. Same shape as the pointer input M6.1 also had to grow back.
//!
//! `wp_cursor_shape_v1` names shapes instead of shipping pixels: no theme to
//! load, no buffer to draw, and the compositor's own cursor theme is what the
//! person already chose. Without it the cursor keeps whatever it had, which is
//! today's behaviour and no worse than before.

use eframe::egui::CursorIcon;
use wayland_client::{
    globals::GlobalList, protocol::wl_pointer, Connection, Dispatch, QueueHandle,
};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};

use super::State;

/// The manager, the device for the current pointer, and what it was last told.
///
/// Two steps because they happen at different times: the global is there from
/// the first roundtrip, the pointer arrives with the seat's capabilities and
/// can come and go afterwards.
pub(super) struct Cursor {
    manager: WpCursorShapeManagerV1,
    device: Option<WpCursorShapeDeviceV1>,
    /// Last shape sent. The protocol is happy to be told again every frame;
    /// sixty round trips a second to say nothing changed is the waste.
    shown: Option<Shape>,
}

impl Cursor {
    /// Bind the manager, if the compositor has the protocol at all.
    pub(super) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Self> {
        let manager: WpCursorShapeManagerV1 = globals.bind(qh, 1..=1, ()).ok()?;
        Some(Cursor {
            manager,
            device: None,
            shown: None,
        })
    }

    /// Take a device for a pointer that has just appeared.
    pub(super) fn attach(&mut self, pointer: &wl_pointer::WlPointer, qh: &QueueHandle<State>) {
        self.device = Some(self.manager.get_pointer(pointer, qh, ()));
        self.shown = None;
    }

    /// Follow what the UI asked for.
    ///
    /// `serial` has to be the one from the last `enter`: the request is
    /// refused otherwise, which is the protocol's way of stopping a client
    /// from changing the cursor while it does not have the pointer.
    pub(super) fn set(&mut self, icon: CursorIcon, serial: u32) {
        let want = shape(icon);
        if self.shown == Some(want) || serial == 0 {
            return;
        }
        if let Some(device) = self.device.as_ref() {
            self.shown = Some(want);
            device.set_shape(serial, want);
        }
    }

    /// The pointer left; whatever we set no longer applies.
    pub(super) fn forget(&mut self) {
        self.shown = None;
    }
}

/// egui's vocabulary into the protocol's.
///
/// Kept as a free function so it can be checked without a compositor. The
/// mapping is mostly one to one; where it is not, the rule is that a cursor
/// which overstates what a control does is worse than a plain arrow.
pub(super) fn shape(icon: CursorIcon) -> Shape {
    match icon {
        CursorIcon::Default => Shape::Default,
        CursorIcon::PointingHand => Shape::Pointer,
        CursorIcon::Grab => Shape::Grab,
        CursorIcon::Grabbing => Shape::Grabbing,
        CursorIcon::Text | CursorIcon::VerticalText => Shape::Text,
        CursorIcon::Move | CursorIcon::AllScroll => Shape::Move,
        CursorIcon::NotAllowed | CursorIcon::NoDrop => Shape::NotAllowed,
        CursorIcon::Wait => Shape::Wait,
        CursorIcon::Progress => Shape::Progress,
        CursorIcon::Help => Shape::Help,
        CursorIcon::Crosshair => Shape::Crosshair,
        CursorIcon::ContextMenu => Shape::ContextMenu,
        CursorIcon::Cell => Shape::Cell,
        CursorIcon::Alias => Shape::Alias,
        CursorIcon::Copy => Shape::Copy,
        CursorIcon::ZoomIn => Shape::ZoomIn,
        CursorIcon::ZoomOut => Shape::ZoomOut,
        // Resize edges. The overlay only ever asks for the horizontal and
        // vertical pairs, through the scroll area's own handles.
        CursorIcon::ResizeHorizontal | CursorIcon::ResizeColumn => Shape::EwResize,
        CursorIcon::ResizeVertical | CursorIcon::ResizeRow => Shape::NsResize,
        CursorIcon::ResizeNeSw => Shape::NeswResize,
        CursorIcon::ResizeNwSe => Shape::NwseResize,
        CursorIcon::ResizeEast => Shape::EResize,
        CursorIcon::ResizeWest => Shape::WResize,
        CursorIcon::ResizeNorth => Shape::NResize,
        CursorIcon::ResizeSouth => Shape::SResize,
        CursorIcon::ResizeNorthEast => Shape::NeResize,
        CursorIcon::ResizeNorthWest => Shape::NwResize,
        CursorIcon::ResizeSouthEast => Shape::SeResize,
        CursorIcon::ResizeSouthWest => Shape::SwResize,
        // `None` means hide the pointer, which this protocol cannot express —
        // that needs `set_cursor` with a null surface. Nothing here asks for
        // it, and an arrow is the honest fallback for anything unmapped.
        _ => Shape::Default,
    }
}

// Both objects are pure factories: created, used, and never heard from again.
impl Dispatch<WpCursorShapeManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpCursorShapeManagerV1,
        _: <WpCursorShapeManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &WpCursorShapeDeviceV1,
        _: <WpCursorShapeDeviceV1 as wayland_client::Proxy>::Event,
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
    fn the_two_the_overlay_actually_asks_for_are_mapped() {
        // A button under the pointer and the panel being dragged. Everything
        // else in the table is there so an unmapped icon cannot silently
        // become an arrow, but these two are the ones this UI produces.
        assert_eq!(shape(CursorIcon::PointingHand), Shape::Pointer);
        assert_eq!(shape(CursorIcon::Grab), Shape::Grab);
        assert_eq!(shape(CursorIcon::Default), Shape::Default);
    }

    #[test]
    fn an_unmapped_icon_is_an_arrow_and_not_a_panic() {
        // egui adds icons; the compositor's list is fixed. A new one has to
        // land on something harmless rather than take the overlay down.
        assert_eq!(shape(CursorIcon::None), Shape::Default);
    }
}
