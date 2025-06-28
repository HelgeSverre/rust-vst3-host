# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a VST3 plugin host application written in Rust. It provides a graphical interface for loading, inspecting, and testing VST3 plugins with real-time parameter manipulation and MIDI event monitoring.

## Essential Commands

### Build and Run
```bash
# Build the project
cargo build

# Build in release mode (recommended for VST3 plugin loading)
cargo build --release

# Run the application
cargo run --release

# Check for compilation errors
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy
```

### Testing
```bash
# Run tests (if any exist)
cargo test

# Run tests with output
cargo test -- --nocapture
```

## Architecture Overview

The application is structured as a monolithic single-file application (`src/main.rs`, ~2600 lines) with the following key components:

### Core Components

1. **VST3Inspector** - Main application state containing:
   - Plugin discovery and listing
   - Current plugin state and metadata
   - GUI state management
   - Parameter values and bus configurations

2. **Platform-specific Window Management**:
   - **macOS**: Uses Cocoa/NSWindow for native window creation
   - **Windows**: Uses Win32 API for window management
   - Implements `IPlugFrame` for VST3 plugin GUI embedding

3. **Plugin Loading System**:
   - Dynamic library loading via `libloading`
   - Automatic discovery of VST3 plugins from standard directories
   - Safe handling of VST3 COM interfaces

4. **Event Handling**:
   - Custom `IEventList` implementation for MIDI events
   - `ComponentHandler` for parameter change notifications
   - Real-time MIDI monitoring with event display

### VST3 Plugin Paths

The application scans these standard directories:

**macOS**:
- `/Library/Audio/Plug-Ins/VST3`
- `~/Library/Audio/Plug-Ins/VST3`

**Windows**:
- `C:\Program Files\Common Files\VST3`
- `C:\Program Files (x86)\Common Files\VST3`

### Key Implementation Details

- Uses `vst3` crate (0.1.2) for VST3 interface bindings
- GUI built with egui/eframe with Catppuccin theme
- Extensive use of unsafe Rust for VST3 COM interface interactions
- Supports both single-component and separate controller VST3 architectures
- Platform-specific binary path resolution within VST3 bundles

### Environment Configuration

The project uses a `.cargo/config.toml` that sets:
```toml
[env]
VST3_SDK_DIR = { value = "vst3sdk", relative = true }
```

This points to the VST3 SDK submodule included in the repository.

## Development Workflow

1. The application defaults to loading "HY-MPS3 free.vst3" on startup
2. Main UI tabs:
   - **Discovery**: Browse and select available VST3 plugins
   - **Plugin**: View detailed plugin information and parameters
   - **Processing**: Configure audio bus settings
   - **MIDI Monitor**: Real-time MIDI event display

3. When modifying the code:
   - All application logic is in `src/main.rs`
   - Follow existing patterns for VST3 COM interface handling
   - Use extensive logging with emoji prefixes for debugging
   - Ensure proper cleanup of VST3 interfaces to prevent memory leaks

## Important Notes

- Always build in release mode when testing with actual VST3 plugins for better performance
- The application uses detailed console logging - run from terminal to see diagnostic output
- VST3 plugin initialization follows a specific sequence that must be maintained
- String conversions between Rust strings and VST3's UTF-16 strings are handled by utility functions in the code