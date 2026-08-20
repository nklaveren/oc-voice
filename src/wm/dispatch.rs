//! Spoken WM-command dispatch — the grammar lands in M3.1. Command mode
//! (M4.2) routes here; until M3.1 fills the tables this recognizes nothing,
//! and by design nothing here ever types text.

use std::sync::Arc;

use crate::config::{Config, LangVocab};
use crate::process::CommandRunner;
use crate::TranscriptEvent;
use crossbeam_channel::Sender;
use tracing::debug;

pub fn dispatch_spoken(
    _vocab: &LangVocab,
    _config: &Config,
    spoken: &str,
    _runner: &Arc<dyn CommandRunner>,
    _tx: &Sender<TranscriptEvent>,
) {
    // M3.1 fills the template grammar; Command mode drops everything else.
    debug!(spoken, "no dispatch grammar yet (M3.1)");
}
