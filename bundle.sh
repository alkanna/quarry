#!/bin/sh
# Builds an optimized release and wraps it as dist/Quarry.app (double-click to run, or drag to /Applications).
set -eu
cd "$(dirname "$0")"

# TARGETS="aarch64-apple-darwin x86_64-apple-darwin" builds both and joins them into one
# universal binary (what the release workflow ships).
if [ -n "${TARGETS:-}" ]; then
  BINS=""
  for t in $TARGETS; do
    cargo build --release --target "$t"
    BINS="$BINS target/$t/release/quarry"
  done
  mkdir -p target/universal
  # shellcheck disable=SC2086
  lipo -create -output target/universal/quarry $BINS
  BIN=target/universal/quarry
else
  cargo build --release
  BIN=target/release/quarry
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Quarry.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/quarry"
# Made from assets/icon.svg; regenerate with rsvg-convert + iconutil if the logo changes.
cp assets/Quarry.icns "$APP/Contents/Resources/Quarry.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Quarry</string>
  <key>CFBundleDisplayName</key><string>Quarry</string>
  <key>CFBundleIdentifier</key><string>dev.quarry.app</string>
  <key>CFBundleExecutable</key><string>quarry</string>
  <key>CFBundleIconFile</key><string>Quarry</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSRequiresAquaSystemAppearance</key><false/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
EOF

# ponytail: ad-hoc signature. Downloads from a browser need the quarantine flag removed; sign with a
# Developer ID and notarize to avoid that.
codesign --force --deep --sign - "$APP"
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
