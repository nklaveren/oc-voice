//! Diagnostic subcommands. Each runs without whisper, without the overlay,
//! and without dispatching anything — they exist to answer "why is it doing
//! that?" before the pipeline is even started.
//!
//!   probe [lang]   type utterances, see the matcher's decision chain
//!   devices        list audio inputs, mark the one capture would use
//!   levels         live meter of the signal whisper receives
//!   asr-test <m>   read the reference passage aloud, get word error rate
//!   ocr <alvo>     what OCR reads off a window, with positions
//!   voices         load the speaker model and report what it declares
//!
//! `ctl` is the exception: it talks to a *running* instance rather than
//! standing alone. See `control.rs` for why that exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};

const SUBCOMMANDS: &[&str] = &[
    "probe", "devices", "levels", "asr-test", "ocr", "voices", "ctl",
];

/// Whether this argument names a diagnostic subcommand rather than a model.
pub fn is_subcommand(arg: &str) -> bool {
    SUBCOMMANDS.contains(&arg)
}

/// Returns true when `arg` named a subcommand and it has run.
pub fn run_subcommand(arg: &str) -> Result<bool> {
    match arg {
        "ctl" => {
            let Some(verb) = std::env::args().nth(2) else {
                println!("usage: oc-voice ctl <verb>");
                for (v, what) in crate::control::verbs() {
                    println!("  {v:<8} {what}");
                }
                return Ok(true);
            };
            crate::control::send(&verb)?;
            Ok(true)
        }
        "probe" => {
            crate::probe::run();
            Ok(true)
        }
        "devices" => {
            crate::audio::capture::list_devices();
            Ok(true)
        }
        "levels" => {
            let running = Arc::new(AtomicBool::new(true));
            let r = running.clone();
            ctrlc::set_handler(move || r.store(false, Ordering::SeqCst)).ok();
            crate::audio::capture::run_level_meter(running)?;
            Ok(true)
        }
        "asr-test" => {
            let model = std::env::args()
                .nth(2)
                .ok_or_else(|| anyhow!("usage: oc-voice asr-test <model.bin>"))?;
            crate::asrtest::run(&model)?;
            Ok(true)
        }
        "voices" => {
            // With a file: measure that recording against the stored voice.
            // Without: report what the model declares, which is the check that
            // came first and still answers a different question.
            if let Some(file) = std::env::args().nth(2) {
                let runner: std::sync::Arc<dyn crate::process::CommandRunner> =
                    std::sync::Arc::new(crate::process::SystemRunner);
                let config = crate::config::Config::load();
                let seg = config.segmentation(false);
                let hang_frames = (seg.hang_ms as usize * 16_000 / 1000) / crate::VAD_FRAME_SAMPLES;
                crate::voiceprobe::report(&runner, std::path::Path::new(&file), hang_frames)?;
                return Ok(true);
            }
            crate::voices::report()?;
            Ok(true)
        }
        "ocr" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            crate::ocrprobe::run(&args)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
