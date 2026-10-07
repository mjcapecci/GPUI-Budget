#!/usr/bin/env bash
# Builds a release binary and wraps it in dist/Budget.app, zipped as
# dist/Budget-macos-arm64.zip.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked

app=dist/Budget.app
rm -rf dist
mkdir -p "$app/Contents/MacOS"
cp target/release/budget "$app/Contents/MacOS/budget"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Budget</string>
    <key>CFBundleDisplayName</key><string>Budget</string>
    <key>CFBundleIdentifier</key><string>io.github.mjcapecci.budget</string>
    <key>CFBundleExecutable</key><string>budget</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>${version}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: not notarized, but required for arm64 and keeps the
# bundle's signature consistent.
codesign --force --deep --sign - "$app"
ditto -c -k --keepParent "$app" dist/Budget-macos-arm64.zip
echo "Built $app ($version)"
