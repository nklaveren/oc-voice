//! The audio pipeline: capture, VAD segmentation, transcription, and routing
//! of every finalized utterance according to the current mode.

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use ringbuf::{traits::*, HeapRb};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info};
use voice_activity_detector::VoiceActivityDetector;
use whisper_rs::{WhisperContext, WhisperContextParameters};

use crate::asr::{self, SpeechSegment};
use crate::audio::capture::{run_capture, run_capture_system};
use crate::commands;
use crate::config;
use crate::process::CommandRunner;
use crate::ui::stdout::emit;
use crate::{
    lock_settings, AppSettings, TranscribeMode, TranscriptEvent, PARTIAL_EVERY,
    PARTIAL_EVERY_ENTER, PARTIAL_MIN_SAMPLES, SEGMENT_MAX_SAMPLES, TARGET_SAMPLE_RATE,
    VAD_FRAME_SAMPLES, VAD_HANG_FRAMES, VAD_HANG_FRAMES_ENTER, VAD_SPEECH_THRESHOLD,
};

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

    let mut vad = VoiceActivityDetector::builder()
        .sample_rate(TARGET_SAMPLE_RATE)
        .chunk_size(VAD_FRAME_SAMPLES)
        .build()
        .context("building silero VAD")?;
    info!("silero VAD ready");

    // ring buffer between capture thread and processing loop; ~4s of 16 kHz audio
    let mut state = ctx.create_state().context("creating whisper state")?;
    let mut frame_buf: Vec<f32> = Vec::with_capacity(VAD_FRAME_SAMPLES * 2);
    let mut segment = SpeechSegment::default();
    let mut enter_buffer: Vec<String> = Vec::new();
    let mut pending: Option<commands::PendingAction> = None;
    // One lock per pipeline run: the source language is a property of the
    // session, not of a single utterance.
    let mut lang_lock = asr::LanguageLock::default();
    let mut recording: Option<crate::session::Session> = None;

    // Dynamic capture management: start/stop capture threads based on mode
    let mut capture_running_flag = Arc::new(AtomicBool::new(true));
    let mut capture_handle: Option<std::thread::JoinHandle<()>> = None;
    let mut consumer: Option<ringbuf::HeapCons<f32>> = None;
    let mut last_mode: Option<TranscribeMode> = None;

    info!("speak into the mic; close the overlay window or press Ctrl+C to exit");

    while running.load(Ordering::SeqCst) {
        let mode = lock_settings(&settings).mode;
        let is_translate = mode == TranscribeMode::Translate;

        if last_mode != Some(mode) || capture_handle.is_none() {
            if let Some(handle) = capture_handle.take() {
                capture_running_flag.store(false, Ordering::SeqCst);
                let _ = handle.join();
            }

            capture_running_flag = Arc::new(AtomicBool::new(true));
            let ring = HeapRb::<f32>::new(TARGET_SAMPLE_RATE as usize * 4);
            let (producer, cons) = ring.split();
            consumer = Some(cons);

            let flag = capture_running_flag.clone();
            let capture_runner = runner.clone();
            capture_handle = Some(std::thread::spawn(move || {
                let result = if is_translate {
                    run_capture_system(producer, flag, capture_runner)
                } else {
                    run_capture(producer, flag)
                };
                if let Err(e) = result {
                    error!(error = ?e, "capture thread failed");
                }
            }));

            last_mode = Some(mode);
            info!(
                ?mode,
                source = if is_translate { "system" } else { "mic" },
                "capture source switched"
            );
        }

        let cons = match consumer.as_mut() {
            Some(c) => c,
            None => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };

        let mut tmp = [0f32; 2048];
        let n = cons.pop_slice(&mut tmp);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        frame_buf.extend_from_slice(&tmp[..n]);

        while frame_buf.len() >= VAD_FRAME_SAMPLES {
            let frame: Vec<f32> = frame_buf.drain(..VAD_FRAME_SAMPLES).collect();
            let prob = vad.predict(frame.clone());
            let is_speech = prob >= VAD_SPEECH_THRESHOLD;

            segment.push_frame(&frame, is_speech);

            // Per-mode tunables: Enter mode tolerates longer pauses and emits
            // partials less often so the overlay doesn't flicker while the
            // user thinks between sentences.
            let (hang_frames, partial_every) = match mode {
                TranscribeMode::Enter => (VAD_HANG_FRAMES_ENTER, PARTIAL_EVERY_ENTER),
                _ => (VAD_HANG_FRAMES, PARTIAL_EVERY),
            };

            if segment.speaking()
                && segment.samples.len() >= PARTIAL_MIN_SAMPLES
                && segment.last_partial.elapsed() >= partial_every
            {
                let infer_start = Instant::now();
                let text = asr::transcribe_locked(
                    &mut state,
                    &segment.samples,
                    &settings,
                    true,
                    &mut lang_lock,
                )?;
                let infer_ms = infer_start.elapsed().as_millis();
                segment.last_partial = Instant::now();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    emit(&tx, TranscriptEvent::Partial(trimmed.to_string()));
                    debug!(
                        infer_ms,
                        samples = segment.samples.len(),
                        prob,
                        "partial emitted"
                    );
                }
            }

            if segment.should_finalize(hang_frames) {
                let infer_start = Instant::now();
                let text = asr::transcribe_locked(
                    &mut state,
                    &segment.samples,
                    &settings,
                    true,
                    &mut lang_lock,
                )?;
                let infer_ms = infer_start.elapsed().as_millis();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let mode = lock_settings(&settings).mode;
                    info!(?mode, source = if matches!(mode, TranscribeMode::Translate) { "system" } else { "mic" }, text = %trimmed, "FINAL");
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    // M7.2: the original goes to the record and the overlay;
                    // the translation is display-only and arrives async.
                    if mode == TranscribeMode::Translate {
                        crate::translate::request(&translator, trimmed);
                    }

                    // M7.1: session control is checked before mode routing,
                    // so "grava" works from any mode; and the ORIGINAL text is
                    // what gets recorded, never the translation.
                    let vocab = config::active_vocab(&config, &settings);
                    match vocab.and_then(|v| commands::classify(trimmed, v, config.threshold())) {
                        Some(commands::VoiceCommand::SessionStart) if recording.is_none() => {
                            let source = if mode == TranscribeMode::Translate {
                                "system"
                            } else {
                                "mic"
                            };
                            recording = Some(crate::session::Session::start(source));
                            emit(&tx, TranscriptEvent::SessionStarted);
                            segment.reset();
                            continue;
                        }
                        Some(commands::VoiceCommand::SessionStop) => {
                            if let Some(s) = recording.take() {
                                let dir = crate::session::default_dir();
                                match s.write(&dir) {
                                    Ok(path) => emit(
                                        &tx,
                                        TranscriptEvent::SessionStopped(
                                            path.display().to_string(),
                                            s.line_count(),
                                        ),
                                    ),
                                    Err(e) => error!(error = ?e, "failed to write session"),
                                }
                                segment.reset();
                                continue;
                            }
                        }
                        _ => {}
                    }
                    if let Some(ref mut s) = recording {
                        let lang = lock_settings(&settings).detected_language.clone();
                        s.push(trimmed, lang.as_deref());
                    }
                    commands::route_final(
                        mode,
                        trimmed,
                        &config,
                        &settings,
                        &mut enter_buffer,
                        &mut pending,
                        &tx,
                        &runner,
                    );
                    debug!(infer_ms, samples = segment.samples.len(), "final emitted");
                } else {
                    emit(&tx, TranscriptEvent::PartialCleared);
                }
                segment.reset();
            }

            if segment.samples.len() > SEGMENT_MAX_SAMPLES {
                let text = asr::transcribe_locked(
                    &mut state,
                    &segment.samples,
                    &settings,
                    true,
                    &mut lang_lock,
                )?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let mode = lock_settings(&settings).mode;
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    // M7.2: the original goes to the record and the overlay;
                    // the translation is display-only and arrives async.
                    if mode == TranscribeMode::Translate {
                        crate::translate::request(&translator, trimmed);
                    }

                    // M7.1: session control is checked before mode routing,
                    // so "grava" works from any mode; and the ORIGINAL text is
                    // what gets recorded, never the translation.
                    let vocab = config::active_vocab(&config, &settings);
                    match vocab.and_then(|v| commands::classify(trimmed, v, config.threshold())) {
                        Some(commands::VoiceCommand::SessionStart) if recording.is_none() => {
                            let source = if mode == TranscribeMode::Translate {
                                "system"
                            } else {
                                "mic"
                            };
                            recording = Some(crate::session::Session::start(source));
                            emit(&tx, TranscriptEvent::SessionStarted);
                            segment.reset();
                            continue;
                        }
                        Some(commands::VoiceCommand::SessionStop) => {
                            if let Some(s) = recording.take() {
                                let dir = crate::session::default_dir();
                                match s.write(&dir) {
                                    Ok(path) => emit(
                                        &tx,
                                        TranscriptEvent::SessionStopped(
                                            path.display().to_string(),
                                            s.line_count(),
                                        ),
                                    ),
                                    Err(e) => error!(error = ?e, "failed to write session"),
                                }
                                segment.reset();
                                continue;
                            }
                        }
                        _ => {}
                    }
                    if let Some(ref mut s) = recording {
                        let lang = lock_settings(&settings).detected_language.clone();
                        s.push(trimmed, lang.as_deref());
                    }
                    commands::route_final(
                        mode,
                        trimmed,
                        &config,
                        &settings,
                        &mut enter_buffer,
                        &mut pending,
                        &tx,
                        &runner,
                    );
                }
                segment.reset();
            }
        }
    }

    info!("stopping capture");
    if let Some(handle) = capture_handle.take() {
        capture_running_flag.store(false, Ordering::SeqCst);
        let _ = handle.join();
    }
    Ok(())
}
