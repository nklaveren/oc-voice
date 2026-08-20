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

use crate::asr::transcribe;
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

/// Words, lowercased, stripped of punctuation — the unit WER counts.
fn words(text: &str) -> Vec<String> {
    crate::commands::fold_diacritics(&text.to_lowercase())
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
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
        mode: TranscribeMode::Input,
        detected_language: None,
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

        let text = transcribe(&mut state, &audio, &settings)?;
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

        total_errors += errors;
        total_words += reference.len();
        summary.push((block.title.clone(), errors, reference.len()));
    }

    println!("\n\x1b[1m── resumo ──\x1b[0m");
    for (title, errors, total) in &summary {
        let rate = if *total == 0 {
            0.0
        } else {
            *errors as f32 / *total as f32 * 100.0
        };
        println!("  {:<38} WER {:5.1}%  ({errors}/{total})", title, rate);
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
mod tests {
    use super::*;

    #[test]
    fn reference_passage_parses_into_blocks() {
        let blocks = parse_blocks();
        assert!(
            blocks.len() >= 6,
            "esperados 6+ blocos, veio {}",
            blocks.len()
        );
        assert!(blocks.iter().all(|b| !b.lines.is_empty()));
        // The command vocabulary must actually appear in the passage,
        // otherwise the test measures something the app never has to hear.
        let all = blocks
            .iter()
            .flat_map(|b| b.lines.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let spoken = words(&all);
        let config = crate::config::Config::embedded();
        let vocab = config.vocab("pt").unwrap();
        for word in vocab.send.iter().chain(vocab.cancel.iter()) {
            let w = words(word);
            assert!(
                w.iter().all(|t| spoken.contains(t)),
                "vocabulário {word:?} não aparece na passagem"
            );
        }
    }

    #[test]
    fn wer_counts_substitutions_insertions_and_deletions() {
        let r = words("o gato subiu no telhado");
        assert_eq!(wer(&r, &words("o gato subiu no telhado")).0, 0);
        assert_eq!(wer(&r, &words("o rato subiu no telhado")).0, 1);
        assert_eq!(wer(&r, &words("o gato subiu telhado")).0, 1);
        assert_eq!(wer(&r, &words("o gato subiu logo no telhado")).0, 1);
    }
}
