//! Where the voiceprint is kept, and what it is.
//!
//! Split from `voicelock.rs` at the size ceiling, on the line the two halves
//! already had between them: this one is a file on disk and its permissions,
//! that one is the decision about whose voice just arrived. The seam is real
//! enough that `voiceprobe.rs` uses this half alone — it builds a lock from a
//! recording without ever owning a microphone.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// What the enrolment measured, and the bar it produced. Plain text on
/// purpose: a person editing this file beats any confidence heuristic, and it
/// is the only defence that still works when the rest is wrong.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    /// The mean embedding of the enrolment, normalised.
    pub centroid: Vec<f32>,
    /// Accept at or above this cosine.
    pub threshold: f32,
    /// How many segments the centroid was built from. Evidence, so a lock
    /// built from three segments can be told from one built from twelve.
    pub segments: usize,
    /// Leave-one-out similarity of your own enrolment samples: the worst and
    /// the average. These are the numbers the threshold came from, kept so it
    /// can be argued with later.
    pub self_worst: f32,
    pub self_mean: f32,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(base.join("oc-voice").join("voice.toml"))
}

impl Store {
    pub(crate) fn load() -> Option<Self> {
        let p = path()?;
        let text = std::fs::read_to_string(&p).ok()?;
        match toml::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(path = %p.display(), error = %e, "voice lock did not parse; ignoring it");
                None
            }
        }
    }

    /// Write it to disk. Public because a lock can now be built from a
    /// recording as well as from the microphone, and both end here.
    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                let _ = std::fs::write(&p, text);
                owner_only(&p);
                info!(path = %p.display(), segments = self.segments, "voice lock saved");
            }
            Err(e) => warn!(error = %e, "voice lock could not be serialised"),
        }
    }

    pub(crate) fn forget() {
        if let Some(p) = path() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Readable by its owner and nobody else.
///
/// A voiceprint is not reversible to audio — it is 512 numbers, not a
/// recording — but it *is* a biometric identifier: whoever holds it can test
/// whether a given recording is you. `fs::write` creates 0644, which would
/// leave that open to every account on the machine, and that is the cheapest
/// possible thing to get wrong.
fn owner_only(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// The lock as it sits on disk, for tools that want to compare against it
/// without owning the microphone.
pub fn stored() -> Option<Store> {
    Store::load()
}

#[cfg(test)]
#[path = "voicestore_tests.rs"]
mod tests;
