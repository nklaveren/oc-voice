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
    #[serde(rename = "session_start")]
    SessionStart,
    #[serde(rename = "session_stop")]
    SessionStop,
    /// List what can be said, from any mode. A closed vocabulary is only
    /// usable if you can find out what is in it without reading the config.
    #[serde(rename = "help")]
    Help,
    /// Switch transcription mode by voice, from any mode (M4.2). The value
    /// is the language-neutral mode name from the vocabulary.
    #[serde(rename = "set_mode")]
    SetMode(String),
    #[serde(rename = "dictation")]
    Dictation,
}

/// Classify a final transcription into a voice command.
///
/// Similarity matching against the closed command vocabulary — see M1.1/M1.2
/// in BACKLOG.md. Utterances with no candidate of the same word count are
/// refused before scoring and fall through to dictation.
/// What can be said right now, built from the active vocabulary rather than
/// written out anywhere.
///
/// A list maintained by hand next to the config it describes goes stale the
/// first time someone edits one and not the other — and a help text that
/// lies is worse than none, because it is believed.
pub fn help_lines(vocab: &crate::config::LangVocab) -> Vec<String> {
    // Labels come from the config too. They are Portuguese words describing
    // Portuguese commands, which is vocabulary by any honest reading — and
    // `just vocab` said so when they were hardcoded here. The key is the
    // language-neutral action name; a missing label falls back to it rather
    // than dropping the row.
    let label = |key: &str| -> String {
        vocab
            .help_labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
    };
    let mut out = Vec::new();
    let mut group = |key: &str, words: &[String]| {
        let shown: Vec<&str> = words.iter().take(4).map(String::as_str).collect();
        if !shown.is_empty() {
            out.push(format!("  {:<12} {}", label(key), shown.join(" / ")));
        }
    };

    group("send", &vocab.send);
    group("cancel", &vocab.cancel);
    group("newline", &vocab.newline);
    group("session_start", &vocab.session_start);
    group("session_stop", &vocab.session_stop);
    group("help", &vocab.help);

    if !vocab.modes.is_empty() {
        let mut phrases: Vec<&str> = vocab.modes.keys().map(String::as_str).collect();
        phrases.sort_unstable();
        out.push(format!("  {:<12} {}", label("modes"), phrases.join(" / ")));
    }
    if let Some(example) = vocab.send_to.first() {
        let target = vocab
            .targets
            .keys()
            .next()
            .map(String::as_str)
            .unwrap_or("");
        out.push(format!("  {:<12} {example} {target}", label("send_to")));
    }
    if !vocab.templates.is_empty() {
        let shown: Vec<&str> = vocab
            .templates
            .iter()
            .take(4)
            .map(|t| t.pattern.as_str())
            .collect();
        out.push(format!(
            "  {:<12} {}",
            label("templates"),
            shown.join(" / ")
        ));
    }
    out
}

/// Remove the discourse words people wrap commands in, so the word-count
/// gate sees the command itself.
///
/// Deliberately a closed list from the vocabulary, not "drop any extra word".
/// The difference is the whole safety property: dropping anything unknown
/// would turn "vamos limpar depois" into "limpar" and eat a dictated line.
fn strip_fillers(text: &str, vocab: &crate::config::LangVocab, threshold: f64) -> String {
    if vocab.fillers.is_empty() {
        return text.to_string();
    }
    let fillers: Vec<&str> = vocab.fillers.iter().map(String::as_str).collect();
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|word| matcher::match_exact(word, &fillers, threshold).is_none())
        .collect();
    // An utterance made only of filler is not a command; leave it intact so
    // it falls through to dictation rather than becoming an empty match.
    if kept.is_empty() {
        return text.to_string();
    }
    kept.join(" ")
}

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

/// Mode switching needs a near-exact match, not merely a good one.
///
/// Measured against a live failure rather than chosen: "Monitor direito"
/// scored above the 0.82 command threshold against "modo ditado" and switched
/// the mode mid-navigation. The two phrases share an opening and
/// Jaro-Winkler rewards that.
pub const MODE_THRESHOLD: f64 = 0.94;

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

    // Measured with `oc-voice probe`, from a live report that "only câmbio
    // works": it was not special, it was the only word being said alone.
    // "ok câmbio" and "limpar tudo" were refused by the word-count gate
    // before anything was scored, because no candidate has two words.
    //
    // The gate is right — it is what stops a dictated sentence becoming a
    // command — so it stays, and the filler comes off first instead. Only
    // words the vocabulary names as filler are dropped, which is why
    // "vamos limpar depois" is still dictation: nothing there is filler, it
    // stays three words, and the gate refuses it exactly as before.
    let stripped = strip_fillers(text, vocab, threshold);
    let text: &str = &stripped;

    for (words, command) in [
        (&vocab.send, VoiceCommand::Send),
        (&vocab.cancel, VoiceCommand::Cancel),
        (&vocab.newline, VoiceCommand::Newline),
        (&vocab.session_start, VoiceCommand::SessionStart),
        (&vocab.session_stop, VoiceCommand::SessionStop),
        (&vocab.help, VoiceCommand::Help),
    ] {
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        if matcher::match_exact(text, &refs, threshold).is_some() {
            return Some(command);
        }
    }

    // Mode switching: spoken phrase to language-neutral mode name. Checked
    // before send-to so "modo comando" is never read as a window target.
    //
    // On a harder threshold than everything else, and the reason is a bug
    // caught in a live log: "Monitor direito" switched the mode to Input.
    // "modo ditado" and "monitor direito" share their opening letters, and
    // Jaro-Winkler pays a prefix bonus, so they scored above 0.82. Switching
    // mode is rare and disruptive — it throws away the grammar the next
    // utterance will be read with — so it earns the same treatment the
    // resolver gives window titles: a near-exact match or nothing.
    let mode_phrases: Vec<&str> = vocab.modes.keys().map(String::as_str).collect();
    let mode_bar = MODE_THRESHOLD.max(threshold);
    if let Some((phrase, _)) = matcher::match_exact(text, &mode_phrases, mode_bar) {
        return Some(VoiceCommand::SetMode(vocab.modes[phrase].clone()));
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
#[path = "commands_tests.rs"]
mod tests;
