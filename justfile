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

# Interactive matcher probe: type utterances, see the whole decision chain
# with scores. Reads live windows/monitors; never dispatches or types.
probe:
    cargo run --release -- probe

probe-en:
    cargo run --release -- probe en

run: fetch-model
    cargo run --release -- {{ models_dir }}/{{ model_name }}

run-cpu: fetch-model
    cargo run --release --no-default-features --features cpu -- {{ models_dir }}/{{ model_name }}

check: limits refs vocab
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

# Fail on spoken vocabulary or known app names in src/ string literals.
# Green since M2.1 moved the vocabulary to commands.toml, and part of
# `just check` from then on. #[cfg(test)] blocks are skipped: matcher tests
# must contain the words they match against.
vocab:
    #!/usr/bin/env bash
    set -uo pipefail
    words_pt='envia|manda|cambio|pronto|cancela|limpa|descarta|nova linha|pula linha'
    words_app='navegador|terminal|editor|chat|browser|firefox|chrome|chromium|opencode|oc-opencode|alacritty|vscode|code|discord|telegram|whatsapp'
    # Test code legitimately contains vocabulary — it is what the matcher tests
    # assert against. Strip #[cfg(test)] blocks before scanning.
    scan=$(mktemp -d)
    # *_tests.rs files are whole test modules (declared with #[path] from a
    # #[cfg(test)] mod) — same exemption as inline test blocks.
    for f in $(find src -name '*.rs' ! -name '*_tests.rs'); do
        awk -v path="$f" '
            /^#\[cfg\(test\)\]/ { skip=1 }
            skip && /^}/ { skip=0; next }
            !skip { print path ":" NR ":" $0 }
        ' "$f"
    done > "$scan/all.txt"
    hits=$(grep -niE "\"[^\"]*(${words_pt}|${words_app})[^\"]*\"" "$scan/all.txt" \
        | sed 's/^[0-9]*://' \
        | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)
    rm -rf "$scan"
    if [ -n "$hits" ]; then
        n=$(printf '%s\n' "$hits" | wc -l)
        echo "spoken vocabulary / app names in src/ string literals ($n occurrences):"
        printf '%s\n' "$hits"
        echo "move the vocabulary to commands.toml — see M1.3 and M2.1 in BACKLOG.md"
        exit 1
    fi
    echo "vocab ok: no spoken vocabulary or app names in src/ string literals"

# Fail on stale file:line references in the docs. Every `file:line` reference
# must name its symbol in backticks next to it — that is what gets checked.
# See M0.4 in BACKLOG.md.
refs:
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    ref_re='`([A-Za-z0-9_./-]+\.[a-z0-9]+):([0-9]+)`'
    for doc in *.md; do
        lineno=0
        while IFS= read -r line || [ -n "$line" ]; do
            lineno=$((lineno + 1))
            rest="$line"
            while [[ $rest =~ $ref_re ]]; do
                file="${BASH_REMATCH[1]}"
                ln="${BASH_REMATCH[2]}"
                before="${rest%%"${BASH_REMATCH[0]}"*}"
                rest="${rest#*"${BASH_REMATCH[0]}"}"
                sym=""
                if [[ $before =~ \`([^\`]+)\`[^\`]*$ ]]; then
                    sym="${BASH_REMATCH[1]}"
                elif [[ $rest =~ ^[^\`]*\`([^\`]+)\` ]]; then
                    sym="${BASH_REMATCH[1]}"
                fi
                if [ ! -f "$file" ]; then
                    echo "$doc:$lineno: \`$file:$ln\` — file not found"
                    fail=1
                    continue
                fi
                total=$(wc -l < "$file")
                if [ "$ln" -gt "$total" ]; then
                    echo "$doc:$lineno: \`$file:$ln\` — $file has only $total lines"
                    fail=1
                    continue
                fi
                if [ -z "$sym" ]; then
                    echo "$doc:$lineno: \`$file:$ln\` — name the symbol in backticks next to the reference"
                    fail=1
                    continue
                fi
                if ! sed -n "${ln}p" "$file" | grep -qF -- "$sym"; then
                    echo "$doc:$lineno: \`$file:$ln\` — line no longer mentions \`$sym\`:"
                    sed -n "${ln}p" "$file" | sed 's/^/    /'
                    fail=1
                fi
            done
        done < <(awk '{ if (/^```/) { fenced = !fenced; print ""; next } if (fenced) print ""; else print }' "$doc")
    done
    if [ "$fail" -ne 0 ]; then
        echo "stale doc references — fix the symbol or the file:line"
        exit 1
    fi
    echo "refs ok: every file:line in the docs still names its symbol"

fmt:
    cargo fmt

clean:
    cargo clean
    rm -rf {{ models_dir }}
