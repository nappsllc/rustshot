#!/usr/bin/env bash
# Regenerate assets/fonts/Inter-Medium.subset.ttf from a static Inter release
# TTF (Inter-4.1.zip → extras/ttf/Inter-Medium.ttf). Needs `pip install fonttools`.
# Usage: tools/subset-font.sh /path/to/Inter-Medium.ttf
set -euo pipefail
src="${1:?usage: $0 /path/to/Inter-Medium.ttf}"
out="$(cd "$(dirname "$0")/.." && pwd)/assets/fonts/Inter-Medium.subset.ttf"
mkdir -p "$(dirname "$out")"
pyftsubset "$src" \
  --output-file="$out" \
  --unicodes="U+0020-007E,U+00B7,U+00D7,U+2014,U+2026,U+2190-2193,U+21E7,U+2318,U+2325,U+23CE" \
  --no-hinting --desubroutinize \
  --layout-features='' \
  --drop-tables+=DSIG,GSUB,GPOS,GDEF,STAT \
  --name-IDs='0,1,2,3,4,5,6' \
  --notdef-outline
ls -l "$out"
