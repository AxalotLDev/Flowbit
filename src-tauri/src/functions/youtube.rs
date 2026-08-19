use crate::functions::dependencies::{ffmpeg, quickjs, yt_dlp};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;

/// Cancellation message — the frontend recognizes it and doesn't show it as an error.
pub const CANCEL_MSG: &str = "Download cancelled";

/// Windows flag suppressing the child process's console window popup.
/// Without it, a cmd window flashes on every yt-dlp/ffmpeg launch.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Decodes yt-dlp/ffmpeg output to a string. Usually UTF-8, but on Windows
/// yt-dlp sometimes writes stdout/stderr in the system ANSI codepage
/// (windows-1251 for Russian locales), and Cyrillic turns into "diamonds"
/// (U+FFFD) when read as UTF-8. Try UTF-8 first, fall back to windows-1251.
pub fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1251.decode(bytes).0.into_owned(),
    }
}

/// yt-dlp may decide the output is a color-capable terminal (this platform
/// heuristic is unreliable when the process is spawned without a console,
/// as on Windows with CREATE_NO_WINDOW). As a backstop, strip control
/// sequences from the already-decoded line so the log panel doesn't show
/// raw garbage like `\x1b[0;33m`.
static ANSI_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap());

fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    ANSI_RE.replace_all(s, "")
}

pub fn new_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.kill_on_drop(true);
    // Force Python (yt-dlp) to emit UTF-8. On Windows the stdout pipe would
    // otherwise be encoded in the ANSI codepage (cp1251), turning Cyrillic in
    // output/paths into "diamonds" (U+FFFD) when decoded as UTF-8.
    cmd.env("PYTHONUTF8", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    // Own process group: yt-dlp (PyInstaller) forks a worker process; must
    // kill the whole group, not just the bootloader (else the worker keeps downloading).
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Kills the child process's entire tree/group by PID. On Unix the process
/// is spawned as its group leader (process_group(0)), so killpg(pid) kills
/// both the bootloader and the worker. On Windows: taskkill /T.
fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

/// Global cancellation signal for the current download: flag + Notify.
/// The flag catches a cancel pressed before the task entered select! (Notify::notify_waiters
/// only wakes already-registered waiters).
static CANCEL: Lazy<Notify> = Lazy::new(Notify::new);
static CANCEL_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Scopes the cancel flag's lifetime to a single download. Resets the flag
/// both on entering a download and on leaving it (any path — success, error,
/// cancel). Without the reset-on-exit, a cancelled download would leave
/// CANCEL_REQUESTED = true, and subsequent metadata requests (get_info →
/// fetch_duration_and_tracks → run_ytdlp_output) would instantly fail with
/// CANCEL_MSG, so the next video's duration would never resolve (00:00:00).
pub struct DownloadGuard;
impl DownloadGuard {
    pub fn new() -> Self {
        CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

fn is_cancelled() -> bool {
    CANCEL_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Waits for the process to finish, but kills it instantly on a cancel signal.
async fn wait_cancellable(
    child: &mut tokio::process::Child,
) -> Result<std::process::ExitStatus, String> {
    // Cancel may have been pressed before entering select! — check the flag up front.
    if is_cancelled() {
        kill_group(child.id());
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(CANCEL_MSG.to_string());
    }
    tokio::select! {
        res = child.wait() => res.map_err(|e| e.to_string()),
        _ = CANCEL.notified() => {
            kill_group(child.id());
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(CANCEL_MSG.to_string())
        }
    }
}

pub async fn run_ffmpeg(args: Vec<String>) -> Result<std::process::ExitStatus, String> {
    let mut child = new_command(&ffmpeg())
        .args(&args)
        .spawn()
        .map_err(|e| format!("ffmpeg failed: {e}"))?;
    wait_cancellable(&mut child).await
}

/// Tauri command: instantly aborts the current download. Already-downloaded
/// files (including finished playlist entries and partial fragments) are not deleted.
#[tauri::command]
pub fn cancel_download() {
    CANCEL_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    CANCEL.notify_waiters();
}

/// Network resilience/speed options — mirrors the flags from the `ytdl` fish
/// function: infinite retries, resume, parallel fragments, timeouts, no mtime.
pub fn network_args() -> Vec<String> {
    [
        "--no-mtime",
        "--retries",
        "infinite",
        "--fragment-retries",
        "infinite",
        "--file-access-retries",
        "10",
        // 5s for a fast reconnect on a short drop. Retries are infinite, so
        // it can't get stuck, and a prolonged issue can still be cancelled manually.
        "--socket-timeout",
        "5",
        "--http-chunk-size",
        "10M",
        "--concurrent-fragments",
        "4",
        "--continue",
        "--progress",
        "--newline",
        "--compat-options",
        "filename-sanitization",
        // Don't show the "version is over 90 days old" warning on every
        // download; the background update check at startup keeps it current.
        "--no-update",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Resolves the destination directory: given path → platform downloads
/// directory (via the Tauri path API) → fallbacks.
pub fn resolve_out_dir(app: &AppHandle, path: Option<String>) -> PathBuf {
    if let Some(p) = path {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(d) = app.path().download_dir() {
        return d;
    }
    if let Some(d) = dirs::download_dir() {
        return d;
    }
    if let Ok(d) = crate::functions::dependencies::app_data_root(app) {
        return d;
    }
    std::env::current_dir().unwrap_or_else(|_| ".".into())
}

/// Checks for a newer yt-dlp version and updates the binary (`yt-dlp -U`).
/// Returns true if yt-dlp is already current or was updated successfully.
/// Output streams to the frontend as "ytdlp-log" events.
pub async fn ytdlp_self_update(app: Option<AppHandle>) -> Result<bool, String> {
    let status = run_ytdlp_status(
        vec!["--update".into()],
        "yt-dlp update failed".to_string(),
        app,
    )
    .await?;
    Ok(status.success())
}

/// Tauri command: update yt-dlp on the frontend's request.
#[tauri::command]
pub async fn update_ytdlp(app: AppHandle) -> Result<bool, String> {
    ytdlp_self_update(Some(app)).await
}

#[derive(Serialize, Clone)]
pub struct DownloadResult {
    pub path: String,
    pub file_size_mb: f64,
}

pub struct DownloadState;
impl DownloadState {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum DownloadMode {
    Video,
    Audio,
}

#[inline]
pub fn quality_to_format(q: Quality) -> &'static str {
    // Prioritize universally playable codecs — H.264 (avc1) + AAC (mp4a) in mp4:
    // any player, including VLC, handles them. YouTube defaults to VP9/AV1 +
    // Opus, which makes VLC report "codec not found". VP9/AV1 is only a
    // fallback (e.g. for 4K, where H.264 isn't available).
    match q {
        Quality::Best => {
            "bv*[vcodec^=avc1]+ba[acodec^=mp4a]/bv*[ext=mp4]+ba[ext=m4a]/bv*+ba/b"
        }
        Quality::High => {
            "bv*[height<=1080][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=1080][ext=mp4]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]/b"
        }
        Quality::Medium => {
            "bv*[height<=720][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=720][ext=mp4]+ba[ext=m4a]/bv*[height<=720]+ba/b[height<=720]/b"
        }
        Quality::Low => {
            "bv*[height<=480][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height<=480][ext=mp4]+ba[ext=m4a]/bv*[height<=480]+ba/b[height<=480]/b"
        }
        Quality::Worst => "wv*[vcodec^=avc1]+wa[acodec^=mp4a]/wv*+wa/w",
    }
}

/// yt-dlp bitrate filter for the audio-only tiers, mirroring `quality_to_format`'s
/// height buckets but for `abr` (average audio bitrate, kbps). Best leaves the
/// track uncapped; High/Medium/Low cap `abr` at thresholds chosen so each tier
/// lands on a genuinely different YouTube itag (251/140 ~128k, 250 ~70k, 249/139
/// ~48-50k); Worst switches from best-audio to worst-audio instead of filtering.
/// Sources that don't report `abr` (e.g. Twitch's HLS audio) never satisfy the
/// capped filter, so `build_audio_format`'s uncapped fallback takes over —
/// there's no separate Twitch-specific path.
#[inline]
fn quality_to_audio_base_and_filter(q: Quality) -> (&'static str, &'static str) {
    match q {
        Quality::Best => ("ba", ""),
        Quality::High => ("ba", "[abr<=160]"),
        Quality::Medium => ("ba", "[abr<=80]"),
        Quality::Low => ("ba", "[abr<=50]"),
        Quality::Worst => ("wa", ""),
    }
}

/// Standalone quality-only audio selector (no language/codec constraints),
/// for callers like the playlist downloader that don't offer those pickers.
pub fn quality_to_audio_format(q: Quality) -> String {
    let (base, qf) = quality_to_audio_base_and_filter(q);
    if qf.is_empty() {
        base.to_string()
    } else {
        format!("{base}{qf}/{base}")
    }
}

/// YouTube client that surfaces multi-language audio dubs. The default client
/// only returns the original track; web_embedded returns all dubs. Specify
/// both so video stays at max quality (up to 4K) while audio covers all languages.
const YT_MULTI_AUDIO_CLIENT: &str = "youtube:player_client=default,web_embedded";

/// Reads the path yt-dlp wrote via `--print-to-file`. Usually UTF-8, but
/// decode leniently (UTF-8 → cp1251) rather than with a strict read_to_string.
pub async fn read_printed_path(path_file: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path_file).await.ok()?;
    let content = decode_output(&bytes);
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .map(String::from)
}

fn clipped_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let ext = file.extension().unwrap_or_default().to_string_lossy();
    file.with_file_name(format!("{stem}_cut.{ext}"))
}

/// yt-dlp video-codec filter for our short name. YouTube's VP9 shows up as
/// both `vp9` and `vp09.*` — cover both with a regex match (~=).
fn vcodec_filter(codec: Option<&str>) -> Option<&'static str> {
    match codec {
        Some("h264") => Some("[vcodec^=avc1]"),
        Some("vp9") => Some("[vcodec~='^vp0?9']"),
        Some("av1") => Some("[vcodec^=av01]"),
        _ => None,
    }
}

fn acodec_filter(codec: Option<&str>) -> Option<&'static str> {
    match codec {
        Some("aac") => Some("[acodec^=mp4a]"),
        Some("opus") => Some("[acodec^=opus]"),
        _ => None,
    }
}

/// Container (--merge-output-format) for the audio choice. Opus isn't
/// officially supported in mp4 — use mkv (plays everywhere), else mp4.
pub fn merge_container(audio_codec: Option<&str>) -> &'static str {
    if audio_codec == Some("opus") {
        "mkv"
    } else {
        "mp4"
    }
}

/// Full format selector for video mode: quality + video codec + audio codec
/// + audio track language. Builds a preference list from exact match to
/// progressively more general (via `/`), so the download doesn't fail when
/// there's no exact combination. For "auto" (no codec given), prefer
/// widely-compatible H.264 + AAC — any player, including VLC, handles them.
fn build_video_format(
    q: Quality,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    audio_lang: Option<&str>,
) -> String {
    let (vbase, abase, cap) = match q {
        Quality::Best => ("bv*", "ba", ""),
        Quality::High => ("bv*", "ba", "[height<=1080]"),
        Quality::Medium => ("bv*", "ba", "[height<=720]"),
        Quality::Low => ("bv*", "ba", "[height<=480]"),
        Quality::Worst => ("wv*", "wa", ""),
    };
    let lang = audio_lang.filter(|l| !l.is_empty());
    let langf = lang.map(|l| format!("[language={l}]")).unwrap_or_default();

    // "Auto" uses compatible codecs; an explicit choice uses the given one.
    let vf = vcodec_filter(video_codec).unwrap_or("[vcodec^=avc1]");
    let af = acodec_filter(audio_codec).unwrap_or("[acodec^=mp4a]");
    let a_explicit = acodec_filter(audio_codec).is_some();

    let v = |extra: &str| format!("{vbase}{cap}{extra}");
    let a = |extra: &str| format!("{abase}{langf}{extra}");

    let mut prefs: Vec<String> = Vec::new();
    // 1. exact combination (H.264 + AAC for auto)
    prefs.push(format!("{}+{}", v(vf), a(af)));
    // 2. chosen video codec + any audio (in the wanted language)
    prefs.push(format!("{}+{}", v(vf), a("")));
    // 3. if audio codec was given explicitly — any video codec + wanted audio
    if a_explicit {
        prefs.push(format!("{}+{}", v(""), a(af)));
    }
    // 4. any video codec + any audio (in the wanted language)
    prefs.push(format!("{}+{}", v(""), a("")));
    // 5. if a language was set — same options without it (track may not exist)
    if lang.is_some() {
        prefs.push(format!("{}+{}", v(vf), abase));
        prefs.push(format!("{}+{}", v(""), abase));
    }
    // 6. final generic fallback
    prefs.push(format!("b{cap}"));
    prefs.push("b".into());

    prefs.dedup();
    prefs.join("/")
}

/// Format selector for audio-only mode: pick the source track by quality
/// tier, language, and codec (output container is set separately via
/// --audio-format).
fn build_audio_format(quality: Quality, audio_codec: Option<&str>, audio_lang: Option<&str>) -> String {
    let lang = audio_lang.filter(|l| !l.is_empty());
    let langf = lang.map(|l| format!("[language={l}]")).unwrap_or_default();
    let af = acodec_filter(audio_codec);
    let (base, qf) = quality_to_audio_base_and_filter(quality);

    let mut prefs: Vec<String> = Vec::new();
    if let Some(f) = af {
        prefs.push(format!("{base}{langf}{qf}{f}"));
    }
    prefs.push(format!("{base}{langf}{qf}"));
    if lang.is_some() {
        if let Some(f) = af {
            prefs.push(format!("{base}{qf}{f}"));
        }
    }
    prefs.push(format!("{base}{qf}"));

    // Uncapped fallback: sources that don't report `abr` (Twitch) or videos
    // with no track at/under the requested tier would otherwise match nothing.
    if !qf.is_empty() {
        prefs.push(format!("{base}{langf}"));
        prefs.push(base.to_string());
    }

    prefs.dedup();
    prefs.join("/")
}

/// Fixed codec display order, so the UI stays stable.
const VCODEC_ORDER: [&str; 3] = ["h264", "vp9", "av1"];
const ACODEC_ORDER: [&str; 2] = ["aac", "opus"];

fn canon_vcodec(vcodec: &str) -> Option<&'static str> {
    if vcodec.starts_with("avc1") || vcodec.starts_with("avc3") || vcodec.starts_with("h264") {
        Some("h264")
    } else if vcodec.starts_with("vp9") || vcodec.starts_with("vp09") {
        Some("vp9")
    } else if vcodec.starts_with("av01") {
        Some("av1")
    } else {
        None
    }
}

fn canon_acodec(acodec: &str) -> Option<&'static str> {
    if acodec.starts_with("mp4a") || acodec.starts_with("aac") {
        Some("aac")
    } else if acodec.starts_with("opus") {
        Some("opus")
    } else {
        None
    }
}

pub fn parse_video_codecs(json: &serde_json::Value) -> Vec<String> {
    let mut found = std::collections::HashSet::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            if let Some(v) = f["vcodec"].as_str() {
                if v != "none" {
                    if let Some(c) = canon_vcodec(v) {
                        found.insert(c);
                    }
                }
            }
        }
    }
    VCODEC_ORDER
        .iter()
        .filter(|c| found.contains(**c))
        .map(|c| c.to_string())
        .collect()
}

pub fn parse_audio_codecs(json: &serde_json::Value) -> Vec<String> {
    let mut found = std::collections::HashSet::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            let is_audio = f["acodec"].as_str().is_some_and(|a| a != "none");
            if is_audio {
                if let Some(a) = f["acodec"].as_str().and_then(canon_acodec) {
                    found.insert(a);
                }
            }
        }
    }
    ACODEC_ORDER
        .iter()
        .filter(|c| found.contains(**c))
        .map(|c| c.to_string())
        .collect()
}

/// Extracts audio track language codes from yt-dlp JSON (unique, in order).
pub fn parse_audio_langs(json: &serde_json::Value) -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    if let Some(formats) = json["formats"].as_array() {
        for f in formats {
            // Audio-only tracks (no video) with a language set.
            let audio_only = f["vcodec"].as_str() == Some("none")
                && f["acodec"].as_str().is_some_and(|a| a != "none");
            if audio_only {
                if let Some(lang) = f["language"].as_str() {
                    if !lang.is_empty() && !langs.iter().any(|x| x == lang) {
                        langs.push(lang.to_string());
                    }
                }
            }
        }
    }
    langs
}

/// Video metadata from a single `yt-dlp -J` call. Used as a fallback source
/// when oembed is unavailable (401/404 for embed-restricted, age- or
/// region-locked videos).
#[derive(Default)]
pub struct YtMeta {
    pub duration: Option<u64>,
    pub audio_tracks: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub thumbnail: Option<String>,
    /// Failure reason (the last "ERROR: ..." line from yt-dlp), set when
    /// neither web_embedded nor the default client could return metadata —
    /// so the user sees the real cause (e.g. "Sign in to confirm you're not
    /// a bot") instead of a generic "failed to fetch" message.
    pub error: Option<String>,
}

/// The last "ERROR: ..." line from yt-dlp's stderr, if any — otherwise the
/// whole non-empty stderr.
fn extract_error_reason(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR:"))
        .map(|l| l.trim().to_string())
        .or_else(|| {
            let trimmed = stderr.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

/// A single `yt-dlp -J` call. `multi_audio` enables the web_embedded client,
/// which surfaces multi-language dubs but sometimes returns an incomplete
/// response (missing duration/format) — hence the fallback to the default client.
async fn run_meta_json(
    url: &str,
    multi_audio: bool,
    extra_args: &[String],
) -> Result<serde_json::Value, String> {
    let mut args = vec!["-J".to_string(), "--no-playlist".to_string()];
    if multi_audio {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.extend_from_slice(extra_args);
    args.push(url.to_string());
    let output = spawn_ytdlp_output(&args, "Failed to fetch info", None).await?;
    let stderr_text = decode_output(&output.stderr);
    if !output.status.success() {
        return Err(extract_error_reason(&stderr_text).unwrap_or_else(|| "yt-dlp failed".into()));
    }
    let json = serde_json::from_str::<serde_json::Value>(&decode_output(&output.stdout))
        .map_err(|e| format!("Invalid yt-dlp output: {e}"))?;
    // On a failed extraction (incl. the "not a bot" captcha), yt-dlp prints
    // "null" and exits 0. That's not metadata — treat it as a failure so the
    // fallback to another client kicks in, instead of silently getting
    // duration = null (00:00:00).
    if !json.is_object() {
        return Err(
            extract_error_reason(&stderr_text).unwrap_or_else(|| "yt-dlp returned no metadata".into())
        );
    }
    Ok(json)
}

/// Tries to get valid metadata JSON, resilient to the "not a bot" captcha.
/// web_embedded first (surfaces dubs); if empty/captcha'd, the default
/// client with a few retries back-to-back — no artificial delay between
/// them (speed over letting the captcha "clear itself").
async fn fetch_meta_json_resilient(url: &str) -> Result<serde_json::Value, String> {
    // Skip straight to the cookies that already worked earlier this
    // session — the video-by-video anonymous-attempt-then-cookie-retry
    // dance below only needs to happen once per process, not once per video.
    if let Some(browser) = WORKING_COOKIE_BROWSER.get() {
        let cookie_args = vec!["--cookies-from-browser".to_string(), (*browser).to_string()];
        if let Ok(j) = run_meta_json(url, true, &cookie_args).await {
            return Ok(j);
        }
        // Cached browser stopped working (closed profile, cleared cookies) —
        // fall through to full discovery below.
    }

    let mut last_err = match run_meta_json(url, true, &[]).await {
        Ok(j) => return Ok(j),
        Err(e) => e,
    };
    for _ in 0..3 {
        match run_meta_json(url, false, &[]).await {
            Ok(j) => return Ok(j),
            Err(e) => last_err = e,
        }
    }
    // Plain retries exhausted. If the cause is YouTube's anti-bot check,
    // retries won't fix it regardless of browser cookies, so try every
    // detected browser's cookies here exactly once each (not on every
    // attempt above — that would multiply the time to a final failure).
    if looks_like_bot_check(&last_err) {
        for browser in COOKIE_BROWSERS.iter() {
            let cookie_args = vec!["--cookies-from-browser".to_string(), (*browser).to_string()];
            match run_meta_json(url, false, &cookie_args).await {
                Ok(j) => {
                    let _ = WORKING_COOKIE_BROWSER.set(browser);
                    return Ok(j);
                }
                Err(e) => {
                    last_err = e;
                    if !looks_like_bot_check(&last_err) {
                        break;
                    }
                }
            }
        }
    }
    Err(last_err)
}

pub async fn fetch_yt_meta(url: &str, app: Option<AppHandle>) -> YtMeta {
    let json = match fetch_meta_json_resilient(url).await {
        Ok(j) => j,
        Err(reason) => {
            let duration = fetch_duration(url).await;
            if duration.is_none() {
                emit_log(&app, &format!("[flowbit] Failed to fetch video data: {reason}"));
            }
            return YtMeta {
                duration,
                error: Some(reason),
                ..Default::default()
            };
        }
    };
    let s = |k: &str| json[k].as_str().filter(|v| !v.is_empty()).map(String::from);
    let is_live = json["is_live"].as_bool().unwrap_or(false);
    let mut duration = json["duration"].as_f64().map(|d| d as u64);
    // A live stream has no fixed duration — respawning yt-dlp just to get
    // "NA" back again wastes a subprocess call.
    if duration.is_none() && !is_live {
        duration = fetch_duration(url).await;
    }
    YtMeta {
        duration,
        audio_tracks: parse_audio_langs(&json),
        video_codecs: parse_video_codecs(&json),
        audio_codecs: parse_audio_codecs(&json),
        title: s("title").or_else(|| s("fulltitle")),
        author: s("uploader").or_else(|| s("channel")).or_else(|| s("uploader_id")),
        thumbnail: s("thumbnail"),
        error: None,
    }
}

pub fn parse_time_to_secs(t: &str) -> Option<u64> {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h = parts[0].parse::<u64>().ok()?;
    let m = parts[1].parse::<u64>().ok()?;
    let s = parts[2].parse::<u64>().ok()?;
    if m >= 60 || s >= 60 {
        return None;
    }
    Some(h * 3600 + m * 60 + s)
}

pub fn section_changed(start: &str, end: &str, duration: Option<u64>) -> bool {
    let start_secs = parse_time_to_secs(start).unwrap_or(0);
    if start_secs != 0 {
        return true;
    }
    let end_secs = match parse_time_to_secs(end) {
        Some(s) => s,
        None => return true,
    };
    if end_secs == 0 {
        return false;
    }
    match duration {
        Some(dur) => end_secs < dur.saturating_sub(1),
        None => true,
    }
}

pub async fn fetch_duration(url: &str) -> Option<u64> {
    let args: Vec<String> = vec![
        "--print".into(),
        "duration".into(),
        "--no-playlist".into(),
        url.into(),
    ];
    let output = run_ytdlp_output(args, "Failed to fetch duration".to_string(), None)
        .await
        .ok()?;
    // Take the first non-empty line and ignore "NA" (yt-dlp prints that for a
    // missing duration, e.g. live streams). Parsing line-by-line rather than
    // the whole buffer is resilient to \r\n and extra output on Windows.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "NA")
        .and_then(|l| l.parse::<f64>().ok())
        .map(|d| d as u64)
}

pub async fn cleanup_temp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let should_remove = name.ends_with(".part")
            || name.ends_with(".tmp")
            || name.ends_with(".frag")
            || name.starts_with("temp_");
        if should_remove {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

pub fn file_mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

fn emit_log(app: &Option<AppHandle>, line: &str) {
    if let Some(a) = app {
        let _ = a.emit("ytdlp-log", line);
    }
}

/// Reads output line-by-line from raw bytes and decodes each line via
/// decode_output. Unlike tokio's `.lines()` (UTF-8 only, aborts on the first
/// non-UTF-8 line), this doesn't lose log lines and correctly shows Cyrillic
/// from yt-dlp's cp1251 output on Windows.
async fn stream_lines(
    reader: impl tokio::io::AsyncRead + Unpin,
    app: Option<AppHandle>,
) -> Vec<String> {
    let mut segments = BufReader::new(reader).split(b'\n');
    let mut lines = Vec::new();
    while let Ok(Some(seg)) = segments.next_segment().await {
        let mut line = decode_output(&seg);
        line.retain(|c| c != '\r');
        if line.contains('\x1b') {
            line = strip_ansi(&line).into_owned();
        }
        emit_log(&app, &line);
        lines.push(line);
    }
    lines
}

/// Flags shared by every yt-dlp run: ffmpeg location, the JS runtime for
/// reading n-sig, and `--color never` — platform detection of ANSI color
/// support is unreliable when the process is spawned without a console
/// (Windows, CREATE_NO_WINDOW), and without disabling it explicitly the log
/// panel sometimes gets raw control sequences.
fn default_ytdlp_args() -> Vec<String> {
    vec![
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--js-runtimes".into(),
        format!("quickjs:{}", quickjs()),
        "--color".into(),
        "never".into(),
    ]
}

/// YouTube sometimes demands an "I'm not a bot" confirmation for anonymous
/// requests — no amount of retrying gets past that without authentication,
/// but yt-dlp has `--cookies-from-browser`, which reads an already-logged-in
/// session straight from the user's installed browser.
fn looks_like_bot_check(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("sign in to confirm") || s.contains("confirm you\u{2019}re not a bot") || s.contains("confirm you're not a bot")
}

/// Every profile found on disk, most-likely-to-work first — Firefox doesn't
/// depend on the OS keyring to decrypt cookies (Chromium-family browsers do,
/// via libsecret/Keychain/DPAPI, and that key store isn't always reachable,
/// e.g. no keyring daemon in the session), so it's checked first. The rest
/// follow in roughly descending popularity. yt-dlp only understands these
/// browser names for `--cookies-from-browser`: brave, chrome, chromium,
/// edge, firefox, opera, safari, vivaldi, whale.
/// Checked once per process run: profile layout doesn't change at runtime.
fn detect_browsers() -> Vec<&'static str> {
    let mut found = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(roaming) = std::env::var("APPDATA") {
            if Path::new(&format!(r"{roaming}\Mozilla\Firefox\Profiles")).is_dir() {
                found.push("firefox");
            }
            if Path::new(&format!(r"{roaming}\Opera Software\Opera Stable")).is_dir() {
                found.push("opera");
            }
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let candidates = [
                ("chrome", format!(r"{local}\Google\Chrome\User Data")),
                ("edge", format!(r"{local}\Microsoft\Edge\User Data")),
                ("brave", format!(r"{local}\BraveSoftware\Brave-Browser\User Data")),
                ("chromium", format!(r"{local}\Chromium\User Data")),
                ("vivaldi", format!(r"{local}\Vivaldi\User Data")),
                ("whale", format!(r"{local}\Naver\Naver Whale\User Data")),
            ];
            for (name, path) in candidates {
                if Path::new(&path).is_dir() {
                    found.push(name);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            if home.join("Library/Application Support/Firefox/Profiles").is_dir() {
                found.push("firefox");
            }
            let candidates = [
                ("chrome", home.join("Library/Application Support/Google/Chrome")),
                ("edge", home.join("Library/Application Support/Microsoft Edge")),
                ("brave", home.join("Library/Application Support/BraveSoftware/Brave-Browser")),
                ("chromium", home.join("Library/Application Support/Chromium")),
                ("vivaldi", home.join("Library/Application Support/Vivaldi")),
                ("opera", home.join("Library/Application Support/com.operasoftware.Opera")),
            ];
            for (name, path) in candidates {
                if path.is_dir() {
                    found.push(name);
                }
            }
            if home.join("Library/Cookies/Cookies.binarycookies").is_file() {
                found.push("safari");
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = dirs::home_dir() {
            if home.join(".mozilla/firefox/profiles.ini").is_file() {
                found.push("firefox");
            }
            let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
            let candidates = [
                ("chrome", config.join("google-chrome")),
                ("chromium", config.join("chromium")),
                ("edge", config.join("microsoft-edge")),
                ("brave", config.join("BraveSoftware/Brave-Browser")),
                ("vivaldi", config.join("vivaldi")),
                ("opera", config.join("opera")),
                ("whale", config.join("naver-whale")),
            ];
            for (name, path) in candidates {
                if path.is_dir() {
                    found.push(name);
                }
            }
        }
    }

    found
}

static COOKIE_BROWSERS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(detect_browsers);

fn has_cookie_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == "--cookies-from-browser")
}

/// Once any yt-dlp call in this process needed a specific browser's cookies
/// to get past YouTube's "not a bot" check, every later call reuses it from
/// the start. Without this, every single video re-discovers the same wall
/// from scratch: an anonymous attempt, a scary ERROR line, then a retry —
/// twice per video (once for metadata, once for the download itself).
static WORKING_COOKIE_BROWSER: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Prepends the cached working browser's cookies to `args`, unless the
/// caller already set cookies explicitly.
fn apply_cached_cookies(args: &mut Vec<String>) {
    if has_cookie_flag(args) {
        return;
    }
    if let Some(browser) = WORKING_COOKIE_BROWSER.get() {
        args.push("--cookies-from-browser".into());
        args.push((*browser).to_string());
    }
}

async fn spawn_ytdlp_status(
    args: &[String],
    error_format: &str,
    app: Option<AppHandle>,
) -> Result<(std::process::ExitStatus, Vec<String>), String> {
    let default_args = default_ytdlp_args();

    let mut child = new_command(&yt_dlp())
        .args(default_args.iter().chain(args.iter()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{error_format}: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let app_out = app.clone();
    let app_err = app.clone();

    let h_out = tokio::spawn(async move {
        if let Some(s) = stdout {
            stream_lines(s, app_out).await;
        }
    });

    let h_err = tokio::spawn(async move {
        match stderr {
            Some(s) => stream_lines(s, app_err).await,
            None => Vec::new(),
        }
    });

    let status = wait_cancellable(&mut child).await?;

    let _ = h_out.await;
    let err_lines = h_err.await.unwrap_or_default();

    Ok((status, err_lines))
}

pub async fn run_ytdlp_status(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::ExitStatus, String> {
    let mut args = args;
    apply_cached_cookies(&mut args);
    let (mut status, err_lines) = spawn_ytdlp_status(&args, &error_format, app.clone()).await?;
    if status.success() || has_cookie_flag(&args) || is_cancelled() {
        return Ok(status);
    }
    if !looks_like_bot_check(&err_lines.join("\n")) {
        return Ok(status);
    }
    // Try every detected browser's cookies in turn — a bot-check failure
    // from one (e.g. Chrome's keyring-locked cookie DB) doesn't mean
    // another (e.g. Firefox) will fail the same way.
    for browser in COOKIE_BROWSERS.iter() {
        emit_log(
            &app,
            &format!("[flowbit] YouTube requires \"I'm not a bot\" confirmation — trying cookies from {browser}…"),
        );
        let mut retry_args = args.clone();
        retry_args.push("--cookies-from-browser".into());
        retry_args.push((*browser).into());
        let (retry_status, retry_err_lines) =
            spawn_ytdlp_status(&retry_args, &error_format, app.clone()).await?;
        status = retry_status;
        if status.success() {
            let _ = WORKING_COOKIE_BROWSER.set(browser);
            return Ok(status);
        }
        if !looks_like_bot_check(&retry_err_lines.join("\n")) {
            return Ok(status);
        }
    }
    Ok(status)
}

async fn spawn_ytdlp_output(
    args: &[String],
    error_format: &str,
    app: Option<AppHandle>,
) -> Result<std::process::Output, String> {
    let default_args = default_ytdlp_args();

    let mut child = new_command(&yt_dlp())
        .args(default_args.iter().chain(args.iter()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{error_format}: {e}"))?;

    let stderr = child.stderr.take();
    let app_err = app.clone();

    let h_err = tokio::spawn(async move {
        match stderr {
            Some(s) => stream_lines(s, app_err).await,
            None => Vec::new(),
        }
    });

    // Grab the pid up front: on the cancel branch child has already moved into wait_with_output.
    let pid = child.id();
    if is_cancelled() {
        kill_group(pid);
        return Err(CANCEL_MSG.into());
    }
    let mut output = tokio::select! {
        out = child.wait_with_output() => out.map_err(|e| format!("{error_format}: {e}"))?,
        _ = CANCEL.notified() => {
            kill_group(pid);   // kill the whole group, not just the bootloader
            return Err(CANCEL_MSG.into());
        }
    };

    // stderr was consumed line-by-line in stream_lines (for live logs), so
    // output.stderr is empty after wait_with_output — rebuild it from the
    // collected lines, otherwise callers (twitch.rs, run_meta_json) can't
    // show the real yt-dlp error reason.
    if let Ok(err_lines) = h_err.await {
        if !err_lines.is_empty() {
            output.stderr = err_lines.join("\n").into_bytes();
        }
    }

    Ok(output)
}

pub async fn run_ytdlp_output(
    args: Vec<String>,
    error_format: String,
    app: Option<AppHandle>,
) -> Result<std::process::Output, String> {
    let mut args = args;
    apply_cached_cookies(&mut args);
    let mut output = spawn_ytdlp_output(&args, &error_format, app.clone()).await?;
    if output.status.success() || has_cookie_flag(&args) || is_cancelled() {
        return Ok(output);
    }
    if !looks_like_bot_check(&decode_output(&output.stderr)) {
        return Ok(output);
    }
    for browser in COOKIE_BROWSERS.iter() {
        emit_log(
            &app,
            &format!("[flowbit] YouTube requires \"I'm not a bot\" confirmation — trying cookies from {browser}…"),
        );
        let mut retry_args = args.clone();
        retry_args.push("--cookies-from-browser".into());
        retry_args.push((*browser).into());
        output = spawn_ytdlp_output(&retry_args, &error_format, app.clone()).await?;
        if output.status.success() {
            let _ = WORKING_COOKIE_BROWSER.set(browser);
            return Ok(output);
        }
        if !looks_like_bot_check(&decode_output(&output.stderr)) {
            return Ok(output);
        }
    }
    Ok(output)
}


#[tauri::command]
pub async fn download_video(
    app: AppHandle,
    url: String,
    path: Option<String>,
    quality: Option<Quality>,
    mode: Option<DownloadMode>,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
    audio_lang: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
) -> Result<DownloadResult, String> {
    let _guard = DownloadGuard::new();
    let out_dir = resolve_out_dir(&app, path);
    let app = Some(app);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(
            &url,
            &out_dir,
            quality.unwrap_or(Quality::Best),
            start,
            end,
            duration,
            audio_lang,
            audio_codec,
            app,
        )
        .await;
    }

    let start_str = start.as_deref().unwrap_or("00:00:00").to_string();

    let resolved_duration = match duration {
        Some(d) => Some(d),
        None => fetch_duration(&url).await,
    };

    let end_str = match end.as_deref() {
        Some(e) if e != "00:00:00" => e.to_string(),
        _ => match resolved_duration {
            Some(dur) => {
                let h = dur / 3600;
                let m = (dur % 3600) / 60;
                let s = dur % 60;
                format!("{:02}:{:02}:{:02}", h, m, s)
            }
            None => "00:00:00".to_string(),
        },
    };

    let need_section = section_changed(&start_str, &end_str, resolved_duration);

    let format = build_video_format(
        quality.unwrap_or(Quality::Best),
        video_codec.as_deref().filter(|c| !c.is_empty()),
        audio_codec.as_deref().filter(|c| !c.is_empty()),
        audio_lang.as_deref(),
    );
    let container = merge_container(audio_codec.as_deref().filter(|c| !c.is_empty()));

    // Separate directory for intermediate files (-P temp:), like the fish function.
    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    // yt-dlp writes the real path here in UTF-8. Don't build the filename
    // ourselves from title: on Windows yt-dlp's stdout is sometimes cp1251,
    // and Cyrillic turns into "diamonds". %(title)s lets yt-dlp write the
    // file with a correct Unicode name, and the path is read from this file
    // (--print-to-file is always UTF-8), not from stdout.
    let path_file = tmp_dir.join("__filepath.txt");
    let _ = tokio::fs::remove_file(&path_file).await;

    let mut args: Vec<String> = vec![
        "-f".into(),
        format,
        "--merge-output-format".into(),
        container.into(),
        "--no-playlist".into(),
        "-P".into(),
        format!("home:{}", out_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
    ];
    // A specific audio track needs the client that surfaces dubs.
    if audio_lang.as_deref().is_some_and(|l| !l.is_empty()) {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.extend(network_args());
    args.push("-o".into());
    args.push("%(title)s.%(ext)s".into());
    args.push("--print-to-file".into());
    args.push("after_move:filepath".into());
    args.push(path_file.to_string_lossy().into_owned());
    args.push(url.clone());

    let status = run_ytdlp_status(args, "Failed to run yt-dlp".to_string(), app.clone()).await?;

    // Read the path BEFORE removing tmp_dir.
    let real_path = read_printed_path(&path_file).await;

    cleanup_temp(&out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !status.success() {
        return Err("Video download failed".into());
    }

    let out_file = PathBuf::from(real_path.ok_or("Cannot resolve output file path")?);

    if need_section {
        let clipped_file = clipped_path(&out_file);

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            out_file.to_string_lossy().into_owned(),
            "-ss".into(),
            start_str.clone(),
            "-to".into(),
            end_str.clone(),
            "-c".into(),
            "copy".into(),
            clipped_file.to_string_lossy().into_owned(),
        ];

        if let Some(ref a) = app {
            let _ = a.emit("ytdlp-log", "[ffmpeg] Trimming video…");
        }

        let ffmpeg_status = run_ffmpeg(ffmpeg_args).await?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg trimming failed".into());
        }

        let _ = tokio::fs::remove_file(&out_file).await;
        let _ = tokio::fs::rename(&clipped_file, &out_file).await;
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;

    if meta.len() == 0 {
        return Err("File is empty".into());
    }

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: file_mb(meta.len()),
    })
}

async fn download_audio(
    url: &str,
    out_dir: &Path,
    quality: Quality,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
    audio_lang: Option<String>,
    audio_codec: Option<String>,
    app: Option<AppHandle>,
) -> Result<DownloadResult, String> {
    let start_str = start.as_deref().unwrap_or("00:00:00").to_string();

    let resolved_duration = match duration {
        Some(d) => Some(d),
        None => fetch_duration(url).await,
    };

    let end_str = match end.as_deref() {
        Some(e) if e != "00:00:00" => e.to_string(),
        _ => match resolved_duration {
            Some(dur) => {
                let h = dur / 3600;
                let m = (dur % 3600) / 60;
                let s = dur % 60;
                format!("{:02}:{:02}:{:02}", h, m, s)
            }
            None => "00:00:00".to_string(),
        },
    };

    let need_section = section_changed(&start_str, &end_str, resolved_duration);

    let tmp_dir = out_dir.join(".flowbit-tmp");
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    // Filename comes from yt-dlp (%(title)s), path read from the UTF-8 file — see download_video.
    let path_file = tmp_dir.join("__filepath.txt");
    let _ = tokio::fs::remove_file(&path_file).await;

    let ac = audio_codec.as_deref().filter(|c| !c.is_empty());
    // Output audio format: keep opus/aac as-is (no re-encode loss), "auto" -> mp3 (plays everywhere).
    let out_audio_format = match ac {
        Some("opus") => "opus",
        Some("aac") => "m4a",
        _ => "mp3",
    };
    let mut args: Vec<String> = vec![
        "-f".into(),
        build_audio_format(quality, ac, audio_lang.as_deref()),
        "-x".into(),
        "--audio-format".into(),
        out_audio_format.into(),
        "--audio-quality".into(),
        "0".into(),
        "--no-playlist".into(),
        "-P".into(),
        format!("home:{}", out_dir.to_string_lossy()),
        "-P".into(),
        format!("temp:{}", tmp_dir.to_string_lossy()),
    ];
    if audio_lang.as_deref().is_some_and(|l| !l.is_empty()) {
        args.push("--extractor-args".into());
        args.push(YT_MULTI_AUDIO_CLIENT.into());
    }
    args.extend(network_args());
    args.push("-o".into());
    args.push("%(title)s.%(ext)s".into());
    args.push("--print-to-file".into());
    args.push("after_move:filepath".into());
    args.push(path_file.to_string_lossy().into_owned());
    args.push(url.to_string());

    let output = run_ytdlp_output(args, "yt-dlp error:".to_string(), app.clone()).await?;

    let real_path = read_printed_path(&path_file).await;

    cleanup_temp(out_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    if !output.status.success() {
        return Err("Audio download failed".into());
    }

    let out_file = PathBuf::from(real_path.ok_or("Cannot resolve output file path from yt-dlp")?);

    if need_section {
        let clipped_file = clipped_path(&out_file);

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            out_file.to_string_lossy().into_owned(),
            "-ss".into(),
            start_str.clone(),
            "-to".into(),
            end_str.clone(),
            "-c".into(),
            "copy".into(),
            clipped_file.to_string_lossy().into_owned(),
        ];

        if let Some(ref a) = app {
            let _ = a.emit("ytdlp-log", "[ffmpeg] Trimming audio…");
        }

        let ffmpeg_status = run_ffmpeg(ffmpeg_args).await?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg trimming failed".into());
        }

        let _ = tokio::fs::remove_file(&out_file).await;
        let _ = tokio::fs::rename(&clipped_file, &out_file).await;
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;

    if meta.len() == 0 {
        return Err("File is empty".into());
    }

    Ok(DownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: file_mb(meta.len()),
    })
}

#[cfg(test)]
mod decode_tests {
    use super::decode_output;
    #[test]
    fn utf8_passthrough() {
        assert_eq!(decode_output("Привет.mp4".as_bytes()), "Привет.mp4");
    }
    #[test]
    fn cp1251_fallback() {
        // "Привет" (Cyrillic "hello") in cp1251
        let cp1251 = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(decode_output(&cp1251), "Привет");
    }
    #[test]
    fn ascii_ok() {
        assert_eq!(decode_output(b"[download] 50%"), "[download] 50%");
    }
}

#[cfg(test)]
mod audio_quality_tests {
    use super::{build_audio_format, quality_to_audio_format, Quality};

    #[test]
    fn best_has_no_bitrate_cap() {
        assert_eq!(build_audio_format(Quality::Best, None, None), "ba");
        assert_eq!(quality_to_audio_format(Quality::Best), "ba");
    }

    #[test]
    fn tiers_cap_abr_with_uncapped_fallback() {
        assert_eq!(build_audio_format(Quality::High, None, None), "ba[abr<=160]/ba");
        assert_eq!(build_audio_format(Quality::Medium, None, None), "ba[abr<=80]/ba");
        assert_eq!(build_audio_format(Quality::Low, None, None), "ba[abr<=50]/ba");
        assert_eq!(quality_to_audio_format(Quality::High), "ba[abr<=160]/ba");
        assert_eq!(quality_to_audio_format(Quality::Medium), "ba[abr<=80]/ba");
        assert_eq!(quality_to_audio_format(Quality::Low), "ba[abr<=50]/ba");
    }

    #[test]
    fn worst_uses_worst_audio_selector() {
        assert_eq!(build_audio_format(Quality::Worst, None, None), "wa");
        assert_eq!(quality_to_audio_format(Quality::Worst), "wa");
    }

    #[test]
    fn tier_combines_with_language_and_codec() {
        assert_eq!(
            build_audio_format(Quality::Medium, Some("opus"), Some("ru")),
            "ba[language=ru][abr<=80][acodec^=opus]/ba[language=ru][abr<=80]/ba[abr<=80][acodec^=opus]/ba[abr<=80]/ba[language=ru]/ba"
        );
    }
}
