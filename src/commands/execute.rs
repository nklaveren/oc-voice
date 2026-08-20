//! Execution of recognized commands, and the confirmation policy that can
//! hold one back (M4.3): destructive actions and low-confidence target
//! resolutions wait for a spoken yes before anything irreversible happens.

use crate::input::inject::{type_key, type_shift_return, type_text};
use crate::process::CommandRunner;
use crate::{emit, TranscriptEvent};
use crossbeam_channel::Sender;
use std::sync::Arc;

use super::{classify, matcher, VoiceCommand};

/// A command held back by the confirmation policy (M4.3): destructive
/// actions and low-confidence target resolutions wait for a spoken yes.
#[derive(Debug, Clone)]
pub enum PendingAction {
    /// Send the buffered text to a resolved window.
    SendTo {
        display: String,
        inject: String,
        address: String,
        class: String,
        score: f64,
    },
    /// Send the buffered text to the currently focused window — the fallback
    /// when no window matched the spoken target.
    SendToFocused { display: String, inject: String },
    /// A WM dispatch (M3.1), e.g. killactive.
    #[allow(dead_code)] // constructed by wm::dispatch from M3.1 on
    Dispatch { args: Vec<String>, label: String },
}

/// Consume a pending confirmation if the utterance answers it. Returns true
/// when the utterance was an answer (confirm or deny) and is now spent.
fn try_settle_pending(
    spoken: &str,
    vocab: &crate::config::LangVocab,
    threshold: f64,
    pending: &mut Option<PendingAction>,
    tx: &Sender<TranscriptEvent>,
    runner: &Arc<dyn CommandRunner>,
) -> bool {
    let Some(action) = pending.take() else {
        return false;
    };
    let confirm: Vec<&str> = vocab.confirm.iter().map(String::as_str).collect();
    let deny: Vec<&str> = vocab.deny.iter().map(String::as_str).collect();
    if matcher::match_exact(spoken, &confirm, threshold).is_some() {
        match action {
            PendingAction::SendTo {
                display,
                inject,
                address,
                class,
                score,
            } => {
                emit(tx, TranscriptEvent::SentTo(display, class, score));
                crate::wm::hyprland::focus_address_and_type(runner, &address, &inject);
            }
            PendingAction::SendToFocused { display, inject } => {
                emit(tx, TranscriptEvent::Sent(display));
                type_text(&**runner, &inject);
                type_key(&**runner, "Return");
            }
            PendingAction::Dispatch { args, label } => {
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let _ = runner.output("hyprctl", &arg_refs);
                emit(tx, TranscriptEvent::Final(format!("[{label}]")));
            }
        }
        return true;
    }
    if matcher::match_exact(spoken, &deny, threshold).is_some() {
        emit(tx, TranscriptEvent::ConfirmationCancelled);
        return true;
    }
    // Anything else: the pending action is dropped (safe default) and the
    // utterance is processed normally.
    emit(tx, TranscriptEvent::ConfirmationCancelled);
    false
}

/// Route one finalized utterance according to the transcribe mode. This is
/// the single point where speech becomes action, and the unit M4.2's
/// "type_text is never called in Command mode" is tested against.
#[allow(clippy::too_many_arguments)]
pub fn route_final(
    mode: crate::TranscribeMode,
    trimmed: &str,
    config: &crate::config::Config,
    settings: &std::sync::Mutex<crate::AppSettings>,
    enter_buffer: &mut Vec<String>,
    pending: &mut Option<PendingAction>,
    tx: &Sender<TranscriptEvent>,
    runner: &Arc<dyn CommandRunner>,
) {
    use crate::TranscribeMode;
    // A pending confirmation intercepts the utterance in the modes that can
    // create one.
    if matches!(mode, TranscribeMode::Enter | TranscribeMode::Command) {
        if let Some(v) = crate::config::active_vocab(config, settings) {
            if try_settle_pending(trimmed, v, config.threshold(), pending, tx, runner) {
                return;
            }
        }
    }
    match mode {
        TranscribeMode::Input => {
            type_text(&**runner, trimmed);
        }
        TranscribeMode::Enter => {
            let vocab = crate::config::active_vocab(config, settings);
            match vocab.and_then(|v| classify(trimmed, v, config.threshold())) {
                Some(VoiceCommand::Dictation) | None => {
                    enter_buffer.push(trimmed.to_string());
                    emit(tx, TranscriptEvent::Buffered(enter_buffer.len()));
                }
                Some(cmd) => {
                    // vocab is Some here: classify returned a command.
                    if let Some(v) = vocab {
                        execute_command(v, config, &cmd, enter_buffer, pending, tx, runner);
                    }
                }
            }
        }
        TranscribeMode::Command => {
            // Everything is a WM command; dictation is dropped, never typed.
            // Dispatch of navigation commands lands in M3.1.
            let vocab = crate::config::active_vocab(config, settings);
            if let Some(v) = vocab {
                crate::wm::dispatch::dispatch_spoken(v, config, trimmed, runner, tx);
            }
        }
        TranscribeMode::Translate => {}
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_command(
    vocab: &crate::config::LangVocab,
    config: &crate::config::Config,
    cmd: &VoiceCommand,
    enter_buffer: &mut Vec<String>,
    pending: &mut Option<PendingAction>,
    tx: &Sender<TranscriptEvent>,
    runner: &Arc<dyn CommandRunner>,
) {
    let threshold = config.threshold();
    match cmd {
        VoiceCommand::Send => {
            let display_text = enter_buffer.join("\n");
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            let clean = inject_text.trim();
            if !clean.is_empty() {
                emit(tx, TranscriptEvent::Sent(display_text));
                type_text(&**runner, clean);
                type_key(&**runner, "Return");
            }
        }
        VoiceCommand::Cancel => {
            enter_buffer.clear();
            emit(tx, TranscriptEvent::Cancelled);
        }
        VoiceCommand::Newline => {
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            if !inject_text.is_empty() {
                type_text(&**runner, inject_text.trim());
            }
            type_shift_return(&**runner);
            emit(tx, TranscriptEvent::Newline);
        }
        VoiceCommand::SendTo { target } => {
            let display_text = enter_buffer.join("\n");
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            let clean = inject_text.trim().to_string();
            if clean.is_empty() {
                return;
            }
            // M2.3/M4.3: resolve first, then decide — act directly on a
            // confident match, ask on a doubtful one, and never silently type
            // into whatever happens to be focused.
            let windows = crate::wm::target::live_windows(runner);
            let resolved = crate::wm::target::resolve(target, &vocab.targets, &windows, threshold);
            match resolved {
                Some(t) if t.score >= config.confirm_below() => {
                    emit(
                        tx,
                        TranscriptEvent::SentTo(display_text, t.class.clone(), t.score),
                    );
                    crate::wm::hyprland::focus_address_and_type(runner, &t.address, &clean);
                }
                Some(t) => {
                    emit(
                        tx,
                        TranscriptEvent::AwaitingConfirmation(format!(
                            "send to {} ({:.2})?",
                            t.class, t.score
                        )),
                    );
                    *pending = Some(PendingAction::SendTo {
                        display: display_text,
                        inject: clean,
                        address: t.address,
                        class: t.class,
                        score: t.score,
                    });
                }
                None => {
                    emit(
                        tx,
                        TranscriptEvent::AwaitingConfirmation(format!(
                            "no window matched \"{target}\" — send to the focused window?"
                        )),
                    );
                    *pending = Some(PendingAction::SendToFocused {
                        display: display_text,
                        inject: clean,
                    });
                }
            }
        }
        VoiceCommand::Dictation => {
            // handled by caller — pushes to enter_buffer
        }
    }
}

#[cfg(test)]
mod tests {
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
}
