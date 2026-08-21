//! Tests for browser-tab targets. The fixture is the real `/json/list` shape
//! from the browser this was built against, trimmed to the fields used.

use super::*;
use crate::process::FakeRunner;

const LIST: &str = r#"[
  {"id":"A1","type":"page","title":"DeepSeek lança Harness e espanta o mundo da IA - YouTube"},
  {"id":"B2","type":"page","title":"WhatsApp"},
  {"id":"C3","type":"page","title":"Feed | LinkedIn"},
  {"id":"D4","type":"page","title":"Mail - Klaveren, Nicolas - Outlook"},
  {"id":"E5","type":"iframe","title":"recaptcha anchor"},
  {"id":"F6","type":"page","title":"   "}
]"#;

fn tabs() -> Vec<Tab> {
    let fake = Arc::new(FakeRunner::new(LIST.as_bytes().to_vec()));
    let runner: Arc<dyn CommandRunner> = fake;
    live_tabs(&runner, 9222)
}

#[test]
fn only_real_pages_become_targets() {
    // Iframes, extension pages and service workers outnumber real tabs and
    // carry titles nobody would ever say out loud.
    let t = tabs();
    assert_eq!(
        t.len(),
        4,
        "got {:?}",
        t.iter().map(|x| &x.title).collect::<Vec<_>>()
    );
    assert!(!t.iter().any(|x| x.kind == "iframe"));
    assert!(!t.iter().any(|x| x.title.trim().is_empty()));
}

#[test]
fn a_tab_is_found_by_one_word_of_its_title() {
    // The case that started this: YouTube is three tabs deep inside a window
    // the window manager only knows as `brave-browser`, so the window list
    // could never answer "vai pro youtube".
    let t = tabs();
    let threshold = 0.90;
    assert_eq!(resolve("youtube", &t, threshold).unwrap().id, "A1");
    assert_eq!(resolve("whatsapp", &t, threshold).unwrap().id, "B2");
    assert_eq!(resolve("linkedin", &t, threshold).unwrap().id, "C3");
    assert_eq!(resolve("outlook", &t, threshold).unwrap().id, "D4");
}

#[test]
fn an_unrelated_word_resolves_to_nothing() {
    // A tab title is arbitrary text that changes with whatever page is
    // loaded. Guessing here would activate a tab the user never named, and
    // the window list behind it would never get its turn.
    let t = tabs();
    for spoken in ["terminal", "editor", "spotify", "planilha"] {
        assert!(
            resolve(spoken, &t, 0.90).is_none(),
            "{spoken:?} matched {:?}",
            resolve(spoken, &t, 0.90).map(|x| &x.title)
        );
    }
}

#[test]
fn a_closed_port_yields_no_tabs_and_no_error() {
    // Most people do not run their browser with a debugging port. That is
    // the normal case, not a failure: an unresolvable target must fall
    // through to the window list rather than break the utterance.
    let fake = Arc::new(FakeRunner::new(b"".to_vec()));
    let runner: Arc<dyn CommandRunner> = fake;
    assert!(live_tabs(&runner, 9222).is_empty());
}

#[test]
fn activating_names_the_tab_it_was_given() {
    let fake = Arc::new(FakeRunner::new(b"".to_vec()));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    let tab = Tab {
        id: "B2".into(),
        title: "WhatsApp".into(),
        kind: "page".into(),
        debugger: String::new(),
    };
    activate(&runner, 9222, &tab);
    assert!(
        fake.calls()
            .iter()
            .any(|(p, a)| p == "curl" && a.iter().any(|s| s.ends_with("/json/activate/B2"))),
        "{:?}",
        fake.calls()
    );
}
