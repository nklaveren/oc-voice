//! Voice-command vocabulary, per language — M1.3 in BACKLOG.md.
//!
//! The embedded default ships pt and en. A user file at
//! `~/.config/oc-voice/commands.toml` overrides per language: a section
//! defined there fully replaces the embedded one, and languages it does not
//! mention keep the embedded vocabulary.
//!
//! A language with no section anywhere (ja, zh) transcribes and dictates
//! normally but never fires commands: the matcher's word-count gate and
//! diacritic fold only hold for space-separated alphabetic script.

use serde::Deserialize;
use std::collections::HashMap;
use tracing::{info, warn};

const EMBEDDED: &str = include_str!("default.toml");

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Matching {
    pub threshold: Option<f64>,
    /// Below this resolution score, act only after spoken confirmation
    /// (M4.3). 0.0 disables confirmation entirely.
    pub confirm_below: Option<f64>,
    /// Action names that always demand confirmation, score regardless
    /// (M3.3). Empty list disables.
    pub destructive: Option<Vec<String>>,
}

/// A spoken WM pattern with one slot, e.g. `"monitor da {direcao}"`, mapped
/// to a language-neutral action name (M3.1).
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateDef {
    pub pattern: String,
    pub action: String,
}

/// One language's spoken vocabulary. Empty lists are legal: a user can
/// disable a command class by defining it as `[]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LangVocab {
    pub prefix: Vec<String>,
    /// When true, only utterances that start with a prefix word are treated
    /// as commands; everything else is literal dictation (M4.1).
    pub require_prefix: bool,
    pub send: Vec<String>,
    pub cancel: Vec<String>,
    pub newline: Vec<String>,
    /// Spoken prefixes that carry a window target: "envia para <alvo>".
    pub send_to: Vec<String>,
    pub confirm: Vec<String>,
    pub deny: Vec<String>,
    pub numbers: HashMap<String, u32>,
    pub directions: HashMap<String, String>,
    pub targets: HashMap<String, Vec<String>>,
    /// User-defined monitor aliases, e.g. `principal = "DP-1"` (M3.1).
    pub monitors: HashMap<String, String>,
    /// Whole-utterance WM commands: spoken words → action name (M3.1).
    pub wm_commands: HashMap<String, String>,
    /// Slotted WM patterns (M3.1).
    pub templates: Vec<TemplateDef>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    matching: Matching,
    #[serde(flatten)]
    languages: HashMap<String, LangVocab>,
}

#[derive(Debug, Clone)]
pub struct Config {
    threshold: f64,
    confirm_below: f64,
    destructive: Vec<String>,
    languages: HashMap<String, LangVocab>,
}

impl Config {
    /// Embedded default only — what tests and `--no-config` runs see.
    pub fn embedded() -> Self {
        let raw: RawConfig = toml::from_str(EMBEDDED).expect("embedded default.toml must parse");
        Self::from_raw(raw)
    }

    /// Embedded default with the user file merged over it, per language.
    pub fn load() -> Self {
        let mut base = Self::embedded();
        let Some(path) = user_config_path() else {
            return base;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return base;
        };
        match toml::from_str::<RawConfig>(&text) {
            Ok(user) => {
                info!(path = %path.display(), "loaded user vocabulary");
                if let Some(t) = user.matching.threshold {
                    base.threshold = t;
                }
                if let Some(c) = user.matching.confirm_below {
                    base.confirm_below = c;
                }
                if let Some(d) = user.matching.destructive {
                    base.destructive = d;
                }
                for (lang, vocab) in user.languages {
                    base.languages.insert(lang, vocab);
                }
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e,
                    "commands.toml did not parse; using embedded vocabulary");
            }
        }
        base
    }

    fn from_raw(raw: RawConfig) -> Self {
        Config {
            threshold: raw
                .matching
                .threshold
                .unwrap_or(crate::commands::matcher::DEFAULT_THRESHOLD),
            confirm_below: raw.matching.confirm_below.unwrap_or(0.9),
            destructive: raw
                .matching
                .destructive
                .unwrap_or_else(|| vec!["kill_active".to_string()]),
            languages: raw.languages,
        }
    }

    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    pub fn confirm_below(&self) -> f64 {
        self.confirm_below
    }

    #[allow(dead_code)] // consulted by wm::dispatch from M3.3 on
    pub fn is_destructive(&self, action: &str) -> bool {
        self.destructive.iter().any(|d| d == action)
    }

    /// The vocabulary for a language code, or None when that language has no
    /// commands (and everything spoken in it is dictation).
    pub fn vocab(&self, lang: &str) -> Option<&LangVocab> {
        self.languages.get(lang)
    }
}

/// The command vocabulary for whatever language is active right now: the
/// explicit selection, or whisper's last detection while in "auto". Languages
/// without a section (ja, zh) return None and everything stays dictation.
pub fn active_vocab<'a>(
    config: &'a Config,
    settings: &std::sync::Mutex<crate::AppSettings>,
) -> Option<&'a LangVocab> {
    let s = crate::lock_settings(settings);
    let lang = if s.language == "auto" {
        s.detected_language.clone()?
    } else {
        s.language.clone()
    };
    drop(s);
    config.vocab(&lang)
}

fn user_config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("oc-voice").join("commands.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_parses_with_pt_and_en() {
        let c = Config::embedded();
        let pt = c.vocab("pt").expect("pt section");
        assert!(pt.send.iter().any(|w| w == "câmbio"));
        assert!(!pt.numbers.is_empty());
        assert!(!pt.directions.is_empty());
        let en = c.vocab("en").expect("en section");
        assert!(en.send.iter().any(|w| w == "send"));
    }

    #[test]
    fn cjk_languages_have_no_commands() {
        let c = Config::embedded();
        assert!(c.vocab("ja").is_none());
        assert!(c.vocab("zh").is_none());
    }

    #[test]
    fn user_section_replaces_embedded_language() {
        let mut base = Config::embedded();
        let user: RawConfig = toml::from_str(
            r#"
            [es]
            send = ["envía", "listo"]
            "#,
        )
        .unwrap();
        for (lang, vocab) in user.languages {
            base.languages.insert(lang, vocab);
        }
        let es = base.vocab("es").expect("es section");
        assert!(es.send.iter().any(|w| w == "listo"));
        assert!(es.cancel.is_empty());
    }
}
