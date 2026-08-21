//! Tests for the toplevel host's float/pin/position compensation.
//!
//! Every one of these pins a bug that actually happened: a toggle that tiled
//! the window it meant to float, a resize that hit whatever had focus, a
//! monitor picked by connector order, and coordinates computed monitor-local
//! and dispatched as global.

use super::*;
use crate::process::FakeRunner;

/// The same numbers the host asks for, so the test cannot pass against a
/// geometry the app never uses.
const WIN_W: f64 = crate::ui::overlay::OVERLAY_W as f64;
const WIN_H: f64 = crate::ui::overlay::OVERLAY_H as f64;
const BOTTOM_MARGIN: f64 = crate::ui::overlay::OVERLAY_BOTTOM_MARGIN as f64;

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
    position_overlay(
        &runner,
        &mon("DP-1", 0, 0, 2560, 1440, 1.0),
        WIN_W,
        WIN_H,
        BOTTOM_MARGIN,
    );
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
    position_overlay(&runner, target, WIN_W, WIN_H, BOTTOM_MARGIN);

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
