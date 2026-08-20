use crate::process::CommandRunner;
use crate::wm::hyprland::try_hyprland_float;
use crate::{AppSettings, TranscribeMode, TranscriptEvent};
use anyhow::{anyhow, Result};
use crossbeam_channel::Receiver;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn run_overlay(
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    runner: Arc<dyn CommandRunner>,
    config: Arc<crate::config::Config>,
) -> Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("oc-voice")
        .with_app_id("oc-voice")
        .with_inner_size([900.0, 350.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(true);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    try_hyprland_float(runner, config.overlay_monitor().to_string());

    eframe::run_native(
        "oc-voice",
        options,
        Box::new(|_cc| Ok(Box::new(OverlayApp::new(rx, running, settings, config)))),
    )
    .map_err(|e| anyhow!("eframe error: {e}"))?;

    Ok(())
}

struct OverlayApp {
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    config: Arc<crate::config::Config>,
    pub(super) partial: String,
    pub(super) finals: Vec<String>,
    pub(super) buffered: usize,
    show_settings: bool,
    pub(super) pipeline_failed: bool,
    /// Display-only translation of the most recent final (M7.2).
    pub(super) translated: Option<String>,
    /// When a session is recording, and how many lines it holds (M7.1).
    /// Recording without a visible indication is not acceptable.
    pub(super) recording: Option<(std::time::Instant, usize)>,
}

const LANGUAGES: &[&str] = &["auto", "pt", "en", "es", "fr", "de", "ja", "zh"];

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

impl OverlayApp {
    fn new(
        rx: Receiver<TranscriptEvent>,
        running: Arc<AtomicBool>,
        settings: Arc<Mutex<AppSettings>>,
        config: Arc<crate::config::Config>,
    ) -> Self {
        Self {
            rx,
            running,
            settings,
            config,
            partial: String::new(),
            finals: Vec::new(),
            buffered: 0,
            show_settings: false,
            pipeline_failed: false,
            translated: None,
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

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // fully transparent background; we draw our own panel on top
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain all pending transcription events before painting the UI.
        self.drain_events();

        // If the pipeline has signalled shutdown, close the window.
        if !self.running.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Repaint periodically so new events show up even without user input.
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let panel_size = ui.available_size();
        let bg = egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(200))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 12));

        bg.show(ui, |ui| {
            ui.set_min_size(panel_size);
            ui.set_width(panel_size.x);

            ui.vertical(|ui| {
                ui.set_width(ui.available_width());

                if let Some((since, lines)) = self.recording {
                    let secs = since.elapsed().as_secs();
                    // Blinks so it cannot be mistaken for a static label.
                    let dot = if secs % 2 == 0 {
                        "\u{23fa}"
                    } else {
                        "\u{25cb}"
                    };
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

                if self.pipeline_failed {
                    ui.label(
                        egui::RichText::new(
                            "[ audio pipeline crashed \u{2014} close and restart oc-voice ]",
                        )
                        .color(egui::Color32::from_rgb(255, 90, 90))
                        .strong()
                        .size(18.0),
                    );
                }

                // Scrollback, pinned to the bottom: a meeting produces far
                // more lines than fit, and the newest must stay visible
                // without the user chasing it.
                //
                // The height is bounded on purpose. `auto_shrink([false,
                // false])` makes the area claim every remaining pixel, so
                // anything laid out after it lands past the bottom edge —
                // which is how the control row and the empty-state hint
                // vanished, leaving a black rectangle with no way to switch
                // modes. Reserve the chrome first, give the scroll what's left.
                // Resolved before the closure borrows `self` for the lines.
                let hint = if self.finals.is_empty() && self.partial.is_empty() {
                    Some(match self.send_word() {
                        Some(w) => format!("[ speak into the mic \u{2014} say \"{w}\" to send ]"),
                        None => "[ speak into the mic \u{2014} dictation only ]".to_string(),
                    })
                } else {
                    None
                };
                let scroll_height = scroll_height(ui.available_height(), CONTROLS_HEIGHT);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(scroll_height)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for line in &self.finals {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(line)
                                        .color(egui::Color32::WHITE)
                                        .size(18.0),
                                )
                                // Without this, long utterances are cut at the
                                // window edge instead of wrapping.
                                .wrap(),
                            );
                        }

                        // M7.2: the translation sits under the last original,
                        // tinted and marked, so it is never mistaken for what
                        // was said. The original stays on screen.
                        if let Some(ref pt) = self.translated {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("\u{21b3} {pt}"))
                                        .color(egui::Color32::from_rgb(120, 200, 255))
                                        .size(18.0),
                                )
                                .wrap(),
                            );
                        }
                        if !self.partial.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.partial)
                                        .color(egui::Color32::from_gray(180))
                                        .italics()
                                        .size(18.0),
                                )
                                .wrap(),
                            );
                        }

                        // The empty state belongs to the scrollback, not below
                        // it — placed after the area it would be off-screen.
                        if let Some(hint) = hint {
                            ui.label(
                                egui::RichText::new(hint)
                                    .color(egui::Color32::from_gray(120))
                                    .italics()
                                    .size(14.0),
                            );
                        }
                    });

                // Control row: always the last thing drawn, always inside the
                // panel because the scroll area above it is bounded.
                ui.horizontal(|ui| {
                    if ui.button("\u{2699} Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }

                    let mode_label = match self.settings.lock().unwrap().mode {
                        TranscribeMode::Input => "\u{1f4dd} Input Mode",
                        TranscribeMode::Translate => "\u{1f310} System Audio \u{2192} EN",
                        TranscribeMode::Enter => "\u{23ce} Enter Mode",
                        TranscribeMode::Command => "\u{1f5a5} Command Mode",
                    };
                    if ui.button(mode_label).clicked() {
                        let mut s = self.settings.lock().unwrap();
                        s.mode = s.mode.next();
                    }

                    if self.buffered > 0 {
                        ui.label(
                            egui::RichText::new(match self.send_word() {
                                Some(w) => format!(
                                    "{} line(s) buffered \u{2014} say \"{w}\" to send",
                                    self.buffered
                                ),
                                None => format!("{} line(s) buffered", self.buffered),
                            })
                            .color(egui::Color32::from_rgb(255, 200, 80))
                            .size(14.0),
                        );
                    }
                });
            });
        });

        if self.show_settings {
            egui::Window::new("Settings")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .resizable(false)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Language:");
                    ui.horizontal(|ui| {
                        let current = self.settings.lock().unwrap().language.clone();
                        for lang in LANGUAGES {
                            if ui.selectable_label(current == *lang, *lang).clicked() {
                                self.settings.lock().unwrap().language = lang.to_string();
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        self.show_settings = false;
                    }
                });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
