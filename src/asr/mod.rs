use crate::commands::matcher;
use crate::{AppSettings, TranscribeMode, MIN_TRANSCRIBE_SAMPLES, TARGET_SAMPLE_RATE};
use anyhow::{Context, Result};
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
    let mut throwaway = LanguageLock::default();
    transcribe_locked(state, audio, settings, single_segment, &mut throwaway)
}

/// Whisper re-detects the language on every segment, and a three-second
/// utterance is thin evidence: a real meeting produced `es` at p=0.24 among
/// a run of `en` at p=0.999, and that one segment came out as "¿Qué?".
///
/// The lock only pins a language after consecutive detections agree, and only
/// unpins after the same number disagree. Once pinned it is passed to whisper
/// as an explicit hint, which is both stabler and more accurate than making
/// it guess again every few seconds.
#[derive(Default)]
pub struct LanguageLock {
    candidate: Option<String>,
    streak: usize,
}

impl LanguageLock {
    /// How many agreeing detections it takes to pin, or disagreeing to drop.
    const AGREEMENT: usize = 3;

    /// Returns the language to lock, the first time a run reaches AGREEMENT.
    pub fn observe(&mut self, code: &str) -> Option<&str> {
        if self.candidate.as_deref() == Some(code) {
            self.streak += 1;
        } else {
            self.candidate = Some(code.to_string());
            self.streak = 1;
        }
        if self.streak == Self::AGREEMENT {
            self.candidate.as_deref()
        } else {
            None
        }
    }
}

pub fn transcribe_locked(
    state: &mut whisper_rs::WhisperState,
    audio: &[f32],
    settings: &Arc<Mutex<AppSettings>>,
    single_segment: bool,
    lock: &mut LanguageLock,
) -> Result<String> {
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

    let (lang, translate) = {
        let s = crate::lock_settings(settings);
        (s.language.clone(), s.mode == TranscribeMode::Translate)
    };
    // A locked language wins over detection: see LanguageLock.
    let locked = crate::lock_settings(settings).detected_language.clone();

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
    if hint.is_none() {
        if let Some(code) = state
            .full_lang_id_from_state()
            .ok()
            .and_then(whisper_rs::get_lang_str)
        {
            if let Some(settled) = lock.observe(code) {
                info!(language = settled, "source language locked");
                crate::lock_settings(settings).detected_language = Some(settled.to_string());
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

#[cfg(test)]
mod tests {
    use super::LanguageLock;

    /// Print what the machine was doing during the run. A latency number
    /// without its conditions misleads later: the first CPU measurement of
    /// this benchmark was taken under a 40 W power cap with a SQL Server VM
    /// running, and got published as a hardware verdict.
    fn report_conditions() {
        let read = |p: &str| {
            std::fs::read_to_string(p)
                .ok()
                .map(|s| s.trim().to_string())
        };
        println!(
            "  perfil: {}   governor: {}",
            read("/sys/firmware/acpi/platform_profile").unwrap_or_else(|| "?".into()),
            read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
                .unwrap_or_else(|| "?".into())
        );
        if let Some(w) = read("/sys/class/powercap/intel-rapl:0/constraint_0_power_limit_uw")
            .and_then(|v| v.parse::<u64>().ok())
        {
            println!("  limite RAPL: {} W", w / 1_000_000);
        }
        if let Some(load) = read("/proc/loadavg") {
            let first = load.split_whitespace().next().unwrap_or("?");
            println!("  load average: {first}");
            if first.parse::<f32>().unwrap_or(0.0) > 2.0 {
                println!(
                    "  AVISO: máquina ocupada — este número não representa a máquina em repouso"
                );
            }
        }
    }

    #[test]
    fn a_single_odd_detection_never_pins_a_language() {
        // The live failure: a run of `en` with one `es` at p=0.24 in the
        // middle, which came out as "¿Qué?" in an English meeting.
        let mut lock = LanguageLock::default();
        assert_eq!(lock.observe("en"), None);
        assert_eq!(lock.observe("en"), None);
        assert_eq!(lock.observe("en"), Some("en"), "three agreeing pins it");
        // The stray reading resets the streak but must not pin anything.
        assert_eq!(lock.observe("es"), None);
        assert_eq!(lock.observe("en"), None);
        assert_eq!(lock.observe("en"), None);
        assert_eq!(lock.observe("en"), Some("en"));
    }

    #[test]
    fn a_genuine_language_change_still_settles() {
        // Someone switching to Spanish for the rest of the call must be
        // followed, just not on the first utterance.
        let mut lock = LanguageLock::default();
        for _ in 0..3 {
            lock.observe("en");
        }
        assert_eq!(lock.observe("es"), None);
        assert_eq!(lock.observe("es"), None);
        assert_eq!(lock.observe("es"), Some("es"));
    }

    /// Latency measurement for M5.4 — run explicitly, needs the model:
    ///   OC_VOICE_MODEL=models/ggml-large-v3-turbo-q8_0.bin \
    ///   cargo test --release [--no-default-features --features cpu] \
    ///     -- --ignored --nocapture measure_transcribe_latency
    ///
    /// Times a 3 s window. Whisper's encoder cost is dominated by the padded
    /// mel window, so silence is a fair stand-in for speech within ~10%.
    #[test]
    #[ignore]
    fn measure_transcribe_latency() {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
        let model = std::env::var("OC_VOICE_MODEL").expect("set OC_VOICE_MODEL");
        report_conditions();
        let load_start = std::time::Instant::now();
        let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
            .expect("model loads");
        let mut state = ctx.create_state().expect("state");
        println!("model load: {} ms", load_start.elapsed().as_millis());
        let audio = vec![0.0_f32; 3 * 16_000];
        for run in 0..3 {
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_single_segment(true);
            params.set_language(Some("pt"));
            let t = std::time::Instant::now();
            state.full(params, &audio).expect("full");
            println!("run {run}: 3 s audio in {} ms", t.elapsed().as_millis());
        }
    }
}
