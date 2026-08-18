# CLAUDE.md

Guidance for agents (Claude Code and others) working in this repo.

## What this is

Flowbit: a Tauri v2 desktop app (React + TypeScript frontend, Rust backend)
for downloading YouTube/Twitch video and audio via yt-dlp + ffmpeg, run as
managed subprocesses. **Vibecoded** — built almost entirely through AI-driven
pair programming rather than hand-crafted line by line. See `README.md` for
the user-facing feature list and setup instructions; this file is for the
things the code and README don't already say.

## Build & verify

- `bun install` once, then `bun run tauri dev` / `bun run tauri build` (or
  the `./dev.sh` / `./build.sh` wrappers — see README's Helper scripts
  section for flags).
- Rust-only check: `cd src-tauri && cargo check` / `cargo test`.
- Frontend-only build: `bun run build` (`tsc && vite build`).
- `cargo test` has 3 pre-existing failing tests in
  `src-tauri/tests/integration_tests.rs` (`is_playlist_url` edge cases,
  one `validate_time_range` case). Don't treat them as a regression
  unless your change actually touches that logic.

## Layout

- `src/App.tsx` — the entire frontend UI (single file, no component split).
- `src-tauri/src/lib.rs` — Tauri command registration and startup (spawns
  dependency install, then yt-dlp self-update).
- `src-tauri/src/functions/` — backend logic: `youtube.rs` (downloads,
  yt-dlp process management, format/codec selection — the biggest file),
  `twitch.rs`, `playlist.rs`, `get_info.rs` (oembed + yt-dlp metadata
  merge), `dependencies.rs` (first-run binary download), `valid.rs` (URL
  validation).

## Conventions this codebase has settled on

- **Comments in English**, and only where they carry information the code
  doesn't already say — a non-obvious bug workaround, a platform quirk, a
  "why this and not the obvious alternative". Never restate the next
  line or the function name.
- yt-dlp/ffmpeg/QuickJS are **not** bundled or build dependencies — they're
  downloaded into the OS app-data dir on first run (`dependencies.rs`).
  Don't add them to `Cargo.toml`/`package.json` as binaries.
- All yt-dlp process spawns go through `run_ytdlp_status`/`run_ytdlp_output`
  in `youtube.rs`, which centralize: hiding the console window on Windows,
  forcing UTF-8 output from the Python subprocess, `--color never`
  (Windows drops color-support auto-detection when spawned without a
  console), and an automatic one-shot retry with
  `--cookies-from-browser <detected browser>` when YouTube's anti-bot
  check blocks an anonymous request. New download/metadata code should
  call these, not spawn yt-dlp directly.
- `decode_output()` tries UTF-8 first, falls back to windows-1251 — yt-dlp
  is forced to UTF-8 via env vars, but don't assume every subprocess
  output is guaranteed UTF-8 on Windows.
- Windows needs its own process-tree kill (`taskkill /T`) because yt-dlp
  (PyInstaller) forks a worker process; Unix uses process groups
  (`process_group(0)` + `killpg`). Both paths live in `kill_group()`.
