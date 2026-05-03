use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

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
fn ytdlp_bin() -> &'static str {
    "libs/yt-dlp"
}

fn default_downloads() -> Option<PathBuf> {
    dirs::download_dir()
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());

    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }

    let s = out.trim();
    if s.is_empty() {
        return "video".into();
    }

    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s.to_string()
    }
}

#[inline]
fn quality_to_format(q: Quality) -> &'static str {
    match q {
        Quality::Best => "bestvideo+bestaudio/best",
        Quality::High => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
        Quality::Medium => "bestvideo[height<=720]+bestaudio/best[height<=720]",
        Quality::Low => "bestvideo[height<=480]+bestaudio/best[height<=480]",
        Quality::Worst => "worst",
    }
}

async fn fetch_title(url: &str) -> Result<String, String> {
    let output = Command::new(ytdlp_bin())
        .args(["--print", "title", "--no-playlist", url])
        .output()
        .await
        .map_err(|e| format!("Failed to fetch title: {e}"))?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

fn file_mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

async fn run_ytdlp(args: &[&str]) -> Result<std::process::ExitStatus, String> {
    Command::new(ytdlp_bin())
        .args(args)
        .status()
        .await
        .map_err(|e| format!("yt-dlp failed: {e}"))
}

#[tauri::command]
pub async fn download_video(
    url: String,
    path: Option<String>,
    quality: Option<Quality>,
    mode: Option<DownloadMode>,
) -> Result<DownloadResult, String> {
    let out_dir = path
        .map(PathBuf::from)
        .or_else(default_downloads)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(&url, &out_dir).await;
    }

    let title = fetch_title(&url).await?;
    let filename = sanitize_filename(&title);

    let out_file = out_dir.join(format!("{filename}.mp4"));
    if out_file.exists() {
        return Err("File already exists".into());
    }

    let format = quality_to_format(quality.unwrap_or(Quality::Best));

    let out_tmpl = out_dir.join(format!("{filename}.%(ext)s"));

    let status = run_ytdlp(&[
        "-f",
        format,
        "--merge-output-format",
        "mp4",
        "--ffmpeg-location",
        "libs/ffmpeg",
        "--no-playlist",
        "-o",
        out_tmpl.to_str().unwrap(),
        &url,
    ])
    .await?;

    cleanup_temp(&out_dir).await;

    if !status.success() {
        return Err("Video download failed".into());
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

async fn download_audio(url: &str, out_dir: &Path) -> Result<DownloadResult, String> {
    let title = fetch_title(url).await?;
    let filename = sanitize_filename(&title);

    let out_file = out_dir.join(format!("{filename}.mp3"));
    if out_file.exists() {
        return Err("File already exists".into());
    }

    let out_tmpl = out_dir.join(format!("{filename}.%(ext)s"));

    let status = run_ytdlp(&[
        "-f",
        "bestaudio",
        "-x",
        "--audio-format",
        "mp3",
        "--audio-quality",
        "0",
        "--ffmpeg-location",
        "libs/ffmpeg",
        "--no-playlist",
        "-o",
        out_tmpl.to_str().unwrap(),
        url,
    ])
    .await?;

    cleanup_temp(out_dir).await;

    if !status.success() {
        return Err("Audio download failed".into());
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
