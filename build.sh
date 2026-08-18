#!/usr/bin/env bash
# Build a release desktop version of Flowbit.
#   ./build.sh                    — release bundle + install to /usr/local/bin/flowbit
#   ./build.sh --windows          — cross-build for Windows (x86_64-pc-windows-gnu)
#   ./build.sh --debug            — debug build (compiles faster)
#   ./build.sh --no-install       — don't copy the binary to /usr/local/bin
#   ./build.sh --bundles deb,rpm  — only the given bundle formats
#   ./build.sh <args...>          — other arguments are forwarded to `tauri build`
set -euo pipefail
cd "$(dirname "$0")"

# linuxdeploy/appimagetool run as an AppImage; on systems without FUSE that
# fails with "failed to run linuxdeploy". Extraction instead of FUSE-mounting fixes it.
export APPIMAGE_EXTRACT_AND_RUN=1

run_tauri() {
  if command -v bun >/dev/null 2>&1; then bun run tauri "$@"
  else npx tauri "$@"; fi
}

# --- argument parsing ---
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

# Target directory (empty for a native build, "<triple>/" for a cross-build)
TARGET_SUBDIR=""
if [[ -n "$TARGET_TRIPLE" ]]; then
  echo "⚠  Cross-building Tauri for Windows from Linux is experimental and may fail."
  echo "▶ Checking Rust target $TARGET_TRIPLE…"
  rustup target add "$TARGET_TRIPLE" >/dev/null 2>&1 || true
  ARGS+=(--target "$TARGET_TRIPLE")
  TARGET_SUBDIR="$TARGET_TRIPLE/"
fi

echo "▶ build ($PROFILE_DIR${TARGET_TRIPLE:+, $TARGET_TRIPLE})…"
# Timestamp marker: lets us tell whether the binary was actually rebuilt just
# now (vs. left over from a previous build), even if some bundle step fails.
MARKER="$(mktemp)"; trap 'rm -f "$MARKER"' EXIT

set +e
run_tauri build ${ARGS[@]+"${ARGS[@]}"}
BUILD_RC=$?
set -e
if [[ "$BUILD_RC" -ne 0 ]]; then
  echo "⚠  tauri build exited with code $BUILD_RC — some bundle format may have failed."
  echo "   The binary itself and other bundles may still have built; continuing."
fi

BUNDLE_DIR="src-tauri/target/${TARGET_SUBDIR}$PROFILE_DIR/bundle"
echo
echo "✅ Done. Artifacts:"
if [[ -d "$BUNDLE_DIR" ]]; then
  find "$BUNDLE_DIR" -maxdepth 2 -type f \
    \( -name '*.AppImage' -o -name '*.deb' -o -name '*.rpm' \
       -o -name '*.exe' -o -name '*.msi' -o -name '*.dmg' -o -name '*.app' \) \
    -printf '   %p\n' 2>/dev/null || true
else
  echo "   (bundle directory not found — see output above)"
fi

# --- install the binary to /usr/local/bin/flowbit (native Linux only) ---
BIN="src-tauri/target/${TARGET_SUBDIR}$PROFILE_DIR/Flowbit"
DEST="/usr/local/bin/flowbit"
if [[ "$INSTALL" -eq 1 ]]; then
  if [[ "$(uname)" != "Linux" ]]; then
    echo "ℹ  Installing to $DEST only happens on Linux — skipping."
  elif [[ ! -f "$BIN" ]]; then
    echo "❌ Binary $BIN not found — compilation failed, skipping install."; exit 1
  elif [[ "$BUILD_RC" -ne 0 && ! "$BIN" -nt "$MARKER" ]]; then
    # Build failed AND the binary wasn't rebuilt this run — so compilation
    # itself failed, not just a bundle step. Must not install the stale binary.
    echo "❌ Build failed, no fresh binary was produced — skipping install."; exit 1
  else
    echo
    # sudo usually needs a password — warn if the session isn't cached.
    if ! sudo -n true 2>/dev/null; then
      echo "🔒 Installing to $DEST needs sudo — enter your password below."
    fi
    echo "▶ Installing $BIN → $DEST…"
    # Via a temp file + atomic mv, so the replacement is all-or-nothing.
    TMP="$DEST.new.$$"
    if sudo install -Dm755 "$BIN" "$TMP" && sudo mv -f "$TMP" "$DEST"; then
      # Verify the file was actually replaced (byte-for-byte matches the new one).
      if cmp -s "$BIN" "$DEST"; then
        echo "✅ Installed and verified: $DEST"
      else
        echo "❌ File is in place, but its contents don't match $BIN"; exit 1
      fi
    else
      sudo rm -f "$TMP" 2>/dev/null || true
      echo "❌ Install failed (no sudo rights?). Manually:"
      echo "   sudo install -Dm755 \"$BIN\" \"$DEST\""
      exit 1
    fi
  fi
fi
