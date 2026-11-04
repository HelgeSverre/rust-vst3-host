#!/usr/bin/env bash
# Bundle the built test synth into a loadable VST3 bundle at test_plugins/TestSynth.vst3.
#
# VST3 bundles are laid out per platform, and the host resolves each shape in
# `discovery::get_vst3_binary_path`:
#
#   macOS    Contents/MacOS/TestSynth          (+ Info.plist, PkgInfo — CFBundle needs them)
#   Linux    Contents/<arch>-linux/TestSynth.so
#   Windows  Contents/<arch>-win/TestSynth.vst3
#
# Runs on all three (Git Bash on Windows). Honours CARGO_TARGET_DIR and PROFILE so a CI job
# that builds somewhere else can still bundle:
#
#   cargo build -p vst3-host-testplug --release && bash scripts/bundle-test-plugin.sh
#   PROFILE=debug bash scripts/bundle-test-plugin.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET_DIR="${CARGO_TARGET_DIR:-target}/${PROFILE:-release}"
BUNDLE="test_plugins/TestSynth.vst3"

case "$(uname -s)" in
  Darwin) PLATFORM=macos ;;
  Linux) PLATFORM=linux ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT) PLATFORM=windows ;;
  *)
    echo "error: unsupported platform $(uname -s)" >&2
    exit 1
    ;;
esac

# The architecture cargo builds for, which is rustc's host. `uname -m` names the shell's, which
# differs on Windows on Arm when Git Bash runs as x64. VST3 spells ARM64 differently per
# platform: `aarch64-linux`, but `arm64-win`.
RUST_ARCH="$(rustc -vV | sed -n 's/^host: \([^-]*\)-.*/\1/p')"
case "$RUST_ARCH" in
  x86_64) ARCH_LINUX=x86_64 ARCH_WIN=x86_64 ;;
  aarch64) ARCH_LINUX=aarch64 ARCH_WIN=arm64 ;;
  *)
    echo "error: unsupported architecture $RUST_ARCH" >&2
    exit 1
    ;;
esac

case "$PLATFORM" in
  macos) SOURCE="$TARGET_DIR/libvst3_host_testplug.dylib"; DEST="$BUNDLE/Contents/MacOS/TestSynth" ;;
  linux) SOURCE="$TARGET_DIR/libvst3_host_testplug.so"; DEST="$BUNDLE/Contents/$ARCH_LINUX-linux/TestSynth.so" ;;
  windows) SOURCE="$TARGET_DIR/vst3_host_testplug.dll"; DEST="$BUNDLE/Contents/$ARCH_WIN-win/TestSynth.vst3" ;;
esac

if [ ! -f "$SOURCE" ]; then
  echo "error: $SOURCE not found — run 'cargo build -p vst3-host-testplug --release' first" >&2
  exit 1
fi

# Rebuild the bundle from scratch. It is a single-architecture fixture for whatever machine is
# running this, so a per-arch folder left by an earlier run, maybe on another machine, would
# hold a stale binary.
rm -rf "$BUNDLE"
mkdir -p "$(dirname "$DEST")"
cp "$SOURCE" "$DEST"

# Only macOS needs bundle metadata: CFBundle refuses to load a directory without it, and the
# loader looks the executable up by CFBundleExecutable rather than guessing.
if [ "$PLATFORM" = macos ]; then
  printf 'BNDL????' > "$BUNDLE/Contents/PkgInfo"
  cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>TestSynth</string>
  <key>CFBundleIdentifier</key><string>com.vst3-host.TestSynth</string>
  <key>CFBundleName</key><string>TestSynth</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleSignature</key><string>????</string>
  <key>CFBundleVersion</key><string>1.0.0</string>
</dict></plist>
PLIST
fi

echo "Bundled $DEST"
