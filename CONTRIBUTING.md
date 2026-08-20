# Contributing

## Setup

Everything runs through Nix and `just`:

```sh
nix develop        # just and cargo only exist inside the dev shell
just fetch-model   # ~874 MB whisper model, one-off
just run           # CUDA path; `just run-cpu` without a GPU
```

CI is deliberately absent: the test suite and the model downloads are heavy,
and the app only does anything useful on a local Hyprland session anyway.
The gate you must run before sending anything is local:

```sh
just check   # limits + refs + vocab + cargo check + clippy -D warnings + fmt + test
```

## Rules this repo enforces

`just check` is not advisory. It fails when:

- a source file passes 400 lines (`limits` — split the module, see M0.2 in
  BACKLOG.md)
- a doc references a `file:line` whose line no longer names the symbol
  (`refs` — fix the doc, not the gate)
- spoken vocabulary or an application name appears in a `src/` string
  literal (`vocab` — it belongs in `commands.toml`, see M1.3 and M2.1 in
  BACKLOG.md)

Beyond the gates:

- **BACKLOG.md is the source of truth for what was built and why.** Every
  design decision there is backed by a measurement. Do not override one
  without a measurement of your own.
- New matching thresholds ship with the measurement that justifies them.
- No per-utterance model inference in latency-sensitive paths — that is why
  the LLM classifier was removed (M0.1).
- Tests for any matching or parsing change; fixtures for anything that
  reads `hyprctl` JSON live in `tests/fixtures/`.

## Language policy

README.md and user-facing docs are in English; code comments, commit
messages and BACKLOG.md are in Portuguese. Both are fine in issues and pull
requests — write in whichever you think more clearly in.

## Platform ports

External commands are isolated behind `CommandRunner` and per-platform
adapters (see `Injector` in src/input/inject.rs). A new platform is a new
impl plus its tests against `FakeRunner` — never an `#ifdef` scattered
through call sites. See M8 in BACKLOG.md for the worked example.
