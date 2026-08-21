//! What the overlay remembers between runs.
//!
//! Deliberately **not** `commands.toml`. That file is written by hand, with
//! comments explaining why each value is what it is, and an app that rewrites
//! it would eventually eat one of those comments. Config is what the person
//! writes; state is what the program writes, and mixing the two costs the
//! person their annotations.
//!
//! Every field is optional and every failure is silent: a missing, corrupt or
//! unwritable state file means the overlay opens at its defaults, which is a
//! perfectly good outcome and not worth a dialog.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::Layout;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct Saved {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub bottom_margin: Option<f32>,
    pub x_offset: Option<f32>,
    pub opacity: Option<f32>,
    /// Which output it was last moved to, so stepping across monitors sticks.
    pub monitor: Option<String>,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(base.join("oc-voice").join("overlay.toml"))
}

impl Saved {
    pub(super) fn load() -> Self {
        let Some(p) = path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            debug!(path = %p.display(), error = %e, "overlay state did not parse");
            Self::default()
        })
    }

    pub(super) fn store(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = std::fs::write(&p, text);
        }
    }

    /// Fill in a layout, leaving defaults where nothing was saved.
    pub(super) fn apply_to(&self, layout: &mut Layout) {
        if let Some(v) = self.width {
            layout.width = v;
        }
        if let Some(v) = self.height {
            layout.height = v;
        }
        if let Some(v) = self.bottom_margin {
            layout.bottom_margin = v;
        }
        if let Some(v) = self.x_offset {
            layout.x_offset = v;
        }
        if let Some(v) = self.opacity {
            layout.opacity = v;
        }
    }
}
