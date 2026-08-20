//! Similarity matcher for voice commands — M1.1 in BACKLOG.md.
//!
//! Given a spoken utterance and a closed pool of candidates, return the best
//! match above a threshold, or nothing. The ability to *refuse* is the core
//! requirement: a wrong match types into the wrong window, so `None` is the
//! safety mechanism.
//!
//! Pipeline, in order: normalize (lowercase, diacritic fold, punctuation
//! removal, plus a `qu`/`c`→`k` collapse covering the most common Portuguese
//! acoustic confusion), filter candidates by word count, score with
//! Jaro-Winkler. The word-count gate is what separates command from dictation:
//! the Winkler prefix bonus otherwise rewards any dictated sentence that
//! starts with a command word.

use std::collections::HashMap;
use strsim::jaro_winkler;

use super::fold_diacritics;

/// Default acceptance threshold, measured against the real vocabulary — see
/// the 17-case table in M1.1. M1.3 moves this to `commands.toml`.
pub(crate) const DEFAULT_THRESHOLD: f64 = 0.82;

/// Lowercase, fold diacritics, collapse `qu` and `c` to `k`, drop punctuation.
pub(crate) fn normalize(text: &str) -> String {
    fold_diacritics(&text.to_lowercase())
        .replace("qu", "k")
        .replace('c', "k")
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Match the whole utterance against `candidates`. Only candidates with the
/// same word count as the normalized utterance are scored; the highest score
/// at or above `threshold` wins. Returns the candidate and its score.
pub(crate) fn match_exact<'a>(
    spoken: &str,
    candidates: &[&'a str],
    threshold: f64,
) -> Option<(&'a str, f64)> {
    let utterance = normalize(spoken);
    let words = utterance.split_whitespace().count();
    let mut best: Option<(&str, f64)> = None;
    for &candidate in candidates {
        let normalized = normalize(candidate);
        if normalized.split_whitespace().count() != words {
            continue;
        }
        let score = jaro_winkler(&utterance, &normalized);
        if score >= threshold && best.is_none_or(|(_, s)| score > s) {
            best = Some((candidate, score));
        }
    }
    best
}

/// A pattern with slots, e.g. `"monitor da {direcao}"`. Slot tables are the
/// per-language vocabulary (`directions`, `numbers`) of M1.3; M2.1 adds the
/// live-window resolver for `{alvo}`.
// Templates gain their caller in M3.1 (the dispatch module); until then only
// tests exercise them.
#[allow(dead_code)]
pub(crate) struct Template {
    pub(crate) pattern: String,
    pub(crate) slots: HashMap<String, Vec<String>>,
}

impl Template {
    #[allow(dead_code)]
    pub(crate) fn new(pattern: &str, slots: &[(&str, &[&str])]) -> Self {
        Template {
            pattern: pattern.to_string(),
            slots: slots
                .iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        values.iter().map(|v| v.to_string()).collect(),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, PartialEq)]
#[allow(dead_code)]
pub(crate) struct TemplateMatch {
    pub(crate) template_index: usize,
    pub(crate) score: f64,
    pub(crate) slots: HashMap<String, String>,
}

/// Match the utterance against slot patterns: `"monitor da direita"` against
/// `"monitor da {direcao}"`. The word-count gate applies to the filled
/// template, so fixed words and slots line up one-to-one with the spoken
/// words. A template's score is its weakest word.
#[allow(dead_code)]
pub(crate) fn match_template(
    spoken: &str,
    templates: &[Template],
    threshold: f64,
) -> Option<TemplateMatch> {
    let spoken_words: Vec<String> = normalize(spoken)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut best: Option<TemplateMatch> = None;
    for (index, template) in templates.iter().enumerate() {
        let pattern_words: Vec<&str> = template.pattern.split_whitespace().collect();
        if pattern_words.len() != spoken_words.len() {
            continue;
        }
        let mut slots = HashMap::new();
        let mut score = 1.0_f64;
        let mut matched = true;
        for (pattern_word, spoken_word) in pattern_words.iter().zip(&spoken_words) {
            let word_score = match pattern_word
                .strip_prefix('{')
                .and_then(|w| w.strip_suffix('}'))
            {
                Some(slot) => {
                    let Some(options) = template.slots.get(slot) else {
                        matched = false;
                        break;
                    };
                    if options.is_empty() {
                        // Wildcard slot: capture the spoken word as-is; the
                        // dispatcher resolves and validates it (M3.1).
                        slots.insert(slot.to_string(), spoken_word.clone());
                        1.0
                    } else {
                        let refs: Vec<&str> = options.iter().map(String::as_str).collect();
                        match match_exact(spoken_word, &refs, threshold) {
                            Some((value, s)) => {
                                slots.insert(slot.to_string(), value.to_string());
                                s
                            }
                            None => {
                                matched = false;
                                break;
                            }
                        }
                    }
                }
                None => jaro_winkler(&normalize(pattern_word), spoken_word),
            };
            if word_score < threshold {
                matched = false;
                break;
            }
            score = score.min(word_score);
        }
        if matched && best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(TemplateMatch {
                template_index: index,
                score,
                slots,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    // The vocabulary under test mirrors the default tables M1.3 moves to
    // commands.toml — same words the M1.1 measurements ran against.
    const SEND: &[&str] = &["cambio", "envia", "manda", "pronto"];
    const CANCEL: &[&str] = &["cancela", "limpa", "descarta"];
    const NEWLINE: &[&str] = &["nova linha", "pula linha"];

    fn all_commands() -> Vec<&'static str> {
        [SEND, CANCEL, NEWLINE].concat()
    }

    fn templates() -> Vec<Template> {
        let directions: &[&str] = &["direita", "esquerda", "cima", "baixo"];
        let numbers: &[&str] = &["um", "dois", "tres", "quatro"];
        vec![
            Template::new("monitor da {direcao}", &[("direcao", directions)]),
            Template::new("area de trabalho {numero}", &[("numero", numbers)]),
            Template::new("leva pra {numero}", &[("numero", numbers)]),
        ]
    }

    #[test]
    fn normalize_collapses_qu_and_c_to_k() {
        assert_eq!(normalize("Área: quatro"), "area katro");
    }

    #[test]
    fn cambio_variants_match_send() {
        for spoken in ["cambio", "sambio", "cambiu", "kambio", "quambio", "cambrio"] {
            let matched = match_exact(spoken, &all_commands(), DEFAULT_THRESHOLD);
            assert!(
                matches!(matched, Some(("cambio", score)) if score >= DEFAULT_THRESHOLD),
                "{spoken} should match \"cambio\", got {matched:?}"
            );
        }
    }

    #[test]
    fn inflected_variants_match_their_command() {
        for (spoken, expected) in [
            ("enviar", "envia"),
            ("mandar", "manda"),
            ("cancelar", "cancela"),
        ] {
            let matched = match_exact(spoken, &all_commands(), DEFAULT_THRESHOLD);
            assert!(
                matches!(matched, Some((c, _)) if c == expected),
                "{spoken} should match {expected:?}, got {matched:?}"
            );
        }
    }

    #[test]
    fn newline_variants_match_newline() {
        for spoken in ["nova linha", "nova linia"] {
            let matched = match_exact(spoken, &all_commands(), DEFAULT_THRESHOLD);
            assert!(
                matches!(matched, Some(("nova linha", _))),
                "{spoken} should match \"nova linha\", got {matched:?}"
            );
        }
    }

    #[test]
    fn dictation_is_refused_by_the_word_count_gate() {
        for spoken in [
            "limpa a tela toda",
            "manda ver o resultado disso",
            "envia isso pro cliente amanha",
        ] {
            assert_eq!(
                match_exact(spoken, &all_commands(), DEFAULT_THRESHOLD),
                None,
                "{spoken} is dictation and must be refused"
            );
        }
    }

    #[test]
    fn short_dictation_scores_below_threshold() {
        for spoken in ["pronto falei", "sao paulo", "bom dia"] {
            assert_eq!(
                match_exact(spoken, &all_commands(), DEFAULT_THRESHOLD),
                None,
                "{spoken} is dictation and must be refused"
            );
        }
    }

    #[test]
    fn template_phrases_resolve_their_slots() {
        let m = match_template("monitor da direita", &templates(), DEFAULT_THRESHOLD)
            .expect("should match monitor template");
        assert_eq!(m.template_index, 0);
        assert_eq!(m.slots.get("direcao").map(String::as_str), Some("direita"));

        let m = match_template("área de trabalho quatro", &templates(), DEFAULT_THRESHOLD)
            .expect("should match workspace template");
        assert_eq!(m.template_index, 1);
        assert_eq!(m.slots.get("numero").map(String::as_str), Some("quatro"));

        let m = match_template("leva pra três", &templates(), DEFAULT_THRESHOLD)
            .expect("should match move-to-workspace template");
        assert_eq!(m.template_index, 2);
        assert_eq!(m.slots.get("numero").map(String::as_str), Some("tres"));
    }

    #[test]
    fn template_refuses_wrong_word_count() {
        assert_eq!(
            match_template("quero monitor da direita", &templates(), DEFAULT_THRESHOLD),
            None
        );
    }
}
