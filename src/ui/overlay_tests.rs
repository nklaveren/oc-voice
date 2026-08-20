//! Tests for the overlay: event plumbing, mode cycling, and the layout
//! invariant that keeps the control row on screen.

use super::*;

fn app_with_channel() -> (OverlayApp, crossbeam_channel::Sender<TranscriptEvent>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let running = Arc::new(AtomicBool::new(true));
    let settings = Arc::new(Mutex::new(AppSettings {
        language: "pt".to_string(),
        mode: TranscribeMode::Enter,
        detected_language: None,
    }));
    let config = Arc::new(crate::config::Config::embedded());
    (OverlayApp::new(rx, running, settings, config), tx)
}

#[test]
fn the_control_row_always_fits() {
    // What this pins: an unbounded scroll area claimed the whole panel and
    // laid the mode button out past the bottom edge. The window rendered
    // as a black rectangle with no text and no way to switch modes, which
    // is indistinguishable from a crash.
    //
    // Honest limit: this covers the arithmetic, not egui's layout. A real
    // guarantee needs a render harness we don't have.
    for available in [0.0f32, 10.0, 32.0, 100.0, 350.0, 2000.0] {
        let scroll = scroll_height(available, CONTROLS_HEIGHT);
        assert!(scroll >= 0.0, "negative scroll height at {available}");
        assert!(
            scroll + CONTROLS_HEIGHT <= available.max(CONTROLS_HEIGHT),
            "at {available}px the controls are pushed off screen"
        );
    }
}

#[test]
fn mode_button_cycles_through_all_four_modes() {
    // M4.2: the selector must reach every mode and come back.
    let start = TranscribeMode::Input;
    let mut seen = vec![start];
    let mut m = start;
    for _ in 0..3 {
        m = m.next();
        assert!(!seen.contains(&m), "cycle revisited {m:?} early");
        seen.push(m);
    }
    assert_eq!(m.next(), start, "cycle must close after all four");
    assert!(seen.contains(&TranscribeMode::Command));
    assert!(seen.contains(&TranscribeMode::Translate));
}

#[test]
fn a_translation_event_reaches_renderable_state() {
    // This is the test that would have caught shipping M7.2 with the
    // state wired and the drawing missing: the worker translated, the
    // event arrived, the field was set, and nothing rendered it.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final("They should have a parent.".into()))
        .unwrap();
    tx.send(TranscriptEvent::Translated("Eles devem ter um pai.".into()))
        .unwrap();
    app.drain_events();
    assert_eq!(
        app.translated.as_deref(),
        Some("Eles devem ter um pai."),
        "translation must survive into the state the UI draws from"
    );
    // And the original is still there: translation adds, never replaces.
    assert!(app
        .finals
        .iter()
        .any(|l| l.contains("They should have a parent.")));
}

#[test]
fn a_new_utterance_clears_the_previous_translation() {
    // Otherwise a stale Portuguese line sits under a fresh English one.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Translated("antiga".into()))
        .unwrap();
    tx.send(TranscriptEvent::Final("something new".into()))
        .unwrap();
    app.drain_events();
    assert!(app.translated.is_none());
}

#[test]
fn dead_pipeline_sets_failure_state() {
    // Dropping the sender is what a panicking pipeline thread does: the
    // overlay must notice instead of looking normal.
    let (mut app, tx) = app_with_channel();
    drop(tx);
    app.drain_events();
    assert!(app.pipeline_failed);
}

#[test]
fn live_pipeline_does_not_set_failure_state() {
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Partial("hello".to_string()))
        .unwrap();
    app.drain_events();
    assert!(!app.pipeline_failed);
    assert_eq!(app.partial, "hello");
}
