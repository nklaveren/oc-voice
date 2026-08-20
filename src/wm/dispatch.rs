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
use crate::wm::tabs;
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
    emit(tx, TranscriptEvent::notice(format!("[{}]", args.join(" "))));
}

/// Interpret one utterance as a WM command. Unrecognized speech is dropped —
/// in Command mode nothing is ever typed.
/// Returns whether the utterance was recognised as a window-manager command.
///
/// The answer matters now that Enter mode consults this grammar before
/// buffering: an utterance it does not recognise has to fall through to
/// dictation rather than vanish.
pub fn dispatch_spoken(
    vocab: &LangVocab,
    config: &Config,
    spoken: &str,
    runner: &Arc<dyn CommandRunner>,
    tx: &Sender<TranscriptEvent>,
    pending: &mut Option<PendingAction>,
) -> bool {
    let threshold = config.threshold();

    // Whole-utterance commands first.
    let words: Vec<&str> = vocab.wm_commands.keys().map(String::as_str).collect();
    if let Some((word, _)) = matcher::match_exact(spoken, &words, threshold) {
        return execute_action(
            &vocab.wm_commands[word].clone(),
            None,
            vocab,
            config,
            runner,
            tx,
            pending,
        );
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
        return execute_action(
            &def.action.clone(),
            slot_value.as_deref(),
            vocab,
            config,
            runner,
            tx,
            pending,
        );
    }

    debug!(spoken, "no WM command recognized");
    false
}

#[allow(clippy::too_many_arguments)]
/// Returns whether anything actually happened.
///
/// It used to return nothing, and `dispatch_spoken` reported success on any
/// pattern match — even when the slot resolved to no window at all. Harmless
/// while Command mode dropped unrecognised speech; not harmless now that
/// Enter mode falls through to dictation, where a swallowed "true" means the
/// sentence is silently discarded instead of typed.
fn execute_action(
    action: &str,
    slot: Option<&str>,
    vocab: &LangVocab,
    config: &Config,
    runner: &Arc<dyn CommandRunner>,
    tx: &Sender<TranscriptEvent>,
    pending: &mut Option<PendingAction>,
) -> bool {
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
        return true;
    }
    if let Some(args) = dispatch_args(action, slot, vocab, config, runner) {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_dispatch(runner, tx, &refs);
        true
    } else {
        debug!(action, ?slot, "slot did not resolve; nothing dispatched");
        false
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
        // Cycle within the current workspace. `cyclenext` wraps, so these are
        // the two directions of one motion rather than two behaviours.
        "next_window" => vec!["dispatch".into(), "cyclenext".into()],
        "previous_window" => vec!["dispatch".into(), "cyclenext".into(), "prev".into()],
        "move_focus" => {
            let dir = resolve_direction(slot?, vocab, threshold)?;
            // hyprctl movefocus only takes l/r/u/d — "janela do centro" is
            // not a thing and must not dispatch an invalid direction.
            if !["l", "r", "u", "d"].contains(&dir.as_str()) {
                return None;
            }
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
            let spoken = slot?;
            let windows = target::live_windows(runner);
            if let Some(resolved) = target::resolve(spoken, &vocab.targets, &windows, threshold) {
                return Some(vec![
                    "dispatch".into(),
                    "focuswindow".into(),
                    format!("address:{}", resolved.address),
                ]);
            }
            // No window answers to it. Before giving up, ask the browser: a
            // whole browser is one window to the compositor, so anything kept
            // in a tab is invisible from here — which is how most people keep
            // most things. Windows first, because a real window is a stronger
            // answer than a page inside one.
            focus_browser_tab(spoken, config, runner)?
        }
        _ => return None,
    };
    Some(args)
}

/// A tab title is arbitrary text that changes with whatever page is loaded,
/// so it earns the same hard bar the window resolver gives window titles.
const TAB_THRESHOLD: f64 = 0.90;

/// Raise a browser tab, then hand back the dispatch that raises its window.
///
/// Two halves that compose: the browser brings the tab to the front of
/// itself, the compositor brings the browser to the front of the screen.
/// Neither pretends to do the other's job.
fn focus_browser_tab(
    spoken: &str,
    config: &Config,
    runner: &Arc<dyn CommandRunner>,
) -> Option<Vec<String>> {
    let port = config.browser_port()?;
    let tabs = tabs::live_tabs(runner, port);
    let tab = tabs::resolve(spoken, &tabs, TAB_THRESHOLD)?;
    if !tabs::activate(runner, port, tab) {
        return None;
    }
    // The window's title becomes the tab's once it is frontmost, so the
    // window is found by what we just asked the browser to show. Re-reading
    // rather than remembering: with two browser windows open, the one holding
    // this tab is not knowable before the activation happened.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let windows = target::live_windows(runner);
    let owner = windows.iter().find(|w| {
        let title = matcher::normalize(&w.title);
        let wanted = matcher::normalize(&tab.title);
        !wanted.is_empty() && title.contains(&wanted)
    })?;
    info!(tab = %tab.title, class = %owner.class, "raised a page inside a window");
    Some(vec![
        "dispatch".into(),
        "focuswindow".into(),
        format!("address:{}", owner.address),
    ])
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
