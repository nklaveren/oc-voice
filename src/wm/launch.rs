//! Starting something that is not running yet.
//!
//! `abre o {alvo}` and `go to {alvo}` both resolved to `focus_window`, which
//! can only reach a window that already exists. Saying "abre o Outlook" with
//! nothing open did exactly nothing, and correctly so — there was nothing to
//! focus. This is the missing third step.
//!
//! The list of what can be opened is **not** written here. Every desktop
//! already keeps one, in `.desktop` files under the XDG data dirs, and it is
//! the only list that knows about the browser apps a person installed: on this
//! machine those files are where "YouTube Music" and "WhatsApp Web" live, as
//! full `brave --app-id=…` command lines. A table in this repo would be a
//! second, worse copy that goes stale — and `just vocab` forbids app names in
//! `src/` for the same reason.
//!
//! Nothing here runs a command. It resolves a name to one, and the dispatcher
//! hands that to the window manager like any other action, so a spoken phrase
//! can only ever start something the desktop already offers from its menu.

use std::collections::HashSet;
use std::path::PathBuf;

use tracing::debug;

use crate::commands::matcher;

/// One thing the desktop knows how to start.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub name: String,
    /// Whatever the platform's own launcher needs to start it: the `Exec`
    /// line with its field codes removed on an XDG desktop, the bundle path
    /// on macOS. Opaque between here and the backend that runs it, the same
    /// way a window address is.
    pub command: String,
}

/// Field codes a launcher is supposed to substitute. With no file and no URL
/// to pass, they are dropped — left in, `%U` reaches the program as a literal
/// argument, and some take it as a filename.
const FIELD_CODES: [&str; 8] = ["%u", "%U", "%f", "%F", "%i", "%c", "%k", "%d"];

/// Where `.desktop` files live, most specific first: a user's own entry for a
/// name shadows the system's, which is what XDG says and what a person who
/// wrote their own launcher expects.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
    {
        dirs.push(home);
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(
        system
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    );
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Everything the desktop offers, deduplicated by name.
///
/// Which registry that is, is the only part that differs: `.desktop` files on
/// an XDG desktop, `.app` bundles on macOS. Both are the list the machine
/// already keeps — a table in this repo would be a second, worse copy.
pub fn installed() -> Vec<App> {
    if cfg!(target_os = "macos") {
        mac_installed()
    } else {
        xdg_installed()
    }
}

/// Where macOS keeps applications, the user's own first — the same rule XDG
/// states, for the same reason.
fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    // Utilities is listed on its own: macOS nests one level there and nowhere
    // else, and a recursive walk of /System would cost far more than naming it.
    for d in [
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// Bundles, which carry their spoken name in the directory name itself.
fn mac_installed() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in app_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            if seen.insert(name.to_lowercase()) {
                apps.push(App {
                    name: name.to_string(),
                    command: path.to_string_lossy().into_owned(),
                });
            }
        }
    }
    debug!(count = apps.len(), "installed applications");
    apps
}

fn xdg_installed() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in data_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(app) = parse(&text) else { continue };
            if seen.insert(app.name.to_lowercase()) {
                apps.push(app);
            }
        }
    }
    debug!(count = apps.len(), "installed applications");
    apps
}

/// One `.desktop` file into an entry, or nothing if it is not a launchable
/// application.
///
/// Only the first group is read. A `.desktop` file may carry extra actions in
/// `[Desktop Action …]` groups, each with its own `Name` and `Exec`, and
/// reading straight through would pick up "Open a New Window" as though it
/// were an application in its own right.
pub fn parse(text: &str) -> Option<App> {
    let mut name = None;
    let mut exec = None;
    let mut kind = None;
    let mut hidden = false;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_entry {
                break;
            }
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            // Localised keys arrive as `Name[pt_BR]`; the plain one is what
            // the rest of the desktop shows, so it is what gets spoken.
            "Name" => name = Some(value.trim().to_string()),
            "Exec" => exec = Some(value.trim().to_string()),
            "Type" => kind = Some(value.trim().to_string()),
            "NoDisplay" | "Hidden" => hidden |= value.trim() == "true",
            _ => {}
        }
    }
    if hidden || kind.as_deref() != Some("Application") {
        return None;
    }
    let command = strip_field_codes(&exec?);
    let name = name?;
    (!name.is_empty() && !command.is_empty()).then_some(App { name, command })
}

fn strip_field_codes(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|w| !FIELD_CODES.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The application a spoken name asks for.
///
/// Whole-name first, then per-token: "youtube" has to reach "YouTube Music",
/// and whole-string Jaro-Winkler punishes that length difference the same way
/// it punished "teams" against a window title. Tokens shorter than three
/// characters are dropped for the reason `target.rs` measured — they score
/// 0.72 and up against anything.
pub fn resolve<'a>(spoken: &str, apps: &'a [App], threshold: f64) -> Option<&'a App> {
    let spoken_norm = matcher::normalize(spoken);
    let spoken_tokens: Vec<String> = spoken_norm
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .map(str::to_string)
        .collect();
    if spoken_tokens.is_empty() {
        return None;
    }
    let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
    if let Some((name, score)) = matcher::match_exact(&spoken_norm, &names, threshold) {
        debug!(spoken, matched = name, score, "application by name");
        return apps.iter().find(|a| a.name == name);
    }

    let mut best: Option<(&App, f64)> = None;
    for app in apps {
        let norm = matcher::normalize(&app.name);
        for token in norm.split_whitespace() {
            if token.chars().count() < 3 {
                continue;
            }
            let score = spoken_tokens
                .iter()
                .map(|s| strsim::jaro_winkler(s, token))
                .fold(0.0, f64::max);
            if score >= threshold && best.is_none_or(|(_, b)| score > b) {
                best = Some((app, score));
            }
        }
    }
    if let Some((app, score)) = best {
        debug!(spoken, matched = %app.name, score, "application by token");
    }
    best.map(|(a, _)| a)
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
