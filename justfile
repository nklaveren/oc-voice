set shell := ["bash", "-euo", "pipefail", "-c"]

models_dir := "./models"
# Module size ceiling. A file past this is a module waiting to be split — see
# M0.2 in BACKLOG.md. Enforced by `just limits`, which `just check` runs.
max_file_lines := "400"
# large-v3-turbo Q8_0: quantized (874 MB), ~2x faster than F16 with minimal quality loss
model_name := "ggml-large-v3-turbo-q8_0.bin"
model_url := "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/" + model_name

default:
    @just --list

fetch-model:
    mkdir -p {{ models_dir }}
    if [ ! -f {{ models_dir }}/{{ model_name }} ]; then \
        echo "downloading {{ model_name }}..."; \
        curl -L -o {{ models_dir }}/{{ model_name }} {{ model_url }}; \
    else \
        echo "{{ model_name }} already present"; \
    fi

build:
    cargo build --release

run: fetch-model
    cargo run --release -- {{ models_dir }}/{{ model_name }}

run-cpu: fetch-model
    cargo run --release --no-default-features --features cpu -- {{ models_dir }}/{{ model_name }}

check: limits
    cargo check
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check
    cargo test

# Fail if any source file grew past the module size ceiling.
limits:
    #!/usr/bin/env bash
    set -uo pipefail
    over=""
    for f in $(find src -name '*.rs' | sort); do
        n=$(wc -l < "$f")
        if [ "$n" -gt {{ max_file_lines }} ]; then
            over+=$(printf '  %-28s %5s lines\n' "$f" "$n")$'\n'
        fi
    done
    if [ -n "$over" ]; then
        echo "files over the {{ max_file_lines }}-line ceiling:"
        printf '%s' "$over"
        echo "split them into modules — see M0.2 in BACKLOG.md"
        exit 1
    fi
    echo "limits ok: no source file over {{ max_file_lines }} lines"

fmt:
    cargo fmt

clean:
    cargo clean
    rm -rf {{ models_dir }}
