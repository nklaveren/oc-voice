use crate::audio::resample::{push_samples, to_mono, Resampler16k};
use crate::process::CommandRunner;
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use ringbuf::traits::*;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Print every input device cpal can see and which one capture would pick.
/// `oc-voice devices` — answers "which mic is it actually using?" without
/// starting the pipeline.
pub fn list_devices() {
    let host = cpal::default_host();
    println!("host: {}", host.id().name());
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "<nenhum>".to_string());
    match host.input_devices() {
        Ok(devices) => {
            println!("\nentradas visíveis ao cpal:");
            for d in devices {
                let name = d.name().unwrap_or_else(|_| "?".into());
                let mark = if name == default_name { "*" } else { " " };
                match d.default_input_config() {
                    Ok(c) => println!(
                        "  {mark} {name}\n      {} Hz, {} canal(is), {:?}",
                        c.sample_rate().0,
                        c.channels(),
                        c.sample_format()
                    ),
                    Err(e) => println!("  {mark} {name}\n      [sem config de entrada: {e}]"),
                }
            }
        }
        Err(e) => println!("não consegui enumerar entradas: {e}"),
    }
    println!("\n* = o que `run_capture` usaria (default_input_device)");
    println!("para trocar, mude a fonte padrão no PipeWire: wpctl set-default <id>");
}

/// Live level meter on the exact path whisper receives: after downmix and
/// after resampling to 16 kHz. `oc-voice levels` — speak and watch.
///
/// Speech should sit around -25 to -15 dBFS RMS. A room at rest reads near
/// -60. If speech never lifts the RMS well above the resting value, whisper
/// is being fed a signal too quiet to transcribe, and no model change fixes
/// that — raise the source volume (`wpctl set-volume <id> 1.5`) or pick a
/// different microphone (`oc-voice devices`).
pub fn run_level_meter(running: Arc<AtomicBool>) -> Result<()> {
    let rb = ringbuf::HeapRb::<f32>::new(16_000 * 4);
    let (producer, mut consumer) = rb.split();
    let capture_running = running.clone();
    let handle = std::thread::spawn(move || {
        if let Err(e) = run_capture(producer, capture_running) {
            error!(error = ?e, "capture failed");
        }
    });

    println!("medindo o sinal que o whisper recebe (16 kHz mono). Ctrl+C sai.\n");
    let mut window: Vec<f32> = Vec::with_capacity(16_000);
    let mut peak_hold: f32 = 0.0;
    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        while let Some(v) = consumer.try_pop() {
            window.push(v);
        }
        if window.len() < 3_200 {
            continue;
        }
        let rms = (window.iter().map(|v| v * v).sum::<f32>() / window.len() as f32).sqrt();
        let peak = window.iter().fold(0.0_f32, |a, v| a.max(v.abs()));
        peak_hold = peak_hold.max(peak);
        let db = |x: f32| if x > 1e-9 { 20.0 * x.log10() } else { -99.0 };
        let rms_db = db(rms);
        // 40-cell bar spanning -60..0 dBFS.
        let filled = (((rms_db + 60.0) / 60.0).clamp(0.0, 1.0) * 40.0) as usize;
        let verdict = if rms_db > -30.0 {
            "voz"
        } else if rms_db > -50.0 {
            "baixo"
        } else {
            "silêncio"
        };
        print!(
            "\r\x1b[2K[{:<40}] RMS {:6.1} dBFS  pico {:6.1}  máx {:6.1}  {}   ",
            "#".repeat(filled),
            rms_db,
            db(peak),
            db(peak_hold),
            verdict
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
        window.clear();
    }
    let _ = handle.join();
    Ok(())
}

/// blocking call that keeps mic capture alive until `running` flips.
pub fn run_capture<P>(producer: P, running: Arc<AtomicBool>) -> Result<()>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;
    info!(device = %device.name().unwrap_or_else(|_| "?".into()), "using input");

    let default_config = device
        .default_input_config()
        .context("getting input config")?;
    let input_sample_rate = default_config.sample_rate().0;
    let input_channels = default_config.channels() as usize;
    info!(
        rate = input_sample_rate,
        channels = input_channels,
        format = ?default_config.sample_format(),
        "input config"
    );

    let err_fn = |err| error!("cpal stream error: {err}");

    let stream = match default_config.sample_format() {
        SampleFormat::F32 => {
            let config = default_config.into();
            let mut resampler = Resampler16k::new(input_sample_rate)
                .with_context(|| format!("building resampler {input_sample_rate}->16000"))?;
            device.build_input_stream(
                &config,
                {
                    let running = running.clone();
                    let mut producer = producer;
                    move |data: &[f32], _| {
                        if !running.load(Ordering::SeqCst) {
                            return;
                        }
                        let mono = to_mono(data, input_channels);
                        let resampled = resampler.process(&mono);
                        push_samples(&mut producer, &resampled);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let config = default_config.into();
            let mut resampler = Resampler16k::new(input_sample_rate)
                .with_context(|| format!("building resampler {input_sample_rate}->16000"))?;
            device.build_input_stream(
                &config,
                {
                    let running = running.clone();
                    let mut producer = producer;
                    move |data: &[i16], _| {
                        if !running.load(Ordering::SeqCst) {
                            return;
                        }
                        let floats: Vec<f32> =
                            data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        let mono = to_mono(&floats, input_channels);
                        let resampled = resampler.process(&mono);
                        push_samples(&mut producer, &resampled);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    stream.play().context("starting input stream")?;

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }

    drop(stream);
    Ok(())
}

pub fn run_capture_system<P>(
    mut producer: P,
    running: Arc<AtomicBool>,
    runner: Arc<dyn CommandRunner>,
) -> Result<()>
where
    P: Producer<Item = f32> + Send + 'static,
{
    let sink_target = get_default_sink_target(&runner);
    info!(target = %sink_target, "capturing system audio from sink monitor");

    // Force the capture stream to connect to the sink's monitor ports
    // instead of the default source. Without this, pw-record silently
    // links to the microphone input even when --target points to a sink.
    let mut child = runner
        .spawn_piped(
            "pw-record",
            &[
                "--raw",
                "--target",
                &sink_target,
                "-P",
                "stream.capture.sink=true",
                "--format",
                "f32",
                "--rate",
                "48000",
                "--channels",
                "2",
                "-",
            ],
        )
        .context("failed to spawn pw-record")?;

    // forward stderr to logs in the background
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|r| r.ok()) {
                warn!(target: "pw-record", "{}", line);
            }
        });
    }

    let stdout = child
        .stdout
        .take()
        .context("pw-record stdout not available")?;

    let mut reader = std::io::BufReader::new(stdout);
    let mut buf = [0u8; 4096 * 4];
    let mut resampler = Resampler16k::new(48000).context("building resampler 48000->16000")?;

    let mut bytes_total: u64 = 0;
    let mut last_log = Instant::now();

    while running.load(Ordering::SeqCst) {
        let n = match reader.read(&mut buf) {
            Ok(0) => {
                warn!("pw-record stdout EOF (process exited)");
                break;
            }
            Ok(n) => n,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                return Err(e).context("reading pw-record stdout");
            }
        };

        bytes_total += n as u64;
        if last_log.elapsed() >= Duration::from_secs(2) {
            info!(bytes_per_sec = bytes_total / 2, "pw-record throughput");
            bytes_total = 0;
            last_log = Instant::now();
        }

        let samples_f32: Vec<f32> = buf[..n]
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        let mono = to_mono(&samples_f32, 2);
        let resampled = resampler.process(&mono);
        push_samples(&mut producer, &resampled);
    }

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Returns the node.name of the current default audio sink. `pw-record --target <name>`
/// captures from the sink's monitor port. Falling back to `@DEFAULT_AUDIO_SINK@`
/// lets pipewire pick the default at runtime.
fn get_default_sink_target(runner: &Arc<dyn CommandRunner>) -> String {
    // Query pipewire for the default sink's node.name via pw-dump.
    let dump = match runner.output("pw-dump", &[]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return "@DEFAULT_AUDIO_SINK@".to_string(),
    };

    let value: serde_json::Value = match serde_json::from_slice(&dump) {
        Ok(v) => v,
        Err(_) => return "@DEFAULT_AUDIO_SINK@".to_string(),
    };

    // Step 1: find default sink name from Metadata object.
    let default_sink_name = value.as_array().and_then(|arr| {
        arr.iter().find_map(|obj| {
            if obj.get("type").and_then(|v| v.as_str()) != Some("PipeWire:Interface:Metadata") {
                return None;
            }
            if obj
                .get("props")
                .and_then(|p| p.get("metadata.name"))
                .and_then(|v| v.as_str())
                != Some("default")
            {
                return None;
            }
            obj.get("metadata")
                .and_then(|m| m.as_array())
                .and_then(|entries| {
                    entries.iter().find_map(|entry| {
                        if entry.get("key").and_then(|v| v.as_str()) == Some("default.audio.sink") {
                            entry
                                .get("value")
                                .and_then(|v| v.get("name"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                })
        })
    });

    default_sink_name.unwrap_or_else(|| "@DEFAULT_AUDIO_SINK@".to_string())
}
