//! Turning pipeline events into overlay state.
//!
//! Split out of overlay.rs when it crossed the size ceiling; this is the one
//! place where a TranscriptEvent becomes something on screen.

use super::{Line, OverlayApp};
use crate::{Source, TranscriptEvent};

/// A meeting is an hour of talking; four lines of scrollback was a debugging
/// default that survived into a subtitle window. Kept bounded so memory does
/// not grow without limit on a long session.
const MAX_HISTORY: usize = 400;

/// Whether an utterance is worth a line of its own.
///
/// Short segments make whisper emit bare punctuation — a screen of lone "."
/// lines between real speech. Nothing without a letter or digit in it is
/// something someone said.
fn is_speech(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

impl OverlayApp {
    fn trim_history(&mut self) {
        if self.finals.len() > MAX_HISTORY {
            let excess = self.finals.len() - MAX_HISTORY;
            self.finals.drain(..excess);
        }
    }

    fn push_line(&mut self, text: impl Into<String>, source: Source) {
        self.finals.push(Line::new(text, source));
        self.trim_history();
    }

    /// The in-progress slot for one stream. Two exist so a meeting and the
    /// microphone speaking at once do not overwrite each other.
    fn partial_mut(&mut self, source: Source) -> &mut String {
        match source {
            Source::Mic => &mut self.partial_mic,
            Source::System => &mut self.partial_system,
        }
    }

    /// Attach a translation to the line it was made from.
    ///
    /// Matched by text rather than by position: translation runs async in a
    /// worker, requests are dropped when it falls behind, and identical
    /// output is discarded — so "the last line" is regularly the wrong one.
    /// Searching from the newest backwards means a repeated sentence attaches
    /// to its most recent occurrence.
    fn attach_translation(&mut self, original: &str, text: String) {
        if let Some(line) = self
            .finals
            .iter_mut()
            .rev()
            .find(|l| l.translation.is_none() && l.text == original)
        {
            line.translation = Some(text);
        }
    }

    pub(super) fn drain_events(&mut self) {
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
                TranscriptEvent::Partial { text, source } => {
                    *self.partial_mut(source) = text;
                }
                TranscriptEvent::PartialCleared(source) => self.partial_mut(source).clear(),
                TranscriptEvent::Final { text, source } => {
                    self.partial_mut(source).clear();
                    if !is_speech(&text) {
                        continue;
                    }
                    if let Some((_, ref mut n)) = self.recording {
                        *n += 1;
                    }
                    self.push_line(text, source);
                }
                TranscriptEvent::Buffered(n) => {
                    self.partial_mic.clear();
                    self.buffered = n;
                }
                TranscriptEvent::Sent(s) => {
                    self.partial_mic.clear();
                    self.buffered = 0;
                    self.push_line(format!("[sent] {s}"), Source::Mic);
                }
                TranscriptEvent::SessionStarted(path) => {
                    self.partial_mic.clear();
                    self.recording = Some((std::time::Instant::now(), 0));
                    if !path.is_empty() {
                        self.session_path = Some(path);
                    }
                }
                TranscriptEvent::SessionStopped(path, lines) => {
                    self.recording = None;
                    // Kept after the session closes: reading back what was
                    // just recorded is exactly when the button is wanted.
                    self.session_path = Some(path.clone());
                    self.push_line(format!("[sessão] {lines} falas -> {path}"), Source::Mic);
                }
                TranscriptEvent::Translated { original, text } => {
                    // Shown under its original, which stays visible: what was
                    // actually said is never replaced by a guess.
                    self.attach_translation(&original, text);
                }
                TranscriptEvent::AwaitingConfirmation(what) => {
                    self.partial_mic.clear();
                    self.push_line(format!("[confirm?] {what}"), Source::Mic);
                }
                TranscriptEvent::ConfirmationCancelled => {
                    self.partial_mic.clear();
                    self.push_line("[confirm?] cancelled", Source::Mic);
                }
                TranscriptEvent::Newline => {
                    self.partial_mic.clear();
                    self.push_line("[newline]", Source::Mic);
                }
                TranscriptEvent::Cancelled => {
                    self.partial_mic.clear();
                    self.buffered = 0;
                    self.push_line("[cancelled] buffer cleared", Source::Mic);
                }
                TranscriptEvent::SentTo(_, target, score) => {
                    self.partial_mic.clear();
                    self.buffered = 0;
                    // M2.3: the overlay shows where the text went and how sure
                    // the resolver was.
                    self.push_line(format!("[sent_to] {target} ({score:.2})"), Source::Mic);
                }
            }
        }
    }
}
