#!/usr/bin/env bash
# Clean build artifacts.
#   ./clean.sh         — dist/, Rust target/ (cargo clean)
#   ./clean.sh --all   — the same, plus node_modules/
set -euo pipefail
cd "$(dirname "$0")"

echo "▶ clean…"
rm -rf dist
(cd src-tauri && cargo clean) || true

if [[ "${1:-}" == "--all" ]]; then
  echo "  + node_modules"
  rm -rf node_modules
fi

echo "✅ Done."
