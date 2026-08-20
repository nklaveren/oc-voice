//! Tests for the speaker-embedding helpers. The model itself is exercised by
//! `oc-voice voices`, which needs the 29 MB file and is not a unit test.

use super::*;

#[test]
fn cosine_is_one_for_a_vector_against_itself() {
    let v = vec![0.3, -0.7, 0.1, 0.9];
    assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
}

#[test]
fn cosine_ignores_loudness_which_is_the_whole_point() {
    // The same person nearer the microphone must not read as a different one.
    let quiet = vec![0.1, 0.2, 0.3];
    let loud: Vec<f32> = quiet.iter().map(|x| x * 8.0).collect();
    assert!((cosine(&quiet, &loud) - 1.0).abs() < 1e-6);
}

#[test]
fn cosine_refuses_rather_than_guesses_on_bad_input() {
    // A zero vector has no direction, and mismatched lengths mean two
    // different models. Both must score 0, never a plausible number.
    assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    assert_eq!(cosine(&[1.0, 2.0], &[1.0, 2.0, 3.0]), 0.0);
    assert_eq!(cosine(&[], &[]), 0.0);
}

#[test]
fn opposite_voices_score_negative_not_zero() {
    // Worth pinning because a threshold applied to the wrong sign convention
    // silently accepts everything.
    let a = vec![1.0, 0.0];
    let b = vec![-1.0, 0.0];
    assert!((cosine(&a, &b) + 1.0).abs() < 1e-6);
}

#[test]
fn a_missing_model_names_the_file_instead_of_failing_obscurely() {
    let Err(err) = SpeakerModel::load(std::path::Path::new("models/does-not-exist.onnx")) else {
        panic!("must not pretend to load a file that is not there");
    };
    assert!(err.to_string().contains("does-not-exist.onnx"));
    assert!(
        err.to_string().contains("M7.4"),
        "points at the plan: {err}"
    );
}
