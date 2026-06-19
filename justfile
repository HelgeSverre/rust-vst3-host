# vst3-host — safe VST3 hosting library + inspector app

PLUGIN := "test_plugins/Dexed.vst3"

# Show available recipes
[private]
default:
    @just --list

# `just lint` is an alias for `just clippy`
alias lint := clippy

# Build the whole workspace
[group('build')]
build:
    cargo build --workspace

# Build the workspace in release mode
[group('build')]
build-release:
    cargo build --workspace --release

# Build the process-isolation helper binary
[group('build')]
helper:
    cargo build -p vst3-host --features process-isolation --bin vst3-host-helper

# Run the full test suite (all features)
[group('test')]
test:
    cargo test --workspace --all-features

# Run the (ignored) process-isolation capstones — needs the helper + a test plugin
[group('test')]
test-isolation: helper
    cargo test -p vst3-host --features process-isolation --test integration_tests -- --ignored isolation

# Launch the VST3 inspector app
[group('run')]
inspector:
    cargo run -p vst3-inspector --release --bin vst3-inspector

# Play a synth through the default audio device (defaults to bundled Dexed)
[group('run')]
play PLUGIN_PATH=PLUGIN:
    cargo run -p vst3-host --example play_synth -- "{{ PLUGIN_PATH }}"

# Headless self-test: drive the library through the inspector binary (no GUI)
[group('test')]
selftest PLUGIN_PATH=PLUGIN:
    cargo run -p vst3-inspector --bin vst3-inspector -- --selftest "{{ PLUGIN_PATH }}"

# Compile + unit-test the library on Linux in Docker — verifies cross-platform builds
# (c_char signedness) and that the X11/XCB editor path compiles. Needs Docker.
[group('test')]
linux-check:
    #!/usr/bin/env bash
    set -euo pipefail
    docker run --rm -v "$PWD":/work -w /work \
      -e CARGO_TARGET_DIR=/tmp/target -e VST3_SDK_DIR=/work/vst3sdk \
      rust:bookworm bash -c '
        apt-get update -qq && \
        apt-get install -y -qq libclang-dev clang libxcb1-dev libxcb-util-dev libasound2-dev pkg-config && \
        cargo build -p vst3-host --all-features && \
        cargo test -p vst3-host --all-features --lib'

# Load + drive a plugin in an isolated process (defaults to bundled Dexed)
[group('run')]
isolated PLUGIN_PATH=PLUGIN: helper
    cargo run -p vst3-host --example isolated_host --features process-isolation -- "{{ PLUGIN_PATH }}"

# Format the code
[group('lint')]
fmt:
    cargo fmt

# Check formatting without writing
[group('lint')]
fmt-check:
    cargo fmt --check

# Lint with clippy (all features)
[group('lint')]
clippy:
    cargo clippy --workspace --all-features

# Pre-merge gate: formatting + clippy + tests
[group('lint')]
check: fmt-check clippy test
