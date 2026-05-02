use std::path::PathBuf;
use std::process::Stdio;
use tauri::{AppHandle, Emitter, State};
use serde::{Serialize, Deserialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, BufReader};

// ──────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct TwitchProgress {
    pub percent: f64,
    pub speed: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Serialize, Clone)]
pub struct TwitchError {
    pub message: String,
    pub error_type: String,
}

#[derive(Serialize, Clone)]
pub struct TwitchVideoInfo {
    pub title: String,
    pub channel: String,
    pub duration: Option<u64>,
    pub is_live: bool,
    pub thumbnail_url: Option<String>,
    pub view_count: Option<u64>,
}

#[derive(Serialize, Clone)]
pub struct TwitchDownloadResult {
    pub path: String,
    pub file_size_bytes: u64,
}

// ──────────────────────────────────────────────
// State
// ──────────────────────────────────────────────

pub struct TwitchDownloadState {
    active: Arc<Mutex<HashMap<String, bool>>>,
}

impl TwitchDownloadState {
    pub fn new() -> Self {
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// ──────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────

fn ytdlp_bin() -> PathBuf {
    PathBuf::from("libs/yt-dlp")
}

fn default_downloads() -> PathBuf {
    for var in &["HOME", "USERPROFILE"] {
        if let Ok(base) = std::env::var(var) {
            let p = PathBuf::from(base).join("Downloads");
            if p.exists() { return p; }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => c,
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() { return "stream".into(); }
    if s.len() > 200 { s[..200].to_string() } else { s }
}

fn emit_twitch_error(app: &AppHandle, message: &str, kind: &str) {
    let _ = app.emit("twitch-error", TwitchError {
        message: message.to_string(),
        error_type: kind.to_string(),
    });
}

async fn cleanup_temp(dir: &std::path::Path) {
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if let Some(n) = e.file_name().to_str() {
                if n.ends_with(".part") || n.ends_with(".ytdl")
                    || n.ends_with(".tmp")  || n.ends_with(".frag")
                    || n.starts_with("temp_")
                {
                    let _ = tokio::fs::remove_file(e.path()).await;
                }
            }
        }
    }
}

// ──────────────────────────────────────────────
// URL validation
// ──────────────────────────────────────────────

#[tauri::command]
pub fn is_twitch_url(text: String) -> bool {
    let t = text.trim().to_lowercase();
    (t.starts_with("https://") || t.starts_with("http://"))
        && (t.contains("twitch.tv/") || t.contains("clips.twitch.tv/"))
}

// ──────────────────────────────────────────────
// get_twitch_info  — вызываем yt-dlp напрямую
// ──────────────────────────────────────────────

#[tauri::command]
pub async fn get_twitch_info(url: String) -> Result<TwitchVideoInfo, String> {
    if !is_twitch_url(url.clone()) {
        return Err("Not a valid Twitch URL".into());
    }

    // yt-dlp --dump-json --no-playlist <url>
    let output = tokio::process::Command::new(ytdlp_bin())
        .args(["--dump-json", "--no-playlist", &url])
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("yt-dlp error: {}", stderr));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let json: Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let title = json["title"]
        .as_str()
        .unwrap_or("Twitch VOD")
        .to_string();

    let channel = json["uploader"]
        .as_str()
        .or_else(|| json["channel"].as_str())
        .or_else(|| json["uploader_id"].as_str())
        .unwrap_or("Unknown channel")
        .to_string();

    let is_live = json["is_live"].as_bool().unwrap_or(false);

    let duration = if is_live {
        None
    } else {
        json["duration"].as_f64().map(|d| d as u64)
    };

    let thumbnail_url = json["thumbnail"]
        .as_str()
        .map(|s| s.to_string());

    let view_count = json["view_count"].as_u64();

    Ok(TwitchVideoInfo {
        title,
        channel,
        duration,
        is_live,
        thumbnail_url,
        view_count,
    })
}

// ──────────────────────────────────────────────
// download_twitch — yt-dlp с парсингом прогресса
// ──────────────────────────────────────────────

#[tauri::command]
pub async fn download_twitch(
    app:   AppHandle,
    state: State<'_, TwitchDownloadState>,
    url:   String,
    path:  Option<String>,
) -> Result<TwitchDownloadResult, String> {

    let out_dir = path.map(PathBuf::from).unwrap_or_else(default_downloads);

    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            let msg = format!("Cannot create directory: {}", e);
            emit_twitch_error(&app, &msg, "FILESYSTEM_ERROR");
            return Err(msg);
        }
    }

    // Получаем мета для имени файла
    let _ = app.emit("twitch-progress", TwitchProgress {
        percent: 5.0, speed: "Fetching info…".into(),
        downloaded_bytes: 0, total_bytes: 0,
    });

    let info = get_twitch_info(url.clone()).await?;
    let filename  = sanitize(&info.title);
    let full_name = format!("{}.mp4", filename);
    let out_file  = out_dir.join(&full_name);
    let out_tmpl  = out_dir.join(format!("{}.%(ext)s", filename));

    if out_file.exists() {
        let msg = format!("File '{}' already exists.", out_file.display());
        emit_twitch_error(&app, &msg, "FILESYSTEM_ERROR");
        return Err(msg);
    }

    // Регистрируем загрузку
    {
        state.active.lock().unwrap().insert(url.clone(), false);
    }

    let _ = app.emit("twitch-progress", TwitchProgress {
        percent: 10.0,
        speed: if info.is_live { "Recording live…".into() } else { "Downloading…".into() },
        downloaded_bytes: 0, total_bytes: 0,
    });

    // Запускаем yt-dlp с прогрессом
    let mut child = tokio::process::Command::new(ytdlp_bin())
        .args([
            "--newline",                        // прогресс построчно
            "--progress",
            "-f", "bestvideo+bestaudio/best",
            "--merge-output-format", "mp4",
            "-o", out_tmpl.to_str().unwrap_or("%(title)s.%(ext)s"),
            &url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let app_clone   = app.clone();
    let url_clone   = url.clone();
    let state_clone = state.active.clone();

    // Читаем stdout для прогресса
    let progress_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Проверяем отмену
            if state_clone.lock().unwrap()
                .get(&url_clone).copied().unwrap_or(false)
            {
                break;
            }

            // Парсим строки вида:
            // [download]  45.3% of  1.23GiB at  2.50MiB/s ETA 00:30
            if line.contains("[download]") && line.contains('%') {
                let progress = parse_progress_line(&line);
                let _ = app_clone.emit("twitch-progress", progress);
            }
        }
    });

    // Читаем stderr для ошибок
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut errors = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[yt-dlp stderr] {}", line);
            errors.push(line);
        }
        errors
    });

    let status = child.wait().await
        .map_err(|e| format!("Failed to wait for yt-dlp: {}", e))?;

    let _ = progress_task.await;
    let errors = stderr_task.await.unwrap_or_default();

    // Убираем из активных
    state.active.lock().unwrap().remove(&url);
    cleanup_temp(&out_dir).await;

    if !status.success() {
        let err_msg = errors.join("\n");
        let msg = classify_ytdlp_error(&err_msg);
        emit_twitch_error(&app, &msg, "DOWNLOAD_ERROR");
        return Err(msg);
    }

    // Проверяем файл
    match tokio::fs::metadata(&out_file).await {
        Ok(meta) if meta.len() > 0 => {
            let size = meta.len();
            let _ = app.emit("twitch-progress", TwitchProgress {
                percent: 100.0,
                speed: format!("Done ({:.1} MB)", size as f64 / 1_048_576.0),
                downloaded_bytes: size,
                total_bytes: size,
            });
            Ok(TwitchDownloadResult {
                path: out_file.to_string_lossy().to_string(),
                file_size_bytes: size,
            })
        }
        Ok(_) => {
            let msg = "Downloaded file is empty.".to_string();
            emit_twitch_error(&app, &msg, "FILESYSTEM_ERROR");
            Err(msg)
        }
        Err(e) => {
            let msg = format!("Cannot access output file: {}", e);
            emit_twitch_error(&app, &msg, "FILESYSTEM_ERROR");
            Err(msg)
        }
    }
}

// ──────────────────────────────────────────────
// cancel_twitch_download
// ──────────────────────────────────────────────

#[tauri::command]
pub async fn cancel_twitch_download(
    state: State<'_, TwitchDownloadState>,
    url: String,
) -> Result<String, String> {
    let mut active = state.active.lock().unwrap();
    match active.get_mut(&url) {
        Some(flag) => { *flag = true; Ok("Cancellation requested".into()) }
        None => Err("No active Twitch download for this URL".into()),
    }
}

// ──────────────────────────────────────────────
// Parse yt-dlp progress line
// ──────────────────────────────────────────────

fn parse_progress_line(line: &str) -> TwitchProgress {
    // Пример: [download]  45.3% of  1.23GiB at  2.50MiB/s ETA 00:30
    let percent = extract_between(line, "", "%")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);

    let speed = extract_between(line, "at ", " ETA")
        .or_else(|| extract_between(line, "at ", "\n"))
        .unwrap_or("—")
        .trim()
        .to_string();

    // Размер: "of 1.23GiB"
    let total_bytes = extract_between(line, "of ", " at")
        .map(|s| parse_size_str(s.trim()))
        .unwrap_or(0);

    let downloaded_bytes = (total_bytes as f64 * percent / 100.0) as u64;

    TwitchProgress { percent, speed, downloaded_bytes, total_bytes }
}

fn extract_between<'a>(s: &'a str, after: &str, before: &str) -> Option<&'a str> {
    let start = if after.is_empty() { 0 } else { s.find(after)? + after.len() };
    let end   = s[start..].find(before).map(|i| start + i)?;
    Some(&s[start..end])
}

fn parse_size_str(s: &str) -> u64 {
    // "1.23GiB", "456.7MiB", "789KiB"
    let s = s.trim();
    if let Some(n) = s.strip_suffix("GiB") {
        return (n.trim().parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0 * 1024.0) as u64;
    }
    if let Some(n) = s.strip_suffix("MiB") {
        return (n.trim().parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0) as u64;
    }
    if let Some(n) = s.strip_suffix("KiB") {
        return (n.trim().parse::<f64>().unwrap_or(0.0) * 1024.0) as u64;
    }
    s.parse::<u64>().unwrap_or(0)
}

fn classify_ytdlp_error(stderr: &str) -> String {
    let e = stderr.to_lowercase();
    if e.contains("private") || e.contains("subscriber") {
        "VOD недоступен — возможно, только для подписчиков.".into()
    } else if e.contains("not found") || e.contains("404") {
        "VOD не найден или удалён.".into()
    } else if e.contains("network") || e.contains("connection") {
        "Ошибка сети. Проверьте интернет-соединение.".into()
    } else if e.contains("ffmpeg") {
        "Ошибка FFmpeg при объединении потоков.".into()
    } else {
        format!("Ошибка yt-dlp: {}", stderr.lines().last().unwrap_or("unknown"))
    }
}