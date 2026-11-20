#!/usr/bin/env bash
# Builds the extension for each browser into dist/<browser>/.
#
#   ./build.sh                 # release build for firefox and chrome
#   ./build.sh firefox         # only firefox
#   PROFILE=dev ./build.sh     # unoptimized, faster build
#   ./build.sh --zip           # also create dist/session-manager-<browser>.zip (installable package)
#
# Requirements: rustup target add wasm32-unknown-unknown
#               cargo install wasm-bindgen-cli --version <same as wasm-bindgen in Cargo.lock>
# Optional:     wasm-opt (binaryen) to shrink the .wasm further.
set -euo pipefail
cd "$(dirname "$0")"

PROFILE="${PROFILE:-release}"
ZIP=0
BROWSERS=()
for arg in "$@"; do
  case "$arg" in
    --zip) ZIP=1 ;;
    *) BROWSERS+=("$arg") ;;
  esac
done
[ ${#BROWSERS[@]} -eq 0 ] && BROWSERS=(firefox chrome)

for b in "${BROWSERS[@]}"; do
  [ -f "$b/manifest.json" ] || { echo "error: unknown browser '$b' (no $b/manifest.json)" >&2; exit 1; }
done

# wasm-bindgen CLI must match the library version exactly.
want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ -n "$want" ] && [ "$want" != "$have" ]; then
  echo "error: wasm-bindgen CLI ${have:-not installed}, need $want:" >&2
  echo "  cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

echo "==> cargo build ($PROFILE)"
cargo build --lib --target wasm32-unknown-unknown --profile "$PROFILE"
target_dir="debug"
[ "$PROFILE" != "dev" ] && target_dir="$PROFILE"
wasm="target/wasm32-unknown-unknown/$target_dir/session_manager.wasm"

echo "==> wasm-bindgen"
rm -rf dist/pkg
wasm-bindgen --target web --no-typescript --out-dir dist/pkg "$wasm"

if [ "$PROFILE" != "dev" ] && command -v wasm-opt >/dev/null 2>&1; then
  echo "==> wasm-opt"
  wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    -o dist/pkg/session_manager_bg.wasm dist/pkg/session_manager_bg.wasm
fi

for b in "${BROWSERS[@]}"; do
  out="dist/$b"
  echo "==> assembling $out"
  rm -rf "$out"
  mkdir -p "$out"
  cp -r web/. "$out/"          # shared pages and loaders
  cp -r icons "$out/icons"     # shared icons
  cp -r "$b/." "$out/"         # browser specific manifest + platform shim
  cp -r dist/pkg "$out/pkg"    # compiled Rust
done

if [ "$ZIP" = 1 ]; then
  for b in "${BROWSERS[@]}"; do
    archive="dist/session-manager-$b.zip"
    rm -f "$archive"
    echo "==> packaging $archive"
    if command -v zip >/dev/null 2>&1; then
      (cd "dist/$b" && zip -qr "../session-manager-$b.zip" .)
    elif command -v pwsh >/dev/null 2>&1; then
      # PowerShell 7 writes '/' separators, which Firefox requires inside the archive.
      pwsh -NoProfile -Command "Compress-Archive -Path 'dist/$b/*' -DestinationPath '$archive'"
    else
      echo "warning: neither zip nor pwsh found; skipping $archive" >&2
    fi
  done
fi

echo "done: $(printf 'dist/%s ' "${BROWSERS[@]}")"
