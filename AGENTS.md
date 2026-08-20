# AGENTS.md

## Project snapshot

- `oc-voice` is voice control for Hyprland: microphone -> text -> dictation, input
  submission, and window/WM commands. Four modes: Input, Enter, Command, Translate.
- Command recognition is similarity matching (Jaro-Winkler + word-count gate) against
  a per-language vocabulary in commands.toml; window targets resolve against live
  hyprctl output in two stages (category, then token). There is no LLM anywhere.
- **`BACKLOG.md` is the source of truth for what was built and why.** Items carry a
  checkmark and commit hash when done. Every design decision there is backed by a
  measurement against real windows; do not override one without a measurement of
  your own.

## Stack

- Rust 2021
- `cpal` microphone capture, `pw-record` system-audio capture (Translate mode)
- `rubato` resampling to 16 kHz mono, `ringbuf` between capture and inference
- `voice_activity_detector` (silero v5 via ONNX) for segmentation
- `whisper-rs` / `whisper.cpp` for local ASR, CUDA by default
- `eframe` / `egui` for the floating overlay
- `wtype` for text injection, `hyprctl` for window and WM control
- Nix flakes for the dev environment, `just` for common commands

## Repo layout

- `BACKLOG.md`: the roadmap. Start here.
- `src/main.rs`: pipeline orchestration, events, settings.
- `src/commands/`: classification (`mod.rs`), similarity matcher (`matcher.rs`),
  execution + confirmation policy (`execute.rs`).
- `src/config/`: per-language vocabulary; `default.toml` is the embedded default.
- `src/wm/`: window-target resolution (`target.rs`), WM dispatch (`dispatch.rs`),
  hyprland helpers.
- `src/audio/`, `src/asr/`, `src/input/`, `src/ui/`: capture, whisper, injection,
  overlay + stdout mirror.
- `tests/fixtures/`: hyprctl JSON snapshots the resolver tests run against.
- `Cargo.toml`, `justfile`, `flake.nix`: crate metadata, dev entrypoints, dev shell.
- `models/`: downloaded models; ignored by git.

## How to work here

1. Enter the environment with `nix develop`.
2. `just fetch-model` downloads the Whisper model (~874 MB) on first use.
3. `just run` for the default CUDA path, `just run-cpu` when no GPU is available.
4. `just check` before wrapping up — it runs the size ceiling (`limits`), the docs
   reference checker (`refs`), the hardcoded-vocabulary ban (`vocab`), `cargo check`,
   `cargo clippy -D warnings`, `cargo fmt --check` and `cargo test`.
5. `just fmt` to apply formatting.

## Engineering guidelines

- Prefer small, direct changes over premature architecture.
- Keep latency-sensitive paths simple and easy to inspect. Latency is why an earlier
  LLM-based classifier was abandoned; do not reintroduce per-utterance model inference.
- Any new matching threshold ships with the measurement that justifies it. Intuition
  about string similarity has been wrong every time it was checked in this project.
- Command vocabulary belongs in configuration, not in source. No application name,
  monitor name, or Portuguese keyword should be hardcoded.
- Keep the CPU fallback working when touching feature flags or run instructions.
- Keep stdout emission behavior stable unless the task explicitly changes the interface.

## Current behavior

- The binary expects a local GGML model path as its first argument.
- Default model is `ggml-large-v3-turbo-q8_0.bin`; see the `justfile`.
- Segmentation is VAD-driven: partials while speaking, final on silence. There is no
  fixed-size chunking.
- Three modes exist: `Input` (types as you speak), `Enter` (buffers until a send
  keyword), `Translate` (transcribes system audio). A `Command` mode is planned.
- Portuguese is the primary language; multi-language command support is planned in M1.3.

## Out of scope unless requested

- LLM-based command classification. It was removed deliberately — see M0.1 and the
  "Fora de escopo" section of the backlog for why, and for the only path back.
- Acoustic wake word. A textual prefix serves the same purpose without a second model.
- TTS, remote API fallback.
- Compositors other than Hyprland; `wtype` and `hyprctl` are assumed.
- Voice commands for Japanese and Chinese — transcription works, commands need a
  tokenizer for scripts without word spacing.

## Validation

- `just check` plus `cargo test`.
- Tests exist for the command matcher. Add tests for any matching or parsing change;
  the backlog states the expected cases for each item.
- If you change runtime behavior, say how to verify it with `just run` or `just run-cpu`.
