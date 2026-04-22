# AGENTS.md

## Project snapshot

- `oc-voice` is a Rust POC for `microphone -> text -> opencode TUI`.
- The current scope is intentionally small: capture audio with `cpal`, downmix/resample to 16 kHz mono, transcribe locally with `whisper-rs`, and print segments to stdout.
- This repo optimizes for validating latency and GPU/CUDA integration first, not production audio quality.

## Stack

- Rust 2021
- `cpal` for microphone capture
- `ringbuf` for streaming audio between capture and transcription
- `whisper-rs` / `whisper.cpp` for local ASR
- Nix flakes for the dev environment
- `just` for common commands

## Repo layout

- `src/main.rs`: the whole POC pipeline lives here today.
- `Cargo.toml`: crate metadata, dependencies, and feature flags.
- `justfile`: developer entrypoints for fetching models, running, checking, and formatting.
- `flake.nix`: Nix dev shell with Rust, CUDA, and audio dependencies.
- `models/`: downloaded Whisper models; ignored by git.

## How to work here

1. Enter the environment with `nix develop`.
2. Use `just run` for the default CUDA path.
3. Use `just run-cpu` if GPU/CUDA is unavailable.
4. Use `just check` before wrapping up changes.
5. Use `just fmt` to apply Rust formatting.

## Engineering guidelines

- Preserve the POC focus unless asked to broaden scope.
- Prefer small, direct changes over premature architecture.
- Keep latency-sensitive paths simple and easy to inspect.
- Treat the current resampling as disposable POC code; do not present it as production quality.
- Keep stdout emission behavior stable unless the task explicitly changes the interface.
- Default feature set enables CUDA; keep CPU fallback working when touching feature flags or run instructions.

## Current behavior assumptions

- The binary expects a local GGML model path as its first argument.
- The default model flow downloads `ggml-base.en.bin` into `models/`.
- Input uses the system default microphone.
- Audio is chunked in 3 second windows with 1 second overlap.
- Portuguese and English mixed speech is part of the evaluation context, even though the default model is `base.en`.

## Out of scope unless requested

- Wake word support
- Overlay / layer-shell UI
- Window routing
- TTS
- Remote API fallback
- Production-grade resampling or VAD

## Validation

- Primary validation is `just check`.
- If you change runtime behavior, mention how to verify with `just run` or `just run-cpu`.
- There are no dedicated tests yet, so avoid claiming test coverage that does not exist.
