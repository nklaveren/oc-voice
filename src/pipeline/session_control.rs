//! Opening, closing and driving a recorded session — M7.1.
//!
//! Split from the transcription loop at the size ceiling, on the line that was
//! already there: up there an utterance is produced, and here it is decided
//! what a *session* does about it. The one rule worth restating is that a
//! session is created in exactly one place, so the spoken command and the
//! overlay's button cannot drift into behaving differently.

use super::*;

/// Open a recording. No-op if one is already open.
///
/// The single place a session is created, so the spoken command and the
/// overlay's button cannot drift into behaving differently.
pub(super) fn start_session(
    source: Source,
    recording: &mut Option<crate::session::Session>,
    tx: &Sender<TranscriptEvent>,
) -> bool {
    if recording.is_some() {
        return false;
    }
    let session = crate::session::Session::start(source.label());
    // Create the file now, empty. The overlay's button needs somewhere to
    // point before the first sentence is finished, and a file that exists and
    // grows is easier to trust than one that appears at the end.
    let path = match session.write(&crate::session::default_dir()) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            error!(error = ?e, "failed to create session file");
            String::new()
        }
    };
    *recording = Some(session);
    emit(tx, TranscriptEvent::SessionStarted(path));
    true
}

/// Close a recording. No-op if none is open.
pub(super) fn stop_session(
    recording: &mut Option<crate::session::Session>,
    tx: &Sender<TranscriptEvent>,
) -> bool {
    let Some(s) = recording.take() else {
        return false;
    };
    match s.write(&crate::session::default_dir()) {
        Ok(path) => {
            info!(path = %path.display(), lines = s.line_count(), "session closed");
            emit(
                tx,
                TranscriptEvent::SessionStopped(path.display().to_string(), s.line_count()),
            );
        }
        Err(e) => error!(error = ?e, "failed to write session"),
    }
    true
}

/// Returns true when the utterance was a session command and is fully handled.
pub(super) fn session_control(source: Source, trimmed: &str, ctx: &mut Ctx<'_>) -> bool {
    let vocab = config::active_vocab(ctx.config, ctx.settings);
    match vocab.and_then(|v| commands::classify(trimmed, v, ctx.config.threshold())) {
        Some(commands::VoiceCommand::SessionStart) => start_session(source, ctx.recording, ctx.tx),
        Some(commands::VoiceCommand::SessionStop) => stop_session(ctx.recording, ctx.tx),
        Some(commands::VoiceCommand::SetMode(name)) => switch_mode(&name, ctx),
        Some(commands::VoiceCommand::Help) => {
            let Some(vocab) = config::active_vocab(ctx.config, ctx.settings) else {
                return false;
            };
            for line in commands::help_lines(vocab) {
                emit(ctx.tx, TranscriptEvent::notice(line));
            }
            true
        }
        _ => false,
    }
}
