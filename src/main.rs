//! oc-voice POC
//!
//! cpal (microphone) -> rubato (16 kHz mono) -> ring buffer -> silero VAD ->
//! hybrid streaming: partial transcription every ~800 ms of speech,
//! final transcription on silence. Emits [partial]/[final] to stdout AND
//! to a floating overlay window (subtitle-style).
//!
//! Press Ctrl+C to stop.
//! Usage: oc-voice-poc <path-to-ggml-model.bin>

use anyhow::{anyhow, Context, Result};
mod llm_classifier;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use llm_classifier::{LlmClassifier, VoiceCommand};
use ringbuf::{traits::*, HeapRb};
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use voice_activity_detector::VoiceActivityDetector;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// whisper expects 16 kHz mono f32
const TARGET_SAMPLE_RATE: u32 = 16_000;
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
const MIN_TRANSCRIBE_SAMPLES: usize = TARGET_SAMPLE_RATE as usize; // 1 s

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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oc_voice_poc=info".into()),
        )
        .init();

    let model_path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("usage: oc-voice-poc <path-to-ggml-model.bin>"))?;

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
    }));

    // Spawn the audio pipeline on a background thread; the main thread is
    // reserved for the GUI event loop (eframe needs to own it on most platforms).
    let pipeline_running = running.clone();
    let pipeline_settings = settings.clone();
    let pipeline_handle = std::thread::spawn(move || {
        if let Err(e) = run_audio_pipeline(&model_path, pipeline_running, tx, pipeline_settings) {
            error!(error = ?e, "audio pipeline failed");
        }
    });

    // Run the overlay on the main thread. When the window closes, signal the
    // pipeline to stop.
    let ui_running = running.clone();
    if let Err(e) = run_overlay(rx, ui_running, settings) {
        error!(error = ?e, "overlay failed");
    }

    // Once the window is gone, tell the pipeline to shut down and wait.
    running.store(false, Ordering::SeqCst);
    let _ = pipeline_handle.join();
    Ok(())
}

/// Owns whisper, VAD, cpal, and runs the main transcription loop.
fn run_audio_pipeline(
    model_path: &str,
    running: Arc<AtomicBool>,
    tx: Sender<TranscriptEvent>,
    settings: Arc<Mutex<AppSettings>>,
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
        let mode = settings.lock().unwrap().mode;
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
            capture_handle = Some(std::thread::spawn(move || {
                let result = if is_translate {
                    run_capture_system(producer, flag)
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
                    let mode = settings.lock().unwrap().mode;
                    info!(?mode, source = if matches!(mode, TranscribeMode::Translate) { "system" } else { "mic" }, text = %trimmed, "FINAL");
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    match mode {
                        TranscribeMode::Input => {
                            type_text(trimmed);
                        }
                        TranscribeMode::Enter => {
                            match LlmClassifier::classify_with_fallback(trimmed) {
                                Some(VoiceCommand::Dictation) | None => {
                                    enter_buffer.push(trimmed.to_string());
                                    emit(&tx, TranscriptEvent::Buffered(enter_buffer.len()));
                                }
                                Some(cmd) => {
                                    execute_command(&cmd, &mut enter_buffer, &tx);
                                }
                            }
                        }
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
                    let mode = settings.lock().unwrap().mode;
                    emit(&tx, TranscriptEvent::Final(trimmed.to_string()));
                    match mode {
                        TranscribeMode::Input => {
                            type_text(trimmed);
                        }
                        TranscribeMode::Enter => {
                            match LlmClassifier::classify_with_fallback(trimmed) {
                                Some(VoiceCommand::Dictation) | None => {
                                    enter_buffer.push(trimmed.to_string());
                                    emit(&tx, TranscriptEvent::Buffered(enter_buffer.len()));
                                }
                                Some(cmd) => {
                                    execute_command(&cmd, &mut enter_buffer, &tx);
                                }
                            }
                        }
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

fn execute_command(
    cmd: &VoiceCommand,
    enter_buffer: &mut Vec<String>,
    tx: &Sender<TranscriptEvent>,
) {
    match cmd {
        VoiceCommand::Send => {
            let display_text = enter_buffer.join("\n");
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            let clean = inject_text.trim();
            if !clean.is_empty() {
                emit(tx, TranscriptEvent::Sent(display_text));
                type_text(clean);
                type_key("Return");
            }
        }
        VoiceCommand::Cancel => {
            enter_buffer.clear();
            emit(tx, TranscriptEvent::Cancelled);
        }
        VoiceCommand::Newline => {
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            if !inject_text.is_empty() {
                type_text(inject_text.trim());
            }
            type_shift_return();
            emit(tx, TranscriptEvent::Newline);
        }
        VoiceCommand::SendTo { target } => {
            let display_text = enter_buffer.join("\n");
            let inject_text = enter_buffer.join(" ");
            enter_buffer.clear();
            let clean = inject_text.trim();
            if !clean.is_empty() {
                emit(tx, TranscriptEvent::SentTo(display_text, target.clone()));
                focus_window_and_type(target, clean);
            }
        }
        VoiceCommand::Dictation => {
            // handled by caller — pushes to enter_buffer
        }
    }
}

/// Fan-out: send to the UI channel AND echo to stdout for debugging.
fn emit(tx: &Sender<TranscriptEvent>, event: TranscriptEvent) {
    match &event {
        TranscriptEvent::Partial(s) => emit_stdout_partial(s),
        TranscriptEvent::PartialCleared => clear_partial_line(),
        TranscriptEvent::Final(s) => emit_stdout_final(s),
        TranscriptEvent::Buffered(n) => {
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            let _ = write!(h, "\r\x1b[2K[buffered] {n} line(s) waiting for keyword");
            let _ = h.flush();
        }
        TranscriptEvent::Sent(s) => {
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            let _ = write!(h, "\r\x1b[2K[sent]     {s}\n");
            let _ = h.flush();
        }
        TranscriptEvent::Newline => {
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            let _ = write!(h, "\r\x1b[2K[newline]  Shift+Return\n");
            let _ = h.flush();
        }
        TranscriptEvent::Cancelled => {
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            let _ = write!(h, "\r\x1b[2K[cancelled] buffer cleared\n");
            let _ = h.flush();
        }
        TranscriptEvent::SentTo(_, target) => {
            let stdout = std::io::stdout();
            let mut h = stdout.lock();
            let _ = write!(h, "\r\x1b[2K[sent_to]  {target}\n");
            let _ = h.flush();
        }
    }
    let _ = tx.send(event);
}

/// State of the current utterance as seen by VAD.
struct SpeechSegment {
    samples: Vec<f32>,
    trailing_silence_frames: usize,
    started_speaking: bool,
    last_partial: Instant,
}

impl Default for SpeechSegment {
    fn default() -> Self {
        Self {
            samples: Vec::with_capacity(TARGET_SAMPLE_RATE as usize * 5),
            trailing_silence_frames: 0,
            started_speaking: false,
            last_partial: Instant::now(),
        }
    }
}

impl SpeechSegment {
    fn push_frame(&mut self, frame: &[f32], is_speech: bool) {
        if is_speech {
            if !self.started_speaking {
                self.started_speaking = true;
                self.last_partial = Instant::now();
            }
            self.samples.extend_from_slice(frame);
            self.trailing_silence_frames = 0;
        } else if self.started_speaking {
            self.samples.extend_from_slice(frame);
            self.trailing_silence_frames += 1;
        }
    }

    fn speaking(&self) -> bool {
        self.started_speaking && self.trailing_silence_frames == 0
    }

    fn should_finalize(&self, hang_frames: usize) -> bool {
        self.started_speaking && self.trailing_silence_frames >= hang_frames
    }

    fn reset(&mut self) {
        self.samples.clear();
        self.trailing_silence_frames = 0;
        self.started_speaking = false;
        self.last_partial = Instant::now();
    }
}

/// \r-based overwrite of the current partial on stdout. Uses ANSI clear-EOL.
fn emit_stdout_partial(text: &str) {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = write!(h, "\r\x1b[2K[partial] {text}");
    let _ = h.flush();
}

fn clear_partial_line() {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = write!(h, "\r\x1b[2K");
    let _ = h.flush();
}

fn emit_stdout_final(text: &str) {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = write!(h, "\r\x1b[2K[final]   {text}\n");
    let _ = h.flush();
}

/// blocking call that keeps mic capture alive until `running` flips.
fn run_capture<P>(producer: P, running: Arc<AtomicBool>) -> Result<()>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;
    info!(device = %device.name().unwrap_or_else(|_| "?".into()), "using input");

    let default_config = device
        .default_input_config()
        .context("getting input config")?;
    let input_sample_rate = default_config.sample_rate().0;
    let input_channels = default_config.channels() as usize;
    info!(
        rate = input_sample_rate,
        channels = input_channels,
        format = ?default_config.sample_format(),
        "input config"
    );

    let err_fn = |err| error!("cpal stream error: {err}");

    let stream = match default_config.sample_format() {
        SampleFormat::F32 => {
            let config = default_config.into();
            let mut resampler = Resampler16k::new(input_sample_rate)
                .with_context(|| format!("building resampler {input_sample_rate}->16000"))?;
            device.build_input_stream(
                &config,
                {
                    let running = running.clone();
                    let mut producer = producer;
                    move |data: &[f32], _| {
                        if !running.load(Ordering::SeqCst) {
                            return;
                        }
                        let mono = to_mono(data, input_channels);
                        let resampled = resampler.process(&mono);
                        push_samples(&mut producer, &resampled);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let config = default_config.into();
            let mut resampler = Resampler16k::new(input_sample_rate)
                .with_context(|| format!("building resampler {input_sample_rate}->16000"))?;
            device.build_input_stream(
                &config,
                {
                    let running = running.clone();
                    let mut producer = producer;
                    move |data: &[i16], _| {
                        if !running.load(Ordering::SeqCst) {
                            return;
                        }
                        let floats: Vec<f32> =
                            data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        let mono = to_mono(&floats, input_channels);
                        let resampled = resampler.process(&mono);
                        push_samples(&mut producer, &resampled);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    stream.play().context("starting input stream")?;

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }

    drop(stream);
    Ok(())
}

fn run_capture_system<P>(mut producer: P, running: Arc<AtomicBool>) -> Result<()>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let sink_target = get_default_sink_target();
    info!(target = %sink_target, "capturing system audio from sink monitor");

    let mut child = std::process::Command::new("pw-record")
        .args([
            "--raw",
            "--target",
            &sink_target,
            // Force the capture stream to connect to the sink's monitor ports
            // instead of the default source. Without this, pw-record silently
            // links to the microphone input even when --target points to a sink.
            "-P",
            "stream.capture.sink=true",
            "--format",
            "f32",
            "--rate",
            "48000",
            "--channels",
            "2",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn pw-record")?;

    // forward stderr to logs in the background
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|r| r.ok()) {
                warn!(target: "pw-record", "{}", line);
            }
        });
    }

    let stdout = child
        .stdout
        .take()
        .context("pw-record stdout not available")?;

    let mut reader = std::io::BufReader::new(stdout);
    let mut buf = [0u8; 4096 * 4];
    let mut resampler = Resampler16k::new(48000).context("building resampler 48000->16000")?;

    let mut bytes_total: u64 = 0;
    let mut last_log = Instant::now();

    while running.load(Ordering::SeqCst) {
        let n = match reader.read(&mut buf) {
            Ok(0) => {
                warn!("pw-record stdout EOF (process exited)");
                break;
            }
            Ok(n) => n,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                return Err(e).context("reading pw-record stdout");
            }
        };

        bytes_total += n as u64;
        if last_log.elapsed() >= Duration::from_secs(2) {
            info!(bytes_per_sec = bytes_total / 2, "pw-record throughput");
            bytes_total = 0;
            last_log = Instant::now();
        }

        let samples_f32: Vec<f32> = buf[..n]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        let mono = to_mono(&samples_f32, 2);
        let resampled = resampler.process(&mono);
        push_samples(&mut producer, &resampled);
    }

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Returns the node.name of the current default audio sink. `pw-record --target <name>`
/// captures from the sink's monitor port. Falling back to `@DEFAULT_AUDIO_SINK@`
/// lets pipewire pick the default at runtime.
fn get_default_sink_target() -> String {
    // Query pipewire for the default sink's node.name via pw-dump.
    let dump = match std::process::Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return "@DEFAULT_AUDIO_SINK@".to_string(),
    };

    let value: serde_json::Value = match serde_json::from_slice(&dump) {
        Ok(v) => v,
        Err(_) => return "@DEFAULT_AUDIO_SINK@".to_string(),
    };

    // Step 1: find default sink name from Metadata object.
    let default_sink_name = value.as_array().and_then(|arr| {
        arr.iter().find_map(|obj| {
            if obj.get("type").and_then(|v| v.as_str()) != Some("PipeWire:Interface:Metadata") {
                return None;
            }
            if obj
                .get("props")
                .and_then(|p| p.get("metadata.name"))
                .and_then(|v| v.as_str())
                != Some("default")
            {
                return None;
            }
            obj.get("metadata")
                .and_then(|m| m.as_array())
                .and_then(|entries| {
                    entries.iter().find_map(|entry| {
                        if entry.get("key").and_then(|v| v.as_str()) == Some("default.audio.sink") {
                            entry
                                .get("value")
                                .and_then(|v| v.get("name"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                })
        })
    });

    default_sink_name.unwrap_or_else(|| "@DEFAULT_AUDIO_SINK@".to_string())
}

/// averages interleaved channels down to mono
fn to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Streaming resampler that converts mono f32 audio from `input_rate` to 16 kHz
/// using rubato's sinc interpolation. Accepts variable-size input chunks by
/// buffering leftover samples between calls.
///
/// If the input rate is already 16 kHz, we skip rubato entirely.
struct Resampler16k {
    inner: Option<SincFixedIn<f32>>,
    input_buffer: Vec<f32>,
    scratch_in: Vec<Vec<f32>>,
    scratch_out: Vec<Vec<f32>>,
    chunk_size: usize,
}

impl Resampler16k {
    fn new(input_rate: u32) -> Result<Self> {
        if input_rate == TARGET_SAMPLE_RATE {
            return Ok(Self {
                inner: None,
                input_buffer: Vec::new(),
                scratch_in: vec![Vec::new()],
                scratch_out: vec![Vec::new()],
                chunk_size: 0,
            });
        }

        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };

        let chunk_size = 1024usize;
        let ratio = TARGET_SAMPLE_RATE as f64 / input_rate as f64;
        let inner = SincFixedIn::<f32>::new(ratio, 1.0, params, chunk_size, 1)
            .context("creating SincFixedIn resampler")?;

        let scratch_out_capacity = inner.output_frames_max();

        Ok(Self {
            inner: Some(inner),
            input_buffer: Vec::with_capacity(chunk_size * 4),
            scratch_in: vec![vec![0.0f32; chunk_size]],
            scratch_out: vec![vec![0.0f32; scratch_out_capacity]],
            chunk_size,
        })
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(resampler) = self.inner.as_mut() else {
            return input.to_vec();
        };

        self.input_buffer.extend_from_slice(input);

        let mut out: Vec<f32> = Vec::new();
        while self.input_buffer.len() >= self.chunk_size {
            self.scratch_in[0].clear();
            self.scratch_in[0].extend_from_slice(&self.input_buffer[..self.chunk_size]);
            self.input_buffer.drain(..self.chunk_size);

            match resampler.process_into_buffer(&self.scratch_in, &mut self.scratch_out, None) {
                Ok((_in_frames, out_frames)) => {
                    out.extend_from_slice(&self.scratch_out[0][..out_frames]);
                }
                Err(e) => {
                    error!(error = ?e, "rubato resample failed; dropping chunk");
                }
            }
        }
        out
    }
}

fn push_samples<P: Producer<Item = f32>>(producer: &mut P, samples: &[f32]) {
    let _pushed = producer.push_slice(samples);
}

fn transcribe(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    settings: &Arc<Mutex<AppSettings>>,
) -> Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(true);
    params.set_n_threads(num_cpus::get_physical() as i32);
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_split_on_word(true);

    let (lang, translate) = {
        let s = settings.lock().unwrap();
        (s.language.clone(), s.mode == TranscribeMode::Translate)
    };
    if lang == "auto" {
        params.set_language(None);
    } else {
        params.set_language(Some(&lang));
    }
    params.set_translate(translate);

    // Whisper refuses inputs shorter than 1 s and prints a warning. Pad with
    // trailing silence so we can transcribe short utterances.
    let padded_storage;
    let audio = if audio.len() < MIN_TRANSCRIBE_SAMPLES {
        let target = MIN_TRANSCRIBE_SAMPLES + TARGET_SAMPLE_RATE as usize / 20; // +50 ms
        let mut v = Vec::with_capacity(target);
        v.extend_from_slice(audio);
        v.resize(target, 0.0);
        padded_storage = v;
        debug!(
            original = audio.len(),
            padded = padded_storage.len(),
            "padded short audio for whisper"
        );
        padded_storage.as_slice()
    } else {
        audio
    };

    state.full(params, audio).context("whisper full()")?;

    let mut out = String::new();
    let n = state.full_n_segments().context("n_segments")?;
    for i in 0..n {
        let text = state.full_get_segment_text(i).unwrap_or_default();
        out.push_str(&text);
    }
    Ok(filter_hallucination(&out))
}

const HALLUCINATIONS: &[&str] = &[
    "O usuário fala português e inglês.",
    "O usuário fala português e inglês",
    "obrigado por assistir",
    "obrigado por ver",
    "obrigado",
    "obrigado.",
    "e aí",
    "e aí.",
    "e ai",
    "e ai.",
    "thank you for watching",
    "thank you.",
    "thank you",
    "subscribe",
    "please subscribe",
    "like and subscribe",
];

fn filter_hallucination(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    for h in HALLUCINATIONS {
        if trimmed.eq_ignore_ascii_case(h) {
            debug!(text = %trimmed, "filtered hallucination");
            return String::new();
        }
    }
    text.to_string()
}

fn type_text(text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(found) = std::process::Command::new("which").arg("wtype").output() {
        if found.status.success() {
            let result = std::process::Command::new("wtype").arg(text).output();
            match result {
                Ok(o) if o.status.success() => {
                    debug!(text = %text, "injected via wtype");
                }
                Ok(o) => {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype failed");
                }
                Err(e) => {
                    debug!(error = %e, "wtype exec failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = std::process::Command::new("which").arg("xdotool").output() {
        if found.status.success() {
            let result = std::process::Command::new("xdotool")
                .args(["type", "--clearmodifiers", text])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    debug!(text = %text, "injected via xdotool");
                }
                Ok(o) => {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "xdotool failed");
                }
                Err(e) => {
                    debug!(error = %e, "xdotool exec failed");
                }
            }
            return;
        }
    }
    debug!("no text injection tool found (wtype / xdotool)");
}

fn type_key(key: &str) {
    if let Ok(found) = std::process::Command::new("which").arg("wtype").output() {
        if found.status.success() {
            let result = std::process::Command::new("wtype")
                .args(["-k", key])
                .output();
            if let Ok(o) = result {
                if o.status.success() {
                    debug!(key = %key, "key press via wtype");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype key failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = std::process::Command::new("which").arg("xdotool").output() {
        if found.status.success() {
            let result = std::process::Command::new("xdotool")
                .args(["key", key])
                .output();
            if let Ok(o) = result {
                if o.status.success() {
                    debug!(key = %key, "key press via xdotool");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "xdotool key failed");
                }
            }
        }
    }
}

fn type_shift_return() {
    if let Ok(found) = std::process::Command::new("which").arg("wtype").output() {
        if found.status.success() {
            let result = std::process::Command::new("wtype")
                .args(["-M", "shift", "-k", "Return"])
                .output();
            if let Ok(o) = result {
                if o.status.success() {
                    debug!("Shift+Return via wtype");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype shift+return failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = std::process::Command::new("which").arg("xdotool").output() {
        if found.status.success() {
            let result = std::process::Command::new("xdotool")
                .args(["key", "Shift+Return"])
                .output();
            if let Ok(o) = result {
                if o.status.success() {
                    debug!("Shift+Return via xdotool");
                }
            }
        }
    }
}

fn focus_window_and_type(target: &str, text: &str) {
    let clients_output = match std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            type_text(text);
            type_key("Return");
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&clients_output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(_) => {
            type_text(text);
            type_key("Return");
            return;
        }
    };

    let clients = match json.as_array() {
        Some(a) => a,
        None => {
            type_text(text);
            type_key("Return");
            return;
        }
    };

    let mut matched = None;
    for client in clients.iter().rev() {
        let class = client
            .get("class")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let title = client
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let target_lower = target.to_lowercase();
        if class.contains(&target_lower)
            || title.contains(&target_lower)
            || target_lower.contains(&class)
        {
            matched = Some(client.clone());
            break;
        }
    }

    if let Some(client) = matched {
        let address = client.get("address").and_then(|v| v.as_str()).unwrap_or("");
        if !address.is_empty() {
            let _ = std::process::Command::new("hyprctl")
                .args(["dispatch", "focuswindow", &format!("address:{address}")])
                .output();
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    type_text(text);
    type_key("Return");
    info!(target = %target, "sent text to target window");
}

// ---- overlay UI ----

fn run_overlay(
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
) -> Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("oc-voice")
        .with_app_id("oc-voice")
        .with_inner_size([900.0, 350.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(true);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    try_hyprland_float();

    eframe::run_native(
        "oc-voice",
        options,
        Box::new(|_cc| Ok(Box::new(OverlayApp::new(rx, running, settings)))),
    )
    .map_err(|e| anyhow!("eframe error: {e}"))?;

    Ok(())
}

/// Best-effort: detect Hyprland and auto-float+pin the oc-voice window.
///
/// Hyprland 0.54.x does not reliably match window rules on xdg_toplevel
/// windows whose app_id arrives after surface creation. This workaround
/// polls `hyprctl clients` for our window and dispatches togglefloating +
/// pin directly.  Non-Hyprland systems are silently skipped.
fn try_hyprland_float() {
    std::thread::spawn(|| {
        if std::process::Command::new("hyprctl")
            .arg("version")
            .output()
            .is_err()
        {
            return;
        }

        let monitor_info = hyprctl_primary_monitor_info();

        std::thread::sleep(Duration::from_millis(300));

        for attempt in 0..15 {
            if let Ok(output) = std::process::Command::new("hyprctl")
                .args(["clients", "-j"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.contains("\"oc-voice\"") {
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = std::process::Command::new("hyprctl")
                        .args(["dispatch", "togglefloating", "class:oc-voice"])
                        .output();
                    let _ = std::process::Command::new("hyprctl")
                        .args(["dispatch", "pin", "class:oc-voice"])
                        .output();

                    if let Some((mon_w, mon_h, scale)) = monitor_info {
                        let win_w: f64 = 900.0;
                        let win_h: f64 = 350.0;
                        let logical_w = mon_w as f64 / scale;
                        let logical_h = mon_h as f64 / scale;
                        let x = ((logical_w - win_w) / 2.0) as i64;
                        let y = ((logical_h - win_h - 60.0) / 1.0) as i64;
                        let pos = format!("{x} {y}");
                        let _ = std::process::Command::new("hyprctl")
                            .args([
                                "dispatch",
                                "movewindowpixel",
                                "exact",
                                &pos,
                                "class:oc-voice",
                            ])
                            .output();
                        let size = format!("{} {}", win_w as i64, win_h as i64);
                        let _ = std::process::Command::new("hyprctl")
                            .args(["dispatch", "resizeactive", "exact", &size])
                            .output();
                        info!(x, y, scale, "positioned overlay at bottom-center");
                    }

                    info!("applied hyprland float + pin via hyprctl");
                    return;
                }
            }
            if attempt == 0 {
                info!("detected Hyprland, waiting for oc-voice window to register...");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        debug!("hyprctl auto-float: window not found after 3 s, giving up");
    });
}

fn hyprctl_primary_monitor_info() -> Option<(i64, i64, f64)> {
    let output = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let mon = json.as_array()?.first()?;
    let w = mon.get("width")?.as_i64()?;
    let h = mon.get("height")?.as_i64()?;
    let scale = mon.get("scale")?.as_f64().unwrap_or(1.0);
    Some((w, h, scale))
}

struct OverlayApp {
    rx: Receiver<TranscriptEvent>,
    running: Arc<AtomicBool>,
    settings: Arc<Mutex<AppSettings>>,
    partial: String,
    finals: Vec<String>,
    buffered: usize,
    show_settings: bool,
}

const LANGUAGES: &[&str] = &["auto", "pt", "en", "es", "fr", "de", "ja", "zh"];

impl OverlayApp {
    fn new(
        rx: Receiver<TranscriptEvent>,
        running: Arc<AtomicBool>,
        settings: Arc<Mutex<AppSettings>>,
    ) -> Self {
        Self {
            rx,
            running,
            settings,
            partial: String::new(),
            finals: Vec::new(),
            buffered: 0,
            show_settings: false,
        }
    }
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // fully transparent background; we draw our own panel on top
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain all pending transcription events before painting the UI.
        while let Ok(event) = self.rx.try_recv() {
            match event {
                TranscriptEvent::Partial(s) => self.partial = s,
                TranscriptEvent::PartialCleared => self.partial.clear(),
                TranscriptEvent::Final(s) => {
                    self.partial.clear();
                    self.finals.push(s);
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Buffered(n) => {
                    self.partial.clear();
                    self.buffered = n;
                }
                TranscriptEvent::Sent(s) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push(format!("[sent] {s}"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Newline => {
                    self.partial.clear();
                    self.finals.push("[newline]".to_string());
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::Cancelled => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push("[cancelled] buffer cleared".to_string());
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
                TranscriptEvent::SentTo(_, target) => {
                    self.partial.clear();
                    self.buffered = 0;
                    self.finals.push(format!("[sent_to] {target}"));
                    let max_keep = 4;
                    if self.finals.len() > max_keep {
                        let excess = self.finals.len() - max_keep;
                        self.finals.drain(..excess);
                    }
                }
            }
        }

        // If the pipeline has signalled shutdown, close the window.
        if !self.running.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Repaint periodically so new events show up even without user input.
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let panel_size = ui.available_size();
        let bg = egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(200))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 12));

        bg.show(ui, |ui| {
            ui.set_min_size(panel_size);
            ui.set_width(panel_size.x);

            ui.vertical(|ui| {
                ui.set_width(ui.available_width());

                for line in &self.finals {
                    ui.label(
                        egui::RichText::new(line)
                            .color(egui::Color32::WHITE)
                            .size(18.0),
                    );
                }
                if !self.partial.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.partial)
                            .color(egui::Color32::from_gray(180))
                            .italics()
                            .size(18.0),
                    );
                }
                if self.finals.is_empty() && self.partial.is_empty() && self.buffered == 0 {
                    ui.label(
                        egui::RichText::new(
                            "[ speak into the mic \u{2014} say \"envia\" to send ]",
                        )
                        .color(egui::Color32::from_gray(120))
                        .italics()
                        .size(14.0),
                    );
                }
                if self.buffered > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} line(s) buffered \u{2014} say \"cambio\" to send",
                            self.buffered
                        ))
                        .color(egui::Color32::from_rgb(255, 200, 80))
                        .size(14.0),
                    );
                }

                let remaining = (ui.available_height() - 28.0).max(8.0);
                ui.add_space(remaining);

                ui.horizontal(|ui| {
                    if ui.button("\u{2699} Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }

                    let mode_label = match self.settings.lock().unwrap().mode {
                        TranscribeMode::Input => "\u{1f4dd} Input Mode",
                        TranscribeMode::Translate => "\u{1f310} Translate Mode",
                        TranscribeMode::Enter => "\u{23ce} Enter Mode",
                    };
                    if ui.button(mode_label).clicked() {
                        let mut s = self.settings.lock().unwrap();
                        s.mode = match s.mode {
                            TranscribeMode::Input => TranscribeMode::Translate,
                            TranscribeMode::Translate => TranscribeMode::Enter,
                            TranscribeMode::Enter => TranscribeMode::Input,
                        };
                    }
                });
            });
        });

        if self.show_settings {
            egui::Window::new("Settings")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .resizable(false)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Language:");
                    ui.horizontal(|ui| {
                        let current = self.settings.lock().unwrap().language.clone();
                        for lang in LANGUAGES {
                            if ui.selectable_label(current == *lang, *lang).clicked() {
                                self.settings.lock().unwrap().language = lang.to_string();
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        self.show_settings = false;
                    }
                });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.running.store(false, Ordering::SeqCst);
    }
}
