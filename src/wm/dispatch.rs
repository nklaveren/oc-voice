//! Spoken WM-command dispatch — M3.1/M3.2/M3.3 in BACKLOG.md.
//!
//! Two grammars, both from the language's config section: `wm_commands`
//! (whole utterance → action) and `templates` (one slot, resolved here).
//! Everything positional comes from hyprctl at dispatch time — monitors from
//! `hyprctl monitors -j` sorted by x, windows from the M2.1 resolver. No
//! connector name, brand, or application name exists in this source.
//!
//! By design nothing in this module can ever type text; Command mode routes
//! here precisely because of that guarantee (M4.2).

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info};

use crate::commands::matcher::{self, Template};
use crate::commands::PendingAction;
use crate::config::{Config, LangVocab};
use crate::process::CommandRunner;
use crate::ui::stdout::emit;
use crate::wm::target;
use crate::TranscriptEvent;
use crossbeam_channel::Sender;

#[derive(Debug, Clone, Deserialize)]
struct MonitorInfo {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    x: i64,
}

fn live_monitors(runner: &Arc<dyn CommandRunner>) -> Vec<MonitorInfo> {
    let Ok(output) = runner.output("hyprctl", &["monitors", "-j"]) else {
        return Vec::new();
    };
    serde_json::from_slice(&output.stdout).unwrap_or_default()
}

/// Position words resolve against the actual x layout, not against hyprctl's
/// relative directions: "o da direita" is the rightmost monitor no matter
/// which one has focus, and it follows the cables when the desk changes.
fn monitor_by_position(monitors: &[MonitorInfo], direction: &str) -> Option<String> {
    let mut sorted: Vec<&MonitorInfo> = monitors.iter().collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by_key(|m| m.x);
    match direction {
        "l" => Some(sorted[0].name.clone()),
        "r" => Some(sorted[sorted.len() - 1].name.clone()),
        "m" => Some(sorted[sorted.len() / 2].name.clone()),
        _ => None,
    }
}

/// Name a monitor by user alias first, then by brand/model tokens from its
/// EDID description.
fn monitor_by_name(
    spoken: &str,
    vocab: &LangVocab,
    monitors: &[MonitorInfo],
    threshold: f64,
) -> Option<String> {
    let aliases: Vec<&str> = vocab.monitors.keys().map(String::as_str).collect();
    if let Some((alias, _)) = matcher::match_exact(spoken, &aliases, threshold) {
        return Some(vocab.monitors[alias].clone());
    }
    let spoken_norm = matcher::normalize(spoken);
    monitors
        .iter()
        .find(|m| {
            matcher::normalize(&m.description)
                .split_whitespace()
                // Short tokens are noise for fuzzy scoring but legitimate as
                // brands (LG, HP): they match only exactly.
                .any(|t| {
                    if t.len() < 3 {
                        t == spoken_norm
                    } else {
                        strsim::jaro_winkler(&spoken_norm, t) >= 0.9
                    }
                })
        })
        .map(|m| m.name.clone())
}

/// Fuzzy-resolve a spoken number: the language's numbers table, or a literal
/// digit string (M3.2 — "área de trabalho quatro" and "área de trabalho 4"
/// dispatch identically).
fn resolve_number(spoken: &str, vocab: &LangVocab, threshold: f64) -> Option<u32> {
    if let Ok(n) = spoken.parse::<u32>() {
        return Some(n);
    }
    let words: Vec<&str> = vocab.numbers.keys().map(String::as_str).collect();
    matcher::match_exact(spoken, &words, threshold).map(|(word, _)| vocab.numbers[word])
}

fn resolve_direction(spoken: &str, vocab: &LangVocab, threshold: f64) -> Option<String> {
    let words: Vec<&str> = vocab.directions.keys().map(String::as_str).collect();
    matcher::match_exact(spoken, &words, threshold).map(|(word, _)| vocab.directions[word].clone())
}

fn run_dispatch(runner: &Arc<dyn CommandRunner>, tx: &Sender<TranscriptEvent>, args: &[&str]) {
    let _ = runner.output("hyprctl", args);
    info!(?args, "dispatched");
    emit(tx, TranscriptEvent::Final(format!("[{}]", args.join(" "))));
}

/// Interpret one utterance as a WM command. Unrecognized speech is dropped —
/// in Command mode nothing is ever typed.
pub fn dispatch_spoken(
    vocab: &LangVocab,
    config: &Config,
    spoken: &str,
    runner: &Arc<dyn CommandRunner>,
    tx: &Sender<TranscriptEvent>,
    pending: &mut Option<PendingAction>,
) {
    let threshold = config.threshold();

    // Whole-utterance commands first.
    let words: Vec<&str> = vocab.wm_commands.keys().map(String::as_str).collect();
    if let Some((word, _)) = matcher::match_exact(spoken, &words, threshold) {
        execute_action(
            &vocab.wm_commands[word].clone(),
            None,
            vocab,
            config,
            runner,
            tx,
            pending,
        );
        return;
    }

    // Slotted templates. Slot options: direcao is closed (validated during
    // matching); numero, alvo and monitor are wildcards resolved below.
    let templates: Vec<Template> = vocab
        .templates
        .iter()
        .map(|t| {
            let directions: Vec<&str> = vocab.directions.keys().map(String::as_str).collect();
            Template::new(
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
        let slot_value = m.slots.values().next().cloned();
        execute_action(
            &def.action.clone(),
            slot_value.as_deref(),
            vocab,
            config,
            runner,
            tx,
            pending,
        );
        return;
    }

    debug!(spoken, "no WM command recognized");
}

#[allow(clippy::too_many_arguments)]
fn execute_action(
    action: &str,
    slot: Option<&str>,
    vocab: &LangVocab,
    config: &Config,
    runner: &Arc<dyn CommandRunner>,
    tx: &Sender<TranscriptEvent>,
    pending: &mut Option<PendingAction>,
) {
    // M3.3/M4.3: destructive actions never fire directly.
    if config.is_destructive(action) {
        emit(
            tx,
            TranscriptEvent::AwaitingConfirmation(format!("{action}?")),
        );
        *pending = Some(PendingAction::Dispatch {
            args: dispatch_args(action, slot, vocab, config, runner)
                .unwrap_or_default()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            label: action.to_string(),
        });
        return;
    }
    if let Some(args) = dispatch_args(action, slot, vocab, config, runner) {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_dispatch(runner, tx, &refs);
    } else {
        debug!(action, ?slot, "slot did not resolve; nothing dispatched");
    }
}

/// Build the hyprctl argument list for an action, resolving the slot. None
/// means the slot failed to resolve and nothing must be dispatched.
fn dispatch_args(
    action: &str,
    slot: Option<&str>,
    vocab: &LangVocab,
    config: &Config,
    runner: &Arc<dyn CommandRunner>,
) -> Option<Vec<String>> {
    let threshold = config.threshold();
    let args: Vec<String> = match action {
        "fullscreen" => vec!["dispatch".into(), "fullscreen".into()],
        "toggle_floating" => vec!["dispatch".into(), "togglefloating".into()],
        "kill_active" => vec!["dispatch".into(), "killactive".into()],
        "move_focus" => {
            let dir = resolve_direction(slot?, vocab, threshold)?;
            vec!["dispatch".into(), "movefocus".into(), dir]
        }
        "workspace" => {
            let n = resolve_number(slot?, vocab, threshold)?;
            vec!["dispatch".into(), "workspace".into(), n.to_string()]
        }
        "move_to_workspace" => {
            let n = resolve_number(slot?, vocab, threshold)?;
            vec!["dispatch".into(), "movetoworkspace".into(), n.to_string()]
        }
        "focus_monitor" => {
            let dir = resolve_direction(slot?, vocab, threshold)?;
            let name = monitor_by_position(&live_monitors(runner), &dir)?;
            vec!["dispatch".into(), "focusmonitor".into(), name]
        }
        "focus_monitor_name" => {
            let name = monitor_by_name(slot?, vocab, &live_monitors(runner), threshold)?;
            vec!["dispatch".into(), "focusmonitor".into(), name]
        }
        "focus_window" => {
            let windows = target::live_windows(runner);
            let resolved = target::resolve(slot?, &vocab.targets, &windows, threshold)?;
            vec![
                "dispatch".into(),
                "focuswindow".into(),
                format!("address:{}", resolved.address),
            ]
        }
        _ => return None,
    };
    Some(args)
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
