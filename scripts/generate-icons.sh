#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
icons="$root/src-tauri/icons"
svg="$icons/icon.svg"

command -v rsvg-convert >/dev/null || { echo "rsvg-convert is required" >&2; exit 1; }
command -v iconutil >/dev/null || { echo "iconutil is required on macOS" >&2; exit 1; }

for size in 32 64 128 256 512 1024; do
  rsvg-convert -w "$size" -h "$size" "$svg" -o "$icons/${size}x${size}.png"
done
cp "$icons/256x256.png" "$icons/icon.png"
cp "$icons/256x256.png" "$icons/128x128@2x.png"

python3 - "$icons" <<'PY'
from pathlib import Path
import sys
from PIL import Image

icons = Path(sys.argv[1])
image = Image.open(icons / "256x256.png").convert("RGBA")
image.save(icons / "icon.ico", format="ICO", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
PY

icns_dir="$(mktemp -d)"
trap 'rm -rf "$icns_dir"' EXIT
mkdir -p "$icns_dir/Async.iconset"
for pair in "16:16x16" "32:32x32" "128:128x128" "256:256x256" "512:512x512"; do
  size="${pair%%:*}"
  name="${pair#*:}"
  rsvg-convert -w "$size" -h "$size" "$svg" -o "$icns_dir/Async.iconset/icon_${name}.png"
  double=$((size * 2))
  rsvg-convert -w "$double" -h "$double" "$svg" -o "$icns_dir/Async.iconset/icon_${name}@2x.png"
done
iconutil -c icns "$icns_dir/Async.iconset" -o "$icons/icon.icns"
