pub mod execute;
pub mod matcher;

pub use execute::{route_final, PendingAction};

use serde::{Deserialize, Serialize};
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
/// Similarity matching against the closed command vocabulary — see M1.1/M1.2
/// in BACKLOG.md. Utterances with no candidate of the same word count are
/// refused before scoring and fall through to dictation.
/// Strip a leading prefix word ("computador, …") if one is spoken. Returns
/// the remainder and whether a prefix was found. The prefix is matched with
/// the same similarity pipeline as everything else — ASR mangles it too.
fn strip_prefix<'a>(
    text: &'a str,
    vocab: &crate::config::LangVocab,
    threshold: f64,
) -> (&'a str, bool) {
    let Some(first_word) = text.split_whitespace().next() else {
        return (text, false);
    };
    let prefixes: Vec<&str> = vocab.prefix.iter().map(String::as_str).collect();
    if matcher::match_exact(first_word, &prefixes, threshold).is_some() {
        let rest = text[first_word.len() + text.find(first_word).unwrap_or(0)..].trim_start();
        return (rest.trim_start_matches([',', ' ']), true);
    }
    (text, false)
}

pub fn classify(
    text: &str,
    vocab: &crate::config::LangVocab,
    threshold: f64,
) -> Option<VoiceCommand> {
    info!(text = %text, "similarity classification");

    // M4.1: the prefix acts before the grammar. With require_prefix on, an
    // unprefixed utterance is literal dictation no matter what it says; the
    // prefix, once recognized, is stripped and the REST goes to the matcher —
    // the word-count gate then applies to that rest, so the two compose.
    let (text, had_prefix) = strip_prefix(text, vocab, threshold);
    if vocab.require_prefix && !had_prefix {
        return Some(VoiceCommand::Dictation);
    }

    for (words, command) in [
        (&vocab.send, VoiceCommand::Send),
        (&vocab.cancel, VoiceCommand::Cancel),
        (&vocab.newline, VoiceCommand::Newline),
    ] {
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        if matcher::match_exact(text, &refs, threshold).is_some() {
            return Some(command);
        }
    }

    // Send-to: a spoken prefix from the vocabulary followed by the target
    // name. The target stays as spoken — resolution against live windows
    // happens at execution time (M2.1), when the window list is current.
    let lower = fold_diacritics(&text.to_lowercase());
    let normalized = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    for prefix in &vocab.send_to {
        let prefix_norm = fold_diacritics(&prefix.to_lowercase());
        if let Some(pos) = normalized.find(&format!("{prefix_norm} ")) {
            let target = normalized[pos + prefix_norm.len()..].trim();
            if !target.is_empty() {
                return Some(VoiceCommand::SendTo {
                    target: target.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// All command tests speak the embedded Portuguese vocabulary.
    fn classify(text: &str) -> Option<VoiceCommand> {
        let config = crate::config::Config::embedded();
        let vocab = config.vocab("pt").expect("embedded pt vocab");
        super::classify(text, vocab, config.threshold())
    }

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
            // M2.1: the target stays as spoken; resolution against live
            // windows happens at execution time, not at classify time.
            Some(VoiceCommand::SendTo {
                target: "navegador".to_string()
            })
        );
    }

    #[test]
    fn asr_variants_classify_through_the_public_api() {
        // M1.2: the M1.1 measurements hold through `classify`, not just the
        // matcher's own unit tests.
        for spoken in ["sambio", "cambiu", "kambio", "quambio", "cambrio"] {
            assert_eq!(classify(spoken), Some(VoiceCommand::Send), "{spoken}");
        }
        assert_eq!(classify("cancelar"), Some(VoiceCommand::Cancel));
        assert_eq!(classify("nova linia"), Some(VoiceCommand::Newline));
        for spoken in ["pronto falei", "sao paulo", "bom dia"] {
            assert_eq!(classify(spoken), Some(VoiceCommand::Dictation), "{spoken}");
        }
    }

    #[test]
    fn prefix_marks_commands_when_required() {
        // M4.1, both directions: with require_prefix on, a bare command word
        // is literal dictation, and the prefixed form is a command.
        let config = crate::config::Config::embedded();
        let mut vocab = config.vocab("pt").unwrap().clone();
        vocab.require_prefix = true;
        let t = config.threshold();
        assert_eq!(
            super::classify("câmbio", &vocab, t),
            Some(VoiceCommand::Dictation),
            "bare command word must dictate literally"
        );
        assert_eq!(
            super::classify("computador, câmbio", &vocab, t),
            Some(VoiceCommand::Send)
        );
        // ASR error on the prefix itself still counts.
        assert_eq!(
            super::classify("comptador câmbio", &vocab, t),
            Some(VoiceCommand::Send)
        );
    }

    #[test]
    fn prefix_is_optional_by_default() {
        // Default config keeps today's behaviour: bare commands work, and the
        // prefixed form works too.
        assert_eq!(classify("câmbio"), Some(VoiceCommand::Send));
        assert_eq!(classify("computador câmbio"), Some(VoiceCommand::Send));
    }

    #[test]
    fn long_speech_is_dictation() {
        assert_eq!(
            classify("isso aqui e uma frase normal de ditado qualquer"),
            Some(VoiceCommand::Dictation)
        );
    }
}
