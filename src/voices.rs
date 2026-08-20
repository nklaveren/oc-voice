//! Speaker embeddings — M7.4 groundwork.
//!
//! Whisper does not do this. It transcribes; it has no notion of who spoke.
//! Telling voices apart is a separate model — CAM++ here, ~29 MB of ONNX —
//! run through the same `ort`/onnxruntime the silero VAD already loads, so no
//! new runtime and no new chance at the protobuf clash of `af4da28`.
//!
//! The model takes **fbank features, not audio**: its graph declares an input
//! `feats` and an output `embs`. That is the whole reason this module exists
//! separately from a one-line inference call — 80-band mel filterbanks have to
//! be computed first, and computing them slightly differently from how the
//! model was trained yields embeddings that are confidently wrong rather than
//! obviously broken.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use ort::session::Session;

/// What CAM++ was trained on: 80-band Kaldi fbank at 16 kHz.
pub const MEL_BANDS: usize = 80;

pub struct SpeakerModel {
    session: Session,
}

/// What the model says about its own tensors, read from the loaded graph
/// rather than assumed. Assumed dimensions are how you get a pipeline that
/// runs, returns numbers, and compares noise to noise.
#[derive(Debug, Clone)]
pub struct ModelShape {
    pub input: String,
    pub input_dims: Vec<Option<i64>>,
    pub output: String,
    pub output_dims: Vec<Option<i64>>,
}

impl SpeakerModel {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(anyhow!(
                "no speaker model at {} — see M7.4 in BACKLOG.md",
                path.display()
            ));
        }
        let session = Session::builder()
            .context("building onnx session")?
            .commit_from_file(path)
            .with_context(|| format!("loading {}", path.display()))?;
        Ok(SpeakerModel { session })
    }

    pub fn shape(&self) -> Result<ModelShape> {
        let input = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow!("model declares no input"))?;
        let output = self
            .session
            .outputs
            .first()
            .ok_or_else(|| anyhow!("model declares no output"))?;
        Ok(ModelShape {
            input: input.name.clone(),
            input_dims: tensor_dims(&input.input_type),
            output: output.name.clone(),
            output_dims: tensor_dims(&output.output_type),
        })
    }

    /// Run one utterance's features through the model.
    ///
    /// `feats` is `frames × MEL_BANDS`, row-major. Returns the embedding.
    pub fn embed(&mut self, feats: &[f32], frames: usize) -> Result<Vec<f32>> {
        if frames == 0 || feats.len() != frames * MEL_BANDS {
            return Err(anyhow!(
                "expected {frames}x{MEL_BANDS} = {} values, got {}",
                frames * MEL_BANDS,
                feats.len()
            ));
        }
        let tensor = ort::value::Tensor::from_array(([1, frames, MEL_BANDS], feats.to_vec()))
            .context("building input tensor")?;
        let outputs = self
            .session
            .run(ort::inputs!["feats" => tensor])
            .context("running the speaker model")?;
        let (_, data) = outputs["embs"]
            .try_extract_tensor::<f32>()
            .context("reading embedding")?;
        Ok(data.to_vec())
    }
}

fn tensor_dims(ty: &ort::value::ValueType) -> Vec<Option<i64>> {
    match ty {
        ort::value::ValueType::Tensor { shape, .. } => shape
            .iter()
            .map(|d| if *d < 0 { None } else { Some(*d) })
            .collect(),
        _ => Vec::new(),
    }
}

/// Cosine similarity, the same shape of decision the window resolver makes:
/// normalise, compare, decide by a measured threshold, refuse when unsure.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// `oc-voice voices` — does the model load here, and what does it declare?
///
/// The question this answers before any feature-extraction work begins: a
/// second onnxruntime consumer in the same process is exactly what broke the
/// VAD in `af4da28`, and finding that out after writing a mel filterbank
/// would be finding it out expensively.
pub fn report() -> Result<()> {
    let path = std::path::Path::new("models/speaker-cam++.onnx");
    println!("carregando {}", path.display());
    let mut model = SpeakerModel::load(path)?;
    let shape = model.shape()?;

    let fmt = |dims: &[Option<i64>]| {
        dims.iter()
            .map(|d| match d {
                Some(n) => n.to_string(),
                None => "?".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" x ")
    };
    println!(
        "  entrada  {:>8}  [{}]",
        shape.input,
        fmt(&shape.input_dims)
    );
    println!(
        "  saída    {:>8}  [{}]",
        shape.output,
        fmt(&shape.output_dims)
    );

    // Synthetic features, purely to prove the plumbing runs. Their embeddings
    // are meaningless and are not reported as if they were not.
    let frames = 200;
    let flat = vec![0.0f32; frames * MEL_BANDS];
    let sloped: Vec<f32> = (0..frames * MEL_BANDS)
        .map(|i| (i % MEL_BANDS) as f32 / MEL_BANDS as f32)
        .collect();

    let start = std::time::Instant::now();
    let a = model.embed(&flat, frames)?;
    let infer_ms = start.elapsed().as_millis();
    println!(
        "\n{} dimensões em {infer_ms} ms sobre {frames} quadros",
        a.len()
    );

    // The question that decides whether a voice database can exist at all: a
    // stored centroid is only comparable to a future embedding if the model
    // returns the same vector for the same input. A graph with dropout left
    // in would score below 1.0 here, and every stored voice would rot.
    let again = model.embed(&flat, frames)?;
    let b = model.embed(&sloped, frames)?;
    println!(
        "  determinismo (mesma entrada duas vezes): {:.6}",
        cosine(&a, &again)
    );
    println!(
        "  entrada diferente:                       {:.6}",
        cosine(&a, &b)
    );
    println!("\n  (os vetores não significam nada — features sintéticas. O que");
    println!("   isto prova é que o runtime aceita o modelo junto com o do VAD,");
    println!("   qual é a dimensão real, e se o mesmo áudio dá sempre o mesmo");
    println!("   vetor, que é a premissa de guardar voz em disco.)");
    Ok(())
}

#[cfg(test)]
#[path = "voices_tests.rs"]
mod tests;
