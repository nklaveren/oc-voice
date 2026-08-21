//! `wl_pointer` and `wl_seat` translated into `egui`.
//!
//! Its own file because it is the half the M6.1 estimate omitted: eframe gets
//! this from winit for free, and without it the overlay's Settings, Gravar,
//! Ata and mode buttons are decoration.

use eframe::egui;
use smithay_client_toolkit::seat::{
    pointer::{PointerEvent, PointerEventKind, PointerHandler},
    Capability, SeatHandler, SeatState,
};
use tracing::warn;
use wayland_client::{
    protocol::{wl_pointer, wl_seat},
    Connection, QueueHandle,
};

use super::State;

impl PointerHandler for State {
    /// `wl_pointer` into `egui::Event`. This is the half the M6.1 estimate
    /// forgot: without it the Settings, Gravar, Ata and mode buttons are
    /// decoration.
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            let pos = egui::pos2(e.position.0 as f32, e.position.1 as f32);
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.cursor = Some(pos);
                    self.events.push(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.cursor = None;
                    self.events.push(egui::Event::PointerGone);
                }
                PointerEventKind::Press { button, .. }
                | PointerEventKind::Release { button, .. } => {
                    let pressed = matches!(e.kind, PointerEventKind::Press { .. });
                    // Linux input codes; egui has no name for the extra ones.
                    let Some(b) = (match button {
                        0x110 => Some(egui::PointerButton::Primary),
                        0x111 => Some(egui::PointerButton::Secondary),
                        0x112 => Some(egui::PointerButton::Middle),
                        _ => None,
                    }) else {
                        continue;
                    };
                    self.events.push(egui::Event::PointerButton {
                        pos,
                        button: b,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    });
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    self.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(-horizontal.absolute as f32, -vertical.absolute as f32),
                        // wl_pointer says nothing about kinetic scrolling here;
                        // reporting Move keeps egui from waiting for an end event
                        // that never arrives.
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            match self.seat.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => warn!(error = %e, "no pointer; the overlay's buttons will not respond"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}
