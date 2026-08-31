//! Tests for the window-manager seam.
//!
//! The grammar's own tests already pin which action a phrase resolves to.
//! What is pinned here is the other half: that each action still reaches the
//! compositor spelled the way it always was. A port swaps this encoding, and
//! an encoding nobody asserts is an encoding that drifts.

use super::*;
use crate::process::FakeRunner;

fn hyprctl(fixture: &[u8]) -> (Arc<FakeRunner>, Hyprctl) {
    let fake = Arc::new(FakeRunner::new(fixture.to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    (fake, Hyprctl::new(runner))
}

#[test]
fn every_action_keeps_the_spelling_it_had() {
    // These strings were `dispatch_args` before the seam existed, and they
    // are what a live Hyprland has been answering to all along.
    let cases: &[(WmAction, &[&str])] = &[
        (WmAction::Fullscreen, &["dispatch", "fullscreen"]),
        (WmAction::ToggleFloating, &["dispatch", "togglefloating"]),
        (WmAction::KillActive, &["dispatch", "killactive"]),
        (
            WmAction::CloseWindow {
                address: "0xb1".into(),
            },
            &["dispatch", "closewindow", "address:0xb1"],
        ),
        (
            WmAction::CycleWindow { previous: false },
            &["dispatch", "cyclenext"],
        ),
        (
            WmAction::CycleWindow { previous: true },
            &["dispatch", "cyclenext", "prev"],
        ),
        (
            WmAction::MoveFocus {
                direction: "l".into(),
            },
            &["dispatch", "movefocus", "l"],
        ),
        (
            WmAction::Workspace { number: 3 },
            &["dispatch", "workspace", "3"],
        ),
        (
            WmAction::MoveToWorkspace { number: 4 },
            &["dispatch", "movetoworkspace", "4"],
        ),
        (
            WmAction::FocusMonitor {
                name: "DP-1".into(),
            },
            &["dispatch", "focusmonitor", "DP-1"],
        ),
        (
            WmAction::FocusWindow {
                address: "0xb1".into(),
            },
            &["dispatch", "focuswindow", "address:0xb1"],
        ),
    ];
    for (action, expected) in cases {
        assert_eq!(
            Hyprctl::args(action),
            *expected,
            "{action:?} changed how it is spelled"
        );
    }
}

#[test]
fn reads_go_through_the_same_seam() {
    // The resolver scores against class and title. Where those came from is
    // exactly what a port replaces, so the read path belongs to the trait
    // rather than to a free function reaching for hyprctl.
    let fixture = br#"[{"class":"brave-browser","title":"docs - Brave","address":"0xb1"}]"#;
    let (fake, wm) = hyprctl(fixture);
    let windows = wm.windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].class, "brave-browser");
    assert!(
        fake.calls()
            .iter()
            .any(|(p, a)| p == "hyprctl" && a == &["clients", "-j"]),
        "{:?}",
        fake.calls()
    );
}

#[test]
fn monitors_arrive_with_their_position() {
    // Sorted by the caller, not here — but the x has to survive the trip, or
    // "monitor da direita" goes back to being connector order.
    let fixture = br#"[{"name":"AAA-1","description":"BOE","x":5000},
                       {"name":"BBB-1","description":"LG ULTRAWIDE","x":0}]"#;
    let (_, wm) = hyprctl(fixture);
    let mons = wm.monitors();
    assert_eq!(mons.len(), 2);
    assert_eq!(mons[0].x, 5000);
    assert_eq!(mons[1].description, "LG ULTRAWIDE");
}

#[test]
fn a_broken_query_yields_nothing_rather_than_panicking() {
    // A compositor that is not running answers with garbage, and the app has
    // to keep dictating.
    let (_, wm) = hyprctl(b"not json at all");
    assert!(wm.windows().is_empty());
    assert!(wm.monitors().is_empty());
    assert!(wm.focused_title().is_none());
}

#[test]
fn macos_says_the_one_verb_it_can_and_refuses_the_rest() {
    // The mac backend is asserted from whatever machine runs the suite, the
    // way `MacInjector` already is: `args` only builds strings, so the
    // encoding is checkable without a mac and without launching anything.
    assert_eq!(
        MacOs::args(&WmAction::Launch {
            command: "/Applications/Xyz.app".to_string(),
        }),
        Some((
            "open",
            vec!["-a".to_string(), "/Applications/Xyz.app".to_string()]
        )),
    );

    // Everything else is M8.4b. Reporting "not carried out" is what lets the
    // fall-through above turn an unspoken-for command back into dictation;
    // inventing an encoding here would make it look like it worked.
    for action in [
        WmAction::Fullscreen,
        WmAction::KillActive,
        WmAction::Workspace { number: 3 },
        WmAction::MoveFocus {
            direction: "l".to_string(),
        },
        WmAction::FocusWindow {
            address: "0xb1".to_string(),
        },
    ] {
        assert_eq!(
            MacOs::args(&action),
            None,
            "{action:?} has no macOS spelling yet"
        );
    }
}
