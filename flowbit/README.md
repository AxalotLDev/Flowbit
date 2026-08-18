# Flowbit

> **Vibecoded project.** This app was built almost entirely through AI-driven
> pair programming (spec-by-conversation, agent-written commits) rather than
> hand-crafted line by line. Expect the code to reflect that workflow: fast
> iteration, real-world bug fixes as they surfaced, and comments that call out
> the non-obvious platform quirks discovered along the way.

Flowbit is a small desktop app for downloading YouTube and Twitch videos
(and audio) with a couple of niceties baked in:

- **YouTube**: single videos or full playlists, quality/codec picking
  (H.264/VP9/AV1, AAC/Opus), multi-language audio track selection, and
  clipping a video/audio to a start–end time range.
- **Twitch**: VODs and clips, including live streams (records as they air).
- **Live log panel** streaming yt-dlp/ffmpeg output as it happens.
- **Automatic dependency management**: yt-dlp, ffmpeg/ffprobe and a QuickJS
  runtime (for yt-dlp's JS challenge solving) are downloaded on first launch
  — nothing to install manually, no bundled binaries to keep in sync by hand.
- **Auto-update for yt-dlp** on every startup, so extractor breakage from
  YouTube's frequent changes gets patched without a Flowbit release.
- **Automatic cookie fallback**: if YouTube's anti-bot check blocks an
  anonymous request, Flowbit retries once using cookies from whichever
  browser is already installed on the machine — no configuration required.

## Stack

- **Frontend**: React 19 + TypeScript, built with Vite.
- **Backend**: Rust, [Tauri v2](https://tauri.app) for the native shell.
- **Downloading**: [yt-dlp](https://github.com/yt-dlp/yt-dlp) + ffmpeg,
  invoked as managed subprocesses.

## Prerequisites

- [Bun](https://bun.sh) (package manager and script runner for the frontend)
- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain, via `rustup`)
- Platform build tools required by Tauri — see the
  [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/)
  for your OS (e.g. WebView2 on Windows, GTK/WebKitGTK on Linux).

yt-dlp, ffmpeg/ffprobe and the QuickJS runtime are **not** build
dependencies — Flowbit downloads them itself into the app's data directory
the first time it runs.

## Getting started

```bash
cd flowbit
bun install
```

### Run in development

```bash
bun run tauri dev
```

On Linux under Wayland, use the Wayland-specific dev script instead if the
window doesn't render correctly under XWayland:

```bash
bun run tauri:wayland
```

### Build a release bundle

```bash
bun run tauri build
```

This runs the frontend production build (`tsc && vite build`) and then
compiles and packages the Tauri app for the host platform (AppImage/deb on
Linux, MSI/NSIS on Windows, `.app`/dmg on macOS), per the `bundle` config in
`src-tauri/tauri.conf.json`.

### Other useful scripts

```bash
bun run dev      # Vite dev server only (frontend, no Tauri shell)
bun run build    # Frontend production build only
cargo check      # Rust: fast type-check of the backend (run from src-tauri/)
cargo test       # Rust: run the backend test suite (run from src-tauri/)
```

## Project layout

```
flowbit/
├── src/                  # React frontend (App.tsx is the whole UI)
├── src-tauri/
│   ├── src/
│   │   ├── lib.rs        # Tauri app setup, command registration
│   │   └── functions/    # Backend logic: youtube.rs, twitch.rs, playlist.rs,
│   │                      # get_info.rs, dependencies.rs, valid.rs
│   └── tauri.conf.json   # App window, bundle and CSP configuration
└── package.json
```
