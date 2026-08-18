#!/usr/bin/env bash
# Сборка релизной десктоп-версии Flowbit.
#   ./build.sh                    — релизный бандл + установка в /usr/local/bin/flowbit
#   ./build.sh --windows          — кросс-сборка под Windows (x86_64-pc-windows-gnu)
#   ./build.sh --debug            — отладочная сборка (быстрее компилируется)
#   ./build.sh --no-install       — не копировать бинарник в /usr/local/bin
#   ./build.sh --bundles deb,rpm  — только указанные форматы бандла
#   ./build.sh <args...>          — прочие аргументы пробрасываются в `tauri build`
set -euo pipefail
cd "$(dirname "$0")"

# linuxdeploy/appimagetool запускаются как AppImage; на системах без FUSE это
# падает с "failed to run linuxdeploy". Извлечение вместо FUSE-монтирования решает.
export APPIMAGE_EXTRACT_AND_RUN=1

run_tauri() {
  if command -v bun >/dev/null 2>&1; then bun run tauri "$@"
  else npx tauri "$@"; fi
}

# --- разбор аргументов ---
INSTALL=1
PROFILE_DIR=release
TARGET_TRIPLE=""
ARGS=()
for a in "$@"; do
  case "$a" in
    --no-install)   INSTALL=0 ;;
    --debug)        PROFILE_DIR=debug; ARGS+=("$a") ;;
    --windows|--win) TARGET_TRIPLE="x86_64-pc-windows-gnu"; INSTALL=0 ;;
    *)              ARGS+=("$a") ;;
  esac
done

# Каталог таргета (пусто для нативной сборки, "<triple>/" для кросс-сборки)
TARGET_SUBDIR=""
if [[ -n "$TARGET_TRIPLE" ]]; then
  echo "⚠  Кросс-сборка Tauri под Windows с Linux экспериментальна и может не собраться."
  echo "▶ Проверяю Rust-таргет $TARGET_TRIPLE…"
  rustup target add "$TARGET_TRIPLE" >/dev/null 2>&1 || true
  ARGS+=(--target "$TARGET_TRIPLE")
  TARGET_SUBDIR="$TARGET_TRIPLE/"
fi

echo "▶ build ($PROFILE_DIR${TARGET_TRIPLE:+, $TARGET_TRIPLE})…"
# Отметка времени: по ней поймём, что бинарник пересобрался именно сейчас
# (а не остался с прошлой сборки), даже если упадёт сборка какого-то бандла.
MARKER="$(mktemp)"; trap 'rm -f "$MARKER"' EXIT

set +e
run_tauri build ${ARGS[@]+"${ARGS[@]}"}
BUILD_RC=$?
set -e
if [[ "$BUILD_RC" -ne 0 ]]; then
  echo "⚠  tauri build завершился с кодом $BUILD_RC — возможно, не собрался какой-то бандл."
  echo "   Сам бинарник и остальные бандлы могли собраться; продолжаю."
fi

BUNDLE_DIR="src-tauri/target/${TARGET_SUBDIR}$PROFILE_DIR/bundle"
echo
echo "✅ Готово. Артефакты:"
if [[ -d "$BUNDLE_DIR" ]]; then
  find "$BUNDLE_DIR" -maxdepth 2 -type f \
    \( -name '*.AppImage' -o -name '*.deb' -o -name '*.rpm' \
       -o -name '*.exe' -o -name '*.msi' -o -name '*.dmg' -o -name '*.app' \) \
    -printf '   %p\n' 2>/dev/null || true
else
  echo "   (каталог бандлов не найден — см. вывод выше)"
fi

# --- установка бинарника в /usr/local/bin/flowbit (только нативный Linux) ---
BIN="src-tauri/target/${TARGET_SUBDIR}$PROFILE_DIR/Flowbit"
DEST="/usr/local/bin/flowbit"
if [[ "$INSTALL" -eq 1 ]]; then
  if [[ "$(uname)" != "Linux" ]]; then
    echo "ℹ  Установка в $DEST выполняется только на Linux — пропускаю."
  elif [[ ! -f "$BIN" ]]; then
    echo "❌ Бинарник $BIN не найден — компиляция не удалась, установку пропускаю."; exit 1
  elif [[ "$BUILD_RC" -ne 0 && ! "$BIN" -nt "$MARKER" ]]; then
    # Сборка упала И бинарник не пересобирался в этот запуск — значит упала именно
    # компиляция, а не бандл. Ставить старый бинарник нельзя.
    echo "❌ Сборка завершилась с ошибкой, свежий бинарник не создан — установку пропускаю."; exit 1
  else
    echo
    # sudo обычно требует пароль — предупреждаем, если сессия не закеширована.
    if ! sudo -n true 2>/dev/null; then
      echo "🔒 Для установки в $DEST нужен sudo — введите пароль ниже."
    fi
    echo "▶ Устанавливаю $BIN → $DEST…"
    # Через временный файл + атомарный mv, чтобы замена была целостной.
    TMP="$DEST.new.$$"
    if sudo install -Dm755 "$BIN" "$TMP" && sudo mv -f "$TMP" "$DEST"; then
      # Проверяем, что файл реально заменён (байт в байт совпадает с новым).
      if cmp -s "$BIN" "$DEST"; then
        echo "✅ Установлено и проверено: $DEST"
      else
        echo "❌ Файл на месте, но содержимое не совпадает с $BIN"; exit 1
      fi
    else
      sudo rm -f "$TMP" 2>/dev/null || true
      echo "❌ Не удалось установить (нет прав sudo?). Вручную:"
      echo "   sudo install -Dm755 \"$BIN\" \"$DEST\""
      exit 1
    fi
  fi
fi
