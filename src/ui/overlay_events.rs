//! Turning pipeline events into overlay state.
//!
//! Split out of overlay.rs when it crossed the size ceiling; this is the one
//! place where a TranscriptEvent becomes something on screen.

use super::OverlayApp;
use crate::TranscriptEvent;

/// A meeting is an hour of talking; four lines of scrollback was a debugging
/// default that survived into a subtitle window. Kept bounded so memory does
/// not grow without limit on a long session.
const MAX_HISTORY: usize = 400;

impl OverlayApp {
    fn trim_history(&mut self) {
        if self.finals.len() > MAX_HISTORY {
            let excess = self.finals.len() - MAX_HISTORY;
            self.finals.drain(..excess);
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
                    self.translated = None;
                    if let Some((_, ref mut n)) = self.recording {
                        *n += 1;
                    }
                    self.finals.push(s);
                    self.trim_history();
                }
                TranscriptEvent::Buffered(n) => {
                    self.partial.clear();
                    self.buffered = n;
                }
                TranscriptEvent::Sent(s) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push(format!("[sent] {s}"));
                    self.trim_history();
                }
                TranscriptEvent::SessionStarted => {
                    self.partial.clear();
                    self.recording = Some((std::time::Instant::now(), 0));
                }
                TranscriptEvent::SessionStopped(path, lines) => {
                    self.recording = None;
                    self.finals.push(format!("[sessão] {lines} falas → {path}"));
                }
                TranscriptEvent::Translated(t) => {
                    // Shown under the original; the original stays visible so
                    // what was actually said is never replaced by a guess.
                    self.translated = Some(t);
                }
                TranscriptEvent::AwaitingConfirmation(what) => {
                    self.partial.clear();
                    self.finals.push(format!("[confirm?] {what}"));
                    self.trim_history();
                }
                TranscriptEvent::ConfirmationCancelled => {
                    self.partial.clear();
                    self.finals.push("[confirm?] cancelled".to_string());
                    self.trim_history();
                }
                TranscriptEvent::Newline => {
                    self.partial.clear();
                    self.finals.push("[newline]".to_string());
                    self.trim_history();
                }
                TranscriptEvent::Cancelled => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push("[cancelled] buffer cleared".to_string());
                    self.trim_history();
                }
                TranscriptEvent::SentTo(_, target, score) => {
                    self.partial.clear();
                    self.buffered = 0;
                    // M2.3: the overlay shows where the text went and how sure
                    // the resolver was.
                    self.finals.push(format!("[sent_to] {target} ({score:.2})"));
                    self.trim_history();
                }
            }
        }
    }
}
