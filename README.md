# oc-voice (POC)

Validates the riskiest path of a bigger idea: **voice → text → opencode TUI**.

This POC only does:

1. Capture microphone via cpal (default input device)
2. Resample to 16 kHz mono (naive — for latency check only, not quality)
3. Stream through whisper.cpp (via whisper-rs, CUDA enabled) in 3s chunks with 1s overlap
4. Print transcribed text to stdout

If latency is acceptable here, the rest (wake word, overlay, window router, TTS) is
plain engineering on top.

## Usage

Requires NixOS + flakes + `nvidia` driver (tested on RTX 3070 Ti).

```bash
nix develop
just run
```

First run downloads `ggml-base.en.bin` (~142 MB). Swap model in `justfile` for
`small.en` (~466 MB) or `medium.en` (~1.5 GB) for better accuracy.

Press Ctrl+C to stop. Stdout carries transcriptions one line per emitted segment.

## What I want to learn from this

- GPU init time (cold)
- End-to-end latency: mouth → stdout
- Whisper accuracy with default mic + Portuguese / English code terms
- Whether `base.en` is good enough, or we need `small.en`+
- Does whisper-rs CUDA link cleanly against my nixpkgs CUDA

## Not in this POC

- Wake word ("hey opencode")
- Wayland layer-shell caption overlay
- Window router (hyprctl)
- TTS
- OpenAI API fallback
- Proper resampling (rubato)
- VAD (silero / webrtc-vad)

Those land once the core loop feels right.
