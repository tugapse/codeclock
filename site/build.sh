#!/bin/sh
# Builds the GitHub Pages demo into _site/: the real dashboard page, with demo.js loaded first
# so its /api/* calls are answered in the browser with sample data.
set -eu
cd "$(dirname "$0")/.."
out=_site
rm -rf "$out" && mkdir -p "$out"
count=$(grep -c '^<script>$' src/web/index.html)
[ "$count" = 1 ] || { echo "expected one <script> line in src/web/index.html, found $count" >&2; exit 1; }
sed -e 's|^<script>$|<script src="demo.js"></script>\n<script>|' \
    -e 's|<title>codeclock history</title>|<title>codeclock demo</title>|' \
    src/web/index.html > "$out/index.html"
cp site/demo.js "$out/demo.js"
echo "built $out/"
