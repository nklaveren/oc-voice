use crate::TARGET_SAMPLE_RATE;
use anyhow::{Context, Result};
use ringbuf::traits::*;
use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use tracing::error;

/// averages interleaved channels down to mono
pub fn to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Streaming resampler that converts mono f32 audio from `input_rate` to 16 kHz
/// using rubato's sinc interpolation. Accepts variable-size input chunks by
/// buffering leftover samples between calls.
///
/// If the input rate is already 16 kHz, we skip rubato entirely.
pub struct Resampler16k {
    inner: Option<SincFixedIn<f32>>,
    input_buffer: Vec<f32>,
    scratch_in: Vec<Vec<f32>>,
    scratch_out: Vec<Vec<f32>>,
    chunk_size: usize,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self> {
        if input_rate == TARGET_SAMPLE_RATE {
            return Ok(Self {
                inner: None,
                input_buffer: Vec::new(),
                scratch_in: vec![Vec::new()],
                scratch_out: vec![Vec::new()],
                chunk_size: 0,
            });
        }

        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };

        let chunk_size = 1024usize;
        let ratio = TARGET_SAMPLE_RATE as f64 / input_rate as f64;
        let inner = SincFixedIn::<f32>::new(ratio, 1.0, params, chunk_size, 1)
            .context("creating SincFixedIn resampler")?;

        let scratch_out_capacity = inner.output_frames_max();

        Ok(Self {
            inner: Some(inner),
            input_buffer: Vec::with_capacity(chunk_size * 4),
            scratch_in: vec![vec![0.0f32; chunk_size]],
            scratch_out: vec![vec![0.0f32; scratch_out_capacity]],
            chunk_size,
        })
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(resampler) = self.inner.as_mut() else {
            return input.to_vec();
        };

        self.input_buffer.extend_from_slice(input);

        let mut out: Vec<f32> = Vec::new();
        while self.input_buffer.len() >= self.chunk_size {
            self.scratch_in[0].clear();
            self.scratch_in[0].extend_from_slice(&self.input_buffer[..self.chunk_size]);
            self.input_buffer.drain(..self.chunk_size);

            match resampler.process_into_buffer(&self.scratch_in, &mut self.scratch_out, None) {
                Ok((_in_frames, out_frames)) => {
                    out.extend_from_slice(&self.scratch_out[0][..out_frames]);
                }
                Err(e) => {
                    error!(error = ?e, "rubato resample failed; dropping chunk");
                }
            }
        }
        out
    }
}

pub fn push_samples<P: Producer<Item = f32>>(producer: &mut P, samples: &[f32]) {
    let _pushed = producer.push_slice(samples);
}
