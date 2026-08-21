//! Tests for command execution and the confirmation policy (M4.3, M3.3).

use super::*;

#[test]
fn a_dispatched_command_is_never_also_typed() {
    // This was `command_mode_never_types`, and the mode it guarded is gone —
    // absorbed into Enter, which does buffer text. The property it existed
    // for is not gone: an utterance the window grammar acts on must not also
    // land in the message you are composing. Losing the mode must not lose
    // the guarantee.
    //
    // The world matters here. With an empty one, "monitor esquerda" resolves
    // to no monitor, dispatches nothing, and correctly falls through to
    // dictation — which is the fall-through working, not the guarantee
    // failing. A command only has to stay out of the buffer when there is
    // something for it to act on.
    use crate::process::FakeRunner;
    let fake = Arc::new(FakeRunner::new(
        br#"[{"name":"AAA-1","description":"BOE","x":0},
             {"name":"BBB-1","description":"LG ULTRAWIDE","x":2000}]"#
            .to_vec(),
    ));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    let config = crate::config::Config::embedded();
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
    });
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer: Vec<String> = Vec::new();
    let mut pending = None;

    for spoken in ["tela cheia", "janela da direita", "monitor esquerda"] {
        route_final(
            crate::TranscribeMode::Enter,
            spoken,
            &config,
            &settings,
            &mut buffer,
            &mut pending,
            &tx,
            &runner,
        );
    }

    assert!(
        buffer.is_empty(),
        "window commands leaked into the composed message: {buffer:?}"
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|(p, _)| p == "wtype" || p == "xdotool"),
        "a window command reached text injection: {:?}",
        fake.calls()
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
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
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
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
    });
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = Vec::new();
    let mut pending = None;
    for spoken in ["fecha", "confirma"] {
        route_final(
            crate::TranscribeMode::Enter,
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
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
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

#[test]
fn enter_mode_navigates_without_leaving_enter_mode() {
    // Reported live: "o modo enter e o modo comando ficar alternando não era
    // a ideia, era conseguir ter tudo junto". Leaving a composed message to
    // switch modes, moving a window, and switching back is friction the
    // word-count gate already makes unnecessary — a command is a handful of
    // words, a dictated line is not.
    let config = crate::config::Config::embedded();
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
    });
    let fake = std::sync::Arc::new(crate::process::FakeRunner::new(b"[]".to_vec()));
    let runner: std::sync::Arc<dyn crate::process::CommandRunner> = fake.clone();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer: Vec<String> = Vec::new();
    let mut pending = None;

    route_final(
        crate::TranscribeMode::Enter,
        "tela cheia",
        &config,
        &settings,
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );

    assert!(
        fake.calls()
            .iter()
            .any(|(p, a)| p == "hyprctl" && a.contains(&"fullscreen".to_string())),
        "the command did not reach the window manager: {:?}",
        fake.calls()
    );
    assert!(
        buffer.is_empty(),
        "a dispatched command must not also be buffered as text"
    );
}

#[test]
fn dictation_that_is_not_a_command_still_reaches_the_buffer() {
    // The other half: consulting the window-manager grammar first must not
    // swallow ordinary speech.
    let config = crate::config::Config::embedded();
    let settings = std::sync::Mutex::new(crate::AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
    });
    let fake = std::sync::Arc::new(crate::process::FakeRunner::new(b"[]".to_vec()));
    let runner: std::sync::Arc<dyn crate::process::CommandRunner> = fake.clone();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer: Vec<String> = Vec::new();
    let mut pending = None;

    let spoken = "preciso revisar aquele documento antes da reunião";
    route_final(
        crate::TranscribeMode::Enter,
        spoken,
        &config,
        &settings,
        &mut buffer,
        &mut pending,
        &tx,
        &runner,
    );

    assert_eq!(buffer, vec![spoken.to_string()]);
}
