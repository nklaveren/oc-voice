//! Kaldi-compatible 80-band mel filterbanks — the input CAM++ actually eats.
//!
//! The model does not take audio. Its graph declares `feats [? × ? × 80]`, and
//! those 80 numbers per frame have to be computed **the way the model was
//! trained**, which is Kaldi's convention down to the window shape and the
//! floor on the log. Getting it subtly wrong does not break anything visibly:
//! the cosine still returns plausible numbers, the clustering still forms
//! clusters, and every one of them is wrong. That is the worst failure mode a
//! feature extractor has, and it is why this file exists at all rather than
//! being three lines inside `voices.rs`.
//!
//! Pure Rust, and deliberately. `knf-rs` would bring leptonica and C++ along,
//! and the last C++ library added to this binary collided with onnxruntime's
//! symbols (`af4da28`). The maths here is a few hundred lines of arithmetic
//! with a reference test against torchaudio's own Kaldi implementation; a
//! dependency would be more code, not less, and a linker problem besides.

use std::f32::consts::PI;

/// What CAM++ was trained on. Not options: changing any of them changes what
/// the embedding means, and nothing downstream would notice.
pub const SAMPLE_RATE: f32 = 16_000.0;
pub const MEL_BANDS: usize = 80;
const FRAME_LENGTH_MS: f32 = 25.0;
const FRAME_SHIFT_MS: f32 = 10.0;
const PREEMPHASIS: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
/// Kaldi's floor on the log, so a silent band is a large negative number
/// rather than an infinity that poisons every downstream sum.
const LOG_FLOOR: f32 = 1.1920929e-7;

pub fn frame_length() -> usize {
    (SAMPLE_RATE * FRAME_LENGTH_MS / 1000.0) as usize
}

pub fn frame_shift() -> usize {
    (SAMPLE_RATE * FRAME_SHIFT_MS / 1000.0) as usize
}

/// How many frames Kaldi gets out of `samples`, with `snip_edges=true`: only
/// whole windows count, and a tail shorter than one window is dropped rather
/// than padded. Padding would invent audio, and the model would embed it.
pub fn frame_count(samples: usize) -> usize {
    let (len, shift) = (frame_length(), frame_shift());
    if samples < len {
        0
    } else {
        (samples - len) / shift + 1
    }
}

/// Hz to mel, Kaldi's variant of the formula. Slaney's differs, and a mel
/// scale that disagrees with the training one moves every filter.
fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// The window Kaldi calls "povey": a Hann raised to 0.85. Not a typo for
/// Hann — the exponent is the difference, and it is what the model saw.
fn povey_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let a = 2.0 * PI * i as f32 / (n as f32 - 1.0);
            (0.5 - 0.5 * a.cos()).powf(0.85)
        })
        .collect()
}

/// Triangular mel filters over the power spectrum bins, in Kaldi's layout:
/// `MEL_BANDS` filters between `LOW_FREQ` and Nyquist, each stored as the bin
/// range it touches and the weights over it.
struct MelBanks {
    /// `(first_bin, weights)` per band.
    bands: Vec<(usize, Vec<f32>)>,
}

impl MelBanks {
    fn new(fft_size: usize) -> Self {
        let num_bins = fft_size / 2;
        let nyquist = SAMPLE_RATE / 2.0;
        let fft_bin_width = SAMPLE_RATE / fft_size as f32;
        let (mel_low, mel_high) = (hz_to_mel(LOW_FREQ), hz_to_mel(nyquist));
        let mel_delta = (mel_high - mel_low) / (MEL_BANDS + 1) as f32;

        let mut bands = Vec::with_capacity(MEL_BANDS);
        for b in 0..MEL_BANDS {
            let left = mel_low + b as f32 * mel_delta;
            let center = left + mel_delta;
            let right = center + mel_delta;
            let mut first = None;
            let mut weights = Vec::new();
            for bin in 0..num_bins {
                let mel = hz_to_mel(fft_bin_width * bin as f32);
                if mel <= left || mel >= right {
                    if first.is_some() && !weights.is_empty() && mel >= right {
                        break;
                    }
                    continue;
                }
                let w = if mel <= center {
                    (mel - left) / (center - left)
                } else {
                    (right - mel) / (right - center)
                };
                if first.is_none() {
                    first = Some(bin);
                }
                weights.push(w);
            }
            bands.push((first.unwrap_or(0), weights));
        }
        MelBanks { bands }
    }

    fn apply(&self, power: &[f32], out: &mut [f32]) {
        for (band, (first, weights)) in self.bands.iter().enumerate() {
            let mut sum = 0.0;
            for (i, w) in weights.iter().enumerate() {
                sum += w * power[first + i];
            }
            out[band] = sum.max(LOG_FLOOR).ln();
        }
    }
}

/// A radix-2 FFT, iterative and in place, on separate re/im in f64.
///
/// Written out rather than pulled in: the only thing needed is a power-of-two
/// real transform of 512 points, and `rustfft` is a dependency and a build for
/// forty lines of arithmetic that a reference test pins exactly.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        // Twiddles from `cos`/`sin` per k, not by rotating a running pair.
        // The recurrence drifts: 256 successive complex multiplies at the
        // last stage put the worst log-mel value 0.03 off the reference,
        // which is far too small to look like a bug and far too large to be
        // f32 addition order.
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let theta = ang * k as f64;
                let (cr, ci) = (theta.cos(), theta.sin());
                let (ur, ui) = (re[start + k], im[start + k]);
                let (vr0, vi0) = (re[start + k + len / 2], im[start + k + len / 2]);
                let vr = vr0 * cr - vi0 * ci;
                let vi = vr0 * ci + vi0 * cr;
                re[start + k] = ur + vr;
                im[start + k] = ui + vi;
                re[start + k + len / 2] = ur - vr;
                im[start + k + len / 2] = ui - vi;
            }
        }
        len <<= 1;
    }
}

/// 80 log-mel values per frame, row-major, `frame_count(samples)` rows.
///
/// The order of operations is Kaldi's and is not interchangeable: DC removal
/// before pre-emphasis, pre-emphasis before windowing, and the first sample of
/// the frame pre-emphasised against itself rather than against the previous
/// frame's tail.
pub fn compute(samples: &[f32]) -> Vec<f32> {
    let (len, shift) = (frame_length(), frame_shift());
    let frames = frame_count(samples.len());
    let fft_size = len.next_power_of_two();
    let window = povey_window(len);
    let banks = MelBanks::new(fft_size);

    let mut out = vec![0.0; frames * MEL_BANDS];
    // The transform runs in f64. At f32 the accumulated rounding put the
    // worst log-mel 0.011 off torchaudio's — small enough to pass for noise
    // and large enough to be a systematic bias in every embedding. A hundred
    // 512-point transforms per second of audio is nowhere near the budget.
    let mut re = vec![0.0f64; fft_size];
    let mut im = vec![0.0f64; fft_size];
    let mut buf = vec![0.0; len];
    let mut power = vec![0.0; fft_size / 2];

    for f in 0..frames {
        let start = f * shift;
        buf.copy_from_slice(&samples[start..start + len]);

        let mean = buf.iter().sum::<f32>() / len as f32;
        for v in buf.iter_mut() {
            *v -= mean;
        }
        // Backwards, so each sample still sees its untouched predecessor.
        for i in (1..len).rev() {
            buf[i] -= PREEMPHASIS * buf[i - 1];
        }
        buf[0] -= PREEMPHASIS * buf[0];

        re[..len]
            .iter_mut()
            .zip(&buf)
            .zip(&window)
            .for_each(|((r, s), w)| *r = (s * w) as f64);
        re[len..].fill(0.0);
        im.fill(0.0);
        fft(&mut re, &mut im);

        for (bin, p) in power.iter_mut().enumerate() {
            *p = (re[bin] * re[bin] + im[bin] * im[bin]) as f32;
        }
        banks.apply(&power, &mut out[f * MEL_BANDS..(f + 1) * MEL_BANDS]);
    }
    out
}

#[cfg(test)]
#[path = "fbank_tests.rs"]
mod tests;
