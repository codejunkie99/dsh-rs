#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="DeepSeek Harness RS.app"
BUNDLE_DIR="$ROOT/dist/$APP_NAME"
CONTENTS="$BUNDLE_DIR/Contents"

cargo build --release --manifest-path "$ROOT/Cargo.toml"
rm -rf "$BUNDLE_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$ROOT/target/release/dsh-app" "$CONTENTS/MacOS/dsh-app"

cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>dsh-app</string>
  <key>CFBundleIdentifier</key><string>dev.arnavdas.dsh-rs</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>DeepSeek Harness RS</string>
  <key>CFBundleDisplayName</key><string>DeepSeek Harness RS</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$CONTENTS/PkgInfo"
codesign --force --deep --sign - "$BUNDLE_DIR"

echo "$BUNDLE_DIR"
