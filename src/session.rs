//! Recorded sessions — M7.1.
//!
//! A session accumulates every finalized utterance with a timestamp relative
//! to its start, and writes a Markdown file when it closes.
//!
//! **The file holds the original transcription, in whatever language was
//! spoken.** The translation of M7.2 exists only on screen: a wrong subtitle
//! costs half a second of confusion, a wrong minute is a falsified record
//! that nobody can audit afterwards because the original is gone.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use tracing::info;

pub struct Session {
    started: Instant,
    started_at: DateTime<Local>,
    source: &'static str,
    lines: Vec<Utterance>,
}

struct Utterance {
    at: Duration,
    /// Who spoke: the microphone, or the meeting. Not a name — the record
    /// says what it knows and no more. Telling the two apart is what M7.4
    /// extends, and even then an unrecognised voice stays unnamed.
    speaker: &'static str,
    text: String,
}

impl Session {
    pub fn start(source: &'static str) -> Self {
        info!(source, "session started");
        Session {
            started: Instant::now(),
            started_at: Local::now(),
            source,
            lines: Vec::new(),
        }
    }

    /// Record one finalized utterance, attributed to whoever produced it.
    pub fn push(&mut self, text: &str, speaker: &'static str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.lines.push(Utterance {
            at: self.started.elapsed(),
            speaker,
            text: text.to_string(),
        });
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The Markdown body. Separate from writing so it can be tested without
    /// touching the filesystem.
    pub fn to_markdown(&self) -> String {
        let end = self.started_at + chrono::Duration::from_std(self.elapsed()).unwrap_or_default();
        let mut out = String::new();
        let _ = writeln!(out, "# Sessão {}", self.started_at.format("%Y-%m-%d %H:%M"));
        let _ = writeln!(out);
        let _ = writeln!(out, "| | |");
        let _ = writeln!(out, "|---|---|");
        let _ = writeln!(out, "| Início | {} |", self.started_at.format("%H:%M:%S"));
        let _ = writeln!(out, "| Fim | {} |", end.format("%H:%M:%S"));
        let _ = writeln!(out, "| Duração | {} |", hms(self.elapsed()));
        let _ = writeln!(out, "| Iniciado por | {} |", self.source);
        let _ = writeln!(out, "| Falas | {} |", self.lines.len());
        for speaker in self.speakers() {
            let n = self.lines.iter().filter(|l| l.speaker == speaker).count();
            let _ = writeln!(out, "| — {speaker} | {n} |");
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "> Transcrição original, sem tradução. Ver M7.2 no BACKLOG."
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "## Transcrição");
        let _ = writeln!(out);
        for line in &self.lines {
            let _ = writeln!(out, "**{}** `{}` {}", hms(line.at), line.speaker, line.text);
            let _ = writeln!(out);
        }
        out
    }

    /// Speakers in the order they first appear.
    fn speakers(&self) -> Vec<&'static str> {
        let mut seen: Vec<&'static str> = Vec::new();
        for line in &self.lines {
            if !seen.contains(&line.speaker) {
                seen.push(line.speaker);
            }
        }
        seen
    }

    /// Write the session and return where it landed.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!(
            "{}.md",
            self.started_at.format("%Y-%m-%d_%H-%M-%S")
        ));
        std::fs::write(&path, self.to_markdown())
            .with_context(|| format!("writing {}", path.display()))?;
        info!(path = %path.display(), lines = self.lines.len(), "session written");
        Ok(path)
    }
}

fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// `~/.local/share/oc-voice/sessions`, honouring XDG_DATA_HOME.
pub fn default_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oc-voice")
        .join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_carries_the_originals_with_timestamps() {
        let mut s = Session::start("reunião");
        s.push("Yeah, sure, yeah.", "reunião");
        s.push("  ", "reunião"); // whitespace-only never becomes a line
        s.push("Okay, do it now.", "reunião");
        let md = s.to_markdown();

        assert!(md.contains("Yeah, sure, yeah."));
        assert!(md.contains("Okay, do it now."));
        assert_eq!(s.line_count(), 2, "empty utterance must not be recorded");
        assert!(md.contains("| Iniciado por | reunião |"));
        // Every line is timestamped.
        assert_eq!(md.matches("**00:00:").count(), 2);
    }

    #[test]
    fn both_sides_of_the_conversation_are_recorded_and_told_apart() {
        // Capturing only system audio produced a record of everything said in
        // a meeting except by the person keeping the record.
        let mut s = Session::start("reunião");
        s.push("They should have a parent.", "reunião");
        s.push("Concordo, faz sentido.", "você");
        s.push("Right, let's do that.", "reunião");
        let md = s.to_markdown();

        assert!(
            md.contains("Concordo, faz sentido."),
            "your own words: {md}"
        );
        assert!(md.contains("`você`"), "and attributed to you");
        assert!(md.contains("`reunião`"));
        // The header counts each side, so a silent participant is visible as
        // a number rather than by scrolling the whole transcript.
        assert!(md.contains("| — reunião | 2 |"), "{md}");
        assert!(md.contains("| — você | 1 |"), "{md}");
    }

    #[test]
    fn the_record_is_never_the_translation() {
        // The whole point of M7.2's split: a session must contain what was
        // said, not a machine's rendering of it.
        let mut s = Session::start("reunião");
        s.push("They should have a parent.", "reunião");
        let md = s.to_markdown();
        assert!(md.contains("They should have a parent."));
        assert!(!md.contains("Eles devem ter um pai"));
        assert!(md.contains("sem tradução"));
    }

    #[test]
    fn duration_formats_past_an_hour() {
        assert_eq!(hms(Duration::from_secs(0)), "00:00:00");
        assert_eq!(hms(Duration::from_secs(59)), "00:00:59");
        assert_eq!(hms(Duration::from_secs(3661)), "01:01:01");
        assert_eq!(hms(Duration::from_secs(7325)), "02:02:05");
    }
}
