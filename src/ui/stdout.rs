//! Stdout mirror of TranscriptEvent — the terminal view of what the overlay
//! shows. Byte-for-byte stable: scripts consume this stream.

use crate::TranscriptEvent;
use crossbeam_channel::Sender;
use std::io::Write;

/// Fan-out: send to the UI channel AND echo to stdout for debugging.
pub fn emit(tx: &Sender<TranscriptEvent>, event: TranscriptEvent) {
    match &event {
        TranscriptEvent::Partial { text, source } => {
            write_stdout(&format!("\r\x1b[2K[partial:{}] {text}", source.label()))
        }
        TranscriptEvent::PartialCleared(_) => write_stdout("\r\x1b[2K"),
        TranscriptEvent::Final { text, source } => {
            write_stdout(&format!("\r\x1b[2K[final:{}] {text}\n", source.label()))
        }
        TranscriptEvent::VoiceLocked(n) => {
            write_stdout(&format!("\r\x1b[2K[voz]      gravada de {n} trecho(s)\n"))
        }
        TranscriptEvent::VoiceRejected(score) => {
            write_stdout(&format!("\r\x1b[2K[voz]      ignorado ({score:.2})\n"))
        }
        TranscriptEvent::Buffered(n) => write_stdout(&format!(
            "\r\x1b[2K[buffered] {n} line(s) waiting for keyword"
        )),
        TranscriptEvent::Sent(s) => write_stdout(&format!("\r\x1b[2K[sent]     {s}\n")),
        TranscriptEvent::Newline => write_stdout("\r\x1b[2K[newline]  Shift+Return\n"),
        TranscriptEvent::Cancelled => write_stdout("\r\x1b[2K[cancelled] buffer cleared\n"),
        TranscriptEvent::SentTo(_, target, score) => {
            write_stdout(&format!("\r\x1b[2K[sent_to]  {target} ({score:.2})\n"))
        }
        TranscriptEvent::Translated { text, .. } => {
            write_stdout(&format!("\r\x1b[2K[pt]      {text}\n"))
        }
        TranscriptEvent::SessionStarted(path) => {
            write_stdout(&format!("\r\x1b[2K[sessão]  gravando -> {path}\n"))
        }
        TranscriptEvent::SessionStopped(path, lines) => {
            write_stdout(&format!("\r\x1b[2K[sessão]  {lines} falas -> {path}\n"))
        }
        TranscriptEvent::AwaitingConfirmation(what) => {
            write_stdout(&format!("\r\x1b[2K[confirm?] {what}\n"))
        }
        TranscriptEvent::ConfirmationCancelled => write_stdout("\r\x1b[2K[confirm?] cancelled\n"),
    }
    let _ = tx.send(event);
}

/// \r-based overwrite of the current partial on stdout. Uses ANSI clear-EOL.
fn write_stdout(s: &str) {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = write!(h, "{s}");
    let _ = h.flush();
}
