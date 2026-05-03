use std::path::{PathBuf, Path};
use tauri::State;
use serde::{Serialize, Deserialize};
use serde_json::Value;

#[derive(Serialize, Clone)]
pub struct TwitchVideoInfo {
    pub title:         String,
    pub channel:       String,
    pub duration:      Option<u64>,
    pub is_live:       bool,
    pub thumbnail_url: Option<String>,
    pub view_count:    Option<u64>,
}

#[derive(Serialize, Clone)]
pub struct TwitchDownloadResult {
    pub path:         String,
    pub file_size_mb: f64,
}

pub struct TwitchDownloadState;

impl TwitchDownloadState {
    pub fn new() -> Self { Self }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TwitchQuality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

impl TwitchQuality {
    fn to_format_str(&self) -> &'static str {
        match self {
            TwitchQuality::Best   => "bestvideo+bestaudio/best",
            TwitchQuality::High   => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
            TwitchQuality::Medium => "bestvideo[height<=720]+bestaudio/best[height<=720]",
            TwitchQuality::Low    => "bestvideo[height<=480]+bestaudio/best[height<=480]",
            TwitchQuality::Worst  => "worstvideo+worstaudio/worst",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadMode {
    Video,
    Audio,
}

fn ytdlp_bin() -> PathBuf { PathBuf::from("libs/yt-dlp") }

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

async fn cleanup_temp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".part")
            || name.ends_with(".ytdl")
            || name.ends_with(".tmp")
            || name.ends_with(".frag")
            || name.ends_with(".webm")
            || name.ends_with(".m4a")
            || name.starts_with("temp_")
        {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

#[tauri::command]
pub fn is_twitch_url(text: String) -> bool {
    let t = text.trim().to_lowercase();
    (t.starts_with("https://") || t.starts_with("http://"))
        && (t.contains("twitch.tv/") || t.contains("clips.twitch.tv/"))
}

#[tauri::command]
pub async fn get_twitch_info(url: String) -> Result<TwitchVideoInfo, String> {
    if !is_twitch_url(url.clone()) {
        return Err("Not a valid Twitch URL".into());
    }

    let output = tokio::process::Command::new(ytdlp_bin())
        .args(["--dump-json", "--no-playlist", &url])
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {}", e))?;

    if !output.status.success() {
        return Err(format!("yt-dlp error: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let is_live = json["is_live"].as_bool().unwrap_or(false);

    Ok(TwitchVideoInfo {
        title: json["title"].as_str().unwrap_or("Twitch VOD").to_string(),
        channel: json["uploader"]
            .as_str()
            .or_else(|| json["channel"].as_str())
            .or_else(|| json["uploader_id"].as_str())
            .unwrap_or("Unknown")
            .to_string(),
        is_live,
        duration:      if is_live { None } else { json["duration"].as_f64().map(|d| d as u64) },
        thumbnail_url: json["thumbnail"].as_str().map(str::to_string),
        view_count:    json["view_count"].as_u64(),
    })
}

#[tauri::command]
pub async fn download_twitch(
    _state:  State<'_, TwitchDownloadState>,
    url:     String,
    path:    Option<String>,
    quality: Option<TwitchQuality>,
    mode:    Option<DownloadMode>,
) -> Result<TwitchDownloadResult, String> {
    let out_dir = path.map(PathBuf::from).unwrap_or_else(default_downloads);

    tokio::fs::create_dir_all(&out_dir).await
        .map_err(|e| format!("Cannot create directory: {}", e))?;

    let info     = get_twitch_info(url.clone()).await?;
    let filename = sanitize(&info.title);

    let is_audio = matches!(mode, Some(DownloadMode::Audio));
    let ext      = if is_audio { "mp3" } else { "mp4" };

    let full_name = format!("{}.{}", filename, ext);
    let out_file  = out_dir.join(&full_name);
    let out_tmpl  = out_dir.join(format!("{}.%(ext)s", filename));

    if out_file.exists() {
        return Err(format!("File '{}' already exists.", out_file.display()));
    }

    let status = if is_audio {
        tokio::process::Command::new(ytdlp_bin())
            .args([
                "-f", "bestaudio",
                "-x",
                "--audio-format", "mp3",
                "--audio-quality", "0",
                "-o", out_tmpl.to_str().unwrap_or("%(title)s.%(ext)s"),
                &url,
            ])
            .status()
            .await
            .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?
    } else {
        let fmt = quality.unwrap_or(TwitchQuality::Best).to_format_str();
        tokio::process::Command::new(ytdlp_bin())
            .args([
                "-f", fmt,
                "--merge-output-format", "mp4",
                "-o", out_tmpl.to_str().unwrap_or("%(title)s.%(ext)s"),
                &url,
            ])
            .status()
            .await
            .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?
    };

    cleanup_temp(&out_dir).await;

    if !status.success() {
        return Err("yt-dlp exited with an error.".into());
    }

    let meta = tokio::fs::metadata(&out_file).await
        .map_err(|e| format!("Cannot access output file: {}", e))?;

    if meta.len() == 0 {
        return Err("Downloaded file is empty.".into());
    }

    Ok(TwitchDownloadResult {
        path:         out_file.to_string_lossy().to_string(),
        file_size_mb: meta.len() as f64 / 1_048_576.0,
    })
}