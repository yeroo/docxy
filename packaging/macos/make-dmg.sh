#!/usr/bin/env bash
# Assemble docxy.app from the release `suite` binary and wrap it in a .dmg.
# macOS only (uses sips / iconutil / hdiutil). Usage: make-dmg.sh <version>
set -euo pipefail

VER="${1:-0.0.0}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PKG="$ROOT/packaging/macos"
BIN="$ROOT/suite/target/release/suite"
OUT="$ROOT/out"
mkdir -p "$OUT"

[ -f "$BIN" ] || { echo "missing suite binary: $BIN" >&2; exit 1; }

APP="$OUT/docxy.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/suite"
chmod +x "$APP/Contents/MacOS/suite"
sed "s/__VERSION__/$VER/g" "$PKG/Info.plist" > "$APP/Contents/Info.plist"

# Build docxy.icns from the 1024px source via a canonical .iconset.
ICONSET="$OUT/docxy.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
SRC="$PKG/docxy-1024.png"
emit() { sips -z "$2" "$2" "$SRC" --out "$ICONSET/$1" >/dev/null; }
emit icon_16x16.png        16
emit icon_16x16@2x.png     32
emit icon_32x32.png        32
emit icon_32x32@2x.png     64
emit icon_128x128.png      128
emit icon_128x128@2x.png   256
emit icon_256x256.png      256
emit icon_256x256@2x.png   512
emit icon_512x512.png      512
cp "$SRC" "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/docxy.icns"

# Compressed .dmg with a drag-to-Applications layout.
STAGE="$OUT/dmgstage"
rm -rf "$STAGE"; mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/docxy-suite-macos-aarch64.dmg"
rm -f "$DMG"
hdiutil create -volname "docxy" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
echo "built $DMG"
