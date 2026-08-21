use crate::process::CommandRunner;
use crate::ui::host::{Flow, Geometry, OverlayUi};
use crate::{AppSettings, TranscribeMode, TranscriptEvent};
use anyhow::Result;
use crossbeam_channel::Receiver;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Logical size of the overlay, and the gap it keeps from the bottom edge.
/// Shared with the positioning correction so the two cannot disagree.
pub const OVERLAY_W: f32 = 900.0;
pub const OVERLAY_H: f32 = 350.0;
pub const OVERLAY_BOTTOM_MARGIN: f32 = 60.0;

pub fn run_overlay(
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    runner: Arc<dyn CommandRunner>,
    config: Arc<crate::config::Config>,
) -> Result<()> {
    // Which surface the overlay lives on, and what that costs to keep in
    // place, are the host's business — not this function's and not the UI's.
    let host = crate::ui::host::default_host(
        Geometry {
            width: OVERLAY_W,
            height: OVERLAY_H,
            bottom_margin: OVERLAY_BOTTOM_MARGIN,
        },
        runner.clone(),
        config.overlay_monitor().to_string(),
        config.overlay_surface(),
    );
    host.run(Box::new(OverlayApp::new(
        rx, running, settings, config, runner,
    )))
}

struct OverlayApp {
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    config: Arc<crate::config::Config>,
    runner: Arc<dyn CommandRunner>,
    /// The session file being written, or the last one written. Kept after
    /// the session closes so the button still works when you want to read
    /// back what was just recorded.
    pub(super) session_path: Option<String>,
    /// One in-progress utterance per stream. A single slot would let the
    /// meeting and the microphone overwrite each other mid-sentence, which is
    /// exactly when both are speaking.
    pub(super) partial_mic: String,
    pub(super) partial_system: String,
    pub(super) finals: Vec<Line>,
    pub(super) buffered: usize,
    show_settings: bool,
    pub(super) pipeline_failed: bool,
    /// When a session is recording, and how many lines it holds (M7.1).
    /// Recording without a visible indication is not acceptable.
    pub(super) recording: Option<(std::time::Instant, usize)>,
}

/// One line of scrollback: what was said, and — for as long as the line
/// exists — what it was translated to.
///
/// The translation used to be a single `Option<String>` holding only the most
/// recent one, cleared on every new utterance. Scrolling back through a
/// meeting then showed originals with the translations stripped out, which is
/// the opposite of useful: the reason to look back is usually to re-read the
/// part you did not follow.
#[derive(Debug, Clone)]
pub(super) struct Line {
    pub text: String,
    pub translation: Option<String>,
    pub source: crate::Source,
}

impl Line {
    pub(super) fn new(text: impl Into<String>, source: crate::Source) -> Self {
        Line {
            text: text.into(),
            translation: None,
            source,
        }
    }
}

/// Your own speech, tinted so it is distinguishable at a glance from the
/// meeting's without reading the label.
const MIC_COLOR: egui::Color32 = egui::Color32::from_rgb(190, 235, 160);

const LANGUAGES: &[&str] = &["auto", "pt", "en", "es", "fr", "de", "ja", "zh"];

/// Marker prefixed to a translated line.
///
/// Deliberately ASCII. The `↳` this replaces rendered as an empty box in the
/// bundled font, so every translation carried a tofu glyph — a missing font is
/// not something a subtitle overlay can detect and fall back from at runtime.
const TRANSLATION_MARKER: &str = "|_ ";

/// Vertical space reserved for the control row, claimed before the scrollback
/// takes what remains.
const CONTROLS_HEIGHT: f32 = 32.0;

/// How much height the scrollback may occupy.
///
/// The invariant: whatever the window height, the row holding the mode button
/// stays on screen. If the window is too short for both, the scrollback gives
/// up its space — a transcript with no reachable mode button is a frozen app,
/// while a one-line transcript is merely cramped.
fn scroll_height(available: f32, controls: f32) -> f32 {
    (available - controls).max(0.0)
}

/// Hand the session file to whatever the desktop opens Markdown with.
///
/// Spawned rather than waited on: `xdg-open` can block for as long as the
/// editor takes to start, and the overlay is drawing subtitles on this thread.
fn open_session_file(runner: &Arc<dyn CommandRunner>, path: &str) {
    match runner.spawn_piped("xdg-open", &[path]) {
        Ok(_) => tracing::info!(path, "opening session record"),
        Err(e) => tracing::warn!(path, error = %e, "could not open session record"),
    }
}

/// What each mode is for, in a few words, shown beside the button.
///
/// Added because the person who built it forgot what Enter mode was for —
/// which was fair, when four modes existed and two pairs of them were nearly
/// the same thing. Two remain, and the difference between them is the one
/// that matters: whose voice is being listened to.
fn mode_hint(mode: TranscribeMode) -> &'static str {
    match mode {
        TranscribeMode::Enter => "sua voz: junta o texto e navega",
        TranscribeMode::Translate => "áudio do sistema: legenda e traduz",
    }
}

/// Colour is the whole distinction between your speech and the meeting's.
/// A text label on every line would double the height of a subtitle overlay
/// for information the reader already has from context.
fn speaker_color(source: crate::Source) -> egui::Color32 {
    match source {
        crate::Source::Mic => MIC_COLOR,
        crate::Source::System => egui::Color32::WHITE,
    }
}

impl OverlayApp {
    fn new(
        rx: Receiver<TranscriptEvent>,
        running: Arc<AtomicBool>,
        settings: Arc<Mutex<AppSettings>>,
        config: Arc<crate::config::Config>,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            rx,
            running,
            settings,
            config,
            runner,
            session_path: None,
            partial_mic: String::new(),
            partial_system: String::new(),
            finals: Vec::new(),
            buffered: 0,
            show_settings: false,
            pipeline_failed: false,
            recording: None,
        }
    }

    /// First send keyword of the active language, for the UI hints. Falls
    /// back to a neutral hint when the language has no command section.
    fn send_word(&self) -> Option<String> {
        let s = crate::lock_settings(&self.settings);
        let lang = if s.language == "auto" {
            s.detected_language.clone().unwrap_or_else(|| "pt".into())
        } else {
            s.language.clone()
        };
        drop(s);
        self.config
            .vocab(&lang)
            .and_then(|v| v.send.first().cloned())
    }
}

#[path = "overlay_events.rs"]
mod events;

#[path = "overlay_draw.rs"]
mod draw;

impl OverlayUi for OverlayApp {
    fn tick(&mut self) -> Flow {
        // Drain all pending transcription events before painting.
        self.drain_events();
        // The pipeline signals shutdown by clearing this; how a window closes
        // is the host's business.
        if self.running.load(Ordering::SeqCst) {
            Flow::Continue
        } else {
            Flow::Exit
        }
    }

    fn paint(&mut self, ui: &mut egui::Ui) {
        self.draw(ui);
    }

    fn clear_color(&self) -> [f32; 4] {
        // Fully transparent; the panel we draw is the only opaque thing.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn on_exit(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
