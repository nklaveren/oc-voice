//! Tests for transcription: the language lock, the hallucination filter,
//! and the segmenter — including the pre-roll that keeps a short command
//! from losing its own first syllable.

use super::{LanguageLock, SpeechSegment};

/// A 512-sample VAD frame, filled with a recognisable value so the test
/// can tell which frames survived into the segment.
fn frame(value: f32) -> Vec<f32> {
    vec![value; crate::VAD_FRAME_SAMPLES]
}

#[test]
fn the_onset_of_a_word_survives_the_vads_hesitation() {
    // The bug this pins, reported from live use: "os comandos não são
    // capturados, somente falas grandes". Silero needs energy to build
    // before it crosses the threshold, so the first frames of a word read
    // as silence and used to be discarded. A long sentence loses a
    // syllable and whisper recovers from context; "envia" is about 400 ms
    // end to end, so losing the onset loses the command.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(300);

    // Three frames of the word that the VAD has not recognised yet.
    for _ in 0..3 {
        seg.push_frame(&frame(0.5), false);
    }
    assert!(seg.samples.is_empty(), "nothing committed before speech");

    // Now it notices.
    seg.push_frame(&frame(0.9), true);

    assert_eq!(
        seg.samples.len(),
        crate::VAD_FRAME_SAMPLES * 4,
        "the three hesitating frames must arrive with the fourth"
    );
    assert_eq!(seg.samples[0], 0.5, "and they must come first, in order");
    assert_eq!(seg.samples[crate::VAD_FRAME_SAMPLES * 3], 0.9);
}

#[test]
fn the_preroll_never_grows_past_its_window() {
    // It runs for the whole idle period between utterances; unbounded, a
    // quiet hour would be held in memory.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(64); // two frames' worth
    for _ in 0..500 {
        seg.push_frame(&frame(0.1), false);
    }
    seg.push_frame(&frame(0.9), true);
    assert_eq!(
        seg.samples.len(),
        crate::VAD_FRAME_SAMPLES * 3,
        "two frames of pre-roll plus the speech frame, not 500"
    );
}

#[test]
fn a_second_command_in_a_row_keeps_its_own_onset() {
    // reset() deliberately does not clear the pre-roll: the tail of one
    // utterance is the run-up to the next, and "envia. câmbio." would
    // otherwise lose the second word exactly like the first bug.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(300);
    seg.push_frame(&frame(0.9), true);
    seg.push_frame(&frame(0.0), false);
    seg.reset();

    seg.push_frame(&frame(0.4), false);
    seg.push_frame(&frame(0.9), true);
    assert_eq!(seg.samples.len(), crate::VAD_FRAME_SAMPLES * 2);
    assert_eq!(seg.samples[0], 0.4, "the second onset was kept too");
}

#[test]
fn zero_preroll_behaves_exactly_as_before() {
    // Someone who sets preroll_ms = 0 gets the old behaviour rather than
    // a panic or a surprise.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(0);
    seg.push_frame(&frame(0.5), false);
    seg.push_frame(&frame(0.9), true);
    assert_eq!(seg.samples.len(), crate::VAD_FRAME_SAMPLES);
    assert_eq!(seg.samples[0], 0.9);
}

#[test]
fn a_single_odd_detection_never_pins_a_language() {
    // The live failure: a run of `en` with one `es` at p=0.24 in the
    // middle, which came out as "¿Qué?" in an English meeting.
    let mut lock = LanguageLock::default();
    assert_eq!(lock.observe("en"), None);
    assert_eq!(lock.observe("en"), None);
    assert_eq!(lock.observe("en"), Some("en"), "three agreeing pins it");
    // The stray reading resets the streak but must not pin anything.
    assert_eq!(lock.observe("es"), None);
    assert_eq!(lock.observe("en"), None);
    assert_eq!(lock.observe("en"), None);
    assert_eq!(lock.observe("en"), Some("en"));
}

#[test]
fn a_genuine_language_change_still_settles() {
    // Someone switching to Spanish for the rest of the call must be
    // followed, just not on the first utterance.
    let mut lock = LanguageLock::default();
    for _ in 0..3 {
        lock.observe("en");
    }
    assert_eq!(lock.observe("es"), None);
    assert_eq!(lock.observe("es"), None);
    assert_eq!(lock.observe("es"), Some("es"));
}

#[test]
fn a_language_nobody_speaks_here_is_noise_not_disagreement() {
    // From a live log: a Portuguese sentence was detected as German at
    // p = 0.198 and came back as "Weil Brot sagt…". Whisper will name any of
    // its hundred languages on thin evidence, and three seconds of speech is
    // thin evidence.
    let mut lock = LanguageLock::restricted_to(vec!["pt".into(), "en".into()]);
    assert_eq!(lock.observe("de"), None);
    assert_eq!(lock.locked(), None, "German must never pin");

    // And the stray reading must not reset a real run, or a single bad
    // segment costs three good ones.
    assert_eq!(lock.observe("pt"), None);
    assert_eq!(lock.observe("pt"), None);
    assert_eq!(lock.observe("de"), None);
    assert_eq!(lock.observe("pt"), Some("pt"), "the run survived the noise");
}

#[test]
fn observing_survives_the_stream_that_never_pins() {
    // The failure this halves: `pt` settles legitimately after three
    // Portuguese turns, and on the microphone every English utterance
    // afterwards was decoded as Portuguese, because the settled code became
    // whisper's source hint. The fix takes the hint away from that stream —
    // and the lock has to keep settling anyway, because the settled code is
    // what picks the command vocabulary.
    //
    // Which stream may pin is the pipeline's decision, asserted in
    // `only_the_meeting_pins_its_language_onto_the_decoder`.
    let mut lock = LanguageLock::restricted_to(vec!["pt".into(), "en".into()]);
    for _ in 0..3 {
        lock.observe("pt");
    }
    assert_eq!(lock.locked(), Some("pt"), "observation still settles");
}

#[test]
fn an_empty_list_still_accepts_anything() {
    // Someone who really does speak German should not have to discover a
    // config key to be understood.
    let mut lock = LanguageLock::restricted_to(Vec::new());
    for _ in 0..3 {
        lock.observe("de");
    }
    assert_eq!(lock.locked(), Some("de"));
}
