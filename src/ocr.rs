//! Reading text off the screen — groundwork for M7.6.
//!
//! Whisper is far better at the *words* than a meeting client's own captions,
//! so the client is only ever asked for the one thing it knows and we cannot
//! infer: **who is speaking**. It knows because it receives a separate stream
//! per participant and mixes them for playback; what reaches the sink monitor
//! is the mix, and no amount of embedding maths recovers what was summed away.
//!
//! `grim` grabs a region, `tesseract` reads it. Both are shelled out to rather
//! than linked: the last C++ library added to this binary (CTranslate2)
//! collided with onnxruntime's protobuf symbols, and leptonica would be
//! another chance at the same bug for no benefit.
//!
//! This module deliberately stops at "what does OCR see". Turning a name into
//! an attributed line is M7.6, and it should be built against what a real
//! recap actually renders rather than against a guess.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::process::CommandRunner;
use crate::wm::target::{live_windows, WindowInfo};

/// A screen rectangle in the logical coordinates `grim -g` speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Region {
    pub fn geometry(&self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.w, self.h)
    }

    /// Parse `x,y wxh` — the same spelling `grim -g` and this tool print, so
    /// a region found by dumping a window can be pasted straight back in.
    pub fn parse(text: &str) -> Option<Region> {
        let (origin, size) = text.trim().split_once(char::is_whitespace)?;
        let (x, y) = origin.split_once(',')?;
        let (w, h) = size.split_once('x')?;
        Some(Region {
            x: x.trim().parse().ok()?,
            y: y.trim().parse().ok()?,
            w: w.trim().parse().ok()?,
            h: h.trim().parse().ok()?,
        })
    }

    pub fn of_window(win: &WindowInfo) -> Option<Region> {
        if win.size[0] <= 0 || win.size[1] <= 0 {
            return None;
        }
        Some(Region {
            x: win.at[0],
            y: win.at[1],
            w: win.size[0],
            h: win.size[1],
        })
    }
}

/// One word tesseract read, with where it sat and how sure it was.
#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    /// 0–100. Anything under ~60 is usually an artefact of the theme rather
    /// than a real character.
    pub conf: f32,
    pub line: i64,
    pub left: i64,
    pub top: i64,
}

fn scratch_png() -> PathBuf {
    std::env::temp_dir().join("oc-voice-ocr.png")
}

/// Grab a region and read it. Returns the words with positions.
///
/// Two processes and one temporary file rather than a pipe, because
/// `CommandRunner` deliberately exposes only "run this and give me the
/// output" — the indirection that makes every external call testable.
pub fn read(runner: &Arc<dyn CommandRunner>, region: Region, lang: &str) -> Result<Vec<Word>> {
    let png = scratch_png();
    let path = png.to_string_lossy().to_string();
    let out = runner
        .output("grim", &["-g", &region.geometry(), &path])
        .context("running grim — is it installed?")?;
    if !out.status.success() {
        return Err(anyhow!(
            "grim failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // `grim -g` takes logical pixels but writes physical ones, so on a scaled
    // monitor every OCR coordinate comes back magnified. Measuring the scale
    // from the file it just wrote beats asking hyprctl for the monitor's
    // scale: it is the actual ratio, rounding included.
    let scale = std::fs::read(&png)
        .ok()
        .and_then(|bytes| png_width(&bytes))
        .map(|w| w as f64 / region.w.max(1) as f64)
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);

    let out = runner
        .output("tesseract", &[&path, "stdout", "-l", lang, "tsv"])
        .context("running tesseract — is it installed?")?;
    if !out.status.success() {
        return Err(anyhow!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout), scale))
}

/// Width from a PNG's IHDR: 8-byte signature, 4-byte length, "IHDR", then the
/// width as a big-endian u32.
fn png_width(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some(u32::from_be_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19],
    ]))
}

/// Tesseract's TSV: level page block par line word left top width height conf text
///
/// `scale` divides the positions back into logical pixels, the units every
/// caller and `grim -g` itself speak.
fn parse_tsv(tsv: &str, scale: f64) -> Vec<Word> {
    let to_logical = |raw: &str| -> i64 {
        let px: f64 = raw.parse().unwrap_or(0.0);
        (px / scale).round() as i64
    };
    tsv.lines()
        .skip(1) // header
        .filter_map(|row| {
            let cols: Vec<&str> = row.split('\t').collect();
            if cols.len() < 12 {
                return None;
            }
            let text = cols[11].trim();
            if text.is_empty() {
                return None;
            }
            Some(Word {
                text: text.to_string(),
                conf: cols[10].parse().unwrap_or(-1.0),
                line: cols[4].parse().unwrap_or(0),
                left: to_logical(cols[6]),
                top: to_logical(cols[7]),
            })
        })
        .collect()
}

/// Words regrouped into lines, with the line's position and worst confidence.
///
/// Worst rather than mean on purpose: one unreadable word in a name makes the
/// whole name untrustworthy, and averaging hides exactly that.
pub fn lines(words: &[Word]) -> Vec<(i64, i64, String, f32)> {
    let mut out: Vec<(i64, i64, String, f32)> = Vec::new();
    let mut current_line = i64::MIN;
    for w in words {
        // Tesseract's own line number, plus vertical proximity: line numbers
        // restart per block, so two blocks can both end and begin at line 1,
        // and words sitting 40 px apart are not one line whatever it says.
        let same_line = w.line == current_line
            && out
                .last()
                .is_some_and(|last: &(i64, i64, String, f32)| (w.top - last.0).abs() <= 4);
        match out.last_mut() {
            Some(last) if same_line => {
                last.2.push(' ');
                last.2.push_str(&w.text);
                last.3 = last.3.min(w.conf);
            }
            _ => {
                current_line = w.line;
                out.push((w.top, w.left, w.text.clone(), w.conf));
            }
        }
    }
    out
}

/// Resolve a window the same way a spoken target is resolved, then take its
/// geometry. Reusing the resolver means "teams" finds the meeting window with
/// the same fuzzy matching that "envia para o navegador" uses.
pub fn window_region(runner: &Arc<dyn CommandRunner>, target: &str) -> Result<Region> {
    let windows = live_windows(runner);
    if windows.is_empty() {
        return Err(anyhow!("no windows — is hyprctl reachable?"));
    }
    let categories = std::collections::HashMap::new();
    let resolved = crate::wm::target::resolve(
        target,
        &categories,
        &windows,
        crate::commands::matcher::DEFAULT_THRESHOLD,
    )
    .ok_or_else(|| {
        anyhow!(
            "no window matched {target:?}; open ones are: {}",
            windows
                .iter()
                .map(|w| w.class.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let win = windows
        .iter()
        .find(|w| w.address == resolved.address)
        .ok_or_else(|| anyhow!("window vanished between resolving and measuring"))?;
    Region::of_window(win).ok_or_else(|| anyhow!("{} reports no size", resolved.class))
}

#[cfg(test)]
#[path = "ocr_tests.rs"]
mod tests;
