use crate::process::CommandRunner;
use crate::ui::host::{Flow, Geometry, LayoutRequest, OverlayUi};
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
/// Matches the `windowrule=opacity 0.85` this replaces closely enough that
/// nobody notices the day the window rule stopped applying.
pub const OVERLAY_OPACITY: f32 = 0.82;

pub fn run_overlay(
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    runner: Arc<dyn CommandRunner>,
    config: Arc<crate::config::Config>,
) -> Result<()> {
    // Start where it was left. Config says where it goes the first time;
    // after that the overlay's own controls have the last word, and forcing
    // it back to the config value on every launch would make those controls
    // feel broken.
    let mut geometry = Geometry {
        width: OVERLAY_W,
        height: OVERLAY_H,
        bottom_margin: OVERLAY_BOTTOM_MARGIN,
        x_offset: 0.0,
    };
    let saved = state::Saved::load();
    if let Some(v) = saved.width {
        geometry.width = v;
    }
    if let Some(v) = saved.height {
        geometry.height = v;
    }
    if let Some(v) = saved.bottom_margin {
        geometry.bottom_margin = v;
    }
    if let Some(v) = saved.x_offset {
        geometry.x_offset = v;
    }
    let monitor = saved
        .monitor
        .clone()
        .unwrap_or_else(|| config.overlay_monitor().to_string());

    // Which surface the overlay lives on, and what that costs to keep in
    // place, are the host's business — not this function's and not the UI's.
    let host = crate::ui::host::default_host(
        geometry,
        runner.clone(),
        monitor.clone(),
        config.overlay_surface(),
    );
    let mut app = OverlayApp::new(rx, running, settings, config, runner);
    app.monitor = monitor;
    host.run(Box::new(app))
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
    /// Whether the one-off style tweak has been applied to the context.
    styled: bool,
    pub(super) pipeline_failed: bool,
    /// When a session is recording, and how many lines it holds (M7.1).
    /// Recording without a visible indication is not acceptable.
    pub(super) recording: Option<(std::time::Instant, usize)>,
    /// How the overlay is laid out, and what it has asked the host to change.
    ///
    /// It lives here rather than in the host because the controls are here:
    /// a layer surface cannot be dragged by a window manager, so the only
    /// place left to move it from is the overlay itself.
    pub(super) layout: Layout,
    pub(super) layout_request: Option<LayoutRequest>,
    /// Which output the overlay was last moved to, remembered across runs.
    pub(super) monitor: String,
    /// When the close button was armed, if it is. A layer surface has no
    /// dialog to ask in, so the question lives in the control row itself.
    pub(super) quit_armed: Option<std::time::Instant>,
    /// The host's answer for how big the surface may get. `None` until the
    /// first configure, and the controls stay conservative until then.
    pub(super) max_size: Option<(f32, f32)>,
}

/// The knobs on the Settings panel's window section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Layout {
    pub width: f32,
    pub height: f32,
    pub bottom_margin: f32,
    /// How far from the monitor's horizontal centre. Dragging sets this;
    /// zero is centred, which is where it starts.
    pub x_offset: f32,
    /// 0.0 fully transparent, 1.0 opaque. Replaces what
    /// `windowrule=opacity` used to do — a layer surface gives the compositor
    /// nothing to dim, so the panel dims itself.
    pub opacity: f32,
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
        TranscribeMode::Translate => "a reunião: legenda e traduz",
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
    /// Write the layout down so it survives a restart. Called on every change
    /// that settles — a slider release, an arrow, closing the panel — rather
    /// than every frame, because a drag is sixty changes a second.
    pub(super) fn save_layout(&self) {
        state::Saved {
            width: Some(self.layout.width),
            height: Some(self.layout.height),
            bottom_margin: Some(self.layout.bottom_margin),
            x_offset: Some(self.layout.x_offset),
            opacity: Some(self.layout.opacity),
            monitor: (!self.monitor.is_empty()).then(|| self.monitor.clone()),
        }
        .store();
    }

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
            styled: false,
            pipeline_failed: false,
            recording: None,
            layout: {
                let mut l = Layout {
                    width: OVERLAY_W,
                    height: OVERLAY_H,
                    bottom_margin: OVERLAY_BOTTOM_MARGIN,
                    x_offset: 0.0,
                    opacity: OVERLAY_OPACITY,
                };
                state::Saved::load().apply_to(&mut l);
                l
            },
            quit_armed: None,
            layout_request: None,
            monitor: String::new(),
            max_size: None,
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

#[path = "overlay_settings.rs"]
mod settings_panel;

#[path = "overlay_quit.rs"]
mod quit;

#[path = "overlay_state.rs"]
mod state;

impl OverlayUi for OverlayApp {
    fn tick(&mut self) -> Flow {
        // Drain all pending transcription events before painting.
        self.drain_events();
        // A keybinding can ask for the panel. Taken, not read, so one press
        // is one toggle.
        if std::mem::take(&mut crate::lock_settings(&self.settings).toggle_settings) {
            self.set_settings_open(!self.show_settings);
        }
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

    fn set_bounds(&mut self, max_w: f32, max_h: f32) {
        self.max_size = Some((max_w, max_h));
        // A saved size from a bigger monitor must not survive the move to a
        // smaller one, or the overlay comes back off-screen with no way to
        // reach the slider that would fix it.
        self.clamp_layout();
    }

    fn take_layout_request(&mut self) -> Option<LayoutRequest> {
        self.layout_request.take()
    }

    fn on_exit(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
