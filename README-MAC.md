# oc-voice no macOS

Guia do que precisa mudar para rodar o oc-voice no macOS. O projeto nasceu
para Hyprland/PipeWire (veja `README.md`); este documento mapeia cada
dependência de plataforma e o substituto no macOS.

## O que já funciona sem mudança de código

| Componente | Como | Observação |
|---|---|---|
| Captura de microfone | cpal → CoreAudio | `default_host()` (`src/audio/capture.rs:17`) resolve para CoreAudio no mac |
| VAD (silero v5) | ort `load-dynamic` | Precisa apontar `ORT_DYLIB_PATH` para um `libonnxruntime.dylib` |
| Whisper ASR | whisper-rs | Tem features `metal` e `coreml` (verificado em whisper-rs 0.14) |
| Overlay | eframe/egui | winit usa Cocoa no mac; as features `wayland`/`x11` do Cargo.toml são aditivas, não quebram o build |
| Tradução en→pt | ct2rs | Rust puro, portável |
| Matcher de comandos | strsim | Rust puro, portável |

## O que é Linux-only e precisa de substituto

| Componente | Hoje (Linux) | Substituto no macOS | Onde no código |
|---|---|---|---|
| Controle de janelas/WM | `hyprctl` | [Aerospace](https://github.com/nikitabobko/AeroSpace) (`aerospace` CLI) ou [yabai](https://github.com/kasm/yabai) + skhd | `src/wm/dispatch.rs`, `src/wm/hyprland.rs`, `src/wm/target.rs`, `dispatch_spoken` (`src/wm/dispatch.rs:120`) |
| Injeção de texto | `wtype`, `xdotool` | `osascript` (`tell application "System Events" to keystroke ...`) ou crate `core-graphics` (CGEvent) | `src/input/inject.rs` |
| Captura de áudio do sistema (modo Translate) | `pw-record` + `pw-dump` | [BlackHole](https://github.com/ExistentialAudio/BlackHole) (dispositivo virtual → capturar como input cpal) ou ScreenCaptureKit | `pw-dump` (`src/audio/capture.rs:289`) (`run_capture_system`) |
| Screenshot p/ OCR | `grim -g` | `screencapture -R x,y,w,h <arquivo>` | `grim` (`src/ocr.rs:92`) |
| OCR | `tesseract` | `tesseract` via brew (mesmo binário) | `src/ocr.rs` |
| GPU no whisper | CUDA (feature default) | Metal ou CoreML | `Cargo.toml` `[features]` |
| Dev shell | Nix flake com alsa/pipewire/cuda/wayland | brew + rustup, ou flake com `pkgs.stdenv.isDarwin` condicional | `flake.nix` |

## Mudanças de build

### 1. Cargo.toml — features de GPU

Hoje:

```toml
[features]
default = ["cuda"]
cuda = ["whisper-rs/cuda"]
cpu = []
```

Adicionar:

```toml
metal = ["whisper-rs/metal"]
```

E em `src/pipeline/mod.rs:54`, o gate `#[cfg(feature = "cuda")]` que chama
`ctx_params.use_gpu(true)` precisa cobrir metal também
(`#[cfg(any(feature = "cuda", feature = "metal"))]`).

Rodar no mac:

```sh
cargo run --release --no-default-features --features metal -- models/ggml-large-v3-turbo-q8_0.bin
```

### 2. onnxruntime

`voice_activity_detector` usa `ort` com `load-dynamic`: o runtime é carregado
de `ORT_DYLIB_PATH` em tempo de execução. No mac:

```sh
brew install onnxruntime
export ORT_DYLIB_PATH="$(brew --prefix onnxruntime)/lib/libonnxruntime.dylib"
```

(ou baixar o release oficial do GitHub onnx/onnxruntime para osx-arm64).

### 3. Dependências de sistema

```sh
brew install onnxruntime tesseract   # tesseract só se for usar o OCR
# BlackHole via: brew install blackhole-2ch  (só para o modo Translate)
```

O `flake.nix` atual lista `alsa-lib`, `pipewire`, `cudaPackages`, `wayland`,
`wtype`, `grim` — todos Linux-only. Ou o flake ganha um bloco condicional
`pkgs.stdenv.isDarwin`, ou no mac se usa brew + rustup direto.

## Mudanças de código, por prioridade

A abstração `CommandRunner` (`src/process.rs:4`) já isola toda chamada de
processo externo — os comandos são strings shelled out, então boa parte do
port é trocar strings de comando atrás de um `cfg!(target_os = "macos")`,
sem reescrever lógica.

### Fase 0 — ditado mínimo (mic → whisper → stdout/overlay)

Nenhuma mudança além do build. O overlay e o espelho em stdout já funcionam.
Os modos que tocam janelas falham silenciosamente (os helpers checam
`wtype` (`src/input/inject.rs:8`) e `hyprctl` (`src/wm/hyprland.rs:34`) antes de agir).

### Fase 1 — injeção de texto (modos Input/Enter)

`src/input/inject.rs`: adicionar branch macOS antes dos fallbacks Linux:

- `type_text` → `osascript -e 'tell application "System Events" to keystroke "..."'`
  (cuidado com escaping de aspas; alternativa mais robusta: CGEvent via
  crate `core-graphics`, sem processo por tecla)
- `type_key` / `type_shift_return` → `key code 36` (Return) com `using shift down`

Requer permissão de **Acessibilidade** (Settings → Privacy & Security →
Accessibility) para o terminal/app que roda o oc-voice.

### Fase 2 — controle de janelas (modo Command)

`hyprctl` → `aerospace` (recomendado: CLI estável, JSON com `--json`) ou
`yabai`:

| hyprctl | aerospace |
|---|---|
| `hyprctl clients -j` | `aerospace list-windows --all --json` |
| `hyprctl monitors -j` | `aerospace list-monitors --json` |
| `dispatch workspace N` | `aerospace workspace N` |
| `dispatch focuswindow` | `aerospace focus --window-id <id>` |
| `dispatch movefocus l/r/u/d` | `aerospace focus left/right/up/down` |

O resolvedor de alvos (`src/wm/target.rs`) roda sobre structs próprias
preenchidas do JSON do hyprctl — portar é escrever um parser do JSON do
aerospace para as mesmas structs. Os testes usam fixtures em
`tests/fixtures/`; adicionar um fixture `aerospace_windows.json` cobre o
resolvedor no mac sem sessão gráfica.

Aerospace precisa de permissão de **Acessibilidade**; yabai precisa disso e,
para parte das funções, desabilitar parcialmente o SIP — por isso Aerospace é
o caminho de menor atrito.

### Fase 3 — modo Translate (áudio do sistema)

`pw-record` não existe no mac. Opções:

1. **BlackHole** (2ch/16ch): cria um dispositivo de áudio virtual. O usuário
   aponta a saída do sistema (ou um Multi-Output Device) para ele, e o
   oc-voice captura como **input** via cpal — reutiliza `run_capture`
   (caminho do microfone), só precisa de seleção de dispositivo por nome,
   que hoje não existe (sempre `default_input_device`).
2. **ScreenCaptureKit** (`screencapturekit` crate): captura áudio do sistema
   nativamente, sem dispositivo virtual, mas é uma integração maior (bindings
   Swift/ObjC) e requer permissão de **Gravação de Tela**.

Começar por BlackHole: zero código novo de captura, só seleção de device.

### Fase 4 — OCR (nomes de falantes em calls)

`grim` (`src/ocr.rs:92`) chama `grim -g "<x>,<y> <w>x<h>" <path>` →
`screencapture -R<x>,<y>,<w>,<h> <path>`. Atenção às unidades: `grim -g`
recebe pixels lógicos e escreve físicos (o código já compensa scale);
`screencapture -R` trabalha em pontos, com o mesmo ajuste em telas Retina.
`tesseract` é o mesmo binário via brew. Requer permissão de **Gravação de Tela**.

## Permissões do macOS (checklist)

- **Microfone** — pedida na primeira captura (obrigatória para tudo)
- **Acessibilidade** — injeção de texto e Aerospace/yabai
- **Gravação de Tela** — OCR e ScreenCaptureKit (se for por esse caminho)

## Notas de portabilidade

- `src/audio/capture.rs:43` menciona `wpctl` nas mensagens de ajuda — texto
  Linux-only, mas cosmético.
- O overlay usa `hyprctl` para pinar/posicionar a janela flutuante
  (`src/wm/hyprland.rs`); sem hyprctl ele se comporta como janela normal.
  No mac o posicionamento ficaria a cargo do Aerospace ou de regras do
  próprio egui/winit.
- Nada no pipeline de áudio/ASR é Linux-only — o port é essencialmente
  "bordas" (injeção, WM, áudio do sistema, screenshot), não o núcleo.
