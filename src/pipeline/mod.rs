//! The audio pipeline: capture, VAD segmentation, transcription, and routing
//! of every finalized utterance according to the current mode.

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info};
use whisper_rs::{WhisperContext, WhisperContextParameters};

use crate::asr;
use crate::commands;
use crate::config;
use crate::process::CommandRunner;
use crate::ui::stdout::emit;
use crate::{
    lock_settings, AppSettings, Source, TranscribeMode, TranscriptEvent, PARTIAL_MIN_SAMPLES,
    TARGET_SAMPLE_RATE,
};

mod stream;
use stream::{streams_for, Stream};

/// Everything the finalize path needs that is not the utterance itself.
/// Grouped because it was seven parameters threaded through two near-identical
/// copies of the same code, which is how the two copies drifted apart.
struct Ctx<'a> {
    config: &'a Arc<config::Config>,
    settings: &'a Arc<Mutex<AppSettings>>,
    tx: &'a Sender<TranscriptEvent>,
    runner: &'a Arc<dyn CommandRunner>,
    translator: &'a Option<Arc<crossbeam_channel::Sender<crate::translate::Request>>>,
    enter_buffer: &'a mut Vec<String>,
    pending: &'a mut Option<commands::PendingAction>,
    recording: &'a mut Option<crate::session::Session>,
}

/// Owns whisper, VAD, cpal, and runs the main transcription loop.
#[deny(clippy::unwrap_used)]
pub fn run_audio_pipeline(
    model_path: &str,
    running: Arc<AtomicBool>,
    tx: Sender<TranscriptEvent>,
    settings: Arc<Mutex<AppSettings>>,
    runner: Arc<dyn CommandRunner>,
    config: Arc<config::Config>,
    translator: Option<Arc<crossbeam_channel::Sender<crate::translate::Request>>>,
) -> Result<()> {
    info!(model = %model_path, "loading whisper model");
    let load_start = Instant::now();

    let mut ctx_params = WhisperContextParameters::default();
    #[cfg(feature = "cuda")]
    ctx_params.use_gpu(true);
    ctx_params.flash_attn(true);

    let ctx =
        WhisperContext::new_with_params(model_path, ctx_params).context("loading whisper model")?;
    info!(
        elapsed_ms = load_start.elapsed().as_millis(),
        "whisper model loaded"
    );

    // One whisper state for both streams: transcription is serialized through
    // this loop anyway, and a second state would double the KV cache for no
    // gain.
    let mut state = ctx.create_state().context("creating whisper state")?;
    let mut enter_buffer: Vec<String> = Vec::new();
    let mut pending: Option<commands::PendingAction> = None;
    let mut recording: Option<crate::session::Session> = None;

    let mut streams: Vec<Stream> = Vec::new();
    let mut last_mode: Option<TranscribeMode> = None;

    info!("speak into the mic; close the overlay window or press Ctrl+C to exit");

    while running.load(Ordering::SeqCst) {
        let mode = lock_settings(&settings).mode;

        // The button, checked every tick rather than on the next utterance:
        // stopping a recording must work in a silent room, which is exactly
        // the situation the spoken command cannot reach.
        if let Some(request) = lock_settings(&settings).session_request.take() {
            match request {
                crate::SessionRequest::Start => {
                    start_session(primary_source(mode), &mut recording, &tx);
                }
                crate::SessionRequest::Stop => {
                    stop_session(&mut recording, &tx);
                }
            }
        }

        if last_mode != Some(mode) || streams.is_empty() {
            streams.clear(); // Drop stops each capture thread.
            for (source, translate) in streams_for(mode) {
                match Stream::start(source, translate, runner.clone(), config.spoken_languages()) {
                    Ok(s) => streams.push(s),
                    Err(e) => error!(?source, error = ?e, "could not start capture"),
                }
            }
            last_mode = Some(mode);
            info!(
                ?mode,
                sources = ?streams.iter().map(|s| s.source).collect::<Vec<_>>(),
                "capture sources switched"
            );
        }

        let mut idle = true;
        for stream in streams.iter_mut() {
            let frames = stream.take_frames();
            if frames.is_empty() {
                continue;
            }
            idle = false;
            let preroll_ms = config
                .segmentation(mode == TranscribeMode::Translate)
                .preroll_ms;
            for (frame, is_speech) in frames {
                // Sized before the frame is pushed: the buffer has to already
                // hold the run-up by the time the VAD says "speech".
                stream.segment.set_preroll_ms(preroll_ms);
                stream.segment.push_frame(&frame, is_speech);
                let mut ctx = Ctx {
                    config: &config,
                    settings: &settings,
                    tx: &tx,
                    runner: &runner,
                    translator: &translator,
                    enter_buffer: &mut enter_buffer,
                    pending: &mut pending,
                    recording: &mut recording,
                };
                advance(stream, &mut state, mode, &mut ctx)?;
            }
        }

        if idle {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    info!("stopping capture");
    streams.clear();

    // Closing the window is a normal way to end a meeting, and it used to
    // discard the whole recording. Every utterance is already on disk by now;
    // this final write only refreshes the duration in the header.
    stop_session(&mut recording, &tx);
    Ok(())
}

/// Emit a partial if one is due, then finalize the segment if the VAD closed
/// it or it hit the cap.
fn advance(
    stream: &mut Stream,
    state: &mut whisper_rs::WhisperState,
    mode: TranscribeMode,
    ctx: &mut Ctx<'_>,
) -> Result<()> {
    // Per-mode tunables: dictation tolerates longer pauses and emits partials
    // less often so the overlay doesn't flicker while the user thinks between
    // sentences. A meeting rarely offers that much silence, so both profiles
    // live in commands.toml.
    let seg_cfg = ctx.config.segmentation(mode == TranscribeMode::Translate);
    let hang_frames =
        (seg_cfg.hang_ms as usize * TARGET_SAMPLE_RATE as usize / 1000) / crate::VAD_FRAME_SAMPLES;
    let partial_every = Duration::from_millis(seg_cfg.partial_every_ms);
    let max_samples = seg_cfg.max_seconds as usize * TARGET_SAMPLE_RATE as usize;

    let language = lock_settings(ctx.settings).language.clone();

    if stream.segment.speaking()
        && stream.segment.samples.len() >= PARTIAL_MIN_SAMPLES
        && stream.segment.last_partial.elapsed() >= partial_every
    {
        let opts = stream.opts(&language, true);
        let infer_start = Instant::now();
        let text = asr::transcribe_locked(state, &stream.segment.samples, &opts, &mut stream.lock)?;
        stream.segment.last_partial = Instant::now();
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            emit(
                ctx.tx,
                TranscriptEvent::Partial {
                    text: trimmed.to_string(),
                    source: stream.source,
                },
            );
            debug!(
                source = ?stream.source,
                infer_ms = infer_start.elapsed().as_millis(),
                samples = stream.segment.samples.len(),
                "partial emitted"
            );
        }
    }

    // The cap exists for the case the VAD never finds a pause; both paths end
    // an utterance, so both go through the same code.
    let closed = stream.segment.should_finalize(hang_frames);
    let capped = stream.segment.samples.len() > max_samples;
    if !closed && !capped {
        return Ok(());
    }

    let opts = stream.opts(&language, true);
    let text = asr::transcribe_locked(state, &stream.segment.samples, &opts, &mut stream.lock)?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        emit(ctx.tx, TranscriptEvent::PartialCleared(stream.source));
        stream.segment.reset();
        return Ok(());
    }

    // The mic decides the command vocabulary's language, because only the mic
    // can fire commands. Letting the meeting set it would pin `en` and stop
    // Portuguese commands from matching.
    if stream.source.may_command() {
        if let Some(code) = stream.lock.locked() {
            lock_settings(ctx.settings).detected_language = Some(code.to_string());
        }
    }

    info!(source = ?stream.source, ?mode, text = %trimmed, "FINAL");
    emit(
        ctx.tx,
        TranscriptEvent::Final {
            text: trimmed.clone(),
            source: stream.source,
        },
    );
    // M7.2: the original goes to the record and the overlay; the translation
    // is display-only and arrives async.
    //
    // Gated on the mode, and only the mode. An earlier version translated
    // whenever English was detected, so that speaking English to practise it
    // showed both languages — which is useful while *following* someone and
    // pure noise while *composing*. Dictation and Enter exist to produce one
    // piece of text in one language; a second rendering underneath every line
    // is something to read past on the way to what you are writing.
    if stream.translate {
        crate::translate::request(ctx.translator, &trimmed);
    }

    handle_final(stream.source, &trimmed, mode, ctx);
    stream.segment.reset();
    Ok(())
}

/// Session control, recording, and mode routing for one finalized utterance.
fn handle_final(source: Source, trimmed: &str, mode: TranscribeMode, ctx: &mut Ctx<'_>) {
    // M7.1: session control is checked before mode routing, so "grava" works
    // from any mode — but only from the microphone. A meeting that happens to
    // say the stop word must not close your recording.
    if source.may_command() && session_control(source, trimmed, ctx) {
        return;
    }

    if let Some(ref mut s) = ctx.recording {
        s.push(trimmed, source.label());
        // Persist immediately. Everything downstream of here — typing,
        // dispatching, the window manager — can fail or hang; the record of
        // what was said should already be on disk before any of it runs.
        if let Err(e) = s.write(&crate::session::default_dir()) {
            error!(error = ?e, "failed to persist session");
        }
    }

    // Audio from a meeting is transcribed and nothing more: never typed, never
    // dispatched to the window manager.
    if !source.may_command() {
        return;
    }
    commands::route_final(
        mode,
        trimmed,
        ctx.config,
        ctx.settings,
        ctx.enter_buffer,
        ctx.pending,
        ctx.tx,
        ctx.runner,
    );
}

/// Whose voice a mode is mainly there to capture, used to attribute a session
/// started from the button rather than by someone speaking.
fn primary_source(mode: TranscribeMode) -> Source {
    match mode {
        TranscribeMode::Translate => Source::System,
        _ => Source::Mic,
    }
}

/// Open a recording. No-op if one is already open.
///
/// The single place a session is created, so the spoken command and the
/// overlay's button cannot drift into behaving differently.
fn start_session(
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
fn stop_session(
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
fn session_control(source: Source, trimmed: &str, ctx: &mut Ctx<'_>) -> bool {
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

/// Change transcription mode by voice. Works from every mode, unlike the
/// window-manager grammar, which only runs in Command mode — a mode you
/// cannot reach if you are stuck in another one without touching the mouse.
fn switch_mode(name: &str, ctx: &mut Ctx<'_>) -> bool {
    let Some(mode) = TranscribeMode::from_name(name) else {
        error!(mode = name, "vocabulary names a mode that does not exist");
        return false;
    };
    let mut settings = lock_settings(ctx.settings);
    if settings.mode == mode {
        return true;
    }
    settings.mode = mode;
    drop(settings);
    info!(?mode, "mode switched by voice");
    emit(ctx.tx, TranscriptEvent::notice(format!("[modo] {name}")));
    true
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
