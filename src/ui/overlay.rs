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
        Box::new(|_cc| Ok(Box::new(OverlayApp::new(rx, running, settings)))),
    )
    .map_err(|e| anyhow!("eframe error: {e}"))?;

    Ok(())
}

struct OverlayApp {
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    partial: String,
    finals: Vec<String>,
    buffered: usize,
    show_settings: bool,
}

const LANGUAGES: &[&str] = &["auto", "pt", "en", "es", "fr", "de", "ja", "zh"];

impl OverlayApp {
    fn new(
        rx: Receiver<TranscriptEvent>,
        running: Arc<AtomicBool>,
        settings: Arc<Mutex<AppSettings>>,
    ) -> Self {
        Self {
            rx,
            running,
            settings,
            partial: String::new(),
            finals: Vec::new(),
            buffered: 0,
            show_settings: false,
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
        while let Ok(event) = self.rx.try_recv() {
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
                TranscriptEvent::SentTo(_, target) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push(format!("[sent_to] {target}"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
            }
        }

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
                    ui.label(
                        egui::RichText::new(
                            "[ speak into the mic \u{2014} say \"envia\" to send ]",
                        )
                        .color(egui::Color32::from_gray(120))
                        .italics()
                        .size(14.0),
                    );
                }
                if self.buffered > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} line(s) buffered \u{2014} say \"cambio\" to send",
                            self.buffered
                        ))
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
                    };
                    if ui.button(mode_label).clicked() {
                        let mut s = self.settings.lock().unwrap();
                        s.mode = match s.mode {
                            TranscribeMode::Input => TranscribeMode::Translate,
                            TranscribeMode::Translate => TranscribeMode::Enter,
                            TranscribeMode::Enter => TranscribeMode::Input,
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
