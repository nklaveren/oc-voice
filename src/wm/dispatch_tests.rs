//! Tests for spoken WM dispatch (M3.1–M3.3), including the live
//! verification suite (`--ignored`).

use super::*;

use crate::process::FakeRunner;

fn setup(fixture: &[u8]) -> (Arc<FakeRunner>, Arc<dyn CommandRunner>, Config) {
    let fake = Arc::new(FakeRunner::new(fixture.to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    (fake, runner, crate::config::Config::embedded())
}

fn dispatched(fake: &FakeRunner) -> Vec<Vec<String>> {
    fake.calls()
        .iter()
        .filter(|(p, a)| p == "hyprctl" && a.first().map(String::as_str) == Some("dispatch"))
        .map(|(_, a)| a.clone())
        .collect()
}

fn say(spoken: &str, fixture: &[u8]) -> (Vec<Vec<String>>, Option<PendingAction>) {
    let (fake, runner, config) = setup(fixture);
    let vocab = config.vocab("pt").unwrap().clone();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut pending = None;
    dispatch_spoken(&vocab, &config, spoken, &runner, &tx, &mut pending);
    (dispatched(&fake), pending)
}

const MONITORS: &[u8] = br#"[
        {"name": "AAA-1", "description": "BOE 0x0A88", "x": 5000},
        {"name": "BBB-1", "description": "Samsung Electric Company Odyssey G30B", "x": 0},
        {"name": "CCC-1", "description": "LG Electronics LG ULTRAWIDE", "x": 2000}
    ]"#;
const CLIENTS: &[u8] = br#"[{"class":"brave-browser","title":"docs - Brave","address":"0xb1"}]"#;

#[test]
fn whole_utterance_commands_dispatch() {
    assert_eq!(say("tela cheia", b"[]").0, [["dispatch", "fullscreen"]]);
    assert_eq!(say("flutuante", b"[]").0, [["dispatch", "togglefloating"]]);
}

#[test]
fn direction_templates_dispatch_movefocus() {
    assert_eq!(
        say("janela da esquerda", b"[]").0,
        [["dispatch", "movefocus", "l"]]
    );
    assert_eq!(
        say("janela de cima", b"[]").0,
        [["dispatch", "movefocus", "u"]]
    );
}

#[test]
fn spoken_and_digit_numbers_dispatch_identically() {
    // M3.2 acceptance.
    assert_eq!(
        say("área de trabalho quatro", b"[]").0,
        [["dispatch", "workspace", "4"]]
    );
    assert_eq!(
        say("área de trabalho 4", b"[]").0,
        [["dispatch", "workspace", "4"]]
    );
    assert_eq!(
        say("leva pra três", b"[]").0,
        [["dispatch", "movetoworkspace", "3"]]
    );
    for (word, n) in [("um", 1), ("dois", 2), ("cinco", 5), ("dez", 10)] {
        assert_eq!(
            say(&format!("área de trabalho {word}"), b"[]").0,
            [vec![
                "dispatch".to_string(),
                "workspace".to_string(),
                n.to_string()
            ]]
        );
    }
}

#[test]
fn monitor_position_follows_the_x_layout() {
    // M3.1 acceptance: positions swapped relative to any real machine —
    // "direita" must be the highest x, not any hardcoded name.
    assert_eq!(
        say("monitor da direita", MONITORS).0,
        [["dispatch", "focusmonitor", "AAA-1"]]
    );
    assert_eq!(
        say("monitor da esquerda", MONITORS).0,
        [["dispatch", "focusmonitor", "BBB-1"]]
    );
    assert_eq!(
        say("monitor do meio", MONITORS).0,
        [["dispatch", "focusmonitor", "CCC-1"]]
    );
}

#[test]
fn monitor_by_brand_matches_the_description() {
    assert_eq!(
        say("monitor samsung", MONITORS).0,
        [["dispatch", "focusmonitor", "BBB-1"]]
    );
    // Short brands (LG) match exactly instead of being dropped as noise.
    assert_eq!(
        say("monitor lg", MONITORS).0,
        [["dispatch", "focusmonitor", "CCC-1"]]
    );
    assert!(say("monitor dell", MONITORS).0.is_empty());
}

#[test]
fn focus_window_goes_through_the_target_resolver() {
    assert_eq!(
        say("foca o brave", CLIENTS).0,
        [["dispatch", "focuswindow", "address:0xb1"]]
    );
}

#[test]
fn kill_active_waits_for_confirmation() {
    // M3.3: "fecha" alone closes nothing.
    let (calls, pending) = say("fecha", b"[]");
    assert!(calls.is_empty(), "destructive action must not dispatch");
    assert!(matches!(pending, Some(PendingAction::Dispatch { .. })));
}

#[test]
fn unrecognized_speech_dispatches_nothing() {
    assert!(say("bom dia pessoal", b"[]").0.is_empty());
}

mod live {
    use super::*;

    use crate::process::SystemRunner;

    /// Live: what the monitor words resolve to on THIS session right now.
    #[test]
    #[ignore]
    fn live_monitors_resolve_by_position_and_brand() {
        let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);
        let monitors = live_monitors(&runner);
        assert!(!monitors.is_empty(), "no monitors listed");
        println!("\nmonitores vivos (por x):");
        let mut sorted: Vec<&MonitorInfo> = monitors.iter().collect();
        sorted.sort_by_key(|m| m.x);
        for m in &sorted {
            println!("  x={:<6} {:<10} {}", m.x, m.name, m.description);
        }
        let config = crate::config::Config::load();
        let vocab = config.vocab("pt").expect("pt vocab");
        println!("\nposição:");
        for (word, dir) in [("esquerda", "l"), ("meio", "m"), ("direita", "r")] {
            match monitor_by_position(&monitors, dir) {
                Some(name) => println!("  monitor da {word:<10} -> {name}"),
                None => println!("  monitor da {word:<10} -> [não resolveu]"),
            }
        }
        println!("\nmarca/modelo:");
        for spoken in ["samsung", "lg", "odyssey", "ultrawide", "dell"] {
            match monitor_by_name(spoken, vocab, &monitors, config.threshold()) {
                Some(name) => println!("  monitor {spoken:<10} -> {name}"),
                None => println!("  monitor {spoken:<10} -> [recusado]"),
            }
        }
    }
}
