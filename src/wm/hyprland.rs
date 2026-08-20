use crate::input::inject::{type_key, type_text};
use crate::process::CommandRunner;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

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

/// Best-effort: detect Hyprland and auto-float+pin the oc-voice window.
///
/// Hyprland 0.54.x does not reliably match window rules on xdg_toplevel
/// windows whose app_id arrives after surface creation. This workaround
/// polls `hyprctl clients` for our window and dispatches togglefloating +
/// pin directly.  Non-Hyprland systems are silently skipped.
pub fn try_hyprland_float(runner: Arc<dyn CommandRunner>) {
    std::thread::spawn(move || {
        if runner.output("hyprctl", &["version"]).is_err() {
            return;
        }

        let monitor_info = hyprctl_primary_monitor_info(&runner);

        std::thread::sleep(Duration::from_millis(300));

        for attempt in 0..15 {
            if let Ok(output) = runner.output("hyprctl", &["clients", "-j"]) {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.contains("\"oc-voice\"") {
                    std::thread::sleep(Duration::from_millis(100));
                    let _ =
                        runner.output("hyprctl", &["dispatch", "togglefloating", "class:oc-voice"]);
                    let _ = runner.output("hyprctl", &["dispatch", "pin", "class:oc-voice"]);

                    if let Some((mon_w, mon_h, scale)) = monitor_info {
                        let win_w: f64 = 900.0;
                        let win_h: f64 = 350.0;
                        let logical_w = mon_w as f64 / scale;
                        let logical_h = mon_h as f64 / scale;
                        let x = ((logical_w - win_w) / 2.0) as i64;
                        let y = ((logical_h - win_h - 60.0) / 1.0) as i64;
                        let pos = format!("{x} {y}");
                        let _ = runner.output(
                            "hyprctl",
                            &[
                                "dispatch",
                                "movewindowpixel",
                                "exact",
                                &pos,
                                "class:oc-voice",
                            ],
                        );
                        let size = format!("{} {}", win_w as i64, win_h as i64);
                        let _ =
                            runner.output("hyprctl", &["dispatch", "resizeactive", "exact", &size]);
                        info!(x, y, scale, "positioned overlay at bottom-center");
                    }

                    info!("applied hyprland float + pin via hyprctl");
                    return;
                }
            }
            if attempt == 0 {
                info!("detected Hyprland, waiting for oc-voice window to register...");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        debug!("hyprctl auto-float: window not found after 3 s, giving up");
    });
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
