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

    try_hyprland_float(runner);

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
    partial: String,
    finals: Vec<String>,
    buffered: usize,
    show_settings: bool,
    pipeline_failed: bool,
}

const LANGUAGES: &[&str] = &["auto", "pt", "en", "es", "fr", "de", "ja", "zh"];

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

    fn drain_events(&mut self) {
        loop {
            let event = match self.rx.try_recv() {
                Ok(event) => event,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // The sender lives in the pipeline thread; a disconnect means
                    // the thread died (panic or error) while we are still running.
                    self.pipeline_failed = true;
                    break;
                }
            };
            match event {
                TranscriptEvent::Partial(s) => self.partial = s,
                TranscriptEvent::PartialCleared => self.partial.clear(),
                TranscriptEvent::Final(s) => {
                    self.partial.clear();
                    self.finals.push(s);
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Buffered(n) => {
                    self.partial.clear();
                    self.buffered = n;
                }
                TranscriptEvent::Sent(s) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push(format!("[sent] {s}"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::AwaitingConfirmation(what) => {
                    self.partial.clear();
                    self.finals.push(format!("[confirm?] {what}"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::ConfirmationCancelled => {
                    self.partial.clear();
                    self.finals.push("[confirm?] cancelled".to_string());
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Newline => {
                    self.partial.clear();
                    self.finals.push("[newline]".to_string());
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Cancelled => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push("[cancelled] buffer cleared".to_string());
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::SentTo(_, target, score) => {
                    self.partial.clear();
                    self.buffered = 0;
                    // M2.3: the overlay shows where the text went and how sure
                    // the resolver was.
                    self.finals.push(format!("[sent_to] {target} ({score:.2})"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
            }
        }
    }
}

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

                for line in &self.finals {
                    ui.label(
                        egui::RichText::new(line)
                            .color(egui::Color32::WHITE)
                            .size(18.0),
                    );
                }
                if !self.partial.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.partial)
                            .color(egui::Color32::from_gray(180))
                            .italics()
                            .size(18.0),
                    );
                }
                if self.finals.is_empty() && self.partial.is_empty() && self.buffered == 0 {
                    let hint = match self.send_word() {
                        Some(w) => {
                            format!("[ speak into the mic \u{2014} say \"{w}\" to send ]")
                        }
                        None => "[ speak into the mic \u{2014} dictation only ]".to_string(),
                    };
                    ui.label(
                        egui::RichText::new(hint)
                            .color(egui::Color32::from_gray(120))
                            .italics()
                            .size(14.0),
                    );
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

                let remaining = (ui.available_height() - 28.0).max(8.0);
                ui.add_space(remaining);

                ui.horizontal(|ui| {
                    if ui.button("\u{2699} Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }

                    let mode_label = match self.settings.lock().unwrap().mode {
                        TranscribeMode::Input => "\u{1f4dd} Input Mode",
                        TranscribeMode::Translate => "\u{1f310} Translate Mode",
                        TranscribeMode::Enter => "\u{23ce} Enter Mode",
                        TranscribeMode::Command => "\u{1f5a5} Command Mode",
                    };
                    if ui.button(mode_label).clicked() {
                        let mut s = self.settings.lock().unwrap();
                        s.mode = match s.mode {
                            TranscribeMode::Input => TranscribeMode::Translate,
                            TranscribeMode::Translate => TranscribeMode::Enter,
                            TranscribeMode::Enter => TranscribeMode::Command,
                            TranscribeMode::Command => TranscribeMode::Input,
                        };
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
mod tests {
    use super::*;

    fn app_with_channel() -> (OverlayApp, crossbeam_channel::Sender<TranscriptEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let running = Arc::new(AtomicBool::new(true));
        let settings = Arc::new(Mutex::new(AppSettings {
            language: "pt".to_string(),
            mode: TranscribeMode::Enter,
            detected_language: None,
        }));
        let config = Arc::new(crate::config::Config::embedded());
        (OverlayApp::new(rx, running, settings, config), tx)
    }

    #[test]
    fn dead_pipeline_sets_failure_state() {
        // Dropping the sender is what a panicking pipeline thread does: the
        // overlay must notice instead of looking normal.
        let (mut app, tx) = app_with_channel();
        drop(tx);
        app.drain_events();
        assert!(app.pipeline_failed);
    }

    #[test]
    fn live_pipeline_does_not_set_failure_state() {
        let (mut app, tx) = app_with_channel();
        tx.send(TranscriptEvent::Partial("hello".to_string()))
            .unwrap();
        app.drain_events();
        assert!(!app.pipeline_failed);
        assert_eq!(app.partial, "hello");
    }
}
