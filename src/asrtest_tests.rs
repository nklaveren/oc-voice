//! Tests for the read-aloud ASR benchmark: passage structure, number
//! normalization, and the WER metric itself.

use super::*;

#[test]
fn reference_passage_parses_into_blocks() {
    let blocks = parse_blocks();
    assert!(
        blocks.len() >= 6,
        "esperados 6+ blocos, veio {}",
        blocks.len()
    );
    assert!(blocks.iter().all(|b| !b.lines.is_empty()));
    // The command vocabulary must actually appear in the passage,
    // otherwise the test measures something the app never has to hear.
    let all = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let spoken = words(&all);
    let config = crate::config::Config::embedded();
    let vocab = config.vocab("pt").unwrap();
    for word in vocab.send.iter().chain(vocab.cancel.iter()) {
        let w = words(word);
        assert!(
            w.iter().all(|t| spoken.contains(t)),
            "vocabulário {word:?} não aparece na passagem"
        );
    }
}

#[test]
fn spoken_numbers_compare_equal_to_digits() {
    // These exact pairs made the first numbers-block measurement
    // meaningless: whisper writes digits, the reference spells them out.
    for (spoken, written) in [
        ("um dois tres quatro cinco", "1 2 3 4 5"),
        ("A porta tres mil e cem", "A porta 3100"),
        ("O erro quatrocentos e quatro", "O erro 404"),
        ("vinte e sete por cento", "27 por cento"),
        ("Versao dois ponto quatorze ponto zero", "Versao 2.14.0"),
        ("dezoito pessoas", "18 pessoas"),
        ("cinquenta e tres", "53"),
    ] {
        assert_eq!(
            words(spoken),
            words(written),
            "{spoken:?} deveria comparar igual a {written:?}"
        );
    }
}

#[test]
fn plain_words_are_untouched_by_number_folding() {
    // "um" is a number word but also an article; folding must not eat
    // ordinary prose.
    assert_eq!(words("abre um arquivo"), vec!["abre", "1", "arquivo"]);
    assert_eq!(words("o gato subiu"), vec!["o", "gato", "subiu"]);
}

#[test]
fn wer_counts_substitutions_insertions_and_deletions() {
    let r = words("o gato subiu no telhado");
    assert_eq!(wer(&r, &words("o gato subiu no telhado")).0, 0);
    assert_eq!(wer(&r, &words("o rato subiu no telhado")).0, 1);
    assert_eq!(wer(&r, &words("o gato subiu telhado")).0, 1);
    assert_eq!(wer(&r, &words("o gato subiu logo no telhado")).0, 1);
}
