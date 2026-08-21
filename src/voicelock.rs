//! Only your voice drives the machine.
//!
//! This is the easy half of M7.4 and the half that pays. Diarization has to
//! group N unknown voices and put a name on each; a mistake there goes into a
//! written record and stays. This asks one binary question against one stored
//! voice, and its mistake is a command that does not fire — you say it again.
//!
//! M7.5 already split the microphone from the meeting's audio, so what this
//! filters is what leaks into *your* stream: someone else in the room, a
//! television, a video playing on speakers loud enough to come back in.
//!
//! **Short utterances are never rejected.** Below about a second the embedding
//! is unstable, and the words that arrive that short are `sim`, `não`,
//! `câmbio` — the confirmations and the send word, which are the commands you
//! use most. Dropping those to be safe would break the app to protect it. So
//! the lock guards sentences, not interjections, and that is a real limit
//! rather than an oversight.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::fbank;
use crate::voices::{cosine, SpeakerModel};

/// How much speech the enrolment collects before it will build a centroid.
pub const ENROL_SECONDS: f32 = 15.0;
/// Below this an embedding is too unstable to judge — see the module note on
/// why the answer is to let it through rather than to drop it.
pub const MIN_JUDGE_SECONDS: f32 = 1.0;
/// Enrolment is pickier, and can afford to be: judging has to answer about
/// whatever you just said, while enrolling can simply wait for a better
/// stretch. Measured on a 24 s recording of one person: segments of 2.4 s and
/// up sat 0.69–0.86 from their own centre, while those under 2 s scattered
/// across 0.26–0.68. Same voice, same microphone — the short ones carry too
/// little to place anybody.
pub const MIN_ENROL_SECONDS: f32 = 2.0;
/// How far below your own worst measured sample the bar sits. Room for a
/// different chair, a cold, a hand near the microphone.
const MARGIN: f32 = 0.05;

/// What the enrolment measured, and the bar it produced. Plain text on
/// purpose: a person editing this file beats any confidence heuristic, and it
/// is the only defence that still works when the rest is wrong.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    /// The mean embedding of the enrolment, normalised.
    pub centroid: Vec<f32>,
    /// Accept at or above this cosine.
    pub threshold: f32,
    /// How many segments the centroid was built from. Evidence, so a lock
    /// built from three segments can be told from one built from twelve.
    pub segments: usize,
    /// Leave-one-out similarity of your own enrolment samples: the worst and
    /// the average. These are the numbers the threshold came from, kept so it
    /// can be argued with later.
    pub self_worst: f32,
    pub self_mean: f32,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(base.join("oc-voice").join("voice.toml"))
}

impl Store {
    fn load() -> Option<Self> {
        let p = path()?;
        let text = std::fs::read_to_string(&p).ok()?;
        match toml::from_str(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!(path = %p.display(), error = %e, "voice lock did not parse; ignoring it");
                None
            }
        }
    }

    fn store(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                let _ = std::fs::write(&p, text);
                owner_only(&p);
                info!(path = %p.display(), segments = self.segments, "voice lock saved");
            }
            Err(e) => warn!(error = %e, "voice lock could not be serialised"),
        }
    }

    fn forget() {
        if let Some(p) = path() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Readable by its owner and nobody else.
///
/// A voiceprint is not reversible to audio — it is 512 numbers, not a
/// recording — but it *is* a biometric identifier: whoever holds it can test
/// whether a given recording is you. `fs::write` creates 0644, which would
/// leave that open to every account on the machine, and that is the cheapest
/// possible thing to get wrong.
fn owner_only(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// The lock as it sits on disk, for tools that want to compare against it
/// without owning the microphone.
pub fn stored() -> Option<Store> {
    Store::load()
}

/// Speech collected so far, and the embeddings taken from it.
#[derive(Default)]
struct Enrolment {
    seconds: f32,
    embeddings: Vec<Vec<f32>>,
}

/// What the pipeline should do with the segment it just offered.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// No lock, or nothing to judge by. Carry on.
    Pass,
    /// Enrolling: this went into the enrolment, and here is how far along it
    /// is, 0.0 to 1.0.
    Enrolling(f32),
    /// Enrolment finished with this many segments.
    Enrolled(usize),
    /// The lock recognised you.
    Accept(f32),
    /// Not your voice. The score is reported so a wrong rejection can be
    /// argued with rather than guessed at.
    Reject(f32),
}

pub struct VoiceLock {
    /// Loaded on first use. 29 MB of ONNX is not worth paying for at startup
    /// by everyone who never enrols.
    model: Option<SpeakerModel>,
    model_path: PathBuf,
    store: Option<Store>,
    enrolment: Option<Enrolment>,
}

impl VoiceLock {
    pub fn new(model_path: PathBuf) -> Self {
        let store = Store::load();
        if let Some(s) = &store {
            info!(
                segments = s.segments,
                threshold = s.threshold,
                "voice lock active"
            );
        }
        VoiceLock {
            model: None,
            model_path,
            store,
            enrolment: None,
        }
    }

    /// How the UI should draw this.
    pub fn state(&self) -> crate::VoiceState {
        if let Some(e) = &self.enrolment {
            return crate::VoiceState::Enrolling((e.seconds / ENROL_SECONDS).clamp(0.0, 1.0));
        }
        match &self.store {
            Some(s) => crate::VoiceState::On {
                segments: s.segments,
                threshold: s.threshold,
            },
            None => crate::VoiceState::Off,
        }
    }

    /// Start collecting. Any previous lock stays until the new one replaces
    /// it, so an enrolment abandoned halfway leaves you no worse off.
    pub fn begin(&mut self) {
        self.enrolment = Some(Enrolment::default());
        info!(seconds = ENROL_SECONDS, "voice enrolment started");
    }

    /// Forget the voice entirely, on disk too.
    pub fn clear(&mut self) {
        self.store = None;
        self.enrolment = None;
        Store::forget();
        info!("voice lock cleared");
    }

    fn embed(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        if self.model.is_none() {
            match SpeakerModel::load(&self.model_path) {
                Ok(m) => self.model = Some(m),
                Err(e) => {
                    warn!(error = %e, "no speaker model; voice lock cannot run");
                    return None;
                }
            }
        }
        let feats = fbank::compute_unit(samples);
        let frames = fbank::frame_count(samples.len());
        self.model.as_mut()?.embed(&feats, frames).ok()
    }

    /// Offer one closed segment of microphone audio.
    pub fn offer(&mut self, samples: &[f32]) -> Verdict {
        let seconds = samples.len() as f32 / fbank::SAMPLE_RATE;
        if self.enrolment.is_some() {
            return self.enrol(samples, seconds);
        }
        if self.store.is_none() {
            return Verdict::Pass;
        }
        // The deliberate hole: too short to judge is not the same as wrong,
        // and the words this catches are the ones the app needs most.
        if seconds < MIN_JUDGE_SECONDS {
            debug!(seconds, "too short to judge; passing");
            return Verdict::Pass;
        }
        let Some(embedding) = self.embed(samples) else {
            return Verdict::Pass;
        };
        let Some(store) = &self.store else {
            return Verdict::Pass;
        };
        let score = cosine(&embedding, &store.centroid);
        if score >= store.threshold {
            Verdict::Accept(score)
        } else {
            Verdict::Reject(score)
        }
    }

    fn enrol(&mut self, samples: &[f32], seconds: f32) -> Verdict {
        // Short segments are noise for a centroid in a way they are not for a
        // yes/no: here there is no cost to waiting for a better one.
        if seconds < MIN_ENROL_SECONDS {
            let progress = self
                .enrolment
                .as_ref()
                .map_or(0.0, |e| e.seconds / ENROL_SECONDS);
            return Verdict::Enrolling(progress);
        }
        if let Some(embedding) = self.embed(samples) {
            if let Some(e) = self.enrolment.as_mut() {
                e.seconds += seconds;
                e.embeddings.push(embedding);
            }
        }
        let Some(e) = self.enrolment.as_ref() else {
            return Verdict::Pass;
        };
        if e.seconds < ENROL_SECONDS {
            return Verdict::Enrolling(e.seconds / ENROL_SECONDS);
        }
        let enrolment = self.enrolment.take().expect("checked just above");
        let count = enrolment.embeddings.len();
        match build(&enrolment.embeddings) {
            Some(store) => {
                info!(
                    segments = store.segments,
                    threshold = store.threshold,
                    worst = store.self_worst,
                    mean = store.self_mean,
                    "voice enrolled"
                );
                store.store();
                self.store = Some(store);
                Verdict::Enrolled(count)
            }
            None => {
                warn!(count, "not enough usable segments to build a voice");
                Verdict::Pass
            }
        }
    }
}

/// Mean embedding, and a bar measured from how much your own samples disagree
/// with each other.
///
/// Leave-one-out on purpose. Scoring a sample against a centroid that contains
/// it flatters it, and with a handful of samples that bias is most of the
/// number — the bar would come out too high and reject you.
///
/// **What this cannot measure:** how close somebody *else* scores. That needs
/// another person's voice. So the bar guards against rejecting you, and its
/// power to reject an impostor is untested. Both numbers are stored so this
/// can be revisited with real data instead of re-derived from nothing.
pub fn build(embeddings: &[Vec<f32>]) -> Option<Store> {
    if embeddings.len() < 2 {
        return None;
    }
    let centroid = mean(embeddings)?;
    let mut worst = f32::MAX;
    let mut total = 0.0;
    let mut scores: Vec<f32> = Vec::with_capacity(embeddings.len());
    for (i, e) in embeddings.iter().enumerate() {
        let others: Vec<Vec<f32>> = embeddings
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, v)| v.clone())
            .collect();
        let Some(without) = mean(&others) else {
            continue;
        };
        let s = cosine(e, &without);
        scores.push(s);
        worst = worst.min(s);
        total += s;
    }
    let mean_self = total / embeddings.len() as f32;
    // The bar comes from the worst sample *after* discarding one, when there
    // are enough to afford it. Measured: in a 24 s recording of a single
    // person, one 1.8 s stretch scored 0.26 against the others while the rest
    // sat between 0.50 and 0.86. Taking the raw minimum would have set the
    // bar at 0.21 and let in anybody at all — one bad stretch of your own
    // voice should not decide who else gets in.
    let floor = if scores.len() >= 4 {
        let mut sorted = scores.clone();
        sorted.sort_by(f32::total_cmp);
        sorted[1]
    } else {
        worst
    };
    Some(Store {
        centroid,
        threshold: (floor - MARGIN).clamp(0.0, 1.0),
        segments: embeddings.len(),
        self_worst: worst,
        self_mean: mean_self,
    })
}

fn mean(embeddings: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dims = embeddings.first()?.len();
    if dims == 0 || embeddings.iter().any(|e| e.len() != dims) {
        return None;
    }
    let mut sum = vec![0.0f32; dims];
    for e in embeddings {
        for (s, v) in sum.iter_mut().zip(e) {
            *s += v;
        }
    }
    let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm <= 0.0 {
        return None;
    }
    Some(sum.into_iter().map(|v| v / norm).collect())
}

/// Where the model lives, so the pipeline does not have to know.
pub fn default_model_path() -> PathBuf {
    PathBuf::from("models/speaker-cam++.onnx")
}

#[cfg(test)]
#[path = "voicelock_tests.rs"]
mod tests;
