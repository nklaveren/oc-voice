//! Tests for the overlay: event plumbing, mode cycling, and the layout
//! invariant that keeps the control row on screen.

use super::*;
use crate::Source;

/// Point saved state at a scratch directory before the first app is built.
///
/// `OverlayApp::new` loads the overlay's saved layout, and several paths here
/// write it back. Without this the suite reads — and then overwrites — the
/// position the person running it left their overlay in. It did exactly that
/// for an afternoon, and the symptom reported was "it is not saving where I
/// put it": every `just check` reset it.
fn isolate_state() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("oc-voice-test-{}", std::process::id()));
        std::env::set_var("XDG_STATE_HOME", &dir);
    });
}

fn app_with_channel() -> (OverlayApp, crossbeam_channel::Sender<TranscriptEvent>) {
    isolate_state();
    let (tx, rx) = crossbeam_channel::unbounded();
    let running = Arc::new(AtomicBool::new(true));
    let settings = Arc::new(Mutex::new(AppSettings {
        language: "pt".to_string(),
        mode: TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
    }));
    let config = Arc::new(crate::config::Config::embedded());
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::process::FakeRunner::new(b"[]".to_vec()));
    (OverlayApp::new(rx, running, settings, config, runner), tx)
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
fn the_mode_button_reaches_both_modes_and_comes_back() {
    // M4.2: the selector must reach every mode. There were four; two pairs
    // of them were the same thing, and cycling past the two dead ones cost
    // an utterance each time.
    let start = TranscribeMode::Enter;
    assert_eq!(start.next(), TranscribeMode::Translate);
    assert_eq!(start.next().next(), start, "the toggle must close");
}

/// A meeting utterance. Most overlay tests are about the subtitle path.
fn heard(text: &str) -> TranscriptEvent {
    TranscriptEvent::Final {
        text: text.to_string(),
        source: Source::System,
    }
}

/// Something you said.
fn spoke(text: &str) -> TranscriptEvent {
    TranscriptEvent::Final {
        text: text.to_string(),
        source: Source::Mic,
    }
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
    tx.send(heard("They should have a parent.")).unwrap();
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
    tx.send(heard("first sentence")).unwrap();
    tx.send(translated("first sentence", "primeira frase"))
        .unwrap();
    tx.send(heard("second sentence")).unwrap();
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
    tx.send(heard("slow one")).unwrap();
    tx.send(heard("dropped by the worker")).unwrap();
    tx.send(heard("fast one")).unwrap();
    tx.send(translated("slow one", "a lenta")).unwrap();
    app.drain_events();

    assert_eq!(app.finals[0].translation.as_deref(), Some("a lenta"));
    assert!(app.finals[1].translation.is_none());
    assert!(app.finals[2].translation.is_none());
}

#[test]
fn a_translation_with_no_matching_original_is_dropped_not_misattached() {
    let (mut app, tx) = app_with_channel();
    tx.send(heard("something said")).unwrap();
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
    tx.send(heard(".")).unwrap();
    tx.send(heard(" ... ")).unwrap();
    tx.send(heard("real speech")).unwrap();
    app.drain_events();

    assert_eq!(app.finals.len(), 1);
    assert_eq!(app.finals[0].text, "real speech");
}

#[test]
fn both_speakers_share_one_scrollback_in_the_order_they_spoke() {
    // The point of capturing both: reading the conversation back as a
    // conversation, not as two disconnected halves.
    let (mut app, tx) = app_with_channel();
    tx.send(heard("So what do you think?")).unwrap();
    tx.send(spoke("Acho que faz sentido.")).unwrap();
    tx.send(heard("Good, let's go with that.")).unwrap();
    app.drain_events();

    let order: Vec<Source> = app.finals.iter().map(|l| l.source).collect();
    assert_eq!(order, vec![Source::System, Source::Mic, Source::System]);
    assert_eq!(app.finals[1].text, "Acho que faz sentido.");
}

#[test]
fn one_stream_finalizing_does_not_erase_the_others_partial() {
    // A single partial slot meant that whenever the meeting finished a
    // sentence, your half-spoken one vanished from the screen — and the two
    // are simultaneous exactly when you talk over each other.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::Partial {
        text: "estou dizendo que".to_string(),
        source: Source::Mic,
    })
    .unwrap();
    tx.send(heard("...and that concludes it.")).unwrap();
    app.drain_events();

    assert_eq!(
        app.partial_mic, "estou dizendo que",
        "your in-progress sentence survived the meeting finalizing theirs"
    );
    assert!(app.partial_system.is_empty());
}

#[test]
fn speakers_are_told_apart_by_colour() {
    // Colour is the whole distinction in the overlay; if both sides render
    // identically, capturing both just interleaves them into confusion.
    assert_ne!(
        speaker_color(Source::Mic),
        speaker_color(Source::System),
        "the two sides must not render identically"
    );
}

#[test]
fn every_mode_says_what_it_is_for() {
    // The person who built this forgot what Enter mode was for. Input and
    // Enter look identical while idle — both listen, both are about your own
    // speech — and the difference only shows after you have committed to one.
    let mut seen = std::collections::HashSet::new();
    for mode in [TranscribeMode::Enter, TranscribeMode::Translate] {
        let hint = mode_hint(mode);
        assert!(!hint.is_empty(), "{mode:?} has no description");
        assert!(seen.insert(hint), "{mode:?} reuses another mode's words");
    }
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
fn the_record_is_reachable_from_the_moment_it_starts() {
    // The button needs a path before the first sentence is finished. The
    // pipeline creates the file at "grava" precisely so this is possible.
    let (mut app, tx) = app_with_channel();
    assert!(app.session_path.is_none(), "nothing to open yet");

    tx.send(TranscriptEvent::SessionStarted(
        "/tmp/sessions/2026-08-20_14-00-00.md".into(),
    ))
    .unwrap();
    app.drain_events();
    assert_eq!(
        app.session_path.as_deref(),
        Some("/tmp/sessions/2026-08-20_14-00-00.md")
    );
}

#[test]
fn the_record_stays_reachable_after_the_session_closes() {
    // Reading back what was just recorded is exactly when you want it, so
    // stopping must not disable the button.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::SessionStarted("/tmp/a.md".into()))
        .unwrap();
    tx.send(TranscriptEvent::SessionStopped("/tmp/a.md".into(), 12))
        .unwrap();
    app.drain_events();

    assert!(app.recording.is_none(), "no longer recording");
    assert_eq!(
        app.session_path.as_deref(),
        Some("/tmp/a.md"),
        "but the record is still openable"
    );
}

#[test]
fn a_session_that_failed_to_create_its_file_offers_nothing_to_open() {
    // The pipeline sends an empty path when the write failed. A button
    // pointing at nothing is worse than a disabled one.
    let (mut app, tx) = app_with_channel();
    tx.send(TranscriptEvent::SessionStarted(String::new()))
        .unwrap();
    app.drain_events();

    assert!(app.recording.is_some(), "recording still started");
    assert!(app.session_path.is_none(), "with nothing to open");
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
    tx.send(TranscriptEvent::Partial {
        text: "hello".to_string(),
        source: Source::System,
    })
    .unwrap();
    app.drain_events();
    assert!(!app.pipeline_failed);
    assert_eq!(app.partial_system, "hello");
}

#[path = "overlay_interaction_tests.rs"]
mod interaction;
