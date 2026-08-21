//! Undoing, from outside, the consequences of asking for the wrong surface.
//!
//! `EframeHost` asks Wayland for an `xdg_toplevel` — the surface type that
//! means "I am an application window" — so the compositor tiles it, focuses
//! it, and lists it in `hyprctl clients`. None of that is wanted for a HUD,
//! and everything in this file exists to reverse it after the fact.
//!
//! It lives beside the host that needs it rather than in `wm/`, because it is
//! not a window-manager feature: it is one host's compensation. The layer-shell
//! host of M6.1 will have no counterpart, and that absence is the point.

use crate::process::CommandRunner;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Keep the overlay floating and pinned on Hyprland.
///
/// Started by `EframeHost`, because it is that host's `xdg_toplevel` that
/// gets tiled and focused in the first place — a layer surface needs none of
/// this (M6.1).
///
/// A window rule does the same thing at map time and does it better, so this
/// is the fallback for anyone without one. It used to claim rules "do not
/// reliably match because app_id arrives after surface creation"; measured on
/// 0.54.3 that is false — `with_app_id` is set on the ViewportBuilder and the
/// window reports `initialClass`, so rules match.
///
/// Everything here is **idempotent and verified**, which the first version
/// was not: `togglefloating` and `pin` are toggles, so running them on a
/// window that was already floating tiled it instead — exactly the failure
/// this function exists to prevent. State is read from `hyprctl clients -j`
/// and only the missing half is applied, then re-checked.
pub fn try_hyprland_float(
    runner: Arc<dyn CommandRunner>,
    want_monitor: String,
    geom: &crate::ui::host::Geometry,
) {
    let (win_w, win_h, margin) = (
        geom.width as f64,
        geom.height as f64,
        geom.bottom_margin as f64,
    );
    std::thread::spawn(move || {
        if runner.output("hyprctl", &["version"]).is_err() {
            return;
        }

        let monitors = hyprctl_monitors(&runner);
        let target = pick_monitor(&monitors, &want_monitor).cloned();

        // No pre-sleep, and a tight poll: every millisecond here is one the
        // overlay spends where the compositor put it instead of where it was
        // asked to go, which is the flicker at startup.
        for attempt in 0..150 {
            if let Some(state) = overlay_state(&runner) {
                // Re-read only when a toggle was dispatched. Confirming that
                // nothing happened cost 120 ms on every start whose window
                // rules had already done the job.
                let settled = if apply_float_and_pin(&runner, &state) {
                    std::thread::sleep(Duration::from_millis(120));
                    overlay_state(&runner).is_some_and(|a| a.floating && a.pinned)
                } else {
                    state.floating && state.pinned
                };
                if settled {
                    if let Some(ref mon) = target {
                        position_overlay(&runner, mon, win_w, win_h, margin);
                    }
                    info!(attempt, "overlay floating and pinned");
                    return;
                }
                debug!(attempt, "state did not stick, retrying");
            } else if attempt == 0 {
                info!("detected Hyprland, waiting for the overlay window...");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        warn!(
            "could not make the overlay float — add a window rule. \
             Hyprland 0.45+: windowrule = float, class:oc-voice"
        );
    });
}

struct OverlayState {
    address: String,
    floating: bool,
    pinned: bool,
}

/// Our window, found by exact class match on parsed JSON. Substring search
/// over the raw output would also match any window whose *title* mentions
/// oc-voice — an editor with the repo open, for instance.
fn overlay_state(runner: &Arc<dyn CommandRunner>) -> Option<OverlayState> {
    let output = runner.output("hyprctl", &["clients", "-j"]).ok()?;
    let clients: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    clients.as_array()?.iter().find_map(|c| {
        if c.get("class")?.as_str()? != "oc-voice" {
            return None;
        }
        Some(OverlayState {
            address: c.get("address")?.as_str()?.to_string(),
            floating: c.get("floating").and_then(|v| v.as_bool()).unwrap_or(false),
            pinned: c.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    })
}

/// Apply only what is missing, and say whether anything was. `setfloating` is
/// idempotent; `pin` is a toggle, so it is issued only when not already pinned.
fn apply_float_and_pin(runner: &Arc<dyn CommandRunner>, state: &OverlayState) -> bool {
    let target = format!("address:{}", state.address);
    let mut changed = false;
    if !state.floating {
        let _ = runner.output("hyprctl", &["dispatch", "setfloating", &target]);
        changed = true;
    }
    if !state.pinned {
        // Pinning requires the window to be floating already.
        std::thread::sleep(Duration::from_millis(60));
        let _ = runner.output("hyprctl", &["dispatch", "pin", &target]);
        changed = true;
    }
    changed
}

/// One monitor as Hyprland reports it. `x`/`y` are the monitor's origin in
/// the **global layout**, already in logical pixels; `width`/`height` are the
/// mode's physical pixels, which is why they get divided by `scale`.
#[derive(Debug, Clone, PartialEq)]
struct Monitor {
    name: String,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    scale: f64,
    focused: bool,
}

impl Monitor {
    fn logical_size(&self) -> (f64, f64) {
        (
            self.width as f64 / self.scale,
            self.height as f64 / self.scale,
        )
    }
}

/// Bottom-centre of the chosen monitor, in **global** layout coordinates.
///
/// The previous version computed monitor-local coordinates and handed them to
/// `movewindowpixel exact`, which is global. On a single-monitor setup those
/// coincide; on this three-monitor layout it put the overlay on a different
/// screen than the one it measured.
fn position_overlay(runner: &Arc<dyn CommandRunner>, mon: &Monitor, w: f64, h: f64, margin: f64) {
    let (logical_w, logical_h) = mon.logical_size();
    let x = mon.x + ((logical_w - w) / 2.0).max(0.0) as i64;
    let y = mon.y + (logical_h - h - margin).max(0.0) as i64;
    // Both calls name the window explicitly: `resizeactive` acted on whatever
    // had focus, which after a moment is usually not the overlay.
    let size = format!("exact {} {},class:^(oc-voice)$", w as i64, h as i64);
    let _ = runner.output("hyprctl", &["dispatch", "resizewindowpixel", &size]);
    let pos = format!("exact {x} {y},class:^(oc-voice)$");
    let _ = runner.output("hyprctl", &["dispatch", "movewindowpixel", &pos]);
    info!(monitor = %mon.name, x, y, scale = mon.scale, "overlay positioned");
}

fn hyprctl_monitors(runner: &Arc<dyn CommandRunner>) -> Vec<Monitor> {
    let Ok(output) = runner.output("hyprctl", &["monitors", "-j"]) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    json.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(Monitor {
                        name: m.get("name")?.as_str()?.to_string(),
                        x: m.get("x")?.as_i64()?,
                        y: m.get("y")?.as_i64()?,
                        width: m.get("width")?.as_i64()?,
                        height: m.get("height")?.as_i64()?,
                        scale: m.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0),
                        focused: m.get("focused").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve `middle` / `left` / `right` / `focused` / a monitor name against
/// the live layout, ordered left to right by their global `x`.
///
/// `hyprctl monitors` returns connector order, not spatial order — taking the
/// first entry gave the laptop panel regardless of where it sat on the desk.
fn pick_monitor<'a>(monitors: &'a [Monitor], want: &str) -> Option<&'a Monitor> {
    if monitors.is_empty() {
        return None;
    }
    let mut ordered: Vec<&Monitor> = monitors.iter().collect();
    ordered.sort_by_key(|m| m.x);

    match want.trim().to_lowercase().as_str() {
        "left" => ordered.first().copied(),
        "right" => ordered.last().copied(),
        // With an even count there is no true middle; the right-of-centre one
        // is chosen so a two-monitor desk gets the external screen, not the
        // laptop panel that usually sits on the left.
        "middle" | "center" | "centre" | "meio" | "centro" => {
            ordered.get(ordered.len() / 2).copied()
        }
        "focused" | "active" => monitors
            .iter()
            .find(|m| m.focused)
            .or_else(|| ordered.get(ordered.len() / 2).copied()),
        name => monitors
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .or_else(|| {
                warn!(
                    monitor = name,
                    "unknown monitor in [overlay]; using the middle one"
                );
                ordered.get(ordered.len() / 2).copied()
            }),
    }
}

#[cfg(test)]
#[path = "host_toplevel_tests.rs"]
mod tests;
