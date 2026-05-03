use yt_dlp::Downloader;
use yt_dlp::client::deps::Libraries;
use yt_dlp::model::selector::{VideoQuality, AudioQuality, VideoCodecPreference};
use std::path::{PathBuf, Path};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Clone)]
pub struct DownloadResult {
    pub path: String,
    pub file_size_mb: f64,
}

pub struct DownloadState;
impl DownloadState {
    pub fn new() -> Self { Self }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

impl Quality {
    fn to_video_quality(&self) -> VideoQuality {
        match self {
            Quality::Best   => VideoQuality::Best,
            Quality::High   => VideoQuality::High,
            Quality::Medium => VideoQuality::Medium,
            Quality::Low    => VideoQuality::Low,
            Quality::Worst  => VideoQuality::Worst,
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

fn sanitize_filename(name: &str) -> String {
    let s: String = name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => c,
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() { return "video".into(); }
    if s.len() > 200 { s[..200].to_string() } else { s }
}

pub async fn cleanup_temp(dir: &Path) {
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
pub async fn download_video(
    url:     String,
    path:    Option<String>,
    quality: Option<Quality>,
    mode:    Option<DownloadMode>,
) -> Result<DownloadResult, String> {
    let out_dir = path.map(PathBuf::from).unwrap_or_else(default_downloads);

    tokio::fs::create_dir_all(&out_dir).await
        .map_err(|e| format!("Cannot create directory: {}", e))?;

    let is_audio = matches!(mode, Some(DownloadMode::Audio));

    // Аудио — напрямую через yt-dlp, минуя библиотеку
    if is_audio {
        return download_audio_ytdlp(&url, &out_dir).await;
    }

    // Видео — через библиотеку yt-dlp-rs
    let libraries = Libraries::new(
        PathBuf::from("libs/yt-dlp"),
        PathBuf::from("libs/ffmpeg"),
    );

    let downloader = Downloader::builder(libraries, &out_dir)
        .build()
        .await
        .map_err(|e| format!("Failed to initialize downloader: {}", e))?;

    let video = downloader
        .fetch_video_infos(url.clone())
        .await
        .map_err(|e| format!("Failed to fetch video info: {}", e))?;

    let filename  = sanitize_filename(&video.title);
    let full_name = format!("{}.mp4", filename);
    let out_file  = out_dir.join(&full_name);

    if out_file.exists() {
        return Err(format!("File '{}' already exists.", out_file.display()));
    }

    let q = quality.unwrap_or(Quality::Best);

    let video_path = downloader
        .download(&video, &full_name)
        .video_quality(q.to_video_quality())
        .video_codec(VideoCodecPreference::Any)
        .audio_quality(AudioQuality::Best)
        .execute()
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    cleanup_temp(&out_dir).await;

    let meta = tokio::fs::metadata(&video_path).await
        .map_err(|e| format!("Cannot access output file: {}", e))?;

    if meta.len() == 0 {
        return Err("Downloaded file is empty.".into());
    }

    Ok(DownloadResult {
        path:         video_path.to_string_lossy().to_string(),
        file_size_mb: meta.len() as f64 / 1_048_576.0,
    })
}

// Аудио через прямой вызов yt-dlp — избегаем проблем с кодеками в библиотеке
async fn download_audio_ytdlp(url: &str, out_dir: &Path) -> Result<DownloadResult, String> {
    // Сначала получаем название через --print title
    let title_output = tokio::process::Command::new(ytdlp_bin())
        .args(["--print", "title", "--no-playlist", url])
        .output()
        .await
        .map_err(|e| format!("Failed to fetch title: {}", e))?;

    let raw_title = String::from_utf8_lossy(&title_output.stdout);
    let filename  = sanitize_filename(raw_title.trim());
    let full_name = format!("{}.mp3", filename);
    let out_file  = out_dir.join(&full_name);
    let out_tmpl  = out_dir.join(format!("{}.%(ext)s", filename));

    if out_file.exists() {
        return Err(format!("File '{}' already exists.", out_file.display()));
    }

    let status = tokio::process::Command::new(ytdlp_bin())
        .args([
            "-f", "bestaudio",
            "-x",
            "--audio-format", "mp3",
            "--audio-quality", "0",
            "--ffmpeg-location", "libs/ffmpeg",
            "--no-playlist",
            "-o", out_tmpl.to_str().unwrap_or("%(title)s.%(ext)s"),
            url,
        ])
        .status()
        .await
        .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?;

    cleanup_temp(out_dir).await;

    if !status.success() {
        return Err("Audio download failed.".into());
    }

    let meta = tokio::fs::metadata(&out_file).await
        .map_err(|e| format!("Cannot access output file: {}", e))?;

    if meta.len() == 0 {
        return Err("Downloaded audio file is empty.".into());
    }

    Ok(DownloadResult {
        path:         out_file.to_string_lossy().to_string(),
        file_size_mb: meta.len() as f64 / 1_048_576.0,
    })
}