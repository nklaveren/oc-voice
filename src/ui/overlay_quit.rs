//! The one control that ends the session, and the step in front of it.
//!
//! A layer surface has nowhere to put a dialog: anything drawn past the
//! strip's edge does not exist, which is the same reason Settings is a mode
//! rather than a window. So the confirmation is this row, showing a question
//! instead of a button for a few seconds.
//!
//! Cancel takes the X's own position. A double click is the likeliest way to
//! arrive here by accident, and it lands on "Não" — the second press of a
//! double click cannot be the one that confirms.

use super::draw::{style_controls, DANGER, INK};
use super::*;

/// How long the question stays up. Long enough to read, short enough that an
/// armed close never survives to meet the next hand on the mouse.
const ARM: std::time::Duration = std::time::Duration::from_secs(4);

impl OverlayApp {
    /// Draw the close control, in whichever of its two states it is in.
    ///
    /// Expects a right-to-left `ui`: the first widget added is the rightmost,
    /// and that ordering is what puts cancel where the X was.
    pub(super) fn quit_control(&mut self, ui: &mut egui::Ui) {
        {
            style_controls(ui);
            if self.quit_armed.is_none_or(|t| t.elapsed() >= ARM) {
                self.quit_armed = None;
                // ASCII, for the third time in this file's neighbourhood:
                // U+2715 drew an empty box, exactly as the record dot and the
                // translation arrow did. Red ink rather than a red fill,
                // unlike `Parar` — that one reports a *state* nobody should
                // have to read twice, this is an action, and a fixed fill is
                // the one thing that cannot show it is under the pointer.
                let quit = egui::Button::new(egui::RichText::new(" X ").color(DANGER).strong());
                if ui.add(quit).on_hover_text("encerrar").clicked() {
                    self.quit_armed = Some(std::time::Instant::now());
                }
                return;
            }

            // Right to left: the first widget added is the rightmost, so this
            // is what inherits the X's place.
            if ui.button("Não").clicked() {
                self.quit_armed = None;
            }
            let yes =
                egui::Button::new(egui::RichText::new("Sim").color(INK).strong()).fill(DANGER);
            if ui.add(yes).clicked() {
                // The same shutdown the pipeline asks for when it stops.
                self.running.store(false, Ordering::SeqCst);
            }
            ui.label(
                egui::RichText::new("encerrar?")
                    .color(DANGER)
                    .strong()
                    .size(14.0),
            );
            // The question has to time out on its own. Without this the
            // countdown only advances while something else asks for frames.
            ui.ctx().request_repaint();
        }
    }
}
