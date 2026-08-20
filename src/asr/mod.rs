use crate::commands::matcher;
use crate::{AppSettings, TranscribeMode, MIN_TRANSCRIBE_SAMPLES, TARGET_SAMPLE_RATE};
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::debug;
use whisper_rs::{FullParams, SamplingStrategy};

pub fn transcribe(
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
        let s = crate::lock_settings(settings);
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

    // In auto mode, remember what whisper detected: it selects the command
    // vocabulary section for this utterance (M1.3).
    if lang == "auto" {
        if let Some(code) = state
            .full_lang_id_from_state()
            .ok()
            .and_then(whisper_rs::get_lang_str)
        {
            crate::lock_settings(settings).detected_language = Some(code.to_string());
        }
    }

    let mut out = String::new();
    let n = state.full_n_segments().context("n_segments")?;
    for i in 0..n {
        let text = state.full_get_segment_text(i).unwrap_or_default();
        out.push_str(&text);
    }
    Ok(filter_hallucination(&out))
}

pub const HALLUCINATIONS: &[&str] = &[
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

pub fn filter_hallucination(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Same similarity path as command classification (M1.2): whisper varies
    // its hallucinations ("obrigado por assistir!" / "obrigado por assistir"),
    // and exact comparison missed every variant not literally in the list.
    if let Some((matched, score)) =
        matcher::match_exact(trimmed, HALLUCINATIONS, matcher::DEFAULT_THRESHOLD)
    {
        debug!(text = %trimmed, matched, score, "filtered hallucination");
        return String::new();
    }
    text.to_string()
}

/// State of the current utterance as seen by VAD.
pub struct SpeechSegment {
    pub samples: Vec<f32>,
    pub trailing_silence_frames: usize,
    pub started_speaking: bool,
    pub last_partial: Instant,
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
    pub fn push_frame(&mut self, frame: &[f32], is_speech: bool) {
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

    pub fn speaking(&self) -> bool {
        self.started_speaking && self.trailing_silence_frames == 0
    }

    pub fn should_finalize(&self, hang_frames: usize) -> bool {
        self.started_speaking && self.trailing_silence_frames >= hang_frames
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.trailing_silence_frames = 0;
        self.started_speaking = false;
        self.last_partial = Instant::now();
    }
}
