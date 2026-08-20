//! Tests for transcription: the language lock, the hallucination filter,
//! and the segmenter — including the pre-roll that keeps a short command
//! from losing its own first syllable.

use super::{LanguageLock, SpeechSegment};

/// A 512-sample VAD frame, filled with a recognisable value so the test
/// can tell which frames survived into the segment.
fn frame(value: f32) -> Vec<f32> {
    vec![value; crate::VAD_FRAME_SAMPLES]
}

#[test]
fn the_onset_of_a_word_survives_the_vads_hesitation() {
    // The bug this pins, reported from live use: "os comandos não são
    // capturados, somente falas grandes". Silero needs energy to build
    // before it crosses the threshold, so the first frames of a word read
    // as silence and used to be discarded. A long sentence loses a
    // syllable and whisper recovers from context; "envia" is about 400 ms
    // end to end, so losing the onset loses the command.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(300);

    // Three frames of the word that the VAD has not recognised yet.
    for _ in 0..3 {
        seg.push_frame(&frame(0.5), false);
    }
    assert!(seg.samples.is_empty(), "nothing committed before speech");

    // Now it notices.
    seg.push_frame(&frame(0.9), true);

    assert_eq!(
        seg.samples.len(),
        crate::VAD_FRAME_SAMPLES * 4,
        "the three hesitating frames must arrive with the fourth"
    );
    assert_eq!(seg.samples[0], 0.5, "and they must come first, in order");
    assert_eq!(seg.samples[crate::VAD_FRAME_SAMPLES * 3], 0.9);
}

#[test]
fn the_preroll_never_grows_past_its_window() {
    // It runs for the whole idle period between utterances; unbounded, a
    // quiet hour would be held in memory.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(64); // two frames' worth
    for _ in 0..500 {
        seg.push_frame(&frame(0.1), false);
    }
    seg.push_frame(&frame(0.9), true);
    assert_eq!(
        seg.samples.len(),
        crate::VAD_FRAME_SAMPLES * 3,
        "two frames of pre-roll plus the speech frame, not 500"
    );
}

#[test]
fn a_second_command_in_a_row_keeps_its_own_onset() {
    // reset() deliberately does not clear the pre-roll: the tail of one
    // utterance is the run-up to the next, and "envia. câmbio." would
    // otherwise lose the second word exactly like the first bug.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(300);
    seg.push_frame(&frame(0.9), true);
    seg.push_frame(&frame(0.0), false);
    seg.reset();

    seg.push_frame(&frame(0.4), false);
    seg.push_frame(&frame(0.9), true);
    assert_eq!(seg.samples.len(), crate::VAD_FRAME_SAMPLES * 2);
    assert_eq!(seg.samples[0], 0.4, "the second onset was kept too");
}

#[test]
fn zero_preroll_behaves_exactly_as_before() {
    // Someone who sets preroll_ms = 0 gets the old behaviour rather than
    // a panic or a surprise.
    let mut seg = SpeechSegment::default();
    seg.set_preroll_ms(0);
    seg.push_frame(&frame(0.5), false);
    seg.push_frame(&frame(0.9), true);
    assert_eq!(seg.samples.len(), crate::VAD_FRAME_SAMPLES);
    assert_eq!(seg.samples[0], 0.9);
}

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
        read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").unwrap_or_else(|| "?".into())
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
            println!("  AVISO: máquina ocupada — este número não representa a máquina em repouso");
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
