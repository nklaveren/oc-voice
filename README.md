# oc-voice

Voice dictation and window control for [Hyprland](https://hyprland.org), fully
local. Speak Portuguese (or English, or any language you configure), watch the
live transcription in a floating overlay, and say a keyword to send the text
into any window — or drive Hyprland itself: focus a monitor, switch
workspaces, move window focus, all by voice. Nothing leaves your machine.

## How it works

```
microphone ──► cpal ──► rubato 16 kHz ──► silero VAD ──► whisper.cpp (CUDA)
                                                              │
              floating egui overlay ◄── partials / finals ◄───┤
              wtype / hyprctl      ◄── commands ◄─────────────┘
```

Partial transcriptions render every ~800 ms while you speak; a final lands on
silence. Finals are matched against a small spoken-command vocabulary by
string similarity (Jaro-Winkler with a word-count gate — no LLM, microsecond
latency, and it survives ASR errors: "sambio", "kambio" and "cambiu" all
resolve to "câmbio"). Window targets resolve against the live `hyprctl
clients` list, so "envia para o navegador" finds whatever browser is actually
open.

## Modes

| Mode | What it does |
|---|---|
| **Enter** | Buffers dictation; "câmbio" sends it to the focused window, "envia para <alvo>" sends it to a named window |
| **Input** | Types as you speak, straight into the focused window |
| **Command** | Everything is a WM command; nothing is ever typed |
| **Translate** | Transcribes the *system audio* (a call, a video) instead of the microphone |

## Voice commands (Portuguese defaults)

| Say | Effect |
|---|---|
| "câmbio" / "envia" / "manda" / "pronto" | send the buffered text |
| "cancela" / "limpa" / "descarta" | discard the buffer |
| "nova linha" / "pula linha" | line break |
| "envia para \<alvo\>" | send buffer to a window ("navegador", "terminal", "teams"…) |
| "monitor \[da\] direita / esquerda / \[do\] meio / centro" | focus monitor by physical position |
| "monitor \<marca\>" | focus monitor by brand ("monitor samsung") |
| "janela da esquerda / de cima …" | move window focus |
| "área de trabalho \<n\>" | switch workspace ("quatro" or "4") |
| "leva pra \<n\>" | move window to workspace |
| "foca o \<alvo\>" | focus a window by name |
| "tela cheia" / "flutuante" | fullscreen / toggle floating |
| "fecha" → "confirma" | close window (always asks first) |

Destructive actions and low-confidence window matches wait for spoken
confirmation ("confirma" / "não"). Everything else fires immediately.

## Configuration

All vocabulary lives in `~/.config/oc-voice/commands.toml`, per language.
Portuguese and English ship built in; a language section you define fully
replaces the built-in one, and adding a new language is just writing one:

```toml
[matching]
threshold = 0.82       # similarity acceptance
confirm_below = 0.9    # ask before acting under this resolution score

[es]
prefix  = ["computadora"]
send    = ["envía", "listo"]
cancel  = ["cancela"]
newline = ["nueva línea"]

[es.numbers]
uno = 1
dos = 2

[es.targets]
navegador = ["firefox", "brave", "chromium"]
```

Set `require_prefix = true` in a language section to only accept commands
that start with the prefix word ("computador, câmbio").

## Requirements

- **NixOS with flakes** (the dev shell provides the whole toolchain)
- **Hyprland on Wayland** — `hyprctl` and `wtype` are assumed
- **NVIDIA GPU** for the default CUDA build (tested: RTX 3070 Ti Laptop,
  driver via `hardware.graphics.enable`); a CPU build exists, see below

## Running

```bash
nix develop
just run          # downloads ggml-large-v3-turbo-q8_0.bin (~874 MB) on first use
```

`just check` runs the full gate suite: clippy, fmt, tests, a per-file size
ceiling, a hardcoded-vocabulary ban, and a docs reference checker.

## CPU fallback

```bash
just run-cpu
```

Measured with `large-v3-turbo` Q8 on this repo's ignored benchmark
(`measure_transcribe_latency`, 3 s of audio):

| Build | 3 s of audio | Model load | Hardware |
|---|---|---|---|
| CUDA (default) | **238–354 ms** | 2.8 s | RTX 3070 Ti Laptop |
| CPU (`just run-cpu`) | **~51 s** | 1.6 s | i7-12700H, 20 threads |

The verdict is unambiguous: `large-v3-turbo` on this CPU runs 17× slower than
realtime and cannot drive live partials. If you have no NVIDIA GPU, swap
`model_name` in the `justfile` for `ggml-small` or `ggml-base` — smaller
models trade accuracy for a realtime-capable CPU path.

## What does NOT work

- Compositors other than Hyprland (window routing and dispatch are hyprctl)
- Voice **commands** in Japanese/Chinese — transcription and dictation work,
  but the command matcher assumes space-separated alphabetic script
- Only NVIDIA/CUDA has been tested for GPU inference

## Development

`BACKLOG.md` is the roadmap; `AGENTS.md` orients coding agents and
contributors. Design decisions in the backlog carry the measurements that
justify them — thresholds here were tuned against real windows, not intuition.

## License

MIT
