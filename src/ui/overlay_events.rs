//! Turning pipeline events into overlay state.
//!
//! Split out of overlay.rs when it crossed the size ceiling; this is the one
//! place where a TranscriptEvent becomes something on screen.

use super::{Line, OverlayApp};
use crate::TranscriptEvent;

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

    fn push_line(&mut self, text: impl Into<String>) {
        self.finals.push(Line::new(text));
        self.trim_history();
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
                TranscriptEvent::Partial(s) => self.partial = s,
                TranscriptEvent::PartialCleared => self.partial.clear(),
                TranscriptEvent::Final(s) => {
                    self.partial.clear();
                    if !is_speech(&s) {
                        continue;
                    }
                    if let Some((_, ref mut n)) = self.recording {
                        *n += 1;
                    }
                    self.push_line(s);
                }
                TranscriptEvent::Buffered(n) => {
                    self.partial.clear();
                    self.buffered = n;
                }
                TranscriptEvent::Sent(s) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.push_line(format!("[sent] {s}"));
                }
                TranscriptEvent::SessionStarted => {
                    self.partial.clear();
                    self.recording = Some((std::time::Instant::now(), 0));
                }
                TranscriptEvent::SessionStopped(path, lines) => {
                    self.recording = None;
                    self.push_line(format!("[sessão] {lines} falas -> {path}"));
                }
                TranscriptEvent::Translated { original, text } => {
                    // Shown under its original, which stays visible: what was
                    // actually said is never replaced by a guess.
                    self.attach_translation(&original, text);
                }
                TranscriptEvent::AwaitingConfirmation(what) => {
                    self.partial.clear();
                    self.push_line(format!("[confirm?] {what}"));
                }
                TranscriptEvent::ConfirmationCancelled => {
                    self.partial.clear();
                    self.push_line("[confirm?] cancelled");
                }
                TranscriptEvent::Newline => {
                    self.partial.clear();
                    self.push_line("[newline]");
                }
                TranscriptEvent::Cancelled => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.push_line("[cancelled] buffer cleared");
                }
                TranscriptEvent::SentTo(_, target, score) => {
                    self.partial.clear();
                    self.buffered = 0;
                    // M2.3: the overlay shows where the text went and how sure
                    // the resolver was.
                    self.push_line(format!("[sent_to] {target} ({score:.2})"));
                }
            }
        }
    }
}
