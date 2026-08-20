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

Two, and they differ by the only thing that matters: whose voice is being
listened to.

| Mode | What it does |
|---|---|
| **Microphone** | Your speech accumulates as text until a send word; a short utterance the window grammar recognises navigates instead of being written down |
| **System audio** | A call or a video, subtitled — and translated for display when a model is installed. The microphone stays on, so a recording holds both sides |

There were four. `Input` typed each utterance where `Enter` accumulated them —
same source, same grammar, one word apart — and `Command` dispatched window
commands where `Enter` already does. Leaving a half-composed message to switch
modes and switch back is friction the word-count gate makes unnecessary: a
command is a handful of words, a dictated line is not, and the grammar refuses
anything that is not literally in it. See M4.5 in `BACKLOG.md`.

Spoken phrases from before the merge (`modo comando`, `modo ditado`) still
resolve, to the mode that absorbed them.

## Voice commands (Portuguese defaults)

| Say | Effect |
|---|---|
| "câmbio" / "envia" / "enviar" / "manda" / "pronto" / "pode enviar" | send the buffered text |
| "cancela" / "limpar" / "descarta" / "apaga" / "esquece" | discard the buffer |
| "ajuda" / "comandos" | list what can be said, built from the live vocabulary |
| "modo microfone" / "modo reunião" | switch mode without touching the overlay |
| "grava" / "encerra" | open and close a recorded session |
| "nova linha" / "pula linha" | line break |
| "envia para \<alvo\>" | send buffer to a window ("navegador", "terminal", "teams"…) |
| "monitor \[da\] direita / esquerda / \[do\] meio / centro" | focus monitor by physical position |
| "monitor \<marca\>" | focus monitor by brand ("monitor samsung") |
| "janela da esquerda / de cima …" | move window focus |
| "workspace 3" / "área 3" / "vai pra 3" | switch workspace |
| "vai pro \<alvo\>" / "abre o \<alvo\>" / "mostra o \<alvo\>" | focus a window, or a browser tab when no window answers |
| "trocar tela" / "próxima janela" | cycle windows in the workspace |
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

`fillers` is the list of discourse words stripped before the word-count gate
counts. It exists because people do not speak in isolated keywords — they say
"ok, câmbio" and "limpar tudo", and the gate refused both before scoring
anything. Only words the vocabulary names are removed, which is why "vamos
limpar depois" stays dictation.

### Which languages you actually speak

```toml
[asr]
languages = ["pt", "en"]   # empty accepts anything
```

Whisper will name any of its hundred languages on thin evidence, and three
seconds of speech is thin evidence: a Portuguese sentence came back as German
at p = 0.198, in German. A detection outside this list is treated as noise
rather than disagreement — ignored outright, which also lets a real run settle
sooner, because a stray reading no longer resets the streak.

### Browser tabs

A whole browser is one window to the compositor, so anything kept in a tab is
invisible to `hyprctl clients` — which is how most people keep most things.
Chromium exposes its tabs over the DevTools endpoint:

```toml
[browser]
debug_port = 9222   # launch the browser with --remote-debugging-port=9222
```

Off by default: nobody should have a debugging port opened on their behalf. A
closed port is the normal case, not a failure — the target simply falls back
to the window list. Windows are always tried first, because a real window is a
stronger answer than a page inside one.

### Where the overlay sits

```toml
[overlay]
monitor = "middle"   # or left, right, focused, or a connector name like DP-1
```

Monitors are ordered by their global x, left to right — not by the order
hyprctl reports them, which is connector order and put the overlay on the
laptop panel regardless of where it sat on the desk.

### Segmentation

How long an utterance runs before it is finalized. Following someone else
speak and speaking yourself want opposite settings, so there are two
profiles — and both are config, not code, because the right values depend on
how the people around you actually talk:

```toml
[segmentation.dictation]   # you, into a window
hang_ms = 960              # silence before the utterance is considered done
max_seconds = 20           # hard cap when no pause ever comes
partial_every_ms = 900

[segmentation.subtitle]    # a meeting, a video — System audio mode
hang_ms = 320
max_seconds = 8
partial_every_ms = 700
```

A meeting rarely offers 960 ms of silence, so a dictation-tuned `hang_ms`
runs every segment to the cap and produces a wall of text that also mixes
several speakers into one block. Shorter `hang_ms` catches the brief pause at
a speaker change, which gives phrase-sized subtitles and one voice per
segment. If subtitles feel chopped mid-sentence, raise it; if they arrive in
paragraphs, lower it.

## Requirements

- **NixOS with flakes** (the dev shell provides the whole toolchain)
- **Hyprland on Wayland** — `hyprctl` and `wtype` are assumed
- **NVIDIA GPU** for the default CUDA build (tested: RTX 3070 Ti Laptop,
  driver via `hardware.graphics.enable`); a CPU build exists, see below

## Running

```bash
nix develop       # required: just and cargo live only inside the dev shell
just run          # downloads ggml-large-v3-turbo-q8_0.bin (~874 MB) on first use
```

`just` is **not** installed system-wide — it comes from the flake. Outside the
shell, prefix commands with `nix develop --command`:

```bash
nix develop --command just check
```

With [direnv](https://direnv.net) + `nix-direnv`, the committed `.envrc`
enters the shell automatically on `cd` (run `direnv allow` once):

```nix
# NixOS configuration
programs.direnv = { enable = true; nix-direnv.enable = true; };
```

`just --list` shows every recipe. `just check` runs the full gate suite:
clippy, fmt, tests, a per-file size ceiling, a hardcoded-vocabulary ban, and
a docs reference checker.

## CPU fallback

```bash
just run-cpu
```

Measured with `large-v3-turbo` Q8 on this repo's ignored benchmark
(`measure_transcribe_latency`, 3 s of audio):

| Build | 3 s of audio | Model load | Hardware |
|---|---|---|---|
| CUDA (default) | **238–354 ms** | 2.8 s | RTX 3070 Ti Laptop |
| CPU (`just run-cpu`) | ~51 s* | 1.6 s | i7-12700H, 20 threads |

\* **Measured on a contended machine** — a low power profile was active and a
SQL Server VM was running, so treat it as a lower bound on what the CPU path
can do, not as this CPU's capability. The benchmark now prints the power
profile, RAPL limit and load average alongside the timing, and warns when the
load makes the number unrepresentative; re-run it on an idle machine before
quoting it.

The direction still holds: `large-v3-turbo` is far from realtime on CPU and
cannot drive live partials. Without an NVIDIA GPU, swap `model_name` in the
`justfile` for `ggml-small` or `ggml-base` — smaller models trade accuracy
for a realtime-capable CPU path.

## Hyprland window rules (recommended)

The overlay makes itself floating and pinned at startup, but that happens
*after* Hyprland has already mapped and tiled the window — you see a frame of
it wedged into the layout. A window rule applies at map time and removes that
entirely:

```
# ~/.config/hypr/hyprland.conf
windowrulev2 = float, class:^(oc-voice)$
windowrulev2 = pin, class:^(oc-voice)$
windowrulev2 = noborder, class:^(oc-voice)$
windowrulev2 = noshadow, class:^(oc-voice)$
windowrulev2 = nofocus, class:^(oc-voice)$
```

`nofocus` is the one worth understanding: without it the overlay steals focus
when it appears, and Enter-mode text then lands in the overlay instead of the
window you were working in.

The built-in fallback stays for anyone without the rule, and it is idempotent
— it reads the window's actual state and applies only what is missing, so it
never fights the rule.

## Diagnostics

Subcommands that answer "why is it doing that?" without starting the pipeline:

```bash
just devices        # which microphone capture would use, and the alternatives
just levels         # live meter of the signal whisper receives — speak and watch
just probe          # type utterances, see the matcher's decision chain with scores
just asr-test       # read the reference passage aloud, get word error rate per block
just voices         # does the speaker model load here, and what does it declare
just ocr <alvo>     # what OCR reads off a window, with the geometry of each line
just ocr-changes    # rank a window's lines by how much they move
```

`just probe` is the one to reach for when a command does not fire. It shows
every class the classifier consults with its score, and whether the utterance
would navigate or become text. Three bugs in one week were diagnosed with it in
under a minute each, and one of them existed only because the probe was not
listing every class — an instrument that hides a class hides the bug in it.

`just levels` is the first thing to run when transcription is poor: speech
should sit around **-25 to -15 dBFS RMS**. A quiet room reads near -60. If
speaking barely moves the meter, no model or threshold change will help —
raise the source volume (`wpctl set-volume <id> 1.5`) or pick another mic.

`just asr-test` reads `tests/fixtures/passagem.txt`, six blocks that isolate
different failure sources — command vocabulary, window commands, natural
Portuguese, Portuguese with English technical terms, numbers, and confusable
pairs. Per-block word error rate says *where* the ASR fails. Below 10% is
usable; above 25% the problem is signal or model, not tuning.

## What does NOT work

- Compositors other than Hyprland (window routing and dispatch are hyprctl).
  Text injection is the exception: it probes for `wtype`, then `xdotool`, so
  dictation works on X11. Two of the three wlroots dependencies — injection
  and screen capture — have a standard `xdg-desktop-portal` path that is not
  used here; listing windows has no portal at all, and that one is a real
  Wayland gap rather than a shortcut.
- macOS. See `README-MAC.md` for the map. Nothing is impossible there and
  several parts would be better, but it is a second platform backend, not a
  build flag.
- Voice **commands** in Japanese/Chinese — transcription and dictation work,
  but the command matcher assumes space-separated alphabetic script
- Only NVIDIA/CUDA has been tested for GPU inference

## Development

`BACKLOG.md` is the roadmap; `AGENTS.md` orients coding agents and
contributors. Design decisions in the backlog carry the measurements that
justify them — thresholds here were tuned against real windows, not intuition.

## License

MIT
