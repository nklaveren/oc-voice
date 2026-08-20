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
    /// Open a recorded session (M7.1).
    pub session_start: Vec<String>,
    /// Close it and write the file.
    pub session_stop: Vec<String>,
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

/// How aggressively the VAD closes a segment, per use. Tunable without
/// recompiling because the right values depend on how people actually talk:
/// dictating alone leaves long pauses, a meeting almost never does.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Segmentation {
    /// Silence, in milliseconds, before an utterance is considered finished.
    pub hang_ms: u64,
    /// Hard cap on one segment. Reaching it means the VAD never found a
    /// pause — in a meeting that produces a wall of text mixing speakers.
    pub max_seconds: u64,
    /// How often a partial is refreshed while someone is speaking.
    pub partial_every_ms: u64,
}

impl Default for Segmentation {
    fn default() -> Self {
        Segmentation {
            hang_ms: 640,
            max_seconds: 20,
            partial_every_ms: 800,
        }
    }
}

/// A user's partial override of one profile.
///
/// Every field is optional on purpose. Deserializing straight into
/// `Segmentation` would let `hang_ms = 400` under `[segmentation.subtitle]`
/// silently reset `max_seconds` to the generic default of 20 — reintroducing
/// the wall-of-text bug the subtitle profile exists to prevent. A patch only
/// changes what it names.
#[derive(Debug, Clone, Deserialize, Default)]
struct SegmentationPatch {
    hang_ms: Option<u64>,
    max_seconds: Option<u64>,
    partial_every_ms: Option<u64>,
}

impl SegmentationPatch {
    fn apply_to(&self, base: &mut Segmentation) {
        if let Some(v) = self.hang_ms {
            base.hang_ms = v;
        }
        if let Some(v) = self.max_seconds {
            base.max_seconds = v;
        }
        if let Some(v) = self.partial_every_ms {
            base.partial_every_ms = v;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SegmentationSetPatch {
    #[serde(default)]
    dictation: SegmentationPatch,
    #[serde(default)]
    subtitle: SegmentationPatch,
}

impl SegmentationSetPatch {
    fn apply_to(&self, base: &mut SegmentationSet) {
        self.dictation.apply_to(&mut base.dictation);
        self.subtitle.apply_to(&mut base.subtitle);
    }
}

/// Where the floating overlay lands.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OverlayCfg {
    /// `middle`, `left`, `right`, `focused`, or a monitor name like `DP-1`.
    pub monitor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    matching: Matching,
    #[serde(default)]
    segmentation: SegmentationSetPatch,
    #[serde(default)]
    overlay: OverlayCfg,
    #[serde(flatten)]
    languages: HashMap<String, LangVocab>,
}

/// One profile per use. Meeting subtitles and dictation want opposite
/// behaviour, so they get separate numbers instead of one compromise.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SegmentationSet {
    /// Dictation into a window, and Enter mode composing a message.
    pub dictation: Segmentation,
    /// Following someone else speak: a meeting, a video.
    pub subtitle: Segmentation,
}

impl Default for SegmentationSet {
    fn default() -> Self {
        SegmentationSet {
            dictation: Segmentation {
                hang_ms: 960,
                max_seconds: 20,
                partial_every_ms: 900,
            },
            // A meeting rarely offers 640 ms of silence, so the old default
            // ran every segment to the 20 s cap: a wall of text that also
            // mixed several speakers into one block, which makes M7.4's
            // diarization impossible before it starts. A speaker change
            // almost always carries a short pause; closing on it gives
            // phrase-sized subtitles AND one voice per segment.
            subtitle: Segmentation {
                hang_ms: 320,
                max_seconds: 8,
                partial_every_ms: 700,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    threshold: f64,
    segmentation: SegmentationSet,
    overlay_monitor: String,
    confirm_below: f64,
    destructive: Vec<String>,
    languages: HashMap<String, LangVocab>,
}

/// Which monitor the overlay is pinned to when nothing says otherwise.
pub const DEFAULT_OVERLAY_MONITOR: &str = "middle";

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
                // Without this the documented "tune hang_ms without
                // recompiling" was a lie: every other field merged and this
                // one was silently dropped.
                user.segmentation.apply_to(&mut base.segmentation);
                if let Some(m) = user.overlay.monitor {
                    base.overlay_monitor = m;
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
        let mut segmentation = SegmentationSet::default();
        raw.segmentation.apply_to(&mut segmentation);
        Config {
            threshold: raw
                .matching
                .threshold
                .unwrap_or(crate::commands::matcher::DEFAULT_THRESHOLD),
            segmentation,
            overlay_monitor: raw
                .overlay
                .monitor
                .unwrap_or_else(|| DEFAULT_OVERLAY_MONITOR.to_string()),
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

    /// Which monitor the overlay should be pinned to.
    pub fn overlay_monitor(&self) -> &str {
        &self.overlay_monitor
    }

    /// Segmentation profile for a mode: following others, or speaking yourself.
    pub fn segmentation(&self, subtitle: bool) -> &Segmentation {
        if subtitle {
            &self.segmentation.subtitle
        } else {
            &self.segmentation.dictation
        }
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
#[path = "config_tests.rs"]
mod tests;
