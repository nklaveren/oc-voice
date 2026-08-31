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
    /// Move the window that was named to a workspace, rather than whichever
    /// one happens to have focus.
    MoveWindowToWorkspace {
        number: u32,
        address: String,
    },
    /// Start something that is not running. The command comes from the
    /// machine's own registry of applications, never from this repo — see
    /// `launch.rs`. Like an address, it is opaque: the backend that produced
    /// the list is the backend that knows how to run one of its entries.
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

    /// How this backend spells the action, for the overlay's notice.
    ///
    /// Part of the contract because the notice is the only thing that tells a
    /// person what was carried out. Printing another platform's CLI there is
    /// a lie in the one place they can check.
    fn spelling(&self, action: &WmAction) -> String;
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
            // One argument, comma-joined: hyprctl reads `3,address:0x…` as a
            // workspace and a window, and reads `3 address:0x…` as a
            // workspace and a syntax error.
            WmAction::MoveWindowToWorkspace { number, address } => vec![
                s("dispatch"),
                s("movetoworkspace"),
                format!("{number},address:{address}"),
            ],
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

    fn spelling(&self, action: &WmAction) -> String {
        Self::args(action).join(" ")
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

/// macOS, over the tools that ship with it.
///
/// Windows are not reachable from here yet — that needs Aerospace, and
/// writing its JSON parser without a machine to capture a fixture from is
/// exactly the invention this repo refuses (M8.4b). So this backend answers
/// the one verb that never needed a window manager: starting something that
/// is not running. Every other action reports that it was not carried out,
/// which is what the confirmation and fall-through paths above already know
/// how to handle — a command that dispatches nothing becomes dictation.
pub struct MacOs {
    runner: Arc<dyn CommandRunner>,
}

impl MacOs {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        MacOs { runner }
    }

    /// The CLI form of an action, or nothing when this platform cannot say it
    /// yet. Kept separate from `dispatch` for the reason `Hyprctl::args` is:
    /// the encoding is the part a port gets wrong, and this way it can be
    /// asserted from any machine.
    pub fn args(action: &WmAction) -> Option<(&'static str, Vec<String>)> {
        match action {
            // `open` hands the launch to the session, the way `dispatch exec`
            // does on Hyprland: a child of this process would inherit its
            // stdio and die with it.
            WmAction::Launch { command } => Some(("open", vec!["-a".to_string(), command.clone()])),
            _ => None,
        }
    }
}

impl WmBackend for MacOs {
    fn windows(&self) -> Vec<super::target::WindowInfo> {
        Vec::new()
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        Vec::new()
    }

    fn focused_title(&self) -> Option<String> {
        None
    }

    fn spelling(&self, action: &WmAction) -> String {
        match Self::args(action) {
            Some((program, args)) => format!("{program} {}", args.join(" ")),
            None => "(sem suporte nesta plataforma)".to_string(),
        }
    }

    fn dispatch(&self, action: &WmAction) -> bool {
        let Some((program, args)) = Self::args(action) else {
            debug!(?action, "no window manager on this platform yet — M8.4b");
            return false;
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.runner.output(program, &refs) {
            Ok(_) => {
                info!(?action, "dispatched");
                true
            }
            Err(e) => {
                debug!(?action, error = %e, "the platform refused");
                false
            }
        }
    }
}

/// The backend this build talks to.
///
/// **The single place a window manager is chosen**, the way
/// `platform_injector` is the single place a keyboard is. Everything above
/// sees `dyn WmBackend` and no `cfg` reaches the grammar.
pub fn platform_backend(runner: Arc<dyn CommandRunner>) -> Box<dyn WmBackend> {
    // Under test this is always Hyprland. The dispatch tests and the snapshot
    // exist to pin *that* encoding — "the way it always was" — and a suite
    // that answers differently depending on the machine it runs on pins
    // nothing. The platform choice is a production concern; the macOS
    // encoding is asserted directly through `MacOs::args`, the same way
    // `MacInjector` is asserted from a Linux machine.
    if cfg!(target_os = "macos") && !cfg!(test) {
        Box::new(MacOs::new(runner))
    } else {
        Box::new(Hyprctl::new(runner))
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
