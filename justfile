set shell := ["bash", "-cu"]

build:
    cargo build --locked --release

build-dev:
    cargo build --locked

install:
    cargo install --path . --locked

check:
    cargo fmt --all --check
    cargo clippy --locked --all-targets -- -D warnings

test:
    cargo test --locked

check-ui:
    npm run check
    npm run test:pi
    npm run test:browser

demo: build-dev
    bash scripts/demo.sh "{{justfile_directory()}}/target/debug/aimark"

demo-dashboard: build-dev
    bash scripts/demo.sh "{{justfile_directory()}}/target/debug/aimark" dashboard
