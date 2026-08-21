//! Asking a page what is on it, instead of guessing from pixels.
//!
//! [`super::tabs`] talks to the browser over two plain GETs and deliberately
//! stops there. Listing and raising a tab fit in a URL; asking "what can be
//! clicked, and where" does not — that needs `Runtime.evaluate`, and that
//! needs the WebSocket the DevTools endpoint hands out per page.
//!
//! **Why this beats OCR inside a browser**, measured against a live Outlook:
//! 115 clickable controls found, 65% carrying text a person could say, and
//! every one of them with exact coordinates and a role. OCR reads pixels, so
//! it cannot tell a button from a paragraph, costs ~2.5 s per look, and reads
//! the remaining 34% — icon-font glyphs like `\u{e98e}` — as nothing at all.
//! Here they are at least *known* to exist.
//!
//! What it does not fix is choosing: `reply` appeared three times on that one
//! page. Finding is easy and picking is the hard part, which is the same
//! lesson `fechar teams` and `vai pro terminal` already taught. So the
//! resolver returns every candidate and lets the caller ask.

use std::net::TcpStream;
use std::time::Duration;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info};
use tungstenite::{connect, Message, WebSocket};

use crate::commands::matcher;
use crate::process::CommandRunner;
use crate::ui::stdout::emit;

/// A control the page says is there.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Control {
    /// Visible text, or the accessible name when there is no text. Empty for
    /// the icon-only third of a modern web app.
    pub text: String,
    /// `button`, `textbox`, `combobox`… as the page declares it.
    #[serde(default)]
    pub role: String,
    /// Centre, in the page's own CSS pixels.
    pub x: f64,
    pub y: f64,
    /// A handle back to the element, for acting without coordinates.
    pub id: u32,
}

impl Control {
    /// Whether a person could name this out loud. The icon-only controls
    /// carry private-use glyphs from an icon font, which are neither
    /// speakable nor readable.
    pub fn speakable(&self) -> bool {
        self.text.chars().filter(|c| c.is_alphanumeric()).count() >= 3
    }
}

/// The script that answers "what is on this page". Runs in the page, returns
/// JSON, and stamps each element with an index so a later call can act on it
/// without re-finding it by text.
///
/// `[role=row]` is deliberately absent, and the reason is measured rather than
/// assumed. A live chat client exposes its conversation list as 69 of them,
/// which looked like exactly the thing to reach — and every one returned empty
/// `innerText` with no accessible name. Adding the role brought 78 controls of
/// which zero could be named out loud. A list is only addressable when its
/// entries carry text; here they do not, and the honest answer is that this
/// app's conversations are out of reach rather than that they are listed.
const SURVEY: &str = r#"(() => {
  const sel = 'button,a,input,textarea,select,[role=button],[role=link],'
            + '[role=textbox],[role=combobox],[role=menuitem],[role=tab],'
            + '[role=option],[role=treeitem],'
            + '[contenteditable=true]';
  window.__ocv = [...document.querySelectorAll(sel)].filter(e => e.getClientRects().length);
  return JSON.stringify(window.__ocv.map((e, id) => {
    const r = e.getBoundingClientRect();
    return {
      text: (e.innerText || e.getAttribute('aria-label') || e.placeholder
             || e.value || e.title || '').trim().slice(0, 60),
      role: e.getAttribute('role') || e.tagName.toLowerCase(),
      x: r.x + r.width / 2, y: r.y + r.height / 2, id
    };
  }));
})()"#;

/// One page, held open for as long as a command needs it.
pub struct Page {
    socket: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Page {
    /// Connect to a tab's debugger. `ws_url` comes from `/json/list`.
    pub fn connect(ws_url: &str) -> Result<Self> {
        let (socket, _) = connect(ws_url).context("opening the page's debugger socket")?;
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = socket.get_ref() {
            // A page that never answers must not hold a voice command open.
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
        }
        Ok(Page { socket, next_id: 1 })
    }

    /// Evaluate an expression in the page and return its value.
    fn eval(&mut self, expression: &str) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "id": id,
            "method": "Runtime.evaluate",
            "params": { "expression": expression, "returnByValue": true, "awaitPromise": true }
        });
        self.socket
            .send(Message::Text(request.to_string().into()))?;
        // CDP interleaves unsolicited events with replies; ours is the one
        // carrying our id, and anything else on the wire is not an error.
        for _ in 0..64 {
            let Message::Text(text) = self.socket.read()? else {
                continue;
            };
            let value: Value = serde_json::from_str(&text)?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = value.get("error") {
                return Err(anyhow!("devtools refused: {err}"));
            }
            if let Some(thrown) = value.pointer("/result/exceptionDetails/text") {
                return Err(anyhow!("the page threw: {thrown}"));
            }
            return value
                .pointer("/result/result/value")
                .cloned()
                .ok_or_else(|| anyhow!("no value in reply"));
        }
        Err(anyhow!("no reply from the page"))
    }

    /// Everything on the page that can be clicked or typed into.
    pub fn controls(&mut self) -> Result<Vec<Control>> {
        let raw = self.eval(SURVEY)?;
        let text = raw
            .as_str()
            .ok_or_else(|| anyhow!("survey returned no JSON"))?;
        Ok(serde_json::from_str(text)?)
    }

    /// Click a control by its handle, in the page rather than with the real
    /// pointer. The cursor does not move and nothing else can land under it
    /// between the decision and the click.
    pub fn click(&mut self, control: &Control) -> Result<()> {
        self.act(control, "el.click()")
    }

    /// Put the caret in a control. For an input this is what was actually
    /// wanted; clicking it is a coordinate-shaped way of asking for the same
    /// thing, with more ways to miss.
    pub fn focus(&mut self, control: &Control) -> Result<()> {
        self.act(control, "el.focus()")
    }

    fn act(&mut self, control: &Control, body: &str) -> Result<()> {
        // Re-checked in the page: the survey may be a second old, and a
        // handle that no longer resolves must fail loudly rather than act on
        // whatever took its index.
        let script = format!(
            "(() => {{ const el = (window.__ocv||[])[{}];
               if (!el || !el.isConnected) return 'gone';
               el.scrollIntoView({{block:'center'}}); {body}; return 'ok'; }})()",
            control.id
        );
        match self.eval(&script)?.as_str() {
            Some("ok") => Ok(()),
            Some(other) => Err(anyhow!("the control is {other}")),
            None => Err(anyhow!("the page gave no answer")),
        }
    }
}

/// A control's label is whatever the page happens to say, so it earns the
/// same bar `target::resolve` gives window titles rather than the command
/// threshold. Measured: at 0.82, "search" also matched *"Assign an archive or
/// retention policy to automatically archive"*, which shares no word with it.
const LABEL_THRESHOLD: f64 = 0.90;
/// Tokens shorter than this score high against everything and mean nothing.
const MIN_TOKEN_LEN: usize = 3;

/// Every control whose label matches what was said, best first.
///
/// Returns all of them on purpose. One page had `reply` eight times, and a
/// resolver that silently picks the first is how "vai pro terminal" ended up
/// opening whichever Alacritty the compositor happened to list first.
pub fn resolve<'a>(spoken: &str, controls: &'a [Control], threshold: f64) -> Vec<&'a Control> {
    let bar = LABEL_THRESHOLD.max(threshold);
    let spoken_norm = matcher::normalize(spoken);
    if spoken_norm.len() < MIN_TOKEN_LEN {
        return Vec::new();
    }
    let mut scored: Vec<(&Control, f64)> = controls
        .iter()
        .filter(|c| c.speakable())
        .filter_map(|c| {
            let score = matcher::normalize(&c.text)
                .split_whitespace()
                .filter(|w| w.len() >= MIN_TOKEN_LEN)
                .map(|w| strsim::jaro_winkler(&spoken_norm, w))
                .fold(0.0_f64, f64::max);
            (score >= bar).then_some((c, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    debug!(spoken, found = scored.len(), "page controls matched");
    scored.into_iter().map(|(c, _)| c).collect()
}

/// The tab the person is looking at: the one whose title the focused window
/// carries. A browser is one window to the compositor, so this is the only
/// bridge back from "the window I am in" to "the page I am on".
pub fn focused_tab(runner: &Arc<dyn CommandRunner>, port: u16) -> Option<super::tabs::Tab> {
    use crate::wm::backend::WmBackend;
    let title =
        matcher::normalize(&crate::wm::backend::Hyprctl::new(runner.clone()).focused_title()?);
    super::tabs::live_tabs(runner, port)
        .into_iter()
        .filter(|t| !t.debugger.is_empty())
        .find(|t| {
            let tab = matcher::normalize(&t.title);
            !tab.is_empty() && title.contains(&tab)
        })
}

/// Act on a named control in the focused page. `Some(true)` when something
/// happened, `Some(false)` when this was a page command that could not be
/// carried out, `None` when it was never one.
pub fn act_on_focused(
    action: &str,
    spoken: Option<&str>,
    config: &crate::config::Config,
    runner: &Arc<dyn CommandRunner>,
    tx: &crossbeam_channel::Sender<crate::TranscriptEvent>,
) -> Option<bool> {
    let want_click = match action {
        "click_text" => true,
        "focus_field" => false,
        _ => return None,
    };
    let spoken = spoken?;
    let port = config.browser_port()?;
    if runner.dry_run() {
        emit(
            tx,
            crate::TranscriptEvent::notice(format!("[{action} {spoken:?} — não executado]")),
        );
        return Some(true);
    }
    let Some(tab) = focused_tab(runner, port) else {
        debug!("no debuggable page in the focused window");
        return Some(false);
    };
    let mut page = match Page::connect(&tab.debugger) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = ?e, "could not reach the page");
            return Some(false);
        }
    };
    let controls = page.controls().unwrap_or_default();
    let hits = resolve(spoken, &controls, config.threshold());

    // One candidate or nothing. A page had `reply` three times, and there is
    // no undo for clicking the wrong one — nor any way to see that it was
    // wrong. Naming the count is more useful than a guess.
    match hits.len() {
        0 => {
            emit(tx, crate::TranscriptEvent::notice(format!("[{action}: 0]")));
            Some(false)
        }
        1 => {
            let target = hits[0].clone();
            let done = if want_click {
                page.click(&target)
            } else {
                page.focus(&target)
            };
            match done {
                Ok(()) => {
                    info!(action, text = %target.text, "acted on a page control");
                    emit(
                        tx,
                        crate::TranscriptEvent::notice(format!("[{action} {:?}]", target.text)),
                    );
                    Some(true)
                }
                Err(e) => {
                    debug!(error = ?e, "the page refused");
                    Some(false)
                }
            }
        }
        n => {
            let names: Vec<&str> = hits.iter().take(4).map(|c| c.text.as_str()).collect();
            emit(
                tx,
                crate::TranscriptEvent::notice(format!("[{action}: {n} — {names:?}]")),
            );
            Some(false)
        }
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
