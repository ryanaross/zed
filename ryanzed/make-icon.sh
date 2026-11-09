#!/usr/bin/env bash
# Regenerate the RyanZed icon from upstream's stable icon: Zed's mark recoloured onto an
# ocean-blue background, with a teal "R" badge bottom-right.
# Writes crates/zed/resources/app-icon-ryanzed{,@2x}.png (used by bundle-mac and bundle-linux).
# Needs ImageMagick (`magick`, or IM6 `convert`: apt install imagemagick). Rerun only if upstream changes app-icon@2x.png.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")/.."
R=crates/zed/resources
SRC="$R/app-icon@2x.png"
BG_DARK='#0a2a4a'; BADGE='#2ad4c0'; BADGE_OPACITY=0.85
if [[ "$(uname -s)" == Darwin ]]; then
  FONT="${FONT:-/System/Library/Fonts/Avenir Next.ttc}"   # renders Avenir Next Heavy
else
  FONT="${FONT:-$(fc-match 'sans:bold' -f '%{file}')}"
fi
IM="$(command -v magick || command -v convert)" || { echo "install ImageMagick" >&2; exit 1; }
w="$(mktemp -d)"; trap 'rm -rf "$w"' EXIT

# Squircle body only, so the soft drop shadow stays neutral.
"$IM" "$SRC" -alpha extract -threshold 90% "$w/body.png"
# Grayscale remap: black -> BG_DARK, white stays white. Keeps the grain and the white lines.
"$IM" "$SRC" -colorspace gray -colorspace sRGB +level-colors "$BG_DARK,#ffffff" "$w/tint.png"
"$IM" "$SRC" \( "$w/tint.png" "$w/body.png" -alpha off -compose CopyOpacity -composite \) \
  -compose Over -composite "$w/bg.png"
"$IM" -size 1024x1024 xc:none -font "$FONT" -pointsize 300 -fill "$BADGE" \
  -gravity southeast -annotate +150+130 R -channel A -evaluate multiply "$BADGE_OPACITY" +channel "$w/r.png"
"$IM" "$w/bg.png" "$w/r.png" -compose Atop -composite "$R/app-icon-ryanzed@2x.png"
"$IM" "$R/app-icon-ryanzed@2x.png" -resize 512x512 "$R/app-icon-ryanzed.png"
echo "wrote $R/app-icon-ryanzed@2x.png and app-icon-ryanzed.png"
