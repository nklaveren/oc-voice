# Security Policy

## What this app touches

oc-voice captures your microphone (and, in Translate mode, system audio),
transcribes it locally, and can inject keystrokes and dispatch window-manager
commands. All processing is on-device: whisper.cpp, silero VAD and CTranslate2
run locally and nothing is sent over the network. Session recordings are
written to the local disk only.

The trust boundaries worth auditing:

- **Text injection** (`wtype`/`xdotool` on Linux, `osascript` on macOS):
  transcribed speech is typed into other applications. Injection goes through
  the `CommandRunner` abstraction in `src/process.rs`; `DryRunRunner` blocks
  anything mutating.
- **Command matching**: a misheard utterance can dispatch a WM action.
  Destructive commands (closing a window) always ask for spoken confirmation
  first — see `src/commands/execute.rs`.
- **External processes**: everything shelled out is a literal command string
  built in `src/`; user speech is passed as a single argument, never
  interpolated into a shell.

## Reporting a vulnerability

Please do not open a public issue. Email the maintainer (see the commit
author address) with a description and, if possible, a reproduction. You can
expect an acknowledgement within a few days.

## Scope

Only the latest commit on the default branch is supported. This is a
single-maintainer project; there are no backport releases.
