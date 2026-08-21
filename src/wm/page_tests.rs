//! Tests for matching spoken text against a page's controls.
//!
//! The socket half is not unit-testable — it needs a browser — so what is
//! pinned here is the half that decides: which controls a word reaches, and
//! that ambiguity is reported rather than resolved.
//!
//! The fixture is real. These labels were read out of a live Outlook by
//! `Runtime.evaluate`, including the icon-font glyphs, because inventing
//! tidier ones would test a page that does not exist.

use super::*;

fn control(text: &str, id: u32) -> Control {
    Control {
        text: text.to_string(),
        role: "button".to_string(),
        x: 0.0,
        y: 0.0,
        id,
    }
}

fn page() -> Vec<Control> {
    [
        "Mail",
        "Calendar",
        "People",
        "To Do",
        "Teams Chat",
        "Search for email, meetings, files",
        "\u{e135}\nReply",
        "\u{e137}\nReply all",
        "\u{e0ff}\nForward",
        "\u{e98e}",
        "\u{efc5}",
    ]
    .iter()
    .enumerate()
    .map(|(i, t)| control(t, i as u32))
    .collect()
}

#[test]
fn icon_only_controls_are_never_offered() {
    // A third of that page was private-use glyphs from an icon font. Nobody
    // can say them, and offering them as candidates would mean a spoken word
    // could land on one by scoring noise against noise.
    let p = page();
    let icons: Vec<&Control> = p.iter().filter(|c| !c.speakable()).collect();
    assert_eq!(icons.len(), 2, "fixture should carry icon-only controls");
    for c in icons {
        assert!(
            !resolve(&c.text, &p, 0.82).iter().any(|r| r.id == c.id),
            "{:?} is unspeakable and must not be reachable",
            c.text
        );
    }
}

#[test]
fn a_word_reaches_the_control_that_carries_it() {
    let p = page();
    for (spoken, expect) in [("calendar", "Calendar"), ("people", "People")] {
        let hits = resolve(spoken, &p, 0.82);
        assert_eq!(
            hits.first().map(|c| c.text.as_str()),
            Some(expect),
            "{spoken:?} did not reach {expect:?}"
        );
    }
    // The label carries a glyph the person cannot say; the word inside it
    // still has to work, which is why scoring is per token and not whole
    // string.
    assert!(
        resolve("forward", &p, 0.82)
            .first()
            .is_some_and(|c| c.text.contains("Forward")),
        "a word next to an icon glyph must still be reachable"
    );
}

#[test]
fn ambiguity_is_returned_rather_than_resolved() {
    // The property this module exists for. "Reply" and "Reply all" both
    // answer to "reply", and picking the first is how the wrong mail gets
    // sent to the wrong list of people. The caller has to be told there were
    // two — the same rule "vai pro terminal" broke by taking whichever window
    // the compositor happened to list first.
    let p = page();
    let hits = resolve("reply", &p, 0.82);
    assert!(
        hits.len() > 1,
        "expected several candidates, got {:?}",
        hits.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

#[test]
fn an_unrelated_word_reaches_nothing() {
    for spoken in ["spotify", "planilha", "terminal"] {
        assert!(
            resolve(spoken, &page(), 0.82).is_empty(),
            "{spoken:?} matched something it should not"
        );
    }
}

/// Live: what the page in the focused window actually offers, and whether a
/// spoken word reaches it. Ignored by default — it needs a browser running
/// with a debugging port and a page in front.
mod live {
    use super::*;
    use crate::process::SystemRunner;
    use std::sync::Arc;

    #[test]
    #[ignore]
    fn the_focused_page_answers() {
        let runner: Arc<dyn crate::process::CommandRunner> = Arc::new(SystemRunner);
        let port = crate::config::Config::load()
            .browser_port()
            .expect("set debug_port in commands.toml");
        let tab = focused_tab(&runner, port).expect("no debuggable page in the focused window");
        println!("página: {}", tab.title);

        let mut page = Page::connect(&tab.debugger).expect("connect");
        let controls = page.controls().expect("survey");
        let speakable = controls.iter().filter(|c| c.speakable()).count();
        println!(
            "  {} controles, {speakable} nomeáveis, {} só ícone",
            controls.len(),
            controls.len() - speakable
        );

        // Words to try can be given on the command line, because the page
        // in front of you is not the page this was written against.
        let probe = std::env::var("OCV_SAY").unwrap_or_else(|_| "search,spotify".into());
        for spoken in probe.split(',') {
            let hits = resolve(spoken, &controls, 0.82);
            let names: Vec<&str> = hits.iter().take(3).map(|c| c.text.as_str()).collect();
            println!("  {spoken:<10} -> {} candidato(s) {names:?}", hits.len());
        }

        // Focusing a field is the one effect safe to take unattended: it
        // moves the caret and changes nothing.
        let first = probe.split(',').next().unwrap_or_default().to_string();
        if let Some(target) = resolve(&first, &controls, 0.82).first() {
            page.focus(target).expect("focus");
            let active = page
                .eval("document.activeElement && (document.activeElement.getAttribute('aria-label')||document.activeElement.tagName)")
                .expect("read activeElement");
            println!("  foco agora em: {active}");
        }
    }
}
