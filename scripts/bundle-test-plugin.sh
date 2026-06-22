#!/usr/bin/env bash
# Bundle the built test synth cdylib into a loadable macOS VST3 bundle under test_plugins/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DYLIB="target/release/libvst3_host_testplug.dylib"
if [ ! -f "$DYLIB" ]; then
  echo "error: $DYLIB not found — run 'cargo build -p vst3-host-testplug --release' first" >&2
  exit 1
fi

BUNDLE="test_plugins/TestSynth.vst3/Contents"
mkdir -p "$BUNDLE/MacOS"
cp "$DYLIB" "$BUNDLE/MacOS/TestSynth"
printf 'BNDL????' > "$BUNDLE/PkgInfo"
cat > "$BUNDLE/Info.plist" <<'PLIST'
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

echo "Bundled test_plugins/TestSynth.vst3"
