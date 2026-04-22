set shell := ["bash", "-euo", "pipefail", "-c"]

models_dir := "./models"
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

check:
    cargo check
    cargo clippy -- -D warnings
    cargo fmt --check

fmt:
    cargo fmt

clean:
    cargo clean
    rm -rf {{ models_dir }}
