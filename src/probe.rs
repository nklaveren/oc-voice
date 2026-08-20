//! Interactive matcher probe — `oc-voice probe`.
//!
//! Reads utterances from stdin (one per line, as if whisper had emitted
//! them) and prints the whole decision chain with scores: normalization,
//! prefix, every vocabulary pool, template matching, slot resolution, and
//! window/monitor resolution against the LIVE session.
//!
//! Runs on a DryRunRunner: hyprctl queries pass through to the real session
//! so resolution is honest, while every dispatch and every keystroke is
//! swallowed and reported instead of executed. Typing "área de trabalho
//! quatro" here shows what would happen; it does not switch your workspace.
//!
//! This is how you test the Jaro-Winkler discovery by hand: type the
//! mishearings ("sambio", "monitor da direta") and watch where they land.

use std::io::BufRead;
use std::sync::Arc;

use crate::commands::matcher;
use crate::config::{Config, LangVocab};
use crate::process::{CommandRunner, DryRunRunner};
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

fn probe_one(
    spoken: &str,
    vocab: &LangVocab,
    config: &Config,
    runner: &Arc<dyn CommandRunner>,
    dry: &Arc<DryRunRunner>,
) {
    let before = dry.blocked().len();
    let threshold = config.threshold();
    println!("normalizado: {:?}", matcher::normalize(spoken));

    pool("send", spoken, &vocab.send, threshold);
    pool("cancel", spoken, &vocab.cancel, threshold);
    pool("newline", spoken, &vocab.newline, threshold);
    pool("confirm", spoken, &vocab.confirm, threshold);
    pool("deny", spoken, &vocab.deny, threshold);
    pool("help", spoken, &vocab.help, threshold);
    // Modes were missing from this listing, and the omission hid a real bug:
    // "monitor direito" matched "modo ditado" and switched mode instead of
    // moving focus. A probe that does not show every class `classify`
    // consults lies by omission — the one it hides is the one you cannot see.
    let modes: Vec<String> = vocab.modes.keys().cloned().collect();
    pool(
        "mode",
        spoken,
        &modes,
        crate::commands::MODE_THRESHOLD.max(threshold),
    );
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

    // Two lines because one utterance means two things: what the classifier
    // calls it, and what the window grammar would do with it. Showing only
    // the first is how "the template matched but it says Dictation" becomes
    // confusing.
    // Enter mode consults the window-manager grammar before buffering, so
    // reporting only `classify` here would say "Dictation" for an utterance
    // that actually navigates. An instrument that does not match the thing it
    // measures is the next bug rather than a way of finding one.
    let verdict = crate::commands::classify(spoken, vocab, threshold);
    let navigates = {
        let (probe_tx, _probe_rx) = crossbeam_channel::unbounded();
        let mut probe_pending = None;
        let quiet = Arc::new(DryRunRunner::new());
        let quiet_runner: Arc<dyn CommandRunner> = quiet;
        crate::wm::dispatch::dispatch_spoken(
            vocab,
            config,
            spoken,
            &quiet_runner,
            &probe_tx,
            &mut probe_pending,
        )
    };
    match verdict {
        Some(crate::commands::VoiceCommand::Dictation) | None if navigates => {
            println!("  => classificação: navega (comando de janela, não vira texto)")
        }
        Some(cmd) => println!("  => classificação: {cmd:?}"),
        None => println!("  => classificação: (nada)"),
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut pending = None;
    crate::wm::dispatch::dispatch_spoken(vocab, config, spoken, runner, &tx, &mut pending);
    let confirmations: Vec<String> = rx
        .try_iter()
        .filter_map(|e| match e {
            crate::TranscriptEvent::AwaitingConfirmation(s) => Some(format!("[confirmaria] {s}")),
            _ => None,
        })
        .collect();
    let would_run: Vec<String> = dry.blocked()[before..]
        .iter()
        .map(|(p, a)| format!("{p} {}", a.join(" ")))
        .collect();
    let mut parts = would_run;
    parts.extend(confirmations);
    if parts.is_empty() {
        println!("  => janelas:       (nada)");
    } else {
        println!("  => janelas:       {}", parts.join(" | "));
    }
    println!();
}

pub fn run() {
    let config = Config::load();
    let dry = Arc::new(DryRunRunner::new());
    let runner: Arc<dyn CommandRunner> = dry.clone();
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
        probe_one(spoken, vocab, &config, &runner, &dry);
    }
}
