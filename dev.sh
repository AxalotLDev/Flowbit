#!/usr/bin/env bash
# Run Flowbit in development mode (desktop, with hot-reload).
#   ./dev.sh              — normal run
#   ./dev.sh --wayland    — force the Wayland GTK backend
#   ./dev.sh <args...>    — any extra arguments are forwarded to `tauri dev`
set -euo pipefail
cd "$(dirname "$0")"

run_tauri() {
  if command -v bun >/dev/null 2>&1; then bun run tauri "$@"
  else npx tauri "$@"; fi
}

if [[ "${1:-}" == "--wayland" ]]; then
  shift
  export GDK_BACKEND=wayland
  export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}"
  echo "▶ dev (Wayland)…"
else
  echo "▶ dev…"
fi

run_tauri dev "$@"
