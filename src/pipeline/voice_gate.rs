//! Who is speaking, between the segmenter and whisper.
//!
//! Split from the transcription loop at the size ceiling, on a line that is
//! genuinely there: up there an utterance is assembled out of frames, and here
//! it is decided whose it is. The decision has to happen before transcription
//! because the embedding costs ~13 ms and whisper costs far more — refusing
//! early is cheaper than refusing late — and because during an enrolment there
//! is nothing to transcribe at all.

use super::*;

/// Put the lock's own view of itself where the UI can read it.
pub(super) fn publish_voice_state(
    settings: &Arc<Mutex<AppSettings>>,
    voice: &crate::voicelock::VoiceLock,
) {
    let state = voice.state();
    let mut s = lock_settings(settings);
    if s.voice_state != state {
        s.voice_state = state;
    }
}

/// Who this segment belongs to.
#[derive(Debug, PartialEq)]
pub(super) enum VoiceCheck {
    /// Nothing to transcribe: it went into an enrolment.
    Swallow,
    /// Yours, or there is no lock and the question does not arise.
    Yours,
    /// Somebody else. Still transcribed, still shown — just not obeyed.
    Someone,
}

/// Ask the lock, and tell the overlay what it said.
///
/// A rejection is reported rather than hidden: the whole job of this feature
/// is to ignore things, and an assistant that ignores you without a word is
/// indistinguishable from one that has crashed.
pub(super) fn voice_check(stream: &Stream, ctx: &mut Ctx<'_>) -> VoiceCheck {
    use crate::voicelock::Verdict;
    if !stream.source.may_command() {
        return VoiceCheck::Yours;
    }
    let verdict = ctx.voice.offer(&stream.segment.samples);
    publish_voice_state(ctx.settings, ctx.voice);
    if let Verdict::Enrolled(segments) = verdict {
        emit(ctx.tx, TranscriptEvent::VoiceLocked(segments));
    }
    if let Verdict::Reject(score) = verdict {
        debug!(score, "not your voice; transcribed but not obeyed");
        emit(ctx.tx, TranscriptEvent::VoiceRejected(score));
    }
    classify(&verdict)
}

/// The verdict, as an instruction. Pure, so the one distinction that matters
/// can be pinned: a rejection is **not** a swallow.
pub(super) fn classify(verdict: &crate::voicelock::Verdict) -> VoiceCheck {
    use crate::voicelock::Verdict;
    match verdict {
        Verdict::Pass | Verdict::Accept(_) => VoiceCheck::Yours,
        Verdict::Enrolling(_) | Verdict::Enrolled(_) => VoiceCheck::Swallow,
        Verdict::Reject(_) => VoiceCheck::Someone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voicelock::Verdict;

    #[test]
    fn a_rejection_is_not_a_swallow() {
        // What this pins, and it is the whole reason `Someone` exists: a
        // rejected utterance used to be dropped, and a bar this new is wrong
        // often enough that dropping made it destroy sentences. It is
        // transcribed and shown now; it just cannot act. The cost of the lock
        // being wrong has to be a keystroke, not a paragraph.
        assert_eq!(classify(&Verdict::Reject(0.4)), VoiceCheck::Someone);
        assert_eq!(classify(&Verdict::Accept(0.9)), VoiceCheck::Yours);
        // With no lock at all nothing changes about anything.
        assert_eq!(classify(&Verdict::Pass), VoiceCheck::Yours);
        // Enrolment is the one case with nothing to transcribe.
        assert_eq!(classify(&Verdict::Enrolling(0.5)), VoiceCheck::Swallow);
        assert_eq!(classify(&Verdict::Enrolled(4)), VoiceCheck::Swallow);
    }
}
