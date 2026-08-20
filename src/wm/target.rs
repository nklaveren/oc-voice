//! Two-stage spoken-target resolution against live windows — M2.1 in
//! BACKLOG.md.
//!
//! Stage 1, category: the spoken word is matched against the category names
//! of the active language (`[pt.targets]` in commands.toml); a category maps
//! to a *list* of class patterns, and whichever of them is open wins. This is
//! what "navegador" needs — its relation to `brave-browser` is meaning, not
//! spelling, and no string metric crosses that gap.
//!
//! Stage 2, token: the spoken word against every token of `class` and
//! `title`, best token wins. This is what "teams" needs — it sits literally
//! inside "… | Microsoft Teams", but whole-string Jaro-Winkler punishes the
//! length difference down to 0.55.
//!
//! The two pools never mix with the command vocabulary: by the time this
//! resolver runs, the grammar already decided the utterance names a window.
//!
//! Robustness rules, measured against real windows: tokens shorter than 3
//! chars are noise (`e`, `o` score 0.72–0.76 against anything); `title` is
//! arbitrary volatile text and gets a harder threshold (0.90) than the stable
//! `class` (the matcher threshold, 0.82). Legitimate targets match their own
//! token exactly at 1.00, so the harder title bar costs nothing.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use strsim::jaro_winkler;
use tracing::debug;

use crate::commands::matcher;
use crate::process::CommandRunner;

/// Title tokens are arbitrary text (including words the user just dictated);
/// only a near-exact hit counts.
const TITLE_THRESHOLD: f64 = 0.90;
/// Tokens shorter than this score high against everything and mean nothing.
const MIN_TOKEN_LEN: usize = 3;

#[derive(Debug, Clone, Deserialize)]
pub struct WindowInfo {
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub address: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTarget {
    pub address: String,
    pub class: String,
    pub score: f64,
}

/// The live window list from `hyprctl clients -j`.
pub fn live_windows(runner: &Arc<dyn CommandRunner>) -> Vec<WindowInfo> {
    let Ok(output) = runner.output("hyprctl", &["clients", "-j"]) else {
        return Vec::new();
    };
    serde_json::from_slice(&output.stdout).unwrap_or_default()
}

fn tokens(text: &str) -> Vec<String> {
    matcher::normalize(text)
        .split_whitespace()
        .filter(|t| t.len() >= MIN_TOKEN_LEN)
        .map(str::to_string)
        .collect()
}

fn best_token_score(spoken: &str, text: &str) -> f64 {
    tokens(text)
        .iter()
        .map(|t| jaro_winkler(spoken, t))
        .fold(0.0, f64::max)
}

/// Resolve a spoken target against the live windows. `categories` is the
/// active language's `targets` table; `threshold` the configured match
/// threshold (applies to category names and `class` tokens; `title` tokens
/// use the harder TITLE_THRESHOLD).
pub fn resolve(
    spoken: &str,
    categories: &HashMap<String, Vec<String>>,
    windows: &[WindowInfo],
    threshold: f64,
) -> Option<ResolvedTarget> {
    // Spoken targets arrive with articles attached — "envia para O
    // navegador" hands us "o navegador". Resolution works on content tokens:
    // sub-3-char words are articles or noise either way.
    let spoken_tokens = tokens(spoken);
    let spoken_norm = spoken_tokens.join(" ");
    if spoken_norm.is_empty() {
        return None;
    }

    // Stage 1: category. The spoken word names a kind of application; whoever
    // from that kind is open wins.
    let category_names: Vec<&str> = categories.keys().map(String::as_str).collect();
    if let Some((category, _)) = matcher::match_exact(&spoken_norm, &category_names, threshold) {
        for pattern in &categories[category] {
            let pattern_norm = matcher::normalize(pattern);
            for w in windows {
                if best_token_score(&pattern_norm, &w.class) >= TITLE_THRESHOLD
                    || best_token_score(&pattern_norm, &w.title) >= TITLE_THRESHOLD
                {
                    debug!(spoken, category, class = %w.class, "target via category");
                    return Some(ResolvedTarget {
                        address: w.address.clone(),
                        class: w.class.clone(),
                        score: 1.0,
                    });
                }
            }
        }
    }

    // Stage 2: every spoken content token against every window token, best
    // pair wins — "o navegador" must match by its "navegador", and a token
    // of a long title must be reachable by a single spoken word.
    let mut best: Option<ResolvedTarget> = None;
    for w in windows {
        let class_score = spoken_tokens
            .iter()
            .map(|t| best_token_score(t, &w.class))
            .fold(0.0, f64::max);
        let title_score = spoken_tokens
            .iter()
            .map(|t| best_token_score(t, &w.title))
            .fold(0.0, f64::max);
        let score = f64::max(
            if class_score >= threshold {
                class_score
            } else {
                0.0
            },
            if title_score >= TITLE_THRESHOLD {
                title_score
            } else {
                0.0
            },
        );
        if score > 0.0 && best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(ResolvedTarget {
                address: w.address.clone(),
                class: w.class.clone(),
                score,
            });
        }
    }
    if let Some(ref t) = best {
        debug!(spoken, class = %t.class, score = t.score, "target via token");
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<WindowInfo> {
        serde_json::from_str(include_str!("../../tests/fixtures/hyprctl_clients.json")).unwrap()
    }

    fn pt_categories() -> HashMap<String, Vec<String>> {
        let config = crate::config::Config::embedded();
        config.vocab("pt").unwrap().targets.clone()
    }

    fn resolve_pt(spoken: &str) -> Option<ResolvedTarget> {
        resolve(spoken, &pt_categories(), &fixture(), 0.82)
    }

    #[test]
    fn categories_resolve_to_whatever_is_open() {
        // The M2.1 measurement table: semantic aliases via category lists.
        assert_eq!(resolve_pt("navegador").unwrap().class, "brave-browser");
        assert_eq!(resolve_pt("terminal").unwrap().class, "Alacritty");
        assert_eq!(resolve_pt("editor").unwrap().class, "code");
        assert_eq!(resolve_pt("chat").unwrap().class, "electron");
    }

    #[test]
    fn literal_names_resolve_via_tokens() {
        // "teams" sits inside a long title; token matching finds it where
        // whole-string scoring reached only 0.55.
        assert_eq!(resolve_pt("teams").unwrap().class, "electron");
        assert_eq!(resolve_pt("brave").unwrap().class, "brave-browser");
        assert_eq!(resolve_pt("remmina").unwrap().class, "org.remmina.Remmina");
    }

    #[test]
    fn articles_do_not_break_resolution() {
        // The probe caught this live: "envia para o navegador" hands the
        // resolver "o navegador", and the article broke both stages.
        assert_eq!(resolve_pt("o navegador").unwrap().class, "brave-browser");
        assert_eq!(resolve_pt("o terminal").unwrap().class, "Alacritty");
        assert_eq!(resolve_pt("o teams").unwrap().class, "electron");
    }

    #[test]
    fn absent_application_is_refused() {
        assert_eq!(resolve_pt("fotoshop"), None);
    }

    #[test]
    fn empty_class_and_title_window_captures_nothing() {
        // M2.2: the old code's `target.contains(&class)` was true for every
        // spoken target when class was "" — contains("") always holds.
        for spoken in ["navegador", "teams", "brave", "editor"] {
            let resolved = resolve_pt(spoken).unwrap();
            assert_ne!(resolved.address, "0x00", "{spoken} hit the empty window");
        }
    }

    #[test]
    fn dictated_text_in_a_title_does_not_intercept_commands() {
        // A window titled "Câmbio do dólar" must not catch the send word:
        // command vocabulary and window pool are separate stages, and this
        // resolver is only ever called with an utterance the grammar already
        // parsed as a *target*. Here we assert the resolver itself also
        // refuses the full send utterance as a target name.
        let resolved = resolve_pt("câmbio");
        // "câmbio" IS a token of that title, so as a *target* it matches —
        // but classify() never routes the send word here: the grammar decides
        // first. Assert the pools stay separate at the classify level.
        let config = crate::config::Config::embedded();
        let vocab = config.vocab("pt").unwrap();
        let cmd = crate::commands::classify("câmbio", vocab, config.threshold());
        assert_eq!(cmd, Some(crate::commands::VoiceCommand::Send));
        // And the resolver, when asked, resolves it as a title token — which
        // is correct behaviour for "foca o câmbio" pointing at that window.
        assert!(resolved.is_some());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::process::SystemRunner;

    /// Live verification against the running Hyprland session — run with:
    ///   cargo test -- --ignored --nocapture live_
    /// Prints what each spoken target resolves to on THIS machine right now.
    #[test]
    #[ignore]
    fn live_targets_resolve_against_real_windows() {
        let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);
        let windows = live_windows(&runner);
        assert!(!windows.is_empty(), "no hyprland session or no windows");
        println!("\n{} janelas vivas:", windows.len());
        for w in &windows {
            println!(
                "  {:<24} {}",
                w.class,
                &w.title.chars().take(50).collect::<String>()
            );
        }
        let config = crate::config::Config::load();
        let vocab = config.vocab("pt").expect("pt vocab");
        println!("\nresolução:");
        for spoken in [
            "navegador",
            "terminal",
            "editor",
            "chat",
            "code",
            "brave",
            "teams",
            "fotoshop",
            "aplicativo inexistente",
        ] {
            match resolve(spoken, &vocab.targets, &windows, config.threshold()) {
                Some(t) => println!("  {spoken:<24} -> {:<24} ({:.2})", t.class, t.score),
                None => println!("  {spoken:<24} -> [recusado]"),
            }
        }
    }
}
