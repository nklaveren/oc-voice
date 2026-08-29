use crate::commands::matcher;
use crate::{AppSettings, TranscribeMode, MIN_TRANSCRIBE_SAMPLES, TARGET_SAMPLE_RATE};
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{debug, info};
use whisper_rs::{FullParams, SamplingStrategy};

/// `single_segment` forces whisper to emit one segment. The live pipeline
/// wants that — VAD already bounded the audio to one utterance — but on a
/// long continuous recording it truncates badly, so the benchmark passes
/// false and lets whisper segment on its own.
pub fn transcribe_with(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    settings: &Arc<Mutex<AppSettings>>,
    single_segment: bool,
) -> Result<String> {
    let (language, translate) = {
        let s = crate::lock_settings(settings);
        (s.language.clone(), s.mode == TranscribeMode::Translate)
    };
    let mut throwaway = LanguageLock {
        locked: crate::lock_settings(settings).detected_language.clone(),
        ..Default::default()
    };
    transcribe_locked(
        state,
        audio,
        &TranscribeOpts {
            language,
            translate,
            // The benchmark path: one long recording of one passage, and
            // the caller seeded the lock deliberately just above. Honour it.
            pin_language: true,
            single_segment,
        },
        &mut throwaway,
    )
}

/// Everything about *how* to transcribe one segment that is not the audio.
///
/// These used to be read from the shared `AppSettings` inside the transcribe
/// call. That was fine while there was one audio stream; with the microphone
/// and the meeting running at once it is not. Both would consult and overwrite
/// the same "detected language", so a meeting in English pinned `en` globally
/// and the next Portuguese utterance from the mic was decoded as English.
/// Per-stream settings are now passed in explicitly.
pub struct TranscribeOpts {
    /// The UI's source-language selection: `auto`, or an explicit code.
    pub language: String,
    /// Whether a settled language may be forced onto whisper as the source
    /// hint. Only the meeting sets it — see `LanguageLock`.
    pub pin_language: bool,
    /// Whisper's translate task, which emits English whatever the source.
    pub translate: bool,
    /// Force one output segment. The live pipeline wants it — VAD already
    /// bounded the audio to one utterance — but it truncates long recordings.
    pub single_segment: bool,
}

/// Whisper re-detects the language on every segment, and a three-second
/// utterance is thin evidence: a real meeting produced `es` at p=0.24 among
/// a run of `en` at p=0.999, and that one segment came out as "¿Qué?".
///
/// The lock only pins a language after consecutive detections agree, and only
/// unpins after the same number disagree. Once pinned it is passed to whisper
/// as an explicit hint, which is both stabler and more accurate than making
/// it guess again every few seconds.
/// The pinned language lives here rather than in the shared settings, so two
/// concurrent streams cannot pin over each other.
///
/// **The hint is the meeting's alone** (`TranscribeOpts::pin_language`).
/// A meeting has one speaker in one language and drifts only under thin
/// evidence, which is the case the pin was built for. Your own microphone
/// is the opposite: the Tutor's whole premise is switching between English
/// and Portuguese mid-turn, and dictation switches whenever you do. Forcing
/// the hint there does not stabilize anything, it freezes the decoder in
/// the language you happened to say three times — measured, from a real
/// session: after nineteen turns alternating correctly, `pt` settled and
/// the English passage "The tide does not arrive all at once. It comes in
/// slow steps" came back as "A tira não chega no almoço, vem em um passo
/// lentamente". Not a mishearing — whisper was told to decode English audio
/// as Portuguese, and obeyed.
///
/// What the lock keeps doing on every stream is *observing*: the settled
/// code is what picks the command vocabulary. That never touched decoding.
#[derive(Default)]
pub struct LanguageLock {
    candidate: Option<String>,
    streak: usize,
    locked: Option<String>,
    /// Languages the speaker actually speaks. Empty means anything goes.
    allowed: Vec<String>,
}

impl LanguageLock {
    /// How many agreeing detections it takes to pin, or disagreeing to drop.
    const AGREEMENT: usize = 3;

    /// Restrict detection to the languages someone actually speaks.
    ///
    /// From a live log: a Portuguese sentence was detected as German at
    /// p = 0.198 and came back as "Weil Brot sagt…". Whisper will name any of
    /// its hundred languages on thin evidence, and a three-second utterance
    /// is thin evidence. Naming the two or three you use turns that from a
    /// plausible answer into an impossible one.
    pub fn restricted_to(allowed: Vec<String>) -> Self {
        LanguageLock {
            allowed,
            ..Default::default()
        }
    }

    /// Returns the language to lock, the first time a run reaches AGREEMENT.
    pub fn observe(&mut self, code: &str) -> Option<&str> {
        // A language nobody here speaks is not a disagreement, it is noise:
        // ignoring it outright also lets a real run reach agreement sooner,
        // because a stray reading no longer resets the streak.
        if !self.allowed.is_empty() && !self.allowed.iter().any(|a| a == code) {
            debug!(code, "detection outside the configured languages, ignored");
            return None;
        }
        if self.candidate.as_deref() == Some(code) {
            self.streak += 1;
        } else {
            self.candidate = Some(code.to_string());
            self.streak = 1;
        }
        if self.streak == Self::AGREEMENT {
            self.locked = self.candidate.clone();
            self.locked.as_deref()
        } else {
            None
        }
    }

    /// The language this stream has settled on, if any.
    pub fn locked(&self) -> Option<&str> {
        self.locked.as_deref()
    }
}

pub fn transcribe_locked(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    opts: &TranscribeOpts,
    lock: &mut LanguageLock,
) -> Result<String> {
    let single_segment = opts.single_segment;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(single_segment);
    params.set_n_threads(num_cpus::get_physical() as i32);
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_split_on_word(true);

    let lang = opts.language.clone();
    let translate = opts.translate;
    // A locked language wins over detection, but only where pinning is
    // allowed at all: see LanguageLock.
    let locked = if opts.pin_language {
        lock.locked().map(str::to_string)
    } else {
        None
    };

    // `language` is the SOURCE hint, not the output language — whisper's
    // translate task only ever emits English. In Translate mode the source is
    // whatever the meeting happens to be speaking, so forcing the UI's
    // selection there tells whisper to decode English audio as Portuguese and
    // it returns noise. Detection is the only correct answer for system audio.
    let hint = if lang == "auto" || translate {
        // Once the source language is known, telling whisper beats making it
        // guess again on every three-second segment — and stops a
        // low-confidence guess from derailing one utterance (M7.2).
        locked.clone()
    } else {
        Some(lang.clone())
    };
    if let Some(ref h) = hint {
        params.set_language(Some(h));
    } else {
        params.set_language(None);
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

    // Feed the detection to the lock. Only agreement pins a language; a
    // single reading never does.
    //
    // The guard is "whisper actually detected", not "nothing is settled":
    // with pinning off the hint stays empty forever, so the stream keeps
    // observing every utterance instead of freezing on the first agreement.
    // That is what keeps `detected_language` — and with it the command
    // vocabulary — following you when you change language.
    if hint.is_none() {
        if let Some(code) = state
            .full_lang_id_from_state()
            .ok()
            .and_then(whisper_rs::get_lang_str)
        {
            if let Some(settled) = lock.observe(code) {
                info!(language = settled, "source language locked");
            }
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
    /// Audio kept from *before* the VAD called it speech.
    ///
    /// Silero needs energy to accumulate before it crosses the threshold, so
    /// the soft onset of a word sits below it for the first few 32 ms frames.
    /// Those frames used to be thrown away. In a long sentence that costs a
    /// syllable and whisper recovers from context; in "envia" — a command
    /// about 400 ms long end to end — losing the onset destroys the whole
    /// thing. Which is exactly what it did: long speech transcribed, short
    /// commands vanished.
    preroll: VecDeque<f32>,
    preroll_capacity: usize,
}

impl Default for SpeechSegment {
    fn default() -> Self {
        Self {
            samples: Vec::with_capacity(TARGET_SAMPLE_RATE as usize * 5),
            trailing_silence_frames: 0,
            started_speaking: false,
            last_partial: Instant::now(),
            preroll: VecDeque::new(),
            preroll_capacity: 0,
        }
    }
}

impl SpeechSegment {
    /// How much audio to keep ahead of the VAD's decision, in milliseconds.
    /// Set from config each tick so it can be tuned without recompiling.
    pub fn set_preroll_ms(&mut self, ms: u64) {
        self.preroll_capacity = (ms as usize * TARGET_SAMPLE_RATE as usize) / 1000;
        while self.preroll.len() > self.preroll_capacity {
            self.preroll.pop_front();
        }
    }

    pub fn push_frame(&mut self, frame: &[f32], is_speech: bool) {
        if is_speech {
            if !self.started_speaking {
                self.started_speaking = true;
                self.last_partial = Instant::now();
                // The word started before the VAD noticed. Take the buffer
                // with it, or the first syllable is gone.
                self.samples.extend(self.preroll.drain(..));
            }
            self.samples.extend_from_slice(frame);
            self.trailing_silence_frames = 0;
        } else if self.started_speaking {
            self.samples.extend_from_slice(frame);
            self.trailing_silence_frames += 1;
        } else {
            // Not speech yet: hold it in case the next frame says otherwise.
            self.preroll.extend(frame.iter().copied());
            while self.preroll.len() > self.preroll_capacity {
                self.preroll.pop_front();
            }
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
        // The pre-roll is deliberately NOT cleared: the tail of the utterance
        // that just ended is the run-up to whatever is said next, and two
        // commands in a row would otherwise lose the second one's onset.
    }
}

#[cfg(test)]
#[path = "asr_tests.rs"]
mod tests;
