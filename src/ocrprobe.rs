//! `oc-voice ocr` — see what OCR reads off a window, before trusting it.
//!
//! Two steps, in this order on purpose:
//!
//!   oc-voice ocr <alvo>            dump the whole window with positions
//!   oc-voice ocr watch <x,y wxh>   sample that region until Ctrl+C
//!
//! The first exists because nobody knows where a meeting client puts the
//! active speaker's name until they look. Guessing a region and building on it
//! is how you get a tool that works on one layout and silently reads the wrong
//! rectangle on every other.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::ocr::{self, Region};
use crate::process::{CommandRunner, SystemRunner};

/// Below this, tesseract is reporting shapes rather than characters.
const READABLE: f32 = 60.0;

pub fn run(args: &[String]) -> Result<()> {
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemRunner);
    let lang = std::env::var("OC_VOICE_OCR_LANG").unwrap_or_else(|_| "eng".to_string());

    match args.first().map(String::as_str) {
        Some("watch") => {
            let region = args
                .get(1..)
                .map(|rest| rest.join(" "))
                .and_then(|s| Region::parse(&s))
                .ok_or_else(|| anyhow!("usage: oc-voice ocr watch <x,y wxh>"))?;
            watch(&runner, region, &lang)
        }
        Some(target) => dump(&runner, target, &lang),
        None => Err(anyhow!(
            "usage: oc-voice ocr <alvo> | oc-voice ocr watch <x,y wxh>"
        )),
    }
}

/// Everything OCR can see in a window, with the geometry to grab each line
/// again on its own.
fn dump(runner: &Arc<dyn CommandRunner>, target: &str, lang: &str) -> Result<()> {
    let region = ocr::window_region(runner, target)?;
    println!("janela: {}", region.geometry());

    let start = Instant::now();
    let words = ocr::read(runner, region, lang)?;
    let elapsed = start.elapsed();

    let lines = ocr::lines(&words);
    println!(
        "{} palavras em {} linhas, {} ms\n",
        words.len(),
        lines.len(),
        elapsed.as_millis()
    );

    for (top, left, text, conf) in &lines {
        // The geometry each line would need on its own — paste it into
        // `ocr watch` to poll just that strip.
        let strip = Region {
            x: region.x + left,
            y: region.y + top - 2,
            w: region.w - left,
            h: 24,
        };
        let mark = if *conf < READABLE { "?" } else { " " };
        println!("{mark} {conf:5.1}  {:<24}  {text}", strip.geometry());
    }

    println!(
        "\nEscolha a faixa do nome e amostre com:\n  oc-voice ocr watch <x,y wxh>\n\
         Confiança abaixo de {READABLE:.0} vem marcada com ?."
    );
    Ok(())
}

/// Poll one region and report only when what it says changes.
///
/// Change-only because a name sits on screen for the length of an utterance:
/// printing every sample would bury the two lines that matter under a hundred
/// identical ones.
fn watch(runner: &Arc<dyn CommandRunner>, region: Region, lang: &str) -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst)).ok();

    println!("amostrando {} — Ctrl+C para parar\n", region.geometry());
    println!("{:>8}  {:>5}  {:>5}  texto", "t", "ms", "conf");

    let start = Instant::now();
    let mut last = String::new();
    let mut samples = 0u32;
    let mut failures = 0u32;

    while running.load(Ordering::SeqCst) {
        let tick = Instant::now();
        let read = ocr::read(runner, region, lang);
        samples += 1;

        match read {
            Ok(words) => {
                let conf = words
                    .iter()
                    .map(|w| w.conf)
                    .fold(f32::INFINITY, f32::min)
                    .min(100.0);
                let text = words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if text != last {
                    println!(
                        "{:>7.1}s  {:>5}  {:>5.1}  {}",
                        start.elapsed().as_secs_f32(),
                        tick.elapsed().as_millis(),
                        if words.is_empty() { 0.0 } else { conf },
                        if text.is_empty() { "(vazio)" } else { &text }
                    );
                    let _ = std::io::stdout().flush();
                    last = text;
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!("{:>7.1}s  falhou: {e}", start.elapsed().as_secs_f32());
            }
        }

        // A name stays up for the length of an utterance; four looks a second
        // is plenty, and leaves the CPU to whisper.
        std::thread::sleep(Duration::from_millis(250));
    }

    println!(
        "\n{samples} amostras, {failures} falhas, {:.0}s",
        start.elapsed().as_secs_f32()
    );
    Ok(())
}
