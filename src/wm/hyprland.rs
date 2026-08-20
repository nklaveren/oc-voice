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
pub fn try_hyprland_float(runner: Arc<dyn CommandRunner>) {
    std::thread::spawn(move || {
        if runner.output("hyprctl", &["version"]).is_err() {
            return;
        }

        let monitor_info = hyprctl_primary_monitor_info(&runner);
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
                            if let Some((w, h, scale)) = monitor_info {
                                position_overlay(&runner, w, h, scale);
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

/// Bottom-centre of the primary monitor, in logical pixels.
fn position_overlay(runner: &Arc<dyn CommandRunner>, mon_w: i64, mon_h: i64, scale: f64) {
    const WIN_W: f64 = 900.0;
    const WIN_H: f64 = 350.0;
    let logical_w = mon_w as f64 / scale;
    let logical_h = mon_h as f64 / scale;
    let x = ((logical_w - WIN_W) / 2.0) as i64;
    let y = (logical_h - WIN_H - 60.0) as i64;
    // Both calls name the window explicitly: `resizeactive` acted on whatever
    // had focus, which after a moment is usually not the overlay.
    let size = format!("exact {} {},class:^(oc-voice)$", WIN_W as i64, WIN_H as i64);
    let _ = runner.output("hyprctl", &["dispatch", "resizewindowpixel", &size]);
    let pos = format!("exact {x} {y},class:^(oc-voice)$");
    let _ = runner.output("hyprctl", &["dispatch", "movewindowpixel", &pos]);
    info!(x, y, scale, "overlay positioned");
}

fn hyprctl_primary_monitor_info(runner: &Arc<dyn CommandRunner>) -> Option<(i64, i64, f64)> {
    let output = runner.output("hyprctl", &["monitors", "-j"]).ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let mon = json.as_array()?.first()?;
    let w = mon.get("width")?.as_i64()?;
    let h = mon.get("height")?.as_i64()?;
    let scale = mon.get("scale")?.as_f64().unwrap_or(1.0);
    Some((w, h, scale))
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

    #[test]
    fn positioning_names_the_window_instead_of_acting_on_the_focused_one() {
        // resizeactive hit whatever had focus, which after a moment is
        // usually not the overlay.
        let fake = Arc::new(FakeRunner::new(b"[]".to_vec()));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        position_overlay(&runner, 2560, 1440, 1.0);
        let calls = fake.calls();
        assert!(!calls
            .iter()
            .any(|(_, a)| a.contains(&"resizeactive".to_string())));
        assert!(calls
            .iter()
            .all(|(_, a)| a.iter().any(|s| s.contains("class:^(oc-voice)$"))));
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
