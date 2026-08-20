//! Tests for the pipeline's routing rules — which stream a mode listens to,
//! and what a stream is allowed to do with what it hears.

use super::*;

#[test]
fn system_audio_mode_listens_to_both_sides() {
    // Capturing only the meeting produced a record of everything said except
    // by the person keeping it.
    let sources: Vec<Source> = streams_for(TranscribeMode::Translate)
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(sources.contains(&Source::Mic), "your own voice is missing");
    assert!(sources.contains(&Source::System));
}

#[test]
fn dictation_modes_never_capture_system_audio() {
    // Otherwise the machine's own output gets typed into whatever has focus.
    for mode in [
        TranscribeMode::Input,
        TranscribeMode::Enter,
        TranscribeMode::Command,
    ] {
        let sources: Vec<Source> = streams_for(mode).into_iter().map(|(s, _)| s).collect();
        assert_eq!(sources, vec![Source::Mic], "{mode:?} listens too widely");
    }
}

#[test]
fn your_own_voice_is_never_put_through_whispers_translator() {
    // Whisper's translate task emits English whatever went in. Applied to the
    // microphone it would write English into your own record — the record is
    // supposed to hold what was actually said.
    for (source, translate) in streams_for(TranscribeMode::Translate) {
        if source == Source::Mic {
            assert!(!translate, "the mic must be transcribed, not translated");
        }
    }
}

#[test]
fn only_the_microphone_may_act_on_what_it_hears() {
    // A meeting that happens to say the stop word must not close your
    // recording, and it must never reach the window manager or the keyboard.
    assert!(Source::Mic.may_command());
    assert!(!Source::System.may_command());
}

#[test]
fn the_button_and_the_spoken_command_open_the_same_session() {
    // Two entry points, one implementation. The button exists because during
    // a call the spoken form fails in both directions: saying the stop word
    // out loud announces you were recording, and a silent room carries no
    // utterance for the command to ride on.
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut recording = None;

    assert!(start_session(Source::Mic, &mut recording, &tx));
    assert!(recording.is_some());
    // Starting twice must not replace a running session with an empty one.
    assert!(!start_session(Source::Mic, &mut recording, &tx));

    assert!(stop_session(&mut recording, &tx));
    assert!(recording.is_none());
    // And stopping when nothing is recording does nothing at all.
    assert!(!stop_session(&mut recording, &tx));

    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, TranscriptEvent::SessionStarted(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, TranscriptEvent::SessionStopped(_, _)))
            .count(),
        1
    );
}

#[test]
fn a_session_started_by_the_button_is_attributed_to_the_modes_own_voice() {
    assert_eq!(primary_source(TranscribeMode::Translate), Source::System);
    assert_eq!(primary_source(TranscribeMode::Enter), Source::Mic);
    assert_eq!(primary_source(TranscribeMode::Command), Source::Mic);
}

#[test]
fn the_two_streams_do_not_share_a_language_lock() {
    // The failure this prevents: an English meeting pins `en`, and the next
    // Portuguese utterance from the mic is handed to whisper as English.
    let mut meeting = asr::LanguageLock::default();
    let mut mic = asr::LanguageLock::default();
    for _ in 0..3 {
        meeting.observe("en");
    }
    for _ in 0..3 {
        mic.observe("pt");
    }
    assert_eq!(meeting.locked(), Some("en"));
    assert_eq!(mic.locked(), Some("pt"));
}
