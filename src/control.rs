//! A control socket, so the overlay can be driven without touching it.
//!
//! Two reasons, and both are real:
//!
//! - **Keybindings.** Reaching for a floating overlay mid-call to press
//!   Gravar is the same friction the spoken commands exist to remove, and
//!   speaking the stop word out loud announces to the room that you were
//!   recording. A `Super+…` binding is the third way in.
//! - **Testing.** The buttons are the one part of the UI that no test can
//!   reach: the pointer path runs through the compositor. Driving the same
//!   state the buttons mutate makes the actions verifiable end to end even
//!   though the clicking is not.
//!
//! Deliberately a *request*, never an action. Everything here sets the same
//! `AppSettings` fields the buttons set, and the pipeline thread — which owns
//! the session — decides what happens. A second way to change state would be
//! a second way for the two to disagree.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};

use crate::{AppSettings, SessionRequest};

/// Where the socket lives. `XDG_RUNTIME_DIR` is per-user and cleared on
/// logout, which is what a liveness marker should be.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("oc-voice.sock")
}

/// The verbs, and what each maps to. Kept in one table so the client's help
/// text and the server's dispatch cannot drift apart.
const VERBS: &[(&str, &str)] = &[
    ("record", "start a recorded session"),
    ("stop", "close the recorded session"),
    ("mode", "switch between microphone and system audio"),
    ("settings", "open or close the Settings panel"),
    ("status", "print mode and whether a session is open"),
];

/// Whether another instance is already serving.
///
/// The socket is the lock: it is per-user, cleared on logout, and a live one
/// answers a connect. A stale one from a killed process does not, which is
/// the case that must *not* count as running.
pub fn already_running() -> bool {
    let path = socket_path();
    path.exists() && UnixStream::connect(&path).is_ok()
}

/// Serve until `running` clears. Errors are logged, never fatal: the app has
/// to work with no socket at all, on a system where the runtime dir is
/// read-only or the path is taken.
pub fn serve(settings: Arc<Mutex<AppSettings>>, running: Arc<AtomicBool>) {
    let path = socket_path();
    // A stale socket from a killed process would refuse to bind. Removing it
    // is safe because a live one answers, which is what `already_running` asks.
    if already_running() {
        warn!(path = %path.display(), "another oc-voice already holds the control socket");
        return;
    }
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "no control socket");
            return;
        }
    };
    // Non-blocking so shutdown does not wait for one more client.
    if listener.set_nonblocking(true).is_err() {
        warn!("control socket cannot poll; not serving");
        return;
    }
    info!(path = %path.display(), "control socket listening");

    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, &settings),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            Err(e) => {
                warn!(error = %e, "control socket accept failed");
                break;
            }
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn handle(stream: UnixStream, settings: &Arc<Mutex<AppSettings>>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let reply = apply(line.trim(), settings);
    let mut out = stream;
    let _ = writeln!(out, "{reply}");
}

/// Run one verb against the shared settings and describe what happened.
fn apply(verb: &str, settings: &Arc<Mutex<AppSettings>>) -> String {
    let mut s = crate::lock_settings(settings);
    match verb {
        "record" => {
            s.session_request = Some(SessionRequest::Start);
            "recording requested".into()
        }
        "stop" => {
            s.session_request = Some(SessionRequest::Stop);
            "stop requested".into()
        }
        "mode" => {
            s.mode = s.mode.next();
            format!("mode={:?}", s.mode)
        }
        "settings" => {
            s.toggle_settings = true;
            "settings toggled".into()
        }
        "status" => format!(
            "mode={:?} pending={:?} language={}",
            s.mode, s.session_request, s.language
        ),
        other => format!("unknown verb {other:?}"),
    }
}

/// Client side: send one verb to a running instance and print its answer.
pub fn send(verb: &str) -> Result<()> {
    if !VERBS.iter().any(|(v, _)| *v == verb) {
        let known: Vec<&str> = VERBS.iter().map(|(v, _)| *v).collect();
        return Err(anyhow!("unknown verb {verb:?}; try one of {known:?}"));
    }
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("no oc-voice listening on {}", path.display()))?;
    writeln!(stream, "{verb}")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    print!("{reply}");
    Ok(())
}

/// The verb list, for the CLI's usage text.
pub fn verbs() -> impl Iterator<Item = (&'static str, &'static str)> {
    VERBS.iter().copied()
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
