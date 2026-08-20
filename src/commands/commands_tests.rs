//! Tests for command classification: the word-count gate, the prefix,
//! filler stripping, and the help text built from the vocabulary.

use super::*;

/// All command tests speak the embedded Portuguese vocabulary.
fn classify(text: &str) -> Option<VoiceCommand> {
    let config = crate::config::Config::embedded();
    let vocab = config.vocab("pt").expect("embedded pt vocab");
    super::classify(text, vocab, config.threshold())
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
        // M2.1: the target stays as spoken; resolution against live
        // windows happens at execution time, not at classify time.
        Some(VoiceCommand::SendTo {
            target: "navegador".to_string()
        })
    );
}

#[test]
fn asr_variants_classify_through_the_public_api() {
    // M1.2: the M1.1 measurements hold through `classify`, not just the
    // matcher's own unit tests.
    for spoken in ["sambio", "cambiu", "kambio", "quambio", "cambrio"] {
        assert_eq!(classify(spoken), Some(VoiceCommand::Send), "{spoken}");
    }
    assert_eq!(classify("cancelar"), Some(VoiceCommand::Cancel));
    assert_eq!(classify("nova linia"), Some(VoiceCommand::Newline));
    for spoken in ["pronto falei", "sao paulo", "bom dia"] {
        assert_eq!(classify(spoken), Some(VoiceCommand::Dictation), "{spoken}");
    }
}

#[test]
fn prefix_marks_commands_when_required() {
    // M4.1, both directions: with require_prefix on, a bare command word
    // is literal dictation, and the prefixed form is a command.
    let config = crate::config::Config::embedded();
    let mut vocab = config.vocab("pt").unwrap().clone();
    vocab.require_prefix = true;
    let t = config.threshold();
    assert_eq!(
        super::classify("câmbio", &vocab, t),
        Some(VoiceCommand::Dictation),
        "bare command word must dictate literally"
    );
    assert_eq!(
        super::classify("computador, câmbio", &vocab, t),
        Some(VoiceCommand::Send)
    );
    // ASR error on the prefix itself still counts.
    assert_eq!(
        super::classify("comptador câmbio", &vocab, t),
        Some(VoiceCommand::Send)
    );
}

#[test]
fn prefix_is_optional_by_default() {
    // Default config keeps today's behaviour: bare commands work, and the
    // prefixed form works too.
    assert_eq!(classify("câmbio"), Some(VoiceCommand::Send));
    assert_eq!(classify("computador câmbio"), Some(VoiceCommand::Send));
}

#[test]
fn long_speech_is_dictation() {
    assert_eq!(
        classify("isso aqui e uma frase normal de ditado qualquer"),
        Some(VoiceCommand::Dictation)
    );
}

#[test]
fn a_command_wrapped_in_filler_still_reaches_the_matcher() {
    // Reported live as "only câmbio works". It was not special — it was
    // the only word being said alone. Measured with `oc-voice probe`:
    // "ok câmbio" and "limpar tudo" were refused by the word-count gate
    // before anything was scored, because no candidate has two words.

    assert!(matches!(classify("ok câmbio"), Some(VoiceCommand::Send)));
    assert!(matches!(
        classify("limpar tudo"),
        Some(VoiceCommand::Cancel)
    ));
    assert!(matches!(
        classify("cancela isso"),
        Some(VoiceCommand::Cancel)
    ));
}

#[test]
fn dictation_containing_a_command_word_is_still_dictation() {
    // The property the word-count gate exists for, and the one the filler
    // list must not cost. Only words the vocabulary NAMES as filler come
    // off; anything unknown keeps the utterance long, and long means
    // dictation. Drop that rule and every sentence mentioning "limpar"
    // silently deletes what the user was writing.

    for spoken in [
        "vamos limpar depois",
        "limpa a casa toda",
        "manda ver no projeto novo",
        "cancela a reunião de amanhã",
    ] {
        let cmd = classify(spoken);
        assert!(
            matches!(cmd, Some(VoiceCommand::Dictation) | None),
            "{spoken:?} became {cmd:?} instead of dictation"
        );
    }
}

#[test]
fn an_utterance_of_pure_filler_is_not_a_command() {
    // Stripping everything would leave an empty string, and an empty
    // string scores oddly against short candidates.
    let cmd = classify("ok então");
    assert!(
        matches!(cmd, Some(VoiceCommand::Dictation) | None),
        "got {cmd:?}"
    );
}

#[test]
fn help_is_built_from_the_vocabulary_it_describes() {
    // A hand-written list next to the config it documents goes stale the
    // first time someone edits one and not the other, and help that lies
    // is worse than none because it is believed.
    let config = crate::config::Config::embedded();
    for lang in ["pt", "en"] {
        let vocab = config.vocab(lang).unwrap();
        let lines = help_lines(vocab);
        assert!(!lines.is_empty(), "{lang} produced no help");
        let joined = lines.join("\n");
        for word in vocab.send.iter().take(1) {
            assert!(
                joined.contains(word.as_str()),
                "{lang} help omits its own send word {word:?}"
            );
        }
    }
}

#[test]
fn navigation_is_never_mistaken_for_a_mode_switch() {
    // Caught in a live log, not by review: "Monitor direito." switched the
    // mode to Input mid-navigation. "modo ditado" and "monitor direito" share
    // an opening and Jaro-Winkler pays a prefix bonus, so they scored 0.84 —
    // over the 0.82 command threshold. Switching mode throws away the grammar
    // the next utterance will be read with, so it earns a near-exact bar.
    for spoken in [
        "monitor direito",
        "monitor esquerda",
        "monitor da direita",
        "modo de trabalho",
    ] {
        let cmd = classify(spoken);
        assert!(
            !matches!(cmd, Some(VoiceCommand::SetMode(_))),
            "{spoken:?} became {cmd:?}"
        );
    }
    // And the real phrases still switch.
    assert!(matches!(
        classify("modo comando"),
        Some(VoiceCommand::SetMode(ref m)) if m == "command"
    ));
    assert!(matches!(
        classify("modo ditado"),
        Some(VoiceCommand::SetMode(ref m)) if m == "input"
    ));
}

#[test]
fn the_mode_bar_is_higher_than_the_command_bar() {
    // The property, not the number: if these ever converge the collision
    // above comes back silently. Compared against the *configured* threshold,
    // which is what classify actually uses — a user who raises it in
    // commands.toml past the mode bar would reopen the same hole.
    let configured = crate::config::Config::embedded().threshold();
    assert!(
        super::MODE_THRESHOLD > configured,
        "mode bar {} must stay above the command bar {configured}",
        super::MODE_THRESHOLD
    );
}
