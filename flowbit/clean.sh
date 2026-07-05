#!/usr/bin/env bash
# Очистка артефактов сборки.
#   ./clean.sh         — dist/, Rust target/ (cargo clean)
#   ./clean.sh --all   — то же + node_modules/ и Android build/
set -euo pipefail
cd "$(dirname "$0")"

echo "▶ clean…"
rm -rf dist
(cd src-tauri && cargo clean) || true

if [[ "${1:-}" == "--all" ]]; then
  echo "  + node_modules, android build"
  rm -rf node_modules
  rm -rf src-tauri/gen/android/app/build
fi

echo "✅ Очищено."
