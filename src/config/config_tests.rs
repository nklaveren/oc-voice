//! Tests for the vocabulary config: embedded defaults, user overrides, and
//! the internal consistency of the shipped bindings.

use super::*;

#[test]
fn embedded_default_parses_with_pt_and_en() {
    let c = Config::embedded();
    let pt = c.vocab("pt").expect("pt section");
    assert!(pt.send.iter().any(|w| w == "câmbio"));
    assert!(!pt.numbers.is_empty());
    assert!(!pt.directions.is_empty());
    let en = c.vocab("en").expect("en section");
    assert!(en.send.iter().any(|w| w == "send"));
}

#[test]
fn cjk_languages_have_no_commands() {
    let c = Config::embedded();
    assert!(c.vocab("ja").is_none());
    assert!(c.vocab("zh").is_none());
}

/// Consistency of the shipped bindings: a typo in an action name or a
/// direction value in default.toml would otherwise only surface as a
/// command that silently does nothing.
#[test]
fn embedded_bindings_are_internally_consistent() {
    const KNOWN_ACTIONS: &[&str] = &[
        "fullscreen",
        "toggle_floating",
        "kill_active",
        "move_focus",
        "workspace",
        "move_to_workspace",
        "focus_monitor",
        "focus_monitor_name",
        "focus_window",
    ];
    const KNOWN_SLOTS: &[&str] = &["direcao", "numero", "alvo", "monitor"];
    let c = Config::embedded();
    for lang in ["pt", "en"] {
        let v = c.vocab(lang).unwrap_or_else(|| panic!("{lang} section"));
        assert!(!v.send.is_empty(), "{lang}: send vocabulary empty");
        assert!(!v.cancel.is_empty(), "{lang}: cancel vocabulary empty");
        // The confirmation policy (M4.3) is dead without these.
        assert!(!v.confirm.is_empty(), "{lang}: confirm empty");
        assert!(!v.deny.is_empty(), "{lang}: deny empty");
        for (word, action) in &v.wm_commands {
            assert!(
                KNOWN_ACTIONS.contains(&action.as_str()),
                "{lang}: wm_command \"{word}\" names unknown action \"{action}\""
            );
        }
        for t in &v.templates {
            assert!(
                KNOWN_ACTIONS.contains(&t.action.as_str()),
                "{lang}: template \"{}\" names unknown action \"{}\"",
                t.pattern,
                t.action
            );
            let slot = t
                .pattern
                .split_whitespace()
                .find_map(|w| w.strip_prefix('{').and_then(|w| w.strip_suffix('}')));
            let slot =
                slot.unwrap_or_else(|| panic!("{lang}: template \"{}\" has no slot", t.pattern));
            assert!(
                KNOWN_SLOTS.contains(&slot),
                "{lang}: template \"{}\" uses unknown slot \"{slot}\"",
                t.pattern
            );
        }
        for (word, dir) in &v.directions {
            assert!(
                ["r", "l", "u", "d", "m"].contains(&dir.as_str()),
                "{lang}: direction \"{word}\" maps to invalid \"{dir}\""
            );
        }
        for (word, n) in &v.numbers {
            assert!(
                (1..=10).contains(n),
                "{lang}: number \"{word}\" maps to out-of-range {n}"
            );
        }
        for (cat, patterns) in &v.targets {
            assert!(
                !patterns.is_empty(),
                "{lang}: target category \"{cat}\" empty"
            );
        }
    }
    // pt additionally drives the send-to grammar.
    assert!(!c.vocab("pt").unwrap().send_to.is_empty());
}

/// Dump of every effective binding — run with --ignored --nocapture.
#[test]
#[ignore]
fn live_dump_effective_bindings() {
    let c = Config::load();
    println!(
        "\nthreshold={} confirm_below={}",
        c.threshold(),
        c.confirm_below()
    );
    for lang in ["pt", "en"] {
        let v = c.vocab(lang).unwrap();
        println!("\n[{lang}]");
        println!(
            "  prefix     {:?} (require: {})",
            v.prefix, v.require_prefix
        );
        println!("  send       {:?}", v.send);
        println!("  cancel     {:?}", v.cancel);
        println!("  newline    {:?}", v.newline);
        println!("  confirm    {:?}  deny {:?}", v.confirm, v.deny);
        println!("  send_to    {:?}", v.send_to);
        let mut nums: Vec<_> = v.numbers.iter().collect();
        nums.sort_by_key(|(_, n)| **n);
        println!("  numbers    {nums:?}");
        println!("  directions {:?}", v.directions);
        for (cat, pats) in &v.targets {
            println!("  target {cat:<10} -> {pats:?}");
        }
        for (w, a) in &v.wm_commands {
            println!("  wm  \"{w}\" -> {a}");
        }
        for t in &v.templates {
            println!("  tpl \"{}\" -> {}", t.pattern, t.action);
        }
    }
}

#[test]
fn user_section_replaces_embedded_language() {
    let mut base = Config::embedded();
    let user: RawConfig = toml::from_str(
        r#"
            [es]
            send = ["envía", "listo"]
            "#,
    )
    .unwrap();
    for (lang, vocab) in user.languages {
        base.languages.insert(lang, vocab);
    }
    let es = base.vocab("es").expect("es section");
    assert!(es.send.iter().any(|w| w == "listo"));
    assert!(es.cancel.is_empty());
}
