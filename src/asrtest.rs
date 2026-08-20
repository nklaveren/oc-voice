//! Read-aloud ASR benchmark — `oc-voice asr-test <model>`.
//!
//! Prints a reference passage block by block, records while you read each
//! one, transcribes it through the same whisper settings the pipeline uses,
//! and reports word error rate per block. Per-block numbers are the point:
//! they say WHERE the ASR is failing (command vocabulary? numbers? English
//! technical terms?), which a single overall figure hides.
//!
//! Nothing is written to disk — audio lives in memory for the length of one
//! block and is dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ringbuf::traits::*;
use whisper_rs::{WhisperContext, WhisperContextParameters};

use crate::asr::transcribe_with;
use crate::audio::capture::run_capture;
use crate::{AppSettings, TranscribeMode, TARGET_SAMPLE_RATE};

const REFERENCE: &str = include_str!("../tests/fixtures/passagem.txt");

struct Block {
    title: String,
    lines: Vec<String>,
}

fn parse_blocks() -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for raw in REFERENCE.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("# --- ") {
            let title = rest.trim_end_matches(" ---").to_string();
            blocks.push(Block {
                title,
                lines: Vec::new(),
            });
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(b) = blocks.last_mut() {
                b.lines.push(line.to_string());
            }
        }
    }
    blocks
}

/// Words, lowercased, stripped of punctuation, with numbers normalized —
/// the unit WER counts.
///
/// Whisper writes numbers as digits ("4", "3100", "2.14.0") where the
/// reference spells them out ("quatro", "três mil e cem"). Comparing those
/// literally scores a correct transcription as a wall of errors, which is
/// what made the first numbers-block measurement meaningless. Both sides are
/// reduced to digits before counting.
fn words(text: &str) -> Vec<String> {
    let folded = crate::commands::fold_diacritics(&text.to_lowercase());
    let raw: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric() && c != '.')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        // Version numbers are spoken "dois ponto quatorze" and written
        // "2.14"; the spoken separator has no written counterpart.
        if (raw[i] == "ponto" || raw[i] == "virgula")
            && !out.is_empty()
            && out[out.len() - 1].chars().all(|c| c.is_ascii_digit())
            && number_run(&raw[i + 1..]).is_some()
        {
            i += 1;
            continue;
        }
        // Longest run of number words collapses into one digit token, so
        // "tres mil e cem" and "3100" compare equal.
        if let Some((value, consumed)) = number_run(&raw[i..]) {
            out.push(value.to_string());
            i += consumed;
            continue;
        }
        // A digit string stays as-is; "2.14.0" splits into its parts.
        for part in raw[i].split('.') {
            if !part.is_empty() {
                out.push(part.to_string());
            }
        }
        i += 1;
    }
    out
}

/// Value of the longest leading run of Portuguese number words, and how many
/// tokens it consumed.
///
/// Composition in Portuguese needs a connector: "vinte e sete" is 27, but
/// "um dois tres" is three separate numbers, not 6. So a run only extends
/// across an explicit "e", or into "mil". Everything else ends the run.
fn number_run(tokens: &[String]) -> Option<(u64, usize)> {
    const UNITS: &[(&str, u64)] = &[
        ("zero", 0),
        ("um", 1),
        ("uma", 1),
        ("dois", 2),
        ("duas", 2),
        ("tres", 3),
        ("quatro", 4),
        ("cinco", 5),
        ("seis", 6),
        ("sete", 7),
        ("oito", 8),
        ("nove", 9),
        ("dez", 10),
        ("onze", 11),
        ("doze", 12),
        ("treze", 13),
        ("quatorze", 14),
        ("catorze", 14),
        ("quinze", 15),
        ("dezesseis", 16),
        ("dezessete", 17),
        ("dezoito", 18),
        ("dezenove", 19),
        ("vinte", 20),
        ("trinta", 30),
        ("quarenta", 40),
        ("cinquenta", 50),
        ("sessenta", 60),
        ("setenta", 70),
        ("oitenta", 80),
        ("noventa", 90),
        ("cem", 100),
        ("cento", 100),
        ("duzentos", 200),
        ("trezentos", 300),
        ("quatrocentos", 400),
        ("quinhentos", 500),
        ("seiscentos", 600),
        ("setecentos", 700),
        ("oitocentos", 800),
        ("novecentos", 900),
    ];
    let value_of = |t: &String| UNITS.iter().find(|(w, _)| w == t).map(|(_, v)| *v);

    let first = value_of(tokens.first()?)?;
    let mut total: u64 = 0;
    let mut current = first;
    let mut consumed = 1usize;

    loop {
        let next = tokens.get(consumed);
        match next.map(String::as_str) {
            // "tres mil", "mil" on its own after a value
            Some("mil") => {
                total += current.max(1) * 1000;
                current = 0;
                consumed += 1;
            }
            // Connector: only continues the run if a number really follows.
            Some("e") => match tokens.get(consumed + 1).and_then(&value_of) {
                Some(v) => {
                    current += v;
                    consumed += 2;
                }
                None if tokens.get(consumed + 1).map(String::as_str) == Some("mil") => {
                    total += current.max(1) * 1000;
                    current = 0;
                    consumed += 2;
                }
                None => break,
            },
            _ => break,
        }
    }
    Some((total + current, consumed))
}

/// Levenshtein distance over words, and the aligned operations for display.
fn wer(reference: &[String], hypothesis: &[String]) -> (usize, Vec<String>) {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate().take(n + 1) {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate().take(m + 1) {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(reference[i - 1] != hypothesis[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
        }
    }
    // Backtrack for a readable diff of what actually went wrong.
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && reference[i - 1] == hypothesis[j - 1] {
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 {
            ops.push(format!("{} -> {}", reference[i - 1], hypothesis[j - 1]));
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            ops.push(format!("{} -> [faltou]", reference[i - 1]));
            i -= 1;
        } else {
            ops.push(format!("[inventou] {}", hypothesis[j - 1]));
            j -= 1;
        }
    }
    ops.reverse();
    (d[n][m], ops)
}

fn wait_for_enter() {
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

pub fn run(model_path: &str) -> Result<()> {
    let blocks = parse_blocks();
    if blocks.is_empty() {
        anyhow::bail!("passagem.txt sem blocos");
    }

    println!("carregando modelo...");
    let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        .context("loading model")?;
    let mut state = ctx.create_state().context("creating state")?;
    let settings = Arc::new(Mutex::new(AppSettings {
        language: "pt".to_string(),
        mode: TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
    }));

    println!(
        "\n{} blocos. Para cada um: Enter começa a gravar, Enter de novo para.\n\
         Leia no ritmo em que você ditaria de verdade.\n",
        blocks.len()
    );

    let mut total_errors = 0usize;
    let mut total_words = 0usize;
    let mut summary: Vec<(String, usize, usize)> = Vec::new();

    for block in &blocks {
        println!("\n\x1b[1m── {} ──\x1b[0m", block.title);
        for l in &block.lines {
            println!("  {l}");
        }
        print!("\n[Enter para gravar] ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        wait_for_enter();

        let running = Arc::new(AtomicBool::new(true));
        let rb = ringbuf::HeapRb::<f32>::new(TARGET_SAMPLE_RATE as usize * 120);
        let (producer, mut consumer) = rb.split();
        let cap_running = running.clone();
        let handle = std::thread::spawn(move || {
            let _ = run_capture(producer, cap_running);
        });

        print!("\x1b[31m● gravando\x1b[0m — [Enter para parar] ");
        let _ = std::io::stdout().flush();
        wait_for_enter();
        running.store(false, Ordering::SeqCst);
        let _ = handle.join();

        let mut audio = Vec::new();
        while let Some(v) = consumer.try_pop() {
            audio.push(v);
        }
        let seconds = audio.len() as f32 / TARGET_SAMPLE_RATE as f32;
        if audio.len() < TARGET_SAMPLE_RATE as usize {
            println!("  [áudio curto demais ({seconds:.1} s), bloco pulado]");
            continue;
        }

        // Long continuous reading: let whisper segment it (see transcribe_with).
        let text = transcribe_with(&mut state, &audio, &settings, false)?;
        let reference = words(&block.lines.join(" "));
        let hypothesis = words(&text);
        let (errors, ops) = wer(&reference, &hypothesis);
        let rate = if reference.is_empty() {
            0.0
        } else {
            errors as f32 / reference.len() as f32 * 100.0
        };

        println!("\n  ouvido: {}", text.trim());
        println!(
            "  WER {:.1}% ({errors} erros em {} palavras, {seconds:.1} s de áudio)",
            rate,
            reference.len()
        );
        if !ops.is_empty() {
            println!("  erros:");
            for op in ops.iter().take(12) {
                println!("    {op}");
            }
            if ops.len() > 12 {
                println!("    ... e mais {}", ops.len() - 12);
            }
        }

        // The homophone block is a reference floor, not a target: no ASR can
        // separate "sessão" from "seção" without context, so counting it in
        // the overall figure would just add noise.
        let informational = block.title.contains("referência");
        if !informational {
            total_errors += errors;
            total_words += reference.len();
        }
        summary.push((block.title.clone(), errors, reference.len()));
    }

    println!("\n\x1b[1m── resumo ──\x1b[0m");
    for (title, errors, total) in &summary {
        let rate = if *total == 0 {
            0.0
        } else {
            *errors as f32 / *total as f32 * 100.0
        };
        let note = if title.contains("referência") {
            "  [fora do total]"
        } else {
            ""
        };
        println!(
            "  {:<44} WER {:5.1}%  ({errors}/{total}){note}",
            title, rate
        );
    }
    if total_words > 0 {
        println!(
            "\n  GERAL  WER {:.1}%  ({total_errors}/{total_words})",
            total_errors as f32 / total_words as f32 * 100.0
        );
    }
    println!("\n  referência: WER abaixo de 10% é utilizável; acima de 25% o");
    println!("  problema é sinal ou modelo, não ajuste de limiar.");
    Ok(())
}

#[cfg(test)]
#[path = "asrtest_tests.rs"]
mod tests;
