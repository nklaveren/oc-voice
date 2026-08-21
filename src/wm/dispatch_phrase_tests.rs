//! Phrases a person actually says, per language.
//!
//! Every other test here is generated from the vocabulary, which means none of
//! them can notice something the vocabulary does not contain. That is how
//! English went without a way to say "go to workspace 3" while the snapshot
//! stayed green and the action-parity check passed: `workspace` *was* reachable
//! in English, by exactly one phrasing nobody says out loud.
//!
//! So this list is written by hand and on purpose. A phrase that stops
//! dispatching is a regression; a phrase that never dispatched is a gap, and
//! both fail the same way here.

use super::*;

/// What was said, and the hyprctl call it has to produce against `CLIENTS`.
const SPOKEN: &[(&str, &str, &[&str])] = &[
    // Workspaces, both directions, both languages.
    ("pt", "vai pro workspace 3", &["dispatch", "workspace", "3"]),
    ("pt", "área de trabalho 3", &["dispatch", "workspace", "3"]),
    ("pt", "manda pra 3", &["dispatch", "movetoworkspace", "3"]),
    ("en", "workspace 3", &["dispatch", "workspace", "3"]),
    ("en", "go to workspace 3", &["dispatch", "workspace", "3"]),
    (
        "en",
        "switch to workspace 3",
        &["dispatch", "workspace", "3"],
    ),
    (
        "en",
        "move to workspace 3",
        &["dispatch", "movetoworkspace", "3"],
    ),
    // The named window, which is the whole point of the two-slot template:
    // moving the thing you are looking at is the easy half.
    (
        "pt",
        "manda o brave pro 3",
        &["dispatch", "movetoworkspace", "3,address:0xb1"],
    ),
    (
        "en",
        "send brave to workspace 3",
        &["dispatch", "movetoworkspace", "3,address:0xb1"],
    ),
    (
        "en",
        "move brave to workspace 3",
        &["dispatch", "movetoworkspace", "3,address:0xb1"],
    ),
    // Reaching a window. "open" existed in Portuguese only.
    (
        "pt",
        "abre o brave",
        &["dispatch", "focuswindow", "address:0xb1"],
    ),
    (
        "en",
        "open brave",
        &["dispatch", "focuswindow", "address:0xb1"],
    ),
    (
        "en",
        "go to brave",
        &["dispatch", "focuswindow", "address:0xb1"],
    ),
    // Whole-utterance commands, which take a different path entirely.
    ("pt", "tela cheia", &["dispatch", "fullscreen"]),
    ("en", "full screen", &["dispatch", "fullscreen"]),
    ("en", "next window", &["dispatch", "cyclenext"]),
];

#[test]
fn every_phrase_a_person_says_still_dispatches() {
    for (lang, spoken, expected) in SPOKEN {
        let (calls, _) = say_in(lang, spoken, CLIENTS);
        assert_eq!(
            calls.len(),
            1,
            "{lang}: \"{spoken}\" dispatched {calls:?}, wanted exactly one call"
        );
        assert_eq!(
            calls[0], *expected,
            "{lang}: \"{spoken}\" dispatched the wrong thing"
        );
    }
}

#[test]
fn the_longer_phrasing_does_not_steal_the_shorter_one() {
    // "go to workspace {numero}" and "go to {alvo}" overlap: the second has a
    // trailing open slot that will swallow "workspace 3" if it gets the
    // chance. It must not, and "go to brave" must still reach the window —
    // the exact-length pass exists for exactly this pair.
    let (calls, _) = say_in("en", "go to workspace 3", CLIENTS);
    assert_eq!(calls[0], ["dispatch", "workspace", "3"]);
    let (calls, _) = say_in("en", "go to brave", CLIENTS);
    assert_eq!(calls[0], ["dispatch", "focuswindow", "address:0xb1"]);
}
