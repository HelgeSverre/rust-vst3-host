# VST3 Host Library - Development Workflow
# Run `make help` to see available commands

.PHONY: help check test build examples clean fix lint format docs release

# Default target
help:
	@echo "VST3 Host Library - Development Commands"
	@echo ""
	@echo "Quick Commands:"
	@echo "  make check      - Fast type checking and linting"
	@echo "  make test       - Run all tests"
	@echo "  make examples   - Build all examples"
	@echo "  make fix        - Auto-fix formatting and simple issues"
	@echo ""
	@echo "Development:"
	@echo "  make build      - Build library in debug mode"
	@echo "  make release    - Build optimized release"
	@echo "  make clean      - Clean build artifacts"
	@echo "  make docs       - Build and open documentation"
	@echo ""
	@echo "Quality:"
	@echo "  make lint       - Run clippy linter"
	@echo "  make format     - Format code with rustfmt"
	@echo ""
	@echo "Examples:"
	@echo "  make run-scanner          - Run plugin scanner"
	@echo "  make run-host             - Run GUI host example"
	@echo "  make run-test-loading     - Run plugin loading test"
	@echo ""

# Development workflow
check:
	@echo "🔍 Running fast checks..."
	cargo check --all-targets --all-features
	cargo clippy --all-targets --all-features -- -D warnings

test:
	@echo "🧪 Running tests..."
	cargo test --all-features
	cargo test --all-features --release

build:
	@echo "🔨 Building library..."
	cargo build --all-features

release:
	@echo "🚀 Building optimized release..."
	cargo build --release --all-features

examples:
	@echo "📚 Building all examples..."
	cargo build --examples --all-features

clean:
	@echo "🧹 Cleaning build artifacts..."
	cargo clean

# Code quality
fix: format lint-fix
	@echo "✨ Auto-fixing issues completed"

lint:
	@echo "🔍 Running clippy..."
	cargo clippy --all-targets --all-features -- -D warnings

lint-fix:
	@echo "🔧 Auto-fixing clippy issues..."
	cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged

format:
	@echo "🎨 Formatting code..."
	cargo fmt --all

# Documentation
docs:
	@echo "📖 Building documentation..."
	cargo doc --all-features --open

docs-check:
	@echo "📋 Checking documentation..."
	cargo doc --all-features --no-deps

# Example runners
run-scanner:
	@echo "🔍 Running plugin scanner..."
	cargo run --example plugin_scanner --features "cpal-backend"

run-host:
	@echo "🎵 Running GUI host..."
	cargo run --example host --features "cpal-backend,egui-widgets"

run-test-loading:
	@echo "🧪 Running plugin loading test..."
	@echo "Usage: make run-test-loading PLUGIN=/path/to/plugin.vst3"
	@if [ -z "$(PLUGIN)" ]; then \
		echo "❌ Please specify PLUGIN path: make run-test-loading PLUGIN=/path/to/plugin.vst3"; \
		exit 1; \
	fi
	cargo run --example test_loading --features "cpal-backend" -- "$(PLUGIN)"

run-automation:
	@echo "🎛️ Running parameter automation example..."
	cargo run --example parameter_automation --features "cpal-backend"

# CI/CD helpers
ci-check: check test docs-check
	@echo "✅ All CI checks passed"

pre-commit: fix check test
	@echo "✅ Pre-commit checks completed"

# Installation helpers
install-deps-macos:
	@echo "🍎 Installing macOS dependencies..."
	brew install cmake

install-deps-ubuntu:
	@echo "🐧 Installing Ubuntu dependencies..."
	sudo apt-get update
	sudo apt-get install cmake libasound2-dev

install-deps-fedora:
	@echo "🎩 Installing Fedora dependencies..."
	sudo dnf install cmake alsa-lib-devel

# Git helpers
init-submodules:
	@echo "📦 Initializing git submodules..."
	git submodule update --init --recursive

update-submodules:
	@echo "🔄 Updating git submodules..."
	git submodule update --recursive --remote

# Benchmarking
bench:
	@echo "⚡ Running benchmarks..."
	cargo bench --all-features

# Security audit
audit:
	@echo "🔒 Running security audit..."
	cargo audit

# Complete setup for new developers
setup: init-submodules install-deps build examples
	@echo "🎉 Development environment setup complete!"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Run 'make run-scanner' to discover VST3 plugins"
	@echo "  2. Run 'make run-host' to try the GUI interface"
	@echo "  3. Check out the tutorials in docs/tutorials/"
	@echo ""

# Platform detection for setup
setup-auto:
	@echo "🔧 Auto-detecting platform and setting up..."
	@if [ "$$(uname)" = "Darwin" ]; then \
		make setup install-deps-macos; \
	elif [ "$$(uname)" = "Linux" ] && [ -f /etc/debian_version ]; then \
		make setup install-deps-ubuntu; \
	elif [ "$$(uname)" = "Linux" ] && [ -f /etc/fedora-release ]; then \
		make setup install-deps-fedora; \
	else \
		echo "⚠️  Platform not auto-detected. Run 'make setup' and install dependencies manually."; \
		make setup; \
	fi