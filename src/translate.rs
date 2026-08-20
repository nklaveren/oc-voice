//! Translation of finalized transcriptions, for display only — M7.2.
//!
//! The translated text goes to the overlay and nowhere else. The session
//! record keeps the original in whatever language was spoken, because a bad
//! translation on screen costs half a second of confusion while a bad
//! translation in the record is a falsified minute that nobody can audit.
//!
//! Runs in a worker thread: translation costs ~240–440 ms for a typical
//! utterance, which must not sit in the audio path where it would delay the
//! next segment's VAD processing.

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use ct2rs::{Config, TranslationOptions, Translator};
use tracing::{info, warn};

use crate::TranscriptEvent;

/// A translation request: the original text and where it came from, so the
/// overlay can pair the result with the line it belongs to.
pub struct Request {
    pub original: String,
}

/// Spawn the translation worker. Returns the sender to feed it, or None when
/// no model is installed — in which case the app runs exactly as before and
/// the overlay shows originals.
pub fn spawn(models_dir: &Path, tx: Sender<TranscriptEvent>) -> Option<Sender<Request>> {
    let model = models_dir.join("ct2-en-pt");
    if !model.join("model.bin").exists() {
        info!(
            path = %model.display(),
            "no translation model; run `just fetch-mt` to enable translated subtitles"
        );
        return None;
    }

    let (req_tx, req_rx): (Sender<Request>, Receiver<Request>) = crossbeam_channel::unbounded();
    let model_path = model.to_string_lossy().to_string();
    std::thread::spawn(move || {
        let translator = match Translator::new(&model_path, &Config::default()) {
            Ok(t) => {
                info!(path = %model_path, "translation model loaded");
                t
            }
            Err(e) => {
                warn!(error = %e, "translation disabled: model failed to load");
                return;
            }
        };
        let opts = TranslationOptions {
            beam_size: 1,
            ..Default::default()
        };
        for req in req_rx {
            let text = req.original.trim().to_string();
            if text.is_empty() {
                continue;
            }
            match translator.translate_batch(&[text.as_str()], &opts, None) {
                Ok(out) => {
                    if let Some((translated, _)) = out.into_iter().next() {
                        let translated = translated.trim().to_string();
                        // Identical output means the source was already the
                        // target language; showing it twice is just noise.
                        if !translated.is_empty() && translated != text {
                            let _ = tx.send(TranscriptEvent::Translated(translated));
                        }
                    }
                }
                Err(e) => warn!(error = %e, "translation failed"),
            }
        }
    });
    Some(req_tx)
}

/// Queue a finalized utterance for translation. Never blocks the audio path:
/// if the worker is behind, the request is simply dropped — a missing
/// subtitle is better than a stalled pipeline.
pub fn request(sender: &Option<Arc<Sender<Request>>>, original: &str) {
    if let Some(tx) = sender {
        let _ = tx.try_send(Request {
            original: original.to_string(),
        });
    }
}
