#!/usr/bin/env bash
# Produce assets/AppIcon.icns from a 1024x1024 PNG.
#
#   scripts/make-icon.sh              # uses assets/icon.png
#   scripts/make-icon.sh my-art.png   # uses some other PNG
set -euo pipefail

cd "$(dirname "$0")/.."

SRC="${1:-assets/icon.png}"
if [[ ! -f "$SRC" ]]; then
  echo "no artwork at $SRC" >&2
  exit 1
fi
echo "==> using $SRC"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"

# The sizes macOS expects, each with its @2x variant.
for size in 16 32 128 256 512; do
  sips -z $size $size             "$SRC" --out "$ICONSET/icon_${size}x${size}.png"    >/dev/null
  sips -z $((size*2)) $((size*2)) "$SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p assets
iconutil -c icns "$ICONSET" -o assets/AppIcon.icns
echo "wrote assets/AppIcon.icns"
