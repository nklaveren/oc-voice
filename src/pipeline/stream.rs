//! One captured audio stream, from device to finalized utterance.
//!
//! There used to be exactly one of these, implicit in the pipeline loop. In
//! System Audio mode there are now two running at once — the microphone and
//! the meeting — because capturing only the meeting produced a record of
//! everything said except by the person keeping it.
//!
//! Everything a stream needs to reach a transcription lives in here, and
//! **nothing is shared between streams except whisper itself**. That is not
//! tidiness: the VAD carries per-stream speech state, and the language lock
//! pins a source language. Sharing either would make the meeting's English
//! decide how your Portuguese is decoded.

use anyhow::{Context, Result};
use ringbuf::{traits::*, HeapCons, HeapRb};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::error;
use voice_activity_detector::VoiceActivityDetector;

use crate::asr::{LanguageLock, SpeechSegment, TranscribeOpts};
use crate::audio::capture::{run_capture, run_capture_system};
use crate::process::CommandRunner;
use crate::{Source, TARGET_SAMPLE_RATE, VAD_FRAME_SAMPLES, VAD_SPEECH_THRESHOLD};

/// Seconds of audio the ring buffer holds before the producer overruns.
const RING_SECONDS: usize = 4;

pub struct Stream {
    pub source: Source,
    /// Whisper's translate task: emit English whatever the source language.
    /// Set for the meeting, never for the microphone — asking whisper to
    /// translate your Portuguese would put English in your own record.
    pub translate: bool,
    consumer: HeapCons<f32>,
    vad: VoiceActivityDetector,
    frame_buf: Vec<f32>,
    pub segment: SpeechSegment,
    pub lock: LanguageLock,
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Stream {
    /// Start capture and everything downstream of it.
    pub fn start(
        source: Source,
        translate: bool,
        runner: Arc<dyn CommandRunner>,
        languages: Vec<String>,
    ) -> Result<Self> {
        let vad = VoiceActivityDetector::builder()
            .sample_rate(TARGET_SAMPLE_RATE)
            .chunk_size(VAD_FRAME_SAMPLES)
            .build()
            .context("building silero VAD")?;

        let ring = HeapRb::<f32>::new(TARGET_SAMPLE_RATE as usize * RING_SECONDS);
        let (producer, consumer) = ring.split();
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let handle = std::thread::spawn(move || {
            let result = match source {
                Source::Mic => run_capture(producer, flag),
                Source::System => run_capture_system(producer, flag, runner),
            };
            if let Err(e) = result {
                error!(?source, error = ?e, "capture thread failed");
            }
        });

        Ok(Stream {
            source,
            translate,
            consumer,
            vad,
            frame_buf: Vec::with_capacity(VAD_FRAME_SAMPLES * 2),
            segment: SpeechSegment::default(),
            lock: LanguageLock::restricted_to(languages),
            running,
            handle: Some(handle),
        })
    }

    /// How to transcribe this stream's audio right now.
    ///
    /// `language` is the UI's source-language selection. It is ignored when
    /// this stream translates: whisper's `language` is the *source* hint, and
    /// forcing the UI's choice onto a meeting tells whisper to decode English
    /// as Portuguese, which returns noise.
    pub fn opts(&self, language: &str, single_segment: bool) -> TranscribeOpts {
        TranscribeOpts {
            language: if self.translate {
                "auto".to_string()
            } else {
                language.to_string()
            },
            translate: self.translate,
            single_segment,
        }
    }

    /// Pull whatever capture has produced and cut it into VAD frames.
    ///
    /// Returns the frames ready for this tick, each already classified as
    /// speech or silence. An empty result means the stream is idle.
    pub fn take_frames(&mut self) -> Vec<(Vec<f32>, bool)> {
        let mut tmp = [0f32; 2048];
        let n = self.consumer.pop_slice(&mut tmp);
        if n > 0 {
            self.frame_buf.extend_from_slice(&tmp[..n]);
        }

        let mut frames = Vec::new();
        while self.frame_buf.len() >= VAD_FRAME_SAMPLES {
            let frame: Vec<f32> = self.frame_buf.drain(..VAD_FRAME_SAMPLES).collect();
            let is_speech = self.vad.predict(frame.clone()) >= VAD_SPEECH_THRESHOLD;
            frames.push((frame, is_speech));
        }
        frames
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Which streams a mode listens to.
///
/// Only System Audio runs both. Dictating into an editor while the machine's
/// own output is also transcribed would type the meeting into your code.
pub fn streams_for(mode: crate::TranscribeMode) -> Vec<(Source, bool)> {
    match mode {
        crate::TranscribeMode::Translate => vec![
            // (source, whisper translate task)
            (Source::System, true),
            // Your own speech is recorded as spoken, not put through
            // whisper's English translator.
            (Source::Mic, false),
        ],
        _ => vec![(Source::Mic, false)],
    }
}
