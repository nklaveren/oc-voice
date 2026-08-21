//! Browser tabs as targets.
//!
//! Hyprland sees one window for a whole browser. "vai pro youtube" therefore
//! resolves to nothing when YouTube is a tab rather than a window, which is
//! how people actually keep things open — the window list said `brave-browser`
//! and the thing being asked for was three tabs deep inside it.
//!
//! Chromium browsers expose their tabs over the DevTools HTTP endpoint when
//! launched with `--remote-debugging-port`. Two plain GETs are enough: one
//! lists, one activates. No WebSocket, no protocol handshake.
//!
//! Reached through `curl` and the `CommandRunner`, like every other external
//! call here — which keeps it testable against a fake and adds no dependency.
//! **Silence is the correct behaviour when the port is closed**: most people
//! do not run their browser this way, and a target that cannot be resolved
//! must fall through to the window list rather than fail loudly.

use std::sync::Arc;

use serde::Deserialize;
use tracing::debug;

use crate::commands::matcher;
use crate::process::CommandRunner;

/// How long to wait on a browser that may not be listening at all.
const TIMEOUT_SECONDS: &str = "1";

#[derive(Debug, Clone, Deserialize)]
pub struct Tab {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    /// The per-page debugger socket. Listing and raising a tab need only the
    /// two GETs above; asking the page what is on it needs this (`page.rs`).
    #[serde(default, rename = "webSocketDebuggerUrl")]
    pub debugger: String,
}

fn endpoint(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/json/{path}")
}

/// Every open tab, or an empty list when nothing is listening.
pub fn live_tabs(runner: &Arc<dyn CommandRunner>, port: u16) -> Vec<Tab> {
    let url = endpoint(port, "list");
    let Ok(out) = runner.output("curl", &["-s", "--max-time", TIMEOUT_SECONDS, &url]) else {
        return Vec::new();
    };
    let tabs: Vec<Tab> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    tabs.into_iter()
        // `page` excludes iframes, extension pages and service workers, which
        // outnumber real tabs and carry titles nobody would ever say.
        .filter(|t| t.kind == "page" && !t.title.trim().is_empty())
        .collect()
}

/// The tab whose title best matches what was said, if any is close enough.
///
/// Scored the same way window titles are, and against the same bar: a title
/// is arbitrary text that changes with whatever page is loaded, so only a
/// near-exact token match counts. "youtube" hits the token in
/// "… - YouTube" at 1.00; a passing mention in an article headline does not.
pub fn resolve<'a>(spoken: &str, tabs: &'a [Tab], threshold: f64) -> Option<&'a Tab> {
    let spoken_tokens: Vec<String> = matcher::normalize(spoken)
        .split_whitespace()
        .filter(|t| t.len() >= 3)
        .map(str::to_string)
        .collect();
    if spoken_tokens.is_empty() {
        return None;
    }

    let mut best: Option<(&Tab, f64)> = None;
    for tab in tabs {
        let score = spoken_tokens
            .iter()
            .map(|s| best_token_score(s, &tab.title))
            .fold(0.0, f64::max);
        if score >= threshold && best.as_ref().is_none_or(|(_, b)| score > *b) {
            best = Some((tab, score));
        }
    }
    if let Some((tab, score)) = best {
        debug!(spoken, title = %tab.title, score, "tab resolved");
    }
    best.map(|(t, _)| t)
}

fn best_token_score(spoken: &str, title: &str) -> f64 {
    matcher::normalize(title)
        .split_whitespace()
        .filter(|t| t.len() >= 3)
        .map(|t| strsim::jaro_winkler(spoken, t))
        .fold(0.0, f64::max)
}

/// Bring a tab to the front of its browser. Returns whether the browser
/// accepted the request.
///
/// This only reaches the tab. Raising the browser *window* is the window
/// manager's job and stays there — the two halves compose rather than one
/// pretending to do both.
pub fn activate(runner: &Arc<dyn CommandRunner>, port: u16, tab: &Tab) -> bool {
    let url = endpoint(port, &format!("activate/{}", tab.id));
    match runner.output("curl", &["-s", "--max-time", TIMEOUT_SECONDS, &url]) {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

#[cfg(test)]
#[path = "tabs_tests.rs"]
mod tests;
