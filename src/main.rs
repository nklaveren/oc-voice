//! oc-voice POC
//!
//! cpal (microphone) -> rubato (16 kHz mono) -> ring buffer -> silero VAD ->
//! hybrid streaming: partial transcription every ~800 ms of speech,
//! final transcription on silence. Emits [partial]/[final] to stdout AND
//! to a floating overlay window (subtitle-style).
//!
//! Press Ctrl+C to stop.
//! Usage: oc-voice <path-to-ggml-model.bin>

use anyhow::{anyhow, Result};
#[deny(clippy::unwrap_used)]
mod asr;
mod asrtest;
#[deny(clippy::unwrap_used)]
mod audio;
mod cli;
mod commands;
mod config;
mod control;
mod fbank;
mod input;
mod ocr;
mod ocrprobe;
mod pipeline;
mod probe;
mod process;
mod session;
#[cfg(test)]
mod testing;
mod translate;
mod ui;
mod voicelock;
mod voiceprobe;
mod voices;
mod voicestore;
mod wm;

use process::{CommandRunner, SystemRunner};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{error, info};
use ui::overlay::run_overlay;
use ui::stdout::emit;

/// whisper expects 16 kHz mono f32
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// silero v5 at 16 kHz requires exactly 512-sample windows (~32 ms)
const VAD_FRAME_SAMPLES: usize = 512;
/// probability threshold above which we consider the frame to be speech.
/// Lower = more sensitive (catches weak syllables, breathing pauses),
/// higher = more conservative (less hallucination from background noise).
const VAD_SPEECH_THRESHOLD: f32 = 0.55;
/// minimum accumulated speech before we bother with a partial transcription
const PARTIAL_MIN_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 600 / 1000; // 600 ms
/// whisper wants at least 1 s of audio; shorter inputs get padded with silence
pub const MIN_TRANSCRIBE_SAMPLES: usize = TARGET_SAMPLE_RATE as usize; // 1 s

/// Where an utterance came from.
///
/// In System Audio mode both run at once: capturing only the meeting produced
/// a record of everything said except by the person keeping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The microphone — you.
    Mic,
    /// The system audio sink monitor — everyone else.
    System,
}

impl Source {
    /// Only the microphone may fire commands, dictate, or set the command
    /// vocabulary's language. Audio arriving from a meeting is transcribed and
    /// nothing more: a call saying "grava" must not start a recording, and it
    /// certainly must not type into the editor.
    pub fn may_command(self) -> bool {
        matches!(self, Source::Mic)
    }

    /// Label used in the overlay and the session record.
    pub fn label(self) -> &'static str {
        match self {
            Source::Mic => "você",
            Source::System => "reunião",
        }
    }
}

/// Events emitted by the audio pipeline, consumed by both stdout and the overlay UI.
#[derive(Debug, Clone)]
pub enum TranscriptEvent {
    Partial {
        text: String,
        source: Source,
    },
    Final {
        text: String,
        source: Source,
    },
    /// Sent when a partial utterance ends with no recognizable text; lets the
    /// UI clear that stream's partial without touching the other's.
    PartialCleared(Source),
    /// The voice lock finished enrolling, from this many segments.
    VoiceLocked(usize),
    /// An utterance was dropped as somebody else's voice, with its score.
    /// Shown, not swallowed: an assistant that ignores you silently is
    /// indistinguishable from one that has crashed.
    VoiceRejected(f32),
    /// In Enter mode: accumulated line count waiting for "cambio".
    Buffered(usize),
    /// In Enter mode: text was sent to the focused input.
    Sent(String),
    /// In Enter mode: newline command was executed.
    Newline,
    /// In Enter mode: buffer was cancelled/discard.
    Cancelled,
    /// In Enter mode: text was sent to a specific window target.
    /// Text, resolved window class, resolution score (M2.3).
    SentTo(String, String, f64),
    /// A recorded session opened (M7.1), carrying the file it is being
    /// written to. The file exists from this moment: it is rewritten after
    /// every utterance so a crash costs the last sentence, not the meeting.
    SessionStarted(String),
    /// It closed: where the file landed, and how many lines it holds.
    SessionStopped(String, usize),
    /// Display-only translation of a Final (M7.2). Never written to a session
    /// record — the original is the record.
    ///
    /// Carries the `original` it was made from so the overlay can attach it to
    /// the right line. Translation is async and some requests are dropped, so
    /// "the most recent line" is not a safe assumption.
    Translated {
        original: String,
        text: String,
    },
    /// A command is waiting for spoken confirmation (M4.3).
    AwaitingConfirmation(String),
    /// The pending command was discarded.
    ConfirmationCancelled,
}

impl TranscriptEvent {
    /// A line the app produced about itself — command feedback, a dispatch
    /// receipt — rather than something a person said. Attributed to the mic
    /// because that is what caused it; only ever shown, never recorded.
    pub fn notice(text: String) -> Self {
        TranscriptEvent::Final {
            text,
            source: Source::Mic,
        }
    }
}

/// What the app is doing with what it hears.
///
/// There were four. Two of them were the same thing wearing different
/// clothes: Input typed every utterance where Enter accumulated them, and
/// Command dispatched window commands where Enter, since the two grammars
/// were merged, already does. Leaving a half-composed message to switch modes
/// and switch back is friction the word-count gate makes unnecessary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TranscribeMode {
    /// Your microphone. Speech accumulates as text until the send word;
    /// a short utterance the window grammar recognises navigates instead of
    /// being written down.
    Enter,
    /// The machine's own output — a meeting, a video — subtitled and, when a
    /// model is installed, translated for display.
    Translate,
}

impl TranscribeMode {
    /// Resolve a language-neutral mode name from the vocabulary.
    ///
    /// `input` and `command` still resolve, to the mode that absorbed them.
    /// A config written before the merge keeps working, and someone who says
    /// "modo comando" out of habit gets the mode that runs those commands
    /// rather than an error they have to read the changelog to understand.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "enter" | "input" | "command" => Some(TranscribeMode::Enter),
            "translate" => Some(TranscribeMode::Translate),
            _ => None,
        }
    }

    /// The overlay button alternates between the two.
    pub fn next(self) -> Self {
        match self {
            TranscribeMode::Enter => TranscribeMode::Translate,
            TranscribeMode::Translate => TranscribeMode::Enter,
        }
    }
}

/// Start or stop a recording from outside the audio path.
///
/// The spoken commands only fire when someone speaks. In a meeting that is
/// the wrong requirement in both directions: saying "para de gravar" out loud
/// announces to everyone that you were recording, and if nobody is talking
/// there is no utterance to carry the command at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRequest {
    Start,
    Stop,
}

/// What the Settings button asked for. Like `session_request`: the lock lives
/// in the pipeline thread, which owns the audio, and only it may change one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiceRequest {
    /// Start listening for `ENROL_SECONDS` of your speech.
    Enrol,
    /// Forget the stored voice.
    Clear,
}

/// What the pipeline has to say about the lock, for the UI to draw. Reported
/// rather than asked, because a button that shows what it *requested* instead
/// of what happened is a button that lies while the microphone is muted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum VoiceState {
    #[default]
    Off,
    /// Fraction of the enrolment collected, 0.0 to 1.0.
    Enrolling(f32),
    On {
        segments: usize,
        threshold: f32,
    },
}

pub struct AppSettings {
    pub language: String,
    pub mode: TranscribeMode,
    /// Last language whisper detected while `language` is "auto"; picks the
    /// command vocabulary section (M1.3).
    pub detected_language: Option<String>,
    /// Set by the overlay's record button, consumed by the pipeline on its
    /// next tick. A request rather than a state, because the session itself
    /// lives in the pipeline thread and only it may open or close one.
    pub session_request: Option<SessionRequest>,
    /// Asks the overlay to toggle its Settings panel. A request like the
    /// others: the panel's state belongs to the UI, and only the UI may flip
    /// it — this just knocks.
    pub toggle_settings: bool,
    /// Set by the Settings button, consumed by the pipeline.
    pub voice_request: Option<VoiceRequest>,
    /// Written by the pipeline, read by the UI.
    pub voice_state: VoiceState,
}

/// Lock shared settings, recovering from mutex poisoning. A poisoned lock
/// means another thread panicked while holding it; AppSettings is plain data,
/// so taking the guard anyway is safe and keeps the pipeline alive.
pub fn lock_settings(settings: &Mutex<AppSettings>) -> std::sync::MutexGuard<'_, AppSettings> {
    settings.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() -> Result<()> {
    let subcommand = std::env::args()
        .nth(1)
        .is_some_and(|a| cli::is_subcommand(&a));
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Diagnostic subcommands are read by a person, not tailed as
                // logs: info lines would break the meter and the WER report.
                if subcommand {
                    "oc_voice=warn".into()
                } else {
                    "oc_voice=info".into()
                }
            }),
        )
        .init();

    let model_path = std::env::args().nth(1).ok_or_else(|| {
        anyhow!(
            "usage: oc-voice <model.bin> | probe [lang] | devices | levels | asr-test <model.bin> | ocr <alvo> | ctl <verb>"
        )
    })?;

    // Diagnostic subcommands short-circuit before any model is loaded.
    if cli::run_subcommand(&model_path)? {
        return Ok(());
    }

    // One instance per session, checked before the model is loaded rather
    // than after — a second copy costs a gigabyte and eight seconds before it
    // gets to do any harm.
    //
    // And it does harm: two instances hear the same microphone and both type
    // what they heard, so every character arrives twice, interleaved. That is
    // how this was found — a message came through as "oonn tthhee
    // hhyyppeerrllaanndd". The socket already detected the condition and only
    // declined to serve on it, which left the half that types running.
    if control::already_running() {
        return Err(anyhow::anyhow!(
            "another oc-voice is already running in this session; \
             two of them type every utterance twice"
        ));
    }

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
        // "auto", not a fixed language. Whisper's `language` is the SOURCE
        // hint: forcing "pt" onto English speech does not merely mis-decode
        // it, it makes whisper emit Portuguese — the system silently
        // translating you in a mode that is supposed to type what you said.
        language: "auto".to_string(),
        mode: TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
        voice_request: None,
        voice_state: crate::VoiceState::Off,
    }));

    // A third way in, beside the buttons and the spoken words: a keybinding.
    // Reaching for a floating overlay mid-call is the friction the spoken
    // commands exist to remove, and saying the stop word out loud announces
    // to the room that you were recording.
    {
        let s = settings.clone();
        let r = running.clone();
        std::thread::spawn(move || control::serve(s, r));
    }

    let vocab_config = Arc::new(config::Config::load());

    let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);

    // Translation is optional: without the model the app behaves exactly as
    // before and the overlay shows originals.
    let models_dir = std::path::Path::new(&model_path)
        .parent()
        .unwrap_or(std::path::Path::new("models"))
        .to_path_buf();
    let translator = translate::spawn(&models_dir, tx.clone()).map(Arc::new);

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
            translator,
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

use pipeline::run_audio_pipeline;
