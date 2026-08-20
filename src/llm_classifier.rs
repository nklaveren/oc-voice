use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[allow(dead_code)]
const LLAMA_PORT: u16 = 17432;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum VoiceCommand {
    #[serde(rename = "send")]
    Send,
    #[serde(rename = "cancel")]
    Cancel,
    #[serde(rename = "newline")]
    Newline,
    #[serde(rename = "send_to")]
    SendTo { target: String },
    #[serde(rename = "dictation")]
    Dictation,
}

pub struct LlmClassifier {
    server: Option<Child>,
}

#[allow(dead_code)]
impl LlmClassifier {
    pub fn start(models_dir: &str) -> Result<Self> {
        let model_path = format!("{models_dir}/Qwen3.5-0.8B-Q4_K_M.gguf");
        if !std::path::Path::new(&model_path).exists() {
            anyhow::bail!("LLM model not found at {model_path}. Run `just fetch-llm` first.");
        }

        info!(path = %model_path, "starting llama-server for command classification");

        let child = Command::new("llama-server")
            .args([
                "-m",
                &model_path,
                "--port",
                &LLAMA_PORT.to_string(),
                "-ngl",
                "99",
                "-c",
                "512",
                "--no-warmup",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn llama-server. Is llama-cpp installed?")?;

        let pid = child.id();
        info!(pid, "llama-server spawned, waiting for ready...");

        let classifier = Self {
            server: Some(child),
        };

        classifier.wait_for_ready()?;
        Ok(classifier)
    }

    fn wait_for_ready(&self) -> Result<()> {
        let start = Instant::now();
        let url = format!("http://127.0.0.1:{LLAMA_PORT}/health");
        loop {
            if let Ok(resp) = ureq::get(&url).call() {
                if resp.status() == 200 {
                    let elapsed = start.elapsed();
                    info!(
                        elapsed_ms = elapsed.as_millis() as u64,
                        "llama-server ready"
                    );
                    return Ok(());
                }
            }
            if start.elapsed() > Duration::from_secs(60) {
                anyhow::bail!("llama-server did not become ready within 60s");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn classify(&self, text: &str) -> Option<VoiceCommand> {
        let prompt = format!(
            r#"You are a voice command classifier for a dictation app. The user speaks Portuguese.
Classify the following transcribed text into exactly one action. Respond ONLY with valid JSON.

Possible actions:
- {{"action":"send"}} — user wants to send/submit the buffered text (keywords: envia, enviar, manda, mandar, pronto, câmbio)
- {{"action":"cancel"}} — user wants to clear/discard the buffer (keywords: cancela, cancelar, limpa, limpar, descarta)
- {{"action":"newline"}} — user wants a line break (keywords: nova linha, pula linha, enter)
- {{"action":"send_to","target":"<app>"}} — user wants to send text to a specific app. Target aliases: "navegador"→"firefox", "opencode"→"oc-opencode", "terminal"→"Alacritty" or "kitty", "editor"→"code", "chat"→"discord" or "telegram"
- {{"action":"dictation"}} — normal speech, not a command

IMPORTANT: Only classify as a command if the text is SHORT (≤5 words) and clearly a command, not part of normal speech.
Text: "{text}"

JSON:"#
        );

        let url = format!("http://127.0.0.1:{LLAMA_PORT}/v1/chat/completions");

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 100,
            "temperature": 0.0,
        });

        let start = Instant::now();
        let agent = ureq::Agent::new_with_defaults();
        let mut resp = match agent.post(&url).send_json(&body) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "LLM request failed");
                return None;
            }
        };

        let resp_body: serde_json::Value = match resp.body_mut().read_json() {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "LLM response parse failed");
                return None;
            }
        };

        let elapsed = start.elapsed();

        let usage = resp_body.get("usage");
        let prompt_tokens = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let completion_tokens = usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let total_tokens = prompt_tokens + completion_tokens;

        info!(
            elapsed_ms = elapsed.as_millis() as u64,
            prompt_tokens, completion_tokens, total_tokens, "LLM classified"
        );

        let content = resp_body
            .get("choices")?
            .get(0)?
            .get("message")?
            .get("content")?
            .as_str()?;

        let content = content.trim();

        let json_str = extract_json(content)?;

        match serde_json::from_str::<VoiceCommand>(json_str) {
            Ok(cmd) => {
                info!(?cmd, text = %text, "LLM classified command");
                Some(cmd)
            }
            Err(e) => {
                warn!(error = %e, raw = %content, "LLM response was not valid VoiceCommand JSON");
                None
            }
        }
    }

    pub fn classify_with_fallback(text: &str) -> Option<VoiceCommand> {
        info!(text = %text, "keyword fallback classification");
        let lower = fold_diacritics(&text.to_lowercase());
        let words: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let normalized = words.join(" ");

        if words.len() > 5 {
            return Some(VoiceCommand::Dictation);
        }

        let send_keywords = ["envia", "enviar", "manda", "mandar", "cambio", "pronto"];
        let cancel_keywords = ["cancela", "cancelar", "limpa", "limpar", "descarta"];
        let newline_keywords = ["nova linha", "pula linha", "newline", "enter"];

        let text_lower = normalized.as_str();

        if send_keywords.contains(&text_lower) {
            return Some(VoiceCommand::Send);
        }
        if cancel_keywords.contains(&text_lower) {
            return Some(VoiceCommand::Cancel);
        }
        if newline_keywords.contains(&text_lower) {
            return Some(VoiceCommand::Newline);
        }

        let send_to_prefixes = [
            "enviar para ",
            "envia para ",
            "enviar pelo ",
            "envia pelo ",
            "enviar via ",
            "envia via ",
            "enviar pro ",
            "envia pro ",
            "manda para ",
            "manda pelo ",
            "manda via ",
            "manda pro ",
        ];
        for prefix in &send_to_prefixes {
            if let Some(pos) = text_lower.find(prefix) {
                let target = text_lower[pos + prefix.len()..].trim();
                if !target.is_empty() {
                    let resolved = resolve_target_alias(target);
                    return Some(VoiceCommand::SendTo {
                        target: resolved.to_string(),
                    });
                }
            }
        }

        Some(VoiceCommand::Dictation)
    }
}

#[allow(dead_code)]
fn extract_json(text: &str) -> Option<&str> {
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if start < end {
                return Some(&text[start..=end]);
            }
        }
    }
    None
}

/// Whisper transcribes Portuguese with accents ("câmbio"), but the keyword
/// tables below are written unaccented. Fold the diacritics pt-BR actually
/// uses so both spellings land on the same entry.
fn fold_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

fn resolve_target_alias(target: &str) -> &str {
    match target {
        "navegador" | "firefox" | "browser" => "firefox",
        "chrome" | "google chrome" | "chromium" => "chromium",
        "opencode" | "open code" | "oc" => "oc-opencode",
        "terminal" | "term" => "Alacritty",
        "editor" | "vscode" | "code" | "vs code" => "code",
        "discord" | "chat" => "discord",
        "telegram" => "telegram",
        "whatsapp" => "whatsapp-nativefier",
        _ => target,
    }
}

impl Drop for LlmClassifier {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.server {
            let _ = child.kill();
            let _ = child.wait();
            info!("llama-server stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str) -> Option<VoiceCommand> {
        LlmClassifier::classify_with_fallback(text)
    }

    #[test]
    fn accented_send_keyword_matches() {
        // The regression: whisper emits "câmbio", the table says "cambio".
        assert_eq!(classify("câmbio"), Some(VoiceCommand::Send));
        assert_eq!(classify("cambio"), Some(VoiceCommand::Send));
        assert_eq!(classify("Câmbio."), Some(VoiceCommand::Send));
    }

    #[test]
    fn other_commands_still_match() {
        assert_eq!(classify("envia"), Some(VoiceCommand::Send));
        assert_eq!(classify("cancela"), Some(VoiceCommand::Cancel));
        assert_eq!(classify("nova linha"), Some(VoiceCommand::Newline));
        assert_eq!(
            classify("envia para navegador"),
            Some(VoiceCommand::SendTo {
                target: "firefox".to_string()
            })
        );
    }

    #[test]
    fn long_speech_is_dictation() {
        assert_eq!(
            classify("isso aqui e uma frase normal de ditado qualquer"),
            Some(VoiceCommand::Dictation)
        );
    }
}
