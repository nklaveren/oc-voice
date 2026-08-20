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
