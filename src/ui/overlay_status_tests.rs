//! Tests for the status strip.

use super::*;
use std::time::{Duration, Instant};

#[test]
fn a_room_full_of_refusals_is_one_line() {
    // The bug this replaces: a video playing near the microphone pushed a
    // banner into the transcript per utterance, five deep in the screenshot
    // that reported it. However many are refused, the answer is one line.
    let now = Instant::now();
    let one = ignored_label(Some((0.33, 1, now)), now).expect("shown");
    assert!(one.contains("0.33"), "{one}");
    assert!(
        !one.contains('1'),
        "a single refusal does not need a count: {one}"
    );
    let many = ignored_label(Some((0.29, 57, now)), now).expect("shown");
    assert!(many.contains("57"), "{many}");
    assert_eq!(many.lines().count(), 1);
}

#[test]
fn it_goes_away_when_the_other_voice_does() {
    // Left up forever it becomes furniture, and furniture is not a signal.
    let now = Instant::now();
    let stale = now - IGNORED_SHOWN - Duration::from_millis(1);
    assert!(ignored_label(Some((0.33, 3, stale)), now).is_none());
    assert!(ignored_label(None, now).is_none());
    // Still inside the window, still shown.
    let fresh = now - IGNORED_SHOWN + Duration::from_millis(100);
    assert!(ignored_label(Some((0.33, 3, fresh)), now).is_some());
}
