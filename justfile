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

# Convert the en->pt translation model into models/ct2-en-pt (one-off,
# needs python; the runtime itself is pure Rust via ct2rs)
fetch-mt:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -d {{ models_dir }}/ct2-en-pt ]; then echo "ct2-en-pt already present"; exit 0; fi
    export HF_HOME={{ models_dir }}/.hf
    nix shell --impure --expr 'let p = import <nixpkgs> {}; in p.python3.withPackages (ps: [ps.ctranslate2 ps.transformers ps.sentencepiece ps.torch])' --command bash -c '
        ct2-transformers-converter --model Helsinki-NLP/opus-mt-tc-big-en-pt \
            --output_dir {{ models_dir }}/ct2-en-pt --quantization int8 --force
        # The converter drops the tokenizers; ct2rs needs them beside the model.
        find {{ models_dir }}/.hf -name "source.spm" -exec cp {} {{ models_dir }}/ct2-en-pt/ \;
        find {{ models_dir }}/.hf -name "target.spm" -exec cp {} {{ models_dir }}/ct2-en-pt/ \;
    '
    rm -rf {{ models_dir }}/.hf
    du -sh {{ models_dir }}/ct2-en-pt

# Download the Whisper model into models/
fetch-model:
    mkdir -p {{ models_dir }}
    if [ ! -f {{ models_dir }}/{{ model_name }} ]; then \
        echo "downloading {{ model_name }}..."; \
        curl -L -o {{ models_dir }}/{{ model_name }} {{ model_url }}; \
    else \
        echo "{{ model_name }} already present"; \
    fi

# Release build
build:
    cargo build --release

# Read the reference passage aloud; reports word error rate per block
asr-test: fetch-model
    cargo run --release -- asr-test {{ models_dir }}/{{ model_name }}

# Live meter of the signal whisper receives — speak and watch the level
levels:
    cargo run --release -- levels

# List audio inputs and show which mic capture would use
devices:
    cargo run --release -- devices

# Type utterances, see the matcher's whole decision chain with scores
probe:
    cargo run --release -- probe

# Same as `probe`, against the English vocabulary
probe-en:
    cargo run --release -- probe en

# Answers this before anything is built on top of the model: a second
# onnxruntime consumer in one process is what broke the VAD in af4da28.
# Does the speaker-embedding model load here, and what does it declare?
voices:
    cargo run --release -- voices

# What OCR reads off a window, with the geometry to grab each line on its own
ocr alvo="teams":
    cargo run --release -- ocr {{ alvo }}

# Sample one region until Ctrl+C, printing only when the text changes
ocr-watch region:
    cargo run --release -- ocr watch "{{ region }}"

# In a running meeting the active speaker's name is the thing that changes
# while the toolbar and the participant list sit still, so the region can
# announce itself instead of someone eyeballing a 72-line dump.
# Rank a window's lines by how much they actually move
ocr-changes alvo="teams" segundos="60":
    cargo run --release -- ocr changes {{ alvo }} {{ segundos }}

# Fetch the model if needed, then run (CUDA)
run: fetch-model
    cargo run --release -- {{ models_dir }}/{{ model_name }}

# Run on CPU — ~17x slower than realtime, see README
run-cpu: fetch-model
    cargo run --release --no-default-features --features cpu -- {{ models_dir }}/{{ model_name }}

# Full gate suite: limits, refs, vocab, check, clippy, fmt, test
check: limits refs vocab
    cargo check
    cargo clippy --all-targets -- -D warnings
    cargo fmt --check
    cargo test

# Fail if any source file grew past the module size ceiling
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

# Fail on spoken vocabulary or app names hardcoded in src/ (test code exempt)
vocab:
    #!/usr/bin/env bash
    set -uo pipefail
    words_pt='envia|manda|cambio|pronto|cancela|limpa|descarta|nova linha|pula linha'
    # `code` is bounded because it is a word inside ordinary ones: it fired on
    # `-acodec` and on "could not decode", neither of which names an app. The
    # rest stay unbounded — nothing legitimate contains "whatsapp".
    words_app='navegador|terminal|editor|chat|browser|firefox|chrome|chromium|opencode|oc-opencode|alacritty|vscode|\\bcode\\b|discord|telegram|whatsapp'
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

# Fail on stale file:line references in the docs (M0.4)
refs:
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    ref_re='`([A-Za-z0-9_./-]+\.[a-z0-9]+):([0-9]+)`'
    bad_re='`([A-Za-z0-9_./-]+\.[a-z0-9]+):([^`0-9][^`]*)?`'
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
            # A reference whose line number is missing or not a number slips
            # past ref_re entirely — the gate sees nothing and reports ok.
            # Found by breaking one by hand: `src/commands/mod.rs:` passed.
            # A gate that can be silenced by malforming its input is worse
            # than no gate, because it is trusted.
            if [[ $line =~ $bad_re ]]; then
                echo "$doc:$lineno: \`${BASH_REMATCH[1]}:${BASH_REMATCH[2]}\` — malformed reference, expected file:line"
                fail=1
            fi
        done < <(awk '{ if (/^```/) { fenced = !fenced; print ""; next } if (fenced) print ""; else print }' "$doc")
    done
    if [ "$fail" -ne 0 ]; then
        echo "stale doc references — fix the symbol or the file:line"
        exit 1
    fi
    echo "refs ok: every file:line in the docs still names its symbol"

# Read the diff the failing test printed before running this — the snapshot
# exists to make a silent change loud, and blindly re-recording turns it off.
# Re-record the dispatch snapshot after an INTENTIONAL binding change
snapshot:
    UPDATE_SNAPSHOT=1 cargo test the_whole_vocabulary

# Apply rustfmt
fmt:
    cargo fmt

# Remove build artifacts and downloaded models
clean:
    cargo clean
    rm -rf {{ models_dir }}
