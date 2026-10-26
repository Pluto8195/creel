#!/usr/bin/env bash
# Rasterise the Mast SVGs and build dist/macos/mast.icns.
# Needs rsvg-convert (librsvg) and iconutil (macOS). Runs generate.py first.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DIST="$(dirname "$HERE")"
RSVG="${RSVG_CONVERT:-rsvg-convert}"

python3 "$HERE/generate.py"

cp "$HERE/mast-icon.svg" "$DIST/mast.svg"
"$RSVG" -w 1024 -h 1024 "$HERE/mast-icon.svg" -o "$DIST/macos/icon-1024.png"
"$RSVG" -w 1024 -h 1024 "$HERE/mast-icon.svg" -o "$DIST/zeron.png"  # Linux icon (dist/zeron.desktop)

# Icon PNGs: the simplified icon is used at 32 px and below.
for n in 1024 512 256 128 64; do "$RSVG" -w "$n" -h "$n" "$HERE/mast-icon.svg" -o "$HERE/mast-icon-$n.png"; done
for n in 32 16; do "$RSVG" -w "$n" -h "$n" "$HERE/mast-icon-small.svg" -o "$HERE/mast-icon-$n.png"; done

# PNG renders of the flat marks and wordmarks (transparent background).
for v in light dark; do
  "$RSVG" -h 512 "$HERE/mast-mark-$v.svg" -o "$HERE/mast-mark-$v.png"
  "$RSVG" -h 152 "$HERE/mast-wordmark-$v.svg" -o "$HERE/mast-wordmark-$v.png"
  "$RSVG" -h 420 "$HERE/mast-lockup-$v.svg" -o "$HERE/mast-lockup-$v.png"
done
"$RSVG" -h 128 "$HERE/mast-mark-mono.svg" -o "$HERE/mast-mark-mono.png"

# README lockups, 640 px wide.
MEDIA="$(dirname "$DIST")/docs/media/readme"
mkdir -p "$MEDIA"
for v in light dark; do "$RSVG" -w 640 "$HERE/mast-lockup-$v.svg" -o "$MEDIA/mast-lockup-$v.png"; done

# Status-lamp icons (1024 px).
for f in "$HERE"/status/mast-icon-*.svg; do
  "$RSVG" -w 1024 -h 1024 "$f" -o "${f%.svg}.png"
done

# Iconset. The simplified icon is used for every entry that is 32 px or
# smaller: icon_16x16, icon_16x16@2x and icon_32x32.
SET="$(mktemp -d)/mast.iconset"
mkdir -p "$SET"
render() { # <px> <name>
  local src="$HERE/mast-icon.svg"
  if [ "$1" -le 32 ]; then src="$HERE/mast-icon-small.svg"; fi
  "$RSVG" -w "$1" -h "$1" "$src" -o "$SET/$2.png"
}
render 16 icon_16x16;     render 32 icon_16x16@2x
render 32 icon_32x32;     render 64 icon_32x32@2x
render 128 icon_128x128;  render 256 icon_128x128@2x
render 256 icon_256x256;  render 512 icon_256x256@2x
render 512 icon_512x512;  render 1024 icon_512x512@2x
iconutil -c icns "$SET" -o "$DIST/macos/mast.icns"
rm -rf "$(dirname "$SET")"
echo "wrote $DIST/macos/mast.icns"
