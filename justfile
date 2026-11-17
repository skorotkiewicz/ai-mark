set shell := ["bash", "-cu"]

build:
    cargo build --release

install:
    cargo install --path .

demo: install
    #!/usr/bin/env bash
    set -euo pipefail
    d=$(mktemp -d)
    cd "$d"
    git init -q
    printf 'fn main() {\n    println!("written by a human");\n}\n' > main.rs
    aimark checkpoint
    printf 'fn main() {\n    println!("written by a human");\n    println!("written by an LLM");\n}\n' > main.rs
    aimark record --harness demo --model big-model --files main.rs
    aimark blame main.rs
    aimark stat
