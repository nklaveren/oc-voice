//! The strip above the transcript: what the app is doing to you right now.
//!
//! Three states share one property that decides how they are drawn — they
//! are *conditions*, not events. Recording is on or off; the pipeline is
//! alive or dead; the voice lock is ignoring somebody or it is not. A
//! condition belongs on a line that updates in place.
//!
//! The voice indicator is here because it was not. It pushed a line into the
//! transcript for every utterance it refused, and a video playing near the
//! microphone put five of them on screen in a row — five copies of the same
//! fact, burying the record the app exists to produce. The reason it was
//! shown at all is still right: an assistant that ignores you without a word
//! is indistinguishable from one that has crashed. But that reason is
//! satisfied by *one* line that says so and keeps a count. Repeating it is
//! not more honest, only louder.

use eframe::egui;

use super::OverlayApp;

/// How long an ignored utterance keeps the indicator up. It is refreshed by
/// each new rejection, so a television talking for an hour costs one line for
/// as long as it talks and clears a few seconds after it stops.
pub(super) const IGNORED_SHOWN: std::time::Duration = std::time::Duration::from_secs(8);

pub(super) fn strip(app: &OverlayApp, ui: &mut egui::Ui) {
    if let Some((since, lines)) = app.recording {
        let secs = since.elapsed().as_secs();
        // Blinks so it cannot be mistaken for a static label. ASCII, like the
        // translation marker: the bundled font has no ⏺/◯ and drew an empty
        // box for both, which blinks exactly as well as nothing at all.
        let dot = if secs % 2 == 0 { "*" } else { " " };
        ui.label(
            egui::RichText::new(format!(
                "{dot} GRAVANDO  {:02}:{:02}:{:02}  ({lines} falas)",
                secs / 3600,
                (secs % 3600) / 60,
                secs % 60
            ))
            .color(egui::Color32::from_rgb(255, 80, 80))
            .strong()
            .size(16.0),
        );
    }

    if app.pipeline_failed {
        ui.label(
            egui::RichText::new("[ audio pipeline crashed \u{2014} close and restart oc-voice ]")
                .color(egui::Color32::from_rgb(255, 90, 90))
                .strong()
                .size(18.0),
        );
    }

    if let Some(text) = ignored_label(app.ignored, std::time::Instant::now()) {
        // Dim. This is the lock working, not a fault, and it competes with the
        // transcript for the same eye.
        ui.label(
            egui::RichText::new(text)
                .color(egui::Color32::from_rgb(130, 130, 140))
                .size(14.0),
        );
    }
}

/// The indicator's text, or nothing if it has gone stale.
///
/// Pure, and takes `now`, so the thing worth pinning can be pinned without a
/// window: that a hundred refusals are one line and a count.
pub(super) fn ignored_label(
    ignored: Option<(f32, usize, std::time::Instant)>,
    now: std::time::Instant,
) -> Option<String> {
    let (score, run, at) = ignored?;
    if now.duration_since(at) >= IGNORED_SHOWN {
        return None;
    }
    Some(if run > 1 {
        format!("[ {run} falas ignoradas \u{2014} não é a sua voz (última {score:.2}) ]")
    } else {
        format!("[ ignorado \u{2014} não é a sua voz ({score:.2}) ]")
    })
}

#[cfg(test)]
#[path = "overlay_status_tests.rs"]
mod tests;
