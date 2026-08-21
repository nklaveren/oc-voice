//! One thing the whole test suite has to agree on: where its state lives.
//!
//! Twice now a test has reached the real `$XDG_STATE_HOME` and the person
//! running `just check` paid for it. The first time it silently rewrote the
//! overlay's saved position, and the report that came back was "it is not
//! saving where I put it" — a UI bug that was not a UI bug. The second time
//! the voice lock's own tests read whatever voiceprint happened to be on the
//! machine, so the suite passed or failed depending on whether the person had
//! enrolled. Next to that file sits a biometric, and the correct amount of
//! test code allowed near it is none.
//!
//! So: one helper, called by every test that touches stored state, before it
//! touches it.

/// Point `XDG_STATE_HOME` at a directory belonging to this test process.
///
/// `set_var` is process-global and the suite is threaded, so this runs once
/// and every caller gets the same answer. Calling it from a test that does not
/// need it is harmless; forgetting it in one that does is not.
pub fn isolate_state() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("oc-voice-test-{}", std::process::id()));
        std::env::set_var("XDG_STATE_HOME", &dir);
    });
}
