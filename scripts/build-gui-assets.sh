#!/usr/bin/env bash
# Precompile the zkv gui's TypeScript sources to plain JS.
#
# The web UI ships as vendored libraries + precompiled JS so the binary
# needs no CDN and `cargo build` needs no Node. Run this whenever the
# sources under crates/zkv/src/gui/assets/src/ change, then commit the
# regenerated crates/zkv/src/gui/assets/js/*.js (and assets/api.js,
# assets/forms.js).
#
# Requires Node. We first type-check with `tsc --noEmit` (esbuild only
# strips types, it never type-checks), then transform each file with
# esbuild. The UI runs as classic scripts (global React, cross-file
# window.* exports), so each file is transformed individually, not bundled.
set -euo pipefail

ESBUILD_VERSION="0.24.2"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$HERE/crates/zkv/src/gui/assets"
SRC="$ASSETS/src"
OUT="$ASSETS/js"

mkdir -p "$OUT"

# Run everything from inside the assets dir so esbuild/tsc receive plain
# relative paths. Under Git Bash on Windows (the release runner) an absolute
# MSYS path like /c/... would reach the native esbuild binary mangled;
# relative paths sidestep that and are equivalent on macOS/Linux.
cd "$ASSETS"

# Type-check first (uses the local devDependencies pinned in package.json;
# run `npm ci` here once beforehand).
echo "  tsc --noEmit (type-check)"
npx --yes tsc --noEmit

# Resolve a source file by basename, preferring the TypeScript extension
# over the legacy one (the TS port lands one file per PR, so during the
# transition some sources are still .jsx/.js). Paths are relative to the
# assets dir (we cd'd there above).
resolve_src() {
  local base="$1" ts="$2" legacy="$3"
  if [[ -f "src/$base.$ts" ]]; then echo "src/$base.$ts"
  elif [[ -f "src/$base.$legacy" ]]; then echo "src/$base.$legacy"
  else echo "ERROR: no source for $base (.$ts/.$legacy)" >&2; return 1; fi
}

# Components (classic JSX runtime) -> js/<name>.js.
JSX_FILES=(icon chrome dashboard keys writeflow flows discover reference app)
for f in "${JSX_FILES[@]}"; do
  in="$(resolve_src "$f" tsx jsx)"
  echo "  esbuild  $in -> js/$f.js"
  npx --yes "esbuild@$ESBUILD_VERSION" "$in" \
    --jsx=transform \
    --jsx-factory=React.createElement \
    --jsx-fragment=React.Fragment \
    --target=es2020 \
    --outfile="js/$f.js" \
    --log-level=warning
done

# Plumbing IIFEs (no JSX). Their output lands at the assets root so
# index.html (/api.js, /forms.js) and assets.rs (include_bytes!) are
# unchanged; these are generated files, like js/*.js.
TS_FILES=(api forms)
for f in "${TS_FILES[@]}"; do
  in="$(resolve_src "$f" ts js)"
  echo "  esbuild  $in -> $f.js"
  npx --yes "esbuild@$ESBUILD_VERSION" "$in" \
    --target=es2020 \
    --outfile="$f.js" \
    --log-level=warning
done

echo "done: $(ls js | wc -l | tr -d ' ') files in $OUT (+ api.js, forms.js)"
