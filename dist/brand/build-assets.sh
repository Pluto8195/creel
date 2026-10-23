#!/usr/bin/env bash
# Rasterise the Creel SVGs and build dist/macos/creel.icns.
# Needs rsvg-convert (librsvg) and iconutil (macOS). Run generate.py first.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DIST="$(dirname "$HERE")"
RSVG="${RSVG_CONVERT:-rsvg-convert}"

python3 "$HERE/generate.py"

cp "$HERE/creel-icon.svg" "$DIST/creel.svg"
"$RSVG" -w 1024 -h 1024 "$HERE/creel-icon.svg" -o "$DIST/macos/icon-1024.png"

# PNG renders of the flat marks and wordmarks (transparent background).
for v in light dark; do
  "$RSVG" -h 512 "$HERE/creel-mark-$v.svg" -o "$HERE/creel-mark-$v.png"
  "$RSVG" -w 1200 "$HERE/creel-wordmark-$v.svg" -o "$HERE/creel-wordmark-$v.png"
  "$RSVG" -w 1200 "$HERE/creel-lockup-$v.svg" -o "$HERE/creel-lockup-$v.png"
done

# Iconset. The simplified icon (no winding) is used for every entry that is
# 32 px or smaller: icon_16x16, icon_16x16@2x and icon_32x32.
SET="$(mktemp -d)/creel.iconset"
mkdir -p "$SET"
render() { # <px> <name>
  local src="$HERE/creel-icon.svg"
  if [ "$1" -le 32 ]; then src="$HERE/creel-icon-small.svg"; fi
  "$RSVG" -w "$1" -h "$1" "$src" -o "$SET/$2.png"
}
render 16 icon_16x16;     render 32 icon_16x16@2x
render 32 icon_32x32;     render 64 icon_32x32@2x
render 128 icon_128x128;  render 256 icon_128x128@2x
render 256 icon_256x256;  render 512 icon_256x256@2x
render 512 icon_512x512;  render 1024 icon_512x512@2x
iconutil -c icns "$SET" -o "$DIST/macos/creel.icns"
rm -rf "$(dirname "$SET")"
echo "wrote $DIST/macos/creel.icns"
