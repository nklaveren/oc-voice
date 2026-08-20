//! Tests for command execution and the confirmation policy (M4.3, M3.3).

use super::*;

#[test]
fn command_mode_never_types() {
    // M4.2 acceptance: in Command mode, type_text is never reached — not
    // for dictation, not even for the send word.
    use crate::process::FakeRunner;
    let fake = Arc::new(FakeRunner::new(b"[]".to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    let config = crate::config::Config::embedded();
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Command,
        detected_language: None,
        session_request: None,
    });
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = Vec::new();
    let mut pending = None;
    for spoken in ["texto ditado qualquer", "câmbio", "envia para navegador"] {
        route_final(
            crate::TranscribeMode::Command,
            spoken,
            &config,
            &settings,
            &mut buffer,
            &mut pending,
            &tx,
            &runner,
        );
    }
    assert!(buffer.is_empty(), "Command mode must not buffer dictation");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|(p, _)| p == "wtype" || p == "xdotool" || p == "which"),
        "Command mode must never reach text injection"
    );
}

fn sendto_setup() -> (
    Arc<crate::process::FakeRunner>,
    Arc<dyn CommandRunner>,
    crate::config::Config,
) {
    let fixture = include_str!("../../tests/fixtures/hyprctl_clients.json");
    let fake = Arc::new(crate::process::FakeRunner::new(fixture.as_bytes().to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    (fake, runner, crate::config::Config::embedded())
}

#[test]
fn confident_target_sends_without_confirmation() {
    // M4.3: high confidence and non-destructive → no delay, no pending.
    let (fake, runner, config) = sendto_setup();
    let vocab = config.vocab("pt").unwrap();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = vec!["hello".to_string()];
    let mut pending = None;
    execute_command(
        vocab,
        &config,
        &VoiceCommand::SendTo {
            target: "brave".to_string(),
        },
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );
    assert!(pending.is_none(), "confident match must not wait");
    assert!(fake
        .calls()
        .iter()
        .any(|(p, a)| p == "hyprctl" && a.iter().any(|s| s == "address:0xb1")));
}

#[test]
fn unmatched_target_waits_for_confirmation() {
    // No window matched: never silently type into whatever is focused.
    let (fake, runner, config) = sendto_setup();
    let vocab = config.vocab("pt").unwrap();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = vec!["hello".to_string()];
    let mut pending = None;
    execute_command(
        vocab,
        &config,
        &VoiceCommand::SendTo {
            target: "fotoshop".to_string(),
        },
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );
    assert!(pending.is_some(), "unmatched target must ask first");
    assert!(
        !fake.calls().iter().any(|(p, _)| p == "which"),
        "nothing typed before confirmation"
    );

    // "confirma" executes the held action; "não" would have discarded it.
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
    });
    route_final(
        crate::TranscribeMode::Enter,
        "confirma",
        &config,
        &settings,
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );
    assert!(pending.is_none(), "confirmation settles the pending action");
    assert!(
        fake.calls().iter().any(|(p, _)| p == "which"),
        "confirmed action reaches text injection"
    );
}

#[test]
fn fecha_then_confirma_kills_the_window() {
    // M3.3 acceptance: "fecha" alone closes nothing; the confirmed
    // sequence dispatches killactive.
    let (fake, runner, config) = sendto_setup();
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Command,
        detected_language: None,
        session_request: None,
    });
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = Vec::new();
    let mut pending = None;
    for spoken in ["fecha", "confirma"] {
        route_final(
            crate::TranscribeMode::Command,
            spoken,
            &config,
            &settings,
            &mut buffer,
            &mut pending,
            &tx,
            &runner,
        );
    }
    assert!(pending.is_none());
    assert!(fake
        .calls()
        .iter()
        .any(|(p, a)| p == "hyprctl" && a == &["dispatch", "killactive"]));
}

#[test]
fn deny_discards_the_pending_action() {
    let (fake, runner, config) = sendto_setup();
    let vocab = config.vocab("pt").unwrap();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = vec!["hello".to_string()];
    let mut pending = None;
    execute_command(
        vocab,
        &config,
        &VoiceCommand::SendTo {
            target: "fotoshop".to_string(),
        },
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
    });
    route_final(
        crate::TranscribeMode::Enter,
        "não",
        &config,
        &settings,
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );
    assert!(pending.is_none());
    assert!(
        !fake.calls().iter().any(|(p, _)| p == "which"),
        "denied action must never type"
    );
}
