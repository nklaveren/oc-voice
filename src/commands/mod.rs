// Only tests call the matcher until M1.2 wires it into `classify`.
#[allow(dead_code)]
pub mod matcher;

use crate::input::inject::{type_key, type_shift_return, type_text};
use crate::process::CommandRunner;
use crate::wm::hyprland::focus_window_and_type;
use crate::{emit, TranscriptEvent};
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum VoiceCommand {
    #[serde(rename = "send")]
    Send,
    #[serde(rename = "cancel")]
    Cancel,
    #[serde(rename = "newline")]
    Newline,
    #[serde(rename = "send_to")]
    SendTo { target: String },
    #[serde(rename = "dictation")]
    Dictation,
}

/// Classify a final transcription into a voice command.
///
/// Keyword matching against a fixed table: the whole normalized utterance must
/// equal a keyword. This is fragile against ASR error and is replaced by
/// similarity matching in M1 — see BACKLOG.md.
pub fn classify(text: &str) -> Option<VoiceCommand> {
    info!(text = %text, "keyword fallback classification");
    let lower = fold_diacritics(&text.to_lowercase());
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let normalized = words.join(" ");

    if words.len() > 5 {
        return Some(VoiceCommand::Dictation);
    }

    let send_keywords = ["envia", "enviar", "manda", "mandar", "cambio", "pronto"];
    let cancel_keywords = ["cancela", "cancelar", "limpa", "limpar", "descarta"];
    let newline_keywords = ["nova linha", "pula linha", "newline", "enter"];

    let text_lower = normalized.as_str();

    if send_keywords.contains(&text_lower) {
        return Some(VoiceCommand::Send);
    }
    if cancel_keywords.contains(&text_lower) {
        return Some(VoiceCommand::Cancel);
    }
    if newline_keywords.contains(&text_lower) {
        return Some(VoiceCommand::Newline);
    }

    let send_to_prefixes = [
        "enviar para ",
        "envia para ",
        "enviar pelo ",
        "envia pelo ",
        "enviar via ",
        "envia via ",
        "enviar pro ",
        "envia pro ",
        "manda para ",
        "manda pelo ",
        "manda via ",
        "manda pro ",
    ];
    for prefix in &send_to_prefixes {
        if let Some(pos) = text_lower.find(prefix) {
            let target = text_lower[pos + prefix.len()..].trim();
            if !target.is_empty() {
                let resolved = resolve_target_alias(target);
                return Some(VoiceCommand::SendTo {
                    target: resolved.to_string(),
                });
            }
        }
    }

    Some(VoiceCommand::Dictation)
}

/// Whisper transcribes Portuguese with accents ("câmbio"), but the keyword
/// tables below are written unaccented. Fold the diacritics pt-BR actually
/// uses so both spellings land on the same entry.
pub(crate) fn fold_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

fn resolve_target_alias(target: &str) -> &str {
    match target {
        "navegador" | "firefox" | "browser" => "firefox",
        "chrome" | "google chrome" | "chromium" => "chromium",
        "opencode" | "open code" | "oc" => "oc-opencode",
        "terminal" | "term" => "Alacritty",
        "editor" | "vscode" | "code" | "vs code" => "code",
        "discord" | "chat" => "discord",
        "telegram" => "telegram",
        "whatsapp" => "whatsapp-nativefier",
        _ => target,
    }
}

pub fn execute_command(
    cmd: &VoiceCommand,
    enter_buffer: &mut Vec<String>,
    tx: &Sender<TranscriptEvent>,
    runner: &Arc<dyn CommandRunner>,
) {
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
            let clean = inject_text.trim();
            if !clean.is_empty() {
                emit(tx, TranscriptEvent::SentTo(display_text, target.clone()));
                focus_window_and_type(runner, target, clean);
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
    fn accented_send_keyword_matches() {
        // The regression: whisper emits "câmbio", the table says "cambio".
        assert_eq!(classify("câmbio"), Some(VoiceCommand::Send));
        assert_eq!(classify("cambio"), Some(VoiceCommand::Send));
        assert_eq!(classify("Câmbio."), Some(VoiceCommand::Send));
    }

    #[test]
    fn other_commands_still_match() {
        assert_eq!(classify("envia"), Some(VoiceCommand::Send));
        assert_eq!(classify("cancela"), Some(VoiceCommand::Cancel));
        assert_eq!(classify("nova linha"), Some(VoiceCommand::Newline));
        assert_eq!(
            classify("envia para navegador"),
            Some(VoiceCommand::SendTo {
                target: "firefox".to_string()
            })
        );
    }

    #[test]
    fn long_speech_is_dictation() {
        assert_eq!(
            classify("isso aqui e uma frase normal de ditado qualquer"),
            Some(VoiceCommand::Dictation)
        );
    }
}
