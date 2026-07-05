#!/usr/bin/env bash
# Сборка/запуск Flowbit под Android.
#   ./android.sh init     — однократная инициализация Android-проекта
#   ./android.sh dev       — запуск на устройстве/эмуляторе с hot-reload
#   ./android.sh build     — релизный APK/AAB
#   ./android.sh <cmd> ... — доп. аргументы пробрасываются в `tauri android <cmd>`
#
# Требуется: Android SDK, NDK и JDK. Переменные ANDROID_HOME, NDK_HOME, JAVA_HOME.
set -euo pipefail
cd "$(dirname "$0")"

CMD="${1:-build}"
shift || true

run_tauri() {
  if command -v bun >/dev/null 2>&1; then bun run tauri "$@"
  else npx tauri "$@"; fi
}

# --- проверка окружения ---
missing=0
for var in ANDROID_HOME NDK_HOME JAVA_HOME; do
  if [[ -z "${!var:-}" ]]; then
    echo "⚠  $var не задан" >&2
    missing=1
  fi
done
if [[ "$missing" -eq 1 ]]; then
  echo "⚠  Настройте Android SDK/NDK/JDK, иначе сборка не пройдёт." >&2
  echo "   Пример: export ANDROID_HOME=\$HOME/Android/Sdk" >&2
  echo "           export NDK_HOME=\$ANDROID_HOME/ndk/<версия>" >&2
  echo "           export JAVA_HOME=/usr/lib/jvm/java-17-openjdk" >&2
fi

# нужные Rust-таргеты для Android
for target in aarch64-linux-android armv7-linux-androideabi \
              i686-linux-android x86_64-linux-android; do
  rustup target add "$target" >/dev/null 2>&1 || true
done

case "$CMD" in
  init)  echo "▶ android init…";  run_tauri android init "$@" ;;
  dev)   echo "▶ android dev…";   run_tauri android dev  "$@" ;;
  build) echo "▶ android build…"; run_tauri android build "$@" ;;
  *)     run_tauri android "$CMD" "$@" ;;
esac
