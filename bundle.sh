#!/bin/sh
# Builds an optimized release for one platform into dist/:
#   macOS    dist/Quarry.app (double-click to run, or drag to /Applications)
#   Linux    dist/quarry-linux-<arch>.tar.gz
#   Windows  dist/quarry-windows-<arch>.zip (run from Git Bash)
# Usage: ./bundle.sh [platform], where platform defaults to this machine and is one of
#   macos-arm64 macos-x86_64 macos-universal linux-x86_64 linux-aarch64 windows-x86_64 windows-aarch64
# Building for another OS needs that OS's linker and libraries; the release workflow builds each natively.
set -eu
cd "$(dirname "$0")"

usage() { sed -n '2,8p' "$0" | cut -c3-; }

host() {
  case "$(uname -m)" in arm64 | aarch64) arch=aarch64 ;; *) arch=x86_64 ;; esac
  case "$(uname -s)" in
    Darwin) if [ "$arch" = aarch64 ]; then echo macos-arm64; else echo macos-x86_64; fi ;;
    Linux) echo "linux-$arch" ;;
    MINGW* | MSYS* | CYGWIN*) echo "windows-$arch" ;;
    *) echo "Unsupported system $(uname -s); pass a platform." >&2; return 1 ;;
  esac
}

PLATFORM=${1:-$(host)}
case "$PLATFORM" in
  macos-arm64) TARGETS=aarch64-apple-darwin ;;
  macos-x86_64) TARGETS=x86_64-apple-darwin ;;
  macos-universal) TARGETS="aarch64-apple-darwin x86_64-apple-darwin" ;;
  linux-x86_64) TARGETS=x86_64-unknown-linux-gnu ;;
  linux-aarch64) TARGETS=aarch64-unknown-linux-gnu ;;
  windows-x86_64) TARGETS=x86_64-pc-windows-msvc ;;
  windows-aarch64) TARGETS=aarch64-pc-windows-msvc ;;
  -h | --help) usage; exit 0 ;;
  *) echo "Unknown platform: $PLATFORM" >&2; usage >&2; exit 1 ;;
esac

if command -v rustup >/dev/null 2>&1; then
  # shellcheck disable=SC2086
  rustup target add $TARGETS >/dev/null
fi
for t in $TARGETS; do
  cargo build --release --target "$t"
done
mkdir -p dist

case "$PLATFORM" in
  linux-*)
    OUT=dist/quarry-$PLATFORM.tar.gz
    tar -czf "$OUT" -C "target/$TARGETS/release" quarry
    echo "Built $OUT"
    exit 0
    ;;
  windows-*)
    OUT=dist/quarry-$PLATFORM.zip
    BIN=target/$TARGETS/release/quarry.exe
    rm -f "$OUT"
    if command -v zip >/dev/null 2>&1; then
      zip -jq "$OUT" "$BIN"
    else
      # Git Bash's tar cannot write zip files; PowerShell can.
      powershell -NoProfile -Command "Compress-Archive -Path '$BIN' -DestinationPath '$OUT'"
    fi
    echo "Built $OUT"
    exit 0
    ;;
esac

# macOS: one target, or both joined into a universal binary (what the release workflow ships).
if [ "$PLATFORM" = macos-universal ]; then
  mkdir -p target/universal
  BIN=target/universal/quarry
  # shellcheck disable=SC2086
  lipo -create -output "$BIN" $(for t in $TARGETS; do echo "target/$t/release/quarry"; done)
else
  BIN=target/$TARGETS/release/quarry
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Quarry.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/quarry"
# Made from docs/logo.figcraft ("A, refined" and its 16 px master); regenerate the iconset there and run iconutil if the logo changes.
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
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 Khiman Louer. MIT License.</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSRequiresAquaSystemAppearance</key><false/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
EOF

# QUARRY_SIGN_ID is a "Developer ID Application" identity in the keychain. Without it the app gets an ad-hoc
# signature, which runs locally but makes Gatekeeper block a browser download.
# Notarization needs the hardened runtime and a secure timestamp, hence --options runtime --timestamp.
if [ -n "${QUARRY_SIGN_ID:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$QUARRY_SIGN_ID" "$APP"
else
  codesign --force --deep --sign - "$APP"
fi
# QUARRY_NOTARY_ARGS are notarytool credentials: "--keychain-profile quarry" locally, or
# "--key <p8> --key-id <id> --issuer <uuid>" in CI. Stapling fails unless Apple accepted the upload.
if [ -n "${QUARRY_NOTARY_ARGS:-}" ]; then
  ditto -c -k --keepParent "$APP" dist/notarize.zip
  # shellcheck disable=SC2086
  xcrun notarytool submit dist/notarize.zip $QUARRY_NOTARY_ARGS --wait
  rm dist/notarize.zip
  xcrun stapler staple "$APP"
fi
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
