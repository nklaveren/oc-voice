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
        "close_window",
        "move_focus",
        "workspace",
        "move_to_workspace",
        "focus_monitor",
        "focus_monitor_name",
        "focus_window",
        "next_window",
        "previous_window",
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

#[test]
fn a_partial_segmentation_override_keeps_the_other_fields() {
    // The trap this guards: deserializing straight into `Segmentation` made
    // `hang_ms = 400` under [segmentation.subtitle] silently reset
    // max_seconds to the generic default of 20 — the wall-of-text bug that
    // the subtitle profile exists to prevent, reappearing from a one-line
    // tweak that looks harmless.
    let raw: RawConfig = toml::from_str(
        r#"
        [segmentation.subtitle]
        hang_ms = 400
        "#,
    )
    .expect("partial override parses");

    let mut set = SegmentationSet::default();
    let before = set.clone();
    raw.segmentation.apply_to(&mut set);

    assert_eq!(set.subtitle.hang_ms, 400, "the named field changes");
    assert_eq!(
        set.subtitle.max_seconds, before.subtitle.max_seconds,
        "an unnamed field must not fall back to the generic default"
    );
    assert_eq!(
        set.subtitle.partial_every_ms,
        before.subtitle.partial_every_ms
    );
    assert_eq!(
        set.dictation.hang_ms, before.dictation.hang_ms,
        "the untouched profile stays untouched"
    );
}

#[test]
fn the_shipped_profiles_keep_meetings_phrase_sized() {
    // These numbers are the fix for segments running to the cap and mixing
    // speakers; a regression here is a regression in the meeting UI.
    let c = Config::embedded();
    let sub = c.segmentation(true);
    let dict = c.segmentation(false);
    assert!(
        sub.hang_ms < dict.hang_ms,
        "following someone must close faster than dictating"
    );
    assert!(
        sub.max_seconds < dict.max_seconds,
        "a subtitle must not be allowed to run as long as a dictation"
    );
}

#[test]
fn the_overlay_monitor_defaults_to_the_middle_one() {
    assert_eq!(
        Config::embedded().overlay_monitor(),
        DEFAULT_OVERLAY_MONITOR
    );
}
