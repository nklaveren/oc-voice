//! Drawing the overlay.
//!
//! Split from overlay.rs at the size ceiling, along the boundary that was
//! already there: overlay_events.rs turns pipeline events into state, and
//! this turns state into pixels. Nothing here mutates transcription state.

use super::*;

/// Text on a filled button, dark enough to read on any of the accents.
pub(super) const INK: egui::Color32 = egui::Color32::from_rgb(20, 20, 24);
/// The only irreversible control in the row.
pub(super) const DANGER: egui::Color32 = egui::Color32::from_rgb(235, 85, 85);
/// Teams' own light accent, the one it uses on dark surfaces. Its brand
/// purple sits too close to this panel to read as a colour at all.
const MEETING_COLOR: egui::Color32 = egui::Color32::from_rgb(123, 131, 235);

/// Pill buttons for the control row.
///
/// Scoped to the row rather than set globally: this overlay draws on top of
/// whatever is behind it, and restyling the whole context would also restyle
/// the Settings window, where plain widgets read better.
///
/// The values are chosen against a translucent dark panel — flat fills would
/// disappear into it, so each state separates by luminance rather than hue,
/// and every widget keeps a hairline so its edge survives a bright wallpaper.
pub(super) fn style_controls(ui: &mut egui::Ui) {
    let radius = egui::CornerRadius::same(9);
    let w = &mut ui.style_mut().visuals.widgets;
    for (state, fill, stroke) in [
        (&mut w.inactive, 32u8, 70u8),
        (&mut w.hovered, 56, 110),
        (&mut w.active, 78, 150),
    ] {
        state.corner_radius = radius;
        state.bg_fill = egui::Color32::from_gray(fill);
        state.weak_bg_fill = egui::Color32::from_gray(fill);
        state.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(stroke));
    }
    w.noninteractive.corner_radius = radius;
    // A disabled control still has to look like a control, or "Ata" before a
    // session exists reads as a rendering fault instead of as not-yet.
    w.noninteractive.bg_fill = egui::Color32::from_gray(24);
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(44));
    let spacing = ui.spacing_mut();
    spacing.button_padding = egui::vec2(10.0, 5.0);
    spacing.item_spacing.x = 8.0;
}

impl OverlayApp {
    pub(super) fn draw(&mut self, ui: &mut egui::Ui) {
        // Once, not per frame: `style_mut` clones the whole style behind an
        // Arc, and this asks egui to name a cursor for anything interactive.
        // The host is what turns that name into an image; between them, the
        // pointer stops wearing whatever it walked in from the last window.
        if !self.styled {
            self.styled = true;
            ui.ctx().global_style_mut(|s| {
                s.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
            });
        }

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
            .fill(egui::Color32::from_black_alpha(
                (self.layout.opacity * 255.0) as u8,
            ))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(PAD_X, PAD_Y));

        // Dragging the panel moves the overlay. There is no title bar to
        // grab and no window manager to grab it with, so the panel itself is
        // the handle.
        //
        // Registered **before** the contents, which is the whole trick. egui
        // resolves a press to the last widget registered under the pointer,
        // and when that one senses drag but not click it suppresses the click
        // outright — `hit_test_on_close`: "the top thing senses only drags, so
        // we ignore the click-widget". Added after the contents, as it was,
        // this handle covered the panel and swallowed every button on it:
        // measured on the running overlay, where no button would even
        // highlight anywhere on the surface, while the sliders — which sense
        // drag themselves — still worked. Registered first, it is the big
        // background egui expects to find under small widgets.
        let handle = ui.interact(
            ui.max_rect(),
            ui.id().with("overlay-drag"),
            egui::Sense::drag(),
        );
        // The panel is the only handle this window has; the cursor is the only
        // place that can be said.
        let handle = if handle.dragged() {
            handle.on_hover_cursor(egui::CursorIcon::Grabbing)
        } else {
            handle.on_hover_cursor(egui::CursorIcon::Grab)
        };
        bg.show(ui, |ui| {
            ui.set_min_size(panel_size);
            ui.set_width(panel_size.x);

            ui.vertical(|ui| {
                ui.set_width(ui.available_width());

                super::status::strip(self, ui);

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
                // Settings is a *mode*, not a window over this one. A layer
                // surface has a fixed extent: a dialog drawn past its edge
                // does not exist, and no z-order changes that. So the strip
                // shows configuration instead of speech, and the control row
                // below stays put so there is always a way back.
                if self.show_settings {
                    let h = scroll_height(ui.available_height(), CONTROLS_HEIGHT);
                    ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
                        self.settings_view(ui);
                    });
                } else {
                    let idle = self.partial_mic.is_empty() && self.partial_system.is_empty();
                    let hint = if self.finals.is_empty() && idle {
                        Some(match self.send_word() {
                            Some(w) => {
                                format!("[ speak into the mic \u{2014} say \"{w}\" to send ]")
                            }
                            None => "[ speak into the mic \u{2014} dictation only ]".to_string(),
                        })
                    } else {
                        None
                    };
                    let scroll_height = scroll_height(ui.available_height(), CONTROLS_HEIGHT);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        // Without this the scroll area is the drag target
                        // over most of the panel and there is nowhere left to
                        // grab it by. The wheel and the bar still scroll.
                        .scroll_source(
                            egui::containers::scroll_area::ScrollSource::MOUSE_WHEEL
                                | egui::containers::scroll_area::ScrollSource::SCROLL_BAR,
                        )
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
                                            egui::RichText::new(format!(
                                                "{TRANSLATION_MARKER}{pt}"
                                            ))
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
                }

                // Control row: always the last thing drawn, always inside the
                // panel because the scroll area above it is bounded.
                ui.horizontal(|ui| {
                    // Closing. Pinned to the right edge, as far from the controls
                    // you reach for during a call as the row allows — and laid
                    // out *before* them, because at the narrowest width the
                    // sliders allow the row overflows, and what gets pushed off
                    // has to be the mode hint rather than the only way out.
                    //
                    // A recorded session loses nothing to it: its file is written
                    // as the session runs. The send buffer does — the lines held
                    // back waiting for the keyword exist nowhere else — which is
                    // what the confirmation in there is for.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.quit_control(ui);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // At the narrowest the sliders allow, this half
                            // overflows. Clipped, it overflows *out of sight*
                            // instead of over the close control, where it
                            // would silently take its presses: an
                            // `interact_rect` is intersected with the clip
                            // rect, so this is a hit-testing statement as much
                            // as a visual one.
                            let room = ui.max_rect();
                            ui.shrink_clip_rect(room);
                            style_controls(ui);
                            if ui.button("\u{2699} Settings").clicked() {
                                self.set_settings_open(!self.show_settings);
                            }

                            // Starting and stopping were spoken-only, which fails in
                            // both directions during a call: saying the stop word out
                            // loud announces that you were recording, and in a silent
                            // room there is no utterance to carry the command at all.
                            let recording = self.recording.is_some();
                            // No glyph: ● and ■ live in the same block as the ◯ that
                            // already rendered as an empty box. Colour carries it —
                            // and while recording it carries it as a filled button,
                            // not just tinted text, because that is the one state
                            // nobody should have to read twice.
                            let record = if recording {
                                egui::Button::new(egui::RichText::new("Parar").color(INK).strong())
                                    .fill(DANGER)
                            } else {
                                egui::Button::new(egui::RichText::new("Gravar"))
                            };
                            if ui.add(record).clicked() {
                                let request = if recording {
                                    crate::SessionRequest::Stop
                                } else {
                                    crate::SessionRequest::Start
                                };
                                // The pipeline thread owns the session; this only asks.
                                crate::lock_settings(&self.settings).session_request =
                                    Some(request);
                            }

                            // Opening the record is only useful because the file is
                            // written as the session runs; before that there was
                            // nothing on disk to open until someone said the stop word.
                            let session = self.session_path.clone();
                            let button = ui
                                .add_enabled(session.is_some(), egui::Button::new("\u{1f4c4} Ata"));
                            if let Some(ref path) = session {
                                button.clone().on_hover_text(path.as_str());
                                if button.clicked() {
                                    open_session_file(&self.runner, path);
                                }
                            }

                            // Emoji come from egui's emoji font; the arrow and return
                            // symbols did not, and rendered as empty boxes.
                            let current_mode = self.settings.lock().unwrap().mode;
                            // Named for what you are doing, not for where the audio
                            // comes from. "System audio" described the plumbing; the
                            // two modes actually differ by whether you are talking to
                            // the machine or following a room.
                            let mode_label = match current_mode {
                                TranscribeMode::Enter => "\u{1f3a4} Agent",
                                // No target language in the label: whether a
                                // translation appears depends on the model being
                                // installed, and a label that promises one when none
                                // is loaded is worse than no label.
                                TranscribeMode::Translate => "\u{1f310} Meeting",
                            };
                            // Your voice keeps the transcript's own green. The
                            // meeting gets Teams' blue rather than the
                            // transcript's white — the button names where the
                            // audio comes from, and white on a button reads as
                            // no colour chosen at all. The subtitles stay
                            // white, because that is what holds up at speed
                            // over whatever wallpaper is behind them.
                            let accent = match current_mode {
                                TranscribeMode::Enter => MIC_COLOR,
                                TranscribeMode::Translate => MEETING_COLOR,
                            };
                            // Accent in the ink, not in the fill. A fixed fill and a
                            // fixed stroke leave nothing for `style_controls` to
                            // vary, so this was the one control in the row that
                            // could not show it was under the pointer.
                            let mode_button = egui::Button::new(
                                egui::RichText::new(mode_label).color(accent).strong(),
                            );
                            if ui.add(mode_button).clicked() {
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
            });
        });
        if handle.dragged() {
            self.drag_by(handle.drag_delta());
        }
        if handle.drag_stopped() {
            // Written down when the drag settles, not on every frame of it.
            self.save_layout();
        }
    }
}
