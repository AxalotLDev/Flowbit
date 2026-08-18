#!/usr/bin/env bash
# Запуск Flowbit в режиме разработки (десктоп, с hot-reload).
#   ./dev.sh              — обычный запуск
#   ./dev.sh --wayland    — принудительно использовать Wayland-бэкенд GTK
#   ./dev.sh <args...>    — любые доп. аргументы пробрасываются в `tauri dev`
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
