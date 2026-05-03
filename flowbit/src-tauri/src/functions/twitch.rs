use crate::functions::get_info::get_twitch_info;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::State;
use tokio::process::Command;

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
    pub file_size_mb: f64,
}

pub struct TwitchDownloadState;
impl TwitchDownloadState {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum TwitchQuality {
    Best,
    High,
    Medium,
    Low,
    Worst,
}

impl TwitchQuality {
    #[inline]
    fn fmt(self) -> &'static str {
        match self {
            Self::Best => "bestvideo+bestaudio/best",
            Self::High => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
            Self::Medium => "bestvideo[height<=720]+bestaudio/best[height<=720]",
            Self::Low => "bestvideo[height<=480]+bestaudio/best[height<=480]",
            Self::Worst => "worstvideo+worstaudio/worst",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadMode {
    Video,
    Audio,
}

#[inline]
fn ytdlp_bin() -> &'static str {
    "libs/yt-dlp"
}

fn default_downloads() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        let downloads = home.join("Downloads");
        if downloads.exists() {
            return downloads;
        }
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| ".".into())
}

fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());

    for c in name.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }

    let s = out.trim();
    if s.is_empty() {
        return "stream".into();
    }

    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s.to_string()
    }
}

#[inline]
fn mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

async fn cleanup_temp(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        let bad = name.ends_with(".part")
            || name.ends_with(".tmp")
            || name.ends_with(".frag")
            || name.starts_with("temp_");

        if bad {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

async fn run_ytdlp(args: &[&str]) -> Result<std::process::ExitStatus, String> {
    Command::new(ytdlp_bin())
        .args(args)
        .status()
        .await
        .map_err(|e| format!("yt-dlp error: {e}"))
}

pub async fn fetch_json(url: &str) -> Result<Value, String> {
    let out = Command::new(ytdlp_bin())
        .args(["--dump-json", "--no-playlist", url])
        .output()
        .await
        .map_err(|e| format!("yt-dlp error: {e}"))?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }

    serde_json::from_slice(&out.stdout).map_err(|e| format!("JSON error: {e}"))
}

#[tauri::command]
pub async fn download_twitch(
    _state: State<'_, TwitchDownloadState>,
    url: String,
    path: Option<String>,
    quality: Option<TwitchQuality>,
    mode: Option<DownloadMode>,
) -> Result<TwitchDownloadResult, String> {
    let out_dir = path.map(PathBuf::from).unwrap_or_else(default_downloads);

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| e.to_string())?;

    let info = get_twitch_info(url.clone()).await?;
    let name = sanitize(&info.title);

    let is_audio = matches!(mode, Some(DownloadMode::Audio));
    let ext = if is_audio { "mp3" } else { "mp4" };

    let out_file = out_dir.join(format!("{name}.{ext}"));
    if out_file.exists() {
        return Err("File already exists".into());
    }

    let out_tmpl = out_dir.join(format!("{name}.%(ext)s"));

    let status = if is_audio {
        run_ytdlp(&[
            "-f",
            "bestaudio",
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "0",
            "-o",
            out_tmpl.to_str().unwrap(),
            &url,
        ])
        .await?
    } else {
        run_ytdlp(&[
            "-f",
            quality.unwrap_or(TwitchQuality::Best).fmt(),
            "--merge-output-format",
            "mp4",
            "-o",
            out_tmpl.to_str().unwrap(),
            &url,
        ])
        .await?
    };

    cleanup_temp(&out_dir).await;

    if !status.success() {
        return Err("yt-dlp failed".into());
    }

    let meta = tokio::fs::metadata(&out_file)
        .await
        .map_err(|e| e.to_string())?;

    if meta.len() == 0 {
        return Err("Empty file".into());
    }

    Ok(TwitchDownloadResult {
        path: out_file.to_string_lossy().into(),
        file_size_mb: mb(meta.len()),
    })
}
