//! oc-voice POC
//!
//! cpal (microphone) -> rubato (16 kHz mono) -> ring buffer -> silero VAD ->
//! hybrid streaming: partial transcription every ~800 ms of speech,
//! final transcription on silence. Emits [partial]/[final] to stdout AND
//! to a floating overlay window (subtitle-style).
//!
//! Press Ctrl+C to stop.
//! Usage: oc-voice <path-to-ggml-model.bin>

use anyhow::{anyhow, Context, Result};
#[deny(clippy::unwrap_used)]
mod asr;
#[deny(clippy::unwrap_used)]
mod audio;
mod commands;
mod config;
mod input;
mod process;
mod ui;
mod wm;

use asr::{transcribe, SpeechSegment};
use audio::capture::{run_capture, run_capture_system};
use commands::{classify, execute_command, VoiceCommand};
use crossbeam_channel::Sender;
use input::inject::type_text;
use process::{CommandRunner, SystemRunner};
use ringbuf::{traits::*, HeapRb};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info};
use ui::overlay::run_overlay;
use ui::stdout::emit;
use voice_activity_detector::VoiceActivityDetector;
use whisper_rs::{WhisperContext, WhisperContextParameters};

/// whisper expects 16 kHz mono f32
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// silero v5 at 16 kHz requires exactly 512-sample windows (~32 ms)
const VAD_FRAME_SAMPLES: usize = 512;
/// probability threshold above which we consider the frame to be speech.
/// Lower = more sensitive (catches weak syllables, breathing pauses),
/// higher = more conservative (less hallucination from background noise).
const VAD_SPEECH_THRESHOLD: f32 = 0.55;
/// how many consecutive non-speech frames until we emit final and reset.
/// 20 frames * 32 ms ~= 640 ms of silence before closing a segment.
const VAD_HANG_FRAMES: usize = 20;
/// Enter mode is more tolerant to pauses while composing the buffer.
/// 30 frames * 32 ms ~= 960 ms of silence before closing a segment.
const VAD_HANG_FRAMES_ENTER: usize = 30;
/// minimum accumulated speech before we bother with a partial transcription
const PARTIAL_MIN_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 600 / 1000; // 600 ms
/// how often to emit partial while speaking
const PARTIAL_EVERY: Duration = Duration::from_millis(800);
/// In Enter mode we want a steadier, less flickery partial. Longer window
/// between partial refreshes avoids the "cutting too fast" feeling.
const PARTIAL_EVERY_ENTER: Duration = Duration::from_millis(900);
/// cap the size of one segment so we don't blow up on very long utterances
const SEGMENT_MAX_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 20; // 20 s
/// whisper wants at least 1 s of audio; shorter inputs get padded with silence
pub const MIN_TRANSCRIBE_SAMPLES: usize = TARGET_SAMPLE_RATE as usize; // 1 s

/// Events emitted by the audio pipeline, consumed by both stdout and the overlay UI.
#[derive(Debug, Clone)]
pub enum TranscriptEvent {
    Partial(String),
    Final(String),
    /// Sent when a partial utterance ends with no recognizable text; lets the UI clear it.
    PartialCleared,
    /// In Enter mode: accumulated line count waiting for "cambio".
    Buffered(usize),
    /// In Enter mode: text was sent to the focused input.
    Sent(String),
    /// In Enter mode: newline command was executed.
    Newline,
    /// In Enter mode: buffer was cancelled/discard.
    Cancelled,
    /// In Enter mode: text was sent to a specific window target.
    SentTo(String, String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TranscribeMode {
    Input,
    Translate,
    Enter,
}

pub struct AppSettings {
    pub language: String,
    pub mode: TranscribeMode,
    /// Last language whisper detected while `language` is "auto"; picks the
    /// command vocabulary section (M1.3).
    pub detected_language: Option<String>,
}

/// Lock shared settings, recovering from mutex poisoning. A poisoned lock
/// means another thread panicked while holding it; AppSettings is plain data,
/// so taking the guard anyway is safe and keeps the pipeline alive.
pub fn lock_settings(settings: &Mutex<AppSettings>) -> std::sync::MutexGuard<'_, AppSettings> {
    settings.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oc_voice=info".into()),
        )
        .init();

    let model_path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("usage: oc-voice <path-to-ggml-model.bin>"))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_ctrl = running.clone();
    ctrlc::set_handler(move || {
        info!("shutdown requested");
        running_ctrl.store(false, Ordering::SeqCst);
    })
    .ok();

    // Channel from audio pipeline -> overlay UI
    let (tx, rx) = crossbeam_channel::unbounded::<TranscriptEvent>();

    let settings = Arc::new(Mutex::new(AppSettings {
        language: "pt".to_string(),
        mode: TranscribeMode::Enter,
        detected_language: None,
    }));

    let vocab_config = Arc::new(config::Config::load());

    let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);

    // Spawn the audio pipeline on a background thread; the main thread is
    // reserved for the GUI event loop (eframe needs to own it on most platforms).
    let pipeline_running = running.clone();
    let pipeline_settings = settings.clone();
    let pipeline_config = vocab_config.clone();
    let pipeline_runner = runner.clone();
    let pipeline_handle = std::thread::spawn(move || {
        if let Err(e) = run_audio_pipeline(
            &model_path,
            pipeline_running,
            tx,
            pipeline_settings,
            pipeline_runner,
            pipeline_config,
        ) {
            error!(error = ?e, "audio pipeline failed");
        }
    });

    // Run the overlay on the main thread. When the window closes, signal the
    // pipeline to stop.
    let ui_running = running.clone();
    if let Err(e) = run_overlay(rx, ui_running, settings, runner, vocab_config) {
        error!(error = ?e, "overlay failed");
    }

    // Once the window is gone, tell the pipeline to shut down and wait.
    running.store(false, Ordering::SeqCst);
    if let Err(e) = pipeline_handle.join() {
        error!(panic = ?e, "audio pipeline thread panicked");
    }
    Ok(())
}

/// Owns whisper, VAD, cpal, and runs the main transcription loop.
#[deny(clippy::unwrap_used)]
fn run_audio_pipeline(
    model_path: &str,
    running: Arc<AtomicBool>,
    tx: Sender<TranscriptEvent>,
    settings: Arc<Mutex<AppSettings>>,
    runner: Arc<dyn CommandRunner>,
    config: Arc<config::Config>,
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
                let text = transcribe(&mut state, &segment.samples, &settings)?;
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
                let text = transcribe(&mut state, &segment.samples, &settings)?;
                let infer_ms = infer_start.elapsed().as_millis();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let mode = lock_settings(&settings).mode;
                    info!(?mode, source = if matches!(mode, TranscribeMode::Translate) { "system" } else { "mic" }, text = %trimmed, "FINAL");
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    match mode {
                        TranscribeMode::Input => {
                            type_text(&*runner, trimmed);
                        }
                        TranscribeMode::Enter => match config::active_vocab(&config, &settings)
                            .and_then(|v| classify(trimmed, v, config.threshold()))
                        {
                            Some(VoiceCommand::Dictation) | None => {
                                enter_buffer.push(trimmed.to_string());
                                emit(&tx, TranscriptEvent::Buffered(enter_buffer.len()));
                            }
                            Some(cmd) => {
                                execute_command(&cmd, &mut enter_buffer, &tx, &runner);
                            }
                        },
                        TranscribeMode::Translate => {}
                    }
                    debug!(infer_ms, samples = segment.samples.len(), "final emitted");
                } else {
                    emit(&tx, TranscriptEvent::PartialCleared);
                }
                segment.reset();
            }

            if segment.samples.len() > SEGMENT_MAX_SAMPLES {
                let text = transcribe(&mut state, &segment.samples, &settings)?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let mode = lock_settings(&settings).mode;
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    match mode {
                        TranscribeMode::Input => {
                            type_text(&*runner, trimmed);
                        }
                        TranscribeMode::Enter => match config::active_vocab(&config, &settings)
                            .and_then(|v| classify(trimmed, v, config.threshold()))
                        {
                            Some(VoiceCommand::Dictation) | None => {
                                enter_buffer.push(trimmed.to_string());
                                emit(&tx, TranscriptEvent::Buffered(enter_buffer.len()));
                            }
                            Some(cmd) => {
                                execute_command(&cmd, &mut enter_buffer, &tx, &runner);
                            }
                        },
                        TranscribeMode::Translate => {}
                    }
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
