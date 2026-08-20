set shell := ["bash", "-euo", "pipefail", "-c"]

models_dir := "./models"
# large-v3-turbo Q8_0: quantized (874 MB), ~2x faster than F16 with minimal quality loss
model_name := "ggml-large-v3-turbo-q8_0.bin"
model_url := "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/" + model_name
# Qwen 3.5 0.8B Q4_K_M: command classifier (~533 MB)
llm_name := "Qwen3.5-0.8B-Q4_K_M.gguf"
llm_url := "https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/" + llm_name

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

fetch-llm:
    mkdir -p {{ models_dir }}
    if [ ! -f {{ models_dir }}/{{ llm_name }} ]; then \
        echo "downloading {{ llm_name }}..."; \
        curl -L -o {{ models_dir }}/{{ llm_name }} {{ llm_url }}; \
    else \
        echo "{{ llm_name }} already present"; \
    fi

build:
    cargo build --release

run: fetch-model
    cargo run --release -- {{ models_dir }}/{{ model_name }}

run-cpu: fetch-model
    cargo run --release --no-default-features --features cpu -- {{ models_dir }}/{{ model_name }}

check:
    cargo check
    cargo clippy -- -D warnings
    cargo fmt --check

fmt:
    cargo fmt

clean:
    cargo clean
    rm -rf {{ models_dir }}
