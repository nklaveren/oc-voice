//! What a window manager has to be able to do — M8.4.
//!
//! Everything above this file decides *what* should happen: the grammar
//! resolves "monitor da direita" to a monitor and "fecha o teams" to a window.
//! Everything below decides *how* to say it. Until now the two were the same
//! thing — the grammar produced `hyprctl` argument vectors — so porting meant
//! editing the grammar.
//!
//! `WmAction` is the seam. It is a closed set, because the spoken vocabulary
//! is closed: twelve verbs, each already reachable by voice. A second backend
//! is a new `impl`, not an edit to anything that resolves speech.
//!
//! Reads are part of the contract too, and for the same reason. The resolver
//! in [`super::target`] scores against `class` and `title`; whether those came
//! from `hyprctl clients -j` or from another CLI's JSON is not its concern.

use std::sync::Arc;

use tracing::{debug, info};

use crate::process::CommandRunner;

/// One thing to do to the windows on screen.
///
/// Addresses are opaque strings from whichever backend produced them, so a
/// backend never has to parse another's identifiers.
#[derive(Debug, Clone, PartialEq)]
pub enum WmAction {
    Fullscreen,
    ToggleFloating,
    /// Close whatever has focus. Destructive, and gated by the confirmation
    /// policy before it ever reaches a backend.
    KillActive,
    /// Close the window that was named, rather than the focused one.
    CloseWindow {
        address: String,
    },
    /// Move through the windows of the current workspace. Wraps.
    CycleWindow {
        previous: bool,
    },
    /// `l` / `r` / `u` / `d`; validated before it gets here.
    MoveFocus {
        direction: String,
    },
    Workspace {
        number: u32,
    },
    MoveToWorkspace {
        number: u32,
    },
    FocusMonitor {
        name: String,
    },
    FocusWindow {
        address: String,
    },
    /// Start something that is not running. The command comes from the
    /// desktop's own `.desktop` files, never from this repo — see `launch.rs`.
    Launch {
        command: String,
    },
}

/// One monitor, in the layout's own logical coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: String,
    pub description: String,
    pub x: i64,
}

pub trait WmBackend: Send + Sync {
    /// Every window that exists right now.
    fn windows(&self) -> Vec<super::target::WindowInfo>;

    /// Every monitor, unordered — callers sort by `x`, because connector
    /// order is not spatial order and that mistake put the overlay on the
    /// laptop panel once already.
    fn monitors(&self) -> Vec<MonitorInfo>;

    /// The title of whatever has focus, for bridging from "the window I am
    /// in" to "the page I am on".
    fn focused_title(&self) -> Option<String>;

    /// Carry out an action. Returns whether it was accepted.
    fn dispatch(&self, action: &WmAction) -> bool;
}

/// Hyprland, over its `hyprctl` CLI.
pub struct Hyprctl {
    runner: Arc<dyn CommandRunner>,
}

impl Hyprctl {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Hyprctl { runner }
    }

    /// The CLI form of an action. Kept separate from `dispatch` so it can be
    /// asserted directly: the encoding is the part a port would get wrong.
    pub fn args(action: &WmAction) -> Vec<String> {
        let s = |v: &str| v.to_string();
        match action {
            WmAction::Fullscreen => vec![s("dispatch"), s("fullscreen")],
            WmAction::ToggleFloating => vec![s("dispatch"), s("togglefloating")],
            WmAction::KillActive => vec![s("dispatch"), s("killactive")],
            WmAction::CloseWindow { address } => {
                vec![
                    s("dispatch"),
                    s("closewindow"),
                    format!("address:{address}"),
                ]
            }
            // `cyclenext` wraps, so these are two directions of one motion
            // rather than two behaviours.
            WmAction::CycleWindow { previous: false } => vec![s("dispatch"), s("cyclenext")],
            WmAction::CycleWindow { previous: true } => {
                vec![s("dispatch"), s("cyclenext"), s("prev")]
            }
            WmAction::MoveFocus { direction } => {
                vec![s("dispatch"), s("movefocus"), direction.clone()]
            }
            WmAction::Workspace { number } => {
                vec![s("dispatch"), s("workspace"), number.to_string()]
            }
            WmAction::MoveToWorkspace { number } => {
                vec![s("dispatch"), s("movetoworkspace"), number.to_string()]
            }
            WmAction::FocusMonitor { name } => {
                vec![s("dispatch"), s("focusmonitor"), name.clone()]
            }
            // The window manager starts it, not this process: a child of the
            // voice daemon dies with the daemon, and inherits its stdio and
            // its environment. `exec` hands it to the session instead.
            WmAction::Launch { command } => {
                vec![s("dispatch"), s("exec"), command.clone()]
            }
            WmAction::FocusWindow { address } => {
                vec![
                    s("dispatch"),
                    s("focuswindow"),
                    format!("address:{address}"),
                ]
            }
        }
    }

    fn query<T: serde::de::DeserializeOwned>(&self, what: &str) -> Vec<T> {
        let Ok(out) = self.runner.output("hyprctl", &[what, "-j"]) else {
            return Vec::new();
        };
        serde_json::from_slice(&out.stdout).unwrap_or_default()
    }
}

impl WmBackend for Hyprctl {
    fn windows(&self) -> Vec<super::target::WindowInfo> {
        self.query("clients")
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        #[derive(serde::Deserialize)]
        struct Raw {
            name: String,
            #[serde(default)]
            description: String,
            #[serde(default)]
            x: i64,
        }
        self.query::<Raw>("monitors")
            .into_iter()
            .map(|m| MonitorInfo {
                name: m.name,
                description: m.description,
                x: m.x,
            })
            .collect()
    }

    fn focused_title(&self) -> Option<String> {
        let out = self
            .runner
            .output("hyprctl", &["activewindow", "-j"])
            .ok()?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        Some(v.get("title")?.as_str()?.to_string())
    }

    fn dispatch(&self, action: &WmAction) -> bool {
        let args = Self::args(action);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.runner.output("hyprctl", &refs) {
            Ok(_) => {
                info!(?action, "dispatched");
                true
            }
            Err(e) => {
                debug!(?action, error = %e, "the window manager refused");
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
