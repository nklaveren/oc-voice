//! Drawing the overlay.
//!
//! Split from overlay.rs at the size ceiling, along the boundary that was
//! already there: overlay_events.rs turns pipeline events into state, and
//! this turns state into pixels. Nothing here mutates transcription state.

use super::*;

impl OverlayApp {
    pub(super) fn draw(&mut self, ui: &mut egui::Ui) {
        // The panel's own padding comes off the space it is given. Asking for
        // the full `available_size()` *inside* a frame that then adds 32x24
        // around it makes the content taller than the surface, and what falls
        // off the bottom is the control row.
        //
        // It never showed under eframe: its CentralPanel had already taken a
        // margin out of `available_size()`, so the overflow fitted in the
        // slack. On a layer surface the Ui spans the whole thing and there is
        // no slack — the drawing was relying on its host, which is exactly
        // what the `OverlayHost` seam exists to stop.
        const PAD_X: i8 = 16;
        const PAD_Y: i8 = 12;
        let padding = egui::vec2(PAD_X as f32 * 2.0, PAD_Y as f32 * 2.0);
        let panel_size = (ui.available_size() - padding).max(egui::Vec2::ZERO);
        let bg = egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(200))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(PAD_X, PAD_Y));

        bg.show(ui, |ui| {
            ui.set_min_size(panel_size);
            ui.set_width(panel_size.x);

            ui.vertical(|ui| {
                ui.set_width(ui.available_width());

                if let Some((since, lines)) = self.recording {
                    let secs = since.elapsed().as_secs();
                    // Blinks so it cannot be mistaken for a static label.
                    // ASCII, like the translation marker: the bundled font has
                    // no ⏺/◯ and drew an empty box for both, which blinks
                    // exactly as well as nothing at all.
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
                let idle = self.partial_mic.is_empty() && self.partial_system.is_empty();
                let hint = if self.finals.is_empty() && idle {
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
                                    egui::RichText::new(&line.text)
                                        .color(speaker_color(line.source))
                                        .size(18.0),
                                )
                                // Without this, long utterances are cut at the
                                // window edge instead of wrapping.
                                .wrap(),
                            );

                            // M7.2: the translation sits under its own
                            // original, tinted and marked, so it is never
                            // mistaken for what was said — and it stays there.
                            // Scrolling back through a meeting must show both.
                            if let Some(ref pt) = line.translation {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("{TRANSLATION_MARKER}{pt}"))
                                            .color(egui::Color32::from_rgb(120, 200, 255))
                                            .size(18.0),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                        // Both in-progress utterances, each in its speaker's
                        // colour: while you talk over a meeting there are two,
                        // and neither should overwrite the other.
                        for (partial, source) in [
                            (&self.partial_system, crate::Source::System),
                            (&self.partial_mic, crate::Source::Mic),
                        ] {
                            if partial.is_empty() {
                                continue;
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(partial)
                                        .color(speaker_color(source).gamma_multiply(0.7))
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

                    // Starting and stopping were spoken-only, which fails in
                    // both directions during a call: saying the stop word out
                    // loud announces that you were recording, and in a silent
                    // room there is no utterance to carry the command at all.
                    let recording = self.recording.is_some();
                    // No glyph: ● and ■ live in the same block as the ◯ that
                    // already rendered as an empty box. Colour carries it.
                    let label = if recording {
                        egui::RichText::new("Parar")
                            .color(egui::Color32::from_rgb(255, 90, 90))
                            .strong()
                    } else {
                        egui::RichText::new("Gravar")
                    };
                    if ui.button(label).clicked() {
                        let request = if recording {
                            crate::SessionRequest::Stop
                        } else {
                            crate::SessionRequest::Start
                        };
                        // The pipeline thread owns the session; this only asks.
                        crate::lock_settings(&self.settings).session_request = Some(request);
                    }

                    // Opening the record is only useful because the file is
                    // written as the session runs; before that there was
                    // nothing on disk to open until someone said the stop word.
                    let session = self.session_path.clone();
                    let button =
                        ui.add_enabled(session.is_some(), egui::Button::new("\u{1f4c4} Ata"));
                    if let Some(ref path) = session {
                        button.clone().on_hover_text(path.as_str());
                        if button.clicked() {
                            open_session_file(&self.runner, path);
                        }
                    }

                    // Emoji come from egui's emoji font; the arrow and return
                    // symbols did not, and rendered as empty boxes.
                    let current_mode = self.settings.lock().unwrap().mode;
                    let mode_label = match current_mode {
                        TranscribeMode::Enter => "\u{1f3a4} Microfone",
                        // No target language in the label: whether a
                        // translation appears depends on the model being
                        // installed, and a label that promises one when none
                        // is loaded is worse than no label.
                        TranscribeMode::Translate => "\u{1f310} Áudio do sistema",
                    };
                    if ui.button(mode_label).clicked() {
                        let mut s = self.settings.lock().unwrap();
                        s.mode = s.mode.next();
                    }
                    // Always visible, not only while the scrollback is empty:
                    // forgetting what a mode does happens mid-session, which
                    // is exactly when an empty-state hint is gone.
                    ui.label(
                        egui::RichText::new(mode_hint(current_mode))
                            .color(egui::Color32::from_gray(130))
                            .italics()
                            .size(13.0),
                    );

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
}
