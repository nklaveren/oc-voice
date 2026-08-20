//! The shape of `commands.toml` on disk.
//!
//! Separate from the `Config` the app runs on, because they are different
//! things that only look alike: this layer is permissive and partial — every
//! override optional, every section absent by default — while `Config` is
//! total and already resolved. Collapsing the two is what let a lone
//! `hang_ms = 400` silently reset `max_seconds`.

use super::{LangVocab, Matching, Segmentation, SegmentationSet};
use serde::Deserialize;
use std::collections::HashMap;

/// A user's partial override of one profile.
///
/// Every field is optional on purpose. Deserializing straight into
/// `Segmentation` would let `hang_ms = 400` under `[segmentation.subtitle]`
/// silently reset `max_seconds` to the generic default of 20 — reintroducing
/// the wall-of-text bug the subtitle profile exists to prevent. A patch only
/// changes what it names.
#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct SegmentationPatch {
    hang_ms: Option<u64>,
    max_seconds: Option<u64>,
    partial_every_ms: Option<u64>,
    preroll_ms: Option<u64>,
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
        if let Some(v) = self.preroll_ms {
            base.preroll_ms = v;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct SegmentationSetPatch {
    #[serde(default)]
    pub(super) dictation: SegmentationPatch,
    #[serde(default)]
    pub(super) subtitle: SegmentationPatch,
}

impl SegmentationSetPatch {
    pub(super) fn apply_to(&self, base: &mut SegmentationSet) {
        self.dictation.apply_to(&mut base.dictation);
        self.subtitle.apply_to(&mut base.subtitle);
    }
}

/// Which languages are worth considering at all.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AsrCfg {
    /// The languages actually spoken here. Empty accepts anything, which is
    /// how whisper produces German from a Portuguese sentence at p = 0.198.
    pub languages: Option<Vec<String>>,
}

/// Chromium's DevTools endpoint, for reaching browser tabs.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BrowserCfg {
    /// Port the browser was launched with via `--remote-debugging-port`.
    /// Absent means the feature is off, which is the default: nobody should
    /// have a debugging port opened on their behalf.
    pub debug_port: Option<u16>,
}

/// Where the floating overlay lands.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OverlayCfg {
    /// `middle`, `left`, `right`, `focused`, or a monitor name like `DP-1`.
    pub monitor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct RawConfig {
    #[serde(default)]
    pub(super) matching: Matching,
    #[serde(default)]
    pub(super) segmentation: SegmentationSetPatch,
    #[serde(default)]
    pub(super) overlay: OverlayCfg,
    #[serde(default)]
    pub(super) browser: BrowserCfg,
    #[serde(default)]
    pub(super) asr: AsrCfg,
    #[serde(flatten)]
    pub(super) languages: HashMap<String, LangVocab>,
}
