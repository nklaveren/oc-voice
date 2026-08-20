use crate::input::inject::{type_key, type_text};
use crate::process::CommandRunner;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Focus a window by its hyprctl address, then type the text into it.
pub fn focus_address_and_type(runner: &Arc<dyn CommandRunner>, address: &str, text: &str) {
    if !address.is_empty() {
        let _ = runner.output(
            "hyprctl",
            &["dispatch", "focuswindow", &format!("address:{address}")],
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    type_text(&**runner, text);
    type_key(&**runner, "Return");
    info!(address, "sent text to target window");
}

/// Keep the overlay floating and pinned on Hyprland.
///
/// Hyprland 0.54.x does not reliably match window rules on xdg_toplevel
/// windows whose app_id arrives after surface creation, so this polls for
/// our window and fixes its state directly.
///
/// Everything here is **idempotent and verified**, which the first version
/// was not: `togglefloating` and `pin` are toggles, so running them on a
/// window that was already floating tiled it instead — exactly the failure
/// this function exists to prevent. State is read from `hyprctl clients -j`
/// and only the missing half is applied, then re-checked.
pub fn try_hyprland_float(runner: Arc<dyn CommandRunner>, want_monitor: String) {
    std::thread::spawn(move || {
        if runner.output("hyprctl", &["version"]).is_err() {
            return;
        }

        let monitors = hyprctl_monitors(&runner);
        let target = pick_monitor(&monitors, &want_monitor).cloned();
        std::thread::sleep(Duration::from_millis(300));

        for attempt in 0..25 {
            match overlay_state(&runner) {
                Some(state) => {
                    apply_float_and_pin(&runner, &state);
                    // Re-read: a toggle that raced with Hyprland mapping the
                    // window silently does the opposite of what we want.
                    std::thread::sleep(Duration::from_millis(120));
                    if let Some(after) = overlay_state(&runner) {
                        if after.floating && after.pinned {
                            if let Some(ref mon) = target {
                                position_overlay(&runner, mon);
                            }
                            info!("overlay floating and pinned");
                            return;
                        }
                        debug!(
                            floating = after.floating,
                            pinned = after.pinned,
                            "state did not stick, retrying"
                        );
                    }
                }
                None if attempt == 0 => {
                    info!("detected Hyprland, waiting for the overlay window...");
                }
                None => {}
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        warn!(
            "could not make the overlay float after 5 s — add a window rule: \
             windowrulev2 = float, class:^(oc-voice)$"
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

/// Apply only what is missing. `setfloating` is idempotent; `pin` is a
/// toggle, so it is issued only when the window is not already pinned.
fn apply_float_and_pin(runner: &Arc<dyn CommandRunner>, state: &OverlayState) {
    let target = format!("address:{}", state.address);
    if !state.floating {
        let _ = runner.output("hyprctl", &["dispatch", "setfloating", &target]);
    }
    if !state.pinned {
        // Pinning requires the window to be floating already.
        std::thread::sleep(Duration::from_millis(60));
        let _ = runner.output("hyprctl", &["dispatch", "pin", &target]);
    }
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

/// Gap between the overlay and the bottom edge of its monitor.
const BOTTOM_MARGIN: f64 = 60.0;
const WIN_W: f64 = 900.0;
const WIN_H: f64 = 350.0;

/// Bottom-centre of the chosen monitor, in **global** layout coordinates.
///
/// The previous version computed monitor-local coordinates and handed them to
/// `movewindowpixel exact`, which is global. On a single-monitor setup those
/// coincide; on this three-monitor layout it put the overlay on a different
/// screen than the one it measured.
fn position_overlay(runner: &Arc<dyn CommandRunner>, mon: &Monitor) {
    let (logical_w, logical_h) = mon.logical_size();
    let x = mon.x + ((logical_w - WIN_W) / 2.0).max(0.0) as i64;
    let y = mon.y + (logical_h - WIN_H - BOTTOM_MARGIN).max(0.0) as i64;
    // Both calls name the window explicitly: `resizeactive` acted on whatever
    // had focus, which after a moment is usually not the overlay.
    let size = format!("exact {} {},class:^(oc-voice)$", WIN_W as i64, WIN_H as i64);
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
mod tests {
    use super::*;
    use crate::process::FakeRunner;

    fn state(floating: bool, pinned: bool) -> OverlayState {
        OverlayState {
            address: "0xaa".to_string(),
            floating,
            pinned,
        }
    }

    fn applied(st: OverlayState) -> Vec<Vec<String>> {
        let fake = Arc::new(FakeRunner::new(b"[]".to_vec()));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        apply_float_and_pin(&runner, &st);
        fake.calls().into_iter().map(|(_, a)| a).collect()
    }

    #[test]
    fn an_already_floating_window_is_never_toggled_back() {
        // The original bug: togglefloating on a floating window TILES it,
        // which is the exact failure this code exists to prevent.
        let calls = applied(state(true, true));
        assert!(calls.is_empty(), "nothing to do, but ran: {calls:?}");

        let calls = applied(state(true, false));
        assert!(
            !calls.iter().any(|a| a.contains(&"setfloating".to_string())),
            "must not re-float an already floating window: {calls:?}"
        );
        assert!(calls.iter().any(|a| a.contains(&"pin".to_string())));

        // And a pinned-but-tiled window gets floated without re-pinning.
        let calls = applied(state(false, true));
        assert!(calls.iter().any(|a| a.contains(&"setfloating".to_string())));
        assert!(!calls.iter().any(|a| a.contains(&"pin".to_string())));
    }

    #[test]
    fn overlay_is_found_by_class_not_by_title_mention() {
        // An editor with the repo open has "oc-voice" in its title; matching
        // on the raw JSON would grab that window instead.
        let fake = Arc::new(FakeRunner::new(
            br#"[{"class":"code","title":"backlog - oc-voice - VS Code","address":"0xc1","floating":false,"pinned":false},
                 {"class":"oc-voice","title":"oc-voice","address":"0xaa","floating":true,"pinned":false}]"#.to_vec(),
        ));
        let runner: Arc<dyn CommandRunner> = fake;
        let found = overlay_state(&runner).expect("overlay found");
        assert_eq!(found.address, "0xaa");
        assert!(found.floating);
        assert!(!found.pinned);
    }

    fn mon(name: &str, x: i64, y: i64, w: i64, h: i64, scale: f64) -> Monitor {
        Monitor {
            name: name.to_string(),
            x,
            y,
            width: w,
            height: h,
            scale,
            focused: false,
        }
    }

    /// The real desk this was debugged against: laptop left, LG ultrawide in
    /// the middle, Samsung far right — reported by hyprctl in connector
    /// order, which is not left-to-right order.
    fn desk() -> Vec<Monitor> {
        vec![
            mon("eDP-1", 0, 576, 2560, 1440, 1.67),
            mon("DP-1", 4976, 360, 1920, 1080, 1.0),
            mon("HDMI-A-1", 1536, 0, 3440, 1440, 1.0),
        ]
    }

    #[test]
    fn positioning_names_the_window_instead_of_acting_on_the_focused_one() {
        // resizeactive hit whatever had focus, which after a moment is
        // usually not the overlay.
        let fake = Arc::new(FakeRunner::new(b"[]".to_vec()));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        position_overlay(&runner, &mon("DP-1", 0, 0, 2560, 1440, 1.0));
        let calls = fake.calls();
        assert!(!calls
            .iter()
            .any(|(_, a)| a.contains(&"resizeactive".to_string())));
        assert!(calls
            .iter()
            .all(|(_, a)| a.iter().any(|s| s.contains("class:^(oc-voice)$"))));
    }

    #[test]
    fn the_middle_monitor_is_spatial_not_the_first_reported() {
        // The bug: `.first()` on hyprctl's array picked the laptop panel,
        // which sits on the left, and called it primary.
        let desk = desk();
        assert_eq!(pick_monitor(&desk, "middle").unwrap().name, "HDMI-A-1");
        assert_eq!(pick_monitor(&desk, "left").unwrap().name, "eDP-1");
        assert_eq!(pick_monitor(&desk, "right").unwrap().name, "DP-1");
        // Spoken/Portuguese spellings resolve the same way.
        assert_eq!(pick_monitor(&desk, "meio").unwrap().name, "HDMI-A-1");
        // An explicit name wins, and an unknown one falls back rather than
        // leaving the overlay wherever Hyprland dropped it.
        assert_eq!(pick_monitor(&desk, "eDP-1").unwrap().name, "eDP-1");
        assert_eq!(pick_monitor(&desk, "DP-9").unwrap().name, "HDMI-A-1");
        assert!(pick_monitor(&[], "middle").is_none());
    }

    #[test]
    fn the_overlay_lands_inside_the_monitor_it_was_measured_against() {
        // The second half of the bug: monitor-local coordinates were passed
        // to movewindowpixel, which is global. On this desk that put a window
        // measured for the LG onto the laptop.
        let desk = desk();
        let target = pick_monitor(&desk, "middle").unwrap();
        let fake = Arc::new(FakeRunner::new(b"[]".to_vec()));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        position_overlay(&runner, target);

        let pos = fake
            .calls()
            .into_iter()
            .find(|(_, a)| a.contains(&"movewindowpixel".to_string()))
            .expect("a move was dispatched");
        let arg = pos.1.last().unwrap().clone();
        let nums: Vec<i64> = arg
            .trim_start_matches("exact ")
            .split(',')
            .next()
            .unwrap()
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        let (x, y) = (nums[0], nums[1]);

        let (lw, lh) = target.logical_size();
        assert!(
            x >= target.x && x + WIN_W as i64 <= target.x + lw as i64,
            "x={x} is outside {}..{}",
            target.x,
            target.x + lw as i64
        );
        assert!(
            y >= target.y && y + WIN_H as i64 <= target.y + lh as i64,
            "y={y} is outside {}..{}",
            target.y,
            target.y + lh as i64
        );
        // Bottom-centre, not wherever it fits.
        assert_eq!(x, target.x + ((lw - WIN_W) / 2.0) as i64);
        assert_eq!(y, target.y + (lh - WIN_H - BOTTOM_MARGIN) as i64);
    }

    #[test]
    fn focuses_resolved_window_before_typing() {
        let fake = Arc::new(FakeRunner::new(
            br#"[{"class":"code","title":"main.rs","address":"0x123"}]"#.to_vec(),
        ));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let windows = crate::wm::target::live_windows(&runner);
        let categories = std::collections::HashMap::new();
        let resolved = crate::wm::target::resolve("code", &categories, &windows, 0.82)
            .expect("code window resolves");
        focus_address_and_type(&runner, &resolved.address, "hello");
        let calls = fake.calls();
        assert!(calls
            .iter()
            .any(|(p, a)| p == "hyprctl" && a == &["dispatch", "focuswindow", "address:0x123"]));
    }
}
