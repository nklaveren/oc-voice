//! Measuring a voiceprint against a recording — `oc-voice voices <arquivo>`.
//!
//! The lock's threshold is derived from how much your own enrolment samples
//! disagree with each other, which is the number that decides whether it will
//! ever reject *you*. The other half — how close somebody else scores — cannot
//! be measured from your voice alone, and a bar chosen without it is a bar
//! chosen from nothing. This is the instrument for that half: hand it a
//! recording of a different person and it says, in numbers, whether the stored
//! lock would have let them through.
//!
//! Decoding goes through ffmpeg. Only this path needs it — enrolling from the
//! microphone, which is what the button does, decodes nothing.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use voice_activity_detector::VoiceActivityDetector;

use crate::fbank;
use crate::process::CommandRunner;
use crate::voicelock;
use crate::voices::{cosine, SpeakerModel};
use crate::{TARGET_SAMPLE_RATE, VAD_FRAME_SAMPLES, VAD_SPEECH_THRESHOLD};

/// A stretch of speech found in the recording.
pub struct Segment {
    pub start: f32,
    pub seconds: f32,
    pub samples: Vec<f32>,
}

/// Decode anything ffmpeg understands into the one format everything here
/// speaks: 16 kHz, mono, f32.
pub fn decode(runner: &Arc<dyn CommandRunner>, path: &Path) -> Result<Vec<f32>> {
    let out = runner
        .output(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-i",
                &path.display().to_string(),
                "-f",
                "f32le",
                "-c:a",
                "pcm_f32le",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-",
            ],
        )
        .context("running ffmpeg; it is in the dev shell, not in the app's own dependencies")?;
    if !out.status.success() {
        return Err(anyhow!(
            "ffmpeg could not decode {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Cut the recording where the speaker paused, using the same VAD the live
/// pipeline uses. Enrolment and judgement then see segments of the same shape,
/// which is the only way the numbers from one apply to the other.
pub fn segments(samples: &[f32], hang_frames: usize) -> Result<Vec<Segment>> {
    let mut vad = VoiceActivityDetector::builder()
        .sample_rate(TARGET_SAMPLE_RATE)
        .chunk_size(VAD_FRAME_SAMPLES)
        .build()
        .context("building silero VAD")?;

    let mut out = Vec::new();
    let mut current: Vec<f32> = Vec::new();
    let mut start_frame = 0usize;
    let mut silence = 0usize;
    for (i, frame) in samples.chunks_exact(VAD_FRAME_SAMPLES).enumerate() {
        let speech = vad.predict(frame.to_vec()) >= VAD_SPEECH_THRESHOLD;
        if speech {
            if current.is_empty() {
                start_frame = i;
            }
            silence = 0;
            current.extend_from_slice(frame);
        } else if !current.is_empty() {
            silence += 1;
            // Keep the tail: a word ending softly is still the word.
            current.extend_from_slice(frame);
            if silence >= hang_frames {
                push(&mut out, &mut current, start_frame);
                silence = 0;
            }
        }
    }
    push(&mut out, &mut current, start_frame);
    Ok(out)
}

fn push(out: &mut Vec<Segment>, current: &mut Vec<f32>, start_frame: usize) {
    if current.is_empty() {
        return;
    }
    let samples = std::mem::take(current);
    out.push(Segment {
        start: (start_frame * VAD_FRAME_SAMPLES) as f32 / fbank::SAMPLE_RATE,
        seconds: samples.len() as f32 / fbank::SAMPLE_RATE,
        samples,
    });
}

/// Embed every segment long enough to mean anything.
fn embed_all(model: &mut SpeakerModel, segs: &[Segment]) -> Vec<(usize, Vec<f32>)> {
    let mut out = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        if s.seconds < voicelock::MIN_ENROL_SECONDS {
            continue;
        }
        let feats = fbank::compute_unit(&s.samples);
        let frames = fbank::frame_count(s.samples.len());
        match model.embed(&feats, frames) {
            Ok(e) => out.push((i, e)),
            Err(e) => tracing::warn!(segment = i, error = %e, "could not embed"),
        }
    }
    out
}

/// Normalised mean, for the per-segment report.
fn mean_of(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dims = vectors.first()?.len();
    let mut sum = vec![0.0f32; dims];
    for v in vectors {
        for (s, x) in sum.iter_mut().zip(v) {
            *s += x;
        }
    }
    let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
    (norm > 0.0).then(|| sum.into_iter().map(|v| v / norm).collect())
}

/// What one recording says about the stored lock.
pub fn report(runner: &Arc<dyn CommandRunner>, path: &Path, hang_frames: usize) -> Result<()> {
    println!("decodificando {}", path.display());
    let samples = decode(runner, path)?;
    let seconds = samples.len() as f32 / fbank::SAMPLE_RATE;
    let segs = segments(&samples, hang_frames)?;
    println!("  {seconds:.1}s de áudio, {} trecho(s) de fala", segs.len());

    let mut model = SpeakerModel::load(&voicelock::default_model_path())?;
    let embedded = embed_all(&mut model, &segs);
    if embedded.len() < 2 {
        return Err(anyhow!(
            "só {} trecho(s) acima de {}s — pouco para medir qualquer coisa",
            embedded.len(),
            voicelock::MIN_ENROL_SECONDS
        ));
    }
    let vectors: Vec<Vec<f32>> = embedded.iter().map(|(_, e)| e.clone()).collect();

    // Every segment, with how long it is and how far it sits from the middle
    // of the recording. A single odd stretch dragging the average down looks
    // completely different from a speaker who simply varies, and the average
    // alone cannot tell them apart.
    if let Some(centre) = mean_of(&vectors) {
        println!("\n  trechos:");
        for ((i, e), _) in embedded.iter().zip(&vectors) {
            println!(
                "    {:>5.1}s  {:>5.1}s de fala   {:.3} do centro",
                segs[*i].start,
                segs[*i].seconds,
                cosine(e, &centre)
            );
        }
    }

    // The check that does not depend on what is in the recording: cut one
    // stretch of speech in half and compare the halves. Same person, adjacent
    // seconds, same microphone — anything much below ~0.8 means the extractor
    // is wrong, not that the speaker varies.
    if let Some(longest) = segs.iter().max_by(|a, b| a.seconds.total_cmp(&b.seconds)) {
        let mid = longest.samples.len() / 2;
        let halves: Vec<Vec<f32>> = [&longest.samples[..mid], &longest.samples[mid..]]
            .iter()
            .filter_map(|half| {
                let feats = fbank::compute_unit(half);
                model.embed(&feats, fbank::frame_count(half.len())).ok()
            })
            .collect();
        if halves.len() == 2 {
            println!(
                "  metades do trecho mais longo ({:.1}s): {:.3}",
                longest.seconds,
                cosine(&halves[0], &halves[1])
            );
        }
    }

    // How much this speaker disagrees with themselves, measured the same way
    // the enrolment measures it. This is the floor any threshold has to sit
    // under, and it comes from the recording rather than from a guess.
    if let Some(own) = voicelock::build(&vectors) {
        println!(
            "\n  contra si mesma: pior {:.3}, média {:.3}  ({} trechos)",
            own.self_worst, own.self_mean, own.segments
        );
    }

    match voicelock::stored() {
        None => {
            println!("\n  não há voz gravada para comparar.");
            println!("  Grave a sua no Settings e rode isto de novo com a voz de outra");
            println!("  pessoa: aí o número que falta — o quanto um impostor pontua —");
            println!("  passa a existir.");
        }
        Some(lock) => {
            println!("\n  contra a voz gravada (limiar {:.3}):", lock.threshold);
            let mut worst = f32::MIN;
            let mut passed = 0;
            for (i, e) in &embedded {
                let score = cosine(e, &lock.centroid);
                let verdict = if score >= lock.threshold {
                    passed += 1;
                    "PASSA"
                } else {
                    "barra"
                };
                worst = worst.max(score);
                println!(
                    "    {:>5.1}s  {:>5.1}s de fala   {score:.3}  {verdict}",
                    segs[*i].start, segs[*i].seconds
                );
            }
            println!(
                "\n  {passed} de {} trechos passariam. Maior pontuação: {worst:.3}",
                embedded.len()
            );
            let margin = lock.threshold - worst;
            if margin > 0.0 {
                println!("  Folga até o limiar: {margin:.3}");
            } else {
                println!("  ATENÇÃO: esta voz alcança o limiar. Ele está baixo demais.");
            }
        }
    }
    Ok(())
}

/// Build the lock from a recording — `oc-voice voices --enrol <arquivo>`.
///
/// The button in Settings enrols from the microphone, and that costs fifteen
/// seconds of talking every single time the lock has to be rebuilt — which is
/// often, because every change to the extractor invalidates the stored
/// centroid. Worse, it leaves no way to ask *why* a lock came out the way it
/// did: the audio it was built from is gone the moment it is used.
///
/// A file fixes both. The same enrolment, from a recording that stays: it can
/// be re-run after a change, compared against the last one, and handed to the
/// probe above to see what it would let through.
pub fn enrol(runner: &Arc<dyn CommandRunner>, path: &Path, hang_frames: usize) -> Result<()> {
    println!("decodificando {}", path.display());
    let samples = decode(runner, path)?;
    let segs = segments(&samples, hang_frames)?;
    let mut model = SpeakerModel::load(&voicelock::default_model_path())?;
    let embedded = embed_all(&mut model, &segs);

    let speech: f32 = embedded.iter().map(|(i, _)| segs[*i].seconds).sum();
    println!(
        "  {:.1}s utilizáveis em {} trecho(s) de {}s ou mais",
        speech,
        embedded.len(),
        voicelock::MIN_ENROL_SECONDS
    );
    if speech < voicelock::ENROL_SECONDS {
        return Err(anyhow!(
            "só {speech:.1}s de fala aproveitável — a gravação precisa de pelo menos {}s",
            voicelock::ENROL_SECONDS
        ));
    }
    // Not the same requirement as the seconds, and it is the one that catches
    // the recording made in a single unbroken breath: the bar is derived from
    // how much the stretches disagree with each other, so there have to be
    // stretches.
    if embedded.len() < voicelock::MIN_ENROL_SEGMENTS {
        return Err(anyhow!(
            "só {} trecho(s) separados — são precisos {}. Fale em frases, com pausas entre elas.",
            embedded.len(),
            voicelock::MIN_ENROL_SEGMENTS
        ));
    }

    let vectors: Vec<Vec<f32>> = embedded.into_iter().map(|(_, e)| e).collect();
    let store = voicelock::build(&vectors)
        .ok_or_else(|| anyhow!("os trechos não formaram uma voz utilizável"))?;
    store.save();
    println!(
        "\n  voz gravada: limiar {:.3}, de {} trechos (pior {:.3}, média {:.3})",
        store.threshold, store.segments, store.self_worst, store.self_mean
    );
    println!("  Rode `oc-voice voices <outra-gravação>` para ver o que ela deixa passar.");
    Ok(())
}
