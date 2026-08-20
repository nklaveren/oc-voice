//! Stdout mirror of TranscriptEvent — the terminal view of what the overlay
//! shows. Byte-for-byte stable: scripts consume this stream.

use crate::TranscriptEvent;
use crossbeam_channel::Sender;
use std::io::Write;

/// Fan-out: send to the UI channel AND echo to stdout for debugging.
pub fn emit(tx: &Sender<TranscriptEvent>, event: TranscriptEvent) {
    match &event {
        TranscriptEvent::Partial(s) => write_stdout(&format!("\r\x1b[2K[partial] {s}")),
        TranscriptEvent::PartialCleared => write_stdout("\r\x1b[2K"),
        TranscriptEvent::Final(s) => write_stdout(&format!("\r\x1b[2K[final]   {s}\n")),
        TranscriptEvent::Buffered(n) => write_stdout(&format!(
            "\r\x1b[2K[buffered] {n} line(s) waiting for keyword"
        )),
        TranscriptEvent::Sent(s) => write_stdout(&format!("\r\x1b[2K[sent]     {s}\n")),
        TranscriptEvent::Newline => write_stdout("\r\x1b[2K[newline]  Shift+Return\n"),
        TranscriptEvent::Cancelled => write_stdout("\r\x1b[2K[cancelled] buffer cleared\n"),
        TranscriptEvent::SentTo(_, target, score) => {
            write_stdout(&format!("\r\x1b[2K[sent_to]  {target} ({score:.2})\n"))
        }
        TranscriptEvent::Translated(s) => write_stdout(&format!("\r\x1b[2K[pt]      {s}\n")),
        TranscriptEvent::SessionStarted => write_stdout("\r\x1b[2K[sessão]  gravando\n"),
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
