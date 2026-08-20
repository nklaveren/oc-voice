//! `oc-voice ocr` — see what OCR reads off a window, before trusting it.
//!
//! Two steps, in this order on purpose:
//!
//!   oc-voice ocr <alvo>              dump the whole window with positions
//!   oc-voice ocr changes <alvo>      rank its lines by how much they move
//!   oc-voice ocr watch <x,y wxh>     sample one region until Ctrl+C
//!
//! Nobody knows where a meeting client puts the active speaker's name until
//! they look, and guessing a region is how you get a tool that works on one
//! layout and silently reads the wrong rectangle on every other.
//!
//! `changes` exists because asking a person to eyeball a 72-line dump is the
//! wrong question too. The speaker's name is the thing that *moves* while the
//! toolbar, the participant list and the chat sit still — so measure movement
//! and let the region announce itself.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::ocr::{self, Region};
use crate::process::{CommandRunner, SystemRunner};

/// Below this, tesseract is reporting shapes rather than characters.
const READABLE: f32 = 60.0;

/// Above this similarity, two readings of a line are the same text read
/// twice, not a change.
///
/// Measured, not guessed: on a completely static window the naive
/// change-counter reported seven "changes" in seven scans for lines like
/// `&9 = Q` and `Adjustments` — anti-aliasing makes tesseract flip a
/// character between passes. Counting raw inequality finds noise everywhere
/// and buries the one line that actually moved.
///
/// This is the same answer the rest of the project reaches for: text is
/// noisy, so compare by similarity and decide by a threshold, rather than by
/// equality.
const SAME_TEXT: f64 = 0.90;

/// A line with fewer letters than this is not a name.
///
/// Short strings also break the similarity test — `&9 = Q` versus `&9 = O`
/// scores 0.77 and reads as a change, which is the same reason the window
/// resolver refuses tokens under three characters. Filtering by letter count
/// removes the toolbar glyphs and the clock in one rule instead of two.
const MIN_NAME_LETTERS: usize = 3;

fn could_be_a_name(text: &str) -> bool {
    text.chars().filter(|c| c.is_alphabetic()).count() >= MIN_NAME_LETTERS
}

/// Whether two readings are different enough to count as a real change.
fn really_changed(before: &str, after: &str) -> bool {
    let a = crate::commands::matcher::normalize(before);
    let b = crate::commands::matcher::normalize(after);
    if a.is_empty() != b.is_empty() {
        return true;
    }
    strsim::jaro_winkler(&a, &b) < SAME_TEXT
}

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
        Some("changes") => {
            let target = args.get(1).map(String::as_str).unwrap_or("teams");
            let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
            changes(&runner, target, &lang, seconds)
        }
        Some(target) => dump(&runner, target, &lang),
        None => Err(anyhow!(
            "usage: oc-voice ocr <alvo> | ocr changes <alvo> [segundos] | ocr watch <x,y wxh>"
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

/// Watch a whole window and rank its lines by how much they move.
///
/// This exists because asking a person to eyeball a 72-line dump and pick the
/// speaker's name is the wrong question. In a running meeting the active
/// speaker's name is, by definition, **the thing that changes** while the
/// participant list, the toolbar and the chat sit still. So do not look for a
/// name — look for movement, and let the region announce itself.
///
/// Full-window OCR costs ~2.5 s, which is far too slow to caption anything and
/// exactly right for a survey run once.
fn changes(runner: &Arc<dyn CommandRunner>, target: &str, lang: &str, seconds: u64) -> Result<()> {
    let region = ocr::window_region(runner, target)?;
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst)).ok();

    println!(
        "observando {} por {seconds}s — fale, ou deixe a reunião correr\n\
         (Ctrl+C encerra antes)\n",
        region.geometry()
    );

    // Keyed by (top, left), not by top alone: two columns at the same height
    // — a sidebar and a chat pane — otherwise share one entry and overwrite
    // each other every scan, reporting more changes than there were scans.
    let mut seen: std::collections::HashMap<(i64, i64), (String, u32)> =
        std::collections::HashMap::new();
    let start = Instant::now();
    let mut rounds = 0u32;

    while running.load(Ordering::SeqCst) && start.elapsed().as_secs() < seconds {
        let words = match ocr::read(runner, region, lang) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("leitura falhou: {e}");
                break;
            }
        };
        rounds += 1;
        for (top, left, text, conf) in ocr::lines(&words) {
            if conf < READABLE || !could_be_a_name(&text) {
                continue;
            }
            let entry = seen.entry((top, left)).or_insert_with(|| (text.clone(), 0));
            if really_changed(&entry.0, &text) {
                entry.0 = text.clone();
                entry.1 += 1;
                print!(
                    "\r{:>6.1}s  {:<20}  {}",
                    start.elapsed().as_secs_f32(),
                    Region {
                        x: region.x + left,
                        y: region.y + top - 2,
                        w: 400,
                        h: 24
                    }
                    .geometry(),
                    text
                );
                println!();
                let _ = std::io::stdout().flush();
            }
        }
    }

    let mut ranked: Vec<_> = seen
        .iter()
        .filter(|(_, (_, changes))| *changes > 0)
        .collect();
    ranked.sort_by_key(|(_, (_, changes))| std::cmp::Reverse(*changes));

    println!(
        "\n{rounds} varreduras em {:.0}s",
        start.elapsed().as_secs_f32()
    );
    if ranked.is_empty() {
        println!(
            "nada mudou. Ou a janela estava parada, ou o nome do falante não é\n\
             texto que o OCR alcance — nesse caso este caminho não serve e a\n\
             diarização local (M7.4) é a resposta."
        );
        return Ok(());
    }
    // A line that changes on nearly every scan is flickering, not speaking:
    // real speaker changes are far rarer than the scan rate.
    println!(
        "\nregiões que mudaram — a de cima é a candidata.\n\
         Mudança em quase toda varredura ({rounds}) é ruído, não fala:\n"
    );
    println!("{:>8}  {:<20}  último valor", "mudanças", "região");
    for ((top, left), (text, count)) in ranked.iter().take(8) {
        let strip = Region {
            x: region.x + left,
            y: region.y + top - 2,
            w: 400,
            h: 24,
        };
        println!("{count:>8}  {:<20}  {text}", strip.geometry());
    }
    println!("\nConfirme com:\n  oc-voice ocr watch \"<região>\"");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_jitter_is_not_a_change() {
        // Measured, not assumed: on a completely static window the naive
        // comparison reported seven "changes" in seven scans. Anti-aliasing
        // makes tesseract flip a character between passes, and counting raw
        // inequality finds noise everywhere while burying the one line that
        // actually moved.
        assert!(!really_changed("Marzioni, Emiliano", "Marzioni, Emihano"));
        assert!(!really_changed("Penubolu, Vijay", "Penubolu Vijay"));
        // A different person is a different person.
        assert!(really_changed("Penubolu, Vijay", "Salla, Shashikanth"));
        // And a line appearing or vanishing always counts.
        assert!(really_changed("", "Aaketi, Sainath"));
        assert!(really_changed("Aaketi, Sainath", ""));
    }

    #[test]
    fn toolbar_glyphs_are_not_candidate_names() {
        // The four lines that survived every earlier filter were `&9 = Q`,
        // `3:00 PM`, `-` and a partial word. Short strings also break the
        // similarity test — `&9 = Q` against `&9 = O` scores 0.77 — which is
        // the same reason the window resolver refuses tokens under three
        // characters.
        for noise in ["&9 = Q", "3:00 PM", "-", "Q", "| B", ""] {
            assert!(!could_be_a_name(noise), "{noise:?} passed as a name");
        }
        for name in ["Vijay", "Salla, Shashikanth", "Klaveren, Nicolas"] {
            assert!(could_be_a_name(name), "{name:?} was rejected");
        }
    }
}
