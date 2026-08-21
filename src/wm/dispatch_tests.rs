//! Tests for spoken WM dispatch (M3.1–M3.3), including the live
//! verification suite (`--ignored`).

use super::*;

use crate::process::FakeRunner;

fn setup(fixture: &[u8]) -> (Arc<FakeRunner>, Arc<dyn CommandRunner>, Config) {
    let fake = Arc::new(FakeRunner::new(fixture.to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    (fake, runner, crate::config::Config::embedded())
}

fn dispatched(fake: &FakeRunner) -> Vec<Vec<String>> {
    fake.calls()
        .iter()
        .filter(|(p, a)| p == "hyprctl" && a.first().map(String::as_str) == Some("dispatch"))
        .map(|(_, a)| a.clone())
        .collect()
}

fn say(spoken: &str, fixture: &[u8]) -> (Vec<Vec<String>>, Option<PendingAction>) {
    say_in("pt", spoken, fixture)
}

fn say_in(lang: &str, spoken: &str, fixture: &[u8]) -> (Vec<Vec<String>>, Option<PendingAction>) {
    let (fake, runner, config) = setup(fixture);
    let vocab = config.vocab(lang).unwrap().clone();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut pending = None;
    dispatch_spoken(&vocab, &config, spoken, &runner, &tx, &mut pending);
    (dispatched(&fake), pending)
}

const MONITORS: &[u8] = br#"[
        {"name": "AAA-1", "description": "BOE 0x0A88", "x": 5000},
        {"name": "BBB-1", "description": "Samsung Electric Company Odyssey G30B", "x": 0},
        {"name": "CCC-1", "description": "LG Electronics LG ULTRAWIDE", "x": 2000}
    ]"#;
const CLIENTS: &[u8] = br#"[{"class":"brave-browser","title":"docs - Brave","address":"0xb1"}]"#;

#[test]
fn whole_utterance_commands_dispatch() {
    assert_eq!(say("tela cheia", b"[]").0, [["dispatch", "fullscreen"]]);
    assert_eq!(say("flutuante", b"[]").0, [["dispatch", "togglefloating"]]);
}

#[test]
fn direction_templates_dispatch_movefocus() {
    assert_eq!(
        say("janela da esquerda", b"[]").0,
        [["dispatch", "movefocus", "l"]]
    );
    assert_eq!(
        say("janela de cima", b"[]").0,
        [["dispatch", "movefocus", "u"]]
    );
}

#[test]
fn spoken_and_digit_numbers_dispatch_identically() {
    // M3.2 acceptance.
    assert_eq!(
        say("área de trabalho quatro", b"[]").0,
        [["dispatch", "workspace", "4"]]
    );
    assert_eq!(
        say("área de trabalho 4", b"[]").0,
        [["dispatch", "workspace", "4"]]
    );
    assert_eq!(
        say("leva pra três", b"[]").0,
        [["dispatch", "movetoworkspace", "3"]]
    );
    for (word, n) in [("um", 1), ("dois", 2), ("cinco", 5), ("dez", 10)] {
        assert_eq!(
            say(&format!("área de trabalho {word}"), b"[]").0,
            [vec![
                "dispatch".to_string(),
                "workspace".to_string(),
                n.to_string()
            ]]
        );
    }
}

#[test]
fn monitor_position_follows_the_x_layout() {
    // M3.1 acceptance: positions swapped relative to any real machine —
    // "direita" must be the highest x, not any hardcoded name.
    assert_eq!(
        say("monitor da direita", MONITORS).0,
        [["dispatch", "focusmonitor", "AAA-1"]]
    );
    assert_eq!(
        say("monitor da esquerda", MONITORS).0,
        [["dispatch", "focusmonitor", "BBB-1"]]
    );
    assert_eq!(
        say("monitor do meio", MONITORS).0,
        [["dispatch", "focusmonitor", "CCC-1"]]
    );
}

#[test]
fn monitor_position_works_with_and_without_preposition() {
    // "monitor esquerda" and "monitor da esquerda" are the same command;
    // "centro" and "meio" are synonyms for the middle monitor.
    for spoken in ["monitor esquerda", "monitor da esquerda"] {
        assert_eq!(
            say(spoken, MONITORS).0,
            [["dispatch", "focusmonitor", "BBB-1"]],
            "{spoken}"
        );
    }
    for spoken in ["monitor meio", "monitor centro", "monitor do centro"] {
        assert_eq!(
            say(spoken, MONITORS).0,
            [["dispatch", "focusmonitor", "CCC-1"]],
            "{spoken}"
        );
    }
    // A bare direction word alone must not switch monitors.
    assert!(say("centro", MONITORS).0.is_empty());
    // And the closed slot outranks the {monitor} wildcard on ties:
    // "esquerda" is a direction, not a monitor named "esquerda".
    assert_eq!(
        say("monitor direita", MONITORS).0,
        [["dispatch", "focusmonitor", "AAA-1"]]
    );
}

#[test]
fn center_is_not_a_window_focus_direction() {
    // hyprctl movefocus only takes l/r/u/d; "janela do centro" must
    // dispatch nothing rather than an invalid direction.
    assert!(say("janela do centro", MONITORS).0.is_empty());
}

#[test]
fn monitor_by_brand_matches_the_description() {
    assert_eq!(
        say("monitor samsung", MONITORS).0,
        [["dispatch", "focusmonitor", "BBB-1"]]
    );
    // Short brands (LG) match exactly instead of being dropped as noise.
    assert_eq!(
        say("monitor lg", MONITORS).0,
        [["dispatch", "focusmonitor", "CCC-1"]]
    );
    assert!(say("monitor dell", MONITORS).0.is_empty());
}

#[test]
fn focus_window_goes_through_the_target_resolver() {
    assert_eq!(
        say("foca o brave", CLIENTS).0,
        [["dispatch", "focuswindow", "address:0xb1"]]
    );
}

#[test]
fn kill_active_waits_for_confirmation() {
    // M3.3: "fecha" alone closes nothing.
    let (calls, pending) = say("fecha", b"[]");
    assert!(calls.is_empty(), "destructive action must not dispatch");
    assert!(matches!(pending, Some(PendingAction::Dispatch { .. })));
}

#[test]
fn unrecognized_speech_dispatches_nothing() {
    assert!(say("bom dia pessoal", b"[]").0.is_empty());
}

#[test]
fn closing_by_name_closes_that_window_and_not_the_focused_one() {
    // Reported live: "Fechar Teams." asked to confirm `kill_active`, and the
    // "Sim." that followed closed whatever was in focus. The named window was
    // never consulted — though the resolver could see it.
    let (calls, pending) = say("fechar brave", CLIENTS);
    assert!(calls.is_empty(), "closing still waits for a yes");
    let Some(PendingAction::Dispatch { action, .. }) = pending else {
        panic!("naming a window to close must arm a confirmation");
    };
    assert_eq!(
        action,
        crate::wm::backend::WmAction::CloseWindow {
            address: "0xb1".into()
        }
    );
}

#[test]
fn closing_a_window_that_is_not_open_asks_nothing() {
    // The half that makes the other half safe. `unwrap_or_default()` used to
    // arm an *empty* dispatch here: the confirmation appeared, "sim" ran
    // `hyprctl` with no arguments, and the sentence was swallowed from
    // dictation on the way. Nothing to close means nothing to ask.
    let (calls, pending) = say("fechar fotoshop", CLIENTS);
    assert!(calls.is_empty());
    assert!(
        pending.is_none(),
        "an unfindable target must not arm a confirmation"
    );
    // And it reports the miss, so Enter mode still dictates the sentence
    // instead of losing it.
    let (fake, runner, config) = setup(CLIENTS);
    let vocab = config.vocab("pt").unwrap().clone();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut p = None;
    assert!(!dispatch_spoken(
        &vocab,
        &config,
        "fechar fotoshop",
        &runner,
        &tx,
        &mut p
    ));
    let _ = fake;
}
/// Every binding the app ships, and exactly what each one dispatches.
///
/// The hand-picked tests above check the cases someone thought of. This walks
/// the whole vocabulary, so a binding added tomorrow is covered tomorrow —
/// the same reason the help text is generated rather than written down.
///
/// It exists for the `WindowManager` refactor: the point is not "is this
/// mapping right" but "is it the same as it was". A refactor that quietly
/// changes what "monitor da direita" does would otherwise pass every test in
/// this file.
fn every_binding(lang: &str) -> Vec<String> {
    let config = crate::config::Config::embedded();
    let vocab = config.vocab(lang).expect("shipped language").clone();
    let mut out = Vec::new();

    let run = |spoken: &str, world: &[u8]| -> String {
        let fake = Arc::new(FakeRunner::new(world.to_vec()));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut pending = None;
        dispatch_spoken(&vocab, &config, spoken, &runner, &tx, &mut pending);
        let calls = dispatched(&fake);
        if calls.is_empty() {
            // A binding that dispatches nothing is either awaiting
            // confirmation or broken; the snapshot records which.
            return match pending {
                Some(_) => "(confirma)".to_string(),
                None => "(nada)".to_string(),
            };
        }
        calls
            .iter()
            .map(|a| a.join(" "))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    let mut words: Vec<&String> = vocab.wm_commands.keys().collect();
    words.sort();
    for w in words {
        out.push(format!("{lang}  {w:<24} -> {}", run(w, SNAPSHOT_MONITORS)));
    }

    // Templates, with a representative value per slot. Directions are
    // enumerated because each one maps somewhere different; the rest get one
    // value, since the resolver's own tests cover their variation.
    let mut directions: Vec<&String> = vocab.directions.keys().collect();
    directions.sort();
    for t in &vocab.templates {
        // `hyprctl monitors -j` and `hyprctl clients -j` are different calls
        // that FakeRunner answers identically, and their shapes do not merge:
        // MonitorInfo requires `name`, which a window does not have, so one
        // combined array deserializes as neither. Each template gets the
        // world it asks about.
        let (fills, world): (Vec<String>, &[u8]) = if t.pattern.contains("{direcao}") {
            (
                directions.iter().map(|d| d.to_string()).collect(),
                SNAPSHOT_MONITORS,
            )
        } else if t.pattern.contains("{numero}") {
            (vec!["3".to_string()], SNAPSHOT_MONITORS)
        } else if t.pattern.contains("{monitor}") {
            (vec!["samsung".to_string()], SNAPSHOT_MONITORS)
        } else {
            (vec!["brave".to_string()], SNAPSHOT_CLIENTS)
        };
        for fill in fills {
            let spoken = t
                .pattern
                .replace("{direcao}", &fill)
                .replace("{numero}", &fill)
                .replace("{monitor}", &fill)
                .replace("{alvo}", &fill);
            out.push(format!("{lang}  {spoken:<24} -> {}", run(&spoken, world)));
        }
    }
    out
}

/// Fixed worlds for the snapshot, so the recorded answers depend on the
/// vocabulary and the code and never on whatever happens to be open.
const SNAPSHOT_MONITORS: &[u8] = br#"[
        {"name": "AAA-1", "description": "BOE 0x0A88", "x": 5000},
        {"name": "BBB-1", "description": "Samsung Electric Company Odyssey G30B", "x": 0},
        {"name": "CCC-1", "description": "LG Electronics LG ULTRAWIDE", "x": 2000}
    ]"#;
const SNAPSHOT_CLIENTS: &[u8] =
    br#"[{"class":"brave-browser","title":"docs - Brave","address":"0xb1"}]"#;

#[test]
fn the_whole_vocabulary_dispatches_what_it_always_did() {
    let mut lines = every_binding("pt");
    lines.extend(every_binding("en"));
    let actual = lines.join("\n") + "\n";

    let path = std::path::Path::new("tests/fixtures/dispatch.snapshot");
    if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
        std::fs::write(path, &actual).expect("writing snapshot");
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_default();
    if expected != actual {
        let mut report = String::from("dispatch snapshot changed:\n");
        for (i, (e, a)) in expected.lines().zip(actual.lines()).enumerate() {
            if e != a {
                report.push_str(&format!("  line {}:\n    era: {e}\n    é:   {a}\n", i + 1));
            }
        }
        let (el, al) = (expected.lines().count(), actual.lines().count());
        if el != al {
            report.push_str(&format!("  {el} linhas antes, {al} agora\n"));
        }
        report.push_str(
            "\nSe a mudança é intencional, revise-a e rode:\n  \
             UPDATE_SNAPSHOT=1 cargo test the_whole_vocabulary\n",
        );
        panic!("{report}");
    }
}

#[path = "dispatch_phrase_tests.rs"]
mod phrases;
