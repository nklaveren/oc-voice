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

fn translated(original: &str, text: &str) -> TranscriptEvent {
    TranscriptEvent::Translated {
        original: original.to_string(),
        text: text.to_string(),
    }
}

#[test]
fn a_translation_event_reaches_renderable_state() {
    // This is the test that would have caught shipping M7.2 with the
    // state wired and the drawing missing: the worker translated, the
    // event arrived, the field was set, and nothing rendered it.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final("They should have a parent.".into()))
        .unwrap();
    tx.send(translated(
        "They should have a parent.",
        "Eles devem ter um pai.",
    ))
    .unwrap();
    app.drain_events();

    let line = app.finals.last().expect("the original became a line");
    assert_eq!(line.text, "They should have a parent.");
    assert_eq!(
        line.translation.as_deref(),
        Some("Eles devem ter um pai."),
        "translation must survive into the state the UI draws from"
    );
}

#[test]
fn old_translations_stay_in_the_scrollback() {
    // The reason to scroll back through a meeting is usually to re-read the
    // part you did not follow. Keeping only the newest translation and
    // clearing it on every utterance stripped exactly that away.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final("first sentence".into()))
        .unwrap();
    tx.send(translated("first sentence", "primeira frase"))
        .unwrap();
    tx.send(TranscriptEvent::Final("second sentence".into()))
        .unwrap();
    tx.send(translated("second sentence", "segunda frase"))
        .unwrap();
    app.drain_events();

    assert_eq!(app.finals.len(), 2);
    assert_eq!(app.finals[0].translation.as_deref(), Some("primeira frase"));
    assert_eq!(app.finals[1].translation.as_deref(), Some("segunda frase"));
}

#[test]
fn a_translation_attaches_to_its_own_original_not_the_newest_line() {
    // Translation runs async in a worker and drops requests when it falls
    // behind, so by the time one arrives the newest line is regularly a
    // different utterance. Pairing by position would caption the wrong one.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final("slow one".into())).unwrap();
    tx.send(TranscriptEvent::Final("dropped by the worker".into()))
        .unwrap();
    tx.send(TranscriptEvent::Final("fast one".into())).unwrap();
    tx.send(translated("slow one", "a lenta")).unwrap();
    app.drain_events();

    assert_eq!(app.finals[0].translation.as_deref(), Some("a lenta"));
    assert!(app.finals[1].translation.is_none());
    assert!(app.finals[2].translation.is_none());
}

#[test]
fn a_translation_with_no_matching_original_is_dropped_not_misattached() {
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final("something said".into()))
        .unwrap();
    tx.send(translated("never said", "nunca dito")).unwrap();
    app.drain_events();

    assert_eq!(app.finals.len(), 1);
    assert!(
        app.finals[0].translation.is_none(),
        "an unmatched translation must not caption an unrelated line"
    );
}

#[test]
fn bare_punctuation_never_becomes_a_line() {
    // Short segments make whisper emit a lone ".", which filled the meeting
    // overlay with blank-looking rows between real speech.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Final(".".into())).unwrap();
    tx.send(TranscriptEvent::Final(" ... ".into())).unwrap();
    tx.send(TranscriptEvent::Final("real speech".into()))
        .unwrap();
    app.drain_events();

    assert_eq!(app.finals.len(), 1);
    assert_eq!(app.finals[0].text, "real speech");
}

#[test]
fn markers_drawn_next_to_speech_are_ascii() {
    // The bundled font has no ↳; every translated line carried an empty box
    // where the marker should be. A missing glyph is not something the
    // overlay can detect and recover from at runtime, so it does not gamble.
    assert!(
        TRANSLATION_MARKER.is_ascii(),
        "{TRANSLATION_MARKER:?} risks rendering as tofu"
    );
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
