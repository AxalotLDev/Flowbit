use crate::LIBS_PATH;
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

#[derive(Deserialize, Copy, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    #[default]
    Auto,
    H264,
    H265,
    Vp9,
    Av1,
}

#[derive(Deserialize, Copy, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    #[default]
    Auto,
    Mp3,
    Aac,
    Opus,
    Flac,
}

impl VideoCodec {
    fn as_vcodec(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,

            // NVIDIA NVENC (GPU)
            Self::H264 => Some("h264_nvenc"),
            Self::H265 => Some("hevc_nvenc"),
            Self::Av1 => Some("av1_nvenc"),
            // Intel/AMD VAAPI (если нужно)
            Self::Vp9 => Some("vp9_vaapi"),
        }
    }
}

impl AudioCodec {
    fn as_acodec(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Mp3 => Some("mp3"),
            Self::Aac => Some("aac"),
            Self::Opus => Some("opus"),
            Self::Flac => Some("flac"),
        }
    }

    fn as_audio_format(self) -> &'static str {
        match self {
            Self::Auto | Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Opus => "opus",
            Self::Flac => "flac",
        }
    }
}

#[inline]
pub fn libs_dir() -> &'static str {
    LIBS_PATH.get().expect("LIBS_PATH not initialized yet")
}
#[inline]
pub fn yt_dlp() -> String {
    format!("{}/yt-dlp", libs_dir())
}

#[inline]
pub fn ffmpeg() -> String {
    format!("{}/ffmpeg", libs_dir())
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
    let output = Command::new(yt_dlp())
        .args(["--print", "title", "--no-playlist", url])
        .output()
        .await
        .map_err(|e| format!("Failed to fetch title: {e}"))?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_time_to_secs(t: &str) -> Option<u64> {
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
    let output = Command::new(yt_dlp())
        .args(["--print", "duration", "--no-playlist", url])
        .output()
        .await
        .ok()?;

    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
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

fn file_mb(len: u64) -> f64 {
    len as f64 / 1_048_576.0
}

async fn run_ytdlp(args: Vec<String>) -> Result<std::process::ExitStatus, String> {
    Command::new(yt_dlp())
        .args(&args)
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
    video_codec: Option<VideoCodec>,
    audio_codec: Option<AudioCodec>,
    start: Option<String>,
    end: Option<String>,
    duration: Option<u64>,
) -> Result<DownloadResult, String> {
    let out_dir = path
        .map(PathBuf::from)
        .or_else(default_downloads)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("Cannot create directory: {e}"))?;

    if matches!(mode, Some(DownloadMode::Audio)) {
        return download_audio(&url, &out_dir, audio_codec.unwrap_or_default()).await;
    }

    let title = fetch_title(&url).await?;
    let filename = sanitize_filename(&title);
    let out_file = out_dir.join(format!("{filename}.mp4"));

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

    let format = quality_to_format(quality.unwrap_or(Quality::Best));
    let out_tmpl = out_dir
        .join(format!("{filename}.%(ext)s"))
        .to_string_lossy()
        .into_owned();

    let mut args: Vec<String> = vec![
        "-f".into(),
        format.into(),
        "--merge-output-format".into(),
        "mp4".into(),
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--no-playlist".into(),
    ];

    // Video codec remux/re-encode
    if let Some(vcodec) = video_codec.unwrap_or_default().as_vcodec() {
        args.push("-S".into());
        args.push(format!("vcodec:{vcodec}"));
    }

    // Audio codec remux/re-encode
    if let Some(acodec) = audio_codec.unwrap_or_default().as_acodec() {
        args.push("--postprocessor-args".into());
        args.push(format!("ffmpeg:-acodec {acodec}"));
    }

    args.push("-o".into());
    args.push(out_tmpl);
    args.push(url.clone());

    let status = run_ytdlp(args).await?;

    cleanup_temp(&out_dir).await;

    if !status.success() {
        return Err("Video download failed".into());
    }

    if need_section {
        let temp_input = out_file.clone();

        let clipped_file = out_dir.join(format!("{filename}_cut.mp4"));

        let ffmpeg_args = vec![
            "-y".into(),
            "-i".into(),
            temp_input.to_string_lossy().into_owned(),
            "-ss".into(),
            start_str.clone(),
            "-to".into(),
            end_str.clone(),
            "-c".into(),
            "copy".into(),
            clipped_file.to_string_lossy().into_owned(),
        ];

        let ffmpeg_status = Command::new(ffmpeg())
            .args(&ffmpeg_args)
            .status()
            .await
            .map_err(|e| format!("ffmpeg failed: {e}"))?;

        if !ffmpeg_status.success() {
            return Err("ffmpeg trimming failed".into());
        }

        let _ = tokio::fs::remove_file(&temp_input).await;
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
    codec: AudioCodec,
) -> Result<DownloadResult, String> {
    let title = fetch_title(url).await?;
    let filename = sanitize_filename(&title);

    let out_tmpl = out_dir
        .join(format!("{filename}.%(ext)s"))
        .to_string_lossy()
        .into_owned();

    let args: Vec<String> = vec![
        "-f".into(),
        "bestaudio".into(),
        "-x".into(),
        "--audio-format".into(),
        codec.as_audio_format().into(),
        "--audio-quality".into(),
        "0".into(),
        "--ffmpeg-location".into(),
        ffmpeg().into(),
        "--no-playlist".into(),
        "-o".into(),
        out_tmpl,
        "--print".into(),
        "after_move:filepath".into(),
        url.to_string(),
    ];

    let output = Command::new(yt_dlp())
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("yt-dlp failed: {e}"))?;

    cleanup_temp(out_dir).await;

    if !output.status.success() {
        return Err("Audio download failed".into());
    }

    // 👉 yt-dlp возвращает реальный путь файла
    let real_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string();

    if real_path.is_empty() {
        return Err("Cannot resolve output file path from yt-dlp".into());
    }

    let out_file = PathBuf::from(&real_path);

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
