//! Tests for the control socket.
//!
//! `apply` is where a verb becomes a state change, and it is the half worth
//! pinning: the socket plumbing either connects or does not, but a verb that
//! quietly stops matching would leave a keybinding that does nothing.

use super::*;

fn settings() -> Arc<Mutex<AppSettings>> {
    Arc::new(Mutex::new(AppSettings {
        language: "pt".to_string(),
        mode: crate::TranscribeMode::Enter,
        detected_language: None,
        session_request: None,
        toggle_settings: false,
    }))
}

#[test]
fn every_advertised_verb_does_something() {
    // The list the client validates against and the list the server handles
    // are the same table; this checks nothing in it falls through to the
    // unknown branch, which is how a documented binding becomes a no-op.
    let s = settings();
    for (verb, _) in verbs() {
        let reply = apply(verb, &s);
        assert!(
            !reply.starts_with("unknown"),
            "{verb:?} is advertised but unhandled"
        );
    }
}

#[test]
fn control_asks_and_never_acts() {
    // The property that keeps two entry points from disagreeing: the socket
    // sets exactly what the buttons set, and the pipeline decides. If this
    // ever opened a session directly, the overlay's own button and the
    // keybinding would be two different code paths for one action.
    let s = settings();
    apply("record", &s);
    assert!(matches!(
        crate::lock_settings(&s).session_request,
        Some(SessionRequest::Start)
    ));
    apply("stop", &s);
    assert!(matches!(
        crate::lock_settings(&s).session_request,
        Some(SessionRequest::Stop)
    ));
}

#[test]
fn mode_cycles_the_same_way_the_button_does() {
    let s = settings();
    let before = crate::lock_settings(&s).mode;
    apply("mode", &s);
    let after = crate::lock_settings(&s).mode;
    assert_ne!(before, after);
    assert_eq!(after, before.next(), "socket and button must agree");
}

#[test]
fn an_unknown_verb_changes_nothing() {
    let s = settings();
    let before = crate::lock_settings(&s).mode;
    let reply = apply("gravar_tudo_agora", &s);
    assert!(reply.starts_with("unknown"));
    assert_eq!(crate::lock_settings(&s).mode, before);
    assert!(crate::lock_settings(&s).session_request.is_none());
}
