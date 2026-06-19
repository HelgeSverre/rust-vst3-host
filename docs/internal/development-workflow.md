# Development Workflow Improvements

## Current Issues

### Build Warnings (Critical)
- **60+ warnings** from clippy, deprecation, unused imports
- **Developer confidence**: Warnings suggest unmaintained code
- **Hidden real issues**: Important warnings get lost in noise

### Feature Flag Complexity
- **Default behavior unclear**: What features are enabled?
- **Optional dependencies**: Hard to understand what's needed when
- **Documentation mismatch**: Features mentioned but not explained

### Testing Gaps
- **Integration tests**: Limited testing with real plugins
- **Platform testing**: No automated testing across platforms
- **Performance testing**: No benchmarks or regression tests

## Immediate Actions Required

### 1. Clean Up Build Warnings

```toml
# Add to Cargo.toml
[lints.rust]
unused_imports = "deny"
dead_code = "warn"
deprecated = "warn"

[lints.clippy]
all = "warn"
pedantic = "warn"
# Allow some pedantic lints that don't add value
module_name_repetitions = "allow"
similar_names = "allow"
```

**Priority fixes needed**:
1. Remove unused imports in `window.rs`, `objc_conflict_resolver.rs`
2. Update deprecated `cocoa` crate usage to `objc2-*` family
3. Fix `scan_default_paths` field that's never read
4. Address unnecessary `unsafe` blocks

### 2. Improve Feature Flag System

**Current confusion**:
```toml
# What does this actually enable?
default = ["cpal-backend"]
cpal-backend = ["cpal"]
```

**Proposed improvement**:
```toml
[features]
default = ["audio-cpal"]

# Audio backends (choose one)
audio-cpal = ["cpal"]        # CPAL cross-platform audio
audio-custom = []            # Bring your own audio backend

# Plugin isolation
process-isolation = []       # Enable plugin crash protection

# GUI frameworks
gui-egui = ["egui"]         # egui widgets for plugin GUIs
gui-native = []             # Native platform GUI support

# Development features
dev-tools = []              # Plugin inspector, test utilities
examples-full = ["audio-cpal", "gui-egui"]  # Enable all example features
```

### 3. Developer Commands

**Create `Makefile` for common tasks**:
```makefile
# Quick development commands
.PHONY: check test lint fix docs

# Fast check for syntax errors
check:
	cargo check --all-features

# Run all tests
test:
	cargo test --all-features
	cargo test --no-default-features

# Lint and format
lint:
	cargo clippy --all-features -- -D warnings
	cargo fmt --check

# Auto-fix issues
fix:
	cargo clippy --all-features --fix --allow-dirty
	cargo fmt

# Build documentation
docs:
	cargo doc --all-features --open

# Clean build (when things are broken)
clean:
	cargo clean
	cargo build --all-features

# Release checklist
release-check: lint test docs
	cargo build --release --all-features
	@echo "✅ Ready for release"
```

**Create `justfile` for modern task runner**:
```justfile
# List available commands
default:
    @just --list

# Quick development check
check:
    cargo check --all-features
    
# Run tests with different feature combinations
test:
    cargo test --all-features
    cargo test --no-default-features
    cargo test --features audio-cpal
    cargo test --features process-isolation

# Fix all auto-fixable issues
fix:
    cargo clippy --all-features --fix --allow-dirty
    cargo fmt
    
# Build examples to verify they work
examples:
    cargo build --examples --all-features
    
# Complete pre-commit check
pre-commit: fix test examples
    @echo "✅ All checks passed"
```

### 4. Testing Infrastructure

**Add proper test configuration**:
```toml
# In Cargo.toml
[[test]]
name = "integration"
path = "tests/integration_tests.rs"
required-features = ["audio-cpal"]

[[test]]
name = "no_std_compat"
path = "tests/no_std_tests.rs"
```

**Plugin testing framework**:
```rust
// tests/plugin_testing.rs
/// Framework for testing against real VST3 plugins
pub struct PluginTestSuite {
    test_plugins: Vec<PathBuf>,
}

impl PluginTestSuite {
    /// Discover test plugins from environment
    pub fn discover() -> Self {
        let mut test_plugins = Vec::new();
        
        // Check for test plugins in known locations
        if let Ok(plugin_dir) = std::env::var("VST3_TEST_PLUGINS") {
            // Use provided test plugin directory
        } else {
            // Use safe system plugins for testing
            test_plugins.extend(self::find_safe_test_plugins());
        }
        
        Self { test_plugins }
    }
    
    /// Run basic loading test on all discovered plugins
    pub fn test_plugin_loading(&self) -> Result<(), Box<dyn std::error::Error>> {
        for plugin_path in &self.test_plugins {
            let mut host = Vst3Host::new()?;
            let plugin = host.load_plugin(plugin_path)?;
            // Basic validation...
        }
        Ok(())
    }
}
```

### 5. Documentation Build Process

**Automated documentation**:
```bash
#!/bin/bash
# scripts/build-docs.sh
set -e

echo "Building comprehensive documentation..."

# Build API docs with all features
cargo doc --all-features --no-deps

# Build examples
cargo build --examples --all-features

# Validate documentation links
cargo doc --all-features --no-deps 2>&1 | grep -i warning && exit 1

# Generate usage examples from tests
cargo test --doc --all-features

echo "✅ Documentation build complete"
```

**Documentation CI check**:
```yaml
# .github/workflows/docs.yml
name: Documentation
on: [push, pull_request]

jobs:
  docs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Build docs
        run: |
          cargo doc --all-features --no-deps
          # Check for broken links
          cargo doc --all-features 2>&1 | grep "warning:" && exit 1
      - name: Test doc examples
        run: cargo test --doc --all-features
```

### 6. Developer Experience Tools

**Plugin inspector CLI**:
```rust
// src/bin/vst3-inspect.rs
use clap::Parser;
use vst3_host::prelude::*;

#[derive(Parser)]
#[command(name = "vst3-inspect")]
#[command(about = "Inspect VST3 plugins")]
struct Cli {
    /// Path to VST3 plugin
    plugin_path: String,
    
    /// Show parameter details
    #[arg(long)]
    parameters: bool,
    
    /// Test audio processing
    #[arg(long)]
    test_audio: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    
    println!("Inspecting VST3 plugin: {}", cli.plugin_path);
    
    let mut host = Vst3Host::new()?;
    let plugin = host.load_plugin(&cli.plugin_path)?;
    
    // Show plugin info
    let info = plugin.info();
    println!("Name: {}", info.name);
    println!("Vendor: {}", info.vendor);
    println!("Audio I/O: {} -> {}", info.audio_inputs, info.audio_outputs);
    println!("Has GUI: {}", info.has_gui);
    
    if cli.parameters {
        show_parameters(&plugin)?;
    }
    
    if cli.test_audio {
        test_audio_processing(&mut plugin)?;
    }
    
    Ok(())
}
```

**Project template generator**:
```bash
#!/bin/bash
# scripts/create-project.sh
# Usage: ./create-project.sh my-vst-host

PROJECT_NAME=$1
if [ -z "$PROJECT_NAME" ]; then
    echo "Usage: $0 <project-name>"
    exit 1
fi

mkdir "$PROJECT_NAME"
cd "$PROJECT_NAME"

# Create Cargo.toml
cat > Cargo.toml << EOF
[package]
name = "$PROJECT_NAME"
version = "0.1.0"
edition = "2021"

[dependencies]
vst3-host = { version = "0.1", features = ["audio-cpal"] }
anyhow = "1.0"

EOF

# Create main.rs with working example
cat > src/main.rs << 'EOF'
use vst3_host::simple;
use anyhow::Result;

fn main() -> Result<()> {
    println!("VST3 Host Example");
    
    // TODO: Replace with path to your VST3 plugin
    let plugin_path = "/Library/Audio/Plug-Ins/VST3/YourPlugin.vst3";
    
    let mut plugin = simple::load_plugin(plugin_path)?;
    
    println!("Loaded plugin: {}", plugin.info().name);
    
    // Play a test note
    simple::test_plugin_with_note(&mut plugin, 60, 100, 1000)?;
    
    println!("✅ Success! You should have heard audio.");
    
    Ok(())
}
EOF

cargo check
echo "✅ Created project '$PROJECT_NAME' with working VST3 host example"
```

## Implementation Roadmap

### Week 1: Critical Fixes
- [ ] Fix all build warnings (highest priority)
- [ ] Update deprecated dependencies
- [ ] Clean up unused code
- [ ] Add basic lints configuration

### Week 2: Developer Experience
- [ ] Create `Makefile` with common commands
- [ ] Improve feature flag documentation
- [ ] Add plugin inspector CLI tool
- [ ] Create project template generator

### Week 3: Testing & CI
- [ ] Add comprehensive integration tests
- [ ] Set up automated testing across platforms
- [ ] Create plugin compatibility test framework
- [ ] Add documentation build validation

### Week 4: Documentation
- [ ] Add examples to all public APIs
- [ ] Create progressive tutorial series
- [ ] Build interactive documentation
- [ ] Add troubleshooting guides

## Success Metrics

### Build Quality
- **Zero warnings** on `cargo clippy --all-features -- -D warnings`
- **All examples build** successfully
- **Clean documentation** with no broken links

### Developer Productivity
- **Fast feedback loops**: `cargo check` under 10 seconds
- **Clear error messages**: Common issues have actionable solutions
- **Easy testing**: One command to verify everything works

### Onboarding Success
- **Time to first success**: < 5 minutes from clone to working example
- **Learning curve**: Progressive complexity in examples
- **Troubleshooting**: Self-service problem resolution