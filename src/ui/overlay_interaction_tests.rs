//! Interaction tests: what a press actually lands on.
//!
//! Split from `overlay_tests.rs` at the size ceiling, along the line that was
//! already there — those tests turn events into state without ever laying
//! anything out, and these run egui for real to ask where the pointer went.

use super::settings_panel::{MIN_H, MIN_W};
use super::*;

/// The overlay driven through egui with no window and no GL.
///
/// The layer host's `draw` is two things: this, and a buffer to put the
/// result in. Only the first half decides whether a press lands on a button,
/// so only the first half is needed to test it — and that half was the one no
/// test could reach, which is how a panel-wide drag handle came to swallow
/// every control on the strip without anything going red.
struct Harness {
    ctx: egui::Context,
    size: egui::Vec2,
}

impl Harness {
    fn new(app: &mut OverlayApp, w: f32, h: f32) -> Self {
        // The saved layout is the user's, loaded from disk by `new`. Pin it,
        // or the assertions below depend on where somebody last dragged the
        // overlay.
        app.layout = Layout {
            width: w,
            height: h,
            bottom_margin: 60.0,
            x_offset: 0.0,
            opacity: OVERLAY_OPACITY,
        };
        let h = Harness {
            ctx: egui::Context::default(),
            size: egui::vec2(w, h),
        };
        // One pass with no input: egui hit-tests a press against the widgets
        // the *previous* pass registered, so there has to be one.
        h.pass(app, Vec::new());
        h
    }

    fn pass(&self, app: &mut OverlayApp, events: Vec<egui::Event>) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            events,
            ..Default::default()
        };
        let _ = self.ctx.run_ui(raw, |ui| app.paint(ui));
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// Press and release without moving — a click, as egui defines one.
    fn click(&self, app: &mut OverlayApp, pos: egui::Pos2) {
        self.pass(app, vec![egui::Event::PointerMoved(pos)]);
        self.pass(app, vec![Self::button(pos, true)]);
        self.pass(app, vec![Self::button(pos, false)]);
    }
}

#[test]
fn a_press_on_the_control_row_reaches_a_button() {
    // What this pins, and it is the whole reason the harness exists: the
    // panel-wide drag handle was registered *after* the contents, and egui
    // resolves a press to the last widget under the pointer — a drag-only
    // widget on top does not let the click through, it deletes it
    // (`hit_test_on_close`: "the top thing senses only drags, so we ignore
    // the click-widget"). Every button on the strip went dead and none of
    // them would even highlight, which from the outside looks like a mouse
    // that cannot aim.
    let (mut app, _tx) = app_with_channel();
    let h = Harness::new(&mut app, 900.0, 350.0);

    // Sweep the bottom strip rather than naming a coordinate: where egui puts
    // the row is egui's business, and a test that hard-codes it breaks on a
    // font change instead of on a regression.
    let mut hits = 0;
    for x in (20..240).step_by(4) {
        for y in (300..345).step_by(3) {
            app.show_settings = false;
            h.click(&mut app, egui::pos2(x as f32, y as f32));
            if app.show_settings {
                hits += 1;
            }
        }
    }
    // A number, because the failure is quantitative rather than absolute.
    // Measured on this sweep: with the handle registered last, 71 of these
    // presses reached the button and the rest vanished into it — one press in
    // three, which is what "I have to hunt for the buttons" is made of. With
    // it registered first, 251 do: the button's own area plus the few points
    // of aim assist egui adds around a small widget on a big background.
    // Anything near the low number is the same bug back.
    assert!(
        hits > 150,
        "only {hits} presses over the control row reached the button"
    );
}

#[test]
fn dragging_empty_panel_still_moves_the_overlay() {
    // The fix above must not cost the gesture it broke: the handle is still
    // there, just underneath, so panel space that is not a widget drags.
    let (mut app, _tx) = app_with_channel();
    let h = Harness::new(&mut app, 900.0, 350.0);

    let start = egui::pos2(450.0, 150.0);
    h.pass(&mut app, vec![egui::Event::PointerMoved(start)]);
    h.pass(&mut app, vec![Harness::button(start, true)]);
    let moved = start + egui::vec2(0.0, -40.0);
    h.pass(&mut app, vec![egui::Event::PointerMoved(moved)]);

    let request = app
        .layout_request
        .take()
        .expect("dragging the panel asked for nothing");
    assert!(
        request.bottom_margin.is_some_and(|m| m > 60.0),
        "dragging up must raise the overlay, got {:?}",
        request.bottom_margin
    );
}

#[test]
fn closing_asks_first_and_never_answers_itself() {
    // Both ends of the range the sliders allow. The narrow one matters: the
    // question is three widgets where the button was one, and a confirmation
    // that lays itself out past the edge of a layer surface does not exist.
    closing_asks_first(900.0, 350.0);
    closing_asks_first(MIN_W, MIN_H);
}

fn closing_asks_first(w: f32, h: f32) {
    // Three properties, and the third is the one that needed a layout
    // decision rather than a check: no single press anywhere ends the
    // session; the question can be answered yes; and pressing the *same
    // place* twice does not, because a double click is the likeliest way to
    // reach the question by accident and cancel is what sits where the X was.
    let (mut app, _tx) = app_with_channel();
    let harness = Harness::new(&mut app, w, h);

    // The row's right-hand end, without naming where egui put the button.
    let (right, bottom) = (w as i32, h as i32);
    let row: Vec<egui::Pos2> = ((right - 110).max(0)..right)
        .step_by(4)
        .flat_map(|x| {
            ((bottom - 50).max(0)..bottom)
                .step_by(4)
                .map(move |y| egui::pos2(x as f32, y as f32))
        })
        .collect();

    let mut armed_at = None;
    for pos in &row {
        app.quit_armed = None;
        app.running.store(true, Ordering::SeqCst);
        harness.click(&mut app, *pos);
        assert!(
            app.running.load(Ordering::SeqCst),
            "one press at {pos:?} ended the session with no question"
        );
        if app.quit_armed.is_some() && armed_at.is_none() {
            armed_at = Some(*pos);
        }
    }
    let armed_at =
        armed_at.unwrap_or_else(|| panic!("nothing on the strip arms the close at {w}x{h}"));

    // The same press again: the answer under it must be the harmless one.
    app.quit_armed = None;
    app.running.store(true, Ordering::SeqCst);
    harness.click(&mut app, armed_at);
    harness.click(&mut app, armed_at);
    assert!(
        app.running.load(Ordering::SeqCst),
        "pressing {armed_at:?} twice closed without ever showing a question"
    );

    // And the question is answerable: somewhere in the row, yes exists.
    let mut confirmed = false;
    for pos in &row {
        app.quit_armed = Some(std::time::Instant::now());
        app.running.store(true, Ordering::SeqCst);
        harness.click(&mut app, armed_at);
        app.quit_armed = Some(std::time::Instant::now());
        harness.click(&mut app, *pos);
        if !app.running.load(Ordering::SeqCst) {
            confirmed = true;
            break;
        }
    }
    assert!(confirmed, "the close can be armed but never confirmed");
}
