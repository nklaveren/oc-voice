//! Diagnostic subcommands. Each runs without whisper, without the overlay,
//! and without dispatching anything — they exist to answer "why is it doing
//! that?" before the pipeline is even started.
//!
//!   probe [lang]   type utterances, see the matcher's decision chain
//!   devices        list audio inputs, mark the one capture would use
//!   levels         live meter of the signal whisper receives
//!   asr-test <m>   read the reference passage aloud, get word error rate
//!   ocr <alvo>     what OCR reads off a window, with positions

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};

const SUBCOMMANDS: &[&str] = &["probe", "devices", "levels", "asr-test", "ocr"];

/// Whether this argument names a diagnostic subcommand rather than a model.
pub fn is_subcommand(arg: &str) -> bool {
    SUBCOMMANDS.contains(&arg)
}

/// Returns true when `arg` named a subcommand and it has run.
pub fn run_subcommand(arg: &str) -> Result<bool> {
    match arg {
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
        "ocr" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            crate::ocrprobe::run(&args)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
