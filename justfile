# VST3 host library and inspector

PLUGIN := "test_plugins/Dexed.vst3"

# List recipes
[private]
default:
    @just --list

# Alias for `clippy`
alias lint := clippy

# Build workspace
[group('build')]
build:
    cargo build --workspace

# Build release workspace
[group('build')]
build-release:
    cargo build --workspace --release

# Build isolation helper
[group('build')]
helper:
    cargo build -p vst3-host --features process-isolation --bin vst3-host-helper

# Run all-feature tests
[group('test')]
test:
    cargo test --workspace --all-features

# Run both audible playback tests through the default host audio device
[group('test')]
test-audio:
    VST3_HOST_TEST_AUDIO_OUTPUT=1 cargo test -p vst3-host --all-features --test integration_tests test_audio_processing -- --ignored --exact --nocapture
    VST3_HOST_TEST_AUDIO_OUTPUT=1 cargo test -p vst3-host --all-features --test integration_tests test_play_realtime_smoke -- --ignored --exact --nocapture

# Build and bundle TestSynth
[group('build')]
test-plugin:
    cargo build -p vst3-host-testplug --release
    bash scripts/bundle-test-plugin.sh

# Run ignored isolation tests
[group('test')]
test-isolation: helper
    cargo test -p vst3-host --features process-isolation --test integration_tests -- --ignored isolation --test-threads=1

# Launch inspector
[group('run')]
inspector:
    cargo run -p vst3-inspector --release --bin vst3-inspector

# Play a synth (default: Dexed)
[group('run')]
play PLUGIN_PATH=PLUGIN:
    cargo run -p vst3-host --example play_synth -- "{{ PLUGIN_PATH }}"

# Render and play trance MIDI (default: TestSynth)
[group('run')]
trance PLUGIN_PATH="test_plugins/TestSynth.vst3": test-plugin
    cargo run -p vst3-host --example trance_timeline_demo -- "{{ PLUGIN_PATH }}"

# Launch live trance GUI
[group('run')]
trance-gui PLUGIN_PATH="test_plugins/TestSynth.vst3": test-plugin
    cargo run -p vst3-host --example trance_timeline_gui -- "{{ PLUGIN_PATH }}"

# Run silent, offline inspector self-test
[group('test')]
selftest PLUGIN_PATH=PLUGIN:
    cargo run -p vst3-inspector --bin vst3-inspector -- --selftest "{{ PLUGIN_PATH }}"

# Smoke-test a plugin editor
[group('test')]
editor-smoke PLUGIN_PATH=PLUGIN:
    cargo run -p vst3-host --example editor_smoke -- "{{ PLUGIN_PATH }}"

# Run all editor smoke tests
[group('test')]
editor-smoke-all: test-plugin
    cargo run -p vst3-host --example editor_smoke -- test_plugins/TestSynth.vst3
    cargo run -p vst3-host --example editor_smoke -- "{{ PLUGIN }}"
    cargo test -p vst3-host --all-features --test editor_open_tests -- --ignored --nocapture
    cargo test -p vst3-host --all-features --test feature_coverage_tests -- --ignored --test-threads=1 testsynth_editor

# Generate compatibility matrix
[group('test')]
compat *PLUGINS: helper
    cargo run -p vst3-host --example compatibility_matrix --features cpal-backend,process-isolation -- {{ PLUGINS }}

# Test Linux build in Docker
[group('test')]
linux-check:
    #!/usr/bin/env bash
    set -euo pipefail
    docker run --rm -v "$PWD":/work -w /work \
      -e CARGO_TARGET_DIR=/tmp/target \
      rust:bookworm bash -c '
        apt-get update -qq && \
        apt-get install -y -qq libclang-dev clang libxcb1-dev libxcb-util-dev libasound2-dev pkg-config && \
        cargo build -p vst3-host --all-features && \
        cargo test -p vst3-host --all-features --lib'

# Load a plugin in isolation (default: Dexed)
[group('run')]
isolated PLUGIN_PATH=PLUGIN: helper
    cargo run -p vst3-host --example isolated_host --features process-isolation -- "{{ PLUGIN_PATH }}"

# Show isolated plugin editor (macOS)
[group('run')]
isolated-gui PLUGIN_PATH=PLUGIN: helper
    cargo run -p vst3-host --example isolated_gui --features cpal-backend,process-isolation -- "{{ PLUGIN_PATH }}"

# Format code
[group('lint')]
fmt:
    cargo fmt

# Check formatting
[group('lint')]
fmt-check:
    cargo fmt --check

# Run clippy with warnings denied
[group('lint')]
clippy:
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# Run formatting, lint, and tests
[group('lint')]
check: fmt-check clippy test
