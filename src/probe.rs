//! Interactive matcher probe — `oc-voice probe`.
//!
//! Reads utterances from stdin (one per line, as if whisper had emitted
//! them) and prints the whole decision chain with scores: normalization,
//! prefix, every vocabulary pool, template matching, slot resolution, and
//! window/monitor resolution against the LIVE session. Nothing is ever
//! dispatched or typed — hyprctl is only read.
//!
//! This is how you test the Jaro-Winkler discovery by hand: type the
//! mishearings ("sambio", "monitor da direta") and watch where they land.

use std::io::BufRead;
use std::sync::Arc;

use crate::commands::matcher;
use crate::config::{Config, LangVocab};
use crate::process::{CommandRunner, SystemRunner};
use crate::wm::target;

fn pool(label: &str, spoken: &str, words: &[String], threshold: f64) {
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    // Best candidate regardless of threshold, so refusals show their score.
    let mut best: Option<(&str, f64)> = None;
    let norm = matcher::normalize(spoken);
    let n = norm.split_whitespace().count();
    for &c in &refs {
        let cn = matcher::normalize(c);
        if cn.split_whitespace().count() != n {
            continue;
        }
        let s = strsim::jaro_winkler(&norm, &cn);
        if best.is_none_or(|(_, b)| s > b) {
            best = Some((c, s));
        }
    }
    match best {
        Some((c, s)) if s >= threshold => println!("  {label:<12} {c:?}  {s:.2}  ACEITO"),
        Some((c, s)) if s >= 0.5 => println!("  {label:<12} {c:?}  {s:.2}  abaixo do limiar"),
        _ => {
            let gate = refs
                .iter()
                .all(|c| matcher::normalize(c).split_whitespace().count() != n);
            if gate && !refs.is_empty() {
                println!("  {label:<12} [gate de contagem: fala tem {n} palavra(s), nenhum candidato tem {n}]");
            }
        }
    }
}

fn probe_one(spoken: &str, vocab: &LangVocab, config: &Config, runner: &Arc<dyn CommandRunner>) {
    let threshold = config.threshold();
    println!("normalizado: {:?}", matcher::normalize(spoken));

    pool("send", spoken, &vocab.send, threshold);
    pool("cancel", spoken, &vocab.cancel, threshold);
    pool("newline", spoken, &vocab.newline, threshold);
    pool("confirm", spoken, &vocab.confirm, threshold);
    pool("deny", spoken, &vocab.deny, threshold);
    let wm: Vec<String> = vocab.wm_commands.keys().cloned().collect();
    pool("wm_command", spoken, &wm, threshold);

    // Templates, with slot values.
    let directions: Vec<&str> = vocab.directions.keys().map(String::as_str).collect();
    let templates: Vec<matcher::Template> = vocab
        .templates
        .iter()
        .map(|t| {
            matcher::Template::new(
                &t.pattern,
                &[
                    ("direcao", directions.as_slice()),
                    ("numero", &[]),
                    ("alvo", &[]),
                    ("monitor", &[]),
                ],
            )
        })
        .collect();
    if let Some(m) = matcher::match_template(spoken, &templates, threshold) {
        let def = &vocab.templates[m.template_index];
        println!(
            "  template     {:?} -> {}  {:.2}  slots {:?}",
            def.pattern, def.action, m.score, m.slots
        );
    }

    // Send-to prefix + live-window target resolution.
    let norm = {
        let lower = crate::commands::fold_diacritics(&spoken.to_lowercase());
        lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    for prefix in &vocab.send_to {
        let p = crate::commands::fold_diacritics(&prefix.to_lowercase());
        if let Some(pos) = norm.find(&format!("{p} ")) {
            let target_spoken = norm[pos + p.len()..].trim();
            println!("  send_to      prefixo {prefix:?} -> alvo falado {target_spoken:?}");
            let windows = target::live_windows(runner);
            match target::resolve(target_spoken, &vocab.targets, &windows, threshold) {
                Some(t) => {
                    let conf = if t.score >= config.confirm_below() {
                        "direto"
                    } else {
                        "pediria confirmação"
                    };
                    println!(
                        "               janela: {} ({:.2}) — {conf}",
                        t.class, t.score
                    );
                }
                None => println!("               nenhuma janela casou — pediria confirmação"),
            }
            break;
        }
    }

    // Bare target probe (what "foca o X" would find).
    let windows = target::live_windows(runner);
    if let Some(t) = target::resolve(spoken, &vocab.targets, &windows, threshold) {
        println!("  como alvo    {} ({:.2})", t.class, t.score);
    }

    // Final verdict through the real classifier.
    match crate::commands::classify(spoken, vocab, threshold) {
        Some(cmd) => println!("  => {cmd:?}"),
        None => println!("  => (nada)"),
    }
    println!();
}

pub fn run() {
    let config = Config::load();
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);
    let lang = std::env::args().nth(2).unwrap_or_else(|| "pt".to_string());
    let Some(vocab) = config.vocab(&lang) else {
        eprintln!("idioma {lang:?} sem seção de comandos (transcrição pura)");
        return;
    };
    println!(
        "probe [{lang}] — threshold {} confirm_below {} — digite frases, Ctrl+D sai\n",
        config.threshold(),
        config.confirm_below()
    );
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let spoken = line.trim();
        if spoken.is_empty() {
            continue;
        }
        probe_one(spoken, vocab, &config, &runner);
    }
}
