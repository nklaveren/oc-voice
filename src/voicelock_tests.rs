//! Tests for the voice lock.
//!
//! What can be tested without a microphone is the arithmetic and the policy:
//! how the bar is derived from your own samples, and what happens to an
//! utterance too short to judge. The part that needs real voices — whether an
//! impostor scores below the bar — is exactly the part no unit test can claim,
//! and `build`'s own documentation says so rather than pretending otherwise.

use super::*;

/// Embeddings that are close to each other, plus a knob to move one away.
fn cluster(n: usize, spread: f32) -> Vec<Vec<f32>> {
    (0..n)
        .map(|i| {
            let drift = i as f32 * spread;
            let mut v = vec![1.0, 0.5, 0.25, 0.1];
            v[1] += drift;
            v[2] -= drift;
            v
        })
        .collect()
}

#[test]
fn one_sample_is_not_a_voice() {
    // A centroid from a single segment describes that segment, not a person,
    // and there is nothing to measure a bar against.
    assert!(build(&cluster(1, 0.0)).is_none());
    assert!(build(&[]).is_none());
}

#[test]
fn the_bar_comes_from_how_much_you_disagree_with_yourself() {
    // Tight cluster, high bar. Loose cluster, lower bar — the same person on
    // a bad microphone should not be locked out by their own enrolment.
    let tight = build(&cluster(6, 0.005)).expect("six segments");
    let loose = build(&cluster(6, 0.08)).expect("six segments");
    assert!(
        tight.threshold > loose.threshold,
        "tight {} should sit above loose {}",
        tight.threshold,
        loose.threshold
    );
    assert!(tight.self_worst <= tight.self_mean);
    assert_eq!(tight.segments, 6);
}

#[test]
fn the_bar_sits_below_your_own_worst_sample() {
    // Otherwise enrolment produces a lock that rejects the very recording it
    // was built from, which is the one case that is certainly wrong.
    let s = build(&cluster(8, 0.02)).expect("eight segments");
    assert!(
        s.threshold < s.self_worst,
        "threshold {} must leave room under the worst sample {}",
        s.threshold,
        s.self_worst
    );
    assert!((0.0..=1.0).contains(&s.threshold));
}

#[test]
fn leaving_the_sample_out_is_what_makes_the_number_honest() {
    // Scoring a sample against a centroid that contains it flatters it. With
    // a handful of samples that bias is most of the number, and the bar would
    // come out too high — this pins that `build` measures the harder way.
    let embeddings = cluster(4, 0.05);
    let s = build(&embeddings).expect("four segments");
    let included = mean(&embeddings).expect("centroid");
    let flattered = embeddings
        .iter()
        .map(|e| cosine(e, &included))
        .fold(f32::MAX, f32::min);
    assert!(
        s.self_worst < flattered,
        "leave-one-out {} should be stricter than including the sample {}",
        s.self_worst,
        flattered
    );
}

#[test]
fn a_short_utterance_is_never_rejected() {
    // The deliberate hole, and the reason it exists: `sim`, `não` and the
    // send word arrive shorter than a second, and they are the commands used
    // most. A lock that swallowed them would break the app to protect it.
    let mut lock = VoiceLock::new(std::path::PathBuf::from("/nonexistent-model.onnx"));
    lock.store = build(&cluster(5, 0.01));
    assert!(matches!(lock.state(), crate::VoiceState::On { .. }));
    let short = vec![0.0f32; (fbank::SAMPLE_RATE * 0.4) as usize];
    assert_eq!(lock.offer(&short), Verdict::Pass);
}

#[test]
fn with_no_voice_stored_nothing_is_filtered() {
    let mut lock = VoiceLock::new(std::path::PathBuf::from("/nonexistent-model.onnx"));
    assert_eq!(lock.state(), crate::VoiceState::Off);
    let long = vec![0.0f32; (fbank::SAMPLE_RATE * 3.0) as usize];
    assert_eq!(lock.offer(&long), Verdict::Pass);
}

#[test]
fn a_missing_model_lets_speech_through_rather_than_blocking_it() {
    // The model file can be absent on a fresh checkout. Failing closed would
    // make the app look broken; failing open loses the filter and says so in
    // the log, which is the lesser harm for a feature that is an assistant.
    let mut lock = VoiceLock::new(std::path::PathBuf::from("/nonexistent-model.onnx"));
    lock.store = build(&cluster(5, 0.01));
    let long = vec![0.1f32; (fbank::SAMPLE_RATE * 2.0) as usize];
    assert_eq!(lock.offer(&long), Verdict::Pass);
}
